//! 加工费计价规则（按面积/件计价 + 最低收费 + 加急费）
//!
//! 氧化、电镀这类来料加工的报价，几乎不是「数量 × 单价」这么干净：
//!
//! - 单价常按 **面积**（㎡/dm²）而不是件；
//! - 小批量要收 **最低收费**（做了 0.5 ㎡ 也得收起步价）；
//! - 赶货要加 **加急费**（固定加价 + 按比例加价可以叠加）。
//!
//! 所以这里把「一条规则」拆成四个数：[`PriceRule::unit_price`] 单价、
//! [`PriceRule::min_charge`] 最低收费、[`PriceRule::rush_fee`] 加急固定费、
//! [`PriceRule::rush_rate`] 加急加价率（%）。计算全在 [`quote`] 里做，
//! 是**纯函数**——同样的输入永远同样的输出，可以被单元测试钉死。
//!
//! 金额一律 `Money`（Decimal）在 Rust 侧算，`round2` 只在最后一步做：
//! 中途四舍五入会让「两张单加起来 ≠ 一张合并单」。
//!
//! 规则匹配看 [`match_rule`]：先精确（物料 + 客户），再退到「只限物料」、
//! 「只限客户」，最后才是通配规则；停用的规则一律不参与匹配。

use fincore::{FinError, Money};
use std::collections::HashMap;

use crate::{Db, DbResult};

/// 加工费计价规则
#[derive(Clone, Debug, serde::Serialize)]
pub struct PriceRule {
    pub id: i64,
    pub name: String,
    /// 空 = 适用任意物料
    pub item_code: String,
    /// 空 = 适用任意客户
    pub customer_code: String,
    /// 计价单位（㎡ / 件 / kg，自由文本）
    pub unit: String,
    pub unit_price: Money,
    /// 最低收费（0 = 不设）
    pub min_charge: Money,
    /// 加急固定加价
    pub rush_fee: Money,
    /// 加急加价率（%，0-100）
    pub rush_rate: Money,
    /// 生效日期（`YYYY-MM-DD`，空 = 立即生效）。提前录下一版价用
    pub effective_from: String,
    /// 工艺档（普通氧化 / 硬质氧化 / 导电氧化 / 本色 / 黑色……）。空 = 不分档
    ///
    /// 同一物料、同一客户、同一口径，换个表面处理就是另一个价。没有这一列，
    /// 只能靠规则名称区分 —— 矩阵里看着像重复录入，报价时还挑不出来。
    pub process: String,
    /// 规格档：拿哪一维跟区间比（`max` 最长边 / `min` 最短边 / `first` 第一维 /
    /// `area` 面积）。空串 = 不分档 —— 这一条价不看尺寸。
    ///
    /// 氧化厂常见「规格 200cm 以下 0.2 元/件，200cm 以上 1.5 元/米」：短件按件、
    /// 长料按米，连计价口径都跟着变，所以区间必须和口径（`unit`）一起存在规则上。
    pub band_dim: String,
    /// 区间下限（cm，空 = 不限）
    pub band_from: String,
    /// 区间上限（cm，空 = 不限）。含端点：「200 以下」填 200，200cm 也算在这一档
    pub band_to: String,
    /// 数量档下限（计价数量，空 = 不限）
    ///
    /// 「500 件以下 0.3、以上 0.25」这种阶梯价：小批量要调机，大批量摊薄。
    /// 比较对象是**计价数量**（要乘单价的那些件/米/㎡），不是库存单位数量 ——
    /// 否则按㎡ 报价的活儿得先自己换算一遍才知道落在哪一档。
    pub qty_from: String,
    /// 数量档上限（空 = 不限）。含端点
    pub qty_to: String,
    pub disabled: bool,
    pub memo: String,
}

impl Default for PriceRule {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            item_code: String::new(),
            customer_code: String::new(),
            unit: "件".to_string(),
            unit_price: Money::ZERO,
            min_charge: Money::ZERO,
            rush_fee: Money::ZERO,
            rush_rate: Money::ZERO,
            effective_from: String::new(),
            process: String::new(),
            band_dim: String::new(),
            band_from: String::new(),
            band_to: String::new(),
            qty_from: String::new(),
            qty_to: String::new(),
            disabled: false,
            memo: String::new(),
        }
    }
}

/// 一次报价的拆解（前端要看到为什么是这个价）
#[derive(Clone, Debug, serde::Serialize)]
pub struct FeeQuote {
    pub rule_id: i64,
    pub rule_name: String,
    pub unit: String,
    pub qty: Money,
    pub unit_price: Money,
    /// 数量 × 单价
    pub base: Money,
    /// 最低收费门槛（0 = 不设）
    pub min_charge: Money,
    /// 为凑够最低收费补的差额
    pub floor_extra: Money,
    pub rush_fee: Money,
    pub rush_rate: Money,
    /// 加急加价合计（固定 + 按比例）
    pub rush_extra: Money,
    pub total: Money,
    /// 这条价从哪天起生效（空 = 立即）
    pub effective_from: String,
    /// 是否还没到生效日（前端要提示「这个价 X 月 X 日才生效」）
    pub future: bool,
}

const COLS: &str = "id,name,item_code,customer_code,unit,unit_price,min_charge,rush_fee,\
                    rush_rate,disabled,memo,effective_from,process,band_dim,band_from,band_to,\
                    qty_from,qty_to";

fn map_rule(r: &rusqlite::Row) -> rusqlite::Result<PriceRule> {
    Ok(PriceRule {
        id: r.get(0)?,
        name: r.get(1)?,
        item_code: r.get(2)?,
        customer_code: r.get(3)?,
        unit: r.get(4)?,
        unit_price: crate::read_money(r, 5)?,
        min_charge: crate::read_money(r, 6)?,
        rush_fee: crate::read_money(r, 7)?,
        rush_rate: crate::read_money(r, 8)?,
        disabled: r.get::<_, i64>(9)? != 0,
        memo: r.get(10)?,
        effective_from: r.get(11)?,
        process: r.get(12)?,
        band_dim: r.get(13)?,
        band_from: r.get(14)?,
        band_to: r.get(15)?,
        qty_from: r.get(16)?,
        qty_to: r.get(17)?,
    })
}

fn hundred() -> Money {
    Money::parse_or_zero("100")
}

/// 按规则算一次加工费（纯函数）
///
/// 顺序固定：先算 `数量 × 单价`，不足最低收费就补到最低收费，
/// 再叠加急（固定 + 按比例，比例按「补完最低收费之后的金额」算）。
pub fn quote(rule: &PriceRule, qty: Money, rush: bool) -> FeeQuote {
    quote_on(rule, qty, rush, "")
}

/// 同 [`quote`]，但按 `today`（`YYYY-MM-DD`）判断这条价是否已生效。
///
/// 单独拆出来是为了可测：`quote` 不碰时钟，测试里永远同样的输入同样的输出；
/// 只有真正对外报价（[`quote_with_units`] / [`quote_by`]）才取当天日期。
pub fn quote_on(rule: &PriceRule, qty: Money, rush: bool, today: &str) -> FeeQuote {
    let base = (qty * rule.unit_price).round2();
    let floor_extra = if rule.min_charge > base {
        rule.min_charge - base
    } else {
        Money::ZERO
    };
    let subtotal = base + floor_extra;
    let rush_extra = if rush {
        let pct = (subtotal * rule.rush_rate)
            .checked_div(hundred())
            .unwrap_or(Money::ZERO)
            .round2();
        (rule.rush_fee + pct).round2()
    } else {
        Money::ZERO
    };
    FeeQuote {
        rule_id: rule.id,
        rule_name: rule.name.clone(),
        unit: rule.unit.clone(),
        qty,
        unit_price: rule.unit_price,
        base,
        min_charge: rule.min_charge,
        floor_extra,
        rush_fee: rule.rush_fee,
        rush_rate: rule.rush_rate,
        rush_extra,
        total: subtotal + rush_extra,
        effective_from: rule.effective_from.clone(),
        future: !today.is_empty() && is_future(rule, today),
    }
}

/// 这条价还没到生效日？（空日期 = 已生效）
fn is_future(rule: &PriceRule, today: &str) -> bool {
    let d = rule.effective_from.trim();
    !d.is_empty() && d > today
}

/// 今天（`YYYY-MM-DD`）。单价生效日按自然日比较，字符串比较即可。
pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 报价单一行该报多少钱（把规格 / 口径 / 工艺档 / 数量都交给价目表去挑）
///
/// 报价单上最终只有一个「数量 × 单价」。但客户说的可能是「1250*250*3 的件，
/// 300 件」：先按规格折成 0.3125 ㎡/件、再按 300 件挑档、最后乘单价。
/// 这里只算不落库 —— 单价与折算过程由报价单自己抄下来（报价单是当时的承诺，
/// 价目表以后怎么调都不该改它）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct QuotePrice {
    pub rule_id: i64,
    pub rule_name: String,
    /// 报价单上该填的单价（按**库存单位**计，4 位小数）
    pub unit_price: Money,
    pub stock_unit: String,
    /// 计价数量与口径（按 ㎡ 报了 93.75 ㎡ 这种）
    pub calc_qty: Money,
    pub calc_unit: String,
    /// 1 库存单位 = factor 计价单位（没折算时为 1）
    pub factor: Money,
    /// manual（用折算系数）/ spec（按规格算）/ unit（指定口径）/ plain（不分档的价）
    pub basis: &'static str,
    pub total: Money,
    pub min_charge: Money,
    /// 为凑够起步价补的差额
    pub floor_extra: Money,
    /// 还没到生效日的价
    pub future: bool,
    pub effective_from: String,
    /// 给人看的一句话：折算过程 / 起步价补差 / 价还没生效
    pub note: String,
}

/// 按价目表算一行报价单的价。`density` 只在按 kg 报、又走规格折算时用得上。
#[allow(clippy::too_many_arguments)]
pub fn price_for_line(
    db: &Db,
    item: &str,
    customer: &str,
    qty: Money,
    spec: &str,
    calc_unit: &str,
    process: &str,
    density: Money,
) -> DbResult<QuotePrice> {
    if item.trim().is_empty() {
        return Err(FinError::msg("先选物料 —— 没有物料不知道报哪一档价").into());
    }
    if !qty.is_positive() {
        return Err(FinError::msg("数量要大于 0，不然「数量 × 单价」算不出金额").into());
    }
    let stock_unit = {
        let u = stock_unit_of(db, item)?;
        if u.trim().is_empty() {
            "件".to_string()
        } else {
            u
        }
    };
    let spec = spec.trim().to_string();
    let calc_unit = calc_unit.trim().to_string();
    let process = process.trim().to_string();

    let (q, calc_qty, calc_unit_out, factor, basis) = if !spec.is_empty() {
        let sq = quote_with_spec(
            db, item, customer, &spec, qty, &calc_unit, density, &process, false,
        )?;
        (
            sq.quote,
            sq.calc_qty,
            sq.calc_unit.clone(),
            sq.factor,
            sq.basis,
        )
    } else if !calc_unit.is_empty() {
        let uq = quote_with_units(db, item, customer, qty, &calc_unit, &process, false)?;
        (
            uq.quote,
            uq.calc_qty,
            uq.calc_unit.clone(),
            uq.factor,
            "unit",
        )
    } else {
        // 什么都没给：按价目表那格自己的口径报，能折就折
        // 未来价的提示由 quote_on 写进 FeeQuote，这里不用再判一次
        let (rule, _future) = match_rule_full(db, item, customer, "", &process, None, Some(qty))?
            .ok_or_else(|| {
            FinError::msg(format!(
                "价目表里没有「{item}」的价（客户「{customer}」）——先在「加工费规则」里加一条"
            ))
        })?;
        let unit = rule.unit.trim().to_string();
        let (cq, cu, f, _basis) = if unit.is_empty() || unit == stock_unit {
            (qty, stock_unit.clone(), Money::ONE, "plain")
        } else {
            match calc_unit_of(db, item, &unit)? {
                Some(c) => (
                    (qty * c.factor).round_dp(c.scale.clamp(0, SCALE_MAX) as u32),
                    unit.clone(),
                    c.factor,
                    "manual",
                ),
                None => {
                    return Err(FinError::msg(format!(
                        "价目表这格按「{unit}」记价，但没录「1 {stock_unit} = 多少 {unit}」—— 先在「加工费规则 → 折算系数」里补一条"
                    ))
                    .into())
                }
            }
        };
        let q = quote_on(&rule, cq, false, &today());
        (q, cq, cu, f, _basis)
    };

    // 报价单只有「数量 × 单价」，所以把总额摊回每库存单位；4 位小数留给「0.2808」这种
    let unit_price = (q
        .total
        .checked_div(qty)
        .ok_or_else(|| FinError::msg("数量不能为 0"))?)
    .round_dp(4);
    let mut note = String::new();
    if !spec.is_empty() {
        // 有手录系数时，折合出来的数字来自那条系数，不是规格几何 —— 说错了会让人
        // 拿着规格去按计算器，算不出单子上的数。
        note = if basis == "manual" {
            format!(
                "按手录系数（1 {stock_unit} = {} {calc_unit_out}）折成 {} {calc_unit_out}；规格 {spec} 只记在单上",
                factor.fmt_qty(),
                calc_qty.fmt_qty()
            )
        } else {
            format!(
                "按规格 {spec} 折成 {} {}",
                calc_qty.fmt_qty(),
                calc_unit_out
            )
        };
    } else if !calc_unit.is_empty() && factor != Money::ONE {
        note = format!(
            "1 {stock_unit} = {} {calc_unit}，共 {} {calc_unit}",
            factor.fmt_qty(),
            calc_qty.fmt_qty()
        );
    }
    if q.floor_extra.is_positive() {
        if !note.is_empty() {
            note.push('；');
        }
        note.push_str(&format!("不足起步价，补 {}", q.floor_extra.fmt_money()));
    }
    if q.future {
        if !note.is_empty() {
            note.push('；');
        }
        note.push_str(&format!(
            "这条价 {} 才生效，现在报的是提前预定的价",
            if q.effective_from.is_empty() {
                "以后".to_string()
            } else {
                q.effective_from.clone()
            }
        ));
    }
    Ok(QuotePrice {
        rule_id: q.rule_id,
        rule_name: q.rule_name.clone(),
        unit_price,
        stock_unit,
        calc_qty,
        calc_unit: calc_unit_out,
        factor,
        basis,
        total: q.total,
        min_charge: q.min_charge,
        floor_extra: q.floor_extra,
        future: q.future,
        effective_from: q.effective_from.clone(),
        note,
    })
}

/// 「这单报的价和价目表对得上吗」的结论
///
/// 报价单上只有一个数（单价），价目表上却可能按 ㎡ / 米 / kg 记。要真比，
/// 得先把价目表那格换成「每库存单位」的价 —— 否则 12 元/㎡ 和 0.15 元/件
/// 摆在一起，只能得出「差 75 倍」这种没用的结论。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PriceCheck {
    /// ok / mismatch / no_rule / unit_unknown
    pub kind: &'static str,
    pub rule_name: String,
    /// 价目表那格的口径（原样）
    pub rule_unit: String,
    /// 价目表那格的单价（按 `rule_unit` 计）
    pub rule_price: Money,
    /// 换成「每库存单位」之后的价
    pub per_unit: Money,
    /// 库存单位（件 / 只 …）
    pub stock_unit: String,
    /// 报价单上的单价
    pub line_price: Money,
    /// 报价单 − 价目表（正数 = 报高了）
    pub diff: Money,
    /// 折算系数（没折算时为空）
    pub factor: Option<Money>,
}

/// 拿报价单的单价跟价目表对一下。
///
/// 只做**读**，不改单据：对不上是提示，不是拦。手工价有时是对的（老客户、
/// 一次性让价），系统不该替业务员拍板 —— 但填错了也得让人看见。
pub fn check_quote_price(
    db: &Db,
    item: &str,
    customer: &str,
    line_price: Money,
    qty: Money,
) -> DbResult<PriceCheck> {
    // 没有单位档案（item_unit 没这一行）时按「件」算 —— 那是价目表的默认口径，
    // 也是报价单最常见的单位。这里空着会让每一单都变成「单位对不上」，全是噪声。
    let stock_unit = {
        let u = stock_unit_of(db, item)?;
        if u.trim().is_empty() {
            "件".to_string()
        } else {
            u
        }
    };
    let base = PriceCheck {
        kind: "no_rule",
        rule_name: String::new(),
        rule_unit: String::new(),
        rule_price: Money::ZERO,
        per_unit: Money::ZERO,
        stock_unit: stock_unit.clone(),
        line_price,
        diff: Money::ZERO,
        factor: None,
    };
    let Some((rule, _future)) = match_rule_full(db, item, customer, "", "", None, Some(qty))?
    else {
        return Ok(base);
    };
    let unit = rule.unit.trim().to_string();
    let (per, factor, kind) = if unit.is_empty() || unit == stock_unit {
        let per = rule.unit_price;
        let k = if (line_price - per).abs() < Money::parse_or_zero("0.005") {
            "ok"
        } else {
            "mismatch"
        };
        (per, None, k)
    } else {
        match calc_unit_of(db, item, &unit)? {
            // 1 个库存单位 = factor 个计价单位，所以「每件价」= 单价 × factor
            Some(cu) => {
                let per = (rule.unit_price * cu.factor).round2();
                let k = if (line_price - per).abs() < Money::parse_or_zero("0.005") {
                    "ok"
                } else {
                    "mismatch"
                };
                (per, Some(cu.factor), k)
            }
            // 价目表按公斤计，又没录「1 件 = 多少 kg」—— 对不上就说对不上，
            // 别拿两个不同单位的数硬减出一个「差 12 元」
            None => (rule.unit_price, None, "unit_unknown"),
        }
    };
    Ok(PriceCheck {
        kind,
        rule_name: rule.name.clone(),
        rule_unit: unit,
        rule_price: rule.unit_price,
        per_unit: per,
        stock_unit,
        line_price,
        diff: (line_price - per).round2(),
        factor,
    })
}

/// 今天到 `days` 天内开始生效的价（含今天），按生效日排。
///
/// 提前录下一版价是好事，**录完就忘**是坑：生效那天没人提醒，客户按新价结账、
/// 系统里还是旧价，一个月后对账才发现少收的钱。所以首页给一条黄字。
///
/// 只排除停用的。同一格排着两三版价时**每一版都要提醒** —— 12 月 1 日那版
/// 在 11 月 20 日那版之后照样会生效，两部都得有人盯着，跳过就等于漏报。
pub fn due_soon(db: &Db, days: i64) -> DbResult<Vec<PriceRule>> {
    let today = today();
    let last = (chrono::Local::now().date_naive() + chrono::Duration::days(days.max(0)))
        .format("%Y-%m-%d")
        .to_string();
    let mut out: Vec<PriceRule> = rule_list(db)?
        .into_iter()
        .filter(|r| {
            let d = r.effective_from.trim();
            !d.is_empty() && !r.disabled && d >= today.as_str() && d <= last.as_str()
        })
        .collect();
    out.sort_by(|a, b| {
        a.effective_from
            .cmp(&b.effective_from)
            .then_with(|| a.item_code.cmp(&b.item_code))
            .then_with(|| a.customer_code.cmp(&b.customer_code))
    });
    Ok(out)
}

