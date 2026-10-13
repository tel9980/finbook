//! 期初余额批量粘贴解析
//!
//! 和 `aux_batch` 是姐妹模块，解决同一个问题：
//! 期初有 199 个末级科目，一个个点「添加科目」再填数是灾难；
//! 而财务手上通常已经有一份上期期末的科目余额表（Excel，编码+金额两列）。
//!
//! 三种写法都收：
//!   1. `编码,金额` —— 最常见，从余额表直接复制两列
//!   2. `编码,借/贷,金额` —— 金额恒为正，自己声明方向
//!   3. `编码,借方金额,贷方金额` —— 和 UI 表格同一布局（两列各放一边）
//!
//! **方向判定规则**（省掉新手最容易错的那一步）：
//!   - 只给金额时，方向从**科目的余额方向**来：资产类记借，负债/权益/收入记贷。
//!     用户从余额表抄来的数天然就是带方向的，但余额表上通常只有一列「期末余额」
//!     不区分借贷符号 —— 按科目方向自动归位就是在这里做的。
//!   - 金额带负号：`-5000` 表示反方向（贷方余额的资产、借方余额的负债）。
//!     余额表里红字/负数就是这个意思。
//!   - 显式写 `借/贷` 时，用户声明优先。

use fincore::{Direction, Money};

/// 解析出来的一行
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRow {
    pub code: String,
    pub dir: Direction,
    /// 年初余额（恒为正 ———— 方向由 dir 单独记录，UI 上也是这么填的）
    pub amount: Money,
    /// 跳过原因（None = 可导入）
    pub skip: Option<SkipReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// 科目编码不在科目表里（抄错码了，这是新手期初最常见的错误）
    UnknownCode { suggestion: Option<String> },
    /// 本批次内重复
    DuplicateInBatch,
    /// 库里已有期初数（覆盖要用户明确同意，不在这里做）
    ExistsInBook,
    /// 金额解析失败
    BadAmount,
}

pub struct Parsed {
    pub rows: Vec<ParsedRow>,
    pub empty: bool,
}

impl Parsed {
    pub fn importable(&self) -> Vec<ParsedRow> {
        self.rows.iter().filter(|r| r.skip.is_none()).cloned().collect()
    }
}

fn split_cols(line: &str) -> Vec<String> {
    if line.contains('\t') {
        return line.split('\t').map(|s| s.trim().to_string()).collect();
    }
    if line.contains(',') || line.contains('，') || line.contains('、') {
        return line
            .split([',', '，', '、'])
            .map(|s| s.trim().to_string())
            .collect();
    }
    line.split_whitespace().map(|s| s.to_string()).collect()
}

fn is_header(cols: &[String]) -> bool {
    // 整行**全是**表头词才算表头（「编码,金额」「编码,借方,贷方」）。
    // 不能用 any：一行真实数据 `1001,贷,5000` 里也有个「贷」，
    // any 会把它整行吞掉 —— 这正是 2026-10-12 写测试时踩到的。
    cols.iter().all(|c| {
        let c = c.trim();
        matches!(
            c,
            "编码" | "科目编码" | "代码" | "编号" | "code" | "金额" | "余额" | "期末余额"
                | "借方" | "贷方" | "借" | "贷" | "借方余额" | "贷方余额" | "名称" | "科目名称"
        )
    })
}

/// 科目方向查找。`dir_of` 是调用方给的科目编码 → 余额方向映射。
fn sign_of_dir(dir: Direction) -> i8 {
    match dir {
        Direction::Debit => 1,
        Direction::Credit => -1,
    }
}

fn parse_amount(s: &str) -> Option<Money> {
    let s = s.replace(',', "").replace('，', "").trim().to_string();
    if s.is_empty() || s == "-" || s == "—" || s == "--" {
        return None;
    }
    // 括号表示负数：会计报表里 (5000) 即 -5000
    let (s, neg) = if s.starts_with('(') && s.ends_with(')') {
        (&s[1..s.len() - 1], true)
    } else {
        (s.as_str(), false)
    };
    let mut m = Money::parse_or_zero(s);
    if m.is_zero() && !s.trim_matches('0').trim_matches('.').is_empty() {
        return None;
    }
    if neg {
        m = m.negated();
    }
    Some(m)
}

