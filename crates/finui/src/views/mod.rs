//! 各功能界面

pub mod about;
pub mod account;
pub mod advanced;
pub mod assets;
pub mod aux_view;
pub mod automation;
pub mod backup;
pub mod balance_table;
pub mod bank_rec;
pub mod begin;
pub mod budget;
pub mod budget_alerts;
pub mod claims;
pub mod custom_report;
pub mod dashboard;
pub mod dim_profit;
pub mod export;
pub mod funds;
pub mod help;
pub mod inventory;
pub mod inventory_deep;
pub mod ledger;
pub mod login;
pub mod logs;
pub mod options;
pub mod overview;
pub mod payroll;
pub mod period_end;
pub mod reports;
pub mod security;
pub mod settle_view;
pub mod scm_deep;
pub mod template;
pub mod users;
pub mod voucher_edit;
pub mod voucher_list;

use crate::state::{AppCtx, DataKind, NavItem};

#[derive(Default)]
pub struct Views {
    pub dashboard: dashboard::Dashboard,
    pub overview: overview::OverviewView,
    pub voucher_edit: voucher_edit::VoucherEdit,
    pub voucher_list: voucher_list::VoucherList,
    pub account: account::AccountView,
    pub begin: begin::BeginView,
    pub ledger: ledger::LedgerView,
    pub balance_table: balance_table::BalanceTableView,
    pub reports: reports::ReportsView,
    pub custom_report: custom_report::CustomReportView,
    pub period_end: period_end::PeriodEndView,
    pub budget: budget::BudgetView,
    pub budget_alerts: budget_alerts::BudgetAlertsView,
    pub budget_analysis: funds::BudgetAnalysisView,
    pub funds: funds::FundsView,
    pub cost: funds::CostView,
    pub dim_profit: dim_profit::DimProfitView,
    pub multi_column: advanced::MultiColumnView,
    pub summary_table: advanced::SummaryTableView,
    pub ratios: advanced::RatiosView,
    pub manufacturing: advanced::ManufacturingView,
    pub approval: advanced::ApprovalView,
    pub report_notes: advanced::ReportNotesView,
    pub archive: advanced::ArchiveView,
    pub aux: aux_view::AuxView,
    pub users: users::UsersView,
    pub security: security::SecurityView,
    pub options: options::OptionsView,
    pub backup: backup::BackupView,
    pub logs: logs::LogsView,
    pub template: template::TemplateView,
    pub assets: assets::AssetsView,
    pub bank_rec: bank_rec::BankRecView,
    pub settle: settle_view::SettleView,
    pub automation: automation::AutomationView,
    pub inventory: inventory::InventoryView,
    pub inventory_deep: inventory_deep::InventoryDeepView,
    pub scm_deep: scm_deep::ScmDeepView,
    pub payroll: payroll::PayrollView,
    pub claims: claims::ClaimsView,
    pub help: help::HelpView,
    pub about: about::AboutView,
    /// 上一次渲染的导航项，用于首次进入时初始化过滤条件
    last: Option<NavItem>,
}

impl Views {
    /// 标记某界面已经完成进入初始化（避免下一次渲染重复初始化，冲掉刚载入的数据）
    pub fn mark_entered(&mut self, n: NavItem) {
        self.last = Some(n);
    }

    /// 切换期间 / 账套数据变动后，让所有界面下次渲染时重新取数
    pub fn invalidate_all(&mut self) {
        self.dashboard.invalidate();
        self.overview.invalidate();
        self.voucher_edit.invalidate();
        self.voucher_list.invalidate();
        self.account.invalidate();
        self.begin.invalidate();
        self.ledger.invalidate();
        self.balance_table.invalidate();
        self.reports.invalidate();
        self.custom_report.invalidate();
        self.period_end.invalidate();
        self.budget.invalidate();
        self.budget_alerts.invalidate();
        self.budget_analysis.invalidate();
        self.funds.invalidate();
        self.cost.invalidate();
        self.dim_profit.invalidate();
        self.multi_column.invalidate();
        self.summary_table.invalidate();
        self.ratios.invalidate();
        self.manufacturing.invalidate();
        self.approval.invalidate();
        self.report_notes.invalidate();
        self.archive.invalidate();
        self.aux.invalidate();
        self.users.invalidate();
        self.security.invalidate();
        self.options.invalidate();
        self.logs.invalidate();
        self.help.invalidate();
        self.inventory_deep.invalidate();
        self.scm_deep.invalidate();
    }