/// 规则列表
pub fn rule_list(db: &Db) -> DbResult<Vec<PriceRule>> {
    let sql = format!("SELECT {COLS} FROM price_rule ORDER BY id");
    let mut st = db.conn().prepare(&sql)?;
    let out = st.query_map([], map_rule)?.collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// 导出价目表：每个「格」取当前该用的一条（已生效里最新的，没有就取最早未来价），
/// 历史版本、停用的都不进导出。返回按 客户 → 物料 → 口径 → 工艺 → 生效日 排序的当前价。
///
/// 导出的意图是「发给客户核对 / 改价发回」：只给当前价，不带一长串历史。
/// 生效日期**保留原样**（不置空）—— 导入的身份里含生效日，客户改价发回时
/// 日期不动，覆盖的正是当前这一版；若置空，而当前价来自未来生效的调价，
/// 导入会覆盖被阴影的旧「立即」版，而未来版仍赢 → 新价不生效。
pub fn export_rules(db: &Db, customer: &str) -> DbResult<Vec<PriceRule>> {
    let rules = rule_list(db)?;
    let today = today();
    let want_cust = customer.trim();
    let mut by_slot: HashMap<String, Vec<&PriceRule>> = HashMap::new();
    for r in rules.iter() {
        if r.disabled {
            continue;
        }
        if !want_cust.is_empty() && r.customer_code.trim() != want_cust {
            continue;
        }
        by_slot.entry(slot_key(r)).or_default().push(r);
    }
    let mut out: Vec<PriceRule> = by_slot
        .into_values()
        .filter_map(|pool| best_for_cell(pool, &today).cloned())
        .collect();
    out.sort_by(|a, b| {
        a.customer_code
            .cmp(&b.customer_code)
            .then_with(|| a.item_code.cmp(&b.item_code))
            .then_with(|| a.unit.cmp(&b.unit))
            .then_with(|| a.process.cmp(&b.process))
            .then_with(|| a.effective_from.cmp(&b.effective_from))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// 保存规则（id=0 新建）
pub fn rule_save(db: &Db, r: &mut PriceRule) -> DbResult<i64> {
    rule_save_by(db, r, "")
}

/// 保存规则，并把这次改动记进调价历史（`price_change`）。
///
/// `who` 是操作人 —— 调价历史里最常被问的一句是「谁改的」，留空等于白记。
pub fn rule_save_by(db: &Db, r: &mut PriceRule, who: &str) -> DbResult<i64> {
    let name = r.name.trim().to_string();
    if name.is_empty() {
        return Err(FinError::msg("规则名称必填").into());
    }
    let unit = if r.unit.trim().is_empty() {
        "件".to_string()
    } else {
        r.unit.trim().to_string()
    };
    if r.unit_price.is_negative() {
        return Err(FinError::msg("单价不能为负").into());
    }
    if r.min_charge.is_negative() {
        return Err(FinError::msg("最低收费不能为负").into());
    }
    if r.rush_fee.is_negative() {
        return Err(FinError::msg("加急固定费不能为负").into());
    }
    if r.rush_rate.is_negative() || r.rush_rate > hundred() {
        return Err(FinError::msg("加急加价率应在 0-100（%）之间").into());
    }
    // 生效日期：空 = 立即。写了就必须是 YYYY-MM-DD，否则「2026-3-1」这种
    // 会跟 `2026-03-01` 比较出错（字符串比较）。宁可当场拒绝，也别让一条
    // 永远不生效的价躺在表里没人发现。
    let effective_from = r.effective_from.trim().to_string();
    // chrono 的 %m 容忍「3」这种不补零的写法（2026-3-1 也能解析成 3 月 1 日），
    // 所以还要回写一遍比对：字符串比较决定生效与否，格式不统一就会比错。
    let date_ok = chrono::NaiveDate::parse_from_str(&effective_from, "%Y-%m-%d")
        .map(|d| d.format("%Y-%m-%d").to_string() == effective_from)
        .unwrap_or(false);
    if !effective_from.is_empty() && !date_ok {
        return Err(FinError::msg(format!(
            "生效日期要写成 2026-03-01 这样（实际：{effective_from}）"
        ))
        .into());
    }
    let process = r.process.trim().to_string();
    // 规格档：维度和区间一起校验。区间单位是 cm，空串 = 不限。
    // 「200 以下」只填上限、「200 以上」只填下限，所以两头都允许为空。
    let band_dim = r.band_dim.trim().to_string();
    if !band_dim.is_empty() && !BAND_DIMS.contains(&band_dim.as_str()) {
        return Err(FinError::msg(format!(
            "规格按哪一维比只能填 最长边/最短边/第一维/面积（实际：{band_dim}）"
        ))
        .into());
    }
    let band_from = r.band_from.trim().to_string();
    let band_to = r.band_to.trim().to_string();
    if band_dim.is_empty() && (!band_from.is_empty() || !band_to.is_empty()) {
        return Err(
            FinError::msg("填了规格区间，就要说明按哪一维比（最长边/最短边/第一维/面积）").into(),
        );
    }
    for (label, v) in [("下限", &band_from), ("上限", &band_to)] {
        if v.is_empty() {
            continue;
        }
        match v.parse::<f64>() {
            Ok(x) if x >= 0.0 => {}
            _ => {
                return Err(FinError::msg(format!(
                    "规格{label}要填数字（厘米，如 200），实际：{v}"
                ))
                .into())
            }
        }
    }
    let (lo, hi) = (
        band_from.parse::<f64>().unwrap_or(0.0),
        band_to.parse::<f64>().unwrap_or(f64::MAX),
    );
    if !band_from.is_empty() && !band_to.is_empty() && lo > hi {
        return Err(FinError::msg(format!(
            "规格区间下限 {band_from} 比上限 {band_to} 还大 —— 这一条永远用不上"
        ))
        .into());
    }
    // 数量档：比较对象是计价数量（件/米/㎡）。空串 = 不限，含端点。
    // 单独校验而不是复用规格档那套 —— 文案要说「件」而不是「厘米」，否则用户
    // 会以为这里也要填尺寸。
    let qty_from = r.qty_from.trim().to_string();
    let qty_to = r.qty_to.trim().to_string();
    let mut qlo: Option<f64> = None;
    let mut qhi: Option<f64> = None;
    for (label, v, slot) in [("下限", &qty_from, &mut qlo), ("上限", &qty_to, &mut qhi)] {
        if v.is_empty() {
            continue;
        }
        match v.parse::<f64>() {
            Ok(x) if x >= 0.0 => *slot = Some(x),
            _ => {
                return Err(FinError::msg(format!(
                    "数量{label}要填数字（计价数量，如 500），实际：{v}"
                ))
                .into())
            }
        }
    }
    if let (Some(lo), Some(hi)) = (qlo, qhi) {
        if lo > hi {
            return Err(FinError::msg(format!(
                "数量区间下限 {qty_from} 比上限 {qty_to} 还大 —— 这一条永远用不上"
            ))
            .into());
        }
    }
    let tx = db.write_tx()?;
    // 改价前先把旧值读出来 —— 历史里要写的正是「改前 / 改后」
    let before: Option<(String, String, bool)> = if r.id > 0 {
        tx.query_row(
            "SELECT unit_price,min_charge,disabled FROM price_rule WHERE id=?1",
            rusqlite::params![r.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)),
        )
        .ok()
    } else {
        None
    };
    let id = if r.id > 0 {
        let n = tx.execute(
            "UPDATE price_rule SET name=?2,item_code=?3,customer_code=?4,unit=?5,
             unit_price=?6,min_charge=?7,rush_fee=?8,rush_rate=?9,disabled=?10,memo=?11,
             effective_from=?12,process=?13,band_dim=?14,band_from=?15,band_to=?16,
             qty_from=?17,qty_to=?18
             WHERE id=?1",
            rusqlite::params![
                r.id,
                &name,
                r.item_code.trim(),
                r.customer_code.trim(),
                &unit,
                crate::money_param(r.unit_price),
                crate::money_param(r.min_charge),
                crate::money_param(r.rush_fee),
                crate::exact_param(r.rush_rate),
                r.disabled as i64,
                &r.memo,
                &effective_from,
                &process,
                &band_dim,
                &band_from,
                &band_to,
                &qty_from,
                &qty_to
            ],
        )?;
        if n == 0 {
            return Err(FinError::msg(format!("规则 {0} 不存在", r.id)).into());
        }
        r.id
    } else {
        tx.execute(
            "INSERT INTO price_rule(name,item_code,customer_code,unit,unit_price,
             min_charge,rush_fee,rush_rate,disabled,memo,effective_from,process,
             band_dim,band_from,band_to,qty_from,qty_to)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                &name,
                r.item_code.trim(),
                r.customer_code.trim(),
                &unit,
                crate::money_param(r.unit_price),
                crate::money_param(r.min_charge),
                crate::money_param(r.rush_fee),
                crate::exact_param(r.rush_rate),
                r.disabled as i64,
                &r.memo,
                &effective_from,
                &process,
                &band_dim,
                &band_from,
                &band_to,
                &qty_from,
                &qty_to
            ],
        )?;
        tx.last_insert_rowid()
    };
    // 调价历史：新建必记；改价/停用只在真变了的时候记，
    // 否则改个备注也留一条，历史表很快就没法看了
    let old_price = before
        .as_ref()
        .map(|(p, _, _)| Money::parse_or_zero(p).fmt_money())
        .unwrap_or_default();
    let old_min = before
        .as_ref()
        .map(|(_, m, _)| Money::parse_or_zero(m).fmt_money())
        .unwrap_or_default();
    let action = match &before {
        None => Some("新增"),
        Some((_, _, was_disabled)) if *was_disabled != r.disabled => {
            Some(if r.disabled { "停用" } else { "启用" })
        }
        Some((p, m, _)) => {
            let changed =
                Money::parse_or_zero(p) != r.unit_price || Money::parse_or_zero(m) != r.min_charge;
            if changed {
                Some("改价")
            } else {
                None
            }
        }
    };
    if let Some(action) = action {
        let memo = if action == "改价" && old_min != r.min_charge.fmt_money() {
            format!(
                "起步价 {} → {}",
                if old_min.is_empty() { "0.00" } else { &old_min },
                r.min_charge.fmt_money()
            )
        } else {
            String::new()
        };
        tx.execute(
            "INSERT INTO price_change(at,who,action,rule_id,name,item_code,customer_code,
             unit,process,effective_from,old_price,new_price,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                now_stamp(),
                who,
                action,
                id,
                name,
                r.item_code.trim(),
                r.customer_code.trim(),
                unit,
                process,
                effective_from,
                old_price,
                r.unit_price.fmt_money(),
                memo
            ],
        )?;
    }
    tx.commit()?;
    r.id = id;
    r.name = name;
    r.unit = unit;
    r.effective_from = effective_from;
    r.process = process;
    r.band_dim = band_dim;
    r.band_from = band_from;
    r.band_to = band_to;
    r.qty_from = qty_from;
    r.qty_to = qty_to;
    Ok(id)
}

/// 删除规则
pub fn rule_delete(db: &Db, id: i64) -> DbResult<()> {
    rule_delete_by(db, id, "")
}

/// 删除规则，并把这一笔记进调价历史。
///
/// 删掉就查不回来了 —— 所以删除也要留痕：客户拿三个月前的单子来问
/// 「当时为什么按 0.15 算」，只有这条记录能回答。
pub fn rule_delete_by(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let row: Option<PriceRule> = tx
        .query_row(
            &format!("SELECT {COLS} FROM price_rule WHERE id=?1"),
            [id],
            map_rule,
        )
        .ok();
    let n = tx.execute("DELETE FROM price_rule WHERE id=?1", [id])?;
    if n == 0 {
        return Err(FinError::msg(format!("规则 {id} 不存在")).into());
    }
    if let Some(r) = row {
        tx.execute(
            "INSERT INTO price_change(at,who,action,rule_id,name,item_code,customer_code,
             unit,process,effective_from,old_price,new_price,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'','删除前这条规则还在用')",
            rusqlite::params![
                now_stamp(),
                who,
                "删除",
                id,
                &r.name,
                &r.item_code,
                &r.customer_code,
                &r.unit,
                &r.process,
                &r.effective_from,
                r.unit_price.fmt_money()
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

// ── 批量调价（v1.8.97） ────────────────────────────────────────────────

/// 批量调价的条件：调谁、怎么调、从哪天起。
///
/// 「客户年年来压价」是最常见的一类活：二十个客户、几百格价，一格一格改要半天，
/// 还容易漏。这里把「选范围 + 给幅度 + 定生效日」做成一次操作，先预览再执行。
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct AdjustSpec {
    /// 只调这个客户（空 = 全部客户）
    pub customer_code: String,
    /// 只调这个物料（空 = 全部物料）
    pub item_code: String,
    /// 调价方式：`pct` 按百分比 / `delta` 加减固定金额 / `set` 直接设定
    pub mode: String,
    /// 数值：`pct` 传 3 表示 +3%；`delta` 传 -0.5 表示降 5 毛；`set` 传 12.5 表示定成 12.5
    pub value: Money,
    /// 生效日期（`YYYY-MM-DD`，空 = 今天）
    pub effective_from: String,
    /// 备注（写进新一版规则和调价历史，比如「2027 年度涨价」）
    pub memo: String,
}

/// 批量调价里的一条：拿哪条当底、调前多少、调后多少、是新建一版还是覆盖同一天那一版
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AdjustRow {
    /// 以哪一版为底（现行价那一版）
    pub base_id: i64,
    /// 要覆盖的那一版（0 = 新建）
    pub rule_id: i64,
    pub name: String,
    pub item_code: String,
    pub customer_code: String,
    pub unit: String,
    pub process: String,
    pub old_price: Money,
    pub new_price: Money,
    pub effective_from: String,
    /// 新建 / 覆盖
    pub action: String,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AdjustResult {
    pub rows: Vec<AdjustRow>,
    pub warnings: Vec<String>,
    pub created: usize,
    pub updated: usize,
}

/// 调价的生效日：没填就今天
fn adjust_target(spec: &AdjustSpec) -> String {
    let d = spec.effective_from.trim();
    if d.is_empty() {
        today()
    } else {
        d.to_string()
    }
}

/// 同一「格」的标识：物料 + 客户 + 口径 + 工艺档 + 规格档 + 数量档。
/// 同一格里可以有多版价（靠生效日区分），批量调价只动「当前该用的那一版」。
fn slot_key(r: &PriceRule) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        r.item_code.trim(),
        r.customer_code.trim(),
        r.unit.trim(),
        r.process.trim(),
        r.band_dim.trim(),
        r.band_from.trim(),
        r.band_to.trim(),
        r.qty_from.trim(),
        r.qty_to.trim()
    )
}

/// 在 `target` 之前生效的那一版（也就是「现在正用的价」）。
///
/// 不能直接用 `best_active`：批量调价的日期可能是未来的，
/// 那时「现在生效的」与「新价之前那一版」恰好是同一条，但重复执行时要稳定 ——
/// 已经存在 target 当天那一版时必须跳过它，否则连点两下就涨两次。
fn best_before<'a>(pool: &[&'a PriceRule], target: &str) -> Option<&'a PriceRule> {
    pool.iter()
        .copied()
        .filter(|r| {
            let d = r.effective_from.trim();
            d.is_empty() || d < target
        })
        .max_by(|a, b| {
            a.effective_from
                .trim()
                .cmp(b.effective_from.trim())
                .then_with(|| b.id.cmp(&a.id))
        })
}

/// 按调价方式算新价；算出 0 或负数时返回 `None`（这条不调）
fn adjusted_price(old: Money, spec: &AdjustSpec) -> Option<Money> {
    let hundred = Money::parse_or_zero("100");
    let new = match spec.mode.trim() {
        "pct" => {
            let k = (hundred + spec.value).checked_div(hundred)?;
            (old * k).round_dp(4)
        }
        "delta" => (old + spec.value).round_dp(4),
        _ => spec.value.round_dp(4),
    };
    if new.is_positive() {
        Some(new)
    } else {
        None
    }
}

/// 预览：调哪些、调成多少（**不落库**）
pub fn adjust_rows(db: &Db, spec: &AdjustSpec) -> DbResult<(Vec<AdjustRow>, Vec<String>)> {
    let mode = spec.mode.trim();
    if !matches!(mode, "pct" | "delta" | "set") {
        return Err(FinError::msg(
            "调价方式只能是 按百分比（pct）/ 加减金额（delta）/ 直接设定（set）",
        )
        .into());
    }
    if mode != "set" && spec.value.is_zero() {
        return Err(FinError::msg("调 0 等于没调 —— 填个幅度，比如 3 表示 +3%").into());
    }
    let target = adjust_target(spec);
    let date_ok = chrono::NaiveDate::parse_from_str(&target, "%Y-%m-%d")
        .map(|d| d.format("%Y-%m-%d").to_string() == target)
        .unwrap_or(false);
    if !date_ok {
        return Err(
            FinError::msg(format!("生效日期要写成 2026-03-01 这样（实际：{target}）")).into(),
        );
    }

    let want_cust = spec.customer_code.trim();
    let want_item = spec.item_code.trim();
    let rules = rule_list(db)?;
    let mut groups: std::collections::BTreeMap<String, Vec<&PriceRule>> = Default::default();
    for r in rules.iter() {
        if r.disabled {
            continue;
        }
        if !want_cust.is_empty() && r.customer_code.trim() != want_cust {
            continue;
        }
        if !want_item.is_empty() && r.item_code.trim() != want_item {
            continue;
        }
        groups.entry(slot_key(r)).or_default().push(r);
    }

    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    for pool in groups.values() {
        let Some(base) = best_before(pool, &target) else {
            continue;
        };
        // 算出 0 或负数时不落库，但要说清是哪条、差多少 —— 静默跳过最让人上火
        let new_price = match adjusted_price(base.unit_price, spec) {
            Some(m) => m,
            None => {
                warnings.push(format!(
                    "「{}」按这个幅度调完不到 0（原价 {}）—— 这条跳过",
                    base.name,
                    base.unit_price.fmt_money()
                ));
                continue;
            }
        };
        let same_day = pool
            .iter()
            .copied()
            .find(|r| r.effective_from.trim() == target);
        rows.push(AdjustRow {
            base_id: base.id,
            rule_id: same_day.map(|r| r.id).unwrap_or(0),
            name: base.name.clone(),
            item_code: base.item_code.clone(),
            customer_code: base.customer_code.clone(),
            unit: base.unit.clone(),
            process: base.process.clone(),
            old_price: base.unit_price,
            new_price,
            effective_from: target.clone(),
            action: if same_day.is_some() {
                "覆盖"
            } else {
                "新建"
            }
            .to_string(),
        });
    }
    Ok((rows, warnings))
}

/// 执行：按预览的结果写新一版价（每一条都过 `rule_save_by`，所以调价历史里留痕）
pub fn adjust_apply(db: &Db, spec: &AdjustSpec, who: &str) -> DbResult<AdjustResult> {
    let (rows, warnings) = adjust_rows(db, spec)?;
    let target = adjust_target(spec);
    let rules = rule_list(db)?;
    let mut out = AdjustResult {
        rows: Vec::new(),
        warnings,
        created: 0,
        updated: 0,
    };
    for row in rows {
        let Some(base) = rules.iter().find(|r| r.id == row.base_id) else {
            out.warnings
                .push(format!("「{}」的底价找不到了 —— 这条跳过", row.name));
            continue;
        };
        let mut nr = base.clone();
        nr.id = row.rule_id;
        nr.unit_price = row.new_price;
        nr.effective_from = target.clone();
        if !spec.memo.trim().is_empty() {
            nr.memo = if nr.memo.trim().is_empty() {
                spec.memo.trim().to_string()
            } else {
                format!("{}；{}", nr.memo.trim(), spec.memo.trim())
            };
        }
        rule_save_by(db, &mut nr, who)?;
        if row.rule_id == 0 {
            out.created += 1;
        } else {
            out.updated += 1;
        }
        out.rows.push(row);
    }
    if !out.rows.is_empty() {
        let desc = match spec.mode.trim() {
            "pct" => format!("{}%", spec.value.fmt_qty()),
            "delta" => spec.value.fmt_money().to_string(),
            _ => format!("直接设定 {}", spec.value.fmt_money()),
        };
        db.log(
            who,
            "销售",
            "加工费规则",
            &format!(
                "批量调价（{desc}）：新增 {} 条、覆盖 {} 条，{} 起生效",
                out.created, out.updated, target
            ),
        )?;
    }
    Ok(out)
}

