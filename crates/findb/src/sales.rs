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
    })
}

const Q_COLS: &str = "id,no,period,date,customer_code,customer_name,item_code,item_name,qty,unit_price,status,prepared_by,memo";

pub fn quo_next_no(db: &Db, period: Period) -> DbResult<String> {
    let prefix = format!("{}{:04}{:02}", crate::doc_prefix(db, "quo", "BJ"), period.year(), period.month());
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
             item_code=?6,item_name=?7,qty=?8,unit_price=?9,status=?10,prepared_by=?11,memo=?12
             WHERE id=?1",
            rusqlite::params![
                q.id, q.period.ymm(), q.date.format("%Y-%m-%d").to_string(),
                q.customer_code, q.customer_name, q.item_code, q.item_name,
                crate::exact_param(q.qty), crate::exact_param(q.unit_price), q.status, q.prepared_by, q.memo
            ],
        )?;
        q.id
    } else {
        db.conn().execute(
            "INSERT INTO quotation(no,period,date,customer_code,customer_name,item_code,item_name,qty,unit_price,status,prepared_by,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![
                q.no, q.period.ymm(), q.date.format("%Y-%m-%d").to_string(),
                q.customer_code, q.customer_name, q.item_code, q.item_name,
                crate::exact_param(q.qty), crate::exact_param(q.unit_price), q.status, q.prepared_by, q.memo
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
        .query_row(&format!("SELECT {Q_COLS} FROM quotation WHERE id=?1"), [id], map_quo)
        .optional()
        .map_err(Into::into)
}

/// 报价单审批：draft → approved
pub fn quo_approve(db: &Db, id: i64) -> DbResult<()> {
    let q = quo_get(db, id)?.ok_or_else(|| fincore::FinError::msg("报价单不存在"))?;
    if q.status != "draft" {
        return Err(fincore::FinError::msg(format!("报价单已{}，不能审批", q.status)).into());
    }
    db.conn().execute(
        "UPDATE quotation SET status='approved' WHERE id=?1",
        [id],
    )?;
    Ok(())
}

/// 报价单转销售订单（approved → converted）：按报价明细生成**草稿**销售订单
/// （税率 0，可在订单明细里再调整；草稿不触发信用检查），并把报价单标记为已转换。
/// 先建订单后改状态；状态并发变化时报错（已生成的订单草稿可在订单列表删除）。
pub fn quo_to_order(db: &Db, id: i64, who: &str) -> DbResult<i64> {
    let q = quo_get(db, id)?
        .ok_or_else(|| fincore::FinError::msg("报价单不存在"))?;
    if q.status != "approved" {
        return Err(fincore::FinError::msg(format!(
            "仅已审批的报价单可转订单（当前：{}）",
            q.status
        ))
        .into());
    }
    let mut so = crate::scm::SalesOrder::new(
        q.period,
        q.date,
        &q.customer_code,
        &q.customer_name,
        who,
    );
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
        return Err(
            fincore::FinError::msg("报价单状态已变化，请刷新后重试").into(),
        );
    }
    // 单据链：报价 → 销售订单（供单据链面板追溯上游）
    crate::docflow::link_add(db, "quote", id, "so", so_id, "报价转订单")?;
    Ok(so_id)
}

// ===========================================================================
// 发货 / 收款 / 退货
// ===========================================================================

pub fn so_shipment_add(db: &Db, so_id: i64, period: Period, date: NaiveDate, qty: Money, memo: &str) -> DbResult<i64> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("发货数量必须为正数").into());
    }
    db.conn().execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![so_id, period.ymm(), date.format("%Y-%m-%d").to_string(), crate::exact_param(qty), memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

/// 销售退货：负发货记录
pub fn so_return_add(db: &Db, so_id: i64, period: Period, date: NaiveDate, qty: Money, memo: &str) -> DbResult<i64> {
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("退货数量必须为正数").into());
    }
    db.conn().execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![so_id, period.ymm(), date.format("%Y-%m-%d").to_string(), qty.negated().to_string(), format!("退货 {}", memo)],
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
    let mut mv = crate::business::StockMove {
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
    crate::business::stock_insert_of(tx, &mut mv)?;
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
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("发货数量必须为正数").into());
    }
    let so = crate::scm::so_get(db, so_id)?
        .ok_or_else(|| fincore::FinError::not_found("销售订单不存在"))?;
    let line = first_so_line(&so)?;
    let item = line.item_code.clone();
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO so_shipment(so_id,period,date,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            so_id,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            crate::exact_param(qty),
            memo
        ],
    )?;
    let rid = tx.last_insert_rowid();
    stock_sale_in(
        &tx,
        &so.no,
        &item,
        qty.negated(),
        period,
        date,
        &format!("销售出库 {}", so.no),
        warehouse,
    )?;
    tx.commit()?;
    Ok(rid)
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
    if qty.is_negative() || qty.is_zero() {
        return Err(fincore::FinError::msg("退货数量必须为正数").into());
    }
    let so = crate::scm::so_get(db, so_id)?
        .ok_or_else(|| fincore::FinError::not_found("销售订单不存在"))?;
    let line = first_so_line(&so)?;
    let item = line.item_code.clone();
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
    Ok(rid)
}

