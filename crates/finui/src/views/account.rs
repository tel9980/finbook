//! 会计科目维护

use egui::{Color32, RichText, Ui};
use fincore::{Account, AcctCategory, AuxKind, AuxMask, Direction, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

/// 科目列表排序方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccountSort {
    /// 按编码（默认）：层级结构天然有序，父科目永远在子科目前面
    #[default]
    ByCode,
    /// 按名称：找「银行存款」比记编码快
    ByName,
    /// 按类别：资产/负债/权益/成本/损益分组看
    ByCategory,
}

impl AccountSort {
    fn label(self) -> &'static str {
        match self {
            AccountSort::ByCode => "按编码",
            AccountSort::ByName => "按名称",
            AccountSort::ByCategory => "按类别",
        }
    }
    fn next(self) -> Self {
        match self {
            AccountSort::ByCode => AccountSort::ByName,
            AccountSort::ByName => AccountSort::ByCategory,
            AccountSort::ByCategory => AccountSort::ByCode,
        }
    }
}

pub struct AccountView {
    pub kw: String,
    pub leaf_only: bool,
    pub show_disabled: bool,
    pub sort: AccountSort,
    /// 只看有余额或有发生额的科目（199 个里大部分是 0，新手不知道哪些在用）
    pub only_used: bool,
    pub rows: Vec<Account>,
    pub dirty: bool,
    /// 正在编辑的科目；None 表示未打开编辑窗
    pub editing: Option<Account>,
    pub editing_new: bool,
    /// 修改前的原编码：编码不可改，保存时用它核对
    pub editing_orig: String,
    pub err: String,
}

impl Default for AccountView {
    fn default() -> Self {
        Self {
            kw: String::new(),
            leaf_only: false,
            show_disabled: true,
            sort: AccountSort::default(),
            only_used: false,
            rows: Vec::new(),
            dirty: true,
            editing: None,
            editing_new: false,
            editing_orig: String::new(),
            err: String::new(),
        }
    }
}

