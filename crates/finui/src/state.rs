//! 应用状态与上下文

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use findb::Db;
use fincore::{AuxKind, Chart, Perm, Period, User};

/// 左侧导航项
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum NavItem {
    /// 首页工作台
    Dashboard,
    /// 管理员 · 账目总览（只读视角，仅系统管理员可见）
    Overview,
    /// 凭证填制（新增）
    VoucherNew,
    /// 凭证查询
    VoucherList,
    /// 科目
    Account,
    /// 期初建账
    BeginBalance,
    /// 明细账 / 总账 / 日记账
    Ledger,
    /// 科目余额表
    BalanceTable,
    /// 会计报表
    Reports,
    /// 期末处理（结转损益 / 结账）
    PeriodEnd,
    /// 固定资产
    Assets,
    /// 出纳银行对账
    BankRec,
    /// 往来核销与账龄
    Settle,
    /// 期末调汇、自动转账、月度检查
    Automation,
    /// 存货核算
    Inventory,
    /// 库存深度（序列号/多单位/账龄/ABC/组装拆卸/分仓库/调拨）
    InventoryDeep,
    /// 采购/销售深度（暂估/对账/配额/订单变更）
    ScmDeep,
    /// 预算预警
    BudgetAlerts,
    /// 工资管理
    Payroll,
    /// 费用报销
    Claims,
    /// 预算管理
    Budget,
    /// 多维损益
    DimProfit,
    /// 资金管理（日报/票据/融资/盘点/支票簿/日记账/借支/预算/预测）
    Funds,
    /// 预算分析（年度逐月 vs 实际，含部门维度）
    BudgetAnalysis,
    /// 成本核算（计价方式配置、期末结价）
    Cost,
    /// 自定义报表
    CustomReport,
    /// 多栏账
    MultiColumn,
    /// 摘要汇总表
    SummaryTable,
    /// 财务指标分析
    Ratios,
    /// 工艺路线 / 报工 / MRP
    Manufacturing,
    /// 审批中心
    Approval,
    /// 报表附注
    ReportNotes,
    /// 电子档案
    Archive,
    /// 凭证模板
    Template,
    /// 辅助档案
    Aux(AuxKind),
    /// 用户与权限
    Users,
    /// 安全中心（口令策略、锁定、登录审计）
    Security,
    /// 账套参数
    Options,
    /// 备份恢复
    Backup,
    /// 操作日志
    Logs,
    /// 帮助
    Help,
    /// 关于
    About,
}

impl NavItem {
    pub fn label(&self) -> String {
        match self {
            NavItem::Dashboard => "首页".to_string(),
            NavItem::Overview => "账目总览".to_string(),
            NavItem::VoucherNew => "填制凭证".to_string(),
            NavItem::VoucherList => "凭证查询".to_string(),
            NavItem::Account => "会计科目".to_string(),
            NavItem::BeginBalance => "期初建账".to_string(),
            NavItem::Ledger => "账簿查询".to_string(),
            NavItem::BalanceTable => "科目余额表".to_string(),
            NavItem::Reports => "会计报表".to_string(),
            NavItem::PeriodEnd => "期末处理".to_string(),
            NavItem::Assets => "固定资产".to_string(),
            NavItem::BankRec => "银行对账".to_string(),
            NavItem::Settle => "往来核销".to_string(),
            NavItem::Automation => "月末自动化".to_string(),
            NavItem::Inventory => "存货核算".to_string(),
            NavItem::InventoryDeep => "库存深度".to_string(),
            NavItem::ScmDeep => "采购销售".to_string(),
            NavItem::BudgetAlerts => "预算预警".to_string(),
            NavItem::Payroll => "工资管理".to_string(),
            NavItem::Claims => "费用报销".to_string(),
            NavItem::Budget => "预算管理".to_string(),
            NavItem::DimProfit => "多维损益".to_string(),
            NavItem::Funds => "资金管理".to_string(),
            NavItem::BudgetAnalysis => "预算分析".to_string(),
            NavItem::Cost => "成本核算".to_string(),
            NavItem::CustomReport => "自定义报表".to_string(),
            NavItem::MultiColumn => "多栏账".to_string(),
            NavItem::SummaryTable => "摘要汇总表".to_string(),
            NavItem::Ratios => "财务指标".to_string(),
            NavItem::Manufacturing => "制造管理".to_string(),
            NavItem::Approval => "审批中心".to_string(),
            NavItem::ReportNotes => "报表附注".to_string(),
            NavItem::Archive => "电子档案".to_string(),
            NavItem::Template => "凭证模板".to_string(),
            NavItem::Aux(k) => format!("{}档案", k.label()),
            NavItem::Users => "用户权限".to_string(),
            NavItem::Security => "安全中心".to_string(),
            NavItem::Options => "账套参数".to_string(),
            NavItem::Backup => "备份恢复".to_string(),
            NavItem::Logs => "操作日志".to_string(),
            NavItem::Help => "帮助".to_string(),
            NavItem::About => "关于".to_string(),
        }
    }

