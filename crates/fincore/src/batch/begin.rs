//! 期初余额批量粘贴解析。
//!
//! 为什么需要它：期初建账是**上手第一道坎** —— 用户手上是一张上期的期末余额
//! 表，而软件要的是「编码 + 年初余额 + 方向」。让他一行行对着录是几百次录入。
//! 从 Excel 把余额表复制过来粘一次，差的是几个小时和几十个错别字。
//!
//! **方向不用填**是这一块的关键设计：新手最不懂的就是「资产记借、负债记贷」
//! （见 §4.59 / §4.62，期初不平的头号原因）。所以只给金额时按**科目表里的余额
//! 方向**自动归位；给了负数或红字括号则自动翻转方向。
//!
//! 三种写法（都能从 Excel 直接复制）：
//!   1. `编码,金额`             —— 最常用，方向按科目自动归位
//!   2. `编码,借/贷,金额`       —— 方向明确时用
//!   3. `编码,借方金额,贷方金额` —— 和期初表格同一布局
//!
//! 分隔符见 [`super::split_cols`]；标题行自动跳过。

use crate::{Direction, Money};

use super::split_cols;

/// 解析结果里的一行
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRow {
    pub code: String,
    pub dir: Direction,
    /// 年初余额，**恒为非负** —— 方向由 `dir` 单独记录
    pub amount: Money,
    /// 该行被跳过的原因（None = 可并入）
    pub skip: Option<SkipReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// 科目编码不在科目表里（抄错码了，这是新手期初最常见的错误）
    UnknownCode {
        /// 最接近的末级科目编码（抄错一两个字时给提示）
        suggestion: Option<String>,
    },
    /// 与本批次里前面的行重复
    DuplicateInBatch,
    /// 库里/草稿里已有这个科目的期初数
    ExistsInBook,
    /// 金额解析失败
    BadAmount,
}

impl SkipReason {
    /// 界面上给用户看的中文说明。两个端点共用，避免两边文案漂移
    pub fn label(&self) -> String {
        match self {
            SkipReason::UnknownCode { suggestion: Some(sg) } => format!("跳过：无此科目，像不像 {sg}？"),
            SkipReason::UnknownCode { suggestion: None } => "跳过：无此科目".to_string(),
            SkipReason::DuplicateInBatch => "跳过：本批内重复".to_string(),
            SkipReason::ExistsInBook => "跳过：已填过".to_string(),
            SkipReason::BadAmount => "跳过：金额看不懂".to_string(),
        }
    }
}

/// 解析结果
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub rows: Vec<ParsedRow>,
}

impl Parsed {
    /// 可并入的行
    pub fn importable(&self) -> Vec<ParsedRow> {
        self.rows.iter().filter(|r| r.skip.is_none()).cloned().collect()
    }
    /// 一条都没解析出来
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 试算：可并入部分的借贷合计与差额。
    ///
    /// 用户粘完先看这个 —— 一张余额表粘进来就能看出平不平，
    /// 比粘完 200 行才发现差额强得多。
    pub fn trial(&self) -> (Money, Money, Money) {
        let (mut d, mut c) = (Money::ZERO, Money::ZERO);
        for r in self.rows.iter().filter(|r| r.skip.is_none()) {
            match r.dir {
                Direction::Debit => d += r.amount,
                Direction::Credit => c += r.amount,
            }
        }
        (d.round2(), c.round2(), (d - c).round2())
    }
}

/// 表头词表。**大小写敏感、精确全等**。
const HEADER_WORDS: &[&str] = &[
    "编码", "科目编码", "代码", "编号", "code", "金额", "余额", "期末余额", "借方", "贷方", "借",
    "贷", "借方余额", "贷方余额", "名称", "科目名称",
];

fn is_header(cols: &[String]) -> bool {
    // 整行**全是**表头词才算表头（「编码,金额」「编码,借方,贷方」）。
    // 不能用 any：一行真实数据 `1001,贷,5000` 里也有个「贷」，
    // any 会把它整行吞掉 —— 用户粘进来的余额表凭空少一行。
    cols.iter().all(|c| HEADER_WORDS.contains(&c.trim()))
}

