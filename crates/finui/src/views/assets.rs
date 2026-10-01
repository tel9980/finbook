//! 固定资产：卡片管理、折旧计提、折旧明细、资产台账
//!
//! 卡片只存静态属性（原值、年限、方法、开始期间），每月该提多少由 `planned_dep` 现算，
//! 所以这里任何一页都不缓存"月折旧额"这种派生值，改了卡片立刻就能看到新结果。

use std::collections::HashMap;

use egui::{RichText, Ui};
use fincore::engine::depreciation::DepMethod;
use fincore::{Money, Period, Perm};
use findb::assets::{Asset, AssetLedgerRow, AssetStatus, DepRecord};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Cards,
    Accrue,
    Detail,
    Ledger,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Cards => "卡片列表",
            Tab::Accrue => "折旧计提",
            Tab::Detail => "折旧明细",
            Tab::Ledger => "资产台账",
        }
    }
    /// 缓存 key 的一部分：切 Tab 要重新取数
    fn index(self) -> u8 {
        match self {
            Tab::Cards => 0,
            Tab::Accrue => 1,
            Tab::Detail => 2,
            Tab::Ledger => 3,
        }
    }
}

/// 本期应计提预览行（计提页与卡片页的"月折旧额"共用）
struct PlanRow {
    asset_id: i64,
    code: String,
    name: String,
    dept: String,
    expense: String,
    dep_account: String,
    /// 本期应提
    amount: Money,
    /// 提完本期后的累计折旧
    accum: Money,
    /// 提完本期后的净值
    net: Money,
}

pub struct AssetsView {
    pub tab: Tab,
    pub period_text: String,
    /// 全部卡片（任何一页都要用它做编码 → 名称的还原）
    pub assets: Vec<Asset>,
    /// 台账行（卡片页的累计折旧 / 净值、台账页本身）
    pub ledger: Vec<AssetLedgerRow>,
    /// 本期已计提记录
    pub deps: Vec<DepRecord>,
    plan: Vec<PlanRow>,
    /// 编辑窗口内容
    pub editing: Option<Asset>,
    pub editing_new: bool,
    f_original: String,
    /// 残值率按百分数录入（5 表示 5%），与卡片里的 0.05 差一个量级，别混
    f_residual: String,
    f_life: String,
    f_start: String,
    pub err: String,
    pub dirty: bool,
    key: String,
}

