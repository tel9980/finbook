//! 到岸成本（Landed Cost）
//!
//! ## 它解决什么问题
//!
//! 一批货从下单到入库，真正的成本不只是货款：运费、关税、保险、清关费、港口
//! 杂费，合计常常是货款的百分之几到几十。这些钱不摊进存货成本，会让三处同时
//! 失真：
//!
//! 1. **存货估值偏低** —— 资产负债表上的存货少了这一块
//! 2. **采购价格差异被算歪** —— 差异表比的是「实际入库单价 vs 标准单价」，而
//!    实际入库单价取自 `stock_move.amount / qty`，那个金额**就是订单单价**。
//!    货价便宜 5%、运费贵 20% 时，差异表显示「节约」，实际成本反而更高。
//!    而且它不报错：数字完整、有正有负、能排序、合计能加，只有跟总账对运费
//!    时才会发现。
//! 3. **销售成本被低估** —— 按偏低的存货单价结转销售成本，毛利虚高
//!
//! ## 关键会计处理：不改原入库单
//!
//! 到岸成本**不回头修改**采购入库单的金额与单价，而是：
//!
//! - 追加一条 `kind=adjust` 的存货流水（数量 0、金额为分摊额），
//!   于是结存金额被抬高，而原始入库单据与已开票数据一个字节都不动
//! - 另开一张独立凭证，记账时与到岸成本单建边
//!
//! 改原单据的坏处很具体：那笔入库一旦已开票、已对账，改单价就得连带重算税额、
//! 重算已对账金额。ERPNext 也是这个口径（社区里为此吵过，结论一致）。
//!
//! ## 三个必须处理对的边界
//!
//! 1. **货已全部出清时，运费是当期费用不是存货。** `stock_adjust` 在结存数量
//!    为 0 时直接拒绝（见 `business::stock_adjust`），否则会造出「数量 0、
//!    结存金额 > 0」的幽灵存货。这里按**金额封顶**：可入存货的部分不超过当前
//!    结存价值，超出部分转费用科目，并在单据上写明原因。
//! 2. **分摊尾差必须有人吸收。** 按权重分摊必然除不尽；尾差挤到最后一行，
//!    保证「各行分摊额之和 == 总额」一分不差。少这一句，成本就会凭空少几分钱。
//! 3. **已过账不可重过。** 状态用条件更新（`AND status='draft'`）防并发，
//!    两次点击只能过一次。

use std::collections::BTreeMap;

use chrono::NaiveDate;
use fincore::engine::costing::CostMethod;
use fincore::{Entry, Money, Period, Voucher, VoucherSource};
use serde::{Deserialize, Serialize};

use crate::{Db, DbResult};

/// 分摊基准
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum AllocBasis {
    /// 按数量
    #[default]
    Qty,
    /// 按货值金额（最常用）
    Amount,
    /// 按重量
    Weight,
    /// 逐行手工指定（表里直接填各行金额）
    Manual,
}

