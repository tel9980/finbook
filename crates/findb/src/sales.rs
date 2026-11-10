//! 销售深化：报价单 / 发货 / 收款 / 退货 / 统计 / 执行跟踪 / 信用管理
//!
//! 对标金蝶/用友销售管理。金额一律 TEXT 存储、Rust 侧 Decimal 累加。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 销售报价单
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Quotation {
    /// 0 = 新增（客户端可省略）
    #[serde(default)]
    pub id: i64,
    /// 单号：可由服务端按期间自动生成，允许客户端省略
    #[serde(default)]
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub customer_code: String,
    pub customer_name: String,
    pub item_code: String,
    pub item_name: String,
    pub qty: Money,
    pub unit_price: Money,
    pub status: String, // draft / approved / converted / cancelled
    pub prepared_by: String,
    pub memo: String,
    /// 规格（如 1250*250*3；空 = 不按规格算）
    #[serde(default)]
    pub spec: String,
    /// 计价口径（㎡ / 米 / kg；空 = 按库存单位）
    #[serde(default)]
    pub calc_unit: String,
    /// 工艺档（硬质氧化 …；空 = 不分档）
    #[serde(default)]
    pub process: String,
    /// 折算后的计价数量快照（93.75 ㎡ 这种）
    #[serde(default)]
    pub calc_qty: String,
    /// 用的是哪一条价（0 = 手填或老单子）
    #[serde(default)]
    pub rule_id: i64,
}

impl Default for Quotation {
    fn default() -> Self {
        Quotation {
            id: 0,
            no: String::new(),
            period: Period::from_ymm(0),
            date: NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
            customer_code: String::new(),
            customer_name: String::new(),
            item_code: String::new(),
            item_name: String::new(),
            qty: Money::ZERO,
            unit_price: Money::ZERO,
            status: String::new(),
            prepared_by: String::new(),
            memo: String::new(),
            spec: String::new(),
            calc_unit: String::new(),
            process: String::new(),
            calc_qty: String::new(),
            rule_id: 0,
        }
    }
}

fn map_quo(r: &rusqlite::Row) -> rusqlite::Result<Quotation> {
    Ok(Quotation {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&r.get::<_, String>(3)?, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        customer_code: r.get(4)?,
        customer_name: r.get(5)?,
        item_code: r.get(6)?,
        item_name: r.get(7)?,
        qty: m(&r.get::<_, String>(8)?),
        unit_price: m(&r.get::<_, String>(9)?),
        status: r.get(10)?,
        prepared_by: r.get(11)?,
        memo: r.get(12)?,
        spec: r.get(13)?,
        calc_unit: r.get(14)?,
        process: r.get(15)?,
        calc_qty: r.get(16)?,
        rule_id: r.get(17)?,
    })
}

const Q_COLS: &str = "id,no,period,date,customer_code,customer_name,item_code,item_name,\
     qty,unit_price,status,prepared_by,memo,spec,calc_unit,process,calc_qty,rule_id";

pub fn quo_next_no(db: &Db, period: Period) -> DbResult<String> {
    let prefix = format!(
        "{}{:04}{:02}",
        crate::doc_prefix(db, "quo", "BJ"),
        period.year(),
        period.month()
    );
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM quotation WHERE no LIKE ?1",
        rusqlite::params![format!("{prefix}%")],
        |r| r.get(0),
    )?;
    Ok(format!("{prefix}-{:03}", n + 1))
}