/// 方向词。**大小写敏感**：`DR`/`Debit` 不认 —— 这些是从系统导出的才常见，
/// 手打的人写 `借` 或 `贷`。放宽会让 `debit` 之类英文单词误判成方向。
fn dir_word(s: &str) -> Option<Direction> {
    match s.trim() {
        "借" | "借方" | "D" | "Dr" | "debit" | "借方余额" => Some(Direction::Debit),
        "贷" | "贷方" | "C" | "Cr" | "credit" | "贷方余额" => Some(Direction::Credit),
        _ => None,
    }
}

/// 解析金额。`None` = 看不懂（→ BadAmount）。
///
/// 容忍的写法都是财报/Excel 里的实际产物：千分位逗号、括号负数（`(5000)`
/// 即 -5000）、破折号占位（`-` / `—` / `--` 视为「没填」而不是 0）。
fn parse_amount(s: &str) -> Option<Money> {
    let s = s.replace(',', "").replace('，', "").trim().to_string();
    if s.is_empty() || s == "-" || s == "—" || s == "--" {
        return None;
    }
    let (s, neg) = if s.starts_with('(') && s.ends_with(')') {
        (&s[1..s.len() - 1], true)
    } else {
        (s.as_str(), false)
    };
    let mut m = Money::parse_or_zero(s);
    // parse_or_zero 把「看不懂」也当 0，所以再问一次：
    // 「abc」「12x」解析成 0，但它们不是「金额是 0」
    if m.is_zero() && !s.trim_matches('0').trim_matches('.').is_empty() {
        return None;
    }
    if neg {
        m = m.negated();
    }
    Some(m)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in dp.iter_mut().enumerate().take(a.len() + 1) {
        row[0] = i;
    }
    for (j, v) in dp[0].iter_mut().enumerate().take(b.len() + 1) {
        *v = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
    }
    dp[a.len()][b.len()]
}

/// 找最接近的编码（抄错一两个字符时给提示）。
///
/// 先按长度差粗筛（编码体系里长度差超过 2 的基本不是笔误），再算 Levenshtein
/// 距离，≤2 且最短者胜出。同分取先出现的 —— 稳定比「更聪明」重要，界面上
/// 同一个输入必须每次给同一个提示。
pub fn suggest<'a>(code: &str, leaf_codes: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for c in leaf_codes {
        if (c.len() as i32 - code.len() as i32).abs() > 2 {
            continue;
        }
        let d = edit_distance(code, c);
        if d <= 2 && best.map(|(b, _)| d < b).unwrap_or(true) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_string())
}

