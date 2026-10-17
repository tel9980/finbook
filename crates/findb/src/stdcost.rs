//! 标准成本差异分析
//!
//! ## 标准成本的意义不在"计价"，在"差异"
//!
//! 有了标准成本，出库按预设单价走、结存金额也稳定——但这本身不是价值，
//! **能算出差异才是**。标准成本系统的全部管理意义就一句话：把「实际花了多少」
//! 和「应该花多少」的差额拆开，让管理层知道贵在哪。
//!
//! ## 本模块算哪两档差异
//!
//! | 差异 | 公式 | 回答的问题 |
//! |---|---|---|
//! | 采购价格差异 | (实际入库单价 − 标准单价) × 入库数量 | 买贵了还是买便宜了 |
//! | 成本超支差异 | 出库实际成本 − 标准成本 × 出库数量 | 实际耗用比标准贵多少 |
//!
//! 刻意**不做**的三档，以及原因：
//!
//! - **用量差异**（实际耗用 vs 标准耗用）需要「标准 BOM + 标准产量」的完整
//!   标准成本体系。缺它时用量差异算出来的数无法解释，硬给一个数比不给更坏。
//! - **效率差异**（工时 × 标准工时）需要标准工时与人工工资率，仓里没有数据源。
//! - **固定制造费用差异**需要产能与固定费用预算。
//!
//! 这三档的输入都还没有维护界面，硬算就是编。差异表里它们在 `missing` 里
//! 点名说明，比填一个假数诚实。
//!
//! ## 为什么差异必须能追到单据
//!
//! 「这个月成本超支了 3 万」不是可行动的信息，「超支集中在 3 个物料、都是从
//! 华东仓采购」才是。所以每行差异都带构成它的流水 id，界面上能点开看是哪几张
//! 入库单/出库单造成的。

use std::collections::BTreeMap;

use fincore::engine::costing::CostMethod;
use fincore::Money;

use crate::{Db, DbResult};

/// 一个存货的差异汇总
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ItemVariance {
    pub item: String,
    pub item_name: String,
    pub method: String,
    /// 标准单价（0 = 未设标准成本，该行不进差异表）
    pub standard_cost: Money,
    /// 存货档案上的「参考成本」（`aux_entity.props_json.ref_cost`）。
    ///
    /// 刻意并列暴露：BOM 滚算与订单级成本差异用的是 ref_cost，出库计价用的是
    /// standard_cost。**仓库里有两个互不相干的"标准成本"**，可以不一致且不报错——
    /// 同一张成本表上，BOM 说的成本和实际出库成本对不上，而没人会发现。
    pub ref_cost: Money,
    /// 入库数量 / 金额（金额为正）
    pub in_qty: Money,
    pub in_amount: Money,
    /// 本期到岸成本（运费/关税等，已计入 in_amount）
    ///
    /// 单独返回是为了**能解释差异**：只看到一个「采购价格差异 -5000」，
    /// 用户无从判断是货价便宜了还是运费忘了算。
    pub landed_cost: Money,
    /// 其他成本调整（盘点差异、成本结转调整等，已计入 in_amount）
    pub other_adjust: Money,
    /// 实际入库加权单价
    pub actual_unit: Money,
    /// 入库价格差异 = (实际单价 − 标准单价) × 入库数量。正=买贵，负=买便宜
    pub price_variance: Money,
    /// 出库数量（正）/ 实际出库成本（正）
    pub out_qty: Money,
    pub out_amount: Money,
    /// 出库按标准成本应发生的金额
    pub out_standard_amount: Money,
    /// 成本超支差异 = 实际出库成本 − 标准成本 × 出库数量
    pub spend_variance: Money,
    /// 构成该存货差异的流水 id（可点开看是哪几张单据）
    pub move_ids: Vec<i64>,
}

/// 一张存货的成本差异表
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct VarianceReport {
    pub period: String,
    pub rows: Vec<ItemVariance>,
    /// 合计（只有设了标准成本的存货参与）
    pub total_price: Money,
    pub total_spend: Money,
    /// 本期无法计算、需要标准 BOM/工时/产能预算才能算的差异项
    pub missing: Vec<String>,
    /// 提示：设了标准成本方法但没录单价的存货（差异恒为 0，静默失真）
    pub warnings: Vec<String>,
}