pub fn quo_save(db: &Db, q: &mut Quotation) -> DbResult<i64> {
    let id = if q.id > 0 {
        db.conn().execute(
            "UPDATE quotation SET period=?2,date=?3,customer_code=?4,customer_name=?5,
             item_code=?6,item_name=?7,qty=?8,unit_price=?9,status=?10,prepared_by=?11,memo=?12,
             spec=?13,calc_unit=?14,process=?15,calc_qty=?16,rule_id=?17
             WHERE id=?1",
            rusqlite::params![
                q.id,
                q.period.ymm(),
                q.date.format("%Y-%m-%d").to_string(),
                q.customer_code,
                q.customer_name,
                q.item_code,
                q.item_name,
                crate::exact_param(q.qty),
                crate::exact_param(q.unit_price),
                q.status,
                q.prepared_by,
                q.memo,
                q.spec,
                q.calc_unit,
                q.process,
                q.calc_qty,
                q.rule_id
            ],
        )?;
        q.id
    } else {
        db.conn().execute(
            "INSERT INTO quotation(no,period,date,customer_code,customer_name,item_code,item_name,
             qty,unit_price,status,prepared_by,memo,spec,calc_unit,process,calc_qty,rule_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                q.no,
                q.period.ymm(),
                q.date.format("%Y-%m-%d").to_string(),
                q.customer_code,
                q.customer_name,
                q.item_code,
                q.item_name,
                crate::exact_param(q.qty),
                crate::exact_param(q.unit_price),
                q.status,
                q.prepared_by,
                q.memo,
                q.spec,
                q.calc_unit,
                q.process,
                q.calc_qty,
                q.rule_id
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    q.id = id;
    Ok(id)
}

pub fn quo_list(db: &Db, period: Period) -> DbResult<Vec<Quotation>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {Q_COLS} FROM quotation WHERE period=?1 ORDER BY id DESC"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], map_quo)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn quo_get(db: &Db, id: i64) -> DbResult<Option<Quotation>> {
    db.conn()
        .query_row(
            &format!("SELECT {Q_COLS} FROM quotation WHERE id=?1"),
            [id],
            map_quo,
        )
        .optional()
        .map_err(Into::into)
}

/// 报价单打印行的说明：把「这个价怎么算出来的」拼进明细行的备注里，
/// 客户问「这 0.28 怎么来的」时照着念就行。只讲锁在单子上的快照
/// （规格 / 折合 / 工艺 / 原备注），不讲价目表当时的费率——
/// 那张价后来可能改了，讲出来反而对不上。
pub fn quo_print_memo(q: &Quotation) -> String {
    let mut bits: Vec<String> = Vec::new();
    if !q.spec.trim().is_empty() {
        bits.push(format!("规格 {}", q.spec.trim()));
    }
    if !q.calc_qty.trim().is_empty() && !q.calc_unit.trim().is_empty() {
        bits.push(format!("折合 {} {}", q.calc_qty.trim(), q.calc_unit.trim()));
    }
    if !q.process.trim().is_empty() {
        bits.push(format!("工艺 {}", q.process.trim()));
    }
    if !q.memo.trim().is_empty() {
        bits.push(q.memo.trim().to_string());
    }
    bits.join("；")
}

/// 报价单审批：draft → approved
pub fn quo_approve(db: &Db, id: i64) -> DbResult<()> {
    let q = quo_get(db, id)?.ok_or_else(|| fincore::FinError::msg("报价单不存在"))?;
    if q.status != "draft" {
        return Err(fincore::FinError::msg(format!("报价单已{}，不能审批", q.status)).into());
    }
    db.conn()
        .execute("UPDATE quotation SET status='approved' WHERE id=?1", [id])?;
    Ok(())
}

/// 报价单转销售订单（approved → converted）：按报价明细生成**草稿**销售订单
/// （税率 0，可在订单明细里再调整；草稿不触发信用检查），并把报价单标记为已转换。
/// 先建订单后改状态；状态并发变化时报错（已生成的订单草稿可在订单列表删除）。
pub fn quo_to_order(db: &Db, id: i64, who: &str) -> DbResult<i64> {
    let q = quo_get(db, id)?.ok_or_else(|| fincore::FinError::msg("报价单不存在"))?;
    if q.status != "approved" {
        return Err(fincore::FinError::msg(format!(
            "仅已审批的报价单可转订单（当前：{}）",
            q.status
        ))
        .into());
    }
    let mut so =
        crate::scm::SalesOrder::new(q.period, q.date, &q.customer_code, &q.customer_name, who);
    so.no = crate::scm::so_next_no(db, q.period)?;
    so.memo = format!("由报价单 {} 转入", q.no);
    let amount = (q.qty * q.unit_price).round2();
    so.lines.push(crate::scm::SoLine {
        id: 0,
        so_id: 0,
        item_code: q.item_code.clone(),
        item_name: q.item_name.clone(),
        qty_ordered: q.qty,
        qty_shipped: Money::ZERO,
        unit_price: q.unit_price,
        tax_rate: Money::ZERO,
        amount,
        tax_amount: Money::ZERO,
        memo: String::new(),
    });
    let so_id = crate::scm::so_save(db, &mut so)?;
    let n = db.conn().execute(
        "UPDATE quotation SET status='converted' WHERE id=?1 AND status='approved'",
        [id],
    )?;
    if n == 0 {
        return Err(fincore::FinError::msg("报价单状态已变化，请刷新后重试").into());
    }
    // 单据链：报价 → 销售订单（供单据链面板追溯上游）
    crate::docflow::link_add(db, "quote", id, "so", so_id, "报价转订单")?;
    Ok(so_id)
}

/// 报价单 vs 实际结算对账（按月）：每张报价单一行，已转订单的带上「实际单价」，
/// 差价 = 实际单价 − 报价单价、差额 = 实际金额 − 报价金额。
///
/// 只认**已转换**（converted）的报价单的差价 —— 草稿 / 已审批还没落到订单，谈不上
/// 「结算」；作废的更不参与。实际单价取下游销售订单首行的 `unit_price`（订单在转单
/// 后仍可在明细里调整，那份才是真正卖给客户的价格），金额取该行的 `amount`。
#[derive(Clone, Debug, serde::Serialize)]
pub struct QuoReconcile {
    pub quo_id: i64,
    pub quo_no: String,
    pub quo_date: String,
    pub customer_code: String,
    pub customer_name: String,
    pub item_code: String,
    pub item_name: String,
    pub quo_qty: Money,
    pub quo_price: Money,
    pub quo_amount: Money,
    /// 0 = 还没转订单
    pub so_id: i64,
    pub so_no: String,
    /// 汉化状态（未转订单为空串）
    pub so_status: String,
    pub so_qty: Money,
    pub so_price: Money,
    pub so_amount: Money,
    /// 实际单价 − 报价单价
    pub diff: Money,
    /// 实际金额 − 报价金额
    pub diff_amount: Money,
}

pub fn quo_reconcile(db: &Db, period: Period) -> DbResult<Vec<QuoReconcile>> {
    let quotes = quo_list(db, period)?;
    let mut out: Vec<QuoReconcile> = Vec::with_capacity(quotes.len());
    for q in quotes {
        let quo_amount = (q.qty * q.unit_price).round2();
        let mut row = QuoReconcile {
            quo_id: q.id,
            quo_no: q.no.clone(),
            quo_date: q.date.format("%Y-%m-%d").to_string(),
            customer_code: q.customer_code.clone(),
            customer_name: q.customer_name.clone(),
            item_code: q.item_code.clone(),
            item_name: q.item_name.clone(),
            quo_qty: q.qty,
            quo_price: q.unit_price,
            quo_amount,
            so_id: 0,
            so_no: String::new(),
            so_status: String::new(),
            so_qty: Money::ZERO,
            so_price: Money::ZERO,
            so_amount: Money::ZERO,
            diff: Money::ZERO,
            diff_amount: Money::ZERO,
        };
        if q.status == "converted" {
            let so_id: Option<i64> = db
                .conn()
                .query_row(
                    "SELECT dst_id FROM doc_link WHERE src_type='quote' AND src_id=?1
                     AND dst_type='so' ORDER BY id LIMIT 1",
                    [q.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(so_id) = so_id {
                if let Some(so) = crate::scm::so_get(db, so_id)? {
                    if let Some(line) = so.lines.first() {
                        row.so_id = so.id;
                        row.so_no = so.no.clone();
                        row.so_status = so.status.label().to_string();
                        row.so_qty = line.qty_ordered;
                        row.so_price = line.unit_price;
                        row.so_amount = line.amount;
                        row.diff = line.unit_price - q.unit_price;
                        row.diff_amount = line.amount - quo_amount;
                    }
                }
            }
        }
        out.push(row);
    }
    // 新单在前，与列表同序
    out.sort_by_key(|a| std::cmp::Reverse(a.quo_id));
    Ok(out)
}

// ===========================================================================
// 发货 / 收款 / 退货
// ===========================================================================

pub fn so_shipment_add(
    db: &Db,
    so_id: i64,
    period: Period,
    date: NaiveDate,
    qty: Money,
    memo: &str,
) -> DbResult<i64> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("发货数量必须为正数").into());
    }
    db.conn().execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            so_id,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            crate::exact_param(qty),
            memo
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

/// 销售退货：负发货记录
pub fn so_return_add(
    db: &Db,
    so_id: i64,
    period: Period,
    date: NaiveDate,
    qty: Money,
    memo: &str,
) -> DbResult<i64> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("退货数量必须为正数").into());
    }
    db.conn().execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            so_id,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            qty.negated().to_string(),
            format!("退货 {}", memo)
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