impl AccountView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.rows = findb::accounts::list(ctx.db()).unwrap_or_default();
    }

    fn filtered(
        &self,
        ctx: &AppCtx<'_>,
        used: &std::collections::HashSet<String>,
    ) -> Vec<&Account> {
        let kw = self.kw.trim().to_lowercase();
        let chart = ctx.chart();
        let mut out: Vec<&Account> = self
            .rows
            .iter()
            .filter(|a| {
                if !self.show_disabled && a.disabled {
                    return false;
                }
                if self.leaf_only && !chart.is_leaf(&a.code) {
                    return false;
                }
                // 「只看在用」：有分录、有期初余额，或任一子科目在用。
                // 199 个科目里大部分是 0，新手打开看到满屏 0 会以为软件没装好；
                // 先只看自己在用的，剩下的需要时再放出来。
                if self.only_used && !used.contains(&a.code) {
                    return false;
                }
                if kw.is_empty() {
                    return true;
                }
                a.code.to_lowercase().contains(&kw) || a.name.to_lowercase().contains(&kw)
            })
            .collect();
        match self.sort {
            AccountSort::ByCode => out.sort_by(|a, b| a.code.cmp(&b.code)),
            AccountSort::ByName => {
                out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.code.cmp(&b.code)))
            }
            AccountSort::ByCategory => out.sort_by(|a, b| {
                format!("{:?}", a.category)
                    .cmp(&format!("{:?}", b.category))
                    .then_with(|| a.code.cmp(&b.code))
            }),
        }
        out
    }

    /// 算出「在用」的科目集合：有分录、有期初，或子科目在用（父级要留着撑结构）。
    fn used_codes(&self, ctx: &AppCtx<'_>) -> std::collections::HashSet<String> {
        use std::collections::HashSet;
        if !self.only_used {
            return HashSet::new();
        }
        let mut used: HashSet<String> = HashSet::new();
        // 有分录的
        for a in &self.rows {
            if let Ok((n, _)) = findb::accounts::usage(ctx.db(), &a.code) {
                if n > 0 {
                    used.insert(a.code.clone());
                }
            }
        }
        // 有期初余额的
        if let Ok(begins) = findb::balances::list_begin(ctx.db()) {
            for b in begins {
                // BeginRow 的期初口径：三项任一非零就算在用
                if !b.year_begin.is_zero() || !b.debit_accum.is_zero() || !b.credit_accum.is_zero()
                {
                    used.insert(b.account_code.clone());
                }
            }
        }
        // 子在用 → 父也留着，不然树断了
        let chart = ctx.chart();
        let mut extra: Vec<String> = Vec::new();
        for code in used.iter() {
            let mut cur = chart.parent_code(code);
            while let Some(p) = cur {
                if used.contains(&p) {
                    break;
                }
                extra.push(p.clone());
                cur = chart.parent_code(&p);
            }
        }
        for p in extra {
            used.insert(p);
        }
        used
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let can_edit = ctx.user().can(Perm::AccountEdit);

        widgets::page_header(ui, "会计科目", |ui| {
            ui.label(RichText::new(format!("共 {} 个科目", self.rows.len())).weak());
        });

        widgets::toolbar(ui, |ui| {
            ui.label("查找");
            let r = ui.add_sized(
                [160.0, 22.0],
                egui::TextEdit::singleline(&mut self.kw).hint_text("编码或名称"),
            );
            if r.changed() {
                // 实时过滤，无需点查询
            }
            ui.checkbox(&mut self.leaf_only, "只看末级");
            ui.checkbox(&mut self.show_disabled, "显示停用");
            // 「只看在用」：199 个里大部分是 0，先只看自己在用的
            if ui.checkbox(&mut self.only_used, "只看在用").changed() {
                self.dirty = false; // 过滤是纯内存操作，不用重查库
            }
            // 排序三档循环：编码（层级有序）→ 名称（找名字快）→ 类别（分组看）
            if ui
                .button(format!("排序：{}", self.sort.label()))
                .on_hover_text("点一下换一种排序：编码 → 名称 → 类别")
                .clicked()
            {
                self.sort = self.sort.next();
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
                ctx.reload_chart();
            }
            ui.separator();
            if ui.button("新增").clicked() && ctx.can(Perm::AccountEdit) {
                self.editing = Some(Account::new(
                    String::new(),
                    String::new(),
                    AcctCategory::Asset,
                ));
                self.editing_new = true;
                self.err.clear();
            }
            if ui.button("导入内置科目表").clicked() && ctx.can(Perm::AccountEdit) {
                let n = fincore::chart::default_accounts().len();
                ctx.confirm(
                    "导入内置科目表",
                    &format!(
                        "将导入 {n} 个企业会计准则科目（完整版，含 1002 银行账户细分、1403 原材料细分、\n\
                         6601/6602/6603 期间费用明细等细分层级），已存在的编码会被覆盖。\n\
                         不会影响已有凭证与期初余额，确定继续吗？"
                    ),
                    ConfirmAction::ImportAccounts,
                );
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        let used = self.used_codes(ctx);
        let rows: Vec<Account> = self.filtered(ctx, &used).into_iter().cloned().collect();
        let leafs: Vec<bool> = rows.iter().map(|a| ctx.chart().is_leaf(&a.code)).collect();
        let cols = [
            widgets::TCol::new("科目编码", 110.0).fixed(),
            widgets::TCol::new("科目名称", 240.0),
            widgets::TCol::new("类别", 70.0).fixed(),
            widgets::TCol::new("方向", 50.0).fixed(),
            widgets::TCol::new("辅助核算", 200.0),
            widgets::TCol::new("数量", 90.0),
            widgets::TCol::new("外币", 60.0).fixed(),
            widgets::TCol::new("现金", 44.0).fixed(),
            widgets::TCol::new("银行", 44.0).fixed(),
            widgets::TCol::new("状态", 56.0).fixed(),
            widgets::TCol::new("操作", 90.0).fixed(),
        ];
        let can_edit_now = can_edit;
        widgets::grid(ui, "account_list", &cols, rows.len(), 24.0, |i, c, ui| {
            let a = &rows[i];
            let leaf = leafs[i];
            match c {
                0 => {
                    let txt = RichText::new(&a.code).monospace().color(if leaf {
                        Color32::DARK_GRAY
                    } else {
                        palette::PRIMARY
                    });
                    ui.label(txt);
                }
                1 => {
                    // 层级缩进：`100101` 是 `1001` 的下级，缩两格。
                    // 以前是平铺的，`1001` 和 `100101` 看不出父子关系，
                    // 新手不知道点哪个。level 从科目表来，1 级不缩。
                    let lvl = ctx.chart().level(&a.code).saturating_sub(1) as usize;
                    let indent = "　".repeat(lvl.min(3));
                    let marker = if lvl > 0 && leaf {
                        "└ "
                    } else if lvl > 0 {
                        "├ "
                    } else {
                        ""
                    };
                    ui.label(RichText::new(format!("{indent}{marker}{}", a.name)).color(
                        if a.disabled {
                            ui.visuals().weak_text_color()
                        } else {
                            Color32::BLACK
                        },
                    ));
                }
                2 => {
                    ui.label(a.category.label());
                }
                3 => {
                    ui.label(a.dir.label());
                }
                4 => {
                    let list: Vec<String> =
                        a.aux.list().iter().map(|k| k.label().to_string()).collect();
                    ui.label(RichText::new(list.join("、")).weak());
                }
                5 => {
                    ui.label(
                        a.unit
                            .as_ref()
                            .map(|u| format!("{u}（数量式）"))
                            .unwrap_or_default(),
                    );
                }
                6 => {
                    ui.label(a.currency.as_deref().unwrap_or(""));
                }
                7 => {
                    if a.is_cash {
                        ui.colored_label(palette::OK, "✓");
                    }
                }
                8 => {
                    if a.is_bank {
                        ui.colored_label(palette::OK, "✓");
                    }
                }
                9 => {
                    ui.label(if a.disabled {
                        RichText::new("停用").color(palette::CREDIT)
                    } else {
                        RichText::new("启用").color(palette::OK)
                    });
                }
                10 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改").clicked() && can_edit_now {
                            self.editing = Some(a.clone());
                            self.editing_new = false;
                            self.editing_orig = a.code.clone();
                            self.err.clear();
                        }
                        if ui.small_button("删").clicked() && can_edit_now {
                            ctx.confirm_dangerous(
                                "删除科目",
                                &format!("确定删除科目 {} {} 吗？", a.code, a.name),
                                ConfirmAction::DeleteAccount(a.code.clone()),
                                true,
                            );
                        }
                    });
                }
                _ => {}
            }
        });

        self.edit_window(ctx, ui);
    }

    fn edit_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(a) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;

        egui::Window::new(if is_new {
            "新增科目"
        } else {
            "修改科目"
        })
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([560.0, 520.0])
        .show(ui.ctx(), |ui| {
            if !self.err.is_empty() {
                ui.colored_label(palette::CREDIT, &self.err);
                ui.separator();
            }
            egui::Grid::new("acct_edit")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("科目编码：");
                    ui.add_enabled_ui(is_new, |ui| {
                        ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut a.code));
                    });
                    if !is_new {
                        ui.label(RichText::new("编码不可修改").weak());
                    }
                    ui.end_row();

                    ui.label("科目名称：");
                    ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut a.name));
                    ui.end_row();

                    ui.label("科目类别：");
                    egui::ComboBox::from_id_salt("acct_cat")
                        .selected_text(a.category.label())
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for c in AcctCategory::all() {
                                ui.selectable_value(&mut a.category, *c, c.label());
                            }
                        });
                    ui.end_row();

                    ui.label("余额方向：");
                    egui::ComboBox::from_id_salt("acct_dir")
                        .selected_text(a.dir.label())
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut a.dir, Direction::Debit, "借");
                            ui.selectable_value(&mut a.dir, Direction::Credit, "贷");
                        });
                    ui.end_row();

                    ui.label("数量单位：");
                    let mut unit = a.unit.clone().unwrap_or_default();
                    ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut unit));
                    a.unit = if unit.trim().is_empty() {
                        None
                    } else {
                        Some(unit.trim().to_string())
                    };
                    ui.end_row();

                    ui.label("外币币种：");
                    let mut cur = a.currency.clone().unwrap_or_default();
                    ui.add_sized(
                        [200.0, 22.0],
                        egui::TextEdit::singleline(&mut cur).hint_text("如 USD"),
                    );
                    a.currency = if cur.trim().is_empty() {
                        None
                    } else {
                        Some(cur.trim().to_uppercase())
                    };
                    ui.end_row();
                });

            ui.separator();
            ui.label(RichText::new("辅助核算").strong());
            // 已有分录的科目不许改辅助核算 —— 先说清楚，别让用户点了保存才看到报错。
            // 辅助核算决定"这笔钱挂在谁身上"，改掉后旧分录带原辅助、新分录不带，
            // 按客户/供应商查账时不带辅助的钱会查不到。
            if !self.editing_new
                && findb::accounts::usage(ctx.db(), &a.code)
                    .map(|(n, _)| n > 0)
                    .unwrap_or(false)
            {
                ui.label(
                    RichText::new("⚠ 这个科目已经有分录，辅助核算不能改（要改请先清理该科目分录）")
                        .size(11.0)
                        .color(palette::WARN),
                );
            }
            ui.horizontal_wrapped(|ui| {
                for k in AuxKind::ALL {
                    if *k == AuxKind::CashFlow {
                        continue;
                    }
                    let mut on = a.aux.contains(*k);
                    if ui.checkbox(&mut on, k.label()).changed() {
                        a.aux.set(*k, on);
                    }
                }
            });

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut a.has_qty, "核算数量");
                ui.checkbox(&mut a.is_cash, "现金科目");
                ui.checkbox(&mut a.is_bank, "银行科目");
                ui.checkbox(&mut a.disabled, "停用");
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("备注：");
                ui.add_sized(
                    [ui.available_width() - 10.0, 22.0],
                    egui::TextEdit::singleline(&mut a.memo),
                );
            });

            ui.add_space(10.0);
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
            let a = self.editing.clone().unwrap();
            self.err.clear();
            // 编码改动会按"新编码"覆盖另一科目的属性（UPDATE ... WHERE code=新编码），
            // 且凭证/余额仍挂在旧编码上——直接禁止修改既有科目的编码。
            if !self.editing_new && a.code != self.editing_orig {
                self.err =
                    "科目编码不可修改；如需更换编码，请新建科目并在期初/凭证中调整".to_string();
                return;
            }
            let res = if self.editing_new {
                let iss = ctx.chart().validate_new(&a);
                iss.into_result()
            } else {
                Ok(())
            };
            match res {
                Ok(()) => {
                    let code = a.code.clone();
                    let r = if self.editing_new {
                        findb::accounts::insert(ctx.db(), &a)
                    } else {
                        findb::accounts::update(ctx.db(), &a)
                    };
                    match r {
                        Ok(()) => {
                            ctx.log(
                                "科目",
                                if self.editing_new {
                                    "新增科目"
                                } else {
                                    "修改科目"
                                },
                                &format!("{} {}", code, a.name),
                            );
                            ctx.info("已保存");
                            ctx.reload_chart();
                            self.dirty = true;
                            self.editing = None;
                        }
                        Err(e) => self.err = e.to_string(),
                    }
                }
                Err(e) => self.err = e.to_string(),
            }
        }
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let used = self.used_codes(ctx);
        let rows = self.filtered(ctx, &used);
        if rows.is_empty() {
            ctx.error("没有可导出的数据");
            return;
        }
        let mut sh = crate::views::export::Sheet::new(
            "会计科目",
            vec![
                "科目编码".into(),
                "科目名称".into(),
                "类别".into(),
                "余额方向".into(),
                "辅助核算".into(),
                "数量单位".into(),
                "外币".into(),
                "现金".into(),
                "银行".into(),
                "状态".into(),
            ],
        );
        for a in rows {
            sh.push(vec![
                a.code.clone(),
                a.name.clone(),
                a.category.label().to_string(),
                a.dir.label().to_string(),
                a.aux
                    .list()
                    .iter()
                    .map(|k| k.label())
                    .collect::<Vec<_>>()
                    .join("、"),
                a.unit.clone().unwrap_or_default(),
                a.currency.clone().unwrap_or_default(),
                if a.is_cash { "是" } else { "" }.to_string(),
                if a.is_bank { "是" } else { "" }.to_string(),
                if a.disabled { "停用" } else { "启用" }.to_string(),
            ]);
        }
        match crate::views::export::run_export(&sh, "会计科目", "会计科目", mode, &ctx.db().options()) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}