/// 成本差异分析（按存货）
///
/// 逐个存货跑它**自己配置的**计价方法算实际成本，而不是全局一个方法 ——
/// 实际企业里原材料用移动加权、产成品用标准成本是常态。
pub fn variance_report(db: &Db, period: fincore::Period) -> DbResult<VarianceReport> {
    let moves = crate::business::stock_list(db, period)?;
    if moves.is_empty() {
        return Ok(VarianceReport {
            period: period.to_string(),
            missing: missing_items(),
            ..Default::default()
        });
    }

    let mut by_item: BTreeMap<String, Vec<&crate::business::StockMove>> = BTreeMap::new();
    for m in &moves {
        by_item.entry(m.item.clone()).or_default().push(m);
    }

    let mut rows: Vec<ItemVariance> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for (item, list) in &by_item {
        let method = crate::business::item_cost_method(db, item)?;
        let std = crate::business::item_standard_cost(db, item)?;
        let ref_cost = item_ref_cost(db, item);
        let name = item_name(db, item);

        // 两个"标准成本"不一致要报出来：BOM 滚算用 ref_cost，出库计价用
        // standard_cost。不一致时同一张成本表上会出现两套成本，且没有任何提示。
        if !ref_cost.is_zero() && !std.is_zero() && ref_cost != std {
            warnings.push(format!(
                "{item} {name}：参考成本（BOM 滚算/订单差异用）{} ≠ 标准成本（出库计价用）{} \
                 —— 同一张成本表会出现两套成本，请统一后再看差异",
                ref_cost.fmt_money(),
                std.fmt_money()
            ));
        }

        let (mut in_qty, mut in_amount) = (Money::ZERO, Money::ZERO);
        let (mut out_qty, mut out_amount) = (Money::ZERO, Money::ZERO);
        // 成本调整（qty=0、只有金额）：到岸成本、盘点差异、成本结转调整都走这里。
        // 它们**不加数量、但确实加成本**，所以必须并入成本口径。
        let (mut landed_amount, mut other_adjust) = (Money::ZERO, Money::ZERO);
        let mut move_ids: Vec<i64> = Vec::new();
        for m in list {
            if m.qty.is_positive() {
                in_qty += m.qty;
                in_amount += m.amount;
            } else if m.qty.is_negative() {
                out_qty += -m.qty;
                out_amount += m.amount; // 出库流水金额记的是负数成本
            } else if m.kind == crate::business::StockKind::Adjust {
                // 区分开是为了能说清差异里有多少是到岸成本、多少是其他调整 ——
                // 混成一个数，用户看到「超支 3 万」也不知道钱花在哪。
                if m.memo.contains("到岸成本") {
                    landed_amount += m.amount;
                } else {
                    other_adjust += m.amount;
                }
            }
            if !m.amount.is_zero() {
                move_ids.push(m.id);
            }
        }
        // 到岸成本计入成本口径：**不加它，差异表算出来的「节约」是假的** ——
        // 货价便宜 5%、运费贵 20% 时，不含运费的差异表会显示节约，
        // 实际成本反而更高，而表面上数字完全正常。
        in_amount += landed_amount + other_adjust;

        // 出库实际成本：出库流水回写的 amount 就是该存货按其**自身**计价方法
        // 算出的成本（stock_summary 已按配置方法回写），直接取用。
        let actual_out = out_amount.abs();
        let actual_unit = if in_qty.is_zero() {
            Money::ZERO
        } else {
            in_amount
                .checked_div(in_qty)
                .unwrap_or(Money::ZERO)
                .round2()
        };

        if method == CostMethod::Standard && std.is_zero() {
            warnings.push(format!(
                "{item} {name}：计价方法已设为标准成本，但没录标准成本单价 —— 差异恒为 0，\
                 这是静默失真，不是「没有差异」"
            ));
            continue;
        }
        if std.is_zero() {
            // 没配标准成本的存货不进差异表：移动加权是另一套口径，
            // 混进来没有可比性。
            continue;
        }

        let price_variance = if in_qty.is_zero() {
            Money::ZERO
        } else {
            ((actual_unit - std) * in_qty).round2()
        };
        let out_standard_amount = (std * out_qty).round2();
        let spend_variance = (actual_out - out_standard_amount).round2();

        rows.push(ItemVariance {
            item: item.clone(),
            item_name: name,
            method: method.code().to_string(),
            standard_cost: std,
            ref_cost,
            in_qty,
            in_amount,
            landed_cost: landed_amount,
            other_adjust,
            actual_unit,
            price_variance,
            out_qty,
            out_amount: actual_out,
            out_standard_amount,
            spend_variance,
            move_ids,
        });
    }

    let mut total_price = Money::ZERO;
    let mut total_spend = Money::ZERO;
    for r in &rows {
        total_price += r.price_variance;
        total_spend += r.spend_variance;
    }
    rows.sort_by(|a, b| {
        b.price_variance
            .abs()
            .partial_cmp(&a.price_variance.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(VarianceReport {
        period: period.to_string(),
        rows,
        total_price: total_price.round2(),
        total_spend: total_spend.round2(),
        missing: missing_items(),
        warnings,
    })
}

/// 本模块**刻意不算**的差异项，连同原因一起返回给界面
fn missing_items() -> Vec<String> {
    vec![
        "用量差异：需要「标准 BOM + 标准产量」，目前无维护入口".into(),
        "效率差异：需要标准工时与人工工资率，仓里无数据源".into(),
        "固定制造费用差异：需要产能与固定费用预算".into(),
    ]
}

/// 存货名称（取不到就返回空串，不为了名字去阻断差异分析）
fn item_name(db: &Db, item: &str) -> String {
    db.conn()
        .query_row(
            "SELECT name FROM aux_entity WHERE kind='item' AND code=?1",
            [item],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default()
}

/// 存货档案上的「参考成本」（`props_json.ref_cost`）——BOM 滚算与订单级差异用它
pub fn item_ref_cost(db: &Db, item: &str) -> Money {
    let props: Option<String> = db
        .conn()
        .query_row(
            "SELECT props_json FROM aux_entity WHERE kind='item' AND code=?1",
            [item],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    props
        .and_then(|p| serde_json::from_str::<std::collections::BTreeMap<String, String>>(&p).ok())
        .and_then(|m| m.get("ref_cost").map(|s| Money::parse_or_zero(s)))
        .unwrap_or(Money::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::{stock_insert, StockKind, StockMove};
    use crate::tests::mem;
    use fincore::Period;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn d(y: i32, mo: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, mo, day).unwrap()
    }

    /// 造一条出入库流水（id 递增只为让 move_ids 稳定有序）
    fn mv(db: &Db, kind: StockKind, item: &str, qty: &str, price: &str) -> i64 {
        static SEQ: AtomicI64 = AtomicI64::new(1);
        let q = m(qty);
        let p = m(price);
        stock_insert(
            db,
            &StockMove {
                id: 0,
                period: Period::new(2026, 1).unwrap(),
                biz_date: d(
                    2026,
                    1,
                    5 + (SEQ.fetch_add(1, Ordering::SeqCst) as u32) % 20,
                ),
                kind,
                item: item.into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap()
    }

    fn set_std(db: &Db, item: &str, std: &str) {
        crate::business::item_cost_method_set(db, item, Some("standard"), m(std)).unwrap();
    }

    /// 造一条成本调整流水（qty=0、只有金额）——到岸成本就走这条路径
    fn mv_adjust(db: &Db, item: &str, amount: &str, memo: &str) {
        static SEQ: AtomicI64 = AtomicI64::new(1);
        stock_insert(
            db,
            &StockMove {
                id: 0,
                period: Period::new(2026, 1).unwrap(),
                biz_date: d(
                    2026,
                    1,
                    6 + (SEQ.fetch_add(1, Ordering::SeqCst) as u32) % 10,
                ),
                kind: StockKind::Adjust,
                item: item.into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: Money::ZERO,
                price: Money::ZERO,
                amount: m(amount),
                voucher_id: None,
                memo: memo.into(),
            },
        )
        .unwrap();
    }

    /// 标准成本必须真的生效：设 7 元标准价，引擎出 5 件必须是 35，而不是移动加权价
    ///
    /// 回归背景：`StockState.standard_cost` 以前**从未被赋值**——
    /// `with_standard_cost` / `set_standard_cost` 在整个 findb 里一次都没被调用，
    /// 于是 `CostMethod::Standard` 永远走 `is_zero()` 那一支、静默退回移动加权。
    /// 设了标准成本、录了单价，出库金额却按加权价算，且不报任何错。
    #[test]
    fn standard_cost_actually_prices_issues() {
        let db = mem();
        mv(&db, StockKind::Purchase, "M001", "100", "9");
        set_std(&db, "M001", "7");
        let p = Period::new(2026, 1).unwrap();

        // 先让 stock_summary 按 standard 回写出库成本
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();
        let cost = crate::business::stock_state(&db, "M001", p, CostMethod::Standard)
            .unwrap()
            .apply(
                &fincore::engine::costing::Move {
                    qty: m("-5"),
                    price: None,
                },
                CostMethod::Standard,
            )
            .unwrap()
            .expect("出库应返回成本，入库才返 None");
        assert_eq!(
            cost,
            m("35"),
            "标准成本 7 × 5 应为 35；等于移动加权价说明 standard_cost 没喂进引擎"
        );
    }

    /// 采购价格差异：买贵了 → 正差异
    #[test]
    fn purchase_price_variance_detects_overpay() {
        let db = mem();
        mv(&db, StockKind::Purchase, "M001", "100", "9"); // 实际 9，标准 7
        set_std(&db, "M001", "7");
        let p = Period::new(2026, 1).unwrap();
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        let row = r
            .rows
            .iter()
            .find(|x| x.item == "M001")
            .expect("应有 M001 行");
        assert_eq!(row.actual_unit, m("9"), "实际单价应取入库加权价");
        assert_eq!(row.price_variance, m("200"), "买贵 2 元 × 100 件 = 200");
        assert_eq!(r.total_price, m("200"));
    }

    /// 买便宜了 → 负差异（不能只报超支，节约也要看得见）
    #[test]
    fn purchase_price_variance_can_be_negative() {
        let db = mem();
        mv(&db, StockKind::Purchase, "M001", "100", "5"); // 实际 5，标准 7
        set_std(&db, "M001", "7");
        let p = Period::new(2026, 1).unwrap();
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        let row = r.rows.iter().find(|x| x.item == "M001").unwrap();
        assert_eq!(row.price_variance, m("-200"), "买便宜了应是负差异");
        assert!(
            r.total_price.is_negative(),
            "合计也要跟着是负的，不能只统计超支"
        );
    }

    /// 出库成本超支差异：实际耗用贵于标准 → 正差异
    #[test]
    fn spend_variance_reflects_actual_issue_cost() {
        let db = mem();
        // 入库价 9、标准 7；出库按标准应 7，出库流水回写的实际成本也是 7 → 差异 0
        mv(&db, StockKind::Purchase, "M001", "100", "9");
        set_std(&db, "M001", "7");
        mv(&db, StockKind::Sale, "M001", "-30", "0");
        let p = Period::new(2026, 1).unwrap();
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        let row = r.rows.iter().find(|x| x.item == "M001").unwrap();
        assert_eq!(row.out_standard_amount, m("210"), "30 × 7");
        assert_eq!(
            row.spend_variance,
            Money::ZERO,
            "按标准成本出库时差异应为 0"
        );
    }

    /// 设了「标准成本」方法但没录单价 → 不进表 + 明确提示（静默失真不是「没差异」）
    #[test]
    fn missing_standard_cost_is_flagged_not_silently_zero() {
        let db = mem();
        mv(&db, StockKind::Purchase, "M001", "100", "9");
        crate::business::item_cost_method_set(&db, "M001", Some("standard"), Money::ZERO).unwrap();
        let p = Period::new(2026, 1).unwrap();
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        assert!(
            r.rows.is_empty(),
            "没有标准单价的存货不该进差异表（进了就是一堆 0）"
        );
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("M001") && w.contains("标准成本单价")),
            "必须点名是哪个存货：{:?}",
            r.warnings
        );
    }

    /// 差异要能追到单据
    #[test]
    fn variance_rows_carry_source_moves() {
        let db = mem();
        mv(&db, StockKind::Purchase, "M001", "100", "9");
        mv(&db, StockKind::Purchase, "M001", "50", "9");
        set_std(&db, "M001", "7");
        mv(&db, StockKind::Sale, "M001", "-20", "0");
        let p = Period::new(2026, 1).unwrap();
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        let row = r.rows.iter().find(|x| x.item == "M001").unwrap();
        assert_eq!(
            row.move_ids.len(),
            3,
            "3 张有金额的流水（2 入 1 出）都应能追到：{:?}",
            row.move_ids
        );
        assert_eq!(row.in_qty, m("150"));
        assert_eq!(row.out_qty, m("20"));
    }

    /// 没配标准成本的存货不进差异表
    #[test]
    fn items_without_standard_cost_are_excluded() {
        let db = mem();
        mv(&db, StockKind::Purchase, "M001", "100", "9");
        mv(&db, StockKind::Purchase, "M002", "100", "9");
        set_std(&db, "M001", "7");
        let p = Period::new(2026, 1).unwrap();
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].item, "M001");
    }

    /// 「算了但没算的」必须说清楚
    #[test]
    fn report_declares_what_it_cannot_compute() {
        let db = mem();
        mv(&db, StockKind::Purchase, "M001", "10", "9");
        set_std(&db, "M001", "7");
        let p = Period::new(2026, 1).unwrap();
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        for want in ["用量差异", "效率差异", "固定制造费用差异"] {
            assert!(
                r.missing.iter().any(|m| m.contains(want)),
                "必须点名没算的差异：{want}；实际 {:?}",
                r.missing
            );
        }
    }

    /// 逐存货配置必须在**销售成本结转**这条路径上也生效
    ///
    /// 回归背景（两个叠在一起的坑）：
    /// ① `stock_summary` 用传入的**全局**方法覆盖全部存货，而 `period_end_cost`
    ///    （期末计价）走逐存货配置。同一张表里结存一个口径、发出另一个口径，
    ///    差额无处可去，成本永远对不平，且不报任何错。
    /// ② 即使把方法解析成逐存货，这条路径自己 `StockState::new()`、不经过
    ///    `stock_state`，所以 standard_cost 仍是 ZERO → 静默退回移动加权。
    #[test]
    fn per_item_cost_method_wins_over_global_in_summary() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 两个存货：M001 配标准成本 7，M002 不配（跟随全局）
        mv(&db, StockKind::Purchase, "M001", "100", "9");
        mv(&db, StockKind::Purchase, "M002", "100", "9");
        set_std(&db, "M001", "7");
        mv(&db, StockKind::Sale, "M001", "-10", "0");
        mv(&db, StockKind::Sale, "M002", "-10", "0");

        // 全局传移动加权：M001 仍应按标准成本 7（逐存货配置优先）
        let sum = crate::business::stock_summary(&db, p, CostMethod::MovingAverage).unwrap();
        let s1 = sum
            .iter()
            .find(|s| s.item == "M001")
            .expect("M001 应有汇总");
        assert_eq!(
            s1.out_amount,
            m("70"),
            "M001 配了标准成本 7，出库 10 件必须是 70（当前 {}）",
            s1.out_amount
        );
        // 没配的跟随全局：M002 按移动加权 9
        let s2 = sum
            .iter()
            .find(|s| s.item == "M002")
            .expect("M002 应有汇总");
        assert_eq!(
            s2.out_amount,
            m("90"),
            "没配逐存货方式的应跟随全局方法：{}",
            s2.out_amount
        );
    }

    /// 到岸成本必须计入采购价格差异的成本口径
    ///
    /// 回归背景：这一条是到岸成本模块存在的**全部理由**。到岸成本以
    /// `kind=adjust`（qty=0、只有金额）落库，而差异表原来只把 `qty > 0` 的
    /// 入库行算进 `in_amount` —— 于是运费/关税被完全排除在成本之外。
    ///
    /// 后果极其隐蔽：货价便宜 5%、运费贵 20% 时，差异表显示**节约**，
    /// 实际成本反而更高；而且数字完整、有正有负、能排序、合计能加，
    /// 只有跟总账对运费时才会发现。所以这里用「货价便宜 + 运费贵」的组合
    /// 来钉：结论必须从「节约」翻转成「超支」。
    #[test]
    fn landed_cost_flips_purchase_variance_from_saving_to_overspend() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        // 货价便宜：实际入库价 6 < 标准 7 → 不含运费时是「节约」1 × 100 = 100
        mv(&db, StockKind::Purchase, "M001", "100", "6");
        set_std(&db, "M001", "7");
        // 到岸成本 500（运费/关税），以 adjust 行落库
        mv_adjust(&db, "M001", "500", "到岸成本 LC2026010001 分摊");
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        let row = r.rows.iter().find(|x| x.item == "M001").unwrap();

        assert_eq!(row.in_amount, m("1100"), "600 货款 + 500 运费 = 1100 成本");
        assert_eq!(row.landed_cost, m("500"), "到岸成本要单列出来供解释差异");
        assert_eq!(row.actual_unit, m("11"), "实际单价 1100 / 100 = 11");
        assert_eq!(
            row.price_variance,
            m("400"),
            "含运费后是超支 4 元 × 100 = 400 —— 不含运费时会误报成「节约 100」"
        );
        assert_eq!(r.total_price, m("400"));
    }

    /// 反向守卫：不含到岸成本时确实会算成「节约」——证明上面那条断言有鉴别力
    #[test]
    fn without_landed_cost_the_same_case_looks_like_a_saving() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        mv(&db, StockKind::Purchase, "M001", "100", "6");
        set_std(&db, "M001", "7");
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        let row = r.rows.iter().find(|x| x.item == "M001").unwrap();
        assert!(
            row.price_variance.is_negative(),
            "不含运费时是「节约」——这正是上一条用例要修的错误结论：{}",
            row.price_variance
        );
        assert_eq!(row.price_variance, m("-100"), "货价便宜 1 元 × 100");
    }

    /// 非到岸成本的调整（盘点差异）也要计入，但要和到岸成本分开报
    #[test]
    fn non_landed_adjustments_are_included_but_separated() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        mv(&db, StockKind::Purchase, "M001", "100", "7"); // 正好等于标准
        set_std(&db, "M001", "7");
        mv_adjust(&db, "M001", "300", "到岸成本 LC2026010001 分摊");
        mv_adjust(&db, "M001", "-50", "盘点差异调整");
        crate::business::stock_summary(&db, p, CostMethod::Standard).unwrap();

        let r = variance_report(&db, p).unwrap();
        let row = r.rows.iter().find(|x| x.item == "M001").unwrap();
        assert_eq!(row.landed_cost, m("300"), "到岸成本单列");
        assert_eq!(row.other_adjust, m("-50"), "其他调整单列，方向保留");
        assert_eq!(row.in_amount, m("950"), "700 + 300 − 50 = 950");
        assert_eq!(row.price_variance, m("250"), "(9.5 − 7) × 100");
    }

    /// 仓库里有**两个互不相干的"标准成本"**，不一致必须报出来
    ///
    /// · `aux_entity.props_json.ref_cost`（参考成本）→ BOM 滚算、订单级差异
    /// · `item_cost_method.standard_cost`（标准成本）→ 出库计价
    ///
    /// 两者可以不一致且不报任何错：同一张成本表上，BOM 说的成本和实际出库成本
    /// 对不上，而没人会发现——因为没有一处代码会去比对它们。
    #[test]
    fn divergent_ref_cost_and_standard_cost_is_flagged() {
        let db = mem();
        // 建存货档案并写一个 ref_cost = 9
        let mut e =
            fincore::auxiliary::AuxEntity::new(fincore::account::AuxKind::Item, "M001", "材料甲");
        e.props.insert("ref_cost".into(), "9".into());
        crate::auxs::insert(&db, &e).unwrap();
        mv(&db, StockKind::Purchase, "M001", "100", "9");
        set_std(&db, "M001", "7"); // 标准成本 7 ≠ 参考成本 9

        let r = variance_report(&db, Period::new(2026, 1).unwrap()).unwrap();
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("参考成本") && w.contains("标准成本")),
            "两个标准成本不一致必须点名：{:?}",
            r.warnings
        );
        let row = r.rows.iter().find(|x| x.item == "M001").unwrap();
        assert_eq!(row.ref_cost, m("9"), "参考成本要一并返回，供界面并排显示");
        assert_eq!(row.standard_cost, m("7"));

        // 一致时不该有这条警告
        set_std(&db, "M001", "9");
        let r2 = variance_report(&db, Period::new(2026, 1).unwrap()).unwrap();
        assert!(
            !r2.warnings.iter().any(|w| w.contains("≠")),
            "一致时不该报这条：{:?}",
            r2.warnings
        );
    }

    /// 空账套不报错，出空表
    #[test]
    fn empty_period_yields_empty_report() {
        let db = mem();
        let r = variance_report(&db, Period::new(2026, 1).unwrap()).unwrap();
        assert!(r.rows.is_empty());
        assert_eq!(r.total_price, Money::ZERO);
    }
}
