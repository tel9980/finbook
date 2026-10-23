// findb::customers —— 以客户为主体的视图层。
//
// 【最重要的一条】余额只有一处算法。
// 这里**不许**出现任何自己算应收余额的 SQL：全部走
// `settle::open_entries("1122")`。理由不是 DRY，是口径一致 ——
// 全仓 H-3 口径是「只认已记账分录」，而 open_entries 里还有
// 「期初挂账（arap_opening）作为影子行参与账龄」这套处理。
// 自己算一遍 = 两套数字，而「为什么这个客户欠款和往来核销页不一样」
// 是最难查的一类问题。
//
// `tools/check-ci.js` 里有对应的守门人：本文件不许出现
// voucher_entry / stock_move 的裸 SELECT。
//
// 应收口径：只统计 1122 系（1122 及其子科目，由 open_entries 的
// `LIKE ?1||'%'` 自动覆盖）。理由见 docs/客户管理模块设计.md §5.1 ——
// 1121 应收票据 / 1123 预付账款 / 1221 其他应收 / 1231 坏账准备
// 性质不同，混进「客户欠款」会让数字没法解释。
use chrono::NaiveDate;
use fincore::{AuxKind, AuxQuery, Money, Period};

use crate::settle::{self, OpenEntry};
use crate::{Db, DbResult};

/// 应收科目（传一级科目即可匹配全部子科目）
pub const AR_ACCOUNT: &str = "1122";