// ---- 发货/退货 × 库存（Web 入口：执行行 + 销售出库流水同事务，与采购侧对称）----

/// 销售出库流水（调用方事务内）：出库负 / 退货正；price=amount=0 ——发出成本由
/// 「销售成本结转」（stock_summary 按计价方法回写）确定，与领料/组装同口径。
fn stock_sale_in(
    tx: &rusqlite::Transaction,
    so_no: &str,
    item: &str,
    qty: Money,
    period: Period,
    date: NaiveDate,
    memo: &str,
    warehouse: &str,
) -> DbResult<()> {
    // 仓库：空 = 默认仓；非空必须存在且未停用（仓库主数据 v30）
    let wh = crate::warehouse::resolve_conn(tx, warehouse)?;
    let mv = crate::business::StockMove {
        id: 0,
        period,
        biz_date: date,
        kind: crate::business::StockKind::Sale,
        item: item.to_string(),
        warehouse: wh,
        batch_no: String::new(),
        qty,
        price: Money::ZERO,
        amount: Money::ZERO,
        voucher_id: None,
        memo: if memo.is_empty() {
            format!("销售出库 {so_no}")
        } else {
            memo.to_string()
        },
    };
    crate::business::stock_insert_of(tx, &mv)?;
    Ok(())
}

fn first_so_line(so: &crate::scm::SalesOrder) -> Result<&crate::scm::SoLine, fincore::FinError> {
    so.lines
        .first()
        .ok_or_else(|| fincore::FinError::msg("销售订单没有明细行，不能出库"))
}

/// 发货：执行行 + 销售出库流水（负数量）**同事务**。
/// 品名取订单首行（多行订单按首行出库）；无明细行拒绝。
pub fn so_shipment_with_stock(
    db: &Db,
    so_id: i64,
    period: Period,
    date: NaiveDate,
    qty: Money,
    memo: &str,
    warehouse: &str,
) -> DbResult<i64> {
    let (rid, _) = so_shipment_all(db, so_id, period, date, qty, memo, warehouse, "")?;
    Ok(rid)
}

/// 发货全链：收入确认凭证 + 执行行 + 销售出库流水，**全部落在同一个事务里**。
///
/// 为什么不拆成两个各自 commit 的函数：早先 Web 入口先调 `so_income_voucher`
/// （自己开事务并提交）再调 `so_shipment_with_stock`（另一个事务）。第二步失败时
/// 收入与应收已经入账并提交，却没有任何发货/出库记录——退货超量、或仓库编码不存在
/// 都会踩到，留下无法自动撤销的孤儿凭证。本函数把三处写入合并，`?` 任意一处失败
/// 全部回滚。
///
/// 返回 `(发货执行行 id, 收入凭证 id 或 None)`。`who` 为空表示不生成收入凭证
/// （保留旧的「只记库存不出凭证」调用方式）。
pub fn so_shipment_all(
    db: &Db,
    so_id: i64,
    period: Period,
    date: NaiveDate,
    qty: Money,
    memo: &str,
    warehouse: &str,
    who: &str,
) -> DbResult<(i64, Option<i64>)> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("发货数量必须为正数").into());
    }
    let so = crate::scm::so_get(db, so_id)?
        .ok_or_else(|| fincore::FinError::not_found("销售订单不存在"))?;
    let item = first_so_line(&so)?.item_code.clone();
    let tx = db.write_tx()?;
    // 有效发货量**只算一次**，收入确认 / 发货流水 / 库存出库三处共用。
    //
    // 回归背景：以前 `so_income_voucher_in` 内部自己封顶（`min(remaining)`），
    // 而 `so_shipment` 插入与 `stock_sale_in` 用的是**原始 qty**。于是订购 10 件、
    // 发 6 再发 6 时：账上只确认 10 件收入，**仓库实际出了 12 件**——账实不符，
    // 而且接口返回 200、不报任何错。金额封顶而实物不封顶是这类链路上最难自查的错，
    // 根治办法是让「能发多少」成为一个数，而不是三处各算一遍。
    //
    // 已发完时保持既有的**静默不确认**语义（返回 200、无凭证、无发货、无出库），
    // 不改成报错：那是产品行为，不在本次修复范围内。截断与否写进审计日志，
    // 便于事后查「为什么发出去的比申请的少」。
    let shipped_now: Money = so_shipment_sum_on(&tx, so_id)?;
    let total_qty: Money = so.lines.iter().map(|l| l.qty_ordered).sum();
    let remaining = total_qty - shipped_now;
    if !remaining.is_positive() {
        db.log(
            who,
            "销售",
            "发货截断",
            &format!(
                "{} 已全部发货（订购 {total_qty}，已发 {shipped_now}），本次请求 {qty} 未执行",
                so.no
            ),
        )?;
        tx.commit()?;
        return Ok((0, None));
    }
    let eff = qty.min(remaining);
    if eff < qty {
        db.log(
            who,
            "销售",
            "发货截断",
            &format!("{} 请求 {qty}，实际发货 {eff}（剩余 {remaining}）", so.no),
        )?;
    }
    let ivid = if who.is_empty() {
        None
    } else {
        so_income_voucher_in(&tx, &so, eff, date, who)?
    };
    tx.execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            so_id,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            crate::exact_param(eff),
            memo
        ],
    )?;
    let rid = tx.last_insert_rowid();
    stock_sale_in(
        &tx,
        &so.no,
        &item,
        eff.negated(),
        period,
        date,
        &format!("销售出库 {}", so.no),
        warehouse,
    )?;
    tx.commit()?;
    Ok((rid, ivid))
}

