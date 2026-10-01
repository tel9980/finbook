//! 工资管理：工资表 / 个税明细 / 凭证生成
//!
//! 个税用累计预扣预缴法（`fincore::engine::tax`），本期税额 = 累计应纳税额 − 已预扣税额，
//! 所以界面上既要给本期数、也要给本年至本月的累计数，否则用户看不懂为什么两个月的税不一样。

use std::collections::BTreeMap;

use chrono::NaiveDate;
use egui::{Align2, RichText, Ui};
use findb::business::{self, Payroll, YtdPayroll};
use fincore::engine::tax::{self, Cumulative};
use fincore::{AuxKind, Money, Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets::{self, Paging};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Sheet,
    Tax,
    Voucher,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Sheet => "工资表",
            Tab::Tax => "个税明细",
            Tab::Voucher => "凭证生成",
        }
    }
}

/// 工资行编辑缓冲区
#[derive(Clone, Default)]
pub struct PayDraft {
    employee: String,
    dept: String,
    gross: String,
    social: String,
    housing: String,
    deduction: String,
    additional: String,
    social_co: String,
    housing_co: String,
    memo: String,
}

/// 凭证生成页签的科目设置，默认值按小企业科目表的常见编码
pub struct VoucherCfg {
    date: String,
    expense: String,
    wage_payable: String,
    social_payable: String,
    housing_payable: String,
    personal_payable: String,
    bank: String,
    tax_payable: String,
}

impl Default for VoucherCfg {
    fn default() -> Self {
        Self {
            date: String::new(),
            expense: "660201".to_string(),
            wage_payable: "221101".to_string(),
            social_payable: "221103".to_string(),
            housing_payable: "221104".to_string(),
            personal_payable: "2241".to_string(),
            bank: "100201".to_string(),
            tax_payable: "222107".to_string(),
        }
    }
}

pub struct PayrollView {
    pub tab: Tab,
    pub period_text: String,
    pub rows: Vec<Payroll>,
    pub paging: Paging,
    /// 个税明细页签选中的职员（职员档案 code）
    pub employee: String,
    /// 职员档案 code → 显示名，表格里不想只显示编码
    pub emp_names: BTreeMap<String, String>,
    pub emp_opts: Vec<String>,
    pub ytd: Option<YtdPayroll>,
    /// 选中员工的本期工资（个税明细里算本期税额要用）
    pub current: Option<Payroll>,
    pub editing: Option<PayDraft>,
    pub editing_new: bool,
    pub cfg: VoucherCfg,
    pub err: String,
    pub dirty: bool,
    key: String,
}