    /// 所属功能组（侧边栏分组标题）
    pub fn group(&self) -> &'static str {
        match self {
            NavItem::Dashboard => "开始",
            NavItem::Overview => "管理员 · 只读",
            NavItem::Template => "凭证", // TEMP
            NavItem::VoucherNew | NavItem::VoucherList => "凭证",
            NavItem::Account | NavItem::BeginBalance | NavItem::Aux(_) => "基础资料",
            NavItem::Ledger | NavItem::BalanceTable | NavItem::Reports | NavItem::CustomReport => {
                "账簿报表"
            }
            NavItem::MultiColumn | NavItem::SummaryTable | NavItem::Ratios | NavItem::ReportNotes => {
                "账簿报表"
            }
            NavItem::PeriodEnd
            | NavItem::Assets
            | NavItem::BankRec
            | NavItem::Settle
            | NavItem::Automation => "期末",
            NavItem::Inventory | NavItem::InventoryDeep | NavItem::ScmDeep | NavItem::Payroll
            | NavItem::Claims | NavItem::Manufacturing => "业务",
            NavItem::Approval | NavItem::Archive => "系统",
            NavItem::Budget | NavItem::BudgetAlerts | NavItem::DimProfit | NavItem::BudgetAnalysis => {
                "管理会计"
            }
            NavItem::Funds => "业务",
            NavItem::Cost => "业务",
            NavItem::Users | NavItem::Security | NavItem::Options | NavItem::Backup
            | NavItem::Logs | NavItem::Help | NavItem::About => "系统",
        }
    }

    /// 需要的权限（None 表示只要登录即可）
    pub fn required_perm(&self) -> Option<Perm> {
        match self {
            NavItem::VoucherNew => Some(Perm::VoucherNew),
            NavItem::Account => Some(Perm::AccountEdit),
            NavItem::Aux(_) => Some(Perm::AuxEdit),
            NavItem::BeginBalance => Some(Perm::Opening),
            NavItem::PeriodEnd | NavItem::Automation => Some(Perm::CarryForward),
            NavItem::Assets | NavItem::Inventory | NavItem::InventoryDeep | NavItem::ScmDeep => {
                Some(Perm::AccountEdit)
            }
            NavItem::BankRec | NavItem::Settle | NavItem::Payroll | NavItem::Claims => {
                Some(Perm::VoucherNew)
            }
            NavItem::Budget | NavItem::BudgetAlerts | NavItem::DimProfit | NavItem::CustomReport
            | NavItem::BudgetAnalysis => Some(Perm::Report),
            NavItem::Funds | NavItem::Cost => Some(Perm::Report),
            NavItem::MultiColumn | NavItem::SummaryTable | NavItem::Ratios | NavItem::ReportNotes
            | NavItem::Archive => Some(Perm::Report),
            NavItem::Manufacturing => Some(Perm::AccountEdit),
            NavItem::Approval => Some(Perm::VoucherNew),
            NavItem::Template => Some(Perm::VoucherNew),
            NavItem::Users | NavItem::Security => Some(Perm::UserManage),
            NavItem::Options => Some(Perm::SysOption),
            NavItem::Backup => Some(Perm::Backup),
            NavItem::Logs => Some(Perm::AuditLog),
            NavItem::Ledger | NavItem::BalanceTable | NavItem::Reports => Some(Perm::Report),
            _ => None,
        }
    }

    /// 是否为管理员专属入口（只读视角），非管理员不可见
    pub fn admin_only(&self) -> bool {
        matches!(self, NavItem::Overview)
    }

    /// 侧边栏顺序（含分组）
    pub fn menu() -> &'static [NavItem] {
        &[
            NavItem::Dashboard,
            NavItem::Overview,
            NavItem::VoucherNew,
            NavItem::VoucherList,
            NavItem::Template,
            NavItem::Account,
            NavItem::BeginBalance,
            NavItem::Aux(AuxKind::Customer),
            NavItem::Aux(AuxKind::Supplier),
            NavItem::Aux(AuxKind::Dept),
            NavItem::Aux(AuxKind::Employee),
            NavItem::Aux(AuxKind::Project),
            NavItem::Aux(AuxKind::Item),
            NavItem::Aux(AuxKind::Bank),
            NavItem::Ledger,
            NavItem::BalanceTable,
            NavItem::Reports,
            NavItem::CustomReport,
            NavItem::MultiColumn,
            NavItem::SummaryTable,
            NavItem::Ratios,
            NavItem::ReportNotes,
            NavItem::PeriodEnd,
            NavItem::Assets,
            NavItem::BankRec,
            NavItem::Settle,
            NavItem::Automation,
            NavItem::Inventory,
            NavItem::InventoryDeep,
            NavItem::ScmDeep,
            NavItem::Manufacturing,
            NavItem::Payroll,
            NavItem::Claims,
            NavItem::Approval,
            NavItem::Archive,
            NavItem::Budget,
            NavItem::BudgetAlerts,
            NavItem::BudgetAnalysis,
            NavItem::DimProfit,
            NavItem::Funds,
            NavItem::Cost,
            NavItem::Users,
            NavItem::Security,
            NavItem::Options,
            NavItem::Backup,
            NavItem::Logs,
            NavItem::Help,
            NavItem::About,
        ]
    }
}

