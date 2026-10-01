//! 期末自动化：汇率、期末调汇、自动转账、月度结账检查清单

use fincore::voucher::{AuxRef, Entry, Voucher, VoucherSource};
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{balances, Db, DbResult};

// ===========================================================================
// 汇率
// ===========================================================================

/// 汇率表一行：1 单位外币 = rate 单位本位币
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FxRate {
    pub period: Period,
    pub currency: String,
    pub rate: Money,
}

pub fn fx_list(db: &Db, period: Period) -> DbResult<Vec<FxRate>> {
    let mut st = db.conn().prepare(
        "SELECT period, currency, rate FROM fx_rate WHERE period=?1 ORDER BY currency",
    )?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], |r| {
            Ok(FxRate {
                period: Period::from_ymm(r.get(0)?),
                currency: r.get(1)?,
                rate: Money::parse_or_zero(&r.get::<_, String>(2)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn fx_get(db: &Db, period: Period, currency: &str) -> DbResult<Option<FxRate>> {
    db.conn()
        .query_row(
            "SELECT period, currency, rate FROM fx_rate WHERE period=?1 AND currency=?2",
            rusqlite::params![period.ymm(), currency],
            |r| {
                Ok(FxRate {
                    period: Period::from_ymm(r.get(0)?),
                    currency: r.get(1)?,
                    rate: Money::parse_or_zero(&r.get::<_, String>(2)?),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub fn fx_set(db: &Db, period: Period, currency: &str, rate: Money) -> DbResult<()> {
    if rate <= Money::ZERO {
        return Err(fincore::FinError::msg("汇率必须大于零").into());
    }
    db.conn().execute(
        "INSERT INTO fx_rate(period,currency,rate) VALUES(?1,?2,?3)
         ON CONFLICT(period,currency) DO UPDATE SET rate=excluded.rate",
        rusqlite::params![period.ymm(), currency, crate::exact_param(rate)],
    )?;
    Ok(())
}

pub fn fx_delete(db: &Db, period: Period, currency: &str) -> DbResult<()> {
    db.conn().execute(
        "DELETE FROM fx_rate WHERE period=?1 AND currency=?2",
        rusqlite::params![period.ymm(), currency],
    )?;
    Ok(())
}

/// 本期用到但没维护汇率的外币币种
pub fn fx_missing(db: &Db, period: Period) -> DbResult<Vec<String>> {
    let mut st = db.conn().prepare(
        "SELECT DISTINCT e.currency FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
         WHERE v.period=?1 AND e.currency IS NOT NULL AND e.currency <> ''
           AND e.currency NOT IN (SELECT currency FROM fx_rate WHERE period=?1)
         ORDER BY e.currency",
    )?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ===========================================================================
// 期末调汇
// ===========================================================================

/// 一条调汇差额
#[derive(Clone, Debug)]
pub struct FxAdjLine {
    pub account_code: String,
    pub aux_key: String,
    pub currency: String,
    /// 外币余额
    pub foreign: Money,
    /// 账面本位币余额
    pub book: Money,
    /// 按期末汇率重算后的本位币余额
    pub restated: Money,
    /// 差额（正=调增本位币余额）
    pub diff: Money,
}

/// 汇兑损益科目
pub const FX_GAIN_ACCOUNT: &str = "660304";

/// 试算期末调汇（不生成凭证）
pub fn fx_calc(db: &Db, period: Period) -> DbResult<Vec<FxAdjLine>> {
    let rates = fx_list(db, period)?;
    if rates.is_empty() {
        return Ok(Vec::new());
    }
    // 取所有外币分录，按 科目+辅助+币种 归集
    let mut st = db.conn().prepare(
        "SELECT e.account_code, e.aux_key, e.currency, e.debit, e.credit, e.rate, e.amount_for
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
         WHERE v.period <= ?1 AND v.status != 'void'
           AND e.currency IS NOT NULL AND e.currency <> ''
           AND (e.debit <> '0' OR e.credit <> '0')
         ORDER BY e.account_code, e.aux_key, e.currency",
    )?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                Money::parse_or_zero(&r.get::<_, String>(3)?),
                Money::parse_or_zero(&r.get::<_, String>(4)?),
                r.get::<_, Option<String>>(5)?.map(|s| Money::parse_or_zero(&s)),
                r.get::<_, Option<String>>(6)?.map(|s| Money::parse_or_zero(&s)),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // key -> (foreign, book)
    let mut map: std::collections::BTreeMap<(String, String, String), (Money, Money)> =
        std::collections::BTreeMap::new();
    for (acct, aux, cur, d, c, _rate, amt_for) in rows {
        let signed = d - c;
        let foreign = match amt_for {
            Some(f) => if signed.is_negative() { f.negated() } else { f },
            None => Money::ZERO,
        };
        let e = map.entry((acct, aux, cur)).or_insert((Money::ZERO, Money::ZERO));
        e.0 += foreign;
        e.1 += signed;
    }

    let mut out = Vec::new();
    for ((acct, aux, cur), (foreign, book)) in map {
        let rate = match rates.iter().find(|r| r.currency == cur) {
            Some(r) => r.rate,
            None => continue,
        };
        if foreign.is_zero() {
            continue;
        }
        let restated = (foreign * rate).round2();
        let diff = restated - book;
        if diff.is_zero() {
            continue;
        }
        out.push(FxAdjLine {
            account_code: acct,
            aux_key: aux,
            currency: cur,
            foreign,
            book,
            restated,
            diff,
        });
    }
    Ok(out)
}

/// 生成期末调汇凭证。差额计入 6061 汇兑损益。
pub fn fx_adjust(
    db: &Db,
    period: Period,
    date: chrono::NaiveDate,
    gain_account: &str,
    who: &str,
) -> DbResult<Option<i64>> {
    let lines = fx_calc(db, period)?;
    if lines.is_empty() {
        return Ok(None);
    }
    let no = crate::vouchers::next_no(db, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::FxAdjust;
    v.memo = "期末调汇".to_string();
    let mut i = 1i32;
    let mut total = Money::ZERO;
    for l in &lines {
        v.push_entry(Entry {
            debit: if l.diff.is_positive() { l.diff } else { Money::ZERO },
            credit: if l.diff.is_negative() { l.diff.abs() } else { Money::ZERO },
            aux: AuxRef::from_key(&l.aux_key),
            currency: Some(l.currency.clone()),
            amount_for: Some(l.foreign.abs()),
            ..Entry::new(i, &l.account_code, "期末调汇")
        });
        total += l.diff;
        i += 1;
    }
    v.push_entry(Entry {
        debit: if total.is_negative() { total.abs() } else { Money::ZERO },
        credit: if total.is_positive() { total } else { Money::ZERO },
        ..Entry::new(i, gain_account, "汇兑损益")
    });
    v.renumber();
    let id = crate::vouchers::save(db, &mut v)?;
    Ok(Some(id))
}

// ===========================================================================
// 自动转账
// ===========================================================================

/// 取数口径
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SrcKind {
    /// 期初余额
    Begin,
    /// 本期借方发生额
    Debit,
    /// 本期贷方发生额
    Credit,
    /// 期末余额
    End,
}

impl SrcKind {
    pub fn label(self) -> &'static str {
        match self {
            SrcKind::Begin => "期初余额",
            SrcKind::Debit => "本期借方发生额",
            SrcKind::Credit => "本期贷方发生额",
            SrcKind::End => "期末余额",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            SrcKind::Begin => "begin",
            SrcKind::Debit => "debit",
            SrcKind::Credit => "credit",
            SrcKind::End => "end",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "begin" => SrcKind::Begin,
            "debit" => SrcKind::Debit,
            "credit" => SrcKind::Credit,
            _ => SrcKind::End,
        }
    }
    pub const ALL: &'static [SrcKind] = &[
        SrcKind::Begin,
        SrcKind::Debit,
        SrcKind::Credit,
        SrcKind::End,
    ];
}

/// 方向
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryDir {
    Debit,
    Credit,
    /// 自动：按科目余额方向取
    Auto,
}

impl EntryDir {
    pub fn label(self) -> &'static str {
        match self {
            EntryDir::Debit => "借",
            EntryDir::Credit => "贷",
            EntryDir::Auto => "自动",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            EntryDir::Debit => "debit",
            EntryDir::Credit => "credit",
            EntryDir::Auto => "auto",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "debit" => EntryDir::Debit,
            "credit" => EntryDir::Credit,
            _ => EntryDir::Auto,
        }
    }
    pub const ALL: &'static [EntryDir] = &[EntryDir::Debit, EntryDir::Credit, EntryDir::Auto];
}

/// 自动转账规则
///
/// 一条规则 = 一张两行凭证：
/// `dst_account` 记 `dst_dir` 方向，金额 A；对方科目记反方向，金额 A。
/// 对方科目留空时取 `src_account`（把来源科目结平，用于结转）；
/// 填了对方科目则来源科目只取数不动（用于计提）。
#[derive(Clone, Debug)]
pub struct AutoTransfer {
    pub id: i64,
    pub name: String,
    pub sort: i32,
    pub active: bool,
    pub src_account: String,
    pub src_aux: String,
    pub src_kind: SrcKind,
    pub src_dir: EntryDir,
    pub ratio: Money,
    /// true=按比例，false=固定金额
    pub ratio_mode_is_ratio: bool,
    pub dst_account: String,
    pub dst_aux: String,
    pub dst_dir: EntryDir,
    pub offset_account: String,
    pub summary: String,
    pub memo: String,
}

fn map_at(r: &rusqlite::Row) -> rusqlite::Result<AutoTransfer> {
    Ok(AutoTransfer {
        id: r.get(0)?,
        name: r.get(1)?,
        sort: r.get(2)?,
        active: r.get::<_, i64>(3)? != 0,
        src_account: r.get(4)?,
        src_aux: r.get(5)?,
        src_kind: SrcKind::parse(&r.get::<_, String>(6)?),
        src_dir: EntryDir::parse(&r.get::<_, String>(7)?),
        ratio: Money::parse_or_zero(&r.get::<_, String>(8)?),
        ratio_mode_is_ratio: r.get::<_, String>(9)? != "amount",
        dst_account: r.get(10)?,
        dst_aux: r.get(11)?,
        dst_dir: EntryDir::parse(&r.get::<_, String>(12)?),
        offset_account: r.get(13)?,
        summary: r.get(14)?,
        memo: r.get(15)?,
    })
}

const AT_COLS: &str = "id,name,sort,active,src_account,src_aux,src_kind,src_dir,ratio,ratio_mode,
     dst_account,dst_aux,dst_dir,offset_account,summary,memo";

pub fn at_list(db: &Db) -> DbResult<Vec<AutoTransfer>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {AT_COLS} FROM auto_transfer ORDER BY sort, id"))?;
    let rows = st.query_map([], map_at)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn at_get(db: &Db, id: i64) -> DbResult<Option<AutoTransfer>> {
    db.conn()
        .query_row(
            &format!("SELECT {AT_COLS} FROM auto_transfer WHERE id=?1"),
            rusqlite::params![id],
            map_at,
        )
        .optional()
        .map_err(Into::into)
}

pub fn at_insert(db: &Db, r: &AutoTransfer) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO auto_transfer(name,sort,active,src_account,src_aux,src_kind,src_dir,ratio,
            ratio_mode,dst_account,dst_aux,dst_dir,offset_account,summary,memo)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        rusqlite::params![
            r.name,
            r.sort,
            r.active as i64,
            r.src_account,
            r.src_aux,
            r.src_kind.code(),
            r.src_dir.code(),
            crate::exact_param(r.ratio),
            if r.ratio_mode_is_ratio { "ratio" } else { "amount" },
            r.dst_account,
            r.dst_aux,
            r.dst_dir.code(),
            r.offset_account,
            r.summary,
            r.memo
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn at_update(db: &Db, r: &AutoTransfer) -> DbResult<()> {
    db.conn().execute(
        "UPDATE auto_transfer SET name=?2,sort=?3,active=?4,src_account=?5,src_aux=?6,src_kind=?7,
            src_dir=?8,ratio=?9,ratio_mode=?10,dst_account=?11,dst_aux=?12,dst_dir=?13,
            offset_account=?14,summary=?15,memo=?16 WHERE id=?1",
        rusqlite::params![
            r.id,
            r.name,
            r.sort,
            r.active as i64,
            r.src_account,
            r.src_aux,
            r.src_kind.code(),
            r.src_dir.code(),
            crate::exact_param(r.ratio),
            if r.ratio_mode_is_ratio { "ratio" } else { "amount" },
            r.dst_account,
            r.dst_aux,
            r.dst_dir.code(),
            r.offset_account,
            r.summary,
            r.memo
        ],
    )?;
    Ok(())
}

pub fn at_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM auto_transfer WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 一条规则的试算结果
#[derive(Clone, Debug)]
pub struct TransferPreview {
    pub rule: AutoTransfer,
    /// 取到的基数
    pub base: Money,
    /// 实际转账金额
    pub amount: Money,
    /// 余额不足等提示
    pub note: String,
}

/// 试算某条规则在本期的取数结果
pub fn at_preview(db: &Db, r: &AutoTransfer, period: Period) -> DbResult<TransferPreview> {
    let snap = balances::BalanceSnapshot::load(db, &balances::BalanceQuery::period(period))?;
    let row = snap.for_account(&r.src_account, None);
    let signed = match r.src_kind {
        SrcKind::Begin => row.begin,
        SrcKind::Debit => row.debit,
        SrcKind::Credit => row.credit,
        SrcKind::End => row.end(),
    };
    let base = match r.src_dir {
        EntryDir::Debit => if signed.is_negative() { Money::ZERO } else { signed },
        EntryDir::Credit => if signed.is_positive() { Money::ZERO } else { signed.abs() },
        EntryDir::Auto => signed.abs(),
    };
    let amount = if r.ratio_mode_is_ratio {
        (base * r.ratio).round2()
    } else {
        r.ratio.round2()
    };
    let note = if amount.is_zero() {
        "取数为零，将跳过".to_string()
    } else {
        String::new()
    };
    Ok(TransferPreview {
        rule: r.clone(),
        base,
        amount,
        note,
    })
}

/// 批量试算
pub fn at_preview_all(db: &Db, period: Period) -> DbResult<Vec<TransferPreview>> {
    let mut out = Vec::new();
    for r in at_list(db)?.into_iter().filter(|r| r.active) {
        out.push(at_preview(db, &r, period)?);
    }
    Ok(out)
}

/// 执行自动转账，返回 (生成的凭证 id 列表, 跳过原因)
/// 某期某规则是否已经生成过凭证
///
/// 自动转账生成的是草稿凭证，未记账前不进余额，直接重复执行会出两张一样的分录。
/// 这里按"期间 + 来源 + 规则名"判断是否已生成过。
pub fn at_generated(db: &Db, period: Period, rule_name: &str) -> DbResult<bool> {
    at_generated_of(db.conn(), period, rule_name)
}

/// 同 `at_generated`，但只依赖连接：判重要与写入同处一个事务才可靠。
pub fn at_generated_of(
    conn: &rusqlite::Connection,
    period: Period,
    rule_name: &str,
) -> DbResult<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM voucher WHERE period=?1 AND source='auto_transfer'
               AND memo = ?2",
            rusqlite::params![period.ymm(), format!("自动转账：{rule_name}")],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(n > 0)
}

/// 同进程内串行化自动转账执行。
///
/// `at_generated` 的判重是「先 SELECT 再 INSERT」，而 `vouchers::save` 自带事务、
/// 没法并进外层事务，所以并发执行两次会各自判重通过、各计提一次（重复记账）。
/// 串行化后，第二次执行必然看到第一次已落库的凭证，走「本期已生成过，跳过」分支。
/// 只覆盖本进程：桌面端与 Web 端各自持锁，跨进程同开一个账套文件仍需人工避免。
static AT_RUN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn at_run(
    db: &Db,
    period: Period,
    date: chrono::NaiveDate,
    who: &str,
) -> DbResult<(Vec<i64>, Vec<String>)> {
    // 锁被毒化（前次执行 panic 过）不影响正确性：取锁只为串行化，接管即可
    let _serial = AT_RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previews = at_preview_all(db, period)?;
    let mut ids = Vec::new();
    let mut skips = Vec::new();
    for p in previews {
        if p.amount.is_zero() {
            skips.push(format!("{}：取数为零，跳过", p.rule.name));
            continue;
        }
        if at_generated(db, period, &p.rule.name)? {
            skips.push(format!("{}：本期已生成过，跳过", p.rule.name));
            continue;
        }
        let r = &p.rule;
        let offset = if r.offset_account.trim().is_empty() {
            r.src_account.clone()
        } else {
            r.offset_account.clone()
        };
        let dst_is_debit = r.dst_dir != EntryDir::Credit;
        // 判重 → 取号 → 写凭证收进同一事务。判重本质是「先查后插」，跨事务时
        // 并发执行会各生成一张、各计提一次；这里权威判重放在事务内，即便绕过
        // 上面的进程内串行锁（跨进程、或未来改成多线程调度）也不会重复计提。
        let tx = db.write_tx()?;
        if at_generated_of(&tx, period, &r.name)? {
            skips.push(format!("{}：本期已生成过，跳过", r.name));
            continue; // tx 在此 drop，即回滚
        }
        let no = crate::vouchers::next_no_of(&tx, period, "记")?;
        let mut v = Voucher::new(period, date, "记", no);
        v.prepared_by = who.to_string();
        v.source = VoucherSource::AutoTransfer;
        v.memo = format!("自动转账：{}", r.name);
        let summary = if r.summary.is_empty() {
            r.name.clone()
        } else {
            r.summary.clone()
        };
        v.push_entry(Entry {
            debit: if dst_is_debit { p.amount } else { Money::ZERO },
            credit: if dst_is_debit { Money::ZERO } else { p.amount },
            aux: AuxRef::from_key(&r.dst_aux),
            ..Entry::new(1, &r.dst_account, &summary)
        });
        v.push_entry(Entry {
            debit: if dst_is_debit { Money::ZERO } else { p.amount },
            credit: if dst_is_debit { p.amount } else { Money::ZERO },
            aux: AuxRef::from_key(&r.src_aux),
            ..Entry::new(2, &offset, &summary)
        });
        v.renumber();
        match crate::vouchers::save_in(&tx, &mut v) {
            Ok(id) => {
                tx.commit()?;
                ids.push(id);
            }
            Err(e) => skips.push(format!("{}：{}", r.name, e)),
        }
    }
    Ok((ids, skips))
}

// ===========================================================================
// 月度结账检查清单
// ===========================================================================

/// 一项检查
#[derive(Clone, Debug)]
pub struct CheckItem {
    pub key: &'static str,
    pub label: String,
    pub ok: bool,
    /// 未通过时的处理入口提示
    pub hint: String,
}

/// 结账前自检：把"最容易漏掉的那几件事"摆在明面上
pub fn checklist(db: &Db, period: Period) -> DbResult<Vec<CheckItem>> {
    let mut out = Vec::new();

    // 1. 未审核 / 未记账凭证
    let (draft, audited, posted, _void) = crate::vouchers::status_summary(db, period)?;
    out.push(CheckItem {
        key: "unaudited",
        label: format!("未审核凭证 {draft} 张"),
        ok: draft == 0,
        hint: "到【凭证管理】批量审核".into(),
    });
    out.push(CheckItem {
        key: "unposted",
        label: format!("未记账凭证 {audited} 张"),
        ok: audited == 0,
        hint: "到【凭证管理】批量记账".into(),
    });

    // 2. 试算平衡
    let snap = balances::BalanceSnapshot::load(db, &balances::BalanceQuery::period(period))?;
    let tb = snap.trial_balance(&crate::accounts::chart(db)?);
    let balanced =
        (tb.period_debit - tb.period_credit).abs() < Money::parse("0.01").unwrap();
    out.push(CheckItem {
        key: "trial",
        label: format!(
            "本期发生额借贷平衡（借 {} / 贷 {}）",
            tb.period_debit, tb.period_credit
        ),
        ok: balanced,
        hint: "查【科目余额表】找不平的科目".into(),
    });

    // 3. 固定资产折旧
    let assets = crate::assets::active_at(db, period)?;
    let deps = crate::assets::dep_list_period(db, period)?;
    out.push(CheckItem {
        key: "depreciation",
        label: format!("折旧已计提（应提 {} 项 / 已提 {} 项）", assets.len(), deps.len()),
        ok: deps.len() >= assets.len(),
        hint: "到【固定资产】计提本月折旧".into(),
    });

    // 4. 银行对账
    let bank_accounts = bank_accounts(db)?;
    let mut unrecon = 0usize;
    for a in &bank_accounts {
        let n = crate::bank::count(db, period, a)?;
        if n == 0 {
            unrecon += 1;
        }
    }
    out.push(CheckItem {
        key: "bank",
        label: format!(
            "银行对账单已导入（{} 个银行账户，{} 个未导入）",
            bank_accounts.len(),
            unrecon
        ),
        ok: unrecon == 0,
        hint: "到【出纳对账】导入对账单并勾对".into(),
    });

    // 5. 汇率
    let missing = fx_missing(db, period)?;
    out.push(CheckItem {
        key: "fx",
        label: if missing.is_empty() {
            "外币汇率已维护".to_string()
        } else {
            format!("未维护汇率：{}", missing.join("、"))
        },
        ok: missing.is_empty(),
        hint: "到【期末调汇】录入期末汇率".into(),
    });

    // 6. 工资
    let pay_n: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM payroll WHERE period=?1",
            rusqlite::params![period.ymm()],
            |r| r.get(0),
        )
        .unwrap_or(0);
    out.push(CheckItem {
        key: "payroll",
        label: if pay_n > 0 {
            format!("工资已计提（{pay_n} 人）")
        } else {
            "本月未生成工资表".to_string()
        },
        ok: pay_n > 0,
        hint: "到【工资管理】生成本月工资表".into(),
    });

    // 7. 损益结转（年末或每月，看账套选项）
    let _ = posted;
    out.push(CheckItem {
        key: "carry",
        label: "损益类科目已结转".to_string(),
        ok: is_profit_carried(db, period)?,
        hint: "到【期末处理】结转损益".into(),
    });

    Ok(out)
}

/// 损益是否已结转：看本期有没有来源为 carry 的凭证
fn is_profit_carried(db: &Db, period: Period) -> DbResult<bool> {
    let n: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM voucher WHERE period=?1 AND source IN ('carry','year_end')",
            rusqlite::params![period.ymm()],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(n > 0)
}

/// 银行存款类科目（1002 开头）
pub fn bank_accounts(db: &Db) -> DbResult<Vec<String>> {
    let chart = crate::accounts::chart(db)?;
    let mut out: Vec<String> = crate::accounts::list(db)?
        .into_iter()
        .filter(|a| a.is_bank && !a.disabled && chart.children(&a.code).is_empty())
        .map(|a| a.code)
        .collect();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::account::{Account, AcctCategory};
    use fincore::voucher::{Entry, Voucher};

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_auto_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }

    fn post_voucher(
        db: &Db,
        period: Period,
        date: chrono::NaiveDate,
        no: i32,
        lines: &[(&str, &str, &str)], // (科目, 借, 贷)
    ) -> i64 {
        let mut v = Voucher::new(period, date, "记", no);
        for (i, (acc, d, c)) in lines.iter().enumerate() {
            v.push_entry(Entry {
                debit: Money::parse(d).unwrap(),
                credit: Money::parse(c).unwrap(),
                aux: if acc.starts_with("1002") {
                    AuxRef {
                        bank: Some("BANK01".into()),
                        ..Default::default()
                    }
                } else if acc.starts_with("1122") || acc.starts_with("2202") {
                    AuxRef {
                        customer: Some("C01".into()),
                        ..Default::default()
                    }
                } else {
                    AuxRef::default()
                },
                ..Entry::new(i as i32 + 1, *acc, "测试")
            });
        }
        let id = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, id, "poster").unwrap();
        id
    }

    #[test]
    fn fx_rate_crud() {
        let db = tmpdb("fx");
        let p = Period::new(2026, 1).unwrap();
        assert!(fx_get(&db, p, "USD").unwrap().is_none());
        fx_set(&db, p, "USD", Money::parse("7.2").unwrap()).unwrap();
        assert_eq!(
            fx_get(&db, p, "USD").unwrap().unwrap().rate,
            Money::parse("7.2").unwrap()
        );
        // 覆盖
        fx_set(&db, p, "USD", Money::parse("7.25").unwrap()).unwrap();
        assert_eq!(fx_list(&db, p).unwrap().len(), 1);
        assert!(fx_set(&db, p, "USD", Money::ZERO).is_err());
        fx_delete(&db, p, "USD").unwrap();
        assert!(fx_get(&db, p, "USD").unwrap().is_none());
    }

    #[test]
    fn fx_adjust_generates_voucher() {
        let db = tmpdb("fxadj");
        let p = Period::new(2026, 1).unwrap();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        // 应收 1000 美元，入账汇率 7.0 → 本位币 7000
        let mut v = Voucher::new(p, d, "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("7000").unwrap(),
            credit: Money::ZERO,
            aux: AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            currency: Some("USD".into()),
            rate: Some(Money::parse("7").unwrap().inner()),
            amount_for: Some(Money::parse("1000").unwrap()),
            ..Entry::new(1, "112201", "出口销售")
        });
        v.push_entry(Entry {
            debit: Money::ZERO,
            credit: Money::parse("7000").unwrap(),
            ..Entry::new(2, "600101", "出口销售")
        });
        let id = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, id, "p").unwrap();

        // 期末汇率 7.2 → 本位币应为 7200，调增 200
        fx_set(&db, p, "USD", Money::parse("7.2").unwrap()).unwrap();
        let lines = fx_calc(&db, p).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].foreign, Money::parse("1000").unwrap());
        assert_eq!(lines[0].diff, Money::parse("200").unwrap());

        let vid = fx_adjust(&db, p, d, FX_GAIN_ACCOUNT, "u").unwrap().unwrap();
        let v2 = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert!(v2.balanced());
        assert_eq!(v2.debit_total(), Money::parse("200").unwrap());
    }

    #[test]
    fn auto_transfer_carry_and_accrue() {
        let db = tmpdb("at");
        let p = Period::new(2026, 1).unwrap();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        // 510101 / 500101 / 222103 / 6403 都在内置科目表里
        // 制造费用发生 3000（借）
        post_voucher(
            &db,
            p,
            d,
            1,
            &[("510101", "3000", "0"), ("100201", "0", "3000")],
        );

        // 结转制造费用：src=510101 期末借方，全部转到 500101，对方留空 → 结平来源
        let rule = AutoTransfer {
            id: 0,
            name: "结转制造费用".into(),
            sort: 10,
            active: true,
            src_account: "510101".into(),
            src_aux: String::new(),
            src_kind: SrcKind::End,
            src_dir: EntryDir::Debit,
            ratio: Money::ONE,
            ratio_mode_is_ratio: true,
            dst_account: "500101".into(),
            dst_aux: String::new(),
            dst_dir: EntryDir::Debit,
            offset_account: String::new(),
            summary: "结转制造费用".into(),
            memo: String::new(),
        };
        at_insert(&db, &rule).unwrap();
        let pv = at_preview_all(&db, p).unwrap();
        assert_eq!(pv.len(), 1);
        assert_eq!(pv[0].base, Money::parse("3000").unwrap());
        assert_eq!(pv[0].amount, Money::parse("3000").unwrap());

        let (ids, skips) = at_run(&db, p, d, "u").unwrap();
        assert_eq!(ids.len(), 1, "{skips:?}");
        let v = crate::vouchers::get(&db, ids[0]).unwrap().unwrap();
        assert!(v.balanced());
        let e = crate::vouchers::entries_of(&db, ids[0]).unwrap();
        assert_eq!(e[0].account_code, "500101");
        assert_eq!(e[0].debit, Money::parse("3000").unwrap());
        assert_eq!(e[1].account_code, "510101");
        assert_eq!(e[1].credit, Money::parse("3000").unwrap());

        // 结平后再次执行应跳过
        let (ids2, skips2) = at_run(&db, p, d, "u").unwrap();
        assert_eq!(ids2.len(), 0);
        assert_eq!(skips2.len(), 1);

        // 计提类：对方科目填了，来源只取数不转出
        let rule2 = AutoTransfer {
            id: 0,
            name: "计提城建税".into(),
            sort: 20,
            active: true,
            src_account: "510101".into(),
            src_kind: SrcKind::Debit,
            src_dir: EntryDir::Debit,
            ratio: Money::parse("0.07").unwrap(),
            dst_account: "6403".into(),
            dst_dir: EntryDir::Debit,
            offset_account: "222103".into(),
            summary: "计提城建税".into(),
            ..rule.clone()
        };
        at_insert(&db, &rule2).unwrap();
        let (ids3, skips3) = at_run(&db, p, d, "u").unwrap();
        let made: Vec<_> = ids3
            .iter()
            .filter_map(|i| crate::vouchers::get(&db, *i).ok().flatten())
            .filter(|v| v.memo.contains("计提城建税"))
            .collect();
        assert_eq!(made.len(), 1, "{skips3:?}");
        let e3 = crate::vouchers::entries_of(&db, made[0].id).unwrap();
        assert_eq!(e3[0].account_code, "6403");
        assert_eq!(e3[0].debit, Money::parse("210").unwrap()); // 3000 × 7%
        assert_eq!(e3[1].account_code, "222103");
    }

    #[test]
    fn checklist_runs() {
        let db = tmpdb("check");
        let p = Period::new(2026, 1).unwrap();
        crate::accounts::insert(&db, &Account::new("100201", "工行", AcctCategory::Asset))
            .ok();
        let items = checklist(&db, p).unwrap();
        assert!(!items.is_empty());
        assert!(items.iter().any(|i| i.key == "depreciation"));
        assert!(items.iter().any(|i| i.key == "bank"));
        assert!(items.iter().any(|i| i.key == "fx"));
    }
}