/// 一次调价（`price_change` 表的一行）
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PriceChange {
    pub id: i64,
    pub at: String,
    pub who: String,
    /// 新增 / 改价 / 停用 / 启用 / 删除
    pub action: String,
    pub rule_id: i64,
    pub name: String,
    pub item_code: String,
    pub customer_code: String,
    pub unit: String,
    pub process: String,
    pub effective_from: String,
    /// 改前的单价（新增时为空）
    pub old_price: String,
    pub new_price: String,
    pub memo: String,
}

const CHANGE_COLS: &str = "id,at,who,action,rule_id,name,item_code,customer_code,unit,\
                           process,effective_from,old_price,new_price,memo";

fn map_change(r: &rusqlite::Row) -> rusqlite::Result<PriceChange> {
    Ok(PriceChange {
        id: r.get(0)?,
        at: r.get(1)?,
        who: r.get(2)?,
        action: r.get(3)?,
        rule_id: r.get(4)?,
        name: r.get(5)?,
        item_code: r.get(6)?,
        customer_code: r.get(7)?,
        unit: r.get(8)?,
        process: r.get(9)?,
        effective_from: r.get(10)?,
        old_price: r.get(11)?,
        new_price: r.get(12)?,
        memo: r.get(13)?,
    })
}

/// 「2026-11-09 14:03:11」——调价历史里给人看的时刻
fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 调价历史（新的在前）。`item` 为空 = 所有物料。
pub fn changes(db: &Db, item: &str, limit: i64) -> DbResult<Vec<PriceChange>> {
    let limit = limit.clamp(1, 500);
    let item = item.trim();
    let mut st = db.conn().prepare(&format!(
        "SELECT {CHANGE_COLS} FROM price_change {} ORDER BY id DESC LIMIT ?1",
        if item.is_empty() {
            ""
        } else {
            "WHERE item_code=?2"
        }
    ))?;
    let v = if item.is_empty() {
        st.query_map(rusqlite::params![limit], map_change)?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        st.query_map(rusqlite::params![limit, item], map_change)?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(v)
}

/// 批量导入单价表的结果
///
/// 为什么不复用 `imports::ImportResult`：这里要区分「新建」和「覆盖已有那一格」——
/// 会计最怕的是把「导入 200 条」当成「加了 200 个价」，其实是改了 200 个价。
#[derive(Clone, Debug, Default)]
pub struct PriceImport {
    /// 新加的价
    pub created: usize,
    /// 覆盖了已有那一格（同物料 + 同客户 + 同口径 + 同生效日）
    pub updated: usize,
    /// 跳过的行（认不出、金额不是数、校验不过）
    pub skipped: usize,
    /// 一行一条人话，带行号
    pub warnings: Vec<String>,
}

/// 单价表的导入列头（模板与解析器共用一份，改一处就都改）
pub const RULE_HEAD: [&str; 16] = [
    "物料编码",
    "客户编码",
    "计价口径",
    "单价",
    "起步价",
    "加急固定费",
    "加急率%",
    "生效日期",
    "名称",
    "备注",
    "工艺档",
    "规格维",
    "规格下限",
    "规格上限",
    "数量下限",
    "数量上限",
];

/// 折算系数的导入列头
pub const UNIT_HEAD: [&str; 4] = ["物料编码", "计价口径", "系数", "保留位数"];

/// 单价表模板（CSV 文本，不带 BOM —— 带不带由调用方决定）
pub fn rule_import_template() -> String {
    let mut out = String::new();
    out.push_str(&RULE_HEAD.join(","));
    out.push('\n');
    // 示例：按㎡ 不分档、按只 + 工艺档、按㎡ + 工艺档、按规格分档（短件按件 / 长料按米）、按数量阶梯
    out.push_str("MB01,,㎡,12.00,30.00,20.00,10,,\"氧化-按平方（通配）\",,,,,,\n");
    out.push_str("MB02,C01,只,0.45,0,,,2026-12-01,\"给客户C01的按只价\",,硬质氧化,,,,,\n");
    out.push_str("MB01,,㎡,18.00,50.00,,,,,\"氧化-硬质\",,硬质氧化,,,,,\n");
    out.push_str("MB03,,件,0.20,,,,,\"短件按件\",,,最长边,,200,,,\n");
    out.push_str("MB03,,米,1.50,,,,,\"长料按米\",,,最长边,200,,,,\n");
    out.push_str("MB04,,件,0.35,,,,,\"小批量\",,,,,,,500\n");
    out.push_str("MB04,,件,0.25,,,,,\"大批量\",,,,,,500,\n");
    out
}

/// 折算系数模板
pub fn unit_import_template() -> String {
    let mut out = String::new();
    out.push_str(&UNIT_HEAD.join(","));
    out.push('\n');
    out.push_str("MB01,㎡,0.0234,2\n");
    out.push_str("MB02,kg,1.8500,2\n");
    out
}

/// 第几列（越界给空串）
fn col(f: &[String], i: usize) -> &str {
    f.get(i).map(|s| s.trim()).unwrap_or("")
}

/// 批量导入单价
///
/// 列顺序见 [`RULE_HEAD`]：`物料编码,客户编码,计价口径,单价,起步价,加急固定费,加急率%,生效日期,名称,备注`。
/// 只有「计价口径」与「单价」必填，其余留空即默认（物料/客户留空 = 通配，起步价等 = 0，
/// 生效日期留空 = 立即）。表头行自动跳过；`#` 开头的行按注释跳过。
///
/// 同「物料 + 客户 + 口径 + 生效日」已有价的，覆盖那一格并计入 `updated`：
/// 这是调价表最常见的用法 —— 客户发来一张新价目表，整张导进去，没变的那些也照抄一遍。
/// 靠生效日区分版本，所以「提前录下个月的新价」不会被今天这张表冲掉。
/// 这一行像不像表头。
///
/// 不能只看第一格：客户从金蝶/用友导出的表，第一行是列名，但数据行的第一格
/// 也可能带「物料」二字。这里数命中：一行里出现两个以上的列名关键词才算表头。
fn looks_like_head(f: &[String]) -> bool {
    const WORDS: [&str; 18] = [
        "物料", "客户", "客商", "口径", "单位", "单价", "起步", "加急", "生效", "名称", "备注",
        "工艺", "系数", "位数", "item", "price", "unit", "code",
    ];
    let hits = f
        .iter()
        .filter(|c| {
            let c = c.trim().to_lowercase();
            !c.is_empty() && WORDS.iter().any(|w| c.contains(w))
        })
        .count();
    hits >= 2
}

pub fn import_rules(db: &Db, text: &str, who: &str) -> DbResult<PriceImport> {
    let mut res = PriceImport::default();
    let mut head: Vec<String> = Vec::new();
    let mut line_no = 0usize;
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        line_no += 1;
        let f = crate::imports::split_csv(line);
        if looks_like_head(&f) {
            head = f.clone();
            continue;
        }
        // 有表头就按列名对齐（客户表里列的顺序爱怎么排怎么排，也能只贴五列）；
        // 没表头按固定顺序 —— 老模板、老脚本照旧能导。
        let at = |aliases: &[&str], fallback: usize| -> String {
            // 有表头就**完全**按列名：表里没这一列就是没给（空），
            // 不能退回「第 N 格」—— 客户的表把工艺档放在第五列时，
            // 按位置读会把「硬质氧化」当成起步价，报一句「起步价不是数字」就完事，
            // 用户根本想不到是列错位。
            let i = if head.is_empty() {
                fallback
            } else {
                match head.iter().position(|h| {
                    let h = h.trim();
                    !h.is_empty() && aliases.iter().any(|a| h.contains(a))
                }) {
                    Some(i) => i,
                    None => return String::new(),
                }
            };
            col(&f, i).trim().to_string()
        };
        let item = at(&["物料"], 0);
        let cust = at(&["客户", "客商"], 1);
        let unit = at(&["口径", "单位"], 2);
        let price_s = at(&["单价"], 3);
        if price_s.is_empty() {
            res.skipped += 1;
            res.warnings.push(format!(
                "第 {line_no} 行：找不到「单价」这一列（这一行只有 {} 列）",
                f.len()
            ));
            continue;
        }
        if unit.is_empty() {
            res.skipped += 1;
            res.warnings.push(format!(
                "第 {line_no} 行：没有计价口径（按㎡还是按只），这一行导不了"
            ));
            continue;
        }
        let price = match Money::parse(&price_s) {
            Ok(v) => v,
            Err(_) => {
                res.skipped += 1;
                res.warnings.push(format!(
                    "第 {line_no} 行：单价「{price_s}」不是数字，这一行导不了"
                ));
                continue;
            }
        };
        let money_of = |aliases: &[&str], fallback: usize, what: &str, res: &mut PriceImport| {
            let s = at(aliases, fallback);
            if s.is_empty() {
                return Some(Money::ZERO);
            }
            match Money::parse(&s) {
                Ok(v) => Some(v),
                Err(_) => {
                    res.skipped += 1;
                    res.warnings.push(format!(
                        "第 {line_no} 行：{what}「{s}」不是数字，这一行导不了"
                    ));
                    None
                }
            }
        };
        let min_charge = match money_of(&["起步", "最低"], 4, "起步价", &mut res) {
            Some(v) => v,
            None => continue,
        };
        let rush_fee = match money_of(&["加急固定", "加急费"], 5, "加急固定费", &mut res)
        {
            Some(v) => v,
            None => continue,
        };
        let rush_rate = match money_of(&["加急率", "加急%"], 6, "加急率", &mut res) {
            Some(v) => v,
            None => continue,
        };
        let effective_from = at(&["生效"], 7);
        let name = at(&["名称"], 8);
        let memo = at(&["备注"], 9);
        // 工艺档（普通氧化 / 硬质氧化 …）：放最后一列，老模板没有这一列就是空 = 不分档
        let proc_s = at(&["工艺", "表面处理"], 10);
        // 规格档（按尺寸分段）：维度 + 上下限（cm）。中文写「最长边/最短边/第一维/面积」，
        // 也认英文 max/min/first/area —— 客户给的表里写什么都有
        let band_raw = at(&["规格维", "按哪一维", "比哪一维", "尺寸维"], 11);
        let band_dim = match band_raw.trim() {
            "" => String::new(),
            "最长边" | "长边" | "max" => "max".to_string(),
            "最短边" | "短边" | "min" => "min".to_string(),
            "第一维" | "长度" | "first" => "first".to_string(),
            "面积" | "area" => "area".to_string(),
            other => {
                res.skipped += 1;
                res.warnings.push(format!(
                    "第 {line_no} 行：规格维「{other}」看不懂 —— 填 最长边/最短边/第一维/面积"
                ));
                continue;
            }
        };
        let band_from = at(&["规格下限", "尺寸下限", "下限"], 12);
        let band_to = at(&["规格上限", "尺寸上限", "上限"], 13);
        // 数量档（按批量分档）：比较对象是计价数量，空 = 不限
        let qty_from = at(&["数量下限", "件数下限", "数量从"], 14);
        let qty_to = at(&["数量上限", "件数上限", "数量到"], 15);
        // 名称留空就按几件事拼一个，免得矩阵里一整列都是空名字
        let auto_name = format!(
            "{} {unit}（{}）{}",
            if item.is_empty() {
                "任意物料"
            } else {
                &item
            },
            if cust.is_empty() { "通用" } else { &cust },
            if proc_s.is_empty() {
                String::new()
            } else {
                format!("·{proc_s}")
            }
        );
        let exist: Option<i64> = db
            .conn()
            .query_row(
                "SELECT id FROM price_rule WHERE item_code=?1 AND customer_code=?2
                 AND unit=?3 AND effective_from=?4 AND process=?5
                 AND band_dim=?6 AND band_from=?7 AND band_to=?8
                 AND qty_from=?9 AND qty_to=?10
                 ORDER BY id LIMIT 1",
                rusqlite::params![
                    item,
                    cust,
                    unit,
                    effective_from,
                    proc_s,
                    band_dim,
                    band_from,
                    band_to,
                    qty_from,
                    qty_to
                ],
                |r| r.get(0),
            )
            .ok();
        let mut r = PriceRule {
            id: exist.unwrap_or(0),
            name: if name.is_empty() { auto_name } else { name },
            item_code: item,
            customer_code: cust,
            unit,
            unit_price: price,
            min_charge,
            rush_fee,
            rush_rate,
            disabled: false,
            memo,
            effective_from,
            process: proc_s,
            band_dim,
            band_from,
            band_to,
            qty_from,
            qty_to,
        };
        match rule_save_by(db, &mut r, who) {
            Ok(_) => {
                if exist.is_some() {
                    res.updated += 1;
                } else {
                    res.created += 1;
                }
            }
            Err(e) => {
                res.skipped += 1;
                res.warnings
                    .push(format!("第 {line_no} 行：{}", short_err(&e)));
            }
        }
    }
    if res.created + res.updated > 0 {
        db.log(
            who,
            "销售",
            "加工费规则",
            &format!(
                "批量导入单价：新增 {} 条、覆盖 {} 条、跳过 {} 行",
                res.created, res.updated, res.skipped
            ),
        )?;
    }
    Ok(res)
}

pub fn import_calc_units(db: &Db, text: &str, who: &str) -> DbResult<PriceImport> {
    let mut res = PriceImport::default();
    let mut head: Vec<String> = Vec::new();
    let mut line_no = 0usize;
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        line_no += 1;
        let f = crate::imports::split_csv(line);
        if looks_like_head(&f) {
            head = f.clone();
            continue;
        }
        let at = |aliases: &[&str], fallback: usize| -> String {
            // 有表头就**完全**按列名：表里没这一列就是没给（空），
            // 不能退回「第 N 格」—— 客户的表把工艺档放在第五列时，
            // 按位置读会把「硬质氧化」当成起步价，报一句「起步价不是数字」就完事，
            // 用户根本想不到是列错位。
            let i = if head.is_empty() {
                fallback
            } else {
                match head.iter().position(|h| {
                    let h = h.trim();
                    !h.is_empty() && aliases.iter().any(|a| h.contains(a))
                }) {
                    Some(i) => i,
                    None => return String::new(),
                }
            };
            col(&f, i).trim().to_string()
        };
        let item = at(&["物料"], 0);
        let unit = at(&["口径", "单位"], 1);
        let factor_s = at(&["系数"], 2);
        if item.is_empty() || unit.is_empty() {
            res.skipped += 1;
            res.warnings.push(format!(
                "第 {line_no} 行：物料编码和计价口径都要填 —— 系数说的是「这个物料的 1 个库存单位折合多少 {unit}」"
            ));
            continue;
        }
        let factor = match Money::parse(&factor_s) {
            Ok(v) => v,
            Err(_) => {
                res.skipped += 1;
                res.warnings.push(format!(
                    "第 {line_no} 行：系数「{factor_s}」不是数字，这一行导不了"
                ));
                continue;
            }
        };
        let scale_s = at(&["位数"], 3);
        let scale = if scale_s.is_empty() {
            2
        } else {
            match scale_s.parse::<i32>() {
                Ok(v) => v,
                Err(_) => {
                    res.skipped += 1;
                    res.warnings.push(format!(
                        "第 {line_no} 行：保留位数「{scale_s}」要写 0-{SCALE_MAX} 的整数"
                    ));
                    continue;
                }
            }
        };
        let exist: Option<i64> = db
            .conn()
            .query_row(
                "SELECT id FROM item_calc_unit WHERE item_code=?1 AND calc_unit=?2",
                rusqlite::params![item, unit],
                |r| r.get(0),
            )
            .ok();
        let mut c = CalcUnit {
            id: exist.unwrap_or(0),
            item_code: item,
            calc_unit: unit,
            factor,
            scale,
            disabled: false,
            memo: String::new(),
        };
        match calc_unit_save(db, &mut c) {
            Ok(_) => {
                if exist.is_some() {
                    res.updated += 1;
                } else {
                    res.created += 1;
                }
            }
            Err(e) => {
                res.skipped += 1;
                res.warnings
                    .push(format!("第 {line_no} 行：{}", short_err(&e)));
            }
        }
    }
    if res.created + res.updated > 0 {
        db.log(
            who,
            "销售",
            "折算系数",
            &format!(
                "批量导入系数：新增 {} 条、覆盖 {} 条、跳过 {} 行",
                res.created, res.updated, res.skipped
            ),
        )?;
    }
    Ok(res)
}

/// 导入警告里用的人话
///
/// `DbError::Fin` 直接显示内层消息（`SQLite 错误：…` 只在真有 SQL 问题时才出现），
/// 所以这里不要再剥前缀 —— 剥「：」会把「生效日期要写成 2026-03-01 这样（实际：2026-3-1）」
/// 截成半句。
fn short_err(e: &dyn std::fmt::Display) -> String {
    format!("{e}")
}

/// 匹配一条规则：物料+客户 精确 > 只限物料 > 只限客户 > 通配
///
/// 停用的规则不参与匹配；同精度取 id 最小的一条（先建的优先）。
///
/// 不挑口径的老接口（桌面端还在用）：口径由调用方自己保证。
pub fn match_rule(db: &Db, item: &str, customer: &str) -> DbResult<Option<PriceRule>> {
    Ok(match_rule_for(db, item, customer, "")?.map(|(r, _)| r))
}

/// 匹配一条规则，并且**只认同口径**。
///
/// `calc_unit` 非空时，规则上的 `unit` 必须与它一致 —— 「12 元/㎡」的价
/// 拿去乘「300 只」是几百倍的错账，不能靠人小心。
///
/// 返回 `(规则, 是否尚未生效)`：还没到生效日的价排在已生效之后；
/// 只有未来价时才用它（并告诉调用方「这个价 X 月 X 日才生效」），
/// 而不是干脆报「没有规则」——那会让老板以为自己的价丢了。
pub fn match_rule_for(
    db: &Db,
    item: &str,
    customer: &str,
    calc_unit: &str,
) -> DbResult<Option<(PriceRule, bool)>> {
    match_rule_for_process(db, item, customer, calc_unit, "")
}

/// 带工艺档的匹配。`process` 空 = 只认「不分档」的价。
///
/// 档位优先于「不分档」，但低于物料/客户的精度：
/// 「这个客户的硬质氧化」>「这个客户的通用价」>「不分档的硬质氧化」>「全部通用」。
/// 换句话说，先按规矩（物料/客户）筛，再看档，最后按生效日挑最新的一条。
pub fn match_rule_for_process(
    db: &Db,
    item: &str,
    customer: &str,
    calc_unit: &str,
    process: &str,
) -> DbResult<Option<(PriceRule, bool)>> {
    pick_rule(db, item, customer, calc_unit, process, None, None)
}

/// 带规格档的匹配：`dims` 非空时，只有「不分档」或「这一档管这个尺寸」的价才算数。
///
/// 分档的价**在没给规格时一律不参与**（宁可报「没有这一档的价」，也不拿
/// 300cm 的 1.5 元/米去乘一件 80cm 的活儿）。
pub fn match_rule_for_spec(
    db: &Db,
    item: &str,
    customer: &str,
    calc_unit: &str,
    process: &str,
    dims: &[Money],
) -> DbResult<Option<(PriceRule, bool)>> {
    pick_rule(db, item, customer, calc_unit, process, Some(dims), None)
}