impl AllocBasis {
    pub fn code(self) -> &'static str {
        match self {
            AllocBasis::Qty => "qty",
            AllocBasis::Amount => "amount",
            AllocBasis::Weight => "weight",
            AllocBasis::Manual => "manual",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            AllocBasis::Qty => "按数量",
            AllocBasis::Amount => "按货值金额",
            AllocBasis::Weight => "按重量",
            AllocBasis::Manual => "手工指定",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "amount" => AllocBasis::Amount,
            "weight" => AllocBasis::Weight,
            "manual" => AllocBasis::Manual,
            _ => AllocBasis::Qty,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LcLine {
    pub id: i64,
    pub lcv_id: i64,
    /// 采购订单（到岸成本摊到哪张采购单的货上）
    pub po_id: i64,
    pub po_no: String,
    pub item_code: String,
    pub item_name: String,
    /// 分摊用数量
    pub qty: Money,
    /// 分摊用重量
    pub weight: Money,
    /// 分摊用货值
    pub amount: Money,
    /// 手工指定时的分摊额（其余基准下由算法算）
    pub manual_amount: Money,
    /// 算出来的分摊额
    pub allocated: Money,
    /// 实际计入存货的金额（受结存价值封顶，可能小于 allocated）
    pub to_stock: Money,
    /// 转费用的金额（货已出清，分摊额无处承载）
    pub to_expense: Money,
    /// 该存货的当前结存数量（界面提示用）
    pub on_hand_qty: Money,
    pub memo: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LandedCost {
    pub id: i64,
    pub no: String,
    pub period: i32,
    pub date: String,
    pub supplier_code: String,
    pub supplier_name: String,
    pub basis: String,
    /// 运费
    pub freight: Money,
    /// 关税
    pub duty: Money,
    /// 保险
    pub insurance: Money,
    /// 清关/港口杂费
    pub clearing: Money,
    /// 其他
    pub other: Money,
    /// 借贷方科目（可按账套改；借贷必平由凭证层保证）
    pub dr_account: String,
    pub cr_account: String,
    /// 付款账户（贷方是银行存款等**核算银行账户**的科目时必填）
    ///
    /// 刻意不省：贷方默认取账套的资金账户（银行存款），而这类科目**核算银行账户**，
    /// 凭证校验要求贷方分录必须带银行辅助。少了这一栏，**默认路径自己走不通** ——
    /// 界面只会报「第 2 行科目 100201 核算银行账户，必须填写银行账户」，
    /// 而用户根本不知道自己漏了什么（运费是付给货代的，从哪个账户付是必填信息）。
    pub bank_account: String,
    /// 转费用时的借方科目
    pub expense_account: String,
    pub status: String,
    pub voucher_id: Option<i64>,
    pub memo: String,
    pub prepared_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub lines: Vec<LcLine>,
}

impl LandedCost {
    pub fn total_charges(&self) -> Money {
        self.freight + self.duty + self.insurance + self.clearing + self.other
    }
    pub fn total_allocated(&self) -> Money {
        self.lines.iter().map(|l| l.allocated).sum()
    }
    pub fn total_to_stock(&self) -> Money {
        self.lines.iter().map(|l| l.to_stock).sum()
    }
    pub fn total_to_expense(&self) -> Money {
        self.lines.iter().map(|l| l.to_expense).sum()
    }
    /// 是否可改可过账：**只有「已过账」才锁**。
    ///
    /// 刻意不用 `status == "draft"`：那样写的话，任何在内存里现搭一个
    /// `LandedCost { ..Default::default() }` 再保存的调用方都会撞上
    /// 「只有草稿状态能修改」——因为 `Default` 给的 `status` 是**空串**，
    /// 不是 `"draft"`。而且空串在 `status_label` 里还会显示成「草稿」，
    /// 看起来完全正常，只有保存时才炸。
    ///
    /// 「只有终态才锁」这种写法本身也更稳：将来多一个终态也不会漏判。
    pub fn is_draft(&self) -> bool {
        self.status != "posted"
    }
}

// ===========================================================================
// 取号与保存
// ===========================================================================

pub fn next_no(db: &Db, period: Period) -> DbResult<String> {
    let prefix = format!(
        "{}{:04}{:02}",
        crate::doc_prefix(db, "lcv", "LC"),
        period.year(),
        period.month()
    );
    let sql = format!(
        "SELECT COALESCE(MAX(CAST(SUBSTR(no, {}) AS INTEGER)), 0) + 1 FROM landed_cost WHERE no LIKE ?",
        prefix.len() + 1
    );
    let n: i64 = db
        .conn()
        .query_row(&sql, [format!("{prefix}%")], |r| r.get(0))?;
    Ok(format!("{prefix}{n:04}"))
}

pub fn save(db: &Db, lc: &mut LandedCost, lines: &[LcLine], who: &str) -> DbResult<i64> {
    if !lc.is_draft() {
        return Err(fincore::FinError::state("只有草稿状态的到岸成本单能修改").into());
    }
    if lines.is_empty() {
        return Err(fincore::FinError::state("到岸成本单至少要有一行存货").into());
    }
    if lc.total_charges().is_zero() {
        return Err(fincore::FinError::state("附加成本合计为 0，无需录入").into());
    }
    if lc.dr_account.trim().is_empty() || lc.cr_account.trim().is_empty() {
        return Err(fincore::FinError::state("借贷方科目不能为空").into());
    }
    let tx = db.write_tx()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let charges = (
        crate::exact_param(lc.freight),
        crate::exact_param(lc.duty),
        crate::exact_param(lc.insurance),
        crate::exact_param(lc.clearing),
        crate::exact_param(lc.other),
    );
    let id = if lc.id > 0 {
        tx.execute(
            "UPDATE landed_cost SET period=?, date=?, supplier_code=?, supplier_name=?, basis=?,
             freight=?, duty=?, insurance=?, clearing=?, other=?,
             dr_account=?, cr_account=?, bank_account=?, expense_account=?, memo=?, updated_at=? WHERE id=?",
            rusqlite::params![
                lc.period,
                lc.date,
                lc.supplier_code,
                lc.supplier_name,
                lc.basis,
                charges.0,
                charges.1,
                charges.2,
                charges.3,
                charges.4,
                lc.dr_account,
                lc.cr_account,
                lc.bank_account,
                lc.expense_account,
                lc.memo,
                now,
                lc.id
            ],
        )?;
        tx.execute("DELETE FROM landed_cost_item WHERE lcv_id=?1", [lc.id])?;
        lc.id
    } else {
        lc.no = next_no(db, Period::from_ymm(lc.period))?;
        tx.execute(
            "INSERT INTO landed_cost(period, no, date, supplier_code, supplier_name, basis,
             freight, duty, insurance, clearing, other,
             dr_account, cr_account, bank_account, expense_account, status, memo, prepared_by, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,'draft',?16,?17,?18,?18)",
            rusqlite::params![
                lc.period,
                lc.no,
                lc.date,
                lc.supplier_code,
                lc.supplier_name,
                lc.basis,
                charges.0,
                charges.1,
                charges.2,
                charges.3,
                charges.4,
                lc.dr_account,
                lc.cr_account,
                lc.bank_account,
                lc.expense_account,
                lc.memo,
                who,
                now
            ],
        )?;
        tx.last_insert_rowid()
    };
    for l in lines {
        if l.item_code.trim().is_empty() {
            return Err(fincore::FinError::state("明细行必须有存货编码").into());
        }
        if l.po_id <= 0 {
            return Err(fincore::FinError::state(format!(
                "{}：到岸成本必须摊到某张采购订单的货上，不能凭空摊",
                l.item_code
            ))
            .into());
        }
        tx.execute(
            "INSERT INTO landed_cost_item(lcv_id, po_id, item_code, item_name, qty, weight,
             amount, manual_amount, memo) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                id,
                l.po_id,
                l.item_code,
                l.item_name,
                crate::exact_param(l.qty),
                crate::exact_param(l.weight),
                crate::exact_param(l.amount),
                crate::exact_param(l.manual_amount),
                l.memo
            ],
        )?;
    }
    tx.commit()?;
    lc.id = id;
    Ok(id)
}

