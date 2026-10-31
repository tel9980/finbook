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
}

const COLS: &str =
    "id,name,item_code,customer_code,unit,unit_price,min_charge,rush_fee,rush_rate,disabled,memo";

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
    }
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
    let tx = db.write_tx()?;
    let id = if r.id > 0 {
        let n = tx.execute(
            "UPDATE price_rule SET name=?2,item_code=?3,customer_code=?4,unit=?5,
             unit_price=?6,min_charge=?7,rush_fee=?8,rush_rate=?9,disabled=?10,memo=?11
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
                r.memo
            ],
        )?;
        if n == 0 {
            return Err(FinError::msg(format!("规则 {0} 不存在", r.id)).into());
        }
        r.id
    } else {
        tx.execute(
            "INSERT INTO price_rule(name,item_code,customer_code,unit,unit_price,
             min_charge,rush_fee,rush_rate,disabled,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
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
                r.memo
            ],
        )?;
        tx.last_insert_rowid()
    };
    tx.commit()?;
    r.id = id;
    r.name = name;
    r.unit = unit;
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
pub fn match_rule(db: &Db, item: &str, customer: &str) -> DbResult<Option<PriceRule>> {
    let item = item.trim();
    let customer = customer.trim();
    let mut best: Option<(u8, PriceRule)> = None;
    for r in rule_list(db)? {
        if r.disabled {
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
        let take = match &best {
            None => true,
            Some((r0, _)) => rank < *r0,
        };
        if take {
            best = Some((rank, r));
        }
    }
    Ok(best.map(|(_, r)| r))
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
    Ok(quote(&rule, qty, rush))
}

/// 规则一览（id → 名称），给前端下拉用
pub fn rule_names(db: &Db) -> DbResult<HashMap<i64, String>> {
    Ok(rule_list(db)?.into_iter().map(|r| (r.id, r.name)).collect())
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