/// 一行一个客户的汇总
#[derive(Clone, Debug, serde::Serialize)]
pub struct CustomerSummary {
    pub code: String,
    pub name: String,
    pub parent_code: String,
    pub disabled: bool,
    /// 应收余额（已记账，含负数 = 预收）
    pub balance: Money,
    /// 未核销余额（balance 的非零部分）
    pub open_amount: Money,
    /// 未核销笔数
    pub open_count: usize,
    /// 最早一笔未核销的日期（空 = 没有未核销）
    pub oldest_open_date: String,
    /// 期初挂账余额（不进 open_entries，来自 arap_opening）
    pub opening_balance: Money,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CustomerDetail {
    #[serde(flatten)]
    pub summary: CustomerSummary,
    /// 该客户的全部往来行（期初 + 已记账分录）
    pub lines: Vec<CustomerLine>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CustomerLine {
    pub entry_id: i64,
    pub voucher_id: i64,
    pub period: i32,
    pub date: String,
    pub doc_no: String,
    pub summary: String,
    pub account_code: String,
    /// 带符号金额（借正贷负）
    pub amount: Money,
    pub settled: Money,
    pub open: Money,
    pub settle_no: String,
    pub posted: bool,
    /// 这行来自期初挂账（无 voucher_id）
    pub from_opening: bool,
}

/// 该客户在某期间的应收未核销分录。
///
/// `period` 传 0 表示「到最新」—— 与 settle 侧其他函数的约定一致。
pub fn open_lines(db: &Db, customer: &str, period: Period) -> DbResult<Vec<OpenEntry>> {
    let want = crate::balances::aux_key_frag(customer, AuxKind::Customer);
    // 在 SQL 层面按 aux_key 过滤（customer=C01），避免取全量应收未核销分录
    // 到 Rust 侧再 filter。`open_entries_with` 已在内部填充 `settled` 字段。
    settle::open_entries_with(db, AR_ACCOUNT, period, false, true, Some(&want))
}

/// 汇总一个客户的余额。只读 [open_lines]，不自己算。
pub fn summary_of(db: &Db, code: &str, period: Period) -> DbResult<CustomerSummary> {
    let lines = open_lines(db, code, period)?;
    Ok(summarize(
        code,
        "",
        &lines,
        opening_balance(db, code, period)?,
    ))
}

/// 期初挂账（应收方向）余额。
///
/// 这部分**不在** open_entries 里 —— open_entries 只读凭证分录。
/// 而 settle::aging 会把 arap_opening 当影子行加进来，所以账龄里能看到、
/// open_entries 里看不到。两者口径不同是有意的（影子行不参与 FIFO 核销），
/// 所以这里必须**单独取并标出来**，不能悄悄加进 balance。
fn opening_balance(db: &Db, code: &str, period: Period) -> DbResult<Money> {
    let mut sum = Money::ZERO;
    for o in settle::arap_opening_list(db, Some("ar"))? {
        if o.party_code.trim() != code.trim() {
            continue;
        }
        // ArapOpening 没有 period 字段，只有 doc_date —— 从日期算期间。
        // 与 settle::aging 里 aging() 的做法一致。
        let p_of_doc = NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
            .map(Period::from_date)
            .unwrap_or(Period::ZERO);
        if period != Period::ZERO && p_of_doc > period {
            continue;
        }
        sum += o.amount;
    }
    Ok(sum)
}

fn summarize(code: &str, name: &str, lines: &[OpenEntry], opening: Money) -> CustomerSummary {
    let mut balance = Money::ZERO;
    let mut open_amount = Money::ZERO;
    let mut open_count = 0usize;
    let mut oldest = String::new();
    for e in lines {
        balance += e.signed();
        if e.is_open() {
            open_amount += e.open();
            open_count += 1;
            let d = e.date.format("%Y-%m-%d").to_string();
            if oldest.is_empty() || d < oldest {
                oldest = d;
            }
        }
    }
    CustomerSummary {
        code: code.to_string(),
        name: name.to_string(),
        parent_code: String::new(),
        disabled: false,
        balance,
        open_amount,
        open_count,
        oldest_open_date: oldest,
        opening_balance: opening,
    }
}

/// 客户列表：一行一客户，带余额汇总。
///
//// 分页结果
#[derive(Debug, serde::Serialize)]
pub struct CustomerListResult {
    pub rows: Vec<CustomerSummary>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

/// `q` 非空时按编码/名称过滤（大小写不敏感）；`only_open` 为真时
/// 只返回有未核销余额的客户 —— 催款场景要的就是这张名单。
///
/// `page` 从 1 开始；`page_size` 为 0 时使用默认值 50。
pub fn list(
    db: &Db,
    period: Period,
    q: &str,
    only_open: bool,
    page: usize,
    page_size: usize,
) -> DbResult<CustomerListResult> {
    let page = page.max(1);
    let page_size = if page_size == 0 { 50 } else { page_size };

    // 一次取全量未核销分录，按 aux_key 分组 —— 免得每个客户查一次
    let all = settle::open_entries(db, AR_ACCOUNT, period, false)?;
    let mut grouped: std::collections::BTreeMap<String, Vec<OpenEntry>> =
        std::collections::BTreeMap::new();
    for e in all {
        // 分组键必须是**客户维度**，不是整条 aux_key ——
        // 一条分录可能同时挂 item/qty/price，整条做键会把同一客户拆成多组。
        let k = crate::balances::aux_key_value_of(&e.aux_key, AuxKind::Customer);
        if k.is_empty() {
            continue;
        }
        grouped.entry(k).or_default().push(e);
    }

    // 一次性加载所有应收期初挂账，按客户编码聚合，避免在下面循环里
    // 对每个客户都跑一次 `arap_opening_list`（原代码 N+1：60+ 客户就查 60+ 次）。
    let openings = settle::arap_opening_list(db, Some("ar"))?;
    let mut opening_by_party: std::collections::BTreeMap<String, Money> =
        std::collections::BTreeMap::new();
    for o in &openings {
        let code = o.party_code.trim();
        if code.is_empty() {
            continue;
        }
        let p_of_doc = NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
            .map(Period::from_date)
            .unwrap_or(Period::ZERO);
        if period != Period::ZERO && p_of_doc > period {
            continue;
        }
        *opening_by_party.entry(code.to_string()).or_default() += o.amount;
    }

    let ents = crate::auxs::list(db, &AuxQuery::kind(AuxKind::Customer))?;
    let kw = q.trim().to_lowercase();
    let mut out = Vec::with_capacity(ents.len());
    for ent in ents {
        if !kw.is_empty()
            && !ent.code.to_lowercase().contains(&kw)
            && !ent.name.to_lowercase().contains(&kw)
        {
            continue;
        }
        let empty = Vec::new();
        let lines = grouped.get(&ent.code).unwrap_or(&empty);
        let opening = opening_by_party.get(&ent.code).copied().unwrap_or(Money::ZERO);
        let mut s = summarize(&ent.code, &ent.name, lines, opening);
        s.parent_code = ent.parent_code.clone().unwrap_or_default();
        s.disabled = ent.disabled;
        // 「有没有人在追着要钱」不能只看凭证行：期初挂账也是钱。
        //
        // 客户只在**期初挂账**欠款时，open_amount = 0（期初不进 open_entries，
        // 它是影子行）→ 只看 open_amount 的话，他会在催款名单上**完全消失**，
        // 而那正是最该催的人。E2E `期初挂账单独显示` 实测到了这个。
        //
        // 注意：balance 字段的语义**不变**（仍不把期初混进去，与账龄页口径一致）；
        // 改的只是「要不要出现在名单里」。
        if only_open && s.open_amount.is_zero() && s.opening_balance.is_zero() {
            continue;
        }
        out.push(s);
    }
    // 有欠款的排前面，同组按余额倒序 —— 催款时先看到最该催的
    out.sort_by(|a, b| {
        b.open_amount
            .cmp(&a.open_amount)
            .then_with(|| a.code.cmp(&b.code))
    });

    let total = out.len();
    let start = (page - 1) * page_size;
    let end = (start + page_size).min(total);
    let rows = if start < total {
        out[start..end].to_vec()
    } else {
        Vec::new()
    };

    Ok(CustomerListResult {
        rows,
        total,
        page,
        page_size,
    })
}

/// 客户详情：汇总 + 全部往来行。
pub fn detail(db: &Db, code: &str, period: Period) -> DbResult<Option<CustomerDetail>> {
    let ent = match crate::auxs::get(db, AuxKind::Customer, code)? {
        Some(e) => e,
        None => return Ok(None),
    };
    let mut lines = open_lines(db, code, period)?;

    // 期初挂账也进明细，但要标出来（from_opening=true、无 voucher_id）。
    //
    // 条件是「**任何**期间都加」，只按 doc_date 过滤 —— 我第一版写的是
    // `if period == Period::ZERO`（只有不限期间时才加），逻辑反了：
    // 期初挂账就发生在建账之前，任何具体期间的账龄/余额都该包含它。
    // 于是「这个客户有 3000 期初欠款」在正常查询下显示不出来，
    // 而汇总字段 opening_balance 却有值 —— 界面上会出现
    // 「顶部写着 3000、明细里一条都没有」。
    for o in settle::arap_opening_list(db, Some("ar"))? {
        if o.party_code.trim() != code.trim() {
            continue;
        }
        let dt = NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
            .unwrap_or_else(|_| chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
        let p_of_doc = Period::from_date(dt);
        if period != Period::ZERO && p_of_doc > period {
            continue;
        }
        lines.push(OpenEntry {
            entry_id: 0,
            voucher_id: 0,
            period: p_of_doc,
            date: dt,
            word: String::new(),
            no: 0,
            line: 0,
            summary: if o.doc_no.is_empty() {
                format!("期初 {}", o.memo)
            } else {
                format!("期初 {}", o.doc_no)
            },
            account_code: AR_ACCOUNT.to_string(),
            aux_key: crate::balances::aux_key_frag(&o.party_code, AuxKind::Customer),
            settle_no: String::new(),
            debit: o.amount,
            credit: Money::ZERO,
            settled: Money::ZERO,
            posted: true,
        });
    }

    let out_lines: Vec<CustomerLine> = lines
        .iter()
        .map(|e| CustomerLine {
            entry_id: e.entry_id,
            voucher_id: e.voucher_id,
            period: e.period.ymm(),
            date: e.date.format("%Y-%m-%d").to_string(),
            doc_no: if e.entry_id == 0 {
                e.summary.clone()
            } else {
                format!("{}-{}", e.word, e.no)
            },
            summary: e.summary.clone(),
            account_code: e.account_code.clone(),
            amount: e.signed(),
            settled: e.settled,
            open: e.open(),
            settle_no: e.settle_no.clone(),
            posted: e.posted,
            from_opening: e.entry_id == 0,
        })
        .collect();

    let mut s = summarize(
        &ent.code,
        &ent.name,
        &lines,
        opening_balance(db, code, period)?,
    );
    s.parent_code = ent.parent_code.clone().unwrap_or_default();
    s.disabled = ent.disabled;
    Ok(Some(CustomerDetail {
        summary: s,
        lines: out_lines,
    }))
}

// ---------------- 交易历史（报价 / 订单 / 发货 / 收款） ----------------

/// 交易历史的一行。口径见 docs/客户管理模块设计.md §3.1 Tab 3、§3.4。
#[derive(Clone, Debug, serde::Serialize)]
pub struct TradeRow {
    pub date: String,
    /// quotation / order / shipment / receipt
    pub kind: String,
    /// 中文标签（报价 / 销售订单 / 发货 / 收款）
    pub kind_label: String,
    pub doc_no: String,
    pub summary: String,
    /// 金额。发货行只有数量没有金额，看 `qty`。
    pub amount: Money,
    /// 数量（发货行有意义，其余行为 0）
    pub qty: Money,
    pub status: String,
    pub status_label: String,
}

fn quo_status_label(s: &str) -> &'static str {
    match s {
        "draft" => "草稿",
        "approved" => "已审批",
        "converted" => "已转订单",
        "cancelled" => "已作废",
        _ => "",
    }
}

fn so_status_label(s: &str) -> &'static str {
    match s {
        "draft" => "草稿",
        "confirmed" => "已确认",
        "partial_ship" => "部分发货",
        "completed" => "已完成",
        "cancelled" => "已作废",
        _ => "",
    }
}

fn receipt_status_label(s: &str) -> &'static str {
    match s {
        "draft" => "待审核",
        "audited" => "已审核",
        _ => "",
    }
}

/// 客户交易历史：报价 → 销售订单 → 发货 → 收款，按日期倒序。
///
/// 【为什么这里可以直接查业务表，而余额不行】这是「客户买过什么」的**单据流水**，
/// 不是余额 —— 余额的口径被 `open_entries` 独占（见本文件头）。单据有自己的来源表，
/// 直接读它们才是对的；而**收款取 `receipt_doc`，不取 `so_payment`**，因为同一笔钱
/// `so_payment` 与 `receipt_doc` 各记一份，客户维度只能认其中一个（设计 §3.4）。
pub fn trade(db: &Db, code: &str, from: Period, to: Period) -> DbResult<Vec<TradeRow>> {
    let code = code.trim();
    let (f, t) = (from.ymm(), to.ymm());
    let mut out: Vec<TradeRow> = Vec::new();

    // 报价单
    {
        let mut st = db.conn().prepare(
            "SELECT date, no, item_name, qty, unit_price, status FROM quotation
             WHERE customer_code = ?1 AND period >= ?2 AND period <= ?3
             ORDER BY date DESC, id DESC",
        )?;
        let rows = st.query_map(rusqlite::params![code, f, t], |r| {
            let qty = Money::parse_or_zero(&r.get::<_, String>(3)?);
            let price = Money::parse_or_zero(&r.get::<_, String>(4)?);
            let status: String = r.get(5)?;
            Ok(TradeRow {
                date: r.get(0)?,
                kind: "quotation".into(),
                kind_label: "报价".into(),
                doc_no: r.get(1)?,
                summary: r.get(2)?,
                amount: qty * price,
                qty,
                status_label: quo_status_label(&status).to_string(),
                status,
            })
        })?;
        for r in rows {
            out.push(r?);
        }
    }

    // 销售订单
    {
        let mut st = db.conn().prepare(
            "SELECT date, no, total_amount, status, memo FROM sales_order
             WHERE customer_code = ?1 AND period >= ?2 AND period <= ?3
             ORDER BY date DESC, id DESC",
        )?;
        let rows = st.query_map(rusqlite::params![code, f, t], |r| {
            let status: String = r.get(3)?;
            Ok(TradeRow {
                date: r.get(0)?,
                kind: "order".into(),
                kind_label: "销售订单".into(),
                doc_no: r.get(1)?,
                summary: r.get(4)?,
                amount: Money::parse_or_zero(&r.get::<_, String>(2)?),
                qty: Money::ZERO,
                status_label: so_status_label(&status).to_string(),
                status,
            })
        })?;
        for r in rows {
            out.push(r?);
        }
    }

    // 发货（so_shipment 只有 so_id → JOIN 订单取客户编码）
    {
        let mut st = db.conn().prepare(
            "SELECT s.date, o.no, s.qty, s.memo
             FROM so_shipment s JOIN sales_order o ON s.so_id = o.id
             WHERE o.customer_code = ?1 AND s.period >= ?2 AND s.period <= ?3
             ORDER BY s.date DESC, s.id DESC",
        )?;
        let rows = st.query_map(rusqlite::params![code, f, t], |r| {
            Ok(TradeRow {
                date: r.get(0)?,
                kind: "shipment".into(),
                kind_label: "发货".into(),
                doc_no: r.get(1)?,
                summary: r.get(3)?,
                amount: Money::ZERO,
                qty: Money::parse_or_zero(&r.get::<_, String>(2)?),
                status: String::new(),
                status_label: String::new(),
            })
        })?;
        for r in rows {
            out.push(r?);
        }
    }

    // 收款（会计口径：receipt_doc 按往来单位；只认 kind='receipt'）
    {
        let mut st = db.conn().prepare(
            "SELECT date, no, amount, memo, status FROM receipt_doc
             WHERE kind = 'receipt' AND party = ?1 AND period >= ?2 AND period <= ?3
             ORDER BY date DESC, id DESC",
        )?;
        let rows = st.query_map(rusqlite::params![code, f, t], |r| {
            let status: String = r.get(4)?;
            Ok(TradeRow {
                date: r.get(0)?,
                kind: "receipt".into(),
                kind_label: "收款".into(),
                doc_no: r.get(1)?,
                summary: r.get(3)?,
                amount: Money::parse_or_zero(&r.get::<_, String>(2)?),
                qty: Money::ZERO,
                status_label: receipt_status_label(&status).to_string(),
                status,
            })
        })?;
        for r in rows {
            out.push(r?);
        }
    }

    // 各来源已按日期倒序；稳定排序后同日仍保持「报价→订单→发货→收款」的相对次序。
    out.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(out)
}

// ---------------- 催款记录（催款单 + 往来对账单） ----------------

/// 催款记录的一行：催款单（dunning）+ 往来对账单（ar_statement）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct CustomerDunning {
    /// 排序与展示日期：催款=单据日期，对账单=生成日期
    pub date: String,
    /// dunning / statement
    pub kind: String,
    pub kind_label: String,
    pub no: String,
    /// 催款=备注；对账单=对账期间
    pub summary: String,
    pub amount: Money,
    pub item_count: i64,
    pub status: String,
    pub status_label: String,
}

