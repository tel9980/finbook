//! 自定义报表设计器
//!
//! 每张报表 = 一组列标题 + 若干行，每个单元格一条公式（沿用用友 UFO 的取数函数）。
//! 界面分两块：预览（按期间取数渲染）与设计（编辑行、列、公式并即时算给用户看）。

use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use findb::mgmt::{self, CustomLine, CustomReport};
use fincore::{Money, Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme;
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)]
pub enum CrTab {
    Preview,
    Design,
}

pub struct CustomReportView {
    pub tab: CrTab,
    pub period_text: String,
    /// 全部报表（只取 key/name/结构，不含取数结果）
    pub list: Vec<CustomReport>,
    pub cur_key: String,
    pub cur: Option<CustomReport>,
    /// 预览页的取数结果（行 × 列）
    pub values: Vec<Vec<Money>>,
    pub rename: String,
    /// 设计器里正在编辑的报表（未保存）
    pub draft: Option<CustomReport>,
    /// 列标题编辑框，用 `|` 分隔
    pub columns_text: String,
    /// 语法检查结果（行, 列, 错误）
    pub checks: Vec<(usize, usize, String)>,
    /// 设计器底部实时预览的取数结果
    pub preview: Vec<Vec<Money>>,
    /// 设计稿版本号：结构或公式有改动就 +1，用来触发预览重算
    pub draft_ver: usize,
    pub dirty: bool,
    key: String,
}

impl Default for CustomReportView {
    fn default() -> Self {
        Self {
            tab: CrTab::Preview,
            period_text: String::new(),
            list: Vec::new(),
            cur_key: String::new(),
            cur: None,
            values: Vec::new(),
            rename: String::new(),
            draft: None,
            columns_text: String::new(),
            checks: Vec::new(),
            preview: Vec::new(),
            draft_ver: 0,
            dirty: true,
            key: String::new(),
        }
    }
}