/// 完整匹配：规格档 + 数量档一起看。报价路径都走这里 ——
/// 只给规格不给数量，阶梯价就永远轮不到。
pub fn match_rule_full(
    db: &Db,
    item: &str,
    customer: &str,
    calc_unit: &str,
    process: &str,
    dims: Option<&[Money]>,
    qty: Option<Money>,
) -> DbResult<Option<(PriceRule, bool)>> {
    pick_rule(db, item, customer, calc_unit, process, dims, qty)
}

fn pick_rule(
    db: &Db,
    item: &str,
    customer: &str,
    calc_unit: &str,
    process: &str,
    dims: Option<&[Money]>,
    qty: Option<Money>,
) -> DbResult<Option<(PriceRule, bool)>> {
    let want_proc = process.trim();
    let item = item.trim();
    let customer = customer.trim();
    let want_unit = calc_unit.trim();
    let today = today();
    let mut cands: Vec<(u8, u8, u8, u8, PriceRule)> = Vec::new();
    for r in rule_list(db)? {
        if r.disabled {
            continue;
        }
        if !want_unit.is_empty() && r.unit.trim() != want_unit {
            continue;
        }
        // 档位：给了档就只认「这一档」和「不分档」；没给档只认「不分档」——
        // 否则没选档位的报价会随手抓到「硬质氧化」的价，客户拿普通氧化的单子来对账说不清。
        let proc_rank = if r.process.trim() == want_proc {
            0u8
        } else if r.process.trim().is_empty() && !want_proc.is_empty() {
            1u8
        } else {
            continue;
        };
        // 规格档：给了规格就看这一档管不管，没给就跳过所有分档价
        let band_rank = if r.band_dim.trim().is_empty() {
            1u8
        } else {
            match dims {
                Some(d) if band_ok(&r, d) => 0u8,
                _ => continue,
            }
        };
        // 数量档：看这一批多少。没给数量时数量档一律不参与 —— 拿「500 以上」的
        // 批发价去报 20 件的小单，客户当场就会发现，但单子已经发出去了。
        let qty_rank = match qty_rank_of(&r, qty) {
            Some(k) => k,
            None => continue,
        };
        let rank = if item.is_empty() && customer.is_empty() {
            // 没给物料也没给客户：只有通配规则能报价
            if r.item_code.is_empty() && r.customer_code.is_empty() {
                3u8
            } else {
                continue;
            }
        } else if !item.is_empty() && !customer.is_empty() {
            if r.item_code == item && r.customer_code == customer {
                0
            } else if r.item_code == item && r.customer_code.is_empty() {
                1
            } else if r.item_code.is_empty() && r.customer_code == customer {
                2
            } else if r.item_code.is_empty() && r.customer_code.is_empty() {
                3
            } else {
                continue;
            }
        } else if !item.is_empty() {
            // 只给了物料：限定物料的规则优先，其次通配
            if r.item_code == item && r.customer_code.is_empty() {
                1
            } else if r.item_code.is_empty() && r.customer_code.is_empty() {
                3
            } else {
                continue;
            }
        } else {
            // 只给了客户：限定客户的规则优先，其次通配
            if r.item_code.is_empty() && r.customer_code == customer {
                2
            } else if r.item_code.is_empty() && r.customer_code.is_empty() {
                3
            } else {
                continue;
            }
        };
        cands.push((rank, proc_rank, band_rank, qty_rank, r));
    }
    // 精度优先，其次「现在该用哪一条」：已生效的价里挑生效日最晚的。
    // 不这么做的话，提前录的下一版价永远排在后面 —— 到了生效日也不会自动换过来，
    // 老板还得记得回来删旧价，那就等于没这个功能。
    for rank in 0..=3u8 {
        for proc_rank in 0..=1u8 {
            for band_rank in 0..=1u8 {
                for qty_rank in 0..=1u8 {
                    let pool: Vec<&PriceRule> = cands
                        .iter()
                        .filter(|(k, p, b, q, _)| {
                            *k == rank && *p == proc_rank && *b == band_rank && *q == qty_rank
                        })
                        .map(|(_, _, _, _, r)| r)
                        .collect();
                    if pool.is_empty() {
                        continue;
                    }
                    if let Some(r) = best_active(&pool, &today) {
                        return Ok(Some((r.clone(), false)));
                    }
                    if let Some(r) = best_next(&pool, &today) {
                        return Ok(Some((r.clone(), true)));
                    }
                }
            }
        }
    }
    Ok(None)
}

/// 这一条规则的数量档能不能用在 `qty` 上：`Some(0)` 命中、`Some(1)` 不分档、`None` 不适用
///
/// 同时返回「档位优先级」，是因为同一格里可能既有「500 件以下」又有不分档的价：
/// 命中的档位优先，没有档位的价兜底 —— 但**没给数量时档位价一律不参与**。
fn qty_rank_of(r: &PriceRule, qty: Option<Money>) -> Option<u8> {
    let from = r.qty_from.trim();
    let to = r.qty_to.trim();
    if from.is_empty() && to.is_empty() {
        return Some(1);
    }
    let q = qty?;
    if !from.is_empty() {
        let lo = Money::parse(from).ok()?;
        // 下限不含：「500 件以上」从 501 起算，正好 500 件算下面那一档 ——
        // 与规格档同一口径，避免一批正好 500 件时两条都命中、谁先建谁赢。
        if q <= lo {
            return None;
        }
    }
    if !to.is_empty() {
        let hi = Money::parse(to).ok()?;
        if q > hi {
            return None;
        }
    }
    Some(0)
}

/// 已生效的价里，生效日最晚的那条（空日期算最早），同日期按建的先后
fn best_active<'a>(pool: &[&'a PriceRule], today: &str) -> Option<&'a PriceRule> {
    pool.iter()
        .copied()
        .filter(|r| !is_future(r, today))
        .min_by(|a, b| {
            b.effective_from
                .trim()
                .cmp(a.effective_from.trim())
                .then_with(|| a.id.cmp(&b.id))
        })
}

/// 只有未来价时，挑最早要生效的那条（下一个要用的价）
fn best_next<'a>(pool: &[&'a PriceRule], today: &str) -> Option<&'a PriceRule> {
    pool.iter()
        .copied()
        .filter(|r| is_future(r, today))
        .min_by(|a, b| {
            a.effective_from
                .trim()
                .cmp(b.effective_from.trim())
                .then_with(|| a.id.cmp(&b.id))
        })
}

/// 一格该显示哪条价：先看已生效里最新的，没有就看看见的最早的未来价
fn best_for_cell<'a>(pool: Vec<&'a PriceRule>, today: &str) -> Option<&'a PriceRule> {
    best_active(&pool, today).or_else(|| best_next(&pool, today))
}

/// 按匹配到的规则报价
pub fn quote_by(db: &Db, item: &str, customer: &str, qty: Money, rush: bool) -> DbResult<FeeQuote> {
    if !qty.is_positive() {
        return Err(FinError::msg("计价数量必须大于 0").into());
    }
    // 数量阶梯价在「老算法」这条路上也要生效：这里给的 qty 就是计价数量
    let (rule, _future) = match_rule_full(db, item, customer, "", "", None, Some(qty))?
        .ok_or_else(|| {
            FinError::msg(format!(
                "没有匹配的加工费规则（物料 {}／客户 {}），先在「加工费规则」里维护一条",
                if item.trim().is_empty() {
                    "任意"
                } else {
                    item.trim()
                },
                if customer.trim().is_empty() {
                    "任意"
                } else {
                    customer.trim()
                }
            ))
        })?;
    Ok(quote_on(&rule, qty, rush, &today()))
}

/// 规则一览（id → 名称），给前端下拉用
pub fn rule_names(db: &Db) -> DbResult<HashMap<i64, String>> {
    Ok(rule_list(db)?.into_iter().map(|r| (r.id, r.name)).collect())
}

// ===========================================================================
// 计价口径与折算系数（v49）
// ===========================================================================
//
// 五金氧化厂的现实：来料按「只 / 条 / 件」数，报价却常常按「㎡ / 米 / kg」。
// 中间那一层换算以前全靠人脑 —— 300 只 × 0.0234 ㎡/只 = 7.02 ㎡，
// 算错一次就是几十块，而且没人能从单子上看出算错了。
//
// 所以每个物料可以挂若干「计价口径」，每个口径一个系数：
// **1 个库存单位 = factor 个计价单位**。口径与 [`PriceRule::unit`] 一一对应，
// 选口径就是选单价表里的哪一列。

/// 折算后允许的最多小数位（再多没有业务意义，只是让人看花眼）
pub const SCALE_MAX: i32 = 4;

/// 物料的计价口径与折算系数
#[derive(Clone, Debug, serde::Serialize)]
pub struct CalcUnit {
    pub id: i64,
    pub item_code: String,
    /// 计价口径（㎡ / 米 / kg / 只 …）
    pub calc_unit: String,
    /// 1 个库存单位 = factor 个计价单位
    pub factor: Money,
    /// 折算后保留几位小数（0-4）
    pub scale: i32,
    pub disabled: bool,
    pub memo: String,
}

impl Default for CalcUnit {
    fn default() -> Self {
        Self {
            id: 0,
            item_code: String::new(),
            calc_unit: String::new(),
            factor: Money::ZERO,
            scale: 2,
            disabled: false,
            memo: String::new(),
        }
    }
}

const CALC_COLS: &str = "id,item_code,calc_unit,factor,scale,disabled,memo";

fn map_calc(r: &rusqlite::Row) -> rusqlite::Result<CalcUnit> {
    Ok(CalcUnit {
        id: r.get(0)?,
        item_code: r.get(1)?,
        calc_unit: r.get(2)?,
        factor: crate::read_money(r, 3)?,
        scale: r.get::<_, i64>(4)? as i32,
        disabled: r.get::<_, i64>(5)? != 0,
        memo: r.get(6)?,
    })
}

/// 口径列表（`item` 空 = 全部物料）
pub fn calc_unit_list(db: &Db, item: &str) -> DbResult<Vec<CalcUnit>> {
    let item = item.trim();
    if item.is_empty() {
        let sql = format!("SELECT {CALC_COLS} FROM item_calc_unit ORDER BY item_code, calc_unit");
        let mut st = db.conn().prepare(&sql)?;
        let v = st.query_map([], map_calc)?.collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    } else {
        let sql =
            format!("SELECT {CALC_COLS} FROM item_calc_unit WHERE item_code=?1 ORDER BY calc_unit");
        let mut st = db.conn().prepare(&sql)?;
        let v = st
            .query_map([item], map_calc)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(v)
    }
}

/// 保存口径（同一物料 + 同一口径天然只有一条，`id` 只用来定位）
pub fn calc_unit_save(db: &Db, c: &mut CalcUnit) -> DbResult<i64> {
    let item = c.item_code.trim();
    let unit = c.calc_unit.trim();
    if item.is_empty() {
        return Err(FinError::msg("口径要挂在具体物料上（物料必填）").into());
    }
    if unit.is_empty() {
        return Err(FinError::msg("计价口径必填（㎡ / 米 / kg / 只 …）").into());
    }
    if !c.factor.is_positive() {
        return Err(FinError::msg(format!("1 {item} 折合多少 {unit} 必须大于 0")).into());
    }
    if c.scale < 0 || c.scale > SCALE_MAX {
        return Err(FinError::msg(format!("保留位数应在 0-{SCALE_MAX} 之间")).into());
    }
    let tx = db.write_tx()?;
    // 先按 (物料, 口径) 找现成的：同一格重复录不该变成两条
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM item_calc_unit WHERE item_code=?1 AND calc_unit=?2",
            rusqlite::params![item, unit],
            |r| r.get(0),
        )
        .ok();
    let id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE item_calc_unit SET factor=?2,scale=?3,disabled=?4,memo=?5 WHERE id=?1",
                rusqlite::params![
                    id,
                    crate::exact_param(c.factor),
                    c.scale as i64,
                    c.disabled as i64,
                    c.memo
                ],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO item_calc_unit(item_code,calc_unit,factor,scale,disabled,memo)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    item,
                    unit,
                    crate::exact_param(c.factor),
                    c.scale as i64,
                    c.disabled as i64,
                    c.memo
                ],
            )?;
            tx.last_insert_rowid()
        }
    };
    tx.commit()?;
    c.id = id;
    c.item_code = item.to_string();
    c.calc_unit = unit.to_string();
    Ok(id)
}

/// 删除一条口径
pub fn calc_unit_delete(db: &Db, id: i64) -> DbResult<()> {
    let n = db
        .conn()
        .execute("DELETE FROM item_calc_unit WHERE id=?1", [id])?;
    if n == 0 {
        return Err(FinError::msg(format!("口径 {id} 不存在")).into());
    }
    Ok(())
}

/// 某物料某口径的系数
pub fn calc_unit_of(db: &Db, item: &str, unit: &str) -> DbResult<Option<CalcUnit>> {
    let sql = format!(
        "SELECT {CALC_COLS} FROM item_calc_unit WHERE item_code=?1 AND calc_unit=?2 AND disabled=0"
    );
    let mut st = db.conn().prepare(&sql)?;
    let mut rows = st.query_map(rusqlite::params![item.trim(), unit.trim()], map_calc)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// 数量折算：库存单位 → 计价单位，**先把数量按口径位数抹平，再乘单价**。
///
/// 顺序不能反。对账单上写「300 只 = 7.02 ㎡ × 9.80 = 68.80」，
/// 客户拿计算器按一遍必须正好是 68.80；先乘后入会出现
/// 「7.015 × 9.8 = 68.75」与单上的 68.80 对不上，一句「你怎么算的」就说不清。
pub fn convert(qty: Money, c: &CalcUnit) -> Money {
    (qty * c.factor).round_dp(c.scale.clamp(0, SCALE_MAX) as u32)
}

/// 一次「按库存单位报价」的完整过程（前端要把每一步都显示出来）
#[derive(Clone, Debug, serde::Serialize)]
pub struct UnitQuote {
    pub item_code: String,
    /// 库存单位下填的数量（300）
    pub stock_qty: Money,
    pub stock_unit: String,
    /// 1 库存单位 = factor 计价单位（0.0234）
    pub factor: Money,
    /// 折算后的计价数量（7.02）
    pub calc_qty: Money,
    pub calc_unit: String,
    pub quote: FeeQuote,
}

/// 按「数量 + 口径」报价：先折算，再套单价规则
pub fn quote_with_units(
    db: &Db,
    item: &str,
    customer: &str,
    stock_qty: Money,
    calc_unit: &str,
    process: &str,
    rush: bool,
) -> DbResult<UnitQuote> {
    let item = item.trim();
    let unit = calc_unit.trim();
    if !stock_qty.is_positive() {
        return Err(FinError::msg("数量必须大于 0").into());
    }
    if item.is_empty() {
        return Err(FinError::msg("先选物料：不同物料的折算系数不一样").into());
    }
    if unit.is_empty() {
        return Err(FinError::msg("先选计价口径（按 ㎡ 还是按只）").into());
    }
    let cu = calc_unit_of(db, item, unit)?.ok_or_else(|| {
        FinError::msg(format!(
            "物料 {item} 还没设「1 {item} = ? {unit}」的折算系数，先在「加工费规则 → 折算系数」里录一条"
        ))
    })?;
    let calc_qty = convert(stock_qty, &cu);
    if !calc_qty.is_positive() {
        return Err(FinError::msg(format!(
            "折算后只有 {calc_qty} {unit}（系数 {} 太小或数量太少），请核对系数",
            cu.factor
        ))
        .into());
    }
    // 数量档按**折算后的计价数量**判 —— 「500 件以上」「500 ㎡ 以上」都自然成立
    let (rule, _future) = match_rule_full(db, item, customer, unit, process, None, Some(calc_qty))?
        .ok_or_else(|| {
            FinError::msg(format!(
                "没有按「{unit}」计的单价（物料 {item}／客户 {}）——单价表里这一格还是空的",
                if customer.trim().is_empty() {
                    "任意"
                } else {
                    customer.trim()
                }
            ))
        })?;
    let mut q = quote_on(&rule, calc_qty, rush, &today());
    let stock_unit = stock_unit_of(db, item)?;
    q.effective_from = rule.effective_from.clone();
    Ok(UnitQuote {
        item_code: item.to_string(),
        stock_qty,
        stock_unit,
        factor: cu.factor,
        calc_qty,
        calc_unit: unit.to_string(),
        quote: q,
    })
}

/// 库存单位（item_unit.base_unit，未设 = 空）
fn stock_unit_of(db: &Db, item: &str) -> DbResult<String> {
    let mut st = db
        .conn()
        .prepare("SELECT base_unit FROM item_unit WHERE item=?1")?;
    let mut rows = st.query_map([item], |r| r.get::<_, String>(0))?;
    Ok(match rows.next() {
        Some(Ok(u)) => u,
        _ => String::new(),
    })
}

/// 单价矩阵里的一格
#[derive(Clone, Debug, serde::Serialize)]
pub struct MatrixCell {
    pub unit: String,
    /// 0 = 这一格还没定价
    pub rule_id: i64,
    pub unit_price: Money,
    pub min_charge: Money,
    pub effective_from: String,
    pub future: bool,
    /// 该物料这个口径的系数（`None` = 没录系数，这一格不能报价）
    pub factor: Option<Money>,
    pub scale: i32,
    /// 这个价是从「不分档」的通用价借来的（当前选的档位没单独定价）
    pub fallback: bool,
}

/// 单价矩阵的一行
#[derive(Clone, Debug, serde::Serialize)]
pub struct MatrixRow {
    pub item_code: String,
    pub item_name: String,
    pub stock_unit: String,
    pub cells: Vec<MatrixCell>,
}

/// 单价矩阵：行 = 物料，列 = 计价口径，格 = 单价
#[derive(Clone, Debug, serde::Serialize)]
pub struct PriceMatrix {
    pub customer: String,
    /// 当前看的是哪一档（空 = 不分档）
    pub process: String,
    pub units: Vec<String>,
    pub rows: Vec<MatrixRow>,
}

/// 口径的显示顺序：常用的排前面，没列到的按字典序垫后
const UNIT_ORDER: &[&str] = &["只", "条", "件", "个", "米", "kg", "公斤", "㎡", "平方"];

fn unit_rank(u: &str) -> (u8, String) {
    match UNIT_ORDER.iter().position(|x| *x == u) {
        Some(i) => (i as u8, String::new()),
        None => (u8::MAX, u.to_string()),
    }
}

/// 单价矩阵（`customer` 空 = 只看通用价）
///
/// 一屏看全「这个客户，每个物料按每种口径各多少钱」——这正是老板问
/// 「单价怎么高效管理」时真正要的那张表：不是一长串规则，是矩阵。
pub fn matrix(db: &Db, customer: &str, process: &str) -> DbResult<PriceMatrix> {
    let customer = customer.trim();
    let process = process.trim();
    let today = today();
    let units_all = calc_unit_list(db, "")?;
    // 分档价（按规格分段）不进矩阵：矩阵一格是「物料×口径」的一个价，
    // 而分档价在同一格里可能有两条（短件按件、长料按米），塞进去会被当成通价。
    // 它们单独在「按规格分段的价」里看。
    let rules: Vec<PriceRule> = rule_list(db)?
        .into_iter()
        .filter(|r| {
            r.band_dim.trim().is_empty()
                && r.qty_from.trim().is_empty()
                && r.qty_to.trim().is_empty()
        })
        .collect();

    // 列：口径来自「系数表」与「单价表」的并集
    let mut unit_set: Vec<String> = Vec::new();
    for c in &units_all {
        if !c.disabled && !unit_set.contains(&c.calc_unit) {
            unit_set.push(c.calc_unit.clone());
        }
    }
    for r in &rules {
        if r.disabled {
            continue;
        }
        let u = r.unit.trim().to_string();
        if !u.is_empty() && !unit_set.contains(&u) {
            unit_set.push(u);
        }
    }
    unit_set.sort_by_key(|u| unit_rank(u));

    // 行：有系数的物料 ∪ 有单价的物料（通配规则单独一行）
    let mut codes: Vec<String> = Vec::new();
    for c in &units_all {
        if !c.disabled && !codes.contains(&c.item_code) {
            codes.push(c.item_code.clone());
        }
    }
    let mut has_wildcard = false;
    for r in &rules {
        if r.disabled {
            continue;
        }
        if r.item_code.trim().is_empty() {
            if r.customer_code.trim().is_empty() || r.customer_code.trim() == customer {
                has_wildcard = true;
            }
        } else if !codes.contains(&r.item_code) {
            codes.push(r.item_code.clone());
        }
    }
    codes.sort();

    let names = item_names(db)?;
    let mut rows = Vec::new();
    if has_wildcard {
        rows.push(MatrixRow {
            item_code: String::new(),
            item_name: "任意物料（通配价）".to_string(),
            stock_unit: String::new(),
            cells: build_cells("", customer, process, &unit_set, &units_all, &rules, &today),
        });
    }
    for code in codes {
        rows.push(MatrixRow {
            item_name: names.get(&code).cloned().unwrap_or_else(|| code.clone()),
            stock_unit: stock_unit_of(db, &code)?,
            cells: build_cells(
                &code, customer, process, &unit_set, &units_all, &rules, &today,
            ),
            item_code: code,
        });
    }
    Ok(PriceMatrix {
        customer: customer.to_string(),
        process: process.to_string(),
        units: unit_set,
        rows,
    })
}

/// 分段价：带规格档（按尺寸）或数量档（按批量）的规则。
///
/// 规格档是「200cm 以下 0.2 元/件、以上 1.5 元/米」的落地形态：每条自带区间和口径，
/// 报价时按规格自动挑档 —— 不用人先算一遍尺寸再决定按件还是按米。
/// 数量档是「500 件以下 0.35、以上 0.25」：同一种活儿小批量要调机、大批量摊薄。
/// 两者都**不进单价矩阵**（一格装不下两条），合在这张表里看。
pub fn band_list(db: &Db, customer: &str) -> DbResult<Vec<PriceRule>> {
    let customer = customer.trim();
    let mut v: Vec<PriceRule> = rule_list(db)?
        .into_iter()
        .filter(|r| {
            (!r.band_dim.trim().is_empty()
                || !r.qty_from.trim().is_empty()
                || !r.qty_to.trim().is_empty())
                && (customer.is_empty() || r.customer_code.trim() == customer)
        })
        .collect();
    v.sort_by(|a, b| {
        a.item_code
            .cmp(&b.item_code)
            .then(a.customer_code.cmp(&b.customer_code))
            .then(a.process.cmp(&b.process))
            .then(
                band_num(&a.band_from)
                    .partial_cmp(&band_num(&b.band_from))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                band_num(&a.qty_from)
                    .partial_cmp(&band_num(&b.qty_from))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.id.cmp(&b.id))
    });
    Ok(v)
}

fn band_num(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.0)
}

