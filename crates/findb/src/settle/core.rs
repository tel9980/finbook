//! 基础核销：核销记录、单笔/部分/整单核销、撤销、差异核销（抹零）。
//!
//! 分册说明：本文件原属 `settle.rs` 单文件（3,437 行），按小节标题切开，
//! 函数体与注释一字未改；跨文件共用的内部函数标 `pub(super)`，对外 API 由
//! `settle/mod.rs` 统一 `pub use` 出去，外部调用路径 `findb::settle::xxx` 不变。

use super::*;

/// 一条核销记录
#[derive(Clone, Debug)]
pub struct SettleRecord {
    pub id: i64,
    pub period: Period,
    pub account_code: String,
    pub aux_key: String,
    /// 被核销的分录（原单据）
    pub from_entry: i64,
    /// 核销方分录（收款 / 付款）
    pub to_entry: i64,
    pub amount: Money,
    pub settled_by: String,
    pub settled_at: String,
}

fn map_rec(r: &rusqlite::Row) -> rusqlite::Result<SettleRecord> {
    Ok(SettleRecord {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        account_code: r.get(2)?,
        aux_key: r.get(3)?,
        from_entry: r.get(4)?,
        to_entry: r.get(5)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(6)?),
        settled_by: r.get(7)?,
        settled_at: r.get(8)?,
    })
}

const COLS: &str =
    "id,period,account_code,aux_key,from_entry,to_entry,amount,settled_by,settled_at";

