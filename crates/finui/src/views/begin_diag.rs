//! 期初试算不平的诊断：算出「改哪一行、改成什么样」能让试算变平。
//!
//! 原来的做法是拿科目表的方向去比（`dir_mismatch`）：负债放借方、资产放贷方。
//! 那个判断有两个问题：
//!
//! 1. 它依赖**科目表**的方向。如果科目表本身标错了（比如把「其他应付款」建成
//!    借方），检查会反过来指认正确的行。
//! 2. 它对**有累计借贷**的行一律放弃 —— 可这类行恰恰也是不平衡的来源。
//!
//! 这里换一条不依赖任何假设的路径：直接把差额当成待解的方程。
//! 一行参与借贷合计的金额是 `s = ±年初余额 + 累计借 − 累计贷`，试算差额
//! `diff = Σs`。能改动一行让它变平的改法只有三种，各自对应一个充要条件：
//!
//! | 改法             | 该行贡献的变化     | 变平的条件             |
//! |------------------|-------------------|------------------------|
//! | 方向取反         | `−2 × ±年初余额`   | `diff == 2 × ±年初余额` |
//! | 累计借/贷对调    | `−2 × (累计借−贷)` | `diff == 2 × (累计借−贷)` |
//! | 这一行是差额本身  | 抹掉整行            | `diff == s`             |
//!
//! 三个条件都是**充要**的 —— 满足就一定平，不满足就一定不平。所以这里报出去的
//! 每一条都不是「嫌疑人」，是用户改完可以当场验证的**解**。第三种（`diff == s`）
//! 不是改这一行就行的，它说的是「除这一行外其余都平了」，多半是对方科目漏填。
//!
//! 只收充要解是有意的：宁可少报，也不要报一条「大概可能」，新手分不清两者，
//! 试一次发现没用，第二次就不信了。

use fincore::{dir_amount_to_signed, Direction, Money};

use super::begin::EditRow;

/// 一条可执行的解
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    /// 命中的行下标
    pub row: usize,
    pub kind: FixKind,
    /// 给用户看的一句话，已经带好数字
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixKind {
    /// 把该行的「方向」反过来就能平
    FlipDir,
    /// 把该行的「累计借方/累计贷方」对调就能平
    SwapAccum,
    /// 差额正好等于该行金额 —— 其余行都平了，多半是这一笔的对方科目漏填
    MissingCounterpart,
}

impl FixKind {
    /// 一键修复按钮的文案。空串表示不提供自动修复（MissingCounterpart 不是改这一行）。
    pub fn action_label(self) -> &'static str {
        match self {
            FixKind::FlipDir => "把方向反过来",
            FixKind::SwapAccum => "把累计借/贷对调",
            FixKind::MissingCounterpart => "",
        }
    }

    /// 排序权重：能一键修的排前面
    fn rank(self) -> u8 {
        match self {
            FixKind::FlipDir => 0,
            FixKind::SwapAccum => 1,
            FixKind::MissingCounterpart => 2,
        }
    }
}

/// 翻转某行方向后，这一行对借贷合计的贡献
fn signed_flipped_dir(r: &EditRow) -> Money {
    let yb = dir_amount_to_signed(opposite(r.dir), Money::parse_or_zero(&r.yb));
    yb + Money::parse_or_zero(&r.ad) - Money::parse_or_zero(&r.ac)
}

/// 累计借贷对调后，这一行对借贷合计的贡献
fn signed_swapped_accum(r: &EditRow) -> Money {
    let yb = dir_amount_to_signed(r.dir, Money::parse_or_zero(&r.yb));
    yb - Money::parse_or_zero(&r.ad) + Money::parse_or_zero(&r.ac)
}

fn opposite(d: Direction) -> Direction {
    match d {
        Direction::Debit => Direction::Credit,
        Direction::Credit => Direction::Debit,
    }
}