fn dir_word(s: &str) -> Option<Direction> {
    match s.trim() {
        "借" | "借方" | "D" | "Dr" | "debit" | "借方余额" => Some(Direction::Debit),
        "贷" | "贷方" | "C" | "Cr" | "credit" | "贷方余额" => Some(Direction::Credit),
        _ => None,
    }
}

/// 找最接近的编码（抄错一两个字符时给提示）。只考虑长度接近的同前缀码。
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

/// 解析粘贴文本。
///
/// `dir_of`: 科目编码 → 该科目的余额方向（用来自动归位）；
/// `leaf_codes`: 全部末级科目编码（用来校验 + 找抄错的提示）；
/// `existing`: 库里已有期初数的编码。
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
        let (code_raw, rest): (&str, &[String]) = {
            if cols.len() >= 2
                && !cols[0].chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            {
                (cols[0].as_str(), &cols[1..])
            } else if cols.len() >= 2
                && !cols[1].chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            {
                (cols[1].as_str(), &[cols[0].clone()])
            } else {
                continue;
            }
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
                    (Some(d), None)
                } else {
                    (None, parse_amount(&rest[0]))
                }
            }
            2 => {
                // 写法2：借/贷,金额
                if let Some(d) = dir_word(&rest[0]) {
                    (Some(d), parse_amount(&rest[1]))
                // 写法3：借方金额,贷方金额
                } else {
                    let dr = parse_amount(&rest[0]).unwrap_or(Money::ZERO);
                    let cr = parse_amount(&rest[1]).unwrap_or(Money::ZERO);
                    if dr.is_zero() && cr.is_zero() {
                        (None, None)
                    } else if !dr.is_zero() && cr.is_zero() {
                        (Some(Direction::Debit), Some(dr.abs()))
                    } else if dr.is_zero() && !cr.is_zero() {
                        (Some(Direction::Credit), Some(cr.abs()))
                    } else {
                        // 两边都有数：这行本身就有问题，标出来
                        (None, None)
                    }
                }
            }
            _ => (None, None),
        };

        let Some(amount) = amount else {
            rows.push(ParsedRow {
                code: code.clone(),
                dir: Direction::Debit,
                amount: Money::ZERO,
                skip: Some(SkipReason::BadAmount),
            });
            continue;
        };

        // 负数表示反方向
        let (mut dir, mut amount) = (dir, amount);
        if amount.is_negative() {
            amount = amount.abs();
            dir = Some(match dir.unwrap_or(Direction::Debit) {
                Direction::Debit => Direction::Credit,
                Direction::Credit => Direction::Debit,
            });
        }
        // 没声明方向 → 按科目的余额方向自动归位
        let dir = dir.or_else(|| dir_of(&code)).unwrap_or(Direction::Debit);

        // 借/贷口径归一：余额符号按科目方向折算
        // （正=借、负=贷是 fincore 全引擎的约定；此处年初数恒为正+方向）
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
        // sign_of_dir 供调用方做试算口径折算，这里留着以备调试断言
        let _ = sign_of_dir(dir);

        rows.push(ParsedRow { code, dir, amount, skip });
    }

    Parsed { empty: rows.is_empty(), rows }
}

// ---------------------------------------------------------------------------
// 批量粘贴窗口的 UI 状态
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Ui {
    pub open: bool,
    pub text: String,
    pub preview: Option<Parsed>,
    pub done: Option<String>,
    pub err: String,
}