// ===========================================================================
// 分摊
// ===========================================================================

/// 按基准把附加成本分摊到各行
///
/// **尾差规则**：按权重分摊必然除不尽，尾差挤到**最后一行**，保证
/// `各行分摊额之和 == 总额` 一分不差。少这一步，成本就会凭空少几分钱，
/// 而报表上根本看不出来。
///
/// 手工指定时直接用各行 `manual_amount`，但仍要校验合计等于总额 —— 否则
/// 「手工指定」就变成了悄悄改总额的入口。
pub fn allocate(total: Money, basis: AllocBasis, lines: &mut [LcLine]) -> DbResult<()> {
    if total.is_zero() {
        for l in lines.iter_mut() {
            l.allocated = Money::ZERO;
        }
        return Ok(());
    }
    if basis == AllocBasis::Manual {
        let sum: Money = lines.iter().map(|l| l.manual_amount).sum();
        if (sum - total).abs() > Money::parse("0.005").unwrap_or(Money::ZERO) {
            return Err(fincore::FinError::state(format!(
                "手工指定的各行分摊额合计 {} 与附加成本合计 {} 不符：手工指定不是改总额的入口",
                sum, total
            ))
            .into());
        }
        for l in lines.iter_mut() {
            l.allocated = l.manual_amount;
        }
        return Ok(());
    }

    let weight_of = |l: &LcLine| -> Money {
        match basis {
            AllocBasis::Qty => l.qty,
            AllocBasis::Amount => l.amount,
            AllocBasis::Weight => l.weight,
            AllocBasis::Manual => Money::ZERO,
        }
    };
    let denom: Money = lines.iter().map(weight_of).sum();
    if !denom.is_positive() {
        return Err(fincore::FinError::state(format!(
            "分摊基准「{}」下各行权重合计为 0，无法分摊",
            basis.label()
        ))
        .into());
    }
    let mut allocated_sum = Money::ZERO;
    let last = lines.len() - 1;
    for (i, l) in lines.iter_mut().enumerate() {
        l.allocated = if i == last {
            // 最后一行吸收尾差
            total - allocated_sum
        } else {
            let w = weight_of(l);
            let part = (total * w).checked_div(denom).unwrap_or(Money::ZERO);
            allocated_sum += part;
            part
        };
    }
    Ok(())
}

/// 把分摊额落到「入存货 / 转费用」两段
///
/// 封顶依据是**当前结存价值**（结存数量 × 结存单价），不是结存数量本身：
/// 一件单价 5 万的仪器卖掉后，留下的那点数量也承载不了一笔大额运费。
/// 超出部分必须转费用，否则会造出「数量 0、金额 > 0」的幽灵存货。
pub fn split_to_stock(db: &Db, period: Period, lines: &mut [LcLine]) -> DbResult<()> {
    for l in lines.iter_mut() {
        let method = crate::business::item_cost_method_opt(db, &l.item_code)?
            .unwrap_or(CostMethod::MovingAverage);
        let st = crate::business::stock_state(db, &l.item_code, period, method)?;
        l.on_hand_qty = st.qty;
        if !l.on_hand_qty.is_positive() {
            l.to_stock = Money::ZERO;
            l.to_expense = l.allocated;
            continue;
        }
        let unit = st
            .amount
            .checked_div(st.qty)
            .map(|u| u.round2())
            .unwrap_or(Money::ZERO);
        let capacity = (l.on_hand_qty * unit).round2();
        if l.allocated <= capacity {
            l.to_stock = l.allocated;
            l.to_expense = Money::ZERO;
        } else {
            l.to_stock = capacity;
            l.to_expense = l.allocated - capacity;
        }
    }
    Ok(())
}

// ===========================================================================
// 过账
// ===========================================================================

