//! 请求 / 响应数据结构（与前端 JSON 契约）

use fincore::user::{DataScope, Perm, Role, User};
use fincore::Voucher;
use serde::{Deserialize, Serialize};

use crate::state::period_to_str;

/// 对外暴露的用户信息（不含口令等敏感字段）
#[derive(Clone, Serialize)]
pub struct PublicUser {
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub role_label: String,
    /// 兼任岗位（主岗位在 role；显示名由 role_labels 合成）
    pub roles: Vec<Role>,
    pub is_admin: bool,
    pub device_name: String,
    /// 是否已停用（前端据此显示账号状态）
    pub disabled: bool,
    /// 该用户拥有的权限（snake_case 名称，最终生效口径）
    pub perms: Vec<String>,
    /// 在角色基础上额外授予的权限（snake_case）
    pub extra_perms: Vec<String>,
    /// 在角色基础上明确关闭的权限（snake_case）
    pub deny_perms: Vec<String>,
    /// 下次登录是否强制改密
    pub must_change_pwd: bool,
    /// 锁定截止时刻（`%Y-%m-%d %H:%M:%S`），None 表示未锁定
    pub locked_until: Option<String>,
    /// 上次登录时间（空串表示从未登录）
    pub last_login_at: String,
    /// 备注
    pub memo: String,
    /// 数据权限范围
    pub data_scope: DataScope,
}

/// 平台级登录返回的用户信息（不含账套内权限，需选账套后再取）
#[derive(Clone, Serialize)]
pub struct PlatformUser {
    pub username: String,
    pub display_name: String,
    pub is_admin: bool,
    pub must_change_pwd: bool,
}

/// 登录响应：返回平台用户与"可进入的账套列表"
#[derive(Serialize)]
pub struct LoginResp {
    pub user: PlatformUser,
    /// 是否强制要求改密
    pub must_change_pwd: bool,
    /// 本次登录是否触发了初始化（新体系下恒为 false，保留字段以兼容前端）
    pub setup: bool,
    /// 该用户可进入的账套（管理员=全部，普通=本人创建的）
    pub books: Vec<serde_json::Value>,
}

/// 账号（开通/列表用，不含敏感字段）
#[derive(Clone, Serialize)]
pub struct PlatformUserItem {
    pub username: String,
    pub display_name: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub must_change_pwd: bool,
    /// 是否已绑定登录设备（"一人一机"）
    pub device_bound: bool,
    pub created_at: String,
}

impl PlatformUserItem {
    pub fn from_realm(u: &crate::realm::RealmUser) -> Self {
        Self {
            username: u.username.clone(),
            display_name: u.display_name.clone(),
            is_admin: u.is_admin,
            disabled: u.disabled,
            must_change_pwd: u.must_change_pwd,
            device_bound: !u.device_id.is_empty(),
            created_at: u.created_at.clone(),
        }
    }
}

/// 开通/修改账号请求
#[derive(Deserialize)]
pub struct PlatformUserReq {
    pub username: String,
    pub display_name: String,
    #[serde(default)]
    pub password: String,
    /// 是否管理员（默认 false，即普通用户）
    #[serde(default)]
    pub is_admin: bool,
}

/// 自建账套请求
#[derive(Deserialize)]
pub struct CreateBookReq {
    /// 账套标识（文件名，不含扩展名）；为空则自动生成
    #[serde(default)]
    pub key: String,
    /// 公司名（建账时写入账套参数）
    #[serde(default)]
    pub company: String,
    /// 启用期间 ymm；0 表示取当前月
    #[serde(default)]
    pub start_period: i32,
}

