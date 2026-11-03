//! 应收应付对账批次（对标「先把对方给的对账金额录进来，对好账再落账」）
//!
//! ## 它解决什么
//!
//! 小加工厂每月要跟客户、供应商对一次账：对方发来一张表「这个月你还欠我多少」，
//! 我们这边逐个核对。核对过程中会改数、会有争议、会有几户对不上要打电话问——
//! 这一段时间里的数字**不能直接进账**，否则报表上会先出现一堆错数，第二天再改一遍。
//!
//! 以前只有两条路，都不好用：
//! - 直接录凭证：录进去就进了报表，对不上账时得逐张作废重录；
//! - 用「导入凭证」当草稿堆：能改，但散在凭证列表里，看不到「20 户对上 17 户」这种进度，
//!   也没有「对方说 3,000 / 我账面 2,800 / 差 200」的逐户对照。
//!
//! 本模块给一个**批次**容器：
//! 1. 建批次（选应收/应付、对账期间）；
//! 2. 把对方表上的「客商 + 金额」粘贴进来（也可逐行加改）；
//! 3. 账面余额由系统按**已记账凭证**算，逐户显示差额；
//! 4. 对得上的打勾，对不上的改数或标「有争议」——这一步随时可改，因为**没进账**；
//! 5. 点「确认这批」：差额≠0 且无争议的户，汇总成**一张调整凭证**（草稿）。
//!
//! ## 三条口径（与全仓一致，别改）
//!
//! - **账面余额只取已记账分录**（`status='posted'`，全仓 H-3 定案），与试算平衡、
//!   账簿、往来核销、账龄完全同源。草稿不进账面——否则「还没入账的凭证」会出现在
//!   对账结果里，而它随时能被删掉。
//! - **往来期初明细算进账面余额**（`arap_opening` 影子挂账，与账龄同一规则）：
//!   老账搬迁时录的期初单据，客户欠的钱一样要核。漏了它，账面会比实际少一截，
//!   用户会以为系统算错了。
//! - **确认只生成草稿凭证，绝不自动记账**（与 `bank::gen_vouchers` 同一条红线）：
//!   调整凭证要过审核、要过出纳签字（若启用），必须由人再过一道手。
//!
//! ## 差额方向
//!
//! `差额 = 对方金额 − 账面余额`，都是**正数表示「对方欠我 / 我欠对方」**：
//! - 应收：差 +200（对方说欠 3,000，账面 2,800）→ 少记了应收 → 借 应收 200；
//! - 应付：差 +200（供应商说我还欠 3,000，账面 2,800）→ 少记了应付 → 贷 应付 200。
//!
//! 对方科目由用户选（默认 660305 其他），一张批次只出一张凭证，对方科目按净额一行。

use std::collections::BTreeMap;

use fincore::{AuxKind, AuxRef, Direction, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::{OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::{Db, DbResult};

/// 应收
pub const KIND_AR: &str = "ar";
/// 应付
pub const KIND_AP: &str = "ap";
/// 对账中（可改）
pub const ST_OPEN: &str = "open";
/// 已确认（已生成调整凭证，仍是草稿）
pub const ST_CONFIRMED: &str = "confirmed";
/// 已撤销
pub const ST_CANCELLED: &str = "cancelled";

const B_COLS: &str = "id,kind,account,post_account,title,period,status,memo,adjust_account,\
     COALESCE(voucher_id,0),created_by,created_at,confirmed_by,confirmed_at";
const L_COLS: &str = "id,batch_id,party_code,party_name,claim,book,diff,disputed,note";

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

/// 一个对账批次
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Batch {
    pub id: i64,
    /// ar / ap
    pub kind: String,
    /// 算账面余额用的科目（可以是上级，如 1122）
    pub account: String,
    /// 调账凭证用的末级科目（如 112201）
    pub post_account: String,
    /// 批次名，如「2026-11 客户对账」
    pub title: String,
    pub period: Period,
    /// open / confirmed / cancelled
    pub status: String,
    pub memo: String,
    /// 差额对方科目（确认时写入）
    pub adjust_account: String,
    /// 确认时生成的调整凭证（草稿）
    pub voucher_id: Option<i64>,
    pub created_by: String,
    pub created_at: String,
    pub confirmed_by: String,
    pub confirmed_at: String,
    /// 户数
    pub party_count: i64,
    /// 对方金额合计
    pub claim_total: Money,
    /// 差额合计
    pub diff_total: Money,
}

/// 一户一行
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Line {
    pub id: i64,
    pub batch_id: i64,
    pub party_code: String,
    pub party_name: String,
    /// 对方对账单上的金额（正数）
    pub claim: Money,
    /// 账面余额（按已记账凭证 + 往来期初算，正数表示对方欠我/我欠对方）
    pub book: Money,
    /// claim − book
    pub diff: Money,
    /// 标了「有争议」，确认时跳过、不调账
    pub disputed: bool,
    pub note: String,
}

impl Line {
    /// 对上了：差额为 0 且没有争议
    pub fn agreed(&self) -> bool {
        !self.disputed && self.diff.is_zero()
    }
}

/// 批次体检数
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub party_count: usize,
    /// 对上的户数（差额 0 且无争议）
    pub agreed: usize,
    /// 标了有争议的户数
    pub disputed: usize,
    /// 会被调账的户数（差额≠0 且无争议）
    pub need_adjust: usize,
    pub claim_total: Money,
    pub book_total: Money,
    /// 差额合计（带符号：正 = 对方金额大于账面）
    pub diff_total: Money,
}

impl Summary {
    fn of(lines: &[Line]) -> Summary {
        let mut s = Summary {
            party_count: lines.len(),
            ..Summary::default()
        };
        for l in lines {
            s.claim_total += l.claim;
            s.book_total += l.book;
            s.diff_total += l.diff;
            if l.disputed {
                s.disputed += 1;
            } else if l.diff.is_zero() {
                s.agreed += 1;
            } else {
                s.need_adjust += 1;
            }
        }
        s
    }
}

/// 批次详情
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Detail {
    pub batch: Batch,
    pub lines: Vec<Line>,
    pub summary: Summary,
}

/// 确认结果
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmResult {
    /// 生成的调整凭证（草稿）；全部对上时为 None
    pub voucher_id: Option<i64>,
    /// 调账户数
    pub adjusted: usize,
    /// 因有争议而跳过的户数
    pub skipped_disputed: usize,
    /// 调账净额（带符号）
    pub diff_total: Money,
}

fn map_batch(r: &Row) -> rusqlite::Result<Batch> {
    Ok(Batch {
        id: r.get(0)?,
        kind: r.get(1)?,
        account: r.get(2)?,
        post_account: r.get(3)?,
        title: r.get(4)?,
        period: Period::from_ymm(r.get(5)?),
        status: r.get(6)?,
        memo: r.get(7)?,
        adjust_account: r.get(8)?,
        voucher_id: match r.get::<_, i64>(9)? {
            0 => None,
            v => Some(v),
        },
        created_by: r.get(10)?,
        created_at: r.get(11)?,
        confirmed_by: r.get(12)?,
        confirmed_at: r.get(13)?,
        party_count: 0,
        claim_total: Money::ZERO,
        diff_total: Money::ZERO,
    })
}

fn map_line(r: &Row) -> rusqlite::Result<Line> {
    Ok(Line {
        id: r.get(0)?,
        batch_id: r.get(1)?,
        party_code: r.get(2)?,
        party_name: r.get(3)?,
        claim: m(&r.get::<_, String>(4)?),
        book: m(&r.get::<_, String>(5)?),
        diff: m(&r.get::<_, String>(6)?),
        disputed: r.get::<_, i64>(7)? != 0,
        note: r.get(8)?,
    })
}

/// 应收/应付的默认科目
fn default_accounts(kind: &str) -> (&'static str, &'static str) {
    if kind == KIND_AP {
        ("2202", "220201")
    } else {
        ("1122", "112201")
    }
}

fn check_kind(kind: &str) -> DbResult<()> {
    if kind == KIND_AR || kind == KIND_AP {
        Ok(())
    } else {
        Err(fincore::FinError::validate(format!(
            "对账类型只能是 ar（应收）或 ap（应付），收到「{kind}」"
        ))
        .into())
    }
}

fn kind_label(kind: &str) -> &'static str {
    if kind == KIND_AP {
        "应付"
    } else {
        "应收"
    }
}

