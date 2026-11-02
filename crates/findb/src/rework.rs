//! 返工件成本单
//!
//! 返工件是「已经做过、要再做一遍」的活。它的材料与工时如果直接记回原生产订单，
//! 原单的单位成本会虚高，标准成本差异与毛利都跟着错 —— 而且谁都说不清
//! 「这批返工到底亏了多少」。所以返工单独开一张单，成本单独归集，
//! 再按「返工成本 ÷ 原单成本」给出占比，车间和老板看的是同一个数。
//!
//! 口径约定：
//! - 返工单**必须挂在一张生产订单上**（来源订单），数量不能超过原单计划量 ——
//!   返工是返原来的活，凭空冒出一批返工件不合理。
//! - 成本三类：`material` 材料 / `labor` 人工 / `overhead` 制造费用，金额一律
//!   `Money`（Decimal），在 Rust 侧加减，绝不写 `SUM(amount)`（会退化成 REAL 浮点）。
//! - 已结案的返工单不许再追加成本：结案意味着这批返工的成本已经认下来，
//!   再往里加钱会让上个月看过的报表变样。
//! - 期间已结账的返工单不许改（与工资表那道闸同一个理由）。
//!
//! 原因与责任：`reason` 是自由文本备注，`reason_code` 指向 `rework_reason` 小字典
//! （没有字典，同一件事会写成「槽液不好」「槽液」「槽液问题」三行，汇不出总额），
//! `dept` 记责任部门/班组。有了这两个字段才能回答「哪种原因最费钱、哪个班组返工最多」。
//!
//! 本模块**不生成凭证**：返工是内部成本归集与差异分析，材料/人工的真实发生额
//! 在领料、报工、工资那边已经入过账，这里再出一次就是重复入账。

use chrono::NaiveDate;
use fincore::{FinError, Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn parse_date(s: &str) -> NaiveDate {
    // 库里存的是 'YYYY-MM-DD'；解析失败回落到 1970-01-01，别让一张脏单据把整张报表打死
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 一定合法"))
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 成本类型：与 `manufacturing::CostType` 的三种归集口径对齐（材料/人工/制造费用）
pub const COST_KINDS: &[(&str, &str)] = &[
    ("material", "材料"),
    ("labor", "人工"),
    ("overhead", "制造费用"),
];

pub fn cost_kind_label(kind: &str) -> &'static str {
    COST_KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, l)| *l)
        .unwrap_or("其它")
}

/// 单据状态：open 进行中（还能追加成本）/ closed 已结案（成本已认下来）
pub const STATUSES: &[(&str, &str)] = &[("open", "进行中"), ("closed", "已结案")];

pub fn status_label(status: &str) -> &'static str {
    STATUSES
        .iter()
        .find(|(k, _)| *k == status)
        .map(|(_, l)| *l)
        .unwrap_or("进行中")
}

/// 未分类/未指定时的展示名（空编码不是错误，是一堆「谁都没填」的单据）
pub const UNCLASSIFIED: &str = "未分类";
pub const NO_DEPT: &str = "未指定";

/// 返工原因分类（小字典）
///
/// 为什么要有字典：`reason` 是自由文本，「槽液不好」「槽液」「槽液问题」会变成三行，
/// 按原因汇总就等于没汇总。字典只做规范化，具体细节仍写在 `reason` 备注里。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Reason {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub disabled: bool,
    pub memo: String,
}

const R_COLS: &str = "id,code,name,disabled,memo";

fn map_reason(r: &rusqlite::Row<'_>) -> rusqlite::Result<Reason> {
    Ok(Reason {
        id: r.get(0)?,
        code: r.get(1)?,
        name: r.get(2)?,
        disabled: r.get::<_, i64>(3)? != 0,
        memo: r.get(4)?,
    })
}

/// 原因分类列表（停用的也返回，界面要能看见并重新启用）
pub fn reason_list(db: &Db) -> DbResult<Vec<Reason>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {R_COLS} FROM rework_reason ORDER BY disabled, code"
    ))?;
    let rows = st.query_map([], map_reason)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 编码 → 名称；报表里把编码翻成中文
fn reason_names(db: &Db) -> DbResult<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for r in reason_list(db)? {
        map.insert(r.code, r.name);
    }
    Ok(map)
}

fn reason_name(code: &str, names: &std::collections::HashMap<String, String>) -> String {
    let code = code.trim();
    if code.is_empty() {
        return UNCLASSIFIED.to_string();
    }
    names
        .get(code)
        .cloned()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| code.to_string())
}