impl CustomReportView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        self.dirty = true;
    }

    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        // 设计稿版本号参与 key：公式改了会立刻重算底部预览
        let key = format!(
            "{}|{:?}|{}|{}",
            p.ymm(),
            self.tab,
            self.cur_key,
            self.draft_ver
        );
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        match mgmt::custom_list(ctx.db()) {
            Ok(v) => self.list = v,
            Err(e) => {
                ctx.error(e.to_string());
                self.list.clear();
            }
        }
        if self.cur_key.is_empty() {
            if let Some(f) = self.list.first() {
                self.cur_key = f.key.clone();
            }
        }
        self.cur = None;
        if !self.cur_key.is_empty() {
            match mgmt::custom_get(ctx.db(), &self.cur_key) {
                Ok(v) => self.cur = v,
                Err(e) => ctx.error(e.to_string()),
            }
        }
        // 改名框跟随当前报表；只在真正重新取数时刷新，避免冲掉正在输入的内容
        let cur = self.cur.clone();
        if let Some(c) = &cur {
            self.rename = c.name.clone();
        }

        self.values.clear();
        self.preview.clear();
        match self.tab {
            CrTab::Preview => {
                if let Some(c) = &cur {
                    match mgmt::custom_report_values(ctx.db(), c, p, Some(ctx.user())) {
                        Ok(v) => self.values = v,
                        Err(e) => ctx.error(e.to_string()),
                    }
                }
            }
            CrTab::Design => {
                // 先克隆出来再算，避免取数期间一直借用 self.draft
                let d = self.draft.clone();
                if let Some(d) = &d {
                    match mgmt::custom_report_values(ctx.db(), d, p, Some(ctx.user())) {
                        Ok(v) => self.preview = v,
                        Err(e) => ctx.error(e.to_string()),
                    }
                }
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "自定义报表", |ui| {
            ui.label(RichText::new(format!("取数期间 {}", p.label())).weak());
        });

        let mut want_new = false;
        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, CrTab::Preview, "报表预览");
            ui.selectable_value(&mut self.tab, CrTab::Design, "设计器");
            ui.separator();
            ui.label("期间");
            let r =
                ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("上期").clicked() {
                self.period_text = p.prev().code();
                self.dirty = true;
            }
            if ui.button("下期").clicked() {
                self.period_text = p.next().code();
                self.dirty = true;
            }
            ui.separator();
            want_new = ui.button("新建").clicked();
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });
        if want_new {
            if ctx.can(Perm::Report) {
                self.new_report(ctx);
            }
        }

        match self.tab {
            CrTab::Preview => self.show_preview(ctx, ui),
            CrTab::Design => self.show_design(ctx, ui),
        }
    }

    // ------------------------- 报表列表 / 预览 -------------------------
    fn show_preview(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let list = self.list.clone();
        let mut want_del = false;
        let mut want_edit = false;
        let mut want_rename = false;
        widgets::toolbar(ui, |ui| {
            ui.label("报表");
            egui::ComboBox::from_id_salt("cr_pick")
                .selected_text(
                    self.cur
                        .as_ref()
                        .map(|c| format!("{} {}", c.key, c.name))
                        .unwrap_or_else(|| "（请选择）".to_string()),
                )
                .width(260.0)
                .show_ui(ui, |ui| {
                    for r in &list {
                        ui.selectable_value(
                            &mut self.cur_key,
                            r.key.clone(),
                            format!("{} {}", r.key, r.name),
                        );
                    }
                });
            ui.label("名称");
            widgets::text_input(ui, &mut self.rename, 200.0, "报表名称");
            want_rename = ui.button("改名").clicked();
            want_edit = ui.button("编辑").clicked();
            if ui.button("删除").clicked() {
                want_del = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                if let Some(c) = &self.cur {
                    let mut sh = crate::views::export::Sheet::new(
                        &c.name,
                        c.columns.iter().cloned().collect(),
                    );
                    for row in &self.values {
                        sh.push(row.iter().map(|v| v.fmt_plain()).collect());
                    }
                    let title = format!("{}（{}）", c.name, self.period(ctx).label());
                    match crate::views::export::run_export(&sh, &c.name, &title, mode) {
                        Ok(m) => ctx.info(m),
                        Err(e) => ctx.error(e),
                    }
                } else {
                    ctx.error("请先选择一张报表");
                }
            }
        });

        if want_rename {
            self.do_rename(ctx);
        }
        if want_edit {
            self.start_edit(ctx);
        }
        if want_del {
            let key = self.cur_key.clone();
            if key.is_empty() {
                ctx.error("请先选择一张报表");
            } else {
                let name = self
                    .cur
                    .as_ref()
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| key.clone());
                ctx.confirm_dangerous(
                    "删除自定义报表",
                    &format!("将删除报表 {key}「{name}」及其全部行公式，且不可撤销。确定继续吗？"),
                    ConfirmAction::DeleteCustomReport(key),
                    true,
                );
            }
        }

        let cur = self.cur.clone();
        match cur {
            None => widgets::empty_hint(ui, "还没有自定义报表，点「新建」开始设计"),
            Some(c) => {
                ui.add_space(4.0);
                render_report(ui, "cr_preview", &c, &self.values);
            }
        }
    }

    fn do_rename(&mut self, ctx: &mut AppCtx<'_>) {
        if !ctx.can(Perm::Report) {
            return;
        }
        let name = self.rename.trim().to_string();
        if name.is_empty() {
            ctx.error("报表名称不能为空");
            return;
        }
        let Some(mut c) = self.cur.clone() else {
            ctx.error("请先选择一张报表");
            return;
        };
        c.name = name;
        let r = mgmt::custom_save(ctx.db(), &c);
        if ctx.handle(r).is_some() {
            ctx.log("自定义报表", "改名", &format!("{} {}", c.key, c.name));
            ctx.info("已保存报表名称");
            self.dirty = true;
        }
    }

    fn start_edit(&mut self, ctx: &mut AppCtx<'_>) {
        if !ctx.can(Perm::Report) {
            return;
        }
        let Some(c) = self.cur.clone() else {
            ctx.error("请先选择一张报表");
            return;
        };
        self.columns_text = c.columns.join("|");
        self.checks.clear();
        self.draft = Some(c);
        self.draft_ver += 1;
        self.tab = CrTab::Design;
        self.dirty = true;
    }

    fn new_report(&mut self, ctx: &mut AppCtx<'_>) {
        let key = match mgmt::custom_next_key(ctx.db()) {
            Ok(k) => k,
            Err(e) => {
                ctx.error(e.to_string());
                return;
            }
        };
        let d = CustomReport::new(
            &key,
            "新报表",
            vec!["期末余额".to_string(), "上年同期".to_string()],
        );
        self.columns_text = d.columns.join("|");
        self.checks.clear();
        self.draft = Some(d);
        self.draft_ver += 1;
        self.tab = CrTab::Design;
        self.dirty = true;
    }

    // ------------------------- 设计器 -------------------------
    fn show_design(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        // 用 take / 回写的方式编辑，免得闭包里同时借用 self 的多个字段
        let Some(mut d) = self.draft.take() else {
            widgets::empty_hint(ui, "点「新建」设计一张新报表，或到「报表预览」选一张后点「编辑」");
            return;
        };

        ui.horizontal(|ui| {
            widgets::kv(ui, "报表编码", &d.key);
            ui.label("名称");
            widgets::text_input(ui, &mut d.name, 220.0, "报表名称");
        });
        ui.horizontal(|ui| {
            ui.label("列标题");
            let r = widgets::text_input(
                ui,
                &mut self.columns_text,
                360.0,
                "期末余额|上年同期|增减额",
            );
            if r.changed() {
                apply_columns(&mut d, &self.columns_text);
                self.draft_ver += 1;
            }
            ui.label(RichText::new("多个列标题用 | 分隔").weak());
        });

        widgets::card(ui, "公式语法提示", |ui| {
            for (f, desc) in [
                ("QC(\"1001\")+QC(\"1002\")", "期初余额，支持 + - * / 与括号"),
                ("QM(\"1122\")", "期末余额"),
                ("FS(\"6001\",,\"贷\")", "本期发生额，方向可写 借 / 贷，留空取借贷差额"),
                ("LFS(\"6601\")", "本年累计发生额"),
                ("QM(\"1122\")/QM(\"1122\",-1)", "第二个参数是期间偏移：0 本期、-1 上期、-12 上年同期"),
                ("JE(\"1001\")", "期末净额（恒为正）"),
            ] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(f).monospace());
                    ui.label(RichText::new(desc).weak());
                });
            }
            ui.label(RichText::new("分母为 0 时返回 0，不会报错；函数不区分大小写。").weak());
        });

        let cols = d.columns.clone();
        let ncol = cols.len().max(1);
        let mut remove: Option<usize> = None;
        let mut up: Option<usize> = None;
        let mut down: Option<usize> = None;
        let mut touched = false;

        {
            let mut tb = TableBuilder::new(ui)
                .id_salt("cr_design_rows")
                .striped(true)
                .resizable(true)
                .min_scrolled_height(120.0)
                .column(Column::initial(200.0).at_least(120.0))
                .column(Column::initial(70.0).resizable(false))
                .column(Column::initial(52.0).resizable(false));
            for _ in 0..ncol {
                tb = tb.column(Column::initial(190.0).at_least(110.0));
            }
            tb = tb.column(Column::initial(104.0).resizable(false));

            tb.header(26.0, |mut h| {
                h.col(|ui| {
                    ui.label(RichText::new("行名称").strong());
                });
                h.col(|ui| {
                    ui.label(RichText::new("缩进").strong());
                });
                h.col(|ui| {
                    ui.label(RichText::new("加粗").strong());
                });
                for c in &cols {
                    h.col(|ui| {
                        ui.label(RichText::new(c).strong());
                    });
                }
                h.col(|ui| {
                    ui.label(RichText::new("操作").strong());
                });
            })
            .body(|body| {
                body.rows(26.0, d.lines.len(), |mut row| {
                    let i = row.index();
                    row.col(|ui| {
                        ui.add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut d.lines[i].name).hint_text("行名称"),
                        );
                    });
                    row.col(|ui| {
                        // 缩进 0-3 级，直接拖拽比下拉更快
                        let r = ui.add(
                            egui::DragValue::new(&mut d.lines[i].indent)
                                .range(0..=3)
                                .suffix(" 级"),
                        );
                        if r.changed() {
                            touched = true;
                        }
                    });
                    row.col(|ui| {
                        if ui.checkbox(&mut d.lines[i].bold, "粗").changed() {
                            touched = true;
                        }
                    });
                    for ci in 0..ncol {
                        row.col(|ui| {
                            if d.lines[i].formulas.len() <= ci {
                                d.lines[i].formulas.resize(ncol, String::new());
                            }
                            let r = ui.add_sized(
                                [ui.available_width(), 22.0],
                                egui::TextEdit::singleline(&mut d.lines[i].formulas[ci])
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text("公式"),
                            );
                            // 公式输入完（失焦）才重算预览，避免每敲一个字符就查一次库
                            if r.lost_focus() {
                                touched = true;
                            }
                        });
                    }
                    row.col(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.button("↑").on_hover_text("上移").clicked() {
                            up = Some(i);
                        }
                        if ui.button("↓").on_hover_text("下移").clicked() {
                            down = Some(i);
                        }
                        if ui.button("✕").on_hover_text("删除该行").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            });
        }

        let mut want_check = false;
        let mut want_save = false;
        widgets::toolbar(ui, |ui| {
            if ui.button("加行").clicked() {
                d.lines.push(CustomLine {
                    name: "新行".to_string(),
                    indent: 0,
                    formulas: vec![String::new(); ncol],
                    bold: false,
                });
                touched = true;
            }
            want_check = ui.button("语法检查").clicked();
            want_save = ui.button("保存").clicked();
            ui.separator();
            ui.label(format!("共 {} 行 / {} 列", d.lines.len(), ncol));
        });

        // 行操作在表格渲染完之后统一执行，避免边遍历边改动
        if let Some(i) = remove {
            if i < d.lines.len() {
                d.lines.remove(i);
                touched = true;
            }
        }
        if let Some(i) = up {
            if i > 0 && i < d.lines.len() {
                d.lines.swap(i - 1, i);
                touched = true;
            }
        }
        if let Some(i) = down {
            if i + 1 < d.lines.len() {
                d.lines.swap(i, i + 1);
                touched = true;
            }
        }

        if want_check {
            self.checks = mgmt::custom_check(&d);
            if self.checks.is_empty() {
                ctx.info("公式检查通过");
            }
        }
        if !self.checks.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new("语法检查结果").strong());
            for (li, ci, msg) in &self.checks {
                ui.horizontal(|ui| {
                    ui.colored_label(palette::CREDIT, "✖");
                    ui.label(format!("第 {} 行 第 {} 列：{}", li + 1, ci + 1, msg));
                });
            }
        }

        if want_save {
            self.save_draft(ctx, &d);
        }
        if touched {
            self.draft_ver += 1;
        }
        self.draft = Some(d.clone());

        ui.add_space(6.0);
        ui.separator();
        ui.label(RichText::new("实时预览（当前期间）").strong());
        if d.lines.is_empty() {
            ui.label(RichText::new("还没有行，先点「加行」并填写公式").weak());
        } else {
            render_report(ui, "cr_design_preview", &d, &self.preview);
        }
    }

    fn save_draft(&mut self, ctx: &mut AppCtx<'_>, d: &CustomReport) {
        if !ctx.can(Perm::Report) {
            return;
        }
        if d.name.trim().is_empty() {
            ctx.error("报表名称不能为空");
            return;
        }
        self.checks = mgmt::custom_check(d);
        if !self.checks.is_empty() {
            ctx.error(format!("有 {} 处公式错误，请先修正再保存", self.checks.len()));
            return;
        }
        let r = mgmt::custom_save(ctx.db(), d);
        if ctx.handle(r).is_some() {
            ctx.log(
                "自定义报表",
                "保存报表",
                &format!("{} {}，{} 行 {} 列", d.key, d.name, d.lines.len(), d.columns.len()),
            );
            ctx.info(format!("已保存报表 {}", d.key));
            self.cur_key = d.key.clone();
            self.dirty = true;
        }
    }
}