/// 新建一个对账批次
pub fn create(
    db: &Db,
    kind: &str,
    period: Period,
    title: &str,
    account: &str,
    post_account: &str,
    who: &str,
) -> DbResult<i64> {
    check_kind(kind)?;
    let (def_acct, def_post) = default_accounts(kind);
    let account = if account.trim().is_empty() {
        def_acct.to_string()
    } else {
        account.trim().to_string()
    };
    let post_account = if post_account.trim().is_empty() {
        def_post.to_string()
    } else {
        post_account.trim().to_string()
    };
    let chart = crate::accounts::chart(db)?;
    if chart.get(&account).is_none() {
        return Err(fincore::FinError::not_found(format!("科目 {account}")).into());
    }
    if chart.get(&post_account).is_none() {
        return Err(fincore::FinError::not_found(format!("科目 {post_account}")).into());
    }
    if !chart.is_leaf(&post_account) {
        return Err(fincore::FinError::validate(format!(
            "调账科目 {post_account} 不是末级科目，凭证上不能直接用"
        ))
        .into());
    }
    let title = if title.trim().is_empty() {
        format!("{} {}对账", period.label(), kind_label(kind))
    } else {
        title.trim().to_string()
    };
    let conn = db.conn();
    conn.execute(
        "INSERT INTO arap_recon_batch(kind,account,post_account,title,period,status,memo,\
         adjust_account,voucher_id,created_by,created_at,confirmed_by,confirmed_at)
         VALUES(?1,?2,?3,?4,?5,?6,'','',NULL,?7,?8,'','')",
        rusqlite::params![
            kind,
            account,
            post_account,
            title,
            period.ymm(),
            ST_OPEN,
            who,
            now()
        ],
    )?;
    let id = conn.last_insert_rowid();
    db.log(
        who,
        "对账",
        &format!("新建{}对账批次", kind_label(kind)),
        &format!("{title}（#{id}）"),
    )?;
    Ok(id)
}

/// 批次列表（按时间倒序）。`kind` / `status` 为 None 表示不过滤。
pub fn list(db: &Db, kind: Option<&str>, status: Option<&str>) -> DbResult<Vec<Batch>> {
    let conn = db.conn();
    let mut sql = format!("SELECT {B_COLS} FROM arap_recon_batch WHERE 1=1");
    let mut args: Vec<String> = Vec::new();
    if let Some(k) = kind.filter(|s| !s.trim().is_empty()) {
        args.push(k.trim().to_string());
        sql.push_str(&format!(" AND kind=?{}", args.len()));
    }
    if let Some(s) = status.filter(|s| !s.trim().is_empty()) {
        args.push(s.trim().to_string());
        sql.push_str(&format!(" AND status=?{}", args.len()));
    }
    sql.push_str(" ORDER BY id DESC");
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params_from_iter(args.iter()), map_batch)?;
    let mut out: Vec<Batch> = rows.collect::<Result<_, _>>()?;
    if out.is_empty() {
        return Ok(out);
    }
    // 合计在 Rust 侧算：金额存 TEXT，SQLite 的 SUM 会走浮点
    let mut agg: BTreeMap<i64, (i64, Money, Money)> = BTreeMap::new();
    let mut st2 = conn.prepare("SELECT batch_id,claim,diff FROM arap_recon_line")?;
    let it = st2.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            m(&r.get::<_, String>(1)?),
            m(&r.get::<_, String>(2)?),
        ))
    })?;
    for row in it {
        let (bid, claim, diff) = row?;
        let e = agg.entry(bid).or_insert((0, Money::ZERO, Money::ZERO));
        e.0 += 1;
        e.1 += claim;
        e.2 += diff;
    }
    for b in out.iter_mut() {
        if let Some((n, claim, diff)) = agg.get(&b.id) {
            b.party_count = *n;
            b.claim_total = *claim;
            b.diff_total = *diff;
        }
    }
    Ok(out)
}

