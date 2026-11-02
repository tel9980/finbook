//! 月度经营简报：一页纸，零新增录入。
//!
//! 小厂老板每月真正要看的就十来个数字：赚了多少、钱在哪、干了多少。
//! 这些数本来都躺在账里，只是分散在利润表、资产负债表、应收应付、生产订单、
//! 返工单五六个页面上，凑一遍要半小时。这个模块把它们拼成一张表。
//!
//! # 三条设计原则
//!
//! 1. **不新增任何录入字段**。所有数从既有表算出来，谁都不用为了看简报多填东西。
//! 2. **口径只有一处**。收入/成本/费用/税金/利润直接取
//!    [`crate::advanced::financial_totals`]（与利润表同一份实现，含「小厂不做成本核算」
//!    时把 5101 等成本类科目余额计入成本的处理）。简报自己再写一遍口径，
//!    迟早会和利润表对不上——而这种不一致最难解释。
//! 3. **同比要能显示「—」**。上个月没数据（新账套、刚开账）时不能崩，
//!    也不能显示成 0%（那会被读成「和上月持平」）。
//!
//! # 为什么不按槽位/面积摊制造费用
//!
//! 氧化厂最细可以按「槽位占用小时」「挂具面积」摊水电与槽液，但那样得先给每条
//! 工艺路线补录槽位数与面积两个属性。一个月几十张单的小厂，录入成本远大于分摊
//! 精度带来的收益。按报工工时/产量摊已经够用（见 [`crate::manufacturing::OverheadBase`]）。

use fincore::{Money, Period};

use crate::{Db, DbResult};

/// 简报的一行。
///
/// 界面直接按 `group` 分组、按数组顺序渲染，不用自己记字段名——
/// 加一行只改这里，两个界面（桌面端与网页端）跟着出，不会一边有一边没有。
#[derive(Clone, Debug, serde::Serialize)]
pub struct BriefRow {
    /// 稳定标识（`revenue` / `cash` …），测试与前端按它取值
    pub key: &'static str,
    /// 中文名
    pub label: &'static str,
    /// 分组：赚了多少 / 钱在哪 / 干了多少
    pub group: &'static str,
    /// 本期数
    pub value: Money,
    /// 上期数（上期期间不存在时为空）
    pub prev: Option<Money>,
    /// 增减额（`value - prev`）
    pub delta: Option<Money>,
    /// 增减百分比（字符串，两位小数）。上期为 0 或负数时为空——
    /// 从 -100 涨到 100 算「涨 200%」没有意义，宁可不显示。
    pub delta_pct: Option<String>,
    /// 这个数变大是不是好事。利润、现金变大是好事；成本、应收变大不是。
    /// 界面据此决定标红还是标绿——不判断方向就会把「应收暴涨」标成绿色。
    pub better_high: bool,
    /// 是不是数量（用 `fmt_qty` 显示，不带小数位与货币符号）
    pub is_qty: bool,
}

/// 一个月的一页纸。
#[derive(Clone, Debug, serde::Serialize)]
pub struct MonthBrief {
    pub period: Period,
    /// 上期期间（1 月时是上一年 12 月）
    pub prev_period: Period,
    /// 上期是否有数据。没有时所有 `prev/delta` 都是空，界面显示「—」
    pub has_prev: bool,
    pub rows: Vec<BriefRow>,
}

/// 一期的原始数字（内部用，拼成 [`BriefRow`] 之前的中间态）。
#[derive(Clone, Debug, Default)]
struct Nums {
    revenue: Money,
    cost: Money,
    expense: Money,
    tax: Money,
    profit: Money,
    cash: Money,
    receivable: Money,
    payable: Money,
    inventory: Money,
    finished_qty: Money,
    rework_cost: Money,
}

