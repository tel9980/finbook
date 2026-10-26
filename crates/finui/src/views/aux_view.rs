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
    /// 批量粘贴窗口
    pub batch: super::aux_batch::Ui,
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
            batch: Default::default(),
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
            if ui.button("批量新增").clicked() && ctx.can(Perm::AuxEdit) {
                self.batch = Default::default();
                self.batch.open = true;
            }
            if ui
                .button("下载模板")
                .on_hover_text("保存一个 .xlsx 空表（带示例和填表说明），用 Excel 填好后再载入")
                .clicked()
                && ctx.can(Perm::AuxEdit)
            {
                let tpl = fincore::batch::aux::template(kind);
                let sheets = crate::views::export::batch_template_sheets(
                    &format!("{}档案", kind.label()),
                    &tpl,
                );
                match crate::views::export::save_template(
                    &sheets,
                    &format!("{}档案导入模板", kind.label()),
                ) {
                    Ok(Some(m)) => ctx.info(m),
                    Ok(None) => {}
                    Err(e) => ctx.error(e),
                }
            }
            // 一键预置：把「通用名」的那几条直接建出来，省得一个个手填。
            // 只对部门/银行账户显示 —— 客户、供应商、存货的名字没有通用值，
            // 预置一个假的比空着更糟（凭证里带出去就成脏数据了）。
            if fincore::auxiliary::has_preset(kind) {
                let already = self.rows.len();
                if ui
                    .button("预置常用档案")
                    .on_hover_text(format!(
                        "建{}：{}。已有的编码不会被覆盖。",
                        kind.label(),
                        fincore::auxiliary::preset_aux(kind)
                            .iter()
                            .map(|p| format!("{} {}", p.code, p.name))
                            .collect::<Vec<_>>()
                            .join("、")
                    ))
                    .clicked()
                    && ctx.can(Perm::AuxEdit)
                {
                    if already == 0 {
                        // 第一次建：直接做，不用再点一次确认 —— 内容在 hover 里写着
                        ctx.confirm(
                            &format!("预置{}常用档案", kind.label()),
                            &format!(
                                "将新建 {} 条：{}\n\
                                 这些是通用占位，建完请按你公司的情况改名或补全。",
                                fincore::auxiliary::preset_aux(kind).len(),
                                fincore::auxiliary::preset_aux(kind)
                                    .iter()
                                    .map(|p| format!("{} {}", p.code, p.name))
                                    .collect::<Vec<_>>()
                                    .join("、")
                            ),
                            ConfirmAction::PresetAux(kind),
                        );
                    } else {
                        // 已经有档案了：这一步可能覆盖，跳过确认直接做的风险更大，
                        // 走危险确认（会把「不覆盖」也说清楚）
                        ctx.confirm_dangerous(
                            &format!("预置{}常用档案", kind.label()),
                            &format!(
                                "当前{}档案已有 {already} 条，将补充缺少的 {} 条。\n\
                                 编码相同的已有档案不会被覆盖。",
                                kind.label(),
                                fincore::auxiliary::preset_aux(kind).len()
                            ),
                            ConfirmAction::PresetAux(kind),
                            true,
                        );
                    }
                }
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
                0 => {
                    ui.label(RichText::new(&e.code).monospace());
                }
                1 => {
                    ui.label(RichText::new(&e.name).color(if e.disabled {
                        ui.visuals().weak_text_color()
                    } else {
                        Color32::BLACK
                    }));
                }
                2 => {
                    ui.label(e.parent_code.as_deref().unwrap_or(""));
                }
                3 => {
                    ui.label(if e.disabled {
                        RichText::new("停用").color(palette::CREDIT)
                    } else {
                        RichText::new("启用").color(palette::OK)
                    });
                }
                4 => {
                    let ext = e.summary();
                    ui.label(
                        RichText::new(if ext.is_empty() { e.memo.clone() } else { ext }).weak(),
                    );
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

        // 批量粘贴窗口：导入成功后要刷新列表
        if self.batch.open {
            let existing = findb::auxs::codes(ctx.db(), kind).unwrap_or_default();
            if self.batch.window(ctx, kind, &existing, ui) {
                self.dirty = true;
                ctx.toast("批量导入完成，列表已刷新");
            }
        }
    }

    fn edit_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(e) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;

        egui::Window::new(if is_new {
            "新增档案"
        } else {
            "修改档案"
        })
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ui.ctx(), |ui| {
            if !self.err.is_empty() {
                ui.colored_label(palette::CREDIT, &self.err);
            }
            // 双向身份提示：这个名称已经是另一种身份的档案了
            //
            // 宏源既做外发加工（供应商），又拿料来加工（客户）。新建其中一条时
            // 如果撞上同名档案，必须当场说清 —— 否则用户不知道"另一头"也该建，
            // 首页「钱在哪」就只显示一半，净头寸是错的。
            if is_new && matches!(e.kind, AuxKind::Customer | AuxKind::Supplier) && !e.name.trim().is_empty() {
                if let Ok(Some(other)) = findb::auxs::find_other_role_by_name(ctx.db(), e.kind, e.name.trim()) {
                    let other_label = match other.kind {
                        AuxKind::Supplier => "供应商",
                        AuxKind::Customer => "客户",
                        _ => "档案",
                    };
                    ui.colored_label(
                        palette::WARN,
                        RichText::new(format!(
                            "↔ 这家已经是你的{}（编码 {}），建好后他既是客户又是供应商，首页会按净额显示",
                            other_label, other.code
                        ))
                        .size(12.0),
                    );
                    ui.add_space(4.0);
                }
            }
            egui::Grid::new("aux_edit")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("编码：");
                    ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut e.code));
                    ui.end_row();
                    ui.label("");
                    ui.label(
                        RichText::new("编码留空自动编号（接着最大号往下排）")
                            .weak()
                            .size(12.0),
                    );
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
                        e.set_prop(fincore::auxiliary::prop::BANK_NAME, bn.trim().to_string());
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
                    // 税点：客户和供应商都要，一客一率是该行业常态
                    if matches!(e.kind, AuxKind::Customer | AuxKind::Supplier) {
                        let mut tr = e
                            .prop(fincore::auxiliary::prop::TAX_RATE)
                            .cloned()
                            .unwrap_or_default();
                        ui.label("税点 %（空=默认）：");
                        ui.add_sized(
                            [220.0, 22.0],
                            egui::TextEdit::singleline(&mut tr)
                                .hint_text("如 3 / 6 / 13，不要票填 0"),
                        );
                        e.set_prop(fincore::auxiliary::prop::TAX_RATE, tr.trim().to_string());
                        ui.end_row();

                        let mut pol = e
                            .prop(fincore::auxiliary::prop::TAX_RATE_POLICY)
                            .cloned()
                            .unwrap_or_default();
                        ui.label("开票情况：");
                        egui::ComboBox::from_id_salt("aux_tax_policy")
                            .selected_text(match pol.as_str() {
                                "invoice" => "每次都开票",
                                "none" => "从不开票",
                                "sometimes" => "有时开有时不开",
                                _ => "未设置",
                            })
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut pol, String::new(), "未设置");
                                ui.selectable_value(&mut pol, "invoice".to_string(), "每次都开票");
                                ui.selectable_value(
                                    &mut pol,
                                    "sometimes".to_string(),
                                    "有时开有时不开",
                                );
                                ui.selectable_value(&mut pol, "none".to_string(), "从不开票");
                            });
                        e.set_prop(fincore::auxiliary::prop::TAX_RATE_POLICY, pol);
                        ui.end_row();
                        ui.label("");
                        ui.label(
                            RichText::new("税点按客户/供应商单独记录，开票时按它算税额")
                                .weak()
                                .size(12.0),
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
            let mut e = self.editing.clone().unwrap();
            self.err.clear();
            if e.code.trim().is_empty() {
                // 留空自动编号：与批量粘贴同一规则（库里最大号 +1）
                let all: Vec<String> = findb::auxs::codes(ctx.db(), e.kind).unwrap_or_default();
                e.code = fincore::batch::aux::next_code(e.kind, &all);
                self.editing = Some(e.clone());
            }
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
                            if self.editing_new {
                                "新增档案"
                            } else {
                                "修改档案"
                            },
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
        match crate::views::export::run_export(&sh, &name, &name, mode, &ctx.db().options()) {
            Ok(m) => ctx.info(m),
            Err(x) => ctx.error(x),
        }
    }
}
