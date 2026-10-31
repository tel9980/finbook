//! 模拟数据体检：造一份「多部门、跨年」的账套，逐项验证不变式与权限口径。
//!
//! 用法: cargo run -p findb --example qa_sim -- /tmp/sim/qa.fbk
//!
//! 与 `bench` 的区别：bench 关心**快不快**，这个关心**对不对**。
//! 它不预设结论，只把「账上应该恒等的关系」和「同一个用户在不同入口看到的
//! 数字应该一致」逐条算一遍，对不上就打印差多少。

use chrono::NaiveDate;
use fincore::chart::default_accounts;
use fincore::report::{self, AmountKind};
use fincore::user::{DataScope, Role, User};
use fincore::{AuxEntity, AuxKind, BookOptions, Entry, Money, Period, Voucher};
use findb::balances::{BalanceQuery, BalanceSnapshot, LedgerQuery};
use findb::{vouchers, Db};

/// 跨月区间的凭证查询（`VoucherQuery` 只有 `period()` 便捷构造，区间要手填）
fn vq(from: Period, to: Period) -> vouchers::VoucherQuery {
    vouchers::VoucherQuery {
        from: Some(from),
        to: Some(to),
        asc: true,
        ..Default::default()
    }
}
use std::path::PathBuf;

fn money(s: &str) -> Money {
    s.parse().unwrap()
}

fn day(y: i32, mo: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, mo, d).unwrap()
}

/// 简易检查器：打印每一条结论，累计失败数。
struct Chk {
    fails: u32,
    known: u32,
    checks: u32,
}

impl Chk {
    fn new() -> Self {
        Self {
            fails: 0,
            known: 0,
            checks: 0,
        }
    }
    /// 已知问题：照样打印实算结果，但不计入失败。
    ///
    /// 目前只有一条：「报表不按部门过滤」。凭证列表按部门过滤（`DataScope.depts`
    /// 下推到 SQL），而明细账/余额表的 `with_user_scope` 完全没看 depts，
    /// 于是部门会计在凭证里看不到别部门的凭证、在余额表里却看得到别部门的金额。
    /// 改哪一边（过滤 or 明确标成「全公司口径」）要产品定，先摆在这里。
    fn known(&mut self, cond: bool, label: &str, detail: String) {
        self.checks += 1;
        if cond {
            println!("  [OK  ] {label}  —— {detail}");
        } else {
            self.known += 1;
            println!("  [KNOWN] {label}  —— {detail}");
        }
    }
    fn ok(&mut self, cond: bool, label: &str, detail: String) {
        self.checks += 1;
        if !cond {
            self.fails += 1;
        }
        println!(
            "  [{}] {label}{}",
            if cond { "OK  " } else { "FAIL" },
            if detail.is_empty() {
                String::new()
            } else {
                format!("  —— {detail}")
            }
        );
    }
}

