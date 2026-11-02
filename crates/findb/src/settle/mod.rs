//! 往来核销与账龄分析
//!
//! 核销是"单据级"的：一张应收单可以分多次收款核销，也可以一笔款核销多张单。
//! 所以这里存的是**分录 ↔ 分录 + 金额**，而不是"客户整体结清"。

use chrono::NaiveDate;
use fincore::engine::aging::{AgingBucket, AgingItem, AgingLine};
use fincore::{AuxRef, Direction, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::{Connection, OptionalExtension};

use crate::{Db, DbResult};

mod aging;
mod auto;
mod core;
mod cross;
mod dunning;
mod open;
mod opening;

pub use aging::*;
pub use auto::*;
pub use core::*;
pub use cross::*;
pub use dunning::*;
pub use open::*;
pub use opening::*;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