/// 草稿 → 已过账。条件更新（`AND status='draft'`）防并发：两个界面同时点过账，
/// 只有一个能成功，另一个拿到「不是草稿」而不是把状态覆盖掉。
pub fn post(db: &Db, id: i64, who: &str) -> DbResult<i64> {
    let mut lc = get(db, id)?.ok_or_else(|| fincore::FinError::not_found("到岸成本单不存在"))?;
    if !lc.is_draft() {
        return Err(fincore::FinError::state(format!(
            "到岸成本单 {} 已是「{}」，不能重复过账",
            lc.no,
            status_label(&lc.status)
        ))
        .into());
    }
    let period = Period::from_ymm(lc.period);
    let date = NaiveDate::parse_from_str(&lc.date, "%Y-%m-%d").unwrap_or(period.first_day());

    // 重新分摊 + 封顶：用**当前**结存重算，而不是信任草稿里算好的数。
    // 草稿到过账之间货可能已经卖掉，信任草稿就会把运费挂到已清空的存货上。
    allocate(
        lc.total_charges(),
        AllocBasis::parse(&lc.basis),
        &mut lc.lines,
    )?;
    split_to_stock(db, period, &mut lc.lines)?;

    let total_stock = lc.total_to_stock();
    let total_expense = lc.total_to_expense();

    let tx = db.write_tx()?;
    // 条件更新：先抢状态，抢不到直接失败，避免重复入账
    let n = tx.execute(
        "UPDATE landed_cost SET status='posted', updated_at=?2 WHERE id=?1 AND status='draft'",
        rusqlite::params![
            id,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    if n == 0 {
        return Err(
            fincore::FinError::state("到账成本单已被他人过账，请刷新后重试".to_string()).into(),
        );
    }

    // 回写分摊结果（可追溯：单据上能看出哪部分进了存货、哪部分进了费用）
    for l in &lc.lines {
        tx.execute(
            "UPDATE landed_cost_item SET allocated=?1, to_stock=?2, to_expense=?3 WHERE id=?4",
            rusqlite::params![
                crate::exact_param(l.allocated),
                crate::exact_param(l.to_stock),
                crate::exact_param(l.to_expense),
                l.id
            ],
        )?;
    }

    // 凭证：借方**逐行**入账（带存货辅助 + 结存数量 + 单位成本增量），
    // 转费用的部分走费用科目；贷方一笔。
    //
    // 为什么借方要逐行而不是汇总一条：凭证校验对「核算存货」的科目强制要求
    // 填存货辅助与数量（`engine::validate` 第 186-214 行），汇总一条会被直接拒。
    // 而拆成逐行还有个好处：过账后总账里**存货与应付同时增加**，资产负债表
    // 当场就平了，不用等会计再做一次存货核算。
    //
    // 数量口径：`to_stock` 是这批货**已收货数量**（`l.qty`）上摊的，所以数量取
    // 收货量、单价取「每件多承担了多少成本」——数量 × 单价 = 本行金额，
    // 校验的容差是 0.01，这里天然满足。
    let mut v = Voucher::new(
        period,
        date,
        "记",
        crate::vouchers::next_no_of(&tx, period, "记")?,
    );
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("到岸成本 {}", lc.no);
    let memo = v.memo.clone();
    let mut seq = 0i32;
    for l in &lc.lines {
        if !l.to_stock.is_positive() {
            continue;
        }
        // 收货量取不到（历史数据）就退回结存数量：宁可口径粗一点，
        // 也不能让凭证因为「数量必填」过不去而整笔失败。
        let qty = if l.qty.is_positive() {
            l.qty
        } else {
            l.on_hand_qty
        };
        if !qty.is_positive() {
            continue;
        }
        let unit = l
            .to_stock
            .checked_div(qty)
            .map(|u| u.round_dp(4))
            .unwrap_or(Money::ZERO);
        seq += 1;
        v.push_entry(Entry {
            debit: l.to_stock,
            aux: fincore::AuxRef {
                item: Some(l.item_code.clone()),
                ..Default::default()
            },
            qty: Some(qty),
            price: Some(unit),
            ..Entry::new(seq, lc.dr_account.as_str(), memo.as_str())
        });
    }
    if total_expense.is_positive() {
        // 转费用的借方：没配就退到账套的存货科目（至少是存货类的成本科目），
        // 而不是留空——留空会让凭证少一条分录，凭证层会因不平而拒。
        let acct = if lc.expense_account.trim().is_empty() {
            db.options().biz_accounts.material.clone()
        } else {
            lc.expense_account.clone()
        };
        seq += 1;
        v.push_entry(Entry {
            debit: total_expense,
            ..Entry::new(seq, acct.as_str(), format!("{memo}（货已出清，转费用）"))
        });
    }
    seq += 1;
    v.push_entry(Entry {
        credit: total_stock + total_expense,
        // 贷方带供应商辅助（到岸成本付给货代/供应商，应能进应付往来对账）
        // + 银行账户辅助（贷方科目核算银行账户时必填，凭证层会强校验）
        aux: fincore::AuxRef {
            supplier: (!lc.supplier_code.trim().is_empty()).then(|| lc.supplier_code.clone()),
            bank: (!lc.bank_account.trim().is_empty()).then(|| lc.bank_account.clone()),
            ..Default::default()
        },
        ..Entry::new(seq, lc.cr_account.as_str(), memo.as_str())
    });
    if !v.balanced() {
        return Err(fincore::FinError::state("到岸成本凭证借贷不平，已回滚".to_string()).into());
    }
    let vid = crate::vouchers::save_in(&tx, &mut v)?;

    // 追加存货成本调整流水（数量 0、金额为入存货的分摊额）
    for l in &lc.lines {
        if !l.to_stock.is_zero() {
            let mv_id = crate::business::stock_insert_of(
                &tx,
                &crate::business::StockMove {
                    id: 0,
                    period,
                    biz_date: date,
                    kind: crate::business::StockKind::Adjust,
                    item: l.item_code.clone(),
                    warehouse: String::new(),
                    batch_no: String::new(),
                    qty: Money::ZERO,
                    price: Money::ZERO,
                    amount: l.to_stock,
                    voucher_id: Some(vid),
                    memo: format!("到岸成本 {} 分摊", lc.no),
                },
            )?;
            let _ = mv_id;
        }
    }
    tx.execute(
        "UPDATE landed_cost SET voucher_id=?2 WHERE id=?1",
        rusqlite::params![id, vid],
    )?;
    // 与来源采购订单建边：否则这张单和它摊的那批货在数据上仍是断的。
    // 用事务内版本：边建在事务外的话，提交后建边失败就留下一张
    // 「已过账但查不到来源单据」的单。
    for l in &lc.lines {
        if l.po_id > 0 {
            crate::docflow::link_add_in(&tx, "po", l.po_id, "lcv", id, "到岸成本分摊")?;
        }
    }
    tx.commit()?;
    Ok(vid)
}

pub fn status_label(s: &str) -> &'static str {
    match s {
        "posted" => "已过账",
        _ => "草稿",
    }
}

// ===========================================================================
// 读取
// ===========================================================================

pub fn get(db: &Db, id: i64) -> DbResult<Option<LandedCost>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, no, period, date, supplier_code, supplier_name, basis,
         freight, duty, insurance, clearing, other,
         dr_account, cr_account, bank_account, expense_account, status, voucher_id, memo, prepared_by
         FROM landed_cost WHERE id=?1",
    )?;
    let mut rows = stmt.query([id])?;
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    let id: i64 = r.get(0)?;
    let mut lc = LandedCost {
        id,
        no: r.get(1)?,
        period: r.get(2)?,
        date: r.get(3)?,
        supplier_code: r.get(4)?,
        supplier_name: r.get(5)?,
        basis: r.get(6)?,
        freight: Money::parse_or_zero(&r.get::<_, String>(7)?),
        duty: Money::parse_or_zero(&r.get::<_, String>(8)?),
        insurance: Money::parse_or_zero(&r.get::<_, String>(9)?),
        clearing: Money::parse_or_zero(&r.get::<_, String>(10)?),
        other: Money::parse_or_zero(&r.get::<_, String>(11)?),
        dr_account: r.get(12)?,
        cr_account: r.get(13)?,
        bank_account: r.get(14)?,
        expense_account: r.get(15)?,
        status: r.get(16)?,
        voucher_id: r.get(17)?,
        memo: r.get(18)?,
        prepared_by: r.get(19)?,
        created_at: String::new(),
        updated_at: String::new(),
        lines: Vec::new(),
    };
    let mut st = db.conn().prepare(
        "SELECT id, lcv_id, po_id, item_code, item_name, qty, weight, amount,
             manual_amount, allocated, to_stock, to_expense, memo
             FROM landed_cost_item WHERE lcv_id=?1 ORDER BY id",
    )?;
    let mut line_rows = st.query([id])?;
    while let Some(r) = line_rows.next()? {
        lc.lines.push(LcLine {
            id: r.get(0)?,
            lcv_id: r.get(1)?,
            po_id: r.get(2)?,
            po_no: String::new(),
            item_code: r.get(3)?,
            item_name: r.get(4)?,
            qty: Money::parse_or_zero(&r.get::<_, String>(5)?),
            weight: Money::parse_or_zero(&r.get::<_, String>(6)?),
            amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
            manual_amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
            allocated: Money::parse_or_zero(&r.get::<_, String>(9)?),
            to_stock: Money::parse_or_zero(&r.get::<_, String>(10)?),
            to_expense: Money::parse_or_zero(&r.get::<_, String>(11)?),
            on_hand_qty: Money::ZERO,
            memo: r.get(12)?,
        });
    }
    Ok(Some(lc))
}

