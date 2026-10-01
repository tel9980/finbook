//! 采购/销售深度：暂估、对账、配额、订单变更
//!
//! 对标金蝶/用友供应链。金额一律 TEXT 存储、Rust 侧 Decimal 累加。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}
fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

// ===========================================================================
// 采购暂估
// ===========================================================================

/// 暂估借方科目：存货编码本身在科目表 → 直接当科目用（请购/暂估惯例 140301）；
/// 否则回退账套配置的暂估材料科目（biz_accounts.material）。盘点凭证复用。
pub(crate) fn estimate_account(db: &Db, item: &str) -> String {
    match crate::accounts::chart(db) {
        Ok(ch) if ch.get(item).is_some() => item.to_string(),
        _ => db.options().biz_accounts.material.clone(),
    }
}

/// 暂估分录（条目）：数量科目带数量/单价（与金额自洽）、启用存货辅助的科目带 item
/// （编码即档案值，presence-only 校验通过）；on_debit=false 时金额落在贷方（冲回用）。
/// 盘盈盘亏凭证复用。
pub(crate) fn estimate_item_entry(
    db: &Db,
    item: &str,
    amount: Money,
    memo: &str,
    line: i32,
    on_debit: bool,
) -> fincore::Entry {
    let dr = estimate_account(db, item);
    let mut e = fincore::Entry::new(line, dr.as_str(), memo);
    if on_debit {
        e.debit = amount;
    } else {
        e.credit = amount;
    }
    if let Ok(ch) = crate::accounts::chart(db) {
        if let Some(a) = ch.get(dr.as_str()) {
            if a.aux.list().contains(&fincore::AuxKind::Item) {
                e.aux = fincore::AuxRef {
                    item: Some(item.to_string()),
                    ..Default::default()
                };
            }
            if a.has_qty {
                e.qty = Some(Money::ONE);
                e.price = Some(amount);
            }
        }
    }
    e
}

/// 暂估：入库未到票，先按估计金额挂账。
/// 同事务生成暂估凭证（借 存货材料科目 / 贷 应付账款-订单供应商），返回（暂估 id，凭证 id）。
pub fn po_estimate_add(
    db: &Db,
    po_id: i64,
    period: Period,
    item: &str,
    est_amount: Money,
    who: &str,
) -> DbResult<(i64, i64)> {
    let po = crate::scm::po_get(db, po_id)?
        .ok_or_else(|| fincore::FinError::not_found("采购订单"))?;
    let biz = db.options().biz_accounts.clone();
    let memo = format!("暂估入库 {} {}", po.no, item);
    let dr = estimate_item_entry(db, item, est_amount, memo.as_str(), 1, true);
    let cr = fincore::Entry {
        credit: est_amount,
        aux: fincore::AuxRef {
            supplier: Some(po.supplier_code.clone()),
            ..Default::default()
        },
        ..fincore::Entry::new(2, biz.ap.as_str(), memo.as_str())
    };
    let tx = db.write_tx()?;
    let date = period.first_day();
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = fincore::Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = fincore::VoucherSource::Business;
    v.memo = memo;
    v.push_entry(dr);
    v.push_entry(cr);
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    tx.execute(
        "INSERT INTO po_estimate(po_id,period,item,est_amount,settled) VALUES(?1,?2,?3,?4,0)",
        rusqlite::params![po_id, period.ymm(), item, crate::money_param(est_amount)],
    )?;
    let est_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok((est_id, vid))
}

