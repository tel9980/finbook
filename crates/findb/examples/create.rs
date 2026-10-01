//! 创建一个空的 FinBook 账套
//!
//! 用法: cargo run --example create --release -- <path>

use findb::Db;
use fincore::{BookOptions, Period};
use std::env;
use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(env::args().nth(1).expect("usage: create <path>"));
    let _ = std::fs::remove_file(&path);
    let opts = BookOptions {
        code_scheme: vec![4, 2, 2, 2],
        start_period: Period::parse("2025-01").unwrap(),
        base_currency: "CNY".into(),
        company: "示例科技有限公司".into(),
        tax_no: "91110000123456789X".into(),
        enable_qty: false,
        enable_foreign: false,
        enable_audit: false,
        require_cashier: false,
        biz_accounts: Default::default(),
        budget_control: String::new(),
        doc_prefixes: Default::default(),
        require_audit: false, // 审核环节已移除（未记账 → 记账两态）
        voucher_words: fincore::chart::default_voucher_words(),
    };
    // 不内置固定管理员：首次登录输入的账号与密码即为系统管理员
    let db = Db::create_no_admin(&path, &opts).expect("create");
    let _ = db.log("-", "系统", "建账", "由 CLI 创建");
    drop(db);
    println!(
        "已创建 {}（无内置账号：首次登录输入的账号将成为系统管理员）",
        path.display()
    );
}
