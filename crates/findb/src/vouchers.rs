//! 凭证仓储
//!
//! 凭证的保存是"先存表头、再整表替换分录"的两步事务——分录整表替换比逐行 diff 简单得多，
//! 单机场景下性能完全够用，还能避免残留孤儿行。

use chrono::NaiveDate;
use fincore::user::User;
use fincore::{AuxRef, Entry, FinError, Money, Period, Voucher, VoucherSource, VoucherStatus};
use rusqlite::OptionalExtension;

use crate::{
    exact_param, money_param, read_date_opt, read_money, read_money_opt, Db, DbError, DbResult,
};

/// 凭证查询条件
#[derive(Clone, Default, Debug)]
pub struct VoucherQuery {
    pub from: Option<Period>,
    pub to: Option<Period>,
    pub status: Option<VoucherStatus>,
    pub word: Option<String>,
    pub no_from: Option<i32>,
    pub no_to: Option<i32>,
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    /// 摘要 / 科目编码 / 凭证号 关键字
    pub keyword: Option<String>,
    /// 只返回涉及该科目的凭证（含下级）
    pub account_code: Option<String>,
    /// 只返回涉及该辅助核算的凭证
    pub aux: Option<AuxRef>,
    /// 只返回该制单人填制的凭证（数据范围：仅看本人凭证）
    pub prepared_by: Option<String>,
    pub source: Option<VoucherSource>,
    pub limit: Option<i64>,
    /// 数据范围下界（科目编码，含端点）。空 = 不限。
    /// 与 `DataScope::allows_account` 的下界同义，供 `list_page` 下推 SQL。
    pub scope_account_from: String,
    /// 数据范围上界（科目编码，含端点及下级）。空 = 不限。
    pub scope_account_to: String,
    /// 数据范围允许的部门编码（`DataScope::depts`）。空 = 不限。
    ///
    /// 语义必须与 `DataScope::allows_dept` 严格一致：**没打部门标签**的分录
    /// 不算越界（`allows_dept("")` 恒真——多数凭证根本不填部门），只有打了
    /// 标签的分录才必须落在列表内。否则一张没选部门的费用凭证会让所有部门
    /// 会计都看不见它。
    pub scope_depts: Vec<String>,
    /// 排序：true 为按日期+凭证号升序（默认），false 为降序
    pub asc: bool,
    /// 键集分页游标：只返回 id 大于此值的行，配合 ORDER BY id ASC。
    /// 替代 OFFSET 分页，避免深度分页扫描前面所有行。
    pub last_id: Option<i64>,
}

impl VoucherQuery {
    pub fn period(p: Period) -> Self {
        Self {
            from: Some(p),
            to: Some(p),
            asc: true,
            ..Default::default()
        }
    }
    pub fn with_status(mut self, s: Option<VoucherStatus>) -> Self {
        self.status = s;
        self
    }
    pub fn with_keyword(mut self, kw: &str) -> Self {
        let kw = kw.trim().to_string();
        self.keyword = if kw.is_empty() { None } else { Some(kw) };
        self
    }

    /// 套用用户的数据范围（DataScope）：
    /// - `own_voucher_only`：只看本人填制的凭证。
    /// - 科目区间与部门：下推到 SQL，必须与 `User::can_see_voucher` 同口径，
    ///   否则分页 `total` 会比实际能翻到的张数大（见 `list_page` 的注释）。
    pub fn with_data_scope(mut self, u: &User) -> Self {
        let scope = &u.data_scope;
        if scope.own_voucher_only {
            // 凭证的 prepared_by 存的是登录账号（username），保存侧写 username，
            // 这里必须按 username 过滤，否则 display_name ≠ username 的用户一张都看不到
            self.prepared_by = Some(u.username.clone());
        }
        self.scope_account_from = scope.account_from.trim().to_string();
        self.scope_account_to = scope.account_to.trim().to_string();
        // 原样拷贝（不 trim / 不过滤空串）：SQL 侧要跟 `allows_dept` 的逐字比较
        // 完全对齐，这里多做一步归一化反而会让两侧判定分叉。
        self.scope_depts = scope.depts.clone();
        self
    }

    /// 只套用区间型数据范围（科目区间 + 部门），不依赖 `User`。
    /// **不含 `own_voucher_only`**：那一位需要 `username`，调用方自己设
    /// `prepared_by`。用 `with_data_scope` 就不用操心这个。
    pub fn with_range_scope(mut self, scope: &fincore::user::DataScope) -> Self {
        self.scope_account_from = scope.account_from.trim().to_string();
        self.scope_account_to = scope.account_to.trim().to_string();
        self.scope_depts = scope.depts.clone();
        self
    }
}

fn map_entry(r: &rusqlite::Row) -> rusqlite::Result<Entry> {
    let aux_json: String = r.get(5)?;
    let mut aux: AuxRef = serde_json::from_str(&aux_json).unwrap_or_default();
    // 现金流量项目单列存储，读回时补进 aux，界面上就能直接编辑
    let cf: Option<String> = r.get(16)?;
    if aux.cash_flow.is_none() {
        aux.cash_flow = cf;
    }
    let rate: Option<String> = r.get(12)?;
    Ok(Entry {
        id: r.get(0)?,
        line: r.get(2)?,
        summary: r.get(3)?,
        account_code: r.get(4)?,
        aux,
        debit: read_money(r, 6)?,
        credit: read_money(r, 7)?,
        qty: read_money_opt(r, 8)?,
        price: read_money_opt(r, 9)?,
        currency: r.get(10)?,
        rate: rate.and_then(|s| s.parse().ok()),
        amount_for: read_money_opt(r, 11)?,
        settle_type: r.get(13)?,
        settle_no: r.get(14)?,
        biz_date: read_date_opt(r, 15)?,
    })
}

fn map_voucher(r: &rusqlite::Row) -> rusqlite::Result<Voucher> {
    let status: String = r.get(5)?;
    let source: String = r.get(11)?;
    Ok(Voucher {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        date: {
            let s: String = r.get(2)?;
            NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).expect("基准日期必然合法"))
        },
        word: r.get(3)?,
        no: r.get(4)?,
        status: serde_json::from_str::<VoucherStatus>(&format!("\"{status}\""))
            .unwrap_or(VoucherStatus::Draft),
        attachments: r.get(6)?,
        prepared_by: r.get(7)?,
        audited_by: r.get(8)?,
        posted_by: r.get(9)?,
        cashier: r.get(10)?,
        source: serde_json::from_str::<VoucherSource>(&format!("\"{source}\""))
            .unwrap_or(VoucherSource::Manual),
        memo: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
        entries: Vec::new(),
    })
}

const VOUCHER_COLS: &str = "id,period,date,word,no,status,attachments,prepared_by,audited_by,
                            posted_by,cashier,source,memo,created_at,updated_at";

/// 按 id 读取（含分录）
pub fn get(db: &Db, id: i64) -> DbResult<Option<Voucher>> {
    get_of(db.conn(), id)
}

/// 同 [`get`]，但只依赖连接：删除凭证的「读 → 校验 → 清核销 → 删」要收在同一事务内。
fn get_of(conn: &rusqlite::Connection, id: i64) -> DbResult<Option<Voucher>> {
    let mut v = conn
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?;
    if let Some(ref mut v) = v {
        v.entries = entries_on(conn, id)?;
    }
    Ok(v)
}

pub fn entries_of(db: &Db, voucher_id: i64) -> DbResult<Vec<Entry>> {
    entries_on(db.conn(), voucher_id)
}

