//! 用模拟数据把期初闭环走一遍，专门盯「模板收不到的那几类期初」。
//!
//! 跑法：`cargo run --release -p finweb --example chk_opening_probe`
//!
//! 探的是 v1.8.43 之前真实存在的四个洞：
//!   1. 存货科目（核算数量）—— 期初数量无处可填（→ ③ 表新增「数量」列）
//!   2. 银行科目（核算银行账户）—— 期初落不到具体账户（→ ③ 表新增「银行账户」列）
//!   3. 预收 / 预付 —— ①② 只收正数，结果被静默跳过（→ 改成可操作的提示 + ③ 收分户）
//!   4. 数量核算科目有金额没数量 —— 没人发现（→ 对账新增第 7 条校验）

use fincore::account::BookOptions;
use fincore::money::Money;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::PathBuf::from("/tmp/finbook_opening_probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let mut opts = BookOptions::default();
    opts.company = "探针加工厂".into();
    opts.start_period = fincore::period::Period::parse("2026-11")?;
    opts.enable_audit = false;
    opts.require_audit = false;
    opts.require_cashier = false;
    let db = findb::Db::create(&dir.join("probe.fbk"), &opts)?;

    // 两个客户 + 一个供应商 + 一个银行账户 + 一个存货
    for (kind, code, name) in [
        (fincore::AuxKind::Customer, "C001", "华达五金"),
        (fincore::AuxKind::Customer, "C002", "鑫盛电器"),
        (fincore::AuxKind::Supplier, "S001", "宏源氧化"),
    ] {
        findb::auxs::insert(&db, &fincore::auxiliary::AuxEntity::new(kind, code, name))?;
    }
    let mut bank = fincore::auxiliary::AuxEntity::new(fincore::AuxKind::Bank, "B01", "工行对公");
    bank.props.insert(
        fincore::auxiliary::prop::BANK_ACCOUNT.into(),
        "6222 0000 1111".into(),
    );
    findb::auxs::insert(&db, &bank)?;
    findb::auxs::insert(
        &db,
        &fincore::auxiliary::AuxEntity::new(fincore::AuxKind::Item, "HG001", "合页 2 寸"),
    )?;

    // 用真模板，只往格子里填字 —— 和用户手上那份一模一样
    let chart = findb::accounts::chart(&db)?;
    let leaves: Vec<&fincore::account::Account> = chart
        .all()
        .into_iter()
        .filter(|a| chart.is_leaf(&a.code))
        .collect();
    let customers = findb::auxs::list(
        &db,
        &fincore::auxiliary::AuxQuery {
            kind: Some(fincore::AuxKind::Customer),
            ..Default::default()
        },
    )?;
    let suppliers = findb::auxs::list(
        &db,
        &fincore::auxiliary::AuxQuery {
            kind: Some(fincore::AuxKind::Supplier),
            ..Default::default()
        },
    )?;
    let sheets = finweb::sheets::opening_collection_template(&leaves, &customers, &suppliers);
    let bal_headers = sheets
        .iter()
        .find(|s| s.name.contains("账户余额"))
        .map(|s| s.headers.clone())
        .unwrap();
    println!("③ 账余额的列头：{bal_headers:?}");

    // 填数：只在既有行上改，找不到就追加
    // `Sheet` 把表头单独放着（title/subtitle/headers/rows），写文件时要拼回一行表头 ——
    // 导入器是**按表头认列**的，漏了这一行整张纸都会被判成「找不到表头」。
    let mut data: Vec<(String, Vec<Vec<String>>)> = sheets
        .iter()
        .map(|s| {
            let mut rows = vec![s.headers.clone()];
            rows.extend(s.rows.iter().cloned());
            (s.name.clone(), rows)
        })
        .collect();
    fn idx(data: &[(String, Vec<Vec<String>>)], name: &str) -> usize {
        data.iter().position(|(n, _)| n == name).unwrap()
    }

    let put = |rows: &mut Vec<Vec<String>>, code: &str, cols: &[(usize, &str)]| {
        let at = match rows.iter().position(|r| r.first().map(|s| s.as_str()) == Some(code)) {
            Some(i) => i,
            None => {
                rows.push(vec![code.to_string(), String::new()]);
                rows.len() - 1
            }
        };
        for (c, v) in cols {
            while rows[at].len() <= *c {
                rows[at].push(String::new());
            }
            rows[at][*c] = (*v).to_string();
        }
    };

    // ① 客户应收：C001 欠 50000；C002 预交 8000 → 负数是预收，应填 ③ 的 2203
    let i1 = idx(&data, "① 客户应收");
    put(&mut data[i1].1, "C001", &[(2, "50000"), (3, "XS-001")]);
    put(&mut data[i1].1, "C002", &[(2, "-8000")]);

    // ② 供应商应付：S001 欠 28000
    let i2 = idx(&data, "② 供应商应付");
    put(&mut data[i2].1, "S001", &[(2, "28000"), (3, "CG-001")]);

    // ③ 账户余额：现金 / 银行（带账户）/ 存货（带数量）/ 预收（带客户）
    let i3 = idx(&data, "③ 账户余额");
    put(&mut data[i3].1, "1001", &[(2, "2000")]);
    put(&mut data[i3].1, "100201", &[(2, "8000"), (6, "B01")]);
    put(&mut data[i3].1, "140501", &[(2, "5000"), (3, "2.5"), (5, "HG001")]);
    put(&mut data[i3].1, "2203", &[(2, "8000"), (4, "C002")]);

    // ④ 实收资本：2000 + 8000 + 5000 + 50000 = 65000 资产，减 28000 应付、8000 预收 → 35000
    let i4 = idx(&data, "④ 基本资料");
    put(&mut data[i4].1, "实收资本", &[(1, "35000")]);

    // 写成 xlsx
    let bytes = to_xlsx(&data)?;
    let res = findb::opening_book::import_collection_sheet(&db, &bytes, "admin")?;
    println!(
        "\n导入：① {} 行 / ② {} 行 / ③ {} 行 / ④ {} 行；借 {} = 贷 {}",
        res.ar.ok,
        res.ap.ok,
        res.balances.ok,
        res.equity.ok,
        res.debit_total.fmt_money(),
        res.credit_total.fmt_money()
    );
    for w in res
        .ar
        .warnings
        .iter()
        .chain(res.ap.warnings.iter())
        .chain(res.balances.warnings.iter())
        .chain(res.equity.warnings.iter())
    {
        println!("  提示：{w}");
    }

    println!("\n库里的期初（看 aux / qty 有没有真的进去）：");
    for r in findb::balances::list_begin(&db)? {
        let aux: Vec<String> = [
            ("客户", &r.aux.customer),
            ("供应商", &r.aux.supplier),
            ("存货", &r.aux.item),
            ("银行", &r.aux.bank),
        ]
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={v}")))
        .collect();
        println!(
            "  {:8} 金额 {:>10}  数量 {:>8}  {}",
            r.account_code,
            r.year_begin.fmt_plain(),
            r.qty_begin.map(|q| q.fmt_plain()).unwrap_or_else(|| "-".into()),
            aux.join(" ")
        );
    }

    let chk = findb::opening_check::reconcile_begin(&db)?;
    println!("\n对账：{}", findb::opening_check::one_liner(&chk));
    for c in &chk.checks {
        println!(
            "  [{}] {} —— {}",
            if c.ok { "OK" } else { "!!" },
            c.name,
            c.detail
        );
    }

    // 台账能不能按客户分开看（预收要挂在 C002 名下）
    println!("\n台账（按客户）：");
    for k in ["ar", "ap"] {
        for r in findb::settle::arap_opening_list(&db, None)?
            .into_iter()
            .filter(|r| r.kind == k)
        {
            println!(
                "  {k} {} {} 金额 {}",
                r.party_code,
                r.party_name,
                r.amount.fmt_plain()
            );
        }
    }
    let _ = Money::ZERO;
    Ok(())
}

/// 把「表名 + 行」写成 xlsx（只在例子用；生产走 `xlsxout`）
fn to_xlsx(data: &[(String, Vec<Vec<String>>)]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use rust_xlsxwriter::{Format, Workbook};
    let mut wb = Workbook::new();
    let f = Format::new();
    for (name, rows) in data {
        let ws = wb.add_worksheet();
        ws.set_name(name.as_str())?;
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if cell.is_empty() {
                    continue;
                }
                // 能当数字就当数字写，和用户在 Excel 里手输一样
                match cell.parse::<f64>() {
                    Ok(n) => {
                        ws.write_number(r as u32, c as u16, n, &f)?;
                    }
                    Err(_) => {
                        ws.write_string(r as u32, c as u16, cell, &f)?;
                    }
                }
            }
        }
    }
    Ok(wb.save_to_buffer()?)
}