impl Default for PayrollView {
    fn default() -> Self {
        Self {
            tab: Tab::Sheet,
            period_text: String::new(),
            rows: Vec::new(),
            paging: Paging::default(),
            employee: String::new(),
            emp_names: BTreeMap::new(),
            emp_opts: Vec::new(),
            ytd: None,
            current: None,
            editing: None,
            editing_new: false,
            cfg: VoucherCfg::default(),
            err: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl PayrollView {
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

    /// 职员档案编码 → "编码 名称"
    fn emp_label(&self, code: &str) -> String {
        match self.emp_names.get(code) {
            Some(n) if !n.is_empty() => format!("{code} {n}"),
            _ => code.to_string(),
        }
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}|{}|{}", p.ymm(), self.tab.name(), self.employee);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        // 「仅看本人经手的业务单据」：工资按员工姓名过滤
        let mut rows = business::payroll_list(ctx.db(), p).unwrap_or_default();
        let u = ctx.user();
        if u.data_scope.own_doc_only {
            rows.retain(|r| r.employee == u.display_name || r.employee == u.username);
        }
        self.rows = rows;
        // 职员档案只用来做显示名与下拉选项，取不到也不影响录入
        let emps = findb::auxs::list(
            ctx.db(),
            &fincore::AuxQuery::kind(AuxKind::Employee).with_disabled(true),
        )
        .unwrap_or_default();
        self.emp_names = emps.iter().map(|e| (e.code.clone(), e.name.clone())).collect();
        self.emp_opts = emps.iter().map(|e| e.code.clone()).collect();
        self.paging.reset();

        self.ytd = None;
        self.current = None;
        if self.tab == Tab::Tax && !self.employee.is_empty() {
            self.ytd = ctx
                .handle(business::payroll_ytd(ctx.db(), p, &self.employee));
            self.current = ctx
                .handle(business::payroll_get(ctx.db(), p, &self.employee))
                .flatten();
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "工资管理", |ui| {
            ui.label(RichText::new(format!("共 {} 人", self.rows.len())).weak());
        });

        widgets::toolbar(ui, |ui| {
            for t in [Tab::Sheet, Tab::Tax, Tab::Voucher] {
                ui.selectable_value(&mut self.tab, t, t.name());
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
            Tab::Sheet => self.show_sheet(ctx, ui),
            Tab::Tax => self.show_tax(ui),
            Tab::Voucher => self.show_voucher(ctx, ui, p),
        }

        self.pay_window(ctx, ui, p);
    }

    // ------------------------- 工资表 -------------------------
    fn show_sheet(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            if ui.button("新增工资行").clicked() && ctx.can(Perm::VoucherNew) {
                self.editing = Some(PayDraft::default());
                self.editing_new = true;
                self.err.clear();
            }
        });

        let shown: Vec<Payroll> = self.paging.slice(&self.rows).to_vec();
        let cols = [
            widgets::TCol::new("员工", 150.0),
            widgets::TCol::new("部门", 100.0),
            widgets::TCol::new("应发工资", 110.0).right(),
            widgets::TCol::new("社保(个人)", 100.0).right(),
            widgets::TCol::new("公积金(个人)", 110.0).right(),
            widgets::TCol::new("专项附加扣除", 110.0).right(),
            widgets::TCol::new("其他扣除", 100.0).right(),
            widgets::TCol::new("计税基数", 110.0).right(),
            widgets::TCol::new("个税", 100.0).right(),
            widgets::TCol::new("实发工资", 110.0).right(),
            widgets::TCol::new("单位社保", 100.0).right(),
            widgets::TCol::new("单位公积金", 110.0).right(),
            widgets::TCol::new("操作", 100.0).fixed(),
        ];
        let mut edit: Option<i64> = None;
        let mut del: Option<i64> = None;
        widgets::grid(ui, "pay_rows", &cols, shown.len(), 24.0, |i, c, ui| {
            let r = &shown[i];
            match c {
                0 => {
                    ui.label(self.emp_label(&r.employee));
                }
                1 => {
                    ui.label(&r.dept);
                }
                2 => widgets::amount_label(ui, r.gross),
                3 => widgets::amount_label(ui, r.social),
                4 => widgets::amount_label(ui, r.housing),
                5 => widgets::amount_label(ui, r.additional),
                6 => widgets::amount_label(ui, r.deduction),
                7 => widgets::amount_label(ui, r.tax_base),
                8 => widgets::amount_label(ui, r.tax),
                9 => widgets::amount_label(ui, r.net),
                10 => widgets::amount_label(ui, r.social_co),
                11 => widgets::amount_label(ui, r.housing_co),
                12 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改").clicked() {
                            edit = Some(r.id);
                        }
                        if ui.small_button("删").clicked() {
                            del = Some(r.id);
                        }
                    });
                }
                _ => {}
            }
        });
        if let Some(id) = edit {
            if let Some(r) = self.rows.iter().find(|x| x.id == id) {
                self.editing = Some(PayDraft {
                    employee: r.employee.clone(),
                    dept: r.dept.clone(),
                    gross: money_str(r.gross),
                    social: money_str(r.social),
                    housing: money_str(r.housing),
                    deduction: money_str(r.deduction),
                    additional: money_str(r.additional),
                    social_co: money_str(r.social_co),
                    housing_co: money_str(r.housing_co),
                    memo: r.memo.clone(),
                });
                self.editing_new = false;
                self.err.clear();
            }
        }
        if let Some(id) = del {
            ctx.confirm_dangerous(
                "删除工资记录",
                "删除后该员工本期的个税累计会跟着变化，确定删除吗？",
                ConfirmAction::DeletePayroll(id),
                true,
            );
        }

        let tg: Money = self.rows.iter().map(|r| r.gross).sum();
        let tt: Money = self.rows.iter().map(|r| r.tax).sum();
        let tn: Money = self.rows.iter().map(|r| r.net).sum();
        let tsc: Money = self.rows.iter().map(|r| r.social_co + r.housing_co).sum();
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!(
                "应发合计 {} ／ 个税合计 {} ／ 实发合计 {} ／ 单位承担社保公积金 {}",
                tg.fmt_money(),
                tt.fmt_money(),
                tn.fmt_money(),
                tsc.fmt_money()
            )).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.paging.bar(ui, self.rows.len());
            });
        });
        ui.label(
            RichText::new(format!("计提与发放的凭证在「{}」页签生成", Tab::Voucher.name())).weak(),
        );
    }

    // ------------------------- 个税明细 -------------------------
    fn show_tax(&mut self, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("员工");
            if widgets::combo(ui, "pay_tax_emp", &mut self.employee, &self.emp_opts, 160.0).changed()
            {
                self.dirty = true;
            }
            if let Some(n) = self.emp_names.get(&self.employee) {
                ui.label(RichText::new(n.clone()).weak());
            }
        });

        if self.employee.is_empty() {
            widgets::empty_hint(ui, "请选择员工查看本年累计个税情况");
            return;
        }
        let Some(y) = self.ytd else {
            widgets::empty_hint(ui, "暂无该员工的累计数据");
            return;
        };

        widgets::card(ui, "本年累计（截至本月前）", |ui| {
            widgets::kv(ui, "累计申报月份：", &format!("{} 个月", y.months));
            widgets::kv(ui, "累计收入：", &y.income.fmt_money());
            widgets::kv(ui, "累计专项扣除（社保+公积金个人）：", &y.special.fmt_money());
            widgets::kv(ui, "累计专项附加扣除：", &y.additional.fmt_money());
            widgets::kv(ui, "累计已预扣税额：", &y.withheld.fmt_money());
        });

        widgets::card(ui, "本期计税过程", |ui| {
            ui.label(
                RichText::new("累计预扣预缴法：本期个税 = 累计应纳税额 − 已预扣税额").weak(),
            );
            let (gross, social, housing, additional) = match &self.current {
                Some(c) => (c.gross, c.social, c.housing, c.additional),
                None => (Money::ZERO, Money::ZERO, Money::ZERO, Money::ZERO),
            };
            if self.current.is_none() {
                ui.colored_label(palette::WARN, "该员工本期没有工资记录，下面只按累计数测算");
            }
            // 累计口径 = 以前月份累计 + 本期，任职月数 +1（封顶 12）
            let c = Cumulative {
                income: y.income + gross,
                special: y.special + social + housing,
                additional: y.additional + additional,
                other: Money::ZERO,
                withheld: y.withheld,
                months: (y.months + 1).min(12),
            };
            widgets::kv(ui, "本期应发工资：", &gross.fmt_money());
            widgets::kv(ui, "本期专项扣除：", &(social + housing).fmt_money());
            widgets::kv(ui, "本期专项附加扣除：", &additional.fmt_money());
            widgets::kv(ui, "累计基本减除费用：", &c.basic_deduction().fmt_money());
            widgets::kv(ui, "累计应纳税所得额：", &c.taxable().fmt_money());
            let (rate, quick) = tax::rate_of(c.taxable());
            widgets::kv(
                ui,
                "适用税率 / 速算扣除数：",
                &format!(
                    "{}% / {}",
                    (rate * Money::from_i64(100)).fmt_qty(),
                    quick.fmt_money()
                ),
            );
            widgets::kv(ui, "累计应纳税额：", &c.tax_due().fmt_money());
            widgets::kv(ui, "减：累计已预扣税额：", &y.withheld.fmt_money());
            widgets::kv(
                ui,
                "本期应扣个税：",
                &tax::current_tax(&c).unwrap_or(Money::ZERO).fmt_money(),
            );
        });
        ui.label(
            RichText::new(
                "说明：适用的是年度税率表；本期算出的税额为负数时按 0 处理，多缴部分留到汇算清缴退税。",
            )
            .weak(),
        );
    }

    // ------------------------- 凭证生成 -------------------------
    fn show_voucher(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        if self.cfg.date.is_empty() {
            self.cfg.date = p.last_day().format("%Y-%m-%d").to_string();
        }
        if self.rows.is_empty() {
            ui.colored_label(palette::WARN, "本期没有工资数据，无法生成凭证");
        } else if let Some(r) = self.rows.first() {
            // 发放状态可见：三类凭证回链（同期间共享）
            let f = |x: Option<i64>| {
                x.map(|v| format!("#{v}"))
                    .unwrap_or_else(|| "未生成".to_string())
            };
            ui.horizontal_wrapped(|ui| {
                ui.label("本期凭证状态：");
                ui.label(format!(
                    "计提 {} · 社保缴纳 {} · 发放 {}",
                    f(r.voucher_id),
                    f(r.social_voucher_id),
                    f(r.paid_voucher_id)
                ));
            });
        }

        widgets::card(ui, "计提工资", |ui| {
            ui.label("借 费用科目（按部门）／ 贷 应付职工薪酬、应付社保、应付公积金");
            egui::Grid::new("pay_v1")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("凭证日期：");
                    ui.add_sized([120.0, 22.0], egui::TextEdit::singleline(&mut self.cfg.date));
                    ui.end_row();
                    ui.label("费用科目：");
                    widgets::account_combo(
                        ui,
                        "pay_v1_expense",
                        &mut self.cfg.expense,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("应付工资：");
                    widgets::account_combo(
                        ui,
                        "pay_v1_wage",
                        &mut self.cfg.wage_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("应付社保（企业）：");
                    widgets::account_combo(
                        ui,
                        "pay_v1_social",
                        &mut self.cfg.social_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("应付公积金（企业）：");
                    widgets::account_combo(
                        ui,
                        "pay_v1_housing",
                        &mut self.cfg.housing_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                });
            if ui.button("生成计提凭证").clicked() && ctx.can(Perm::VoucherNew) {
                self.gen_accrue(ctx, p);
            }
        });

        widgets::card(ui, "缴纳社保公积金", |ui| {
            ui.label("借 应付社保、应付公积金、其他应付款（个人部分）／ 贷 银行存款");
            egui::Grid::new("pay_v2")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("应付社保（企业）：");
                    widgets::account_combo(
                        ui,
                        "pay_v2_social",
                        &mut self.cfg.social_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("应付公积金（企业）：");
                    widgets::account_combo(
                        ui,
                        "pay_v2_housing",
                        &mut self.cfg.housing_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("其他应付款（个人）：");
                    widgets::account_combo(
                        ui,
                        "pay_v2_personal",
                        &mut self.cfg.personal_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("银行存款：");
                    widgets::account_combo(
                        ui,
                        "pay_v2_bank",
                        &mut self.cfg.bank,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                });
            if ui.button("生成缴纳凭证").clicked() && ctx.can(Perm::VoucherNew) {
                self.gen_social(ctx, p);
            }
        });

        widgets::card(ui, "发放工资", |ui| {
            ui.label("借 应付职工薪酬 ／ 贷 银行存款、应交个人所得税、其他应付款（代扣社保）");
            egui::Grid::new("pay_v3")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("应付职工薪酬：");
                    widgets::account_combo(
                        ui,
                        "pay_v3_payable",
                        &mut self.cfg.wage_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("银行存款：");
                    widgets::account_combo(
                        ui,
                        "pay_v3_bank",
                        &mut self.cfg.bank,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("应交个人所得税：");
                    widgets::account_combo(
                        ui,
                        "pay_v3_tax",
                        &mut self.cfg.tax_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                    ui.label("其他应付款（代扣）：");
                    widgets::account_combo(
                        ui,
                        "pay_v3_social",
                        &mut self.cfg.personal_payable,
                        ctx.chart(),
                        true,
                        240.0,
                    );
                    ui.end_row();
                });
            if ui.button("生成发放凭证").clicked() && ctx.can(Perm::VoucherNew) {
                self.gen_pay(ctx, p);
            }
        });
    }

    fn cfg_date(&self, p: Period) -> NaiveDate {
        NaiveDate::parse_from_str(self.cfg.date.trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| p.last_day())
    }

    fn gen_accrue(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let date = self.cfg_date(p);
        let who = ctx.user().display_name.clone();
        let (e, w, s, h) = (
            self.cfg.expense.clone(),
            self.cfg.wage_payable.clone(),
            self.cfg.social_payable.clone(),
            self.cfg.housing_payable.clone(),
        );
        let r = business::payroll_accrue_voucher(ctx.db(), p, date, &e, &w, &s, &h, &who);
        match r {
            Ok(Some(id)) => {
                let no = voucher_no(ctx.db(), id);
                ctx.log("工资", "计提工资", &format!("{} 凭证{no}", p.label()));
                ctx.info(format!("已生成计提工资凭证 {no}"));
                self.dirty = true;
            }
            Ok(None) => ctx.error("本期没有工资数据"),
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn gen_social(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let date = self.cfg_date(p);
        let who = ctx.user().display_name.clone();
        let (s, h, pp, b) = (
            self.cfg.social_payable.clone(),
            self.cfg.housing_payable.clone(),
            self.cfg.personal_payable.clone(),
            self.cfg.bank.clone(),
        );
        let r = business::payroll_social_voucher(ctx.db(), p, date, &s, &h, &pp, &b, &who);
        match r {
            Ok(Some(id)) => {
                let no = voucher_no(ctx.db(), id);
                ctx.log("工资", "缴纳社保公积金", &format!("{} 凭证{no}", p.label()));
                ctx.info(format!("已生成缴纳社保公积金凭证 {no}"));
                self.dirty = true;
            }
            Ok(None) => ctx.error("本期没有应缴的社保公积金金额"),
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn gen_pay(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let date = self.cfg_date(p);
        let who = ctx.user().display_name.clone();
        let (w, b, t, s) = (
            self.cfg.wage_payable.clone(),
            self.cfg.bank.clone(),
            self.cfg.tax_payable.clone(),
            self.cfg.personal_payable.clone(),
        );
        let r = business::payroll_pay_voucher(ctx.db(), p, date, &w, &b, &t, &s, &who);
        match r {
            Ok(Some(id)) => {
                let no = voucher_no(ctx.db(), id);
                ctx.log("工资", "发放工资", &format!("{} 凭证{no}", p.label()));
                ctx.info(format!("已生成发放工资凭证 {no}"));
                self.dirty = true;
            }
            Ok(None) => ctx.error("本期没有工资数据"),
            Err(e) => ctx.error(e.to_string()),
        }
    }

    // ------------------------- 工资行编辑 -------------------------
    fn pay_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        let Some(d) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;
        let err = self.err.clone();
        let emp_opts = self.emp_opts.clone();

        egui::Window::new(if is_new { "新增工资行" } else { "修改工资行" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if !err.is_empty() {
                    ui.colored_label(palette::CREDIT, &err);
                }
                ui.label(
                    RichText::new("个税由系统按累计预扣预缴法自动计算，保存后可在「个税明细」查看")
                        .weak(),
                );
                egui::Grid::new("pay_edit")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("员工：");
                        ui.horizontal(|ui| {
                            widgets::text_input(ui, &mut d.employee, 140.0, "职员编码");
                            widgets::combo(ui, "pay_emp_pick", &mut d.employee, &emp_opts, 90.0);
                        });
                        ui.end_row();
                        ui.label("部门：");
                        widgets::text_input(ui, &mut d.dept, 140.0, "部门编码");
                        ui.end_row();
                        ui.label("应发工资：");
                        widgets::money_input(ui, &mut d.gross, 140.0);
                        ui.end_row();
                        ui.label("社保（个人）：");
                        widgets::money_input(ui, &mut d.social, 140.0);
                        ui.end_row();
                        ui.label("公积金（个人）：");
                        widgets::money_input(ui, &mut d.housing, 140.0);
                        ui.end_row();
                        ui.label("专项附加扣除：");
                        widgets::money_input(ui, &mut d.additional, 140.0);
                        ui.end_row();
                        ui.label("其他扣除：");
                        widgets::money_input(ui, &mut d.deduction, 140.0);
                        ui.end_row();
                        ui.label("社保（企业）：");
                        widgets::money_input(ui, &mut d.social_co, 140.0);
                        ui.end_row();
                        ui.label("公积金（企业）：");
                        widgets::money_input(ui, &mut d.housing_co, 140.0);
                        ui.end_row();
                        ui.label("备注：");
                        widgets::text_input(ui, &mut d.memo, 240.0, "可留空");
                        ui.end_row();
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
        if !save {
            return;
        }

        let d = self.editing.clone().unwrap();
        self.err.clear();
        if d.employee.trim().is_empty() {
            self.err = "请填写员工编码".to_string();
            return;
        }
        // 金额不合法必须报错，不能静默按 0 保存（应发 0 会直接算错个税与实发）
        let money = |s: &str| -> Result<Money, String> {
            let t = s.trim();
            if t.is_empty() {
                return Ok(Money::ZERO);
            }
            Money::parse(t)
                .map(|m| m.round2())
                .map_err(|_| format!("金额格式不正确：{t}"))
        };
        let parsed: Result<Vec<Money>, String> = [
            &d.gross, &d.social, &d.housing, &d.deduction, &d.additional, &d.social_co,
            &d.housing_co,
        ]
        .iter()
        .map(|s| money(s))
        .collect();
        let vals = match parsed {
            Ok(v) => v,
            Err(e) => {
                self.err = e;
                return;
            }
        };
        // 税额不让界面自己算：payroll_calc 会带上本年累计数，用累计预扣法算
        let r = business::payroll_calc(
            ctx.db(),
            p,
            d.employee.trim(),
            d.dept.trim(),
            vals[0],
            vals[1],
            vals[2],
            vals[3],
            vals[4],
            vals[5],
            vals[6],
            d.memo.trim(),
        );
        match r.and_then(|x| business::payroll_upsert(ctx.db(), &x).map(|_| x)) {
            Ok(rec) => {
                ctx.log(
                    "工资",
                    if self.editing_new { "新增工资行" } else { "修改工资行" },
                    &format!(
                        "{} {} 应发{} 个税{} 实发{}",
                        p.label(),
                        rec.employee,
                        rec.gross.fmt_money(),
                        rec.tax.fmt_money(),
                        rec.net.fmt_money()
                    ),
                );
                ctx.info(format!(
                    "已保存，个税 {} 、实发 {}",
                    rec.tax.fmt_money(),
                    rec.net.fmt_money()
                ));
                self.dirty = true;
                self.editing = None;
            }
            Err(e) => self.err = e.to_string(),
        }
    }
}

/// 金额 → 编辑框文本：零值留空，避免满屏 0.00
fn money_str(v: Money) -> String {
    if v.is_zero() {
        String::new()
    } else {
        v.fmt_plain()
    }
}

fn voucher_no(db: &findb::Db, id: i64) -> String {
    match findb::vouchers::get(db, id) {
        Ok(Some(v)) => v.voucher_no(),
        _ => format!("#{id}"),
    }
}