fn main() {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/sim/qa.fbk".into()),
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("fbk-wal"));
    let _ = std::fs::remove_file(path.with_extension("fbk-shm"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }

    // ── 造账套 ────────────────────────────────────────────────
    let opts = BookOptions {
        start_period: Period::new(2026, 1).unwrap(),
        company: "模拟体检厂".into(),
        enable_audit: false,
        require_cashier: false,
        ..Default::default()
    };
    let db = Db::create_no_admin(&path, &opts).expect("建账");
    findb::accounts::import_many(&db, &default_accounts()).expect("科目");
    findb::reports::reset_cash_flow_items(&db).expect("现金流量项目");

    // 部门 / 客户 / 供应商 / 银行
    let depts = ["D01", "D02", "D03"];
    for (code, name) in [("D01", "生产部"), ("D02", "销售部"), ("D03", "管理部")] {
        findb::auxs::insert(&db, &AuxEntity::new(AuxKind::Dept, code, name)).unwrap();
    }
    let customers: Vec<String> = (1..=20).map(|i| format!("C{i:03}")).collect();
    for c in &customers {
        findb::auxs::insert(
            &db,
            &AuxEntity::new(AuxKind::Customer, c, format!("客户{c}")),
        )
        .unwrap();
    }
    for i in 1..=8 {
        let s = format!("S{i:03}");
        findb::auxs::insert(
            &db,
            &AuxEntity::new(AuxKind::Supplier, &s, format!("供应商{i}")),
        )
        .unwrap();
    }
    for (c, n) in [("B01", "G银行对公户"), ("B02", "N银行结算户")] {
        findb::auxs::insert(&db, &AuxEntity::new(AuxKind::Bank, c, n)).unwrap();
    }

    // 部门核算：默认科目表里**没有**挂部门的末级科目（Dept 只挂在 5001/5101/6601/
    // 6602/6603 这几个上级科目上，而上级不能记账），所以像真实用户那样，手动给
    // 两个末级科目打开部门辅助核算。
    let mut no_dept_leaf = 0;
    {
        let all = findb::accounts::list(&db).unwrap();
        let is_leaf = |code: &str| {
            !all.iter()
                .any(|b| b.code.len() > code.len() && b.code.starts_with(code))
        };
        no_dept_leaf = all
            .iter()
            .filter(|a| is_leaf(&a.code) && a.aux.contains(AuxKind::Dept))
            .count();
        let dept_parents = all
            .iter()
            .filter(|a| a.aux.contains(AuxKind::Dept) && !is_leaf(&a.code))
            .count();
        println!(
            "   默认科目表：挂部门的末级科目 {no_dept_leaf} 个，挂部门的上级科目 {dept_parents} 个"
        );
    }
    let dept_expense = "660201".to_string();
    let dept_cost = "510101".to_string();
    for code in [&dept_expense, &dept_cost] {
        let mut a = findb::accounts::get(&db, code).unwrap().expect("科目存在");
        a.aux = fincore::AuxMask::NONE.with(AuxKind::Dept);
        findb::accounts::update(&db, &a).expect("打开部门核算");
    }

    println!(
        "① 挂部门科目：费用={dept_expense} 成本={dept_cost}；部门 {:?}",
        depts
    );

    // ── 灌凭证：12 个月 × 24 张 ────────────────────────────────
    let months = 12;
    let per_month = 24;
    let mut made = 0;
    for mo in 1..=months {
        let p = Period::new(2026, mo).unwrap();
        for k in 0..per_month {
            let dd = (k % 27 + 1) as u32;
            let mut v = Voucher::new(p, day(2026, mo, dd), "记", 0);
            v.no = vouchers::next_no(&db, p, "记").unwrap();
            let cust = &customers[(made as usize) % customers.len()];
            let dept = depts[(made as usize) % depts.len()];
            let amt = money(&format!("{}.00", 1000 + (made % 50) * 100));
            v.entries = match k % 4 {
                // 卖货挂应收
                0 => {
                    let mut a = Entry::new(1, "112201", "卖货");
                    a.debit = amt;
                    a.aux.customer = Some(cust.clone());
                    let mut b = Entry::new(2, "600101", "卖货");
                    b.credit = amt;
                    vec![a, b]
                }
                // 客户回款
                1 => {
                    let mut a = Entry::new(1, "100201", "收款");
                    a.debit = amt;
                    a.aux.bank = Some("B01".into());
                    let mut b = Entry::new(2, "112201", "收款");
                    b.credit = amt;
                    b.aux.customer = Some(cust.clone());
                    vec![a, b]
                }
                // 部门费用（挂部门！）
                2 => {
                    let mut a = Entry::new(1, &dept_expense, "部门费用");
                    a.debit = amt;
                    a.aux.dept = Some(dept.to_string());
                    let mut b = Entry::new(2, "100201", "部门费用");
                    b.credit = amt;
                    b.aux.bank = Some("B01".into());
                    vec![a, b]
                }
                // 部门成本（挂部门）
                _ => {
                    let mut a = Entry::new(1, &dept_cost, "部门成本");
                    a.debit = amt;
                    a.aux.dept = Some(dept.to_string());
                    let mut b = Entry::new(2, "100201", "部门成本");
                    b.credit = amt;
                    b.aux.bank = Some("B02".into());
                    vec![a, b]
                }
            };
            let id = vouchers::save(&db, &mut v).expect("存凭证");
            vouchers::post(&db, id, "qa").expect("记账");
            made += 1;
        }
    }
    println!("② 凭证 {made} 张（{} 条分录）", made * 2);

    let chart = findb::accounts::chart(&db).unwrap();

    // ── 结转损益 + 结账：真实账套每月都会做这两步 ────────────────────
    // 只结账不结转，损益类科目余额会一直留在账上，资产负债表必然对不上；
    // 所以要按真实操作顺序：先结转，再结账，且结账时要求结转已完成。
    let mut closed = 0;
    let mut carried = 0;
    let mut p = Period::new(2026, 1).unwrap();
    while p <= Period::new(2026, 12).unwrap() {
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        // 结转取数直接用产品 API。曾经这里得自己按 (科目, 部门) 逐条拼行，
        // 因为 `carry_forward_rows()` 用 `for_account(code, None)` 把各部门聚成
        // 一条不带部门的行，生成的结转凭证保存时被「必须填写部门」拒掉——
        // v1.8.61 修好了（改走 aux_breakdown），所以这里也回到最朴素的写法。
        let rows = snap.carry_forward_rows(&chart);
        if !rows.is_empty() {
            let no = vouchers::next_no(&db, p, "转").unwrap();
            let mut cv = fincore::engine::period_end::generate_carry_forward(
                p,
                p.last_day(),
                "转",
                no,
                &rows,
                &chart,
                "4103",
                "qa",
            )
            .expect("生成结转凭证");
            let id = vouchers::save(&db, &mut cv).expect("存结转凭证");
            vouchers::post(&db, id, "qa").expect("记结转凭证");
            carried += 1;
        }
        // 12 月只结转不结账：留着它当「当前期间」，好让 A2 也能验证开放期间的恒等式
        if p < Period::new(2026, 12).unwrap() {
            match findb::periods::close(&db, p, "qa", true) {
                Ok(iss) if iss.is_empty() => closed += 1,
                Ok(iss) => {
                    println!("   结账 {p:?} 被拦：{}", iss.join("；"));
                    break;
                }
                Err(e) => {
                    println!("   结账 {p:?} 失败：{e}");
                    break;
                }
            }
        }
        p = p.next();
    }
    println!("   结转 {carried} 次、结账 {closed} 期（12 月只结转不结算）");
    let mut c = Chk::new();

    // ── A. 跨期不变式 ─────────────────────────────────────────
    println!("\n=== A. 跨期不变式 ===");
    let year_start = Period::new(2026, 1).unwrap();
    let mut bad_tb = Vec::new();
    let mut bad_bs = Vec::new();
    for mo in 1..=months {
        let p = Period::new(2026, mo).unwrap();
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let tb = snap.trial_balance(&chart);
        if !(tb.begin_balanced() && tb.period_balanced()) {
            bad_tb.push(format!("{p:?}"));
        }
        // 资产负债表恒等式（期末列）
        let ysnap = BalanceSnapshot::load(&db, &BalanceQuery::range(year_start, p)).unwrap();
        let bdef = report::balance_sheet::balance_sheet_def();
        let table = report::render(
            &bdef,
            &ysnap,
            "模拟体检厂",
            &format!("{p:?} 期末"),
            vec![Box::new(report::identity)],
        );
        let at = report::balance_sheet::asset_total_row(&bdef).unwrap();
        let lt = report::balance_sheet::liab_equity_total_row(&bdef).unwrap();
        let assets = table.rows[at].values[0];
        let le = table.rows[lt].values[0];
        if assets != le {
            bad_bs.push(format!(
                "{p:?} 资产 {} ≠ 负债+权益 {}（差 {}）",
                assets.fmt_plain(),
                le.fmt_plain(),
                (assets - le).fmt_plain()
            ));
        }
    }
    c.ok(
        bad_tb.is_empty(),
        "A1 各期试算平衡",
        if bad_tb.is_empty() {
            format!("{months} 期全平衡")
        } else {
            bad_tb.join("; ")
        },
    );
    c.ok(
        bad_bs.is_empty(),
        "A2 资产负债表恒等式（资产 = 负债 + 所有者权益）",
        if bad_bs.is_empty() {
            format!("{months} 期全平")
        } else {
            bad_bs.join("; ")
        },
    );

    // A3 利润表净利润 == 首页口径净利润（12 月）
    let cur = Period::new(2026, 12).unwrap();
    let is_def = report::income::income_statement_def();
    let isn = BalanceSnapshot::load(&db, &BalanceQuery::range(cur, cur).with_exclude_carry(true))
        .unwrap();
    let istable = report::render_single(&is_def, &isn, "模拟体检厂", "2026-12", report::identity);
    let npr = report::income::net_profit_row(&is_def).unwrap();
    let is_net = istable.rows[npr].values[0];
    let ft = findb::advanced::financial_totals(&db, cur, cur, None).unwrap();
    c.ok(
        is_net == ft.net_profit,
        "A3 利润表净利润 == 首页净利润（2026-12）",
        format!(
            "利润表 {} / 首页 {}",
            is_net.fmt_plain(),
            ft.net_profit.fmt_plain()
        ),
    );

    // A4 明细账发生额 == 余额表发生额（按科目）
    println!("\n=== A4 明细账 vs 科目余额表（全年发生额）===");
    for code in [&dept_expense, &dept_cost, "112201", "100201"] {
        let lq = LedgerQuery {
            code: (*code).to_string(),
            include_children: false,
            aux: None,
            from: year_start,
            to: cur,
            posted_only: true,
            prepared_by: None,
            code_from: None,
            code_to: None,
        };
        let rows = findb::balances::ledger(&db, &chart, &lq).unwrap();
        let ldebit: Money = rows.iter().fold(Money::ZERO, |s, r| s + r.debit);
        let lcredit: Money = rows.iter().fold(Money::ZERO, |s, r| s + r.credit);
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::range(year_start, cur)).unwrap();
        let row = snap.for_account(code, None);
        c.ok(
            ldebit == row.ytd_debit && lcredit == row.ytd_credit,
            &format!("A4 明细账 == 余额表  {code}"),
            format!(
                "明细账 借 {} 贷 {} / 余额表 借 {} 贷 {}",
                ldebit.fmt_plain(),
                lcredit.fmt_plain(),
                row.ytd_debit.fmt_plain(),
                row.ytd_credit.fmt_plain()
            ),
        );
    }

    // ── B. 部门范围：四个入口看到的数字必须一致 ────────────────
    println!("\n=== B. 部门范围（只看 D01）===");
    let mut dept_user = User::new("deptview", "部门会计", Role::Accountant);
    dept_user.data_scope = DataScope {
        depts: vec!["D01".into()],
        ..Default::default()
    };

    // ① 凭证列表：SQL 下推 + can_see_voucher 过滤后可见的张数
    let all_v = vouchers::list(&db, &vq(year_start, cur).with_data_scope(&dept_user)).unwrap();
    let mut manual = vouchers::list(&db, &vq(year_start, cur)).unwrap();
    // can_see_voucher 靠 entries 判定部门，而 list() 默认不带分录：空 entries 会让
    // all() 恒真，过滤结果永远等于全量。这里必须补齐，否则测的是「没过滤」。
    vouchers::fill_entries(&db, &mut manual).unwrap();
    let expect_v = manual
        .iter()
        .filter(|v| dept_user.can_see_voucher(v))
        .count();
    c.ok(
        all_v.len() == expect_v,
        "B1 凭证列表（SQL 下推）== can_see_voucher 逐张过滤",
        format!("SQL {} 张 / 逐张过滤 {} 张", all_v.len(), expect_v),
    );

    // ② 明细账：部门会计看到的部门费用发生额
    let lq = LedgerQuery {
        code: dept_expense.clone(),
        include_children: false,
        aux: None,
        from: year_start,
        to: cur,
        posted_only: true,
        prepared_by: None,
        code_from: None,
        code_to: None,
    }
    .with_user_scope(&dept_user);
    let ledger_rows = findb::balances::ledger(&db, &chart, &lq).unwrap();
    let ledger_debit: Money = ledger_rows.iter().fold(Money::ZERO, |s, r| s + r.debit);

    // ③ 余额表：同一用户、同一科目、同一区间
    let snap = BalanceSnapshot::load(
        &db,
        &BalanceQuery::range(year_start, cur).with_user_scope(&dept_user),
    )
    .unwrap();
    let bal = snap.for_account(&dept_expense, None);
    let bal_debit = bal.ytd_debit;

    // ④ 真值：只把「部门在范围内」的分录加起来
    let mut full = vouchers::list(&db, &vq(year_start, cur)).unwrap();
    vouchers::fill_entries(&db, &mut full).unwrap();
    let mut truth = Money::ZERO;
    let mut all_dept = Money::ZERO;
    for v in &full {
        for e in &v.entries {
            if e.account_code == dept_expense {
                all_dept += e.debit;
                if e.aux.dept.as_deref().map(|d| d.trim()) == Some("D01") {
                    truth += e.debit;
                }
            }
        }
    }
    println!(
        "   ④ 真值（只算 D01 的分录）= {}；全部门合计 = {}",
        truth.fmt_plain(),
        all_dept.fmt_plain()
    );
    c.known(
        ledger_debit == truth,
        "B2 明细账只含本部门分录",
        format!(
            "明细账 {} / 真值 {}",
            ledger_debit.fmt_plain(),
            truth.fmt_plain()
        ),
    );
    c.known(
        bal_debit == truth,
        "B3 科目余额表只含本部门发生额",
        format!(
            "余额表 {} / 真值 {}",
            bal_debit.fmt_plain(),
            truth.fmt_plain()
        ),
    );

    // ── C. 已结账期间不得再写 ─────────────────────────────────
    println!("\n=== C. 已结账期间写入 ===");
    let closed_p = Period::new(2026, 3).unwrap();
    let mut v = Voucher::new(closed_p, day(2026, 3, 5), "记", 0);
    v.no = 900;
    let mut a = Entry::new(1, "112201", "越界写入");
    a.debit = money("100.00");
    a.aux.customer = Some("C001".into());
    let mut b = Entry::new(2, "600101", "越界写入");
    b.credit = money("100.00");
    v.entries = vec![a, b];
    let r = vouchers::save(&db, &mut v);
    c.ok(
        r.is_err(),
        "C1 向已结账期间保存凭证被拒",
        match &r {
            Ok(_) => "竟然成功了".to_string(),
            Err(e) => format!("{e}"),
        },
    );

    // ── D. 分页 total 与真实可见张数（假分页体检）────────────────
    println!("\n=== D. 分页 total 一致性 ===");
    let scopes: Vec<(&str, DataScope)> = vec![
        (
            "只看 D01",
            DataScope {
                depts: vec!["D01".into()],
                ..Default::default()
            },
        ),
        (
            "只看 D02",
            DataScope {
                depts: vec!["D02".into()],
                ..Default::default()
            },
        ),
        (
            "D01+D02",
            DataScope {
                depts: vec!["D01".into(), "D02".into()],
                ..Default::default()
            },
        ),
        ("不限部门", DataScope::default()),
        (
            "D01 + 科目区间",
            DataScope {
                depts: vec!["D01".into()],
                account_from: "6602".into(),
                ..Default::default()
            },
        ),
    ];
    for (label, scope) in scopes {
        let mut u = User::new("u", "u", Role::Accountant);
        u.data_scope = scope;
        let q = vq(year_start, cur).with_data_scope(&u);
        let (rows, total) = vouchers::list_page(&db, &q, 0, 20).unwrap();
        // 翻完所有页，数一数实际能拿到多少张不同凭证
        let mut seen = std::collections::HashSet::new();
        let mut off = 0;
        loop {
            let (page, _) = vouchers::list_page(&db, &q, off, 20).unwrap();
            if page.is_empty() {
                break;
            }
            for v in page {
                seen.insert(v.id);
            }
            off += 20;
            if off > total + 100 {
                break;
            }
        }
        c.ok(
            total as usize == seen.len() && rows.len() <= 20,
            &format!("D total == 能翻到的张数  [{label}]"),
            format!("total={} 实翻到={}", total, seen.len()),
        );
    }

    // ── 汇总 ─────────────────────────────────────────────────
    println!(
        "\n========== 汇总：{} 项检查，{} 项失败，{} 项已知问题 ==========",
        c.checks, c.fails, c.known
    );
    println!(
        "库文件 {} KB",
        std::fs::metadata(&path).unwrap().len() / 1024
    );
}