pub fn list(db: &Db, period: Period) -> DbResult<Vec<LandedCost>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT id FROM landed_cost WHERE period=?1 ORDER BY date DESC, id DESC")?;
    let ids: Vec<i64> = stmt
        .query_map([period.ymm()], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(lc) = get(db, id)? {
            out.push(lc);
        }
    }
    Ok(out)
}

/// 从采购订单取可分摊的存货行（已收的货才摊得到）
///
/// 刻意只取**净收货大于 0** 的行：没收到的货没有附加成本可摊，摊上去就是
/// 给一份不存在的存货加成本。
pub fn fetch_lines_from_po(db: &Db, po_id: i64) -> DbResult<Vec<LcLine>> {
    let po = crate::scm::po_get(db, po_id)?
        .ok_or_else(|| fincore::FinError::not_found("采购订单不存在"))?;
    let received = crate::procurement::po_receipt_sum(db, po_id)?;
    let mut out = Vec::new();
    for l in &po.lines {
        // 逐行按比例取已收量（po_receipt 只记整单数量，不分行）
        let ordered: Money = po.lines.iter().map(|x| x.qty_ordered).sum();
        let got = if ordered.is_positive() {
            (received * l.qty_ordered)
                .checked_div(ordered)
                .unwrap_or(Money::ZERO)
        } else {
            Money::ZERO
        };
        if !got.is_positive() {
            continue;
        }
        let amount = if l.qty_ordered.is_positive() {
            (received * l.amount)
                .checked_div(ordered)
                .unwrap_or(Money::ZERO)
                .round2()
        } else {
            Money::ZERO
        };
        out.push(LcLine {
            id: 0,
            lcv_id: 0,
            po_id,
            po_no: po.no.clone(),
            item_code: l.item_code.clone(),
            item_name: l.item_name.clone(),
            qty: got,
            weight: Money::ZERO,
            amount,
            ..Default::default()
        });
    }
    Ok(out)
}

