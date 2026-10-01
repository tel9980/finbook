//! 存货成本核算
//!
//! 三种计价方式，核心难点都在**出库成本**怎么定：
//!
//! | 方法 | 出库单价来源 |
//! |------|-------------|
//! | 移动加权平均 | 每次入库后重算：`(原金额 + 本次金额) / (原数量 + 本次数量)` |
//! | 先进先出 FIFO | 按最早批次依次消耗，批内单价不同 |
//! | 个别计价 | 指定批次（凭证分录直接带单价，不走本模块） |
//!
//! 两个必须处理好的边界：
//! 1. **负库存**：先出库后入库时会出现。本模块允许负数量但**单价取上一次已知成本**，
//!    等入库时再冲回差额——实务上叫"暂估"，这里简化处理但保证金额守恒。
//! 2. **尾差**：最后一次出库把结存金额清零，误差挤到当期成本，不留在结存里。

use crate::money::{Money, QTY_DP};
use crate::FinError;

/// 计价方式
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CostMethod {
    /// 移动加权平均
    #[default]
    MovingAverage,
    /// 先进先出
    Fifo,
    /// 个别计价（出库必须指定批次单价）
    Specific,
    /// 标准成本（出库按预设标准成本，差异另行处理）
    Standard,
    /// 全月一次加权平均（期末按"期初+本期入库"加权，一次统一计算出库成本）
    MonthAverage,
}

impl CostMethod {
    pub fn label(&self) -> &'static str {
        match self {
            CostMethod::MovingAverage => "移动加权平均",
            CostMethod::Fifo => "先进先出",
            CostMethod::Specific => "个别计价",
            CostMethod::Standard => "标准成本",
            CostMethod::MonthAverage => "全月一次加权平均",
        }
    }
    pub fn code(&self) -> &'static str {
        match self {
            CostMethod::MovingAverage => "moving_average",
            CostMethod::Fifo => "fifo",
            CostMethod::Specific => "specific",
            CostMethod::Standard => "standard",
            CostMethod::MonthAverage => "month_average",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "fifo" => CostMethod::Fifo,
            "specific" | "individual" => CostMethod::Specific,
            "standard" => CostMethod::Standard,
            "month_average" | "monthly_average" => CostMethod::MonthAverage,
            _ => CostMethod::MovingAverage,
        }
    }
    pub const ALL: &'static [CostMethod] = &[
        CostMethod::MovingAverage,
        CostMethod::Fifo,
        CostMethod::Specific,
        CostMethod::Standard,
        CostMethod::MonthAverage,
    ];
}

/// 出入库流水（正数入库、负数出库）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    /// 数量，正=入库 负=出库
    pub qty: Money,
    /// 入库时的单价；出库时可为 None（由系统按计价方式算）
    pub price: Option<Money>,
}

/// 一批存货（FIFO 用）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lot {
    pub qty: Money,
    pub unit_cost: Money,
}

/// 单个存货的结存
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StockState {
    pub qty: Money,
    /// 结存金额（不含税成本）
    pub amount: Money,
    /// FIFO 的批次队列（最早在前）
    pub lots: Vec<Lot>,
    /// 上一次已知单价（负库存出库时用）
    last_price: Money,
    /// 标准成本单价（Standard 方法出库用）
    standard_cost: Money,
}

