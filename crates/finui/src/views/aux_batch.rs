//! 辅助档案批量粘贴的**界面部分**（解析逻辑在 `fincore::batch::aux`）。
//!
//! 为什么解析不在这个文件里：Web 端也要同一套解析。若各写一份，桌面端支持了
//! 某种写法而 Web 没支持，用户会得出「这软件有的地方能用有的不能用」。
//! 解析下沉到 `fincore::batch`，两端共用；这里只管窗口与「预览 → 确认导入」两步。
//!
//! 两步是强制的：直接点确认会把一坨文本当数据写库 —— 粘贴错位（多带表头、
//! 带上了合计行）会静默变成脏档案，而用户毫无察觉。

use egui::RichText;
use fincore::AuxKind;

pub use fincore::batch::aux::{
    code_prefix, parse, template, to_entities, Parsed, ParsedRow, SkipReason,
};

// ---------------------------------------------------------------------------
// 批量粘贴窗口的 UI 状态
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Ui {
    pub open: bool,
    pub text: String,
    /// 解析预览（点「预览」后填充）
    pub preview: Option<Parsed>,
    /// 导入完成后的结果提示
    pub done: Option<String>,
    pub err: String,
}

impl Ui {
    /// 展示批量粘贴窗口。`existing` 是库里已有的编码（用来标出重复）。
    /// 导入成功后返回 true，调用方据此刷新列表。
    pub fn window(
        &mut self,
        ctx: &mut crate::state::AppCtx<'_>,
        kind: AuxKind,
        existing: &[String],
        egui_ui: &mut egui::Ui,
    ) -> bool {
        if !self.open {
            return false;
        }
        let mut keep_open = true;
        let mut do_preview = false;
        let mut do_import = false;
        let mut close = false;

        egui::Window::new("批量新增档案")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(true)
            .default_width(580.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(egui_ui.ctx(), |ui| {
                ui.label(
                    RichText::new(
                        "把 Excel 里的名单复制过来粘贴，每行一条。\n\
                         两种写法都行：\n\
                         • 两列「编码,名称」—— 有自己的编码体系时用\n\
                         • 只写名称 —— 编码自动编号（C01、C02…），不用自己想编码",
                    )
                    .weak(),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "嫌格式难整理？点下面的「下载 Excel 模板」拿空表去 Excel 里填，\n\
                         填完点「从 Excel 文件载入」直接读进来，一样要过预览。",
                    )
                    .weak(),
                );
                ui.label(RichText::new("粘贴到这里：").strong());
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .desired_rows(10)
                        .hint_text("C01,杭州明远贸易有限公司\nC02,宁波华信电子有限公司"),
                );

                if !self.err.is_empty() {
                    ui.colored_label(crate::theme::palette::CREDIT, &self.err);
                }

                ui.horizontal(|ui| {
                    if ui.button("预览").clicked() {
                        do_preview = true;
                    }
                    if ui.button("确认导入").clicked() {
                        do_import = true;
                    }
                    if ui
                        .button("下载 Excel 模板")
                        .on_hover_text("保存 .xlsx 空表（带示例和填表说明）")
                        .clicked()
                    {
                        let sheets = crate::views::export::batch_template_sheets(
                            &format!("{}档案", kind.label()),
                            &template(kind),
                        );
                        match crate::views::export::save_template(
                            &sheets,
                            &format!("{}档案导入模板", kind.label()),
                        ) {
                            Ok(Some(m)) => {
                                self.done = Some(m);
                                self.err.clear();
                            }
                            Ok(None) => {}
                            Err(e) => self.err = e,
                        }
                    }
                    if ui
                        .button("从 Excel 文件载入")
                        .on_hover_text("选择填好的 .xlsx，直接读进来预览")
                        .clicked()
                    {
                        if let Some(path) =
                            crate::views::export::pick_open(&["xlsx", "xls", "ods"])
                        {
                            match findb::imports::read_xlsx(&path) {
                                Ok(rows) => {
                                    self.text = findb::imports::xlsx_to_csv_text(&rows);
                                    self.preview = None;
                                    self.done = None;
                                    self.err.clear();
                                    // 载入后直接走一遍预览，条数对不对立刻看见
                                    do_preview = true;
                                }
                                Err(e) => self.err = format!("读取 Excel 失败：{e}"),
                            }
                        }
                    }
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });

                if do_preview {
                    let p = parse(&self.text, kind, existing);
                    if p.is_empty() {
                        self.preview = None;
                        self.err =
                            "没解析出内容。每行一条，编码和名称之间用逗号或 Tab 隔开就行。".to_string();
                    } else {
                        self.err.clear();
                        self.preview = Some(p);
                    }
                }

                if let Some(p) = &self.preview {
                    let ok = p.rows.iter().filter(|r| r.skip.is_none()).count();
                    let dup = p.rows.len() - ok;
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("解析到 {} 条，其中 {} 条将导入", p.rows.len(), ok)).strong());
                    if dup > 0 {
                        ui.colored_label(
                            crate::theme::palette::CREDIT,
                            &format!("{dup} 条编码已存在，将跳过（不会覆盖原有档案）"),
                        );
                    }
                    let cols = [
                        crate::widgets::TCol::new("编码", 110.0).fixed(),
                        crate::widgets::TCol::new("名称", 260.0),
                        crate::widgets::TCol::new("状态", 130.0).fixed(),
                    ];
                    let rows = p.rows.clone();
                    crate::widgets::grid(ui, "aux_batch_preview", &cols, rows.len(), 22.0, |i, c, ui| {
                        match c {
                            0 => {
                                ui.label(&rows[i].code);
                            }
                            1 => {
                                ui.label(&rows[i].name);
                            }
                            _ => {
                                let txt = match &rows[i].skip {
                                    None => "将导入",
                                    // 文案统一走 fincore 的 label()，两端共用一份
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
            // 先要求点过「预览」：直接点确认就把一坨文本当数据写库，
            // 粘贴错位（多带表头、带上了合计行）会静默变成脏档案。
            let Some(p) = &self.preview else {
                self.err = "请先点「预览」确认要导入哪些".to_string();
                return false;
            };
            let items = to_entities(kind, &p.importable());
            if items.is_empty() {
                self.err = "没有可导入的新档案（编码都已存在）".to_string();
            } else {
                match findb::auxs::insert_many_skip_existing(ctx.db(), &items) {
                    Ok((n, skipped)) => {
                        let mut msg = format!("已导入 {n} 条");
                        if !skipped.is_empty() {
                            msg.push_str(&format!("，跳过 {} 条（编码已存在）", skipped.len()));
                        }
                        self.done = Some(msg);
                        self.text.clear();
                        self.preview = None;
                        return true;
                    }
                    Err(e) => self.err = format!("导入失败：{e}"),
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(p: &Parsed) -> Vec<String> {
        p.rows.iter().map(|r| r.code.clone()).collect()
    }

    #[test]
    fn 解析两列_编码加名称() {
        let p = parse("C01,杭州明远\nC02,宁波华信", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01", "C02"]);
        assert_eq!(p.rows[1].name, "宁波华信");
        assert!(p.rows.iter().all(|r| r.skip.is_none()));
    }

    #[test]
    fn 解析excel制表符() {
        // Excel 复制两列出来就是 tab 分隔
        let p = parse("C01\t杭州明远\nC02\t宁波华信", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01", "C02"]);
        assert_eq!(p.rows[0].name, "杭州明远");
    }

    #[test]
    fn 只给名称时自动编号() {
        let p = parse("杭州明远\n宁波华信", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01", "C02"]);
        assert_eq!(p.rows[0].name, "杭州明远");
    }

    /// 小白最容易踩的：编号要从库里的最大值接着排，不能从 01 重来撞车
    #[test]
    fn 自动编号接着库里的最大号() {
        let existing = vec!["C01".to_string(), "C02".to_string(), "C07".to_string()];
        let p = parse("新客户甲\n新客户乙", AuxKind::Customer, &existing);
        assert_eq!(codes(&p), vec!["C08", "C09"]);
    }

    /// 不同档案用不同前缀，别全成 C01
    #[test]
    fn 各类型前缀不同() {
        for (k, want) in [
            (AuxKind::Customer, "C01"),
            (AuxKind::Supplier, "S01"),
            (AuxKind::Dept, "D01"),
            (AuxKind::Employee, "E01"),
            (AuxKind::Bank, "B01"),
        ] {
            let p = parse("某某", k, &[]);
            assert_eq!(p.rows[0].code, want, "{:?} 的前缀不对", k);
        }
    }

    /// 从 Excel 整块复制过来第一行通常是表头，不能当成一条数据
    #[test]
    fn 跳过表头行() {
        let p = parse("编码,名称\nC01,杭州明远", AuxKind::Customer, &[]);
        assert_eq!(p.rows.len(), 1, "表头不该被当成数据");
        assert_eq!(p.rows[0].code, "C01");
    }

    /// 冲突必须**报告出来**而不是静默覆盖 —— 覆盖会悄悄改掉客户名
    #[test]
    fn 库里已有的编码标为跳过() {
        let existing = vec!["C01".to_string()];
        let p = parse("C01,新名字\nC02,新客户", AuxKind::Customer, &existing);
        assert_eq!(p.rows[0].skip, Some(SkipReason::ExistsInBook));
        assert_eq!(p.rows[1].skip, None);
        assert_eq!(p.importable().len(), 1, "只有新的那条该被导入");
    }

    #[test]
    fn 批次内重复也标出() {
        let p = parse("C01,甲\nC01,乙", AuxKind::Customer, &[]);
        assert_eq!(p.rows[0].skip, None);
        assert_eq!(p.rows[1].skip, Some(SkipReason::DuplicateInBatch));
    }

    /// 从 WPS/记事本贴过来的空行和首尾空格不能变成空档案
    #[test]
    fn 清理空行与空白() {
        let p = parse("\n  \nC01,杭州明远\n\n  C02 , 宁波华信  \n", AuxKind::Customer, &[]);
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].name, "杭州明远");
        assert_eq!(p.rows[1].code, "C02");
        assert_eq!(p.rows[1].name, "宁波华信");
    }

    #[test]
    fn 全空输入不报错() {
        let p = parse("", AuxKind::Customer, &[]);
        assert!(p.is_empty());
        assert_eq!(p.importable().len(), 0);
    }
}