/// 取一期的数字。
///
/// `has_activity` 用来判断「这期到底有没有账」：一张凭证都没有时，
/// 把上月数摆出来做同比毫无意义（新账套第一个月就是这种情形）。
fn nums_of(db: &Db, period: Period) -> DbResult<(Nums, bool)> {
    use crate::balances::{BalanceQuery, BalanceSnapshot};

    // 损益：与利润表同一份实现（含未结转成本类科目余额计入成本的处理）
    let t = crate::advanced::financial_totals(db, period, period, None)?;

    // 资产负债类时点数。用含结转的快照——结转只影响损益与权益，
    // 不影响资产/负债（与 fin_totals_impl 里的注释同一个理由）。
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(period))?;
    let b = |code: &str| snap.for_account(code, None).end();
    // 货币资金 = 库存现金 + 银行存款 + 其他货币资金
    let cash = b("1001") + b("1002") + b("1012");
    // 应收/应付取正数：应收是借方余额（正），应付是贷方余额（负），取反才好看
    let receivable = b("1122");
    let payable = b("2202").negated();
    // 存货：原材料 + 库存商品 + 发出商品 + 周转材料（与速动资产口径一致）
    let inventory = b("1403") + b("1405") + b("1406") + b("1411");

    // 本月完工数量：生产订单上的累计完工量按单汇总。
    // 只数本期的单——跨月完工的订单，完工量记在它自己那一期，
    // 硬按「今天」拆会把上月的活算进这个月。
    let mut finished_qty = Money::ZERO;
    for o in crate::scm::prod_list(db, period, None)? {
        finished_qty += o.completed_qty;
    }

    // 返工成本：返工单是独立成本单（不记回原单），这里取本期合计
    let mut rework_cost = Money::ZERO;
    for r in crate::rework::report(db, period, period)? {
        rework_cost += r.total;
    }

    // 「这期有没有账」：看有没有凭证。比看金额更可靠——一个只有
    // 结转分录的空月份，金额全是 0 但确实结过账。
    let vouchers: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM voucher WHERE period = ?1",
            [period.ymm()],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let nums = Nums {
        revenue: t.revenue,
        cost: t.cost,
        expense: t.expense,
        tax: t.tax,
        profit: t.net_profit,
        cash,
        receivable,
        payable,
        inventory,
        finished_qty,
        rework_cost,
    };
    Ok((nums, vouchers > 0))
}