impl StockState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置标准成本单价（Standard 方法）
    pub fn with_standard_cost(mut self, c: Money) -> Self {
        self.standard_cost = c;
        self
    }

    /// 设置标准成本单价（可变引用）
    pub fn set_standard_cost(&mut self, c: Money) {
        self.standard_cost = c;
    }

    /// 成本调整：在数量不变的前提下把结存金额调整为指定值，
    /// 差额即成本调整额（正=调增，负=调减）。返回调整金额。
    ///
    /// 调整额按每批所占金额的比例分摊到各 FIFO 批次，保持各批次的
    /// **相对单价不变**（口径不漂移）；舍入尾差由最后一非零批次吸收，
    /// 保证批次金额之和恒等于结存金额。
    pub fn adjust_amount(&mut self, new_amount: Money) -> Result<Money, FinError> {
        let delta = new_amount - self.amount;
        self.amount = new_amount;
        if !self.qty.is_zero() && !self.lots.is_empty() {
            let old_total: Money = self
                .lots
                .iter()
                .map(|l| (l.qty * l.unit_cost).round2())
                .sum();
            if !old_total.is_zero() {
                // 最后一个非零数量批次作为尾差吸收者
                let mut residual_idx = None;
                for (i, l) in self.lots.iter().enumerate() {
                    if !l.qty.is_zero() {
                        residual_idx = Some(i);
                    }
                }
                let mut remaining = delta;
                for (i, lot) in self.lots.iter_mut().enumerate() {
                    if lot.qty.is_zero() {
                        continue;
                    }
                    let share = if Some(i) == residual_idx {
                        remaining
                    } else {
                        let ratio = (lot.qty * lot.unit_cost)
                            .round2()
                            .checked_div(old_total)
                            .expect("old_total 已判非零");
                        let s = (delta * ratio).round2();
                        remaining -= s;
                        s
                    };
                    lot.unit_cost += share
                        .checked_div(lot.qty)
                        .expect("批次数量已在循环头判非零");
                }
            }
            self.last_price = new_amount
                .checked_div(self.qty)
                .expect("self.qty 已判非零")
                .round_dp(QTY_DP + 2);
        } else if !self.qty.is_zero() {
            self.last_price = new_amount
                .checked_div(self.qty)
                .expect("self.qty 已判非零")
                .round_dp(QTY_DP + 2);
        }
        Ok(delta.round2())
    }

    /// 当前结存单价（零数量时为 0，避免除零）
    pub fn unit_cost(&self) -> Money {
        if self.qty.is_zero() {
            return Money::ZERO;
        }
        self.amount
            .checked_div(self.qty)
            .expect("数量已判非零")
            .round_dp(QTY_DP + 2)
    }

    /// 应用一条流水，返回本次出库成本（入库返回 None）
    pub fn apply(&mut self, mv: &Move, method: CostMethod) -> Result<Option<Money>, FinError> {
        if mv.qty.is_zero() {
            return Ok(None);
        }
        if mv.qty.is_positive() {
            self.apply_in(mv, method)?;
            Ok(None)
        } else {
            let cost = self.apply_out(mv, method)?;
            Ok(Some(cost))
        }
    }

    fn apply_in(&mut self, mv: &Move, _method: CostMethod) -> Result<(), FinError> {
        let qty = mv.qty;
        // 入库必须有单价
        let price = match mv.price {
            Some(p) if p >= Money::ZERO => p,
            _ => {
                // 没给单价就按当前结存价入账，避免金额凭空消失
                if self.qty.is_zero() {
                    return Err(FinError::msg("首次入库必须指定单价"));
                }
                self.unit_cost()
            }
        };
        let amount = (qty * price).round2();

        // 负库存补回：先把负数量填平，这部分按 last_price 冲回
        if self.qty.is_negative() {
            let fill = (-self.qty).min(qty);
            let back = (fill * self.last_price).round2();
            self.qty += fill;
            self.amount += back;
            let rest = qty - fill;
            if rest > Money::ZERO {
                self.qty += rest;
                self.amount += (rest * price).round2();
                self.last_price = price;
                self.push_lot(rest, price);
            }
            return Ok(());
        }

        self.qty += qty;
        self.amount += amount;
        self.last_price = price;
        self.push_lot(qty, price);
        Ok(())
    }

    fn push_lot(&mut self, qty: Money, cost: Money) {
        // 同价批次合并，避免批次列表无限膨胀
        if let Some(last) = self.lots.last_mut() {
            if last.unit_cost == cost {
                last.qty += qty;
                return;
            }
        }
        self.lots.push(Lot {
            qty,
            unit_cost: cost,
        });
    }

    fn apply_out(&mut self, mv: &Move, method: CostMethod) -> Result<Money, FinError> {
        let want = (-mv.qty).round_dp(QTY_DP); // 需要出库的正数量

        if let Some(p) = mv.price {
            // 指定单价（个别计价 / 手工调整），直接按它出库
            let cost = (want * p).round2();
            self.consume_lots(want);
            self.qty -= want;
            self.amount -= cost;
            self.normalize_if_empty();
            return Ok(cost);
        }

        // 个别计价：出库必须指定单价
        if method == CostMethod::Specific {
            return Err(FinError::msg("个别计价的出库必须指定批次单价"));
        }

        let cost = match method {
            CostMethod::MovingAverage | CostMethod::MonthAverage => {
                // 全月一次平均的"逐笔重放"与移动加权同口径；
                // 真正的月末统一计价由 run_month_average 在期末一次性完成。
                let unit = if self.qty.is_zero() {
                    self.last_price
                } else {
                    self.unit_cost()
                };
                (want * unit).round2()
            }
            CostMethod::Fifo => self.cost_by_fifo(want),
            CostMethod::Standard => {
                // 标准成本：无预设时回退到结存单价
                let unit = if self.standard_cost.is_zero() {
                    self.unit_cost()
                } else {
                    self.standard_cost
                };
                (want * unit).round2()
            }
            CostMethod::Specific => unreachable!("上面已拦截"),
        };

        self.consume_lots(want);
        self.qty = (self.qty - want).round_dp(QTY_DP);
        self.amount -= cost;
        self.normalize_if_empty();
        Ok(cost)
    }

    fn cost_by_fifo(&self, want: Money) -> Money {
        let mut remain = want;
        let mut cost = Money::ZERO;
        for lot in &self.lots {
            if remain <= Money::ZERO {
                break;
            }
            let take = remain.min(lot.qty);
            cost += (take * lot.unit_cost).round2();
            remain -= take;
        }
        // 批次不够（负库存）：剩余部分按 last_price 计价
        if remain > Money::ZERO {
            cost += (remain * self.last_price).round2();
        }
        cost.round2()
    }

    fn consume_lots(&mut self, want: Money) {
        let mut remain = want;
        let mut idx = 0;
        while idx < self.lots.len() && remain > Money::ZERO {
            let take = remain.min(self.lots[idx].qty);
            self.lots[idx].qty -= take;
            remain -= take;
            if self.lots[idx].qty.abs() < Money::new(rust_decimal::Decimal::new(1, QTY_DP)) {
                self.lots.remove(idx);
            } else {
                idx += 1;
            }
        }
    }

    /// 数量清零时把金额也抹平，尾差挤进当期成本（已在上一步从 amount 扣除）
    fn normalize_if_empty(&mut self) {
        if self.qty.abs() < Money::new(rust_decimal::Decimal::new(1, QTY_DP)) {
            self.qty = Money::ZERO;
            self.amount = Money::ZERO;
            self.lots.clear();
        }
    }
}