    /// 某模块数据变动后，让相关界面刷新
    pub fn data_changed(&mut self, what: DataKind) {
        self.dashboard.invalidate();
        self.overview.invalidate();
        match what {
            DataKind::Voucher => {
                self.voucher_list.invalidate();
                self.ledger.invalidate();
                self.balance_table.invalidate();
                self.reports.invalidate();
                self.period_end.invalidate();
                self.budget.invalidate();
                self.dim_profit.invalidate();
                self.multi_column.invalidate();
                self.summary_table.invalidate();
                self.ratios.invalidate();
            }
            DataKind::Account => {
                self.account.invalidate();
                self.balance_table.invalidate();
                self.reports.invalidate();
            }
            DataKind::Aux => {
                self.aux.invalidate();
                self.voucher_edit.invalidate();
            }
            DataKind::Begin => {
                self.begin.invalidate();
                self.balance_table.invalidate();
                self.reports.invalidate();
                self.ledger.invalidate();
            }
            DataKind::User => {
                self.users.invalidate();
                self.security.invalidate();
            }
            DataKind::Options => {
                self.options.invalidate();
                self.reports.invalidate();
            }
            DataKind::Business => {
                self.voucher_list.invalidate();
                self.ledger.invalidate();
                self.balance_table.invalidate();
                self.reports.invalidate();
            }
            DataKind::Mgmt => {
                self.budget.invalidate();
                self.dim_profit.invalidate();
                self.custom_report.invalidate();
                self.reports.invalidate();
            }
            DataKind::Asset => {
            }
            DataKind::Template => {
                self.voucher_edit.invalidate();
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut egui::Ui) {
        let nav = ctx.st.nav;
        if self.last != Some(nav) {
            match nav {
                NavItem::Dashboard => self.dashboard.invalidate(),
                NavItem::Overview => self.overview.enter(ctx),
                NavItem::VoucherNew => self.voucher_edit.invalidate(),
                NavItem::VoucherList => self.voucher_list.enter(ctx),
                NavItem::Template => self.template.enter(ctx),
                NavItem::Account => self.account.invalidate(),
                NavItem::BeginBalance => self.begin.invalidate(),
                NavItem::Ledger => self.ledger.enter(ctx, None),
                NavItem::BalanceTable => self.balance_table.enter(ctx),
                NavItem::Reports => self.reports.enter(ctx),
                NavItem::CustomReport => self.custom_report.enter(ctx),
                NavItem::PeriodEnd => self.period_end.enter(ctx),
                NavItem::Assets => self.assets.enter(ctx),
                NavItem::BankRec => self.bank_rec.enter(ctx),
                NavItem::Settle => self.settle.enter(ctx),
                NavItem::Automation => self.automation.enter(ctx),
                NavItem::Inventory => self.inventory.enter(ctx),
                NavItem::InventoryDeep => self.inventory_deep.enter(ctx),
                NavItem::ScmDeep => self.scm_deep.enter(ctx),
                NavItem::Payroll => self.payroll.enter(ctx),
                NavItem::Claims => self.claims.enter(ctx),
                NavItem::Budget => self.budget.enter(ctx),
                NavItem::BudgetAlerts => self.budget_alerts.enter(ctx),
                NavItem::BudgetAnalysis => self.budget_analysis.invalidate(),
                NavItem::Funds => self.funds.invalidate(),
                NavItem::Cost => self.cost.invalidate(),
                NavItem::DimProfit => self.dim_profit.enter(ctx),
                NavItem::MultiColumn => self.multi_column.enter(ctx),
                NavItem::SummaryTable => self.summary_table.enter(ctx),
                NavItem::Ratios => self.ratios.enter(ctx),
                NavItem::Manufacturing => self.manufacturing.enter(ctx),
                NavItem::Approval => self.approval.enter(ctx),
                NavItem::ReportNotes => self.report_notes.enter(ctx),
                NavItem::Archive => self.archive.enter(ctx),
                NavItem::Aux(_) => self.aux.invalidate(),
                NavItem::Users => self.users.invalidate(),
                NavItem::Security => self.security.enter(ctx),
                NavItem::Options => self.options.invalidate(),
                NavItem::Backup => {}
                NavItem::Logs => self.logs.invalidate(),
                NavItem::Help => self.help.enter(ctx),
                NavItem::About => {}
            }
            self.last = Some(nav);
        }

        match nav {
            NavItem::Dashboard => self.dashboard.show(ctx, ui),
            NavItem::Overview => self.overview.show(ctx, ui),
            NavItem::VoucherNew => self.voucher_edit.show(ctx, ui),
            NavItem::Template => self.template.show(ctx, ui),
            NavItem::VoucherList => match self.voucher_list.show(ctx, ui) {
                voucher_list::Action::Open(id) => {
                    ctx.st.nav = NavItem::VoucherNew;
                    self.mark_entered(NavItem::VoucherNew);
                    self.voucher_edit.load(ctx, Some(id));
                }
                voucher_list::Action::New => {
                    ctx.st.nav = NavItem::VoucherNew;
                    self.mark_entered(NavItem::VoucherNew);
                    self.voucher_edit.load(ctx, None);
                }
                voucher_list::Action::None => {}
            },
            NavItem::Account => self.account.show(ctx, ui),
            NavItem::BeginBalance => self.begin.show(ctx, ui),
            NavItem::Ledger => self.ledger.show(ctx, ui),
            NavItem::BalanceTable => self.balance_table.show(ctx, ui),
            NavItem::Reports => self.reports.show(ctx, ui),
            NavItem::CustomReport => self.custom_report.show(ctx, ui),
            NavItem::PeriodEnd => self.period_end.show(ctx, ui),
            NavItem::Assets => self.assets.show(ctx, ui),
            NavItem::BankRec => self.bank_rec.show(ctx, ui),
            NavItem::Settle => self.settle.show(ctx, ui),
            NavItem::Automation => self.automation.show(ctx, ui),
            NavItem::Inventory => self.inventory.show(ctx, ui),
            NavItem::InventoryDeep => self.inventory_deep.show(ctx, ui),
            NavItem::ScmDeep => self.scm_deep.show(ctx, ui),
            NavItem::Payroll => self.payroll.show(ctx, ui),
            NavItem::Claims => self.claims.show(ctx, ui),
            NavItem::Budget => self.budget.show(ctx, ui),
            NavItem::BudgetAlerts => self.budget_alerts.show(ctx, ui),
            NavItem::BudgetAnalysis => self.budget_analysis.show(ctx, ui),
            NavItem::Funds => self.funds.show(ctx, ui),
            NavItem::Cost => self.cost.show(ctx, ui),
            NavItem::DimProfit => self.dim_profit.show(ctx, ui),
            NavItem::MultiColumn => self.multi_column.show(ctx, ui),
            NavItem::SummaryTable => self.summary_table.show(ctx, ui),
            NavItem::Ratios => self.ratios.show(ctx, ui),
            NavItem::Manufacturing => self.manufacturing.show(ctx, ui),
            NavItem::Approval => self.approval.show(ctx, ui),
            NavItem::ReportNotes => self.report_notes.show(ctx, ui),
            NavItem::Archive => self.archive.show(ctx, ui),
            NavItem::Aux(k) => self.aux.show(ctx, ui, k),
            NavItem::Users => self.users.show(ctx, ui),
            NavItem::Security => self.security.show(ctx, ui),
            NavItem::Options => self.options.show(ctx, ui),
            NavItem::Backup => self.backup.show(ctx, ui),
            NavItem::Logs => self.logs.show(ctx, ui),
            NavItem::Help => self.help.show(ctx, ui),
            NavItem::About => self.about.show(ctx, ui),
        }
    }
}
