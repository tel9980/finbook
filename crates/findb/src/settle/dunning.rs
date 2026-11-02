//! 催款单 / 对账函（应收催收闭环）与坏账准备计提。
//!
//! 分册说明：本文件原属 `settle.rs` 单文件（3,437 行），按小节标题切开，
//! 函数体与注释一字未改；跨文件共用的内部函数标 `pub(super)`，对外 API 由
//! `settle/mod.rs` 统一 `pub use` 出去，外部调用路径 `findb::settle::xxx` 不变。

use super::*;

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
    /// 方向：「应收/应付」= 对方欠我方（催收对象）；「预收/预付」= 我方欠对方。
    ///
    /// 老账套里已经存下的催款单没有这个字段（`serde(default)` 得到空串），
    /// **空串一律按应收/应付处理** —— 那些单据生成时还没有方向概念，
    /// 把它们的金额改成预收等于篡改历史单据。
    #[serde(default)]
    pub direction: String,
    /// 这一笔按客商账期算的逾期天数（`0` = 未到期或档案里没填账期）
    ///
    /// 老账套存下的催款单没这个字段（`serde(default)` 得 0），不追溯重算 ——
    /// 那是当时那封函上写过的数字，重算等于改历史单据。
    #[serde(default)]
    pub overdue_days: i64,
}

/// 单张单据的逾期天数：`(开函日 − 单据日) − 账期`，不足 0 记 0
///
/// 与账龄页的口径一致（`AgingLine.overdue_days`），但这里是逐单算 ——
/// 催款函上每一行都要说清「这笔超了几天」，只给一个总数客户对不上是哪张。
fn overdue_of(letter_date: NaiveDate, doc_date: NaiveDate, pay_days: Option<i64>) -> i64 {
    let Some(days) = pay_days else {
        return 0;
    };
    (letter_date - doc_date)
        .num_days()
        .saturating_sub(days)
        .max(0)
}

/// 明细行是否属于「对方欠我方」的那一侧。
pub fn dunning_item_is_owed(direction: &str) -> bool {
    // 空串 = 老数据（见 DunningItem.direction 的注释）
    direction.is_empty() || direction == "应收" || direction == "应付"
}

fn dunning_dir_label(kind: &str, owed: bool) -> &'static str {
    match (kind, owed) {
        ("ar", true) => "应收",
        ("ar", false) => "预收",
        ("ap", true) => "应付",
        _ => "预付",
    }
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

impl Dunning {
    /// 对方真正欠我方的合计（催款单上的「应向我方支付」就是这个数）。
    pub fn owed_total(&self) -> Money {
        self.detail
            .iter()
            .filter(|i| dunning_item_is_owed(&i.direction))
            .map(|i| i.amount)
            .fold(Money::ZERO, |a, b| a + b)
    }

