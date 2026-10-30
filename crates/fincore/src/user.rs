//! 用户、角色与权限
//!
//! 财务软件讲究不相容职务分离：制单、审核、记账、结账应是不同的人。
//! 这里用角色 → 权限集合的模型，口令用 argon2id 加盐哈希存储（不上明文）。

use serde::{Deserialize, Serialize};

/// 操作权限
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Perm {
    /// 新增凭证
    VoucherNew,
    /// 修改凭证
    VoucherEdit,
    /// 删除凭证
    VoucherDelete,
    /// 审核凭证
    VoucherAudit,
    /// 反审核
    VoucherUnaudit,
    /// 记账
    VoucherPost,
    /// 反记账
    VoucherUnpost,
    /// 出纳签字
    CashierSign,
    /// 维护科目
    AccountEdit,
    /// 维护辅助档案
    AuxEdit,
    /// 期初建账
    Opening,
    /// 期末结转
    CarryForward,
    /// 期末结账 / 反结账
    PeriodClose,
    /// 查看账簿报表
    Report,
    /// 财务报表（账簿、三大报表、试算、资金、成本、预算、发票——业务岗不可见，对标金蝶报表按角色授权）
    FinReport,
    /// 导出数据（Excel / CSV 文件落地，仅管理员与财务主管）
    Export,
    /// 用户与权限管理
    UserManage,
    /// 账套参数
    SysOption,
    /// 备份恢复
    Backup,
    /// 查看操作日志
    AuditLog,
    /// 订单作业（订单 / 报价 / 执行 / 请购 / 暂估 / 配额——与科目维护分离，订单部独立上岗）
    OrderOps,
    /// 仓储作业（库存调整 / 序列号 / 多单位 / 组装拆卸——与科目维护分离，仓管独立上岗）
    Warehouse,
    /// 价格查看（订单列表 / 套打中的单价与金额字段，服务端强制裁剪）
    PriceView,
    /// 价格修改（无此权限者改单：已有行保留原价，新行单价置零）
    PriceEdit,
    /// 生产作业（MRP / 工艺路线 / 生产订单 / 报工）
    ProductionOps,
    /// 成本作业（计价方式配置 / 期末结价）
    CostOps,
}

impl Perm {
    pub fn label(self) -> &'static str {
        match self {
            Perm::VoucherNew => "填制凭证",
            Perm::VoucherEdit => "修改凭证",
            Perm::VoucherDelete => "删除凭证",
            Perm::VoucherAudit => "审核凭证",
            Perm::VoucherUnaudit => "反审核凭证",
            Perm::VoucherPost => "记账",
            Perm::VoucherUnpost => "反记账",
            Perm::CashierSign => "出纳签字",
            Perm::AccountEdit => "科目维护",
            Perm::AuxEdit => "档案维护",
            Perm::Opening => "期初建账",
            Perm::CarryForward => "期末结转",
            Perm::PeriodClose => "期末结账",
            Perm::Report => "账簿报表",
            Perm::FinReport => "财务报表",
            Perm::Export => "导出数据",
            Perm::UserManage => "用户权限",
            Perm::SysOption => "账套参数",
            Perm::Backup => "备份恢复",
            Perm::AuditLog => "操作日志",
            Perm::OrderOps => "订单作业",
            Perm::Warehouse => "仓储作业",
            Perm::PriceView => "价格查看",
            Perm::PriceEdit => "价格修改",
            Perm::ProductionOps => "生产作业",
            Perm::CostOps => "成本作业",
        }
    }
    pub fn all() -> &'static [Perm] {
        &[
            Perm::VoucherNew,
            Perm::VoucherEdit,
            Perm::VoucherDelete,
            Perm::VoucherAudit,
            Perm::VoucherUnaudit,
            Perm::VoucherPost,
            Perm::VoucherUnpost,
            Perm::CashierSign,
            Perm::AccountEdit,
            Perm::AuxEdit,
            Perm::Opening,
            Perm::CarryForward,
            Perm::PeriodClose,
            Perm::Report,
            Perm::FinReport,
            Perm::Export,
            Perm::UserManage,
            Perm::SysOption,
            Perm::Backup,
            Perm::AuditLog,
            Perm::OrderOps,
            Perm::Warehouse,
            Perm::PriceView,
            Perm::PriceEdit,
            Perm::ProductionOps,
            Perm::CostOps,
        ]
    }
}

