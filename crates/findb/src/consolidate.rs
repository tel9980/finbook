//! 合并报表（跨账套汇总）
//!
//! ## 为什么放在 findb 而不是 Web handler
//!
//! 这段聚合原先只写在 `crates/finweb/src/handlers/books.rs` 里，于是「合并报表」
//! 成了 Web 独有的功能——桌面端明明是同一套账套文件、同一套科目表，却打不开。
//! 口径是业务规则，不是 HTTP 细节，所以搬到这里：两端调同一个函数，
//! 谁也不可能悄悄算成两个数。
//!
//! ## 口径（v1）
//!
//! - 只取**叶子科目**的**期末余额**（`BalanceSnapshot::for_account(...).end()`），
//!   余额为零的科目不占行——否则几十个空科目会把表撑成没人看的长度。
//! - 行 = 各账套科目编码的**并集**，按编码排序（`BTreeSet`），这样同一个科目在
//!   各账套列里能对齐。
//! - **不做内部往来抵销、不做账套间科目映射**（留待 v2）。这不是偷懒：抵销需要
//!   知道「哪笔内部交易对应哪笔」，而各账套目前没有统一的往来方标识，硬做会
//!   抵错。所以 v1 明说「只是加总」，界面上也必须写清，不能让使用者以为是合并
//!   报表准则口径。
//!
//! ## 为什么不是一个 `Db` 参数
//!
//! 合并天然跨多个账套文件，每个账套是独立的 `Db`。所以这里收 `&[ConsolSource]`，
//! 由调用方（Web 的多账套 / 桌面的多选文件）自己把账套打开。

use std::collections::{BTreeMap, BTreeSet};

use fincore::{Money, Period};

use crate::balances::{BalanceQuery, BalanceSnapshot};
use crate::{accounts, Db, DbResult};

/// 参与合并的一个账套
pub struct ConsolSource<'a> {
    /// 账套标识（Web 端是 b1/b2 这类 key，桌面端是文件名）
    pub key: String,
    /// 公司名（取自账套参数）
    pub company: String,
    /// 已打开的账套
    pub db: &'a Db,
}

/// 合并报表的一行：一个科目在各账套的期末余额与合计
#[derive(Clone, Debug)]
pub struct ConsolRow {
    pub account_code: String,
    pub account_name: String,
    /// 与 `sources` 顺序一一对应；缺该科目记 0
    pub values: Vec<Money>,
    pub total: Money,
}

/// 合并报表结果
#[derive(Clone, Debug)]
pub struct ConsolReport {
    pub period: Period,
    /// (key, company)，与 `ConsolRow::values` 的顺序一一对应
    pub books: Vec<(String, String)>,
    pub rows: Vec<ConsolRow>,
}

/// 汇总口径说明。两端都用这一句，避免文案各写一份又慢慢走偏。
pub const CONSOL_NOTE: &str =
    "汇总口径：各账套仅已记账余额（H-3）；v1 为跨账套汇总，内部往来抵销留待 v2";

/// 单个账套的「叶子科目 → (名称, 期末余额)」，零余额不占行
fn book_map(db: &Db, period: Period) -> DbResult<BTreeMap<String, (String, Money)>> {
    let chart = accounts::chart(db)?;
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(period))?;
    let mut map: BTreeMap<String, (String, Money)> = BTreeMap::new();
    for a in chart.all() {
        if !chart.is_leaf(&a.code) {
            continue;
        }
        let end = snap.for_account(&a.code, None).end();
        if end.is_zero() {
            continue;
        }
        map.insert(a.code.clone(), (a.name.clone(), end));
    }
    Ok(map)
}

