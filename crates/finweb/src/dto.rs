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

#[derive(Serialize)]
pub struct Dashboard {
    pub company: String,
    pub start_period: String,
    pub current_period: String,
    pub closed_upto: Option<String>,
    pub vouchers: i64,
    pub entries: i64,
    pub accounts: i64,
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