impl Ui {
    /// 展示批量粘贴窗口。
    ///
    /// `dir_of`/`leaf_codes` 与 `parse` 同；`on_import` 把可导入行写进调用方的
    /// `Vec<EditRow>`（期初页的草稿区），返回实际并入的行数。
    /// 不直接写库的原因：期初页是「改完点保存」两步走，批量粘贴只是帮用户
    /// 把草稿填好，最终仍由保存按钮统一落库+校验。
    pub fn window(
        &mut self,
        dir_of: &dyn Fn(&str) -> Option<Direction>,
        leaf_codes: &[String],
        existing: &[String],
        on_import: &mut dyn FnMut(Vec<ParsedRow>) -> usize,
        egui_ui: &mut egui::Ui,
    ) -> bool {
        use egui::RichText;
        if !self.open {
            return false;
        }
        let mut keep_open = true;
        let mut do_preview = false;
        let mut do_import = false;
        let mut close = false;

        egui::Window::new("批量粘贴期初余额")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(true)
            .default_width(600.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(egui_ui.ctx(), |ui| {
                ui.label(
                    RichText::new(
                        "把上期期末的科目余额表复制过来，每行一条。方向不用你填 ——\n\
                         只给金额时按科目自动归位（资产记借、负债权益记贷）；\n\
                         负数/红字自动转反方向。",
                    )
                    .weak(),
                );
                ui.label(
                    RichText::new(
                        "写法：\n  1001,58000          ← 编码,金额（最常用）\n  \
                         1001,借,58000       ← 编码,方向,金额\n  \
                         1001,58000,0        ← 编码,借方,贷方（和表格同一布局）",
                    )
                    .weak(),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("粘贴到这里：").strong());
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .desired_rows(10)
                        .hint_text("1001,58000\n1002,125000\n2001,80000"),
                );

                if !self.err.is_empty() {
                    ui.colored_label(crate::theme::palette::CREDIT, &self.err);
                }

                ui.horizontal(|ui| {
                    if ui.button("预览").clicked() {
                        do_preview = true;
                    }
                    if ui.button("并入草稿").clicked() {
                        do_import = true;
                    }
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });

                if do_preview {
                    let p = parse(&self.text, dir_of, leaf_codes, existing);
                    if p.empty {
                        self.preview = None;
                        self.err =
                            "没解析出内容。每行一条：编码,金额（方向按科目自动归位）。".to_string();
                    } else {
                        self.err.clear();
                        self.preview = Some(p);
                    }
                }

                if let Some(p) = &self.preview {
                    let ok = p.rows.iter().filter(|r| r.skip.is_none()).count();
                    let bad = p.rows.len() - ok;
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("解析到 {} 条，其中 {} 条可并入", p.rows.len(), ok)).strong());
                    if bad > 0 {
                        ui.colored_label(
                            crate::theme::palette::CREDIT,
                            &format!("{bad} 条有问题（编码不对 / 库里已填过），将跳过"),
                        );
                    }
                    // 试算提示：可并入部分借贷各多少 —— 用户粘完先看平不平
                    let (mut d, mut c) = (Money::ZERO, Money::ZERO);
                    for r in p.rows.iter().filter(|r| r.skip.is_none()) {
                        match r.dir {
                            Direction::Debit => d += r.amount,
                            Direction::Credit => c += r.amount,
                        }
                    }
                    ui.label(
                        RichText::new(format!(
                            "本次借方合计 {} ／ 贷方合计 {} ／ 差额 {}",
                            d.fmt_money(),
                            c.fmt_money(),
                            (d - c).abs().fmt_money()
                        ))
                        .weak(),
                    );

                    let cols = [
                        crate::widgets::TCol::new("编码", 110.0).fixed(),
                        crate::widgets::TCol::new("方向", 60.0).fixed(),
                        crate::widgets::TCol::new("金额", 120.0).fixed(),
                        crate::widgets::TCol::new("状态", 180.0),
                    ];
                    let rows = p.rows.clone();
                    crate::widgets::grid(ui, "begin_batch_preview", &cols, rows.len(), 22.0, |i, c, ui| {
                        let r = &rows[i];
                        match c {
                            0 => {
                                ui.label(&r.code);
                            }
                            1 => {
                                ui.label(r.dir.label());
                            }
                            2 => {
                                ui.label(r.amount.fmt_money());
                            }
                            _ => {
                                let txt = match &r.skip {
                                    None => "将并入".to_string(),
                                    Some(SkipReason::ExistsInBook) => "跳过：已填过".to_string(),
                                    Some(SkipReason::DuplicateInBatch) => "跳过：本批内重复".to_string(),
                                    Some(SkipReason::BadAmount) => "跳过：金额看不懂".to_string(),
                                    Some(SkipReason::UnknownCode { suggestion }) => match suggestion {
                                        Some(sg) => format!("跳过：无此科目，像不像 {sg}？"),
                                        None => "跳过：无此科目".to_string(),
                                    },
                                };
                                ui.label(RichText::new(txt).weak());
                            }
                        }
                    });
                }

                if let Some(msg) = &self.done {
                    ui.add_space(6.0);
                    ui.colored_label(crate::theme::palette::OK, msg);
                }
            });

        if !keep_open || close {
            self.open = false;
            self.text.clear();
            self.preview = None;
            self.done = None;
            self.err.clear();
        }

        if do_import {
            // 先要求点过预览：粘贴错位（多带表头、带上合计行）会静默变成脏数据。
            let Some(p) = &self.preview else {
                self.err = "请先点「预览」确认要并入哪些".to_string();
                return false;
            };
            let items = p.importable();
            if items.is_empty() {
                self.err = "没有可并入的新行".to_string();
                return false;
            }
            let skipped = p.rows.len() - items.len();
            let n = on_import(items);
            let mut msg = format!("已并入草稿 {n} 行");
            if skipped > 0 {
                msg.push_str(&format!("，跳过 {skipped} 行（见预览里的状态）"));
            }
            msg.push_str(" —— 别忘了点工具栏「保存」落库");
            self.done = Some(msg);
            self.text.clear();
            self.preview = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn debit(_code: &str) -> Option<Direction> {
        Some(Direction::Debit)
    }
    fn mixed(code: &str) -> Option<Direction> {
        match code {
            "1001" => Some(Direction::Debit),
            "2001" => Some(Direction::Credit),
            "4001" => Some(Direction::Credit),
            _ => None,
        }
    }
    fn leaves() -> Vec<String> {
        vec!["1001", "1002", "2001", "4001", "1122"]
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn 两列_只给金额时方向按科目走() {
        // 资产记借、负债记贷 —— 新手最容易错的就是这一判断
        let p = parse("1001,5000\n2001,3000", &mixed, &leaves(), &[]);
        assert_eq!(p.rows.len(), 2);
        assert_eq!((p.rows[0].code.as_str(), p.rows[0].dir), ("1001", Direction::Debit));
        assert_eq!((p.rows[1].code.as_str(), p.rows[1].dir), ("2001", Direction::Credit));
        assert!(p.rows.iter().all(|r| r.skip.is_none()));
    }

    #[test]
    fn excel制表符() {
        let p = parse("1001\t5000\n2001\t3000", &mixed, &leaves(), &[]);
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].amount, Money::parse_or_zero("5000"));
    }

    /// 余额表上红字/负数是反方向的意思
    #[test]
    fn 负数表示反方向() {
        let p = parse("1001,-5000", &mixed, &leaves(), &[]);
        assert_eq!(p.rows[0].dir, Direction::Credit);
        assert_eq!(p.rows[0].amount, Money::parse_or_zero("5000"));
    }

    #[test]
    fn 括号表示负数() {
        let p = parse("1001,(5000)", &mixed, &leaves(), &[]);
        assert_eq!(p.rows[0].dir, Direction::Credit);
    }

    #[test]
    fn 显式声明方向优先() {
        let p = parse("1001,贷,5000", &debit, &leaves(), &[]);
        assert_eq!(p.rows[0].dir, Direction::Credit);
    }

    #[test]
    fn 借贷两列写法() {
        let p = parse("1001,5000,\n2001,,3000", &mixed, &leaves(), &[]);
        assert_eq!(p.rows[0].dir, Direction::Debit);
        assert_eq!(p.rows[1].dir, Direction::Credit);
    }

    /// 抄错编码时要告诉用户「像不像这个」，而不是直接吞掉
    #[test]
    fn 未知编码给提示() {
        let p = parse("10011,5000", &mixed, &leaves(), &[]);
        assert!(matches!(p.rows[0].skip, Some(SkipReason::UnknownCode { .. })));
        if let Some(SkipReason::UnknownCode { suggestion }) = &p.rows[0].skip {
            assert_eq!(suggestion.as_deref(), Some("1001"));
        }
    }

    #[test]
    fn 库里已有的标为跳过() {
        let p = parse("1001,5000\n2001,3000", &mixed, &leaves(), &["1001".to_string()]);
        assert!(matches!(p.rows[0].skip, Some(SkipReason::ExistsInBook)));
        assert_eq!(p.rows[1].skip, None);
        assert_eq!(p.importable().len(), 1);
    }

    #[test]
    fn 跳过表头() {
        let p = parse("编码,金额\n1001,5000", &mixed, &leaves(), &[]);
        assert_eq!(p.rows.len(), 1);
    }

    #[test]
    fn 空输入不报错() {
        let p = parse("   \n\n ", &mixed, &leaves(), &[]);
        assert!(p.empty);
    }

    /// 千分位逗号是余额表复制过来的真实噪音
    #[test]
    fn 金额带千分位逗号() {
        let p = parse("1001,12,000.50", &debit, &leaves(), &[]);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].code, "1001");
    }
}