/// 提示消息
#[derive(Clone, Debug)]
pub struct Toast {
    pub msg: String,
    pub is_error: bool,
    /// 到期时间（秒），基于 `ui.input(|i| i.time)`
    pub until: f64,
}

#[derive(Clone, Debug, Default)]
pub struct ToastQueue(pub Vec<Toast>);

impl ToastQueue {
    pub fn push(&mut self, msg: impl Into<String>, is_error: bool, now: f64) {
        self.0.push(Toast {
            msg: msg.into(),
            is_error,
            until: now + if is_error { 8.0 } else { 4.0 },
        });
        // 最多保留 6 条，避免刷屏
        if self.0.len() > 6 {
            self.0.remove(0);
        }
    }
    pub fn retain(&mut self, now: f64) {
        self.0.retain(|t| t.until > now);
    }
}

/// 需要用户二次确认的危险操作
#[derive(Clone, Debug)]
pub enum ConfirmAction {
    /// 删除凭证
    DeleteVoucher(i64),
    /// 删除科目
    DeleteAccount(String),
    /// 删除辅助档案
    DeleteAux(i64),
    /// 删除期初行
    DeleteBegin(i64),
    /// 期末结转损益
    CarryForward(Period),
    /// 期末结账
    ClosePeriod(Period),
    /// 反结账
    UnclosePeriod(Period),
    /// 清空业务数据
    ClearVouchers,
    /// 删除用户
    DeleteUser(i64),
    /// 恢复账套
    RestoreBook(PathBuf),
    /// 重算期初（按期末余额自动填列）
    AutoFillBegin,
    /// 导入内置科目表
    ImportAccounts,
    /// 导入内置现金流量项目
    ImportCashFlowItems,
    /// 删除固定资产卡片
    DeleteAsset(i64),
    /// 删除某期间折旧记录
    DeleteDepreciation(i64),
    /// 清空某科目某期间的银行对账单
    ClearBankStatement(String),
    /// 删除工资记录
    DeletePayroll(i64),
    /// 删除报销单
    DeleteClaim(i64),
    /// 删除凭证模板
    DeleteTemplate(i64),
    /// 删除自定义报表
    DeleteCustomReport(String),
    /// 清空全部预算
    ClearBudget,
    /// 重置某账号的设备绑定（参数：用户名）
    ResetDevice(String),
}

/// 数据变动类型
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DataKind {
    Voucher,
    Account,
    Aux,
    Begin,
    User,
    Options,
    /// 业务单据（存货、工资、报销、核销、对账）
    Business,
    /// 管理会计（预算、自定义报表）
    Mgmt,
    /// 固定资产
    Asset,
    /// 凭证模板 / 摘要库
    Template,
}

#[derive(Clone, Debug)]
pub struct Confirm {
    pub title: String,
    pub message: String,
    pub action: ConfirmAction,
    /// 危险操作标红
    pub dangerous: bool,
}

/// 应用全局状态
pub struct AppState {
    pub db: Option<Arc<Db>>,
    pub chart: Option<Arc<Chart>>,
    pub user: Option<User>,
    pub book_path: Option<PathBuf>,
    /// 辅助档案 "kind:code" → 名称，凭证分录列展示用（避免逐行查库）
    pub aux_names: BTreeMap<String, String>,
    /// 当前业务期间（所有账簿报表的默认口径）
    pub period: Period,
    pub nav: NavItem,
    /// 出纳日记账「登记收付」预填科目（凭证录入页打开时消费一次）
    pub pending_cash: Option<String>,
    pub toasts: ToastQueue,
    /// 状态栏消息（消息, 是否错误）
    pub status: Option<(String, bool)>,
    pub confirm: Option<Confirm>,
    /// 关闭窗口请求
    pub want_quit: bool,
    /// 退出登录请求
    pub want_logout: bool,
    /// 登录成功但被口令策略要求立即改密
    pub must_change_pwd: bool,
    /// 会话空闲计时（登录后启动，超时自动登出）
    pub session: findb::security::Session,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            db: None,
            chart: None,
            user: None,
            book_path: None,
            aux_names: BTreeMap::new(),
            period: Period::default(),
            nav: NavItem::Dashboard,
            pending_cash: None,
            toasts: ToastQueue::default(),
            status: None,
            confirm: None,
            want_quit: false,
            want_logout: false,
            must_change_pwd: false,
            session: findb::security::Session::new(0),
        }
    }
}

