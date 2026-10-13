//! 期初余额批量粘贴的**界面部分**（解析逻辑在 `fincore::batch::begin`）。
//!
//! 为什么解析不在这个文件里：Web 端也要同一套解析。若各写一份，桌面端支持了
//! 某种写法而 Web 没支持，用户会得出「这软件有的地方能用有的不能用」。
//! 所以解析下沉到 `fincore::batch`，两端共用；这里只管窗口长什么样、
//! 「预览 → 并入草稿」两步怎么走。
//!
//! 两步是强制的：直接点确认会把一坨文本当数据写库 —— 粘贴错位（多带表头、
//! 带上了合计行）会静默变成脏数据，而用户毫无察觉。

pub use fincore::batch::begin::{parse, Parsed, ParsedRow, SkipReason};
use fincore::{Direction, Money};

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
                    if p.is_empty() {
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
                                    // 文案统一走 fincore 的 label()，两端共用一份，
                                    // 免得桌面端改了 Web 端没改
                                    Some(reason) => reason.label(),
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
        assert!(p.is_empty());
    }

    /// 千分位逗号是余额表复制过来的真实噪音
    #[test]
    fn 金额带千分位逗号() {
        let p = parse("1001,12,000.50", &debit, &leaves(), &[]);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].code, "1001");
    }
}