fn build_cells(
    item: &str,
    customer: &str,
    process: &str,
    units: &[String],
    calc_units: &[CalcUnit],
    rules: &[PriceRule],
    today: &str,
) -> Vec<MatrixCell> {
    units
        .iter()
        .map(|u| {
            let cu = calc_units
                .iter()
                .find(|c| !c.disabled && c.item_code == item && c.calc_unit == *u);
            // 同一格可能有好几条（专属价/通用价，以及不同生效日）：专属优先，
            // 再按「现在生效的最新一条」
            let pick = |want_cust: &str, want_proc: &str| -> Vec<&PriceRule> {
                rules
                    .iter()
                    .filter(|r| {
                        !r.disabled
                            && r.item_code == item
                            && r.unit.trim() == u.as_str()
                            && r.customer_code.trim() == want_cust
                            && r.process.trim() == want_proc
                    })
                    .collect()
            };
            // 一格的价格优先级：客户+档 > 客户+通用档 > 通用客户+档 > 通用客户+通用档。
            // 后两种借来的价标 fallback，界面上要说清楚「这是通用档的价」——
            // 不标的话，老板以为这一档定过价，实际开单时按的是别的档。
            let try_pair = |want_cust: &str| -> Option<(&PriceRule, bool)> {
                if let Some(r) = best_for_cell(pick(want_cust, process), today) {
                    return Some((r, false));
                }
                if process.is_empty() {
                    return None;
                }
                best_for_cell(pick(want_cust, ""), today).map(|r| (r, true))
            };
            let hit = if customer.is_empty() {
                try_pair("")
            } else {
                try_pair(customer).or_else(|| try_pair(""))
            };
            let (rule, fallback) = (hit.map(|(r, _f)| r), hit.map(|(_r, f)| f).unwrap_or(false));
            MatrixCell {
                unit: u.clone(),
                rule_id: rule.map(|r| r.id).unwrap_or(0),
                unit_price: rule.map(|r| r.unit_price).unwrap_or(Money::ZERO),
                min_charge: rule.map(|r| r.min_charge).unwrap_or(Money::ZERO),
                effective_from: rule.map(|r| r.effective_from.clone()).unwrap_or_default(),
                future: rule.map(|r| is_future(r, today)).unwrap_or(false),
                factor: cu.map(|c| c.factor),
                scale: cu.map(|c| c.scale).unwrap_or(2),
                fallback,
            }
        })
        .collect()
}

// ── 按规格折算（v1.8.89） ──────────────────────────────────────────────
//
// 氧化厂报价最常听见的两句话：「这个件 2.5 元一米」「这个件 80 元一个平方」。
// 客户报的是**费率**，工件给的是**规格**，中间的「1 件折合多少米 / 多少平方」
// 不该让人拿计算器按一遍再抄进折算系数 —— 抄错一次就差几百块，而且单子看着没问题。
//
// 口径与规格的对应关系（这张表就是全部规则）：
//   只 / 条 / 件 / 个 / 支 / 片 / 套 → 1（数件，规格不影响）
//   米 / 米长                        → 第一个尺寸 ÷ 1000（毫米 → 米）
//   ㎡ / 平方 / 平方米 / 平米         → 前两个尺寸相乘 ÷ 1e6
//   kg / 公斤 / 千克                 → 三个尺寸相乘 ÷ 1e6 × 密度（g/cm³）
//
// 密度只对重量口径有意义，默认按钢 7.85 g/cm³；客户送来的若是铝件（2.70）、
// 铜件（8.90），在「密度」那一格改掉即可。

/// 规格里能出现的分隔符：客户表里 `*`、`×`、`x`、全角 `＊`、逗号混着写是常态。
const SPEC_SEPS: [char; 9] = ['*', '×', 'x', 'X', '＊', ',', '，', ';', '；'];

/// 一条规格最多认几维（长 × 宽 × 厚）。
const SPEC_MAX_DIM: usize = 3;

/// 解析规格：`1250*250*3` → `[1250, 250, 3]`（单位统一按毫米）。
///
/// 宽进：`1250×250×3`、`1200 x 80`、`1250mm*250mm`、`1200,80` 都认；
/// 严出：**看不懂就给 `None`，绝不按 0 或按第一维猜** —— 猜错的代价是报价单上
/// 一个看着正常、实际差几倍的单价。少于两维（只有一个长度）也算看不懂。
pub fn parse_spec(s: &str) -> Option<Vec<Money>> {
    let mut out: Vec<Money> = Vec::new();
    for raw in s.split(|c: char| SPEC_SEPS.contains(&c)) {
        // 去掉客户顺手写上的单位（mm/cm/MM），数字本身必须还在
        let t = raw.trim().trim_end_matches(|c: char| {
            c.is_ascii_alphabetic() || matches!(c, 'ｍ' | 'Ｍ' | '㎜' | '㎝')
        });
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        let v = Money::parse(t).ok()?;
        if !v.is_positive() || out.len() >= SPEC_MAX_DIM {
            return None;
        }
        out.push(v);
    }
    if out.len() < 2 {
        return None;
    }
    Some(out)
}

