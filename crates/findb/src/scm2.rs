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
    let po =
        crate::scm::po_get(db, po_id)?.ok_or_else(|| fincore::FinError::not_found("采购订单"))?;
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
    // 暂估封顶：同一 PO + 物料的**未结算**暂估累计，不得超过该 PO 的累计到货金额。
    //
    // 回归背景：`po_estimate` 表只有普通索引、没有唯一约束，金额又全靠手填，
    // 所以同一批货能被反复登记暂估 —— 每登记一次就生成一张「借 存货 / 贷 应付」
    // 的凭证，应付与存货双双虚增，且没有任何一处会报错。这不是「数据脏」，
    // 是账实可以凭空做出来。
    //
    // 销售侧有对等守卫（`scm::prod_from_so` 封顶在「订购 − 已发 − 已下推」），
    // 采购侧此前**完全裸奔**。对标 ERPNext / Odoo 的 received vs billed 一致性检查。
    //
    // 口径直接复用 `scm::po_received_gross`（价税合计 × 已收/订购，含税），
    // 不另造算法 —— 两处口径一旦分叉，守卫就会拿错误的额度去拦正确的操作。
    if !est_amount.is_positive() {
        return Err(fincore::FinError::msg("暂估金额必须为正数").into());
    }
    // 额度统一走 `estimate_cap_for_line` —— 守卫与界面查询共用同一份算法。
    // 两处各写一遍必然分叉，而分叉的后果是「界面显示还能登 800，一点就被拒」。
    let cap = estimate_cap_for_line(db, &po, item)?;

    let mut st2 = db.conn().prepare(
        "SELECT est_amount FROM po_estimate WHERE po_id=?1 AND item=?2 AND settled=0 ORDER BY id",
    )?;
    let open_est = st2
        .query_map(rusqlite::params![po_id, item], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .fold(fincore::Money::ZERO, |acc, a| {
            acc + fincore::Money::parse_or_zero(a)
        });
    if open_est + est_amount > cap {
        return Err(fincore::FinError::state(format!(
            "暂估金额超出可暂估额度：本次 {est_amount}，本物料未结算暂估累计 {open_est}，\
             但累计到货金额只有 {cap}。\n\
             （到货金额按订单价税合计 × 已收/订购推导，含税。\
             暂估不得超过实收 —— 否则应付与存货会凭空多出一截。）"
        ))
        .into());
    }
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

/// 某一行的**可暂估额度**（含税），即「该物料累计到货金额」。
///
/// 口径：
/// - 有该物料的**分行**收货记录（`po_receipt.item_code = 物料`）时，用
///   **该行含税单价 × 该行实收量** —— 精确。
/// - 只有整单收货记录（`item_code = ''`，旧数据或调用方未指定行）时，退回
///   `po_received_gross` × 本行订购占比的折算。
///
/// 折算为什么不能作主口径：两张行各 100 件、含税单价 11，实收 100 件（全是第一行），
/// 折算会摊成 550，而实际是 1100 —— 差一半。而这个额度同时是暂估封顶的依据，
/// 折算错就等于守卫拿错额度去拦正确的操作。
///
/// 守卫与界面查询**共用这一个函数**：两处各写一遍必然分叉，分叉的后果是
/// 「界面显示还能登 800，用户一点就被拒」。
pub fn estimate_cap_for_line(
    db: &Db,
    po: &crate::scm::PurchaseOrder,
    item: &str,
) -> DbResult<fincore::Money> {
    let line = po
        .lines
        .iter()
        .find(|l| l.item_code == item)
        .ok_or_else(|| fincore::FinError::msg(format!("采购订单里没有物料 {item}")))?;
    // 金额/数量列一律 TEXT 存储：不在 SQL 里 SUM（SQLite 对 TEXT 的 SUM 返回
    // Real，精度与类型都不可靠），取回后在 Money 里累加。
    //
    // 实收量 = 分行收货（本物料）+ 整单收货（item_code=''，旧数据 / 未指定行）。
    // 两者都算，否则旧账套升级后额度会突然变 0，把合法的暂估全挡掉。
    let rows: Vec<String> = {
        let conn = db.conn();
        let mut st = conn.prepare(
            "SELECT qty FROM po_receipt
             WHERE po_id=?1 AND (item_code=?2 OR item_code='') ORDER BY id",
        )?;
        let v = st
            .query_map(rusqlite::params![po.id, item], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    let recv_qty = rows.iter().fold(fincore::Money::ZERO, |acc, q| {
        acc + fincore::Money::parse_or_zero(q)
    });
    let has_line_receipt: bool = db.conn().query_row(
        "SELECT COUNT(*) FROM po_receipt WHERE po_id=?1 AND item_code=?2",
        rusqlite::params![po.id, item],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if has_line_receipt {
        Ok(line
            .amount
            .checked_add(line.tax_amount)
            .and_then(|g| g.checked_div(line.qty_ordered))
            .map(|u| u * recv_qty)
            .unwrap_or(fincore::Money::ZERO))
    } else {
        let gross = crate::scm::po_received_gross(po, recv_qty);
        let ordered_all: fincore::Money = po.lines.iter().map(|l| l.qty_ordered).sum();
        Ok(gross
            .checked_div(ordered_all)
            .map(|u| u * line.qty_ordered)
            .unwrap_or(fincore::Money::ZERO))
    }
}

/// 一行物料的暂估状态：可暂估额度 / 已暂估 / 未暂估 / 收货与欠收。
///
/// 界面用它替代「让用户手填金额再试错」：三笔数摆在面前，用户自己判断。
#[derive(Clone, Debug, serde::Serialize)]
pub struct EstimateLine {
    pub item_code: String,
    pub item_name: String,
    /// 累计到货金额（含税）= 可暂估额度上限
    pub cap: fincore::Money,
    /// 未结算暂估累计
    pub estimated: fincore::Money,
    /// 还能登的（cap − estimated，负数归零）
    pub remaining: fincore::Money,
    pub qty_ordered: fincore::Money,
    /// 累计收货量（分行 + 整单折算口径，与 cap 同源）
    pub qty_received: fincore::Money,
    /// 欠收量 = 订购 − 收货
    pub qty_outstanding: fincore::Money,
}

/// 逐行列出该 PO 的暂估状态（收货 → 暂估的联动界面靠它）
pub fn estimate_status_by_line(db: &Db, po_id: i64) -> DbResult<Vec<EstimateLine>> {
    let po =
        crate::scm::po_get(db, po_id)?.ok_or_else(|| fincore::FinError::msg("采购订单不存在"))?;
    let mut out = Vec::new();
    for line in &po.lines {
        let cap = estimate_cap_for_line(db, &po, &line.item_code)?;
        let rows: Vec<String> = {
            let conn = db.conn();
            let mut st = conn.prepare(
                "SELECT est_amount FROM po_estimate
                 WHERE po_id=?1 AND item=?2 AND settled=0 ORDER BY id",
            )?;
            let v = st
                .query_map(rusqlite::params![po_id, line.item_code], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        let estimated = rows.iter().fold(fincore::Money::ZERO, |acc, a| {
            acc + fincore::Money::parse_or_zero(a)
        });
        let recv_rows: Vec<String> = {
            let conn = db.conn();
            let mut st = conn.prepare(
                "SELECT qty FROM po_receipt
                 WHERE po_id=?1 AND (item_code=?2 OR item_code='') ORDER BY id",
            )?;
            let v = st
                .query_map(rusqlite::params![po_id, line.item_code], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            v
        };
        let qty_received = recv_rows.iter().fold(fincore::Money::ZERO, |acc, q| {
            acc + fincore::Money::parse_or_zero(q)
        });
        let remaining = cap - estimated;
        out.push(EstimateLine {
            item_code: line.item_code.clone(),
            item_name: line.item_name.clone(),
            cap,
            estimated,
            remaining: if remaining.is_positive() {
                remaining
            } else {
                fincore::Money::ZERO
            },
            qty_ordered: line.qty_ordered,
            qty_received,
            qty_outstanding: line.qty_ordered - qty_received,
        });
    }
    Ok(out)
}

/// 暂估冲回：发票到票后标记 settled，并同事务生成反向冲回凭证（借 应付 / 贷 存货）。
/// 返回冲回凭证 id；已冲回或并发重复冲回返回 None（幂等）。
pub fn po_estimate_settle(db: &Db, id: i64, date: NaiveDate, who: &str) -> DbResult<Option<i64>> {
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
    let po =
        crate::scm::po_get(db, po_id)?.ok_or_else(|| fincore::FinError::not_found("采购订单"))?;
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
    let mut st = db
        .conn()
        .prepare("SELECT est_amount FROM po_estimate WHERE po_id=?1 AND settled=0")?;
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
pub fn quota_set(
    db: &Db,
    period: Period,
    supplier: &str,
    item: &str,
    quota_qty: Money,
) -> DbResult<()> {
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
pub fn quota_remaining(
    db: &Db,
    period: Period,
    supplier: &str,
    item: &str,
) -> DbResult<Option<Money>> {
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

pub fn change_log_add(
    db: &Db,
    order_type: &str,
    order_id: i64,
    field: &str,
    old_value: &str,
    new_value: &str,
    who: &str,
) -> DbResult<()> {
    change_log_add_conn(
        db.conn(),
        order_type,
        order_id,
        field,
        old_value,
        new_value,
        who,
    )
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

pub fn change_log_list(
    db: &Db,
    order_type: &str,
    order_id: i64,
) -> DbResult<Vec<(String, String, String, String, String)>> {
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

    /// 暂估封顶：应收货限制，防止同一批货被反复登记暂估把应付做虚。
    ///
    /// 回归背景：`po_estimate` 只有普通索引（`idx_pe_po`，非 UNIQUE），
    /// `est_amount` 又是全手填、`po_estimate_add` 此前**零校验**。于是同一 PO
    /// 同一物料可以无限次登记暂估，每登一次就出一张「借 存货 / 贷 应付」凭证 ——
    /// 应付与存货双双虚增，全程无任何报错。销售侧 `prod_from_so` 早就有封顶，
    /// 采购侧此前完全裸奔。
    ///
    /// 对标 ERPNext / Odoo 的 received vs billed 一致性检查。
    #[test]
    fn estimate_is_capped_by_received_amount() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = crate::scm::PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S01",
            "供应商A",
            "u",
        );
        po.no = crate::scm::po_next_no(&db, p).unwrap();
        po.lines.push(crate::scm::PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".into(),
            item_name: "原料".into(),
            qty_ordered: m("100"),
            qty_received: m("0"),
            unit_price: m("10"),
            tax_rate: m("0"),
            amount: m("1000"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let po_id = crate::scm::po_save(&db, &mut po).unwrap();

        // ① 货还没到就暂估 —— 应收货限制
        let e = po_estimate_add(&db, po_id, p, "140301", m("800"), "u").unwrap_err();
        assert!(
            format!("{e:?}").contains("超出可暂估额度"),
            "没收货就登暂估必须被拒：{e:?}"
        );
        assert_eq!(
            po_estimate_open_sum(&db, po_id).unwrap(),
            Money::ZERO,
            "被拒时不能留下暂估记录"
        );

        // ② 收货 100（= 订购量，到货金额 1000）后可暂估，但不能超过
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id,period,date,qty,memo) VALUES(?1,?2,'2026-01-08',100,'')",
                rusqlite::params![po_id, p.ymm()],
            )
            .unwrap();
        po_estimate_add(&db, po_id, p, "140301", m("800"), "u").unwrap();
        // 再登 300 → 累计 1100 > 1000，第二次必须被拒
        let e2 = po_estimate_add(&db, po_id, p, "140301", m("300"), "u").unwrap_err();
        assert!(
            format!("{e2:?}").contains("超出可暂估额度"),
            "累计超实收必须被拒：{e2:?}"
        );
        // 剩余额度仍可登：800 + 200 = 1000，正好用完
        po_estimate_add(&db, po_id, p, "140301", m("200"), "u").unwrap();
        assert_eq!(
            po_estimate_open_sum(&db, po_id).unwrap(),
            m("1000"),
            "额度应当正好用满，不能因为守卫而少登"
        );

        // ③ 超收可如实入账，额度也随之放大（守卫不该把真实超收挡掉）
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id,period,date,qty,memo) VALUES(?1,?2,'2026-01-09',50,'')",
                rusqlite::params![po_id, p.ymm()],
            )
            .unwrap();
        po_estimate_add(&db, po_id, p, "140301", m("500"), "u").unwrap();
        assert_eq!(po_estimate_open_sum(&db, po_id).unwrap(), m("1500"));

        // ④ 已冲回的不占额度：否则发票到票后反而登不进新暂估
        let open: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM po_estimate WHERE po_id=?1 AND settled=0",
                [po_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 3, "三条未结算暂估");
    }

    /// 冲回后额度释放：发票到票、反向冲回之后，该物料应能重新暂估。
    #[test]
    fn settled_estimate_frees_up_the_cap() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = crate::scm::PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S01",
            "供应商A",
            "u",
        );
        po.no = crate::scm::po_next_no(&db, p).unwrap();
        po.lines.push(crate::scm::PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".into(),
            item_name: "原料".into(),
            qty_ordered: m("100"),
            qty_received: m("0"),
            unit_price: m("10"),
            tax_rate: m("0"),
            amount: m("1000"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let po_id = crate::scm::po_save(&db, &mut po).unwrap();
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id,period,date,qty,memo) VALUES(?1,?2,'2026-01-08',100,'')",
                rusqlite::params![po_id, p.ymm()],
            )
            .unwrap();
        // 额度 1000：一次性登满
        let (est_id, _) = po_estimate_add(&db, po_id, p, "140301", m("1000"), "u").unwrap();
        assert!(
            po_estimate_add(&db, po_id, p, "140301", m("1"), "u").is_err(),
            "额度已用满，追加必须被拒"
        );
        // 冲回后额度释放
        po_estimate_settle(
            &db,
            est_id,
            NaiveDate::from_ymd_opt(2026, 1, 20).unwrap(),
            "u",
        )
        .unwrap()
        .unwrap();
        po_estimate_add(&db, po_id, p, "140301", m("1000"), "u")
            .expect("冲回后额度应释放，能重新暂估全额");
    }

    /// 收货**分行**时，额度按**该行含税单价 × 该行实收量**精确算。
    ///
    /// 回归背景：收货早先只有「一个总数量」，多行订单的部分收货说不清这批货是
    /// 哪些行的，金额只能按订购量占比折算 —— 那是错的：
    /// 两张行各 100 件、含税单价 11，实际收 100 件（全是第一行），
    /// 折算摊成 550，而实际应付 1100 —— 差一半。
    /// 而暂估封顶的额度取自这个折算，于是折算错 = 守卫拿错额度去拦正确操作。
    #[test]
    fn line_level_receipt_gives_exact_cap() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = crate::scm::PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S01",
            "供应商A",
            "u",
        );
        po.no = crate::scm::po_next_no(&db, p).unwrap();
        // 两行，各 100 件、单价 10、税率 10% → 含税单价 11
        for (code, name) in [("140301", "原料A"), ("140302", "原料B")] {
            po.lines.push(crate::scm::PoLine {
                id: 0,
                po_id: 0,
                item_code: code.into(),
                item_name: name.into(),
                qty_ordered: m("100"),
                qty_received: m("0"),
                unit_price: m("10"),
                tax_rate: m("0.1"),
                amount: m("1000"),
                tax_amount: m("100"),
                memo: String::new(),
            });
        }
        let po_id = crate::scm::po_save(&db, &mut po).unwrap();

        // 只收第一行 100 件（走真实收货路径，不是直接 INSERT 汇总表）
        crate::procurement::po_receipt_with_stock(
            &db,
            &crate::procurement::PoReceipt {
                id: 0,
                po_id,
                period: p,
                date: NaiveDate::from_ymd_opt(2026, 1, 8).unwrap(),
                qty: m("100"),
                memo: "只到 A".into(),
                item_code: "140301".into(),
                warehouse: "01".into(),
            },
            "01",
        )
        .unwrap();

        // 第一行额度 = 11 × 100 = 1100（精确），不是折算的 550
        let cap_a = estimate_cap_for_line(&db, &po, "140301").unwrap();
        assert_eq!(
            cap_a,
            m("1100"),
            "第一行额度应按该行含税单价精确算：{cap_a}"
        );
        // 第二行没收货 → 额度 0（不能被第一行的收货撑起来）
        let cap_b = estimate_cap_for_line(&db, &po, "140302").unwrap();
        assert_eq!(cap_b, m("0"), "没收货的行额度必须是 0");

        // 暂估 1100 可以，超出一厘就被拒
        po_estimate_add(&db, po_id, p, "140301", m("1100"), "u").unwrap();
        let e = po_estimate_add(&db, po_id, p, "140301", m("0.01"), "u").unwrap_err();
        assert!(
            format!("{e:?}").contains("超出可暂估额度"),
            "超出一厘就该被拒（金额是精确十进制，不是浮点近似）：{e:?}"
        );
        // 第二行还没收货，登不了暂估
        let e2 = po_estimate_add(&db, po_id, p, "140302", m("1"), "u").unwrap_err();
        assert!(format!("{e2:?}").contains("超出可暂估额度"), "{e2:?}");
    }

    /// 超收要按实收如实入库，暂估额度随之放大（走真实收货路径）。
    ///
    /// 早先的超收用例是**直接 INSERT po_receipt**，只断言 `po_list` 的汇总字段 ——
    /// 存货流水、凭证一个都没产生，所以「超收时存货与应付是否自洽」一直没被验证。
    #[test]
    fn over_receipt_flows_through_stock_and_estimate_cap() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = crate::scm::PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S01",
            "供应商A",
            "u",
        );
        po.no = crate::scm::po_next_no(&db, p).unwrap();
        po.lines.push(crate::scm::PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".into(),
            item_name: "原料".into(),
            qty_ordered: m("100"),
            qty_received: m("0"),
            unit_price: m("10"),
            tax_rate: m("0"),
            amount: m("1000"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let po_id = crate::scm::po_save(&db, &mut po).unwrap();

        // 超收 30%：收 130 而订 100
        crate::procurement::po_receipt_with_stock(
            &db,
            &crate::procurement::PoReceipt {
                id: 0,
                po_id,
                period: p,
                date: NaiveDate::from_ymd_opt(2026, 1, 8).unwrap(),
                qty: m("130"),
                memo: "超收".into(),
                item_code: "140301".into(),
                warehouse: "01".into(),
            },
            "01",
        )
        .unwrap();

        // ① 存货流水按实收 130 入库，不是按订购 100 截断
        let stock_qty: String = db
            .conn()
            .query_row(
                "SELECT qty FROM stock_move WHERE kind='purchase' AND item='140301'
                 ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            stock_qty.starts_with("130"),
            "存货要按实收 130 入库，超收不能被悄悄截成 100：{stock_qty}"
        );
        // ② 暂估额度随之放大到 1300（守卫不该把真实超收挡掉）
        let po2 = crate::scm::po_get(&db, po_id).unwrap().unwrap();
        assert_eq!(
            estimate_cap_for_line(&db, &po2, "140301").unwrap(),
            m("1300")
        );
        po_estimate_add(&db, po_id, p, "140301", m("1300"), "u").expect("超收后的额度应能全额暂估");
    }

    #[test]
    fn estimate_and_reconcile() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = crate::scm::PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S01",
            "供应商A",
            "u",
        );
        po.no = crate::scm::po_next_no(&db, p).unwrap();
        po.lines.push(crate::scm::PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".into(),
            item_name: "原料".into(),
            qty_ordered: m("100"),
            qty_received: m("0"),
            unit_price: m("10"),
            tax_rate: m("0"),
            amount: m("1000"),
            tax_amount: m("0"),
            memo: String::new(),
        });
        let po_id = crate::scm::po_save(&db, &mut po).unwrap();
        // 先收货再暂估：暂估额度来自实收量（`po_received_gross`），
        // 货还没到就登暂估会被封顶守卫拒掉 —— 这是有意的，见下方守卫测试。
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id,period,date,qty,memo) VALUES(?1,?2,'2026-01-08',100,'')",
                rusqlite::params![po_id, p.ymm()],
            )
            .unwrap();
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
            po_estimate_settle(
                &db,
                est_id,
                NaiveDate::from_ymd_opt(2026, 1, 21).unwrap(),
                "u"
            )
            .unwrap()
            .is_none(),
            "重复冲回应幂等"
        );
        // 付款 600 → 对账未付 400
        crate::procurement::po_payment_add(
            &db,
            &crate::procurement::PoPayment {
                id: 0,
                po_id,
                period: p,
                date: NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
                amount: m("600"),
                memo: String::new(),
            },
        )
        .unwrap();
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
        assert_eq!(
            quota_remaining(&db, p, "S01", "140301").unwrap(),
            Some(m("500"))
        );
        quota_use(&db, p, "S01", "140301", m("120")).unwrap();
        assert_eq!(
            quota_remaining(&db, p, "S01", "140301").unwrap(),
            Some(m("380"))
        );
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
