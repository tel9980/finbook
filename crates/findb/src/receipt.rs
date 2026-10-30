//! 收付款单（对标金蝶收款单 / 付款单）
//!
//! 出纳的资金动作单据：保存即生成记账凭证草稿（H-3：草稿不入余额），并按往来单位对
//! **未清挂账**做 FIFO 自动核销——应收/应付账龄与核销记录随即更新。删除受凭证状态约束
//! （先作废/删除凭证；凭证删除时其分录上的核销配对会被一并清理）。
//!
//! 分录口径：
//! - 收款（receipt）：借 资金账户 / 贷 应收账款（客户辅助），按挂账逐笔分摊核销；
//! - 付款（payment）：借 应付账款（供应商辅助）/ 贷 资金账户，同上；
//! - 无挂账时全额落在账套配置的默认往来科目（纯预收/预付性质），不核销。

use chrono::NaiveDate;
use fincore::{AuxRef, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 收付款单
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReceiptDoc {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    /// receipt=收款 / payment=付款
    pub kind: String,
    /// 资金账户（末级，如 100201 / 1001；空 = 账套默认资金账户）
    pub fund_account: String,
    /// 往来单位（辅助编码：收款=客户、付款=供应商；决定与谁核销）
    pub party: String,
    /// 资金账户辅助编码（银行 B01/B02 或微信 WECHAT，空则按 fund_account 推断默认 B01）
    pub bank: String,
    /// 跨方对冲往来单位（收款=供应商、付款=客户；空则不做跨方对冲）
    pub cross_party: String,
    /// 跨方对冲金额（0 表示不跨方）
    pub cross_amount: Money,
    pub amount: Money,
    pub memo: String,
    /// 生成的记账凭证（草稿阶段为 None，审核时生成）
    #[serde(default)]
    pub voucher_id: Option<i64>,
    /// draft 待审核 / audited 已审核（历史数据默认 audited）
    #[serde(default)]
    pub status: String,
    pub created_by: String,
    pub created_at: String,
}

fn map_doc(r: &rusqlite::Row) -> rusqlite::Result<ReceiptDoc> {
    let d: String = r.get(3)?;
    Ok(ReceiptDoc {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1977, 1, 1).unwrap()),
        kind: r.get(4)?,
        fund_account: r.get(5)?,
        party: r.get(6)?,
        bank: r.get(7)?,
        cross_party: r.get(8)?,
        cross_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
        amount: Money::parse_or_zero(&r.get::<_, String>(10)?),
        memo: r.get(11)?,
        voucher_id: r.get(12)?,
        created_by: r.get(13)?,
        created_at: r.get(14)?,
        status: r.get(15)?,
    })
}

const R_COLS: &str =
    "id,no,period,date,kind,fund_account,party,bank,cross_party,cross_amount,amount,memo,voucher_id,created_by,created_at,status";

pub fn receipt_list(db: &Db) -> DbResult<Vec<ReceiptDoc>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {R_COLS} FROM receipt_doc ORDER BY date DESC, id DESC"
    ))?;
    let rows = st.query_map([], map_doc)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn receipt_get(db: &Db, id: i64) -> DbResult<Option<ReceiptDoc>> {
    db.conn()
        .query_row(
            &format!("SELECT {R_COLS} FROM receipt_doc WHERE id=?1"),
            rusqlite::params![id],
            map_doc,
        )
        .optional()
        .map_err(Into::into)
}

