//! 辅助核算档案（客户 / 供应商 / 部门 / 职员 / 项目 / 存货 / 银行账户）

use egui::{Color32, RichText, Ui};
use fincore::{AuxEntity, AuxKind, AuxQuery, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

pub struct AuxView {
    pub kind: AuxKind,
    pub kw: String,
    pub show_disabled: bool,
    pub rows: Vec<AuxEntity>,
    pub editing: Option<AuxEntity>,
    pub editing_new: bool,
    pub dirty: bool,
    pub err: String,
}

impl Default for AuxView {
    fn default() -> Self {
        Self {
            kind: AuxKind::Customer,
            kw: String::new(),
            show_disabled: true,
            rows: Vec::new(),
            editing: None,
            editing_new: false,
            dirty: true,
            err: String::new(),
        }
    }
}

impl AuxView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>, kind: AuxKind) {
        if kind != self.kind {
            self.kind = kind;
            self.dirty = true;
        }
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let kw = self.kw.trim().to_string();
        let q = AuxQuery::kind(kind)
            .with_disabled(self.show_disabled)
            .with_keyword(&kw);
        self.rows = findb::auxs::list(ctx.db(), &q).unwrap_or_default();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, kind: AuxKind) {
        self.reload(ctx, kind);
        let can_edit = ctx.user().can(Perm::AuxEdit);

        widgets::page_header(ui, &format!("{}档案", kind.label()), |ui| {
            ui.label(RichText::new(format!("共 {} 条", self.rows.len())).weak());
        });

        widgets::toolbar(ui, |ui| {
            if ui.button("新增").clicked() && ctx.can(Perm::AuxEdit) {
                self.editing = Some(AuxEntity::new(kind, String::new(), String::new()));
                self.editing_new = true;
                self.err.clear();
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            ui.label("查找");
            let r = ui.add_sized(
                [150.0, 22.0],
                egui::TextEdit::singleline(&mut self.kw).hint_text("编码或名称"),
            );
            if r.changed() {
                self.dirty = true;
            }
            ui.checkbox(&mut self.show_disabled, "显示停用");
            ui.separator();
            if ui.button("导入内置现金流量项目").clicked() && kind == AuxKind::CashFlow {
                ctx.confirm(
                    "导入现金流量项目",
                    "将导入企业会计准则现金流量表标准项目，已存在的编码会被覆盖。",
                    ConfirmAction::ImportCashFlowItems,
                );
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("编码", 120.0).fixed(),
            widgets::TCol::new("名称", 260.0),
            widgets::TCol::new("上级编码", 120.0).fixed(),
            widgets::TCol::new("状态", 60.0).fixed(),
            widgets::TCol::new("备注", 240.0),
            widgets::TCol::new("操作", 100.0).fixed(),
        ];
        let mut del: Option<i64> = None;
        widgets::grid(ui, "aux_list", &cols, rows.len(), 24.0, |i, c, ui| {
            let e = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&e.code).monospace()); }
                1 => {
                    ui.label(RichText::new(&e.name).color(if e.disabled {
                        ui.visuals().weak_text_color()
                    } else {
                        Color32::BLACK
                    }));
                }
                2 => { ui.label(e.parent_code.as_deref().unwrap_or("")); }
                3 => {
                    ui.label(if e.disabled {
                        RichText::new("停用").color(palette::CREDIT)
                    } else {
                        RichText::new("启用").color(palette::OK)
                    });
                }
                4 => {
                    let ext = e.summary();
                    ui.label(RichText::new(if ext.is_empty() { e.memo.clone() } else { ext }).weak());
                }
                5 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改").clicked() && can_edit {
                            self.editing = Some(e.clone());
                            self.editing_new = false;
                            self.err.clear();
                        }
                        if ui.small_button("删").clicked() && can_edit {
                            del = Some(e.id);
                        }
                    });
                }
                _ => {}
            }
        });
        if let Some(id) = del {
            ctx.confirm_dangerous(
                "删除档案",
                "已被引用的档案无法删除，只能停用。确定删除吗？",
                ConfirmAction::DeleteAux(id),
                true,
            );
        }

        self.edit_window(ctx, ui);
    }

    fn edit_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(e) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;

        egui::Window::new(if is_new { "新增档案" } else { "修改档案" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.err);
                }
                egui::Grid::new("aux_edit")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("编码：");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut e.code));
                        ui.end_row();
                        ui.label("名称：");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut e.name));
                        ui.end_row();
                        if e.kind == AuxKind::Bank {
                            let mut bn = e
                                .prop(fincore::auxiliary::prop::BANK_NAME)
                                .cloned()
                                .unwrap_or_default();
                            ui.label("开户行：");
                            ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut bn));
                            e.set_prop(
                                fincore::auxiliary::prop::BANK_NAME,
                                bn.trim().to_string(),
                            );
                            ui.end_row();
                            let mut ba = e
                                .prop(fincore::auxiliary::prop::BANK_ACCOUNT)
                                .cloned()
                                .unwrap_or_default();
                            ui.label("账号：");
                            ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut ba));
                            e.set_prop(
                                fincore::auxiliary::prop::BANK_ACCOUNT,
                                ba.trim().to_string(),
                            );
                            ui.end_row();
                        }
                        if e.kind == AuxKind::Customer {
                            let mut cl = e
                                .prop(fincore::auxiliary::prop::CREDIT_LIMIT)
                                .cloned()
                                .unwrap_or_default();
                            ui.label("信用额度（0=不限）：");
                            ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut cl));
                            e.set_prop(
                                fincore::auxiliary::prop::CREDIT_LIMIT,
                                cl.trim().to_string(),
                            );
                            ui.end_row();
                        }
                        ui.label("上级编码：");
                        let mut p = e.parent_code.clone().unwrap_or_default();
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut p));
                        e.parent_code = if p.trim().is_empty() { None } else { Some(p) };
                        ui.end_row();
                        ui.label("备注：");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut e.memo));
                        ui.end_row();
                    });
                ui.checkbox(&mut e.disabled, "停用");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui.button("保存").clicked() {
                            save = true;
                        }
                    });
                });
            });

        if close || !open {
            self.editing = None;
            self.err.clear();
            return;
        }
        if save {
            let e = self.editing.clone().unwrap();
            self.err.clear();
            let existing: Vec<String> = self
                .rows
                .iter()
                .filter(|x| x.id != e.id)
                .map(|x| x.code.clone())
                .collect();
            let errs = fincore::auxiliary::validate_aux(&e, &existing);
            if errs.is_empty() {
                let r = if self.editing_new {
                    findb::auxs::insert(ctx.db(), &e).map(|_| ())
                } else {
                    findb::auxs::update(ctx.db(), &e)
                };
                match r {
                    Ok(()) => {
                        ctx.log(
                            "档案",
                            if self.editing_new { "新增档案" } else { "修改档案" },
                            &format!("{} {} {}", e.kind.label(), e.code, e.name),
                        );
                        ctx.info("已保存");
                        ctx.reload_aux_names();
                        self.dirty = true;
                        self.editing = None;
                    }
                    Err(x) => self.err = x.to_string(),
                }
            } else {
                self.err = errs.join("；");
            }
        }
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        if self.rows.is_empty() {
            ctx.error("没有可导出的数据");
            return;
        }
        let mut sh = crate::views::export::Sheet::new(
            &format!("{}档案", self.kind.label()),
            vec![
                "编码".into(),
                "名称".into(),
                "上级编码".into(),
                "状态".into(),
                "备注".into(),
            ],
        );
        for e in &self.rows {
            sh.push(vec![
                e.code.clone(),
                e.name.clone(),
                e.parent_code.clone().unwrap_or_default(),
                if e.disabled { "停用" } else { "启用" }.to_string(),
                e.memo.clone(),
            ]);
        }
        let name = format!("{}档案", self.kind.label());
        match crate::views::export::run_export(&sh, &name, &name, mode) {
            Ok(m) => ctx.info(m),
            Err(x) => ctx.error(x),
        }
    }
}
