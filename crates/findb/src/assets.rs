//! 固定资产卡片与折旧明细
//!
//! 卡片只存**静态属性**（原值、年限、方法、开始期间），每月折旧额不落库也能算出来；
//! 但为了能追溯"某张凭证提了哪个月的折旧"，折旧明细仍然落一条记录。

use chrono::NaiveDate;
use fincore::engine::depreciation::{DepInput, DepMethod};
use fincore::{AuxKind, Chart, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

use std::collections::BTreeMap;

/// 资产状态
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssetStatus {
    /// 在用
    InUse,
    /// 停用 / 未使用
    Idle,
    /// 已清理
    Disposed,
}

impl AssetStatus {
    pub fn label(self) -> &'static str {
        match self {
            AssetStatus::InUse => "在用",
            AssetStatus::Idle => "停用",
            AssetStatus::Disposed => "已清理",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            AssetStatus::InUse => "in_use",
            AssetStatus::Idle => "idle",
            AssetStatus::Disposed => "disposed",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "idle" => AssetStatus::Idle,
            "disposed" => AssetStatus::Disposed,
            _ => AssetStatus::InUse,
        }
    }
    pub const ALL: &'static [AssetStatus] = &[
        AssetStatus::InUse,
        AssetStatus::Idle,
        AssetStatus::Disposed,
    ];
}

/// 固定资产卡片
#[derive(Clone, Debug)]
pub struct Asset {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub category: String,
    pub spec: String,
    pub dept: String,
    /// 资产科目（默认 1601 固定资产）
    pub asset_account: String,
    /// 累计折旧科目（默认 1602）
    pub dep_account: String,
    /// 折旧费用科目（默认 6602 管理费用）
    pub expense_account: String,
    pub original_value: Money,
    pub residual_rate: Money,
    pub life_months: i32,
    pub method: DepMethod,
    pub start_period: Period,
    pub disposed_period: Option<Period>,
    pub dispose_amount: Option<Money>,
    pub status: AssetStatus,
    pub voucher_id: Option<i64>,
    pub memo: String,
}

impl Asset {
    /// 转成引擎输入并做业务校验
    pub fn dep_input(&self) -> Result<DepInput, fincore::FinError> {
        let input = DepInput {
            original: self.original_value,
            residual_rate: self.residual_rate,
            life_months: self.life_months,
            method: self.method,
        };
        input.validate()?;
        Ok(input)
    }
    /// 已计提期数（含本期）
    pub fn elapsed_months(&self, at: Period) -> i32 {
        if at.ymm() < self.start_period.ymm() {
            return 0;
        }
        (at.year() - self.start_period.year()) * 12
            + (at.month() as i32 - self.start_period.month() as i32)
            + 1
    }
    /// 该期是否还应计提（停用不计提、清理当月仍提、超过年限不再提）
    pub fn should_depreciate(&self, at: Period) -> bool {
        if self.status == AssetStatus::Disposed {
            return false;
        }
        if self.status == AssetStatus::Idle {
            return false;
        }
        if at.ymm() < self.start_period.ymm() {
            return false;
        }
        if let Some(d) = self.disposed_period {
            // 清理当月照提，次月停提
            if at.ymm() > d.ymm() {
                return false;
            }
        }
        self.elapsed_months(at) <= self.life_months
    }
}

/// 折旧明细（每月一条）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepRecord {
    pub id: i64,
    pub asset_id: i64,
    pub period: Period,
    pub amount: Money,
    pub accum: Money,
    pub net_value: Money,
    pub voucher_id: Option<i64>,
}

fn map_asset(r: &rusqlite::Row) -> rusqlite::Result<Asset> {
    let dp: Option<i64> = r.get(14)?;
    let da: Option<String> = r.get(15)?;
    Ok(Asset {
        id: r.get(0)?,
        code: r.get(1)?,
        name: r.get(2)?,
        category: r.get(3)?,
        spec: r.get(4)?,
        dept: r.get(5)?,
        asset_account: r.get(6)?,
        dep_account: r.get(7)?,
        expense_account: r.get(8)?,
        original_value: Money::parse_or_zero(&r.get::<_, String>(9)?),
        residual_rate: Money::parse_or_zero(&r.get::<_, String>(10)?),
        life_months: r.get(11)?,
        method: DepMethod::parse(&r.get::<_, String>(12)?),
        start_period: Period::from_ymm(r.get(13)?),
        disposed_period: dp.map(|v| Period::from_ymm(v as i32)),
        dispose_amount: da.map(|s| Money::parse_or_zero(&s)),
        status: AssetStatus::parse(&r.get::<_, String>(16)?),
        voucher_id: r.get(17)?,
        memo: r.get(18)?,
    })
}

const COLS: &str = "id,code,name,category,spec,dept,asset_account,dep_account,expense_account,
     original_value,residual_rate,life_months,method,start_period,disposed_period,dispose_amount,
     status,voucher_id,memo";

pub fn list(db: &Db) -> DbResult<Vec<Asset>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {COLS} FROM fixed_asset ORDER BY code"))?;
    let rows = st.query_map([], map_asset)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 按状态过滤
pub fn list_by_status(db: &Db, status: AssetStatus) -> DbResult<Vec<Asset>> {
    Ok(list(db)?.into_iter().filter(|a| a.status == status).collect())
}

