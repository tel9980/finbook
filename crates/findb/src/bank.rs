//! 银行对账（出纳模块）
//!
//! 流程：导入银行对账单 → 自动勾对 → 手工补勾 → 生成余额调节表。
//!
//! 自动勾对的判定顺序很关键，实务上按这个优先级最不容易勾错：
//! 1. 结算号相同（最可靠的锚点）
//! 2. 金额完全相同 + 方向相反 + 业务日期在 ±N 天内
//! 3. 金额完全相同 + 方向相反（日期不限）
//!
//! 一对多（一笔银行流水对多张凭证）在自动阶段不做，留给手工。
//!
//! 另有一条**可选**的收尾链路：导入流水 → 自动勾对 → **对没勾上的流水生成凭证草稿**
//! → 审核 → 记账。见 [`gen_vouchers`]。

use chrono::NaiveDate;
use fincore::{AuxKind, Entry, Money, Period, Voucher, VoucherSource};

use crate::{balances, Db, DbResult};

/// 一条被跳过的流水（附原因）—— 让用户知道「哪些没处理、为什么」，
/// 而不是只看到一个数字。
#[derive(Clone, Debug)]
pub struct SkipRow {
    pub stmt_id: i64,
    pub biz_date: String,
    pub summary: String,
    pub amount: Money,
    pub reason: String,
}

/// 流水生成凭证的结果
#[derive(Clone, Debug, Default)]
pub struct GenResult {
    /// 生成的凭证张数
    pub generated: usize,
    /// 生成后顺带勾上的流水行数（应当 == generated：一张凭证一条流水）
    pub linked: usize,
    /// 跳过的流水 + 原因
    pub skipped: Vec<SkipRow>,
    /// 生成的凭证 id（便于前端跳过去看）
    pub voucher_ids: Vec<i64>,
}

/// 对方科目怎么定：进账认客户（贷应收）、支出认供应商（借应付）。
///
/// 方向搞反会做出方向相反的往来，所以这里写死成对偶关系而不是让调用方传：
/// 银行进账只可能是「客户给钱」（贷应收），银行支出只可能是「我们给供应商钱」
/// （借应付）。反过来意味着对方是我们欠的（预收/预付），那是另一类业务，
/// 不该由「银行流水自动生成凭证」去猜。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Counterparty {
    Customer,
    Supplier,
}

/// 摘要里找出对方单位。
///
/// 银行摘要格式千奇百怪（「网银转入 深圳市XX有限公司」「收-华东商贸」…），
/// 没法可靠解析。唯一稳的锚点是**对方名称作为子串出现在摘要里**。
/// 多个都命中时取**名称最长**的 —— 最长的那个通常是最具体的那个
/// （「深圳市XX有限公司」比「XX」更可能是真名）。
fn match_counterparty(summary: &str, map: &std::collections::HashMap<String, String>) -> Option<(String, String)> {
    let mut best: Option<(String, String)> = None;
    for (code, name) in map {
        if name.is_empty() || !summary.contains(name.as_str()) {
            continue;
        }
        let better = match &best {
            None => true,
            Some((_, n)) => name.chars().count() > n.chars().count(),
        };
        if better {
            best = Some((code.clone(), name.clone()));
        }
    }
    best
}