/// 新建收付款单（**草稿，仅台账**）：凭证与 FIFO 自动核销在「审核」时同事务生成——
/// 对标金蝶收付款单审核流（录单 → 审核 → 记账）。守卫：金额>0、往来单位必填
/// （决定与谁核销）。草稿可直接删除。返回单据 id。
pub fn receipt_create(
    db: &Db,
    kind: &str,
    date: NaiveDate,
    fund_account: &str,
    party: &str,
    bank: &str,
    cross_party: &str,
    cross_amount: Money,
    amount: Money,
    memo: &str,
    who: &str,
) -> DbResult<i64> {
    if kind != "receipt" && kind != "payment" {
        return Err(fincore::FinError::state("类型只能是收款(receipt)/付款(payment)").into());
    }
    if !amount.is_positive() {
        return Err(fincore::FinError::validate("金额必须大于 0").into());
    }
    let party = party.trim();
    if party.is_empty() {
        return Err(fincore::FinError::validate(
            "往来单位（辅助编码，如 C01/S01）必填，用于自动核销",
        )
        .into());
    }
    let biz = db.options().biz_accounts.clone();
    let fund = if fund_account.trim().is_empty() {
        biz.fund
    } else {
        fund_account.trim().to_string()
    };
    let period = Period::from_date(date);
    let receipt = kind == "receipt";
    let tx = db.write_tx()?;
    let doc_no = format!(
        "{}{}",
        if receipt { "SK" } else { "FK" },
        chrono::Local::now().format("%y%m%d%H%M%S")
    );
    tx.execute(
        "INSERT INTO receipt_doc(no,period,date,kind,fund_account,party,bank,cross_party,cross_amount,amount,memo,created_by,created_at,status)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'draft')",
        rusqlite::params![
            doc_no,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            kind,
            fund,
            party,
            bank,
            cross_party,
            crate::money_param(cross_amount),
            crate::money_param(amount),
            memo,
            who,
            now()
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

/// 审核收付款单：同事务生成资金凭证 + 按往来单位 FIFO 自动核销未清挂账 → 已审核。
///
/// 挂账可跨往来科目的下级叶子（按账套一级科目长度取根覆盖）；核销配对要求同科目同辅助
/// （`settle_in_tx` 校验）。金额超出挂账的部分不配对，留在资金凭证的往来腿上
/// （预收/预付性质）。挂账在事务外先读，`settle_in_tx` 会按事务内当时已核销额复校验——
/// 并发竞态下整单回滚，重试即可。仅 draft 可审核（条件更新防并发重复）。
///
/// 返回 `(凭证 id, 自动核销笔数)`。
pub fn receipt_audit(db: &Db, id: i64, who: &str) -> DbResult<(i64, usize)> {
    let d = receipt_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("收付款单"))?;
    if d.status != "draft" {
        return Err(fincore::FinError::state("仅待审核的单据可审核").into());
    }
    if d.voucher_id.is_some() {
        return Err(fincore::FinError::state("单据已生成凭证").into());
    }
    let kind = d.kind.as_str();
    let receipt = kind == "receipt";
    let party = d.party.trim();
    if party.is_empty() {
        return Err(fincore::FinError::validate("往来单位为空，无法核销").into());
    }
    let bank = if d.bank.trim().is_empty() {
        None
    } else {
        Some(d.bank.trim())
    };
    let amount = d.amount;
    let date = d.date;
    let memo = d.memo.as_str();
    let fund = if d.fund_account.trim().is_empty() {
        db.options().biz_accounts.fund.clone()
    } else {
        d.fund_account.clone()
    };
    let biz = db.options().biz_accounts.clone();
    let party_base = if receipt {
        biz.ar.clone()
    } else {
        biz.ap.clone()
    };
    // 一级科目根：覆盖该往来科目的全部下级叶子（按账套编码方案第一级长度截断）
    let scheme = db.options().code_scheme.clone();
    let root_len = scheme.first().map(|s| *s as usize).unwrap_or(4);
    let root = if party_base.len() >= root_len {
        party_base[..root_len].to_string()
    } else {
        party_base.clone()
    };
    let aux = if receipt {
        AuxRef {
            customer: Some(party.to_string()),
            ..Default::default()
        }
    } else {
        AuxRef {
            supplier: Some(party.to_string()),
            ..Default::default()
        }
    };
    let aux_key = aux.key();
    let period = d.period;

    // 未清挂账（事务外先读；settle_in_tx 会在事务内复校验未核销额）
    let mut open = crate::settle::open_entries(db, &root, period, false)?;
    // 按辅助核算过滤必须用「包含 wanted 的全部片段」，不能要求整条 aux_key 完全相等：
    // 往来科目常同时核算部门/项目，凭证分录的键是 `customer=C001\x1fdept=D01`，
    // 而收款单只填了客户。整条相等会把这些分录全滤掉 → FIFO 分摊落空 →
    // 收款全额退回预收、往来一直不核销（金额不会错，但核销与账龄静默失效）。
    open.retain(|e| AuxRef::key_contains(&e.aux_key, &aux_key));
    let want_debit_side = receipt; // 收款冲借方挂账；付款冲贷方挂账
    open.retain(|e| {
        if want_debit_side {
            e.debit > Money::ZERO
        } else {
            e.credit > Money::ZERO
        }
    });

    // 同方 FIFO 分摊：按日期顺序逐笔吃掉挂账，金额 = 总额 - 跨方金额
    let cross_party = d.cross_party.trim();
    let cross_amount = d.cross_amount;
    let same_party_amount = amount - cross_amount;
    let mut remaining = same_party_amount;
    let mut leg_alloc: std::collections::BTreeMap<String, (Money, AuxRef)> =
        std::collections::BTreeMap::new();
    let mut allocs: Vec<(i64, String, Money)> = Vec::new(); // (挂账分录 id, 科目, 金额)
    for e in &open {
        if remaining.is_zero() {
            break;
        }
        let take = remaining.min(e.open());
        if take.is_positive() {
            allocs.push((e.entry_id, e.account_code.clone(), take));
            // 键带上 aux_key，同一科目不同辅助组合分行
            let ek = format!("{}\u{1f}{}", e.account_code, e.aux_key);
            let ea = AuxRef::from_key(&e.aux_key);
            let slot = leg_alloc.entry(ek).or_insert((Money::ZERO, ea));
            slot.0 += take;
            remaining -= take;
        }
    }
    // 同方多余/无挂账 → 落账套默认往来科目（预收/预付性质），辅助核算用收款单自己的
    if remaining.is_positive() || leg_alloc.is_empty() {
        let pk = format!("{}\u{1f}{}", party_base, aux.key());
        let slot = leg_alloc.entry(pk).or_insert((Money::ZERO, aux.clone()));
        slot.0 += remaining;
    }

    // 跨方 FIFO 分摊：针对另一往来单位，使用相反科目与相反辅助维度
    if cross_amount.is_positive() && !cross_party.is_empty() {
        let cross_account = if receipt { biz.ap } else { biz.ar }; // 收款→应付，付款→应收
        let cross_root = if cross_account.len() >= root_len {
            cross_account[..root_len].to_string()
        } else {
            cross_account.clone()
        };
        let cross_aux = if receipt {
            AuxRef {
                supplier: Some(cross_party.to_string()),
                ..Default::default()
            }
        } else {
            AuxRef {
                customer: Some(cross_party.to_string()),
                ..Default::default()
            }
        };
        let cross_aux_key = cross_aux.key();

        // 未清挂账：跨方单位，相反科目
        let mut cross_open = crate::settle::open_entries(db, &cross_root, period, false)?;
        cross_open.retain(|e| AuxRef::key_contains(&e.aux_key, &cross_aux_key));
        let cross_want_debit_side = !receipt; // 收款冲贷方应付；付款冲借方应收
        cross_open.retain(|e| {
            if cross_want_debit_side {
                e.debit > Money::ZERO
            } else {
                e.credit > Money::ZERO
            }
        });

        let mut cross_remaining = cross_amount;
        for e in &cross_open {
            if cross_remaining.is_zero() {
                break;
            }
            let take = cross_remaining.min(e.open());
            if take.is_positive() {
                allocs.push((e.entry_id, e.account_code.clone(), take));
                let ek = format!("{}\u{1f}{}", e.account_code, e.aux_key);
                let ea = AuxRef::from_key(&e.aux_key);
                let slot = leg_alloc.entry(ek).or_insert((Money::ZERO, ea));
                slot.0 += take;
                cross_remaining -= take;
            }
        }
        // 跨方多余 → 落账跨方默认科目（预收/预付性质）
        if cross_remaining.is_positive() {
            let pk = format!("{}\u{1f}{}", cross_account, cross_aux.key());
            let slot = leg_alloc
                .entry(pk)
                .or_insert((Money::ZERO, cross_aux.clone()));
            slot.0 += cross_remaining;
        }
    }

    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = if memo.trim().is_empty() {
        if receipt {
            format!("收款 {}", party)
        } else {
            format!("付款 {}", party)
        }
    } else {
        memo.trim().to_string()
    };
    let summary = v.memo.clone();
    // 资金腿辅助核算统一取一处（收付款/调拨共用），见 funds::fund_account_aux
    let fund_aux = crate::funds::fund_account_aux(&fund, bank.as_deref());
    let mut line: i32 = 1;
    if receipt {
        v.push_entry(Entry {
            debit: amount,
            aux: fund_aux,
            ..Entry::new(line, fund.as_str(), summary.as_str())
        });
        line += 1;
        for (slot_key, (alloc, ea)) in &leg_alloc {
            if alloc.is_zero() {
                continue;
            }
            // 键是 "科目\x1faux_key"，取回科目名
            let acct = slot_key.split('\u{1f}').next().unwrap_or(slot_key);
            v.push_entry(Entry {
                credit: *alloc,
                aux: ea.clone(),
                ..Entry::new(line, acct, summary.as_str())
            });
            line += 1;
        }
    } else {
        for (slot_key, (alloc, ea)) in &leg_alloc {
            if alloc.is_zero() {
                continue;
            }
            let acct = slot_key.split('\u{1f}').next().unwrap_or(slot_key);
            v.push_entry(Entry {
                debit: *alloc,
                aux: ea.clone(),
                ..Entry::new(line, acct, summary.as_str())
            });
            line += 1;
        }
        v.push_entry(Entry {
            credit: amount,
            aux: fund_aux,
            ..Entry::new(line, fund.as_str(), summary.as_str())
        });
    }
    let vid = crate::vouchers::save_in(&tx, &mut v)?;

    // 分摊结果逐笔核销（同事务；settle_in_tx 复校验未核销额，超出即整单回滚）
    let mut settled_n = 0usize;
    if !allocs.is_empty() {
        let mut st =
            tx.prepare("SELECT id, account_code FROM voucher_entry WHERE voucher_id=?1")?;
        let entries: Vec<(i64, String)> = st
            .query_map([vid], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(st);
        let my_id_of = |acct: &str| -> Option<i64> {
            entries.iter().find(|(_, a)| a == acct).map(|(id, _)| *id)
        };
        for (from_id, acct, take) in &allocs {
            if let Some(my_id) = my_id_of(acct) {
                crate::settle::settle_in_tx(&tx, *from_id, my_id, *take, who)?;
                settled_n += 1;
            }
        }
    }

    // 状态推进：仅 draft → audited（条件更新，防并发重复审核/重复出凭证）
    let n = tx.execute(
        "UPDATE receipt_doc SET voucher_id=?2, status='audited' WHERE id=?1 AND status='draft'",
        rusqlite::params![id, vid],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("单据状态已变化，请刷新后重试").into());
    }
    tx.commit()?;
    Ok((vid, settled_n))
}

/// 撤销审核：删除其**未记账**凭证（顺带清理核销配对）→ 单据回到草稿（可改可删）。
/// 凭证已记账时拒绝（先反记账再撤审）。
pub fn receipt_unaudit(db: &Db, id: i64) -> DbResult<()> {
    let d = receipt_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("收付款单"))?;
    if d.status != "audited" {
        return Err(fincore::FinError::state("仅已审核的单据可撤销审核").into());
    }
    let vid = d
        .voucher_id
        .ok_or_else(|| fincore::FinError::state("单据未关联凭证，请刷新"))?;
    let v = crate::vouchers::get(db, vid)?
        .ok_or_else(|| fincore::FinError::not_found("关联凭证已不存在，请刷新"))?;
    if v.status == fincore::VoucherStatus::Posted {
        return Err(fincore::FinError::state("凭证已记账，请先反记账后再撤销审核").into());
    }
    let tx = db.write_tx()?;
    let n = tx.execute(
        "UPDATE receipt_doc SET status='draft', voucher_id=NULL WHERE id=?1 AND status='audited'",
        rusqlite::params![id],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("单据状态已变化，请刷新后重试").into());
    }
    // 同连接事务内删除凭证：含核销配对清理（vouchers::delete 内置），失败整体回滚
    crate::vouchers::delete(db, vid)?;
    tx.commit()?;
    Ok(())
}