/// 角色
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 系统管理员：全部权限
    Admin,
    /// 财务主管：除用户管理外的全部账务权限
    Supervisor,
    /// 会计：制单、记账、报表（无独立审核权限；录入与记账同账户完成）
    #[default]
    Accountant,
    /// 出纳：现金银行相关、出纳签字
    Cashier,
    /// 审核人：只能审核
    Auditor,
    /// 订单专员：订单 / 报价 / 执行 / 请购 / 暂估 + 档案 + 报表（动不了科目表与凭证）
    OrderClerk,
    /// 仓管员：仓储作业 + 报表（动不了科目表与凭证）
    Keeper,
    /// 应收会计：核销 / 收付款单 / 账龄 / 坏账 + 往来建档（不做记账）
    Receivables,
    /// 应付会计：采购订单域 + 付款核销 + 价格（不做记账）
    Payables,
    /// 成本会计：计价配置 / 期末结价 + 成本凭证草稿
    CostAccountant,
    /// 生产计划员：MRP / 工艺路线 / 生产订单 / 报工
    Production,
    /// 只读：查看报表
    Viewer,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Admin => "系统管理员",
            Role::Supervisor => "财务主管",
            Role::Accountant => "会计",
            Role::Cashier => "出纳",
            Role::Auditor => "审核人",
            Role::OrderClerk => "订单专员",
            Role::Keeper => "仓管员",
            Role::Receivables => "应收会计",
            Role::Payables => "应付会计",
            Role::CostAccountant => "成本会计",
            Role::Production => "生产计划员",
            Role::Viewer => "只读",
        }
    }
    pub fn all() -> &'static [Role] {
        &[
            Role::Admin,
            Role::Supervisor,
            Role::Accountant,
            Role::Cashier,
            Role::Auditor,
            Role::OrderClerk,
            Role::Keeper,
            Role::Receivables,
            Role::Payables,
            Role::CostAccountant,
            Role::Production,
            Role::Viewer,
        ]
    }
    pub fn perms(self) -> &'static [Perm] {
        use Perm::*;
        // 备份（Backup）与导出（Export）是数据外带通道：
        // - Backup 仅限系统管理员：普通账户拿不到账套文件副本，防止数据转移
        // - Export 仅限管理员、财务主管、应收/应付会计：
        //   应收应付会计的活就是「把账龄表发给客户」「把应付清单发给老板」，
        //   不给导出他们就只能截图 —— 于是绕过审计留痕，比开权限更糟。
        //   其余岗位（会计/出纳/仓管/只读…）仍然只能打印预览（纸面留痕可控）。
        match self {
            Role::Admin => Perm::all(),
            Role::Supervisor => &[
                VoucherNew,
                VoucherEdit,
                VoucherDelete,
                VoucherAudit,
                VoucherUnaudit,
                VoucherPost,
                VoucherUnpost,
                AccountEdit,
                AuxEdit,
                Opening,
                CarryForward,
                PeriodClose,
                Report,
                FinReport,
                Export,
                AuditLog,
                OrderOps,
                Warehouse,
                PriceView,
                PriceEdit,
                CostOps,
                ProductionOps,
                // ⚠️ 这里原来有 `CashierSign`，后果是**财务主管这个岗位根本建不出来**。
                //
                // `validate_duty_separation` 的判据是「有效权限里同时出现 CashierSign 与
                // 任一会计核心权限就拒绝」，而主管这一组恰好两样都有 —— 于是每一次
                // `create_user(role="supervisor")` 都被拒：
                //   「会计与出纳权限不可出现在同一账号：出纳签字与会计核心权限（…）互斥」
                // 讽刺的是这条规则**本来就是为了阻止这件事**才写的。
                //
                // 为什么零测试发现：`backup_admin_only` 之类用例都是 `User::new()`
                // 直接造对象、**不走 `users::insert`**，所以根本没触发校验；而
                // 全仓**没有任何一个测试真的创建过主管账号**。
                //
                // 从职责上也不该给：财务主管是审核/监督岗，兼了出纳签字就等于
                // 「既审又签」，把出纳签字这道内控架空了 —— 而出纳签字正是这套系统
                // 里最有价值的控制之一。出纳签字只归出纳（`Role::Cashier`）。
            ],
            Role::Accountant => &[
                VoucherNew,
                VoucherEdit,
                VoucherDelete,
                VoucherPost,
                AccountEdit,
                AuxEdit,
                // PeriodClose：结账是会计的本职动作，不是监督动作。
                //
                // 为什么给：审核（VoucherAudit）之所以不给会计，是三权分离 ——
                // 制单/审核/记账要分开才有控制意义。但**结账没有这个控制价值**，
                // 它只是把本期封起来。原来只有 Supervisor 有 PeriodClose，于是
                // 「只有会计+出纳、没有财务主管」的小微企业**永远结不了账**：
                // 期末处理页对会计直接显示「没有「期末结账」权限」，连按钮都没有。
                // 实测确认过这条死路。
                PeriodClose,
                Opening,
                CarryForward,
                Report,
                FinReport,
                OrderOps,
                Warehouse,
                PriceView,
                PriceEdit,
                CostOps,
                ProductionOps,
            ],
            Role::Cashier => &[VoucherNew, VoucherEdit, CashierSign, Report, FinReport],
            Role::Auditor => &[VoucherAudit, VoucherUnaudit, Report, FinReport],
            Role::OrderClerk => &[OrderOps, AuxEdit, Report, PriceView, PriceEdit],
            Role::Keeper => &[Warehouse, Report],
            Role::Receivables => &[VoucherNew, AuxEdit, Report, FinReport, Export],
            Role::Payables => &[
                OrderOps, VoucherNew, AuxEdit, Report, FinReport, PriceView, PriceEdit, Export,
            ],
            Role::CostAccountant => &[CostOps, VoucherNew, Report, FinReport],
            Role::Production => &[ProductionOps, Report],
            Role::Viewer => &[Report, FinReport],
        }
    }
}

/// 数据权限范围
///
/// 角色决定了"能做什么操作"，数据范围决定"能看到哪些数据"。
/// 两者分开，是因为同一个会计在不同公司可能要限制到不同部门。
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DataScope {
    /// 允许查看的部门编码；为空表示不限制
    pub depts: Vec<String>,
    /// 科目范围（含端点），为空表示不限
    pub account_from: String,
    pub account_to: String,
    /// 只能查看自己填制的凭证
    pub own_voucher_only: bool,
    /// 只能查看自己经手的业务单据（报销 / 工资）
    pub own_doc_only: bool,
}