impl Default for AssetsView {
    fn default() -> Self {
        Self {
            tab: Tab::Cards,
            period_text: String::new(),
            assets: Vec::new(),
            ledger: Vec::new(),
            deps: Vec::new(),
            plan: Vec::new(),
            editing: None,
            editing_new: false,
            f_original: String::new(),
            f_residual: String::new(),
            f_life: String::new(),
            f_start: String::new(),
            err: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl AssetsView {
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
        let key = format!("{}|{}", p.ymm(), self.tab.index());
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        self.ledger.clear();
        self.deps.clear();

        let r = findb::assets::list(ctx.db());
        self.assets = ctx.handle(r).unwrap_or_default();

        match self.tab {
            Tab::Cards => {
                let r = findb::assets::ledger(ctx.db(), p);
                self.ledger = ctx.handle(r).unwrap_or_default();
                // 卡片页要显示"月折旧额"，也在本期应提清单里
                self.load_plan(ctx, p);
            }
            Tab::Ledger => {
                let r = findb::assets::ledger(ctx.db(), p);
                self.ledger = ctx.handle(r).unwrap_or_default();
            }
            Tab::Accrue => self.load_plan(ctx, p),
            Tab::Detail => {
                let r = findb::assets::dep_list_period(ctx.db(), p);
                self.deps = ctx.handle(r).unwrap_or_default();
            }
        }
    }

    /// 本期应计提清单。已落库的按落库值显示（手工调过折旧时要以库里为准），
    /// 没落库的用"前期累计 + 本期应提"推算，用户点按钮前就能看到提完的样子。
    fn load_plan(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let assets = self.assets.clone();
        let mut plan = Vec::new();
        for a in &assets {
            let amount = findb::assets::planned_dep(a, p)
                .unwrap_or(None)
                .unwrap_or(Money::ZERO);
            let booked = findb::assets::dep_of(ctx.db(), a.id, p).unwrap_or(None);
            let accum = match booked {
                Some(r) => r.accum,
                None => findb::assets::accum_before(ctx.db(), a.id, p).unwrap_or(Money::ZERO) + amount,
            };
            if amount.is_zero() && !a.should_depreciate(p) {
                continue;
            }
            plan.push(PlanRow {
                asset_id: a.id,
                code: a.code.clone(),
                name: a.name.clone(),
                dept: a.dept.clone(),
                expense: a.expense_account.clone(),
                dep_account: a.dep_account.clone(),
                amount,
                accum,
                net: a.original_value - accum,
            });
        }
        self.plan = plan;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        let total_original: Money = self.assets.iter().map(|a| a.original_value).sum();

        widgets::page_header(ui, "固定资产", |ui| {
            ui.label(
                RichText::new(format!(
                    "共 {} 张卡片 · 原值合计 {}",
                    self.assets.len(),
                    total_original.fmt_money()
                ))
                .weak(),
            );
        });

        widgets::toolbar(ui, |ui| {
            for t in [Tab::Cards, Tab::Accrue, Tab::Detail, Tab::Ledger] {
                ui.selectable_value(&mut self.tab, t, t.label());
            }
            ui.separator();
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
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
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        match self.tab {
            Tab::Cards => self.show_cards(ctx, ui, p),
            Tab::Accrue => self.show_accrue(ctx, ui, p),
            Tab::Detail => self.show_detail(ctx, ui, p),
            Tab::Ledger => self.show_ledger(ui, p),
        }

        self.edit_window(ctx, ui);
    }

    // ------------------------- 卡片列表 -------------------------
    fn show_cards(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        let can_edit = ctx.user().can(Perm::AccountEdit);
        ui.horizontal(|ui| {
            if ui.button("新增").clicked() && can_edit {
                self.open_new(ctx);
            }
            ui.label(RichText::new(format!("期间：{}", p.label())).weak());
        });
        ui.add_space(4.0);

        // 台账与应提清单都按 asset_id 索引，避免在渲染闭包里逐行查库
        let mut acc: HashMap<i64, (Money, Money, i32)> = HashMap::new();
        for r in &self.ledger {
            acc.insert(r.asset.id, (r.accum, r.net, r.months));
        }
        let mut monthly: HashMap<i64, Money> = HashMap::new();
        for r in &self.plan {
            monthly.insert(r.asset_id, r.amount);
        }

        let rows = self.assets.clone();
        let cols = [
            widgets::TCol::new("资产编码", 96.0).fixed(),
            widgets::TCol::new("资产名称", 170.0),
            widgets::TCol::new("类别", 110.0),
            widgets::TCol::new("使用部门", 110.0),
            widgets::TCol::new("原值", 120.0).right(),
            widgets::TCol::new("残值率", 76.0).right(),
            widgets::TCol::new("使用月数", 84.0).right(),
            widgets::TCol::new("已提月数", 84.0).right(),
            widgets::TCol::new("月折旧额", 110.0).right(),
            widgets::TCol::new("累计折旧", 120.0).right(),
            widgets::TCol::new("净值", 120.0).right(),
            widgets::TCol::new("状态", 70.0).fixed(),
            widgets::TCol::new("操作", 96.0).fixed(),
        ];
        let mut edit: Option<i64> = None;
        let mut del: Option<i64> = None;
        widgets::grid(ui, "asset_cards", &cols, rows.len(), 24.0, |i, c, ui| {
            let a = &rows[i];
            let (accum, net, months) = acc.get(&a.id).copied().unwrap_or((Money::ZERO, a.original_value, 0));
            match c {
                0 => { ui.label(RichText::new(&a.code).monospace()); }
                1 => { ui.label(&a.name); }
                2 => { ui.label(&a.category); }
                3 => { ui.label(&a.dept); }
                4 => widgets::amount_label(ui, a.original_value),
                5 => { ui.label(format!("{}%", (a.residual_rate * 100).round2().fmt_money())); }
                6 => { ui.label(a.life_months.to_string()); }
                7 => { ui.label(months.to_string()); }
                8 => widgets::amount_label(ui, monthly.get(&a.id).copied().unwrap_or(Money::ZERO)),
                9 => widgets::amount_label(ui, accum),
                10 => widgets::amount_label(ui, net),
                11 => {
                    let (t, color) = match a.status {
                        AssetStatus::InUse => ("在用", palette::OK),
                        AssetStatus::Idle => ("停用", palette::WARN),
                        AssetStatus::Disposed => ("已清理", palette::CREDIT),
                    };
                    ui.label(RichText::new(t).color(color));
                }
                12 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改").clicked() && can_edit {
                            edit = Some(a.id);
                        }
                        if ui.small_button("删").clicked() && can_edit {
                            del = Some(a.id);
                        }
                    });
                }
                _ => {}
            }
        });

        if let Some(id) = edit {
            if let Ok(Some(a)) = findb::assets::get(ctx.db(), id) {
                self.open_edit(a);
            }
        }
        if let Some(id) = del {
            ctx.confirm_dangerous(
                "删除资产卡片",
                "已计提过折旧的卡片不能删除（只能走资产清理）。确定删除吗？",
                ConfirmAction::DeleteAsset(id),
                true,
            );
        }
    }

