//! 制造业成本核算
//!
//! 支持生产订单的成本归集、分配与完工结转。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbError, DbResult, FinError};
use crate::scm::{BomItem, ProdStatus, ProductionOrder};

// ===========================================================================
// 成本归集
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum CostType {
    Material,
    Labor,
    Overhead,
}

impl CostType {
    pub fn label(self) -> &'static str {
        match self {
            CostType::Material => "直接材料",
            CostType::Labor => "直接人工",
            CostType::Overhead => "制造费用",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            CostType::Material => "material",
            CostType::Labor => "labor",
            CostType::Overhead => "overhead",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProdCostItem {
    pub id: i64,
    pub po_id: i64,
    pub cost_type: CostType,
    pub amount: Money,
    pub memo: String,
}

// ===========================================================================
// 数据库操作
// ===========================================================================

pub fn add_cost(db: &Db, po_id: i64, cost_type: CostType, amount: Money, memo: &str) -> DbResult<i64> {
    let id = add_cost_of(db.conn(), po_id, cost_type, amount, memo)?;
    Ok(id)
}

/// 同 `add_cost`，但只依赖连接：领料要把「扣库存 + 归集成本 + 出凭证」放进同一事务。
pub fn add_cost_of(
    conn: &rusqlite::Connection,
    po_id: i64,
    cost_type: CostType,
    amount: Money,
    memo: &str,
) -> DbResult<i64> {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    conn.execute(
        "INSERT INTO prod_cost(po_id, cost_type, amount, memo, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            po_id,
            cost_type.code(),
            crate::money_param(amount),
            memo,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_prod_cost(db: &Db, po_id: i64) -> DbResult<(Money, Money, Money)> {
    // 使用TEXT聚合避免浮点精度问题
    let mat_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='material'";
    let lab_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='labor'";
    let oh_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='overhead'";
    
    let mat_str: String = db.conn().query_row(mat_sql, [po_id], |r| r.get(0))?;
    let lab_str: String = db.conn().query_row(lab_sql, [po_id], |r| r.get(0))?;
    let oh_str: String = db.conn().query_row(oh_sql, [po_id], |r| r.get(0))?;
    
    // 解析累加公式
    let parse_sum = |s: &str| -> Money {
        if s == "0" { return Money::ZERO; }
        s.split('+').map(|x| Money::parse_or_zero(x)).sum()
    };
    
    Ok((
        parse_sum(&mat_str),
        parse_sum(&lab_str),
        parse_sum(&oh_str),
    ))
}

/// 同 `get_prod_cost`，但只依赖连接：完工结转要在自己的事务里读归集成本。
pub fn get_prod_cost_of(conn: &rusqlite::Connection, po_id: i64) -> DbResult<(Money, Money, Money)> {
    let mat_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='material'";
    let lab_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='labor'";
    let oh_sql = "SELECT COALESCE(GROUP_CONCAT(amount, '+'), '0') FROM prod_cost WHERE po_id=? AND cost_type='overhead'";

    let mat_str: String = conn.query_row(mat_sql, [po_id], |r| r.get(0))?;
    let lab_str: String = conn.query_row(lab_sql, [po_id], |r| r.get(0))?;
    let oh_str: String = conn.query_row(oh_sql, [po_id], |r| r.get(0))?;

    let parse_sum = |s: &str| -> Money {
        if s == "0" {
            return Money::ZERO;
        }
        s.split('+').map(|x| Money::parse_or_zero(x)).sum()
    };

    Ok((parse_sum(&mat_str), parse_sum(&lab_str), parse_sum(&oh_str)))
}

pub fn get_prod_total_cost(db: &Db, po_id: i64) -> DbResult<Money> {
    let (mat, lab, oh) = get_prod_cost(db, po_id)?;
    Ok(mat + lab + oh)
}

// ===========================================================================
// 在制品成本 / 成本差异 / 成本分摊
// ===========================================================================

/// 在制品成本：某期间内未完工生产订单的累计成本合计
#[derive(Clone, Debug, serde::Serialize)]
pub struct WipRow {
    pub po_id: i64,
    pub no: String,
    pub item_name: String,
    pub qty: Money,
    pub material: Money,
    pub labor: Money,
    pub overhead: Money,
    pub total: Money,
}

/// 在制品汇总（按期间列未完工订单）
pub fn wip_cost(db: &Db, period: Period) -> DbResult<Vec<WipRow>> {
    let orders = crate::scm::prod_list(db, period, None)?;
    let mut out = Vec::new();
    for o in orders {
        if matches!(o.status, ProdStatus::Completed | ProdStatus::Cancelled) {
            continue;
        }
        let (m, l, oh) = get_prod_cost(db, o.id)?;
        out.push(WipRow {
            po_id: o.id,
            no: o.no,
            item_name: o.item_name,
            qty: o.planned_qty,
            material: m,
            labor: l,
            overhead: oh,
            total: m + l + oh,
        });
    }
    Ok(out)
}

/// 制造费用分摊基准
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum OverheadBase {
    /// 按已归集成本（材料+人工）占比
    #[default]
    Cost,
    /// 按直接人工占比
    Labor,
    /// 按计划产量占比
    Qty,
}

impl OverheadBase {
    pub fn label(self) -> &'static str {
        match self {
            OverheadBase::Cost => "按成本占比",
            OverheadBase::Labor => "按直接人工",
            OverheadBase::Qty => "按计划产量",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "labor" | "人工" => OverheadBase::Labor,
            "qty" | "产量" | "plan" => OverheadBase::Qty,
            _ => OverheadBase::Cost,
        }
    }
}

fn overhead_base_sum(wip: &[WipRow], base: OverheadBase) -> Money {
    match base {
        OverheadBase::Cost => wip.iter().map(|w| w.material + w.labor).sum(),
        OverheadBase::Labor => wip.iter().map(|w| w.labor).sum(),
        OverheadBase::Qty => wip.iter().map(|w| w.qty).sum(),
    }
}

/// 制造费用分摊：把一笔制造费用总额按所选基准分摊到各在制订单。
/// 默认按成本占比；`overhead_allocate_with` 可指定基准，`apply` 落地写入 prod_cost。
pub fn overhead_allocate(db: &Db, period: Period, amount: Money) -> DbResult<Vec<(i64, Money)>> {
    overhead_allocate_with(db, period, amount, OverheadBase::Cost, false)
}

/// 按基准分摊制造费用，可选落地写入 prod_cost(Overhead)
pub fn overhead_allocate_with(
    db: &Db,
    period: Period,
    amount: Money,
    base: OverheadBase,
    apply: bool,
) -> DbResult<Vec<(i64, Money)>> {
    // 落地时才开写事务（试算不加写锁）。判重与逐单 add_cost 必须在同一事务内：
    // 否则并发请求都能通过"已分摊？"检查，各归集一次；中途失败也会留下半批数据。
    let tx = if apply { Some(db.write_tx()?) } else { None };
    if let Some(tx) = &tx {
        let already: i64 = tx.query_row(
            "SELECT COUNT(*) FROM prod_cost pc JOIN production_order o ON o.id = pc.po_id
             WHERE o.period=?1 AND pc.cost_type='overhead' AND pc.memo='制造费用分摊'",
            rusqlite::params![period.ymm()],
            |r| r.get(0),
        )?;
        if already > 0 {
            return Err(DbError::Fin(FinError::msg(
                "本期已执行过制造费用分摊，请勿重复分摊",
            )));
        }
    }
    let wip = wip_cost(db, period)?;
    let base_sum = overhead_base_sum(&wip, base);
    if base_sum.is_zero() || amount.is_zero() {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(wip.len());
    let mut assigned = Money::ZERO;
    let n = wip.len();
    for (i, w) in wip.iter().enumerate() {
        let b = match base {
            OverheadBase::Cost => w.material + w.labor,
            OverheadBase::Labor => w.labor,
            OverheadBase::Qty => w.qty,
        };
        let share = if i == n - 1 {
            amount - assigned // 尾差给最后一单
        } else {
            // 分摊基数合计为 0 → 按 0 分摊，差额全部由最后一单吸收
            //（原"除零返 0"口径显式化，行为不变）
            ((b * amount.inner()))
                .checked_div(base_sum.inner())
                .unwrap_or(Money::ZERO)
                .round2()
        };
        assigned += share;
        out.push((w.po_id, share));
        if apply && !share.is_zero() {
            if let Some(tx) = &tx {
                add_cost_of(tx, w.po_id, CostType::Overhead, share, "制造费用分摊")?;
            }
        }
    }
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(out)
}

/// 成本差异：实际累计成本 vs 标准成本（按 BOM 参考成本 × 计划量）。
/// 返回 (实际成本, 标准成本, 差异=实际−标准)。
pub fn cost_variance(db: &Db, po_id: i64) -> DbResult<(Money, Money, Money)> {
    let order = get_prod_order(db, po_id)?.ok_or_else(|| FinError::msg("生产订单不存在"))?;
    let actual = get_prod_total_cost(db, po_id)?;
    let standard = crate::scm::bom_cost_rollup(db, &order.item_code, order.planned_qty)?;
    Ok((actual, standard, actual - standard))
}

/// 成本差异报表行
#[derive(Clone, Debug, serde::Serialize)]
pub struct VarianceRow {
    pub po_id: i64,
    pub no: String,
    pub item_name: String,
    pub planned_qty: Money,
    pub actual: Money,
    pub standard: Money,
    pub variance: Money,
    pub variance_pct: f64,
}

/// 成本差异报表：按期间列出各订单 实际 vs 标准 成本差异。
pub fn cost_variance_report(db: &Db, period: Period) -> DbResult<Vec<VarianceRow>> {
    let mut out = Vec::new();
    for o in crate::scm::prod_list(db, period, None)? {
        if matches!(o.status, ProdStatus::Cancelled) {
            continue;
        }
        let actual = get_prod_total_cost(db, o.id)?;
        let standard = crate::scm::bom_cost_rollup(db, &o.item_code, o.planned_qty)?;
        let variance = actual - standard;
        let variance_pct = if standard.is_zero() {
            0.0
        } else {
            (variance.to_f64() / standard.to_f64()) * 100.0
        };
        out.push(VarianceRow {
            po_id: o.id,
            no: o.no,
            item_name: o.item_name,
            planned_qty: o.planned_qty,
            actual,
            standard,
            variance,
            variance_pct,
        });
    }
    Ok(out)
}

/// 成本预测报表行
#[derive(Clone, Debug, serde::Serialize)]
pub struct ForecastRow {
    pub po_id: i64,
    pub no: String,
    pub item_name: String,
    pub planned_qty: Money,
    pub forecast: Money,
}

/// 成本预测报表：按 BOM 参考成本 × 计划量预测各生产订单的物料成本。
pub fn cost_forecast_report(db: &Db, period: Period) -> DbResult<Vec<ForecastRow>> {
    let mut out = Vec::new();
    for o in crate::scm::prod_list(db, period, None)? {
        if matches!(o.status, ProdStatus::Cancelled) {
            continue;
        }
        let forecast = crate::scm::bom_cost_rollup(db, &o.item_code, o.planned_qty)?;
        out.push(ForecastRow {
            po_id: o.id,
            no: o.no,
            item_name: o.item_name,
            planned_qty: o.planned_qty,
            forecast,
        });
    }
    Ok(out)
}

/// 成本预测：按 BOM 参考成本 × 计划量预测某生产订单的物料成本
pub fn cost_forecast(db: &Db, po_id: i64) -> DbResult<Money> {
    let order = get_prod_order(db, po_id)?.ok_or_else(|| FinError::msg("生产订单不存在"))?;
    crate::scm::bom_cost_rollup(db, &order.item_code, order.planned_qty)
}

// ===========================================================================
// BOM展开与领料
// ===========================================================================

pub fn prod_issue_materials(
    db: &Db,
    po_id: i64,
    issue_date: NaiveDate,
    period: Period,
    who: &str,
) -> DbResult<Vec<(String, Money, Money)>> {
    use crate::scm::bom_list;
    use crate::business::{stock_insert_of, StockMove, StockKind};
    
    let order = match get_prod_order(db, po_id)? {
        Some(o) => o,
        None => return Err(FinError::msg("生产订单不存在").into()),
    };
    
    let bom_items = bom_list(db, &order.item_code)?;
    if bom_items.is_empty() {
        return Err(FinError::msg(format!("产品 {} 无BOM，无法领料", order.item_code)).into());
    }
    
    // 先把所有物料的单位成本算完再开始写库：`get_item_cost` 对没有采购记录的物料会
    // 报「物料成本未知」，若留在写入循环里，前面的领料已经落了库和成本归集、后面的
    // 没做，这张订单的成本就永久停在半路上。
    let mut planned: Vec<(String, Money, Money)> = Vec::with_capacity(bom_items.len());
    for item in &bom_items {
        let qty_required = (item.qty * order.planned_qty * (Money::ONE + item.loss_rate)).abs();
        let unit_cost = get_item_cost(db, &item.child_code, period.ymm())?;
        planned.push((item.child_code.clone(), qty_required, qty_required * unit_cost));
    }

    let mut results = Vec::new();
    // 归集每笔领料的分录：借 生产成本-直接材料，贷 各物料科目（数量核算）
    let mut credit_entries: Vec<(String, Money, Money)> = Vec::new(); // (item, qty, amount)

    // 扣库存、归集成本、出结转凭证必须在同一事务：原先各写各的提交，中途失败
    // （最常见的是末尾出凭证报错）会留下「库存已扣、成本已归集、账上无凭证」的
    // 半成品，这张订单的成本再也补不齐。
    let tx = db.write_tx()?;
    for (child_code, qty, amount) in planned {
        let mut move_record = StockMove {
            id: 0,
            period,
            biz_date: issue_date,
            kind: StockKind::OtherOut,
            item: child_code.clone(),
            warehouse: String::new(),
            batch_no: String::new(),
            qty: -qty,
            price: Money::ZERO,
            amount: Money::ZERO,
            voucher_id: None,
            memo: format!("生产领料 PO#{}", order.no),
        };

        stock_insert_of(&tx, &mut move_record)?;
        // 归集材料成本到 prod_cost（供完工结转取用）
        add_cost_of(&tx, po_id, CostType::Material, amount, "生产领料")?;
        credit_entries.push((child_code.clone(), qty, amount));
        results.push((child_code, qty, amount));
    }

    // 生成领料结转凭证：借 500101 / 贷 各物料科目
    material_voucher_in(&tx, period, issue_date, &order, &credit_entries, who)?;
    tx.commit()?;

    Ok(results)
}

/// 生成生产领料结转凭证：借 生产成本-直接材料(500101) / 贷 各物料科目。
/// 借方可选（默认 500101），用于把已领用的原料成本计入生产成本归集。
pub fn material_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    order: &ProductionOrder,
    issues: &[(String, Money, Money)],
    who: &str,
) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let id = material_voucher_in(&tx, period, date, order, issues, who)?;
    tx.commit()?;
    Ok(id)
}

/// 在调用方事务内生成领料结转凭证（不发 BEGIN、不提交）。
/// 领料的「扣库存 + 归集成本 + 出凭证」必须同一事务。
pub fn material_voucher_in(
    tx: &rusqlite::Transaction,
    period: Period,
    date: NaiveDate,
    order: &ProductionOrder,
    issues: &[(String, Money, Money)],
    who: &str,
) -> DbResult<i64> {
    use fincore::{AuxRef, Entry, Voucher, VoucherSource};
    let total: Money = issues.iter().map(|i| i.2).sum();
    if total.is_zero() {
        return Ok(0);
    }
    let no = crate::vouchers::next_no_of(tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("生产领料 PO#{}", order.no);
    v.push_entry(Entry {
        debit: total,
        ..Entry::new(1, "500101", "生产领料")
    });
    for (idx, (code, qty, amount)) in issues.iter().enumerate() {
        v.push_entry(Entry {
            credit: *amount,
            aux: AuxRef { item: Some(code.clone()), ..Default::default() },
            qty: Some(*qty),
            ..Entry::new(idx as i32 + 2, code, "生产领料")
        });
    }
    v.renumber();
    crate::vouchers::save_in(tx, &mut v)
}

fn get_item_cost(db: &Db, item_code: &str, period_ymm: i32) -> DbResult<Money> {
    let sql = "SELECT price FROM stock_move
               WHERE item=? AND kind='purchase' AND period<=?
               ORDER BY biz_date DESC LIMIT 1";

    if let Some(price) = db
        .conn()
        .query_row(sql, [item_code, &period_ymm.to_string()], |r| r.get::<_, String>(0))
        .optional()?
    {
        let p = Money::parse_or_zero(&price);
        if p.is_positive() {
            return Ok(p);
        }
    }
    // 回退：计价配置的标准价（Web 侧入库不带采购单价；与盘点/暂估同口径）。
    // 两者都缺才报错——错误文案保持不变，既有测试兼容。
    let std = crate::business::item_standard_cost(db, item_code)?;
    if std.is_positive() {
        return Ok(std);
    }
    Err(FinError::msg("物料成本未知").into())
}

// ===========================================================================
// 退料管理
// ===========================================================================

/// 生产退料：把某物料的一部分退回库存（冲减领料）。
/// 生成一条「其他入库」流水，数量为正、金额按最近采购价。
pub fn prod_return_materials(
    db: &Db,
    po_id: i64,
    return_date: NaiveDate,
    period: Period,
    item_code: &str,
    qty: Money,
    memo: &str,
) -> DbResult<i64> {
    use crate::business::{stock_insert, StockMove, StockKind};

    let order = get_prod_order(db, po_id)?.ok_or_else(|| FinError::msg("生产订单不存在"))?;
    if order.status != ProdStatus::InProgress && order.status != ProdStatus::Released {
        return Err(FinError::msg("只有已下达/进行中的订单能退料").into());
    }
    if qty.is_negative() || qty.is_zero() {
        return Err(FinError::msg("退料数量必须为正数").into());
    }
    let unit_cost = get_item_cost(db, item_code, period.ymm())?;
    let amount = (qty * unit_cost).round2();
    stock_insert(
        db,
        &StockMove {
            id: 0,
            period,
            biz_date: return_date,
            kind: StockKind::OtherIn,
            item: item_code.to_string(),
            warehouse: String::new(),
            batch_no: String::new(),
            qty,
            price: unit_cost,
            amount,
            voucher_id: None,
            memo: format!("生产退料 PO#{} {}", order.no, memo),
        },
    )
}

// ===========================================================================
// 完工入库
// ===========================================================================

pub fn prod_complete(
    db: &Db,
    po_id: i64,
    complete_date: NaiveDate,
    period: Period,
    completed_qty: Money,
    who: &str,
) -> DbResult<i64> {
    use crate::business::{stock_insert_of, StockMove, StockKind};
    
    let order = match get_prod_order(db, po_id)? {
        Some(o) => o,
        None => return Err(FinError::msg("生产订单不存在").into()),
    };
    
    if order.status != ProdStatus::InProgress {
        return Err(FinError::msg("只有进行中的订单才能完工入库").into());
    }

    // 工序检验门槛（对标金蝶）：工艺路线含检验点且本单尚无检验记录 → 拒绝
    if prod_qc_required(db, &order.item_code)? && prod_qc_list(db, po_id)?.is_empty() {
        return Err(FinError::state(
            "该产品工艺路线含工序检验点，请先录入工序检验单（工序报工页「工序检验」）",
        )
        .into());
    }
    
    // 归集成本、推进订单、入库、结转凭证收进同一事务：
    // - 订单推进是条件更新（只允许 in_progress → completed），抢输的一方整体回滚，
    //   不会各入库一次、重复结转成本，也不再需要「抢输后手工回收入库行」的补丁；
    // - 出凭证失败则整个事务回滚，订单仍是进行中、库存未动，可以直接重试。
    let tx = db.write_tx()?;
    let (mat, lab, oh) = get_prod_cost_of(&tx, po_id)?;
    let total_cost = mat + lab + oh;
    let unit_cost = if completed_qty > Money::ZERO {
        total_cost
            .checked_div(completed_qty)
            .expect("completed_qty 已判 > 0")
    } else {
        Money::ZERO
    };

    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let advanced = tx.execute(
        "UPDATE production_order SET status='completed', completed_qty=?2, updated_at=?3
         WHERE id=?1 AND status='in_progress'",
        rusqlite::params![po_id, crate::exact_param(completed_qty), now],
    )?;
    if advanced == 0 {
        return Err(FinError::state("订单状态已被他人变更，请刷新后重试").into());
    }

    let mut move_record = StockMove {
        id: 0,
        period,
        biz_date: complete_date,
        kind: StockKind::OtherIn,
        item: order.item_code.clone(),
        warehouse: String::new(),
        batch_no: String::new(),
        qty: completed_qty,
        price: unit_cost,
        amount: total_cost,
        voucher_id: None,
        memo: format!("完工入库 PO#{}", order.no),
    };
    let move_id = stock_insert_of(&tx, &mut move_record)?;

    // 生成完工结转凭证：借 库存商品(140501) / 贷 生产成本各要素。
    // 材料、人工、制造费用分别由 500101 / 500102 / 500103 承接。
    completion_voucher_in(&tx, order.period, complete_date, &order, completed_qty, who)?;
    tx.commit()?;

    Ok(move_id)
}

/// 生成完工入库结转凭证：借 库存商品(140501, 数量核算) / 贷 生产成本各要素科目。
/// 金额取该订单累计归集的 材料/人工/制造费用（prod_cost）。
pub fn completion_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    order: &ProductionOrder,
    completed_qty: Money,
    who: &str,
) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let id = completion_voucher_in(&tx, period, date, order, completed_qty, who)?;
    tx.commit()?;
    Ok(id)
}

/// 在调用方事务内生成完工结转凭证（不发 BEGIN、不提交）。
pub fn completion_voucher_in(
    tx: &rusqlite::Transaction,
    period: Period,
    date: NaiveDate,
    order: &ProductionOrder,
    completed_qty: Money,
    who: &str,
) -> DbResult<i64> {
    use fincore::{AuxRef, Entry, Voucher, VoucherSource};
    let (mat, lab, oh) = get_prod_cost_of(tx, order.id)?;
    let total = mat + lab + oh;
    if total.is_zero() || completed_qty.is_zero() {
        return Ok(0);
    }
    let no = crate::vouchers::next_no_of(tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("完工入库 PO#{}", order.no);
    v.push_entry(Entry {
        debit: total,
        aux: AuxRef { item: Some(order.item_code.clone()), ..Default::default() },
        qty: Some(completed_qty),
        ..Entry::new(1, "140501", "完工入库")
    });
    let mut idx = 2;
    for (cost_type, qty_money) in [
        (CostType::Material, mat),
        (CostType::Labor, lab),
        (CostType::Overhead, oh),
    ] {
        if qty_money.is_zero() {
            continue;
        }
        let account = match cost_type {
            CostType::Material => "500101",
            CostType::Labor => "500102",
            CostType::Overhead => "500103",
        };
        v.push_entry(Entry {
            credit: qty_money,
            ..Entry::new(idx, account, "完工入库")
        });
        idx += 1;
    }
    v.renumber();
    crate::vouchers::save_in(tx, &mut v)
}

/// 某生产订单的领料流水笔数（按单限额领料的累计口径）
pub fn prod_issue_count(db: &Db, po_no: &str) -> DbResult<i64> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM stock_move WHERE kind='other_out' AND memo=?1",
        [format!("生产领料 PO#{po_no}")],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// 生产订单开工：已下达 → 生产中（条件更新防并发；完工入库的前置状态）。
pub fn prod_start(db: &Db, po_id: i64) -> DbResult<()> {
    let tx = db.write_tx()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let n = tx.execute(
        "UPDATE production_order SET status='in_progress', updated_at=?2
         WHERE id=?1 AND status='released'",
        rusqlite::params![po_id, now],
    )?;
    if n == 0 {
        return Err(FinError::state("仅「已下达」的订单可开工（或状态已被他人变更）").into());
    }
    tx.commit()?;
    Ok(())
}

/// 生产订单变更（仅草稿/已下达）：数量不得低于已完工数量；计划日期/备注可改；
/// 逐字段写入 `order_change_log(order_type='prod')` 留痕。
pub fn prod_update(
    db: &Db,
    po_id: i64,
    qty: Option<Money>,
    plan_start: Option<&str>,
    plan_end: Option<&str>,
    memo: Option<&str>,
    who: &str,
) -> DbResult<()> {
    let order = get_prod_order(db, po_id)?.ok_or_else(|| FinError::not_found("生产订单"))?;
    if !matches!(order.status, ProdStatus::Draft | ProdStatus::Released) {
        return Err(FinError::state("仅「草稿/已下达」的订单可变更（已开工请先完工）").into());
    }
    if let Some(q) = qty {
        if !q.is_positive() {
            return Err(FinError::msg("计划数量必须大于 0").into());
        }
        if q < order.completed_qty {
            return Err(FinError::state(format!(
                "计划数量 {} 不能低于已完工数量 {}",
                q.fmt_qty(),
                order.completed_qty.fmt_qty()
            ))
            .into());
        }
    }
    let tx = db.write_tx()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut logs: Vec<(&str, String, String)> = Vec::new();
    if let Some(q) = qty {
        if q != order.planned_qty {
            tx.execute(
                "UPDATE production_order SET planned_qty=?2, updated_at=?3 WHERE id=?1",
                rusqlite::params![po_id, crate::exact_param(q), now],
            )?;
            logs.push(("计划数量", order.planned_qty.fmt_qty(), q.fmt_qty()));
        }
    }
    if let Some(ps) = plan_start {
        let ps = ps.trim();
        if ps != order.plan_start {
            tx.execute(
                "UPDATE production_order SET plan_start=?2, updated_at=?3 WHERE id=?1",
                rusqlite::params![po_id, ps, now],
            )?;
            logs.push(("计划开工", order.plan_start.clone(), ps.to_string()));
        }
    }
    if let Some(pe) = plan_end {
        let pe = pe.trim();
        if pe != order.plan_end {
            tx.execute(
                "UPDATE production_order SET plan_end=?2, updated_at=?3 WHERE id=?1",
                rusqlite::params![po_id, pe, now],
            )?;
            logs.push(("计划完工", order.plan_end.clone(), pe.to_string()));
        }
    }
    if let Some(m) = memo {
        let m = m.trim();
        if m != order.memo {
            tx.execute(
                "UPDATE production_order SET memo=?2, updated_at=?3 WHERE id=?1",
                rusqlite::params![po_id, m, now],
            )?;
            logs.push(("备注", order.memo.clone(), m.to_string()));
        }
    }
    for (field, old_v, new_v) in logs {
        crate::scm2::change_log_add_conn(&tx, "prod", po_id, field, &old_v, &new_v, who)?;
    }
    tx.commit()?;
    Ok(())
}

/// 生产订单取消（仅草稿/已下达；已开工/完工拒绝——先完工或退料）。
pub fn prod_cancel(db: &Db, po_id: i64, who: &str) -> DbResult<()> {
    let order = get_prod_order(db, po_id)?.ok_or_else(|| FinError::not_found("生产订单"))?;
    if !matches!(order.status, ProdStatus::Draft | ProdStatus::Released) {
        return Err(FinError::state("仅「草稿/已下达」的订单可取消（已开工请先完工/退料）").into());
    }
    let tx = db.write_tx()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let n = tx.execute(
        "UPDATE production_order SET status='cancelled', updated_at=?2
         WHERE id=?1 AND status IN ('draft','released')",
        rusqlite::params![po_id, now],
    )?;
    if n == 0 {
        return Err(FinError::state("状态已被他人变更，请刷新后重试").into());
    }
    crate::scm2::change_log_add_conn(&tx, "prod", po_id, "状态", order.status.code(), "cancelled", who)?;
    tx.commit()?;
    Ok(())
}

// ===========================================================================
// 工序检验（对标金蝶工序质检：合格 / 返修 / 报废 / 让步接收）
// ===========================================================================

/// 工序检验单
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProdQc {
    pub id: i64,
    pub no: String,
    pub prod_id: i64,
    pub item_code: String,
    pub qty_insp: Money,
    pub qty_pass: Money,
    pub qty_fail: Money,
    /// rework 返修 / scrap 报废 / concession 让步接收（无不合格时为空）
    pub disposition: String,
    /// pass / partial / fail
    pub result: String,
    pub date: String,
    pub inspector: String,
    pub memo: String,
    pub created_at: String,
}

fn map_prod_qc(r: &rusqlite::Row) -> rusqlite::Result<ProdQc> {
    Ok(ProdQc {
        id: r.get(0)?,
        no: r.get(1)?,
        prod_id: r.get(2)?,
        item_code: r.get(3)?,
        qty_insp: Money::parse_or_zero(&r.get::<_, String>(4)?),
        qty_pass: Money::parse_or_zero(&r.get::<_, String>(5)?),
        qty_fail: Money::parse_or_zero(&r.get::<_, String>(6)?),
        disposition: r.get(7)?,
        result: r.get(8)?,
        date: r.get(9)?,
        inspector: r.get(10)?,
        memo: r.get(11)?,
        created_at: r.get(12)?,
    })
}

const QC_COLS: &str =
    "id,no,prod_id,item_code,qty_insp,qty_pass,qty_fail,disposition,result,date,inspector,memo,created_at";

pub fn prod_qc_list(db: &Db, prod_id: i64) -> DbResult<Vec<ProdQc>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {QC_COLS} FROM prod_qc WHERE prod_id=?1 ORDER BY id DESC"
    ))?;
    let rows = st
        .query_map(rusqlite::params![prod_id], map_prod_qc)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 工艺路线是否存在工序检验点