fn dunning_status_label(s: &str) -> &'static str {
    match s {
        "draft" => "草稿",
        "sent" => "已发出",
        "settled" => "已结清",
        "cancelled" => "已作废",
        _ => "",
    }
}

fn statement_status_label(s: &str) -> &'static str {
    match s {
        "draft" => "草稿",
        "sent" => "已发出",
        "confirmed" => "已确认",
        "cancelled" => "已作废",
        _ => "",
    }
}

/// 该客户的催款记录：催款单 + 往来对账单，按日期倒序。
///
/// 两者都**复用**各自模块的 list（`settle::dunning_list` / `statement::list`），
/// 现在在 DB 层按 `party_code` 过滤，避免全表取回再 filter。
pub fn dunning_records(db: &Db, code: &str) -> DbResult<Vec<CustomerDunning>> {
    let code = code.trim();
    let mut out: Vec<CustomerDunning> = Vec::new();

    for d in settle::dunning_list(db, Some("ar"), Some(code))? {
        out.push(CustomerDunning {
            date: d.date.format("%Y-%m-%d").to_string(),
            kind: "dunning".into(),
            kind_label: "催款单".into(),
            no: d.no,
            summary: d.memo,
            amount: d.amount,
            item_count: d.item_count,
            status_label: dunning_status_label(&d.status).to_string(),
            status: d.status,
        });
    }

    for s in crate::statement::list(db, Some("ar"), Some(code))? {
        // 对账单没有单一「单据日期」：用生成时间的前 10 位当日期排序；
        // 展示上「对账期间」比时间戳更有意义，放 summary。
        let date = s.created_at.get(..10).unwrap_or(&s.created_at).to_string();
        out.push(CustomerDunning {
            date,
            kind: "statement".into(),
            kind_label: "对账单".into(),
            no: s.no,
            summary: format!("{} 至 {}", s.period_from.label(), s.period_to.label()),
            amount: s.end_balance,
            item_count: s.line_count,
            status_label: statement_status_label(&s.status).to_string(),
            status: s.status,
        });
    }

    out.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(out)
}