/// 某科目（含下级）的全部核销记录
///
/// 口径与 [`open_entries`]、[`aging`] 一致：父级科目能查到下级科目的记录，
/// 否则界面默认按 1122 查询时会出现"未核销已清、记录却为空"的矛盾。
pub fn list(db: &Db, account: &str) -> DbResult<Vec<SettleRecord>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {COLS} FROM settle_record WHERE (account_code=?1 OR account_code LIKE ?1||'%') ORDER BY period, id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![account], map_rec)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 涉及某分录的全部核销
pub fn list_for_entry(db: &Db, entry_id: i64) -> DbResult<Vec<SettleRecord>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {COLS} FROM settle_record WHERE from_entry=?1 OR to_entry=?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![entry_id], map_rec)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 某分录已被核销掉的金额
///
/// 收 `&Connection` 而非 `&Db`：核销要在同一事务里「读未核销额 → 校验 → 写回」，
/// 需要能借给事务句柄（`Transaction` 会 Deref 到 `Connection`）。
pub fn settled_of(conn: &Connection, entry_id: i64) -> DbResult<Money> {
    // 金额在库里是 TEXT，SUM 会走浮点，精度不可控；这里取行在 Rust 侧用定点累加
    let mut st =
        conn.prepare("SELECT amount FROM settle_record WHERE from_entry=?1 OR to_entry=?1")?;
    let rows = st
        .query_map(rusqlite::params![entry_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut sum = Money::ZERO;
    for s in rows {
        sum += Money::parse_or_zero(&s);
    }
    Ok(sum)
}

/// 批量取多个分录的已核销额（避免 N+1 查询）
pub fn settled_map(db: &Db, entry_ids: &[i64]) -> DbResult<std::collections::HashMap<i64, Money>> {
    let mut out = std::collections::HashMap::new();
    if entry_ids.is_empty() {
        return Ok(out);
    }
    let ph = entry_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    // 同样不能用 SUM()：TEXT 列求和会走 REAL，分位精度不可控
    let sql = format!(
        "SELECT k, amount FROM (
            SELECT from_entry AS k, id, amount FROM settle_record WHERE from_entry IN ({ph})
            UNION ALL
            SELECT to_entry AS k, id, amount FROM settle_record WHERE to_entry IN ({ph})
         ) ORDER BY k"
    );
    let mut st = db.conn().prepare(&sql)?;
    // 两个 IN 各吃全部 id（先全部、再全部）：之前写的是逐个翻倍
    // `[id1,id1,id2,id2…]`，每个 IN 只吃到连续一半 —— from 侧永远漏后一半、
    // to 侧永远漏前一半。两行以内碰巧全对，三行以上已配平的分录会被当成未清，
    // 自动核销第二轮就去重复配，被超额校验拦下（"超过核销方未核销额 0.00"）。
    let ids2: Vec<&dyn rusqlite::ToSql> = entry_ids
        .iter()
        .chain(entry_ids.iter())
        .map(|i| i as &dyn rusqlite::ToSql)
        .collect();
    let rows = st
        .query_map(ids2.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                Money::parse_or_zero(&r.get::<_, String>(1)?),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (k, v) in rows {
        *out.entry(k).or_insert(Money::ZERO) += v;
    }
    Ok(out)
}

pub(super) fn settled_map_stub(
    db: &Db,
    entry_ids: &[i64],
) -> DbResult<std::collections::HashMap<i64, Money>> {
    settled_map(db, entry_ids)
}

/// 手工核销。会自动校验：两边分录同科目、同往来单位、金额不超过各自未核销额。
pub fn settle(db: &Db, from_entry: i64, to_entry: i64, amount: Money, who: &str) -> DbResult<i64> {
    // 「读未核销额 → 校验 → 累加写回」必须整体原子。BEGIN IMMEDIATE 在开头就取得
    // 写锁，期间不会有别的写事务插进来；否则两笔并发核销会各自通过超额校验，
    // 并把对方的累计额覆盖掉（唯一索引只保证行唯一，管不住金额）。
    let tx = db.write_tx()?;
    let id = settle_in_tx(&tx, from_entry, to_entry, amount, who)?;
    tx.commit()?;
    Ok(id)
}

/// 同事务版核销：供收付款单在建单事务内复用。校验与 [`settle`] 完全一致，**不 commit**。
pub fn settle_in_tx(
    tx: &rusqlite::Transaction,
    from_entry: i64,
    to_entry: i64,
    amount: Money,
    who: &str,
) -> DbResult<i64> {
    if from_entry == to_entry {
        return Err(fincore::FinError::msg("不能把分录核销到自己身上").into());
    }
    if amount <= Money::ZERO {
        return Err(fincore::FinError::msg("核销金额必须大于零").into());
    }
    let f = match entry_of(tx, from_entry)? {
        Some(e) => e,
        None => return Err(fincore::FinError::not_found("被核销分录").into()),
    };
    let t = match entry_of(tx, to_entry)? {
        Some(e) => e,
        None => return Err(fincore::FinError::not_found("核销方分录").into()),
    };
    if f.account_code != t.account_code {
        return Err(fincore::FinError::msg(format!(
            "两条分录不在同一科目（{} vs {}），不能核销",
            f.account_code, t.account_code
        ))
        .into());
    }
    if f.aux_key != t.aux_key {
        return Err(fincore::FinError::msg("两条分录的往来单位不一致，不能核销").into());
    }
    // 核销的**被核销方**（from_entry，即被冲销的应收/应付）必须已记账。
    //
    // 手工核销的 entry id 由前端传入，绕过了 open_entries 的 posted-only 过滤，
    // 必须在这里兜住：拿一张草稿当债权去核销，那张凭证一删核销记录就没了。
    // `OpenEntry::posted` 就是为此带到这里的。
    //
    // 注意**只卡 from 侧、不卡 to 侧**——这是刻意的，也是金蝶的做法：
    // 收款单审核后即可与已记账的应收核销，而它自己的凭证往往还没记账
    // （金蝶文档把「单据审核 → 核销 → 生成凭证」列为三个并列步骤）。
    // FinBook 没有独立的应收单层，收款凭证就是那张凭证，所以 to 侧允许是草稿。
    if !f.posted {
        return Err(fincore::FinError::msg(
            "被核销方必须是已记账的往来分录；未记账凭证请先记账（列表页可勾「含未记账」查看待记账往来）",
        )
        .into());
    }
    // 检查超额
    let open_f = f.signed().abs() - settled_of(tx, from_entry)?;
    let open_t = t.signed().abs() - settled_of(tx, to_entry)?;
    if amount > open_f {
        return Err(fincore::FinError::msg(format!(
            "核销金额 {amount} 超过被核销方未核销额 {open_f}"
        ))
        .into());
    }
    if amount > open_t {
        return Err(fincore::FinError::msg(format!(
            "核销金额 {amount} 超过核销方未核销额 {open_t}"
        ))
        .into());
    }

    // 读取现有累计核销额（TEXT 列，避免在 SQL 侧做字符串加法）
    let existing = tx
        .query_row(
            "SELECT amount FROM settle_record WHERE from_entry=?1 AND to_entry=?2",
            rusqlite::params![from_entry, to_entry],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default();
    let old_amount = Money::parse_or_zero(&existing);
    let new_amount = old_amount + amount;

    // 这条记录记在**哪个期间**：记「核销发生的期间」，不是「被核销单据的期间」。
    //
    // 以前写的是 `f.period`，于是 11 月收到客户还 8 月的货款，台账里这笔核销记在
    // 202608。两个后果：①账面上看不出「钱是哪个月收的」；②8 月可能已经结账，
    // 而结账后本该谁也动不了 8 月 —— 却还能往里面塞核销记录。
    //
    // 改成当前期间之后，规则自洽：新增核销永远落在未结账的当前期，
    // 撤销时若记录所在期间已结账就拒绝（见 `unsettle` / `unsettle_part`）。
    let rec_period = crate::periods::current_period_of(tx)?;

    tx.execute(
        "INSERT INTO settle_record(period,account_code,aux_key,from_entry,to_entry,amount,
            settled_by,settled_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(from_entry,to_entry) DO UPDATE SET
             amount=excluded.amount, settled_by=excluded.settled_by, settled_at=excluded.settled_at",
        rusqlite::params![
            rec_period.ymm(),
            f.account_code,
            f.aux_key,
            from_entry,
            to_entry,
            crate::money_param(new_amount), // 无千分位的定点数，保证后续计算正确
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    let id = tx.last_insert_rowid();
    Ok(id)
}

/// 撤销一条核销记录（全额）。
///
/// **已结账期间的记录不允许撤销**：账龄、未核销额都是「截至某期」的数，
/// 结账之后把它改掉，等于让已经出过的报表对不上账 —— 而报表是给人签过字的。
/// 要改就只能先反结账，那条路有日志、有明确意图。
pub fn unsettle(db: &Db, id: i64) -> DbResult<()> {
    let tx = db.write_tx()?;
    let rec = record_of(&tx, id)?;
    guard_not_closed(&tx, &rec)?;
    tx.execute(
        "DELETE FROM settle_record WHERE id=?1",
        rusqlite::params![id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 部分撤销：把一条核销记录减掉 `amount`，减到 0 就整条删掉。
///
/// 现实场景：11 月收到 5 万，先核销了三张单共 5 万；第二天发现其中一张应该是 3 万、
/// 另外 2 万挂在下个月的单上。以前只能整条撤销再重录 —— 撤销的瞬间三张单全变成
/// 「未核销」，账龄立刻虚高，而且这段时间里别人看到的数就是错的。
///
/// 守卫：金额必须 `0 < amount <= 记录金额`；记录所在期间已结账则拒绝。
pub fn unsettle_part(db: &Db, id: i64, amount: Money) -> DbResult<Money> {
    if amount <= Money::ZERO {
        return Err(fincore::FinError::validate("撤销金额必须大于零").into());
    }
    let tx = db.write_tx()?;
    let rec = record_of(&tx, id)?;
    guard_not_closed(&tx, &rec)?;
    if amount > rec.amount {
        return Err(fincore::FinError::msg(format!(
            "撤销金额 {amount} 超过这条核销记录本身的金额 {}",
            rec.amount
        ))
        .into());
    }
    let left = rec.amount - amount;
    if left.is_zero() {
        tx.execute(
            "DELETE FROM settle_record WHERE id=?1",
            rusqlite::params![id],
        )?;
    } else {
        tx.execute(
            "UPDATE settle_record SET amount=?1 WHERE id=?2",
            rusqlite::params![crate::money_param(left), id],
        )?;
    }
    tx.commit()?;
    Ok(left)
}

fn record_of(conn: &Connection, id: i64) -> DbResult<SettleRecord> {
    conn.query_row(
        &format!("SELECT {COLS} FROM settle_record WHERE id=?1"),
        rusqlite::params![id],
        map_rec,
    )
    .optional()?
    .ok_or_else(|| fincore::FinError::not_found("核销记录").into())
}

/// 记录所在期间已结账 → 拒绝改动。
fn guard_not_closed(tx: &rusqlite::Transaction, rec: &SettleRecord) -> DbResult<()> {
    if let Some(upto) = crate::periods::closed_upto_of(tx)? {
        if rec.period <= upto {
            return Err(fincore::FinError::msg(format!(
                "这条核销记在 {}，该期间已结账（已结至 {}）——已结账期间的核销不能撤销。\
                 确需更正请先反结账，再改动。",
                rec.period.label(),
                upto.label()
            ))
            .into());
        }
    }
    Ok(())
}

/// 清除某分录的全部核销（删改凭证时用）
pub fn unsettle_entry(db: &Db, entry_id: i64) -> DbResult<usize> {
    Ok(db.conn().execute(
        "DELETE FROM settle_record WHERE from_entry=?1 OR to_entry=?1",
        rusqlite::params![entry_id],
    )?)
}

/// 带「差异」的核销结果。
#[derive(Clone, Debug)]
pub struct DiffSettleResult {
    /// 实际金额那笔核销记录的 id
    pub settle_id: i64,
    /// 差异抹零生成的凭证 id；差异为 0 时为 `None`（退化成普通核销）
    pub diff_voucher_id: Option<i64>,
}

/// 带「差异（现金折扣 / 抹零）」的核销。
///
/// 现实场景：应收 10000，客户实付 9980，差 20 抹零。只做
/// `settle(from,to,9980)` 的话，被核销方还剩 20 永远挂着，账龄越拖越长。本函数在核销
/// `amount` 之外，把 `diff` 通过**一张新凭证**记入 `diff_account`，并把这笔差异分录
/// 与被核销方也核销掉——于是被核销方**全额结清**，差异落到损益（财务费用 / 营业外支出）。
///
/// 分录方向与核销方 `to_entry` 同侧（这样才能继续冲减被核销方）：
/// - 收款抹零：`借 差异科目 / 贷 应收`（贷方与被核销的借方应收对冲）；
/// - 付款抹零：`借 应付 / 贷 差异科目`。
///
/// 守卫：`amount>0`、`diff>=0`、`diff_account` 存在且不与被核销科目相同；超额由
/// `settle_in_tx` 复校验。**事务原子**：差异凭证 + 两笔核销同进同出。
pub fn settle_with_diff(
    db: &Db,
    from_entry: i64,
    to_entry: i64,
    amount: Money,
    diff: Money,
    diff_account: &str,
    who: &str,
) -> DbResult<DiffSettleResult> {
    if diff < Money::ZERO {
        return Err(fincore::FinError::validate("差异金额不能为负").into());
    }
    if amount <= Money::ZERO {
        return Err(fincore::FinError::validate("核销金额必须大于零").into());
    }
    // 无差异：退化成普通核销（不生成差异凭证）
    if diff.is_zero() {
        let tx = db.write_tx()?;
        let id = settle_in_tx(&tx, from_entry, to_entry, amount, who)?;
        tx.commit()?;
        return Ok(DiffSettleResult {
            settle_id: id,
            diff_voucher_id: None,
        });
    }
    let diff_account = diff_account.trim();
    if diff_account.is_empty() {
        return Err(fincore::FinError::validate(
            "差异科目不能为空（如 6603 财务费用 / 6711 营业外支出）",
        )
        .into());
    }
    if crate::accounts::get(db, diff_account)?.is_none() {
        return Err(fincore::FinError::validate(format!("差异科目 {diff_account} 不存在")).into());
    }
    let tx = db.write_tx()?;
    let f = entry_of(&tx, from_entry)?.ok_or_else(|| fincore::FinError::not_found("被核销分录"))?;
    let t = entry_of(&tx, to_entry)?.ok_or_else(|| fincore::FinError::not_found("核销方分录"))?;
    if diff_account == f.account_code {
        return Err(fincore::FinError::validate(
            "差异科目不能与被核销的往来科目相同（那样差异仍挂在往来上，清不掉）",
        )
        .into());
    }
    // 差异分录必须与被核销方同科目 + 同往来辅助，才能被 settle_in_tx 接受
    let aux = AuxRef::from_key(&f.aux_key);
    let period = f.period;
    let date = f.date;
    let to_is_debit = t.debit > Money::ZERO;
    let summary = format!("核销差异 {}", f.account_code);
    let mut v = Voucher::new(
        period,
        date,
        "记",
        crate::vouchers::next_no_of(&tx, period, "记")?,
    );
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("核销差异（{}）", f.account_code);
    // 差异分录与核销方 to_entry 同侧，另一侧落差异科目
    if to_is_debit {
        v.push_entry(Entry {
            debit: diff,
            aux: aux.clone(),
            ..Entry::new(1, f.account_code.as_str(), summary.as_str())
        });
        v.push_entry(Entry {
            credit: diff,
            ..Entry::new(2, diff_account, summary.as_str())
        });
    } else {
        v.push_entry(Entry {
            debit: diff,
            ..Entry::new(1, diff_account, summary.as_str())
        });
        v.push_entry(Entry {
            credit: diff,
            aux: aux.clone(),
            ..Entry::new(2, f.account_code.as_str(), summary.as_str())
        });
    }
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    let diff_entry: i64 = tx.query_row(
        "SELECT id FROM voucher_entry WHERE voucher_id=?1 AND account_code=?2 ORDER BY line LIMIT 1",
        rusqlite::params![vid, f.account_code],
        |r| r.get(0),
    )?;
    // 先核销实际金额，再核销差异（两笔都以被核销方为 from，故各自复校验未核销额）
    let settle_id = settle_in_tx(&tx, from_entry, to_entry, amount, who)?;
    settle_in_tx(&tx, from_entry, diff_entry, diff, who)?;
    tx.commit()?;
    Ok(DiffSettleResult {
        settle_id,
        diff_voucher_id: Some(vid),
    })
}