pub fn prod_qc_required(db: &Db, item_code: &str) -> DbResult<bool> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM routing WHERE item_code=?1 AND qc_required=1",
        rusqlite::params![item_code],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn prod_qc_next_no(db: &Db, period: Period) -> DbResult<String> {
    let mut n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM prod_qc WHERE no LIKE ?1",
        rusqlite::params![format!("QC{}%", period.ymm())],
        |r| r.get(0),
    )?;
    loop {
        n += 1;
        let no = format!("QC{}{:03}", period.ymm(), n);
        let exists: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM prod_qc WHERE no=?1",
            rusqlite::params![no],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Ok(no);
        }
    }
}

/// 录入工序检验：仅「生产中」订单；不合格必须选处置；**报废同步扣减计划量**
/// （不得低于已完工）并写订单变更留痕。返回 (id, 单号, 结论)。
#[allow(clippy::too_many_arguments)]
pub fn prod_qc_save(
    db: &Db,
    prod_id: i64,
    qty_insp: Money,
    qty_fail: Money,
    disposition: &str,
    date: NaiveDate,
    memo: &str,
    who: &str,
) -> DbResult<(i64, String, String)> {
    let order = get_prod_order(db, prod_id)?.ok_or_else(|| FinError::not_found("生产订单"))?;
    if order.status != ProdStatus::InProgress {
        return Err(FinError::state("仅「生产中」的订单可录工序检验").into());
    }
    if !qty_insp.is_positive() {
        return Err(FinError::msg("检验数量必须大于 0").into());
    }
    if qty_fail.is_negative() || qty_fail > qty_insp {
        return Err(FinError::msg("不合格数必须在 0 ~ 检验数量之间").into());
    }
    let dispo = disposition.trim();
    if !qty_fail.is_zero() && !matches!(dispo, "rework" | "scrap" | "concession") {
        return Err(FinError::msg(
            "存在不合格品时必须选择处置：rework 返修 / scrap 报废 / concession 让步接收",
        )
        .into());
    }
    let pass = qty_insp - qty_fail;
    let result = if qty_fail.is_zero() {
        "pass"
    } else if pass.is_zero() {
        "fail"
    } else {
        "partial"
    };
    let period = Period::from_date(date);
    let no = prod_qc_next_no(db, period)?;
    let tx = db.write_tx()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    // 报废：同步扣减计划量（对标金蝶：报废减少订单产出计划）
    if dispo == "scrap" {
        let new_planned = order.planned_qty - qty_fail;
        if new_planned < order.completed_qty {
            return Err(FinError::state(format!(
                "报废 {} 后计划量 {} 低于已完工 {}，请先调整订单数量",
                qty_fail.fmt_qty(),
                new_planned.fmt_qty(),
                order.completed_qty.fmt_qty()
            ))
            .into());
        }
        tx.execute(
            "UPDATE production_order SET planned_qty=?2, updated_at=?3 WHERE id=?1",
            rusqlite::params![prod_id, crate::exact_param(new_planned), now],
        )?;
        crate::scm2::change_log_add_conn(
            &tx,
            "prod",
            prod_id,
            "计划数量（报废扣减）",
            &order.planned_qty.fmt_qty(),
            &new_planned.fmt_qty(),
            who,
        )?;
    }
    tx.execute(
        "INSERT INTO prod_qc(no,prod_id,item_code,qty_insp,qty_pass,qty_fail,disposition,result,
         date,inspector,memo,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        rusqlite::params![
            no,
            prod_id,
            order.item_code,
            crate::exact_param(qty_insp),
            crate::exact_param(pass),
            crate::exact_param(qty_fail),
            dispo,
            result,
            date.format("%Y-%m-%d").to_string(),
            who,
            memo,
            now
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok((id, no, result.to_string()))
}

/// 委外加工费确认：归集人工（CostType::Labor → 完工结转由 500102 承接）
/// + 生成应付凭证 借 500102 / 贷 应付科目（供应商辅助）。仅委外订单、金额 > 0；期间随订单。
pub fn outsource_fee(db: &Db, po_id: i64, amount: Money, date: NaiveDate, who: &str) -> DbResult<i64> {
    if !amount.is_positive() {
        return Err(FinError::msg("加工费必须大于 0").into());
    }
    let order = get_prod_order(db, po_id)?.ok_or_else(|| FinError::msg("生产订单不存在"))?;
    if order.order_kind != "outsourcing" {
        return Err(FinError::msg("仅委外订单可确认加工费").into());
    }
    if order.supplier_code.trim().is_empty() {
        return Err(FinError::msg("委外订单缺少供应商，请先补充").into());
    }
    use fincore::{AuxRef, Entry, Voucher, VoucherSource};
    let biz = db.options().biz_accounts.clone();
    let period = order.period;
    let tx = db.write_tx()?;
    add_cost_of(&tx, po_id, CostType::Labor, amount, "委外加工费")?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("委外加工费 {}", order.no);
    v.push_entry(Entry {
        debit: amount,
        ..Entry::new(1, "500102", "委外加工费")
    });
    v.push_entry(Entry {
        credit: amount,
        aux: AuxRef {
            supplier: Some(order.supplier_code.clone()),
            ..Default::default()
        },
        ..Entry::new(2, biz.ap.as_str(), "委外加工费")
    });
    v.renumber();
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    tx.commit()?;
    Ok(vid)
}

pub fn get_prod_order(db: &Db, po_id: i64) -> DbResult<Option<ProductionOrder>> {
    let row = db.conn()
        .query_row(
            "SELECT id, no, period, date, item_code, item_name, planned_qty, completed_qty, status, work_center, prepared_by, memo, order_kind, supplier_code, supplier_name, plan_start, plan_end
             FROM production_order WHERE id=?",
            [po_id],
            |r| Ok(ProductionOrder {
                id: r.get(0)?,
                no: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: r.get(3)?,
                item_code: r.get(4)?,
                item_name: r.get(5)?,
                planned_qty: Money::parse_or_zero(&r.get::<_, String>(6)?),
                completed_qty: Money::parse_or_zero(&r.get::<_, String>(7)?),
                status: crate::scm::prod_status_from(&r.get::<_, String>(8)?),
                work_center: r.get(9)?,
                prepared_by: r.get(10)?,
                memo: r.get(11)?,
                order_kind: r.get(12)?,
                supplier_code: r.get(13)?,
                supplier_name: r.get(14)?,
                plan_start: r.get(15)?,
                plan_end: r.get(16)?,
            }),
        );
    match row {
        Ok(order) => Ok(Some(order)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    
    #[test]
    fn cost_tracking() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        
        let tx = db.write_tx().unwrap();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        tx.execute(
            "INSERT INTO production_order(period, no, date, item_code, item_name, planned_qty, completed_qty, status, work_center, prepared_by, memo, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                p.ymm(), "SC2026010001", "2026-01-05", "1001", "成品A",
                "100", "0", "in_progress", "WC01", "u1", "",
                now.clone(), now.clone()
            ],
        ).unwrap();
        let po_id = tx.last_insert_rowid();
        tx.commit().unwrap();
        
        add_cost(&db, po_id, CostType::Material, Money::parse("5000").unwrap(), "领料").unwrap();
        add_cost(&db, po_id, CostType::Labor, Money::parse("1000").unwrap(), "人工").unwrap();
        add_cost(&db, po_id, CostType::Overhead, Money::parse("500").unwrap(), "制造费用").unwrap();
        
        let (mat, lab, oh) = get_prod_cost(&db, po_id).unwrap();
        assert_eq!(mat, Money::parse("5000").unwrap());
        assert_eq!(lab, Money::parse("1000").unwrap());
        assert_eq!(oh, Money::parse("500").unwrap());
        
        let total = get_prod_total_cost(&db, po_id).unwrap();
        assert_eq!(total, Money::parse("6500").unwrap());

        // 在制品成本
        let wip = wip_cost(&db, p).unwrap();
        assert_eq!(wip.len(), 1);
        assert_eq!(wip[0].total, m("6500"));

        // 制造费用分摊（1000 分摊到唯一在制单 → 全部）
        let alloc = overhead_allocate(&db, p, m("1000")).unwrap();
        assert_eq!(alloc.len(), 1);
        assert_eq!(alloc[0].1, m("1000"));
    }

    #[test]
    fn transfer_vouchers_balanced() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let date = NaiveDate::from_ymd(2026, 1, 15);

        fn dc(db: &Db, id: i64) -> (Money, Money) {
            let v = crate::vouchers::get(db, id).unwrap().unwrap();
            (v.entries.iter().map(|e| e.debit).sum(), v.entries.iter().map(|e| e.credit).sum())
        }

        // 生产订单
        let tx = db.write_tx().unwrap();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        tx.execute(
            "INSERT INTO production_order(period, no, date, item_code, item_name, planned_qty, completed_qty, status, work_center, prepared_by, memo, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                p.ymm(), "SC2026010002", "2026-01-02", "140501", "成品X",
                "3", "0", "in_progress", "WC01", "u1", "",
                now.clone(), now.clone()
            ],
        ).unwrap();
        let po_id = tx.last_insert_rowid();
        tx.commit().unwrap();

        let order = ProductionOrder {
            id: po_id, no: "SC2026010002".into(), period: p, date,
            item_code: "140501".into(), item_name: "成品X".into(),
            planned_qty: m("3"), completed_qty: m("0"),
            status: ProdStatus::InProgress, work_center: "WC01".into(),
            prepared_by: "u1".into(), memo: "".into(),
            order_kind: "inhouse".into(),
            supplier_code: String::new(),
            supplier_name: String::new(),
            plan_start: String::new(),
            plan_end: String::new(),
        };

        // 领料结转：借 500101=60 / 贷 140301=60
        let vid = material_voucher(&db, p, date, &order, &[("140301".into(), m("2"), m("60"))], "u1").unwrap();
        let (di, ce) = dc(&db, vid);
        assert_eq!(di, m("60"));
        assert_eq!(ce, m("60"));

        // 归集人工，完工结转：借 140501=90 / 贷 500101=60 + 500102=30
        add_cost(&db, po_id, CostType::Material, m("60"), "领料").unwrap();
        add_cost(&db, po_id, CostType::Labor, m("30"), "人工").unwrap();
        let vcid = completion_voucher(&db, p, date, &order, m("3"), "u1").unwrap();
        let (di, ce) = dc(&db, vcid);
        assert_eq!(di, m("90"));
        assert_eq!(ce, m("90"));

        let v = crate::vouchers::get(&db, vcid).unwrap().unwrap();
        let debit140501 = v.entries.iter().find(|e| e.account_code == "140501").unwrap().debit;
        let credit500101 = v.entries.iter().find(|e| e.account_code == "500101").unwrap().credit;
        let credit500102 = v.entries.iter().find(|e| e.account_code == "500102").unwrap().credit;
        assert_eq!(debit140501, m("90"));
        assert_eq!(credit500101, m("60"));
        assert_eq!(credit500102, m("30"));
    }

    #[test]
    fn overhead_apply_rejects_duplicate() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let tx = db.write_tx().unwrap();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        tx.execute(
            "INSERT INTO production_order(period, no, date, item_code, item_name, planned_qty, completed_qty, status, work_center, prepared_by, memo, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                p.ymm(), "SC2026010003", "2026-01-02", "140501", "成品Y",
                "5", "0", "in_progress", "WC01", "u1", "",
                now.clone(), now.clone()
            ],
        ).unwrap();
        tx.commit().unwrap();
        // 给在制单归集一部分成本，使分摊基准非零
        let po_id = 1;
        add_cost(&db, po_id, CostType::Material, m("100"), "领料").unwrap();
        add_cost(&db, po_id, CostType::Labor, m("50"), "人工").unwrap();

        // 第一次落地成功
        let alloc = overhead_allocate_with(&db, p, m("30"), OverheadBase::Cost, true).unwrap();
        assert_eq!(alloc.len(), 1);
        assert_eq!(alloc[0].1, m("30"));
        // 第二次落地被拒绝（防重复归集）
        let err = overhead_allocate_with(&db, p, m("30"), OverheadBase::Cost, true).unwrap_err();
        assert!(err.to_string().contains("请勿重复分摊"), "实际错误：{err}");
    }
}
