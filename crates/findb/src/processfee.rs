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
                    rush_rate,disabled,memo,effective_from";

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

/// 规则列表
pub fn rule_list(db: &Db) -> DbResult<Vec<PriceRule>> {
    let sql = format!("SELECT {COLS} FROM price_rule ORDER BY id");
    let mut st = db.conn().prepare(&sql)?;
    let out = st.query_map([], map_rule)?.collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// 保存规则（id=0 新建）
pub fn rule_save(db: &Db, r: &mut PriceRule) -> DbResult<i64> {
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
    let tx = db.write_tx()?;
    let id = if r.id > 0 {
        let n = tx.execute(
            "UPDATE price_rule SET name=?2,item_code=?3,customer_code=?4,unit=?5,
             unit_price=?6,min_charge=?7,rush_fee=?8,rush_rate=?9,disabled=?10,memo=?11,
             effective_from=?12
             WHERE id=?1",
            rusqlite::params![
                r.id,
                name,
                r.item_code.trim(),
                r.customer_code.trim(),
                unit,
                crate::money_param(r.unit_price),
                crate::money_param(r.min_charge),
                crate::money_param(r.rush_fee),
                crate::exact_param(r.rush_rate),
                r.disabled as i64,
                r.memo,
                effective_from
            ],
        )?;
        if n == 0 {
            return Err(FinError::msg(format!("规则 {0} 不存在", r.id)).into());
        }
        r.id
    } else {
        tx.execute(
            "INSERT INTO price_rule(name,item_code,customer_code,unit,unit_price,
             min_charge,rush_fee,rush_rate,disabled,memo,effective_from)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            rusqlite::params![
                name,
                r.item_code.trim(),
                r.customer_code.trim(),
                unit,
                crate::money_param(r.unit_price),
                crate::money_param(r.min_charge),
                crate::money_param(r.rush_fee),
                crate::exact_param(r.rush_rate),
                r.disabled as i64,
                r.memo,
                effective_from
            ],
        )?;
        tx.last_insert_rowid()
    };
    tx.commit()?;
    r.id = id;
    r.name = name;
    r.unit = unit;
    r.effective_from = effective_from;
    Ok(id)
}

/// 删除规则
pub fn rule_delete(db: &Db, id: i64) -> DbResult<()> {
    let n = db
        .conn()
        .execute("DELETE FROM price_rule WHERE id=?1", [id])?;
    if n == 0 {
        return Err(FinError::msg(format!("规则 {id} 不存在")).into());
    }
    Ok(())
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
    let item = item.trim();
    let customer = customer.trim();
    let want_unit = calc_unit.trim();
    let today = today();
    let mut cands: Vec<(u8, PriceRule)> = Vec::new();
    for r in rule_list(db)? {
        if r.disabled {
            continue;
        }
        if !want_unit.is_empty() && r.unit.trim() != want_unit {
            continue;
        }
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
        cands.push((rank, r));
    }
    // 精度优先，其次「现在该用哪一条」：已生效的价里挑生效日最晚的。
    // 不这么做的话，提前录的下一版价永远排在后面 —— 到了生效日也不会自动换过来，
    // 老板还得记得回来删旧价，那就等于没这个功能。
    for rank in 0..=3u8 {
        let pool: Vec<&PriceRule> = cands
            .iter()
            .filter(|(k, _)| *k == rank)
            .map(|(_, r)| r)
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
    Ok(None)
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
    let rule = match_rule(db, item, customer)?.ok_or_else(|| {
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
    let (rule, _future) = match_rule_for(db, item, customer, unit)?.ok_or_else(|| {
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
pub fn matrix(db: &Db, customer: &str) -> DbResult<PriceMatrix> {
    let customer = customer.trim();
    let today = today();
    let units_all = calc_unit_list(db, "")?;
    let rules = rule_list(db)?;

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
            cells: build_cells("", customer, &unit_set, &units_all, &rules, &today),
        });
    }
    for code in codes {
        rows.push(MatrixRow {
            item_name: names.get(&code).cloned().unwrap_or_else(|| code.clone()),
            stock_unit: stock_unit_of(db, &code)?,
            cells: build_cells(&code, customer, &unit_set, &units_all, &rules, &today),
            item_code: code,
        });
    }
    Ok(PriceMatrix {
        customer: customer.to_string(),
        units: unit_set,
        rows,
    })
}

fn build_cells(
    item: &str,
    customer: &str,
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
            let pick = |want_cust: &str| -> Vec<&PriceRule> {
                rules
                    .iter()
                    .filter(|r| {
                        !r.disabled
                            && r.item_code == item
                            && r.unit.trim() == u.as_str()
                            && r.customer_code.trim() == want_cust
                    })
                    .collect()
            };
            let rule = if customer.is_empty() {
                best_for_cell(pick(""), today)
            } else {
                best_for_cell(pick(customer), today).or_else(|| best_for_cell(pick(""), today))
            };
            MatrixCell {
                unit: u.clone(),
                rule_id: rule.map(|r| r.id).unwrap_or(0),
                unit_price: rule.map(|r| r.unit_price).unwrap_or(Money::ZERO),
                min_charge: rule.map(|r| r.min_charge).unwrap_or(Money::ZERO),
                effective_from: rule.map(|r| r.effective_from.clone()).unwrap_or_default(),
                future: rule.map(|r| is_future(r, today)).unwrap_or(false),
                factor: cu.map(|c| c.factor),
                scale: cu.map(|c| c.scale).unwrap_or(2),
            }
        })
        .collect()
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
        let err = quote_with_units(&db, "MB09", "", m("300"), "㎡", false)
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
        let q = quote_with_units(&db, "MB01", "", m("300"), "㎡", false).unwrap();
        assert_eq!(q.calc_qty, m("7.02"));
        assert_eq!(q.quote.total, m("84.24"));
        // 同一批货按只算：300 × 0.8 = 240
        let q2 = quote_with_units(&db, "MB01", "", m("300"), "只", false).unwrap();
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
        let q = quote_with_units(&db, "MB01", "", m("1"), "㎡", false).unwrap();
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

        let mx = matrix(&db, "C01").unwrap();
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
        let other = matrix(&db, "C02").unwrap();
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
}