/// 从**未勾对**的银行流水生成凭证草稿。
///
/// ## 为什么只做「能匹配往来单位」的那部分
///
/// 另一种做法是给所有未勾对流水生成凭证、对方挂「待定」科目。**这里刻意不做**：
/// 「待定」会在期末变成一堆没法处理的余额，而且用户往往到期末才发现。
/// 匹配不上的行原样留在对账页让人处理 —— 看得见的未处理，好过看不见的垃圾。
///
/// ## 生成的凭证长什么样
///
/// 只包含「银行侧 + 往来侧」两行：
/// - 进账（客户回款）：借 `account`（银行/现金科目） / 贷 `biz.ar` 应收账款（辅助=客户）
/// - 支出（付供应商）：借 `biz.ap` 应付账款（辅助=供应商） / 贷 `account`（银行/现金科目）
///
/// **不含收入/成本侧**。所以若对应的销售/采购尚未入账，应收/应付会出现
/// 负数（贷方余额）。这不是 bug，是这条路子的固有前提：**它只适合往来已经
/// 记好、只差银行侧没记的场景**（即「勾对没勾上但业务确实发生过」）。
/// 这一点在返回值里如实说明，界面上也要显示。
///
/// ## 内控
///
/// 生成的凭证一律 `Draft`，且 `source = Import`，与手工录入同一条路径：
/// 要过 `enable_audit`（若开启）、要过 `require_cashier`（资金类科目必然命中）
/// 才能记账。**绝不自动记账** —— 自动拿银行流水直接记账等于绕过出纳签字，
/// 那是这套系统里最不该被自动掉的一关。
///
/// ## 幂等
///
/// 生成后立刻把该流水行 `link` 到新分录，于是它不再是 `entry_id IS NULL`，
/// 再跑一次不会重复生成。
pub fn gen_vouchers(db: &Db, period: Period, account: &str, who: &str) -> DbResult<GenResult> {
    let acct = account.trim();
    if acct.is_empty() {
        return Err(fincore::FinError::validate("缺少银行/现金科目").into());
    }
    let chart = crate::accounts::chart(db)?;
    if !chart.get(acct).is_some() {
        return Err(fincore::FinError::not_found(format!("科目不存在：{acct}")).into());
    }
    let biz = crate::options_of(db.conn()).biz_accounts;
    let customers = aux_map(db, AuxKind::Customer)?;
    let suppliers = aux_map(db, AuxKind::Supplier)?;

    let mut res = GenResult::default();
    let stmts = list(db, period, acct)?;

    // 整批一个事务：中途失败整体回滚，不留「生成了一半」的凭证
    let tx = db.write_tx()?;
    for s in stmts.iter().filter(|s| !s.matched()) {
        let amt = s.signed();
        if amt.is_zero() {
            res.skipped.push(skip_of(s, "金额为 0，无需生成"));
            continue;
        }
        let in_flow = amt.is_positive();
        // 进账找客户、支出找供应商（方向与往来类型是对偶的，见 Counterparty 说明）
        let want = if in_flow { Counterparty::Customer } else { Counterparty::Supplier };
        let map = match want {
            Counterparty::Customer => &customers,
            Counterparty::Supplier => &suppliers,
        };
        let Some((aux_code, aux_name)) = match_counterparty(&s.summary, map) else {
            let kindname = if in_flow { "客户" } else { "供应商" };
            res.skipped.push(skip_of(
                s,
                &format!("摘要里认不出{kindname}名称（不猜对方科目，留给人工处理）"),
            ));
            continue;
        };
        let other = if in_flow { biz.ar.as_str() } else { biz.ap.as_str() };
        if other.trim().is_empty() {
            res.skipped.push(skip_of(
                s,
                "账套参数未配置应收/应付科目（参数 → 业务科目），无法确定对方科目",
            ));
            continue;
        }
        if chart.get(other).is_none() {
            res.skipped.push(skip_of(
                s,
                &format!("对方科目 {other} 在科目表里不存在，请先修正业务科目参数"),
            ));
            continue;
        }

        let word = "记";
        let no = crate::vouchers::next_no_of(&tx, period, word)?;
        let mut v = Voucher::new(period, s.biz_date, word.to_string(), no);
        v.source = VoucherSource::Import;
        let text = format!("{} {}", aux_name, s.summary);
        v.memo = format!("银行流水自动生成：{} {}", s.biz_date, text);

        // 两种方向都是「借方一笔 + 贷方一笔」：
        //   进账（客户回款）借 银行 / 贷 应收(客户)
        //   支出（付供应商）借 应付(供应商) / 贷 银行
        let mut debit = if in_flow {
            Entry::new(1, acct, text.clone())
        } else {
            Entry::new(1, other, text.clone())
        };
        let mut credit = if in_flow {
            Entry::new(2, other, text.clone())
        } else {
            Entry::new(2, acct, text.clone())
        };
        // 方向判断用带符号的 `amt`，**填进分录必须用绝对值** ——
        // 支出时 `signed()` 是负数，直接写进 debit 会撞「金额不能为负数」。
        let magnitude = if amt.is_negative() { -amt } else { amt };
        debit.debit = magnitude;
        credit.credit = magnitude;
        let kind = want_aux_kind(want);
        if in_flow {
            // 贷应收，辅助挂客户
            credit.aux.set(kind, Some(aux_code.clone()));
        } else {
            // 借应付，辅助挂供应商
            debit.aux.set(kind, Some(aux_code.clone()));
        }
        // 银行/现金科目本身也带辅助（银行账户），不填过不了记账校验 ——
        // 「科目 100201 核算银行账户，必须填写银行账户」。取该科目下的第一个
        // 银行账户；一个都没有就跳过这一笔，不静默塞个假的。
        let bank_aux = first_bank_aux(db)?;
        let Some(bank_code) = bank_aux else {
            res.skipped.push(skip_of(
                s,
                &format!("科目 {acct} 核算银行账户但账套里一个银行账户都没建，请先在「资金 → 银行账户」建一个"),
            ));
            continue;
        };
        if in_flow {
            debit.aux.set(AuxKind::Bank, Some(bank_code));
        } else {
            credit.aux.set(AuxKind::Bank, Some(bank_code));
        }
        v.entries = vec![debit, credit];
        let vid = crate::vouchers::save_in(&tx, &mut v)?;
        // 找到银行侧那条分录，勾上流水行 —— 这一步同时保证幂等
        let entry_id: i64 = tx.query_row(
            "SELECT id FROM voucher_entry WHERE voucher_id=?1 AND account_code=?2 ORDER BY line LIMIT 1",
            rusqlite::params![vid, acct],
            |r| r.get(0),
        )?;
        link(&tx, s.id, entry_id, who)?;
        res.generated += 1;
        res.linked += 1;
        res.voucher_ids.push(vid);
    }
    tx.commit()?;
    Ok(res)
}