/// 规格的展示写法：`1250×250×3`（去掉没意义的小数尾巴）。
pub fn spec_label(dims: &[Money]) -> String {
    dims.iter()
        .map(|d| {
            let s = d.fmt_plain();
            if s.contains('.') {
                s.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join("×")
}

/// 1 件折合多少计价单位。看不懂的口径 / 尺寸不够 / 密度缺失 → `None`。
///
/// 返回 4 位小数：1250×250 的板一块是 0.3125 ㎡，只留 2 位就变 0.31，
/// 80 元/㎡ 会从 25.00 变成 24.80 —— 一块板差两毛，一万块就是两千。
pub fn spec_factor(dims: &[Money], unit: &str, density: Money) -> Option<Money> {
    let u = unit.trim();
    let n = dims.len();
    match u {
        "只" | "条" | "件" | "个" | "支" | "片" | "套" | "根" => Some(Money::ONE),
        "米" | "米长" | "m" => dims[0].checked_div(1000i64).map(|v| v.round_dp(4)),
        "㎡" | "平方" | "平方米" | "平米" => dims[0]
            .checked_mul(dims[1])
            .and_then(|v| v.checked_div(1_000_000i64))
            .map(|v| v.round_dp(4)),
        "kg" | "公斤" | "千克" => {
            if n < SPEC_MAX_DIM || !density.is_positive() {
                return None;
            }
            dims[0]
                .checked_mul(dims[1])
                .and_then(|v| v.checked_mul(dims[2]))
                .and_then(|v| v.checked_div(1_000_000i64))
                .and_then(|v| v.checked_mul(density))
                .map(|v| v.round_dp(4))
        }
        _ => None,
    }
}

/// 一行「规格 → 单件价」的结果。`note` 非空表示这一行有话说（看不懂 / 口径不适用）。
#[derive(Debug)]
pub struct SpecPrice {
    pub spec: String,
    pub dims: String,
    /// 1 件折合多少计价单位
    pub factor: Money,
    /// 单件价 = 费率 × 折合量
    pub price: Money,
    pub note: String,
}

/// 按一个费率把一批规格折成单件价（就是不落库的算价器）。
///
/// 空行自动跳过（从 Excel 复制过来常带空行）；看不懂的规格不猜，原样子返回并带上说明。
/// 规格档能拿哪一维比（存库的是英文键，界面上是中文）
pub const BAND_DIMS: &[&str] = &["max", "min", "first", "area"];

/// 把「按哪一维比」翻成人话
pub fn band_dim_label(dim: &str) -> &'static str {
    match dim.trim() {
        "max" => "最长边",
        "min" => "最短边",
        "first" => "第一维",
        "area" => "面积",
        _ => "不分档",
    }
}

/// 把规格换成「按 dim 比的那一个数」，单位 cm（`area` 是 cm²）。
///
/// 规格录的是毫米，客户说的「200 以下」是厘米 —— 换算只在这一处做，
/// 免得两个地方各换一次、差十倍。
pub fn spec_measure(dims: &[Money], dim: &str) -> Option<Money> {
    if dims.is_empty() {
        return None;
    }
    let ten = Money::parse_or_zero("10");
    let cm = |m: Money| m.checked_div(ten);
    match dim.trim() {
        "max" => dims.iter().copied().max().and_then(cm),
        "min" => dims.iter().copied().min().and_then(cm),
        "first" => cm(dims[0]),
        "area" if dims.len() >= 2 => (dims[0] * dims[1]).checked_div(Money::parse_or_zero("100")),
        _ => None,
    }
}

/// 这一条价管不管这个规格：**上限含、下限不含**。
///
/// 客户说「200cm 以下（含 200）0.2 元，以上 1.5 元/米」，两句是**接**上的：
/// 正好 200 只算下面那一档。所以下限写成「严格大于」——两头都含的话，200 同时命中
/// 两条，谁赢取决于谁先建，报价会随建单顺序变化，这种不确定性最难查。
/// 上限含则保留了客户那句「含 200」。
pub fn band_ok(r: &PriceRule, dims: &[Money]) -> bool {
    let dim = r.band_dim.trim();
    if dim.is_empty() {
        return true;
    }
    let v = match spec_measure(dims, dim) {
        Some(v) => v,
        None => return false,
    };
    if !r.band_from.trim().is_empty() {
        let from = Money::parse_or_zero(r.band_from.trim());
        if v <= from {
            return false;
        }
    }
    if !r.band_to.trim().is_empty() {
        let to = Money::parse_or_zero(r.band_to.trim());
        if v > to {
            return false;
        }
    }
    true
}

pub fn spec_prices(rate: Money, unit: &str, specs: &[String], density: Money) -> Vec<SpecPrice> {
    let mut out = Vec::new();
    for raw in specs {
        let spec = raw.trim();
        if spec.is_empty() {
            continue;
        }
        let Some(dims) = parse_spec(spec) else {
            out.push(SpecPrice {
                spec: spec.to_string(),
                dims: String::new(),
                factor: Money::ZERO,
                price: Money::ZERO,
                note: "看不懂这个规格 —— 写成 1250*250*3 这样（毫米）".to_string(),
            });
            continue;
        };
        let label = spec_label(&dims);
        let Some(factor) = spec_factor(&dims, unit, density) else {
            out.push(SpecPrice {
                spec: spec.to_string(),
                dims: label,
                factor: Money::ZERO,
                price: Money::ZERO,
                note: if matches!(unit.trim(), "kg" | "公斤" | "千克") {
                    "按重量算要三维尺寸 + 密度（钢 7.85、铝 2.70、铜 8.90）".to_string()
                } else {
                    format!("「{unit}」这个口径按规格算不了（只 / 条 / 米 / ㎡ / kg 才行）")
                },
            });
            continue;
        };
        let price = factor
            .checked_mul(rate)
            .map(|v| v.round2())
            .unwrap_or(Money::ZERO);
        out.push(SpecPrice {
            spec: spec.to_string(),
            dims: label,
            factor,
            price,
            note: String::new(),
        });
    }
    out
}

/// 带规格的报价结果：比 [`UnitQuote`] 多一句「这个折算量是哪来的」。
#[derive(Debug)]
pub struct SpecQuote {
    pub item_code: String,
    pub spec: String,
    /// 规格的规范写法（`1250×250×3`）；手录系数时报空串
    pub dims: String,
    /// `"manual"` 用折算系数 / `"spec"` 按规格算出来
    pub basis: &'static str,
    pub factor: Money,
    pub stock_qty: Money,
    pub stock_unit: String,
    pub calc_qty: Money,
    pub calc_unit: String,
    pub quote: FeeQuote,
}

/// 报价：有折算系数就用系数，没有就按规格算。
///
/// 顺序是刻意的 —— 系数是人手工核过的（比如客户认的「1 只 = 0.0234 ㎡」），
/// 规格是几何算的；两者都在时**听人的**，规格只在缺系数时兜底。
pub fn quote_with_spec(
    db: &Db,
    item: &str,
    customer: &str,
    spec: &str,
    stock_qty: Money,
    calc_unit: &str,
    density: Money,
    process: &str,
    rush: bool,
) -> DbResult<SpecQuote> {
    let item = item.trim();
    let spec = spec.trim();
    let mut unit = calc_unit.trim().to_string();
    if !stock_qty.is_positive() {
        return Err(FinError::msg("数量必须大于 0").into());
    }
    if item.is_empty() {
        return Err(FinError::msg("先选物料：不同物料的单价不一样").into());
    }
    let dims_opt = parse_spec(spec);
    // 没指定口径：按规格定档，用这一档自己的口径。
    // 「200cm 以下 0.2 元/件、以上 1.5 元/米」这种价，口径本来就是档位决定的 ——
    // 让人先选口径再算，等于让他把档位判断在脑子里做一遍。
    if unit.is_empty() {
        let dims = dims_opt.as_ref().ok_or_else(|| {
            FinError::msg(format!(
                "没选口径时要用规格定档，但「{spec}」看不懂 —— 写成 1250*250*3 这样（毫米）"
            ))
        })?;
        // 这一步还不知道按哪种口径折算，也就没有「计价数量」可比 ——
        // 所以数量阶梯价只在**自己带口径**时生效（见 4.135）。
        let (r, _) = match_rule_full(db, item, customer, "", process, Some(dims), None)?
            .ok_or_else(|| {
                FinError::msg(format!(
                    "规格「{spec}」没有对应的单价（物料 {item}／客户 {}）—— 单价表里这一档还是空的",
                    if customer.trim().is_empty() {
                        "任意"
                    } else {
                        customer.trim()
                    }
                ))
            })?;
        unit = r.unit.trim().to_string();
    }
    let (factor, scale, basis, dims_txt) = match calc_unit_of(db, item, &unit)? {
        Some(cu) if cu.factor.is_positive() => (cu.factor, cu.scale, "manual", String::new()),
        _ => {
            let dims = dims_opt.as_ref().ok_or_else(|| {
                FinError::msg(format!(
                    "物料 {item} 没设「1 {item} = ? {unit}」的折算系数；填了规格也能算，但「{spec}」看不懂 —— 写成 1250*250*3 这样（毫米）"
                ))
            })?;
            let label = spec_label(dims);
            let f = spec_factor(dims, &unit, density).ok_or_else(|| {
                FinError::msg(format!(
                    "规格 {label} 按「{unit}」算不出折合量：{}",
                    if matches!(unit.as_str(), "kg" | "公斤" | "千克") {
                        "按重量算要三维尺寸，还要填密度（钢 7.85、铝 2.70、铜 8.90）".to_string()
                    } else {
                        format!("「{unit}」这个口径要 {} 个尺寸", 2)
                    }
                ))
            })?;
            (f, 4i32, "spec", label)
        }
    };
    let calc_qty = (stock_qty * factor).round_dp(scale.clamp(0, SCALE_MAX) as u32);
    if !calc_qty.is_positive() {
        return Err(FinError::msg(format!(
            "折算后只有 {calc_qty} {unit}（折合量 {factor} 太小或数量太少），请核对规格/系数"
        ))
        .into());
    }
    // 有规格就让分档价参与（含端点判定），没有就只认不分档的价。
    let mut hit = match dims_opt.as_deref() {
        Some(d) if !d.is_empty() => {
            match_rule_full(db, item, customer, &unit, process, Some(d), Some(calc_qty))?
        }
        _ => None,
    };
    if hit.is_none() {
        hit = match_rule_full(db, item, customer, &unit, process, None, Some(calc_qty))?;
    }
    let (rule, _future) = hit.ok_or_else(|| {
        FinError::msg(format!(
            "没有按「{unit}」计的单价（物料 {item}／客户 {}）——单价表里这一格还是空的",
            if customer.trim().is_empty() {
                "任意"
            } else {
                customer.trim()
            }
        ))
    })?;
    let mut q = quote_on(&rule, calc_qty, rush, &today());
    q.effective_from = rule.effective_from.clone();
    let stock_unit = stock_unit_of(db, item)?;
    Ok(SpecQuote {
        item_code: item.to_string(),
        spec: spec.to_string(),
        dims: dims_txt,
        basis,
        factor,
        stock_qty,
        stock_unit,
        calc_qty,
        calc_unit: unit,
        quote: q,
    })
}

fn item_names(db: &Db) -> DbResult<HashMap<String, String>> {
    let mut st = db
        .conn()
        .prepare("SELECT code,name FROM aux_entity WHERE kind='item'")?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    fn rule(
        name: &str,
        unit: &str,
        price: &str,
        min: &str,
        rush_fee: &str,
        rush_rate: &str,
    ) -> PriceRule {
        PriceRule {
            name: name.to_string(),
            unit: unit.to_string(),
            unit_price: m(price),
            min_charge: m(min),
            rush_fee: m(rush_fee),
            rush_rate: m(rush_rate),
            ..Default::default()
        }
    }

    #[test]
    fn 批量调价只动现在生效的那一版() {
        let db = crate::tests::mem();
        // 同一条价的老版本（2020 年 10 元）+ 现行版本（12 元）
        let mut old = rule("MB01 老价", "㎡", "10", "0", "0", "0");
        old.item_code = "MB01".to_string();
        old.effective_from = "2020-01-01".to_string();
        rule_save(&db, &mut old).unwrap();
        let mut cur = rule("MB01 现价", "㎡", "12", "0", "0", "0");
        cur.item_code = "MB01".to_string();
        cur.effective_from = "2025-01-01".to_string();
        rule_save(&db, &mut cur).unwrap();
        // 别的客户不动
        let mut other = rule("MB01 给 C02", "㎡", "15", "0", "0", "0");
        other.item_code = "MB01".to_string();
        other.customer_code = "C02".to_string();
        rule_save(&db, &mut other).unwrap();

        let spec = AdjustSpec {
            customer_code: "".to_string(),
            item_code: "MB01".to_string(),
            mode: "pct".to_string(),
            value: m("3"),
            effective_from: "2027-01-01".to_string(),
            memo: "年度涨价".to_string(),
        };
        let (rows, warns) = adjust_rows(&db, &spec).unwrap();
        assert!(warns.is_empty(), "不该有警告：{warns:?}");
        // 两格（通用 12、C02 15）各一条，都是「新建」；老版本 10 元不该被乘一遍
        assert_eq!(rows.len(), 2);
        let common = rows.iter().find(|r| r.customer_code.is_empty()).unwrap();
        assert_eq!(common.old_price.fmt_money(), "12.00");
        assert_eq!(common.new_price.fmt_money(), "12.36");
        assert_eq!(common.action, "新建");
    }

    #[test]
    fn 批量调价同一天重跑不叠加() {
        let db = crate::tests::mem();
        let mut r = rule("MB02 现价", "件", "0.50", "0", "0", "0");
        r.item_code = "MB02".to_string();
        rule_save(&db, &mut r).unwrap();

        let spec = AdjustSpec {
            mode: "pct".to_string(),
            value: m("3"),
            effective_from: "2027-01-01".to_string(),
            ..Default::default()
        };
        let a = adjust_apply(&db, &spec, "李四").unwrap();
        assert_eq!((a.created, a.updated), (1, 0));
        // 再点一次：底价仍是 0.50（不是上一轮调完的 0.515），覆盖同一天那一版
        let b = adjust_apply(&db, &spec, "李四").unwrap();
        assert_eq!((b.created, b.updated), (0, 1));
        assert_eq!(b.rows[0].old_price.fmt_money(), "0.50");
        assert_eq!(b.rows[0].new_price.fmt_dp(4), "0.5150");
        // 价目表里只有两版（0.50 与 0.5150），没有涨成第三条
        assert_eq!(rule_list(&db).unwrap().len(), 2);
    }

    #[test]
    fn 批量调价跳过算成零或负数的() {
        let db = crate::tests::mem();
        let mut r = rule("MB03 现价", "件", "0.50", "0", "0", "0");
        r.item_code = "MB03".to_string();
        rule_save(&db, &mut r).unwrap();
        let spec = AdjustSpec {
            mode: "delta".to_string(),
            value: m("-0.80"),
            effective_from: String::new(),
            ..Default::default()
        };
        let (rows, warns) = adjust_rows(&db, &spec).unwrap();
        assert!(rows.is_empty(), "调到负数的一条都不该出现");
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("跳过"), "{:?}", warns);
        // 不落库
        assert_eq!(rule_list(&db).unwrap().len(), 1);
    }

    #[test]
    fn 批量调价的参数得看得懂() {
        let db = crate::tests::mem();
        let e = adjust_rows(
            &db,
            &AdjustSpec {
                mode: "涨".to_string(),
                value: m("3"),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{e}").contains("调价方式只能是"), "{e}");
        let e = adjust_rows(
            &db,
            &AdjustSpec {
                mode: "pct".to_string(),
                value: Money::ZERO,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{e}").contains("调 0 等于没调"), "{e}");
    }

    #[test]
    fn 调价留下改前改后和是谁改的() {
        let db = crate::tests::mem();
        let mut r = rule("MB01 按平方", "㎡", "0.2", "0", "0", "0");
        r.item_code = "MB01".to_string();
        rule_save_by(&db, &mut r, "张三").unwrap();
        r.unit_price = m("0.15");
        rule_save_by(&db, &mut r, "李四").unwrap();
        let hist = changes(&db, "MB01", 10).unwrap();
        assert_eq!(hist.len(), 2, "新增一条、改价一条");
        assert_eq!(hist[0].action, "改价");
        assert_eq!(hist[0].old_price, "0.20");
        assert_eq!(hist[0].new_price, "0.15");
        assert_eq!(hist[0].who, "李四");
        assert_eq!(hist[1].action, "新增");
        assert_eq!(hist[1].old_price, "");
        assert_eq!(hist[1].new_price, "0.20");
        assert!(hist[0].at.len() >= 19, "时间要能看懂：{}", hist[0].at);
    }

    #[test]
    fn 报价单的价和价目表对不上的时候要说清楚() {
        let db = crate::tests::mem();
        let mut c = cu("MB01", "㎡", "0.0234", 4);
        calc_unit_save(&db, &mut c).unwrap();
        let mut r = rule("MB01 按平方", "㎡", "12", "0", "0", "0");
        r.item_code = "MB01".to_string();
        rule_save(&db, &mut r).unwrap();

        // 12 元/㎡ × 0.0234 = 0.2808 → 每件 0.28。报 0.28 算对上
        let good = check_quote_price(&db, "MB01", "", m("0.28"), m("100")).unwrap();
        assert_eq!(good.kind, "ok");
        assert_eq!(good.rule_unit, "㎡");
        assert_eq!(good.per_unit.fmt_money(), "0.28");
        assert_eq!(good.stock_unit, "件", "库存单位取 item_unit.base_unit");
        assert_eq!(good.factor.map(|f| f.fmt_qty()), Some("0.0234".to_string()));

        // 报 0.15 是报低了 0.13
        let low = check_quote_price(&db, "MB01", "", m("0.15"), m("100")).unwrap();
        assert_eq!(low.kind, "mismatch");
        assert_eq!(low.diff.fmt_money(), "-0.13");

        // 价目表按 kg、又没录「1 件 = 多少 kg」→ 说「对不上」，绝不硬减出一个差额
        let mut r2 = rule("MB02 按公斤", "kg", "30", "0", "0", "0");
        r2.item_code = "MB02".to_string();
        rule_save(&db, &mut r2).unwrap();
        let odd = check_quote_price(&db, "MB02", "", m("0.5"), m("1")).unwrap();
        assert_eq!(odd.kind, "unit_unknown");
        assert_eq!(odd.rule_price.fmt_money(), "30.00");

        // 价目表里根本没有这个物料
        let none = check_quote_price(&db, "MB99", "", m("1"), m("1")).unwrap();
        assert_eq!(none.kind, "no_rule");
    }

    #[test]
    fn 到点提醒只报真会生效的那几条() {
        let db = crate::tests::mem();
        let t0 = today();
        let day = |n: i64| {
            (chrono::Local::now().date_naive() + chrono::Duration::days(n))
                .format("%Y-%m-%d")
                .to_string()
        };
        let new_rule = |name: &str, price: &str, cust: &str, from: &str| {
            let mut r = rule(name, "㎡", price, "0", "0", "0");
            r.item_code = "MB01".to_string();
            r.customer_code = cust.to_string();
            r.effective_from = from.to_string();
            rule_save(&db, &mut r).unwrap();
            r
        };
        new_rule("今天起", "10", "", &t0);
        new_rule("三天后", "12", "C01", &day(3));
        // 窗口外（30 天后）：不该提醒
        new_rule("下个月", "9", "C02", &day(30));
        // 停用：不该提醒
        let mut off = new_rule("停用的", "8", "C03", &day(1));
        off.disabled = true;
        rule_save(&db, &mut off).unwrap();
        // 同一格排两版、两版都在窗口内：两个都得提醒 —— 11 月那版之后
        // 12 月那版照样会生效，跳过谁都是漏报
        new_rule("一号那版", "7", "C04", &day(2));
        new_rule("五号那版", "6", "C04", &day(5));

        let v = due_soon(&db, 7).unwrap();
        let names: Vec<&str> = v.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["今天起", "一号那版", "三天后", "五号那版"],
            "窗口内的每一版都要报，停用的不报"
        );
        assert_eq!(v[0].effective_from, t0);
        assert_eq!(v[1].effective_from, day(2));
        assert_eq!(v[2].effective_from, day(3));
        assert_eq!(v[3].effective_from, day(5));
    }

    #[test]
    fn 只改备注不动价就不留历史() {
        let db = crate::tests::mem();
        let mut r = rule("MB01 按平方", "㎡", "0.2", "0", "0", "0");
        r.item_code = "MB01".to_string();
        rule_save_by(&db, &mut r, "张三").unwrap();
        r.memo = "改个备注".to_string();
        rule_save_by(&db, &mut r, "张三").unwrap();
        assert_eq!(changes(&db, "MB01", 10).unwrap().len(), 1, "改备注不叫调价");
    }

    #[test]
    fn 停用启用和删除都留痕() {
        let db = crate::tests::mem();
        let mut r = rule("MB02 按只", "只", "0.45", "0", "0", "0");
        r.item_code = "MB02".to_string();
        rule_save_by(&db, &mut r, "张三").unwrap();
        r.disabled = true;
        rule_save_by(&db, &mut r, "张三").unwrap();
        r.disabled = false;
        rule_save_by(&db, &mut r, "张三").unwrap();
        rule_delete_by(&db, r.id, "王五").unwrap();
        let hist = changes(&db, "MB02", 10).unwrap();
        let acts: Vec<&str> = hist.iter().map(|x| x.action.as_str()).collect();
        assert_eq!(acts, vec!["删除", "启用", "停用", "新增"]);
        assert_eq!(hist[0].who, "王五");
        assert_eq!(hist[0].old_price, "0.45", "删掉也要记得它当时多少钱");
        assert!(rule_list(&db).unwrap().is_empty());
    }

    #[test]
    fn 导入单价_表头自动跳过_通配和专属都进来() {
        let db = crate::tests::mem();
        let text =
            "\u{feff}物料编码,客户编码,计价口径,单价,起步价,加急固定费,加急率%,生效日期,名称,备注
MB01,,㎡,12.00,30.00,20.00,10,,,
MB02,C01,只,0.45,,,,2026-12-01,
# 这是注释行，不该算进任何计数
";
        let r = import_rules(&db, text, "tester").unwrap();
        assert_eq!(
            (r.created, r.updated, r.skipped),
            (2, 0, 0),
            "{:?}",
            r.warnings
        );
        // 名称留空会自动拼一个，矩阵里不至于一整列空名字
        let names: Vec<String> = rule_list(&db)
            .unwrap()
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert!(names.contains(&"MB01 ㎡（通用）".to_string()), "{names:?}");
        assert!(names.contains(&"MB02 只（C01）".to_string()), "{names:?}");
        // 通配行 + MB02 行；C01 的专属价不会串到别的客户
        let mx = matrix(&db, "", "").unwrap();
        assert!(mx.rows.iter().any(|r| r.item_code == "MB01"));
    }

    #[test]
    fn 导入单价_覆盖同一格而不是再加一条() {
        let db = crate::tests::mem();
        let head = "物料编码,客户编码,计价口径,单价\n";
        let first = import_rules(&db, &format!("{head}MB01,C01,㎡,12"), "t").unwrap();
        assert_eq!((first.created, first.updated), (1, 0));
        let second = import_rules(&db, &format!("{head}MB01,C01,㎡,13.5"), "t").unwrap();
        assert_eq!(
            (second.created, second.updated),
            (0, 1),
            "同一格要覆盖，不能变两条"
        );
        let all = rule_list(&db).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].unit_price, m("13.5"));
        // 生效日不同就是另一版价，不算覆盖
        let third = import_rules(
            &db,
            &format!("{head}MB01,C01,㎡,14,,\n").replace(",,\n", "\n"),
            "t",
        )
        .unwrap();
        assert_eq!(third.created, 0);
        let dated = import_rules(
            &db,
            "物料编码,客户编码,计价口径,单价,起步价,加急固定费,加急率%,生效日期\nMB01,C01,㎡,15,,,,2027-01-01\n",
            "t",
        )
        .unwrap();
        assert_eq!(
            (dated.created, dated.updated),
            (1, 0),
            "换了生效日 = 新一版价"
        );
        assert_eq!(rule_list(&db).unwrap().len(), 2);
    }

    #[test]
    fn 导入单价_坏行只跳过那一行() {
        let db = crate::tests::mem();
        let text = "物料编码,客户编码,计价口径,单价,起步价,加急固定费,加急率%,生效日期
MB01,,㎡,abc
MB02,,,10
MB03,,㎡,10,,,,2026-3-1
MB04,,㎡,9
";
        let r = import_rules(&db, text, "t").unwrap();
        assert_eq!(r.created, 1, "只有 MB04 那行是对的");
        assert_eq!(r.skipped, 3);
        assert!(r.warnings[0].contains("第 2 行"), "{:?}", r.warnings);
        assert!(
            r.warnings[0].contains("abc"),
            "要说清是哪一格坏了：{:?}",
            r.warnings
        );
        assert!(r.warnings[1].contains("第 3 行"));
        assert!(r.warnings[1].contains("计价口径"));
        assert!(
            r.warnings[2].contains("2026-03-01"),
            "日期错要把正确写法给出来：{:?}",
            r.warnings
        );
    }

    #[test]
    fn 导入系数_覆盖同物料同口径_坏行不拦别人() {
        let db = crate::tests::mem();
        let text = "物料编码,计价口径,系数,保留位数
MB01,㎡,0.0234,2
MB02,kg,0,2
,只,1,2
MB03,米,x,2
";
        let r = import_calc_units(&db, text, "t").unwrap();
        assert_eq!((r.created, r.skipped), (1, 3), "{:?}", r.warnings);
        // 系数 0 被 calc_unit_save 拦住，x 不是数字 —— 两种都要说清
        assert!(
            r.warnings.iter().any(|w| w.contains("必须大于 0")),
            "{:?}",
            r.warnings
        );
        assert!(
            r.warnings.iter().any(|w| w.contains("不是数字")),
            "{:?}",
            r.warnings
        );
        // 再导一遍同一行 = 覆盖
        let again = import_calc_units(&db, "MB01,㎡,0.0235,2\n", "t").unwrap();
        assert_eq!((again.created, again.updated), (0, 1));
        assert_eq!(
            calc_unit_of(&db, "MB01", "㎡").unwrap().unwrap().factor,
            m("0.0235")
        );
    }

    #[test]
    fn 面积计价_最低收费_加急费() {
        // 氧化厂典型规则：18 元/㎡，起步 30 元，加急 20 元 + 10%
        let r = rule("氧化-普通", "㎡", "18", "30", "20", "10");
        // 12.5 ㎡ × 18 = 225，超过起步价，加急不打
        let q = quote(&r, m("12.5"), false);
        assert_eq!(q.base, m("225"));
        assert_eq!(q.floor_extra, Money::ZERO);
        assert_eq!(q.total, m("225"));

        // 0.5 ㎡ × 18 = 9，不足起步 30 → 补 21
        let q = quote(&r, m("0.5"), false);
        assert_eq!(q.base, m("9"));
        assert_eq!(q.floor_extra, m("21"));
        assert_eq!(q.total, m("30"), "最低收费兜底");

        // 12.5 ㎡ 加急：225 + 22.5(10%) + 20 = 267.5
        let q = quote(&r, m("12.5"), true);
        assert_eq!(q.rush_extra, m("42.5"), "加急 = 固定 20 + 10% 的 22.5");
        assert_eq!(q.total, m("267.5"));

        // 0.5 ㎡ 加急：先补到 30，再加 3 + 20 = 53（比例按补完起步价的金额算）
        let q = quote(&r, m("0.5"), true);
        assert_eq!(q.total, m("53"));
    }

    #[test]
    fn 规则匹配优先级与停用() {
        let db = crate::tests::mem();
        let mut general = rule("通用", "件", "0.5", "0", "0", "0");
        rule_save(&db, &mut general).unwrap();
        let mut by_cust = PriceRule {
            customer_code: "C001".to_string(),
            ..rule("客户价", "件", "2", "0", "0", "0")
        };
        rule_save(&db, &mut by_cust).unwrap();
        let mut by_item = PriceRule {
            item_code: "MB01".to_string(),
            ..rule("物料价", "件", "3", "0", "0", "0")
        };
        rule_save(&db, &mut by_item).unwrap();
        let mut both = PriceRule {
            item_code: "MB01".to_string(),
            customer_code: "C001".to_string(),
            ..rule("专属价", "件", "4", "0", "0", "0")
        };
        rule_save(&db, &mut both).unwrap();

        assert_eq!(
            quote_by(&db, "MB01", "C001", m("1"), false).unwrap().total,
            m("4")
        );
        assert_eq!(
            quote_by(&db, "MB01", "C002", m("1"), false).unwrap().total,
            m("3")
        );
        assert_eq!(
            quote_by(&db, "MB02", "C001", m("1"), false).unwrap().total,
            m("2")
        );
        assert_eq!(
            quote_by(&db, "MB02", "C002", m("1"), false).unwrap().total,
            m("0.5")
        );

        // 停用专属价 → 退回到「物料价」
        both.disabled = true;
        rule_save(&db, &mut both).unwrap();
        assert_eq!(
            quote_by(&db, "MB01", "C001", m("1"), false).unwrap().total,
            m("3"),
            "停用的规则不参与匹配"
        );

        // 全部删掉 → 报错而不是静默 0 元
        rule_delete(&db, general.id).unwrap();
        rule_delete(&db, by_cust.id).unwrap();
        rule_delete(&db, by_item.id).unwrap();
        rule_delete(&db, both.id).unwrap();
        let err = quote_by(&db, "MB01", "C001", m("1"), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("没有匹配的加工费规则"), "实际：{err}");
    }

    fn cu(item: &str, unit: &str, factor: &str, scale: i32) -> CalcUnit {
        CalcUnit {
            item_code: item.to_string(),
            calc_unit: unit.to_string(),
            factor: m(factor),
            scale,
            ..Default::default()
        }
    }

    #[test]
    fn 报价单一行按规格折出每件单价() {
        let db = crate::tests::mem();
        let mut r = rule("MB01 按平方", "㎡", "12", "0", "0", "0");
        r.item_code = "MB01".to_string();
        rule_save(&db, &mut r).unwrap();
        let mut c = cu("MB01", "㎡", "0.0234", 2);
        calc_unit_save(&db, &mut c).unwrap();

        // 300 件、规格 1250*250*3 → 0.3125 ㎡/件（这里用 0.0234 的系数，取的是手录值）→ 7.02 ㎡ × 12 = 84.24
        let p =
            price_for_line(&db, "MB01", "", m("300"), "1250*250*3", "", "", Money::ZERO).unwrap();
        assert_eq!(p.calc_qty.fmt_qty(), "7.02");
        assert_eq!(p.total.fmt_money(), "84.24");
        // 报价单只有「数量 × 单价」，所以摊回每件：84.24 / 300 = 0.2808
        // 单价摊到 4 位：0.2808。用 fmt_money() 会显示成 0.28，
        // 按 300 件乘回去就变 84.00，差的那 0.24 会被当成算错。
        assert_eq!(p.unit_price.fmt_dp(4), "0.2808");
        assert_eq!(p.stock_unit, "件");
        assert_eq!(p.basis, "manual");
        // 这条用的是手录系数 0.0234，不是规格几何（0.3125）—— 提示必须说清来源
        assert!(p.note.contains("手录系数"), "note={}", p.note);
        assert!(p.note.contains("1250*250*3"), "note={}", p.note);
        // 摊回去的数字必须能反过来凑出总额，否则单子上的金额是「凭空来的」
        assert_eq!(
            (p.unit_price * m("300")).round2().fmt_money(),
            "84.24",
            "单价 × 数量要还原成合计"
        );
    }

    #[test]
    fn 报价单一行按数量挑档() {
        let db = crate::tests::mem();
        // 先建大批量、再建小批量：让 id 顺序与档位方向相反，避免「碰巧对上」
        let mut big = rule("大批量", "件", "0.25", "0", "0", "0");
        big.item_code = "MB04".to_string();
        big.qty_from = "500".to_string();
        rule_save(&db, &mut big).unwrap();
        let mut small = rule("小批量", "件", "0.35", "0", "0", "0");
        small.item_code = "MB04".to_string();
        small.qty_to = "500".to_string();
        rule_save(&db, &mut small).unwrap();

        let p = price_for_line(&db, "MB04", "", m("800"), "", "", "", Money::ZERO).unwrap();
        assert_eq!(p.rule_name, "大批量");
        assert_eq!(p.unit_price.fmt_money(), "0.25");
        assert_eq!(p.total.fmt_money(), "200.00");

        let p = price_for_line(&db, "MB04", "", m("300"), "", "", "", Money::ZERO).unwrap();
        assert_eq!(p.rule_name, "小批量");
        assert_eq!(p.total.fmt_money(), "105.00");
    }

    #[test]
    fn 报价单一行按规格挑档() {
        let db = crate::tests::mem();
        // 短件按件 0.2、长料按米 1.5（按最长边 200cm 分界，上限含、下限不含）
        let mut short = rule("短件", "件", "0.2", "0", "0", "0");
        short.item_code = "MB32".to_string();
        short.band_dim = "max".to_string();
        short.band_to = "200".to_string();
        rule_save(&db, &mut short).unwrap();
        let mut long = rule("长料", "米", "1.5", "0", "0", "0");
        long.item_code = "MB32".to_string();
        long.band_dim = "max".to_string();
        long.band_from = "200".to_string();
        rule_save(&db, &mut long).unwrap();

        // 120cm：落在短件那档，口径由档位定成「件」
        let p = price_for_line(&db, "MB32", "", m("300"), "1200*80", "", "", Money::ZERO).unwrap();
        assert_eq!(p.rule_name, "短件");
        assert_eq!(p.calc_unit, "件");
        assert_eq!(p.unit_price.fmt_dp(4), "0.2000");
        assert_eq!(p.total.fmt_money(), "60.00");

        // 250cm：落在长料那档，口径变成「米」（1 件 = 2.5 米 × 1.5 = 3.75 元/件）
        let p = price_for_line(&db, "MB32", "", m("10"), "2500*100", "", "", Money::ZERO).unwrap();
        assert_eq!(p.rule_name, "长料");
        assert_eq!(p.calc_unit, "米");
        assert_eq!(p.unit_price.fmt_dp(4), "3.7500");
        assert_eq!(p.total.fmt_money(), "37.50");

        // 不给规格：分档价一律不参与 —— 必须报「没有价」，不能随手抓一条
        let e = price_for_line(&db, "MB32", "", m("10"), "", "", "", Money::ZERO).unwrap_err();
        assert!(format!("{e}").contains("价目表里没有"), "{e}");
    }

    #[test]
    fn 报价单一行没价要说清缺什么() {
        let db = crate::tests::mem();
        let e = price_for_line(&db, "MB99", "", m("10"), "", "", "", Money::ZERO).unwrap_err();
        assert!(format!("{e}").contains("价目表里没有"), "{e}");
        let e = price_for_line(&db, "MB99", "", Money::ZERO, "", "", "", Money::ZERO).unwrap_err();
        assert!(format!("{e}").contains("数量要大于 0"), "{e}");
    }

    #[test]
    fn 折算先按位数抹平再乘单价() {
        // 300 只 × 0.0234 ㎡/只 = 7.02 ㎡（不是 7.019999…）
        let c = cu("MB01", "㎡", "0.0234", 2);
        assert_eq!(convert(m("300"), &c), m("7.02"));
        // 保留 3 位时不该被提前抹平
        let c3 = cu("MB01", "㎡", "0.0234", 3);
        assert_eq!(convert(m("300"), &c3), m("7.02"));
        // 半值进位：0.5 ㎡ 口径下 2.005 → 2.01（会计惯例，不是银行家舍入）
        let c2 = cu("MB01", "㎡", "1", 2);
        assert_eq!(convert(m("2.005"), &c2), m("2.01"));
        // 1 米型材 = 1.85 kg
        let kg = cu("MB02", "kg", "1.85", 2);
        assert_eq!(convert(m("6"), &kg), m("11.1"));
        // 系数为 1 的口径（按只）就是原数
        let pc = cu("MB03", "只", "1", 0);
        assert_eq!(convert(m("300"), &pc), m("300"));
    }

    #[test]
    fn 没设系数就报错并说清缺哪一条() {
        let db = crate::tests::mem();
        let err = quote_with_units(&db, "MB09", "", m("300"), "㎡", "", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("折算系数"), "实际：{err}");
        assert!(err.contains("MB09"), "要指出是哪个物料：{err}");
    }

    #[test]
    fn 单价必须同口径才认() {
        let db = crate::tests::mem();
        let mut by_area = rule("按面积", "㎡", "12", "30", "0", "0");
        by_area.item_code = "MB01".to_string();
        rule_save(&db, &mut by_area).unwrap();
        let mut by_piece = rule("按只", "只", "0.8", "0", "0", "0");
        by_piece.item_code = "MB01".to_string();
        rule_save(&db, &mut by_piece).unwrap();
        let mut area_cu = cu("MB01", "㎡", "0.0234", 2);
        calc_unit_save(&db, &mut area_cu).unwrap();
        let mut piece_cu = cu("MB01", "只", "1", 0);
        calc_unit_save(&db, &mut piece_cu).unwrap();

        // 300 只按平方：7.02 × 12 = 84.24，超过起步价 30 不补
        let q = quote_with_units(&db, "MB01", "", m("300"), "㎡", "", false).unwrap();
        assert_eq!(q.calc_qty, m("7.02"));
        assert_eq!(q.quote.total, m("84.24"));
        // 同一批货按只算：300 × 0.8 = 240
        let q2 = quote_with_units(&db, "MB01", "", m("300"), "只", "", false).unwrap();
        assert_eq!(q2.calc_qty, m("300"));
        assert_eq!(q2.quote.total, m("240"));
        assert_eq!(q2.quote.unit, "只", "不能拿面积的价按只算");
    }

    #[test]
    fn 未来生效的价排在已生效之后() {
        let db = crate::tests::mem();
        let mut old = rule("旧价", "㎡", "10", "0", "0", "0");
        old.item_code = "MB01".to_string();
        old.effective_from = "2020-01-01".to_string();
        rule_save(&db, &mut old).unwrap();
        let mut new = rule("新价", "㎡", "12", "0", "0", "0");
        new.item_code = "MB01".to_string();
        new.effective_from = "2999-01-01".to_string();
        rule_save(&db, &mut new).unwrap();
        let mut c = cu("MB01", "㎡", "1", 2);
        calc_unit_save(&db, &mut c).unwrap();

        // 今天：先用已生效的旧价
        let q = quote_with_units(&db, "MB01", "", m("1"), "㎡", "", false).unwrap();
        assert_eq!(q.quote.total, m("10"));
        assert!(!q.quote.future);
        // 新价标明「还没生效」，而不是从表里消失
        let (r, future) = match_rule_for(&db, "MB01", "", "㎡").unwrap().unwrap();
        assert_eq!(r.unit_price, m("10"));
        assert!(!future);
    }

    #[test]
    fn 只有未来价时也报价但标明未生效() {
        let db = crate::tests::mem();
        let mut new = rule("下月新价", "㎡", "12", "0", "0", "0");
        new.item_code = "MB01".to_string();
        new.effective_from = "2999-01-01".to_string();
        rule_save(&db, &mut new).unwrap();
        let (r, future) = match_rule_for(&db, "MB01", "", "㎡").unwrap().unwrap();
        assert_eq!(r.unit_price, m("12"));
        assert!(future, "还没到生效日要能看出来");
    }

    #[test]
    fn 同精度取生效日更晚的那条() {
        let db = crate::tests::mem();
        let mut old = rule("2020 价", "㎡", "10", "0", "0", "0");
        old.item_code = "MB01".to_string();
        old.effective_from = "2020-01-01".to_string();
        rule_save(&db, &mut old).unwrap();
        let mut mid = rule("2024 价", "㎡", "11", "0", "0", "0");
        mid.item_code = "MB01".to_string();
        mid.effective_from = "2024-01-01".to_string();
        rule_save(&db, &mut mid).unwrap();
        // 两条都已生效：该用新的（11），不能还用 2020 那条
        let (r, future) = match_rule_for(&db, "MB01", "", "㎡").unwrap().unwrap();
        assert_eq!(r.unit_price, m("11"), "同精度要用生效日更晚的价");
        assert!(!future);
        // 生效日期写错格式要拦（字符串比较的前提是格式统一）
        let mut bad = rule("歪日期", "㎡", "12", "0", "0", "0");
        bad.item_code = "MB01".to_string();
        bad.effective_from = "2026-3-1".to_string();
        assert!(rule_save(&db, &mut bad).is_err(), "要写成 2026-03-01");
    }

    #[test]
    fn 矩阵按物料行_口径列摆开() {
        let db = crate::tests::mem();
        let mut a = cu("MB01", "㎡", "0.0234", 2);
        calc_unit_save(&db, &mut a).unwrap();
        let mut b = cu("MB01", "只", "1", 0);
        calc_unit_save(&db, &mut b).unwrap();
        let mut r1 = rule("C01 按平方", "㎡", "12", "30", "0", "0");
        r1.item_code = "MB01".to_string();
        r1.customer_code = "C01".to_string();
        rule_save(&db, &mut r1).unwrap();
        let mut r2 = rule("通用按只", "只", "0.8", "0", "0", "0");
        r2.item_code = "MB01".to_string();
        rule_save(&db, &mut r2).unwrap();

        let mx = matrix(&db, "C01", "").unwrap();
        assert_eq!(
            mx.units,
            vec!["只".to_string(), "㎡".to_string()],
            "口径列按常用顺序"
        );
        assert_eq!(mx.rows.len(), 1);
        let row = &mx.rows[0];
        assert_eq!(row.item_code, "MB01");
        let area = row.cells.iter().find(|c| c.unit == "㎡").unwrap();
        assert_eq!(area.unit_price, m("12"), "C01 的专属平方价");
        assert_eq!(area.factor, Some(m("0.0234")));
        assert!(area.rule_id > 0);
        let piece = row.cells.iter().find(|c| c.unit == "只").unwrap();
        assert_eq!(piece.unit_price, m("0.8"), "没有专属价时退回通用价");
        // 别的客户看不到 C01 的专属价
        let other = matrix(&db, "C02", "").unwrap();
        let area2 = other.rows[0].cells.iter().find(|c| c.unit == "㎡").unwrap();
        assert_eq!(
            area2.unit_price,
            Money::ZERO,
            "没定价的格子是 0，不是别人的价"
        );
    }

    #[test]
    fn 系数与口径的字段校验() {
        let db = crate::tests::mem();
        let mut no_item = cu("", "㎡", "1", 2);
        assert!(calc_unit_save(&db, &mut no_item).is_err(), "物料必填");
        let mut no_unit = cu("MB01", "", "1", 2);
        assert!(calc_unit_save(&db, &mut no_unit).is_err(), "口径必填");
        let mut zero = cu("MB01", "㎡", "0", 2);
        assert!(calc_unit_save(&db, &mut zero).is_err(), "系数必须大于 0");
        let mut bad_scale = cu("MB01", "㎡", "1", 9);
        assert!(calc_unit_save(&db, &mut bad_scale).is_err(), "位数 0-4");
        // 同一物料同一口径保存两次 = 改，不是新增
        let mut a = cu("MB01", "㎡", "0.02", 2);
        calc_unit_save(&db, &mut a).unwrap();
        let mut b = cu("MB01", "㎡", "0.03", 2);
        let id2 = calc_unit_save(&db, &mut b).unwrap();
        assert_eq!(a.id, id2, "同格重复录要覆盖");
        assert_eq!(calc_unit_list(&db, "MB01").unwrap().len(), 1);
        assert_eq!(
            calc_unit_of(&db, "MB01", "㎡").unwrap().unwrap().factor,
            m("0.03")
        );
        calc_unit_delete(&db, a.id).unwrap();
        assert!(calc_unit_of(&db, "MB01", "㎡").unwrap().is_none());
    }

    #[test]
    fn 规则字段校验() {
        let db = crate::tests::mem();
        let mut bad = rule("", "㎡", "18", "0", "0", "0");
        assert!(rule_save(&db, &mut bad).is_err(), "名称必填");

        let mut bad = rule("x", "㎡", "-1", "0", "0", "0");
        assert!(rule_save(&db, &mut bad).is_err(), "单价不能为负");

        let mut bad = rule("x", "㎡", "18", "0", "0", "120");
        let err = rule_save(&db, &mut bad).unwrap_err().to_string();
        assert!(err.contains("0-100"), "加急率不能超过 100%，实际：{err}");

        // 单位留空默认「件」
        let mut ok = rule("好规则", "", "10", "0", "0", "0");
        rule_save(&db, &mut ok).unwrap();
        assert_eq!(ok.unit, "件");
        assert_eq!(quote_by(&db, "", "", m("2"), false).unwrap().total, m("20"));
        // 数量 0 直接拦
        assert!(quote_by(&db, "", "", Money::ZERO, false).is_err());
    }

    // ── v1.8.89 按规格折算 ──────────────────────────────────────────────

    #[test]
    fn 规格算米长和面积() {
        let d = parse_spec("1200*80").unwrap();
        assert_eq!(spec_label(&d), "1200×80");
        assert_eq!(
            spec_factor(&d, "米", Money::ZERO),
            Some(m("1.2")),
            "1200mm = 1.2 米"
        );
        assert_eq!(
            spec_factor(&d, "㎡", Money::ZERO),
            Some(m("0.0960")),
            "1200×80mm = 0.096 ㎡"
        );

        // 板料：1250×250×3
        let d = parse_spec("1250*250*3").unwrap();
        assert_eq!(
            spec_factor(&d, "㎡", Money::ZERO),
            Some(m("0.3125")),
            "面积 0.3125 ㎡ 必须留 4 位 —— 只留 2 位就变 0.31"
        );
        assert_eq!(spec_factor(&d, "米", Money::ZERO), Some(m("1.25")));
        // 数件的口径跟规格无关
        assert_eq!(spec_factor(&d, "只", Money::ZERO), Some(Money::ONE));
    }

    #[test]
    fn 规格算重量要密度() {
        let d = parse_spec("1250*250*3").unwrap();
        // 钢 7.85 g/cm³：1250×250×3mm 的板一块 7.36 kg
        assert_eq!(
            spec_factor(&d, "kg", m("7.85")),
            Some(m("7.3594")),
            "0.9375 × 7.85"
        );
        // 铝件密度不同，同一块板轻得多
        assert_eq!(spec_factor(&d, "kg", m("2.70")), Some(m("2.5313")));
        // 没密度就不猜
        assert_eq!(spec_factor(&d, "kg", Money::ZERO), None);
        // 两维算不了重量
        let two = parse_spec("1250*250").unwrap();
        assert_eq!(spec_factor(&two, "kg", m("7.85")), None);
    }

    #[test]
    fn 规格写法宽容但看不懂就不猜() {
        for s in [
            "1250*250*3",
            "1250×250×3",
            "1250 x 250 x 3",
            "1250mm*250mm*3mm",
            "1250,250,3",
            "  1250 ＊ 250 ＊ 3  ",
        ] {
            assert_eq!(
                parse_spec(s).map(|d| spec_label(&d)),
                Some("1250×250×3".to_string()),
                "这个写法该认：{s}"
            );
        }
        // 只有一维 / 空 / 有非数字 / 有 0 —— 一律不猜
        for s in ["", "1200", "1250*abc", "1250*0", "1250*-5", "1250*250*3*4"] {
            assert!(parse_spec(s).is_none(), "这个不该认：{s}");
        }
        // 认不出的口径也不猜
        let d = parse_spec("1250*250*3").unwrap();
        assert_eq!(spec_factor(&d, "吨", Money::ZERO), None);
    }

    #[test]
    fn 按费率把一批规格折成单件价() {
        // 「2.5 元一米」：1200mm 的件 1.2 米 → 3.00 元
        let rows = spec_prices(
            m("2.5"),
            "米",
            &["1200*80".to_string(), "920*60".to_string()],
            Money::ZERO,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].dims, "1200×80");
        assert_eq!(rows[0].factor, m("1.2"));
        assert_eq!(rows[0].price, m("3"), "1.2 米 × 2.5");
        assert_eq!(rows[1].price, m("2.3"), "0.92 米 × 2.5");
        assert!(rows[0].note.is_empty());

        // 「80 元一个平方」：1250×250mm 的板 0.3125 ㎡ → 25.00 元
        let rows = spec_prices(m("80"), "㎡", &["1250*250*3".to_string()], Money::ZERO);
        assert_eq!(rows[0].factor, m("0.3125"));
        assert_eq!(rows[0].price, m("25"), "0.3125 ㎡ × 80");

        // 空行跳过、坏行原样带回来并说明
        let rows = spec_prices(
            m("80"),
            "㎡",
            &["".to_string(), "坏行".to_string(), "1200*80".to_string()],
            Money::ZERO,
        );
        assert_eq!(rows.len(), 2, "空行不占结果");
        assert!(
            rows[0].note.contains("看不懂"),
            "坏行要说明：{}",
            rows[0].note
        );
        assert_eq!(rows[1].price, m("7.68"), "0.096 ㎡ × 80");
    }

    #[test]
    fn 没录系数也能按规格报价() {
        let db = crate::tests::mem();
        let mut r = rule("按平方", "㎡", "80", "0", "0", "0");
        r.item_code = "MB01".to_string();
        rule_save(&db, &mut r).unwrap();

        // 一块 1250×250×3 的板 = 0.3125 ㎡，300 块 = 93.75 ㎡ → 7500 元
        let q = quote_with_spec(
            &db,
            "MB01",
            "",
            "1250*250*3",
            m("300"),
            "㎡",
            Money::ZERO,
            "",
            false,
        )
        .unwrap();
        assert_eq!(q.basis, "spec");
        assert_eq!(q.dims, "1250×250×3");
        assert_eq!(q.factor, m("0.3125"));
        assert_eq!(q.calc_qty, m("93.75"));
        assert_eq!(q.quote.total, m("7500"));

        // 规格看不懂就报错，别按 0 算
        let err = quote_with_spec(
            &db,
            "MB01",
            "",
            "1250",
            m("1"),
            "㎡",
            Money::ZERO,
            "",
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("看不懂"), "实际：{err}");
    }

    #[test]
    fn 手录系数优先于规格() {
        let db = crate::tests::mem();
        let mut c = cu("MB01", "㎡", "0.0234", 2);
        calc_unit_save(&db, &mut c).unwrap();
        let mut r = rule("按平方", "㎡", "12", "0", "0", "0");
        r.item_code = "MB01".to_string();
        rule_save(&db, &mut r).unwrap();

        // 系数是人核过的（客户认的 1 只 = 0.0234 ㎡），规格只是兜底
        let q = quote_with_spec(
            &db,
            "MB01",
            "",
            "1250*250*3",
            m("300"),
            "㎡",
            Money::ZERO,
            "",
            false,
        )
        .unwrap();
        assert_eq!(q.basis, "manual", "有系数就该用系数");
        assert_eq!(q.factor, m("0.0234"));
        assert_eq!(q.calc_qty, m("7.02"), "按系数折算，规格不参与");
        assert!(q.dims.is_empty());
    }
    #[test]
    fn 工艺档不同价不同() {
        let db = crate::tests::mem();
        let mut plain = rule("MB01 普通氧化", "㎡", "12", "30", "0", "0");
        plain.item_code = "MB01".to_string();
        plain.customer_code = "C01".to_string();
        plain.process = "普通氧化".to_string();
        rule_save(&db, &mut plain).unwrap();
        let mut hard = rule("MB01 硬质氧化", "㎡", "18", "50", "0", "0");
        hard.item_code = "MB01".to_string();
        hard.customer_code = "C01".to_string();
        hard.process = "硬质氧化".to_string();
        rule_save(&db, &mut hard).unwrap();

        let (r, _) = match_rule_for_process(&db, "MB01", "C01", "㎡", "硬质氧化")
            .unwrap()
            .expect("硬质氧化有价");
        assert_eq!(r.unit_price, m("18"), "选中哪一档就报哪一档的价");
        let (r, _) = match_rule_for_process(&db, "MB01", "C01", "㎡", "普通氧化")
            .unwrap()
            .expect("普通氧化有价");
        assert_eq!(r.unit_price, m("12"));

        // 没填档位就不拿档位价：客户拿普通氧化的单子来对账，报成硬质价要赔钱
        assert!(
            match_rule_for_process(&db, "MB01", "C01", "㎡", "")
                .unwrap()
                .is_none(),
            "没选档位时不能随手抓一个档位价"
        );
    }

    #[test]
    fn 档位价优先于不分档的通用价() {
        let db = crate::tests::mem();
        let mut any = rule("MB01 通用", "㎡", "12", "0", "0", "0");
        any.item_code = "MB01".to_string();
        rule_save(&db, &mut any).unwrap();
        let mut hard = rule("MB01 硬质", "㎡", "18", "0", "0", "0");
        hard.item_code = "MB01".to_string();
        hard.process = "硬质氧化".to_string();
        rule_save(&db, &mut hard).unwrap();

        let (r, _) = match_rule_for_process(&db, "MB01", "", "㎡", "硬质氧化")
            .unwrap()
            .unwrap();
        assert_eq!(r.unit_price, m("18"), "这一档有价就用这一档的");
        let (r, _) = match_rule_for_process(&db, "MB01", "", "㎡", "")
            .unwrap()
            .unwrap();
        assert_eq!(r.unit_price, m("12"), "不分档时用通用价");
        // 别的档位没有单独定价 → 退回通用价（不是报「没有规则」）
        let (r, _) = match_rule_for_process(&db, "MB01", "", "㎡", "导电氧化")
            .unwrap()
            .unwrap();
        assert_eq!(r.unit_price, m("12"), "没单独定价的档位退回通用价");
    }

    #[test]
    fn 矩阵按档显示并标出借来的通用价() {
        let db = crate::tests::mem();
        let mut hard = rule("MB01 硬质", "㎡", "18", "0", "0", "0");
        hard.item_code = "MB01".to_string();
        hard.process = "硬质氧化".to_string();
        rule_save(&db, &mut hard).unwrap();
        let mut mb02 = rule("MB02 通用", "㎡", "12", "0", "0", "0");
        mb02.item_code = "MB02".to_string();
        rule_save(&db, &mut mb02).unwrap();

        let mx = matrix(&db, "", "硬质氧化").unwrap();
        assert_eq!(mx.process, "硬质氧化");
        assert_eq!(mx.rows.len(), 2, "换档后行不变，工件还是那些工件");
        let mb01 = mx.rows.iter().find(|r| r.item_code == "MB01").unwrap();
        let c = mb01.cells.iter().find(|c| c.unit == "㎡").unwrap();
        assert_eq!(c.unit_price, m("18"));
        assert!(!c.fallback, "这一档自己定的价，不是借的");
        let mb02 = mx.rows.iter().find(|r| r.item_code == "MB02").unwrap();
        let c = mb02.cells.iter().find(|c| c.unit == "㎡").unwrap();
        assert_eq!(c.unit_price, m("12"));
        assert!(c.fallback, "这一档没定价，借的是通用价 —— 界面要说清楚");
    }

    #[test]
    fn 导入能带工艺档且同档重导算覆盖() {
        let db = crate::tests::mem();
        let text =
            "物料编码,客户编码,计价口径,单价,工艺档\nMB01,C01,㎡,18,硬质氧化\nMB01,C01,㎡,12,\n";
        let r = import_rules(&db, text, "t").unwrap();
        assert_eq!((r.created, r.updated, r.skipped), (2, 0, 0), "{:?}", r);

        let all = rule_list(&db).unwrap();
        let hard = all
            .iter()
            .find(|x| x.process == "硬质氧化")
            .expect("导进来了");
        assert_eq!(hard.unit_price, m("18"));
        assert!(
            hard.name.contains("硬质氧化"),
            "自动名带上档位：{}",
            hard.name
        );

        // 同一格（同物料/客户/口径/生效日/档位）重导 = 覆盖，不是再加一条
        let again = import_rules(
            &db,
            "物料编码,客户编码,计价口径,单价,工艺档\nMB01,C01,㎡,19,硬质氧化\n",
            "t",
        )
        .unwrap();
        assert_eq!((again.created, again.updated), (0, 1), "{:?}", again);
        assert_eq!(rule_list(&db).unwrap().len(), 2);
    }
    #[test]
    fn 导入时按列名对齐_列顺序随便排() {
        let db = crate::tests::mem();
        // 客户把表和我们的模板顺序完全不一样：单价在最前，口径在最后
        let text = "单价,单价（元）,物料编码,工艺档,计价口径\n12,12,MB01,硬质氧化,㎡\n";
        let r = import_rules(&db, text, "t").unwrap();
        assert_eq!((r.created, r.updated, r.skipped), (1, 0, 0), "{:?}", r);
        let all = rule_list(&db).unwrap();
        assert_eq!(all.len(), 1, "别把同一张表的两列当成两条价");
        assert_eq!(all[0].unit_price, m("12"));
        assert_eq!(all[0].process, "硬质氧化");
        assert_eq!(all[0].unit, "㎡");
        assert_eq!(all[0].item_code, "MB01");
    }
    #[test]
    fn 按规格分档_短件按件长料按米() {
        let db = crate::tests::mem();
        // 「200cm 以下 0.2 元/件，以上 1.5 元/米」—— 口径也跟着档位变
        // 先建「以上」这条、再建「以下」这条：id 顺序反过来，才能守住
        // 「正好 200cm 归下面那档」不是靠谁先建、而是靠区间判定本身。
        let mut long = rule("MB03 长料按米", "米", "1.5", "0", "0", "0");
        long.item_code = "MB03".to_string();
        long.band_dim = "max".to_string();
        long.band_from = "200".to_string();
        rule_save(&db, &mut long).unwrap();
        let mut short = rule("MB03 短件按件", "件", "0.2", "0", "0", "0");
        short.item_code = "MB03".to_string();
        short.band_dim = "max".to_string();
        short.band_to = "200".to_string();
        rule_save(&db, &mut short).unwrap();

        // 1200mm = 120cm ≤ 200 → 按件，0.2 元/件
        let q =
            quote_with_spec(&db, "MB03", "", "1200*80", m("300"), "", m("0"), "", false).unwrap();
        assert_eq!(q.calc_unit, "件", "短件自动按件，不用人再选口径");
        assert_eq!(q.quote.unit_price, m("0.2"));
        assert_eq!(q.quote.total, m("60"));

        // 2500mm = 250cm > 200 → 按米，1.5 元/米 × 2.5 米
        let q =
            quote_with_spec(&db, "MB03", "", "2500*100", m("10"), "", m("0"), "", false).unwrap();
        assert_eq!(q.calc_unit, "米");
        assert_eq!(q.factor, m("2.5"));
        assert_eq!(q.quote.total, m("37.5"));

        // 正好 200cm 算在「以下」那一档（客户说的「含 200」）
        let q =
            quote_with_spec(&db, "MB03", "", "2000*80", m("100"), "", m("0"), "", false).unwrap();
        assert_eq!(q.calc_unit, "件");
        assert_eq!(q.quote.total, m("20"));
    }

    #[test]
    fn 没给规格就不拿分档价() {
        let db = crate::tests::mem();
        let mut short = rule("MB03 短件按件", "件", "0.2", "0", "0", "0");
        short.item_code = "MB03".to_string();
        short.band_dim = "max".to_string();
        short.band_to = "200".to_string();
        rule_save(&db, &mut short).unwrap();

        // 先给一个折算系数，把「缺系数」这条路堵掉，剩下才看得出档位没被乱用
        let mut cu = cu("MB03", "件", "1", 0);
        calc_unit_save(&db, &mut cu).unwrap();
        // 没有规格 → 不知道是哪一档 → 宁可报「没有价」，也别拿 200cm 以下那档去乘
        let e = quote_with_units(&db, "MB03", "", m("300"), "件", "", false).unwrap_err();
        assert!(format!("{e}").contains("空的"), "实际：{e}");
    }

    #[test]
    fn 按数量分档_批发价不落到小单上() {
        let db = crate::tests::mem();
        // 500 件以下 0.35、以上 0.25 —— 同一物料同一口径，只差批量
        // 同样先建「以上」这条：正好 500 件该归下面那档，不能靠 id 顺序碰巧对上
        let mut big = rule("MB04 大批量", "件", "0.25", "0", "0", "0");
        big.item_code = "MB04".to_string();
        big.qty_from = "500".to_string();
        rule_save(&db, &mut big).unwrap();
        let mut small = rule("MB04 小批量", "件", "0.35", "0", "0", "0");
        small.item_code = "MB04".to_string();
        small.qty_to = "500".to_string();
        rule_save(&db, &mut small).unwrap();

        // 300 件 → 小批量那档（0.35）
        let q = quote_by(&db, "MB04", "", m("300"), false).unwrap();
        assert_eq!(q.unit_price, m("0.35"));
        assert_eq!(q.total, m("105"));

        // 800 件 → 大批量那档（0.25）
        let q = quote_by(&db, "MB04", "", m("800"), false).unwrap();
        assert_eq!(q.unit_price, m("0.25"));
        assert_eq!(q.total, m("200"));

        // 正好 500 件算下面那一档（下限不含），不会两条都命中
        let q = quote_by(&db, "MB04", "", m("500"), false).unwrap();
        assert_eq!(q.unit_price, m("0.35"));
        assert_eq!(q.total, m("175"));
        assert_eq!(q.rule_name, "MB04 小批量");
    }

    #[test]
    fn 数量档按折算后的计价数量判() {
        let db = crate::tests::mem();
        // 按㎡ 计价、500 ㎡ 以上便宜：300 只 = 7.02 ㎡ 走小批量，3000 只 = 70.2 ㎡ 还是小批量，
        // 20000 只 = 468 ㎡ 也还是 —— 这里用 30000 只 = 702 ㎡ 跨过 500
        let mut cu = cu("MB05", "㎡", "0.0234", 2);
        calc_unit_save(&db, &mut cu).unwrap();
        let mut small = rule("MB05 小批量", "㎡", "10", "0", "0", "0");
        small.item_code = "MB05".to_string();
        small.qty_to = "500".to_string();
        rule_save(&db, &mut small).unwrap();
        let mut big = rule("MB05 大批量", "㎡", "9", "0", "0", "0");
        big.item_code = "MB05".to_string();
        big.qty_from = "500".to_string();
        rule_save(&db, &mut big).unwrap();

        let q = quote_with_units(&db, "MB05", "", m("300"), "㎡", "", false).unwrap();
        assert_eq!(q.calc_qty, m("7.02"));
        assert_eq!(q.quote.unit_price, m("10"), "7.02 ㎡ 没到 500，走小批量");
        assert_eq!(q.quote.total, m("70.2"));

        let q = quote_with_units(&db, "MB05", "", m("30000"), "㎡", "", false).unwrap();
        assert_eq!(q.calc_qty, m("702"));
        assert_eq!(q.quote.unit_price, m("9"), "702 ㎡ 过了 500，走大批量");
        assert_eq!(q.quote.total, m("6318"));

        // 600 只 = 14.04 ㎡：库存数量过了 500、计价数量没过 —— 必须按计价数量判，
        // 否则一件小活儿被按 30000 件的大批量价算，看着便宜、实际亏在人工上
        let q = quote_with_units(&db, "MB05", "", m("600"), "㎡", "", false).unwrap();
        assert_eq!(q.calc_qty, m("14.04"));
        assert_eq!(q.quote.unit_price, m("10"), "14.04 ㎡ 没到 500，仍走小批量");
        assert_eq!(q.quote.total, m("140.4"));
    }

    #[test]
    fn 数量档不进矩阵但要列在分段表里() {
        let db = crate::tests::mem();
        let mut big = rule("MB06 大批量", "件", "0.25", "0", "0", "0");
        big.item_code = "MB06".to_string();
        big.qty_from = "500".to_string();
        rule_save(&db, &mut big).unwrap();
        // 矩阵里看不到（一格放不下「500 以下」和「以上」两条）
        let mx = matrix(&db, "", "").unwrap();
        assert!(mx.rows.iter().all(|r| r.item_code != "MB06"));
        // 分段表里看得到
        let bands = band_list(&db, "").unwrap();
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].qty_from, "500");
    }

    #[test]
    fn 数量区间的上下限要自洽() {
        let db = crate::tests::mem();
        let mut r = rule("坏数量区间", "件", "1", "0", "0", "0");
        r.qty_from = "500".to_string();
        r.qty_to = "200".to_string();
        let e = rule_save(&db, &mut r).unwrap_err().to_string();
        assert!(e.contains("永远用不上"), "{e}");

        let mut r = rule("坏数量", "件", "1", "0", "0", "0");
        r.qty_from = "abc".to_string();
        let e = rule_save(&db, &mut r).unwrap_err().to_string();
        assert!(e.contains("数量下限要填数字"), "{e}");
    }

    #[test]
    fn 分档价不进单价矩阵() {
        let db = crate::tests::mem();
        let mut plain = rule("MB01 通用", "㎡", "12", "0", "0", "0");
        plain.item_code = "MB01".to_string();
        rule_save(&db, &mut plain).unwrap();
        let mut banded = rule("MB03 长料按米", "米", "1.5", "0", "0", "0");
        banded.item_code = "MB03".to_string();
        banded.band_dim = "max".to_string();
        banded.band_from = "200".to_string();
        rule_save(&db, &mut banded).unwrap();

        let mx = matrix(&db, "", "").unwrap();
        assert!(
            mx.rows.iter().all(|r| r.item_code != "MB03"),
            "分档价不该在矩阵里冒充一个通价"
        );
        assert_eq!(
            band_list(&db, "").unwrap().len(),
            1,
            "但它要在「按规格分段的价」里看得到"
        );
    }

    #[test]
    fn 规格档的区间和维度要自洽() {
        let db = crate::tests::mem();
        let mut r = rule("填反了", "件", "1", "0", "0", "0");
        r.band_dim = "max".to_string();
        r.band_from = "300".to_string();
        r.band_to = "200".to_string();
        let e = rule_save(&db, &mut r).unwrap_err();
        assert!(format!("{e}").contains("永远用不上"), "实际：{e}");

        let mut r2 = rule("没填维度", "件", "1", "0", "0", "0");
        r2.band_to = "200".to_string();
        let e = rule_save(&db, &mut r2).unwrap_err();
        assert!(format!("{e}").contains("按哪一维比"), "实际：{e}");

        let mut r3 = rule("维度写错", "件", "1", "0", "0", "0");
        r3.band_dim = "对角线".to_string();
        let e = rule_save(&db, &mut r3).unwrap_err();
        assert!(format!("{e}").contains("最长边"), "实际：{e}");
    }

    #[test]
    fn 导出只给每格当前生效的那一版并排除停用() {
        let db = crate::tests::mem();
        let mut old = rule("MB01 老价", "㎡", "10", "0", "0", "0");
        old.item_code = "MB01".to_string();
        old.effective_from = "2020-01-01".to_string();
        rule_save(&db, &mut old).unwrap();
        let mut cur = rule("MB01 现价", "㎡", "12", "0", "0", "0");
        cur.item_code = "MB01".to_string();
        cur.effective_from = "2025-01-01".to_string();
        rule_save(&db, &mut cur).unwrap();
        let mut dead = rule("MB01 停用", "㎡", "99", "0", "0", "0");
        dead.item_code = "MB01".to_string();
        dead.disabled = true;
        rule_save(&db, &mut dead).unwrap();
        // 另一个物料、整格都停用：这一格必须整个不出现（否则「删掉停用过滤」的变异看不到）
        let mut dead2 = rule("MB05 停用", "只", "9.99", "0", "0", "0");
        dead2.item_code = "MB05".to_string();
        dead2.disabled = true;
        rule_save(&db, &mut dead2).unwrap();

        let rows = export_rules(&db, "").unwrap();
        assert_eq!(rows.len(), 1, "历史与停用的都不该进导出：{rows:?}");
        assert_eq!(rows[0].unit_price.fmt_money(), "12.00");
        assert_eq!(rows[0].effective_from, "2025-01-01", "生效日期要原样保留");
    }

    #[test]
    fn 导出按客户过滤且只有未来价时也带上() {
        let db = crate::tests::mem();
        let mut c1 = rule("给C01", "只", "0.45", "0", "0", "0");
        c1.item_code = "MB02".to_string();
        c1.customer_code = "C01".to_string();
        c1.effective_from = "2027-01-01".to_string(); // 只有未来版也要报（这是下一个要用的价）
        rule_save(&db, &mut c1).unwrap();
        let mut c2 = rule("给C02", "只", "0.50", "0", "0", "0");
        c2.item_code = "MB02".to_string();
        c2.customer_code = "C02".to_string();
        rule_save(&db, &mut c2).unwrap();

        let rows = export_rules(&db, "C01").unwrap();
        assert_eq!(rows.len(), 1, "只导出 C01 自己的价");
        assert_eq!(rows[0].customer_code, "C01");
        assert_eq!(rows[0].effective_from, "2027-01-01");
    }

    #[test]
    fn 导出包含规格档和数量档的价() {
        let db = crate::tests::mem();
        let mut banded = rule("短件按件", "件", "0.20", "0", "0", "0");
        banded.item_code = "MB03".to_string();
        banded.band_dim = "max".to_string();
        banded.band_to = "200".to_string();
        rule_save(&db, &mut banded).unwrap();
        let mut qty = rule("小批量", "件", "0.35", "0", "0", "0");
        qty.item_code = "MB04".to_string();
        qty.qty_to = "500".to_string();
        rule_save(&db, &mut qty).unwrap();

        let rows = export_rules(&db, "").unwrap();
        assert_eq!(rows.len(), 2);
        let banded_row = rows.iter().find(|r| r.item_code == "MB03").unwrap();
        assert_eq!(banded_row.band_dim, "max");
        assert_eq!(banded_row.band_to, "200");
        let qty_row = rows.iter().find(|r| r.item_code == "MB04").unwrap();
        assert_eq!(qty_row.qty_to, "500");
    }
}