impl DataScope {
    /// 无任何限制
    pub fn unrestricted() -> Self {
        Self::default()
    }
    pub fn is_unrestricted(&self) -> bool {
        self.depts.is_empty()
            && self.account_from.is_empty()
            && self.account_to.is_empty()
            && !self.own_voucher_only
            && !self.own_doc_only
    }
    /// 部门是否在可见范围内
    pub fn allows_dept(&self, dept: &str) -> bool {
        if self.depts.is_empty() || dept.is_empty() {
            return true;
        }
        self.depts.iter().any(|d| d == dept)
    }
    /// 科目是否在可见范围内（按编码前缀比较，父级自动含下级）
    pub fn allows_account(&self, code: &str) -> bool {
        let lo = self.account_from.trim();
        let hi = self.account_to.trim();
        if lo.is_empty() && hi.is_empty() {
            return true;
        }
        if !lo.is_empty() && code < lo {
            return false;
        }
        if !hi.is_empty() {
            // 上界按"该编码及其所有下级"理解：1002 应包含 100201
            return code <= hi || code.starts_with(hi);
        }
        true
    }
}

/// 口令策略
///
/// 默认给一套"够用但不折腾"的规则：8 位 + 字母 + 数字。
/// 财务软件加太复杂的策略，结果往往是有人把密码写在显示器边上。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PasswordPolicy {
    pub min_len: usize,
    pub need_letter: bool,
    pub need_digit: bool,
    pub need_symbol: bool,
    /// 口令有效期（天），0 表示永不过期
    pub max_age_days: i64,
    /// 连续失败多少次锁定
    pub max_fail: i64,
    /// 锁定时长（分钟）
    pub lock_minutes: i64,
    /// 空闲多久自动登出（分钟），0 表示不自动登出
    pub idle_minutes: i64,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_len: 8,
            need_letter: true,
            need_digit: true,
            need_symbol: false,
            max_age_days: 90,
            max_fail: 5,
            lock_minutes: 15,
            idle_minutes: 30,
        }
    }
}

impl PasswordPolicy {
    /// 校验口令，返回第一个不满足的规则
    pub fn check(&self, pwd: &str) -> Result<(), String> {
        // 按**字符**数而不是字节数：4 个汉字是 12 字节但只有 4 个字符，
        // 用 len() 会让「口令长度不能少于 8 位」被 4 个汉字骗过。
        if pwd.chars().count() < self.min_len {
            return Err(format!("口令长度不能少于 {} 位", self.min_len));
        }
        if self.need_letter && !pwd.chars().any(|c| c.is_ascii_alphabetic()) {
            return Err("口令必须包含字母".to_string());
        }
        if self.need_digit && !pwd.chars().any(|c| c.is_ascii_digit()) {
            return Err("口令必须包含数字".to_string());
        }
        if self.need_symbol && !pwd.chars().any(|c| !c.is_alphanumeric()) {
            return Err("口令必须包含特殊字符".to_string());
        }
        Ok(())
    }
    /// 口令强度 0~4，用于界面上的进度条
    pub fn strength(&self, pwd: &str) -> u8 {
        let n = pwd.chars().count();
        let mut s = 0u8;
        if n >= self.min_len {
            s += 1;
        }
        if n >= 12 {
            s += 1;
        }
        if pwd.chars().any(|c| c.is_ascii_alphabetic()) && pwd.chars().any(|c| c.is_ascii_digit()) {
            s += 1;
        }
        if pwd.chars().any(|c| !c.is_alphanumeric()) {
            s += 1;
        }
        s.min(4)
    }
    /// 距上次改口令是否已超过有效期
    pub fn is_expired(&self, changed_at: &str, now: chrono::NaiveDate) -> bool {
        if self.max_age_days <= 0 || changed_at.is_empty() {
            return false;
        }
        let d = changed_at.get(..10).unwrap_or("");
        match chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
            Ok(c) => (now - c).num_days() > self.max_age_days,
            Err(_) => false,
        }
    }
}

/// 用户
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    /// 加盐后的口令摘要，格式 `salt$hash`
    pub password_hash: String,
    pub role: Role,
    /// 兼任岗位（身兼多职；有效权限 = 主岗位 ∪ 兼任 ∪ extra − deny）
    pub roles: Vec<Role>,
    pub disabled: bool,
    /// 额外权限（在角色基础上追加）
    pub extra_perms: Vec<Perm>,
    /// 禁止的权限（在角色基础上例外关闭，逐项覆盖角色预设）
    pub deny_perms: Vec<Perm>,
    pub memo: String,
    /// 上次修改口令时间
    pub pwd_changed_at: String,
    /// 下次登录必须改口令（管理员重置后常用）
    pub must_change_pwd: bool,
    /// 锁定截止时刻（`%Y-%m-%d %H:%M:%S`），None 表示未锁定
    pub locked_until: Option<String>,
    /// 上次登录时间
    pub last_login_at: String,
    /// 绑定的设备指纹（空 = 未绑定，首次登录自动绑定当前设备）
    pub device_id: String,
    /// 绑定设备的展示名（方便管理员辨认是哪台机器）
    pub device_name: String,
    /// 数据权限范围
    pub data_scope: DataScope,
}

impl User {
    pub fn new(username: &str, display_name: &str, role: Role) -> Self {
        // 多岗位协作（订单/仓管/会计/出纳同账套）默认放开：只看本人会让各会计
        // 互相看不见对方分录、余额与试算口径碎裂。需要收紧时由管理员在「用户
        // 编辑 → 数据范围」按账号勾选 own_voucher_only（过滤机制保留）。
        let own_voucher_only = false;
        Self {
            id: 0,
            username: username.to_string(),
            display_name: display_name.to_string(),
            password_hash: String::new(),
            role,
            disabled: false,
            extra_perms: Vec::new(),
            deny_perms: Vec::new(),
            memo: String::new(),
            pwd_changed_at: String::new(),
            must_change_pwd: false,
            locked_until: None,
            last_login_at: String::new(),
            device_id: String::new(),
            device_name: String::new(),
            roles: Vec::new(),
            data_scope: DataScope {
                own_voucher_only,
                ..DataScope::default()
            },
        }
    }