pub fn so_shipment_sum(db: &Db, so_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT qty FROM so_shipment WHERE so_id=?1")?;
    let rows = st
        .query_map([so_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| m(s)).sum())
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
    if matches!(so.status, crate::scm::SoStatus::Draft | crate::scm::SoStatus::Cancelled) {
        return Err(
            fincore::FinError::state("订单未确认，确认后才能发出发货通知").into(),
        );
    }
    let unshipped: Money = so
        .lines
        .iter()
        .map(|l| l.qty_ordered - l.qty_shipped)
        .sum();
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

pub fn so_payment_add(db: &Db, so_id: i64, period: Period, date: NaiveDate, amount: Money, memo: &str) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO so_payment(so_id,period,date,amount,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![so_id, period.ymm(), date.format("%Y-%m-%d").to_string(), crate::money_param(amount), memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn so_payment_sum(db: &Db, so_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT amount FROM so_payment WHERE so_id=?1")?;
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
    let total_qty: Money = so.lines.iter().map(|l| l.qty_ordered).sum();
    if total_qty.is_zero() || (so.total_amount.is_zero() && so.total_tax.is_zero()) {
        return Ok(None);
    }
    let shipped = so_shipment_sum(db, so_id)?;
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
    let biz = db.options().biz_accounts.clone();
    let period = fincore::Period::from_date(date);
    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
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
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    tx.commit()?;
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
        let e = map.entry(o.customer_code.clone()).or_insert_with(|| SalesStat {
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
            ((shipped.abs() * Money::from_i64(100)))
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

/// 客户信用额度（辅助档案 props.credit_limit），0 = 未设额度
pub fn customer_credit_limit(db: &Db, customer_code: &str) -> DbResult<Money> {
    let props: Option<String> = db.conn().query_row(
        "SELECT props_json FROM aux_entity WHERE kind='customer' AND code=?1",
        [customer_code],
        |r| r.get(0),
    ).optional()?;
    let Some(props) = props else { return Ok(Money::ZERO) };
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&props).unwrap_or_default();
    Ok(map.get("credit_limit").map(|s| m(s)).unwrap_or(Money::ZERO))
}

/// 信用检查：某客户**已确认**订单占用（订单总额 − 已收款）是否超额度。
/// 对标金蝶：草稿订单不占用信用（可自由编辑/废弃），确认起才形成承诺；
/// 返回 (累计占用, 信用额度, 是否超限)。
pub fn credit_check(db: &Db, customer_code: &str, period: Period) -> DbResult<(Money, Money, bool)> {
    let limit = customer_credit_limit(db, customer_code)?;
    let orders = crate::scm::so_list(db, period, None)?;
    let mut receivable = Money::ZERO;
    for o in orders
        .iter()
        .filter(|o| {
            o.customer_code == customer_code
                && !matches!(o.status, crate::scm::SoStatus::Cancelled | crate::scm::SoStatus::Draft)
        })
    {
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
            id: 0, no: quo_next_no(&db, p).unwrap(), period: p,
            date: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            customer_code: "C01".into(), customer_name: "客户A".into(),
            item_code: "140501".into(), item_name: "成品".into(),
            qty: m("50"), unit_price: m("20"), status: "draft".into(),
            prepared_by: "张三".into(), memo: String::new(),
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
        let mut so = crate::scm::SalesOrder::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "C01", "客户A", "u");
        so.no = crate::scm::so_next_no(&db, p).unwrap();
        so.lines.push(crate::scm::SoLine {
            id: 0, so_id: 0, item_code: "140501".into(), item_name: "成品".into(),
            qty_ordered: m("100"), qty_shipped: m("0"), unit_price: m("20"),
            tax_rate: m("0"), amount: m("2000"), tax_amount: m("0"), memo: String::new(),
        });
        let so_id = crate::scm::so_save(&db, &mut so).unwrap();
        so_shipment_add(&db, so_id, p, NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(), m("70"), "").unwrap();
        so_return_add(&db, so_id, p, NaiveDate::from_ymd_opt(2026, 1, 12).unwrap(), m("10"), "拒收").unwrap();
        so_payment_add(&db, so_id, p, NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(), m("1200"), "").unwrap();
        assert_eq!(so_shipment_sum(&db, so_id).unwrap(), m("60"));
        assert_eq!(so_payment_sum(&db, so_id).unwrap(), m("1200"));
        let track = so_execution_track(&db, p).unwrap();
        assert_eq!(track[0].shipped_qty, m("60"));
        assert_eq!(track[0].rate, m("60"));
        let stats = sales_stats(&db, p).unwrap();
        assert_eq!(stats[0].amount, m("2000"));
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