/// 保存原因分类（code 是主键语义，改名不换码 —— 换码会让历史单据变成「未分类」）
pub fn reason_save(db: &Db, r: &mut Reason) -> DbResult<i64> {
    let code = r.code.trim().to_uppercase();
    let name = r.name.trim();
    if code.is_empty() {
        return Err(invalid("原因分类编码必填"));
    }
    if name.is_empty() {
        return Err(invalid("原因分类名称必填"));
    }
    let tx = db.write_tx()?;
    let id = if r.id > 0 {
        tx.execute(
            "UPDATE rework_reason SET code=?2,name=?3,disabled=?4,memo=?5 WHERE id=?1",
            rusqlite::params![r.id, code, name, r.disabled as i64, r.memo.trim()],
        )?;
        r.id
    } else {
        let hit: Option<i64> = tx
            .query_row(
                "SELECT id FROM rework_reason WHERE code=?1",
                rusqlite::params![code],
                |x| x.get(0),
            )
            .optional()?;
        if let Some(id) = hit {
            // 编码唯一：重复新增当成改名，别让用户对着「约束冲突」发呆
            tx.execute(
                "UPDATE rework_reason SET name=?2,disabled=?3,memo=?4 WHERE id=?1",
                rusqlite::params![id, name, r.disabled as i64, r.memo.trim()],
            )?;
            id
        } else {
            tx.execute(
                "INSERT INTO rework_reason(code,name,disabled,memo) VALUES(?1,?2,?3,?4)",
                rusqlite::params![code, name, r.disabled as i64, r.memo.trim()],
            )?;
            tx.last_insert_rowid()
        }
    };
    tx.commit()?;
    r.id = id;
    r.code = code;
    Ok(id)
}