    /// 是否系统管理员（设备绑定、导出限制对管理员不生效）——主岗位或兼任任一为 Admin 即是
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin || self.roles.contains(&Role::Admin)
    }

    /// 主岗位 + 兼任岗位（去重，主岗位在前）
    pub fn all_roles(&self) -> Vec<Role> {
        let mut out = vec![self.role];
        for r in &self.roles {
            if !out.contains(r) {
                out.push(*r);
            }
        }
        out
    }

    /// 角色显示名（身兼多职用 / 连接，如「订单专员/仓管员」）
    pub fn role_labels(&self) -> String {
        self.all_roles()
            .iter()
            .map(|r| r.label())
            .collect::<Vec<_>>()
            .join("/")
    }

    pub fn can(&self, p: Perm) -> bool {
        if self.disabled {
            return false;
        }
        // 系统管理员始终拥有全部权限，不接受逐项关闭（避免把自己锁在门外）
        if self.is_admin() {
            return true;
        }
        // 逐项覆盖：主岗位 + 兼任岗位（并集）+ 额外授权 − 明确关闭
        if self.deny_perms.contains(&p) {
            return false;
        }
        self.all_roles().iter().any(|r| r.perms().contains(&p)) || self.extra_perms.contains(&p)
    }

    /// 不相容职务分离（会计 × 出纳）：**出纳签字**不得与**会计角色核心权限**同现于同一账号。
    /// 会计核心 = 会计预设中出纳没有的部分（按角色预设动态求差，随预设同步演进）；
    /// 系统管理员天然持有全部权限，豁免。建号（insert）与每次权限变更（update）都会调用，
    /// 两端（Web / 桌面）与数据范围保存共用这一拦截点。
    pub fn validate_duty_separation(u: &User) -> Result<(), String> {
        if u.is_admin() {
            return Ok(());
        }
        // 有效权限 = 主岗位 ∪ 兼任岗位 ∪ 额外授权 − 明确关闭（多岗位并集口径，与 User::can 一致）
        let roles = u.all_roles();
        let eff = |p: Perm| {
            !u.deny_perms.contains(&p)
                && (roles.iter().any(|r| r.perms().contains(&p)) || u.extra_perms.contains(&p))
        };
        if !eff(Perm::CashierSign) {
            return Ok(());
        }
        let accountant_core: Vec<Perm> = Role::Accountant
            .perms()
            .iter()
            .copied()
            .filter(|p| !Role::Cashier.perms().contains(p))
            // 价格查看/修改是业务字段权限，不属于会计记账职责，不参与互斥
            .filter(|p| !matches!(p, Perm::PriceView | Perm::PriceEdit))
            .collect();
        if accountant_core.iter().any(|&p| eff(p)) {
            return Err(
                "会计与出纳权限不可出现在同一账号：出纳签字与会计核心权限（记账/删证/科目/档案/建账/结转/订单/仓储）互斥"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn set_password(&mut self, plain: &str) {
        self.password_hash = hash_password(plain);
        self.pwd_changed_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        self.must_change_pwd = false;
    }
    pub fn verify_password(&self, plain: &str) -> bool {
        verify_password(plain, &self.password_hash)
    }
    /// 禁用或处于锁定期都算"进不来"
    pub fn is_locked_out(&self) -> bool {
        if self.disabled {
            return true;
        }
        match &self.locked_until {
            Some(s) if s.is_empty() => false,
            Some(s) => parse_ts(s)
                .map(|t| t > chrono::Local::now().naive_local())
                .unwrap_or(false),
            None => false,
        }
    }
    /// 剩余锁定分钟数（未锁定返回 0）
    pub fn lock_remaining_min(&self) -> i64 {
        match &self.locked_until {
            Some(s) if !s.is_empty() => match parse_ts(s) {
                Some(t) => {
                    let left = (t - chrono::Local::now().naive_local()).num_minutes();
                    left.max(0)
                }
                None => 0,
            },
            _ => 0,
        }
    }
    /// 是否还能查看某科目的数据
    pub fn can_see_account(&self, code: &str) -> bool {
        self.data_scope.allows_account(code)
    }
    pub fn can_see_dept(&self, dept: &str) -> bool {
        self.data_scope.allows_dept(dept)
    }
    /// 凭证是否在数据范围内可见：
    /// - 科目范围：凭证涉及的全部科目都在范围内才可见（空范围 = 不限制）
    /// - 部门范围：凭证涉及的全部**带部门标签**的分录都在范围内才可见
    ///   （没打部门标签的分录不算越界 —— 与报销/工资的口径一致：`allows_dept("")`
    ///   为 `true`。否则一张没选部门的费用凭证会让所有部门会计都看不见它，
    ///   而实际情况是本厂大部分凭证根本不填部门）
    /// - 本人凭证：`own_voucher_only` 时仅制单人本人可见
    pub fn can_see_voucher(&self, v: &crate::voucher::Voucher) -> bool {
        let scope = &self.data_scope;
        if scope.own_voucher_only && v.prepared_by != self.username {
            return false;
        }
        let has_account_limit =
            !scope.account_from.trim().is_empty() || !scope.account_to.trim().is_empty();
        let has_dept_limit = !scope.depts.is_empty();
        if !has_account_limit && !has_dept_limit {
            return true;
        }
        // 「全部科目都在范围内」= 逐条 `all`：一张凭证若有一条分录落在范围外
        // （例如范围 1122..1122，凭证含 1001 + 1122），整张凭证都不该可见。
        // 早先这里是 `any`，与上面注释的规格相反——范围外的科目金额会随整张
        // 凭证一起泄露（实测确认）。空凭证视为不可见更安全，但没有分录的凭证
        // 本不该存在，这里按"无可见证据"处理。
        v.entries.iter().all(|e| {
            (!has_account_limit || scope.allows_account(&e.account_code))
                && (!has_dept_limit
                    || scope.allows_dept(e.aux.dept.as_deref().unwrap_or("").trim()))
        })
    }
}

/// 解析 `%Y-%m-%d %H:%M:%S` 时间戳
fn parse_ts(s: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(s.get(..10).unwrap_or(""), "%Y-%m-%d")
                .ok()
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap())
        })
}