/// 退货：负执行行 + 销售流水回库（正数量）同事务；**超退防呆**（本次 ≤ 净发货，
/// 净发货 = 发货 − 历史退货，与采购侧 po_receipt_sum 对称）。
pub fn so_return_with_stock(
    db: &Db,
    so_id: i64,
    period: Period,
    date: NaiveDate,
    qty: Money,
    memo: &str,
    warehouse: &str,
) -> DbResult<i64> {
    let (rid, _) = so_return_all(db, so_id, period, date, qty, memo, warehouse, "")?;
    Ok(rid)
}

/// 退货全链：收入冲回凭证 + 退货执行行 + 回库流水，**全部同一事务**。
/// 与 [`so_shipment_all`] 对称，见该函数的「为什么不拆成两个事务」说明。
pub fn so_return_all(
    db: &Db,
    so_id: i64,
    period: Period,
    date: NaiveDate,
    qty: Money,
    memo: &str,
    warehouse: &str,
    who: &str,
) -> DbResult<(i64, Option<i64>)> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("退货数量必须为正数").into());
    }
    let so = crate::scm::so_get(db, so_id)?
        .ok_or_else(|| fincore::FinError::not_found("销售订单不存在"))?;
    let item = first_so_line(&so)?.item_code.clone();
    // 超退校验放在任何写入之前：早先它在第二个事务里，报错时第一个事务的收入
    // 冲回凭证已经提交了
    let shipped = so_shipment_sum(db, so_id)?;
    if qty > shipped {
        return Err(fincore::FinError::state(format!(
            "退货数量 {} 超过净发货 {}",
            qty.fmt_qty(),
            shipped.fmt_qty()
        ))
        .into());
    }
    let tx = db.write_tx()?;
    let ivid = if who.is_empty() {
        None
    } else {
        so_income_voucher_in(&tx, &so, qty.negated(), date, who)?
    };
    tx.execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            so_id,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            qty.negated().to_string(),
            format!("退货 {memo}")
        ],
    )?;
    let rid = tx.last_insert_rowid();
    stock_sale_in(
        &tx,
        &so.no,
        &item,
        qty,
        period,
        date,
        &format!("销售退货 {}", so.no),
        warehouse,
    )?;
    tx.commit()?;
    Ok((rid, ivid))
}

/// 净发货量（连接级）：发货执行行数量之和（退货以负行计入）。
pub fn so_shipment_sum_on(conn: &rusqlite::Connection, so_id: i64) -> DbResult<Money> {
    let mut st = conn.prepare("SELECT qty FROM so_shipment WHERE so_id=?1")?;
    let rows = st
        .query_map([so_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| m(s)).sum())
}

pub fn so_shipment_sum(db: &Db, so_id: i64) -> DbResult<Money> {
    so_shipment_sum_on(db.conn(), so_id)
}

// ---------------- 发货通知（对标金蝶发货通知单） ----------------

/// 发货通知单：订单确认后备货指令；出库后自动完成
#[derive(Clone, Debug, serde::Serialize)]
pub struct ShipNotice {
    pub id: i64,
    pub so_id: i64,
    pub qty: Money,
    pub date: String,
    /// pending / shipped
    pub status: String,
    pub memo: String,
    pub created_by: String,
}