/// 取该银行/现金科目下的第一个银行账户辅助编码。
///
/// 银行科目本身带 `AuxKind::Bank` 辅助，不填会被记账校验拒
/// （「科目 100201 核算银行账户，必须填写银行账户」）。取第一个是有意的：
/// 流水只带账号的少数情况才需要精确对应，绝大多数对账单不带账号，
/// 按第一个挂上即可 —— 人工仍可在凭证上改。
fn first_bank_aux(db: &Db) -> DbResult<Option<String>> {
    Ok(crate::auxs::codes(db, AuxKind::Bank)?.into_iter().next())
}

fn want_aux_kind(c: Counterparty) -> AuxKind {
    match c {
        Counterparty::Customer => AuxKind::Customer,
        Counterparty::Supplier => AuxKind::Supplier,
    }
}

fn aux_map(
    db: &Db,
    kind: AuxKind,
) -> DbResult<std::collections::HashMap<String, String>> {
    let mut m = std::collections::HashMap::new();
    for code in crate::auxs::codes(db, kind)? {
        if let Some(e) = crate::auxs::get(db, kind, &code)? {
            if !e.disabled {
                m.insert(e.code, e.name);
            }
        }
    }
    Ok(m)
}

fn skip_of(s: &Statement, reason: &str) -> SkipRow {
    SkipRow {
        stmt_id: s.id,
        biz_date: s.biz_date.to_string(),
        summary: s.summary.clone(),
        amount: s.signed(),
        reason: reason.to_string(),
    }
}

/// 银行对账单流水
#[derive(Clone, Debug)]
pub struct Statement {
    pub id: i64,
    pub period: Period,
    pub account_code: String,
    pub biz_date: NaiveDate,
    pub summary: String,
    pub settle_no: String,
    /// 银行口径进账
    pub debit: Money,
    /// 银行口径支出
    pub credit: Money,
    /// 该笔后的银行余额
    pub balance: Money,
    pub entry_id: Option<i64>,
    pub matched_at: Option<String>,
    pub matched_by: Option<String>,
}

impl Statement {
    /// 带符号金额（进账为正）
    pub fn signed(&self) -> Money {
        self.debit - self.credit
    }
    pub fn matched(&self) -> bool {
        self.entry_id.is_some()
    }
}

/// 账面（凭证）这一侧的一笔银行收支
#[derive(Clone, Debug)]
pub struct BookEntry {
    pub entry_id: i64,
    pub voucher_id: i64,
    pub date: NaiveDate,
    pub word: String,
    pub no: i32,
    pub summary: String,
    pub settle_no: String,
    pub debit: Money,
    pub credit: Money,
}

impl BookEntry {
    pub fn signed(&self) -> Money {
        self.debit - self.credit
    }
    pub fn voucher_label(&self) -> String {
        format!("{}-{}", self.word, self.no)
    }
}

fn map_stmt(r: &rusqlite::Row) -> rusqlite::Result<Statement> {
    let d: String = r.get(3)?;
    Ok(Statement {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        account_code: r.get(2)?,
        biz_date: NaiveDate::parse_from_str(&d, "%Y-%m-%d").unwrap_or_else(|_| {
            NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
        }),
        summary: r.get(4)?,
        settle_no: r.get(5)?,
        debit: Money::parse_or_zero(&r.get::<_, String>(6)?),
        credit: Money::parse_or_zero(&r.get::<_, String>(7)?),
        balance: Money::parse_or_zero(&r.get::<_, String>(8)?),
        entry_id: r.get(9)?,
        matched_at: r.get(10)?,
        matched_by: r.get(11)?,
    })
}

const COLS: &str = "id,period,account_code,biz_date,summary,settle_no,debit,credit,balance,
     entry_id,matched_at,matched_by";

pub fn list(c: &impl crate::AsConn, period: Period, account: &str) -> DbResult<Vec<Statement>> {
    let mut st = c.conn_ref().prepare(&format!(
        "SELECT {COLS} FROM bank_statement WHERE period=?1 AND account_code=?2
         ORDER BY biz_date, id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm(), account], map_stmt)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 本期本科目已导入的流水数
pub fn count(c: &impl crate::AsConn, period: Period, account: &str) -> DbResult<i64> {
    Ok(c.conn_ref().query_row(
        "SELECT COUNT(*) FROM bank_statement WHERE period=?1 AND account_code=?2",
        rusqlite::params![period.ymm(), account],
        |r| r.get(0),
    )?)
}

pub fn insert(c: &impl crate::AsConn, s: &Statement) -> DbResult<i64> {
    c.conn_ref().execute(
        "INSERT INTO bank_statement(period,account_code,biz_date,summary,settle_no,
            debit,credit,balance,entry_id,matched_at,matched_by)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        rusqlite::params![
            s.period.ymm(),
            s.account_code,
            s.biz_date.format("%Y-%m-%d").to_string(),
            s.summary,
            s.settle_no,
            crate::money_param(s.debit),
            crate::money_param(s.credit),
            crate::money_param(s.balance),
            s.entry_id,
            s.matched_at,
            s.matched_by
        ],
    )?;
    Ok(c.conn_ref().last_insert_rowid())
}

pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM bank_statement WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 清空某科目某期的对账单（重新导入前用）
pub fn clear(db: &Db, period: Period, account: &str) -> DbResult<usize> {
    Ok(db.conn().execute(
        "DELETE FROM bank_statement WHERE period=?1 AND account_code=?2",
        rusqlite::params![period.ymm(), account],
    )?)
}

/// 勾对：一条银行流水 ? 一条凭证分录
pub fn link(c: &impl crate::AsConn, stmt_id: i64, entry_id: i64, who: &str) -> DbResult<()> {
    c.conn_ref().execute(
        "UPDATE bank_statement SET entry_id=?2, matched_at=?3, matched_by=?4 WHERE id=?1",
        rusqlite::params![
            stmt_id,
            entry_id,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            who
        ],
    )?;
    Ok(())
}

pub fn unlink(db: &Db, stmt_id: i64) -> DbResult<()> {
    db.conn().execute(
        "UPDATE bank_statement SET entry_id=NULL, matched_at=NULL, matched_by=NULL WHERE id=?1",
        rusqlite::params![stmt_id],
    )?;
    Ok(())
}

/// 取消某科目某期的全部勾对
pub fn unlink_all(db: &Db, period: Period, account: &str) -> DbResult<usize> {
    Ok(db.conn().execute(
        "UPDATE bank_statement SET entry_id=NULL, matched_at=NULL, matched_by=NULL
         WHERE period=?1 AND account_code=?2",
        rusqlite::params![period.ymm(), account],
    )?)
}

// ---------------- 账面侧 ----------------

