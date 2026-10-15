//! 凭证批量导入的**界面部分**（解析、预检与落库在 `findb::imports`，与 Web 端共用）。
//!
//! 两步走：预检（只读：数据行数、缺失科目、借贷不平/金额问题逐行列出）→ 执行导入。
//! 缺失科目本次直接跳过（不覆盖、不新建）：先在「会计科目」建好或改编码一致，
//! 再导一次。借贷不平的整张跳过、不落库。导进来全是草稿，检查后按正常流程记账。

use std::collections::HashMap;

use egui::RichText;
use findb::imports::{ImportPreview, ImportTemplate};

#[derive(Default)]
pub struct Ui {
    pub open: bool,
    pub text: String,
    pub tmpl_idx: usize,
    pub preview: Option<ImportPreview>,
    pub done: Option<String>,
    pub err: String,
}

const TMPLS: [ImportTemplate; 3] =
    [ImportTemplate::Generic, ImportTemplate::Kingdee, ImportTemplate::Yonyou];

fn tmpl_hint(t: ImportTemplate) -> &'static str {
    match t {
        ImportTemplate::Generic => {
            "写法：日期,凭证字,摘要,科目编码,借方,贷方（后可加 客户编码,银行编码 两列）"
        }
        ImportTemplate::Kingdee => {
            "写法：日期,凭证字,凭证号,摘要,科目编码,科目名称,借方,贷方（后可加 客户,银行）"
        }
        ImportTemplate::Yonyou => {
            "写法：日期,凭证字号,摘要,科目编码,借方,贷方（后可加 客户,银行）"
        }
    }
}