/// 读批次（不含明细）
pub fn get_batch(db: &Db, id: i64) -> DbResult<Option<Batch>> {
    let b = db
        .conn()
        .query_row(
            &format!("SELECT {B_COLS} FROM arap_recon_batch WHERE id=?1"),
            [id],
            map_batch,
        )
        .optional()?;
    Ok(b)
}

/// 读批次详情（含明细与体检数）
pub fn get(db: &Db, id: i64) -> DbResult<Option<Detail>> {
    let Some(mut batch) = get_batch(db, id)? else {
        return Ok(None);
    };
    let lines = lines_of(db, id)?;
    batch.party_count = lines.len() as i64;
    let summary = Summary::of(&lines);
    batch.claim_total = summary.claim_total;
    batch.diff_total = summary.diff_total;
    Ok(Some(Detail {
        batch,
        lines,
        summary,
    }))
}

/// 某批次的明细（按对方金额降序，方便一眼看到大额）
pub fn lines_of(db: &Db, batch_id: i64) -> DbResult<Vec<Line>> {
    let conn = db.conn();
    let mut st = conn.prepare(&format!(
        "SELECT {L_COLS} FROM arap_recon_line WHERE batch_id=?1 ORDER BY id"
    ))?;
    let rows = st.query_map([batch_id], map_line)?;
    let mut out: Vec<Line> = rows.collect::<Result<_, _>>()?;
    out.sort_by(|a, b| b.claim.cmp(&a.claim).then(a.party_code.cmp(&b.party_code)));
    Ok(out)
}

fn open_batch(db: &Db, id: i64) -> DbResult<Batch> {
    let b = get_batch(db, id)?
        .ok_or_else(|| fincore::FinError::not_found(format!("对账批次 #{id}")))?;
    if b.status != ST_OPEN {
        return Err(fincore::FinError::state(format!(
            "批次「{}」当前状态是{}，只有「对账中」的批次才能改",
            b.title,
            status_label(&b.status)
        ))
        .into());
    }
    Ok(b)
}

fn status_label(s: &str) -> &'static str {
    match s {
        ST_CONFIRMED => "已确认",
        ST_CANCELLED => "已撤销",
        _ => "对账中",
    }
}

// ---------------- 账面余额 ----------------

/// 某往来科目按户的账面余额：`对方编码 → (名称, 余额)`
///
/// 余额方向统一成**正数 = 对方欠我（应收）/ 我欠对方（应付）**，与对账单、账龄
/// 的展示口径一致。分录侧按科目真实方向归一（贷方性质科目取反），期初单据本来就
/// 是按该方向存的正数，不再翻第二次——翻两次就回到负数、整张应付表变空白。
pub fn book_balances(db: &Db, account: &str, upto: Period) -> DbResult<BTreeMap<String, (String, Money)>> {
    let chart = crate::accounts::chart(db)?;
    let kind = crate::settle::party_kind_of(&chart, account);
    let flip = chart
        .get(account)
        .map(|a| a.dir == Direction::Credit)
        .unwrap_or_else(|| account.starts_with('2'));
    let mut out: BTreeMap<String, Money> = BTreeMap::new();
    for e in crate::settle::open_entries(db, account, upto, false)? {
        let code = crate::balances::aux_key_value_of(&e.aux_key, kind);
        if code.trim().is_empty() {
            continue;
        }
        let signed = if flip { e.signed().negated() } else { e.signed() };
        let open = e.open();
        let v = if signed.is_negative() {
            open.negated()
        } else {
            open
        };
        *out.entry(code).or_insert(Money::ZERO) += v;
    }
    // 往来期初明细（老账搬迁的影子挂账）：与账龄同一规则计入
    let want = if kind == AuxKind::Supplier { "ap" } else { "ar" };
    for o in crate::settle::arap_opening_list(db, Some(want))? {
        if Period::from_date(
            chrono::NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d").unwrap_or_else(|_| {
                chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("基准日期必然合法")
            }),
        )
        .ymm()
            > upto.ymm()
        {
            continue;
        }
        *out.entry(o.party_code.clone()).or_insert(Money::ZERO) += o.amount;
    }
    let names = crate::auxs::name_map(db, kind)?;
    Ok(out
        .into_iter()
        .map(|(code, bal)| {
            let name = names.get(&code).cloned().unwrap_or_default();
            (code, (name, bal))
        })
        .collect())
}

// ---------------- 录数与改数 ----------------

/// 逐行加/改一户（同一户重复录入 = 覆盖金额，不新增重复行）
pub fn upsert_line(db: &Db, batch_id: i64, party: &str, claim: Money, note: &str) -> DbResult<i64> {
    let b = open_batch(db, batch_id)?;
    let code = party.trim();
    if code.is_empty() {
        return Err(fincore::FinError::validate("客商编码不能为空").into());
    }
    if claim.is_negative() {
        return Err(fincore::FinError::validate(
            "对账金额填正数；预付/预收不在这张表里对",
        )
        .into());
    }
    let chart = crate::accounts::chart(db)?;
    let kind = crate::settle::party_kind_of(&chart, &b.account);
    let name = crate::auxs::get(db, kind, code)?
        .map(|e| e.name)
        .unwrap_or_default();
    let book = book_balances(db, &b.account, b.period)?
        .get(code)
        .map(|(_, v)| *v)
        .unwrap_or(Money::ZERO);
    let diff = claim - book;
    let conn = db.conn();
    let existed: Option<i64> = conn
        .query_row(
            "SELECT id FROM arap_recon_line WHERE batch_id=?1 AND party_code=?2",
            rusqlite::params![batch_id, code],
            |r| r.get(0),
        )
        .optional()?;
    match existed {
        Some(id) => {
            conn.execute(
                "UPDATE arap_recon_line SET claim=?1,book=?2,diff=?3,note=?4,\
                 party_name=CASE WHEN ?5='' THEN party_name ELSE ?5 END WHERE id=?6",
                rusqlite::params![
                    claim.fmt_exact(),
                    book.fmt_exact(),
                    diff.fmt_exact(),
                    note,
                    name,
                    id
                ],
            )?;
            Ok(id)
        }
        None => {
            conn.execute(
                "INSERT INTO arap_recon_line(batch_id,party_code,party_name,claim,book,diff,disputed,note)
                 VALUES(?1,?2,?3,?4,?5,?6,0,?7)",
                rusqlite::params![
                    batch_id,
                    code,
                    name,
                    claim.fmt_exact(),
                    book.fmt_exact(),
                    diff.fmt_exact(),
                    note
                ],
            )?;
            Ok(conn.last_insert_rowid())
        }
    }
}