/// 含中文吗（用于「哪一列是名称」的判定）
fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// 解析粘贴文本。
///
/// `dir_of`: 科目编码 → 该科目的余额方向（用来自动归位 + 判定科目是否存在）；
/// `leaf_codes`: 全部末级科目编码（找抄错的提示用）；
/// `existing`: 库里/草稿里已有期初数的编码。
pub fn parse(
    text: &str,
    dir_of: &dyn Fn(&str) -> Option<Direction>,
    leaf_codes: &[String],
    existing: &[String],
) -> Parsed {
    let mut rows: Vec<ParsedRow> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let cols = split_cols(line);
        if cols.is_empty() {
            continue;
        }
        if lineno == 0 && is_header(&cols) {
            continue;
        }

        // 编码必须是第一列；容忍「名称,编码」倒序（纯中文的那列是名称）
        let (code_raw, rest): (&str, &[String]) = if cols.len() >= 2 && !has_cjk(&cols[0]) {
            (cols[0].as_str(), &cols[1..])
        } else if cols.len() >= 2 && !has_cjk(&cols[1]) {
            (cols[1].as_str(), &[cols[0].clone()])
        } else {
            // 单列行、或者两列都是中文（那就是一张名称表，不是余额表）
            continue;
        };
        let code = code_raw.trim().to_string();
        if code.is_empty() {
            continue;
        }

        // 按列数分三种写法
        let (dir, amount): (Option<Direction>, Option<Money>) = match rest.len() {
            0 => (None, None),
            1 => {
                if let Some(d) = dir_word(&rest[0]) {
                    // 只有方向没有金额 → BadAmount（下面统一处理）
                    (Some(d), None)
                } else {
                    // 写法1：编码,金额
                    (None, parse_amount(&rest[0]))
                }
            }
            2 => {
                if let Some(d) = dir_word(&rest[0]) {
                    // 写法2：编码,借/贷,金额
                    (Some(d), parse_amount(&rest[1]))
                } else {
                    // 写法3：编码,借方金额,贷方金额
                    let dr = parse_amount(&rest[0]).unwrap_or(Money::ZERO);
                    let cr = parse_amount(&rest[1]).unwrap_or(Money::ZERO);
                    match (dr.is_zero(), cr.is_zero()) {
                        (true, true) => (None, None),
                        (false, true) => (Some(Direction::Debit), Some(dr.abs())),
                        (true, false) => (Some(Direction::Credit), Some(cr.abs())),
                        // 两边都有数：这行本身就有问题，标出来
                        (false, false) => (None, None),
                    }
                }
            }
            _ => (None, None),
        };

        let Some(amount) = amount else {
            // BadAmount 直接记一行就走：这时还不知道方向对不对，
            // 也没有意义的金额，再往下判重复/科目只会给出更让人困惑的理由。
            rows.push(ParsedRow {
                code: code.clone(),
                dir: Direction::Debit,
                amount: Money::ZERO,
                skip: Some(SkipReason::BadAmount),
            });
            continue;
        };

        // 负数（含括号红字）表示反方向
        let (mut dir, mut amount) = (dir, amount);
        if amount.is_negative() {
            amount = amount.abs();
            dir = Some(match dir.unwrap_or(Direction::Debit) {
                Direction::Debit => Direction::Credit,
                Direction::Credit => Direction::Debit,
            });
        }
        // 没声明方向 → 按科目的余额方向自动归位（期初最关键的一步）
        let dir = dir.or_else(|| dir_of(&code)).unwrap_or(Direction::Debit);

        let skip = if seen.iter().any(|c| *c == code) {
            Some(SkipReason::DuplicateInBatch)
        } else if existing.iter().any(|c| *c == code) {
            Some(SkipReason::ExistsInBook)
        } else if dir_of(&code).is_none() {
            let sg = suggest(&code, leaf_codes.iter().map(|s| s.as_str()));
            Some(SkipReason::UnknownCode { suggestion: sg })
        } else {
            seen.push(code.clone());
            None
        };

        rows.push(ParsedRow { code, dir, amount, skip });
    }

    Parsed { rows }
}

