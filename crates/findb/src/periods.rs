//! 期间状态：期末结账与反结账

use fincore::engine::check_can_close;
use fincore::{FinError, Period};

use crate::balances::{BalanceQuery, BalanceSnapshot};
use crate::vouchers::status_summary;
use crate::{accounts, vouchers, Db, DbResult};

/// 已结账的最大期间。结账必须逐月连续，因此只需要记住最后一个。
pub fn closed_upto(db: &Db) -> DbResult<Option<Period>> {
    closed_upto_of(db.conn())
}

/// 同 `closed_upto`，但只依赖连接，可在已开启的事务内调用
/// （`rusqlite::Transaction` 会 Deref 到 `Connection`）。凭证守卫要在事务里读它，
/// 否则「查结账线 → 写入」之间存在把凭证写进刚被结账期间的窗口。
pub fn closed_upto_of(conn: &rusqlite::Connection) -> DbResult<Option<Period>> {
    // 只有「查无已结账期间」才算 None；查询本身失败必须上抛，
    // 否则 `.ok()` 一吞，结账线读不到时所有期间守卫都 fail-open。
    let v: Option<i32> = conn.query_row(
        "SELECT MAX(period) FROM period_state WHERE closed=1",
        [],
        |r| r.get(0),
    )?;
    Ok(v.map(Period::from_ymm))
}

pub fn is_closed(db: &Db, p: Period) -> DbResult<bool> {
    let c: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM period_state WHERE period=?1 AND closed=1",
        rusqlite::params![p.ymm()],
        |r| r.get(0),
    )?;
    Ok(c > 0)
}

