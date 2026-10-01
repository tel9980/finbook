//! 把账套里所有用户密码重置为 admin123
//!
//! 用法: cargo run --example reset_pwd --release -- <path>

use findb::Db;
use fincore::user::hash_password;
use std::env;
use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(env::args().nth(1).expect("usage: reset_pwd <path>"));
    let db = Db::open(&path).expect("open");
    let hash = hash_password("admin123");
    db.conn()
        .execute("UPDATE user SET password_hash=?1", rusqlite::params![hash])
        .expect("update");
    drop(db);
    println!("已将所有用户密码重置为 admin123");
}