impl AppState {
    /// 打开账套并载入上下文
    pub fn attach(&mut self, db: Arc<Db>, path: Option<PathBuf>) -> Result<(), String> {
        let chart = findb::accounts::chart(&db).map_err(|e| e.to_string())?;
        self.aux_names = findb::auxs::full_name_map(&db).unwrap_or_default();
        self.period = findb::periods::current_period(&db).unwrap_or_else(|_| db.options().start_period);
        self.chart = Some(Arc::new(chart));
        self.db = Some(db);
        self.book_path = path;
        Ok(())
    }

    /// 辅助档案有变动后刷新名称缓存
    pub fn reload_aux_names(&mut self) {
        if let Some(db) = &self.db {
            if let Ok(m) = findb::auxs::full_name_map(db) {
                self.aux_names = m;
            }
        }
    }

    /// 科目表有变动后刷新缓存
    #[allow(dead_code)]
    pub fn reload_chart(&mut self) {
        if let Some(db) = &self.db {
            if let Ok(c) = findb::accounts::chart(db) {
                self.chart = Some(Arc::new(c));
            }
        }
    }

    pub fn detach(&mut self) {
        self.db = None;
        self.chart = None;
        self.user = None;
        self.book_path = None;
        self.aux_names.clear();
        self.must_change_pwd = false;
        self.session = findb::security::Session::new(0);
    }
}

/// 传给各个界面的上下文
pub struct AppCtx<'a> {
    pub st: &'a mut AppState,
    /// 当前帧时间（用于 toast 过期）
    pub now: f64,
}

impl<'a> AppCtx<'a> {
    pub fn db(&self) -> &Db {
        self.st.db.as_ref().expect("界面渲染时账套必然已打开")
    }
    pub fn chart(&self) -> &Chart {
        self.st.chart.as_ref().expect("科目表必然已加载")
    }
    pub fn user(&self) -> &User {
        self.st.user.as_ref().expect("已登录")
    }
    pub fn period(&self) -> Period {
        self.st.period
    }
    pub fn set_period(&mut self, p: Period) {
        self.st.period = p;
    }

    /// 权限判断（无权限时自动提示）
    pub fn can(&mut self, p: Perm) -> bool {
        let ok = self.user().can(p);
        if !ok {
            self.error(format!("没有「{}」权限", p.label()));
        }
        ok
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        let now = self.now;
        self.st.toasts.push(msg, false, now);
        self.st.status = Some((String::new(), false));
        self.st.status = None;
    }

    pub fn info(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        let now = self.now;
        self.st.toasts.push(&m, false, now);
        self.st.status = Some((m, false));
    }

    /// 科目表有变动后刷新缓存
    pub fn reload_chart(&mut self) {
        self.st.reload_chart();
    }

    /// 辅助档案有变动后刷新名称缓存
    pub fn reload_aux_names(&mut self) {
        self.st.reload_aux_names();
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        let now = self.now;
        self.st.toasts.push(&m, true, now);
        self.st.status = Some((m, true));
    }

    /// 处理 Result，失败时提示
    pub fn handle<T>(&mut self, r: Result<T, impl std::fmt::Display>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.error(e.to_string());
                None
            }
        }
    }

    pub fn nav(&mut self, n: NavItem) {
        if let Some(p) = n.required_perm() {
            if !self.user().can(p) {
                self.error(format!("没有「{}」权限，无法进入{}", p.label(), n.label()));
                return;
            }
        }
        self.st.nav = n;
    }

    /// 记录操作日志
    pub fn log(&self, module: &str, action: &str, detail: &str) {
        if let Err(e) = self.db().log(self.user().username.as_str(), module, action, detail) {
            log::warn!("写操作日志失败：{e}");
        }
    }

    /// 请求确认
    pub fn confirm(&mut self, title: &str, message: &str, action: ConfirmAction) {
        self.confirm_dangerous(title, message, action, false);
    }

    pub fn confirm_dangerous(
        &mut self,
        title: &str,
        message: &str,
        action: ConfirmAction,
        dangerous: bool,
    ) {
        self.st.confirm = Some(Confirm {
            title: title.to_string(),
            message: message.to_string(),
            action,
            dangerous,
        });
    }
}