/// 删除原因分类：已经被返工单引用的不许删（删了历史单据就成「未分类」，报表口径会变）
pub fn reason_delete(db: &Db, id: i64) -> DbResult<()> {
    let code: String = db
        .conn()
        .query_row(
            "SELECT code FROM rework_reason WHERE id=?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| invalid(format!("原因分类 {id} 不存在")))?;
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM rework_order WHERE reason_code=?1",
        rusqlite::params![code],
        |r| r.get(0),
    )?;
    if n > 0 {
        return Err(blocked(format!(
            "有 {n} 张返工单用了这个原因分类，删掉它们会变成「未分类」：先改单，或改成停用"
        )));
    }
    let tx = db.write_tx()?;
    tx.execute(
        "DELETE FROM rework_reason WHERE id=?1",
        rusqlite::params![id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 分组汇总的一行（按原因 / 按部门共用）
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Bucket {
    /// 分组键（原因编码或部门名；空 = 未分类/未指定）
    pub key: String,
    pub label: String,
    /// 返工单张数
    pub count: i64,
    /// 返工件数合计
    pub qty: Money,
    pub material: Money,
    pub labor: Money,
    pub overhead: Money,
    pub total: Money,
    /// 占本期返工成本合计的比例（%）；合计为 0 时是空串
    pub share: String,
}

/// 返工成本的分组分析：按原因 + 按部门
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Analysis {
    pub from: Period,
    pub to: Period,
    pub by_reason: Vec<Bucket>,
    pub by_dept: Vec<Bucket>,
    /// 本期返工成本合计（两个维度分组的分子分母都以它为基准）
    pub total: Money,
    pub count: i64,
}

#[derive(Clone, Debug, Default)]
struct Acc {
    count: i64,
    qty: Money,
    material: Money,
    labor: Money,
    overhead: Money,
    total: Money,
}

impl Acc {
    fn add(&mut self, s: &ReworkStat) {
        self.count += 1;
        self.qty += s.qty;
        self.material += s.material;
        self.labor += s.labor;
        self.overhead += s.overhead;
        self.total += s.total;
    }
}

fn bucket_of(key: String, label: String, acc: Acc, grand: Money) -> Bucket {
    // 分母用「本期返工成本合计」，不是行内金额：0 时给空串而不是 0%
    let share = if grand.is_zero() {
        String::new()
    } else {
        (acc.total * 100)
            .checked_div(grand)
            .unwrap_or(Money::ZERO)
            .round2()
            .fmt_plain()
    };
    Bucket {
        key,
        label,
        count: acc.count,
        qty: acc.qty,
        material: acc.material,
        labor: acc.labor,
        overhead: acc.overhead,
        total: acc.total,
        share,
    }
}

/// 按原因 / 按部门汇总返工成本，金额大的排前面
///
/// 空编码/空部门不丢：它们是「谁都没填」的单据，丢掉会让各组合计不等于总合计，
/// 看上去像算错了。所以单独成一桶「未分类 / 未指定」。
pub fn analyze(db: &Db, from: Period, to: Period) -> DbResult<Analysis> {
    let stats = report(db, from, to)?;
    let mut grand = Money::ZERO;
    let mut count = 0i64;
    let mut by_r: std::collections::BTreeMap<String, (String, Acc)> =
        std::collections::BTreeMap::new();
    let mut by_d: std::collections::BTreeMap<String, Acc> = std::collections::BTreeMap::new();
    for s in &stats {
        grand += s.total;
        count += 1;
        let rk = s.reason_code.trim().to_string();
        let entry = by_r
            .entry(rk.clone())
            .or_insert_with(|| (s.reason_label.clone(), Acc::default()));
        entry.1.add(s);
        let dk = s.dept.trim().to_string();
        by_d.entry(dk).or_default().add(s);
    }
    let mut by_reason: Vec<Bucket> = by_r
        .into_iter()
        .map(|(k, (label, acc))| bucket_of(k, label, acc, grand))
        .collect();
    let mut by_dept: Vec<Bucket> = by_d
        .into_iter()
        .map(|(k, acc)| {
            let label = if k.trim().is_empty() {
                NO_DEPT.to_string()
            } else {
                k.clone()
            };
            bucket_of(k, label, acc, grand)
        })
        .collect();
    // 金额大的在前；同额按编码/名称排，保证输出稳定（测试才写得死）
    by_reason.sort_by(|a, b| b.total.cmp(&a.total).then(a.key.cmp(&b.key)));
    by_dept.sort_by(|a, b| b.total.cmp(&a.total).then(a.key.cmp(&b.key)));
    Ok(Analysis {
        from,
        to,
        by_reason,
        by_dept,
        total: grand,
        count,
    })
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ReworkOrder {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    /// 原生产订单
    pub po_id: i64,
    pub po_no: String,
    pub item_code: String,
    pub item_name: String,
    pub qty: Money,
    /// 原因备注（自由文本）
    pub reason: String,
    /// 原因分类（指向 `rework_reason.code`，空 = 未分类）
    pub reason_code: String,
    /// 责任部门 / 班组（空 = 未指定）
    pub dept: String,
    pub status: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
}

impl ReworkOrder {
    pub fn new(po_id: i64, date: NaiveDate) -> Self {
        ReworkOrder {
            period: Period::from_date(date),
            date,
            po_id,
            qty: Money::ZERO,
            status: "open".to_string(),
            ..Default::default()
        }
    }

    pub fn status_label(&self) -> &'static str {
        status_label(&self.status)
    }

    pub fn is_closed(&self) -> bool {
        self.status == "closed"
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ReworkCost {
    pub id: i64,
    pub rw_id: i64,
    pub cost_type: String,
    pub amount: Money,
    pub memo: String,
    pub created_at: String,
}

impl ReworkCost {
    pub fn kind_label(&self) -> &'static str {
        cost_kind_label(&self.cost_type)
    }
}

/// 返工单 + 成本明细（一次取回，界面不用再查一趟）
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ReworkDetail {
    pub order: ReworkOrder,
    pub costs: Vec<ReworkCost>,
    pub material: Money,
    pub labor: Money,
    pub overhead: Money,
    pub total: Money,
}

/// 返工成本分析行：按返工单聚合，并给出与原单成本的占比
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ReworkStat {
    pub rw_id: i64,
    pub no: String,
    pub date: String,
    pub po_no: String,
    pub item_code: String,
    pub item_name: String,
    pub qty: Money,
    pub reason: String,
    /// 原因分类编码（空 = 未分类）
    pub reason_code: String,
    /// 原因分类名称（字典里的中文名；未分类时是「未分类」）
    pub reason_label: String,
    /// 责任部门 / 班组（空 = 未指定）
    pub dept: String,
    pub status: String,
    pub material: Money,
    pub labor: Money,
    pub overhead: Money,
    pub total: Money,
    /// 原单已归集成本（材料+人工+制造费用）
    pub po_cost: Money,
    /// 返工成本占原单成本的比例（%）；原单没有成本时为空串
    pub ratio: String,
}

const O_COLS: &str = "id,no,period,date,po_id,po_no,item_code,item_name,qty,reason,reason_code,dept,status,memo,created_by,created_at";
const C_COLS: &str = "id,rw_id,cost_type,amount,memo,created_at";

fn map_order(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReworkOrder> {
    Ok(ReworkOrder {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: parse_date(&r.get::<_, String>(3)?),
        po_id: r.get(4)?,
        po_no: r.get(5)?,
        item_code: r.get(6)?,
        item_name: r.get(7)?,
        qty: crate::read_money(r, 8)?,
        reason: r.get(9)?,
        reason_code: r.get(10)?,
        dept: r.get(11)?,
        status: r.get(12)?,
        memo: r.get(13)?,
        created_by: r.get(14)?,
        created_at: r.get(15)?,
    })
}

fn map_cost(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReworkCost> {
    Ok(ReworkCost {
        id: r.get(0)?,
        rw_id: r.get(1)?,
        cost_type: r.get(2)?,
        amount: crate::read_money(r, 3)?,
        memo: r.get(4)?,
        created_at: r.get(5)?,
    })
}

/// 单号：RG-<期间>-<序号>，与生产订单（SC）、客供料（LR/JH）同一套习惯
fn next_no(tx: &rusqlite::Transaction<'_>, period: Period) -> DbResult<String> {
    // 用 ymm（202601）而不是 label()（2026年01期）：单号要能被 LIKE 前缀稳定匹配
    let prefix = format!("RG-{}", period.ymm());
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM rework_order WHERE no LIKE ?1 || '-%'",
        rusqlite::params![prefix],
        |r| r.get(0),
    )?;
    let mut seq = n + 1;
    loop {
        let no = format!("{prefix}-{seq:03}");
        let hit: Option<i64> = tx
            .query_row(
                "SELECT id FROM rework_order WHERE no=?1",
                rusqlite::params![no],
                |r| r.get(0),
            )
            .optional()?;
        if hit.is_none() {
            return Ok(no);
        }
        seq += 1;
    }
}

/// 期间已结账就不许再动（与工资表那道闸同一个理由：凭证已出，改了就与账不符）
fn guard_open_period(db: &Db, period: Period) -> DbResult<()> {
    if let Some(upto) = crate::periods::closed_upto_of(db.conn())? {
        if period <= upto {
            return Err(blocked(format!(
                "{} 及以前期间已结账，返工成本单不能再改",
                upto.label()
            )));
        }
    }
    Ok(())
}

fn blocked(msg: impl Into<String>) -> crate::DbError {
    crate::DbError::Fin(FinError::state(msg))
}

fn invalid(msg: impl Into<String>) -> crate::DbError {
    crate::DbError::Fin(FinError::validate(msg))
}

pub fn list(db: &Db, period: Option<Period>) -> DbResult<Vec<ReworkOrder>> {
    let mut out = Vec::new();
    match period {
        Some(p) => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {O_COLS} FROM rework_order WHERE period=?1 ORDER BY date DESC, id DESC"
            ))?;
            let rows = st.query_map(rusqlite::params![p.ymm()], map_order)?;
            for r in rows {
                out.push(r?);
            }
        }
        None => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {O_COLS} FROM rework_order ORDER BY date DESC, id DESC"
            ))?;
            let rows = st.query_map([], map_order)?;
            for r in rows {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

pub fn get(db: &Db, id: i64) -> DbResult<Option<ReworkOrder>> {
    Ok(db
        .conn()
        .query_row(
            &format!("SELECT {O_COLS} FROM rework_order WHERE id=?1"),
            rusqlite::params![id],
            map_order,
        )
        .optional()?)
}

pub fn costs(db: &Db, rw_id: i64) -> DbResult<Vec<ReworkCost>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {C_COLS} FROM rework_cost WHERE rw_id=?1 ORDER BY id"
    ))?;
    let rows = st.query_map(rusqlite::params![rw_id], map_cost)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 返工单明细：单据 + 分项成本 + 合计
pub fn detail(db: &Db, id: i64) -> DbResult<Option<ReworkDetail>> {
    let Some(order) = get(db, id)? else {
        return Ok(None);
    };
    let costs = costs(db, id)?;
    let mut d = ReworkDetail {
        order,
        costs,
        ..Default::default()
    };
    for c in &d.costs {
        match c.cost_type.as_str() {
            "material" => d.material += c.amount,
            "labor" => d.labor += c.amount,
            _ => d.overhead += c.amount,
        }
    }
    d.total = d.material + d.labor + d.overhead;
    Ok(Some(d))
}

pub fn total(db: &Db, rw_id: i64) -> DbResult<Money> {
    Ok(detail(db, rw_id)?.map(|d| d.total).unwrap_or(Money::ZERO))
}

/// 保存返工单（新建或改原单/数量/原因/状态）
pub fn save(db: &Db, o: &mut ReworkOrder, who: &str) -> DbResult<i64> {
    if o.po_id <= 0 {
        return Err(invalid("返工单必须选一张来源生产订单"));
    }
    let po = crate::scm::prod_get(db, o.po_id)?
        .ok_or_else(|| invalid(format!("生产订单 {} 不存在", o.po_id)))?;
    if !o.qty.is_positive() {
        return Err(invalid("返工数量必须大于 0"));
    }
    // 返工是返原来的活：超过原单计划量的返工件没有出处
    if o.qty > po.planned_qty {
        return Err(invalid(format!(
            "返工数量 {} 超过原单计划量 {}",
            o.qty.fmt_qty(),
            po.planned_qty.fmt_qty()
        )));
    }
    if !STATUSES.iter().any(|(k, _)| *k == o.status) {
        return Err(invalid(format!("返工单状态不认识：{}", o.status)));
    }
    if o.id > 0 {
        let old = get(db, o.id)?.ok_or_else(|| invalid(format!("返工单 {} 不存在", o.id)))?;
        guard_open_period(db, old.period)?;
    }
    guard_open_period(db, o.period)?;

    // 来源订单上的编号/物料由系统带出，不接受界面乱填（否则报表按物料汇总会串）
    o.po_no = po.no.clone();
    o.item_code = po.item_code.clone();
    o.item_name = po.item_name.clone();

    let tx = db.write_tx()?;
    let id = if o.id > 0 {
        tx.execute(
            "UPDATE rework_order SET period=?2,date=?3,po_id=?4,po_no=?5,item_code=?6,item_name=?7,
                    qty=?8,reason=?9,reason_code=?10,dept=?11,status=?12,memo=?13 WHERE id=?1",
            rusqlite::params![
                o.id,
                o.period.ymm(),
                o.date.format("%Y-%m-%d").to_string(),
                o.po_id,
                o.po_no,
                o.item_code,
                o.item_name,
                crate::exact_param(o.qty),
                o.reason.trim(),
                o.reason_code.trim(),
                o.dept.trim(),
                o.status,
                o.memo.trim(),
            ],
        )?;
        o.id
    } else {
        let no = next_no(&tx, o.period)?;
        tx.execute(
            "INSERT INTO rework_order(no,period,date,po_id,po_no,item_code,item_name,qty,reason,reason_code,dept,status,memo,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            rusqlite::params![
                no,
                o.period.ymm(),
                o.date.format("%Y-%m-%d").to_string(),
                o.po_id,
                o.po_no,
                o.item_code,
                o.item_name,
                crate::exact_param(o.qty),
                o.reason.trim(),
                o.reason_code.trim(),
                o.dept.trim(),
                o.status,
                o.memo.trim(),
                who,
                now(),
            ],
        )?;
        o.no = no;
        tx.last_insert_rowid()
    };
    tx.commit()?;
    o.id = id;
    Ok(id)
}