fn entries_on(conn: &rusqlite::Connection, voucher_id: i64) -> DbResult<Vec<Entry>> {
    let mut stmt = conn.prepare(
        "SELECT id,voucher_id,line,summary,account_code,aux_json,debit,credit,qty,price,
                currency,amount_for,rate,settle_type,settle_no,biz_date,cf_item
         FROM voucher_entry WHERE voucher_id=?1 ORDER BY line",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![voucher_id], map_entry)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 为列表结果批量补充分录（列表界面展示摘要与借贷合计依赖分录）。
/// 一次 IN 查询按批取回，避免逐张 N+1。
pub fn fill_entries(db: &Db, vouchers: &mut [Voucher]) -> DbResult<()> {
    const CHUNK: usize = 500; // SQLite 绑定参数上限以下，分批防超限
    for chunk in vouchers.chunks_mut(CHUNK) {
        let ids: Vec<i64> = chunk.iter().map(|v| v.id).collect();
        let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "SELECT id,voucher_id,line,summary,account_code,aux_json,debit,credit,qty,price,\
                    currency,amount_for,rate,settle_type,settle_no,biz_date,cf_item
             FROM voucher_entry WHERE voucher_id IN ({}) ORDER BY voucher_id, line",
            placeholders.join(",")
        );
        let mut stmt = db.conn().prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|i| i as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(refs.as_slice(), |r| {
                let vid: i64 = r.get(1)?;
                Ok((vid, map_entry(r)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut by_id: std::collections::HashMap<i64, Vec<Entry>> =
            std::collections::HashMap::new();
        for (vid, e) in rows {
            by_id.entry(vid).or_default().push(e);
        }
        for v in chunk.iter_mut() {
            v.entries = by_id.remove(&v.id).unwrap_or_default();
        }
    }
    Ok(())
}

/// 条件查询（只含表头，列表界面不需要分录）
pub fn list(db: &Db, q: &VoucherQuery) -> DbResult<Vec<Voucher>> {
    let (whered, mut params) = build_where(q)?;
    let mut sql = format!("SELECT {VOUCHER_COLS} FROM voucher v WHERE 1=1{whered}");

    sql.push_str(" ORDER BY v.date ");
    sql.push_str(if q.asc { "ASC" } else { "DESC" });
    sql.push_str(", v.word ASC, v.no ");
    sql.push_str(if q.asc { "ASC" } else { "DESC" });

    if let Some(l) = q.limit {
        sql.push_str(" LIMIT ?");
        params.push(Box::new(l));
    }

    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), map_voucher)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 分页查询：返回 `(本页凭证, 过滤后总张数)`。
///
/// 为什么要单独一个函数而不是给 `list` 加 offset：分页必须知道**总张数**才能
/// 渲染页码和合计，而总张数要跟 `VoucherQuery` 的 WHERE 完全一致——一旦两处
/// 条件写得不齐，页码就会算错（页数少于实际能翻到的范围，末页永远打不开）。
/// 所以这里复用同一个 `build_where`，不允许出现第二份条件。
///
/// **返回的凭证同样只含表头**（没有分录），要借贷合计/摘要得再调 `fill_entries`
/// —— 与 `list` 的约定完全一致，不要以为分页版自带明细。
///
/// `total` 与权限过滤的一致性依赖 `with_data_scope` / `with_range_scope` 把数据
/// 范围（科目区间 + 部门）**下推到 SQL**（见这两个方法的注释）；`own_voucher_only`
/// 也早就由 `with_data_scope` 变成 `prepared_by = ?` 了。调用方仍可再 `retain`
/// 一次做兜底，但那时若真过滤掉了行就说明 SQL 条件写错了，应该报错而不是默默
/// 给出错误页码。
pub fn list_page(
    db: &Db,
    q: &VoucherQuery,
    offset: i64,
    limit: i64,
) -> DbResult<(Vec<Voucher>, i64)> {
    let (whered, params) = build_where(q)?;
    // 总数：只取 WHERE，COUNT 走 voucher 主表（NOT EXISTS 子查询在 COUNT 里同样生效）
    let count_sql = format!("SELECT COUNT(*) FROM voucher v WHERE 1=1{whered}");
    let mut stmt = db.conn().prepare(&count_sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let total: i64 = stmt.query_row(refs.as_slice(), |r| r.get(0))?;

    let mut sql = format!("SELECT {VOUCHER_COLS} FROM voucher v WHERE 1=1{whered}");
    sql.push_str(" ORDER BY v.date ");
    sql.push_str(if q.asc { "ASC" } else { "DESC" });
    sql.push_str(", v.word ASC, v.no ");
    sql.push_str(if q.asc { "ASC" } else { "DESC" });
    sql.push_str(" LIMIT ? OFFSET ?");
    let mut p2 = params;
    p2.push(Box::new(limit));
    p2.push(Box::new(offset));
    let mut stmt = db.conn().prepare(&sql)?;
    let refs2: Vec<&dyn rusqlite::types::ToSql> = p2.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs2.as_slice(), map_voucher)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((rows, total))
}

/// 键集分页列表（替代 OFFSET 分页）。
///
/// 用 `WHERE id > ? ORDER BY id ASC LIMIT ?`，深度分页不再扫描前面已跳过的行。
/// 多取一行用于判断 `has_more`，返回的 `next_id` 即下一页的 `last_id`。
/// 排序以 `id`（录入先后）为准，与 `list_page` 的 `(date, word, no)` 不同，
/// 调用方需知悉两者顺序可能不同，但均为确定性顺序。
pub fn list_page_keyset(
    db: &Db,
    q: &VoucherQuery,
    limit: i64,
) -> DbResult<(Vec<Voucher>, Option<i64>, bool)> {
    let (whered, mut params) = build_where(q)?;
    if let Some(last_id) = q.last_id {
        params.push(Box::new(last_id));
    }
    let id_param_idx = params.len(); // v.id > ? 的参数位置（若 last_id 存在则已 push）

    let mut sql = format!(
        "SELECT {VOUCHER_COLS} FROM voucher v WHERE 1=1{whered}",
    );
    if q.last_id.is_some() {
        sql.push_str(&format!(" AND v.id > ?{}", id_param_idx + 1));
    }
    sql.push_str(" ORDER BY v.id ASC LIMIT ?");
    params.push(Box::new(limit + 1)); // 多取一行探测 has_more

    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = stmt
        .query_map(refs.as_slice(), map_voucher)?
        .collect::<Result<Vec<_>, _>>()?;

    let has_more = rows.len() > limit as usize;
    let rows = if has_more { rows.into_iter().take(limit as usize).collect() } else { rows };
    let next_id = rows.last().map(|v| v.id);

    Ok((rows, next_id, has_more))
}

/// 构造 `list` 与 `list_page` 共用的 WHERE 片段（**唯一的一份条件**）。
fn build_where(q: &VoucherQuery) -> DbResult<(String, Vec<Box<dyn rusqlite::types::ToSql>>)> {
    let mut sql = String::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(f) = q.from {
        sql.push_str(" AND v.period >= ?");
        params.push(Box::new(f.ymm()));
    }
    if let Some(t) = q.to {
        sql.push_str(" AND v.period <= ?");
        params.push(Box::new(t.ymm()));
    }
    if let Some(s) = q.status {
        sql.push_str(" AND v.status = ?");
        params.push(Box::new(
            serde_json::to_value(s)?
                .as_str()
                .unwrap_or("draft")
                .to_string(),
        ));
    }
    if let Some(ref w) = q.word {
        sql.push_str(" AND v.word = ?");
        params.push(Box::new(w.clone()));
    }
    if let Some(n) = q.no_from {
        sql.push_str(" AND v.no >= ?");
        params.push(Box::new(n));
    }
    if let Some(n) = q.no_to {
        sql.push_str(" AND v.no <= ?");
        params.push(Box::new(n));
    }
    if let Some(d) = q.date_from {
        sql.push_str(" AND v.date >= ?");
        params.push(Box::new(d.format("%Y-%m-%d").to_string()));
    }
    if let Some(d) = q.date_to {
        sql.push_str(" AND v.date <= ?");
        params.push(Box::new(d.format("%Y-%m-%d").to_string()));
    }
    if let Some(src) = q.source {
        sql.push_str(" AND v.source = ?");
        params.push(Box::new(
            serde_json::to_value(src)?
                .as_str()
                .unwrap_or("manual")
                .to_string(),
        ));
    }
    if let Some(ref who) = q.prepared_by {
        sql.push_str(" AND v.prepared_by = ?");
        params.push(Box::new(who.clone()));
    }
    if let Some(ref code) = q.account_code {
        sql.push_str(
            " AND EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id AND e.account_code LIKE ? ESCAPE '\\')",
        );
        params.push(Box::new(format!("{}%", crate::escape_like(code))));
    }
    // 数据范围的科目区间下推到 SQL：语义必须和 `User::can_see_voucher` 的
    // 「全部科目都在范围内才可见」严格一致，否则分页 total 与实际能翻到的
    // 范围对不上。逐条对照 `DataScope::allows_account`：
    //   下界非空 → 任一分录 code < lo 即不可见 → NOT EXISTS(越下界的分录)
    //   上界非空 → 任一分录 !(code <= hi || 前缀 hi) 即不可见
    // 空分录时两个 NOT EXISTS 都为真（可见），与 Rust 侧 `all()` 对空迭代
    // 返回 true 的行为一致。
    let lo = q.scope_account_from.trim();
    let hi = q.scope_account_to.trim();
    if !lo.is_empty() {
        sql.push_str(
            " AND NOT EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id AND e.account_code < ?)",
        );
        params.push(Box::new(lo.to_string()));
    }
    if !hi.is_empty() {
        sql.push_str(
            " AND NOT EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id
                             AND NOT (e.account_code <= ? OR e.account_code LIKE ? ESCAPE '\\'))",
        );
        params.push(Box::new(hi.to_string()));
        params.push(Box::new(format!("{}%", crate::escape_like(hi))));
    }
    // 数据范围的部门区间下推到 SQL，语义照抄 `DataScope::allows_dept`：
    //   depts 为空      → 不限
    //   分录没打部门标签 → 放行（`allows_dept("")` 恒真）
    //   分录打了部门标签 → 必须落在允许列表内
    // 「有没有部门片段」要按 `\x1fdept=` 判：aux_key 由 `AuxRef::key_parts` 用
    // \x1f 连接 `kind=value` 而成，两端补分隔符后既不会命中别的维度，也不会
    // 像裸子串那样把 `dept=` 匹配到值里面去。
    // 注：Rust 侧对分录的部门值做了 `trim()` 兜底，这里按原值整段比较；部门
    // 编码是界面上选出来的档案码，不带首尾空白，两种口径等价。
    if !q.scope_depts.is_empty() {
        sql.push_str(
            " AND NOT EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id
                             AND (char(31) || e.aux_key || char(31)) LIKE ? ESCAPE '\\'
                             AND NOT (",
        );
        params.push(Box::new("%\u{1f}dept=%".to_string()));
        for (i, d) in q.scope_depts.iter().enumerate() {
            if i > 0 {
                sql.push_str(" OR ");
            }
            sql.push_str("(char(31) || e.aux_key || char(31)) LIKE ? ESCAPE '\\'");
            params.push(Box::new(format!(
                "%\u{1f}dept={}\u{1f}%",
                crate::escape_like(d)
            )));
        }
        sql.push_str("))");
    }
    if let Some(ref aux) = q.aux {
        // 逐维度整段匹配，不能把整条 aux_key 当子串 LIKE：
        //   旧写法 `aux_key LIKE '%customer=C001%'` 会连带命中 `customer=C0011`，
        //   也会把 `project=ACME` 命中到按 `customer=AC` 过滤的查询上（实测确认）。
        // 现在把两侧都补上 \x1f 分隔符再匹配整段，等价于 Rust 侧
        // `balances::aux_key_contains` 的"按维度切开逐段精确匹配"语义。
        for part in aux.key_parts() {
            sql.push_str(
                " AND EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id
                             AND (char(31) || e.aux_key || char(31)) LIKE ? ESCAPE '\\')",
            );
            params.push(Box::new(format!(
                "%\u{1f}{}\u{1f}%",
                crate::escape_like(&part)
            )));
        }
    }
    if let Some(ref kw) = q.keyword {
        sql.push_str(
            " AND (v.memo LIKE ? ESCAPE '\\' OR CAST(v.no AS TEXT) LIKE ? ESCAPE '\\'
                   OR EXISTS(SELECT 1 FROM voucher_entry e WHERE e.voucher_id=v.id
                             AND (e.summary LIKE ? ESCAPE '\\' OR e.account_code LIKE ? ESCAPE '\\')))",
        );
        let k = format!("%{}%", crate::escape_like(kw));
        for _ in 0..4 {
            params.push(Box::new(k.clone()));
        }
    }
    Ok((sql, params))
}

/// 保存（新增或更新）。返回凭证 id。
///
/// 这里是**持久化层的最后一道防线**：即使调用方忘了校验，也不允许把
/// 借贷不平衡、科目不合法、期间已结账、或已审核/已记账的凭证写进账套。
/// 界面层负责给友好提示，数据库层必须无条件拦住。
pub fn save(db: &Db, v: &mut Voucher) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let id = save_in(&tx, v)?;
    tx.commit()?;
    v.id = id;
    Ok(id)
}

/// 在**调用方已开启的事务**里保存凭证：守卫与落库都在该事务内完成，既不发
/// `BEGIN` 也不提交。
///
/// 「生成凭证 + 回写业务单据 / 扣库存 / 归集成本」需要原子的调用方用它。
/// `save` 自带事务，那些流程只能拆成两次提交，中途失败就留下「库存已扣、账上无
/// 凭证」的半成品；`next_no` 取号也因跨事务而会并发撞号。
pub fn save_in(tx: &rusqlite::Transaction, v: &mut Voucher) -> DbResult<i64> {
    save_on(tx, v)
}

/// 守卫 + 落库，全部跑在给定的连接上（通常就是调用方的事务；`Transaction` 会
/// Deref 到 `Connection`）。不发 BEGIN、不提交。
fn save_on(tx: &rusqlite::Connection, v: &mut Voucher) -> DbResult<i64> {
    // ---- 0. 前置守卫 ----
    // 守卫一律在同一事务里读，否则「查结账线 / 查凭证号占用 → 写入」之间存在
    // 别人见缝插针的窗口（期间刚被结账、或同号凭证刚被别人插入）。
    let closed = crate::periods::closed_upto_of(tx)?;
    if let Some(upto) = closed {
        if v.period <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能再保存该期间的凭证",
                upto.label()
            ))
            .into());
        }
    }
    if v.id > 0 {
        // 更新：草稿可直接改；已审核/已记账需先反审核/反记账。
        // 只取 status 一列，不必像 get() 那样把分录全捞出来。
        let old_status: Option<String> = tx
            .query_row(
                "SELECT status FROM voucher WHERE id=?1",
                rusqlite::params![v.id],
                |r| r.get(0),
            )
            .optional()?;
        let old_status =
            old_status.ok_or_else(|| FinError::not_found(format!("凭证 #{}", v.id)))?;
        let old_status = serde_json::from_str::<VoucherStatus>(&format!("\"{old_status}\""))
            .unwrap_or(VoucherStatus::Draft);
        if !old_status.can_edit() {
            return Err(FinError::state(format!(
                "凭证当前状态为「{}」，不能修改（请先反审核 / 反记账）",
                old_status.label()
            ))
            .into());
        }
        // 启用审核环节的账套：已审核凭证不能直接改，须先反审核
        if old_status == VoucherStatus::Audited && crate::options_of(tx).enable_audit {
            return Err(FinError::state("已审核凭证不能修改，请先反审核").into());
        }
    } else if v.status == VoucherStatus::Void {
        return Err(FinError::state("新增凭证不能直接标记为「已作废」".to_string()).into());
    }
    let taken: i64 = tx.query_row(
        "SELECT COUNT(*) FROM voucher WHERE period=?1 AND word=?2 AND no=?3 AND id<>?4",
        rusqlite::params![v.period.ymm(), v.word, v.no, v.id],
        |r| r.get(0),
    )?;
    if taken > 0 {
        return Err(FinError::msg(format!("{}-{:04} 已存在，请更换凭证号", v.word, v.no)).into());
    }
    let chart = crate::accounts::chart_of(tx)?;
    let opts = crate::options_of(tx);
    fincore::engine::validate_for_save(
        v,
        &fincore::engine::ValidateCtx::new(&chart, &opts, closed),
    )
    .into_result()?;

    // ---- 1. 落库 ----
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let id: i64 = if v.id > 0 {
        tx.execute(
            "UPDATE voucher SET period=?2,date=?3,word=?4,no=?5,attachments=?6,status=?7,
                    prepared_by=?8,audited_by=?9,posted_by=?10,cashier=?11,source=?12,memo=?13,
                    updated_at=?14
             WHERE id=?1",
            rusqlite::params![
                v.id,
                v.period.ymm(),
                v.date.format("%Y-%m-%d").to_string(),
                v.word,
                v.no,
                v.attachments,
                serde_json::to_value(v.status)?.as_str().unwrap_or("draft"),
                v.prepared_by,
                v.audited_by,
                v.posted_by,
                v.cashier,
                serde_json::to_value(v.source)?.as_str().unwrap_or("manual"),
                v.memo,
                now,
            ],
        )?;
        tx.execute(
            "DELETE FROM voucher_entry WHERE voucher_id=?1",
            rusqlite::params![v.id],
        )?;
        v.id
    } else {
        tx.execute(
            "INSERT INTO voucher(period,date,word,no,attachments,status,prepared_by,audited_by,
                                 posted_by,cashier,source,memo,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",
            rusqlite::params![
                v.period.ymm(),
                v.date.format("%Y-%m-%d").to_string(),
                v.word,
                v.no,
                v.attachments,
                serde_json::to_value(v.status)?.as_str().unwrap_or("draft"),
                v.prepared_by,
                v.audited_by,
                v.posted_by,
                v.cashier,
                serde_json::to_value(v.source)?.as_str().unwrap_or("manual"),
                v.memo,
                now,
            ],
        )?;
        tx.last_insert_rowid()
    };

    // 分录：跳过借贷均为零的空行
    let mut stmt = tx.prepare(
        "INSERT INTO voucher_entry(voucher_id,period,line,summary,account_code,aux_key,aux_json,
                debit,credit,qty,price,currency,amount_for,rate,settle_type,settle_no,biz_date,cf_item)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
    )?;
    let mut line = 0i32;
    for e in &v.entries {
        if e.is_blank() {
            continue;
        }
        line += 1;
        stmt.execute(rusqlite::params![
            id,
            v.period.ymm(),
            line,
            e.summary,
            e.account_code,
            e.aux.key(),
            serde_json::to_string(&e.aux)?,
            money_param(e.debit),
            money_param(e.credit),
            e.qty.map(exact_param),
            e.price.map(exact_param),
            e.currency,
            e.amount_for.map(money_param),
            e.rate.map(|r| r.to_string()),
            e.settle_type,
            e.settle_no,
            e.biz_date.map(|d| d.format("%Y-%m-%d").to_string()),
            e.aux.cash_flow.clone(),
        ])?;
    }
    drop(stmt);
    v.id = id;
    Ok(id)
}

