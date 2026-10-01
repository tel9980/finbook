//! 账簿查询：明细账 / 总账 / 日记账

use egui::{RichText, Ui};
use findb::balances::{BalanceQuery, LedgerQuery};
use findb::reports::DailyRow;
use fincore::{
    signed_to_dir_amount, GeneralLedgerRow, JournalRow, LedgerRow, Money, Perm, Period,
};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets::{self, AccountPickerState, Paging};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Detail,
    General,
    Journal,
    Daily,
}

pub struct LedgerView {
    pub tab: Tab,
    pub code: String,
    pub from: String,
    pub to: String,
    pub include_children: bool,
    pub posted_only: bool,
    pub paging: Paging,
    pub picker: AccountPickerState,

    pub detail: Vec<LedgerRow>,
    pub general: Vec<GeneralLedgerRow>,
    pub journal: Vec<JournalRow>,
    pub daily: Vec<DailyRow>,
    pub begin: Money,
    /// 数据范围拦截：所选科目不在可见范围内
    pub scope_blocked: bool,
    pub dirty: bool,
    key: String,
}

impl Default for LedgerView {
    fn default() -> Self {
        Self {
            tab: Tab::Detail,
            code: "1001".to_string(),
            from: String::new(),
            to: String::new(),
            include_children: false,
            posted_only: true,
            paging: Paging::default(),
            picker: AccountPickerState::default(),
            detail: Vec::new(),
            general: Vec::new(),
            journal: Vec::new(),
            daily: Vec::new(),
            begin: Money::ZERO,
            scope_blocked: false,
            dirty: true,
            key: String::new(),
        }
    }
}

impl LedgerView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>, code: Option<String>) {
        let p = ctx.period();
        if self.from.is_empty() {
            self.from = p.code();
        }
        if self.to.is_empty() {
            self.to = p.code();
        }
        if let Some(c) = code {
            self.code = c;
        }
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let key = format!(
            "{}|{}|{}|{}|{}|{:?}",
            self.code, self.from, self.to, self.include_children, self.posted_only, self.tab
        );
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        self.paging.reset();

        let from = Period::parse(&self.from).unwrap_or_else(|_| ctx.period());
        let to = Period::parse(&self.to).unwrap_or_else(|_| ctx.period());
        let code = self.code.trim().to_string();
        // 数据范围（科目范围）：所选科目不在可见范围时，清空账簿并提示
        if !ctx.user().can_see_account(&code) {
            self.detail.clear();
            self.general.clear();
            self.journal.clear();
            self.daily.clear();
            self.begin = Money::ZERO;
            self.scope_blocked = true;
            return;
        }
        self.scope_blocked = false;
        let q = LedgerQuery {
            code: code.clone(),
            include_children: self.include_children,
            aux: None,
            from,
            to,
            posted_only: self.posted_only,
            prepared_by: None,
            code_from: None,
            code_to: None,
        }
        .with_user_scope(ctx.user());
        match self.tab {
            Tab::Detail => {
                self.detail = findb::balances::ledger(ctx.db(), ctx.chart(), &q).unwrap_or_default();
            }
            Tab::General => {
                self.general =
                    findb::balances::general_ledger(ctx.db(), &q).unwrap_or_default();
            }
            Tab::Journal => {
                self.journal = findb::balances::journal(ctx.db(), ctx.chart(), &q).unwrap_or_default();
            }
            Tab::Daily => {
                self.daily = findb::reports::account_daily_report(ctx.db(), &code, from, to, Some(ctx.user()), self.posted_only).unwrap_or_default();
            }
        }
        // 期初余额
        let snap = findb::balances::BalanceSnapshot::load(
            ctx.db(),
            &BalanceQuery::range(from, from).with_user_scope(ctx.user()),
        );
        self.begin = match snap {
            Ok(s) => s.for_account(self.code.trim(), None).begin,
            Err(_) => Money::ZERO,
        };
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let ectx = ui.ctx().clone();
        let acct_name = ctx
            .chart()
            .get(&self.code)
            .map(|a| a.name.clone())
            .unwrap_or_default();