impl Ui {
    /// 展示导入窗口。导入成功返回 true，调用方据此刷新列表。
    pub fn window(
        &mut self,
        ctx: &mut crate::state::AppCtx<'_>,
        egui_ui: &mut egui::Ui,
    ) -> bool {
        if !self.open {
            return false;
        }
        let mut keep_open = true;
        let mut do_preview = false;
        let mut do_import = false;
        let mut close = false;
        let mut imported = false;
        let period = ctx.period();

        egui::Window::new("批量导入凭证")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(egui_ui.ctx(), |ui| {
                ui.label(
                    RichText::new(
                        "从旧软件导出的凭证粘进来（或载入 Excel）。日期必须在当前期间内，\n\
                         跨月分两次导。导进来全是草稿，检查无误后按正常流程记账。",
                    )
                    .weak(),
                );
                ui.label(RichText::new(format!("当前期间：{}", period.label())).weak());
                ui.horizontal(|ui| {
                    ui.label("来源模板：");
                    for (i, t) in TMPLS.iter().enumerate() {
                        ui.radio_value(&mut self.tmpl_idx, i, t.label());
                    }
                    if ui.button("下载 Excel 模板").clicked() {
                        let sheets = crate::views::export::batch_template_sheets(
                            "凭证导入",
                            &findb::imports::voucher_template(),
                        );
                        match crate::views::export::save_template(&sheets, "凭证导入模板") {
                            Ok(Some(m)) => {
                                self.done = Some(m);
                                self.err.clear();
                            }
                            Ok(None) => {}
                            Err(e) => self.err = e,
                        }
                    }
                    if ui.button("从文件载入").clicked() {
                        if let Some(path) =
                            crate::views::export::pick_open(&["xlsx", "xls", "ods", "csv"])
                        {
                            let is_csv = path
                                .extension()
                                .and_then(|e| e.to_str())
                                .map(|e| e.eq_ignore_ascii_case("csv"))
                                .unwrap_or(false);
                            if is_csv {
                                match std::fs::read_to_string(&path) {
                                    Ok(t) => {
                                        self.text = t;
                                        self.preview = None;
                                        self.done = None;
                                        self.err.clear();
                                    }
                                    Err(e) => self.err = format!("读取文件失败：{e}"),
                                }
                            } else {
                                match findb::imports::read_xlsx(&path) {
                                    Ok(rows) => {
                                        self.text =
                                            findb::imports::xlsx_to_csv_text(&rows);
                                        self.preview = None;
                                        self.done = None;
                                        self.err.clear();
                                    }
                                    Err(e) => self.err = format!("读取 Excel 失败：{e}"),
                                }
                            }
                        }
                    }
                });
                ui.label(RichText::new(tmpl_hint(TMPLS[self.tmpl_idx])).weak());
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .desired_rows(10)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("2026-11-03,记,收加工费,100201,68000,0"),
                );

                if !self.err.is_empty() {
                    ui.colored_label(crate::theme::palette::CREDIT, &self.err);
                }

                ui.horizontal(|ui| {
                    if ui.button("预检").clicked() {
                        do_preview = true;
                    }
                    if ui.button("执行导入").clicked() {
                        do_import = true;
                    }
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });

                if do_preview {
                    let tmpl = TMPLS[self.tmpl_idx];
                    match findb::imports::analyze_import(
                        ctx.db(),
                        &self.text,
                        tmpl,
                        false,
                        Some(period),
                    ) {
                        Ok(pv) => {
                            self.err.clear();
                            self.preview = Some(pv);
                        }
                        Err(e) => self.err = format!("预检失败：{e}"),
                    }
                }

                if let Some(pv) = &self.preview {
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "数据 {} 行，其中缺失科目 {} 个、错误 {} 条、提醒 {} 条",
                            pv.total_rows,
                            pv.missing_accounts.len(),
                            pv.errors(),
                            pv.warns()
                        ))
                        .strong(),
                    );
                    if !pv.missing_accounts.is_empty() {
                        ui.colored_label(
                            crate::theme::palette::CREDIT,
                            "以下科目账套里没有，涉及的行会跳过（先去会计科目建好再导）：",
                        );
                        for m in pv.missing_accounts.iter().take(10) {
                            ui.label(format!("  {}（出现 {} 次）", m.code, m.count).as_str());
                        }
                        if pv.missing_accounts.len() > 10 {
                            ui.label(
                                format!("  ……另有 {} 个", pv.missing_accounts.len() - 10)
                                    .as_str(),
                            );
                        }
                    }
                    for iss in pv.issues.iter().take(15) {
                        let level = if iss.level == "error" { "错" } else { "注" };
                        let row = iss
                            .row
                            .map(|r| format!("第{r}行"))
                            .unwrap_or_default();
                        ui.label(
                            format!("[{level}·{}] {row} {}", iss.category, iss.detail).as_str(),
                        );
                    }
                    if pv.issues.len() > 15 {
                        ui.label(format!("……另有 {} 条", pv.issues.len() - 15).as_str());
                    }
                    if pv.blocking() {
                        ui.colored_label(
                            crate::theme::palette::CREDIT,
                            "有阻断错误：先按上面的提示改好，再点执行导入。",
                        );
                    }
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
            return false;
        }

        if do_import {
            // 先要求点过预检：没看过问题清单就执行，错行悄悄跳过用户还以为全进了
            let Some(pv) = &self.preview else {
                self.err = "请先点「预检」确认要导入哪些".to_string();
                return false;
            };
            if pv.blocking() {
                self.err = "有阻断错误，先按预检提示改好再执行".to_string();
                return false;
            }
            let tmpl = TMPLS[self.tmpl_idx];
            let who = ctx.user().username.clone();
            match findb::imports::import_vouchers(
                ctx.db(),
                period,
                &self.text,
                &who,
                &HashMap::new(),
                tmpl,
            ) {
                Ok(r) => {
                    let mut msg = format!("已导入 {} 张", r.ok);
                    if r.skipped > 0 {
                        msg.push_str(&format!("，跳过 {} 行", r.skipped));
                    }
                    self.done = Some(msg);
                    if !r.warnings.is_empty() {
                        let mut w = r.warnings.iter().take(8).cloned().collect::<Vec<_>>();
                        if r.warnings.len() > 8 {
                            w.push(format!("……另有 {} 条", r.warnings.len() - 8));
                        }
                        self.err = w.join("\n");
                    } else {
                        self.err.clear();
                    }
                    self.text.clear();
                    self.preview = None;
                    imported = true;
                }
                Err(e) => self.err = format!("导入失败：{e}"),
            }
        }
        imported
    }
}