/// 删除凭证（连同分录，外键 ON DELETE CASCADE）
///
/// 「读凭证 → 校验 → 清核销 → 删表」整体收在一个 `write_tx` 里：核销清理是逐条
/// `DELETE`，第 3 条失败时前两条已经在别的事务里提交，凭证却还在——账上留着一条
/// 已核销的凭证，核销记录却只剩半截，比删不掉更难查。
///
/// 文件删除在提交之后做：反过来做（旧实现先删文件）一旦删库失败——状态被拦、
/// 拿不到写锁、外键报错——凭证还留在账上而附件文件已经没了，变成引用空文件的坏账。
/// 删文件失败只留下孤儿文件，无害得多。
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    // 附件清单先取（attach::list 是纯读，与事务无关）
    let files: Vec<String> = crate::attach::list(db, id)?
        .into_iter()
        .filter(|a| !a.inline)
        .filter_map(|a| a.path)
        .collect();
    if db.conn().is_autocommit() {
        let tx = db.write_tx()?;
        delete_in(&tx, id)?;
        tx.commit()?;
    } else {
        // 调用方（如 receipt_unaudit）已开启事务：复用它，失败由它整体回滚。
        // 这里再 BEGIN 会被 SQLite 拒掉（cannot start a transaction within a
        // transaction），所以必须走同连接直写这条路。
        delete_in_of(db.conn(), id)?;
    }
    let dir = crate::attach::dir_of(db);
    for rel in files {
        // 与 attach::delete 保持同样的路径约束，避免越出附件目录
        if rel.contains("..") || rel.contains('/') || rel.contains('\\') {
            continue;
        }
        let _ = std::fs::remove_file(dir.join(rel));
    }
    Ok(())
}

/// 在调用方已开启的事务里删凭证（不 commit）：供已有事务的流程复用。
pub fn delete_in(tx: &rusqlite::Transaction, id: i64) -> DbResult<()> {
    delete_in_of(tx, id)
}

fn delete_in_of(conn: &rusqlite::Connection, id: i64) -> DbResult<()> {
    // 持久层最后防线：状态与结账线在这里再校验一次，UI/Web 漏检也删不掉
    let v = get_of(conn, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    // 启用审核环节的账套：已审核凭证须先反审核才能删除
    if v.status == VoucherStatus::Audited && crate::options_of(conn).enable_audit {
        return Err(FinError::state("已审核凭证不能删除，请先反审核").into());
    }
    fincore::engine::validate_delete(&v, crate::periods::closed_upto_of(conn)?).into_result()?;
    // 删除前清掉该凭证分录上的核销配对（手工/收付款自动核销），避免留下悬空核销记录
    let entry_ids: Vec<i64> = {
        let mut st = conn.prepare("SELECT id FROM voucher_entry WHERE voucher_id=?1")?;
        let ids = st
            .query_map([id], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids
    };
    for e in entry_ids {
        conn.execute(
            "DELETE FROM settle_record WHERE from_entry=?1 OR to_entry=?1",
            rusqlite::params![e],
        )?;
    }
    // 条件删除：状态在上面的校验与本次写入之间若被改动（并发反记账/作废），这里拒绝
    let n = conn.execute(
        "DELETE FROM voucher WHERE id=?1 AND status<>'posted'",
        rusqlite::params![id],
    )?;
    if n == 0 {
        return Err(FinError::state("凭证状态已变化，请刷新后重试").into());
    }
    Ok(())
}

/// 下一个可用凭证号
///
/// 单独取号只在**同一个写事务里接着 `save_in`** 时才可靠；两者跨事务时并发调用者
/// 会取到同一个号，靠 `voucher(period,word,no)` 唯一索引兜底成一次失败。
pub fn next_no(db: &Db, period: Period, word: &str) -> DbResult<i32> {
    next_no_of(db.conn(), period, word)
}

/// 同 `next_no`，但只依赖连接，可在调用方的事务内取号。
pub fn next_no_of(tx: &rusqlite::Connection, period: Period, word: &str) -> DbResult<i32> {
    let max: Option<i32> = tx
        .query_row(
            "SELECT MAX(no) FROM voucher WHERE period=?1 AND word=?2",
            rusqlite::params![period.ymm(), word],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(max.unwrap_or(0) + 1)
}

/// 检查凭证号是否被占用（排除自身）
pub fn no_taken(db: &Db, period: Period, word: &str, no: i32, except_id: i64) -> DbResult<bool> {
    let c: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM voucher WHERE period=?1 AND word=?2 AND no=?3 AND id<>?4",
        rusqlite::params![period.ymm(), word, no, except_id],
        |r| r.get(0),
    )?;
    Ok(c > 0)
}

// ---------------- 状态流转 ----------------

fn set_status_on(conn: &rusqlite::Connection, id: i64, s: VoucherStatus) -> DbResult<()> {
    conn.execute(
        "UPDATE voucher SET status=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![
            id,
            serde_json::to_value(s)?.as_str().unwrap_or("draft"),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(())
}

/// 一张凭证的更正链信息
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AmendLink {
    /// 本凭证更正的是哪张
    pub amends_id: i64,
    pub amends_no: String,
    /// 本凭证是被哪张**红冲**了（更正链的第一张后续凭证）
    pub amended_by: i64,
    pub amended_by_no: String,
    /// 与红冲同批产生的**更正凭证**（重做后的那张）
    ///
    /// 单列出来是因为它与 `amended_by` 是**两张**凭证：`amended_by` 只够指一张，
    /// 而只列一张就等于账上凭空多出一张没交代来路的凭证。
    pub amended_by_new: i64,
    pub amended_by_new_no: String,
    /// 更正原因（必填 —— 没有原因的更正等于没更正）
    pub reason: String,
}

/// 读一张凭证的更正链（凭证不存在时返回 None）
pub fn amend_link(db: &Db, id: i64) -> DbResult<Option<AmendLink>> {
    let row: Option<(i64, i64, String)> = db
        .conn()
        .query_row(
            "SELECT amends_id, amended_by, amend_reason FROM voucher WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((amends_id, amended_by, reason)) = row else {
        return Ok(None);
    };
    // 单号做成「记-000012」这种可读形式：链上要能一眼看懂是哪张，
    // 光给个数字 id 在对账时没用
    let label = |vid: i64| -> String {
        db.conn()
            .query_row("SELECT word, no FROM voucher WHERE id=?1", [vid], |r| {
                Ok(format!(
                    "{}-{:06}",
                    r.get::<_, String>(0)?,
                    r.get::<_, i32>(1)?
                ))
            })
            .unwrap_or_default()
    };
    // 同批产生的两张后续凭证：红冲那张记在 amended_by，更正那张要单独找出来。
    // 两张的 `amends_id` 都指向**原凭证**（即本行 id），且带同一个更正原因 ——
    // 所以用原凭证 id 去查，不能拿 `amended_by`（那是红冲自己的 id）去查。
    let sibling = |orig: i64, exclude: i64| -> i64 {
        db.conn()
            .query_row(
                "SELECT id FROM voucher
                 WHERE amends_id=?1 AND id<>?2 AND amend_reason=?3 ORDER BY id LIMIT 1",
                rusqlite::params![orig, exclude, reason],
                |r| r.get(0),
            )
            .unwrap_or(0)
    };
    let sibling_id = if amended_by > 0 {
        sibling(id, amended_by)
    } else {
        0
    };
    Ok(Some(AmendLink {
        amends_id,
        amends_no: if amends_id > 0 {
            label(amends_id)
        } else {
            String::new()
        },
        amended_by,
        amended_by_no: if amended_by > 0 {
            label(amended_by)
        } else {
            String::new()
        },
        amended_by_new: sibling_id,
        amended_by_new_no: if sibling_id > 0 {
            label(sibling_id)
        } else {
            String::new()
        },
        reason,
    }))
}

/// 单据更正链：红字冲销原凭证 + 生成一张带链接的新凭证
///
/// 一次 amend 在**一个事务**里产生两张新凭证，总账形态是：
///
/// ```text
///   原凭证   借 存货 1000 / 贷 应付 1000     （保持已记账，不动）
///   红冲凭证 借 应付 1000 / 贷 存货 1000     （新，抵消原凭证）
///   更正凭证 借 存货 1000 / 贷 应付 1000     （新，草稿待记账；改成 1200 后净 +1200）
/// ```
///
/// ## 为什么是这个形态
///
/// 中国准则对「以前年度差错」要求**红字冲销 + 蓝字重做** —— 总账里要看得见
/// 那一笔反向记录。ERPNext 的 Cancel 同理（官方论坛原话：
/// "the ledger will not be deleted, but reverse entries will be added"）。
///
/// 早先的实现是把原凭证置为 `void` 再复制一张。作废凭证不进总账（balances
/// 只取 `status='posted'`），**总账上因此「看不出曾经记错过」** —— 链信息只
/// 留在凭证表里。是形式上留痕、账上无痕。
///
/// ## 与 `reverse`（红字冲销）的分工
///
/// | | 本函数（amend） | `reverse`
/// |---|---|---|
/// | 原凭证 | 保留已记账 | 保留已记账 |
/// | 新凭证 | **红冲 + 蓝字重做**两张 | 只有红冲一张 |
/// | 适合 | 整单内容错，要重做分录 | 只调整金额或时点 |
///
/// 只调金额/时点走红字冲销更省事；整单重做走本函数。
///
/// ## 规则
///
/// - 只有**已记账**的凭证需要更正。草稿直接改，不必走这里。
/// - 更正原因**必填**：没有原因的更正等于没更正。
/// - 一张凭证**只能被更正一次**（`amended_by` 非 0 就拒绝），否则链会分叉，
///   审计时看不出「哪条是最终版本」。
/// - 全部动作在**一个事务**里：原凭证已作废却没有替代凭证是最坏状态 ——
///   账实凭空少一笔，而且凭证号已经用掉了。
/// - 新凭证落在**原凭证的期间与日期**（不是当前期间/今天）：更正属于被更正的那个
///   会计期间，否则两张凭证分属不同期间，月度报表会出现「凭空一笔」；而凭证校验
///   强制 `date` 必须属于 `period`，用今天的日子配 1 月的期间根本存不进去。
pub fn amend(db: &Db, id: i64, reason: &str, who: &str) -> DbResult<i64> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(FinError::msg("更正原因不能为空：没有原因的更正等于没更正").into());
    }
    let tx = db.write_tx()?;
    let old: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    // 「已被更正过」要**先于**状态检查说：更正过的凭证一定是「已作废」，
    // 反过来判就会得到笼统的「只有已记账的凭证才需要更正」，
    // 而真正的理由（别再改这张，去改新那张）被吞掉了。
    let already: i64 = tx.query_row("SELECT amended_by FROM voucher WHERE id=?1", [id], |r| {
        r.get(0)
    })?;
    if already > 0 {
        return Err(FinError::state(format!(
            "本凭证已被 #{already} 更正过，不能重复更正 —— 更正链分叉后审计就看不出哪条是最终版本了"
        ))
        .into());
    }
    if old.status != VoucherStatus::Posted {
        return Err(FinError::state(format!(
            "只有已记账的凭证才需要更正，当前是「{}」—— 草稿/未记账的直接改就行",
            old.status.label()
        ))
        .into());
    }
    // 期间已结账的凭证不能更正（与 set_void 同口径）
    let closed: Option<i32> = tx.query_row(
        "SELECT MAX(period) FROM period_state WHERE closed=1",
        [],
        |r| r.get(0),
    )?;
    if let Some(upto) = closed {
        if old.period.ymm() <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能更正该期间的凭证",
                Period::from_ymm(upto).label()
            ))
            .into());
        }
    }

    let entries = entries_on(&tx, id)?;
    if entries.is_empty() {
        return Err(FinError::state("原凭证没有分录，无法更正".to_string()).into());
    }

    // ① 红字冲销（准则形态：先红冲，再蓝字重做）
    //
    // 原凭证**保持已记账**：中国准则对以前年度差错要求「红字冲销 + 蓝字重做」，
    // 也就是总账里要看得见那一笔反向记录；ERPNext 的 Cancel 同理
    // （"the ledger will not be deleted, but reverse entries will be added"）。
    //
    // 早先这里是把原凭证置为 void —— 作废凭证不进总账（balances 只取
    // `status='posted'`），于是总账上「看不出曾经记错过」，链信息只留在凭证表。
    // 那是形式上留痕、账上无痕。
    // `VOUCHER_COLS` 不含分录（分录在 voucher_entry 表），`old.entries` 是空的。
    // 直接拿它去 reverse 会得到一张零分录的凭证，保存时被校验拒掉。
    let mut old_full = old.clone();
    old_full.entries = entries.clone();
    let mut red = fincore::engine::reverse_voucher(&old_full);
    red.date = old.date;
    red.period = old.period;
    red.no = next_no_of(&tx, old.period, &old.word)?;
    red.status = VoucherStatus::Draft;
    red.prepared_by = who.to_string();
    red.audited_by = None;
    red.posted_by = None;
    red.cashier = None;
    red.source = old.source;
    red.memo = format!("红冲 {}-{:06}（更正原因：{}）", old.word, old.no, reason);
    let red_id = save_on(&tx, &mut red)?;

    // ② 蓝字重做：复制原分录成新凭证（草稿态，等会计审核记账）
    //
    // 日期必须落在**原期间内**（凭证校验强制 date 属于 period）。所以沿用原凭证
    // 日期而不是今天 —— 用今天会把一张 1 月的更正记进 9 月的账，月度报表凭空多一笔。
    let mut v = Voucher::new(old.period, old.date, &old.word, 0);
    v.prepared_by = who.to_string();
    v.source = old.source;
    v.memo = format!("更正 {}-{:06}（{}）", old.word, old.no, reason);
    v.attachments = old.attachments;
    v.entries = entries;
    v.no = next_no_of(&tx, old.period, &old.word)?;
    let new_id = save_on(&tx, &mut v)?;

    // ③ 双向建链（原凭证 ← 红冲凭证 + 更正凭证）
    //
    // 红冲凭证自己的 `amends_id` 也挂原凭证：账上不止一张凭证由本次更正产生，
    // 审计时从原凭证要能看到**两张**（只挂一张就等于有一张凭空出现）。
    tx.execute(
        "UPDATE voucher SET amends_id=?2, amend_reason=?3, updated_at=?4 WHERE id=?1",
        rusqlite::params![red_id, id, reason, now],
    )?;
    tx.execute(
        "UPDATE voucher SET amends_id=?2, amend_reason=?3, updated_at=?4 WHERE id=?1",
        rusqlite::params![new_id, id, reason, now],
    )?;
    tx.execute(
        "UPDATE voucher SET amended_by=?2, amend_reason=?3, updated_at=?4 WHERE id=?1",
        rusqlite::params![id, red_id, reason, now],
    )?;
    // ④ 操作日志必须**在同一个事务里**（与 post / unpost 同口径）。
    // 放在调用方写的话：更正已提交、日志还没写就崩溃 —— 账改了却查不到谁改的，
    // 那正是这套更正链要解决的审计洞，不能自己再捅一个。
    crate::log_on(
        &tx,
        who,
        "凭证",
        "更正",
        &format!("凭证 #{id} -> 红冲 #{red_id} + 更正 #{new_id}：{reason}"),
    )?;
    tx.commit()?;
    Ok(new_id)
}