/// 通知列表（待发优先，近期 50 条）
pub fn notice_list(db: &Db) -> DbResult<Vec<ShipNotice>> {
    let mut st = db.conn().prepare(
        "SELECT id,so_id,qty,date,status,memo,created_by FROM ship_notice
         ORDER BY CASE status WHEN 'pending' THEN 0 ELSE 1 END, id DESC LIMIT 50",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(ShipNotice {
                id: r.get(0)?,
                so_id: r.get(1)?,
                qty: m(&r.get::<_, String>(2)?),
                date: r.get(3)?,
                status: r.get(4)?,
                memo: r.get(5)?,
                created_by: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 建发货通知：订单需已确认；数量 ≤ 未发量（Σ行 qty_ordered - qty_shipped）
pub fn notice_create(
    db: &Db,
    so_id: i64,
    qty: Money,
    date: NaiveDate,
    memo: &str,
    who: &str,
) -> DbResult<i64> {
    if !qty.is_positive() {
        return Err(fincore::FinError::msg("通知数量必须大于 0").into());
    }
    let so = crate::scm::so_get(db, so_id)?
        .ok_or_else(|| fincore::FinError::not_found("销售订单不存在"))?;
    if matches!(
        so.status,
        crate::scm::SoStatus::Draft | crate::scm::SoStatus::Cancelled
    ) {
        return Err(fincore::FinError::state("订单未确认，确认后才能发出发货通知").into());
    }
    let unshipped: Money = so.lines.iter().map(|l| l.qty_ordered - l.qty_shipped).sum();
    if qty > unshipped {
        return Err(fincore::FinError::state(format!(
            "通知数量 {} 超过未发量 {}",
            qty.fmt_qty(),
            unshipped.fmt_qty()
        ))
        .into());
    }
    db.conn().execute(
        "INSERT INTO ship_notice(so_id,qty,date,status,memo,created_by,created_at)
         VALUES(?1,?2,?3,'pending',?4,?5,?6)",
        rusqlite::params![
            so_id,
            crate::exact_param(qty),
            date.format("%Y-%m-%d").to_string(),
            memo,
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

/// 发货成功后完成该订单最早一条待发通知（v1：通知=备货指令，出库即完成；
/// 逐条数量对齐留待迭代）。返回是否有通知被完成。
pub fn notice_fulfill_on_shipment(db: &Db, so_id: i64) -> DbResult<bool> {
    let n = db.conn().execute(
        "UPDATE ship_notice SET status='shipped'
         WHERE id = (SELECT id FROM ship_notice WHERE so_id=?1 AND status='pending' ORDER BY id LIMIT 1)",
        [so_id],
    )?;
    Ok(n > 0)
}

pub fn so_payment_add(
    db: &Db,
    so_id: i64,
    period: Period,
    date: NaiveDate,
    amount: Money,
    memo: &str,
) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO so_payment(so_id,period,date,amount,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            so_id,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            crate::money_param(amount),
            memo
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn so_payment_sum(db: &Db, so_id: i64) -> DbResult<Money> {
    let mut st = db
        .conn()
        .prepare("SELECT amount FROM so_payment WHERE so_id=?1")?;
    let rows = st
        .query_map([so_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| m(s)).sum())
}

// ===========================================================================
// 统计 / 执行跟踪 / 信用管理

/// 发货 / 退货 → 收入确认凭证（对标金蝶：出库时点确认收入与应收）
///
/// 按订单累计口径**按比例确认**：确认额 = 订单(不含税收入 / 税额) × (本次数量 ÷ 订购总数量)；
/// 发货封顶到未发货余量（超发不重复确认），退货封顶到已发货量（不超额冲回）。
/// 科目取账套 `biz_accounts`（应收/收入/销项税），应收 = 收入 + 税额（借贷必平）。
/// 金额为零或订单无明细 → `Ok(None)`（不生成凭证）。
pub fn so_income_voucher(
    db: &Db,
    so_id: i64,
    delta_qty: Money,
    date: NaiveDate,
    who: &str,
) -> DbResult<Option<i64>> {
    let Some(so) = crate::scm::so_get(db, so_id)? else {
        return Err(fincore::FinError::not_found("销售订单").into());
    };
    let tx = db.write_tx()?;
    let vid = so_income_voucher_in(&tx, &so, delta_qty, date, who)?;
    tx.commit()?;
    Ok(vid)
}

/// [`so_income_voucher`] 的事务内版本：写入调用方的事务，**不自行 commit**。
///
/// 供 [`so_shipment_all`] / [`so_return_all`] 把「收入确认 + 库存流水」合成单事务；
/// 直接调本函数时调用方负责 commit。
pub fn so_income_voucher_in(
    tx: &rusqlite::Transaction,
    so: &crate::scm::SalesOrder,
    delta_qty: Money,
    date: NaiveDate,
    who: &str,
) -> DbResult<Option<i64>> {
    let total_qty: Money = so.lines.iter().map(|l| l.qty_ordered).sum();
    if total_qty.is_zero() || (so.total_amount.is_zero() && so.total_tax.is_zero()) {
        return Ok(None);
    }
    let shipped = so_shipment_sum_on(tx, so.id)?;
    let eff = if delta_qty.is_positive() {
        let remaining = total_qty - shipped;
        if !remaining.is_positive() {
            return Ok(None); // 已发完，超发不重复确认
        }
        delta_qty.min(remaining)
    } else {
        let back = delta_qty.abs().min(shipped);
        if !back.is_positive() {
            return Ok(None); // 未发过货，无从冲回
        }
        back.negated()
    };
    let ratio = eff
        .checked_div(total_qty)
        .ok_or_else(|| fincore::FinError::state("订购总量为零"))?;
    let income = (so.total_amount * ratio).round2();
    let tax = (so.total_tax * ratio).round2();
    let ar = income + tax;
    if income.is_zero() && tax.is_zero() {
        return Ok(None);
    }
    let ret = eff.is_negative();
    let biz = crate::options_of(tx).biz_accounts.clone();
    let period = fincore::Period::from_date(date);
    let no = crate::vouchers::next_no_of(tx, period, "记")?;
    let mut v = fincore::Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = fincore::VoucherSource::Business;
    v.memo = if ret {
        format!("销售退货冲回 {}", so.no)
    } else {
        format!("发货确认 {}", so.no)
    };
    let memo = v.memo.clone();
    let ar_aux = fincore::AuxRef {
        customer: Some(so.customer_code.clone()),
        ..Default::default()
    };
    if !ret {
        v.push_entry(fincore::Entry {
            debit: ar,
            aux: ar_aux,
            ..fincore::Entry::new(1, biz.ar.as_str(), memo.as_str())
        });
        v.push_entry(fincore::Entry {
            credit: income,
            ..fincore::Entry::new(2, biz.income.as_str(), memo.as_str())
        });
        if !tax.is_zero() {
            v.push_entry(fincore::Entry {
                credit: tax,
                ..fincore::Entry::new(3, biz.tax_sales.as_str(), memo.as_str())
            });
        }
    } else {
        v.push_entry(fincore::Entry {
            debit: income.abs(),
            ..fincore::Entry::new(1, biz.income.as_str(), memo.as_str())
        });
        if !tax.is_zero() {
            v.push_entry(fincore::Entry {
                debit: tax.abs(),
                ..fincore::Entry::new(2, biz.tax_sales.as_str(), memo.as_str())
            });
        }
        v.push_entry(fincore::Entry {
            credit: ar.abs(),
            aux: ar_aux,
            ..fincore::Entry::new(3, biz.ar.as_str(), memo.as_str())
        });
    }
    v.renumber();
    let vid = crate::vouchers::save_in(tx, &mut v)?;
    Ok(Some(vid))
}
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize)]
pub struct SalesStat {
    pub customer_code: String,
    pub customer_name: String,
    pub order_count: i64,
    pub amount: Money,
}

pub fn sales_stats(db: &Db, period: Period) -> DbResult<Vec<SalesStat>> {
    let orders = crate::scm::so_list(db, period, None)?;
    let mut map: std::collections::BTreeMap<String, SalesStat> = std::collections::BTreeMap::new();
    for o in orders {
        let e = map
            .entry(o.customer_code.clone())
            .or_insert_with(|| SalesStat {
                customer_code: o.customer_code.clone(),
                customer_name: o.customer_name.clone(),
                order_count: 0,
                amount: Money::ZERO,
            });
        e.order_count += 1;
        e.amount += o.total_amount;
    }
    Ok(map.into_values().collect())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoTrack {
    pub so_id: i64,
    pub no: String,
    pub customer_name: String,
    pub ordered_qty: Money,
    pub shipped_qty: Money,
    pub rate: Money,
}

pub fn so_execution_track(db: &Db, period: Period) -> DbResult<Vec<SoTrack>> {
    let orders = crate::scm::so_list(db, period, None)?;
    let mut out = Vec::new();
    for o in orders {
        if matches!(o.status, crate::scm::SoStatus::Cancelled) {
            continue;
        }
        let ordered: Money = o.lines.iter().map(|l| l.qty_ordered).sum();
        let shipped = so_shipment_sum(db, o.id)?;
        let rate = if ordered.is_zero() {
            Money::ZERO
        } else {
            (shipped.abs() * Money::from_i64(100))
                .checked_div(ordered.abs().inner())
                .expect("ordered 已判非零")
                .round2()
        };
        out.push(SoTrack {
            so_id: o.id,
            no: o.no,
            customer_name: o.customer_name,
            ordered_qty: ordered,
            shipped_qty: shipped,
            rate,
        });
    }
    Ok(out)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoReceivable {
    pub so_id: i64,
    pub no: String,
    pub date: String,
    pub customer_code: String,
    pub customer_name: String,
    pub status: String,
    pub total_amount: Money,
    pub collected: Money,
    pub balance: Money,
    pub pay_days: i64,
    pub overdue_days: i64,
}

/// 谁还欠我钱：所有未作废、未结清的销售订单，按未收金额从大到小排。
///
/// 跨期间——上个月的单还没收完，这个月也要列出来；草稿订单不算数（还没承诺）。
///
/// `overdue_days` 以**订单日期**起算账期（近似口径）：`max(0, 今天 − 订单日期 − 账期)`。
/// 真正的到期日是「发货/开票日 + 账期」，订单日期是手里最接近的起点。
pub fn so_receivable(db: &Db) -> DbResult<Vec<SoReceivable>> {
    so_receivable_as_of(db, chrono::Local::now().date_naive())
}

/// 内部：以指定「今天」计算账期（测试传固定日期，避免依赖机器时钟）。
fn so_receivable_as_of(db: &Db, today: chrono::NaiveDate) -> DbResult<Vec<SoReceivable>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, no, date, customer_code, customer_name, status, total_amount
         FROM sales_order
         WHERE status NOT IN ('Cancelled','Draft')
         ORDER BY date DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            Money::parse_or_zero(&r.get::<_, String>(6)?),
        ))
    })?;
    let pay_days_map = crate::settle::customer_pay_days_map(db)?;
    let mut out = Vec::new();
    for row in rows {
        let (id, no, date, cc, cn, status, total) = row?;
        let collected = so_payment_sum(db, id)?;
        let balance = (total - collected).round2();
        if balance.is_zero() {
            continue; // 已经结清，不进「谁还欠我钱」这张表
        }
        let pay_days = pay_days_map.get(cc.trim()).copied().unwrap_or(0);
        let age = chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
            .map(|d| (today - d).num_days())
            .unwrap_or(0);
        let overdue_days = (age - pay_days).max(0);
        out.push(SoReceivable {
            so_id: id,
            no,
            date,
            customer_code: cc,
            customer_name: cn,
            status: crate::scm::status_from::<crate::scm::SoStatus>(&status)
                .map(|s| s.label().to_string())
                .unwrap_or(status),
            total_amount: total,
            collected,
            balance,
            pay_days,
            overdue_days,
        });
    }
    out.sort_by(|a, b| {
        b.balance
            .cmp(&a.balance)
            .then_with(|| b.so_id.cmp(&a.so_id))
    });
    Ok(out)
}

/// 客户信用额度（辅助档案 props.credit_limit），0 = 未设额度
pub fn customer_credit_limit(db: &Db, customer_code: &str) -> DbResult<Money> {
    let props: Option<String> = db
        .conn()
        .query_row(
            "SELECT props_json FROM aux_entity WHERE kind='customer' AND code=?1",
            [customer_code],
            |r| r.get(0),
        )
        .optional()?;
    let Some(props) = props else {
        return Ok(Money::ZERO);
    };
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&props).unwrap_or_default();
    Ok(map.get("credit_limit").map(|s| m(s)).unwrap_or(Money::ZERO))
}