    /// 我方欠对方的合计（预收/预付）。挂在同一张单上是为了让客户看得懂
    /// 「你给我 5 万、我先收了 3 万、实际还欠 2 万」——但这一行**不是**催收金额。
    pub fn advance_total(&self) -> Money {
        self.detail
            .iter()
            .filter(|i| !dunning_item_is_owed(&i.direction))
            .map(|i| i.amount)
            .fold(Money::ZERO, |a, b| a + b)
    }
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

/// 催款单列表（可按 kind='ar'/'ap' 与 party_code 过滤）
pub fn dunning_list(
    db: &Db,
    kind: Option<&str>,
    party_code: Option<&str>,
) -> DbResult<Vec<Dunning>> {
    let mut out = Vec::new();
    let party = party_code.filter(|s| !s.trim().is_empty());
    match kind.filter(|k| !k.trim().is_empty()) {
        Some(k) => {
            if let Some(p) = party {
                let mut st = db.conn().prepare(&format!(
                    "SELECT {DUN_COLS} FROM dunning WHERE kind=?1 AND party_code=?2 ORDER BY date DESC, id DESC"
                ))?;
                let rows = st.query_map(rusqlite::params![k, p], map_dunning)?;
                for r in rows {
                    out.push(r?);
                }
            } else {
                let mut st = db.conn().prepare(&format!(
                    "SELECT {DUN_COLS} FROM dunning WHERE kind=?1 ORDER BY date DESC, id DESC"
                ))?;
                let rows = st.query_map(rusqlite::params![k], map_dunning)?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        None => {
            if let Some(p) = party {
                let mut st = db.conn().prepare(&format!(
                    "SELECT {DUN_COLS} FROM dunning WHERE party_code=?1 ORDER BY date DESC, id DESC"
                ))?;
                let rows = st.query_map(rusqlite::params![p], map_dunning)?;
                for r in rows {
                    out.push(r?);
                }
            } else {
                let mut st = db.conn().prepare(&format!(
                    "SELECT {DUN_COLS} FROM dunning ORDER BY date DESC, id DESC"
                ))?;
                let rows = st.query_map([], map_dunning)?;
                for r in rows {
                    out.push(r?);
                }
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
    // 方向归一：与 `aging()` 用同一套规则 —— 贷方性质科目（2202 等）的分录要翻号，
    // 翻完「正数 = 对方欠我方/我方欠对方」才是同一个方向。
    //
    // 不翻会怎样：应付账款的分录本来就是贷方，`signed()` 是负数，一张正常的
    // 供应商发票会被判成「预付」。
    let flip = match crate::accounts::chart(db)
        .ok()
        .as_ref()
        .and_then(|c| c.get(account))
    {
        Some(a) => a.dir == fincore::account::Direction::Credit,
        None => account.starts_with('2'),
    };
    // 账期：与账龄页同一个来源（客商档案的 pay_days），一次全读
    let pay_days = pay_days_map(
        db,
        if kind == "ar" {
            fincore::AuxKind::Customer
        } else {
            fincore::AuxKind::Supplier
        },
    )?
    .get(party)
    .copied();
    let mut items: Vec<DunningItem> = Vec::new();
    let mut amount = Money::ZERO;
    let mut owed_count: i64 = 0;
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
        let signed = if flip {
            e.signed().negated()
        } else {
            e.signed()
        };
        // `open()` 是绝对值（`abs(signed) - settled`），方向只能从 signed 上看。
        // 以前直接 `amount += amt` —— 客户预存 3 万（贷方）会被算成「应向我方支付 3 万」，
        // 账龄页把它单列在「其中预收」，催款单却加进合计：同一份数据两个口径。
        let owed = !signed.is_negative();
        let direction = dunning_dir_label(kind, owed).to_string();
        if owed {
            amount += amt;
            owed_count += 1;
        }
        items.push(DunningItem {
            doc_no: format!("{}-{}", e.word, e.no),
            date: e.date.format("%Y-%m-%d").to_string(),
            summary: e.summary.clone(),
            amount: amt,
            direction,
            overdue_days: overdue_of(date, e.date, pay_days),
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
        // 期初单据的金额入库时就要求为正、且方向由 kind 决定（ar=应收 / ap=应付），
        // 所以它恒在「对方欠我方」那一侧。
        amount += o.amount;
        owed_count += 1;
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
            direction: dunning_dir_label(kind, true).to_string(),
            overdue_days: overdue_of(date, d, pay_days),
        });
    }
    if items.is_empty() {
        return Err(
            fincore::FinError::msg("该客商在所选科目下无未核销单据（或期初），无需催款").into(),
        );
    }
    if amount.is_zero() {
        // 只有预收/预付，一分钱也不欠。以前这种情况会生成一张「金额 0」的催款单，
        // 客户收到的是「应向我方支付 0.00 元」加一串预收明细 —— 比不发还糟。
        let adv: Money = items
            .iter()
            .filter(|i| !dunning_item_is_owed(&i.direction))
            .map(|i| i.amount)
            .fold(Money::ZERO, |a, b| a + b);
        return Err(fincore::FinError::msg(format!(
            "该客商只有预收/预付 {}，没有欠款，不需要催款",
            adv.fmt_plain()
        ))
        .into());
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
            owed_count,
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
        lines.extend(aging(db, acct, period, date, &buckets, None)?);
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
