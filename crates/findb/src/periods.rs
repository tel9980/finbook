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
    let n = db
        .conn()
        .execute("DELETE FROM period_state WHERE period=?1", rusqlite::params![p.ymm()])?;
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