pub fn post(db: &Db, id: i64, who: &str) -> DbResult<()> {
    // 校验与状态更新必须在同一个写事务里重读：否则并发结账/作废后，
    // 本请求按事务外的旧快照仍会写入 posted。
    let tx = db.write_tx()?;
    let label = post_tx(&tx, id, who)?;
    crate::log_on(&tx, who, "凭证", "记账", &label)?;
    tx.commit()?;
    Ok(())
}

/// 反记账（已记账 → 未记账）
/// 更正链守卫：更正产生的三张凭证（原 + 红冲 + 更正）必须**成套存在**。
///
/// 单独把任何一张移出总账，净额都会错：
/// - 移出原凭证 → 只剩 红冲 + 更正，账上凭空少一笔
/// - 移出红冲凭证 → 只剩 原 + 更正，一笔钱记两遍
/// - 移出更正凭证 → 只剩 原 + 红冲，凭空少一笔
///
/// 唯一放行的是**尚未记账**的更正凭证：它还没进总账，单独作废只是「重做这
/// 一张」，不动净额。
///
/// `unpost` 与 `set_void` 两个入口共用这一份守卫 —— 两处各写一份必然分叉，
/// 而分叉的后果是「作废挡住了但反记账没挡住」，等于没守卫。
fn guard_amend_set(
    conn: &rusqlite::Connection,
    id: i64,
    status: VoucherStatus,
    action: &str,
) -> DbResult<()> {
    let (by, from): (i64, i64) = conn.query_row(
        "SELECT amended_by, amends_id FROM voucher WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    // 未记账的不在总账里，动它不改变净额
    if status != VoucherStatus::Posted {
        return Ok(());
    }
    if by > 0 {
        return Err(FinError::state(format!(
            "本凭证已被 #{by} 红冲更正，不能{action} —— 账上三张凭证（原 + 红冲 + 更正）必须成套存在，\
             少任何一张净额就错。要重做请作废那张尚未记账的更正凭证后重新录入"
        ))
        .into());
    }
    if from > 0 {
        return Err(FinError::state(format!(
            "本凭证是 #{from} 的冲销/更正凭证，不能单独{action} —— 它与 #{from} 必须成套存在，少一张净额就错"
        ))
        .into());
    }
    Ok(())
}

pub fn unpost(db: &Db, id: i64) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    let closed: Option<i32> = tx.query_row(
        "SELECT MAX(period) FROM period_state WHERE closed=1",
        [],
        |r| r.get(0),
    )?;
    fincore::engine::validate_unpost(&v, closed.map(Period::from_ymm)).into_result()?;
    guard_amend_set(&tx, id, v.status, "反记账")?;
    // 反记账回到审核前的状态：有审核记录的回「已审核」，否则回「未记账」
    let back = if v.audited_by.is_some() {
        "audited"
    } else {
        "draft"
    };
    tx.execute(
        "UPDATE voucher SET status=?3, posted_by=NULL, updated_at=?2 WHERE id=?1",
        rusqlite::params![
            id,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            back
        ],
    )?;
    crate::log_on(
        &tx,
        v.posted_by.as_deref().unwrap_or_default(),
        "凭证",
        "反记账",
        &v.voucher_no(),
    )?;
    tx.commit()?;
    Ok(())
}

/// 审核（未记账 → 已审核）。幂等：已审核时直接返回成功。
pub fn audit(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if v.status == VoucherStatus::Audited {
        return Ok(());
    }
    if v.status != VoucherStatus::Draft {
        return Err(FinError::state(format!("状态为「{}」，不能审核", v.status.label())).into());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE voucher SET status='audited', audited_by=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, who, now],
    )?;
    crate::log_on(&tx, who, "凭证", "审核", &v.voucher_no())?;
    tx.commit()?;
    Ok(())
}

/// 反审核（已审核 → 未记账）
pub fn unaudit(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if v.status != VoucherStatus::Audited {
        return Err(FinError::state("只有已审核凭证才能反审核").into());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE voucher SET status='draft', audited_by=NULL, updated_at=?2 WHERE id=?1",
        rusqlite::params![id, now],
    )?;
    crate::log_on(&tx, who, "凭证", "反审核", &v.voucher_no())?;
    tx.commit()?;
    Ok(())
}

/// 出纳签字（未记账凭证记录签字人）。幂等：已签字直接返回成功（保留首位签字人，改签先取消）。
///
/// 与 `BookOptions::require_cashier` 配合：账套开启后，涉及现金/银行科目的凭证
/// 须先经出纳签字才能记账（把关在 `post_tx`）。
pub fn sign(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    match v.status {
        VoucherStatus::Draft | VoucherStatus::Audited => {}
        VoucherStatus::Posted => {
            return Err(FinError::state("已记账凭证不能签字，请先反记账").into())
        }
        VoucherStatus::Void => return Err(FinError::state("已作废凭证不能签字").into()),
    }
    if v.cashier.is_some() {
        return Ok(());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE voucher SET cashier=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, who, now],
    )?;
    crate::log_on(&tx, who, "凭证", "出纳签字", &v.voucher_no())?;
    tx.commit()?;
    Ok(())
}

/// 取消出纳签字（未记账凭证清空签字人）。幂等：未签字直接返回成功。
pub fn unsign(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    match v.status {
        VoucherStatus::Draft | VoucherStatus::Audited => {}
        VoucherStatus::Posted => {
            return Err(FinError::state("已记账凭证不能取消签字，请先反记账").into())
        }
        VoucherStatus::Void => return Err(FinError::state("已作废凭证不能取消签字").into()),
    }
    if v.cashier.is_none() {
        return Ok(());
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    tx.execute(
        "UPDATE voucher SET cashier=NULL, updated_at=?2 WHERE id=?1",
        rusqlite::params![id, now],
    )?;
    crate::log_on(&tx, who, "凭证", "取消签字", &v.voucher_no())?;
    tx.commit()?;
    Ok(())
}

/// 作废 / 恢复。作废与取消作废都会记入操作日志。
pub fn set_void(db: &Db, id: i64, void: bool, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    // 已结账期间的凭证不能作废/恢复，否则已封账期间的报表口径会被改写
    let closed: Option<i32> = tx.query_row(
        "SELECT MAX(period) FROM period_state WHERE closed=1",
        [],
        |r| r.get(0),
    )?;
    if let Some(upto) = closed {
        if v.period.ymm() <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能作废或恢复凭证",
                Period::from_ymm(upto).label()
            ))
            .into());
        }
    }
    if void {
        guard_amend_set(&tx, id, v.status, "作废")?;
        let iss = fincore::engine::validate_void(&v);
        iss.into_result()?;
        set_status_on(&tx, id, VoucherStatus::Void)?;
    } else {
        if v.status != VoucherStatus::Void {
            return Err(FinError::state("该凭证未处于作废状态").into());
        }
        // 已被更正的凭证不能取消作废。
        //
        // 恢复作废后原凭证回到未记账，看着「只是回到未记账」没什么，但**它可以
        // 被重新记账** —— 那时它与红冲、更正凭证同时进总账，一笔钱记两遍，
        // 而链上还挂着更正关系。审计看到「已被更正」，账上却是两笔。
        let by: i64 = tx.query_row("SELECT amended_by FROM voucher WHERE id=?1", [id], |r| {
            r.get(0)
        })?;
        if by > 0 {
            return Err(FinError::state(format!(
                "本凭证已被 #{by} 更正过，不能恢复作废 —— 恢复后它与那张更正凭证会同时进总账。要改请改那张更正凭证"
            ))
            .into());
        }
        let back = if v.posted_by.is_some() {
            VoucherStatus::Posted
        } else if v.audited_by.is_some() {
            VoucherStatus::Audited
        } else {
            VoucherStatus::Draft
        };
        set_status_on(&tx, id, back)?;
    }
    // 状态与日志同事务：崩溃也不会留下"状态已改、无审计"的缺口
    crate::log_on(
        &tx,
        who,
        "凭证",
        if void { "作废" } else { "取消作废" },
        &v.voucher_no(),
    )?;
    tx.commit()?;
    Ok(())
}

/// 批量记账（未记账 → 已记账）
pub fn post_many(db: &Db, ids: &[i64], who: &str) -> DbResult<(usize, Vec<String>)> {
    let mut ok = 0usize;
    let mut errs = Vec::new();
    let mut labels = Vec::new();
    let tx = db.write_tx()?;
    for id in ids {
        match post_tx(&tx, *id, who) {
            Ok(label) => {
                ok += 1;
                labels.push(label);
            }
            Err(e) => errs.push(format!("#{id}：{e}")),
        }
    }
    tx.commit()?;
    if !labels.is_empty() {
        db.log(who, "凭证", "批量记账", &labels.join("、"))?;
    }
    Ok((ok, errs))
}

