//! 往来核销与账龄分析
//!
//! 核销是"单据级"的：一张应收单可以分多次收款核销，也可以一笔款核销多张单。
//! 所以这里存的是**分录 ↔ 分录 + 金额**，而不是"客户整体结清"。

use chrono::NaiveDate;
use fincore::engine::aging::{AgingBucket, AgingItem, AgingLine};
use fincore::{AuxRef, Direction, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::{Connection, OptionalExtension};

use crate::{Db, DbResult};

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

fn settled_map_stub(db: &Db, entry_ids: &[i64]) -> DbResult<std::collections::HashMap<i64, Money>> {
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

    tx.execute(
        "INSERT INTO settle_record(period,account_code,aux_key,from_entry,to_entry,amount,
            settled_by,settled_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(from_entry,to_entry) DO UPDATE SET
             amount=excluded.amount, settled_by=excluded.settled_by, settled_at=excluded.settled_at",
        rusqlite::params![
            f.period.ymm(),
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

pub fn unsettle(db: &Db, id: i64) -> DbResult<()> {
    db.conn().execute(
        "DELETE FROM settle_record WHERE id=?1",
        rusqlite::params![id],
    )?;
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

// ---------------- 通用往来互抵（跨往来单位 / 跨科目） ----------------

/// 通用往来互抵的结果
#[derive(Clone, Debug)]
pub struct CrossSettleResult {
    /// 自动生成的过渡凭证 id。两侧本就同科目同辅助时无需过渡，为 `None`。
    pub bridge_voucher_id: Option<i64>,
    /// 落下的核销记录 id（借方侧、贷方侧）
    pub settle_ids: Vec<i64>,
}

/// 把 `aux_key`（形如 `customer=C01\x1fdept=D01`）压成短标签 `(C01/D01)`，供凭证摘要显示。
fn aux_short(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    let vals: Vec<&str> = key
        .split('\u{1f}')
        .filter_map(|seg| seg.split_once('=').map(|(_, v)| v))
        .collect();
    if vals.is_empty() {
        String::new()
    } else {
        format!("({})", vals.join("/"))
    }
}

/// 通用往来互抵（跨往来单位 / 跨科目）。
///
/// 现实里"不是一一对应"的挂账远不止同客户同名：A 的付款可能清的是 B 的货款
/// （代收代付），客户的应收也可能直接顶掉我们欠供应商的应付（三方抵账）。
/// 这些两边**科目或往来单位并不相同**，普通核销（要求同科目同辅助）挡得住，
/// 所以这里换个做法：自动出一张**过渡凭证**把两边接上，再各自照常核销。
///
/// 过渡凭证永远是「借 [贷方分录科目/辅助] / 贷 [借方分录科目/辅助]」：
/// - 代收代付：借方挂账 `应收-B` + 贷方挂账 `应收-A`（A 的收款 / 预收）
///   → 过渡 `借 应收-A / 贷 应收-B`，两边同时清；与那张收款合起来净额即 `借 银行 / 贷 应收-B`；
/// - 三方抵账：借方挂账 `应收-A` + 贷方挂账 `应付-B`
///   → 过渡 `借 应付-B / 贷 应收-A`（等价于往来对冲单，只是这里按**分录**精确挑）。
///
/// 撤销：过渡凭证是一张普通草稿凭证；删除它（需先反记账）会连带清掉这里的核销记录，
/// 两条挂账随之复原。
pub fn settle_cross(
    db: &Db,
    debit_entry: i64,
    credit_entry: i64,
    amount: Money,
    date: NaiveDate,
    who: &str,
) -> DbResult<CrossSettleResult> {
    if debit_entry == credit_entry {
        return Err(fincore::FinError::msg("借方与贷方不能是同一笔分录").into());
    }
    if amount <= Money::ZERO {
        return Err(fincore::FinError::msg("互抵金额必须大于零").into());
    }
    let tx = db.write_tx()?;
    let d =
        entry_of(&tx, debit_entry)?.ok_or_else(|| fincore::FinError::not_found("借方互抵分录"))?;
    let c =
        entry_of(&tx, credit_entry)?.ok_or_else(|| fincore::FinError::not_found("贷方互抵分录"))?;
    if d.signed() <= Money::ZERO {
        return Err(fincore::FinError::msg(format!(
            "分录 #{debit_entry} 不是借方挂账（借方 {} / 贷方 {}），请把它选为贷方",
            d.debit, d.credit
        ))
        .into());
    }
    if c.signed() >= Money::ZERO {
        return Err(fincore::FinError::msg(format!(
            "分录 #{credit_entry} 不是贷方挂账（借方 {} / 贷方 {}），请把它选为借方",
            c.debit, c.credit
        ))
        .into());
    }
    // 两条都必须是**已记账**的真实挂账：核销记录一旦挂在随时可删的草稿上，凭证一删就凭空消失
    if !d.posted || !c.posted {
        return Err(
            fincore::FinError::msg("互抵的两笔往来分录都必须已记账；未记账的请先记账").into(),
        );
    }
    let open_d = d.open();
    let open_c = c.open();
    if amount > open_d {
        return Err(
            fincore::FinError::msg(format!("互抵金额 {amount} 超过借方未核销额 {open_d}")).into(),
        );
    }
    if amount > open_c {
        return Err(
            fincore::FinError::msg(format!("互抵金额 {amount} 超过贷方未核销额 {open_c}")).into(),
        );
    }

    // 同科目同辅助：本来就是普通核销，不必出过渡凭证
    if d.account_code == c.account_code && d.aux_key == c.aux_key {
        let id = settle_in_tx(&tx, debit_entry, credit_entry, amount, who)?;
        tx.commit()?;
        return Ok(CrossSettleResult {
            bridge_voucher_id: None,
            settle_ids: vec![id],
        });
    }

    let period = Period::from_date(date);
    let mut v = Voucher::new(
        period,
        date,
        "记",
        crate::vouchers::next_no_of(&tx, period, "记")?,
    );
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!(
        "往来互抵 {}{} ↔ {}{}",
        d.account_code,
        aux_short(&d.aux_key),
        c.account_code,
        aux_short(&c.aux_key)
    );
    let summary = v.memo.clone();
    // 借 贷方分录那一侧（把它的贷方余额抵掉）
    v.push_entry(Entry {
        debit: amount,
        aux: AuxRef::from_key(&c.aux_key),
        ..Entry::new(1, c.account_code.as_str(), summary.as_str())
    });
    // 贷 借方分录那一侧
    v.push_entry(Entry {
        credit: amount,
        aux: AuxRef::from_key(&d.aux_key),
        ..Entry::new(2, d.account_code.as_str(), summary.as_str())
    });
    let vid = crate::vouchers::save_in(&tx, &mut v)?;

    // 取回过渡凭证两条分录 id：按「科目 + 辅助」精确匹配到对应腿
    // （同科目不同辅助也可区分；两侧完全相同时上面已走普通核销分支）
    let bridge_debit: i64 = tx.query_row(
        "SELECT id FROM voucher_entry WHERE voucher_id=?1 AND account_code=?2 AND aux_key=?3
         ORDER BY line LIMIT 1",
        rusqlite::params![vid, c.account_code, c.aux_key],
        |r| r.get(0),
    )?;
    let bridge_credit: i64 = tx.query_row(
        "SELECT id FROM voucher_entry WHERE voucher_id=?1 AND account_code=?2 AND aux_key=?3
         ORDER BY line LIMIT 1",
        rusqlite::params![vid, d.account_code, d.aux_key],
        |r| r.get(0),
    )?;

    let settle_ids = vec![
        settle_in_tx(&tx, debit_entry, bridge_credit, amount, who)?,
        settle_in_tx(&tx, credit_entry, bridge_debit, amount, who)?,
    ];
    tx.commit()?;
    Ok(CrossSettleResult {
        bridge_voucher_id: Some(vid),
        settle_ids,
    })
}

// ---------------- 往来单据 ----------------

/// 一条往来分录（含已核销额）
#[derive(Clone, Debug)]
pub struct OpenEntry {
    pub entry_id: i64,
    pub voucher_id: i64,
    pub period: Period,
    pub date: NaiveDate,
    pub word: String,
    pub no: i32,
    pub line: i32,
    pub summary: String,
    pub account_code: String,
    pub aux_key: String,
    pub settle_no: String,
    pub debit: Money,
    pub credit: Money,
    pub settled: Money,
    /// 所属凭证是否已记账。
    ///
    /// 核销 / 账龄 / 催款只认已记账（全仓 H-3 口径），所以默认查询不会返回
    /// 未记账行，这个字段恒为 true。列表页勾了「含未记账」时才会出现 false 的行，
    /// 用来在前端标注「仅供查看、不可核销」。
    pub posted: bool,
}

impl OpenEntry {
    /// 带符号金额（借方为正）
    pub fn signed(&self) -> Money {
        self.debit - self.credit
    }
    /// 未核销额（带符号）
    pub fn open(&self) -> Money {
        let o = self.signed().abs() - self.settled;
        if o < Money::parse("0.005").unwrap() {
            Money::ZERO
        } else {
            o
        }
    }
    pub fn is_open(&self) -> bool {
        !self.open().is_zero()
    }
    /// 方向标签
    pub fn dir_label(&self) -> &'static str {
        if self.debit > Money::ZERO {
            "借"
        } else {
            "贷"
        }
    }
}

fn entry_of(conn: &Connection, entry_id: i64) -> DbResult<Option<OpenEntry>> {
    conn.query_row(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                    e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit,
                    v.status
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE e.id=?1",
        rusqlite::params![entry_id],
        |r| {
            let d: String = r.get(3)?;
            let status: String = r.get(13)?;
            Ok(OpenEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(4)?,
                no: r.get(5)?,
                line: r.get(6)?,
                summary: r.get(7)?,
                account_code: r.get(8)?,
                aux_key: r.get(9)?,
                settle_no: r.get(10)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                settled: Money::ZERO,
                posted: status == "posted",
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// 取某科目（含下级）截至某日的全部**已记账**往来分录，并填充已核销额。
///
/// 口径与全仓 H-3 定案一致：**只取已记账**。早先写的是 `status != 'void'`，
/// 把草稿/已审核未记账也算成未核销往来，于是自动核销、账龄、催款单都能对着
/// 一张还没入账的凭证做事——那张凭证随时可删，核销记录会凭空消失。
///
/// 核销、账龄、催款**一律走本函数**，不要用 [`open_entries_with`] 放宽。
pub fn open_entries(
    db: &Db,
    account: &str,
    upto: Period,
    include_all: bool,
) -> DbResult<Vec<OpenEntry>> {
    open_entries_with(db, account, upto, include_all, true)
}

/// 同 [`open_entries`]，但可显式要求**连未记账的一起返回**（`posted_only = false`）。
///
/// 只给往来核销**列表页的「含未记账」只读开关**用：让用户看得见待记账的往来，
/// 避免以为数据丢了。金蝶的单据驱动模型里，未审核单据同样能在列表里看到、
/// 只是不参与核销——这里复刻的就是那一层。返回行带 [`OpenEntry::posted`] = false，
/// 前端据此标注「仅供查看」；核销入口（自动核销 / 手工核销）不读这个函数，
/// 仍走 `open_entries`，所以未记账的行不可能被核销。
pub fn open_entries_with(
    db: &Db,
    account: &str,
    upto: Period,
    include_all: bool,
    posted_only: bool,
) -> DbResult<Vec<OpenEntry>> {
    // 已记账分支固定用字面量 'posted'（走索引），放宽分支才用参数
    let status_clause = if posted_only {
        "= 'posted'"
    } else {
        "!= 'void'"
    };
    let sql = format!(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit,
                v.status
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
         WHERE (e.account_code = ?1 OR e.account_code LIKE ?1||'%')
           AND v.period <= ?2 AND v.status {status_clause}
           AND (e.debit <> '0' OR e.credit <> '0')
         ORDER BY v.date, v.no, e.line"
    );
    let mut st = db.conn().prepare(&sql)?;
    let mut rows = st
        .query_map(rusqlite::params![account, upto.ymm()], |r| {
            let d: String = r.get(3)?;
            let status: String = r.get(13)?;
            Ok(OpenEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(4)?,
                no: r.get(5)?,
                line: r.get(6)?,
                summary: r.get(7)?,
                account_code: r.get(8)?,
                aux_key: r.get(9)?,
                settle_no: r.get(10)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                settled: Money::ZERO,
                posted: status == "posted",
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let ids: Vec<i64> = rows.iter().map(|r| r.entry_id).collect();
    let sm = settled_map_stub(db, &ids)?;
    for r in rows.iter_mut() {
        r.settled = sm.get(&r.entry_id).copied().unwrap_or(Money::ZERO);
    }
    if !include_all {
        rows.retain(|r| r.is_open());
    }
    Ok(rows)
}

// ---------------- 自动核销 ----------------

/// 自动核销结果
#[derive(Clone, Debug, Default)]
pub struct AutoSettleResult {
    pub pairs: usize,
    pub amount: Money,
    /// 同金额精确匹配的笔数
    pub exact: usize,
    /// 尾数清零（余额小于阈值直接抹平）的笔数
    pub written_off: usize,
    /// 核销失败的提示（尾数抹平等「尽力而为」的动作，失败不阻断整批，但必须可见）
    pub warnings: Vec<String>,
}

/// 自动核销
///
/// 策略（保守优先，宁可少勾也不错勾）：
/// 1. 同一往来单位内，先找**金额完全相等的**一借一贷配对（最可靠）
/// 2. 剩余部分尝试用一笔收款逐条吃掉最早的单据（FIFO），吃掉后余额小于阈值则抹平
/// 3. 匹配不上的全部留空，交给手工
pub fn auto_settle(
    db: &Db,
    account: &str,
    upto: Period,
    tolerance: Money,
    who: &str,
) -> DbResult<AutoSettleResult> {
    let mut res = AutoSettleResult::default();
    let all = open_entries(db, account, upto, false)?;

    // 按往来单位分组
    let mut groups: std::collections::BTreeMap<String, Vec<OpenEntry>> =
        std::collections::BTreeMap::new();
    for e in all {
        groups.entry(e.aux_key.clone()).or_default().push(e);
    }

    for (_key, mut g) in groups {
        // 第一轮：精确等额配对
        let mut i = 0usize;
        while i < g.len() {
            if g[i].open().is_zero() {
                i += 1;
                continue;
            }
            let dir_pos = g[i].debit > Money::ZERO;
            let amt = g[i].open();
            let hit = g.iter().position(|x| {
                x.entry_id != g[i].entry_id
                    && (x.debit > Money::ZERO) != dir_pos
                    && x.open() == amt
                    && !x.open().is_zero()
            });
            if let Some(j) = hit {
                let (from, to) = if dir_pos {
                    (g[i].entry_id, g[j].entry_id)
                } else {
                    (g[j].entry_id, g[i].entry_id)
                };
                let id = settle(db, from, to, amt, who)?;
                let _ = id;
                res.pairs += 1;
                res.exact += 1;
                res.amount += amt;
                // 重新读一次，保证 open() 是最新的
                g = open_entries_of(db, &g)?;
                i = 0;
                continue;
            }
            i += 1;
        }

        // 第二轮：FIFO 逐笔吃掉，余额小于容差则抹平
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 200 {
                break;
            }
            g = open_entries_of(db, &g)?;
            let debtors: Vec<&OpenEntry> = g
                .iter()
                .filter(|x| x.debit > Money::ZERO && x.is_open())
                .collect();
            let credits: Vec<&OpenEntry> = g
                .iter()
                .filter(|x| x.credit > Money::ZERO && x.is_open())
                .collect();
            if debtors.is_empty() || credits.is_empty() {
                break;
            }
            let d = debtors[0];
            let c = credits[0];
            let amt = d.open().min(c.open());
            if amt.is_zero() {
                break;
            }
            let id = settle(db, d.entry_id, c.entry_id, amt, who)?;
            let _ = id;
            res.pairs += 1;
            res.amount += amt;
            // 吃掉后若剩余小于容差，直接抹平
            let d_left = d.open() - amt;
            let c_left = c.open() - amt;
            for (left, id) in [(d_left, d.entry_id), (c_left, c.entry_id)] {
                if !left.is_zero() && left <= tolerance {
                    // 找反向未结清分录把尾数吃掉
                    if let Some(other) = g.iter().find(|x| {
                        x.entry_id != id
                            && x.is_open()
                            && (x.debit > Money::ZERO) != (d.debit > Money::ZERO)
                    }) {
                        // 尾数抹平是「尽力而为」：失败不阻断整批，但也绝不能计数——
                        // 旧写法丢弃错误后照样 pairs/amount +=，界面显示"已核销 N 笔"
                        // 而库里一条核销记录都没有。
                        match settle(db, id, other.entry_id, left, who) {
                            Ok(_) => {
                                res.pairs += 1;
                                res.written_off += 1;
                                res.amount += left;
                            }
                            Err(e) => res.warnings.push(format!(
                                "分录 {} 的尾数 {} 抹平失败：{e}",
                                id,
                                left.fmt_money()
                            )),
                        }
                    }
                }
            }
        }
    }
    Ok(res)
}

/// 用当前库里的数据刷新一批分录的已核销额（保留原顺序与范围）
fn open_entries_of(db: &Db, old: &[OpenEntry]) -> DbResult<Vec<OpenEntry>> {
    let ids: Vec<i64> = old.iter().map(|o| o.entry_id).collect();
    let sm = settled_map(db, &ids)?;
    let mut out = Vec::with_capacity(old.len());
    let mut st = db.conn().prepare(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit,
                v.status
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id WHERE e.id=?1",
    )?;
    for id in ids {
        let mut e: OpenEntry = st.query_row(rusqlite::params![id], |r| {
            let d: String = r.get(3)?;
            let status: String = r.get(13)?;
            Ok(OpenEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(4)?,
                no: r.get(5)?,
                line: r.get(6)?,
                summary: r.get(7)?,
                account_code: r.get(8)?,
                aux_key: r.get(9)?,
                settle_no: r.get(10)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                settled: Money::ZERO,
                posted: status == "posted",
            })
        })?;
        e.settled = sm.get(&id).copied().unwrap_or(Money::ZERO);
        out.push(e);
    }
    Ok(out)
}

// ---------------- 账龄 ----------------

/// 往来科目 → 辅助核算维度：**科目首位**决定（1 开头 = 应收/客户，2 开头 = 应付/供应商）。
///
/// 设为 `pub`：调用方拿到 `AgingLine.key`（**裸编码** `C01`）后，要显示名称或写回
/// AuxRef，都得先知道它是客户还是供应商。这个判断不许在别处重写一遍 ——
/// 写死 customer 会让应付账龄整页空白（`2202` 挂的是 `supplier=`）。
pub fn party_kind(account: &str) -> fincore::AuxKind {
    match account.as_bytes().first() {
        Some(b'2') => fincore::AuxKind::Supplier,
        _ => fincore::AuxKind::Customer,
    }
}

/// 往来维度：按科目的**真实辅助核算**取；取不到（如父级科目无辅助标记、
/// 账套里没这个科目）才退回首位推断。
///
/// 首位规则对 2203（预收挂的是客户核算）、1123（其他应收挂的是供应商）
/// 是错的 —— 账龄整页空白且不报错（2203 已实锤：5000 余额账龄 0 行）。
pub fn party_kind_of(chart: &fincore::Chart, account: &str) -> fincore::AuxKind {
    use fincore::AuxKind::{Customer, Employee, Supplier};
    if let Some(a) = chart.get(account) {
        let kinds = a.aux.list();
        // 单一维度是常态；真有两个时按客户＞供应商＞职员取
        for k in [Customer, Supplier, Employee] {
            if kinds.contains(&k) {
                return k;
            }
        }
    }
    party_kind(account)
}

/// 带账套的维度查询：读得到科目表就按真实辅助核算，读不到退回首位推断。
/// 界面层（往来核销页、Web 账龄接口）统一走这个，别自己重写一遍。
pub fn party_kind_in(db: &Db, account: &str) -> fincore::AuxKind {
    crate::accounts::chart(db)
        .map(|c| party_kind_of(&c, account))
        .unwrap_or_else(|_| party_kind(account))
}

/// 账龄分析
pub fn aging(
    db: &Db,
    account: &str,
    upto: Period,
    as_of: NaiveDate,
    buckets: &[AgingBucket],
) -> DbResult<Vec<AgingLine>> {
    let entries = open_entries(db, account, upto, false)?;
    // 往来维度按科目的真实辅助核算取（取不到才按首位推），见 party_kind_of。
    //
    // ⚠️ 不能写死 customer —— 应付挂的是 `supplier=`，写死 customer 会让
    // 应付账龄一行都不出（整个 2202 报表变空白，而且**不报错**）。
    // 同理不能只按首位猜：2203 预收挂的是客户核算，首位规则下整页空白。
    let chart = crate::accounts::chart(db)?;
    let party = party_kind_of(&chart, account);
    // 期初明细的 ar/ap 与维度是同一条规则，一起算一次。
    let want = match party {
        fincore::AuxKind::Supplier => Some("ap"),
        // Customer 与 Employee：期初单据只有 ar/ap 两种 kind，
        // 1221 职员单据历来存 ar，保持现状
        _ => Some("ar"),
    };
    // 没挂往来单位的分录不进往来账龄 —— 往来账龄的每一行都应该是一个
    // 具体的往来单位；一行 key 为空的记录在界面上无法解释、也无法操作。
    //
    // 能发生的情形是**用户自己加的**不带 aux 的应收/应付科目（预置的 112201/220201
    // 在科目表里就声明了核算客商，凭证校验会先拒掉）。
    //
    // 方向归一：贷方性质科目的分录是贷增借减，账龄展示统一翻成正数
    // （往来期初明细存的本来就是正数）。否则整张应付账龄全落在预收/预付栏：
    // 合计恒 0、最长账龄恒 0 —— 新手打开以为没有欠款。
    // 按科目真实方向取（取不到才按首位推；标准表上两者一致）。
    let flip = chart
        .get(account)
        .map(|a| a.dir == Direction::Credit)
        .unwrap_or_else(|| account.starts_with('2'));
    let mut items: Vec<AgingItem> = entries
        .iter()
        .filter(|e| !crate::balances::aux_key_value_of(&e.aux_key, party).is_empty())
        .map(|e| AgingItem {
            // key 取**往来维度**（裸编码），不是整条 aux_key。
            //
            // 原来这里用 `e.aux_key.clone()` → "customer=C01"，而下面的期初行用
            // `o.party_code` → "C01"。同一个客户于是被 analyze() 分成两行
            // （实测：AGING_KEYS=["customer=C01", "C01"]，3000 与 500 分在两行），
            // 界面上看起来是两个往来单位，按行做的操作会漏掉一半。
            //
            // 用 balances.rs 的解析函数而不是自己 split —— aux_key 的格式
            // 只有那一个权威定义点。
            key: crate::balances::aux_key_value_of(&e.aux_key, party),
            date: e.date,
            amount: if flip {
                e.signed().negated()
            } else {
                e.signed()
            },
            settled: e.settled,
            doc_no: format!("{}-{}", e.word, e.no),
        })
        .collect();
    // 往来期初明细（迁移数据，影子行）：按科目方向取对应类型（1开头=应收 / 2开头=应付），
    // 单据日期期间不晚于 upto；**只进账龄展示、不参与 FIFO 核销**（核销 v2，避免伪 entry 外键）。
    if let Some(w) = want {
        for o in arap_opening_list(db, Some(w))? {
            let d = NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
            if Period::from_date(d).ymm() > upto.ymm() {
                continue;
            }
            items.push(AgingItem {
                key: o.party_code.clone(),
                date: d,
                // 期初单据恒按正数进账龄（入库时已要求金额为正）。
                // 应付翻转只在分录侧做一次 —— 这里若再按 kind 取反，
                // 翻两次就回到负数，整张应付账龄又掉回预付栏。
                amount: o.amount,
                settled: Money::ZERO,
                doc_no: format!(
                    "期初 {}",
                    if o.doc_no.is_empty() {
                        format!("#{}", o.id)
                    } else {
                        o.doc_no.clone()
                    }
                ),
            });
        }
    }
    Ok(fincore::engine::aging::analyze(&items, as_of, buckets)?)
}

// ---------------- 往来期初明细（按单据，平台迁移导入 v2） ----------------

/// 往来期初明细（应收/应付逐单据）。**影子挂账**：进账龄展示、不参与 FIFO 核销（v2）——
/// 金额与客商总额仍由「科目期初」负责，本表是迁移来的逐单欠款凭证信息。
#[derive(Clone, Debug, serde::Serialize)]
pub struct ArapOpening {
    pub id: i64,
    /// ar 应收 / ap 应付
    pub kind: String,
    pub party_code: String,
    pub party_name: String,
    pub doc_no: String,
    pub doc_date: String,
    pub amount: Money,
    pub memo: String,
    pub created_by: String,
}

const ARO_COLS: &str = "id,kind,party_code,party_name,doc_no,doc_date,amount,memo,created_by";

fn map_arap_opening(r: &rusqlite::Row) -> rusqlite::Result<ArapOpening> {
    Ok(ArapOpening {
        id: r.get(0)?,
        kind: r.get(1)?,
        party_code: r.get(2)?,
        party_name: r.get(3)?,
        doc_no: r.get(4)?,
        doc_date: r.get(5)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(6)?),
        memo: r.get(7)?,
        created_by: r.get(8)?,
    })
}

pub fn arap_opening_list(db: &Db, kind: Option<&str>) -> DbResult<Vec<ArapOpening>> {
    let out = match kind {
        Some(k) => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {ARO_COLS} FROM arap_opening WHERE kind=?1 ORDER BY doc_date, id"
            ))?;
            let rows = st.query_map([k], map_arap_opening)?;
            rows.collect::<Result<Vec<_>, _>>()?
        }
        None => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {ARO_COLS} FROM arap_opening ORDER BY kind, doc_date, id"
            ))?;
            let rows = st.query_map([], map_arap_opening)?;
            rows.collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok(out)
}

/// 是否已有同 kind+客商+单据号（幂等重导跳过）
pub fn arap_opening_exists(db: &Db, kind: &str, party: &str, doc_no: &str) -> DbResult<bool> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM arap_opening WHERE kind=?1 AND party_code=?2 AND doc_no=?3",
        rusqlite::params![kind, party, doc_no],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn arap_opening_insert(db: &Db, o: &ArapOpening, who: &str) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO arap_opening(kind,party_code,party_name,doc_no,doc_date,amount,memo,created_by,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        rusqlite::params![
            o.kind,
            o.party_code,
            o.party_name,
            o.doc_no,
            o.doc_date,
            crate::exact_param(o.amount),
            o.memo,
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn arap_opening_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM arap_opening WHERE id=?1", [id])?;
    Ok(())
}

// ---------------- 催款单 / 对账函（应收催收闭环） ----------------

fn dunning_now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 催款单明细行（快照：凭证/期初单据 + 未核销额）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DunningItem {
    pub doc_no: String,
    pub date: String,
    pub summary: String,
    pub amount: Money,
}

/// 催款单 / 对账函
#[derive(Clone, Debug, serde::Serialize)]
pub struct Dunning {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    /// ar 催款 / ap 对账函
    pub kind: String,
    pub account: String,
    pub party_code: String,
    pub party_name: String,
    pub amount: Money,
    pub item_count: i64,
    /// draft/sent/settled/cancelled
    pub status: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
    pub sent_at: String,
    pub detail: Vec<DunningItem>,
}

fn map_dunning(r: &rusqlite::Row) -> rusqlite::Result<Dunning> {
    let date: String = r.get(3)?;
    let detail_json: String = r.get(14)?;
    Ok(Dunning {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        kind: r.get(4)?,
        account: r.get(5)?,
        party_code: r.get(6)?,
        party_name: r.get(7)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
        item_count: r.get(9)?,
        status: r.get(10)?,
        memo: r.get(11)?,
        created_by: r.get(12)?,
        created_at: r.get(13)?,
        sent_at: r.get(15)?,
        detail: serde_json::from_str(&detail_json).unwrap_or_default(),
    })
}

const DUN_COLS: &str = "id,no,period,date,kind,account,party_code,party_name,amount,item_count,status,memo,created_by,created_at,detail_json,sent_at";

/// 催款单号：CK + 期间 + 3 位序号（按期间递增，撞号顺延）
pub fn dunning_next_no(db: &Db, period: Period) -> DbResult<String> {
    let mut n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM dunning WHERE period=?1",
        rusqlite::params![period.ymm()],
        |r| r.get(0),
    )?;
    loop {
        n += 1;
        let no = format!("CK{}{:03}", period.ymm(), n);
        let exists: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM dunning WHERE no=?1",
            rusqlite::params![no],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Ok(no);
        }
    }
}

pub fn dunning_list(db: &Db, kind: Option<&str>) -> DbResult<Vec<Dunning>> {
    let mut out = Vec::new();
    match kind.filter(|k| !k.trim().is_empty()) {
        Some(k) => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {DUN_COLS} FROM dunning WHERE kind=?1 ORDER BY date DESC, id DESC"
            ))?;
            let rows = st.query_map(rusqlite::params![k], map_dunning)?;
            for r in rows {
                out.push(r?);
            }
        }
        None => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {DUN_COLS} FROM dunning ORDER BY date DESC, id DESC"
            ))?;
            let rows = st.query_map([], map_dunning)?;
            for r in rows {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

pub fn dunning_get(db: &Db, id: i64) -> DbResult<Option<Dunning>> {
    db.conn()
        .query_row(
            &format!("SELECT {DUN_COLS} FROM dunning WHERE id=?1"),
            rusqlite::params![id],
            map_dunning,
        )
        .optional()
        .map_err(Into::into)
}

/// 生成催款单（快照）：按客商汇总所选往来科目下**未核销分录** + 往来期初影子挂账；
/// 无任何欠款行 → 拒绝。金额与明细服务端计算，不信前端。
#[allow(clippy::too_many_arguments)]
pub fn dunning_create(
    db: &Db,
    kind: &str,
    account: &str,
    party_code: &str,
    party_name: &str,
    date: NaiveDate,
    memo: &str,
    who: &str,
) -> DbResult<Dunning> {
    let kind = match kind.trim() {
        "ar" => "ar",
        "ap" => "ap",
        _ => return Err(fincore::FinError::msg("类型只能是 ar（催款）或 ap（对账函）").into()),
    };
    let party = party_code.trim();
    if party.is_empty() {
        return Err(fincore::FinError::msg("客商编码不能为空").into());
    }
    let account = if account.trim().is_empty() {
        if kind == "ar" {
            "1122"
        } else {
            "2202"
        }
    } else {
        account.trim()
    };
    let upto = Period::from_date(date);
    let mut items: Vec<DunningItem> = Vec::new();
    let mut amount = Money::ZERO;
    for e in open_entries(db, account, upto, false)? {
        let aux = fincore::voucher::AuxRef::from_key(&e.aux_key);
        let p = if kind == "ar" {
            aux.customer.clone()
        } else {
            aux.supplier.clone()
        };
        if p.as_deref() != Some(party) {
            continue;
        }
        let amt = e.open();
        if amt.is_zero() {
            continue;
        }
        amount += amt;
        items.push(DunningItem {
            doc_no: format!("{}-{}", e.word, e.no),
            date: e.date.format("%Y-%m-%d").to_string(),
            summary: e.summary.clone(),
            amount: amt,
        });
    }
    for o in arap_opening_list(db, Some(kind))? {
        if o.party_code != party {
            continue;
        }
        let d = NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
        if Period::from_date(d).ymm() > upto.ymm() {
            continue;
        }
        amount += o.amount;
        items.push(DunningItem {
            doc_no: format!(
                "期初 {}",
                if o.doc_no.is_empty() {
                    format!("#{}", o.id)
                } else {
                    o.doc_no.clone()
                }
            ),
            date: o.doc_date.clone(),
            summary: "往来期初".to_string(),
            amount: o.amount,
        });
    }
    if items.is_empty() {
        return Err(
            fincore::FinError::msg("该客商在所选科目下无未核销单据（或期初），无需催款").into(),
        );
    }
    let no = dunning_next_no(db, upto)?;
    let detail = serde_json::to_string(&items)
        .map_err(|e| fincore::FinError::msg(format!("明细序列化失败：{e}")))?;
    db.conn().execute(
        "INSERT INTO dunning(no,period,date,kind,account,party_code,party_name,amount,item_count,
         status,memo,created_by,created_at,sent_at,detail_json)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'draft',?10,?11,?12,'',?13)",
        rusqlite::params![
            no,
            upto.ymm(),
            date.format("%Y-%m-%d").to_string(),
            kind,
            account,
            party,
            party_name.trim(),
            crate::money_param(amount),
            items.len() as i64,
            memo,
            who,
            dunning_now(),
            detail
        ],
    )?;
    let id = db.conn().last_insert_rowid();
    dunning_get(db, id)?.ok_or_else(|| fincore::FinError::msg("催款单创建失败").into())
}