impl PublicUser {
    pub fn from_user(u: &User) -> Self {
        let perm_code = |p: &Perm| {
            serde_json::to_value(*p)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default()
        };
        let perms = Perm::all()
            .iter()
            .filter(|p| u.can(**p))
            .map(perm_code)
            .collect();
        let extra_perms = u.extra_perms.iter().map(perm_code).collect();
        let deny_perms = u.deny_perms.iter().map(perm_code).collect();
        PublicUser {
            username: u.username.clone(),
            display_name: u.display_name.clone(),
            role: u.role,
            role_label: u.role_labels(),
            roles: u.roles.clone(),
            is_admin: u.is_admin(),
            device_name: u.device_name.clone(),
            disabled: u.disabled,
            perms,
            extra_perms,
            deny_perms,
            must_change_pwd: u.must_change_pwd,
            locked_until: u.locked_until.clone(),
            last_login_at: u.last_login_at.clone(),
            memo: u.memo.clone(),
            data_scope: u.data_scope.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct SetupStatus {
    /// 管理员账号是否已设定（不暴露用户名，避免登录页被枚举）
    pub admin_set: bool,
    /// 账套是否已初始化建账（公司名/启用期间是否已设定）
    pub needs_setup: bool,
    pub company: String,
    pub version: String,
    /// 账套文件路径（内部诊断用，前端不展示）
    #[serde(skip_serializing)]
    pub book: Option<String>,
}

impl VoucherDetail {
    pub fn from_voucher(v: Voucher) -> Self {
        let period_label = period_to_str(v.period);
        let voucher_no = v.voucher_no();
        VoucherDetail {
            voucher: v,
            voucher_no,
            period_label,
            // from_voucher 拿不到 db（纯函数），更正链由 get_voucher 单独补
            amend: None,
        }
    }
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
    /// 浏览器/设备指纹（前端生成并持久化在 localStorage）
    pub device_id: String,
    /// 设备展示名（浏览器 UA / 主机名）
    pub device_name: String,
    /// 账套 key（空 = 默认账套）
    #[serde(default)]
    pub book_key: String,
}

/// 账号修改请求（管理员用，仅改展示名/停用/管理员标志，口令走 reset-password）
#[derive(Deserialize)]
pub struct UpdatePlatformUserReq {
    pub display_name: Option<String>,
    pub disabled: Option<bool>,
    pub is_admin: Option<bool>,
}

#[derive(Deserialize)]
pub struct ChangePwdReq {
    pub old: String,
    pub new: String,
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    pub username: String,
    pub display_name: String,
    /// 单密码统一后不再需要独立口令：可选、兼容旧前端；若传了则忽略。
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub role: Role,
    /// 兼任岗位（身兼多职；有效权限 = 主岗位 ∪ 兼任 ∪ extra − deny）
    #[serde(default)]
    pub roles: Vec<Role>,
    /// 是否强制首次登录改密（默认 true，测试场景可设为 false）
    #[serde(default = "default_true")]
    pub must_change_pwd: bool,
    /// 备注（可选）
    #[serde(default)]
    pub memo: String,
    /// 在角色基础上额外授予的权限
    #[serde(default)]
    pub extra_perms: Vec<Perm>,
    /// 在角色基础上明确关闭的权限
    #[serde(default)]
    pub deny_perms: Vec<Perm>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct UpdateUserReq {
    pub display_name: Option<String>,
    pub role: Option<Role>,
    /// 兼任岗位整体替换（与主岗位去重后落库）
    #[serde(default)]
    pub roles: Option<Vec<Role>>,
    pub disabled: Option<bool>,
    pub must_change_pwd: Option<bool>,
    pub memo: Option<String>,
    /// 完整替换数据范围（前端提交当前 scope 的完整副本）
    pub data_scope: Option<DataScope>,
    /// 完整替换在角色基础上额外授予的权限
    pub extra_perms: Option<Vec<Perm>>,
    /// 完整替换在角色基础上明确关闭的权限
    pub deny_perms: Option<Vec<Perm>>,
}

#[derive(Deserialize)]
pub struct ResetPwdReq {
    pub new: String,
}

#[derive(Deserialize)]
pub struct PeriodReq {
    pub ymm: i32,
}

#[derive(Deserialize)]
pub struct BatchPostReq {
    /// 要批量记账的凭证 id 列表
    pub ids: Vec<i64>,
}

/// 凭证分录（前端提交的字段；可选要素不传则沿用原行，避免 Web 编辑丢桌面录入的数据）
#[derive(Deserialize, Default)]
pub struct VoucherEntryDto {
    pub line: i32,
    pub summary: String,
    pub account_code: String,
    /// 借方金额（十进制字符串）
    pub debit: String,
    /// 贷方金额（十进制字符串）
    pub credit: String,
    /// 辅助核算（客户/供应商/部门/职员/项目/存货/银行）
    #[serde(default)]
    pub aux: Option<fincore::AuxRef>,
    /// 数量（数量核算科目）
    #[serde(default)]
    pub qty: Option<fincore::Money>,
    /// 单价
    #[serde(default)]
    pub price: Option<fincore::Money>,
    /// 原币币种（如 USD）
    #[serde(default)]
    pub currency: Option<String>,
    /// 汇率（1 外币 = ? 本位币）
    #[serde(default)]
    pub rate: Option<rust_decimal::Decimal>,
    /// 原币金额
    #[serde(default)]
    pub amount_for: Option<fincore::Money>,
    /// 现金流量项目编码（现金/银行科目）
    #[serde(default)]
    pub cf: Option<String>,
}

/// 保存凭证请求
#[derive(Deserialize)]
pub struct SaveVoucherReq {
    pub id: i64,
    /// 期间 ymm
    pub period: i32,
    /// 日期 YYYY-MM-DD
    pub date: String,
    pub word: String,
    pub no: i32,
    pub attachments: i32,
    pub memo: String,
    pub entries: Vec<VoucherEntryDto>,
}

/// 发票新增/更新请求
#[derive(Deserialize, Default)]
pub struct InvoiceReq {
    pub id: i64,
    /// in / out
    pub kind: String,
    pub code: String,
    pub number: String,
    pub date: String,
    pub buyer: String,
    pub seller: String,
    /// 价税合计（十进制字符串）
    pub amount_tax: String,
    /// 不含税金额
    pub amount: String,
    /// 税额
    pub tax: String,
    pub tax_rate: String,
    pub status: String,
    pub memo: String,
}

/// 发票状态流转请求
#[derive(Deserialize)]
pub struct InvoiceStatusReq {
    pub status: String,
}

/// 凭证列表项（轻量，不含分录明细）
#[derive(Serialize)]
pub struct VoucherListItem {
    pub id: i64,
    pub period: String,
    pub date: String,
    pub word: String,
    pub no: i32,
    pub voucher_no: String,
    pub summary: String,
    pub debit_total: String,
    pub credit_total: String,
    pub status: String,
    pub status_label: String,
    pub prepared_by: String,
}

/// 凭证列表分页结果。
///
/// 为什么单独一个结构而不是给 `Vec<VoucherListItem>` 加字段：老接口
/// `GET /api/vouchers` 返回裸数组，有别的调用方（含导出、OpenAPI 文档、
/// 第三方脚本）依赖它。新增分页端点而不是改老接口的形状，破坏面为零。
///
/// `total` 是**过滤后**的真实张数（含数据范围），页码和「共 N 张」都靠它；
/// `debit_total` / `credit_total` 是**全量合计**（不是本页合计）——
/// 财务界面里「本期间借方合计」指的就是这个期间的总账数字，
/// 按页算合计会让人以为对不上账。
#[derive(Serialize)]
pub struct VoucherPage {
    pub items: Vec<VoucherListItem>,
    pub page: i64,
    pub page_size: i64,
    pub total: i64,
    /// 全量借方合计（字符串金额，与 item 字段同格式，避免前端浮点误差）
    pub debit_total: String,
    /// 全量贷方合计
    pub credit_total: String,
}

#[derive(Serialize)]
pub struct Dashboard {
    pub company: String,
    pub start_period: String,
    pub current_period: String,
    pub closed_upto: Option<String>,
    pub vouchers: i64,
    pub entries: i64,
    pub accounts: i64,
    /// 首页首屏那两块（本期赚没赚 + 钱在哪）。取数失败时为 None，首页照常显示，
    /// 不会因为某张报表算不出来就整页报错。
    pub brief: Option<HomeBrief>,
}

/// 首页首屏：本期赚没赚 + 钱在哪
///
/// 这两块必须并排出现才看得懂：利润是**账面**赚的，「钱在哪」是真正压在外面的
/// 钱。小厂常见的死法是账面赚钱、钱全压在客户手里 —— 只看净利润看不出来。
#[derive(Serialize)]
pub struct HomeBrief {
    /// 统计期间（YYYY-MM）
    pub period: String,
    /// 营业收入（本期）
    pub revenue: String,
    /// 营业成本（本期；口径含未结转的制造费用，与利润表一致）
    pub cost: String,
    /// 净利润（本期）
    pub net_profit: String,
    /// 本年累计净利润
    pub net_profit_ytd: String,
    /// 上月净利润（只看环比才有意义）
    pub net_profit_prev: String,
    /// 本月 − 上月（正=改善）
    pub net_profit_diff: String,
    /// 利润率 %（收入为 0 时缺省）
    pub margin: Option<f64>,
    /// 成本构成 Top 3（"钱都花哪了"）
    pub cost_top: Vec<HomeDriver>,
    /// 没记账的凭证张数（草稿 + 已审未记）—— 这些没算进上面任何数字
    pub unposted: i64,
    pub money: HomeMoney,
}

#[derive(Serialize)]
pub struct HomeDriver {
    pub name: String,
    pub amount: String,
}

/// 「钱在哪」：应收应付合到一起看
#[derive(Serialize)]
pub struct HomeMoney {
    /// 别人欠我（客户应收未收）
    pub receivable: String,
    /// 我欠别人（供应商应付未付）
    pub payable: String,
    /// 我预付出去的（供应商欠我货）
    pub advance_out: String,
    /// 客户预收我的（我欠客户钱/货）
    pub advance_in: String,
    /// 手头能动用的钱（现金 + 银行）
    pub cash: String,
    /// 净头寸 = 应收 + 预付 − 应付 − 预收
    pub net: String,
    /// 净头寸方向：true = 别人欠我的多
    pub net_is_receivable: bool,
    /// 超过 90 天没收回的应收
    pub overdue_90: String,
    pub overdue_90_count: usize,
    pub top_customers: Vec<HomeParty>,
    pub top_suppliers: Vec<HomeParty>,
}

/// 一户往来：金额 + 账龄 + 「同一家两种身份」的轧差
#[derive(Serialize)]
pub struct HomeParty {
    pub code: String,
    pub name: String,
    /// 本方向未结金额
    pub amount: String,
    /// 最老一笔的天数
    pub max_days: i64,
    /// 另一身份方向的金额（同一家既欠我又被我欠时才有）
    pub also: String,
    /// 轧差后净额（永远是正数，方向看 net_is_receivable）
    pub net: String,
    /// 净额方向：true = 他欠我
    pub net_is_receivable: bool,
    /// 这家同时是客户和供应商
    pub both: bool,
}

/// 科目余额表行（对外契约：方向已展开、金额已格式化，前端直接渲染）
#[derive(Serialize)]
pub struct TrialRow {
    pub account_code: String,
    pub account_name: String,
    /// 期初方向："借" / "贷" / "平"
    pub begin_dir: String,
    /// 期初余额（绝对值，已格式化）
    pub begin: String,
    /// 本期借方（已格式化）
    pub debit: String,
    /// 本期贷方（已格式化）
    pub credit: String,
    /// 期末方向
    pub end_dir: String,
    /// 期末余额（绝对值，已格式化）
    pub end: String,
    pub ytd_debit: String,
    pub ytd_credit: String,
}

impl TrialRow {
    pub fn from_row(r: &fincore::balance::BalanceRow) -> Self {
        let (b_dir, b_amt) = r.begin_dir_amount();
        let (e_dir, e_amt) = r.end_dir_amount();
        TrialRow {
            account_code: r.account_code.clone(),
            account_name: r.account_name.clone(),
            begin_dir: b_dir.label().to_string(),
            begin: b_amt.fmt_money(),
            debit: r.debit.fmt_money(),
            credit: r.credit.fmt_money(),
            end_dir: e_dir.label().to_string(),
            end: e_amt.fmt_money(),
            ytd_debit: r.ytd_debit.fmt_money(),
            ytd_credit: r.ytd_credit.fmt_money(),
        }
    }
}

/// 凭证详情（补上列表同款的 voucher_no，避免前端拿不到凭证号）
#[derive(Serialize)]
pub struct VoucherDetail {
    #[serde(flatten)]
    pub voucher: Voucher,
    /// 凭证号（如 记-0001）
    pub voucher_no: String,
    /// 期间 "YYYY-MM"
    pub period_label: String,
    /// 更正链：更正自谁 / 被谁更正 / 更正原因
    ///
    /// `skip_serializing_if = "Option::is_none"`：绝大多数凭证既没被更正、
    /// 也不是更正凭证，前端拿到 null 就当普通凭证渲染，不必多写分支。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amend: Option<findb::vouchers::AmendLink>,
}

/// 导入预检请求
#[derive(Deserialize)]
pub struct ImportAnalyzeReq {
    /// begin = 期初余额表；voucher = 凭证
    pub kind: String,
    /// CSV 文本（选择文件上传时可为空）
    #[serde(default)]
    pub text: String,
    /// 来源模板：generic / kingdee / yonyou
    #[serde(default)]
    pub template: String,
    /// Excel 文件内容（base64，.xlsx/.xls/.ods）；与 text 二选一
    #[serde(default)]
    pub file: Option<String>,
    /// 期间（YYYYMM）。预检要靠它判断「这个期间是不是已经结账了」——
    /// 闭期导进去也记账不了，必须在写入前就告诉用户。
    /// 0 / 不传 = 用账套当前期间。
    #[serde(default)]
    pub period: i32,
    /// 科目映射（源编码 → 目标编码）。预检也要带上：映射后能落到真实科目上的
    /// 就不该再报「缺失科目」，否则用户刚选完映射、预检还是说缺，会开始不信它。
    #[serde(default)]
    pub mapping: std::collections::HashMap<String, String>,
}

/// 导入执行请求
#[derive(Deserialize)]
pub struct ImportRunReq {
    /// begin = 期初余额表；voucher = 凭证
    pub kind: String,
    /// CSV 文本（选择文件上传时可为空）
    #[serde(default)]
    pub text: String,
    /// 来源模板：generic / kingdee / yonyou
    #[serde(default)]
    pub template: String,
    /// Excel 文件内容（base64，.xlsx/.xls/.ods）；与 text 二选一
    #[serde(default)]
    pub file: Option<String>,
    /// 凭证导入时的期间（YYYYMM）
    #[serde(default)]
    pub period: i32,
    /// 缺失科目映射：源编码 → 目标编码
    #[serde(default)]
    pub mapping: std::collections::HashMap<String, String>,
}

// ===== 从 handlers.rs 收进来的共享请求/响应结构（v1.8.59）=====

#[derive(Deserialize)]
pub struct OrderTransitionReq {
    pub status: String,
}

pub enum WfGate {
    /// 无流程，或已到「通过」终态 → 调用方照常执行原业务动作
    Continue,
    /// 未到终态，已推进到某节点 → 调用方只推进流程、**不**改业务状态
    Pending(String),
    /// 本次动作被驳回 → 调用方不要改业务状态
    Rejected,
}

#[derive(Deserialize)]
pub struct AssetReq {
    #[serde(default)]
    pub id: i64,
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default)]
    pub dept: String,
    pub asset_account: String,
    pub dep_account: String,
    pub expense_account: String,
    pub original_value: String,
    /// 残值率按百分数录入（5 = 5%）
    #[serde(default)]
    pub residual_rate: String,
    pub life_months: i32,
    #[serde(default)]
    pub method: String,
    /// 启用期间 YYYYMM
    pub start_period: i32,
    #[serde(default)]
    pub memo: String,
}

#[derive(serde::Deserialize, Default)]
pub struct KindQuery {
    #[serde(default)]
    pub kind: String,
}

#[derive(Deserialize)]
pub struct PayrollInput {
    pub employee: String,
    #[serde(default)]
    pub dept: String,
    pub gross: String,
    #[serde(default)]
    pub social: String,
    #[serde(default)]
    pub housing: String,
    #[serde(default)]
    pub deduction: String,
    /// 专项附加扣除
    #[serde(default)]
    pub additional: String,
    #[serde(default)]
    pub social_co: String,
    #[serde(default)]
    pub housing_co: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Clone, Copy)]
pub enum StatementKind {
    BalanceSheet,
    Income,
    CashFlow,
    Equity,
}

#[derive(serde::Serialize)]
pub struct ReconcileSheetDto {
    pub label: String,
    pub rows: usize,
    pub total: String,
    pub hint: String,
}

#[derive(serde::Serialize)]
pub struct ReconcileCheckDto {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}