/// 试算不平（`diff != 0`）时，算出所有能让它变平的改法。
///
/// 返回按「可一键修复」优先排序；同档按行号升序，保证界面显示稳定。
/// 找不到解时返回空 Vec —— 调用方要据此说「以上都不是常见原因」。
pub fn explain(diff: Money, rows: &[EditRow]) -> Vec<Fix> {
    if diff.is_zero() {
        return Vec::new();
    }
    let mut out: Vec<Fix> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let cur = r.begin_signed();
        if cur.is_zero() {
            continue;
        }

        // 1) 翻方向：改完必须是 0
        let after_flip = diff - cur + signed_flipped_dir(r);
        if after_flip.is_zero() {
            let amt = Money::parse_or_zero(&r.yb);
            out.push(Fix {
                row: i,
                kind: FixKind::FlipDir,
                detail: format!(
                    "年初 {} 现在记在{}方，改成{}方后试算就平了",
                    amt.fmt_money(),
                    dir_word(r.dir),
                    dir_word(opposite(r.dir))
                ),
            });
        }

        // 2) 累计借贷对调：改完必须是 0
        let ad = Money::parse_or_zero(&r.ad);
        let ac = Money::parse_or_zero(&r.ac);
        if !(ad.is_zero() && ac.is_zero()) {
            let after_swap = diff - cur + signed_swapped_accum(r);
            if after_swap.is_zero() {
                out.push(Fix {
                    row: i,
                    kind: FixKind::SwapAccum,
                    detail: format!(
                        "累计借 {}、累计贷 {} 对调后试算就平了",
                        ad.fmt_money(),
                        ac.fmt_money()
                    ),
                });
            }
        }

        // 3) 这一行本身就是差额：抹掉它其余全平
        if diff == cur {
            out.push(Fix {
                row: i,
                kind: FixKind::MissingCounterpart,
                detail: format!(
                    "除了这一行，其余 {} 行都平了 —— \
                     多半是这笔的对方科目漏填了，检查有没有只填了一边的",
                    rows.len() - 1
                ),
            });
        }
    }
    out.sort_by_key(|f| (f.kind.rank(), f.row));
    out
}

/// 缺一行对方科目时的补法：给出金额和方向，用户只需选科目。
///
/// 借方合计大于贷方合计（`diff > 0`）说明贷方缺一笔，反之缺借方。
/// 差 0 时返回 None。
pub fn counterpart_fill(diff: Money) -> Option<(Direction, Money)> {
    if diff.is_zero() {
        return None;
    }
    let (d, amt) = if diff.is_positive() {
        (Direction::Credit, diff)
    } else {
        (Direction::Debit, diff.negated())
    };
    Some((d, amt.round2()))
}