/// 改一行的金额 / 争议标记 / 备注
pub fn set_line(db: &Db, line_id: i64, claim: Money, disputed: bool, note: &str) -> DbResult<()> {
    let conn = db.conn();
    let batch_id: i64 = conn
        .query_row(
            "SELECT batch_id FROM arap_recon_line WHERE id=?1",
            [line_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found(format!("对账明细 #{line_id}")))?;
    open_batch(db, batch_id)?;
    if claim.is_negative() {
        return Err(fincore::FinError::validate("对账金额填正数").into());
    }
    let book: Money = conn.query_row(
        "SELECT book FROM arap_recon_line WHERE id=?1",
        [line_id],
        |r| Ok(m(&r.get::<_, String>(0)?)),
    )?;
    conn.execute(
        "UPDATE arap_recon_line SET claim=?1,diff=?2,disputed=?3,note=?4 WHERE id=?5",
        rusqlite::params![
            claim.fmt_exact(),
            (claim - book).fmt_exact(),
            i64::from(disputed),
            note,
            line_id
        ],
    )?;
    Ok(())
}

/// 删一行
pub fn delete_line(db: &Db, line_id: i64) -> DbResult<()> {
    let conn = db.conn();
    let batch_id: i64 = conn
        .query_row(
            "SELECT batch_id FROM arap_recon_line WHERE id=?1",
            [line_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found(format!("对账明细 #{line_id}")))?;
    open_batch(db, batch_id)?;
    conn.execute("DELETE FROM arap_recon_line WHERE id=?1", [line_id])?;
    Ok(())
}

/// 重算全部行的账面余额与差额（凭证补录后点一下）
///
/// 返回重算的行数。**不动用户填的对方金额**，只刷新 book / diff。
pub fn sync_book(db: &Db, batch_id: i64) -> DbResult<usize> {
    let b = open_batch(db, batch_id)?;
    let books = book_balances(db, &b.account, b.period)?;
    let conn = db.conn();
    let mut st = conn.prepare("SELECT id,party_code,claim FROM arap_recon_line WHERE batch_id=?1")?;
    let rows = st.query_map([batch_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            m(&r.get::<_, String>(2)?),
        ))
    })?;
    let mut todo: Vec<(i64, Money, Money)> = Vec::new();
    for row in rows {
        let (id, code, claim) = row?;
        let book = books
            .get(&code)
            .map(|(_, v)| *v)
            .unwrap_or(Money::ZERO);
        todo.push((id, book, claim - book));
    }
    drop(st);
    for (id, book, diff) in &todo {
        conn.execute(
            "UPDATE arap_recon_line SET book=?1,diff=?2 WHERE id=?3",
            rusqlite::params![book.fmt_exact(), diff.fmt_exact(), id],
        )?;
    }
    Ok(todo.len())
}

/// 粘贴导入：列顺序 `客商编码, 对账金额[, 备注]`
///
/// 表头（含「客商」「编码」等字样）自动跳过；同一户重复出现按最后一行为准（覆盖）。
/// 编码不在档案里不拦——先把数录进来，档案可以随后补，否则一次导入会全被挡回去。
pub fn import_lines(db: &Db, batch_id: i64, text: &str, who: &str) -> DbResult<crate::imports::ImportResult> {
    let b = open_batch(db, batch_id)?;
    let chart = crate::accounts::chart(db)?;
    let kind = crate::settle::party_kind_of(&chart, &b.account);
    let names = crate::auxs::name_map(db, kind)?;
    let mut res = crate::imports::ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    let mut lines = 0usize;
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        lines += 1;
        let f = crate::imports::split_csv(line);
        if f.is_empty() {
            continue;
        }
        let code = f.first().map(|s| s.trim()).unwrap_or_default();
        if code.is_empty() {
            continue;
        }
        // 表头行
        if code.contains("客商") || code.contains("编码") || code.eq_ignore_ascii_case("code") {
            continue;
        }
        let claim_s = f.get(1).map(|s| s.trim()).unwrap_or_default();
        if claim_s.is_empty() {
            res.warnings
                .push(format!("第 {lines} 行：没有对账金额，已跳过"));
            res.skipped += 1;
            continue;
        }
        let claim = match Money::parse(claim_s) {
            Ok(v) => v,
            Err(_) => {
                res.warnings.push(format!(
                    "第 {lines} 行：金额「{claim_s}」不是数字，已跳过"
                ));
                res.skipped += 1;
                continue;
            }
        };
        if claim.is_negative() {
            res.warnings.push(format!(
                "第 {lines} 行：金额是负数，已跳过 —— 预付/预收属于 1123/2203，不在这张表里对"
            ));
            res.skipped += 1;
            continue;
        }
        let note = f.get(2).map(|s| s.trim()).unwrap_or_default();
        if !names.contains_key(code) {
            res.warnings.push(format!(
                "第 {lines} 行：档案里没有{}{code}，已照录；记得在「辅助档案」里补上名称",
                kind.label()
            ));
        }
        upsert_line(db, batch_id, code, claim, note)?;
        res.ok += 1;
    }
    if res.ok > 0 {
        db.log(
            who,
            "对账",
            &format!("导入{}对账明细", kind_label(&b.kind)),
            &format!("{} 共 {} 户", b.title, res.ok),
        )?;
    }
    Ok(res)
}