/// 生成 argon2id PHC 格式口令哈希（前缀 `$argon2id$`）。
///
/// 旧版 `salt$sha256(...)` 格式仍可被 [`verify_password`] 验证（兼容旧账套），
/// 登录成功后会由持久化层透明升级为 argon2（见 findb::security::login）。
///
/// # Panics
/// 若 argon2 哈希失败（极端情况下），直接 panic 而非降级为弱哈希，
/// 防止新账户意外落库为 sha256 格式。
pub fn hash_password(plain: &str) -> String {
    use argon2::password_hash::{rand_core::OsRng, SaltString};
    use argon2::{Argon2, PasswordHasher};
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map(|h| h.to_string())
        .expect("argon2 哈希失败：系统随机源异常，请检查 OsRng 是否正常")
}

/// 校验口令：兼容 argon2 PHC 与旧版 `salt$sha256` 两种存储格式
pub fn verify_password(plain: &str, stored: &str) -> bool {
    if stored.starts_with("$argon2") {
        use argon2::password_hash::PasswordHash;
        use argon2::{Argon2, PasswordVerifier};
        match PasswordHash::new(stored) {
            Ok(parsed) => Argon2::default()
                .verify_password(plain.as_bytes(), &parsed)
                .is_ok(),
            Err(_) => false,
        }
    } else {
        match stored.split_once('$') {
            Some((salt, hash)) => {
                let calc = sha256_hex(&format!("{salt}{plain}"));
                constant_time_eq(calc.as_bytes(), hash.as_bytes())
            }
            None => false,
        }
    }
}

/// 该哈希是否为旧版 `salt$sha256` 格式（登录成功后应透明升级为 argon2）
pub fn is_legacy_hash(stored: &str) -> bool {
    !stored.starts_with("$argon2")
}

/// 时序缓解：执行一次与真实口令校验等价的 argon2 哈希。
///
/// 登录接口在"用户不存在"或"口令错误"路径上用它做等价空校验，
/// 避免两种路径耗时差异过大造成用户名枚举。返回恒为 false，仅消耗时间。
pub fn burn_argon2(plain: &str) -> bool {
    use argon2::password_hash::{rand_core::OsRng, SaltString};
    use argon2::{Argon2, PasswordHasher};
    let salt = SaltString::generate(&mut OsRng);
    let _ = Argon2::default().hash_password(plain.as_bytes(), &salt);
    false
}

/// 旧版 `salt$sha256(password)`。仅测试用：验证旧哈希的升级迁移路径。
#[cfg(test)]
fn legacy_hash_password(plain: &str) -> String {
    let salt = random_hex(16);
    let hash = sha256_hex(&format!("{salt}{plain}"));
    format!("{salt}${hash}")
}

/// 定长比较，避免时序侧信道
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut r = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        r |= x ^ y;
    }
    r == 0
}

/// 简易随机十六进制（仅测试用：生成旧版哈希的盐）
#[cfg(test)]
fn random_hex(n: usize) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut out = String::new();
    let mut x = seed;
    for _ in 0..n {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let b = ((x >> 33) & 0xf) as u8;
        out.push(char::from_digit(b as u32, 16).unwrap_or('0'));
    }
    out
}