/// 某存货在指定期间的附加成本合计（差异表/存货分析用）
pub fn landed_cost_sum(db: &Db, item: &str, period: Period) -> DbResult<Money> {
    let mut total = Money::ZERO;
    let mut st = db.conn().prepare(
        "SELECT i.to_stock FROM landed_cost_item i JOIN landed_cost c ON c.id = i.lcv_id
         WHERE i.item_code=?1 AND c.period=?2 AND c.status='posted'",
    )?;
    let rows = st.query_map(rusqlite::params![item, period.ymm()], |r| {
        r.get::<_, String>(0)
    })?;
    for r in rows {
        total += Money::parse_or_zero(&r?);
    }
    Ok(total)
}

/// 各存货到岸成本汇总（存货分析视图用）
pub fn by_item(db: &Db, period: Period) -> DbResult<BTreeMap<String, Money>> {
    let mut map: BTreeMap<String, Money> = BTreeMap::new();
    let mut st = db.conn().prepare(
        "SELECT i.item_code, i.to_stock FROM landed_cost_item i JOIN landed_cost c ON c.id = i.lcv_id
         WHERE c.period=?1 AND c.status='posted'",
    )?;
    let rows = st.query_map([period.ymm()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for r in rows {
        let (item, amt) = r?;
        *map.entry(item).or_insert(Money::ZERO) += Money::parse_or_zero(&amt);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::{stock_insert, StockKind, StockMove};
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn d(y: i32, mo: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, mo, day).unwrap()
    }
    fn p() -> Period {
        Period::new(2026, 1).unwrap()
    }

    fn line(item: &str, qty: &str, amount: &str) -> LcLine {
        LcLine {
            item_code: item.into(),
            item_name: item.into(),
            qty: m(qty),
            amount: m(amount),
            po_id: 1,
            ..Default::default()
        }
    }

    /// 按货值分摊：尾差必须被最后一行吸收，合计一分不差
    ///
    /// 这是这类算法的经典陷阱：3 行分 100 元，除不尽；若各行独立四舍五入，
    /// 合计会变成 99.99 或 100.01，成本凭空少/多几分而报表上看不出来。
    #[test]
    fn allocation_absorbs_rounding_residual_in_last_line() {
        let mut ls = vec![
            line("A", "1", "33.33"),
            line("B", "1", "33.33"),
            line("C", "1", "33.34"),
        ];
        allocate(m("100"), AllocBasis::Amount, &mut ls).unwrap();
        let sum: Money = ls.iter().map(|l| l.allocated).sum();
        assert_eq!(sum, m("100"), "各行分摊额之和必须等于总额，一分不差");
        // 1/3 这类除不尽的也要守住
        let mut ls2 = vec![
            line("A", "1", "1"),
            line("B", "1", "1"),
            line("C", "1", "1"),
        ];
        allocate(m("100"), AllocBasis::Qty, &mut ls2).unwrap();
        assert_eq!(ls2.iter().map(|l| l.allocated).sum::<Money>(), m("100"));
    }

    /// 权重合计为 0 必须报错，不能悄悄全摊给第一行
    #[test]
    fn allocation_rejects_zero_weight() {
        let mut ls = vec![line("A", "0", "0"), line("B", "0", "0")];
        let e = allocate(m("100"), AllocBasis::Qty, &mut ls).unwrap_err();
        assert!(format!("{e:?}").contains("权重"), "该明确报错：{:?}", e);
    }

    /// 手工指定时合计必须等于总额 —— 否则「手工指定」就成了改总额的暗门
    #[test]
    fn manual_basis_must_sum_to_total() {
        let mut ls = vec![line("A", "1", "100"), line("B", "1", "100")];
        ls[0].manual_amount = m("60");
        ls[1].manual_amount = m("50"); // 合计 110 ≠ 100
        assert!(allocate(m("100"), AllocBasis::Manual, &mut ls).is_err());

        ls[1].manual_amount = m("40"); // 合计 100
        allocate(m("100"), AllocBasis::Manual, &mut ls).unwrap();
        assert_eq!(ls[0].allocated, m("60"));
        assert_eq!(ls[1].allocated, m("40"));
    }

    /// 分摊必须落到「入存货 / 转费用」两段，且转费用是因货已出清
    ///
    /// 回归背景：`StockState::adjust_amount` 无条件写 amount，所以结存数量为 0
    /// 时调成本会造出「数量 0 / 结存金额 > 0」的幽灵存货 —— 资产负债表凭空多一笔
    /// 存货且不报错。这里用金额封顶（结存价值）而不是数量封顶：一件单价 5 万的
    /// 仪器卖掉后，留下的那点数量也承载不了一笔大额运费。
    #[test]
    fn charges_exceeding_stock_value_go_to_expense() {
        let db = mem();
        let dt = d(2026, 1, 5);
        // 进 10 件 × 8 元 → 结存价值 80
        stock_insert(
            &db,
            &StockMove {
                id: 0,
                period: p(),
                biz_date: dt,
                kind: StockKind::Purchase,
                item: "A".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: m("10"),
                price: m("8"),
                amount: m("80"),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        let mut ls = vec![line("A", "10", "80")];
        ls[0].allocated = m("50");
        split_to_stock(&db, p(), &mut ls).unwrap();
        assert_eq!(ls[0].to_stock, m("50"), "结存价值 80，50 全部能入存货");
        assert_eq!(ls[0].to_expense, Money::ZERO);

        // 分摊 120 > 结存价值 80 → 只有 80 进存货，40 转费用
        let mut ls2 = vec![line("A", "10", "80")];
        ls2[0].allocated = m("120");
        split_to_stock(&db, p(), &mut ls2).unwrap();
        assert_eq!(ls2[0].to_stock, m("80"), "入存货的不能超过结存价值");
        assert_eq!(
            ls2[0].to_expense,
            m("40"),
            "超出部分必须转费用，否则是幽灵存货"
        );
        assert_eq!(
            ls2[0].to_stock + ls2[0].to_expense,
            m("120"),
            "两段之和必须等于分摊额"
        );
    }

    /// 货已全部出清 → 整笔转费用，绝不挂存货
    #[test]
    fn charges_on_fully_sold_goods_go_entirely_to_expense() {
        let db = mem();
        let dt = d(2026, 1, 5);
        for (kind, qty, price, amount) in [
            (StockKind::Purchase, "10", "8", "80"),
            (StockKind::Sale, "-10", "0", "0"),
        ] {
            stock_insert(
                &db,
                &StockMove {
                    id: 0,
                    period: p(),
                    biz_date: dt,
                    kind,
                    item: "A".into(),
                    warehouse: String::new(),
                    batch_no: String::new(),
                    qty: m(qty),
                    price: m(price),
                    amount: m(amount),
                    voucher_id: None,
                    memo: String::new(),
                },
            )
            .unwrap();
        }
        let mut ls = vec![line("A", "10", "80")];
        ls[0].allocated = m("30");
        split_to_stock(&db, p(), &mut ls).unwrap();
        assert_eq!(ls[0].on_hand_qty, Money::ZERO);
        assert_eq!(ls[0].to_stock, Money::ZERO, "货已卖光，不能留一分钱在存货");
        assert_eq!(ls[0].to_expense, m("30"));
    }

    // ------------------------------------------------------------------
    // 过账端到端
    // ------------------------------------------------------------------

    fn seed_goods(db: &Db, item: &str, qty: &str, price: &str) {
        let q = m(qty);
        let pr = m(price);
        stock_insert(
            db,
            &StockMove {
                id: 0,
                period: p(),
                biz_date: d(2026, 1, 5),
                kind: StockKind::Purchase,
                item: item.into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: pr,
                amount: (q * pr).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }

    fn mk_lcv(db: &Db, basis: &str, freight: &str) -> LandedCost {
        let mut lc = LandedCost {
            period: p().ymm(),
            date: "2026-01-08".to_string(),
            supplier_code: "S001".to_string(),
            supplier_name: "供应商甲".to_string(),
            basis: basis.to_string(),
            freight: m(freight),
            dr_account: "140301".to_string(),
            cr_account: "220201".to_string(),
            expense_account: "660201".to_string(),
            ..Default::default()
        };
        let lines = vec![line("A", "10", "800"), line("B", "10", "200")];
        save(db, &mut lc, &lines, "u1").unwrap();
        lc
    }

    /// 过账的完整效果：存货金额被抬高、凭证出、与采购单建边、状态变已过账
    #[test]
    fn posting_raises_inventory_and_creates_voucher() {
        let db = mem();
        seed_goods(&db, "A", "10", "80"); // 结存价值 800
        seed_goods(&db, "B", "10", "20"); // 结存价值 200
        let lc = mk_lcv(&db, "amount", "100"); // 运费 100 按货值分摊

        let vid = post(&db, lc.id, "u1").unwrap();
        assert!(vid > 0, "过账应生成凭证");

        let after = get(&db, lc.id).unwrap().unwrap();
        assert_eq!(after.status, "posted");
        // 货值 800 : 200 → A 分摊 80、B 分摊 20
        let a = after.lines.iter().find(|l| l.item_code == "A").unwrap();
        let b = after.lines.iter().find(|l| l.item_code == "B").unwrap();
        assert_eq!(a.to_stock, m("80"), "A 按货值占 80%");
        assert_eq!(b.to_stock, m("20"), "B 按货值占 20%");
        assert_eq!(
            after.total_to_stock(),
            m("100"),
            "全额入存货（结存价值充足）"
        );
        assert_eq!(after.total_to_expense(), Money::ZERO);

        // 存货金额确实被抬高
        let st_a = crate::business::stock_state(&db, "A", p(), CostMethod::MovingAverage).unwrap();
        assert_eq!(st_a.qty, m("10"), "调整不该动数量");
        assert_eq!(st_a.amount, m("880"), "结存金额 800 + 80 = 880");
        let st_b = crate::business::stock_state(&db, "B", p(), CostMethod::MovingAverage).unwrap();
        assert_eq!(st_b.amount, m("220"), "200 + 20");

        // 凭证借贷必平，且借方**逐存货**入账（带存货辅助 + 数量 + 单价）
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert!(v.balanced(), "凭证必须借贷平");
        assert_eq!(v.entries.len(), 3, "2 条借方（逐存货）+ 1 条贷方");
        assert_eq!(v.debit_total(), m("100"));
        assert_eq!(v.credit_total(), m("100"));
        // 借方每条都要带存货辅助与数量——凭证校验对核算存货科目是强制的
        for e in v.entries.iter().filter(|e| e.debit.is_positive()) {
            assert!(
                e.aux.item.is_some(),
                "借方分录必须带存货辅助，缺了总账里看不出是哪个存货涨了：{:?}",
                e
            );
            assert!(
                e.qty.map(|q| q.is_positive()).unwrap_or(false),
                "借方必须填数量"
            );
        }
        // 贷方带供应商辅助，运费才能进应付往来对账
        let cr = v.entries.last().unwrap();
        assert_eq!(
            cr.aux.supplier.as_deref(),
            Some("S001"),
            "贷方应带供应商辅助"
        );

        // 与来源采购单建边（否则这张单和它摊的货在数据上仍是断的）
        assert!(
            crate::docflow::has_link(&db, "po", 1, "lcv").unwrap(),
            "应与来源采购订单建边"
        );
    }

    /// 重复过账必须被拒（条件更新防并发：两个界面同时点，只有一个能成）
    #[test]
    fn double_posting_is_rejected() {
        let db = mem();
        seed_goods(&db, "A", "10", "80");
        let lc = mk_lcv(&db, "amount", "100");
        post(&db, lc.id, "u1").unwrap();
        let e = post(&db, lc.id, "u1").unwrap_err();
        assert!(
            format!("{e:?}").contains("重复过账"),
            "应明确说「已过账，不能重复」，实际 {:?}",
            e
        );
        // 且金额没被加第二次
        let st = crate::business::stock_state(&db, "A", p(), CostMethod::MovingAverage).unwrap();
        assert_eq!(st.amount, m("880"), "不能重复加：{}", st.amount);
    }

    /// 货已出清时过账：费用科目必须出分录，且一分钱都不许留在存货
    #[test]
    fn posting_sold_goods_posts_expense_not_inventory() {
        let db = mem();
        seed_goods(&db, "A", "10", "80");
        // 全部卖掉
        stock_insert(
            &db,
            &StockMove {
                id: 0,
                period: p(),
                biz_date: d(2026, 1, 6),
                kind: StockKind::Sale,
                item: "A".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: m("-10"),
                price: Money::ZERO,
                amount: Money::ZERO,
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        let mut lc = mk_lcv(&db, "qty", "50");
        let only_a = vec![line("A", "10", "800")];
        save(&db, &mut lc, &only_a, "u1").unwrap();

        let vid = post(&db, lc.id, "u1").unwrap();
        let after = get(&db, lc.id).unwrap().unwrap();
        assert_eq!(after.total_to_stock(), Money::ZERO, "货已出清，不得入存货");
        assert_eq!(after.total_to_expense(), m("50"), "整笔转费用");

        // 凭证：借 费用 / 贷 应付（**没有**借存货那条）
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.entries[0].account_code, "660201", "借方应是费用科目");
        assert!(v.balanced());

        // 关键：结存数量 0、结存金额也必须 0（幽灵存货检查）
        let st = crate::business::stock_state(&db, "A", p(), CostMethod::MovingAverage).unwrap();
        assert!(
            st.qty.is_zero() && st.amount.is_zero(),
            "不得留下幽灵存货：数量 {} 金额 {}",
            st.qty,
            st.amount
        );
    }

    /// 部分出清：分摊额超出剩余结存价值的部分转费用，且两段之和等于分摊额
    #[test]
    fn posting_splits_between_stock_and_expense() {
        let db = mem();
        seed_goods(&db, "A", "10", "80"); // 结存价值 800
                                          // 卖掉 9 件 → 剩 1 件 × 80 = 80
        stock_insert(
            &db,
            &StockMove {
                id: 0,
                period: p(),
                biz_date: d(2026, 1, 6),
                kind: StockKind::Sale,
                item: "A".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: m("-9"),
                price: Money::ZERO,
                amount: Money::ZERO,
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        let mut lc = mk_lcv(&db, "qty", "300"); // 运费 300 > 结存价值 80
        let only_a = vec![line("A", "1", "80")];
        save(&db, &mut lc, &only_a, "u1").unwrap();

        let vid = post(&db, lc.id, "u1").unwrap();
        let after = get(&db, lc.id).unwrap().unwrap();
        assert_eq!(after.total_to_stock(), m("80"), "入存货的以结存价值为限");
        assert_eq!(after.total_to_expense(), m("220"), "超出部分转费用");
        assert_eq!(
            after.total_to_stock() + after.total_to_expense(),
            m("300"),
            "两段之和必须等于分摊额"
        );

        // 凭证三条：借存货 80 / 借费用 220 / 贷应付 300
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 3);
        assert!(v.balanced());
        assert_eq!(v.debit_total(), m("300"));
    }

    /// 附件成本为 0 的单不该能保存（否则就是一张空单）
    #[test]
    fn zero_charge_lcv_is_rejected() {
        let db = mem();
        let mut lc = LandedCost {
            period: p().ymm(),
            date: "2026-01-08".to_string(),
            basis: "amount".to_string(),
            dr_account: "140301".to_string(),
            cr_account: "220201".to_string(),
            ..Default::default()
        };
        let e = save(&db, &mut lc, &[line("A", "10", "800")], "u1").unwrap_err();
        assert!(format!("{e:?}").contains("合计为 0"), "实际 {:?}", e);
    }
}
