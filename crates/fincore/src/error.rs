//! 统一错误类型

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinError {
    /// 一般性消息（含业务规则拦截）
    Msg(String),
    /// 业务校验未通过（不阻断流程，仅提示）
    Validate(String),
    /// 找不到目标对象
    NotFound(String),
    /// 当前状态下不允许该操作
    InvalidState(String),
    /// 权限不足
    Denied(String),
    /// 持久化层错误
    Db(String),
    /// 输入输出错误
    Io(String),
}

impl FinError {
    pub fn msg<S: Into<String>>(s: S) -> Self {
        FinError::Msg(s.into())
    }
    pub fn validate<S: Into<String>>(s: S) -> Self {
        FinError::Validate(s.into())
    }
    pub fn not_found<S: Into<String>>(s: S) -> Self {
        FinError::NotFound(s.into())
    }
    pub fn state<S: Into<String>>(s: S) -> Self {
        FinError::InvalidState(s.into())
    }
    pub fn db<S: Into<String>>(s: S) -> Self {
        FinError::Db(s.into())
    }
    pub fn io<S: Into<String>>(s: S) -> Self {
        FinError::Io(s.into())
    }
}

impl std::fmt::Display for FinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinError::Msg(s) => f.write_str(s),
            FinError::Validate(s) => write!(f, "校验未通过：{s}"),
            FinError::NotFound(s) => write!(f, "未找到：{s}"),
            FinError::InvalidState(s) => write!(f, "当前状态不允许：{s}"),
            FinError::Denied(s) => write!(f, "没有权限：{s}"),
            FinError::Db(s) => write!(f, "数据库错误：{s}"),
            FinError::Io(s) => write!(f, "文件错误：{s}"),
        }
    }
}

impl std::error::Error for FinError {}

impl From<String> for FinError {
    fn from(s: String) -> Self {
        FinError::Msg(s)
    }
}
impl From<&str> for FinError {
    fn from(s: &str) -> Self {
        FinError::Msg(s.to_string())
    }
}

/// 业务校验结果：收集多条问题一次性反馈给用户，而不是弹第一个错就停。
///
/// 分**错误**与**提醒**两档：
/// - 错误（[`Issues::push`]）：违反会计约束，必须改，否则不能保存；
/// - 提醒（[`Issues::warn`]）：不符合惯例但不影响账目正确，**只提示不拦截**。
///   典型是"数量 × 单价 ≠ 金额"——抹零、取整、折扣都会让两者不等，
///   这是正常的商业事实，拦下来等于逼用户去改数据迎合公式。
#[derive(Debug, Clone, Default)]
pub struct Issues {
    errs: Vec<String>,
    warns: Vec<String>,
}

impl Issues {
    pub fn new() -> Self {
        Self::default()
    }
    /// 记一条错误（会导致保存失败）
    pub fn push<S: Into<String>>(&mut self, s: S) {
        self.errs.push(s.into());
    }
    /// 记一条提醒（不影响保存，只提示）
    pub fn warn<S: Into<String>>(&mut self, s: S) {
        self.warns.push(s.into());
    }
    pub fn check<C: Into<bool>>(&mut self, cond: C, msg: impl Into<String>) {
        if !cond.into() {
            self.errs.push(msg.into());
        }
    }
    /// 没有错误（提醒不算）
    pub fn is_empty(&self) -> bool {
        self.errs.is_empty()
    }
    /// 错误条数（不含提醒）
    pub fn len(&self) -> usize {
        self.errs.len()
    }
    /// 提醒条数
    pub fn warn_len(&self) -> usize {
        self.warns.len()
    }
    /// 遍历错误
    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.errs.iter()
    }
    /// 遍历提醒
    pub fn warnings(&self) -> std::slice::Iter<'_, String> {
        self.warns.iter()
    }
    /// 错误 + 提醒，按"先错误后提醒"的顺序合并（给用户看的完整列表）
    pub fn all(&self) -> Vec<&str> {
        self.errs
            .iter()
            .chain(self.warns.iter())
            .map(|s| s.as_str())
            .collect()
    }
    /// 有错误则返回 Err（多条合并为一条消息）；**只有提醒时返回 Ok**
    pub fn into_result(self) -> Result<(), FinError> {
        if self.errs.is_empty() {
            Ok(())
        } else {
            let mut msg = self.errs.join("；");
            if !self.warns.is_empty() {
                msg.push_str("；提醒：");
                msg.push_str(&self.warns.join("；"));
            }
            Err(FinError::Validate(msg))
        }
    }
    /// 以分号连接（错误 + 提醒）
    pub fn join(&self) -> String {
        self.all().join("\n")
    }
}