/// 一次性跑完一批流水，返回每条流水对应的出库成本（入库为 None）
///
/// `moves` 必须按业务日期升序。
pub fn run(
    moves: &[Move],
    method: CostMethod,
) -> Result<(Vec<Option<Money>>, StockState), FinError> {
    let mut st = StockState::new();
    let mut out = Vec::with_capacity(moves.len());
    for mv in moves {
        out.push(st.apply(mv, method)?);
    }
    Ok((out, st))
}

/// 全月一次加权平均：期末统一计算出库成本。
///
/// 单价 = (期初金额 + 本期全部入库金额) / (期初数量 + 本期全部入库数量)，
/// 本期所有出库都用同一个单价。`opening` 为期初结存（跨期重放得到），
/// `moves` 是本期出入库流水（按日期升序）。
///
/// 返回每条本期流水的出库成本（入库为 None）与期末结存。
pub fn run_month_average(
    opening: &StockState,
    moves: &[Move],
) -> Result<(Vec<Option<Money>>, StockState), FinError> {
    let mut st = opening.clone();
    // 汇总本期入库
    let mut in_qty = Money::ZERO;
    let mut in_amount = Money::ZERO;
    for mv in moves {
        if mv.qty.is_positive() {
            let price = mv.price.unwrap_or(st.unit_cost());
            in_qty += mv.qty;
            in_amount += (mv.qty * price).round2();
        }
    }
    // 全月一次单价
    let denom = opening.qty + in_qty;
    let unit = if denom.is_zero() {
        st.unit_cost()
    } else {
        (opening.amount + in_amount)
            .checked_div(denom)
            .expect("denom 已判非零")
            .round_dp(QTY_DP + 2)
    };
    let mut out = Vec::with_capacity(moves.len());
    for mv in moves {
        if mv.qty.is_positive() {
            let price = mv.price.unwrap_or(unit);
            st.apply(&Move { qty: mv.qty, price: Some(price) }, CostMethod::MovingAverage)?;
            out.push(None);
        } else {
            let want = (-mv.qty).round_dp(QTY_DP);
            let cost = (want * unit).round2();
            st.consume_lots(want);
            st.qty = (st.qty - want).round_dp(QTY_DP);
            st.amount -= cost;
            st.normalize_if_empty();
            out.push(Some(cost));
        }
    }
    Ok((out, st))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn q(s: &str) -> Money {
        Money::parse(s).unwrap().round_dp(QTY_DP)
    }

    #[test]
    fn moving_average_reprices_on_each_purchase() {
        let moves = vec![
            Move { qty: q("100"), price: Some(m("10")) },   // 入 100 @10 → 1000
            Move { qty: q("50"), price: Some(m("12")) },    // 入 50 @12 → 累计 150 / 1600，均价 10.6667
            Move { qty: q("-80"), price: None },            // 出 80 @10.6667 = 853.33
        ];
        let (costs, st) = run(&moves, CostMethod::MovingAverage).unwrap();
        assert_eq!(costs[0], None);
        assert_eq!(costs[1], None);
        assert_eq!(costs[2], Some(m("853.33")));
        assert_eq!(st.qty, q("70"));
        assert_eq!(st.amount, m("746.67"));
    }

    #[test]
    fn fifo_consumes_oldest_first() {
        let moves = vec![
            Move { qty: q("100"), price: Some(m("10")) },
            Move { qty: q("50"), price: Some(m("12")) },
            Move { qty: q("-80"), price: None }, // 先吃 100@10 的 80 → 800
        ];
        let (costs, st) = run(&moves, CostMethod::Fifo).unwrap();
        assert_eq!(costs[2], Some(m("800")));
        assert_eq!(st.qty, q("70")); // 20@10 + 50@12
        assert_eq!(st.amount, m("800"));
        assert_eq!(st.lots.len(), 2);
    }

    #[test]
    fn fifo_crosses_lots() {
        let moves = vec![
            Move { qty: q("10"), price: Some(m("5")) },
            Move { qty: q("10"), price: Some(m("7")) },
            Move { qty: q("-15"), price: None }, // 10@5 + 5@7 = 50 + 35 = 85
        ];
        let (costs, _) = run(&moves, CostMethod::Fifo).unwrap();
        assert_eq!(costs[2], Some(m("85")));
    }

    #[test]
    fn sell_out_clears_to_zero() {
        let moves = vec![
            Move { qty: q("10"), price: Some(m("3.33")) },
            Move { qty: q("-10"), price: None },
        ];
        let (_, st) = run(&moves, CostMethod::MovingAverage).unwrap();
        assert_eq!(st.qty, Money::ZERO);
        assert_eq!(st.amount, Money::ZERO); // 尾差已被抹平，不残留
        assert!(st.lots.is_empty());
    }

    #[test]
    fn negative_stock_recovers_on_purchase() {
        // 先卖后买：出库 10（此时无库存，按 0 计价），再入库 10 @8
        let moves = vec![
            Move { qty: q("-10"), price: None },
            Move { qty: q("10"), price: Some(m("8")) },
        ];
        let (costs, st) = run(&moves, CostMethod::MovingAverage).unwrap();
        assert_eq!(costs[0], Some(Money::ZERO));
        assert_eq!(st.qty, Money::ZERO);
        assert_eq!(st.amount, Money::ZERO);
    }

    #[test]
    fn first_in_must_have_price() {
        let moves = vec![Move { qty: q("10"), price: None }];
        assert!(run(&moves, CostMethod::MovingAverage).is_err());
    }

    #[test]
    fn method_parse_roundtrip() {
        for c in CostMethod::ALL {
            assert_eq!(CostMethod::parse(c.code()), *c);
        }
    }

    #[test]
    fn specific_requires_price() {
        let moves = vec![
            Move { qty: q("10"), price: Some(m("8")) },
            Move { qty: q("-4"), price: None }, // 个别计价缺单价 → 报错
        ];
        assert!(run(&moves, CostMethod::Specific).is_err());
        let moves = vec![
            Move { qty: q("10"), price: Some(m("8")) },
            Move { qty: q("-4"), price: Some(m("9")) }, // 指定批次单价
        ];
        let (costs, _) = run(&moves, CostMethod::Specific).unwrap();
        assert_eq!(costs[1], Some(m("36"))); // 4 × 9
    }

    #[test]
    fn standard_cost_fixed_unit() {
        let mut st = StockState::new().with_standard_cost(m("7"));
        st.apply(&Move { qty: q("10"), price: Some(m("9")) }, CostMethod::Standard).unwrap();
        let c = st.apply(&Move { qty: q("-5"), price: None }, CostMethod::Standard).unwrap();
        assert_eq!(c, Some(m("35"))); // 5 × 标准成本 7，与入库单价无关
        assert_eq!(st.qty, q("5"));
    }

    #[test]
    fn adjust_amount_delta() {
        let mut st = StockState::new();
        st.apply(&Move { qty: q("10"), price: Some(m("8")) }, CostMethod::MovingAverage).unwrap();
        let delta = st.adjust_amount(m("100")).unwrap(); // 结存 80 → 100
        assert_eq!(delta, m("20"));
        assert_eq!(st.qty, q("10"));
        assert_eq!(st.unit_cost(), m("10"));
    }

    #[test]
    fn month_average_uniform_unit_cost() {
        // 期初 100@10=1000；本月入 100@12=1200；
        // 全月一次单价 = (1000+1200)/(100+100) = 11
        let opening = StockState {
            qty: q("100"),
            amount: m("1000"),
            lots: vec![Lot { qty: q("100"), unit_cost: m("10") }],
            last_price: m("10"),
            standard_cost: Money::ZERO,
        };
        let moves = vec![
            Move { qty: q("100"), price: Some(m("12")) }, // 入
            Move { qty: q("-80"), price: None },          // 出 80@11 = 880
        ];
        let (costs, st) = run_month_average(&opening, &moves).unwrap();
        assert_eq!(costs[0], None);
        assert_eq!(costs[1], Some(m("880")));
        assert_eq!(st.qty, q("120"));
        // 结存金额 = 1000 + 1200 - 880 = 1320
        assert_eq!(st.amount, m("1320"));
        assert_eq!(st.unit_cost(), m("11"));
    }

    #[test]
    fn method_roundtrip_includes_month_average() {
        assert_eq!(CostMethod::parse("month_average"), CostMethod::MonthAverage);
        assert_eq!(CostMethod::MonthAverage.label(), "全月一次加权平均");
    }
}