//! # fincore —— FinBook 会计核心引擎
//!
//! 这一层不碰数据库、不碰界面，只做纯会计逻辑，因此可以脱离 GUI 做单元测试。
//!
//! 分层：
//! - [`money`]   金额（定点十进制，杜绝浮点误差）
//! - [`period`]  会计期间
//! - [`account`] 科目体系与科目表
//! - [`voucher`] 凭证模型
//! - [`balance`] 余额与账簿行
//! - [`engine`]  凭证校验、期末处理
//! - [`report`]  报表模板与取数引擎
//! - [`chart`]   内置科目表
//! - [`aux`]     辅助核算档案
//! - [`user`]    用户、角色、权限

pub mod account;
pub mod auxiliary;
pub mod balance;
pub mod chart;
pub mod engine;
pub mod error;
pub mod money;
pub mod period;
pub mod report;
pub mod user;
pub mod voucher;

pub use account::{
    Account, AcctCategory, AuxKind, AuxMask, BookOptions, Chart, CodeScheme, Direction,
};
pub use auxiliary::{AuxEntity, AuxQuery};
pub use balance::{
    dir_amount_to_signed, signed_to_dir_amount, BalanceRow, GeneralLedgerRow, JournalRow,
    LedgerRow, QtyRow, TrialBalance,
};
pub use error::{FinError, Issues};
pub use money::Money;
pub use period::Period;
pub use user::{Perm, Role, User};
pub use voucher::{AuxRef, Entry, Voucher, VoucherSource, VoucherStatus};

/// 数据库与界面层共用的版本号
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");
