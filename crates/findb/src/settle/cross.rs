//! 通用往来互抵：跨往来单位、跨科目的互抵，自动出过渡凭证。
//!
//! 分册说明：本文件原属 `settle.rs` 单文件（3,437 行），按小节标题切开，
//! 函数体与注释一字未改；跨文件共用的内部函数标 `pub(super)`，对外 API 由
//! `settle/mod.rs` 统一 `pub use` 出去，外部调用路径 `findb::settle::xxx` 不变。

use super::*;

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