/// 把「列标题编辑框」的文本拆成列，并同步每行公式的个数
fn apply_columns(d: &mut CustomReport, text: &str) {
    let cols: Vec<String> = text
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    d.columns = cols;
    for l in d.lines.iter_mut() {
        l.formulas.resize(d.columns.len(), String::new());
    }
}

/// 渲染一张报表：首列是行名（带缩进、可加粗），其余列是取数结果
fn render_report(ui: &mut Ui, id: &str, r: &CustomReport, values: &[Vec<Money>]) {
    let mut cols = vec![widgets::TCol::new("项目", 260.0)];
    for c in &r.columns {
        cols.push(widgets::TCol::new(c, 160.0).right());
    }
    widgets::grid(ui, id, &cols, r.lines.len(), 24.0, |i, c, ui| {
        let l = &r.lines[i];
        if c == 0 {
            let txt = format!("{}{}", "　".repeat(l.indent as usize), l.name);
            let t = RichText::new(txt);
            ui.label(if l.bold { t.strong() } else { t });
            return;
        }
        let Some(v) = values.get(i).and_then(|row| row.get(c - 1)) else {
            return;
        };
        let t = if v.is_zero() {
            RichText::new("—").weak()
        } else {
            RichText::new(v.fmt_money()).color(theme::amount_color(*v))
        };
        ui.label(if l.bold { t.strong() } else { t });
    });
}