// ---------------- 账龄（复用 settle::aging，桶与算法都不在这里重算） ----------------

/// 一个客户的账龄分桶。
///
/// 为什么单独一个结构而不是直接返回 `AgingLine`：账龄页是「所有客户一行一个」，
/// 客户页是「这一个客户一整行」。形状不同，但**桶定义与算法必须是同一份**
/// （`settle::aging`）—— 客户页自己分一次桶，就会出现「客户页说 60 天以上 3000、
/// 账龄页说 0」，而两个数都是「按账龄算的」，没人会去怀疑口径不同。
#[derive(Clone, Debug, serde::Serialize)]
pub struct CustomerAging {
    /// 桶标签，顺序与 `amounts` 一一对应（0/30/60/…）
    pub buckets: Vec<String>,
    /// 各档金额
    pub amounts: Vec<Money>,
    /// 合计（借方性质未核销额）
    pub total: Money,
    /// 贷方性质未核销额（预收），单独列示不混进桶
    pub credit_total: Money,
    /// 最老一笔单据的账龄天数
    pub max_days: i64,
    /// 该客户在账龄里**没有行**（无未核销余额）。
    ///
    /// 与「有行但金额都是 0」分开，因为界面上该显示「无未核销余额」
    /// 而不是一张全 0 的表 —— 后者看起来像「算过了，确实是 0」。
    pub empty: bool,
}