// ---------------- 确认 / 撤销 / 删除 ----------------

/// 确认这一批：差额≠0 且无争议的户汇总出一张**草稿**调整凭证
///
/// - 有争议的行跳过（留到下一批）；
/// - 差额全为 0 → 不生成凭证，批次照样标记已确认（对完账本身就是结果）；
/// - 凭证一律草稿，必须人再过一道审核/记账（与 `bank::gen_vouchers` 同一条红线）。
pub fn confirm(db: &Db, batch_id: i64, who: &str, adjust_account: &str) -> DbResult<ConfirmResult> {
    let b = open_batch(db, batch_id)?;
    let lines = lines_of(db, batch_id)?;
    let mut need: Vec<&Line> = lines
        .iter()
        .filter(|l| !l.disputed && !l.diff.is_zero())
        .collect();
    let skipped_disputed = lines.iter().filter(|l| l.disputed).count();
    let diff_total: Money = need.iter().map(|l| l.diff).sum();

    // 全部对上：直接标记，不生成凭证
    if need.is_empty() {
        let conn = db.conn();
        conn.execute(
            "UPDATE arap_recon_batch SET status=?1,confirmed_by=?2,confirmed_at=?3 WHERE id=?4",
            rusqlite::params![ST_CONFIRMED, who, now(), batch_id],
        )?;
        db.log(
            who,
            "对账",
            &format!("确认{}对账批次", kind_label(&b.kind)),
            &format!("{} 全部对上，未生成凭证", b.title),
        )?;
        return Ok(ConfirmResult {
            voucher_id: None,
            adjusted: 0,
            skipped_disputed,
            diff_total: Money::ZERO,
        });
    }

    let adjust = adjust_account.trim();
    if adjust.is_empty() {
        return Err(fincore::FinError::validate(
            "差额需要用对方科目调账，请先选一个科目（如 660305 其他 / 6711 营业外支出）",
        )
        .into());
    }
    let chart = crate::accounts::chart(db)?;
    if chart.get(adjust).is_none() {
        return Err(fincore::FinError::not_found(format!("科目 {adjust}")).into());
    }
    if !chart.is_leaf(adjust) {
        return Err(fincore::FinError::validate(format!(
            "对方科目 {adjust} 不是末级科目"
        ))
        .into());
    }
    let ar = b.kind != KIND_AP;
    let summary_text = format!("{}对账调整（{}）", kind_label(&b.kind), b.title);

    let tx = db.write_tx()?;
    let word = db
        .voucher_words()
        .first()
        .cloned()
        .unwrap_or_else(|| "记".to_string());
    let no = crate::vouchers::next_no_of(&tx, b.period, &word)?;
    let mut v = Voucher::new(b.period, b.period.last_day(), word, no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = summary_text.clone();

    need.sort_by(|a, b| a.party_code.cmp(&b.party_code));
    let mut line_no = 1i32;
    // 应收：正差额记借方；应付：正差额记贷方。记账侧合计决定对方科目方向。
    let mut side_total = Money::ZERO;
    for l in &need {
        let d = l.diff;
        let on_debit = if ar { d.is_positive() } else { d.is_negative() };
        let mut e = Entry::new(line_no, b.post_account.clone(), &summary_text);
        if on_debit {
            e.debit = d.abs();
            side_total += d.abs();
        } else {
            e.credit = d.abs();
            side_total -= d.abs();
        }
        fill_party_aux(&chart, &mut e, &b.post_account, &l.party_code)?;
        v.push_entry(e);
        line_no += 1;
    }
    // 对方科目按净额一行；净额方向与往来侧相反
    let mut bal = Entry::new(line_no, adjust, &summary_text);
    if side_total.is_positive() {
        bal.credit = side_total;
    } else {
        bal.debit = side_total.abs();
    }
    fill_party_aux(&chart, &mut bal, adjust, "")?;
    v.push_entry(bal);

    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    tx.execute(
        "UPDATE arap_recon_batch SET status=?1,adjust_account=?2,voucher_id=?3,\
         confirmed_by=?4,confirmed_at=?5 WHERE id=?6",
        rusqlite::params![ST_CONFIRMED, adjust, vid, who, now(), batch_id],
    )?;
    crate::log_on(
        &tx,
        who,
        "对账",
        &format!("确认{}对账批次", kind_label(&b.kind)),
        &format!(
            "{} 调账 {} 户 差额 {} 凭证#{}",
            b.title,
            need.len(),
            diff_total.fmt_money(),
            vid
        ),
    )?;
    tx.commit()?;
    Ok(ConfirmResult {
        voucher_id: Some(vid),
        adjusted: need.len(),
        skipped_disputed,
        diff_total,
    })
}

/// 给分录补上往来辅助核算
///
/// 调账科目若要求别的必填辅助（项目/存货…），这里填不出来，直接说清楚让用户换科目——
/// 静默留空会被凭证校验挡在保存之外，报错信息还指不到真正原因。
fn fill_party_aux(chart: &fincore::Chart, e: &mut Entry, code: &str, party: &str) -> DbResult<()> {
    let acct = chart
        .get(code)
        .ok_or_else(|| fincore::FinError::not_found(format!("科目 {code}")))?;
    for k in acct.aux.list() {
        match k {
            AuxKind::Customer | AuxKind::Supplier => {
                if party.trim().is_empty() {
                    return Err(fincore::FinError::validate(format!(
                        "科目 {code} 核算{}，调账凭证上必须填往来单位；\
                         请换一个不核算往来的对方科目",
                        k.label()
                    ))
                    .into());
                }
                e.aux.set(k, Some(party.to_string()));
            }
            // 部门可选（见 fincore::engine::validate），其余维度无法自动确定
            AuxKind::Dept => {}
            other => {
                return Err(fincore::FinError::validate(format!(
                    "科目 {code} 核算{}，系统自动生成的调整凭证填不出来；\
                     请换一个只核算往来的对方科目",
                    other.label()
                ))
                .into());
            }
        }
    }
    Ok(())
}

/// 打回重对：删除未记账的调整凭证，把批次退回「对账中」
///
/// 凭证已经记账（或已被审核）时不硬来——那要人先反记账/作废，本函数只报错。
pub fn reopen(db: &Db, batch_id: i64, who: &str) -> DbResult<()> {
    let b = get_batch(db, batch_id)?
        .ok_or_else(|| fincore::FinError::not_found(format!("对账批次 #{batch_id}")))?;
    if b.status != ST_CONFIRMED {
        return Err(fincore::FinError::state(format!(
            "批次「{}」当前是{}，不是「已确认」，不用打回",
            b.title,
            status_label(&b.status)
        ))
        .into());
    }
    if let Some(vid) = b.voucher_id {
        let v = crate::vouchers::get(db, vid)?;
        match v {
            Some(v) if v.status == fincore::VoucherStatus::Draft => {
                crate::vouchers::delete(db, vid)?;
            }
            Some(v) => {
                return Err(fincore::FinError::state(format!(
                    "调整凭证#{} 已经是「{}」，请先反记账或作废，再打回重对",
                    vid,
                    v.status.label()
                ))
                .into());
            }
            None => {}
        }
    }
    let conn = db.conn();
    conn.execute(
        "UPDATE arap_recon_batch SET status=?1,voucher_id=NULL,confirmed_by='',confirmed_at='' \
         WHERE id=?2",
        rusqlite::params![ST_OPEN, batch_id],
    )?;
    db.log(
        who,
        "对账",
        "打回重对",
        &format!("{}（#{batch_id}）", b.title),
    )?;
    Ok(())
}

/// 撤销批次（只有「对账中」的能撤销，已确认的先打回）
pub fn cancel(db: &Db, batch_id: i64, who: &str) -> DbResult<()> {
    let b = open_batch(db, batch_id)?;
    let conn = db.conn();
    conn.execute(
        "UPDATE arap_recon_batch SET status=?1 WHERE id=?2",
        rusqlite::params![ST_CANCELLED, batch_id],
    )?;
    db.log(
        who,
        "对账",
        "撤销对账批次",
        &format!("{}（#{batch_id}）", b.title),
    )?;
    Ok(())
}

/// 删批次（连明细一起删）。已确认的不许删——它挂着调整凭证。
pub fn delete(db: &Db, batch_id: i64, who: &str) -> DbResult<()> {
    let b = open_batch(db, batch_id)?;
    let conn = db.conn();
    conn.execute("DELETE FROM arap_recon_line WHERE batch_id=?1", [batch_id])?;
    conn.execute("DELETE FROM arap_recon_batch WHERE id=?1", [batch_id])?;
    db.log(
        who,
        "对账",
        "删除对账批次",
        &format!("{}（#{batch_id}）", b.title),
    )?;
    Ok(())
}

/// 校验批次里那些「档案里没有的客商」，返回编码清单
///
/// 界面在确认前调用：对完账才发现编码打错，比一开始就提示贵得多。
pub fn unknown_parties(db: &Db, batch_id: i64) -> DbResult<Vec<String>> {
    let Some(b) = get_batch(db, batch_id)? else {
        return Ok(Vec::new());
    };
    let chart = crate::accounts::chart(db)?;
    let kind = crate::settle::party_kind_of(&chart, &b.account);
    let names = crate::auxs::name_map(db, kind)?;
    let mut out = Vec::new();
    for l in lines_of(db, batch_id)? {
        if !names.contains_key(&l.party_code) {
            out.push(l.party_code);
        }
    }
    Ok(out)
}

/// 按账面余额把所有有来往的户先列进来（金额先填账面数）
///
/// 对账的实际动作是「对方报了数，我核对」，不是「我凭空录二十个客户」。
/// 所以批次一建出来就先按账面列一遍名单，用户只需要把对方报来的数改上去，
/// 差额自己就冒出来了 —— 比让人对着纸单一个个敲编码现实得多。
/// 已经在批次里的户不动（返回新增行数）。
pub fn seed_from_book(db: &Db, batch_id: i64, who: &str) -> DbResult<usize> {
    let b = open_batch(db, batch_id)?;
    let books = book_balances(db, &b.account, b.period)?;
    let have: std::collections::HashSet<String> =
        lines_of(db, batch_id)?.into_iter().map(|l| l.party_code).collect();
    let mut n = 0;
    for (code, (_, bal)) in books {
        if have.contains(&code) || bal.is_zero() {
            continue;
        }
        upsert_line(db, batch_id, &code, bal, "按账面预填")?;
        n += 1;
    }
    if n > 0 {
        db.log(
            who,
            "应收应付对账",
            "按账面列名单",
            &format!("#{batch_id} 新增 {n} 户（{name}）", name = b.title),
        )?;
    }
    Ok(n)
}

/// 账面有余额、但这一批还没录进来的对方单位
///
/// 对账最常见的漏项不是「金额算错」，而是「少了一户」：二十个客户对完十九个，
/// 剩下那户谁也想不起来。界面把这份差额列出来，一眼就能看出漏了谁。
/// 返回 `(编码, 名称, 账面余额)`，按余额绝对值降序。
pub fn book_only(db: &Db, batch_id: i64) -> DbResult<Vec<(String, String, Money)>> {
    let Some(b) = get_batch(db, batch_id)? else {
        return Ok(Vec::new());
    };
    let books = book_balances(db, &b.account, b.period)?;
    let listed: std::collections::HashSet<String> =
        lines_of(db, batch_id)?.into_iter().map(|l| l.party_code).collect();
    let mut out: Vec<(String, String, Money)> = books
        .into_iter()
        .filter(|(code, (_, bal))| !listed.contains(code) && !bal.is_zero())
        .map(|(code, (name, bal))| (code, name, bal))
        .collect();
    out.sort_by(|a, b| b.2.abs().cmp(&a.2.abs()).then(a.0.cmp(&b.0)));
    Ok(out)
}

/// 用户手写的辅助核算键（`customer=C01`）→ 裸编码，供界面显示
pub fn party_of_key(key: &str) -> String {
    AuxRef::from_key(key)
        .customer
        .or_else(|| AuxRef::from_key(key).supplier)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use fincore::account::{Account, AcctCategory};

    fn m_(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    fn p() -> Period {
        Period::new(2026, 1).unwrap()
    }

    /// 差额对方科目：干净的损益科目，不核算任何辅助维度
    fn ensure_adjust(db: &Db) -> &'static str {
        if crate::accounts::get(db, "660199").unwrap().is_none() {
            crate::accounts::insert(db, &Account::new("660199", "对账差额", AcctCategory::Expense))
                .unwrap();
        }
        "660199"
    }

    /// 造一张应收凭证（借 112201 客户 / 贷 660199），返回凭证 id
    fn ar_voucher(db: &Db, no: i32, amt: &str, post_it: bool) -> i64 {
        ensure_adjust(db);
        let p = p();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let mut v = Voucher::new(p, d, "记", no);
        v.prepared_by = "u".into();
        let a = m_(amt);
        v.push_entry(Entry {
            debit: a,
            aux: AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "销售应收")
        });
        v.push_entry(Entry {
            credit: a,
            ..Entry::new(2, "660199", "收入")
        });
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        if post_it {
            crate::vouchers::post(db, vid, "u").unwrap();
        }
        vid
    }

    /// 造一张挂在指定客户名下的已记账应收凭证（多户场景用）
    fn ar_for(db: &Db, party: &str, no: i32, amt: &str) -> i64 {
        ensure_adjust(db);
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let mut v = Voucher::new(p(), d, "记", no);
        v.prepared_by = "u".into();
        let a = m_(amt);
        v.push_entry(Entry {
            debit: a,
            aux: AuxRef {
                customer: Some(party.into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "销售应收")
        });
        v.push_entry(Entry {
            credit: a,
            ..Entry::new(2, "660199", "收入")
        });
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, vid, "u").unwrap();
        vid
    }

    /// 造一张应付凭证（贷 220201 供应商 / 借 660199）
    fn ap_voucher(db: &Db, no: i32, amt: &str, post_it: bool) -> i64 {
        ensure_adjust(db);
        let p = p();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let mut v = Voucher::new(p, d, "记", no);
        v.prepared_by = "u".into();
        let a = m_(amt);
        v.push_entry(Entry {
            credit: a,
            aux: AuxRef {
                supplier: Some("S01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "220201", "采购应付")
        });
        v.push_entry(Entry {
            debit: a,
            ..Entry::new(2, "660199", "采购")
        });
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        if post_it {
            crate::vouchers::post(db, vid, "u").unwrap();
        }
        vid
    }

    #[test]
    fn 新建批次用默认科目且状态是对账中() {
        let db = mem();
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        let b = get_batch(&db, id).unwrap().unwrap();
        assert_eq!(b.kind, KIND_AR);
        assert_eq!(b.account, "1122");
        assert_eq!(b.post_account, "112201");
        assert_eq!(b.status, ST_OPEN);
        assert!(b.title.contains("应收"), "默认批次名要带类型：{}", b.title);
        assert_eq!(b.created_by, "u");
        assert!(list(&db, Some(KIND_AR), None).unwrap().len() == 1);
        assert!(list(&db, Some(KIND_AP), None).unwrap().is_empty());
    }

    #[test]
    fn 账面余额按户汇总且只认已记账凭证() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true);
        ar_voucher(&db, 2, "500", false); // 草稿不进账面
        let books = book_balances(&db, "1122", p()).unwrap();
        assert_eq!(books.get("C01").unwrap().1, m_("1000"), "草稿不该进账面");
    }

    #[test]
    fn 往来期初的旧账也计入账面余额() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true);
        crate::settle::arap_opening_insert(
            &db,
            &crate::settle::ArapOpening {
                id: 0,
                kind: "ar".into(),
                party_code: "C01".into(),
                party_name: "客户甲".into(),
                doc_no: "2025-001".into(),
                doc_date: "2025-12-20".into(),
                amount: m_("500"),
                memo: String::new(),
                created_by: String::new(),
            },
            "u",
        )
        .unwrap();
        let books = book_balances(&db, "1122", p()).unwrap();
        assert_eq!(books.get("C01").unwrap().1, m_("1500"), "期初单据要算进账面");
    }

    #[test]
    fn 粘贴导入跳表头坏行且同一户覆盖() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true);
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        let text = "客商编码,对账金额,备注\nC01,1200,对方口径\nC02,abc,写错了\nC01,1300,改过\n";
        let r = import_lines(&db, id, text, "u").unwrap();
        assert_eq!(r.ok, 2, "C01 出现两次只算改数");
        assert_eq!(r.skipped, 1, "非数字那行要跳过");
        let d = get(&db, id).unwrap().unwrap();
        assert_eq!(d.lines.len(), 1, "同一户不能出两行");
        let l = &d.lines[0];
        assert_eq!(l.party_code, "C01");
        assert_eq!(l.claim, m_("1300"));
        assert_eq!(l.book, m_("1000"));
        assert_eq!(l.diff, m_("300"));
    }

    #[test]
    fn 全部对上时不生成凭证也照样确认() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true);
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        upsert_line(&db, id, "C01", m_("1000"), "").unwrap();
        let before = db.stats().unwrap().0;
        let r = confirm(&db, id, "u", "").unwrap();
        assert!(r.voucher_id.is_none());
        assert_eq!(db.stats().unwrap().0, before, "对上账不该凭空多一张凭证");
        let b = get_batch(&db, id).unwrap().unwrap();
        assert_eq!(b.status, ST_CONFIRMED);
        assert_eq!(b.confirmed_by, "u");
    }

    #[test]
    fn 差额生成一张草稿调整凭证() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true); // 账面 1000
        let adj = ensure_adjust(&db);
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        upsert_line(&db, id, "C01", m_("1200"), "").unwrap(); // 对方说过 1200
        let r = confirm(&db, id, "u", adj).unwrap();
        let vid = r.voucher_id.expect("有差额就该生成凭证");
        assert_eq!(r.adjusted, 1);
        assert_eq!(r.diff_total, m_("200"));
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.status, fincore::VoucherStatus::Draft, "绝不自动记账");
        assert_eq!(v.source, VoucherSource::Business);
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.entries[0].account_code, "112201");
        assert_eq!(v.entries[0].debit, m_("200"));
        assert_eq!(
            v.entries[0].aux.customer.as_deref(),
            Some("C01"),
            "调账分录必须带上客户，否则账面按户还是对不上"
        );
        assert_eq!(v.entries[1].account_code, adj);
        assert_eq!(v.entries[1].credit, m_("200"));
        assert!(v.balanced());
        // 草稿不进账面
        assert_eq!(
            book_balances(&db, "1122", p()).unwrap().get("C01").unwrap().1,
            m_("1000")
        );
        let b = get_batch(&db, id).unwrap().unwrap();
        assert_eq!(b.adjust_account, adj);
        assert_eq!(b.voucher_id, Some(vid));
    }

    #[test]
    fn 应付差额往贷方调() {
        let db = mem();
        ap_voucher(&db, 1, "1000", true); // 我欠 S01 1000
        let adj = ensure_adjust(&db);
        let id = create(&db, KIND_AP, p(), "", "", "", "u").unwrap();
        upsert_line(&db, id, "S01", m_("1200"), "").unwrap(); // 供应商说我欠 1200
        let r = confirm(&db, id, "u", adj).unwrap();
        let v = crate::vouchers::get(&db, r.voucher_id.unwrap()).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "220201");
        assert_eq!(v.entries[0].credit, m_("200"), "应付多了要贷记应付");
        assert_eq!(v.entries[0].aux.supplier.as_deref(), Some("S01"));
        assert_eq!(v.entries[1].debit, m_("200"));
        assert!(v.balanced());
    }

    /// 「按账面列名单」是给人省事的：只补没有的户，金额先等于账面（差额 0），
    /// 用户改哪户就把哪户的差额显出来。
    #[test]
    fn 按账面列名单只补缺的户() {
        let db = mem();
        ar_for(&db, "C01", 1, "1000");
        ar_for(&db, "C02", 2, "800");
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        // 先手工录一户（对方报了个不一样的数），seed 不能把它冲掉
        upsert_line(&db, id, "C01", m_("1200"), "").unwrap();
        let n = seed_from_book(&db, id, "u").unwrap();
        assert_eq!(n, 1, "C01 已在批次里，只该补 C02");
        let d = get(&db, id).unwrap().unwrap();
        assert_eq!(d.lines.len(), 2);
        let c1 = d.lines.iter().find(|l| l.party_code == "C01").unwrap();
        assert_eq!(c1.claim, m_("1200"), "用户填的数不能被账面数覆盖");
        assert_eq!(c1.diff, m_("200"));
        let c2 = d.lines.iter().find(|l| l.party_code == "C02").unwrap();
        assert_eq!(c2.claim, c2.book, "新补的户金额先等于账面");
        assert_eq!(c2.diff, Money::ZERO);
        // 再点一次不该重复加
        assert_eq!(seed_from_book(&db, id, "u").unwrap(), 0);
    }

    #[test]
    fn 有争议的户确认时跳过() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true);
        let adj = ensure_adjust(&db);
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        upsert_line(&db, id, "C01", m_("1200"), "").unwrap();
        let d = get(&db, id).unwrap().unwrap();
        let lid = d.lines[0].id;
        set_line(&db, lid, m_("1200"), true, "对方发票号不对").unwrap();
        let r = confirm(&db, id, "u", adj).unwrap();
        assert_eq!(r.adjusted, 0);
        assert_eq!(r.skipped_disputed, 1);
        assert!(r.voucher_id.is_none(), "全是争议就不该动账");
    }

    #[test]
    fn 打回重对删掉草稿凭证() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true);
        let adj = ensure_adjust(&db);
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        upsert_line(&db, id, "C01", m_("1200"), "").unwrap();
        let vid = confirm(&db, id, "u", adj).unwrap().voucher_id.unwrap();
        let before = db.stats().unwrap().0;
        reopen(&db, id, "u").unwrap();
        assert!(crate::vouchers::get(&db, vid).unwrap().is_none(), "草稿凭证要删掉");
        assert_eq!(db.stats().unwrap().0, before - 1);
        let b = get_batch(&db, id).unwrap().unwrap();
        assert_eq!(b.status, ST_OPEN);
        assert!(b.voucher_id.is_none());
        // 退回后可以继续改数
        upsert_line(&db, id, "C01", m_("1100"), "").unwrap();
    }

    #[test]
    fn 已确认的批次不许改数() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true);
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        upsert_line(&db, id, "C01", m_("1000"), "").unwrap();
        confirm(&db, id, "u", "").unwrap();
        assert!(upsert_line(&db, id, "C01", m_("1200"), "").is_err());
        assert!(sync_book(&db, id).is_err());
    }

    #[test]
    fn 重算账面不动用户填的金额() {
        let db = mem();
        ar_voucher(&db, 1, "1000", true);
        let id = create(&db, KIND_AR, p(), "", "", "", "u").unwrap();
        upsert_line(&db, id, "C01", m_("1200"), "").unwrap();
        ar_voucher(&db, 2, "300", true); // 账面变 1300
        assert_eq!(sync_book(&db, id).unwrap(), 1);
        let l = &get(&db, id).unwrap().unwrap().lines[0];
        assert_eq!(l.claim, m_("1200"), "用户填的金额不能被覆盖");
        assert_eq!(l.book, m_("1300"));
        assert_eq!(l.diff, m_("-100"));
    }
}
