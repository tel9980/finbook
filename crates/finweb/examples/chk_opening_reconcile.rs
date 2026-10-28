//! 期初对账视图的实测脚本（模拟数据）。
//!
//! 跑法：`cargo run --release -p finweb --example chk_opening_reconcile 2>/dev/null`
//!
//! 做的是工厂真实走的那条路：
//!   1. 建一个空账套（内置科目表）
//!   2. 建 3 个客户、2 个供应商、1 个银行账户（像老板整理档案那样）
//!   3. 用 `sheets::opening_sheet` 生成「按当前账套预填」的期初收集表
//!   4. **在表里填金额**（模拟财务在 Excel 里填）
//!   5. 导入 → 打印对账视图
//!
//! 然后故意做两种常见错误，看对账视图能不能当场指出是哪张纸的问题。

use std::collections::BTreeMap;

use fincore::account::BookOptions;
use fincore::{AuxEntity, AuxKind};
use fincore::account::Account;
use fincore::money::Money;

fn m(s: &str) -> Money {
    Money::parse(s).unwrap()
}

fn seed_aux(db: &findb::Db) {
    let customers = [
        ("C001", "华达五金"),
        ("C002", "鑫盛电器"),
        ("C003", "永泰贸易"),
    ];
    for (code, name) in customers {
        let mut e = AuxEntity::new(AuxKind::Customer, code, name);
        e.memo = "月结 30 天".into();
        findb::auxs::insert(db, &e).unwrap();
    }
    for (code, name) in [("S001", "宏源氧化"), ("S002", "顺发化工")] {
        findb::auxs::insert(db, &AuxEntity::new(AuxKind::Supplier, code, name)).unwrap();
    }
    for (code, name) in [("B01", "G银行对公户"), ("B02", "微信钱包")] {
        findb::auxs::insert(db, &AuxEntity::new(AuxKind::Bank, code, name)).unwrap();
    }
}

/// 用 `sheets::opening_sheet` 生成模板（与网页端「下载期初收集表」同一条路径），
/// 然后按「Excel 里填数」的方式把金额塞进指定行。
fn build_filled_sheet(
    accounts: &[&Account],
    customers: &[AuxEntity],
    suppliers: &[AuxEntity],
    ar: &[(&str, &str, &str)],   // (客户编码, 单据号, 金额)
    ap: &[(&str, &str, &str)],   // (供应商编码, 单据号, 金额)
    bal: &[(&str, &str)],        // (科目编码, 金额)
    basic: &[(&str, &str)],      // (项目, 金额) 实收资本/本年利润
) -> Vec<u8> {
    let mut sheets = finweb::sheets::opening_collection_template(accounts, customers, suppliers);
    // 哪张纸对应哪类内容靠表名判断（模板里 ①②③④ 都在表名里）
    for sheet in sheets.iter_mut() {
    for row in sheet.rows.iter_mut() {
        let first = row.first().cloned().unwrap_or_default();
        // ① 客户应收：编码在 ar 里就填单据号与金额
        // 列序是 编码 / 名称 / 欠款金额 / 单据号 / 备注 —— 金额在第 3 列
        if let Some((_, doc, amt)) = ar.iter().find(|(c, _, _)| *c == first) {
            row[2] = amt.to_string();
            row[3] = doc.to_string();
        }
        // ② 供应商应付（sheet 内 ② 的行首也是编码，用 ap 列表区分）
        if let Some((_, doc, amt)) = ap.iter().find(|(c, _, _)| *c == first) {
            row[2] = amt.to_string();
            row[3] = doc.to_string();
        }
        // ③ 账户余额：第 3 列是金额
        if let Some((_, amt)) = bal.iter().find(|(c, _)| *c == first) {
            row[2] = amt.to_string();
        }
        // ④ 基本资料：第 2 列是金额
        if let Some((_, amt)) = basic.iter().find(|(k, _)| first.starts_with(*k)) {
            row[1] = amt.to_string();
        }
    }

    }
    if std::env::var("DUMP_SHEET").is_ok() {
        for sh in &sheets {
            println!("--- 表：{}  headers={:?}", sh.name, sh.headers);
            for r in sh.rows.iter().take(4) {
                println!("    {:?}", r);
            }
        }
    }
    finweb::xlsxout::build(&sheets).unwrap()
}