/// 状态流转：draft → sent/settled/cancelled；sent → settled/cancelled（条件更新防并发）
pub fn dunning_status(db: &Db, id: i64, status: &str) -> DbResult<()> {
    let cur = dunning_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("催款单"))?;
    let allowed = match status {
        "sent" => cur.status == "draft",
        "settled" => cur.status == "draft" || cur.status == "sent",
        "cancelled" => cur.status == "draft" || cur.status == "sent",
        _ => false,
    };
    if !allowed {
        return Err(
            fincore::FinError::msg(format!("状态不能从 {} 变更为 {}", cur.status, status)).into(),
        );
    }
    let ts = dunning_now();
    let n = db.conn().execute(
        "UPDATE dunning SET status=?2,
         sent_at=CASE WHEN ?2='sent' THEN ?3 ELSE sent_at END
         WHERE id=?1 AND status=?4",
        rusqlite::params![id, status, ts, cur.status],
    )?;
    if n == 0 {
        return Err(fincore::FinError::msg("催款单已被处理，请刷新").into());
    }
    Ok(())
}

/// 计提坏账准备：按应收（1122/1221）账龄与默认坏账比例计算**目标余额**，
/// 与账上 1231 现有余额比对，只按差额计提/冲回，辅助核算保留往来对象。
///
/// 这样同期间重复执行、跨期data变化后的再执行都是幂等的：
/// 差额为 0 时返回 `Ok(None)`，不会重复全额计提；应提额减少时自动冲回。
pub fn bad_debt_provision_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    who: &str,
) -> DbResult<Option<i64>> {
    use fincore::engine::aging::{buckets_by_year, default_bad_debt_rates};
    let buckets = buckets_by_year();
    let rates = default_bad_debt_rates();

    let mut lines = Vec::new();
    for acct in ["1122", "1221"] {
        lines.extend(aging(db, acct, period, date, &buckets)?);
    }

    // ⚠️ key 的契约：`aging()` 返回的 `AgingLine.key` 是**往来维度的裸编码**
    // （`C01`），不是 aux_key（`customer=C01`）。它有两个用途：给人看（账龄报表），
    // 和在这里当比对键。所以本函数里 **targets 与 existing 必须归一到同一个空间**。
    //
    // 这里踩过一次：`AgingLine.key` 原来返回整条 aux_key，与 existing 的
    // `e.aux_key` 天然相等；改成裸编码后两者对不上，`existing` 永远读不到
    // 上一轮计提的数 → 同期间重复执行会**反复全额计提**（幂等性断掉，
    // 而且第一轮就报「目标不变时不应重复计提」）。所以 existing 侧也要取出
    // 往来维度。
    //
    // 维度取 Customer：1122 应收账款与 1221 其他应收都挂客户（应付是 2202，
    // 不在本函数范围内）。别用「第一行 kind 是什么」去猜 —— 显式写出来。
    const PARTY: fincore::AuxKind = fincore::AuxKind::Customer;

    // 目标余额按往来对象汇总（同一对象可能同时有应收/其他应收）
    let mut targets: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for l in &lines {
        let prov: Money = l
            .amounts
            .iter()
            .zip(rates.iter())
            .map(|(a, r)| (*a * *r).round2())
            .sum();
        if prov.is_zero() {
            continue;
        }
        *targets.entry(l.key.clone()).or_insert(Money::ZERO) += prov;
    }

    // 差额 = 目标 − 现有（口径与余额表一致：非作废凭证，含未记账草稿）
    let tx = db.write_tx()?;
    let mut existing: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    {
        let mut st = tx.prepare(
            "SELECT e.aux_key, e.credit, e.debit
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE e.account_code='1231' AND e.period <= ?1 AND v.status <> 'void'",
        )?;
        let mut rows = st.query(rusqlite::params![period.ymm()])?;
        while let Some(r) = rows.next()? {
            let raw_key: String = r.get(0)?;
            let code = crate::balances::aux_key_value_of(&raw_key, PARTY);
            if code.is_empty() {
                // 没挂往来单位的 1231 余额（历史上手工记的）不能并进任何客户 ——
                // 归到空串会让某一个客户凭空多出一笔准备，或者反过来。
                continue;
            }
            let c = Money::parse_or_zero(&r.get::<_, String>(1)?);
            let d = Money::parse_or_zero(&r.get::<_, String>(2)?);
            *existing.entry(code).or_insert(Money::ZERO) += c - d;
        }
    }

    let mut keys: Vec<String> = targets.keys().chain(existing.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    let mut deltas: Vec<(String, Money)> = Vec::new();
    let (mut inc, mut dec) = (Money::ZERO, Money::ZERO);
    for k in keys {
        let t = targets.get(&k).copied().unwrap_or(Money::ZERO);
        let e = existing.get(&k).copied().unwrap_or(Money::ZERO);
        let d = t - e;
        if d.is_positive() {
            inc += d;
            deltas.push((k, d));
        } else if d.is_negative() {
            dec += d.abs();
            deltas.push((k, d));
        }
    }
    if inc.is_zero() && dec.is_zero() {
        return Ok(None); // 已按目标计提，无差额可调
    }

    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = fincore::Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = fincore::voucher::VoucherSource::Business;
    v.memo = if dec > inc {
        "冲回坏账准备".to_string()
    } else {
        "计提坏账准备".to_string()
    };
    let mut line_no = 1;
    for (key, d) in &deltas {
        // AuxRef 从「裸编码 + 维度」**现场构造**，不要 `from_key(key)` ——
        // `from_key` 吃的是 `kind=value` 串，喂裸编码会得到空 aux，于是这一行
        // 1231 挂不上往来对象，下一轮 existing 里它的 key 是空串，幂等性又断了。
        let mut aux = fincore::voucher::AuxRef::default();
        aux.set(PARTY, Some(key.clone()));
        if d.is_positive() {
            v.push_entry(fincore::voucher::Entry {
                credit: *d,
                aux,
                ..fincore::voucher::Entry::new(line_no, "1231", "计提坏账准备")
            });
        } else {
            v.push_entry(fincore::voucher::Entry {
                debit: d.abs(),
                aux,
                ..fincore::voucher::Entry::new(line_no, "1231", "冲回坏账准备")
            });
        }
        line_no += 1;
    }
    // 净额腿：增提走借方 6701，冲回走贷方 6701，保证借贷恒等
    if inc > dec {
        v.push_entry(fincore::voucher::Entry {
            debit: inc - dec,
            ..fincore::voucher::Entry::new(line_no, "6701", "计提坏账准备")
        });
    } else if dec > inc {
        v.push_entry(fincore::voucher::Entry {
            credit: dec - inc,
            ..fincore::voucher::Entry::new(line_no, "6701", "冲回坏账准备")
        });
    }
    v.renumber();
    let id = crate::vouchers::save_in(&tx, &mut v)?;
    tx.commit()?;
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use fincore::voucher::{AuxRef, Entry, Voucher};

    /// 同一客户既有凭证又有期初挂账时，账龄**必须合并成一行**、金额相加。
    ///
    /// 回归：凭证行的 key 曾用 `e.aux_key.clone()`（`customer=C01`），期初行用
    /// `o.party_code`（`C01`）—— 两种格式混用，`analyze()` 把同一个客户分成两行
    /// （实测 `["customer=C01", "C01"]`，3000 与 500 分在两行）。界面上看起来是两个
    /// 往来单位，按行做的操作（核销/导出/催款）会漏掉一半。
    ///
    /// 而把 `aux_key.clone()` 放回去**不会有任何其他测试变红** —— 所以钉在这里。
    #[test]
    fn aging_merges_voucher_and_opening_for_same_party() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();

        arap_opening_insert(
            &db,
            &ArapOpening {
                id: 0,
                kind: "ar".into(),
                party_code: "C01".into(),
                party_name: "客户甲".into(),
                doc_no: "XS-1".into(),
                doc_date: "2025-12-01".into(),
                amount: Money::parse("500").unwrap(),
                memo: String::new(),
                created_by: "u".into(),
            },
            "u",
        )
        .unwrap();

        let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("3000").unwrap(),
            credit: Money::ZERO,
            aux: AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "应收")
        });
        v.push_entry(Entry {
            debit: Money::ZERO,
            credit: Money::parse("3000").unwrap(),
            ..Entry::new(2, "1001", "收")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "poster").unwrap();

        let lines = aging(
            &db,
            "1122",
            p,
            NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
            &fincore::engine::aging::buckets_by_days(),
        )
        .unwrap();
        assert_eq!(
            lines.len(),
            1,
            "同一客户必须只有一行：{:?}",
            lines
                .iter()
                .map(|l| (l.key.clone(), l.total.fmt_money()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            lines[0].key, "C01",
            "key 必须是裸客户编码，不能带 customer= 前缀 —— 那是 aux_key 的内部存储格式"
        );
        assert_eq!(
            lines[0].total,
            Money::parse("3500").unwrap(),
            "金额应相加（3000 凭证 + 500 期初），不能分在两行里"
        );
    }

    /// 维度按科目的真实辅助核算取，不按首位猜。
    /// 2203 预收挂的是客户核算、1123 其他应收挂的是供应商 —— 首位规则下
    /// 2203 整页空白（已实锤：5000 余额账龄 0 行），1123 同理。
    #[test]
    fn party_dim_按科目真实辅助核算() {
        let db = tmpdb("party_dim");
        let chart = crate::accounts::chart(&db).unwrap();
        assert_eq!(party_kind_of(&chart, "2203"), fincore::AuxKind::Customer);
        assert_eq!(party_kind_of(&chart, "1123"), fincore::AuxKind::Supplier);
        // 父级科目无辅助标记，退回首位推断（行为不变）
        assert_eq!(party_kind_of(&chart, "1122"), fincore::AuxKind::Customer);
        assert_eq!(party_kind_of(&chart, "2202"), fincore::AuxKind::Supplier);
        // 账套里没有的科目，退回首位推断
        assert_eq!(party_kind_of(&chart, "9999"), fincore::AuxKind::Customer);
    }

    /// 2203 预收账款的账龄必须出行（挂客户核算，收预收时贷记）。
    #[test]
    fn aging_2203_预收按客户出() {
        let db = tmpdb("aging_2203");
        let p = Period::new(2026, 11).unwrap();
        let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(), "收", 1);
        v.push_entry(Entry {
            debit: Money::parse("5000").unwrap(),
            credit: Money::ZERO,
            ..Entry::new(1, "1001", "收预收")
        });
        v.push_entry(Entry {
            debit: Money::ZERO,
            credit: Money::parse("5000").unwrap(),
            aux: AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "2203", "预收甲款")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "poster").unwrap();

        let lines = aging(
            &db,
            "2203",
            p,
            NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
            &fincore::engine::aging::buckets_by_days(),
        )
        .unwrap();
        assert_eq!(lines.len(), 1, "2203 账龄必须出行");
        assert_eq!(lines[0].key, "C01");
        assert_eq!(lines[0].total, Money::parse("5000").unwrap());
    }

    /// 应付账龄（2202）必须仍按 **supplier** 维度出行。
    ///
    /// 回归（我自己差点发货的那一个）：key 的取值函数第一版写死了 customer，
    /// 而应付挂的是 `supplier=` —— 于是整个应付账龄页**一行都不出，且不报错**。
    /// 写死 customer 的版本里，**应收的 3 条测试全部通过**，只有这条会红。
    ///
    /// 「不报错」是它危险的地方：一个只查应收的测试集永远发现不了。
    #[test]
    fn aging_uses_supplier_dim_for_payable() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("2000").unwrap(),
            credit: Money::ZERO,
            aux: AuxRef {
                supplier: Some("S01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "220201", "应付货款")
        });
        v.push_entry(Entry {
            debit: Money::ZERO,
            credit: Money::parse("2000").unwrap(),
            ..Entry::new(2, "1001", "付")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "poster").unwrap();

        let lines = aging(
            &db,
            "2202",
            p,
            NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
            &fincore::engine::aging::buckets_by_days(),
        )
        .unwrap();
        assert_eq!(
            lines.len(),
            1,
            "应付账龄必须出行 —— key 取值写死 customer 会让 2202 整页空白：{:?}",
            lines
                .iter()
                .map(|l| (l.key.clone(), l.total.fmt_money()))
                .collect::<Vec<_>>()
        );
        assert_eq!(lines[0].key, "S01", "key 是**裸供应商编码**");
        // 这笔是借方 220201（预付供应商）—— 翻正后落在预付栏，不进合计。
        // （归一化之前它进合计栏，和"欠供应商多少"混在一起；且整张应付账龄的
        // 合计恒 0、最长账龄恒 0。预付就该在预付栏。）
        assert!(lines[0].total.is_zero());
        assert_eq!(lines[0].credit_total, Money::parse("2000").unwrap());
    }

    /// 没挂客户辅助核算的分录不进往来账龄。
    ///
    /// 否则页面上会出现一行 key 为空的记录：无法解释、也无法操作。
    ///
    /// 场景必须是**自定义的应收科目**（不带 aux 的）—— 预置的 112201/112202
    /// 在科目表里就声明了「核算客户」，凭证校验会直接拒（我第一版想用 112201 挂存货，
    /// 报的是「第 1 行科目 112201 核算客户，必须填写客户」）。所以这条 filter
    /// 守的是**用户自己加的**应收科目那种情形 —— 那才是它能生效的地方。
    ///
    /// 走 `accounts::insert` 而不是手写 INSERT：`account` 表的辅助核算是
    /// `aux_mask` 位掩码且**没有 `leaf` 列**（我照抄别处的 `aux`/`leaf` 写法，
    /// 报的是 `table account has no column named leaf`）。手写 SQL 会随 schema 漂移。
    #[test]
    fn aging_skips_entries_without_party() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        crate::accounts::insert(
            &db,
            &fincore::account::Account {
                code: "112299".into(),
                name: "自定义应收(无辅助)".into(),
                category: fincore::account::AcctCategory::Asset,
                dir: fincore::account::Direction::Debit,
                aux: Default::default(),
                unit: None,
                currency: None,
                has_qty: false,
                is_cash: false,
                is_bank: false,
                cash_flow_item: None,
                bs_item: None,
                pl_item: None,
                disabled: false,
                memo: String::new(),
            },
        )
        .unwrap();
        let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("1000").unwrap(),
            credit: Money::ZERO,
            ..Entry::new(1, "112299", "应收")
        });
        v.push_entry(Entry {
            debit: Money::ZERO,
            credit: Money::parse("1000").unwrap(),
            ..Entry::new(2, "1001", "收")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "poster").unwrap();

        let lines = aging(
            &db,
            "1122",
            p,
            NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
            &fincore::engine::aging::buckets_by_days(),
        )
        .unwrap();
        assert!(
            lines.is_empty(),
            "没挂往来单位的行不该进往来账龄：{:?}",
            lines.iter().map(|l| l.key.clone()).collect::<Vec<_>>()
        );
    }

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_settle_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &crate::tests::test_opts()).unwrap()
    }

    /// 建一张已记账的往来凭证
    fn ar_voucher(
        db: &Db,
        period: Period,
        date: NaiveDate,
        no: i32,
        cust: &str,
        amount: &str,
        dir_debit: bool,
        counterpart: &str,
    ) -> (i64, i64) {
        let mut v = Voucher::new(Period::from_date(date), date, "记", no);
        let a = Money::parse(amount).unwrap();
        let e1 = Entry {
            debit: if dir_debit { a } else { Money::ZERO },
            credit: if dir_debit { Money::ZERO } else { a },
            aux: AuxRef {
                customer: Some(cust.into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "往来")
        };
        let e2 = Entry {
            debit: if dir_debit { Money::ZERO } else { a },
            credit: if dir_debit { a } else { Money::ZERO },
            aux: if counterpart == "100201" {
                AuxRef {
                    bank: Some("BANK01".into()),
                    ..Default::default()
                }
            } else {
                AuxRef::default()
            },
            ..Entry::new(2, counterpart, "往来")
        };
        let _ = period;
        v.push_entry(e1);
        v.push_entry(e2);
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, vid, "poster").unwrap();
        let entries = crate::vouchers::entries_of(db, vid).unwrap();
        (vid, entries[0].id)
    }

    /// 建一张**未记账**（草稿）的往来凭证，返回 (凭证 id, 112201 分录 id)
    fn ar_voucher_draft(
        db: &Db,
        date: NaiveDate,
        no: i32,
        cust: &str,
        amount: &str,
        dir_debit: bool,
        counterpart: &str,
    ) -> (i64, i64) {
        let mut v = Voucher::new(Period::from_date(date), date, "记", no);
        let a = Money::parse(amount).unwrap();
        let e1 = Entry {
            debit: if dir_debit { a } else { Money::ZERO },
            credit: if dir_debit { Money::ZERO } else { a },
            aux: AuxRef {
                customer: Some(cust.into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "往来")
        };
        let e2 = Entry {
            debit: if dir_debit { Money::ZERO } else { a },
            credit: if dir_debit { a } else { Money::ZERO },
            ..Entry::new(2, counterpart, "往来")
        };
        v.push_entry(e1);
        v.push_entry(e2);
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        // 不 post —— 停在草稿
        let entries = crate::vouchers::entries_of(db, vid).unwrap();
        (vid, entries[0].id)
    }

    /// 「含未记账」只读开关：默认看不到草稿，开了能看到且 `posted=false`。
    /// 对标金蝶单据驱动模型——未审核单据在列表里可见，但不参与核销。
    #[test]
    fn open_entries_with_can_show_drafts_readonly() {
        let db = tmpdb("drafts");
        let d1 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
        // 一张已记账 + 一张草稿
        ar_voucher(
            &db,
            Period::new(2026, 1).unwrap(),
            d1,
            1,
            "C01",
            "1000",
            true,
            "600101",
        );
        ar_voucher_draft(&db, d2, 2, "C01", "500", true, "600101");
        let p = Period::new(2026, 1).unwrap();

        // 默认（核销口径）：只有已记账那笔
        let posted = open_entries(&db, "112201", p, false).unwrap();
        assert_eq!(posted.len(), 1, "默认不应返回未记账行");
        assert!(posted[0].posted);
        assert_eq!(posted[0].open(), Money::parse("1000").unwrap());

        // 打开开关：两笔都在，草稿那笔标 posted=false
        let all = open_entries_with(&db, "112201", p, false, false).unwrap();
        assert_eq!(all.len(), 2, "「含未记账」应能看到草稿行");
        let drafts: Vec<&OpenEntry> = all.iter().filter(|e| !e.posted).collect();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].open(), Money::parse("500").unwrap());
        assert!(all.iter().filter(|e| e.posted).count() == 1);
    }

    /// 核销的**被核销方**（from_entry）必须已记账——手工核销的 entry id 由前端传入，
    /// 绕过了 open_entries 的 posted-only 过滤，只能靠 `settle` 自己兜住。
    /// 拿草稿当债权去核销，那张凭证一删核销记录就没了。
    /// 而 to 侧（收款单那张还没记账的凭证）允许是草稿——这正是金蝶的做法。
    #[test]
    fn settle_requires_posted_from_side() {
        let db = tmpdb("unposted_settle");
        let d1 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
        let (_, e_ar_posted) = ar_voucher(
            &db,
            Period::new(2026, 1).unwrap(),
            d1,
            1,
            "C01",
            "1000",
            true,
            "600101",
        );
        // 对方用 1001（库存现金，不核算银行账户），避免 100201 要求 bank 辅助核算
        let (_, e_cash_posted) = ar_voucher(
            &db,
            Period::new(2026, 1).unwrap(),
            d2,
            2,
            "C01",
            "600",
            false,
            "1001",
        );

        // from 侧是草稿 → 拒绝
        let (_, e_ar_draft) = ar_voucher_draft(&db, d2, 3, "C01", "800", true, "600101");
        let err = settle(
            &db,
            e_ar_draft,
            e_ar_posted,
            Money::parse("100").unwrap(),
            "u1",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("已记账"),
            "草稿债权应被拒绝：{err}"
        );
        assert_eq!(
            settled_of(db.conn(), e_ar_draft).unwrap(),
            Money::ZERO,
            "被拒后不得留下核销记录"
        );

        // to 侧是草稿（收款单凭证未记账）→ 允许，金蝶即此语义
        let (_, e_receipt_draft) = ar_voucher_draft(&db, d2, 4, "C01", "400", false, "1001");
        settle(
            &db,
            e_ar_posted,
            e_receipt_draft,
            Money::parse("400").unwrap(),
            "u1",
        )
        .expect("已记账债权 + 未记账收款单应允许核销（对标金蝶「单据审核→核销→生成凭证」）");
        assert_eq!(
            settled_of(db.conn(), e_ar_posted).unwrap(),
            Money::parse("400").unwrap()
        );

        // 两边都已记账 → 照常
        settle(
            &db,
            e_ar_posted,
            e_cash_posted,
            Money::parse("600").unwrap(),
            "u1",
        )
        .unwrap();
        assert_eq!(
            settled_of(db.conn(), e_ar_posted).unwrap(),
            Money::parse("1000").unwrap()
        );
    }

    #[test]
    fn manual_settle_flow() {
        let db = tmpdb("manual");
        let p = Period::new(2026, 1).unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
        let (_, e_ar) = ar_voucher(&db, p, d1, 1, "C01", "1000", true, "600101");
        let (_, e_cash) = ar_voucher(&db, p, d2, 2, "C01", "600", false, "100201");

        settle(&db, e_ar, e_cash, Money::parse("600").unwrap(), "u1").unwrap();
        assert_eq!(
            settled_of(db.conn(), e_ar).unwrap(),
            Money::parse("600").unwrap()
        );

        // 超额核销要拦
        assert!(settle(&db, e_ar, e_cash, Money::parse("500").unwrap(), "u1").is_err());
        // 自己核销自己要拦
        assert!(settle(&db, e_ar, e_ar, Money::parse("10").unwrap(), "u1").is_err());

        let opens = open_entries(&db, "1122", p, false).unwrap();
        assert_eq!(opens.len(), 1); // 收款那条已被核完
        assert_eq!(opens[0].open(), Money::parse("400").unwrap());

        // 取消核销后恢复
        let recs = list_for_entry(&db, e_ar).unwrap();
        unsettle(&db, recs[0].id).unwrap();
        assert_eq!(settled_of(db.conn(), e_ar).unwrap(), Money::ZERO);
    }

    #[test]
    fn entry_rewrite_clears_dangling_refs() {
        let db = tmpdb("dangling");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let (vid, e_ar) = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101");
        // 收款凭证（贷 112201），提供同科目的核销方分录
        let (_, e_pay) = ar_voucher(&db, p, d, 2, "C01", "600", false, "100201");

        settle(&db, e_ar, e_pay, Money::parse("600").unwrap(), "u1").unwrap();
        // 银行勾对也挂在将被重写的这张凭证的分录上
        db.conn()
            .execute(
                "INSERT INTO bank_statement(period,account_code,biz_date,summary,settle_no,debit,credit,balance,entry_id,matched_at,matched_by)
                 VALUES(?1,'112201','2026-01-05','测试','','1000','0','0',?2,'now','u')",
                rusqlite::params![p.ymm(), e_ar],
            )
            .unwrap();
        assert_eq!(list_for_entry(&db, e_ar).unwrap().len(), 1);

        // 反记账后重写凭证（save 会整表 DELETE 旧分录再插入）：
        // 核销记录应级联删除、银行勾对应置空，不能留下指向已删分录的悬空引用
        crate::vouchers::unpost(&db, vid).unwrap();
        let mut v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        v.memo = "改过摘要".to_string();
        crate::vouchers::save(&db, &mut v).unwrap();

        assert_eq!(
            list_for_entry(&db, e_ar).unwrap().len(),
            0,
            "重写分录后核销记录不应悬空"
        );
        let linked: Option<i64> = db
            .conn()
            .query_row(
                "SELECT entry_id FROM bank_statement WHERE period=?1 AND account_code='112201'",
                rusqlite::params![p.ymm()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(linked.is_none(), "银行勾对应被置空");
    }

    #[test]
    fn cross_account_rejected() {
        let db = tmpdb("cross");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let (_, e1) = ar_voucher(&db, p, d, 1, "C01", "100", true, "600101");
        let (_, e2) = ar_voucher(&db, p, d, 2, "C02", "100", false, "100201");
        // 往来单位不同
        assert!(settle(&db, e1, e2, Money::parse("100").unwrap(), "u").is_err());
    }

    /// 带差异（抹零）的核销：应收 1000，实收 998，差 2 记入差异科目，
    /// 被核销方**全额结清**，差异落到损益，账龄不挂尾。
    #[test]
    fn settle_with_diff_writes_off_and_clears() {
        let db = tmpdb("diff");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let (_, e_ar) = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101");
        let (_, e_cash) = ar_voucher(&db, p, d, 2, "C01", "998", false, "100201");

        let r = settle_with_diff(
            &db,
            e_ar,
            e_cash,
            Money::parse("998").unwrap(),
            Money::parse("2").unwrap(),
            "660305",
            "u",
        )
        .unwrap();
        let dv_id = r.diff_voucher_id.expect("有差异必须生成差异凭证");
        // 应收全额结清（998 实收 + 2 差异）
        let left = open_entries(&db, "1122", p, false)
            .unwrap()
            .iter()
            .fold(Money::ZERO, |a, e| a + e.open());
        assert_eq!(left, Money::ZERO, "应收应被全额结清");
        // 差异凭证：借 660305 / 贷 112201（带同往来单位），金额 2
        let dv = crate::vouchers::get(&db, dv_id).unwrap().unwrap();
        assert_eq!(dv.entries.len(), 2);
        let debit = dv.entries.iter().find(|e| e.debit > Money::ZERO).unwrap();
        assert_eq!(debit.account_code, "660305");
        assert_eq!(debit.debit, Money::parse("2").unwrap());
        let credit = dv.entries.iter().find(|e| e.credit > Money::ZERO).unwrap();
        assert_eq!(credit.account_code, "112201");
        assert_eq!(
            credit.aux.customer.as_deref(),
            Some("C01"),
            "差异分录必须带同往来单位，否则核销对不上"
        );

        // 差异为 0：退化成普通核销，不生成差异凭证（凭证号跳开，避免与差异凭证撞号）
        let (_, e_ar2) = ar_voucher(&db, p, d, 10, "C02", "500", true, "600101");
        let (_, e_cash2) = ar_voucher(&db, p, d, 11, "C02", "500", false, "100201");
        let r0 = settle_with_diff(
            &db,
            e_ar2,
            e_cash2,
            Money::parse("500").unwrap(),
            Money::ZERO,
            "",
            "u",
        )
        .unwrap();
        assert!(r0.diff_voucher_id.is_none(), "无差异不应生成凭证");
    }

    /// 差异核销的守卫：负差异 / 空或不存在或同往来的差异科目 / 超额，全部拦下且整单回滚。
    #[test]
    fn settle_with_diff_guards() {
        let db = tmpdb("diffguard");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let (_, e_ar) = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101");
        let (_, e_cash) = ar_voucher(&db, p, d, 2, "C01", "998", false, "100201");
        let (a, dv) = (Money::parse("998").unwrap(), Money::parse("2").unwrap());
        assert!(settle_with_diff(
            &db,
            e_ar,
            e_cash,
            a,
            Money::parse("-1").unwrap(),
            "660305",
            "u"
        )
        .is_err());
        assert!(settle_with_diff(&db, e_ar, e_cash, a, dv, "", "u").is_err());
        assert!(settle_with_diff(&db, e_ar, e_cash, a, dv, "999999", "u").is_err());
        assert!(settle_with_diff(&db, e_ar, e_cash, a, dv, "112201", "u").is_err());
        // 998 + 100 = 1098 > 未核销 1000
        assert!(settle_with_diff(
            &db,
            e_ar,
            e_cash,
            a,
            Money::parse("100").unwrap(),
            "660305",
            "u"
        )
        .is_err());
        // 全部失败：一条核销记录都不该留下（整单回滚，不残留差异凭证与核销）
        assert!(
            list(&db, "1122").unwrap().is_empty(),
            "失败应整单回滚，不得残留核销记录"
        );
    }

    /// 建一张已记账的应付凭证：贷 220201（供应商），借 counterpart。返回 (凭证 id, 220201 分录 id)。
    fn ap_voucher(
        db: &Db,
        date: NaiveDate,
        no: i32,
        sup: &str,
        amount: &str,
        counterpart: &str,
    ) -> (i64, i64) {
        let mut v = Voucher::new(Period::from_date(date), date, "记", no);
        let a = Money::parse(amount).unwrap();
        v.push_entry(Entry {
            debit: a,
            ..Entry::new(1, counterpart, "付款")
        });
        v.push_entry(Entry {
            credit: a,
            aux: AuxRef {
                supplier: Some(sup.into()),
                ..Default::default()
            },
            ..Entry::new(2, "220201", "应付挂账")
        });
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, vid, "u").unwrap();
        let ids = crate::vouchers::entries_of(db, vid).unwrap();
        (vid, ids[1].id)
    }

    /// 代收代付：A 打来的钱清的是 B 的货款——**同科目（应收）跨往来单位**。
    ///
    /// 普通核销要求同科目同辅助，这里两边客户不同（B vs A），只能靠过渡凭证接上：
    /// 过渡 `借 应收-A / 贷 应收-B`，与 A 那张收款合起来净额就是 `借 银行 / 贷 应收-B`。
    #[test]
    fn settle_cross_跨往来单位代收代付() {
        let db = tmpdb("cross_pay");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        // B 的货款：借 应收-B 1000
        let e_b = ar_voucher(&db, p, d, 1, "B01", "1000", true, "600101").1;
        // A 打来的钱记在 A 名下：贷 应收-A 1000
        let e_a = ar_voucher(&db, p, d, 2, "A01", "1000", false, "1001").1;

        let r = settle_cross(&db, e_b, e_a, Money::parse("1000").unwrap(), d, "u").unwrap();
        assert_eq!(r.settle_ids.len(), 2, "两侧各落一条核销记录");
        let vid = r.bridge_voucher_id.expect("跨往来单位必须出过渡凭证");

        // 过渡凭证：借 应收-A / 贷 应收-B
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 2);
        let debit = v.entries.iter().find(|e| e.debit > Money::ZERO).unwrap();
        assert_eq!(debit.account_code, "112201");
        assert_eq!(debit.aux.customer.as_deref(), Some("A01"));
        let credit = v.entries.iter().find(|e| e.credit > Money::ZERO).unwrap();
        assert_eq!(credit.account_code, "112201");
        assert_eq!(credit.aux.customer.as_deref(), Some("B01"));

        // 两边同时清
        let left = open_entries(&db, "1122", p, false).unwrap();
        assert!(
            left.is_empty(),
            "互抵后应收应全清：{:?}",
            left.iter()
                .map(|e| (e.entry_id, e.open().fmt_money()))
                .collect::<Vec<_>>()
        );
    }

    /// 三方抵账：客户 A 欠我们、我们欠供应商 B，A 直接抵掉 B——**跨科目**。
    /// 过渡 `借 应付-B / 贷 应收-A`（与往来对冲单同口径，只是这里按分录精确挑）。
    #[test]
    fn settle_cross_跨科目三方抵账() {
        let db = tmpdb("cross_three");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let e_ar = ar_voucher(&db, p, d, 1, "A01", "1000", true, "600101").1;
        let e_ap = ap_voucher(&db, d, 2, "S01", "600", "1001").1;

        let r = settle_cross(&db, e_ar, e_ap, Money::parse("600").unwrap(), d, "u").unwrap();
        let vid = r.bridge_voucher_id.expect("跨科目必须出过渡凭证");
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        let debit = v.entries.iter().find(|e| e.debit > Money::ZERO).unwrap();
        assert_eq!(debit.account_code, "220201");
        assert_eq!(debit.aux.supplier.as_deref(), Some("S01"));
        let credit = v.entries.iter().find(|e| e.credit > Money::ZERO).unwrap();
        assert_eq!(credit.account_code, "112201");
        assert_eq!(credit.aux.customer.as_deref(), Some("A01"));

        // 应收剩 400、应付全清
        let ar = open_entries(&db, "1122", p, false)
            .unwrap()
            .iter()
            .fold(Money::ZERO, |a, e| a + e.open());
        assert_eq!(ar, Money::parse("400").unwrap(), "三方抵账后应收应剩 400");
        let ap = open_entries(&db, "2202", p, false).unwrap();
        assert!(ap.is_empty(), "应付应被全额冲销，实际 {} 笔", ap.len());
    }

    /// 同科目同辅助：本就是普通核销，不该出过渡凭证（省一张多余凭证）。
    #[test]
    fn settle_cross_同往来不需要过渡凭证() {
        let db = tmpdb("cross_same");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let e_ar = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101").1;
        let e_pre = ar_voucher(&db, p, d, 2, "C01", "300", false, "1001").1;

        let r = settle_cross(&db, e_ar, e_pre, Money::parse("300").unwrap(), d, "u").unwrap();
        assert!(r.bridge_voucher_id.is_none(), "同往来不该生成过渡凭证");
        assert_eq!(r.settle_ids.len(), 1);
        let left = open_entries(&db, "1122", p, false)
            .unwrap()
            .iter()
            .fold(Money::ZERO, |a, e| a + e.open());
        assert_eq!(left, Money::parse("700").unwrap(), "应收应剩 700");
    }

    /// 互抵守卫：自核自 / 零额 / 方向反 / 超额 / 草稿，全部拦下，且失败不残留任何东西。
    #[test]
    fn settle_cross_guards() {
        let db = tmpdb("crossguard");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let e_b = ar_voucher(&db, p, d, 1, "B01", "1000", true, "600101").1;
        let e_a = ar_voucher(&db, p, d, 2, "A01", "1000", false, "1001").1;
        let e_dr = ar_voucher(&db, p, d, 3, "C01", "1000", true, "600101").1;
        let e_cr = ar_voucher(&db, p, d, 4, "D01", "1000", false, "1001").1;
        let full = Money::parse("1000").unwrap();
        let over = Money::parse("1000.01").unwrap();

        // 自核自
        assert!(settle_cross(&db, e_b, e_b, full, d, "u").is_err());
        // 零额 / 负额
        assert!(settle_cross(&db, e_b, e_a, Money::ZERO, d, "u").is_err());
        // 方向反了：把贷方当借方、借方当贷方
        assert!(settle_cross(&db, e_a, e_b, full, d, "u").is_err());
        // 两边同向（都是借方 / 都是贷方）
        assert!(settle_cross(&db, e_b, e_dr, full, d, "u").is_err());
        assert!(settle_cross(&db, e_a, e_cr, full, d, "u").is_err());
        // 超额
        assert!(settle_cross(&db, e_b, e_a, over, d, "u").is_err());
        // 草稿（未记账）不能互抵
        let (_, e_draft) = ar_voucher_draft(&db, d, 9, "E01", "1000", true, "600101");
        assert!(settle_cross(&db, e_draft, e_a, full, d, "u").is_err());

        // 全部失败：一条核销记录都不该留下，也不该留下过渡凭证
        assert!(
            list(&db, "1122").unwrap().is_empty(),
            "失败应整单回滚，不得残留核销记录"
        );
        let draft_cnt: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM voucher WHERE memo LIKE '往来互抵%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(draft_cnt, 0, "失败不得留下过渡凭证");
    }

    #[test]
    fn auto_settle_exact_then_fifo() {
        let db = tmpdb("auto");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 两笔应收各 500，一笔收款 800
        ar_voucher(&db, p, d, 1, "C01", "500", true, "600101");
        ar_voucher(&db, p, d, 2, "C01", "500", true, "600101");
        ar_voucher(&db, p, d, 3, "C01", "800", false, "100201");

        let res = auto_settle(&db, "1122", p, Money::parse("0.01").unwrap(), "u").unwrap();
        assert!(res.pairs >= 2, "{res:?}");
        let opens = open_entries(&db, "1122", p, false).unwrap();
        let total: Money = opens.iter().map(|o| o.open()).sum();
        assert_eq!(total, Money::parse("200").unwrap());
    }

    /// 回归：尾数抹平失败时不得计数。
    ///
    /// 旧写法 `let _ = settle(...); res.pairs += 1; res.written_off += 1;` 把错误丢掉
    /// 照样计数，界面显示"已核销 N 笔 / 金额 X"，库里却没有那条核销记录
    /// （本例 X 甚至超过实际核销总额）。
    #[test]
    fn auto_settle_does_not_count_failed_writeoff() {
        let db = tmpdb("auto_wo");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 应收 100；收款 99.99；再一笔 0.005 的收款（尾数比应收的 0.01 尾差还小）
        let (_, e_ar) = ar_voucher(&db, p, d, 1, "C01", "100", true, "600101");
        ar_voucher(&db, p, d, 2, "C01", "99.99", false, "100201");
        ar_voucher(&db, p, d, 3, "C01", "0.005", false, "100201");

        let res = auto_settle(&db, "1122", p, Money::parse("0.01").unwrap(), "u").unwrap();
        assert_eq!(res.written_off, 0, "抹平失败不得计入 written_off：{res:?}");
        assert!(
            res.warnings.iter().any(|w| w.contains("抹平失败")),
            "抹平失败必须进 warnings：{res:?}"
        );
        // 计数必须与库里的核销记录一致
        let really = settled_of(db.conn(), e_ar).unwrap();
        assert_eq!(
            res.amount, really,
            "统计金额必须等于实际核销额（回归前会把失败的抹平也算进去）：{res:?}"
        );
        assert_eq!(
            res.pairs,
            list(&db, "1122").unwrap().len(),
            "核销笔数必须等于实际记录数：{res:?}"
        );
    }

    /// settled_map 参数曾经是 [id1,id1,id2,id2…] 交错排列，而 SQL 里两个 IN
    /// 各吃连续 N 个 —— from 侧永远漏后一半、to 侧永远漏前一半。
    /// 两行以内碰巧全对（N=1 时两边吃到同一个 id），三行以上已配平的分录
    /// 在循环视图里还是"未清"，自动核销第二轮就去重复配，被超额校验拦下
    /// （"核销金额 24700.00 超过核销方未核销额 0.00"）。
    /// 触发条件是 to 侧分录的 id 排在第一位 —— 现实里就是"先收款、后开票"
    /// （氧化厂演示账：11-03 收 68000，11-04 才开 92700 发票），稳定复现。
    #[test]
    fn settled_map_三行以上双向都计数() {
        let db = tmpdb("settle_map_3rows");
        let p = Period::new(2026, 1).unwrap();
        // 先收款 68000、再开票 92700、再收款 50000，同客户
        let (_, e_r68) = ar_voucher(
            &db,
            p,
            NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(),
            1,
            "C01",
            "68000",
            false,
            "1001",
        );
        let (_, e_inv) = ar_voucher(
            &db,
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            2,
            "C01",
            "92700",
            true,
            "600101",
        );
        let (_, e_r50) = ar_voucher(
            &db,
            p,
            NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
            3,
            "C01",
            "50000",
            false,
            "1001",
        );

        // 手工配第一对：发票 → 68000 收款
        settle(&db, e_inv, e_r68, Money::parse("68000").unwrap(), "u").unwrap();
        // 三个 id 必须都有数：修之前 to 侧的 e_r68 会漏掉（还是 0）
        let sm = settled_map(&db, &[e_r68, e_inv, e_r50]).unwrap();
        assert_eq!(sm.get(&e_inv), Some(&Money::parse("68000").unwrap()));
        assert_eq!(sm.get(&e_r68), Some(&Money::parse("68000").unwrap()));
        assert!(!sm.contains_key(&e_r50));

        // 自动核销必须一口气吃完发票剩下的 24700，而不是报错
        let res = auto_settle(&db, "112201", p, Money::ZERO, "u").unwrap();
        assert_eq!(res.amount, Money::parse("24700").unwrap());
        assert_eq!(
            settled_of(db.conn(), e_inv).unwrap(),
            Money::parse("92700").unwrap(),
            "发票应全额配平"
        );
    }

    #[test]
    fn aging_report() {
        let db = tmpdb("aging");
        let p = Period::new(2026, 3).unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 3, 20).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        ar_voucher(&db, p, d1, 1, "C01", "1000", true, "600101");
        ar_voucher(&db, p, d2, 2, "C01", "2000", true, "600101");

        let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        let b = fincore::engine::aging::buckets_by_days();
        let lines = aging(&db, "1122", p, as_of, &b).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].total, Money::parse("3000").unwrap());
        assert_eq!(lines[0].amounts[0], Money::parse("1000").unwrap()); // 11 天
                                                                        // 2025-06-01 → 2026-03-31 约 303 天，落在 181-365 档
        assert_eq!(lines[0].amounts[4], Money::parse("2000").unwrap());
    }

    #[test]
    fn bad_debt_provision_generates_voucher() {
        let db = tmpdb("baddebt");
        let p = Period::new(2026, 3).unwrap();
        // 一笔 3 年以上的老应收 10000，一笔 1 年以内 2000
        ar_voucher(
            &db,
            p,
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            1,
            "C01",
            "2000",
            true,
            "600101",
        );
        ar_voucher(
            &db,
            p,
            NaiveDate::from_ymd_opt(2023, 1, 5).unwrap(),
            2,
            "C01",
            "10000",
            true,
            "600101",
        );

        let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        let id = bad_debt_provision_voucher(&db, p, as_of, "u1")
            .unwrap()
            .expect("应生成坏账准备凭证");
        let v = crate::vouchers::get(&db, id).unwrap().unwrap();
        // 借 资产减值损失 / 贷 坏账准备，借贷平衡
        assert!(v.balanced());
        assert!(v.entries.iter().any(|e| e.account_code == "6701"));
        assert!(v.entries.iter().any(|e| e.account_code == "1231"));
        // 3-4 年老账 10000 × 30% + 1 年内新账 2000 × 5% = 3000 + 100 = 3100
        let credit: Money = v
            .entries
            .iter()
            .filter(|e| e.account_code == "1231")
            .map(|e| e.credit)
            .sum();
        assert_eq!(credit, Money::parse("3100").unwrap());
        assert_eq!(v.source, fincore::voucher::VoucherSource::Business);
    }

    #[test]
    fn bad_debt_provision_adjusts_target_and_reverses() {
        let db = tmpdb("baddebt_diff");
        let p1 = Period::new(2026, 3).unwrap();
        let (_, e_new) = ar_voucher(
            &db,
            p1,
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            1,
            "C01",
            "2000",
            true,
            "600101",
        );
        let (_, e_old) = ar_voucher(
            &db,
            p1,
            NaiveDate::from_ymd_opt(2023, 1, 5).unwrap(),
            2,
            "C01",
            "10000",
            true,
            "600101",
        );
        let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        let first = bad_debt_provision_voucher(&db, p1, as_of, "u1")
            .unwrap()
            .expect("首次应计提");
        // 计提出来的 1231 分录**必须挂上往来对象** —— 这是幂等性的机械保证：
        // 下一轮 existing 从 `e.aux_key` 读回上一轮的数，aux 没挂上就取不到，
        // 于是每轮都当成「还没提过」而全额重复计提（而且不报错）。
        //
        // 「为什么这条重要」不写在断言消息里，因为上面那句注释就是原因；
        // 这里只钉住事实：aux 存在，且值是**裸编码经 aux_key_frag 还原**的
        // `customer=C01` 形式，而不是反过来（`from_key` 吃裸编码会得到空 aux）。
        {
            let v = crate::vouchers::get(&db, first).unwrap().unwrap();
            let e1231 = v
                .entries
                .iter()
                .find(|e| e.account_code == "1231")
                .expect("应有 1231 分录");
            assert_eq!(
                e1231
                    .aux
                    .get(fincore::AuxKind::Customer)
                    .map(|s| s.as_str()),
                Some("C01"),
                "计提的 1231 分录必须挂客户辅助核算，否则下一轮读不回来、幂等性断裂"
            );
        }
        // 目标未变：重复执行差额为 0，不再生成
        assert!(
            bad_debt_provision_voucher(&db, p1, as_of, "u1")
                .unwrap()
                .is_none(),
            "目标不变时不应重复计提"
        );

        // 期后全部收回 → 应提额降为 0，应自动冲回
        let p2 = Period::new(2026, 4).unwrap();
        let (_, e_pay) = ar_voucher(
            &db,
            p2,
            NaiveDate::from_ymd_opt(2026, 4, 10).unwrap(),
            3,
            "C01",
            "12000",
            false,
            "100201",
        );
        settle(&db, e_new, e_pay, Money::parse("2000").unwrap(), "u1").unwrap();
        settle(&db, e_old, e_pay, Money::parse("10000").unwrap(), "u1").unwrap();

        let rev = bad_debt_provision_voucher(
            &db,
            p2,
            NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
            "u1",
        )
        .unwrap()
        .expect("应生成冲回凭证");
        let v = crate::vouchers::get(&db, rev).unwrap().unwrap();
        assert!(v.balanced());
        let debit: Money = v
            .entries
            .iter()
            .filter(|e| e.account_code == "1231")
            .map(|e| e.debit)
            .sum();
        let credit: Money = v
            .entries
            .iter()
            .filter(|e| e.account_code == "6701")
            .map(|e| e.credit)
            .sum();
        assert_eq!(debit, Money::parse("3100").unwrap());
        assert_eq!(credit, Money::parse("3100").unwrap());
        // 已到目标余额：再执行不再生成
        assert!(bad_debt_provision_voucher(
            &db,
            p2,
            NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
            "u1"
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn bad_debt_provision_skip_when_none() {
        let db = tmpdb("baddebt0");
        let p = Period::new(2026, 3).unwrap();
        let r =
            bad_debt_provision_voucher(&db, p, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(), "u1")
                .unwrap();
        assert!(r.is_none(), "无应收时不应生成凭证");
    }
}