    // ------------------------- 折旧计提 -------------------------
    fn show_accrue(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        ui.label(
            RichText::new(
                "按部门（+ 费用科目）汇总本期折旧生成一张凭证：借记折旧费用科目、贷记累计折旧科目。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        if self.plan.is_empty() {
            widgets::empty_hint(ui, "本期没有需要计提折旧的资产");
            return;
        }

        let rows: Vec<(String, String, Money, Money, Money)> = self
            .plan
            .iter()
            .map(|r| (r.code.clone(), r.name.clone(), r.amount, r.accum, r.net))
            .collect();
        let total: Money = self.plan.iter().map(|r| r.amount).sum();
        let cols = [
            widgets::TCol::new("资产编码", 110.0).fixed(),
            widgets::TCol::new("资产名称", 240.0),
            widgets::TCol::new("本期应计提", 140.0).right(),
            widgets::TCol::new("提后累计折旧", 140.0).right(),
            widgets::TCol::new("提后净值", 140.0).right(),
        ];
        widgets::grid(ui, "asset_plan", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.0).monospace()); }
                1 => { ui.label(&r.1); }
                2 => widgets::amount_label(ui, r.2),
                3 => widgets::amount_label(ui, r.3),
                4 => widgets::amount_label(ui, r.4),
                _ => {}
            }
        });

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("本期折旧合计：{}", total.fmt_money())).strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("生成本期折旧凭证").clicked() && ctx.can(Perm::VoucherNew) {
                    self.do_accrue(ctx, p);
                }
            });
        });
    }

    fn do_accrue(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let who = ctx.user().display_name.clone();
        match findb::assets::depreciate_period(ctx.db(), p, &who) {
            Ok(res) if res.already => {
                ctx.info(format!(
                    "本期已计提过折旧（凭证 {}），如需重算请先删除本期折旧",
                    res.voucher_no.unwrap_or_default()
                ));
            }
            Ok(res) => {
                ctx.log(
                    "固定资产",
                    "计提折旧",
                    &format!(
                        "{} 凭证#{} 金额 {}",
                        p.label(),
                        res.voucher_id.unwrap_or(0),
                        res.total.fmt_money()
                    ),
                );
                ctx.info(format!(
                    "已生成折旧凭证 {}（{}）",
                    res.voucher_no.unwrap_or_default(),
                    res.total.fmt_money()
                ));
                self.dirty = true;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    // ------------------------- 折旧明细 -------------------------
    fn show_detail(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        widgets::toolbar(ui, |ui| {
            // 数据层只提供按期间整批删除（dep_delete_period），所以这里不做逐行删，
            // 重新计提的正确姿势就是先删本期再生成，避免删单条导致累计折旧断档。
            if ui.button("删除本期折旧").clicked() && ctx.can(Perm::VoucherNew) {
                ctx.confirm_dangerous(
                    "删除本期折旧",
                    &format!(
                        "将删除 {} 的全部折旧记录（共 {} 条），已生成的折旧凭证不会一并删除。\n确定继续吗？",
                        p.label(),
                        self.deps.len()
                    ),
                    ConfirmAction::DeleteDepreciation(p.ymm() as i64),
                    true,
                );
            }
            ui.label(RichText::new(format!("{} 已计提 {} 条", p.label(), self.deps.len())).weak());
        });

        if self.deps.is_empty() {
            widgets::empty_hint(ui, "本期还没有计提折旧");
            return;
        }

        // 编码 / 名称要从卡片还原，折旧记录里只有 asset_id
        let mut names: HashMap<i64, (String, String)> = HashMap::new();
        for a in &self.assets {
            names.insert(a.id, (a.code.clone(), a.name.clone()));
        }
        let rows = self.deps.clone();
        let cols = [
            widgets::TCol::new("资产编码", 110.0).fixed(),
            widgets::TCol::new("资产名称", 220.0),
            widgets::TCol::new("本期折旧", 130.0).right(),
            widgets::TCol::new("累计折旧", 130.0).right(),
            widgets::TCol::new("净值", 130.0).right(),
            widgets::TCol::new("凭证号", 100.0).fixed(),
        ];
        widgets::grid(ui, "asset_deps", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            let name = names.get(&r.asset_id);
            match c {
                0 => { ui.label(RichText::new(name.map(|n| n.0.as_str()).unwrap_or("")).monospace()); }
                1 => { ui.label(name.map(|n| n.1.as_str()).unwrap_or("（卡片已删除）")); }
                2 => widgets::amount_label(ui, r.amount),
                3 => widgets::amount_label(ui, r.accum),
                4 => widgets::amount_label(ui, r.net_value),
                5 => {
                    ui.label(
                        RichText::new(
                            r.voucher_id
                                .map(|v| format!("#{v}"))
                                .unwrap_or_else(|| "—".to_string()),
                        )
                        .weak(),
                    );
                }
                _ => {}
            }
        });

        let total: Money = rows.iter().map(|r| r.amount).sum();
        ui.separator();
        ui.label(
            RichText::new(format!("本期折旧合计：{}", total.fmt_money())).strong(),
        );
    }

    // ------------------------- 资产台账 -------------------------
    fn show_ledger(&mut self, ui: &mut Ui, p: Period) {
        ui.label(
            RichText::new(format!("{} 资产台账（累计折旧优先取落库值）", p.label())).weak(),
        );
        ui.add_space(4.0);

        if self.ledger.is_empty() {
            widgets::empty_hint(ui, "没有固定资产卡片");
            return;
        }
        let rows: Vec<(String, String, Money, Money, Money, i32)> = self
            .ledger
            .iter()
            .map(|r| {
                (
                    r.asset.code.clone(),
                    r.asset.name.clone(),
                    r.asset.original_value,
                    r.accum,
                    r.net,
                    r.months,
                )
            })
            .collect();
        let cols = [
            widgets::TCol::new("资产编码", 110.0).fixed(),
            widgets::TCol::new("资产名称", 240.0),
            widgets::TCol::new("原值", 140.0).right(),
            widgets::TCol::new("累计折旧", 140.0).right(),
            widgets::TCol::new("净值", 140.0).right(),
            widgets::TCol::new("已提月数", 100.0).right(),
        ];
        widgets::grid(ui, "asset_ledger", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.0).monospace()); }
                1 => { ui.label(&r.1); }
                2 => widgets::amount_label(ui, r.2),
                3 => widgets::amount_label(ui, r.3),
                4 => widgets::amount_label(ui, r.4),
                5 => { ui.label(r.5.to_string()); }
                _ => {}
            }
        });

        let t_orig: Money = rows.iter().map(|r| r.2).sum();
        let t_acc: Money = rows.iter().map(|r| r.3).sum();
        let t_net: Money = rows.iter().map(|r| r.4).sum();
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("原值合计：{}", t_orig.fmt_money())).strong());
            ui.label(RichText::new(format!("累计折旧合计：{}", t_acc.fmt_money())).strong());
            ui.label(RichText::new(format!("净值合计：{}", t_net.fmt_money())).strong());
        });
    }

    // ------------------------- 卡片编辑窗口 -------------------------
    fn open_new(&mut self, ctx: &mut AppCtx<'_>) {
        let code = findb::assets::next_code(ctx.db()).unwrap_or_else(|_| "GD0001".to_string());
        let a = Asset {
            id: 0,
            code,
            name: String::new(),
            category: String::new(),
            spec: String::new(),
            dept: String::new(),
            asset_account: "1601".to_string(),
            dep_account: "1602".to_string(),
            expense_account: "6602".to_string(),
            original_value: Money::ZERO,
            residual_rate: Money::parse("0.05").unwrap_or(Money::ZERO),
            life_months: 60,
            method: DepMethod::Straight,
            start_period: self.period(ctx),
            disposed_period: None,
            dispose_amount: None,
            status: AssetStatus::InUse,
            voucher_id: None,
            memo: String::new(),
        };
        self.f_original = String::new();
        self.f_residual = (a.residual_rate * 100).round2().fmt_money();
        self.f_life = a.life_months.to_string();
        self.f_start = a.start_period.code();
        self.editing_new = true;
        self.err.clear();
        self.editing = Some(a);
    }

    fn open_edit(&mut self, a: Asset) {
        self.f_original = a.original_value.to_string();
        self.f_residual = (a.residual_rate * 100).round2().fmt_money();
        self.f_life = a.life_months.to_string();
        self.f_start = a.start_period.code();
        self.editing_new = false;
        self.err.clear();
        self.editing = Some(a);
    }

    fn edit_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.editing.is_none() {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;

        egui::Window::new(if is_new { "新增资产卡片" } else { "修改资产卡片" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.err);
                }
                let e = self.editing.as_mut().unwrap();
                egui::Grid::new("asset_edit")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("资产编码：");
                        widgets::text_input(ui, &mut e.code, 220.0, "GD0001");
                        ui.end_row();
                        ui.label("资产名称：");
                        widgets::text_input(ui, &mut e.name, 220.0, "如：联想台式机");
                        ui.end_row();
                        ui.label("类别：");
                        widgets::text_input(ui, &mut e.category, 220.0, "如：电子设备");
                        ui.end_row();
                        ui.label("规格型号：");
                        widgets::text_input(ui, &mut e.spec, 220.0, "");
                        ui.end_row();
                        ui.label("使用部门：");
                        widgets::text_input(ui, &mut e.dept, 220.0, "部门编码（部门核算科目用）");
                        ui.end_row();
                    });

                ui.add_space(4.0);
                ui.label(RichText::new("科目设置").strong());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let e = self.editing.as_mut().unwrap();
                    ui.label("资产科目");
                    widgets::account_combo(ui, "as_asset_acct", &mut e.asset_account, ctx.chart(), true, 230.0);
                    ui.label("折旧科目");
                    widgets::account_combo(ui, "as_dep_acct", &mut e.dep_account, ctx.chart(), true, 230.0);
                    ui.label("费用科目");
                    widgets::account_combo(ui, "as_exp_acct", &mut e.expense_account, ctx.chart(), true, 230.0);
                });

                ui.add_space(4.0);
                ui.label(RichText::new("折旧参数").strong());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label("原值");
                    widgets::money_input(ui, &mut self.f_original, 130.0);
                    ui.label("残值率(%)");
                    widgets::money_input(ui, &mut self.f_residual, 80.0);
                    ui.label("使用年限(月)");
                    widgets::text_input(ui, &mut self.f_life, 80.0, "60");
                    ui.label("开始期间");
                    widgets::text_input(ui, &mut self.f_start, 100.0, "202601");
                });
                ui.horizontal(|ui| {
                    ui.label("折旧方法");
                    let e = self.editing.as_mut().unwrap();
                    let mut m = e.method;
                    egui::ComboBox::from_id_salt("as_method")
                        .selected_text(m.label())
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for d in DepMethod::ALL {
                                ui.selectable_value(&mut m, *d, d.label());
                            }
                        });
                    e.method = m;
                    ui.add_space(10.0);
                    let mut s = e.status;
                    ui.label("状态");
                    egui::ComboBox::from_id_salt("as_status")
                        .selected_text(s.label())
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            for st in AssetStatus::ALL {
                                ui.selectable_value(&mut s, *st, st.label());
                            }
                        });
                    e.status = s;
                });
                ui.horizontal(|ui| {
                    ui.label("备注");
                    let e = self.editing.as_mut().unwrap();
                    widgets::text_input(ui, &mut e.memo, 420.0, "");
                });

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
            self.commit(ctx);
        }
    }

    fn commit(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(e) = self.editing.as_mut() else {
            return;
        };
        if e.code.trim().is_empty() || e.name.trim().is_empty() {
            self.err = "资产编码与名称不能为空".to_string();
            return;
        }
        e.original_value = match Money::parse(&self.f_original) {
            Ok(v) => v,
            Err(x) => {
                self.err = x.to_string();
                return;
            }
        };
        // 界面按百分数录入（5 = 5%），库里存小数
        e.residual_rate = match Money::parse(&self.f_residual) {
            Ok(v) => v.checked_div(100).expect("字面量 100 非零").round_dp(4),
            Err(x) => {
                self.err = x.to_string();
                return;
            }
        };
        e.life_months = self.f_life.trim().parse::<i32>().unwrap_or(0);
        if e.life_months <= 0 {
            self.err = "使用年限必须大于 0 个月".to_string();
            return;
        }
        e.start_period = match Period::parse(&self.f_start) {
            Ok(p) => p,
            Err(x) => {
                self.err = x.to_string();
                return;
            }
        };

        // 编码唯一：库里唯一索引会拦，但在这儿先拦一次能给出可读的提示
        match findb::assets::get_by_code(ctx.db(), &e.code) {
            Ok(Some(old)) if old.id != e.id => {
                self.err = format!("资产编码 {} 已存在", e.code);
                return;
            }
            Ok(_) => {}
            Err(x) => {
                self.err = x.to_string();
                return;
            }
        }

        let is_new = self.editing_new;
        let r = if is_new {
            findb::assets::insert(ctx.db(), e).map(|_| ())
        } else {
            findb::assets::update(ctx.db(), e)
        };
        match r {
            Ok(()) => {
                ctx.log(
                    "固定资产",
                    if is_new { "新增卡片" } else { "修改卡片" },
                    &format!("{} {}", e.code, e.name),
                );
                ctx.info(if is_new { "已新增资产卡片" } else { "已保存资产卡片" });
                self.editing = None;
                self.err.clear();
                self.dirty = true;
            }
            Err(x) => self.err = x.to_string(),
        }
    }
}