/// 信用检查：某客户**已确认**订单占用（订单总额 − 已收款）是否超额度。
/// 对标金蝶：草稿订单不占用信用（可自由编辑/废弃），确认起才形成承诺；
/// 返回 (累计占用, 信用额度, 是否超限)。
pub fn credit_check(
    db: &Db,
    customer_code: &str,
    period: Period,
) -> DbResult<(Money, Money, bool)> {
    let limit = customer_credit_limit(db, customer_code)?;
    let orders = crate::scm::so_list(db, period, None)?;
    let mut receivable = Money::ZERO;
    for o in orders.iter().filter(|o| {
        o.customer_code == customer_code
            && !matches!(
                o.status,
                crate::scm::SoStatus::Cancelled | crate::scm::SoStatus::Draft
            )
    }) {
        receivable += o.total_amount;
        receivable -= so_payment_sum(db, o.id)?;
    }
    let over = !limit.is_zero() && receivable > limit;
    Ok((receivable, limit, over))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn quotation_flow() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut q = Quotation {
            id: 0,
            no: quo_next_no(&db, p).unwrap(),
            period: p,
            date: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            customer_code: "C01".into(),
            customer_name: "客户A".into(),
            item_code: "140501".into(),
            item_name: "成品".into(),
            qty: m("50"),
            unit_price: m("20"),
            status: "draft".into(),
            prepared_by: "张三".into(),
            memo: String::new(),
            ..Default::default()
        };
        let id = quo_save(&db, &mut q).unwrap();
        quo_approve(&db, id).unwrap();
        assert_eq!(quo_get(&db, id).unwrap().unwrap().status, "approved");
        assert_eq!(quo_list(&db, p).unwrap().len(), 1);
    }

    #[test]
    fn shipment_payment_track() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut so = crate::scm::SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "C01",
            "客户A",
            "u",
        );
        so.no = crate::scm::so_next_no(&db, p).unwrap();
        so.lines.push(crate::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: "140501".into(),
            item_name: "成品".into(),
            qty_ordered: m("100"),
            qty_shipped: m("0"),
            unit_price: m("20"),
            tax_rate: m("0"),
            amount: m("2000"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let so_id = crate::scm::so_save(&db, &mut so).unwrap();
        so_shipment_add(
            &db,
            so_id,
            p,
            NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
            m("70"),
            "",
        )
        .unwrap();
        so_return_add(
            &db,
            so_id,
            p,
            NaiveDate::from_ymd_opt(2026, 1, 12).unwrap(),
            m("10"),
            "拒收",
        )
        .unwrap();
        so_payment_add(
            &db,
            so_id,
            p,
            NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            m("1200"),
            "",
        )
        .unwrap();
        assert_eq!(so_shipment_sum(&db, so_id).unwrap(), m("60"));
        assert_eq!(so_payment_sum(&db, so_id).unwrap(), m("1200"));
        let track = so_execution_track(&db, p).unwrap();
        assert_eq!(track[0].shipped_qty, m("60"));
        assert_eq!(track[0].rate, m("60"));
        let stats = sales_stats(&db, p).unwrap();
        assert_eq!(stats[0].amount, m("2000"));
    }

    #[test]
    fn so_receivable_lists_unsettled_orders_by_balance() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 一张已确认、收了 1200 还欠 800 的单
        let mut so1 = crate::scm::SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "C01",
            "客户A",
            "u",
        );
        so1.no = crate::scm::so_next_no(&db, p).unwrap();
        so1.lines.push(crate::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: "140501".into(),
            item_name: "成品".into(),
            qty_ordered: m("100"),
            qty_shipped: m("0"),
            unit_price: m("20"),
            tax_rate: m("0"),
            amount: m("2000"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let so1_id = crate::scm::so_save(&db, &mut so1).unwrap();
        crate::scm::so_set_status(&db, so1_id, crate::scm::SoStatus::Confirmed).unwrap();
        so_payment_add(
            &db,
            so1_id,
            p,
            NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            m("1200"),
            "",
        )
        .unwrap();

        // 一张已确认、一分没收的单（欠 500）
        let mut so2 = crate::scm::SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
            "C02",
            "客户B",
            "u",
        );
        so2.no = crate::scm::so_next_no(&db, p).unwrap();
        so2.lines.push(crate::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: "140502".into(),
            item_name: "配件".into(),
            qty_ordered: m("10"),
            qty_shipped: m("0"),
            unit_price: m("50"),
            tax_rate: m("0"),
            amount: m("500"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let so2_id = crate::scm::so_save(&db, &mut so2).unwrap();
        crate::scm::so_set_status(&db, so2_id, crate::scm::SoStatus::Confirmed).unwrap();

        // 一张作废（不算）+ 一张草稿（不承诺、不算）
        let mut so3 = crate::scm::SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 7).unwrap(),
            "C03",
            "客户C",
            "u",
        );
        so3.no = crate::scm::so_next_no(&db, p).unwrap();
        so3.lines.push(crate::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: "140503".into(),
            item_name: "废件".into(),
            qty_ordered: m("1"),
            qty_shipped: m("0"),
            unit_price: m("999"),
            tax_rate: m("0"),
            amount: m("999"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let so3_id = crate::scm::so_save(&db, &mut so3).unwrap();
        crate::scm::so_set_status(&db, so3_id, crate::scm::SoStatus::Cancelled).unwrap();

        let mut so4 = crate::scm::SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 8).unwrap(),
            "C04",
            "客户D",
            "u",
        );
        so4.no = crate::scm::so_next_no(&db, p).unwrap();
        so4.lines.push(crate::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: "140504".into(),
            item_name: "草稿件".into(),
            qty_ordered: m("1"),
            qty_shipped: m("0"),
            unit_price: m("888"),
            tax_rate: m("0"),
            amount: m("888"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        crate::scm::so_save(&db, &mut so4).unwrap(); // 留在 Draft

        // 一张已确认、但钱已经收齐的单（欠 0，不该出现在「谁还欠我钱」里）
        let mut so5 = crate::scm::SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 9).unwrap(),
            "C05",
            "客户E",
            "u",
        );
        so5.no = crate::scm::so_next_no(&db, p).unwrap();
        so5.lines.push(crate::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: "140505".into(),
            item_name: "已结清件".into(),
            qty_ordered: m("10"),
            qty_shipped: m("0"),
            unit_price: m("30"),
            tax_rate: m("0"),
            amount: m("300"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let so5_id = crate::scm::so_save(&db, &mut so5).unwrap();
        crate::scm::so_set_status(&db, so5_id, crate::scm::SoStatus::Confirmed).unwrap();
        so_payment_add(
            &db,
            so5_id,
            p,
            NaiveDate::from_ymd_opt(2026, 1, 20).unwrap(),
            m("300"),
            "",
        )
        .unwrap();

        // C01 账期 30 天：账期内的欠款不算逾期；C02 没设账期按 0 天（到期即逾期）
        let mut c01 = fincore::AuxEntity::new(fincore::AuxKind::Customer, "C01", "客户A");
        c01.set_prop(fincore::auxiliary::prop::PAY_DAYS, "30");
        crate::auxs::insert(&db, &c01).unwrap();

        // 固定「今天」= 2026-02-10：C01 订单 01-05（隔 36 天，扣账期 30 → 逾期 6 天），
        // C02 订单 01-06（隔 35 天，无账期 → 逾期 35 天）
        let today = NaiveDate::from_ymd_opt(2026, 2, 10).unwrap();
        let rows = so_receivable_as_of(&db, today).unwrap();
        assert_eq!(rows.len(), 2, "只列已确认未结清的两张：{rows:?}");
        assert_eq!(rows[0].customer_code, "C01", "欠得多的排前面");
        assert_eq!(rows[0].total_amount, m("2000"));
        assert_eq!(rows[0].collected, m("1200"));
        assert_eq!(rows[0].balance, m("800"));
        assert_eq!(rows[0].status, "已确认");
        assert_eq!(rows[0].pay_days, 30);
        assert_eq!(rows[0].overdue_days, 6, "36 天 − 账期 30 = 逾期 6 天");
        assert_eq!(rows[1].customer_code, "C02");
        assert_eq!(rows[1].balance, m("500"));
        assert_eq!(rows[1].pay_days, 0, "没设账期按 0 天");
        assert_eq!(rows[1].overdue_days, 35, "35 天 − 账期 0 = 逾期 35 天");
    }

    #[test]
    fn so_receivable_does_not_flag_orders_within_pay_days() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 账期 30 天，订单日期是「今天」前 2 天 → 账龄 2 天 < 账期 30 → 不算逾期
        let mut c = fincore::AuxEntity::new(fincore::AuxKind::Customer, "C06", "客户F");
        c.set_prop(fincore::auxiliary::prop::PAY_DAYS, "30");
        crate::auxs::insert(&db, &c).unwrap();
        let mut so = crate::scm::SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 2, 8).unwrap(),
            "C06",
            "客户F",
            "u",
        );
        so.no = crate::scm::so_next_no(&db, p).unwrap();
        so.lines.push(crate::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: "140506".into(),
            item_name: "新品".into(),
            qty_ordered: m("5"),
            qty_shipped: m("0"),
            unit_price: m("20"),
            tax_rate: m("0"),
            amount: m("100"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let so_id = crate::scm::so_save(&db, &mut so).unwrap();
        crate::scm::so_set_status(&db, so_id, crate::scm::SoStatus::Confirmed).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 2, 10).unwrap();
        let rows = so_receivable_as_of(&db, today).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pay_days, 30);
        assert_eq!(rows[0].overdue_days, 0, "账龄 2 天在账期 30 天内，不算逾期");
    }

    #[test]
    fn quo_print_memo_summarizes_the_locked_snapshot() {
        let q = Quotation {
            item_code: "140501".into(),
            item_name: "成品".into(),
            spec: "1250*250*3".into(),
            calc_qty: "7.02".into(),
            calc_unit: "㎡".into(),
            process: "硬质氧化".into(),
            memo: "加急".into(),
            ..Default::default()
        };
        let s = quo_print_memo(&q);
        assert!(s.contains("规格 1250*250*3"), "缺规格：{s}");
        assert!(s.contains("折合 7.02 ㎡"), "缺折合：{s}");
        assert!(s.contains("工艺 硬质氧化"), "缺工艺：{s}");
        assert!(s.contains("加急"), "缺原备注：{s}");
        // 空字段一律不进说明，全空就是空串（老单子/手填价）
        let empty = Quotation {
            item_code: "140501".into(),
            ..Default::default()
        };
        assert_eq!(quo_print_memo(&empty), "");
    }

    #[test]
    fn quotation_reconcile_flags_price_drift() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 报价单 1：0.28 一件 × 300 → 转订单后被改成 0.31
        let mut q1 = Quotation {
            id: 0,
            no: quo_next_no(&db, p).unwrap(),
            period: p,
            date: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            customer_code: "C01".into(),
            customer_name: "客户甲".into(),
            item_code: "MB01".into(),
            item_name: "铝件".into(),
            qty: m("300"),
            unit_price: m("0.28"),
            status: "draft".into(),
            prepared_by: "张三".into(),
            memo: String::new(),
            ..Default::default()
        };
        let q1_id = quo_save(&db, &mut q1).unwrap();
        quo_approve(&db, q1_id).unwrap();
        let so_id = quo_to_order(&db, q1_id, "张三").unwrap();
        let mut so = crate::scm::so_get(&db, so_id).unwrap().unwrap();
        so.lines[0].unit_price = m("0.31");
        so.lines[0].amount = (so.lines[0].qty_ordered * so.lines[0].unit_price).round2();
        crate::scm::so_save(&db, &mut so).unwrap();

        // 报价单 2：已审批但还没转订单
        let mut q2 = Quotation {
            id: 0,
            no: quo_next_no(&db, p).unwrap(),
            period: p,
            date: NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
            customer_code: "C02".into(),
            customer_name: "客户乙".into(),
            item_code: "MB02".into(),
            item_name: "钢件".into(),
            qty: m("100"),
            unit_price: m("0.50"),
            status: "approved".into(),
            prepared_by: "张三".into(),
            memo: String::new(),
            ..Default::default()
        };
        quo_save(&db, &mut q2).unwrap();

        let rows = quo_reconcile(&db, p).unwrap();
        assert_eq!(rows.len(), 2);
        // 新单在前
        let r2 = &rows[0];
        assert_eq!(r2.quo_no, q2.no);
        assert_eq!(r2.so_id, 0);
        assert!(r2.diff.is_zero() && r2.diff_amount.is_zero());
        let r1 = &rows[1];
        assert_eq!(r1.quo_no, q1.no);
        assert_eq!(r1.so_id, so_id);
        assert_eq!(r1.quo_price, m("0.28"));
        assert_eq!(r1.so_price, m("0.31"));
        assert_eq!(r1.diff, m("0.03"));
        assert_eq!(r1.quo_amount, m("84.00"));
        assert_eq!(r1.so_amount, m("93.00"));
        assert_eq!(r1.diff_amount, m("9.00"));
    }

    #[test]
    fn credit_check_basic() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 未设额度 → 不超限
        let (recv, limit, over) = credit_check(&db, "C01", p).unwrap();
        assert_eq!(limit, m("0"));
        assert_eq!(recv, m("0"));
        assert!(!over);
    }
}