/// 生成一期的一页纸。
pub fn month_brief(db: &Db, period: Period) -> DbResult<MonthBrief> {
    let (cur, _) = nums_of(db, period)?;
    let prev_period = period.prev();
    let (prev, prev_has) = nums_of(db, prev_period)?;
    let has_prev = prev_has;

    let mut rows = Vec::with_capacity(11);
    let mut push = |key: &'static str,
                    label: &'static str,
                    group: &'static str,
                    value: Money,
                    prev_value: Money,
                    better_high: bool,
                    is_qty: bool| {
        let (p, d, pct) = if has_prev {
            // 上期为 0 或负数时不给百分比：从 -100 到 100 算「涨 200%」是误导。
            let pct = if prev_value.is_positive() {
                (value - prev_value)
                    .checked_mul(100i64)
                    .and_then(|x| x.checked_div(prev_value))
                    .map(|x| x.fmt_plain())
            } else {
                None
            };
            (Some(prev_value), Some(value - prev_value), pct)
        } else {
            (None, None, None)
        };
        rows.push(BriefRow {
            key,
            label,
            group,
            value,
            prev: p,
            delta: d,
            delta_pct: pct,
            better_high,
            is_qty,
        });
    };

    // —— 赚了多少 ——
    push(
        "revenue",
        "营业收入",
        "赚了多少",
        cur.revenue,
        prev.revenue,
        true,
        false,
    );
    push(
        "cost",
        "营业成本",
        "赚了多少",
        cur.cost,
        prev.cost,
        false,
        false,
    );
    push(
        "expense",
        "期间费用",
        "赚了多少",
        cur.expense,
        prev.expense,
        false,
        false,
    );
    push("tax", "税金", "赚了多少", cur.tax, prev.tax, false, false);
    push(
        "profit",
        "净利润",
        "赚了多少",
        cur.profit,
        prev.profit,
        true,
        false,
    );

    // —— 钱在哪 ——
    push(
        "cash",
        "货币资金",
        "钱在哪",
        cur.cash,
        prev.cash,
        true,
        false,
    );
    push(
        "receivable",
        "应收账款",
        "钱在哪",
        cur.receivable,
        prev.receivable,
        false,
        false,
    );
    push(
        "payable",
        "应付账款",
        "钱在哪",
        cur.payable,
        prev.payable,
        false,
        false,
    );
    push(
        "inventory",
        "存货",
        "钱在哪",
        cur.inventory,
        prev.inventory,
        true,
        false,
    );

    // —— 干了多少 ——
    push(
        "finished_qty",
        "本月完工数量",
        "干了多少",
        cur.finished_qty,
        prev.finished_qty,
        true,
        true,
    );
    push(
        "rework_cost",
        "返工成本",
        "干了多少",
        cur.rework_cost,
        prev.rework_cost,
        false,
        false,
    );

    Ok(MonthBrief {
        period,
        prev_period,
        has_prev,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use crate::vouchers;
    use chrono::NaiveDate;
    use fincore::{Entry, Voucher};

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    /// 记一张借贷配对的凭证并记账。
    ///
    /// 分录写成 `(科目, 借, 贷, 客户)`：应收类科目强制核算客户，
    /// 不填会被校验挡下来（`第 1 行科目 112201 核算客户，必须填写客户`）。
    fn post(db: &Db, period: Period, day: u32, lines: Vec<(&str, &str, &str, &str)>) {
        let date = NaiveDate::from_ymd_opt(period.year(), period.month(), day).unwrap();
        let mut v = Voucher::new(
            period,
            date,
            "记",
            crate::vouchers::next_no(db, period, "记").unwrap(),
        );
        for (i, (code, debit, credit, customer)) in lines.into_iter().enumerate() {
            let mut e = Entry::new(i as i32 + 1, code, "简报测试");
            e.debit = m(debit);
            e.credit = m(credit);
            if !customer.is_empty() {
                e.aux.customer = Some(customer.to_string());
            }
            v.entries.push(e);
        }
        let id = vouchers::save(db, &mut v).unwrap();
        vouchers::post(db, id, "t").unwrap();
    }

    fn row<'a>(b: &'a MonthBrief, key: &str) -> &'a BriefRow {
        b.rows.iter().find(|r| r.key == key).expect("没有这一行")
    }

    #[test]
    fn 简报把一个月拆成十一个数并带上月对比() {
        let db = mem();
        let jan = Period::from_ymm(202601);
        let feb = Period::from_ymm(202602);

        // 1 月：收 1000、成本 400、管理费用 100、现金进 1000、应收 600
        post(
            &db,
            jan,
            10,
            vec![("1001", "1000", "0", ""), ("600101", "0", "1000", "")],
        );
        post(
            &db,
            jan,
            12,
            vec![("6401", "400", "0", ""), ("1001", "0", "400", "")],
        );
        post(
            &db,
            jan,
            15,
            vec![("660201", "100", "0", ""), ("1001", "0", "100", "")],
        );
        // 2 月：收 2000（翻倍），成本 500，没有费用
        post(
            &db,
            feb,
            10,
            vec![("112201", "2000", "0", "C01"), ("600101", "0", "2000", "")],
        );
        post(
            &db,
            feb,
            12,
            vec![("6401", "500", "0", ""), ("1001", "0", "500", "")],
        );

        let b = month_brief(&db, feb).unwrap();
        assert_eq!(b.rows.len(), 11);
        assert!(b.has_prev, "1 月有凭证，2 月应该能比");

        // 损益：与利润表同口径
        assert_eq!(row(&b, "revenue").value, m("2000"));
        assert_eq!(row(&b, "revenue").prev, Some(m("1000")));
        assert_eq!(row(&b, "revenue").delta, Some(m("1000")));
        assert_eq!(row(&b, "revenue").delta_pct.as_deref(), Some("100.00"));
        assert_eq!(row(&b, "cost").value, m("500"));
        assert_eq!(row(&b, "expense").value, m("0"));
        assert_eq!(row(&b, "profit").value, m("1500"));

        // 钱在哪：期末时点数
        // 现金：1 月 1000-400-100=500，2 月再 -500 → 0
        assert_eq!(row(&b, "cash").value, m("0"));
        // 应收：2 月挂 2000
        assert_eq!(row(&b, "receivable").value, m("2000"));
        assert_eq!(row(&b, "receivable").prev, Some(m("0")));
        // 上月是 0，百分比必须为空（不能显示 0%，那是「持平」的意思）
        assert_eq!(row(&b, "receivable").delta_pct, None);

        // 方向标记：应收涨了不是好事
        assert!(!row(&b, "receivable").better_high);
        assert!(row(&b, "profit").better_high);
    }

    #[test]
    fn 上个月没有账时同比显示空而不是零() {
        let db = mem();
        let mar = Period::from_ymm(202603);
        post(
            &db,
            mar,
            5,
            vec![("1001", "800", "0", ""), ("600101", "0", "800", "")],
        );

        let b = month_brief(&db, mar).unwrap();
        assert!(!b.has_prev, "2 月一张凭证都没有");
        assert_eq!(b.prev_period, Period::from_ymm(202602));
        for r in &b.rows {
            assert!(r.prev.is_none(), "{} 不该有上月数", r.key);
            assert!(r.delta.is_none(), "{} 不该有增减", r.key);
            assert!(r.delta_pct.is_none(), "{} 不该有百分比", r.key);
        }
        // 本期数照出
        assert_eq!(row(&b, "revenue").value, m("800"));
    }

    #[test]
    fn 一月的上一期是去年十二月() {
        let db = mem();
        let b = month_brief(&db, Period::from_ymm(202601)).unwrap();
        assert_eq!(b.prev_period, Period::from_ymm(202512));
        assert!(!b.has_prev);
    }
}