/// 删除返工单（连同它的成本明细一起删；已结案的不许删）
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    let old = get(db, id)?.ok_or_else(|| invalid(format!("返工单 {id} 不存在")))?;
    guard_open_period(db, old.period)?;
    if old.is_closed() {
        return Err(blocked("返工单已结案，要删先改回「进行中」"));
    }
    let tx = db.write_tx()?;
    tx.execute(
        "DELETE FROM rework_cost WHERE rw_id=?1",
        rusqlite::params![id],
    )?;
    tx.execute(
        "DELETE FROM rework_order WHERE id=?1",
        rusqlite::params![id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 追加一笔返工成本
pub fn add_cost(db: &Db, rw_id: i64, cost_type: &str, amount: Money, memo: &str) -> DbResult<i64> {
    if !COST_KINDS.iter().any(|(k, _)| *k == cost_type) {
        return Err(invalid(format!("成本类型不认识：{cost_type}")));
    }
    if amount.is_negative() || amount.is_zero() {
        return Err(invalid("返工成本金额必须大于 0"));
    }
    let order = get(db, rw_id)?.ok_or_else(|| invalid(format!("返工单 {rw_id} 不存在")))?;
    guard_open_period(db, order.period)?;
    if order.is_closed() {
        return Err(blocked(
            "返工单已结案，不能再追加成本（要补先把状态改回「进行中」）",
        ));
    }
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO rework_cost(rw_id,cost_type,amount,memo,created_at) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            rw_id,
            cost_type,
            crate::money_param(amount),
            memo.trim(),
            now()
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

pub fn delete_cost(db: &Db, cost_id: i64) -> DbResult<()> {
    let (rw_id,): (i64,) = db
        .conn()
        .query_row(
            "SELECT rw_id FROM rework_cost WHERE id=?1",
            rusqlite::params![cost_id],
            |r| Ok((r.get(0)?,)),
        )
        .optional()?
        .ok_or_else(|| invalid(format!("返工成本 {cost_id} 不存在")))?;
    let order = get(db, rw_id)?.ok_or_else(|| invalid(format!("返工单 {rw_id} 不存在")))?;
    guard_open_period(db, order.period)?;
    if order.is_closed() {
        return Err(blocked("返工单已结案，成本明细不能再删"));
    }
    let tx = db.write_tx()?;
    tx.execute(
        "DELETE FROM rework_cost WHERE id=?1",
        rusqlite::params![cost_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 返工成本分析：期间内每张返工单一行，带原单成本与占比
///
/// 「这批返工亏了多少」就靠这张表回答：total 是返工本身花掉的钱，
/// ratio 是它占原单已归集成本的比例。
pub fn report(db: &Db, from: Period, to: Period) -> DbResult<Vec<ReworkStat>> {
    let names = reason_names(db)?;
    let mut out = Vec::new();
    let mut st = db.conn().prepare(&format!(
        "SELECT {O_COLS} FROM rework_order WHERE period>=?1 AND period<=?2 ORDER BY date, id"
    ))?;
    let rows = st.query_map(rusqlite::params![from.ymm(), to.ymm()], map_order)?;
    let mut orders = Vec::new();
    for r in rows {
        orders.push(r?);
    }
    for o in orders {
        let d = detail(db, o.id)?;
        let (material, labor, overhead, total) = d
            .as_ref()
            .map(|d| (d.material, d.labor, d.overhead, d.total))
            .unwrap_or_default();
        let po_cost = crate::manufacturing::get_prod_total_cost(db, o.po_id)?;
        // 占比用原单成本做分母：原单没归集过成本时分母是 0，这时给空串而不是 0%
        let ratio = if po_cost.is_zero() {
            String::new()
        } else {
            let pct = (total * 100)
                .checked_div(po_cost)
                .unwrap_or(Money::ZERO)
                .round2();
            pct.fmt_plain()
        };
        out.push(ReworkStat {
            rw_id: o.id,
            no: o.no,
            date: o.date.format("%Y-%m-%d").to_string(),
            po_no: o.po_no,
            item_code: o.item_code,
            item_name: o.item_name,
            qty: o.qty,
            reason: o.reason,
            reason_code: o.reason_code.clone(),
            reason_label: reason_name(&o.reason_code, &names),
            dept: o.dept,
            status: o.status,
            material,
            labor,
            overhead,
            total,
            po_cost,
            ratio,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manufacturing::{self, CostType};
    use crate::tests::mem;
    use fincore::Period;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    /// 造一张生产订单（返工单必须挂它）
    fn po(db: &Db, p: Period, no: &str, item: &str, planned: &str) -> i64 {
        let now = now();
        db.conn()
            .execute(
                "INSERT INTO production_order(period, no, date, item_code, item_name, planned_qty, completed_qty, status, work_center, prepared_by, memo, created_at, updated_at)
                 VALUES(?1,?2,'2026-01-05',?3,?3,?4,'0','in_progress','WC01','u1','',?5,?5)",
                rusqlite::params![p.ymm(), no, item, planned, now],
            )
            .unwrap();
        db.conn().last_insert_rowid()
    }

    fn rw(po_id: i64, qty: &str) -> ReworkOrder {
        let mut o = ReworkOrder::new(po_id, NaiveDate::from_ymd_opt(2026, 1, 20).unwrap());
        o.qty = m(qty);
        o.reason = "挂具印痕".into();
        o
    }

    /// 建单：编号自动生成、原单上的编号/物料由系统带出、数量与来源订单都要校验
    #[test]
    fn 返工单必须挂来源订单并带出物料() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let po_id = po(&db, p, "SC2026010001", "铝件A", "10");

        let mut o = rw(po_id, "3");
        // 界面乱填的编号/物料要被来源订单覆盖
        o.po_no = "乱填".into();
        o.item_code = "XXXX".into();
        let id = save(&db, &mut o, "u1").unwrap();
        assert!(id > 0);
        assert_eq!(o.no, "RG-202601-001", "编号按期间生成");
        assert_eq!(o.po_no, "SC2026010001");
        assert_eq!(o.item_code, "铝件A");
        assert_eq!(o.item_name, "铝件A");

        // 第二张单递增
        let mut o2 = rw(po_id, "1");
        save(&db, &mut o2, "u1").unwrap();
        assert_eq!(o2.no, "RG-202601-002");

        // 没挂来源订单
        let mut bad = rw(0, "1");
        let err = save(&db, &mut bad, "u1").unwrap_err();
        assert!(
            err.to_string().contains("必须选一张来源生产订单"),
            "实际：{err}"
        );

        // 挂了一张不存在的订单
        let mut bad2 = rw(999, "1");
        let err = save(&db, &mut bad2, "u1").unwrap_err();
        assert!(err.to_string().contains("不存在"), "实际：{err}");

        // 数量 0
        let mut bad3 = rw(po_id, "0");
        let err = save(&db, &mut bad3, "u1").unwrap_err();
        assert!(err.to_string().contains("必须大于 0"), "实际：{err}");

        // 超过原单计划量（10）
        let mut bad4 = rw(po_id, "11");
        let err = save(&db, &mut bad4, "u1").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("超过原单计划量"), "实际：{msg}");
        assert!(
            msg.contains("11") && msg.contains("10"),
            "报错要带上两个数量：{msg}"
        );

        // 状态不认识
        let mut bad5 = rw(po_id, "1");
        bad5.status = "half".into();
        let err = save(&db, &mut bad5, "u1").unwrap_err();
        assert!(err.to_string().contains("状态不认识"), "实际：{err}");
    }

    /// 成本三段汇总与「占原单成本」的比例
    #[test]
    fn 返工成本三段汇总并算出占原单的比例() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let po_id = po(&db, p, "SC2026010002", "铝件B", "10");
        // 原单已经归集了 900 元成本
        manufacturing::add_cost(&db, po_id, CostType::Material, m("600"), "领料").unwrap();
        manufacturing::add_cost(&db, po_id, CostType::Labor, m("300"), "报工").unwrap();

        let mut o = rw(po_id, "2");
        let id = save(&db, &mut o, "u1").unwrap();
        add_cost(&db, id, "material", m("100"), "返工补料").unwrap();
        add_cost(&db, id, "labor", m("50"), "返工工时").unwrap();
        add_cost(&db, id, "overhead", m("30"), "返工分摊").unwrap();

        let d = detail(&db, id).unwrap().unwrap();
        assert_eq!(d.material, m("100"));
        assert_eq!(d.labor, m("50"));
        assert_eq!(d.overhead, m("30"));
        assert_eq!(d.total, m("180"), "三段之和");
        assert_eq!(d.costs.len(), 3);
        assert_eq!(d.costs[0].kind_label(), "材料");

        // 180 ÷ 900 = 20%
        let rows = report(&db, p, p).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.rw_id, id);
        assert_eq!(r.total, m("180"));
        assert_eq!(r.po_cost, m("900"));
        assert_eq!(r.ratio, "20.00", "占原单成本 20%");
        assert_eq!(r.item_code, "铝件B");

        // 原单没有成本时不给比例（除零会 panic）
        let po2 = po(&db, p, "SC2026010003", "铝件C", "5");
        let mut o2 = rw(po2, "1");
        save(&db, &mut o2, "u1").unwrap();
        add_cost(&db, o2.id, "material", m("10"), "").unwrap();
        let rows = report(&db, p, p).unwrap();
        let r2 = rows.iter().find(|x| x.rw_id == o2.id).unwrap();
        assert_eq!(r2.ratio, "", "原单没成本时比例为空白");

        // 成本类型不认识
        let err = add_cost(&db, id, "travel", m("1"), "").unwrap_err();
        assert!(err.to_string().contains("成本类型不认识"), "实际：{err}");
    }

    /// 结案之后不许再追加成本，也不许删单
    #[test]
    fn 已结案的返工单不许改也不许删() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let po_id = po(&db, p, "SC2026010004", "铝件D", "10");
        let mut o = rw(po_id, "2");
        let id = save(&db, &mut o, "u1").unwrap();
        add_cost(&db, id, "material", m("20"), "").unwrap();

        let mut o = get(&db, id).unwrap().unwrap();
        o.status = "closed".into();
        save(&db, &mut o, "u1").unwrap();
        assert!(o.is_closed());

        let err = add_cost(&db, id, "labor", m("10"), "").unwrap_err();
        assert!(err.to_string().contains("已结案"), "实际：{err}");
        let cost_id = costs(&db, id).unwrap()[0].id;
        let err = delete_cost(&db, cost_id).unwrap_err();
        assert!(err.to_string().contains("已结案"), "实际：{err}");
        let err = delete(&db, id).unwrap_err();
        assert!(err.to_string().contains("已结案"), "实际：{err}");

        // 改回进行中就能继续补
        let mut o = get(&db, id).unwrap().unwrap();
        o.status = "open".into();
        save(&db, &mut o, "u1").unwrap();
        add_cost(&db, id, "labor", m("10"), "补录工时").unwrap();
        assert_eq!(total(&db, id).unwrap(), m("30"));
    }

    /// 期间结账之后返工单不能再改（与工资表那道闸同一个理由）
    #[test]
    fn 结账后的返工单不许改() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let po_id = po(&db, p, "SC2026010005", "铝件E", "10");
        let mut o = rw(po_id, "2");
        let id = save(&db, &mut o, "u1").unwrap();
        add_cost(&db, id, "material", m("20"), "").unwrap();

        crate::periods::close(&db, p, "u1", false).unwrap();

        let mut o = get(&db, id).unwrap().unwrap();
        o.reason = "改个理由".into();
        let err = save(&db, &mut o, "u1").unwrap_err();
        assert!(err.to_string().contains("已结账"), "实际：{err}");
        let err = add_cost(&db, id, "labor", m("5"), "").unwrap_err();
        assert!(err.to_string().contains("已结账"), "实际：{err}");
        let err = delete(&db, id).unwrap_err();
        assert!(err.to_string().contains("已结账"), "实际：{err}");
        // 新单落在已结账期间也不许建
        let mut later = rw(po_id, "1");
        later.period = p;
        let err = save(&db, &mut later, "u1").unwrap_err();
        assert!(err.to_string().contains("已结账"), "实际：{err}");
    }

    /// 返工单只是成本归集，不该在总账里多出凭证（材料/人工那边已经入过账了）
    #[test]
    fn 返工单不生成凭证() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let po_id = po(&db, p, "SC2026010006", "铝件F", "10");
        let before: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM voucher", [], |r| r.get(0))
            .unwrap();
        let mut o = rw(po_id, "2");
        let id = save(&db, &mut o, "u1").unwrap();
        add_cost(&db, id, "material", m("20"), "").unwrap();
        add_cost(&db, id, "labor", m("10"), "").unwrap();
        let after: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM voucher", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "返工单不该生成凭证");
    }

    /// 原因分类：自由文本汇不出总额，字典才能；空编码/空部门单独成桶，不能丢
    #[test]
    fn 返工成本按原因与部门分组且不丢未分类() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 字典：槽液 SLUG / 挂具 GJ
        let mut r1 = Reason {
            code: "slug".into(),
            name: "槽液".into(),
            ..Default::default()
        };
        reason_save(&db, &mut r1).unwrap();
        assert_eq!(r1.code, "SLUG", "编码统一大写，历史单据才对得上");
        let mut r2 = Reason {
            code: "GJ".into(),
            name: "挂具".into(),
            ..Default::default()
        };
        reason_save(&db, &mut r2).unwrap();

        let po_id = po(&db, p, "SC2026010007", "铝件G", "20");
        let mk = |qty: &str, code: &str, dept: &str, cost: &str| {
            let mut o = rw(po_id, qty);
            o.reason_code = code.into();
            o.dept = dept.into();
            let id = save(&db, &mut o, "u1").unwrap();
            add_cost(&db, id, "labor", m(cost), "").unwrap();
            id
        };
        mk("3", "SLUG", "氧化一班", "300");
        mk("2", "SLUG", "氧化二班", "100");
        mk("1", "GJ", "氧化一班", "50");
        mk("1", "", "", "50"); // 谁都没填

        let a = analyze(&db, p, p).unwrap();
        assert_eq!(a.count, 4);
        assert_eq!(a.total, m("500"), "合计 300+100+50+50");
        // 按原因：槽液 400（80%）、挂具 50（10%）、未分类 50（10%）
        assert_eq!(a.by_reason.len(), 3, "三种原因各一桶，空编码也要有一桶");
        let slug = a.by_reason.iter().find(|b| b.label == "槽液").unwrap();
        assert_eq!(slug.key, "SLUG");
        assert_eq!(slug.total, m("400"));
        assert_eq!(slug.share, "80.00");
        let gj = a.by_reason.iter().find(|b| b.label == "挂具").unwrap();
        assert_eq!(gj.total, m("50"));
        let none = a
            .by_reason
            .iter()
            .find(|b| b.label == UNCLASSIFIED)
            .expect("没填分类的不能丢");
        assert_eq!(none.total, m("50"));
        assert_eq!(none.key, "");
        // 按部门：氧化一班 350、氧化二班 100、未指定 50；分组合计 == 总合计
        let sum_d: Money = a.by_dept.iter().fold(Money::ZERO, |acc, b| acc + b.total);
        assert_eq!(sum_d, a.total, "各分组合计必须等于总合计（不丢行才成立）");
        let one = a.by_dept.iter().find(|b| b.label == "氧化一班").unwrap();
        assert_eq!(one.count, 2);
        assert_eq!(one.total, m("350"));
        assert_eq!(a.by_dept.last().unwrap().label, NO_DEPT);
        // 报表行也带中文分类名
        let rows = report(&db, p, p).unwrap();
        assert!(rows.iter().any(|x| x.reason_label == "槽液"));
        assert!(rows.iter().any(|x| x.reason_label == UNCLASSIFIED));
    }

    /// 分类被单据引用就不许删（删了历史单据会变成「未分类」，口径跟着变）
    #[test]
    fn 原因分类被引用就不许删只能停用() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut r = Reason {
            code: "SLUG".into(),
            name: "槽液".into(),
            ..Default::default()
        };
        reason_save(&db, &mut r).unwrap();
        let po_id = po(&db, p, "SC2026010008", "铝件H", "5");
        let mut o = rw(po_id, "1");
        o.reason_code = "SLUG".into();
        save(&db, &mut o, "u1").unwrap();

        let e = reason_delete(&db, r.id).unwrap_err().to_string();
        assert!(e.contains("先改单"), "被引用时要提示怎么办：{e}");
        // 停用是可以的：新单不再选它，历史单据照样显示
        r.disabled = true;
        reason_save(&db, &mut r).unwrap();
        assert!(reason_list(&db).unwrap().iter().any(|x| x.disabled));
    }
}