/// 返回该凭证的凭证号，供调用方写操作日志
fn post_tx(tx: &rusqlite::Transaction, id: i64, who: &str) -> Result<String, DbError> {
    let v: Voucher = tx
        .query_row(
            &format!("SELECT {VOUCHER_COLS} FROM voucher WHERE id=?1"),
            rusqlite::params![id],
            map_voucher,
        )
        .optional()?
        .ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if !v.status.can_post() {
        return Err(FinError::state(format!("状态为「{}」，不能记账", v.status.label())).into());
    }
    // 与单条记账保持同一套把关：借贷必须平衡，期间不能已结账
    if !v.balanced() {
        return Err(FinError::state("凭证借贷不平衡，不能记账").into());
    }
    // 启用审核环节的账套：必须先审核才能记账
    let opts = crate::options_of(tx);
    if opts.enable_audit && v.status != VoucherStatus::Audited {
        return Err(FinError::state("该账套启用了审核环节，请先审核凭证再记账").into());
    }
    // 启用出纳签字前置的账套：涉及现金/银行科目的凭证须出纳签字后才能记账
    if opts.require_cashier && v.cashier.is_none() {
        let touches: Option<i32> = tx
            .query_row(
                "SELECT 1 FROM voucher_entry e JOIN account a ON a.code = e.account_code
                 WHERE e.voucher_id = ?1 AND (a.is_cash = 1 OR a.is_bank = 1)
                 LIMIT 1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        if touches.is_some() {
            return Err(FinError::state(
                "该账套要求出纳签字：涉及现金/银行科目的凭证需出纳先签字再记账",
            )
            .into());
        }
    }
    let closed: Option<i32> = tx.query_row(
        "SELECT MAX(period) FROM period_state WHERE closed=1",
        [],
        |r| r.get(0),
    )?;
    if let Some(upto) = closed {
        if v.period.ymm() <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能记账",
                Period::from_ymm(upto).label()
            ))
            .into());
        }
    }
    tx.execute(
        "UPDATE voucher SET status='posted', posted_by=?2 WHERE id=?1",
        rusqlite::params![id, who],
    )?;
    Ok(v.voucher_no())
}

/// 统计某期间的凭证状态分布
pub fn status_summary(db: &Db, period: Period) -> DbResult<(i64, i64, i64, i64)> {
    let c = |s: &str| -> DbResult<i64> {
        Ok(db.conn().query_row(
            "SELECT COUNT(*) FROM voucher WHERE period=?1 AND status=?2",
            rusqlite::params![period.ymm(), s],
            |r| r.get(0),
        )?)
    };
    Ok((c("draft")?, c("audited")?, c("posted")?, c("void")?))
}

/// 检查期间内凭证号是否连续（断号检查）
pub fn find_gaps(db: &Db, period: Period, word: &str) -> DbResult<Vec<i32>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT no FROM voucher WHERE period=?1 AND word=?2 ORDER BY no")?;
    let nos: Vec<i32> = stmt
        .query_map(rusqlite::params![period.ymm(), word], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut gaps = Vec::new();
    let mut expect = 1;
    for n in nos {
        while expect < n {
            gaps.push(expect);
            expect += 1;
        }
        expect = n + 1;
    }
    Ok(gaps)
}

/// 红字冲销一张凭证：生成借贷互换、摘要加「冲销」前缀的反向凭证并落库。
///
/// 原凭证保留不动（符合审计要求），冲销凭证落在 `period`（通常为当前期），
/// 状态为「未记账」，由用户核对后手动记账。
pub fn reverse(db: &Db, id: i64, who: &str, period: Period, date: NaiveDate) -> DbResult<i64> {
    let v = get(db, id)?.ok_or_else(|| FinError::not_found(format!("凭证 #{id}")))?;
    if v.status == VoucherStatus::Void {
        return Err(FinError::state("已作废凭证无需冲销").into());
    }
    if v.entries.iter().filter(|e| !e.is_blank()).count() == 0 {
        return Err(FinError::state("凭证无有效分录，无法冲销").into());
    }
    let mut r = fincore::engine::reverse_voucher(&v);
    r.period = period;
    r.date = date;
    r.no = next_no(db, period, &v.word)?;
    r.status = VoucherStatus::Draft;
    r.prepared_by = who.to_string();
    r.posted_by = None;
    r.audited_by = None;
    r.cashier = None;
    r.source = VoucherSource::Manual;
    r.memo = format!(
        "红字冲销 {}-{:04}（{}）",
        v.word,
        v.no,
        v.date.format("%Y-%m-%d")
    );
    let nid = save(db, &mut r)?;
    db.log(
        who,
        "凭证",
        "红字冲销",
        &format!("{}-{:04} → 冲销凭证 {}-{:04}", v.word, v.no, v.word, r.no),
    )?;
    Ok(nid)
}

/// 把某期间内同凭证字的凭证号重排为连续（按 日期 + 原凭证号 升序）。
///
/// 用于删除后消除断号。返回重排的凭证张数。
pub fn renumber(db: &Db, period: Period, word: &str) -> DbResult<usize> {
    let rows: Vec<(i64, NaiveDate, i32)> = {
        let mut stmt = db.conn().prepare(
            "SELECT id,date,no FROM voucher WHERE period=?1 AND word=?2 ORDER BY date,no",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![period.ymm(), word], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i32>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, ds, no)| {
                let d = NaiveDate::parse_from_str(&ds, "%Y-%m-%d").unwrap_or_else(|_| {
                    NaiveDate::from_ymd_opt(1970, 1, 1).expect("基准日期必然合法")
                });
                (id, d, no)
            })
            .collect()
    };
    if rows.is_empty() {
        return Ok(0);
    }
    let closed = crate::periods::closed_upto(db)?;
    if let Some(upto) = closed {
        if period <= upto {
            return Err(FinError::state(format!(
                "{} 及以前期间已结账，不能重排凭证号",
                upto.label()
            ))
            .into());
        }
    }
    let tx = db.write_tx()?;
    let mut stmt = tx.prepare("UPDATE voucher SET no=?2 WHERE id=?1")?;
    // 两阶段重排：日期序与原凭证号序可能不一致，直接按目标号更新会撞到
    // 尚未让位的号（UNIQUE(period,word,no)）。先写成互不冲突的临时负数，
    // 再写成 1..n，整批最多两次 UPDATE。
    let mut n = 0i32;
    for (id, _d, _no) in &rows {
        n += 1;
        stmt.execute(rusqlite::params![id, -n])?;
    }
    let mut n = 0i32;
    for (id, _d, _no) in &rows {
        n += 1;
        stmt.execute(rusqlite::params![id, n])?;
    }
    drop(stmt);
    tx.commit()?;
    db.log(
        "系统",
        "凭证",
        "重排凭证号",
        &format!("{}-{} 共 {} 张", period.label(), word, rows.len()),
    )?;
    Ok(rows.len())
}

/// 按科目汇总某期间的分录（用于多栏账、现金流量表等）
///
/// 聚合在 Rust 侧用 `Decimal` 完成：SQLite 没有十进制类型，`SUM(CAST(x AS REAL))`
/// 会引入浮点误差，宁可多传几行数据也不能让账不平。
#[derive(Clone, Debug)]
pub struct AccountSum {
    pub account_code: String,
    pub aux: AuxRef,
    pub debit: Money,
    pub credit: Money,
}

pub fn sum_by_account(
    db: &Db,
    from: Option<Period>,
    to: Option<Period>,
) -> DbResult<Vec<AccountSum>> {
    let (f, t) = (
        from.map(|p| p.ymm()).unwrap_or(0),
        to.map(|p| p.ymm()).unwrap_or(999_999),
    );
    let mut stmt = db.conn().prepare(
        "SELECT e.account_code, e.aux_json, e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE v.status != 'void' AND e.period BETWEEN ?1 AND ?2",
    )?;
    let mut acc: std::collections::BTreeMap<(String, String), (Money, Money, AuxRef)> =
        std::collections::BTreeMap::new();
    let mut rows = stmt.query(rusqlite::params![f, t])?;
    while let Some(r) = rows.next()? {
        let code: String = r.get(0)?;
        let aux_json: String = r.get(1)?;
        let aux: AuxRef = serde_json::from_str(&aux_json).unwrap_or_default();
        let d = read_money(r, 2)?;
        let c = read_money(r, 3)?;
        let e = acc
            .entry((code, aux.key()))
            .or_insert((Money::ZERO, Money::ZERO, aux));
        e.0 += d;
        e.1 += c;
    }
    Ok(acc
        .into_iter()
        .map(|((code, _), (d, c, aux))| AccountSum {
            account_code: code,
            aux,
            debit: d,
            credit: c,
        })
        .collect())
}