        widgets::page_header(ui, "账簿查询", |ui| {
            ui.label(RichText::new(format!("{} {}", self.code, acct_name)).weak());
        });
        if self.scope_blocked {
            ui.colored_label(
                crate::theme::palette::CREDIT,
                format!("当前数据范围限制了科目「{}」的查看权限", self.code),
            );
        }

        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, Tab::Detail, "明细账");
            ui.selectable_value(&mut self.tab, Tab::General, "总账");
            ui.selectable_value(&mut self.tab, Tab::Journal, "日记账");
            ui.selectable_value(&mut self.tab, Tab::Daily, "科目日报表");
            ui.separator();
            ui.label("科目");
            ui.add_sized([100.0, 22.0], egui::TextEdit::singleline(&mut self.code));
            if ui.button("选择…").clicked() {
                self.picker.open = true;
                self.picker.leaf_only = false;
            }
            ui.label("期间");
            ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.from));
            ui.label("—");
            ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.to));
            ui.checkbox(&mut self.include_children, "含下级科目");
            ui.checkbox(&mut self.posted_only, "只含已记账");
            if ui.button("查询").clicked() {
                self.dirty = true;
            }
            if ui.button("本期").clicked() {
                let p = ctx.period();
                self.from = p.code();
                self.to = p.code();
                self.dirty = true;
            }
            if ui.button("本年").clicked() {
                let p = ctx.period();
                self.from = Period::new(p.year(), 1).map(|x| x.code()).unwrap_or(p.code());
                self.to = p.code();
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        // 账簿套打：按会计档案三栏账版式打印，不落地数据文件（Report 权限即可）
        if ctx.user().can(Perm::Report) {
            widgets::toolbar(ui, |ui| {
                ui.label(RichText::new("套打").strong());
                if ui.button("账簿套打").clicked() {
                    self.print_taoda(ctx);
                }
            });
        }

        ui.horizontal(|ui| {
            let (d, amt) = signed_to_dir_amount(self.begin);
            ui.label(RichText::new(format!(
                "期初余额：{} {}",
                d.label(),
                amt.fmt_money()
            )));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.paging.bar(
                    ui,
                    match self.tab {
                        Tab::Detail => self.detail.len(),
                        Tab::General => self.general.len(),
                        Tab::Journal => self.journal.len(),
                        Tab::Daily => self.daily.len(),
                    },
                );
            });
        });

        match self.tab {
            Tab::Detail => self.show_detail(ui),
            Tab::General => self.show_general(ui),
            Tab::Journal => self.show_journal(ui),
            Tab::Daily => self.show_daily(ui),
        }

        if let Some(code) = self.picker.show(&ectx, ctx.chart()) {
            self.code = code;
            self.dirty = true;
        }
    }

    fn show_detail(&mut self, ui: &mut Ui) {
        let page = self.paging.slice(&self.detail).to_vec();
        // 数量核算科目：追加数量余额列（数据在 LedgerRow 里已带，原先不展示）
        let show_qty = self.detail.iter().any(|r| r.qty_balance.is_some());
        let mut cols = vec![
            widgets::TCol::new("日期", 92.0).fixed(),
            widgets::TCol::new("凭证号", 88.0).fixed(),
            widgets::TCol::new("摘要", 260.0),
            widgets::TCol::new("借方", 130.0).right(),
            widgets::TCol::new("贷方", 130.0).right(),
            widgets::TCol::new("方向", 44.0).fixed(),
            widgets::TCol::new("余额", 140.0).right(),
        ];
        if show_qty {
            cols.push(widgets::TCol::new("数量余额", 120.0).right());
        }
        widgets::grid(ui, "ledger_detail", &cols, page.len(), 24.0, |i, c, ui| {
            let r = &page[i];
            match c {
                0 => {
                    ui.label(r.date.format("%Y-%m-%d").to_string());
                }
                1 => {
                    ui.label(RichText::new(&r.voucher_no).monospace());
                }
                2 => {
                    ui.label(&r.summary);
                }
                3 => widgets::amount_label(ui, r.debit),
                4 => widgets::amount_label(ui, r.credit),
                5 => {
                    ui.label(if r.balance.is_zero() {
                        RichText::new("平").weak()
                    } else {
                        RichText::new(r.dir.label())
                    });
                }
                6 => {
                    widgets::amount_label(ui, r.balance);
                }
                7 => {
                    if let Some(q) = r.qty_balance {
                        ui.label(
                            RichText::new(q.fmt_qty())
                                .color(palette::CREDIT)
                                .monospace(),
                        );
                    }
                }
                _ => {}
            }
        });
        if page.is_empty() {
            return;
        }
        let mut td = Money::ZERO;
        let mut tc = Money::ZERO;
        for r in &self.detail {
            td += r.debit;
            tc += r.credit;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "本期合计：借 {} / 贷 {}",
                    td.fmt_money(),
                    tc.fmt_money()
                ))
                .strong(),
            );
        });
    }

    fn show_general(&mut self, ui: &mut Ui) {
        let page = self.paging.slice(&self.general).to_vec();
        let cols = [
            widgets::TCol::new("期间", 100.0).fixed(),
            widgets::TCol::new("摘要", 300.0),
            widgets::TCol::new("借方", 140.0).right(),
            widgets::TCol::new("贷方", 140.0).right(),
            widgets::TCol::new("方向", 44.0).fixed(),
            widgets::TCol::new("余额", 150.0).right(),
        ];
        widgets::grid(ui, "ledger_general", &cols, page.len(), 24.0, |i, c, ui| {
            let r = &page[i];
            match c {
                0 => {
                    ui.label(r.period.label());
                }
                1 => {
                    ui.label(&r.summary);
                }
                2 => widgets::amount_label(ui, r.debit),
                3 => widgets::amount_label(ui, r.credit),
                4 => {
                    ui.label(if r.balance.is_zero() {
                        RichText::new("平").weak()
                    } else {
                        RichText::new(r.dir.label())
                    });
                }
                5 => widgets::amount_label(ui, r.balance),
                _ => {}
            }
        });
    }

    fn show_journal(&mut self, ui: &mut Ui) {
        let page = self.paging.slice(&self.journal).to_vec();
        let cols = [
            widgets::TCol::new("日期", 92.0).fixed(),
            widgets::TCol::new("凭证号", 88.0).fixed(),
            widgets::TCol::new("摘要", 220.0),
            widgets::TCol::new("对方科目", 240.0),
            widgets::TCol::new("借方", 130.0).right(),
            widgets::TCol::new("贷方", 130.0).right(),
            widgets::TCol::new("方向", 44.0).fixed(),
            widgets::TCol::new("出纳", 72.0).fixed(),
            widgets::TCol::new("余额", 140.0).right(),
        ];
        widgets::grid(ui, "ledger_journal", &cols, page.len(), 24.0, |i, c, ui| {
            let r = &page[i];
            match c {
                0 => {
                    ui.label(r.date.format("%Y-%m-%d").to_string());
                }
                1 => {
                    ui.label(RichText::new(&r.voucher_no).monospace());
                }
                2 => {
                    ui.label(&r.summary);
                }
                3 => {
                    ui.label(RichText::new(&r.opposite_accounts).weak());
                }
                4 => widgets::amount_label(ui, r.debit),
                5 => widgets::amount_label(ui, r.credit),
                6 => {
                    ui.label(if r.balance.is_zero() {
                        RichText::new("平").weak()
                    } else {
                        RichText::new(r.dir.label())
                    });
                }
                7 => {
                    ui.label(RichText::new(r.cashier.clone().unwrap_or_default()).weak());
                }
                8 => {
                    widgets::amount_label(ui, r.balance);
                }
                _ => {}
            }
        });
    }

    fn show_daily(&mut self, ui: &mut Ui) {
        let page = self.paging.slice(&self.daily).to_vec();
        let cols = [
            widgets::TCol::new("日期", 120.0).fixed(),
            widgets::TCol::new("借方", 140.0).right(),
            widgets::TCol::new("贷方", 140.0).right(),
            widgets::TCol::new("日末余额", 150.0).right(),
        ];
        widgets::grid(ui, "ledger_daily", &cols, page.len(), 24.0, |i, c, ui| {
            let r = &page[i];
            match c {
                0 => {
                    ui.label(&r.date);
                }
                1 => widgets::amount_label(ui, r.debit),
                2 => widgets::amount_label(ui, r.credit),
                3 => widgets::amount_label(ui, r.balance),
                _ => {}
            }
        });
        if page.is_empty() {
            return;
        }
        let mut td = Money::ZERO;
        let mut tc = Money::ZERO;
        for r in &self.daily {
            td += r.debit;
            tc += r.credit;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "期间合计：借 {} / 贷 {}",
                    td.fmt_money(),
                    tc.fmt_money()
                ))
                .strong(),
            );
        });
    }

    fn print_taoda(&mut self, ctx: &mut AppCtx<'_>) {
        use findb::printform::{LedgerPrint, LedgerPrintRow};
        let company = ctx.db().options().company.clone();
        let acct = ctx
            .chart()
            .get(&self.code)
            .map(|a| format!("{} {}", self.code, a.name))
            .unwrap_or(self.code.clone());
        let tab_name = match self.tab {
            Tab::Detail => "明细账",
            Tab::General => "总账",
            Tab::Journal => "日记账",
            Tab::Daily => "科目日报表",
        };
        // 期初余额方向
        let (bd, bamt) = signed_to_dir_amount(self.begin);
        let begin_dir = if bamt.is_zero() { "平".to_string() } else { bd.label().to_string() };

        let (title, rows): (String, Vec<LedgerPrintRow>) = match self.tab {
            Tab::Detail => (
                "明细账".to_string(),
                self.detail
                    .iter()
                    .map(|r| LedgerPrintRow {
                        date: r.date.format("%Y-%m-%d").to_string(),
                        voucher_no: r.voucher_no.clone(),
                        summary: r.summary.clone(),
                        debit: r.debit,
                        credit: r.credit,
                        dir: if r.balance.is_zero() {
                            "平".to_string()
                        } else {
                            r.dir.label().to_string()
                        },
                        balance: r.balance,
                    })
                    .collect(),
            ),
            Tab::General => (
                "总账".to_string(),
                self.general
                    .iter()
                    .map(|r| LedgerPrintRow {
                        date: r.period.code(),
                        voucher_no: String::new(),
                        summary: r.summary.clone(),
                        debit: r.debit,
                        credit: r.credit,
                        dir: if r.balance.is_zero() {
                            "平".to_string()
                        } else {
                            r.dir.label().to_string()
                        },
                        balance: r.balance,
                    })
                    .collect(),
            ),
            Tab::Journal => (
                "日记账".to_string(),
                self.journal
                    .iter()
                    .map(|r| LedgerPrintRow {
                        date: r.date.format("%Y-%m-%d").to_string(),
                        voucher_no: r.voucher_no.clone(),
                        summary: r.summary.clone(),
                        debit: r.debit,
                        credit: r.credit,
                        dir: if r.balance.is_zero() {
                            "平".to_string()
                        } else {
                            r.dir.label().to_string()
                        },
                        balance: r.balance,
                    })
                    .collect(),
            ),
            Tab::Daily => {
                ctx.error("科目日报表暂不支持套打，请切换为明细账/总账/日记账");
                return;
            }
        };
        let ledger = LedgerPrint {
            title,
            account_name: acct,
            period_label: format!("{}~{}", self.from, self.to),
            begin_dir,
            begin_balance: bamt,
            rows,
            page_from_1: true,
        };
        let html = findb::printform::ledger_form_html(&company, &ledger);
        let name = format!("{tab_name}_{}_{}", self.code, self.from);
        match crate::views::export::print_html_content(&name, &html) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let name = ctx
            .chart()
            .get(&self.code)
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let tab_name = match self.tab {
            Tab::Detail => "明细账",
            Tab::General => "总账",
            Tab::Journal => "日记账",
            Tab::Daily => "科目日报表",
        };
        let title = format!("{}_{}", self.code, name);
        let print_title = format!("{} {}_{}", tab_name, self.code, name);
        let mut sh = match self.tab {
            Tab::Detail => {
                let mut sh = crate::views::export::Sheet::new(
                    "明细账",
                    vec![
                        "日期".into(),
                        "凭证号".into(),
                        "摘要".into(),
                        "借方".into(),
                        "贷方".into(),
                        "方向".into(),
                        "余额".into(),
                    ],
                );
                for r in &self.detail {
                    sh.push(vec![
                        r.date.format("%Y-%m-%d").to_string(),
                        r.voucher_no.clone(),
                        r.summary.clone(),
                        r.debit.fmt_plain(),
                        r.credit.fmt_plain(),
                        r.dir.label().to_string(),
                        r.balance.fmt_plain(),
                    ]);
                }
                sh
            }
            Tab::General => {
                let mut sh = crate::views::export::Sheet::new(
                    "总账",
                    vec![
                        "期间".into(),
                        "摘要".into(),
                        "借方".into(),
                        "贷方".into(),
                        "方向".into(),
                        "余额".into(),
                    ],
                );
                for r in &self.general {
                    sh.push(vec![
                        r.period.code(),
                        r.summary.clone(),
                        r.debit.fmt_plain(),
                        r.credit.fmt_plain(),
                        r.dir.label().to_string(),
                        r.balance.fmt_plain(),
                    ]);
                }
                sh
            }
            Tab::Journal => {
                let mut sh = crate::views::export::Sheet::new(
                    "日记账",
                    vec![
                        "日期".into(),
                        "凭证号".into(),
                        "摘要".into(),
                        "对方科目".into(),
                        "借方".into(),
                        "贷方".into(),
                        "方向".into(),
                        "余额".into(),
                    ],
                );
                for r in &self.journal {
                    sh.push(vec![
                        r.date.format("%Y-%m-%d").to_string(),
                        r.voucher_no.clone(),
                        r.summary.clone(),
                        r.opposite_accounts.clone(),
                        r.debit.fmt_plain(),
                        r.credit.fmt_plain(),
                        r.dir.label().to_string(),
                        r.balance.fmt_plain(),
                    ]);
                }
                sh
            }
            Tab::Daily => {
                let mut sh = crate::views::export::Sheet::new(
                    "科目日报表",
                    vec!["日期".into(), "借方".into(), "贷方".into(), "日末余额".into()],
                );
                for r in &self.daily {
                    sh.push(vec![
                        r.date.clone(),
                        r.debit.fmt_plain(),
                        r.credit.fmt_plain(),
                        r.balance.fmt_plain(),
                    ]);
                }
                sh
            }
        };
        sh.headers.insert(0, "科目".to_string());
        for row in sh.rows.iter_mut() {
            row.insert(0, format!("{} {}", self.code, name));
        }
        match crate::views::export::run_export(&sh, &title, &print_title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}