/// 暂估冲回：发票到票后标记 settled，并同事务生成反向冲回凭证（借 应付 / 贷 存货）。
/// 返回冲回凭证 id；已冲回或并发重复冲回返回 None（幂等）。
pub fn po_estimate_settle(
    db: &Db,
    id: i64,
    date: NaiveDate,
    who: &str,
) -> DbResult<Option<i64>> {
    let row: (i64, String, String, bool) = db
        .conn()
        .query_row(
            "SELECT po_id, item, est_amount, settled FROM po_estimate WHERE id=?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found("暂估记录"))?;
    let (po_id, item, amount_s, settled) = row;
    if settled {
        return Ok(None);
    }
    let po = crate::scm::po_get(db, po_id)?
        .ok_or_else(|| fincore::FinError::not_found("采购订单"))?;
    let amount = Money::parse_or_zero(&amount_s);
    let biz = db.options().biz_accounts.clone();
    let memo = format!("暂估冲回 {} {}", po.no, item);
    let dr_ap = fincore::Entry {
        debit: amount,
        aux: fincore::AuxRef {
            supplier: Some(po.supplier_code.clone()),
            ..Default::default()
        },
        ..fincore::Entry::new(1, biz.ap.as_str(), memo.as_str())
    };
    let cr_item = estimate_item_entry(db, &item, amount, memo.as_str(), 2, false);
    let period = Period::from_date(date);
    let tx = db.write_tx()?;
    let affected = tx.execute(
        "UPDATE po_estimate SET settled=1 WHERE id=?1 AND settled=0",
        [id],
    )?;
    if affected == 0 {
        return Ok(None);
    }
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = fincore::Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = fincore::VoucherSource::Business;
    v.memo = memo;
    v.push_entry(dr_ap);
    v.push_entry(cr_item);
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    tx.commit()?;
    Ok(Some(vid))
}

/// 未冲回的暂估合计
pub fn po_estimate_open_sum(db: &Db, po_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT est_amount FROM po_estimate WHERE po_id=?1 AND settled=0")?;
    let rows = st
        .query_map([po_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| m(s)).sum())
}

/// 某采购订单的暂估明细
#[derive(Clone, Debug, serde::Serialize)]
pub struct PoEstimate {
    pub id: i64,
    pub po_id: i64,
    pub period: i32,
    pub item: String,
    pub est_amount: Money,
    pub settled: bool,
}

/// 列出某采购订单的全部暂估明细
pub fn po_estimate_list(db: &Db, po_id: i64) -> DbResult<Vec<PoEstimate>> {
    let mut st = db.conn().prepare(
        "SELECT id, po_id, period, item, est_amount, settled FROM po_estimate WHERE po_id=?1 ORDER BY id",
    )?;
    let rows = st
        .query_map([po_id], |r| {
            Ok(PoEstimate {
                id: r.get(0)?,
                po_id: r.get(1)?,
                period: r.get(2)?,
                item: r.get(3)?,
                est_amount: m(&r.get::<_, String>(4)?),
                settled: r.get::<_, i64>(5)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ===========================================================================
// 对账
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize)]
pub struct PoRecon {
    pub po_id: i64,
    pub no: String,
    pub supplier: String,
    /// 订单金额
    pub order_amount: Money,
    /// 已付款
    pub paid: Money,
    /// 未付款（应付）
    pub unpaid: Money,
    /// 未冲回暂估
    pub open_estimate: Money,
}

/// 采购对账：订单金额 vs 付款 vs 暂估
pub fn po_reconcile(db: &Db, period: Period) -> DbResult<Vec<PoRecon>> {
    let orders = crate::scm::po_list(db, period, None)?;
    let mut out = Vec::new();
    for o in orders {
        if matches!(o.status, crate::scm::PoStatus::Cancelled) {
            continue;
        }
        let paid = crate::procurement::po_payment_sum(db, o.id)?;
        let est = po_estimate_open_sum(db, o.id)?;
        out.push(PoRecon {
            po_id: o.id,
            no: o.no,
            supplier: o.supplier_name,
            order_amount: o.total_amount,
            paid,
            unpaid: o.total_amount - paid,
            open_estimate: est,
        });
    }
    Ok(out)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoRecon {
    pub so_id: i64,
    pub no: String,
    pub customer: String,
    pub order_amount: Money,
    pub received: Money,
    /// 未收（应收）
    pub unreceived: Money,
}

/// 销售对账：订单金额 vs 收款
pub fn so_reconcile(db: &Db, period: Period) -> DbResult<Vec<SoRecon>> {
    let orders = crate::scm::so_list(db, period, None)?;
    let mut out = Vec::new();
    for o in orders {
        if matches!(o.status, crate::scm::SoStatus::Cancelled) {
            continue;
        }
        let received = crate::sales::so_payment_sum(db, o.id)?;
        out.push(SoRecon {
            so_id: o.id,
            no: o.no,
            customer: o.customer_name,
            order_amount: o.total_amount,
            received,
            unreceived: o.total_amount - received,
        });
    }
    Ok(out)
}

// ===========================================================================
// 配额
// ===========================================================================

/// 供应商配额：设置某期间某供应商某物料的可采购上限
pub fn quota_set(db: &Db, period: Period, supplier: &str, item: &str, quota_qty: Money) -> DbResult<()> {
    if quota_qty < Money::ZERO {
        return Err(fincore::FinError::msg("配额不能为负").into());
    }
    db.conn().execute(
        "INSERT INTO supplier_quota(period,supplier_code,item,quota_qty,used_qty) VALUES(?1,?2,?3,?4,'0')
         ON CONFLICT(period,supplier_code,item) DO UPDATE SET quota_qty=excluded.quota_qty",
        rusqlite::params![period.ymm(), supplier, item, crate::exact_param(quota_qty)],
    )?;
    Ok(())
}

/// 配额占用：采购订单保存后累计已用数量（读改写进同一事务，避免丢更新）
pub fn quota_use(db: &Db, period: Period, supplier: &str, item: &str, qty: Money) -> DbResult<()> {
    let tx = db.write_tx()?;
    let used: Option<String> = tx
        .query_row(
            "SELECT used_qty FROM supplier_quota WHERE period=?1 AND supplier_code=?2 AND item=?3",
            rusqlite::params![period.ymm(), supplier, item],
            |r| r.get(0),
        )
        .optional()?;
    let new_used = used.map(|s| m(&s)).unwrap_or(Money::ZERO) + qty;
    tx.execute(
        "UPDATE supplier_quota SET used_qty=?2 WHERE period=?1 AND supplier_code=?3 AND item=?4",
        rusqlite::params![period.ymm(), crate::exact_param(new_used), supplier, item],
    )?;
    tx.commit()?;
    Ok(())
}

/// 配额检查：某供应商某物料的剩余配额（quota 未设置返回 None）
pub fn quota_remaining(db: &Db, period: Period, supplier: &str, item: &str) -> DbResult<Option<Money>> {
    let row: Option<(String, String)> = db.conn().query_row(
        "SELECT quota_qty, used_qty FROM supplier_quota WHERE period=?1 AND supplier_code=?2 AND item=?3",
        rusqlite::params![period.ymm(), supplier, item],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    match row {
        Some((q, u)) => Ok(Some(m(&q) - m(&u))),
        None => Ok(None),
    }
}

// ===========================================================================
// 订单变更历史
// ===========================================================================

pub fn change_log_add(db: &Db, order_type: &str, order_id: i64, field: &str, old_value: &str, new_value: &str, who: &str) -> DbResult<()> {
    change_log_add_conn(db.conn(), order_type, order_id, field, old_value, new_value, who)
}

/// 连接版（事务内可用）
pub fn change_log_add_conn(
    conn: &rusqlite::Connection,
    order_type: &str,
    order_id: i64,
    field: &str,
    old_value: &str,
    new_value: &str,
    who: &str,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO order_change_log(order_type,order_id,field,old_value,new_value,changed_by,changed_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![order_type, order_id, field, old_value, new_value, who, now()],
    )?;
    Ok(())
}

pub fn change_log_list(db: &Db, order_type: &str, order_id: i64) -> DbResult<Vec<(String, String, String, String, String)>> {
    let mut st = db.conn().prepare(
        "SELECT field, old_value, new_value, changed_by, changed_at FROM order_change_log
         WHERE order_type=?1 AND order_id=?2 ORDER BY id DESC",
    )?;
    let rows = st
        .query_map(rusqlite::params![order_type, order_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn estimate_and_reconcile() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = crate::scm::PurchaseOrder::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "S01", "供应商A", "u");
        po.no = crate::scm::po_next_no(&db, p).unwrap();
        po.lines.push(crate::scm::PoLine {
            id: 0, po_id: 0, item_code: "140301".into(), item_name: "原料".into(),
            qty_ordered: m("100"), qty_received: m("0"), unit_price: m("10"),
            tax_rate: m("0"), amount: m("1000"), tax_amount: m("0"), memo: String::new(),
        });
        let po_id = crate::scm::po_save(&db, &mut po).unwrap();
        // 暂估 800（自动出凭证：借 140301 / 贷 220201 供应商 S01）
        let (est_id, evid) = po_estimate_add(&db, po_id, p, "140301", m("800"), "u").unwrap();
        assert_eq!(po_estimate_open_sum(&db, po_id).unwrap(), m("800"));
        let v = crate::vouchers::get(&db, evid).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "140301");
        assert_eq!(v.entries[0].debit, m("800"));
        assert!(v.entries[0].qty.is_some(), "数量科目应带数量");
        assert_eq!(v.entries[1].account_code, "220201");
        assert_eq!(v.entries[1].aux.supplier.as_deref(), Some("S01"));
        // 冲回 → 反向凭证 + 幂等
        let rvid = po_estimate_settle(
            &db,
            est_id,
            NaiveDate::from_ymd_opt(2026, 1, 20).unwrap(),
            "u",
        )
        .unwrap()
        .unwrap();
        assert_eq!(po_estimate_open_sum(&db, po_id).unwrap(), m("0"));
        let v = crate::vouchers::get(&db, rvid).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "220201");
        assert_eq!(v.entries[0].debit, m("800"));
        assert_eq!(v.entries[1].account_code, "140301");
        assert_eq!(v.entries[1].credit, m("800"));
        assert!(
            po_estimate_settle(&db, est_id, NaiveDate::from_ymd_opt(2026, 1, 21).unwrap(), "u")
                .unwrap()
                .is_none(),
            "重复冲回应幂等"
        );
        // 付款 600 → 对账未付 400
        crate::procurement::po_payment_add(&db, &crate::procurement::PoPayment {
            id: 0, po_id, period: p, date: NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            amount: m("600"), memo: String::new(),
        }).unwrap();
        let r = po_reconcile(&db, p).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].order_amount, m("1000"));
        assert_eq!(r[0].unpaid, m("400"));
    }

    #[test]
    fn quota_tracking() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        quota_set(&db, p, "S01", "140301", m("500")).unwrap();
        assert_eq!(quota_remaining(&db, p, "S01", "140301").unwrap(), Some(m("500")));
        quota_use(&db, p, "S01", "140301", m("120")).unwrap();
        assert_eq!(quota_remaining(&db, p, "S01", "140301").unwrap(), Some(m("380")));
        // 未设置配额的返回 None
        assert_eq!(quota_remaining(&db, p, "S01", "9999").unwrap(), None);
    }

    #[test]
    fn change_log_recorded() {
        let db = mem();
        change_log_add(&db, "po", 1, "status", "draft", "confirmed", "张三").unwrap();
        let log = change_log_list(&db, "po", 1).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "status");
        assert_eq!(log[0].2, "confirmed");
    }
}