/// 某筛选条件下的**全量**借贷合计（不分页）。
///
/// 为什么不给分页端点在 Rust 侧把本页的数字加起来：财务界面里
/// 「本期间借方合计 = 贷方合计」是这个**期间的总账数字**，按页汇总会让人
/// 以为账不平（翻到第 3 页突然合计变了）。所以合计必须走全量。
///
/// 条件复用 `build_where`，与列表/COUNT 同源，不会出现「合计和列表对不上」。
/// 聚合同样在 Rust 侧用 `Decimal` 完成（SQLite 无十进制类型，
/// `SUM(CAST(x AS REAL))` 会引入浮点误差）。
///
/// 作废凭证不计入合计（`status != 'void'`），与账簿口径一致。
pub fn totals(db: &Db, q: &VoucherQuery) -> DbResult<(Money, Money)> {
    let (whered, params) = build_where(q)?;
    // 分录别名故意用 `te` 而不是 `e`：`whered` 里的辅助子查询也起名叫 `e`
    // （EXISTS(SELECT 1 FROM voucher_entry e ...)）。同名会让内层 e 遮蔽外层 e，
    // 虽然 SQLite 允许遮蔽、结果碰巧也对，但读代码的人会误判到底在查哪张表，
    // 而且改 WHERE 时极易踩坏。`te` 与 `v` 都是外层唯一占用的名字。
    let sql = format!(
        "SELECT te.debit, te.credit FROM voucher_entry te
         JOIN voucher v ON te.voucher_id = v.id
         WHERE v.status != 'void' AND 1=1{whered}"
    );
    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(refs.as_slice())?;
    let (mut d, mut c) = (Money::ZERO, Money::ZERO);
    while let Some(r) = rows.next()? {
        d += read_money(r, 0)?;
        c += read_money(r, 1)?;
    }
    Ok((d, c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    // ------------------------------------------------------------------
    // 分页 + 数据范围：SQL 下推与 can_see_voucher 必须逐张一致
    // ------------------------------------------------------------------
    //
    // 为什么这个测试是必需的：分页要显示「共 N 张」并据此算页码。N 来自
    // `list_page` 的 COUNT(*)，而真正返回的行还要过一遍
    // `User::can_see_voucher`。如果 COUNT 的条件比 can_see_voucher 宽
    // （例如漏了科目区间），用户会看到「共 100 张」却只能翻出 60 张，
    // 后面 40 张**通过界面完全访问不到**——分页就成了假分页。
    // 所以这里显式造数据，让两套判定在同一批凭证上跑，把 id 集合拿来对比。

    /// 造一张指定科目组合的凭证（`codes` 里第 0 个借、其余贷，金额自平衡）。
    /// 只用默认科目表里**不需要辅助核算**的末级科目（1001/1901/2001/5xxxxx/6xxxxx），
    /// 否则 `save` 的守卫会以「必须填写银行/客户辅助」拒掉，测的就不是范围而是守卫了。
    fn scope_voucher(db: &Db, day: u32, codes: &[&str]) -> i64 {
        let p = Period::new(2026, 1).unwrap();
        let mut v = Voucher::new(
            p,
            chrono::NaiveDate::from_ymd_opt(2026, 1, day).unwrap(),
            "记",
            0,
        );
        v.no = next_no(db, p, "记").unwrap();
        v.memo = format!("范围测试 {}", day);
        for (i, code) in codes.iter().enumerate() {
            let code: &str = code;
            let mut e = Entry::new(i as i32 + 1, code, code);
            if i == 0 {
                e.debit = Money::parse("120.00").unwrap();
            } else {
                e.credit = Money::parse("120.00").unwrap();
            }
            v.push_entry(e);
        }
        save(db, &mut v).unwrap()
    }

    fn scoped_user(from: &str, to: &str) -> fincore::user::User {
        let mut u = fincore::user::User::default();
        u.username = "tester".into();
        u.display_name = "测试".into();
        u.data_scope.account_from = from.into();
        u.data_scope.account_to = to.into();
        u
    }

    /// 同一批凭证，SQL 下推得到的 id 集合 必须等于 can_see_voucher 过滤后的集合。
    #[test]
    fn list_page_scope_matches_can_see_voucher() {
        let db = mem();
        // 三种典型科目组合：全低（1xxx）/ 全高（6xxx）/ 一低一高（跨界的关键用例）
        let a = scope_voucher(&db, 5, &["1001", "1901"]);
        let b = scope_voucher(&db, 6, &["660201", "600101"]);
        let c = scope_voucher(&db, 7, &["1001", "660201"]);

        for (from, to) in [
            ("", ""),
            ("1001", ""),
            ("", "1901"),
            ("1001", "1001"),
            ("1001", "1901"),
            ("660201", "600101"),
            ("500101", "600101"),
        ] {
            let u = scoped_user(from, to);
            let mut q = VoucherQuery::period(Period::new(2026, 1).unwrap()).with_data_scope(&u);
            q.limit = None;
            let sql_ids: Vec<i64> = list(&db, &q).unwrap().iter().map(|v| v.id).collect();
            // Rust 侧：全量取回后逐张过滤（这是改造前 handler 的做法）。
            // **必须先 fill_entries**：`list` 只读 voucher 表头，`v.entries` 是空的，
            // 而 `can_see_voucher` 是「全部科目都在范围内才可见」——对空分录
            // `all()` 恒真，于是会把什么都判成可见。生产路径（handler）也是
            // 先 fill_entries 再 retain，所以这里必须复刻同一顺序。
            let mut base = VoucherQuery::period(Period::new(2026, 1).unwrap());
            base.limit = None;
            let mut all = list(&db, &base).unwrap();
            fill_entries(&db, &mut all).unwrap();
            let rust_ids: Vec<i64> = all
                .iter()
                .filter(|v| u.can_see_voucher(v))
                .map(|v| v.id)
                .collect();
            assert_eq!(
                sql_ids, rust_ids,
                "范围 [{from}..{to}] 下 SQL 下推与 can_see_voucher 不一致（a={a} b={b} c={c}）"
            );
            // 分页的 COUNT 也必须等于过滤后的真实张数
            let (_, total) = list_page(&db, &q, 0, 10).unwrap();
            assert_eq!(
                total as usize,
                rust_ids.len(),
                "范围 [{from}..{to}] 下 COUNT(*) 与 can_see_voucher 过滤后的张数不一致"
            );
        }
    }

    /// 造一张带部门标签的凭证：`rows` 为 (科目, 部门)，部门 `None` = 该分录不打标签。
    /// 科目仍用不需要辅助核算的末级科目，避免撞上保存守卫。
    fn dept_voucher(db: &Db, day: u32, rows: &[(&str, Option<&str>)]) -> i64 {
        let p = Period::new(2026, 1).unwrap();
        let mut v = Voucher::new(
            p,
            chrono::NaiveDate::from_ymd_opt(2026, 1, day).unwrap(),
            "记",
            0,
        );
        v.no = next_no(db, p, "记").unwrap();
        v.memo = format!("部门范围测试 {}", day);
        for (i, (code, dept)) in rows.iter().enumerate() {
            let mut e = Entry::new(i as i32 + 1, *code, *code);
            e.aux.dept = dept.map(|s| s.to_string());
            if i == 0 {
                e.debit = Money::parse("120.00").unwrap();
            } else {
                e.credit = Money::parse("120.00").unwrap();
            }
            v.push_entry(e);
        }
        save(db, &mut v).unwrap()
    }

    /// 部门范围下推到 SQL 后必须与 `can_see_voucher` 同口径：没打部门标签的分录
    /// 不算越界，打了标签的分录必须落在允许列表内。
    ///
    /// 和 `list_page_scope_matches_can_see_voucher` 同一套路（SQL 集合 == Rust 集合），
    /// 额外再对一份**显式期望**：只比对两侧无法发现"两边一起错"的情况。
    #[test]
    fn list_page_dept_scope_matches_can_see_voucher() {
        let db = mem();
        let d01 = dept_voucher(&db, 5, &[("660201", Some("D01")), ("1001", Some("D01"))]);
        let d02 = dept_voucher(&db, 6, &[("660201", Some("D02")), ("1001", Some("D02"))]);
        let untagged = dept_voucher(&db, 7, &[("660201", None), ("1001", None)]);
        let mixed_ok = dept_voucher(&db, 8, &[("660201", Some("D01")), ("1001", None)]);
        let mixed_bad = dept_voucher(&db, 9, &[("660201", None), ("1001", Some("D02"))]);

        let cases: Vec<(Vec<&str>, Vec<i64>)> = vec![
            (vec![], vec![d01, d02, untagged, mixed_ok, mixed_bad]),
            (vec!["D01"], vec![d01, untagged, mixed_ok]),
            (vec!["D02"], vec![d02, untagged, mixed_bad]),
            (
                vec!["D01", "D02"],
                vec![d01, d02, untagged, mixed_ok, mixed_bad],
            ),
            (vec!["D99"], vec![untagged]),
        ];
        for (depts, want) in cases {
            let mut u = fincore::user::User::default();
            u.username = "tester".into();
            u.display_name = "测试".into();
            u.data_scope.depts = depts.iter().map(|s| s.to_string()).collect();
            let mut q = VoucherQuery::period(Period::new(2026, 1).unwrap()).with_data_scope(&u);
            q.limit = None;
            let mut sql_ids: Vec<i64> = list(&db, &q).unwrap().iter().map(|v| v.id).collect();
            sql_ids.sort();
            // Rust 侧：必须先 fill_entries 再过滤（空 entries 时 all() 恒真）
            let mut base = VoucherQuery::period(Period::new(2026, 1).unwrap());
            base.limit = None;
            let mut all = list(&db, &base).unwrap();
            fill_entries(&db, &mut all).unwrap();
            let mut rust_ids: Vec<i64> = all
                .iter()
                .filter(|v| u.can_see_voucher(v))
                .map(|v| v.id)
                .collect();
            rust_ids.sort();
            assert_eq!(
                sql_ids, want,
                "部门范围 {depts:?} 下 SQL 下推结果与显式期望不符"
            );
            assert_eq!(
                sql_ids, rust_ids,
                "部门范围 {depts:?} 下 SQL 下推与 can_see_voucher 不一致"
            );
            // 分页的 COUNT 必须等于过滤后的真实张数，否则 total 会算错页码
            let (_, total) = list_page(&db, &q, 0, 10).unwrap();
            assert_eq!(
                total as usize,
                want.len(),
                "部门范围 {depts:?} 下 COUNT(*) 与过滤后的张数不一致"
            );
        }
    }

    /// 分页翻完每一页，去重后的 id 集合 == 全量集合，且不重不漏。
    /// 这是「末页能打开、第 N 页不丢行」的直接证据。
    #[test]
    fn list_page_walks_every_row_exactly_once() {
        let db = mem();
        let total = 23;
        for d in 1..=total {
            scope_voucher(&db, (d as u32 % 28) + 1, &["1001", "1901"]);
        }
        let mut q = VoucherQuery::period(Period::new(2026, 1).unwrap());
        q.limit = None;
        let all: Vec<i64> = list(&db, &q).unwrap().iter().map(|v| v.id).collect();
        assert_eq!(all.len(), total, "造数本身就该是 {total} 张");

        let mut seen: Vec<i64> = Vec::new();
        for (page, page_size) in [5usize, 5, 5, 5, 5].iter().enumerate() {
            let (rows, cnt) =
                list_page(&db, &q, (page * page_size) as i64, *page_size as i64).unwrap();
            assert_eq!(cnt as usize, total, "每一页的 COUNT 都得是总张数");
            seen.extend(rows.iter().map(|v| v.id));
        }
        // 越界的那一页必须为空而不是报错或回绕
        let (rows, _) = list_page(&db, &q, 9999, 5).unwrap();
        assert!(rows.is_empty(), "越界页应为空");
        seen.sort();
        assert_eq!(seen, all, "分页走完应恰好覆盖全部行且不重复");
    }

    /// 全量合计必须等于「每张凭证的借贷合计之和」，且**不受分页参数影响**。
    /// 会计核对时看的是本期间的总数；按页汇总会让人以为账不平。
    #[test]
    fn totals_is_whole_set_not_page() {
        let db = mem();
        for d in 1..=9u32 {
            scope_voucher(&db, d, &["1001", "1901"]); // 借 120 / 贷 120
        }
        let mut q = VoucherQuery::period(Period::new(2026, 1).unwrap());
        let (d_all, c_all) = totals(&db, &q).unwrap();
        assert_eq!(d_all, Money::parse("1080.00").unwrap());
        assert_eq!(c_all, Money::parse("1080.00").unwrap());

        // 一页只要 4 张时，合计仍是 9 张的全量（不是 4 张的）
        // 注意 list_page 和 list 一样**只读表头**，借贷合计要靠 fill_entries 填
        // （生产 handler 也是这个顺序）。不填的话每张的 debit_total 都是 0，
        // 测试就会变成「0 累加等于 1080」这种假失败。
        let (mut rows, total) = list_page(&db, &q, 0, 4).unwrap();
        fill_entries(&db, &mut rows).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(total, 9);
        let page_sum: Money = rows.iter().map(|v| v.debit_total()).sum();
        assert_ne!(page_sum, d_all, "本测试要成立：本页合计 ≠ 全量合计");
        // 各页累加 = 全量
        let mut acc = Money::ZERO;
        for off in [0i64, 4, 8] {
            let (mut r, _) = list_page(&db, &q, off, 4).unwrap();
            fill_entries(&db, &mut r).unwrap();
            acc += r.iter().map(|v| v.debit_total()).sum::<Money>();
        }
        assert_eq!(acc, d_all, "各页借方合计累加应等于全量");
        // 条件收窄后合计要跟着变（证明合计与列表同源，不是写死的）
        q.status = Some(VoucherStatus::Void);
        let (dv, cv) = totals(&db, &q).unwrap();
        assert_eq!(
            (dv, cv),
            (Money::ZERO, Money::ZERO),
            "没有作废凭证时合计应为 0"
        );
    }

    // ------------------------------------------------------------------
    // 单据更正链（Cancel -> Amend）
    // ------------------------------------------------------------------

    fn am_voucher(db: &Db, amt: &str) -> i64 {
        let p = Period::new(2026, 1).unwrap();
        let mut v = Voucher::new(
            p,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
            "记",
            0,
        );
        v.no = next_no(db, p, "记").unwrap();
        v.memo = "测试凭证".to_string();
        // 科目选型照同文件既有用例（`voucher_roundtrip` 用的是 1001 / 100201）：
        // 1001 是无辅助的末级；100201 核算银行账户，必须带 bank 辅助。
        // 本用例只验更正链的链接与作废语义，不关心业务含义。
        v.push_entry(Entry {
            debit: Money::parse(amt).unwrap(),
            ..Entry::new(1, "1001", "借 库存现金")
        });
        v.push_entry(Entry {
            credit: Money::parse(amt).unwrap(),
            aux: AuxRef {
                bank: Some("B01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "100201", "贷 银行存款")
        });
        save(db, &mut v).unwrap()
    }

    /// 更正链的完整效果：红冲 + 更正两张，原凭证保持已记账，三张互相认得。
    ///
    /// 形态（对齐中国准则「红字冲销 + 蓝字重做」与 ERPNext 的 Cancel-then-Amend）：
    /// ```text
    ///   原凭证   +1000（保持 posted）
    ///   红冲凭证 -1000（新，draft）
    ///   更正凭证 +1000（新，draft；改成 1200 后净 +1200）
    /// ```
    /// 总账里**看得见**那一笔反向记录 —— 这是与「作废原凭证」的关键差别：
    /// 作废不进总账，账上就看不出曾经记错过。
    #[test]
    fn amend_makes_reversal_plus_rewrite_and_links_all_three() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        assert_eq!(
            get(&db, old).unwrap().unwrap().status,
            VoucherStatus::Posted
        );

        let new_id = amend(&db, old, "金额录错，应为 1200", "boss").unwrap();
        assert!(new_id > 0);

        // 原凭证**保持已记账**（准则形态的关键：账上要留得住那一笔反向记录）
        assert_eq!(
            get(&db, old).unwrap().unwrap().status,
            VoucherStatus::Posted,
            "原凭证必须保持已记账，由红冲凭证抵消；作废会让总账看不出曾经记错过"
        );

        // 链上要拿到两张后续凭证
        let a_old = amend_link(&db, old).unwrap().unwrap();
        let red_id = a_old.amended_by;
        assert!(red_id > 0, "原凭证要标注「被红冲」");
        assert!(
            a_old.amended_by_no.starts_with("记-"),
            "要可读单号：{}",
            a_old.amended_by_no
        );
        assert_eq!(
            a_old.amended_by_new, new_id,
            "同批的更正凭证要单独列出：只列一张等于账上有一张凭证没交代来路"
        );
        assert!(a_old.amended_by_new_no.starts_with("记-"));
        assert_eq!(a_old.reason, "金额录错，应为 1200");

        // 红冲凭证：借贷互换、摘要带「冲销」、草稿待记账
        let red_v = get(&db, red_id).unwrap().unwrap();
        assert_eq!(red_v.status, VoucherStatus::Draft);
        assert!(
            red_v.memo.contains("红冲") && red_v.memo.contains("金额录错"),
            "红冲凭证摘要要说清冲的是哪张、为什么：{}",
            red_v.memo
        );
        let old_ents = entries_of(&db, old).unwrap();
        assert_eq!(red_v.entries.len(), old_ents.len());
        assert_eq!(
            red_v.entries[0].credit, old_ents[0].debit,
            "红冲必须借贷互换，才抵得住原凭证"
        );
        assert!(
            red_v.entries.iter().all(|e| e.summary.contains("冲销")),
            "红冲凭证的每条摘要都要带冲销标记，否则账上看着像重复录入：{:?}",
            red_v.entries.iter().map(|e| &e.summary).collect::<Vec<_>>()
        );

        // 更正凭证：分录原样复制、草稿待记账
        let new_v = get(&db, new_id).unwrap().unwrap();
        assert_eq!(new_v.status, VoucherStatus::Draft);
        assert!(
            new_v.memo.contains("更正") && new_v.memo.contains("金额录错"),
            "摘要要说清是更正哪张、为什么：{}",
            new_v.memo
        );
        assert_eq!(new_v.entries.len(), old_ents.len());
        assert_eq!(
            new_v.entries[0].debit, old_ents[0].debit,
            "分录必须原样复制，否则「更正」就变成了别的业务"
        );
        assert_eq!(new_v.entries[1].credit, old_ents[1].credit);
        let a_new = amend_link(&db, new_id).unwrap().unwrap();
        assert_eq!(a_new.amends_id, old, "新凭证要标注「更正自谁」");

        // 终态：两张都记账后，1001 **净额**仍是 +1000
        // （原 借1000，红冲 贷1000，更正 借1000）
        post(&db, red_id, "boss").unwrap();
        post(&db, new_id, "boss").unwrap();
        let (net, n): (f64, i64) = db
            .conn()
            .query_row(
                "SELECT COALESCE(SUM(CAST(e.debit AS REAL)),0) - COALESCE(SUM(CAST(e.credit AS REAL)),0),
                        COUNT(*)
                 FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
                 WHERE e.account_code='1001' AND v.status='posted'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(
            (net - 1000.0).abs() < 0.01,
            "三张凭证净额应等于原始那一笔 1000，实际 {net}"
        );
        // 账上确实看得见三笔（留痕）—— 这正是与「作废原凭证」的关键差别
        assert_eq!(n, 3, "总账里应看得见三笔（留痕），而不是只剩一笔");
    }

    /// 三张凭证必须成套存在：单独作废或反记账任何一张，净额都会错。
    ///
    /// 这是红冲形态引入的**新**风险：早先「作废原凭证 + 替换」只有两张且原凭证
    /// 早已不在总账，动它不影响净额；改成红冲后原凭证仍在账上，任何一张被单独
    /// 撤出都会出错，而 `unpost` 与 `set_void` 是两个入口 —— 漏掉一个等于没守卫。
    #[test]
    fn amend_set_cannot_pull_one_third_of_the_set_out() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        let new_id = amend(&db, old, "金额录错", "boss").unwrap();
        let red_id = amend_link(&db, old).unwrap().unwrap().amended_by;
        // 两张都记账，进入「成就在总账里」的状态
        post(&db, red_id, "boss").unwrap();
        post(&db, new_id, "boss").unwrap();

        // ① 原凭证：作废 → 凭空少一笔；反记账 → 一笔钱记两遍（都做过一次更正后
        //    再来一次才触发，因为被更正的原凭证本就不该再动）
        for (what, r) in [
            ("作废", set_void(&db, old, true, "boss")),
            ("反记账", unpost(&db, old)),
        ] {
            let e = r.unwrap_err();
            assert!(
                format!("{e:?}").contains("成套存在"),
                "{what} 必须被守卫挡住：{e:?}"
            );
        }
        // ② 红冲凭证：作废 → 只剩 原+更正，一笔钱记两遍
        let e = set_void(&db, red_id, true, "boss").unwrap_err();
        assert!(
            format!("{e:?}").contains("成套存在"),
            "红冲凭证不能单独作废：{e:?}"
        );
        let e = unpost(&db, red_id).unwrap_err();
        assert!(
            format!("{e:?}").contains("成套存在"),
            "红冲凭证不能单独反记账：{e:?}"
        );
        // ③ 更正凭证同样
        let e = set_void(&db, new_id, true, "boss").unwrap_err();
        assert!(
            format!("{e:?}").contains("成套存在"),
            "更正凭证不能单独作废：{e:?}"
        );

        // 守卫失败不该动任何状态
        for id in [old, red_id, new_id] {
            assert_eq!(
                get(&db, id).unwrap().unwrap().status,
                VoucherStatus::Posted,
                "守卫失败不该动凭证 #{id} 的状态"
            );
        }
        // 净额仍是 1000
        let net: f64 = db
            .conn()
            .query_row(
                "SELECT COALESCE(SUM(CAST(e.debit AS REAL)),0) - COALESCE(SUM(CAST(e.credit AS REAL)),0)
                 FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
                 WHERE e.account_code='1001' AND v.status='posted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!((net - 1000.0).abs() < 0.01, "净额应仍为 1000，实际 {net}");
    }

    /// 「重做这一张」是唯一放行的操作：更正凭证还没记账时，把它作废重来。
    ///
    /// 不放行的话，用户改了错的更正凭证就只能再更正一次（凭空多出三张），
    /// 而那张错的草稿还在列表里等着被误记账。
    #[test]
    fn draft_rewrite_can_be_voided_and_reentered() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        let new_id = amend(&db, old, "第一次更正", "boss").unwrap();
        // 草稿状态（未记账）→ 不在总账里，作废它不改变净额
        assert_eq!(
            get(&db, new_id).unwrap().unwrap().status,
            VoucherStatus::Draft
        );
        set_void(&db, new_id, true, "boss").expect("未记账的更正凭证应可作废重来");
        assert_eq!(
            get(&db, new_id).unwrap().unwrap().status,
            VoucherStatus::Void
        );
    }

    /// 操作日志必须与更正**同在一条记录里**：日志写在事务外的话，
    /// 更正已提交、日志还没写就崩溃 —— 账改了却查不到谁改的，
    /// 那正是这套更正链要解决的审计洞。
    #[test]
    fn amend_writes_audit_log_with_the_reason() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        amend(&db, old, "金额录错，应为 1200", "张三").unwrap();
        let (who, action, detail): (String, String, String) = db
            .conn()
            .query_row(
                "SELECT user, action, detail FROM audit_log
                 WHERE module='凭证' AND action='更正' ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(who, "张三", "日志要记是谁改的");
        assert_eq!(action, "更正");
        assert!(
            detail.contains("金额录错") && detail.contains(&format!("#{old}")),
            "日志要带原凭证 id 和更正原因（只记动作不记理由等于没记）：{detail}"
        );
    }

    /// 更正链上的原凭证不能被反记账 —— 那会让一笔钱记两遍。
    ///
    /// 原凭证仍保持已记账（靠红冲抵消），所以对它反记账等于把「抵消项」撤掉：
    /// 总账里剩下 红冲(-1000) + 更正(+1000) = 0，但原凭证那笔业务凭空消失。
    /// 反过来若只撤更正凭证，又会变成 原(+1000) + 红冲(-1000) = 0，业务同样丢了。
    /// 无论撤哪张，正确答案都是「三张一起撤」，所以逐张的入口必须都挡住。
    #[test]
    fn amended_voucher_cannot_be_unposted() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        let new_id = amend(&db, old, "金额录错", "boss").unwrap();
        let red_id = amend_link(&db, old).unwrap().unwrap().amended_by;
        post(&db, red_id, "boss").unwrap();
        post(&db, new_id, "boss").unwrap();

        let e = unpost(&db, old).unwrap_err();
        assert!(
            format!("{e:?}").contains("成套存在"),
            "错误信息要说清是「成套」问题：{e:?}"
        );
        // 被拒后状态不该变
        assert_eq!(
            get(&db, old).unwrap().unwrap().status,
            VoucherStatus::Posted,
            "守卫失败不该动凭证状态"
        );
        // 终态核对：总账里 1001 仍是三笔、净额 1000
        let (n, net): (i64, f64) = db
            .conn()
            .query_row(
                "SELECT COUNT(*),
                        COALESCE(SUM(CAST(e.debit AS REAL)),0) - COALESCE(SUM(CAST(e.credit AS REAL)),0)
                 FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
                 WHERE e.account_code='1001' AND v.status='posted'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(n, 3, "总账里应仍是三笔（留痕）");
        assert!((net - 1000.0).abs() < 0.01, "净额应仍为 1000，实际 {net}");
    }

    /// 未被更正的普通作废凭证仍可恢复（守卫不能误伤既有流程）
    #[test]
    fn void_restore_still_works_without_amend() {
        let db = mem();
        let id = am_voucher(&db, "1000");
        set_void(&db, id, true, "boss").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Void);
        set_void(&db, id, false, "boss").unwrap();
        assert_eq!(
            get(&db, id).unwrap().unwrap().status,
            VoucherStatus::Draft,
            "没被更正过的凭证要能正常恢复，否则守卫误伤了作废/恢复这条老路"
        );
    }

    /// 更正原因必填：没有原因的更正等于没更正
    #[test]
    fn amend_requires_a_reason() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        let e = amend(&db, old, "   ", "boss").unwrap_err();
        assert!(format!("{e:?}").contains("更正原因"), "实际 {:?}", e);
        // 被拒时原凭证必须还是已记账，不能被半途作废
        assert_eq!(
            get(&db, old).unwrap().unwrap().status,
            VoucherStatus::Posted,
            "校验失败不该动原凭证"
        );
    }

    /// 一张凭证只能被更正一次：链分叉后审计就看不出哪条是最终版本
    #[test]
    fn amend_refuses_to_branch_the_chain() {
        let db = mem();
        let old = am_voucher(&db, "1000");
        post(&db, old, "boss").unwrap();
        let new_id = amend(&db, old, "第一次更正", "boss").unwrap();
        let e = amend(&db, old, "再改一次", "boss").unwrap_err();
        assert!(format!("{e:?}").contains("重复更正"), "实际 {:?}", e);
        // 新凭证自己是草稿，所以更正它会被状态拦下
        let e2 = amend(&db, new_id, "改新凭证", "boss").unwrap_err();
        assert!(
            format!("{e2:?}").contains("已记账"),
            "草稿凭证不需要走更正流程：{:?}",
            e2
        );
    }

    /// 草稿凭证不能走更正：它直接改就行，更正只会凭空多一张单
    #[test]
    fn amend_rejects_draft_voucher() {
        let db = mem();
        let id = am_voucher(&db, "1000");
        let e = amend(&db, id, "草稿也要更正", "boss").unwrap_err();
        assert!(format!("{e:?}").contains("已记账"), "实际 {:?}", e);
    }

    /// 审核环节默认**开**（对标金蝶云·星空 + 会计内控底线：制单/审核/记账三权分离）。
    ///
    /// 这个用例是 `crate::tests::mem()` 显式 `enable_audit = false` 的正当性来源：
    /// 夹具为了让别的用例专注各自主题而关掉审核，默认值本身必须有人盯住，否则
    /// 「默认关」和「测试方便」就永远分不清了。
    ///
    /// 四段都要验：
    /// ① `BookOptions::default()` 里**两道闸门**都是开的
    /// ② 默认账套下草稿直接记账被拒，且错误信息指向「先审核」这个下一步
    /// ③ 审核通过后**仍然**被出纳闸门拦住 —— 证明两道闸门是串联的，
    ///    审核不是出纳签字的替代品（反过来也成立：签字不能顶替审核）
    /// ④ 两道都过了才能记账（闸门不是死路）
    #[test]
    fn audit_default_blocks_direct_post() {
        // ① 默认值
        let d = fincore::BookOptions::default();
        assert!(
            d.enable_audit,
            "审核环节必须默认开启：默认关等于「谁录的单谁就能记」，三权分离形同虚设"
        );
        // 注意：assert! 的第 2 个参数是 **token tree** 不是表达式，
        // 所以不能写成 "a" + "b"（rustc 报 expected `,` found `+`）——必须是一个字面量。
        assert!(
            d.require_cashier,
            "出纳签字必须默认开启：现金/银行是内控风险最高的一块，默认关等于新建账套一出生就默认绕过出纳 —— 谁都不必做任何决定，就已经在绕了"
        );

        // ② 默认账套：草稿直接记账被拒
        let o = fincore::BookOptions {
            start_period: Period::new(2026, 1).unwrap(),
            ..Default::default()
        };
        let db = crate::Db::in_memory(&o).unwrap();
        // sample() 已是借贷平衡的完整凭证（借 1001 库存现金 / 贷 100201 银行存款）
        let mut v = sample(Period::new(2026, 1).unwrap(), 10, 1);
        let id = save(&db, &mut v).unwrap();
        let err = post(&db, id, "u").unwrap_err().to_string();
        assert!(err.contains("审核"), "错误信息应指向「先审核」：{err}");

        // ③ 审核过了**仍然**被出纳闸门拦住：sample() 借记 1001 库存现金，
        //    命中 is_cash。
        audit(&db, id, "auditor").unwrap();
        let err2 = post(&db, id, "u").unwrap_err().to_string();
        assert!(
            err2.contains("出纳") || err2.contains("签字"),
            "审核之后应轮到出纳签字这道闸，错误信息要指向它：{err2}"
        );

        // ④ 两道都过 → 记账成功（闸门不是死路）
        sign(&db, id, "出纳小李").unwrap();
        post(&db, id, "u").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Posted);
    }

    fn sample(period: Period, day: u32, no: i32) -> Voucher {
        let d = NaiveDate::from_ymd_opt(period.year(), period.month(), day).unwrap();
        let mut v = Voucher::new(period, d, "记", no);
        v.prepared_by = "张三".to_string();
        v.push_entry(Entry {
            debit: Money::parse("1000").unwrap(),
            ..Entry::new(1, "1001", "提取现金")
        });
        v.push_entry(Entry {
            credit: Money::parse("1000").unwrap(),
            aux: AuxRef {
                // 100201 核算银行账户，必须填银行账户档案
                bank: Some("B01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "100201", "提取现金")
        });
        v
    }

    /// 辅助核算过滤必须按维度**整段**匹配。
    /// 回归：旧实现把整条 aux_key 包成 `%key%` 做子串 LIKE，导致
    /// 按 `customer=C001` 过滤会连带返回 `customer=C0011` 的凭证，
    /// 按 `customer=AC` 过滤会返回 `customer=ACME` 的凭证（财务上的越权读取）。
    #[test]
    fn aux_filter_is_exact_segment_match() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();

        let mk = |cust: &str, no: i32| {
            let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            let mut v = Voucher::new(p, d, "记", no);
            v.prepared_by = "张三".to_string();
            v.push_entry(Entry {
                debit: Money::parse("100").unwrap(),
                aux: AuxRef {
                    customer: Some(cust.into()),
                    ..Default::default()
                },
                ..Entry::new(1, "112201", "应收货款")
            });
            v.push_entry(Entry {
                credit: Money::parse("100").unwrap(),
                ..Entry::new(2, "600101", "产品销售收入")
            });
            let id = save(&db, &mut v).unwrap();
            post(&db, id, "张三").unwrap();
            id
        };
        let id_c001 = mk("C001", 1);
        let id_c0011 = mk("C0011", 2);
        let id_acme = mk("ACME", 3);

        let ids_for = |cust: &str| -> Vec<i64> {
            let mut want = AuxRef::default();
            want.customer = Some(cust.to_string());
            let mut q = VoucherQuery::period(p);
            q.aux = Some(want);
            let mut ids: Vec<i64> = list(&db, &q).unwrap().iter().map(|v| v.id).collect();
            ids.sort();
            ids
        };

        // 对照：不过滤时三张都在
        let all: Vec<i64> = list(&db, &VoucherQuery::period(p))
            .unwrap()
            .iter()
            .map(|v| v.id)
            .collect();
        assert_eq!(all.len(), 3, "对照：不过滤应返回 3 张");

        assert_eq!(ids_for("C001"), vec![id_c001], "C001 不得带上 C0011");
        assert_eq!(ids_for("C0011"), vec![id_c0011]);
        assert_eq!(ids_for("ACME"), vec![id_acme]);
        assert!(ids_for("AC").is_empty(), "AC 是子串不是整段，不应命中 ACME");
        assert!(ids_for("C00").is_empty());
        assert!(ids_for("C002").is_empty(), "不存在的客户不应命中任何凭证");
    }

    #[test]
    fn save_get_delete() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        let id = save(&db, &mut v).unwrap();
        assert!(id > 0);

        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(got.entries.len(), 2);
        assert_eq!(got.entries[0].debit, Money::parse("1000").unwrap());
        assert_eq!(got.entries[1].aux.bank.as_deref(), Some("B01"));
        assert!(got.balanced());

        delete(&db, id).unwrap();
        assert!(get(&db, id).unwrap().is_none());
    }

    /// 回归：删除凭证的「校验 + 清核销 + 删表」必须整体原子。
    ///
    /// 旧实现把校验与清核销都放在事务外、每条 `unsettle_entry` 各自提交：
    /// 3 条分录里第 3 条清核销失败时，前 2 条核销记录已经被提交并消失，凭证却还在，
    /// 账上留下一张「分录挂账、但核销记录只剩半截」的凭证。
    #[test]
    fn delete_rolls_back_unsettle_on_failure() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 待删凭证：两条应收分录（600 / 700），各自与收款凭证的应收分录核销
        let mut v = Voucher::new(p, d, "记", 1);
        v.prepared_by = "张三".to_string();
        for (i, amt) in ["600", "700"].iter().enumerate() {
            v.push_entry(Entry {
                debit: Money::parse(amt).unwrap(),
                aux: AuxRef {
                    customer: Some("C01".into()),
                    ..Default::default()
                },
                ..Entry::new(i as i32 * 2 + 1, "112201", "应收")
            });
            v.push_entry(Entry {
                credit: Money::parse(amt).unwrap(),
                ..Entry::new(i as i32 * 2 + 2, "600101", "收入")
            });
        }
        let vid = save(&db, &mut v).unwrap();
        // 应收凭证必须**已记账**：核销的「被核销方」(from 侧) 要求已记账
        // （settle::settle 守卫，全仓 H-3 口径）
        post(&db, vid, "u").unwrap();
        // 收款凭证：贷应收 600 / 700。保持**草稿**——已记账的凭证删不掉，
        // 而本用例要验证的正是「删掉带核销记录的凭证时清核销的原子性」，
        // 所以删的对象取这张收款凭证（核销的 to 侧，允许是草稿：对标金蝶
        // 「单据审核 → 核销 → 生成凭证」三个并列步骤）
        let mut pay = Voucher::new(p, d, "记", 2);
        pay.prepared_by = "张三".to_string();
        pay.push_entry(Entry {
            debit: Money::parse("1300").unwrap(),
            aux: AuxRef {
                bank: Some("B01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "100201", "收款")
        });
        for (i, amt) in ["600", "700"].iter().enumerate() {
            pay.push_entry(Entry {
                credit: Money::parse(amt).unwrap(),
                aux: AuxRef {
                    customer: Some("C01".into()),
                    ..Default::default()
                },
                ..Entry::new(i as i32 + 2, "112201", "冲应收")
            });
        }
        let pid = save(&db, &mut pay).unwrap();
        let e = entries_of(&db, vid).unwrap();
        let f = entries_of(&db, pid).unwrap();
        crate::settle::settle(&db, e[0].id, f[1].id, Money::parse("600").unwrap(), "u").unwrap();
        crate::settle::settle(&db, e[2].id, f[2].id, Money::parse("700").unwrap(), "u").unwrap();
        let cnt = |db: &Db| -> i64 {
            db.conn()
                .query_row("SELECT COUNT(*) FROM settle_record", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(cnt(&db), 2);

        // 让第 2 条核销记录删不掉：删凭证必须整体失败，且第 1 条核销记录必须还在
        let second: i64 = db
            .conn()
            .query_row(
                "SELECT id FROM settle_record ORDER BY id LIMIT 1 OFFSET 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        db.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER t_block BEFORE DELETE ON settle_record
                 WHEN OLD.id = {second} BEGIN SELECT RAISE(ABORT, '核销记录被占用'); END;"
            ))
            .unwrap();
        assert!(delete(&db, pid).is_err(), "清核销失败必须让删除失败");
        assert_eq!(
            cnt(&db),
            2,
            "第 1 条核销记录不得被提交掉（回归前会只剩 1 条，而凭证还在）"
        );
        assert!(get(&db, pid).unwrap().is_some(), "凭证必须还在");

        // 去掉阻塞后删除应成功，核销记录一并清干净
        db.conn().execute_batch("DROP TRIGGER t_block;").unwrap();
        delete(&db, pid).unwrap();
        assert!(get(&db, pid).unwrap().is_none());
        assert_eq!(cnt(&db), 0);
        // 应收凭证本身不受影响
        assert!(get(&db, vid).unwrap().is_some());
    }

    /// 回归：已记账凭证不能删（`AND status<>'posted'` 守卫）。
    #[test]
    fn delete_rejects_posted_voucher() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        let id = save(&db, &mut v).unwrap();
        post(&db, id, "张三").unwrap();
        assert!(delete(&db, id).is_err(), "已记账凭证不得删除");
        assert!(get(&db, id).unwrap().is_some());
        unpost(&db, id).unwrap();
        delete(&db, id).unwrap();
        assert!(get(&db, id).unwrap().is_none());
    }

    #[test]
    fn blank_lines_dropped() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        v.push_entry(Entry::new(3, "1001", "")); // 空行
        let id = save(&db, &mut v).unwrap();
        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(got.entries.len(), 2, "空分录不应入库");
    }

    #[test]
    fn line_renumbering() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        v.push_entry(Entry {
            debit: Money::parse("500").unwrap(),
            ..Entry::new(3, "660101", "广告费")
        });
        v.push_entry(Entry {
            credit: Money::parse("500").unwrap(),
            ..Entry::new(4, "1001", "广告费")
        });
        let id = save(&db, &mut v).unwrap();
        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(
            got.entries.iter().map(|e| e.line).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn reverse_generates_mirror_voucher() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        v.status = VoucherStatus::Posted;
        v.posted_by = Some("张三".into());
        let id = save(&db, &mut v).unwrap();

        let rid = reverse(
            &db,
            id,
            "李四",
            p,
            NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
        )
        .unwrap();
        let r = get(&db, rid).unwrap().unwrap();
        // 原凭证不动
        let orig = get(&db, id).unwrap().unwrap();
        assert_eq!(orig.status, VoucherStatus::Posted);
        // 冲销凭证：借贷互换、未记账、摘要带「冲销」前缀
        assert_eq!(r.status, VoucherStatus::Draft);
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].credit, Money::parse("1000").unwrap()); // 原借方 → 冲销贷方
        assert!(r.entries.iter().all(|e| e.summary.starts_with("冲销")));
        assert!(r.balanced());
        assert_eq!(r.no, 2, "冲销凭证应取下一个凭证号");
    }

    #[test]
    fn renumber_compacts_gaps() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v1 = sample(p, 5, 1);
        save(&db, &mut v1).unwrap();
        let mut v2 = sample(p, 6, 2);
        save(&db, &mut v2).unwrap();
        let mut v4 = sample(p, 7, 4);
        save(&db, &mut v4).unwrap(); // 断号 3

        assert_eq!(find_gaps(&db, p, "记").unwrap(), vec![3]);
        let n = renumber(&db, p, "记").unwrap();
        assert_eq!(n, 3);
        assert!(find_gaps(&db, p, "记").unwrap().is_empty());
        // 按日期顺序重排后，各张编号 1..=3
        let rows = list(&db, &VoucherQuery::period(p)).unwrap();
        assert_eq!(rows.iter().map(|v| v.no).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn flow_post_unpost() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        let id = save(&db, &mut v).unwrap();

        // 无审核环节：未记账凭证直接记账
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Draft);
        post(&db, id, "张三").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Posted);
        unpost(&db, id).unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Draft);
        // 反记账后再记账
        post(&db, id, "张三").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().status, VoucherStatus::Posted);
    }

    #[test]
    fn next_no_and_gaps() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        assert_eq!(next_no(&db, p, "记").unwrap(), 1);
        let mut v = sample(p, 5, 1);
        save(&db, &mut v).unwrap();
        assert_eq!(next_no(&db, p, "记").unwrap(), 2);
        let mut v3 = sample(p, 6, 3);
        save(&db, &mut v3).unwrap();
        assert_eq!(find_gaps(&db, p, "记").unwrap(), vec![2]);
        assert_eq!(next_no(&db, p, "记").unwrap(), 4);
    }

    #[test]
    fn query_filters() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v1 = sample(p, 5, 1);
        save(&db, &mut v1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        let mut v2 = sample(p2, 5, 1);
        save(&db, &mut v2).unwrap();

        assert_eq!(list(&db, &VoucherQuery::period(p)).unwrap().len(), 1);
        assert_eq!(list(&db, &VoucherQuery::default()).unwrap().len(), 2);
        assert_eq!(
            list(
                &db,
                &VoucherQuery::default().with_status(Some(VoucherStatus::Draft))
            )
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            list(&db, &VoucherQuery::default().with_keyword("提取"))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            list(
                &db,
                &VoucherQuery {
                    account_code: Some("1001".into()),
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            list(
                &db,
                &VoucherQuery {
                    aux: Some(AuxRef {
                        bank: Some("B01".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }
            )
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn status_counts() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = sample(p, 5, 1);
        save(&db, &mut v).unwrap();
        assert_eq!(status_summary(&db, p).unwrap(), (1, 0, 0, 0));
    }

    #[test]
    fn data_scope_own_voucher_only() {
        use fincore::user::DataScope;

        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // prepared_by 存的是登录账号（username），与保存侧一致
        let mut v1 = sample(p, 5, 1);
        v1.prepared_by = "zhangsan".to_string();
        save(&db, &mut v1).unwrap();
        let mut v2 = sample(p, 6, 2);
        v2.prepared_by = "lisi".to_string();
        save(&db, &mut v2).unwrap();

        // zhangsan：只看到自己填制的 1 张（display_name 与 username 不同，仍应按 username 过滤）
        let mut zhang = fincore::User::new("zhangsan", "张三", fincore::Role::Accountant);
        zhang.data_scope = DataScope {
            own_voucher_only: true,
            ..Default::default()
        };
        let rows = list(&db, &VoucherQuery::period(p).with_data_scope(&zhang)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].prepared_by, "zhangsan");

        // 管理员/无限制用户：2 张都可见
        let admin = fincore::User::new("admin", "管理员", fincore::Role::Admin);
        let rows = list(&db, &VoucherQuery::period(p).with_data_scope(&admin)).unwrap();
        assert_eq!(rows.len(), 2);
    }
}