/// 所有已结账期间
pub fn list_closed(db: &Db) -> DbResult<Vec<Period>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT period FROM period_state WHERE closed=1 ORDER BY period")?;
    let rows = stmt
        .query_map([], |r| Ok(Period::from_ymm(r.get(0)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 结账。返回问题清单（为空表示成功）。
pub fn close(db: &Db, p: Period, who: &str, require_carry: bool) -> DbResult<Vec<String>> {
    let opts = db.options();
    let (drafts, audited, _posted, _void) = status_summary(db, p)?;
    let unposted = drafts + audited;

    let chart = accounts::chart(db)?;
    let snap = BalanceSnapshot::load(
        db,
        &BalanceQuery {
            from: p,
            to: p,
            ..BalanceQuery::period(p)
        },
    )?;
    let trial = snap.trial_balance(&chart);
    let pl_rows = snap.profit_loss_rows(&chart);

    let input = fincore::engine::CloseCheckInput {
        period: p,
        unposted: unposted as usize,
        drafts: drafts as usize,
        trial: &trial,
        pl_rows: &pl_rows,
        require_carry,
        closed_upto: closed_upto(db)?,
        start_period: opts.start_period,
    };
    let iss = check_can_close(&input);
    if !iss.is_empty() {
        return Ok(iss.iter().cloned().collect());
    }

    // 写操作进同一事务：结账标记与操作日志要么一起落库，要么一起回滚。
    let tx = db.write_tx()?;
    // 事务内复核：上面的检查跑在事务外，检查与落库之间可能有人补录凭证或并发结账。
    // BEGIN IMMEDIATE 保证复核后不会再被写事务插队。
    let unposted_now: i64 = tx.query_row(
        "SELECT COUNT(*) FROM voucher WHERE period=?1 AND status IN ('draft','audited')",
        rusqlite::params![p.ymm()],
        |r| r.get(0),
    )?;
    if unposted_now > 0 {
        return Ok(vec![format!(
            "{} 还有 {unposted_now} 张未记账凭证，请先记账后再结账",
            p.label()
        )]);
    }
    if let Some(upto) = closed_upto_of(&tx)? {
        if p <= upto {
            return Ok(vec![format!("{} 及以前期间已结账", p.label())]);
        }
    }
    tx.execute(
        "INSERT INTO period_state(period,closed,closed_at,closed_by)
         VALUES(?1,1,?2,?3)
         ON CONFLICT(period) DO UPDATE SET closed=1, closed_at=excluded.closed_at,
             closed_by=excluded.closed_by",
        rusqlite::params![
            p.ymm(),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            who
        ],
    )?;
    crate::log_on(&tx, who, "期末", "结账", &p.label())?;
    tx.commit()?;
    Ok(Vec::new())
}

/// 反结账。这是敏感操作，需要记录是谁做的。
pub fn unclose(db: &Db, p: Period, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    // 复核放在事务内：check_can_unclose 依赖结账线，事务外读会与并发结账竞争
    fincore::engine::check_can_unclose(p, closed_upto_of(&tx)?).into_result()?;
    tx.execute(
        "UPDATE period_state SET closed=0, closed_at=NULL, closed_by=NULL WHERE period=?1",
        rusqlite::params![p.ymm()],
    )?;
    crate::log_on(&tx, who, "期末", "反结账", &p.label())?;
    tx.commit()?;
    Ok(())
}

/// 当前可录入凭证的期间：最后一个已结账期间的下一期，或启用期间
pub fn current_period(db: &Db) -> DbResult<Period> {
    match closed_upto(db)? {
        Some(u) => {
            let next = u.next();
            // 已结到账套最后一期时停在原地，避免无限前进
            Ok(next)
        }
        None => Ok(db.options().start_period),
    }
}

/// 同 [`current_period`]，但只依赖连接，可在事务内调用。
///
/// 核销要在一个写事务里同时「读结账线 → 写核销记录」，两步之间不能有窗口：
/// 否则刚判完「当前期没结账」，期间就被别人结了账，核销记录落进已结账期间。
pub fn current_period_of(conn: &rusqlite::Connection) -> DbResult<Period> {
    match closed_upto_of(conn)? {
        Some(u) => Ok(u.next()),
        None => Ok(crate::options_of(conn).start_period),
    }
}

/// 某期间是否允许录入凭证
pub fn is_open(db: &Db, p: Period) -> DbResult<bool> {
    Ok(!is_closed(db, p)? && p >= db.options().start_period)
}

/// 该期间是否存在任何凭证
pub fn has_vouchers(db: &Db, p: Period) -> DbResult<bool> {
    let c: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM voucher WHERE period=?1",
        rusqlite::params![p.ymm()],
        |r| r.get(0),
    )?;
    Ok(c > 0)
}

/// 结账前检查（只检查不执行），供界面预览用
pub fn precheck(db: &Db, p: Period, require_carry: bool) -> DbResult<Vec<String>> {
    let opts = db.options();
    let (drafts, audited, _, _) = status_summary(db, p)?;
    let chart = accounts::chart(db)?;
    let snap = BalanceSnapshot::load(
        db,
        &BalanceQuery {
            from: p,
            to: p,
            ..BalanceQuery::period(p)
        },
    )?;
    let trial = snap.trial_balance(&chart);
    let pl_rows = snap.profit_loss_rows(&chart);
    let iss = check_can_close(&fincore::engine::CloseCheckInput {
        period: p,
        unposted: (drafts + audited) as usize,
        drafts: drafts as usize,
        trial: &trial,
        pl_rows: &pl_rows,
        require_carry,
        closed_upto: closed_upto(db)?,
        start_period: opts.start_period,
    });
    Ok(iss.iter().cloned().collect())
}

/// 强制删除某期间的结账标记（仅供数据修复，界面不暴露）
pub fn force_reset(db: &Db, p: Period) -> DbResult<()> {
    let n = db.conn().execute(
        "DELETE FROM period_state WHERE period=?1",
        rusqlite::params![p.ymm()],
    )?;
    if n == 0 {
        return Err(FinError::not_found(format!("期间 {} 无结账记录", p.label())).into());
    }
    Ok(())
}

/// 该期间未记账凭证数量（界面标题栏提示用）
pub fn unposted_count(db: &Db, p: Period) -> DbResult<i64> {
    let c: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM voucher WHERE period=?1 AND status IN ('draft','audited')",
        rusqlite::params![p.ymm()],
        |r| r.get(0),
    )?;
    Ok(c)
}

/// 批量记账某期间的待记账凭证（无审核环节：所有未记账凭证一并记账）
pub fn post_all(db: &Db, p: Period, who: &str) -> DbResult<(usize, Vec<String>)> {
    let q = vouchers::VoucherQuery {
        from: Some(p),
        to: Some(p),
        ..Default::default()
    };
    let list = vouchers::list(db, &q)?;
    let ids: Vec<i64> = list
        .iter()
        .filter(|v| v.status.can_post())
        .map(|v| v.id)
        .collect();
    vouchers::post_many(db, &ids, who)
}

#[cfg(test)]
mod close_cycle_tests {
    use super::*;
    use crate::balances::{BalanceQuery, BalanceSnapshot};
    use crate::Db;
    use fincore::engine::period_end::{
        check_can_unclose, generate_carry_forward, generate_year_end_carry, is_year_end,
        PROFIT_ACCOUNT,
    };
    use fincore::{Entry, Money, Period, Voucher};

    fn m(n: &str) -> Money {
        Money::parse(n).unwrap()
    }

    fn entry(no: i32, code: &str, dr: &str, cr: &str) -> Entry {
        let mut e = Entry::new(no, code, "测试");
        e.debit = m(dr);
        e.credit = m(cr);
        e
    }

    /// 记一张已记账凭证
    fn book(db: &Db, p: Period, no: i32, dr_code: &str, cr_code: &str, amt: &str) {
        let mut v = Voucher::new(
            p,
            chrono::NaiveDate::from_ymd_opt(p.year(), p.month(), 10).unwrap(),
            "记",
            no,
        );
        v.push_entry(entry(1, dr_code, amt, "0"));
        v.push_entry(entry(2, cr_code, "0", amt));
        let id = vouchers::save(db, &mut v).unwrap();
        vouchers::post(db, id, "admin").unwrap();
    }

    fn carry(db: &Db, p: Period, no: i32) {
        let chart = crate::accounts::chart(db).unwrap();
        let snap =
            BalanceSnapshot::load(db, &BalanceQuery::period(p).with_posted_only(true)).unwrap();
        let rows: Vec<_> = snap
            .carry_forward_rows(&chart)
            .into_iter()
            .filter(|r| !(r.debit - r.credit).round2().is_zero())
            .collect();
        if rows.is_empty() {
            return;
        }
        let date = chrono::NaiveDate::from_ymd_opt(p.year(), p.month(), 28).unwrap();
        let mut v =
            generate_carry_forward(p, date, "记", no, &rows, &chart, PROFIT_ACCOUNT, "admin")
                .unwrap();
        let id = vouchers::save(db, &mut v).unwrap();
        vouchers::post(db, id, "admin").unwrap();
    }

    fn profit_of(db: &Db, p: Period) -> Money {
        let snap =
            BalanceSnapshot::load(db, &BalanceQuery::period(p).with_posted_only(true)).unwrap();
        snap.for_account(PROFIT_ACCOUNT, None).end()
    }

    /// 结转 → 结账 → 反结账 → 补账 → 重结转 → 重结账，全程数值正确
    #[test]
    fn 结转结账反结账往返() {
        let db = crate::tests::mem();
        let p = Period::new(2026, 1).unwrap();
        let chart = crate::accounts::chart(&db).unwrap();

        // 收入 20000，费用 5000
        book(&db, p, 1, "1001", "600101", "20000");
        book(&db, p, 2, "510103", "1001", "5000");

        // 未结转时不允许结账
        let problems = close(&db, p, "admin", true).unwrap();
        assert!(!problems.is_empty(), "未结转就结账必须被拒");
        assert!(
            problems.iter().any(|s| s.contains("尚未结转")),
            "{problems:?}"
        );

        // 结转后损益类净额归零，本年利润 = 15000（贷方）
        carry(&db, p, 3);
        assert_eq!(profit_of(&db, p), m("-15000"));
        let snap =
            BalanceSnapshot::load(&db, &BalanceQuery::period(p).with_posted_only(true)).unwrap();
        for r in snap.carry_forward_rows(&chart) {
            assert!(
                (r.debit - r.credit).round2().is_zero(),
                "{} 未归零",
                r.account_code
            );
        }

        // 结账成功，且该期间不再允许录凭证
        assert!(close(&db, p, "admin", true).unwrap().is_empty());
        assert_eq!(closed_upto(&db).unwrap(), Some(p));
        assert!(is_closed(&db, p).unwrap());
        let mut v = Voucher::new(
            p,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 20).unwrap(),
            "记",
            9,
        );
        v.push_entry(entry(1, "1001", "100", "0"));
        v.push_entry(entry(2, "2241", "0", "100"));
        assert!(
            vouchers::save(&db, &mut v).is_err(),
            "已结账期间必须拒写凭证"
        );

        // 反结账后可以补账，重新结转、重新结账
        unclose(&db, p, "admin").unwrap();
        assert_eq!(closed_upto(&db).unwrap(), None);
        assert!(is_open(&db, p).unwrap());

        book(&db, p, 4, "510104", "1001", "800");
        carry(&db, p, 5);
        assert_eq!(profit_of(&db, p), m("-14200"));
        assert!(close(&db, p, "admin", true).unwrap().is_empty());
    }

    /// 不能跳月结账，反结账只能从最后期间开始
    #[test]
    fn 结账顺序约束() {
        let db = crate::tests::mem();
        let p1 = Period::new(2026, 1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        let p3 = Period::new(2026, 3).unwrap();

        // 首次结账必须从启用期间开始
        assert!(!precheck(&db, p2, false).unwrap().is_empty());

        carry(&db, p1, 1);
        assert!(close(&db, p1, "admin", true).unwrap().is_empty());
        carry(&db, p2, 2);
        assert!(close(&db, p2, "admin", true).unwrap().is_empty());

        // 跳过 2 月直接结 3 月被拒（此时已结到 2 月，3 月其实可以；改测重复结账）
        assert!(
            !precheck(&db, p2, false).unwrap().is_empty(),
            "重复结账必须被拒"
        );

        // 反结账只能从最后期间
        assert!(!check_can_unclose(p1, Some(p2)).all().is_empty());
        assert!(unclose(&db, p1, "admin").is_err());
        unclose(&db, p2, "admin").unwrap();
        assert_eq!(closed_upto(&db).unwrap(), Some(p1));
        let _ = p3;
    }

    /// 年末结转：盈利、亏损、零、缺科目四种情况
    #[test]
    fn 年末结转四种情况() {
        let db = crate::tests::mem();
        let chart = crate::accounts::chart(&db).unwrap();
        let year_end = Period::new(2026, 12).unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();

        assert!(is_year_end(year_end));
        assert!(!is_year_end(Period::new(2026, 11).unwrap()));

        // 盈利：贷方余额 → 借本年利润 贷未分配利润
        let v = generate_year_end_carry(
            year_end,
            date,
            "记",
            1,
            m("-44200"),
            "410401",
            &chart,
            "admin",
        )
        .unwrap();
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.entries[0].debit, m("44200"));
        assert_eq!(v.entries[1].credit, m("44200"));

        // 亏损：借方余额 → 反向
        let v = generate_year_end_carry(
            year_end,
            date,
            "记",
            2,
            m("5000"),
            "410401",
            &chart,
            "admin",
        )
        .unwrap();
        assert_eq!(v.entries[0].credit, m("5000"));
        assert_eq!(v.entries[1].debit, m("5000"));

        // 余额为零：拒绝
        assert!(generate_year_end_carry(
            year_end,
            date,
            "记",
            3,
            Money::ZERO,
            "410401",
            &chart,
            "admin"
        )
        .is_err());

        // 目标科目不存在：拒绝
        assert!(generate_year_end_carry(
            year_end,
            date,
            "记",
            4,
            m("-1"),
            "999999",
            &chart,
            "admin"
        )
        .is_err());
    }

    /// 跨年：12 月结账后 1 月可以正常开始，closed_upto 正确推进
    #[test]
    fn 跨年结账连续() {
        let db = crate::tests::mem();
        let p12 = Period::new(2026, 12).unwrap();
        let p_next = p12.next();
        assert_eq!(p_next, Period::new(2027, 1).unwrap());

        // 启用期间是 2026-01，必须从 1 月逐月结到 12 月，不能直接跳
        assert!(
            !close(&db, p12, "admin", false).unwrap().is_empty(),
            "跳月必须被拒"
        );
        for mo in 1..=12u32 {
            let p = Period::new(2026, mo).unwrap();
            carry(&db, p, mo as i32);
            assert!(
                close(&db, p, "admin", true).unwrap().is_empty(),
                "结账 {mo} 月失败"
            );
        }
        assert_eq!(closed_upto(&db).unwrap(), Some(p12));
        assert!(is_open(&db, p_next).unwrap(), "跨年后 1 月应可录凭证");
        assert!(close(&db, p_next, "admin", false).unwrap().is_empty());
        assert_eq!(closed_upto(&db).unwrap(), Some(p_next));
    }
}