/// 某期间应计提的卡片
pub fn active_at(db: &Db, period: Period) -> DbResult<Vec<Asset>> {
    Ok(list(db)?
        .into_iter()
        .filter(|a| a.should_depreciate(period))
        .collect())
}

pub fn get(db: &Db, id: i64) -> DbResult<Option<Asset>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM fixed_asset WHERE id=?1"),
            rusqlite::params![id],
            map_asset,
        )
        .optional()
        .map_err(Into::into)
}

pub fn get_by_code(db: &Db, code: &str) -> DbResult<Option<Asset>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM fixed_asset WHERE code=?1"),
            rusqlite::params![code],
            map_asset,
        )
        .optional()
        .map_err(Into::into)
}

/// 生成下一个资产编码：`GD` + 4 位序号
pub fn next_code(db: &Db) -> DbResult<String> {
    let max: i64 = db
        .conn()
        .query_row(
            "SELECT COALESCE(MAX(CAST(SUBSTR(code,3) AS INTEGER)),0) FROM fixed_asset
             WHERE code GLOB 'GD[0-9]*'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(format!("GD{:04}", max + 1))
}

pub fn insert(db: &Db, a: &Asset) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO fixed_asset(code,name,category,spec,dept,asset_account,dep_account,
            expense_account,original_value,residual_rate,life_months,method,start_period,
            disposed_period,dispose_amount,status,voucher_id,memo)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        rusqlite::params![
            a.code,
            a.name,
            a.category,
            a.spec,
            a.dept,
            a.asset_account,
            a.dep_account,
            a.expense_account,
            crate::money_param(a.original_value),
            crate::exact_param(a.residual_rate),
            a.life_months,
            a.method.code(),
            a.start_period.ymm(),
            a.disposed_period.map(|p| p.ymm()),
            a.dispose_amount.map(crate::money_param),
            a.status.code(),
            a.voucher_id,
            a.memo
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn update(db: &Db, a: &Asset) -> DbResult<()> {
    update_on(db.conn(), a)
}

/// 同 `update`，但只依赖连接，可在调用方的事务内执行
pub fn update_on(conn: &rusqlite::Connection, a: &Asset) -> DbResult<()> {
    conn.execute(
        "UPDATE fixed_asset SET code=?2,name=?3,category=?4,spec=?5,dept=?6,asset_account=?7,
            dep_account=?8,expense_account=?9,original_value=?10,residual_rate=?11,life_months=?12,
            method=?13,start_period=?14,disposed_period=?15,dispose_amount=?16,status=?17,
            voucher_id=?18,memo=?19 WHERE id=?1",
        rusqlite::params![
            a.id,
            a.code,
            a.name,
            a.category,
            a.spec,
            a.dept,
            a.asset_account,
            a.dep_account,
            a.expense_account,
            crate::money_param(a.original_value),
            crate::exact_param(a.residual_rate),
            a.life_months,
            a.method.code(),
            a.start_period.ymm(),
            a.disposed_period.map(|p| p.ymm()),
            a.dispose_amount.map(crate::money_param),
            a.status.code(),
            a.voucher_id,
            a.memo
        ],
    )?;
    Ok(())
}

/// 卡片字段级变更记录（对标金蝶固定资产「变动历史」）
#[derive(Clone, Debug, serde::Serialize)]
pub struct AssetChange {
    pub id: i64,
    pub asset_id: i64,
    pub ts: String,
    pub who: String,
    pub field: String,
    pub old_value: String,
    pub new_value: String,
    pub memo: String,
}

fn map_change(r: &rusqlite::Row) -> rusqlite::Result<AssetChange> {
    Ok(AssetChange {
        id: r.get(0)?,
        asset_id: r.get(1)?,
        ts: r.get(2)?,
        who: r.get(3)?,
        field: r.get(4)?,
        old_value: r.get(5)?,
        new_value: r.get(6)?,
        memo: r.get(7)?,
    })
}

/// 某卡片的变更历史（倒序）
pub fn changes(db: &Db, asset_id: i64) -> DbResult<Vec<AssetChange>> {
    let mut st = db.conn().prepare(
        "SELECT id,asset_id,ts,who,field,old_value,new_value,memo FROM asset_change
         WHERE asset_id=?1 ORDER BY id DESC",
    )?;
    let rows = st
        .query_map(rusqlite::params![asset_id], map_change)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 更新卡片并**同事务记录字段级变更**（只写实际变化的字段；`old` 为更新前快照）
pub fn update_logged(db: &Db, old: &Asset, a: &Asset, who: &str, memo: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    update_on(&tx, a)?;
    let diffs: Vec<(&str, String, String)> = vec![
        ("名称", old.name.clone(), a.name.clone()),
        ("类别", old.category.clone(), a.category.clone()),
        ("规格", old.spec.clone(), a.spec.clone()),
        ("使用部门", old.dept.clone(), a.dept.clone()),
        ("资产科目", old.asset_account.clone(), a.asset_account.clone()),
        ("累计折旧科目", old.dep_account.clone(), a.dep_account.clone()),
        ("折旧费用科目", old.expense_account.clone(), a.expense_account.clone()),
        ("原值", old.original_value.fmt_money(), a.original_value.fmt_money()),
        ("残值率", crate::exact_param(old.residual_rate), crate::exact_param(a.residual_rate)),
        ("使用年限（月）", old.life_months.to_string(), a.life_months.to_string()),
        ("折旧方法", old.method.label().to_string(), a.method.label().to_string()),
        ("启用期间", old.start_period.label(), a.start_period.label()),
        ("状态", old.status.label().to_string(), a.status.label().to_string()),
        ("备注", old.memo.clone(), a.memo.clone()),
    ]
    .into_iter()
    .filter(|(_, o, n)| o != n)
    .collect();
    if !diffs.is_empty() {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        for (field, o, n) in diffs {
            tx.execute(
                "INSERT INTO asset_change(asset_id,ts,who,field,old_value,new_value,memo)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![a.id, ts, who, field, o, n, memo],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// 删除卡片。已提过折旧的卡片不允许直接删，避免账实不符。
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM asset_depreciation WHERE asset_id=?1",
        rusqlite::params![id],
        |r| r.get(0),
    )?;
    if n > 0 {
        return Err(fincore::FinError::msg(format!(
            "该卡片已计提 {n} 期折旧，不能删除。请走资产清理流程。"
        ))
        .into());
    }
    db.conn()
        .execute("DELETE FROM fixed_asset WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

// ---------------- 折旧明细 ----------------

fn map_dep(r: &rusqlite::Row) -> rusqlite::Result<DepRecord> {
    Ok(DepRecord {
        id: r.get(0)?,
        asset_id: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        amount: Money::parse_or_zero(&r.get::<_, String>(3)?),
        accum: Money::parse_or_zero(&r.get::<_, String>(4)?),
        net_value: Money::parse_or_zero(&r.get::<_, String>(5)?),
        voucher_id: r.get(6)?,
    })
}

pub fn dep_list(db: &Db, asset_id: i64) -> DbResult<Vec<DepRecord>> {
    let mut st = db.conn().prepare(
        "SELECT id,asset_id,period,amount,accum,net_value,voucher_id FROM asset_depreciation
         WHERE asset_id=?1 ORDER BY period",
    )?;
    let rows = st
        .query_map(rusqlite::params![asset_id], map_dep)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn dep_list_period(db: &Db, period: Period) -> DbResult<Vec<DepRecord>> {
    let mut st = db.conn().prepare(
        "SELECT id,asset_id,period,amount,accum,net_value,voucher_id FROM asset_depreciation
         WHERE period=?1 ORDER BY asset_id",
    )?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], map_dep)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn dep_of(db: &Db, asset_id: i64, period: Period) -> DbResult<Option<DepRecord>> {
    db.conn()
        .query_row(
            "SELECT id,asset_id,period,amount,accum,net_value,voucher_id FROM asset_depreciation
             WHERE asset_id=?1 AND period=?2",
            rusqlite::params![asset_id, period.ymm()],
            map_dep,
        )
        .optional()
        .map_err(Into::into)
}

/// 写入 / 覆盖某期折旧（幂等，重复计提不会产生两条）
pub fn dep_upsert(db: &Db, r: &DepRecord) -> DbResult<()> {
    dep_upsert_on(db.conn(), r)
}

/// 同 `dep_upsert`，但只依赖连接，可在调用方的事务内执行
pub fn dep_upsert_on(conn: &rusqlite::Connection, r: &DepRecord) -> DbResult<()> {
    conn.execute(
        "INSERT INTO asset_depreciation(asset_id,period,amount,accum,net_value,voucher_id)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(asset_id,period) DO UPDATE SET
            amount=excluded.amount, accum=excluded.accum,
            net_value=excluded.net_value, voucher_id=excluded.voucher_id",
        rusqlite::params![
            r.asset_id,
            r.period.ymm(),
            crate::money_param(r.amount),
            crate::money_param(r.accum),
            crate::money_param(r.net_value),
            r.voucher_id
        ],
    )?;
    Ok(())
}

/// 删除某期折旧（重新计提 / 反结账时用）
pub fn dep_delete_period(db: &Db, period: Period) -> DbResult<usize> {
    let n = db.conn().execute(
        "DELETE FROM asset_depreciation WHERE period=?1",
        rusqlite::params![period.ymm()],
    )?;
    Ok(n)
}

/// 截至某期前（不含该期）的累计折旧
pub fn accum_before(db: &Db, asset_id: i64, period: Period) -> DbResult<Money> {
    let s: Option<String> = db
        .conn()
        .query_row(
            "SELECT accum FROM asset_depreciation WHERE asset_id=?1 AND period<?2
             ORDER BY period DESC LIMIT 1",
            rusqlite::params![asset_id, period.ymm()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(s.map(|x| Money::parse_or_zero(&x)).unwrap_or(Money::ZERO))
}

/// 计算某卡片在某期的应计折旧（不落库，给预览用）
pub fn planned_dep(a: &Asset, period: Period) -> DbResult<Option<Money>> {
    if !a.should_depreciate(period) {
        return Ok(None);
    }
    let input = a.dep_input()?;
    let seq = a.elapsed_months(period);
    let rows = fincore::engine::depreciation::schedule(&input)?;
    Ok(rows.get((seq - 1) as usize).map(|r| r.amount))
}

/// 某期折旧计划行（生成凭证与两端界面共用）
#[derive(Clone, Debug, serde::Serialize)]
pub struct DepPlanRow {
    pub asset_id: i64,
    pub code: String,
    pub name: String,
    pub dept: String,
    pub expense_account: String,
    pub dep_account: String,
    pub amount: Money,
    pub accum: Money,
    pub net: Money,
}

/// 计提折旧结果
#[derive(Clone, Debug, serde::Serialize)]
pub struct DepAccrual {
    pub count: usize,
    pub total: Money,
    pub voucher_id: Option<i64>,
    pub voucher_no: Option<String>,
    /// true = 本期已计提过（幂等返回，未生成新凭证）
    pub already: bool,
}

/// 计算某期折旧计划（不改库）。已落库的按落库值，没落库的按"前期累计 + 本期应提"推算。
pub fn dep_plan(db: &Db, period: Period) -> DbResult<Vec<DepPlanRow>> {
    let mut plan = Vec::new();
    for a in list(db)? {
        let amount = planned_dep(&a, period)?.unwrap_or(Money::ZERO);
        let booked = dep_of(db, a.id, period)?;
        let accum = match booked {
            Some(r) => r.accum,
            None => accum_before(db, a.id, period)? + amount,
        };
        if amount.is_zero() && !a.should_depreciate(period) {
            continue;
        }
        plan.push(DepPlanRow {
            asset_id: a.id,
            code: a.code.clone(),
            name: a.name.clone(),
            dept: a.dept.clone(),
            expense_account: a.expense_account.clone(),
            dep_account: a.dep_account.clone(),
            amount,
            accum,
            net: a.original_value - accum,
        });
    }
    Ok(plan)
}

/// 计提某期折旧：折旧明细与折旧凭证在同一事务内落库；本期已计提则幂等返回。
pub fn depreciate_period(db: &Db, period: Period, who: &str) -> DbResult<DepAccrual> {
    // 幂等：本期已有折旧记录就不再生成新凭证（重算请先删除本期折旧）
    let existing = dep_list_period(db, period)?;
    if !existing.is_empty() {
        let voucher_id = existing.iter().find_map(|r| r.voucher_id);
        let voucher_no = match voucher_id {
            Some(id) => db
                .conn()
                .query_row(
                    "SELECT word, no FROM voucher WHERE id=?1",
                    rusqlite::params![id],
                    |r| {
                        let w: String = r.get(0)?;
                        let n: i32 = r.get(1)?;
                        Ok(format!("{w}-{n:04}"))
                    },
                )
                .optional()?,
            None => None,
        };
        return Ok(DepAccrual {
            count: existing.len(),
            total: existing.iter().map(|r| r.amount).sum(),
            voucher_id,
            voucher_no,
            already: true,
        });
    }

    let plan = dep_plan(db, period)?;
    let total: Money = plan.iter().map(|r| r.amount).sum();
    if total.is_zero() {
        return Err(fincore::FinError::msg("本期折旧额为 0，无需计提").into());
    }
    let chart = crate::accounts::chart(db)?;
    let word = db
        .voucher_words()
        .first()
        .cloned()
        .unwrap_or_else(|| "记".to_string());

    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, &word)?;
    let mut v = Voucher::new(period, period.last_day(), word, no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("{}计提固定资产折旧", period.label());

    // 借方按「部门 + 费用科目」汇总；贷方按累计折旧科目汇总
    let mut debits: BTreeMap<(String, String), Money> = BTreeMap::new();
    let mut credits: BTreeMap<String, Money> = BTreeMap::new();
    let mut count = 0usize;
    for r in &plan {
        if r.amount.is_zero() {
            continue;
        }
        count += 1;
        *debits
            .entry((r.dept.clone(), r.expense_account.clone()))
            .or_insert(Money::ZERO) += r.amount;
        *credits.entry(r.dep_account.clone()).or_insert(Money::ZERO) += r.amount;
    }

    let summary = format!("计提{}折旧", period.label());
    let mut line = 1i32;
    for ((dept, code), amt) in &debits {
        let mut e = Entry::new(line, code.clone(), &summary);
        e.debit = *amt;
        fill_dep_aux(&chart, &mut e, code, dept)?;
        v.push_entry(e);
        line += 1;
    }
    for (code, amt) in &credits {
        let mut e = Entry::new(line, code.clone(), &summary);
        e.credit = *amt;
        fill_dep_aux(&chart, &mut e, code, "")?;
        v.push_entry(e);
        line += 1;
    }
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    for r in &plan {
        if r.amount.is_zero() {
            continue;
        }
        dep_upsert_on(
            &tx,
            &DepRecord {
                id: 0,
                asset_id: r.asset_id,
                period,
                amount: r.amount,
                accum: r.accum,
                net_value: r.net,
                voucher_id: Some(vid),
            },
        )?;
    }
    crate::log_on(
        &tx,
        who,
        "固定资产",
        "计提折旧",
        &format!("{} 凭证#{} 金额 {}", period.label(), vid, total.fmt_money()),
    )?;
    tx.commit()?;
    Ok(DepAccrual {
        count,
        total,
        voucher_id: Some(vid),
        voucher_no: Some(v.voucher_no()),
        already: false,
    })
}

/// 折旧凭证的辅助核算填充：只支持部门（其余维度无法自动确定）
fn fill_dep_aux(chart: &Chart, e: &mut Entry, code: &str, dept: &str) -> DbResult<()> {
    let acct = chart.get(code).ok_or_else(|| {
        fincore::FinError::msg(format!("科目 {code} 不存在，请先在科目表里维护"))
    })?;
    for k in acct.aux.list() {
        match k {
            AuxKind::Dept => {
                if dept.trim().is_empty() {
                    return Err(fincore::FinError::msg(format!(
                        "科目 {code} {} 要求核算部门，请先填写资产卡片的「使用部门」",
                        acct.name
                    ))
                    .into());
                }
                e.aux.dept = Some(dept.to_string());
            }
            other => {
                return Err(fincore::FinError::msg(format!(
                    "科目 {code} {} 要求核算{}，折旧凭证无法自动填写，请手工制单",
                    acct.name,
                    other.label()
                ))
                .into());
            }
        }
    }
    Ok(())
}

/// 资产台账：卡片 + 累计折旧 + 净值
#[derive(Clone, Debug)]
pub struct AssetLedgerRow {
    pub asset: Asset,
    pub accum: Money,
    pub net: Money,
    pub months: i32,
}

pub fn ledger(db: &Db, at: Period) -> DbResult<Vec<AssetLedgerRow>> {
    let mut out = Vec::new();
    for a in list(db)? {
        // 优先取落库值（可能与理论值不同，比如手工调整过），没有再算
        let (accum, months) = match dep_of(db, a.id, at)? {
            Some(r) => (r.accum, a.elapsed_months(at)),
            None => {
                let input = match a.dep_input() {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                let seq = a.elapsed_months(at);
                let m = seq.min(a.life_months).max(0);
                let rows = fincore::engine::depreciation::schedule(&input)?;
                let acc = rows
                    .get((m as usize).saturating_sub(1))
                    .map(|r| r.accum)
                    .unwrap_or_else(|| accum_before(db, a.id, at).unwrap_or(Money::ZERO));
                (acc, m)
            }
        };
        let net = a.original_value - accum;
        out.push(AssetLedgerRow {
            asset: a,
            accum,
            net,
            months,
        });
    }
    Ok(out)
}

/// 资产清理：标记状态并删除清理期之后的折旧记录（同一事务，避免半更新）
/// 资产清理（处置）：置状态 + 截断未来折旧 + **同事务生成清理转销凭证**（对标金蝶固定资产清理第一步）：
/// 借 累计折旧（截至清理期已计提）/ 借 固定资产减值准备（如有）/
/// 借或贷 固定资产清理（账面净值）/ 贷 固定资产原值。
/// 说明：变卖收款与清理净损益结转按实际收付另行制单（1606 余额结平后转 6301/6711）；
/// 返回本次生成的凭证 id。
pub fn dispose(db: &Db, id: i64, period: Period, amount: Money, who: &str) -> DbResult<i64> {
    let mut a = match get(db, id)? {
        Some(a) => a,
        None => return Err(fincore::FinError::not_found("资产卡片").into()),
    };
    if a.status == AssetStatus::Disposed {
        return Err(fincore::FinError::state("该卡片已清理").into());
    }
    let chart = crate::accounts::chart(db)?;
    a.status = AssetStatus::Disposed;
    a.disposed_period = Some(period);
    a.dispose_amount = Some(amount);
    let tx = db.write_tx()?;
    update_on(&tx, &a)?;
    tx.execute(
        "DELETE FROM asset_depreciation WHERE asset_id=?1 AND period>?2",
        rusqlite::params![id, period.ymm()],
    )?;
    let accum = accum_at_conn(&tx, id, period)?;
    let impair = impairment_sum_conn(&tx, id)?;
    let net = a.original_value - accum - impair;
    let memo = format!("资产清理转销 {} {}", a.code, a.name);
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, period.last_day(), "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = memo.clone();
    let mut line = 1;
    if !accum.is_zero() {
        let mut e = Entry::new(line, a.dep_account.as_str(), memo.as_str());
        e.debit = accum;
        fill_dep_aux(&chart, &mut e, &a.dep_account, &a.dept)?;
        v.push_entry(e);
        line += 1;
    }
    if !impair.is_zero() {
        let mut e = Entry::new(line, "1603", memo.as_str());
        e.debit = impair;
        v.push_entry(e);
        line += 1;
    }
    if !net.is_zero() {
        let mut e = Entry::new(line, "1606", memo.as_str());
        if net.is_positive() {
            e.debit = net;
        } else {
            e.credit = -net;
        }
        v.push_entry(e);
        line += 1;
    }
    let mut e = Entry::new(line, a.asset_account.as_str(), memo.as_str());
    e.credit = a.original_value;
    fill_dep_aux(&chart, &mut e, &a.asset_account, &a.dept)?;
    v.push_entry(e);
    v.renumber();
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    tx.commit()?;
    Ok(vid)
}

/// 截至指定期间（含）的累计折旧（取最近一期落库累计值）
fn accum_at_conn(conn: &rusqlite::Connection, asset_id: i64, period: Period) -> DbResult<Money> {
    let s: Option<String> = conn
        .query_row(
            "SELECT accum FROM asset_depreciation WHERE asset_id=?1 AND period<=?2
             ORDER BY period DESC LIMIT 1",
            rusqlite::params![asset_id, period.ymm()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(s.map(|x| Money::parse_or_zero(&x)).unwrap_or(Money::ZERO))
}

/// 连接版减值合计（供事务内使用）
fn impairment_sum_conn(conn: &rusqlite::Connection, asset_id: i64) -> DbResult<Money> {
    let mut st = conn.prepare("SELECT amount FROM asset_impairment WHERE asset_id=?1")?;
    let rows = st
        .query_map([asset_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| Money::parse_or_zero(s)).sum())
}

/// 解析业务日期（资产模块只在导入 CSV 时用得到）
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(s, "%Y/%m/%d").ok())
}

// ===========================================================================
// 资产类别 / 减值 / 附属设备 / 盘点
// ===========================================================================

/// 资产类别清单（去重）
pub fn categories(db: &Db) -> DbResult<Vec<String>> {
    let mut st = db.conn().prepare(
        "SELECT DISTINCT category FROM fixed_asset WHERE category <> '' ORDER BY category",
    )?;
    let rows = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 资产减值：记录减值金额
pub fn impair(db: &Db, asset_id: i64, period: Period, amount: Money, memo: &str) -> DbResult<i64> {
    if amount.is_zero() {
        return Err(fincore::FinError::msg("减值金额不能为 0").into());
    }
    db.conn().execute(
        "INSERT INTO asset_impairment(asset_id,period,amount,memo) VALUES(?1,?2,?3,?4)",
        rusqlite::params![asset_id, period.ymm(), crate::money_param(amount), memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn impairment_sum(db: &Db, asset_id: i64) -> DbResult<Money> {
    let mut st = db.conn().prepare("SELECT amount FROM asset_impairment WHERE asset_id=?1")?;
    let rows = st
        .query_map([asset_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| Money::parse_or_zero(s)).sum())
}

/// 固定资产 ↔ 总账对账行
#[derive(Clone, Debug, serde::Serialize)]
pub struct AssetGlRow {
    pub account_code: String,
    pub account_name: String,
    /// 原值 / 累计折旧
    pub kind: String,
    /// 资产模块侧金额（未清理卡片；原值为正，累计折旧取绝对值）
    pub asset_value: Money,
    /// 总账侧余额（仅已记账 H-3；累计折旧已取绝对值）
    pub gl_value: Money,
    /// 差异 = 资产侧 − 总账侧
    pub diff: Money,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AssetGlReport {
    pub rows: Vec<AssetGlRow>,
    pub cost_asset: Money,
    pub cost_gl: Money,
    pub cost_diff: Money,
    pub dep_asset: Money,
    pub dep_gl: Money,
    pub dep_diff: Money,
}

/// 固定资产 ↔ 总账对账（对标金蝶固定资产与总账对账）：
/// 资产侧 = 启用期 ≤ 期间且未清理（清理期晚于期间）卡片的原值 / 截至期间累计折旧；
/// 总账侧 = 各资产/累计折旧科目期末余额（仅已记账，H-3）；逐科目差异高亮。
/// 口径说明：减值准备（1603）暂不纳入资产侧（卡片侧只记减值累计、无净值口径）。
pub fn gl_reconcile(db: &Db, at: Period) -> DbResult<AssetGlReport> {
    use crate::balances::{BalanceQuery, BalanceSnapshot};
    let all = list(db)?;
    let names: std::collections::HashMap<String, String> = crate::accounts::list(db)?
        .into_iter()
        .map(|a| (a.code, a.name))
        .collect();
    let mut cost_map: BTreeMap<String, Money> = BTreeMap::new();
    let mut dep_map: BTreeMap<String, Money> = BTreeMap::new();
    let mut codes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for a in &all {
        if !a.asset_account.is_empty() {
            codes.insert(a.asset_account.clone());
        }
        if !a.dep_account.is_empty() {
            codes.insert(a.dep_account.clone());
        }
        let disposed = a
            .disposed_period
            .map(|d| d.ymm() <= at.ymm())
            .unwrap_or(false);
        if a.start_period.ymm() > at.ymm() || disposed {
            continue;
        }
        let e = cost_map.entry(a.asset_account.clone()).or_insert(Money::ZERO);
        *e = *e + a.original_value;
        let accum = accum_at_conn(db.conn(), a.id, at)?;
        if !accum.is_zero() {
            let e = dep_map.entry(a.dep_account.clone()).or_insert(Money::ZERO);
            *e = *e + accum;
        }
    }
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(at))?;
    let mut rows = Vec::new();
    let (mut ca, mut cg, mut da, mut dg) = (
        Money::ZERO,
        Money::ZERO,
        Money::ZERO,
        Money::ZERO,
    );
    for code in codes {
        let is_dep = all.iter().any(|a| a.dep_account == code)
            && !all.iter().any(|a| a.asset_account == code);
        let asset_value = if is_dep {
            dep_map.get(&code).copied().unwrap_or(Money::ZERO)
        } else {
            cost_map.get(&code).copied().unwrap_or(Money::ZERO)
        };
        let end = snap.for_account(&code, None).end();
        let gl_value = if is_dep && end.is_negative() { -end } else { end };
        let diff = asset_value - gl_value;
        if is_dep {
            da = da + asset_value;
            dg = dg + gl_value;
        } else {
            ca = ca + asset_value;
            cg = cg + gl_value;
        }
        rows.push(AssetGlRow {
            account_code: code.clone(),
            account_name: names.get(&code).cloned().unwrap_or_default(),
            kind: if is_dep { "累计折旧".into() } else { "原值".into() },
            asset_value,
            gl_value,
            diff,
        });
    }
    Ok(AssetGlReport {
        rows,
        cost_asset: ca,
        cost_gl: cg,
        cost_diff: ca - cg,
        dep_asset: da,
        dep_gl: dg,
        dep_diff: da - dg,
    })
}

/// 附属设备
#[derive(Clone, Debug)]
pub struct Accessory {
    pub id: i64,
    pub asset_id: i64,
    pub name: String,
    pub spec: String,
    pub qty: i32,
    pub memo: String,
}

pub fn accessory_add(db: &Db, asset_id: i64, name: &str, spec: &str, qty: i32, memo: &str) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO asset_accessory(asset_id,name,spec,qty,memo) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![asset_id, name, spec, qty, memo],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn accessory_list(db: &Db, asset_id: i64) -> DbResult<Vec<Accessory>> {
    let mut st = db.conn().prepare(
        "SELECT id,asset_id,name,spec,qty,memo FROM asset_accessory WHERE asset_id=?1 ORDER BY id",
    )?;
    let rows = st
        .query_map([asset_id], |r| {
            Ok(Accessory {
                id: r.get(0)?,
                asset_id: r.get(1)?,
                name: r.get(2)?,
                spec: r.get(3)?,
                qty: r.get(4)?,
                memo: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn accessory_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn().execute("DELETE FROM asset_accessory WHERE id=?1", [id])?;
    Ok(())
}

/// 资产盘点单
#[derive(Clone, Debug, serde::Serialize)]
pub struct AssetCount {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub status: String, // draft / posted
    pub prepared_by: String,
    pub memo: String,
    /// (asset_id, found) 明细
    pub lines: Vec<(i64, bool, String)>,
}

pub fn ac_next_no(db: &Db, period: Period) -> DbResult<String> {
    let prefix = format!("ZCPD{:04}{:02}", period.year(), period.month());
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM asset_count WHERE no LIKE ?1",
        rusqlite::params![format!("{prefix}%")],
        |r| r.get(0),
    )?;
    Ok(format!("{prefix}-{:03}", n + 1))
}

pub fn ac_save(db: &Db, c: &mut AssetCount) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let id = if c.id > 0 {
        tx.execute(
            "UPDATE asset_count SET period=?2, date=?3, status=?4, prepared_by=?5, memo=?6 WHERE id=?1",
            rusqlite::params![c.id, c.period.ymm(), c.date.format("%Y-%m-%d").to_string(), c.status, c.prepared_by, c.memo],
        )?;
        c.id
    } else {
        tx.execute(
            "INSERT INTO asset_count(no,period,date,status,prepared_by,memo) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![c.no, c.period.ymm(), c.date.format("%Y-%m-%d").to_string(), c.status, c.prepared_by, c.memo],
        )?;
        tx.last_insert_rowid()
    };
    tx.execute("DELETE FROM asset_count_line WHERE ac_id=?1", [id])?;
    for (asset_id, found, memo) in &c.lines {
        tx.execute(
            "INSERT INTO asset_count_line(ac_id,asset_id,found,memo) VALUES(?1,?2,?3,?4)",
            rusqlite::params![id, asset_id, if *found { 1 } else { 0 }, memo],
        )?;
    }
    tx.commit()?;
    c.id = id;
    Ok(id)
}

/// 盘点过账：盘亏（found=false）的资产标记为 Idle 并记录
pub fn ac_post(db: &Db, id: i64) -> DbResult<usize> {
    let mut st = db.conn().prepare(
        "SELECT asset_id, found FROM asset_count_line WHERE ac_id=?1",
    )?;
    let lines: Vec<(i64, bool)> = st
        .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? != 0)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut n = 0;
    let tx = db.write_tx()?;
    for (asset_id, found) in lines {
        if !found {
            if let Some(mut a) = get(db, asset_id)? {
                if a.status != AssetStatus::Disposed {
                    a.status = AssetStatus::Idle;
                    a.memo = format!("{} 盘亏", a.memo);
                    update(db, &a)?;
                    n += 1;
                }
            }
        }
    }
    tx.execute("UPDATE asset_count SET status='posted' WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(n)
}

/// 资产盘点单列表（含明细，倒序，最近 200 张）
pub fn ac_list(db: &Db) -> DbResult<Vec<AssetCount>> {
    let mut st = db.conn().prepare(
        "SELECT id,no,period,date,status,prepared_by,memo FROM asset_count ORDER BY id DESC LIMIT 200",
    )?;
    let mut out: Vec<AssetCount> = st
        .query_map([], |r| {
            let date: String = r.get(3)?;
            Ok(AssetCount {
                id: r.get(0)?,
                no: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                status: r.get(4)?,
                prepared_by: r.get(5)?,
                memo: r.get(6)?,
                lines: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(st);
    let mut lst = db
        .conn()
        .prepare("SELECT asset_id, found, memo FROM asset_count_line WHERE ac_id=?1")?;
    for c in out.iter_mut() {
        c.lines = lst
            .query_map([c.id], |r| {
                Ok((r.get(0)?, r.get::<_, i64>(1)? != 0, r.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Db;

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_asset_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }

    fn asset(code: &str, original: &str, months: i32) -> Asset {
        Asset {
            id: 0,
            code: code.into(),
            name: "测试设备".into(),
            category: "电子设备".into(),
            spec: String::new(),
            dept: String::new(),
            asset_account: "1601".into(),
            dep_account: "1602".into(),
            expense_account: "6602".into(),
            original_value: Money::parse(original).unwrap(),
            residual_rate: Money::parse("0.05").unwrap(),
            life_months: months,
            method: DepMethod::Straight,
            start_period: Period::new(2026, 1).unwrap(),
            disposed_period: None,
            dispose_amount: None,
            status: AssetStatus::InUse,
            voucher_id: None,
            memo: String::new(),
        }
    }

    #[test]
    fn crud_and_dep() {
        let db = tmpdb("crud");
        let id = insert(&db, &asset("GD0001", "12000", 12)).unwrap();
        let a = get(&db, id).unwrap().unwrap();
        assert_eq!(a.original_value, Money::parse("12000").unwrap());
        assert_eq!(a.method, DepMethod::Straight);

        let p = Period::new(2026, 1).unwrap();
        let amt = planned_dep(&a, p).unwrap().unwrap();
        // (12000 - 600) / 12 = 950
        assert_eq!(amt, Money::parse("950").unwrap());

        dep_upsert(
            &db,
            &DepRecord {
                id: 0,
                asset_id: id,
                period: p,
                amount: amt,
                accum: amt,
                net_value: a.original_value - amt,
                voucher_id: None,
            },
        )
        .unwrap();
        // 幂等
        dep_upsert(
            &db,
            &DepRecord {
                id: 0,
                asset_id: id,
                period: p,
                amount: amt,
                accum: amt,
                net_value: a.original_value - amt,
                voucher_id: None,
            },
        )
        .unwrap();
        assert_eq!(dep_list(&db, id).unwrap().len(), 1);

        // 已提折旧不能删卡片
        assert!(delete(&db, id).is_err());
    }

    #[test]
    fn elapsed_and_should() {
        let db = tmpdb("elapsed");
        let id = insert(&db, &asset("GD0002", "6000", 6)).unwrap();
        let mut a = get(&db, id).unwrap().unwrap();
        let p1 = Period::new(2026, 1).unwrap();
        let p7 = Period::new(2026, 7).unwrap();
        assert_eq!(a.elapsed_months(p1), 1);
        assert_eq!(a.elapsed_months(p7), 7);
        assert!(!a.should_depreciate(p7)); // 超过 6 个月不再提
        assert!(a.should_depreciate(Period::new(2026, 6).unwrap()));

        a.status = AssetStatus::Idle;
        assert!(!a.should_depreciate(p1));

        // 清理当月仍提，次月停
        a.status = AssetStatus::InUse;
        a.disposed_period = Some(p1);
        assert!(a.should_depreciate(p1));
        assert!(!a.should_depreciate(Period::new(2026, 2).unwrap()));
    }

    #[test]
    fn next_code_seq() {
        let db = tmpdb("code");
        assert_eq!(next_code(&db).unwrap(), "GD0001");
        insert(&db, &asset("GD0001", "1000", 12)).unwrap();
        assert_eq!(next_code(&db).unwrap(), "GD0002");
    }

    #[test]
    fn impairment_accessory_count() {
        let db = tmpdb("imp");
        let id = insert(&db, &asset("GD0001", "10000", 60)).unwrap();
        // 减值 2000
        impair(&db, id, Period::new(2026, 2).unwrap(), Money::parse("2000").unwrap(), "减值测试").unwrap();
        assert_eq!(impairment_sum(&db, id).unwrap(), Money::parse("2000").unwrap());
        // 类别
        assert_eq!(categories(&db).unwrap(), vec!["电子设备".to_string()]);
        // 附属设备
        accessory_add(&db, id, "显卡", "RTX", 2, "").unwrap();
        assert_eq!(accessory_list(&db, id).unwrap().len(), 1);
        // 盘点：盘亏 → 过账后资产变 Idle
        let p = Period::new(2026, 2).unwrap();
        let mut c = AssetCount {
            id: 0, no: ac_next_no(&db, p).unwrap(), period: p,
            date: NaiveDate::from_ymd_opt(2026, 2, 28).unwrap(),
            status: "draft".into(), prepared_by: "张三".into(), memo: String::new(),
            lines: vec![(id, false, "盘亏".to_string())],
        };
        let cid = ac_save(&db, &mut c).unwrap();
        assert_eq!(ac_post(&db, cid).unwrap(), 1);
        assert_eq!(get(&db, id).unwrap().unwrap().status, AssetStatus::Idle);
    }
}