/// 期初余额的 Excel 导入模板（桌面端「下载模板」用）。
///
/// 布局必须是 [`parse`] 认得的写法 —— 这里用最省事的「编码,金额」两列，
/// 方向按科目自动归位；示例行特意一借一贷，下载后直接预览能看到试算是平的。
pub fn template() -> super::Template {
    super::Template {
        headers: vec!["科目编码".to_string(), "金额".to_string()],
        examples: vec![
            vec!["1001".to_string(), "58000".to_string()],
            vec!["2001".to_string(), "58000".to_string()],
        ],
        help: vec![
            "第一页第一行是表头，从第二行开始填你自己的数据。".to_string(),
            "先把两行示例删掉，再填。".to_string(),
            "方向不用填：资产自动记借、负债权益自动记贷；负数/红字括号自动转反方向。".to_string(),
            "想自己写方向也行：加一列，写成「编码,方向,金额」（方向填 借/贷）。"
                .to_string(),
            "填完后回软件：点「从 Excel 文件载入」直接读进来，或复制粘贴到输入框。"
                .to_string(),
            "载入后先看预览里的试算（借方合计/贷方合计/差额），平了再点「并入草稿」。"
                .to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 资产=借、负债=贷 的科目表（够测试用，不建完整科目表）
    fn dir_of(code: &str) -> Option<Direction> {
        match code {
            "1001" | "1002" | "1403" => Some(Direction::Debit),
            "2001" | "2202" | "4001" => Some(Direction::Credit),
            _ => None,
        }
    }
    fn leaves() -> Vec<String> {
        ["1001", "1002", "1403", "2001", "2202", "4001"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }
    fn p(text: &str) -> Parsed {
        parse(text, &dir_of, &leaves(), &[])
    }

    #[test]
    fn 两列按科目方向归位() {
        // 资产 1001 → 借；负债 2001 → 贷。方向都没填，全靠科目表
        let r = p("1001,58000\n2001,80000");
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0].dir, Direction::Debit);
        assert_eq!(r.rows[1].dir, Direction::Credit);
        assert_eq!(r.rows[0].amount, Money::parse_or_zero("58000"));
        assert!(r.rows.iter().all(|x| x.skip.is_none()));
    }

    #[test]
    fn excel制表符() {
        let r = p("1001\t58000\n1002\t125000");
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[1].code, "1002");
        assert_eq!(r.rows[1].amount, Money::parse_or_zero("125000"));
    }

    #[test]
    fn 负数表示反方向() {
        let r = p("1001,-5000");
        assert_eq!(r.rows[0].amount, Money::parse_or_zero("5000"), "金额取绝对值");
        assert_eq!(r.rows[0].dir, Direction::Credit, "负数翻转方向");
    }

    #[test]
    fn 括号负数() {
        let r = p("1001,(5000)");
        assert_eq!(r.rows[0].amount, Money::parse_or_zero("5000"));
        assert_eq!(r.rows[0].dir, Direction::Credit);
    }

    #[test]
    fn 显式方向优先于科目方向() {
        // 1001 是资产（借），但用户明写「贷」—— 尊重用户
        let r = p("1001,贷,5000");
        assert_eq!(r.rows[0].dir, Direction::Credit);
    }

    #[test]
    fn 借贷两列写法() {
        let r = p("1001,58000,0\n2001,0,80000");
        assert_eq!(r.rows[0].dir, Direction::Debit);
        assert_eq!(r.rows[0].amount, Money::parse_or_zero("58000"));
        assert_eq!(r.rows[1].dir, Direction::Credit);
        assert_eq!(r.rows[1].amount, Money::parse_or_zero("80000"));
    }

    /// 借贷两列都有数 → 这行本身有问题，必须标出来而不是随便挑一边
    #[test]
    fn 借贷两列都有数标看不懂() {
        let r = p("1001,100,200");
        assert_eq!(r.rows[0].skip, Some(SkipReason::BadAmount));
    }

    #[test]
    fn 未知编码给提示() {
        let r = p("10011,5000");
        match &r.rows[0].skip {
            Some(SkipReason::UnknownCode { suggestion }) => {
                assert_eq!(suggestion.as_deref(), Some("1001"), "应提示最接近的科目");
            }
            other => panic!("应是无此科目，实际 {other:?}"),
        }
    }

    #[test]
    fn 库中已有的标跳过() {
        let r = parse("1001,58000\n1002,1000", &dir_of, &leaves(), &["1001".into()]);
        assert_eq!(r.rows[0].skip, Some(SkipReason::ExistsInBook));
        assert_eq!(r.rows[1].skip, None);
        assert_eq!(r.importable().len(), 1);
    }

    #[test]
    fn 跳过表头() {
        let r = p("科目编码,期末余额\n1001,58000");
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0].code, "1001");
    }

    /// 表头必须整行都是词。一行真数据里带个「贷」不能被吞掉
    #[test]
    fn 半行表头不跳() {
        let r = p("1001,贷,5000");
        assert_eq!(r.rows.len(), 1, "`1001,贷,5000` 是数据不是表头");
    }

    #[test]
    fn 空输入不报错() {
        assert!(p("").is_empty());
        assert!(p("\n\n  \n").is_empty());
    }

    #[test]
    fn 破折号当没填而不是零() {
        // 余额表里的「—」是「无」，不是 0。当成 0 会把真实缺口填平
        let r = p("1001,—");
        assert_eq!(r.rows[0].skip, Some(SkipReason::BadAmount));
    }

    #[test]
    fn 零是合法金额() {
        let r = p("1001,0");
        assert_eq!(r.rows[0].skip, None);
        assert!(r.rows[0].amount.is_zero());
    }

    /// 金额带千分位时会被切列切开 —— 先按 tab 粘才不被切坏。
    /// 这里锁住现状（1 借 2 贷，两边都有数 → BadAmount），提醒改切列逻辑时要一起改。
    #[test]
    fn 千分位逗号会切坏行() {
        let r = p("1001,12,000.50");
        assert_eq!(
            r.rows[0].skip,
            Some(SkipReason::BadAmount),
            "千分位逗号被当分隔符了。改动切列逻辑时要同步更新这条"
        );
        // tab 粘就不会被切坏
        let ok = p("1001\t12000.50");
        assert_eq!(ok.rows[0].amount, Money::parse_or_zero("12000.50"));
    }

    /// 试算：粘完先看平不平
    #[test]
    fn 试算借贷合计与差额() {
        let r = p("1001,50000\n2001,50000");
        let (d, c, diff) = r.trial();
        assert_eq!(d, Money::parse_or_zero("50000"));
        assert_eq!(c, Money::parse_or_zero("50000"));
        assert!(diff.is_zero(), "应该是平的");

        let r2 = p("1001,50000\n2001,40000");
        let (_, _, diff2) = r2.trial();
        assert_eq!(diff2, Money::parse_or_zero("10000"), "差 10000 应该在试算里看见");
    }

    /// 试算只算可并入的行 —— 跳过的不该影响用户对平不平的判断
    #[test]
    fn 试算跳过坏行() {
        let r = parse("1001,50000\n2001,40000\n9999,99999", &dir_of, &leaves(), &[]);
        assert!(matches!(r.rows[2].skip, Some(SkipReason::UnknownCode { .. })));
        let (d, c, _) = r.trial();
        assert_eq!(d, Money::parse_or_zero("50000"), "未知科目不该计入借方");
        assert_eq!(c, Money::parse_or_zero("40000"));
    }

    #[test]
    fn 跳过原因都有中文文案() {
        for r in [
            SkipReason::DuplicateInBatch,
            SkipReason::ExistsInBook,
            SkipReason::BadAmount,
            SkipReason::UnknownCode { suggestion: None },
            SkipReason::UnknownCode { suggestion: Some("1001".into()) },
        ] {
            let label = r.label();
            assert!(!label.is_empty(), "{r:?} 缺文案");
        }
        assert!(SkipReason::UnknownCode { suggestion: Some("1001".into()) }
            .label()
            .contains("1001"));
    }

    /// 方向词大小写敏感：DR / Debit 不认（手打的人写「借」或「贷」）
    #[test]
    fn 方向词大小写敏感() {
        assert_eq!(dir_word("借"), Some(Direction::Debit));
        assert_eq!(dir_word("Dr"), Some(Direction::Debit));
        assert_eq!(dir_word("DR"), None);
        assert_eq!(dir_word("Debit"), None);
        assert_eq!(dir_word("贷方余额"), Some(Direction::Credit));
    }

    /// 编码建议要稳定：同样输入每次给同一个答案
    #[test]
    fn 建议稳定() {
        let a = suggest("10011", leaves().iter().map(|s| s.as_str()));
        let b = suggest("10011", leaves().iter().map(|s| s.as_str()));
        assert_eq!(a, b);
        assert_eq!(a.as_deref(), Some("1001"));
    }

    #[test]
    fn 差太远不给建议() {
        assert_eq!(suggest("9999", leaves().iter().map(|s| s.as_str())), None);
    }

    /// 模板出去的列，解析必须能认回来 —— 否则用户填完表导不进来。
    /// 模板改了列布局却没改解析（或反过来），这条先挂。
    #[test]
    fn 模板列能被自家解析认回来() {
        let t = template();
        let mut text = t.headers.join(",");
        for r in &t.examples {
            text.push('\n');
            text.push_str(&r.join(","));
        }
        let parsed = parse(&text, &dir_of, &leaves(), &[]);
        assert_eq!(parsed.rows.len(), t.examples.len(), "表头应被跳过，只剩示例行");
        assert_eq!(
            parsed.importable().len(),
            t.examples.len(),
            "示例行都该是可并入的"
        );
        assert_eq!(parsed.rows[0].dir, Direction::Debit, "1001 资产应归借方");
        assert_eq!(parsed.rows[1].dir, Direction::Credit, "2001 负债应归贷方");
        let (_, _, diff) = parsed.trial();
        assert!(diff.is_zero(), "示例一借一贷，试算应该是平的");
    }
}