fn show(title: &str, db: &findb::Db) {
    println!("\n═══ {title} ═══");
    let r = findb::opening_check::reconcile_begin(db).unwrap();
    println!("{}", findb::opening_check::one_liner(&r));
    println!("\n四张纸：");
    for s in &r.sheets {
        println!(
            "  {:<12} {:>3} 行  合计 {:>14}   {}",
            s.label,
            s.rows,
            s.total.fmt_plain(),
            s.hint
        );
    }
    println!(
        "\n借方 {}  |  贷方 {}  |  资产 {}  |  负债 {}  |  权益 {}",
        r.debit_total.fmt_plain(),
        r.credit_total.fmt_plain(),
        r.asset_total.fmt_plain(),
        r.liab_total.fmt_plain(),
        r.equity_total.fmt_plain()
    );
    println!("\n五项校验：");
    for c in &r.checks {
        println!(
            "  [{}] {:<26} {}",
            if c.ok { "OK" } else { "!!" },
            c.name,
            c.detail
        );
    }
    if !r.issues.is_empty() {
        println!("\n问题：");
        for i in &r.issues {
            println!("  - {i}");
        }
    }
}

fn main() {
    let dir = std::env::temp_dir().join("finbook_opening_check");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut opts = BookOptions::default();
    opts.company = "模拟氧化厂".into();
    opts.start_period = fincore::period::Period::parse("202611").unwrap();
    let db = findb::Db::create_no_admin(dir.join("mock.fbk"), &opts).unwrap();
    seed_aux(&db);

    let chart = findb::accounts::chart(&db).unwrap();
    let accounts: Vec<&Account> = chart.all();
    let customers = findb::auxs::list(
        &db,
        &fincore::AuxQuery {
            kind: Some(AuxKind::Customer),
            ..Default::default()
        },
    )
    .unwrap();
    let suppliers = findb::auxs::list(
        &db,
        &fincore::AuxQuery {
            kind: Some(AuxKind::Supplier),
            ..Default::default()
        },
    )
    .unwrap();

    // ── 场景 A：一份填得对的期初 ─────────────────────────────────
    let bytes = build_filled_sheet(
        &accounts,
        &customers,
        &suppliers,
        &[("C001", "XS-001", "50000"), ("C002", "XS-002", "45000")],
        &[("S001", "CG-001", "28000")],
        &[("1001", "20000")],
        &[("实收资本", "87000")],
    );
    // 注意：112201/220201 由 ①② 两张纸的合计生成，这里刻意**先不填**，
    // 让脚本只靠往来两张纸去算，才能看出对账视图是否会重复计。

    match findb::opening_book::import_collection_sheet(&db, &bytes, "模拟会计") {
        Ok(r) => {
            println!("导入结果：");
            for s in [&r.ar, &r.ap, &r.balances, &r.equity] {
                println!(
                    "  {:<10} ok={} 跳过={} 合计={}",
                    s.sheet,
                    s.ok,
                    s.skipped,
                    s.total.fmt_plain()
                );
            }
            println!(
                "  借 {} / 贷 {}  {}",
                r.debit_total.fmt_plain(),
                r.credit_total.fmt_plain(),
                if r.debit_total == r.credit_total {
                    "（平）"
                } else {
                    "（不平！）"
                }
            );
        }
        Err(e) => {
            println!("导入被拒：{e}");
            return;
        }
    }
    show("场景 A：正常导入后的对账视图", &db);

    // ── 场景 B：只导台账没导科目（模拟「两处都对、合起来不对」）──
    println!("\n\n──── 场景 B：把 112201 的期初删掉，模拟「只导台账没导科目」────");
    db.conn()
        .execute("DELETE FROM begin_balance WHERE account_code LIKE '1122%'", [])
        .unwrap();
    show("场景 B：台账有、科目没有", &db);

    // ── 场景 C：往来单位编码笔误 ────────────────────────────────
    println!("\n\n──── 场景 C：把一条台账的客户编码改成档案里没有的 ────");
    db.conn()
        .execute("UPDATE arap_opening SET party_code='C999' WHERE party_code='C002'", [])
        .unwrap();
    show("场景 C：编码笔误", &db);

    println!("\n库文件：{}", dir.join("mock.fbk").display());
    let _: BTreeMap<String, String> = BTreeMap::new();
}