/// SHA-256，十六进制输出
fn sha256_hex(input: &str) -> String {
    // 依赖 sha2 crate
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let out = h.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// 操作日志
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AuditLog {
    pub id: i64,
    pub ts: String,
    pub user: String,
    pub action: String,
    pub detail: String,
    /// 影响的模块
    pub module: String,
}

impl AuditLog {
    pub fn new(user: &str, module: &str, action: &str, detail: &str) -> Self {
        Self {
            id: 0,
            ts: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            user: user.to_string(),
            action: action.to_string(),
            detail: detail.to_string(),
            module: module.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 应收/应付会计必须能导出 —— 他们的日常活就是「把账龄表发给客户」
    /// 「把应付清单发给老板」。以前这两个岗位没有 `Export`，点导出直接 403，
    /// 于是真正的操作变成截图，而截图既不进日志也不可对账。
    ///
    /// 同时钉住反向：会计、出纳、仓管、只读**不能**导出，别为了省事把 Export
    /// 加进 `Role::Accountant` —— 那等于给所有人开了数据外带通道。
    #[test]
    fn ar_ap_clerks_can_export_but_others_cannot() {
        for r in [Role::Receivables, Role::Payables] {
            assert!(
                r.perms().contains(&Perm::Export),
                "{} 应当能导出（否则账龄表只能截图发给客户）",
                r.label()
            );
        }
        for r in [
            Role::Accountant,
            Role::Cashier,
            Role::Auditor,
            Role::OrderClerk,
            Role::Keeper,
            Role::CostAccountant,
            Role::Production,
            Role::Viewer,
        ] {
            assert!(
                !r.perms().contains(&Perm::Export),
                "{} 不应拥有导出权限（数据外带通道仅限管理员、财务主管、应收/应付会计）",
                r.label()
            );
        }
        // 管理员与财务主管一直是有的，别被上面的循环改动带走
        assert!(Role::Admin.perms().contains(&Perm::Export));
        assert!(Role::Supervisor.perms().contains(&Perm::Export));
    }

    #[test]
    fn password_roundtrip() {
        let h = hash_password("Abc123!!");
        assert!(verify_password("Abc123!!", &h));
        assert!(!verify_password("abc123!!", &h));
        assert!(!verify_password("x", "garbage"));
        // 同样的明文两次哈希结果应不同（盐不同）
        assert_ne!(hash_password("same"), hash_password("same"));
    }

    #[test]
    fn argon2_format_and_legacy_compat() {
        // 新哈希使用 argon2id PHC 格式
        let h = hash_password("Abc123!!");
        assert!(h.starts_with("$argon2"), "新哈希应为 argon2 PHC：{h}");
        assert!(!is_legacy_hash(&h));
        assert!(verify_password("Abc123!!", &h));
        // 旧版 salt$sha256 格式仍可验证，且被识别为 legacy
        let old = legacy_hash_password("OldPwd!1");
        assert!(is_legacy_hash(&old));
        assert!(verify_password("OldPwd!1", &old));
        assert!(!verify_password("Wrong!1", &old));
    }

    #[test]
    fn role_perms() {
        let mut u = User::new("zs", "张三", Role::Accountant);
        assert!(u.can(Perm::VoucherNew));
        assert!(!u.can(Perm::VoucherAudit));
        // 结账：会计必须有（原来这里断言"没有"，把 bug 当规范钉住了）。
        // 见 two_person_company_can_close_the_month 的说明。
        assert!(u.can(Perm::PeriodClose));
        // 额外授权机制本身仍然有效 —— 拿一个会计确实没有的权限验
        u.extra_perms.push(Perm::Backup);
        assert!(u.can(Perm::Backup), "逐项开启应生效");
        u.disabled = true;
        assert!(!u.can(Perm::VoucherNew));
    }

    #[test]
    fn admin_has_all() {
        let u = User::new("root", "管理员", Role::Admin);
        for p in Perm::all() {
            assert!(u.can(*p), "管理员缺少权限 {}", p.label());
        }
    }

    #[test]
    fn backup_admin_only() {
        // 备份是数据外带通道，仅系统管理员可用；财务主管也不拥有
        let admin = User::new("admin", "管理员", Role::Admin);
        assert!(admin.can(Perm::Backup));

        let sup = User::new("sup", "财务主管", Role::Supervisor);
        assert!(!sup.can(Perm::Backup), "财务主管不应有备份权限");

        let acc = User::new("acc", "会计", Role::Accountant);
        assert!(!acc.can(Perm::Backup), "会计不应有备份权限");

        let viewer = User::new("v", "只读", Role::Viewer);
        assert!(!viewer.can(Perm::Backup));
    }

    #[test]
    fn normal_user_default_own_voucher_only() {
        // 多岗位协作：默认放开（可见全账套凭证与一致的余额口径）；
        // 过滤机制保留，管理员可按账号在数据范围里勾选收紧。
        let mut acc = User::new("acc1", "会计一", Role::Accountant);
        assert!(!acc.data_scope.own_voucher_only, "非管理员默认应放开");
        acc.data_scope.own_voucher_only = true; // 按账号收紧仍然生效
        assert!(acc.data_scope.own_voucher_only);
        assert!(!acc.data_scope.is_unrestricted());

        let admin = User::new("admin", "管理员", Role::Admin);
        assert!(!admin.data_scope.own_voucher_only);
        assert!(admin.data_scope.is_unrestricted());
    }

    /// 科目数据范围是「全部科目都在范围内」语义（AND），不是「任一命中」（OR）。
    /// 回归：范围 1122..1122 时，含 1001（越权）+ 1122（在范围）的凭证早先
    /// 用 `.any()` 判定为可见，把范围外科目的金额一起泄露了。
    #[test]
    fn can_see_voucher_requires_all_entries_in_scope() {
        use crate::money::Money;
        use crate::period::Period;
        use crate::voucher::{Entry, Voucher};
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        let mut u = User::new("acc1", "会计一", Role::Accountant);
        u.data_scope.account_from = "1122".into();
        u.data_scope.account_to = "1122".into();

        let mk = |codes: &[&str]| -> Voucher {
            let mut v = Voucher::new(Period::new(2026, 1).unwrap(), d, "记", 1);
            v.prepared_by = "acc1".into();
            for (i, c) in codes.iter().enumerate() {
                let mut e = Entry::new(i as i32 + 1, *c, "s");
                if i % 2 == 0 {
                    e.debit = Money::parse("100").unwrap();
                } else {
                    e.credit = Money::parse("100").unwrap();
                }
                v.entries.push(e);
            }
            v.renumber();
            v
        };

        assert!(
            u.can_see_voucher(&mk(&["1122", "112201"])),
            "全部在范围内应可见"
        );
        // 1001 在 1122..1122 之外 → 整张不可见
        assert!(
            !u.can_see_voucher(&mk(&["1122", "1001"])),
            "只要有一条分录越界，整张凭证都不可见"
        );
        assert!(
            !u.can_see_voucher(&mk(&["1122", "6001"])),
            "对方科目越界同样不可见"
        );
        // 空范围不限制
        let un = User::new("acc2", "会计二", Role::Accountant);
        assert!(un.can_see_voucher(&mk(&["1122", "1001"])));
    }

    /// 部门范围只判定**带部门标签**的分录，没打标签的一律放行。
    ///
    /// 「空部门 = 放行」不是放水：本厂大部分凭证根本不填部门，若按「全部部门
    /// 都在范围内」硬判，部门会计会丢掉所有没选部门的费用凭证。
    #[test]
    fn can_see_voucher_dept_scope_only_judges_tagged_entries() {
        use crate::money::Money;
        use crate::period::Period;
        use crate::voucher::{AuxRef, Entry, Voucher};
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        let mut u = User::new("acc1", "会计一", Role::Accountant);
        u.data_scope.depts = vec!["D01".into()];

        let mk = |rows: &[(&str, Option<&str>)]| -> Voucher {
            let mut v = Voucher::new(Period::new(2026, 1).unwrap(), d, "记", 1);
            v.prepared_by = "acc1".into();
            for (i, (c, dept)) in rows.iter().enumerate() {
                let mut e = Entry::new(i as i32 + 1, *c, "s");
                e.aux = AuxRef {
                    dept: dept.map(|s| s.to_string()),
                    ..Default::default()
                };
                if i % 2 == 0 {
                    e.debit = Money::parse("100").unwrap();
                } else {
                    e.credit = Money::parse("100").unwrap();
                }
                v.entries.push(e);
            }
            v.renumber();
            v
        };

        assert!(
            u.can_see_voucher(&mk(&[("660201", Some("D01")), ("1001", Some("D01"))])),
            "部门标签都在范围内应可见"
        );
        assert!(
            !u.can_see_voucher(&mk(&[("660201", Some("D02")), ("1001", Some("D02"))])),
            "部门标签越界应不可见"
        );
        assert!(
            u.can_see_voucher(&mk(&[("660201", None), ("1001", None)])),
            "没打部门标签的分录不算越界"
        );
        assert!(
            u.can_see_voucher(&mk(&[("660201", Some("D01")), ("1001", None)])),
            "混合：有标签的那条在范围内即可见"
        );
        assert!(
            !u.can_see_voucher(&mk(&[("660201", None), ("1001", Some("D02"))])),
            "混合：只要有一条带标签的分录越界，整张凭证都不可见"
        );
        assert!(
            u.can_see_voucher(&mk(&[("660201", Some("  ")), ("1001", None)])),
            "只有空白的部门等同没打标签"
        );
        // 部门与科目两个范围是 AND
        u.data_scope.account_from = "6602".into();
        u.data_scope.account_to = "6602".into();
        assert!(
            u.can_see_voucher(&mk(&[("660201", Some("D01")), ("660201", None)])),
            "科目与部门都在范围内才可见"
        );
        assert!(
            !u.can_see_voucher(&mk(&[("660201", Some("D01")), ("1001", None)])),
            "部门在范围内但科目越界，仍不可见"
        );
        // 部门范围为空 = 不限制
        let un = User::new("acc2", "会计二", Role::Accountant);
        assert!(un.can_see_voucher(&mk(&[("660201", Some("D99"))])));
    }

    /// 口令长度按**字符**计，不是字节。
    /// 回归：`pwd.len()` 是字节数，4 个汉字（12 字节）能通过「8 位 + 字母 + 数字」
    /// 之外的宽松策略；纯汉字口令在关掉 need_letter/need_digit 后就能过 8 位门槛。
    #[test]
    fn password_length_counts_chars_not_bytes() {
        let mut p = PasswordPolicy::default();
        // 默认策略：纯汉字被 need_letter 拦下
        assert!(p.check("密码密码").is_err());
        // 关掉字母/数字要求后，4 个汉字（12 字节 / 4 字符）必须仍然不达标
        p.need_letter = false;
        p.need_digit = false;
        let four_cn = "密码密码"; // 4 字
        assert_eq!(four_cn.len(), 12, "4 个汉字应为 12 字节");
        assert_eq!(four_cn.chars().count(), 4);
        assert!(p.check(four_cn).is_err(), "4 个字符不满足 8 位要求");
        // 8 个汉字应当达标
        assert!(p.check("密码密码密码密码").is_ok());
        // 强度条同样按字符计
        assert!(p.strength(four_cn) < p.strength("Abcdefgh1234"));
    }

    /// 科目范围按定长编码的字典序比较——这是**有意**的语义，不是 bug。
    /// 上界 `hi` 意为「hi 及其所有下级 + 字典序在 hi 之前的兄弟子树」，
    /// 下界 `lo` 意为「lo 及其所有下级」。锁住这个语义，避免以后被"修"成别的。
    #[test]
    fn account_scope_range_semantics() {
        use crate::account::CodeScheme;
        let mut sc = DataScope::default();
        // 默认方案 4-2-2-2-2 下合法编码长度只有 4/6/8/10/12
        let scheme = CodeScheme::default();
        for c in ["1001", "100201", "10020101"] {
            assert!(scheme.is_valid_code(c), "{c} 应为合法编码");
        }
        assert!(!scheme.is_valid_code("10015"), "5 位不是合法编码");

        sc.account_from = "1002".into();
        sc.account_to = "1002".into();
        assert!(sc.allows_account("1002"));
        assert!(sc.allows_account("100201"), "上界应含其下级");
        assert!(sc.allows_account("10020101"), "上界含其整棵子树");
        assert!(!sc.allows_account("1001"), "下界之外不可见");
        assert!(!sc.allows_account("1003"));
        // 注：`allows_account` 是纯字符串前缀比较，非法编码（如 5 位的 "10021"）
        // 会被 starts_with 判成 1002 的下级。这不是漏洞——`CodeScheme::is_valid_code`
        // 在建科目时就拒掉非级长边界编码，这类值不可能进科目表。

        // 只设上界 = 从最早到该子树
        sc.account_from = String::new();
        sc.account_to = "1122".into();
        assert!(sc.allows_account("1001"), "字典序在前的兄弟子树应可见");
        assert!(sc.allows_account("1122"));
        assert!(sc.allows_account("112201"));
        assert!(!sc.allows_account("1123"), "上界之后不可见");

        // 两端都空 = 不限制
        assert!(DataScope::default().is_unrestricted());
        assert!(DataScope::default().allows_account("9999"));
    }

    #[test]
    fn per_perm_override_deny_and_extra() {
        // 会计角色本身拥有 VoucherNew / Report，但没有 Backup
        let mut acc = User::new("acc1", "会计一", Role::Accountant);
        assert!(acc.can(Perm::VoucherNew));
        assert!(acc.can(Perm::Report));
        assert!(!acc.can(Perm::Backup));

        // 逐项关闭：角色里有的也能关掉
        acc.deny_perms.push(Perm::VoucherNew);
        assert!(!acc.can(Perm::VoucherNew), "逐项关闭后角色权限失效");
        assert!(acc.can(Perm::Report), "未关闭的权限不受影响");

        // 逐项开启：角色里没有的也能加上
        acc.extra_perms.push(Perm::Backup);
        assert!(acc.can(Perm::Backup), "逐项开启后获得额外权限");

        // 管理员始终全权，不受 deny 影响（防止把自己锁在门外）
        let mut admin = User::new("root", "管理员", Role::Admin);
        admin.deny_perms.push(Perm::UserManage);
        assert!(admin.can(Perm::UserManage), "管理员不受逐项关闭限制");
        admin.deny_perms.push(Perm::Backup);
        assert!(admin.can(Perm::Backup));
    }

    /// 「只有会计 + 出纳、没有财务主管」的小微企业必须能把一个月走完。
    ///
    /// 这条是真实踩出来的：原来 `PeriodClose` 只挂在 `Role::Supervisor` 上，于是
    /// 期末处理页对会计连按钮都不渲染（只显示「没有「期末结账」权限」）。两人公司
    /// 里根本没有财务主管，也就**永远结不了账** —— 账做到 12 月就断了。
    ///
    /// 之所以该给会计：审核（`VoucherAudit`）不给会计是三权分离，制单/审核/记账
    /// 分开才有控制意义；**结账没有这个控制价值**，它只是把本期封起来。所以本测试
    /// 同时钉住两头：结账要给，审核仍然不给（免得哪天为了"跑通流程"把三权分离也
    /// 一起废掉）。
    /// 每个角色都**建得出来** —— 而且是真走校验那条路。
    ///
    /// 这条是被一个真 bug 逼出来的：`Role::Supervisor` 的权限组里原本含
    /// `CashierSign`，于是它同时命中「出纳签字」和「会计核心权限」，
    /// `validate_duty_separation` 必然拒绝 —— **财务主管这个岗位根本建不出来**。
    ///
    /// 之所以零测试发现：既有用例都是 `User::new()` 直接造对象、不走
    /// `validate_duty_separation`，而全仓没有任何一个测试真的创建过主管账号。
    /// 只测 `Role::perms()` 的内容是看不出"这个角色根本用不了"的。
    #[test]
    fn every_role_passes_duty_separation() {
        // 关联函数不能 use 出来（`use Type::assoc_fn` 不合法），只能写全名
        fn check(u: &User) -> Result<(), String> {
            User::validate_duty_separation(u)
        }
        for (role, name) in [
            (Role::Admin, "系统管理员"),
            (Role::Supervisor, "财务主管"),
            (Role::Accountant, "会计"),
            (Role::Cashier, "出纳"),
            (Role::Viewer, "只读"),
        ] {
            let u = User::new("probe", name, role);
            assert!(
                check(&u).is_ok(),
                "{} 角色建不出来：{:?} —— 它的权限组同时命中了出纳签字与会计核心权限，\
                 users::insert 会直接拒绝，于是这个岗位形同虚设",
                name,
                check(&u)
            );
        }
        // 出纳签字只归出纳：主管兼了就把这道内控架空了
        let sup = User::new("sup", "财务主管", Role::Supervisor);
        assert!(!sup.can(Perm::CashierSign), "财务主管不该兼出纳签字");
        let cash = User::new("cash", "出纳", Role::Cashier);
        assert!(cash.can(Perm::CashierSign), "出纳必须有签字权");
    }

    #[test]
    fn two_person_company_can_close_the_month() {
        let acc = User::new("acc", "会计", Role::Accountant);
        assert!(
            acc.can(Perm::PeriodClose),
            "会计必须能期末结账，否则两人公司断在年末"
        );

        let cashier = User::new("cash", "出纳", Role::Cashier);
        // 出纳连凭证都不该碰，结账更不必给
        assert!(!cashier.can(Perm::PeriodClose), "结账不应下沉到出纳");

        // 三权分离仍然成立：会计自己录的凭证自己不能审
        assert!(
            !acc.can(Perm::VoucherAudit),
            "会计不得自带审核权（制单/审核须分离）"
        );
        // 但记账权必须在，否则开了审核环节就两头落空
        assert!(acc.can(Perm::VoucherPost), "会计必须有记账权");
    }
}