/// 某个客户的账龄。**只调 [`settle::aging`]**，不自己算分桶。
///
/// `period` 传 [`Period::ZERO`] 表示「到最新」，与 settle 侧约定一致。
///
/// 客户在账龄里没有行时返回全 0 + `empty=true`（而不是 None）：界面需要一个
/// 稳定的形状去渲染「无未核销余额」，而不是按有无数据换两套模板。
pub fn aging_of(
    db: &Db,
    code: &str,
    period: Period,
    as_of: NaiveDate,
    buckets: &[fincore::engine::aging::AgingBucket],
) -> DbResult<CustomerAging> {
    let labels: Vec<String> = buckets.iter().map(|b| b.label.to_string()).collect();
    let code = code.trim();
    let all = settle::aging(db, AR_ACCOUNT, period, as_of, buckets, Some(code))?;
    match all.iter().find(|l| l.key.trim() == code) {
        Some(l) => Ok(CustomerAging {
            buckets: labels,
            amounts: l.amounts.clone(),
            total: l.total,
            credit_total: l.credit_total,
            max_days: l.max_days,
            empty: false,
        }),
        None => Ok(CustomerAging {
            buckets: labels,
            amounts: vec![Money::ZERO; buckets.len()],
            total: Money::ZERO,
            credit_total: Money::ZERO,
            max_days: 0,
            empty: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use crate::{auxs, settle::ArapOpening, vouchers};
    use chrono::NaiveDate;
    use fincore::{AuxRef, Entry, Voucher};

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// 凭证号：同期间 + 同凭证字内必须唯一，而 `Voucher::new(..., 0)`
    /// **不会**自动编号（重复就报「凭证号已存在」）。所以这里自增。
    static NEXT_NO: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

    fn next_no() -> i32 {
        NEXT_NO.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }

    fn mk_cust(db: &Db, code: &str, name: &str) {
        let mut e = fincore::AuxEntity::new(AuxKind::Customer, code, name);
        e.memo = String::new();
        auxs::insert(db, &e).unwrap();
    }

    /// 造一张已记账的应收凭证。debit=true → 客户欠款（借 112201）。
    fn ar_posted(db: &Db, date: &str, cust: &str, amount: &str, debit: bool) -> i64 {
        ar_posted_at(db, date, cust, amount, debit, "112201")
    }

    /// 同上，但可指定挂在哪个（末级）应收科目上。
    ///
    /// 为什么要能指定：真实数据分布在 112201 / 112202 两个子科目，
    /// 而统计侧传的是父科目 1122 —— 能不能合并，是这条测试要验的。
    fn ar_posted_at(db: &Db, date: &str, cust: &str, amount: &str, debit: bool, acct: &str) -> i64 {
        let dt = d(date);
        let mut v = Voucher::new(Period::from_date(dt), dt, "记", next_no());
        let a = m(amount);
        v.push_entry(Entry {
            debit: if debit { a } else { Money::ZERO },
            credit: if debit { Money::ZERO } else { a },
            aux: AuxRef {
                customer: Some(cust.into()),
                ..Default::default()
            },
            ..Entry::new(1, acct, "往来")
        });
        v.push_entry(Entry {
            debit: if debit { Money::ZERO } else { a },
            credit: if debit { a } else { Money::ZERO },
            ..Entry::new(2, "1001", "往来")
        });
        let vid = vouchers::save(db, &mut v).unwrap();
        vouchers::post(db, vid, "poster").unwrap();
        vid
    }

    /// 只保存不记账（草稿）
    fn ar_draft(db: &Db, date: &str, cust: &str, amount: &str) -> i64 {
        let dt = d(date);
        let mut v = Voucher::new(Period::from_date(dt), dt, "记", next_no());
        let a = m(amount);
        v.push_entry(Entry {
            debit: a,
            credit: Money::ZERO,
            aux: AuxRef {
                customer: Some(cust.into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "往来")
        });
        v.push_entry(Entry {
            debit: Money::ZERO,
            credit: a,
            ..Entry::new(2, "1001", "往来")
        });
        let vid = vouchers::save(db, &mut v).unwrap();
        // 草稿状态（H-3：只认已记账）
        db.conn()
            .execute("UPDATE voucher SET status='draft' WHERE id=?1", [vid])
            .unwrap();
        vid
    }

    /// ① 余额含负数（预收）：客户先付款时应收为负。
    ///
    /// 界面上如果显示 0，会让会计以为「他既没欠也没付」——
    /// 而实际上他有 500 预收款，抵掉以后发货就不用收钱。
    #[test]
    fn balance_can_be_negative() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        let per = Period::new(2026, 1).unwrap();
        ar_posted(&db, "2026-01-10", "C01", "500", false);

        let s = summary_of(&db, "C01", per).unwrap();
        assert_eq!(s.balance, m("-500"), "预收 500 → 应收为**负数**，不是 0");
    }

    /// ② 未记账不计入（H-3）。
    #[test]
    fn draft_entries_excluded() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        let per = Period::new(2026, 1).unwrap();
        ar_draft(&db, "2026-01-10", "C01", "888");

        let s = summary_of(&db, "C01", per).unwrap();
        assert_eq!(
            s.balance,
            Money::ZERO,
            "草稿不算应收（H-3）—— 记进去就和账龄页对不上了"
        );
        assert_eq!(s.open_count, 0, "草稿不该出现在未核销笔数里");
    }

    /// ③ 期初挂账单独计入并标出来。
    ///
    /// 这条最容易做错：期初挂在 arap_opening 表，**不在** open_entries 里。
    /// 不单独取 → 「欠款」凭空少一笔；
    /// 悄悄加进 balance → 与账龄页口径混淆（那边它是影子行，不参与 FIFO 核销）。
    #[test]
    fn opening_balance_is_separate_and_flagged() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        let per = Period::new(2026, 1).unwrap();
        settle::arap_opening_insert(
            &db,
            &ArapOpening {
                id: 0,
                kind: "ar".into(),
                party_code: "C01".into(),
                party_name: "客户甲".into(),
                doc_no: "XS-1".into(),
                doc_date: "2025-12-01".into(),
                amount: m("3000"),
                memo: "上年末欠款".into(),
                created_by: "u".into(),
            },
            "u",
        )
        .unwrap();

        let s = summary_of(&db, "C01", per).unwrap();
        assert_eq!(s.opening_balance, m("3000"), "期初挂账必须计入");
        assert_eq!(
            s.balance,
            Money::ZERO,
            "balance 只含已记账分录；期初单独一个字段，不混进去"
        );

        let det = detail(&db, "C01", per).unwrap().expect("客户应存在");
        let opening: Vec<&CustomerLine> = det.lines.iter().filter(|l| l.from_opening).collect();
        assert_eq!(opening.len(), 1, "明细里应有 1 行期初：{:?}", det.lines);
        assert_eq!(opening[0].doc_no, "期初 XS-1");
        assert_eq!(opening[0].voucher_id, 0, "期初行没有凭证");
    }

    /// ④ 多客户隔离 + 排序 + 搜索
    #[test]
    fn customers_are_isolated_and_sorted() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        mk_cust(&db, "C02", "客户乙");
        let per = Period::new(2026, 1).unwrap();
        ar_posted(&db, "2026-01-05", "C01", "1000", true);
        ar_posted(&db, "2026-01-06", "C02", "250", true);

        assert_eq!(summary_of(&db, "C01", per).unwrap().balance, m("1000"));
        assert_eq!(
            summary_of(&db, "C02", per).unwrap().balance,
            m("250"),
            "乙不能带上甲的"
        );

        let all = list(&db, per, "", false, 1, 100).unwrap();
        assert_eq!(all.rows.len(), 2);
        assert_eq!(all.rows[0].code, "C01", "欠款多的排前面");
        assert_eq!(all.rows[1].code, "C02");

        let q = list(&db, per, "乙", false, 1, 100).unwrap();
        assert_eq!(q.rows.len(), 1);
        assert_eq!(q.rows[0].code, "C02");
    }

    /// 已全部核销的客户不该出现在「只看待催款」名单里。
    #[test]
    fn settled_customer_excluded_from_only_open() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        mk_cust(&db, "C02", "客户乙");
        let per = Period::new(2026, 1).unwrap();
        ar_posted(&db, "2026-01-05", "C01", "1000", true);
        ar_posted(&db, "2026-01-06", "C02", "250", true);
        // 收到 C01 的钱（贷 1122）
        ar_posted(&db, "2026-01-10", "C01", "1000", false);
        settle::auto_settle(&db, AR_ACCOUNT, per, Money::ZERO, "u").unwrap();

        let s = summary_of(&db, "C01", per).unwrap();
        assert_eq!(s.balance, Money::ZERO, "两笔抵平");
        assert_eq!(s.open_amount, Money::ZERO, "全部核销");

        let only = list(&db, per, "", true, 1, 100).unwrap();
        assert_eq!(
            only.rows.len(),
            1,
            "只剩乙还在待催款；已结清的甲是噪音（只留它会让人以为催款名单没过滤）"
        );
        assert_eq!(only.rows[0].code, "C02");
    }

    /// 子科目要一并计入 —— 而且这条测试顺带锁住一个容易搞反的事实。
    ///
    /// 【记账只能用末级，统计可以含父科目】
    ///   凭证里**不能**记在 1122 上（非末级，会被 `Validate` 拒：
    ///   「科目 1122 是非末级科目，不能记账」）。所以真实数据全落在
    ///   112201 / 112202 上。而统计侧传的是 `AR_ACCOUNT = "1122"`，
    ///   靠 open_entries 的 `LIKE ?1||'%'` 把子科目一并捞进来。
    ///
    ///   这两件事必须分开：AR_ACCOUNT 若改成末级（112201），就会漏掉 112202；
    ///   若统计也只认末级，就得为每个子科目各查一次再相加 —— 那正是
    ///   「自己算余额、两套口径」的起点。
    #[test]
    fn parent_account_not_postable_but_sub_accounts_aggregated() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        let per = Period::new(2026, 1).unwrap();

        // 对照：父科目确实不能记账（把「统计含父科目」和「记账用父科目」区分开）
        let dt = d("2026-01-05");
        let mut v = Voucher::new(Period::from_date(dt), dt, "记", next_no());
        v.push_entry(Entry {
            debit: m("100"),
            credit: Money::ZERO,
            aux: AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            ..Entry::new(1, AR_ACCOUNT, "往来")
        });
        v.push_entry(Entry {
            debit: Money::ZERO,
            credit: m("100"),
            ..Entry::new(2, "1001", "往来")
        });
        let parent_err = vouchers::save(&db, &mut v).unwrap_err().to_string();
        assert!(
            parent_err.contains("非末级"),
            "1122 是非末级科目，不该能记账 —— 若哪天能记了，说明科目表变了，这条要重读：{parent_err}"
        );

        // 真实数据：两个不同的子科目
        ar_posted_at(&db, "2026-01-06", "C01", "700", true, "112201");
        ar_posted_at(&db, "2026-01-07", "C01", "50", true, "112202");

        assert_eq!(
            summary_of(&db, "C01", per).unwrap().balance,
            m("750"),
            "112201 与 112202 必须**合并**计入；AR_ACCOUNT 若改成只认一个末级，会静默少算"
        );
    }

    /// 只有期初挂账的客户，必须出现在「只看待催款」名单里。
    ///
    /// 回归：期初挂账不进 balance（open_entries 只读凭证分录），所以若判据只看
    /// balance，客户就会**凭空消失** —— 而他欠 3000，正是最该催的人。
    /// E2E `期初挂账单独显示，不混进余额` 实测到了这个。
    #[test]
    fn opening_only_customer_still_in_only_open() {
        let db = mem();
        mk_cust(&db, "C01", "只有期初");
        mk_cust(&db, "C02", "真的没欠");
        let per = Period::new(2026, 1).unwrap();
        settle::arap_opening_insert(
            &db,
            &ArapOpening {
                id: 0,
                kind: "ar".into(),
                party_code: "C01".into(),
                party_name: "只有期初".into(),
                doc_no: "XS-1".into(),
                doc_date: "2025-12-01".into(),
                amount: m("3000"),
                memo: String::new(),
                created_by: "u".into(),
            },
            "u",
        )
        .unwrap();

        let s = summary_of(&db, "C01", per).unwrap();
        assert_eq!(s.opening_balance, m("3000"));
        assert_eq!(
            s.balance,
            Money::ZERO,
            "balance 仍为 0 —— 期初不混进余额（与账龄页口径一致），这是对的"
        );

        let only = list(&db, per, "", true, 1, 100).unwrap();
        let codes: Vec<&str> = only.rows.iter().map(|r| r.code.as_str()).collect();
        assert_eq!(
            codes,
            vec!["C01"],
            "只有期初挂账的客户要出现在催款名单里；真没欠的 C02 不该出现"
        );
    }

    /// 不存在的客户 → None（而不是造一个空壳）
    #[test]
    fn detail_missing_customer_is_none() {
        let db = mem();
        assert!(
            detail(&db, "NOPE", Period::new(2026, 1).unwrap())
                .unwrap()
                .is_none(),
            "不存在的客户应返回 None，界面才能提示「未建档」而不是显示一片空白"
        );
    }

    // ---- 账龄 ----

    fn day_buckets() -> Vec<fincore::engine::aging::AgingBucket> {
        fincore::engine::aging::buckets_by_days()
    }

    /// 账龄与 `settle::aging` 的**那一行**逐档相同。
    ///
    /// 这是「桶定义与算法只有一处」的落点：`aging_of` 只是从全体行里挑出
    /// 属于这个客户的那一行，一分钱都不自己算。
    /// 若它自己按天数分档，这里就会红 —— 而两个数都是「按账龄算的」，
    /// 谁也不会怀疑口径不同。
    #[test]
    fn aging_of_equals_its_row_in_settle_aging() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        mk_cust(&db, "C02", "客户乙");
        ar_posted(&db, "2025-01-10", "C01", "3000", true);
        ar_posted(&db, "2026-01-05", "C01", "500", true);
        ar_posted(&db, "2026-01-06", "C02", "700", true);

        let p = Period::new(2026, 1).unwrap();
        let as_of = d("2026-01-31");
        let all = settle::aging(&db, AR_ACCOUNT, p, as_of, &day_buckets(), None).unwrap();
        let row = all.iter().find(|l| l.key == "C01").expect("C01 应有一行");

        let mine = aging_of(&db, "C01", p, as_of, &day_buckets()).unwrap();
        assert!(!mine.empty, "有未核销余额，不该是 empty");
        assert_eq!(mine.amounts, row.amounts, "各档金额必须逐档相同");
        assert_eq!(mine.total, row.total, "合计必须相同");
        assert_eq!(mine.credit_total, row.credit_total);
        assert_eq!(mine.max_days, row.max_days);
        assert_eq!(
            mine.buckets,
            day_buckets()
                .iter()
                .map(|b| b.label.to_string())
                .collect::<Vec<_>>(),
            "桶标签必须原样透传（桶定义只有一处）"
        );
        // 两笔落在不同的桶里，否则「逐档相同」是个退化断言（全在一档也能通过）
        assert_eq!(
            mine.amounts.iter().filter(|m| !m.is_zero()).count(),
            2,
            "3000（>365天）与 500（<30天）应落在两个不同的桶：{:?} 桶 {:?}",
            mine.amounts,
            mine.buckets
        );
    }

    /// 无未核销余额 → `empty=true` + 形状完整（全 0，不是缺字段）
    ///
    /// 为什么返回全 0 而不是 None：界面要一个稳定形状渲染「无未核销余额」，
    /// 而不是按有无数据换两套模板。改成 None 的话前端就会开始写
    /// `if (!aging) ...` 分支，久了那分支会按别的条件触发。
    #[test]
    fn aging_of_no_balance_is_empty_with_full_shape() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        let p = Period::new(2026, 1).unwrap();
        let a = aging_of(&db, "C01", p, d("2026-01-31"), &day_buckets()).unwrap();
        assert!(a.empty, "无未核销余额 → empty=true");
        assert_eq!(a.total, Money::ZERO);
        assert_eq!(a.credit_total, Money::ZERO);
        assert_eq!(a.max_days, 0);
        assert_eq!(
            a.amounts.len(),
            day_buckets().len(),
            "桶与金额仍要一一对应（全 0），否则前端按桶渲染会拿到长度不一的数组"
        );
        assert!(a.amounts.iter().all(|m| m.is_zero()));
    }

    /// 编码前后空格不该把客户查丢
    ///
    /// 编码是路径参数 `Path(code)`，界面拼 URL 时不会 trim。这里守的是
    /// 「findb 层自己兜住」—— 带空格的编码静默返回全 0，界面上会显示
    /// 「该客户无未核销余额」，而真客户明明欠着钱。
    #[test]
    fn aging_of_trims_customer_code() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        ar_posted(&db, "2026-01-05", "C01", "800", true);
        let p = Period::new(2026, 1).unwrap();
        let a = aging_of(&db, "  C01  ", p, d("2026-01-31"), &day_buckets()).unwrap();
        assert!(!a.empty, "带空格的编码也要查到（否则静默显示「无欠款」）");
        assert_eq!(a.total, m("800"));
    }

    /// 客户编码前缀不能互相命中（`C01` 与 `C011`）
    ///
    /// `aging_of` 用的是 `l.key.trim() == code`（**等值**比较），不是 LIKE。
    /// 若有人图省事改成 `starts_with` / `contains`，这个客户会凭空背上
    /// 别人的欠款 —— 而界面上只会显示一个更大的数字，没人会去查它从哪来。
    ///
    /// 与 `balances.rs` 里记着的教训同源：`%customer=C001%` 会误命中 `C0011`。
    #[test]
    fn aging_of_does_not_match_prefix_of_other_code() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        mk_cust(&db, "C011", "客户乙");
        ar_posted(&db, "2026-01-05", "C011", "700", true);
        let p = Period::new(2026, 1).unwrap();
        let a = aging_of(&db, "C01", p, d("2026-01-31"), &day_buckets()).unwrap();
        assert!(
            a.empty && a.total.is_zero(),
            "C01 不得命中 C011 的 700（等值比较，不是前缀匹配）：{:?}",
            a
        );
    }

    // ---- 交易历史 ----

    /// 交易历史把四类单据（报价/订单/发货/收款）合并、按日期倒序，并严格按客户隔离。
    ///
    /// 发货表 `so_shipment` 只有 `so_id`、没有客户编码，靠 JOIN 订单拿客户 ——
    /// 这条测试钉住那个 JOIN：写错 JOIN 条件时发货行要么消失、要么串到别的客户。
    /// 收款取 `receipt_doc`（会计口径），并与 C02 的订单隔离。
    #[test]
    fn trade_merges_sources_sorted_and_isolated() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        mk_cust(&db, "C02", "客户乙");
        let p1 = Period::new(2026, 1).unwrap();

        db.conn()
            .execute(
                "INSERT INTO quotation(no,period,date,customer_code,customer_name,item_code,item_name,qty,unit_price,status,prepared_by,memo)
                 VALUES('BJ-1',202601,'2026-01-03','C01','客户甲','RM1','氧化料','10','100','approved','u','')",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO sales_order(period,no,date,customer_code,customer_name,status,total_amount,total_tax)
                 VALUES(202601,'XS-1','2026-01-05','C01','客户甲','confirmed','2000','0')",
                [],
            )
            .unwrap();
        let so_id = db.conn().last_insert_rowid();
        db.conn()
            .execute(
                "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,202601,'2026-01-08','5','发一半')",
                rusqlite::params![so_id],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO receipt_doc(no,period,date,kind,fund_account,party,amount,memo,status,created_by,created_at)
                 VALUES('SK-1',202601,'2026-01-10','receipt','100201','C01','800','收到货款','audited','u','2026-01-10 09:00:00')",
                [],
            )
            .unwrap();
        // 干扰：C02 的订单不该出现
        db.conn()
            .execute(
                "INSERT INTO sales_order(period,no,date,customer_code,customer_name,status,total_amount,total_tax)
                 VALUES(202601,'XS-9','2026-01-06','C02','客户乙','confirmed','999','0')",
                [],
            )
            .unwrap();

        let rows = trade(&db, "C01", p1, p1).unwrap();
        let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["receipt", "shipment", "order", "quotation"],
            "应按日期倒序：收款(01-10) > 发货(01-08) > 订单(01-05) > 报价(01-03)"
        );
        assert_eq!(rows[0].amount, m("800"), "收款金额");
        assert_eq!(rows[1].qty, m("5"), "发货行带数量");
        assert!(
            rows.iter().all(|r| r.doc_no != "XS-9"),
            "不得混入 C02 的订单：{:?}",
            rows.iter().map(|r| &r.doc_no).collect::<Vec<_>>()
        );

        // 期间外应为空（防止「忽略 from/to 一律全返回」）
        let p2 = Period::new(2026, 2).unwrap();
        assert!(
            trade(&db, "C01", p2, p2).unwrap().is_empty(),
            "期间外应为空"
        );
    }

    // ---- 催款记录 ----

    /// 催款 Tab 同时收催款单与对账单，且只返回该客户的。
    #[test]
    fn dunning_records_include_dunning_and_statement_for_customer_only() {
        let db = mem();
        mk_cust(&db, "C01", "客户甲");
        mk_cust(&db, "C02", "客户乙");
        let p = Period::new(2026, 1).unwrap();
        ar_posted(&db, "2026-01-05", "C01", "1000", true);
        ar_posted(&db, "2026-01-06", "C02", "300", true);

        settle::dunning_create(&db, "ar", "", "C01", "客户甲", d("2026-01-31"), "催收", "u")
            .unwrap();
        crate::statement::create(&db, "ar", "", "C01", "客户甲", p, p, "", "u").unwrap();
        // 干扰：C02 的催款单不该出现
        settle::dunning_create(&db, "ar", "", "C02", "客户乙", d("2026-01-31"), "催收", "u")
            .unwrap();

        let rows = dunning_records(&db, "C01").unwrap();
        assert_eq!(
            rows.iter().filter(|r| r.kind == "dunning").count(),
            1,
            "只应有 C01 的 1 张催款单：{:?}",
            rows.iter().map(|r| (&r.kind, &r.no)).collect::<Vec<_>>()
        );
        assert_eq!(
            rows.iter().filter(|r| r.kind == "statement").count(),
            1,
            "只应有 C01 的 1 张对账单"
        );
        assert_eq!(
            rows.iter().find(|r| r.kind == "statement").unwrap().amount,
            m("1000"),
            "对账单期末余额应等于 C01 的应收"
        );

        // 没有记录的客户返回空（而不是别人的）
        mk_cust(&db, "C03", "客户丙");
        assert!(dunning_records(&db, "C03").unwrap().is_empty());
    }
}