/// 跨账套汇总。`sources` 为空时返回空报表（不报错——「没选账套」是正常状态）。
pub fn preview(sources: &[ConsolSource<'_>], period: Period) -> DbResult<ConsolReport> {
    let mut per_book: Vec<(String, String, BTreeMap<String, (String, Money)>)> = Vec::new();
    let mut books = Vec::new();
    for s in sources {
        let map = book_map(s.db, period)?;
        books.push((s.key.clone(), s.company.clone()));
        per_book.push((s.key.clone(), s.company.clone(), map));
    }

    // 科目并集（BTreeSet 天然按编码排序，各账套列才对得齐）
    let mut codes: BTreeSet<String> = BTreeSet::new();
    for (_, _, m) in &per_book {
        for c in m.keys() {
            codes.insert(c.clone());
        }
    }

    let mut rows = Vec::with_capacity(codes.len());
    for code in &codes {
        let mut values = Vec::with_capacity(per_book.len());
        let mut total = Money::ZERO;
        let mut name = String::new();
        for (_, _, m) in &per_book {
            let v = m
                .get(code)
                .map(|(n, v)| {
                    if name.is_empty() {
                        name = n.clone();
                    }
                    *v
                })
                .unwrap_or(Money::ZERO);
            values.push(v);
            total += v;
        }
        rows.push(ConsolRow {
            account_code: code.clone(),
            account_name: name,
            values,
            total,
        });
    }

    Ok(ConsolReport {
        period,
        books,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use fincore::voucher::{Entry, Voucher};

    /// 造一张已记账凭证：借 debit / 贷 credit，金额 amount
    ///
    /// 只用**不挂辅助核算**的科目（1001 库存现金 / 4001 实收资本 / 600101 产品销售收入），
    /// 免得为了造数还要先建银行账户、客户档案——那会把「合并口径」的测试变成
    /// 「辅助核算校验」的测试。
    fn two_line(db: &Db, period: Period, no: i32, debit: &str, credit: &str, amount: &str) {
        let d = chrono::NaiveDate::from_ymd_opt(period.year(), period.month(), 15).unwrap();
        let mut v = Voucher::new(period, d, "记", no);
        let a = m(amount);
        v.entries.push(Entry {
            debit: a,
            ..Entry::new(1, debit, "合并测试")
        });
        v.entries.push(Entry {
            credit: a,
            ..Entry::new(2, credit, "合并测试")
        });
        let id = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, id, "u").unwrap();
    }

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    #[test]
    fn preview_unions_accounts_and_sums_across_books() {
        let p = Period::new(2026, 1).unwrap();
        let a = mem();
        let b = mem();
        // 账套 A：现金 1000 / 实收资本 1000；现金 500 / 收入 500
        two_line(&a, p, 1, "1001", "4001", "1000");
        two_line(&a, p, 2, "1001", "600101", "500");
        // 账套 B：只有 现金 300 / 实收资本 300
        two_line(&b, p, 1, "1001", "4001", "300");

        let sources = vec![
            ConsolSource {
                key: "b1".into(),
                company: "甲公司".into(),
                db: &a,
            },
            ConsolSource {
                key: "b2".into(),
                company: "乙公司".into(),
                db: &b,
            },
        ];
        let r = preview(&sources, p).unwrap();

        assert_eq!(r.books.len(), 2);
        assert_eq!(r.books[0], ("b1".to_string(), "甲公司".to_string()));
        // 科目并集：A 有 3 个、B 有 2 个（与 A 重叠），并集 3 个
        assert_eq!(r.rows.len(), 3, "科目并集应当是 3 个：{:?}", r.rows);
        let cash = r
            .rows
            .iter()
            .find(|x| x.account_code == "1001")
            .expect("现金行");
        assert_eq!(cash.values, vec![m("1500"), m("300")]);
        assert_eq!(cash.total, m("1800"));
        // B 没有的科目记 0，不是漏列
        let income = r
            .rows
            .iter()
            .find(|x| x.account_code == "600101")
            .expect("收入行");
        // 余额是**有符号**的：贷方科目为负。这是 `BalanceRow::end()` 的既有语义，
        // 合并报表不改它——否则同一科目在「余额表」和「合并报表」里符号相反，
        // 使用者对账时会以为是错。
        assert_eq!(income.values, vec![m("-500"), Money::ZERO]);
        assert_eq!(income.total, m("-500"));
        // 科目名从有余额的那套取
        assert_eq!(income.account_name, "产品销售收入");
        // 行按科目编码升序（各账套列才对得齐）
        let codes: Vec<&str> = r.rows.iter().map(|x| x.account_code.as_str()).collect();
        assert_eq!(codes, vec!["1001", "4001", "600101"]);
    }

    #[test]
    fn preview_skips_zero_balance_accounts_and_empty_sources() {
        let p = Period::new(2026, 1).unwrap();
        let a = mem();
        two_line(&a, p, 1, "1001", "4001", "1000");
        // 空账套：没有任何凭证，所有科目余额为零
        let empty = mem();

        let r = preview(
            &[
                ConsolSource {
                    key: "b1".into(),
                    company: "甲".into(),
                    db: &a,
                },
                ConsolSource {
                    key: "b2".into(),
                    company: "乙".into(),
                    db: &empty,
                },
            ],
            p,
        )
        .unwrap();
        // 空账套不贡献任何行，只贡献一列 0
        assert_eq!(r.rows.len(), 2, "只有 A 的两个科目");
        assert!(r.rows.iter().all(|x| x.values[1].is_zero()));

        // 没选账套 → 空报表，不报错
        let none = preview(&[], p).unwrap();
        assert!(none.rows.is_empty());
        assert!(none.books.is_empty());
    }

    #[test]
    fn preview_only_lists_leaf_accounts() {
        let p = Period::new(2026, 1).unwrap();
        let a = mem();
        two_line(&a, p, 1, "1001", "4001", "1000");
        let r = preview(
            &[ConsolSource {
                key: "b1".into(),
                company: "甲".into(),
                db: &a,
            }],
            p,
        )
        .unwrap();
        let chart = accounts::chart(&a).unwrap();
        // 每一行都必须是末级科目：父级出现就会父子双计
        for row in &r.rows {
            assert!(
                chart.is_leaf(&row.account_code),
                "{} 不是末级科目",
                row.account_code
            );
        }
        assert!(r.rows.iter().any(|x| x.account_code == "1001"));
    }
}
