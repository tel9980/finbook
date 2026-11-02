//! 自动核销：按日期 FIFO 把收付款与挂账分录配上，剩余额留给人工。
//!
//! 分册说明：本文件原属 `settle.rs` 单文件（3,437 行），按小节标题切开，
//! 函数体与注释一字未改；跨文件共用的内部函数标 `pub(super)`，对外 API 由
//! `settle/mod.rs` 统一 `pub use` 出去，外部调用路径 `findb::settle::xxx` 不变。

use super::*;

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