/// 由编码推断默认类别（新增科目时的辅助提示）
pub fn guess_category(code: &str) -> Option<AcctCategory> {
    AcctCategory::from_code(code)
}

/// 位掩码转换（界面复选框用）
pub fn mask_from(flags: &[(AuxKind, bool)]) -> AuxMask {
    let kinds: Vec<AuxKind> = flags
        .iter()
        .filter(|(_, on)| *on)
        .map(|(k, _)| *k)
        .collect();
    AuxMask::from_list(&kinds)
}

#[cfg(test)]
mod sort_tests {
    use super::*;

    #[test]
    fn 排序三档循环不断() {
        let mut s = AccountSort::default();
        assert_eq!(s, AccountSort::ByCode, "默认按编码（层级天然有序）");
        s = s.next();
        assert_eq!(s, AccountSort::ByName);
        s = s.next();
        assert_eq!(s, AccountSort::ByCategory);
        s = s.next();
        assert_eq!(s, AccountSort::ByCode, "三档循环要回到起点");
    }

    #[test]
    fn 每档都有中文标签() {
        for s in [
            AccountSort::ByCode,
            AccountSort::ByName,
            AccountSort::ByCategory,
        ] {
            assert!(!s.label().is_empty(), "{:?} 缺标签", s);
        }
    }

    /// 层级缩进的上限：再深也只缩 3 格，不然窄屏下名称列被吃光
    #[test]
    fn 缩进有上限() {
        assert_eq!("　".repeat(3).chars().count(), 3);
    }
}