/// 大白话的方向词（界面面向新手，不用 Debit/Credit）
pub fn dir_word(d: Direction) -> &'static str {
    match d {
        Direction::Debit => "借方",
        Direction::Credit => "贷方",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(code: &str, dir: Direction, yb: &str, ad: &str, ac: &str) -> EditRow {
        let mut e = EditRow::blank(code.to_string());
        e.dir = dir;
        e.yb = yb.to_string();
        e.ad = ad.to_string();
        e.ac = ac.to_string();
        e
    }

    /// 平衡时不该报任何解 —— 不能平着也提示「改这行」
    #[test]
    fn 平了不给解() {
        let rows = vec![row("1001", Direction::Debit, "5000", "", "")];
        assert!(explain(Money::ZERO, &rows).is_empty());
        assert_eq!(counterpart_fill(Money::ZERO), None);
    }

    /// 经典场景：负债 3000 填成了借方。
    ///
    /// 1001 和 2001 都填成了借方，Σs = 6000。翻**任意一行**都能归零 ——
    /// 数学上都是解，所以两条都要报出来，用户自己认哪行是负债。
    /// 这里只关心 2001 那条（它才是真正的负债）。
    #[test]
    fn 翻方向能平() {
        let rows = vec![
            row("1001", Direction::Debit, "3000", "", ""),
            row("2001", Direction::Debit, "3000", "", ""), // 应该是贷方
        ];
        let got = explain(Money::parse_or_zero("6000"), &rows);
        assert_eq!(got.len(), 2, "两行都能平，都要报：{got:?}");
        assert!(got.iter().all(|f| f.kind == FixKind::FlipDir));
        let f = got.iter().find(|f| f.row == 1).expect("应指出 2001");
        assert!(f.detail.contains("贷方"), "{}", f.detail);
        assert_eq!(f.kind.action_label(), "把方向反过来");
    }

    /// 解必须是**充要**的：报出来的改法，代回去算一定归零
    #[test]
    fn 每条解代回去都归零() {
        let cases = vec![
            (
                vec![
                    row("1001", Direction::Debit, "3000", "", ""),
                    row("2001", Direction::Debit, "3000", "", ""),
                ],
                "6000",
            ),
            (
                vec![
                    row("1001", Direction::Debit, "1000", "", ""),
                    row("2001", Direction::Debit, "3000", "2000", ""),
                ],
                "6000",
            ),
        ];
        for (rows, diffs) in cases {
            let diff = Money::parse_or_zero(diffs);
            assert_eq!(
                diff,
                rows.iter().map(|r| r.begin_signed()).sum::<Money>(),
                "diff 要等于实际 Σs"
            );
            for f in explain(diff, &rows) {
                let after = match f.kind {
                    FixKind::FlipDir => {
                        diff - rows[f.row].begin_signed() + signed_flipped_dir(&rows[f.row])
                    }
                    FixKind::SwapAccum => {
                        diff - rows[f.row].begin_signed() + signed_swapped_accum(&rows[f.row])
                    }
                    // 这一类不是改这行，单独验证：抹掉它应该归零
                    FixKind::MissingCounterpart => {
                        let rest: Money = rows
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| *j != f.row)
                            .map(|(_, r)| r.begin_signed())
                            .sum();
                        assert!(rest.is_zero(), "其余行应全平，实际 {rest}");
                        continue;
                    }
                };
                assert!(after.is_zero(), "{:?} 代回去是 {after}，不是 0", f.kind);
            }
        }
    }

    /// 有累计借贷的行方向填反 —— 旧检查会放弃，这里照样能算出来。
    ///
    /// 行 1 是负债，年初 3000 应在贷方，用户填了借方，还带了累计借 2000。
    /// 行 0 是资产 1000，正好让行 1 满足充要条件 `diff == 2 × 年初余额`：
    /// Σs = 1000 + (3000 + 2000) = 6000 = 2 × 3000 ✓
    /// 翻行 1 的方向后 = −3000 + 2000，6000 − 5000 + (−1000) = 0 ✓
    #[test]
    fn 有累计数的行也能定位() {
        let rows = vec![
            row("1001", Direction::Debit, "1000", "", ""),
            row("2001", Direction::Debit, "3000", "2000", ""),
        ];
        let diff: Money = rows.iter().map(|r| r.begin_signed()).sum();
        assert_eq!(diff, Money::parse_or_zero("6000"));
        let got = explain(diff, &rows);
        assert!(
            got.iter().any(|f| f.kind == FixKind::FlipDir && f.row == 1),
            "应能定位到有累计数的行：{got:?}"
        );
        // 旧检查（dir_mismatch）在这行上会放弃 —— 这正是本模块存在的理由
        assert!(
            !rows[1].dir_mismatch(Some(Direction::Credit)),
            "前提：旧检查确实放弃这一行"
        );
    }

    /// 累计借贷填反了 —— 对调就能平。
    ///
    /// 充要条件是 `diff == 2 × (累计借 − 累计贷)`。这里构造：
    ///   行 0：年初 0、累计借 1000 → 贡献 +1000
    ///   行 1：借方 1000          → 贡献 +1000
    /// Σs = 2000，把行 0 的累计借/贷对调后它变成 −1000，于是
    /// diff − 1000 + (−1000) = 0，正好归零。
    #[test]
    fn 累计借贷对调能平() {
        let rows = vec![
            row("1001", Direction::Debit, "0", "1000", "0"),
            row("1002", Direction::Debit, "1000", "", ""),
        ];
        let diff = Money::parse_or_zero("2000");
        let got = explain(diff, &rows);
        assert!(
            got.iter()
                .any(|f| f.kind == FixKind::SwapAccum && f.row == 0),
            "应报出对调累计借/贷的解：{got:?}"
        );
        let after = diff - rows[0].begin_signed() + signed_swapped_accum(&rows[0]);
        assert!(after.is_zero(), "对调后应是 0，实际 {after}");
    }

    /// 「只填了一边」：除某行外其余都平了 → 那一行就是全部差额。
    ///
    /// 行 1、行 2 互相抵平（各贷 5000），行 0 借 10000 独自多出来 ——
    /// 用户只填了资产的这一边，欠一个 10000 的贷方科目。
    #[test]
    fn 差额等于某行报漏填对方() {
        // 行 1、行 2 互相抵平（各 5000，一借一贷），行 0 的 10000 独自多出来
        let rows = vec![
            row("1001", Direction::Debit, "10000", "", ""),
            row("1002", Direction::Debit, "5000", "", ""),
            row("2001", Direction::Credit, "5000", "", ""),
        ];
        let diff: Money = rows.iter().map(|r| r.begin_signed()).sum();
        assert_eq!(diff, Money::parse_or_zero("10000"));
        let got = explain(diff, &rows);
        assert!(
            got.iter()
                .any(|f| f.kind == FixKind::MissingCounterpart && f.row == 0),
            "应报出漏填对方科目：{got:?}"
        );
    }

    /// 平的时候不该报「漏填对方」—— 这个检查也要守平
    #[test]
    fn 平了不报漏填对方() {
        let rows = vec![
            row("1001", Direction::Debit, "10000", "", ""),
            row("2001", Direction::Credit, "10000", "", ""),
        ];
        let diff: Money = rows.iter().map(|r| r.begin_signed()).sum();
        assert!(diff.is_zero());
        assert!(explain(diff, &rows).is_empty());
    }

    /// 这一类不该提供一键修复 —— 改这一行没用，缺的是另一行
    #[test]
    fn 漏填对方不提供一键修复() {
        assert_eq!(FixKind::MissingCounterpart.action_label(), "");
    }

    /// 零金额的行不该被当成解（「把空行的方向反过来」毫无意义）
    #[test]
    fn 零金额行被跳过() {
        let rows = vec![
            row("1001", Direction::Debit, "1000", "", ""),
            row("1002", Direction::Debit, "0", "", ""),
        ];
        let got = explain(Money::parse_or_zero("1000"), &rows);
        assert!(
            !got.iter().any(|f| f.row == 1),
            "空行不该出现在解里：{got:?}"
        );
    }

    /// 补一行对方科目：借方多就补贷方，贷方多就补借方
    #[test]
    fn 补对方科目的方向和金额() {
        let (d, amt) = counterpart_fill(Money::parse_or_zero("5000")).unwrap();
        assert_eq!(d, Direction::Credit, "借方多了 5000，要补贷方");
        assert_eq!(amt, Money::parse_or_zero("5000"));

        let (d, amt) = counterpart_fill(Money::parse_or_zero("-3000")).unwrap();
        assert_eq!(d, Direction::Debit, "贷方多了 3000，要补借方");
        assert_eq!(amt, Money::parse_or_zero("3000"), "金额取绝对值");
    }

    /// 排序稳定：能一键修的排前面，漏填对方垫底。
    ///
    /// 这组数据会同时命中两类：行 1 翻方向能平（可一键修）、
    /// 行 0 是漏填对方（只能提示）。前者必须排在前面。
    #[test]
    fn 解按可修复性排序() {
        let rows = vec![
            row("1001", Direction::Debit, "10000", "", ""),
            row("1002", Direction::Debit, "5000", "", ""),
            row("2001", Direction::Credit, "5000", "", ""),
        ];
        let diff: Money = rows.iter().map(|r| r.begin_signed()).sum();
        let got = explain(diff, &rows);
        assert_eq!(
            got.iter()
                .filter(|f| f.kind == FixKind::MissingCounterpart)
                .count(),
            1,
            "行 0 应报漏填对方：{got:?}"
        );
        assert!(
            got.iter().any(|f| f.kind == FixKind::FlipDir && f.row == 1),
            "行 1 翻方向能平：{got:?}"
        );
        for w in got.windows(2) {
            assert!(w[0].kind.rank() <= w[1].kind.rank(), "顺序没排开：{got:?}");
        }
        assert_eq!(got.first().unwrap().kind, FixKind::FlipDir);
    }
}
