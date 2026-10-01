//! 库存深度：序列号 / 多单位换算 / 账龄分析 / ABC 分析 / 组装拆卸 / 库存状态 / 调拨报表
//!
//! 对标金蝶/用友库存管理。金额数量一律 TEXT 存储、Rust 侧 Decimal 累加。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 序列号
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Serial {
    pub serial: String,
    pub item: String,
    pub batch_no: String,
    pub status: String, // in / out / scrapped
    pub in_date: String,
    pub out_date: Option<String>,
    pub memo: String,
}

/// 入库登记序列号（批量）
pub fn serial_in(db: &Db, item: &str, serials: &[String], batch_no: &str, date: NaiveDate) -> DbResult<usize> {
    let tx = db.write_tx()?;
    let mut n = 0;
    for s in serials {
        tx.execute(
            "INSERT INTO item_serial(serial,item,batch_no,status,in_date) VALUES(?1,?2,?3,'in',?4)
             ON CONFLICT(serial) DO UPDATE SET item=excluded.item, batch_no=excluded.batch_no, status='in', in_date=excluded.in_date",
            rusqlite::params![s, item, batch_no, date.format("%Y-%m-%d").to_string()],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// 出库登记序列号：在库 → 已出库
pub fn serial_out(db: &Db, serials: &[String], date: NaiveDate) -> DbResult<usize> {
    let tx = db.write_tx()?;
    let mut n = 0;
    for s in serials {
        let cnt = tx.execute(
            "UPDATE item_serial SET status='out', out_date=?2 WHERE serial=?1 AND status='in'",
            rusqlite::params![s, date.format("%Y-%m-%d").to_string()],
        )?;
        n += cnt;
    }
    tx.commit()?;
    Ok(n)
}

/// 在库序列号清单
pub fn serial_list(db: &Db, item: &str) -> DbResult<Vec<Serial>> {
    let mut st = db.conn().prepare(
        "SELECT serial,item,batch_no,status,in_date,out_date,memo FROM item_serial WHERE item=?1 ORDER BY serial",
    )?;
    let rows = st
        .query_map([item], |r| {
            Ok(Serial {
                serial: r.get(0)?,
                item: r.get(1)?,
                batch_no: r.get(2)?,
                status: r.get(3)?,
                in_date: r.get(4)?,
                out_date: r.get(5)?,
                memo: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ===========================================================================
// 多单位换算
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ItemUnit {
    pub item: String,
    pub base_unit: String,
    pub alt_unit: String,
    /// 1 主单位 = factor 辅助单位
    pub factor: Money,
}

pub fn unit_get(db: &Db, item: &str) -> DbResult<Option<ItemUnit>> {
    db.conn()
        .query_row(
            "SELECT item,base_unit,alt_unit,factor FROM item_unit WHERE item=?1",
            [item],
            |r| {
                Ok(ItemUnit {
                    item: r.get(0)?,
                    base_unit: r.get(1)?,
                    alt_unit: r.get(2)?,
                    factor: m(&r.get::<_, String>(3)?),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub fn unit_set(db: &Db, u: &ItemUnit) -> DbResult<()> {
    if u.factor <= Money::ZERO {
        return Err(fincore::FinError::msg("换算系数必须大于 0").into());
    }
    db.conn().execute(
        "INSERT INTO item_unit(item,base_unit,alt_unit,factor) VALUES(?1,?2,?3,?4)
         ON CONFLICT(item) DO UPDATE SET base_unit=excluded.base_unit, alt_unit=excluded.alt_unit, factor=excluded.factor",
        rusqlite::params![u.item, u.base_unit, u.alt_unit, crate::exact_param(u.factor)],
    )?;
    Ok(())
}

/// 主单位数量 → 辅助单位数量
pub fn unit_to_alt(db: &Db, item: &str, base_qty: Money) -> DbResult<Money> {
    match unit_get(db, item)? {
        Some(u) => Ok(base_qty
            .checked_div(u.factor.inner())
            .expect("换算系数已校验 > 0（保存入口 inventory2.rs:115）")
            .round_dp(fincore::money::QTY_DP)),
        None => Ok(base_qty),
    }
}

// ===========================================================================
// 账龄分析 / ABC 分析
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize)]
pub struct InvAging {
    pub item: String,
    /// 最近一次入库日期
    pub last_in: Option<String>,
    /// 账龄天数（距今天）
    pub days: i64,
    /// 结存数量
    pub qty: Money,
    /// 结存金额
    pub amount: Money,
}

/// 库存账龄：按最近入库日期算账龄，按账龄降序
pub fn inv_aging(db: &Db, upto: Period) -> DbResult<Vec<InvAging>> {
    let items = crate::business::stock_items(db)?;
    let today = chrono::Local::now().date_naive();
    let mut out = Vec::new();
    for item in items {
        let st = crate::business::stock_state(db, &item, upto, fincore::engine::costing::CostMethod::MovingAverage)?;
        if st.qty.is_zero() {
            continue;
        }
        let last_in: Option<String> = db.conn().query_row(
            "SELECT biz_date FROM stock_move WHERE item=?1 AND qty>0 ORDER BY biz_date DESC LIMIT 1",
            [&item],
            |r| r.get(0),
        ).optional()?;
        let days = match &last_in {
            Some(d) => {
                let d = NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap_or(today);
                (today - d).num_days().max(0)
            }
            None => 0,
        };
        out.push(InvAging { item, last_in, days, qty: st.qty, amount: st.amount });
    }
    out.sort_by(|a, b| b.days.cmp(&a.days));
    Ok(out)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AbcRow {
    pub item: String,
    pub amount: Money,
    /// 累计金额占比（%）
    pub cum_pct: Money,
    /// A/B/C 分类
    pub class: String,
}

/// ABC 分析：按结存金额降序，累计占比 <=80% A、<=95% B、其余 C
pub fn abc_analysis(db: &Db, upto: Period) -> DbResult<Vec<AbcRow>> {
    let mut aging = inv_aging(db, upto)?;
    // 按结存金额降序（inv_aging 返回的是按账龄排序，这里重排）
    aging.sort_by(|a, b| b.amount.cmp(&a.amount));
    let total: Money = aging.iter().map(|a| a.amount).sum();
    let mut rows: Vec<AbcRow> = aging
        .iter()
        .map(|a| AbcRow { item: a.item.clone(), amount: a.amount, cum_pct: Money::ZERO, class: "C".into() })
        .collect();
    let mut cum = Money::ZERO;
    for r in rows.iter_mut() {
        cum += r.amount;
        let pct = if total.is_zero() {
            Money::ZERO
        } else {
            (cum * Money::from_i64(100))
                .checked_div(total.inner())
                .expect("total 已判非零")
                .round2()
        };
        r.cum_pct = pct;
        r.class = if pct <= Money::parse("80").unwrap() {
            "A"
        } else if pct <= Money::parse("95").unwrap() {
            "B"
        } else {
            "C"
        }
        .into();
    }
    Ok(rows)
}

// ===========================================================================
// 组装 / 拆卸 / 库存状态 / 调拨报表
// ===========================================================================

/// 组装：多个子件 → 1 个成品。
/// **成本口径（对标金蝶组装单按成本构成入账）**：子件按各自移动加权成本出库，
/// 成品按**子件成本合计**入库（等值转入）；任一子件无成本价 → 拒绝
/// （0 价首入会污染计价引擎与销售成本结转，与形态转换同口径）。
pub fn assemble(
    db: &Db,
    period: Period,
    date: NaiveDate,
    parent: &str,
    children: &[(String, Money)],
    memo: &str,
) -> DbResult<()> {
    use crate::business::{stock_insert, StockKind, StockMove};
    if children.is_empty() {
        return Err(fincore::FinError::msg("组装至少需要一个子件").into());
    }
    // 先全量校验并算价，任一失败不落任何流水（避免半成品状态）
    let mut plan: Vec<(String, Money, Money, Money)> = Vec::new();
    let mut total = Money::ZERO;
    for (item, qty) in children {
        if !qty.is_positive() {
            return Err(fincore::FinError::msg(format!("子件 {item} 数量必须大于 0")).into());
        }
        let unit = crate::business::stock_state(
            db,
            item,
            period,
            fincore::engine::costing::CostMethod::MovingAverage,
        )?
        .unit_cost();
        if !unit.is_positive() {
            return Err(fincore::FinError::msg(format!(
                "子件 {item} 无成本价，无法组装（请先入库带价或执行期末结价）"
            ))
            .into());
        }
        let amount = *qty * unit;
        total = total + amount;
        plan.push((item.clone(), *qty, unit, amount));
    }
    let wh = crate::warehouse::resolve(db, "")?;
    for (item, qty, unit, amount) in &plan {
        stock_insert(
            db,
            &StockMove {
                id: 0,
                period,
                biz_date: date,
                kind: StockKind::OtherOut,
                item: item.clone(),
                warehouse: wh.clone(),
                batch_no: String::new(),
                qty: qty.negated(),
                price: *unit,
                amount: *amount,
                voucher_id: None,
                memo: format!("组装 {}", memo),
            },
        )?;
    }
    stock_insert(
        db,
        &StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: StockKind::OtherIn,
            item: parent.to_string(),
            warehouse: wh,
            batch_no: String::new(),
            qty: Money::ONE,
            price: total,
            amount: total,
            voucher_id: None,
            memo: format!("组装 {}", memo),
        },
    )?;
    Ok(())
}

/// 拆卸：1 个成品 → 多个子件。
/// **成本口径**：成品按移动加权成本出库，子件按**数量比例**分摊成品成本入库
/// （尾差归最后一行）；成品无成本价 → 拒绝。等值转换，不产生总账凭证。
pub fn disassemble(
    db: &Db,
    period: Period,
    date: NaiveDate,
    parent: &str,
    children: &[(String, Money)],
    memo: &str,
) -> DbResult<()> {
    use crate::business::{stock_insert, StockKind, StockMove};
    if children.is_empty() {
        return Err(fincore::FinError::msg("拆卸至少需要一个子件").into());
    }
    if children.iter().any(|(item, _)| item == parent) {
        return Err(fincore::FinError::msg("子件不能与成品相同").into());
    }
    let qty_sum: Money = children.iter().map(|(_, q)| *q).sum();
    if !qty_sum.is_positive() {
        return Err(fincore::FinError::msg("子件数量合计必须大于 0").into());
    }
    let unit = crate::business::stock_state(
        db,
        parent,
        period,
        fincore::engine::costing::CostMethod::MovingAverage,
    )?
    .unit_cost();
    if !unit.is_positive() {
        return Err(fincore::FinError::msg(
            "成品无成本价，无法拆卸（请先入库带价或执行期末结价）",
        )
        .into());
    }
    let total = unit; // 成品出库 1 件
    let wh = crate::warehouse::resolve(db, "")?;
    // 成品出库
    stock_insert(
        db,
        &StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: StockKind::OtherOut,
            item: parent.to_string(),
            warehouse: wh.clone(),
            batch_no: String::new(),
            qty: Money::ONE.negated(),
            price: unit,
            amount: total,
            voucher_id: None,
            memo: format!("拆卸 {}", memo),
        },
    )?;
    // 子件入库：按数量比例分摊成本，尾差归最后一行
    let mut allocated = Money::ZERO;
    let last = children.len() - 1;
    for (i, (item, qty)) in children.iter().enumerate() {
        let amount = if i == last {
            total - allocated
        } else {
            (total * *qty)
                .checked_div(qty_sum.0)
                .expect("qty_sum 已判正")
        };
        allocated = allocated + amount;
        let price = amount
            .checked_div(qty.0)
            .expect("子件数量已判正");
        stock_insert(
            db,
            &StockMove {
                id: 0,
                period,
                biz_date: date,
                kind: StockKind::OtherIn,
                item: item.clone(),
                warehouse: wh.clone(),
                batch_no: String::new(),
                qty: *qty,
                price,
                amount,
                voucher_id: None,
                memo: format!("拆卸 {}", memo),
            },
        )?;
    }
    Ok(())
}

/// 形态转换（对标金蝶形态转换单）：源物料出库 → 目标物料入库，同数量一减一增。
/// **金额按源存货当前移动加权成本平移**（等值转换；目标 0 价首入会污染计价引擎与
/// 销售成本结转，故源无成本价时拒绝转换）；无总账凭证（存货内部结构调整）。
pub fn form_convert(
    db: &Db,
    period: Period,
    date: NaiveDate,
    from_item: &str,
    to_item: &str,
    qty: Money,
    memo: &str,
) -> DbResult<()> {
    if from_item == to_item {
        return Err(fincore::FinError::msg("源物料与目标物料不能相同").into());
    }
    if !from_item.trim().is_empty() && !to_item.trim().is_empty() && !qty.is_positive() {
        return Err(fincore::FinError::msg("转换数量必须大于 0").into());
    }
    let unit = crate::business::stock_state(
        db,
        from_item,
        period,
        fincore::engine::costing::CostMethod::MovingAverage,
    )?
    .unit_cost();
    if !unit.is_positive() {
        return Err(fincore::FinError::msg(
            "源物料无成本价，无法形态转换（请先入库带价或执行期末结价）",
        )
        .into());
    }
    let amount = qty * unit;
    let wh = crate::warehouse::resolve(db, "")?;
    use crate::business::{stock_insert, StockKind, StockMove};
    stock_insert(
        db,
        &StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: StockKind::OtherOut,
            item: from_item.to_string(),
            warehouse: wh.clone(),
            batch_no: String::new(),
            qty: qty.negated(),
            price: unit,
            amount,
            voucher_id: None,
            memo: format!("形态转换 {}", memo),
        },
    )?;
    stock_insert(
        db,
        &StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: StockKind::OtherIn,
            item: to_item.to_string(),
            warehouse: String::new(),
            batch_no: String::new(),
            qty,
            price: unit,
            amount,
            voucher_id: None,
            memo: format!("形态转换 {}", memo),
        },
    )?;
    Ok(())
}

/// 低于安全库存的存货：item_plan.safety_stock > 0 且现有库存（流水汇总）< 安全量。
/// 返回 (存货, 现有库存, 安全库存)——工作台仓管预警与低库存待办数据源。
pub fn below_safety(db: &Db) -> DbResult<Vec<(String, Money, Money)>> {
    let mut st = db.conn().prepare(
        "SELECT item_code, CAST(safety_stock AS REAL) FROM item_plan
         WHERE CAST(safety_stock AS REAL) > 0 ORDER BY item_code",
    )?;
    let plans: Vec<(String, f64)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (item, safety) in plans {
        let on: f64 = db.conn().query_row(
            "SELECT COALESCE(SUM(CAST(qty AS REAL)),0) FROM stock_move WHERE item=?1 AND qc_status=''",
            [&item],
            |r| r.get(0),
        )?;
        if on < safety {
            out.push((
                item,
                Money::parse_or_zero(&format!("{on:.4}")),
                Money::parse_or_zero(&format!("{safety:.4}")),
            ));
        }
    }
    Ok(out)
}

/// 库存状态：分仓库结存（含质检三口径：qty=结存、available=可用、pending=待检、quarantine=隔离）
#[derive(Clone, Debug, serde::Serialize)]
pub struct WhStock {
    pub warehouse: String,
    pub item: String,
    pub qty: Money,
    pub available: Money,
    pub pending: Money,
    pub quarantine: Money,
}

pub fn warehouse_stock(db: &Db, item: &str) -> DbResult<Vec<WhStock>> {
    let mut st = db.conn().prepare(
        "SELECT warehouse, qc_status, qty FROM stock_move WHERE item=?1 ORDER BY warehouse",
    )?;
    let rows = st
        .query_map([item], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                m(&r.get::<_, String>(2)?),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // 每仓四桶：结存 / 可用 / 待检 / 隔离（qc_status=''或未知→可用；pending/quarantine 各归其桶）
    let mut map: std::collections::BTreeMap<String, (Money, Money, Money, Money)> =
        std::collections::BTreeMap::new();
    for (w, qc, q) in rows {
        let e = map
            .entry(if w.is_empty() { "默认仓".to_string() } else { w })
            .or_insert((Money::ZERO, Money::ZERO, Money::ZERO, Money::ZERO));
        e.0 += q;
        match qc.as_str() {
            "pending" => e.2 += q,
            "quarantine" => e.3 += q,
            _ => e.1 += q,
        }
    }
    Ok(map
        .into_iter()
        .map(
            |(warehouse, (qty, available, pending, quarantine))| WhStock {
                warehouse,
                item: item.to_string(),
                qty,
                available,
                pending,
                quarantine,
            },
        )
        .collect())
}

/// 调拨报表：期间内调拨流水
pub fn transfer_report(db: &Db, period: Period) -> DbResult<Vec<crate::business::StockMove>> {
    let rows = crate::business::stock_list(db, period)?;
    Ok(rows.into_iter().filter(|r| r.kind == crate::business::StockKind::Transfer).collect())
}

// ---------------- 批次成本勾稽 / 批次调拨（链5） ----------------

/// 批次成本明细（item+batch 层，流水金额聚合）
#[derive(Clone, Debug, serde::Serialize)]
pub struct BatchCostDetail {
    pub item: String,
    pub batch_no: String,
    pub qty: Money,
    pub amount: Money,
}

/// 逐存货勾稽行：Σ批次价值 vs 账面（存货辅助账期末）
#[derive(Clone, Debug, serde::Serialize)]
pub struct BatchCostTotal {
    pub item: String,
    pub qty: Money,
    pub amount: Money,
    pub book: Money,
    pub diff: Money,
}

/// 批次成本勾稽：批次层价值（建账以来全部流水按 item+batch 聚合，金额=计价引擎结算后）
/// vs 存货辅助账期末余额（aux end，kind=Item，期初取极早期）。**口径声明**：两侧期间起点
/// 不同（批次侧不含期初手工建账前历史、账面侧含期初），差异本身即定位信号
/// （期初未建批次、辅助手工调整、未结算流水等）。
pub fn batch_cost_report(
    db: &Db,
    to: Period,
) -> DbResult<(Vec<BatchCostDetail>, Vec<BatchCostTotal>)> {
    // 批次侧：逐行取文本内存聚合（与 batch_balance 同模式，避免 SUM 类型亲和问题）
    let mut st = db
        .conn()
        .prepare("SELECT item, batch_no, qty, amount FROM stock_move WHERE batch_no <> ''")?;
    let raw: Vec<(String, String, String, String)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    use std::collections::HashMap;
    let mut agg: HashMap<(String, String), (Money, Money)> = HashMap::new();
    for (item, bn, q, a) in raw {
        let e = agg.entry((item, bn)).or_insert((Money::ZERO, Money::ZERO));
        e.0 += Money::parse_or_zero(&q);
        e.1 += Money::parse_or_zero(&a);
    }
    let mut detail: Vec<BatchCostDetail> = agg
        .iter()
        .map(|((item, bn), (q, a))| BatchCostDetail {
            item: item.clone(),
            batch_no: bn.clone(),
            qty: *q,
            amount: *a,
        })
        .collect();
    detail.sort_by(|a, b| a.item.cmp(&b.item).then_with(|| a.batch_no.cmp(&b.batch_no)));

    // 逐存货合计
    let mut tot: HashMap<String, (Money, Money)> = HashMap::new();
    for (k, (q, a)) in &agg {
        let e = tot.entry(k.0.clone()).or_insert((Money::ZERO, Money::ZERO));
        e.0 += *q;
        e.1 += *a;
    }
    // 账面侧：存货辅助账期末（from 取极早期覆盖全部历史）
    let book_rows = crate::balances::aux_balance(
        db,
        fincore::AuxKind::Item,
        Period::from_ymm(195001),
        to,
        None,
    )?;
    let book: HashMap<String, Money> = book_rows.into_iter().map(|r| (r.key, r.end)).collect();
    let mut items: Vec<String> = tot.keys().cloned().collect();
    items.extend(book.keys().cloned());
    items.sort();
    items.dedup();
    let mut totals: Vec<BatchCostTotal> = items
        .into_iter()
        .map(|item| {
            let (q, a) = tot.get(&item).copied().unwrap_or((Money::ZERO, Money::ZERO));
            let b = book.get(&item).copied().unwrap_or(Money::ZERO);
            BatchCostTotal {
                diff: a - b,
                item,
                qty: q,
                amount: a,
                book: b,
            }
        })
        .collect();
    totals.retain(|t| !t.qty.is_zero() || !t.book.is_zero() || !t.amount.is_zero());
    Ok((detail, totals))
}

/// 批次调拨（对标金蝶调拨单 v1）：源仓分仓余额校验 → 调出（qty 负）/ 调入（qty 正）两条
/// Transfer 流水（标准价口径，amount=0 由计价引擎结算参与成本序列）→ 批次主仓标签改写
/// （stock_batch 全仓唯一，warehouse 仅为主仓标签；分仓数量以流水分账为准）。
/// 返回 (调出流水 id, 调入流水 id)。
pub fn transfer_do(
    db: &Db,
    period: Period,
    date: NaiveDate,
    item: &str,
    batch_no: &str,
    from_wh: &str,
    to_wh: &str,
    qty: Money,
    memo: &str,
) -> DbResult<(i64, i64)> {
    let item = item.trim();
    let bn = batch_no.trim();
    // 仓库主数据校验：源/目标仓必须存在且未停用（空值已在下方拒绝）
    let from = crate::warehouse::resolve(db, from_wh)?;
    let to = crate::warehouse::resolve(db, to_wh)?;
    if item.is_empty() || bn.is_empty() {
        return Err(fincore::FinError::msg("存货编码与批号必填").into());
    }
    if from_wh.trim().is_empty() || to_wh.trim().is_empty() {
        return Err(fincore::FinError::state("源仓与目标仓必填").into());
    }
    if from == to {
        return Err(fincore::FinError::state("源仓与目标仓不能相同").into());
    }
    if !qty.is_positive() {
        return Err(fincore::FinError::msg("调拨数量必须大于 0").into());
    }
    let cnt: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM stock_batch WHERE item=?1 AND batch_no=?2",
        rusqlite::params![item, bn],
        |r| r.get(0),
    )?;
    if cnt == 0 {
        return Err(fincore::FinError::not_found("批次不存在").into());
    }
    // 源仓分仓余额（出负入正净额）
    let mut st = db.conn().prepare(
        "SELECT qty FROM stock_move WHERE item=?1 AND batch_no=?2 AND warehouse=?3",
    )?;
    let rows = st
        .query_map(rusqlite::params![item, bn, from], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let bal: Money = rows.iter().map(|s| Money::parse_or_zero(s)).sum();
    if qty > bal {
        return Err(fincore::FinError::state(format!(
            "源仓 {from} 该批次余额 {} 不足（待调 {}）",
            bal.fmt_qty(),
            qty.fmt_qty()
        ))
        .into());
    }
    let price = crate::business::item_standard_cost(db, item).unwrap_or(Money::ZERO);
    let tag = format!("调拨 {from}→{to} {memo}");
    let tx = db.write_tx()?;
    let out_id = crate::business::stock_insert_of(
        &tx,
        &crate::business::StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: crate::business::StockKind::Transfer,
            item: item.to_string(),
            warehouse: from.to_string(),
            batch_no: bn.to_string(),
            qty: qty.negated(),
            price,
            amount: Money::ZERO,
            voucher_id: None,
            memo: tag.clone(),
        },
    )?;
    let in_id = crate::business::stock_insert_of(
        &tx,
        &crate::business::StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: crate::business::StockKind::Transfer,
            item: item.to_string(),
            warehouse: to.to_string(),
            batch_no: bn.to_string(),
            qty,
            price,
            amount: Money::ZERO,
            voucher_id: None,
            memo: tag,
        },
    )?;
    tx.execute(
        "UPDATE stock_batch SET warehouse=?3 WHERE item=?1 AND batch_no=?2",
        rusqlite::params![item, bn, to],
    )?;
    tx.commit()?;
    Ok((out_id, in_id))
}

// ---------------- 存货档案一站式（独立存货档案页，C 选项） ----------------

/// 存货档案聚合行：aux(item) + 计划参数 + 库存现量 + 主单位
#[derive(Clone, Debug, serde::Serialize)]
pub struct ItemMasterRow {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub memo: String,
    pub disabled: bool,
    pub parent: Option<String>,
    /// 保质期天（props.shelf_life_days 原文，空/0 = 未启用）
    pub shelf_life: String,
    /// 启用来料检验（props.qc_required）
    pub qc: bool,
    pub safety: Money,
    pub lead_days: i32,
    pub lot: Money,
    /// 现存量（全部流水汇总，出负入正）
    pub qty: Money,
    /// 主单位（item_unit.base_unit，未设为空）
    pub unit: String,
}

/// 存货档案一站式聚合（四查询拼装，档案量级本地毫秒）
pub fn item_master(db: &Db) -> DbResult<Vec<ItemMasterRow>> {
    use std::collections::HashMap;
    // 1) 库存现量（逐行内存汇总，与 batch_balance 同模式）
    let mut qty_map: HashMap<String, Money> = HashMap::new();
    {
        let mut st = db
            .conn()
            .prepare("SELECT item, qty FROM stock_move")?;
        let rows = st
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (item, q) in rows {
            *qty_map.entry(item).or_insert(Money::ZERO) += Money::parse_or_zero(&q);
        }
    }
    // 2) 计划参数
    let mut plan_map: HashMap<String, (Money, i32, Money)> = HashMap::new();
    {
        let mut st = db.conn().prepare(
            "SELECT item_code, safety_stock, lead_days, lot_size FROM item_plan",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i32>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (c, s, l, lot) in rows {
            plan_map.insert(c, (Money::parse_or_zero(&s), l, Money::parse_or_zero(&lot)));
        }
    }
    // 3) 主单位
    let mut unit_map: HashMap<String, String> = HashMap::new();
    {
        let mut st = db
            .conn()
            .prepare("SELECT item, base_unit FROM item_unit")?;
        let rows = st
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (c, u) in rows {
            unit_map.insert(c, u);
        }
    }
    // 4) aux(item) 全量
    let mut st = db.conn().prepare(
        "SELECT id,code,name,memo,disabled,parent_code,props_json
         FROM aux_entity WHERE kind='item' ORDER BY code",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let out = rows
        .into_iter()
        .map(|(id, code, name, memo, disabled, parent, props_json)| {
            let props: std::collections::BTreeMap<String, String> =
                serde_json::from_str(&props_json).unwrap_or_default();
            let (safety, lead, lot) = plan_map
                .get(&code)
                .cloned()
                .unwrap_or((Money::ZERO, 0, Money::ZERO));
            ItemMasterRow {
                shelf_life: props
                    .get("shelf_life_days")
                    .cloned()
                    .unwrap_or_else(|| "0".into()),
                qc: matches!(props.get("qc_required").map(String::as_str), Some("1") | Some("true")),
                id,
                code: code.clone(),
                qty: qty_map.get(&code).copied().unwrap_or(Money::ZERO),
                unit: unit_map.get(&code).cloned().unwrap_or_default(),
                name,
                memo,
                disabled: disabled != 0,
                parent,
                safety,
                lead_days: lead,
                lot,
            }
        })
        .collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn serial_flow() {
        let db = mem();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        serial_in(&db, "P001", &["S1".into(), "S2".into(), "S3".into()], "B1", d).unwrap();
        assert_eq!(serial_list(&db, "P001").unwrap().len(), 3);
        serial_out(&db, &["S1".into()], d).unwrap();
        let list = serial_list(&db, "P001").unwrap();
        assert_eq!(list.iter().filter(|s| s.status == "in").count(), 2);
        assert_eq!(list.iter().filter(|s| s.status == "out").count(), 1);
    }

    #[test]
    fn unit_conversion() {
        let db = mem();
        unit_set(&db, &ItemUnit { item: "P001".into(), base_unit: "个".into(), alt_unit: "箱".into(), factor: m("10") }).unwrap();
        assert_eq!(unit_to_alt(&db, "P001", m("50")).unwrap(), m("5"));
        // 系数非法
        assert!(unit_set(&db, &ItemUnit { item: "P001".into(), base_unit: "个".into(), alt_unit: "箱".into(), factor: m("0") }).is_err());
    }

    #[test]
    fn abc_analysis_classes() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        use crate::business::{stock_insert, StockKind, StockMove};
        // A=700(70%)、B=200(90%)、C=100(100%) → A/B/C 三档分明
        for (item, qty, price) in [("A", "70", "10"), ("B", "20", "10"), ("C", "10", "10")] {
            stock_insert(&db, &StockMove {
                id: 0, period: p, biz_date: d, kind: StockKind::Purchase,
                item: item.into(), warehouse: "主仓".into(), batch_no: String::new(),
                qty: m(qty), price: m(price), amount: m(qty) * m(price), voucher_id: None, memo: String::new(),
            }).unwrap();
        }
        let rows = abc_analysis(&db, p).unwrap();
        assert_eq!(rows.len(), 3);
        // 按金额降序：A 700、B 200、C 100
        assert_eq!(rows[0].item, "A");
        assert_eq!(rows[0].class, "A");
        assert_eq!(rows[1].item, "B");
        assert_eq!(rows[1].class, "B");
        assert_eq!(rows[2].item, "C");
        assert_eq!(rows[2].class, "C");
    }

    #[test]
    fn assemble_disassemble() {
        use crate::business::{stock_insert, StockKind, StockMove};
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        // 子件先带价入库：RM1 2×5、RM2 1×10（合计 20）
        for (item, qty, price) in [("RM1", "2", "5"), ("RM2", "1", "10")] {
            stock_insert(
                &db,
                &StockMove {
                    id: 0,
                    period: p,
                    biz_date: d,
                    kind: StockKind::OtherIn,
                    item: item.into(),
                    warehouse: String::new(),
                    batch_no: String::new(),
                    qty: m(qty),
                    price: m(price),
                    amount: m(qty) * m(price),
                    voucher_id: None,
                    memo: "建账".into(),
                },
            )
            .unwrap();
        }
        // 无成本子件 → 拒绝（0 价首入污染计价引擎）
        assert!(assemble(&db, p, d, "FG", &[("RM9".into(), m("1"))], "测试").is_err());
        // 组装：成品 1 件按子件成本合计 20 入库
        assemble(
            &db,
            p,
            d,
            "FG",
            &[("RM1".into(), m("2")), ("RM2".into(), m("1"))],
            "测试",
        )
        .unwrap();
        let fg = warehouse_stock(&db, "FG").unwrap();
        assert_eq!(fg.iter().map(|w| w.qty).sum::<Money>(), m("1"));
        let fg_unit = crate::business::stock_state(
            &db,
            "FG",
            p,
            fincore::engine::costing::CostMethod::MovingAverage,
        )
        .unwrap()
        .unit_cost();
        assert_eq!(fg_unit, m("20"), "成品按子件成本合计入库");
        let rm1 = warehouse_stock(&db, "RM1").unwrap();
        // 先入 2、组装出 2 → 净 0
        assert_eq!(rm1.iter().map(|w| w.qty).sum::<Money>(), m("0"));
        // 拆卸：成品按成本 20 出库，子件按数量比例分摊 → RM1 回补 2 件（成本 20）
        disassemble(&db, p, d, "FG", &[("RM1".into(), m("2"))], "拆").unwrap();
        let fg = warehouse_stock(&db, "FG").unwrap();
        assert_eq!(fg.iter().map(|w| w.qty).sum::<Money>(), m("0"));
        let rm1 = warehouse_stock(&db, "RM1").unwrap();
        assert_eq!(rm1.iter().map(|w| w.qty).sum::<Money>(), m("2"));
        let rm1_unit = crate::business::stock_state(
            &db,
            "RM1",
            p,
            fincore::engine::costing::CostMethod::MovingAverage,
        )
        .unwrap()
        .unit_cost();
        assert_eq!(rm1_unit, m("10"), "成品成本 20 全部摊给唯一子件（2 件 → 单价 10）");
        // 成品已无库存 → 再拆拒绝
        assert!(disassemble(&db, p, d, "FG", &[("RM1".into(), m("1"))], "拆").is_err());
    }
}