/// 删除收付款单：其凭证须先作废或删除（凭证删除会顺带清理核销配对），随后可删
pub fn receipt_delete(db: &Db, id: i64) -> DbResult<()> {
    let d = receipt_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("收付款单"))?;
    if let Some(vid) = d.voucher_id {
        let status = crate::vouchers::get(db, vid)?
            .map(|v| v.status)
            .unwrap_or(fincore::VoucherStatus::Void);
        if status != fincore::VoucherStatus::Void {
            return Err(
                fincore::FinError::state("该单已生成凭证，请先作废或删除凭证后再删单").into(),
            );
        }
    }
    db.conn()
        .execute("DELETE FROM receipt_doc WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn d(y: i32, mo: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, mo, dd).unwrap()
    }

    /// 期初挂账：借应收（debit=true）或贷应付（debit=false），并记账
    fn ar_voucher(db: &Db, p: Period, day: u32, party: &str, amount: Money, debit: bool) {
        let date = NaiveDate::from_ymd_opt(p.year(), p.month(), day).unwrap();
        let mut v = Voucher::new(
            p,
            date,
            "记",
            crate::vouchers::next_no(db, p, "记").unwrap(),
        );
        if debit {
            v.push_entry(Entry {
                debit: amount,
                aux: AuxRef {
                    customer: Some(party.to_string()),
                    ..Default::default()
                },
                ..Entry::new(1, "112201", "挂账")
            });
            v.push_entry(Entry {
                credit: amount,
                ..Entry::new(2, "600101", "挂账")
            });
        } else {
            v.push_entry(Entry {
                credit: amount,
                aux: AuxRef {
                    supplier: Some(party.to_string()),
                    ..Default::default()
                },
                ..Entry::new(1, "220201", "挂账")
            });
            v.push_entry(Entry {
                debit: amount,
                ..Entry::new(2, "660201", "挂账")
            });
        }
        let id = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, id, "u").unwrap();
    }

    /// 应收分录带**多个**辅助维度时，收款单只填客户也必须能自动核销。
    /// 回归：`open.retain(|e| e.aux_key == aux_key)` 要求整条 aux_key 完全相等，
    /// 而凭证分录的键是 `customer=C01\x1fdept=D01`、收款单的键只有 `customer=C01`，
    /// 于是 FIFO 分摊落空、收款全额退回预收，往来永远不核销。
    #[test]
    fn receipt_autosettles_multi_dim_aux() {
        use fincore::account::{Account, AcctCategory, AuxKind, AuxMask};
        let db = mem();
        let p = Period::new(2026, 1).unwrap();

        // 建一个同时核算客户 + 部门的应收叶子科目（内置 1122 只核算客户）
        let mut a = Account::new("112299", "多维应收", AcctCategory::Asset);
        a.aux = AuxMask::NONE.with(AuxKind::Customer).with(AuxKind::Dept);
        crate::accounts::insert(&db, &a).unwrap();
        // 对方科目：不核算任何辅助维度
        crate::accounts::insert(
            &db,
            &Account::new("600199", "多维对方科目", AcctCategory::Income),
        )
        .unwrap();

        // 挂账 1000：分录带 customer=C01 + dept=D01
        let date = d(2026, 1, 5);
        let mut v = Voucher::new(
            p,
            date,
            "记",
            crate::vouchers::next_no(&db, p, "记").unwrap(),
        );
        v.push_entry(Entry {
            debit: m("1000"),
            aux: AuxRef {
                customer: Some("C01".into()),
                dept: Some("D01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "112299", "多维挂账")
        });
        v.push_entry(Entry {
            credit: m("1000"),
            ..Entry::new(2, "600199", "多维挂账")
        });
        let id = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, id, "u").unwrap();

        // 收款单只填客户 C01（不填部门）
        let doc_id = receipt_create(
            &db,
            "receipt",
            d(2026, 1, 10),
            "100201",
            "C01",
            "",
            "",
            Money::ZERO,
            m("600"),
            "",
            "u",
        )
        .unwrap();
        receipt_audit(&db, doc_id, "u").unwrap();

        let settled = crate::settle::list(&db, "112299").unwrap();
        assert_eq!(
            settled.len(),
            1,
            "多维辅助的应收应被自动核销，实际核销记录：{settled:?}"
        );
        assert_eq!(settled[0].amount, m("600"), "核销金额应为 600");
    }

    #[test]
    fn receipt_audit_autosettles_fifo() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();

        // 应收挂账 1000（客户 C01）
        ar_voucher(&db, p, 5, "C01", m("1000"), true);

        // 建单 = 草稿：不出凭证、不核销（审核流对标金蝶）
        let doc_id = receipt_create(
            &db,
            "receipt",
            d(2026, 1, 10),
            "100201",
            "C01",
            "",
            "",
            Money::ZERO,
            m("600"),
            "",
            "u",
        )
        .unwrap();
        assert!(doc_id > 0);
        {
            let doc = receipt_get(&db, doc_id).unwrap().unwrap();
            assert_eq!(doc.status, "draft");
            assert!(doc.voucher_id.is_none(), "草稿不应生成凭证");
        }
        assert!(
            crate::settle::list(&db, "112201").unwrap().is_empty(),
            "草稿阶段不应有核销记录"
        );

        // 审核：借 100201 / 贷 112201，FIFO 核销 600，单据置已审核
        let (vid, n) = receipt_audit(&db, doc_id, "u").unwrap();
        assert_eq!(n, 1, "应自动核销一笔");
        assert_eq!(receipt_get(&db, doc_id).unwrap().unwrap().status, "audited");
        // 重复审核拒绝（条件更新防并发）
        assert!(receipt_audit(&db, doc_id, "u").is_err(), "重复审核应拒绝");
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.entries[0].account_code, "100201");
        assert_eq!(v.entries[0].debit, m("600"));
        assert_eq!(v.entries[0].aux.bank.as_deref(), Some("B01"));
        assert_eq!(v.entries[1].account_code, "112201");
        assert_eq!(v.entries[1].credit, m("600"));
        assert_eq!(v.entries[1].aux.customer.as_deref(), Some("C01"));

        // 挂账剩余 400 未核销；核销记录 600
        let open = crate::settle::open_entries(&db, "1122", p, false).unwrap();
        let open_sum: Money = open.iter().map(|e| e.open()).sum();
        assert_eq!(open_sum, m("400"), "剩余未核销应为 400");
        let recs = crate::settle::list(&db, "112201").unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].amount, m("600"));

        // 付款侧：应付挂账 500（S01）→ 审核后付400 核销 400、剩 100
        ar_voucher(&db, p, 6, "S01", m("500"), false);
        let d2 = receipt_create(
            &db,
            "payment",
            d(2026, 1, 12),
            "100201",
            "S01",
            "",
            "",
            Money::ZERO,
            m("400"),
            "付货款",
            "u",
        )
        .unwrap();
        let (_v2, n2) = receipt_audit(&db, d2, "u").unwrap();
        assert_eq!(n2, 1);
        let open_ap = crate::settle::open_entries(&db, "2202", p, false).unwrap();
        let ap_sum: Money = open_ap.iter().map(|e| e.open()).sum();
        assert_eq!(ap_sum, m("100"), "应付剩余 100");

        // 无挂账（纯预收）：不核销、全额落账套默认应收
        let d3 = receipt_create(
            &db,
            "receipt",
            d(2026, 1, 15),
            "1001",
            "C77",
            "",
            "",
            Money::ZERO,
            m("88"),
            "",
            "u",
        )
        .unwrap();
        let (v3, n3) = receipt_audit(&db, d3, "u").unwrap();
        assert_eq!(n3, 0, "无挂账不核销");
        let v3 = crate::vouchers::get(&db, v3).unwrap().unwrap();
        assert_eq!(v3.entries.len(), 2);
        assert_eq!(v3.entries[1].account_code, "112201");
        assert_eq!(v3.entries[1].credit, m("88"));

        // 撤审：凭证（未记账）删除、核销配对清理 → 单据回草稿、可直接删
        receipt_unaudit(&db, d3).unwrap();
        let doc3 = receipt_get(&db, d3).unwrap().unwrap();
        assert_eq!(doc3.status, "draft", "撤审应回草稿");
        assert!(doc3.voucher_id.is_none());
        receipt_delete(&db, d3).unwrap();
        assert!(
            receipt_list(&db).unwrap().iter().all(|x| x.id != d3),
            "草稿可直接删除"
        );

        // 守卫：空往来单位 / 零金额（建单即拦）
        assert!(receipt_create(
            &db,
            "receipt",
            d(2026, 1, 16),
            "1001",
            "",
            "",
            "",
            Money::ZERO,
            m("1"),
            "",
            "u"
        )
        .is_err());
        assert!(receipt_create(
            &db,
            "receipt",
            d(2026, 1, 16),
            "1001",
            "C01",
            "",
            "",
            Money::ZERO,
            Money::ZERO,
            "",
            "u"
        )
        .is_err());

        // 删除链：已审核有凭证 → 拒；删凭证（清理核销）→ 挂账恢复、单据可删
        assert!(receipt_delete(&db, doc_id).is_err(), "凭证存在时不能删单");
        crate::vouchers::delete(&db, vid).unwrap();
        let open2 = crate::settle::open_entries(&db, "1122", p, false).unwrap();
        // 只看借方（原挂账）：预收 C77 的贷方未配对腿不应混入
        let after: Money = open2
            .iter()
            .filter(|e| e.debit > Money::ZERO)
            .map(|e| e.open())
            .sum();
        assert_eq!(after, m("1000"), "凭证删除应清理核销配对，挂账恢复");
        receipt_delete(&db, doc_id).unwrap();
        assert!(receipt_list(&db).unwrap().iter().all(|x| x.id != doc_id));
    }
}
