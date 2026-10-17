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

/// 业务校验结果：收集多条问题一次性反馈给用户，而不是弹第一个错就停
#[derive(Debug, Clone, Default)]
pub struct Issues(Vec<String>);

impl Issues {
    pub fn new() -> Self {
        Self(Vec::new())
    }
    pub fn push<S: Into<String>>(&mut self, s: S) {
        self.0.push(s.into());
    }
    pub fn check<C: Into<bool>>(&mut self, cond: C, msg: impl Into<String>) {
        if !cond.into() {
            self.0.push(msg.into());
        }
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.0.iter()
    }
    /// 有问题则返回 Err（多条合并为一条消息）
    pub fn into_result(self) -> Result<(), FinError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(FinError::Validate(self.0.join("；")))
        }
    }
    /// 以分号连接
    pub fn join(&self) -> String {
        self.0.join("\n")
    }
}