/// 取出某科目某期所有已记账的银行收支分录（含是否已被勾对）
pub fn book_side(c: &impl crate::AsConn, period: Period, account: &str) -> DbResult<Vec<BookEntry>> {
    let mut st = c.conn_ref().prepare(
        "SELECT e.id, v.id, v.date, v.word, v.no, e.summary,
                COALESCE(e.settle_no,''), e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON v.id = e.voucher_id
         WHERE e.account_code = ?1 AND v.period = ?2 AND v.status = 'posted'
           AND (e.debit <> '0' OR e.credit <> '0')
         ORDER BY v.date, v.no, e.line",
    )?;
    let rows = st
        .query_map(rusqlite::params![account, period.ymm()], |r| {
            let d: String = r.get(2)?;
            Ok(BookEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(3)?,
                no: r.get(4)?,
                summary: r.get(5)?,
                settle_no: r.get(6)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(7)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(8)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 账面侧已被勾对的分录 id
pub fn linked_entry_ids(c: &impl crate::AsConn, period: Period, account: &str) -> DbResult<Vec<i64>> {
    let mut st = c.conn_ref().prepare(
        "SELECT entry_id FROM bank_statement
         WHERE period=?1 AND account_code=?2 AND entry_id IS NOT NULL",
    )?;
    let rows = st
        .query_map(rusqlite::params![period.ymm(), account], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------- 自动勾对 ----------------

/// 自动勾对结果
#[derive(Clone, Debug, Default)]
pub struct MatchResult {
    /// 成功勾对的对数
    pub matched: usize,
    /// 按结算号勾上的
    pub by_no: usize,
    /// 按金额+日期勾上的
    pub by_amount_date: usize,
    /// 按金额勾上的
    pub by_amount: usize,
    /// 一笔银行流水对上多条凭证的（只标记不自动处理）
    pub ambiguous: usize,
}

/// 自动勾对。
///
/// `date_tolerance` 是日期容差天数，默认 3 天（跨月的银行流水很常见）。
pub fn auto_match(
    db: &Db,
    period: Period,
    account: &str,
    date_tolerance: i64,
    who: &str,
) -> DbResult<MatchResult> {
    // 读快照与写勾对必须在同一事务：勾对本质是「查哪些没勾 → 逐条写 entry_id」，
    // 两个并发 auto_match（或自动勾对撞上手工勾对）会各自基于旧快照判定，把同一
    // 条流水/分录勾到别处。BEGIN IMMEDIATE 让整个过程独占写锁。
    let tx = db.write_tx()?;
    let mut stmts = list(&tx, period, account)?;
    let books = book_side(&tx, period, account)?;
    let linked = linked_entry_ids(&tx, period, account)?;
    let linked: std::collections::HashSet<i64> = linked.into_iter().collect();

    let mut res = MatchResult::default();
    let mut used: std::collections::HashSet<i64> = std::collections::HashSet::new();
    // 本轮内已勾对成功的流水：stmts 里的 matched() 反映的是「进入本函数时」的状态，
    // link() 只写库不更新内存，若不单独记录，后续轮次会重复勾对同一笔流水并覆盖首次结果。
    let mut done_stmt: std::collections::HashSet<i64> = std::collections::HashSet::new();

    // 第一轮：结算号精确匹配
    for s in &stmts {
        if s.matched() || done_stmt.contains(&s.id) {
            continue;
        }
        if s.settle_no.trim().is_empty() {
            continue;
        }
        let cands: Vec<&BookEntry> = books
            .iter()
            .filter(|b| {
                !used.contains(&b.entry_id)
                    && !linked.contains(&b.entry_id)
                    && !b.settle_no.trim().is_empty()
                    && b.settle_no.trim() == s.settle_no.trim()
                    && b.signed() == s.signed()
            })
            .collect();
        if cands.len() == 1 {
            used.insert(cands[0].entry_id);
            done_stmt.insert(s.id);
            link(&tx, s.id, cands[0].entry_id, who)?;
            res.by_no += 1;
        } else if cands.len() > 1 {
            res.ambiguous += 1;
        }
    }

    // 第二轮：金额 + 方向 + 日期容差
    for s in &stmts {
        if s.matched() || done_stmt.contains(&s.id) {
            continue;
        }
        let cands: Vec<&BookEntry> = books
            .iter()
            .filter(|b| {
                !used.contains(&b.entry_id)
                    && !linked.contains(&b.entry_id)
                    && b.signed() == s.signed()
                    && (b.debit + b.credit).round2() == (s.debit + s.credit).round2()
                    && (b.date - s.biz_date).num_days().abs() <= date_tolerance
            })
            .collect();
        if cands.len() == 1 {
            used.insert(cands[0].entry_id);
            done_stmt.insert(s.id);
            link(&tx, s.id, cands[0].entry_id, who)?;
            res.by_amount_date += 1;
        } else if cands.len() > 1 {
            res.ambiguous += 1;
        }
    }

    // 第三轮：只看金额 + 方向（日期不限）
    for s in &stmts {
        if s.matched() || done_stmt.contains(&s.id) {
            continue;
        }
        let cands: Vec<&BookEntry> = books
            .iter()
            .filter(|b| {
                !used.contains(&b.entry_id)
                    && !linked.contains(&b.entry_id)
                    && b.signed() == s.signed()
                    && (b.debit + b.credit).round2() == (s.debit + s.credit).round2()
            })
            .collect();
        if cands.len() == 1 {
            used.insert(cands[0].entry_id);
            done_stmt.insert(s.id);
            link(&tx, s.id, cands[0].entry_id, who)?;
            res.by_amount += 1;
        } else if cands.len() > 1 {
            res.ambiguous += 1;
        }
    }

    // 重新读一次，统计最终勾对数（含本次之前已勾的）
    stmts = list(&tx, period, account)?;
    res.matched = stmts.iter().filter(|s| s.matched()).count();
    tx.commit()?;
    Ok(res)
}

// ---------------- 余额调节表 ----------------

/// 余额调节表
#[derive(Clone, Debug)]
pub struct Reconciliation {
    pub period: Period,
    pub account_code: String,
    /// 银行对账单期末余额
    pub bank_balance: Money,
    /// 企业账面期末余额（带符号，借方为正）
    pub book_balance: Money,
    /// 企业已收、银行未收（账面有、对账单没有的进账）
    pub book_only_in: Vec<BookEntry>,
    /// 企业已付、银行未付
    pub book_only_out: Vec<BookEntry>,
    /// 银行已收、企业未记
    pub bank_only_in: Vec<Statement>,
    /// 银行已付、企业未记
    pub bank_only_out: Vec<Statement>,
    /// 银行侧调节后余额
    pub bank_adjusted: Money,
    /// 企业侧调节后余额
    pub book_adjusted: Money,
}

impl Reconciliation {
    /// 两侧调节后余额是否一致（容差 0.01，避免分位尾差误报）
    pub fn balanced(&self) -> bool {
        (self.bank_adjusted - self.book_adjusted).abs() < Money::parse("0.01").unwrap()
    }
    /// 差额
    pub fn diff(&self) -> Money {
        self.bank_adjusted - self.book_adjusted
    }
}

pub fn reconcile(db: &Db, period: Period, account: &str) -> DbResult<Reconciliation> {
    let stmts = list(db, period, account)?;
    let books = book_side(db, period, account)?;
    let linked: std::collections::HashSet<i64> = linked_entry_ids(db, period, account)?
        .into_iter()
        .collect();

    let bank_balance = stmts.last().map(|s| s.balance).unwrap_or(Money::ZERO);

    let snap = balances::BalanceSnapshot::load(db, &balances::BalanceQuery::period(period))?;
    let book_balance = snap.for_account(account, None).end();

    let mut book_only_in = Vec::new();
    let mut book_only_out = Vec::new();
    for b in books {
        if linked.contains(&b.entry_id) {
            continue;
        }
        if b.signed().is_positive() {
            book_only_in.push(b);
        } else if b.signed().is_negative() {
            book_only_out.push(b);
        }
    }

    let mut bank_only_in = Vec::new();
    let mut bank_only_out = Vec::new();
    for s in stmts {
        if s.matched() {
            continue;
        }
        if s.signed().is_positive() {
            bank_only_in.push(s);
        } else if s.signed().is_negative() {
            bank_only_out.push(s);
        }
    }

    let sum_in: Money = book_only_in.iter().map(|b| b.signed()).sum();
    let sum_out: Money = book_only_out.iter().map(|b| b.signed().abs()).sum();
    let bank_in: Money = bank_only_in.iter().map(|s| s.signed()).sum();
    let bank_out: Money = bank_only_out.iter().map(|s| s.signed().abs()).sum();

    Ok(Reconciliation {
        period,
        account_code: account.to_string(),
        bank_balance,
        book_balance,
        bank_adjusted: bank_balance + sum_in - sum_out,
        book_adjusted: book_balance + bank_in - bank_out,
        book_only_in,
        book_only_out,
        bank_only_in,
        bank_only_out,
    })
}

// ---------------- CSV 导入 ----------------

/// 解析一行 CSV（支持逗号 / 制表符 / 分号分隔），返回字段向量
pub fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_q && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_q = !in_q;
                }
            }
            ',' | '\t' | ';' if !in_q => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// 日期解析：支持 `2026-01-05` `2026/1/5` `20260105` `2026年1月5日`
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    for f in ["%Y-%m-%d", "%Y/%m/%d", "%Y.%m.%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return Some(d);
        }
    }
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        let y = s[0..4].parse().ok()?;
        let m = s[4..6].parse().ok()?;
        let d = s[6..8].parse().ok()?;
        return NaiveDate::from_ymd_opt(y, m, d);
    }
    // 2026年1月5日
    let t = s.replace(['年', '月'], "-").replace('日', "");
    NaiveDate::parse_from_str(&t, "%Y-%m-%d").ok()
}

/// 金额解析：去掉千分位、货币符号、括号负数
pub fn parse_money(s: &str) -> Money {
    let mut t = s.trim().replace([',', '￥', '￥', '$', ' '], "");
    let neg = t.starts_with('(') && t.ends_with(')');
    if neg {
        t = t.trim_start_matches('(').trim_end_matches(')').to_string();
    }
    let v = Money::parse_or_zero(&t);
    if neg {
        v.negated()
    } else {
        v
    }
}

/// 导入对账单。
///
/// 列顺序（首行是表头则自动跳过）：业务日期, 摘要, 结算号, 借方发生额, 贷方发生额, 余额
/// 也兼容"单金额列"格式：业务日期, 摘要, 结算号, 金额, 余额（负数表示支出）。
/// 返回 (导入条数, 跳过的行数)。
pub fn import_csv(
    db: &Db,
    period: Period,
    account: &str,
    text: &str,
) -> DbResult<(usize, Vec<String>)> {
    let mut warns: Vec<String> = Vec::new();
    // 先把能解析的行全部解析出来，再一次事务写库：逐行自动提交的话，导入中途
    // 失败会留下半份对账单，重新导入同一文件就会重复。
    let mut rows: Vec<Statement> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let f = split_csv(line);
        // 表头：第一列不是合法日期就跳过
        if i == 0 && parse_date(&f[0]).is_none() {
            continue;
        }
        let date = match parse_date(f.first().unwrap_or(&String::new())) {
            Some(d) => d,
            None => {
                warns.push(format!("第 {} 行：日期无法识别，已跳过", i + 1));
                continue;
            }
        };
        if f.len() < 3 {
            warns.push(format!("第 {} 行：列数不足，已跳过", i + 1));
            continue;
        }
        let summary = f.get(1).cloned().unwrap_or_default();
        let settle_no = f.get(2).cloned().unwrap_or_default();

        let (debit, credit, balance) = if f.len() >= 6 {
            let d = parse_money(f.get(3).unwrap_or(&String::new()));
            let c = parse_money(f.get(4).unwrap_or(&String::new()));
            (d, c, parse_money(f.get(5).unwrap_or(&String::new())))
        } else {
            // 单金额列
            let v = parse_money(f.get(3).unwrap_or(&String::new()));
            let bal = parse_money(f.get(4).unwrap_or(&String::new()));
            if v.is_negative() {
                (Money::ZERO, v.abs(), bal)
            } else {
                (v, Money::ZERO, bal)
            }
        };

        if debit.is_zero() && credit.is_zero() {
            warns.push(format!("第 {} 行：金额为零，已跳过", i + 1));
            continue;
        }

        rows.push(Statement {
            id: 0,
            period,
            account_code: account.to_string(),
            biz_date: date,
            summary,
            settle_no,
            debit,
            credit,
            balance,
            entry_id: None,
            matched_at: None,
            matched_by: None,
        });
    }
    let n = rows.len();
    let tx = db.write_tx()?;
    for s in &rows {
        insert(&tx, s)?;
    }
    tx.commit()?;
    Ok((n, warns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::voucher::{Entry, Voucher, VoucherStatus};
    use fincore::AuxEntity;
    use fincore::Period;

    /// 构造一条「除测试自己填的字段外全为空」的流水基准。
    /// 刻意不复用上面那个 `stmt()` 辅助：它会对金额做 `Money::parse`，
    /// 传 "0" 会撞 `ParseError::TooShort`。
    fn mk_stmt(period: Period) -> Statement {
        Statement {
            id: 0,
            period,
            account_code: "100201".into(),
            biz_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            summary: String::new(),
            settle_no: String::new(),
            debit: Money::ZERO,
            credit: Money::ZERO,
            balance: Money::ZERO,
            entry_id: None,
            matched_at: None,
            matched_by: None,
        }
    }

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_bank_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &crate::tests::test_opts()).unwrap()
    }

    /// 流水生成凭证：只对**认得出往来单位**的未勾对流水生成，其余原样留下。
    ///
    /// 断言的是**边界**而不只是 happy path：认不出的行必须留在对账页、
    /// 不能给它们挂「待定」科目 —— 那种挂账要到期末才暴露且往往没人处理。
    /// 顺带验幂等（生成后流水已被勾上，再跑不会重复生成）与内控（凭证是草稿）。
    #[test]
    fn gen_vouchers_only_handles_recognizable_counterparties() {
        let db = tmpdb("gen");
        let period = Period::new(2026, 1).unwrap();
        // 业务科目参数：应收/应付必须有值，否则对方科目定不下来
        let mut opts = crate::options_of(db.conn());
        // 必须用**末级**科目：1122 / 2202 是汇总节点，记账校验会拒
        // （「非末级科目，不能记账」）。末级才是真正能落分录的地方。
        opts.biz_accounts.ar = "112201".into(); // 应收货款
        opts.biz_accounts.ap = "220201".into(); // 应付货款
        db.set_options(&opts).unwrap();
        // 往来单位
        crate::auxs::insert(
            &db,
            &AuxEntity::new(AuxKind::Customer, "C001", "华东商贸有限公司"),
        )
        .unwrap();
        crate::auxs::insert(
            &db,
            &AuxEntity::new(AuxKind::Supplier, "S001", "南方原材料厂"),
        )
        .unwrap();
        // 银行科目本身带「银行账户」辅助，不建一个过不了记账校验
        crate::auxs::insert(
            &db,
            &AuxEntity::new(AuxKind::Bank, "B01", "工行基本户"),
        )
        .unwrap();

        // ① 进账，摘要含客户名 → 应生成
        // ② 支出，摘要含供应商名 → 应生成
        // ③ 进账但认不出对方（"网银转入"） → 必须跳过
        insert(
            &db,
            &Statement {
                biz_date: NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
                summary: "收 华东商贸有限公司 货款".into(),
                settle_no: "SN1".into(),
                debit: Money::parse("1000").unwrap(),
                credit: Money::ZERO,
                balance: Money::parse("1000").unwrap(),
                ..mk_stmt(period)
            },
        )
        .unwrap();
        insert(
            &db,
            &Statement {
                biz_date: NaiveDate::from_ymd_opt(2026, 1, 8).unwrap(),
                summary: "付 南方原材料厂 采购款".into(),
                settle_no: "SN2".into(),
                debit: Money::ZERO,
                credit: Money::parse("600").unwrap(),
                balance: Money::parse("400").unwrap(),
                ..mk_stmt(period)
            },
        )
        .unwrap();
        insert(
            &db,
            &Statement {
                biz_date: NaiveDate::from_ymd_opt(2026, 1, 9).unwrap(),
                summary: "网银转入 手续费".into(),
                settle_no: "SN3".into(),
                debit: Money::parse("20").unwrap(),
                credit: Money::ZERO,
                balance: Money::parse("420").unwrap(),
                ..mk_stmt(period)
            },
        )
        .unwrap();

        let r = gen_vouchers(&db, period, "100201", "u1").unwrap();
        assert_eq!(r.generated, 2, "只应生成认得出对方的 2 张：{r:?}");
        assert_eq!(r.linked, 2, "生成后应把流水勾上：{r:?}");
        assert_eq!(r.skipped.len(), 1, "认不出对方的必须留下：{r:?}");
        assert!(
            r.skipped[0].reason.contains("认不出"),
            "跳过原因要说清为什么：{:?}",
            r.skipped[0]
        );

        // 生成的凭证必须是**草稿**（不绕过审核/出纳签字）
        for vid in &r.voucher_ids {
            let v = crate::vouchers::get(&db, *vid).unwrap().unwrap();
            assert_eq!(
                v.status,
                VoucherStatus::Draft,
                "自动生成的凭证必须是草稿，不能绕过内控"
            );
            assert_eq!(v.entries.len(), 2, "只含银行侧 + 往来侧两行");
            assert!(v.balanced(), "生成的凭证必须借贷平衡：{:?}", v.entries);
            // 银行侧那条必须在，且带银行科目
            assert!(
                v.entries.iter().any(|e| e.account_code == "100201"),
                "应含银行科目那一行：{:?}",
                v.entries
            );
        }

        // 幂等：再跑一次不应重复生成（流水已被勾上）
        let r2 = gen_vouchers(&db, period, "100201", "u1").unwrap();
        assert_eq!(r2.generated, 0, "已勾对的流水不该再生成：{r2:?}");
        let total: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM voucher", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2, "库里应当只有 2 张凭证：{total}");
    }

    #[allow(dead_code)]
    fn stmt(date: &str, summary: &str, no: &str, signed: &str, bal: &str) -> Statement {
        let v = Money::parse(signed).unwrap();
        Statement {
            id: 0,
            period: Period::new(2026, 1).unwrap(),
            account_code: "100201".into(),
            biz_date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            summary: summary.into(),
            settle_no: no.into(),
            debit: if v.is_positive() { v } else { Money::ZERO },
            credit: if v.is_negative() { v.abs() } else { Money::ZERO },
            balance: Money::parse(bal).unwrap(),
            entry_id: None,
            matched_at: None,
            matched_by: None,
        }
    }

    #[test]
    fn csv_helpers() {
        assert_eq!(
            parse_date("2026-01-05").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
        );
        assert_eq!(
            parse_date("2026/1/5").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
        );
        assert_eq!(
            parse_date("20260105").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
        );
        assert_eq!(parse_money("1,234.56"), Money::parse("1234.56").unwrap());
        assert_eq!(parse_money("(500)"), Money::parse("-500").unwrap());
        assert_eq!(parse_money("￥88.80"), Money::parse("88.80").unwrap());
        let f = split_csv("a,\"b,c\",d");
        assert_eq!(f, vec!["a", "b,c", "d"]);
    }

    #[test]
    fn import_and_auto_match() {
        let db = tmpdb("match");
        let p = Period::new(2026, 1).unwrap();

        // 100201 / 1122 在内置科目表里已有，直接用
        // 一张已记账的收款凭证
        let d = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let mut v = Voucher::new(p, d, "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("1000").unwrap(),
            settle_no: Some("SN001".into()),
            aux: fincore::voucher::AuxRef {
                bank: Some("BANK01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "100201", "收货款")
        });
        v.push_entry(Entry {
            credit: Money::parse("1000").unwrap(),
            aux: fincore::voucher::AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "112201", "收货款")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "poster").unwrap();
        let _ = VoucherStatus::Posted;

        // 导入对账单（6 列格式）
        let csv = "业务日期,摘要,结算号,借方发生额,贷方发生额,余额\n\
                   2026-01-06,收到货款,SN001,1000.00,0.00,101000.00\n\
                   2026-01-20,支付手续费,,0.00,15.00,100985.00\n";
        let (n, warns) = import_csv(&db, p, "100201", csv).unwrap();
        assert_eq!(n, 2, "warnings={warns:?}");
        assert!(warns.is_empty());

        let res = auto_match(&db, p, "100201", 3, "tester").unwrap();
        assert_eq!(res.by_no, 1);
        assert_eq!(res.matched, 1);

        let rows = list(&db, p, "100201").unwrap();
        assert!(rows[0].matched());
        assert!(!rows[1].matched());

        // 余额调节表：银行侧 100985 + 0 - 0；企业侧 1000 + 0 - 15
        let rec = reconcile(&db, p, "100201").unwrap();
        assert_eq!(rec.bank_balance, Money::parse("100985").unwrap());
        assert_eq!(rec.book_balance, Money::parse("1000").unwrap());
        assert_eq!(rec.bank_only_out.len(), 1); // 银行已付企业未记：手续费 15
        assert_eq!(rec.bank_adjusted, Money::parse("100985").unwrap());
        assert_eq!(rec.book_adjusted, Money::parse("985").unwrap());
        assert!(!rec.balanced()); // 还没记手续费，自然不平
        assert_eq!(rec.diff(), Money::parse("100000").unwrap());

        let _ = vid;
    }

    #[test]
    fn import_single_amount_column() {
        let db = tmpdb("single");
        let p = Period::new(2026, 1).unwrap();
        let csv = "2026-01-06,收到货款,SN001,1000.00,101000.00\n\
                   2026-01-20,支付手续费,,-15.00,100985.00\n";
        let (n, _) = import_csv(&db, p, "100201", csv).unwrap();
        assert_eq!(n, 2);
        let rows = list(&db, p, "100201").unwrap();
        assert_eq!(rows[0].debit, Money::parse("1000").unwrap());
        assert_eq!(rows[1].credit, Money::parse("15").unwrap());
    }
}
