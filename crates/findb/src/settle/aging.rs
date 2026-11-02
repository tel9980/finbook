//! 账龄：按往来对象与账龄区间聚合未清余额，并判断往来维度（客户/供应商）。
//!
//! 分册说明：本文件原属 `settle.rs` 单文件（3,437 行），按小节标题切开，
//! 函数体与注释一字未改；跨文件共用的内部函数标 `pub(super)`，对外 API 由
//! `settle/mod.rs` 统一 `pub use` 出去，外部调用路径 `findb::settle::xxx` 不变。

use super::*;

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

/// 账龄分析。
///
/// `key_filter` 为 `Some(key)` 时只返回该往来单位的账龄行（如查单个客户账龄），
/// 避免算全量所有客户再在 Rust 侧 `.find()`；为 `None` 时返回全部（供账龄报表复用）。
pub fn aging(
    db: &Db,
    account: &str,
    upto: Period,
    as_of: NaiveDate,
    buckets: &[AgingBucket],
    key_filter: Option<&str>,
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
    // 按单个往来单位过滤（查单个客户账龄时不算全量所有客户，再在 Rust 侧 .find()）
    if let Some(key) = key_filter {
        let k = key.trim();
        items.retain(|i| i.key.trim() == k);
    }
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
    let mut lines = fincore::engine::aging::analyze(&items, as_of, buckets)?;
    // 账期在这里补上：analyze 是纯函数（fincore 不碰数据库），而账期存在客商档案里。
    //
    // 逾期按**最老一笔未核销单据**算（`as_of - max_days` 就是它的日期）：
    // 客户 9/1 卖的货账期 30 天，10/25 还没给，就是逾期 24 天。
    // 一个客户有十笔单据时这是最保守的那个数 —— 也正是催款时要说的那个数。
    if !lines.is_empty() {
        let days_map = pay_days_map(db, party)?;
        for l in lines.iter_mut() {
            if let Some(d) = days_map.get(l.key.trim()) {
                l.pay_days = Some(*d);
                let due_days = *d - l.max_days;
                l.overdue_days = (-due_days).max(0);
            }
        }
    }
    Ok(lines)
}

/// 账期表：客商编码 → 账期天数
///
/// 一次全读，不要在账龄循环里逐行查档案（几十个客户就是几十次查询）。
/// 解析不了的账期直接忽略 —— 填错的账期当成没填，比让整张账龄报错好。
pub(super) fn pay_days_map(
    db: &Db,
    kind: fincore::AuxKind,
) -> DbResult<std::collections::HashMap<String, i64>> {
    // 停用的档案也要算：客户停用了，欠的钱不会消失
    let q = fincore::AuxQuery {
        kind: Some(kind),
        keyword: None,
        include_disabled: true,
        parent_code: None,
    };
    let mut m = std::collections::HashMap::new();
    for a in crate::auxs::list(db, &q)? {
        if let Some(v) = a.props.get(fincore::auxiliary::prop::PAY_DAYS) {
            if let Ok(d) = v.trim().parse::<i64>() {
                if (0..=3650).contains(&d) {
                    m.insert(a.code.trim().to_string(), d);
                }
            }
        }
    }
    Ok(m)
}
