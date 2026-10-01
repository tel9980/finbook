//! 业务闭环：存货核算、工资个税、费用报销
//!
//! 这三个模块共同点是**业务单据在前、会计凭证在后**。单据审批流走完才生成凭证，
//! 凭证生成后回写单据的 `voucher_id`，形成双向可追溯——查账时能从凭证穿透到业务，
//! 也能从业务穿透到凭证。

use chrono::NaiveDate;
use fincore::engine::costing::{CostMethod, Move as StockMoveIn, StockState};
use fincore::voucher::{AuxRef, Entry, Voucher, VoucherSource};
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

// ===========================================================================
// 存货核算
// ===========================================================================

/// 出入库类型
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StockKind {
    /// 采购入库
    Purchase,
    /// 销售出库
    Sale,
    /// 其他入库
    OtherIn,
    /// 其他出库
    OtherOut,
    /// 调拨（同存货不同仓库，暂按普通出库+入库记）
    Transfer,
    /// 成本调整（数量不变，只调金额）
    Adjust,
}

impl StockKind {
    pub fn label(self) -> &'static str {
        match self {
            StockKind::Purchase => "采购入库",
            StockKind::Sale => "销售出库",
            StockKind::OtherIn => "其他入库",
            StockKind::OtherOut => "其他出库",
            StockKind::Transfer => "调拨",
            StockKind::Adjust => "成本调整",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            StockKind::Purchase => "purchase",
            StockKind::Sale => "sale",
            StockKind::OtherIn => "other_in",
            StockKind::OtherOut => "other_out",
            StockKind::Transfer => "transfer",
            StockKind::Adjust => "adjust",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "sale" => StockKind::Sale,
            "other_in" => StockKind::OtherIn,
            "other_out" => StockKind::OtherOut,
            "transfer" => StockKind::Transfer,
            "adjust" => StockKind::Adjust,
            _ => StockKind::Purchase,
        }
    }
    /// 该类型默认是入库（数量为正）还是出库
    pub fn is_inbound(self) -> bool {
        matches!(self, StockKind::Purchase | StockKind::OtherIn)
    }
    pub const ALL: &'static [StockKind] = &[
        StockKind::Purchase,
        StockKind::Sale,
        StockKind::OtherIn,
        StockKind::OtherOut,
        StockKind::Transfer,
        StockKind::Adjust,
    ];
}

/// 出入库流水
#[derive(Clone, Debug)]
pub struct StockMove {
    pub id: i64,
    pub period: Period,
    pub biz_date: NaiveDate,
    pub kind: StockKind,
    pub item: String,
    pub warehouse: String,
    /// 批次号（批次管理，空=不分批）
    pub batch_no: String,
    /// 正=入库 负=出库
    pub qty: Money,
    pub price: Money,
    pub amount: Money,
    pub voucher_id: Option<i64>,
    pub memo: String,
}

fn map_move(r: &rusqlite::Row) -> rusqlite::Result<StockMove> {
    let d: String = r.get(2)?;
    Ok(StockMove {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        biz_date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        kind: StockKind::parse(&r.get::<_, String>(3)?),
        item: r.get(4)?,
        warehouse: r.get(5)?,
        batch_no: r.get::<_, String>(6).unwrap_or_default(),
        qty: Money::parse_or_zero(&r.get::<_, String>(7)?),
        price: Money::parse_or_zero(&r.get::<_, String>(8)?),
        amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
        voucher_id: r.get(10)?,
        memo: r.get(11)?,
    })
}

const MV_COLS: &str = "id,period,biz_date,kind,item,warehouse,batch_no,qty,price,amount,voucher_id,memo";

pub fn stock_list(db: &Db, period: Period) -> DbResult<Vec<StockMove>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {MV_COLS} FROM stock_move WHERE period=?1 ORDER BY biz_date, id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], map_move)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn stock_list_item(db: &Db, item: &str, upto: Period) -> DbResult<Vec<StockMove>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {MV_COLS} FROM stock_move WHERE item=?1 AND period<=?2 ORDER BY biz_date, id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![item, upto.ymm()], map_move)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn stock_insert(db: &Db, m: &StockMove) -> DbResult<i64> {
    let id = stock_insert_of(db.conn(), m)?;
    Ok(id)
}

/// 同 `stock_insert`，但只依赖连接：领料/完工要把「扣库存 + 归集成本 + 出凭证」
/// 放进同一个事务，调用方持的是事务句柄。
pub fn stock_insert_of(conn: &rusqlite::Connection, m: &StockMove) -> DbResult<i64> {
    conn.execute(
        "INSERT INTO stock_move(period,biz_date,kind,item,warehouse,batch_no,qty,price,amount,voucher_id,memo)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        rusqlite::params![
            m.period.ymm(),
            m.biz_date.format("%Y-%m-%d").to_string(),
            m.kind.code(),
            m.item,
            m.warehouse,
            m.batch_no,
            crate::exact_param(m.qty),
            crate::exact_param(m.price),
            crate::money_param(m.amount),
            m.voucher_id,
            m.memo
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn stock_update_amount(db: &Db, id: i64, price: Money, amount: Money) -> DbResult<()> {
    db.conn().execute(
        "UPDATE stock_move SET price=?2, amount=?3 WHERE id=?1",
        rusqlite::params![id, crate::exact_param(price), crate::money_param(amount)],
    )?;
    Ok(())
}

pub fn stock_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM stock_move WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

pub fn stock_items(db: &Db) -> DbResult<Vec<String>> {
    let mut st = db
        .conn()
        .prepare("SELECT DISTINCT item FROM stock_move ORDER BY item")?;
    let rows = st
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 某存货截至某期的结存（重放全部流水）
pub fn stock_state(db: &Db, item: &str, upto: Period, method: CostMethod) -> DbResult<StockState> {
    let rows = stock_list_item(db, item, upto)?;
    let mut adjusts: Vec<Money> = Vec::new();
    let moves: Vec<StockMoveIn> = rows
        .iter()
        .filter(|r| r.kind != StockKind::Adjust)
        .map(|r| StockMoveIn {
            qty: r.qty,
            price: if r.price.is_zero() { None } else { Some(r.price) },
        })
        .collect();
    for r in &rows {
        if r.kind == StockKind::Adjust {
            adjusts.push(r.amount);
        }
    }
    let mut st = fincore::engine::costing::run(&moves, method)?.1;
    // 成本调整按总额叠加到结存金额
    if !adjusts.is_empty() {
        let sum: Money = adjusts.iter().fold(Money::ZERO, |a, b| a + *b);
        let new_amount = st.amount + sum;
        st.adjust_amount(new_amount)?;
    }
    Ok(st)
}

/// 成本调整：数量不变，仅调结存金额。`delta` 正=调增、负=调减。
/// 以 kind=adjust 的流水落库（qty=0），保证可追溯。
pub fn stock_adjust(
    db: &Db,
    period: Period,
    date: NaiveDate,
    item: &str,
    warehouse: &str,
    delta: Money,
    memo: &str,
) -> DbResult<i64> {
    if delta.is_zero() {
        return Err(fincore::FinError::msg("成本调整金额不能为 0").into());
    }
    stock_insert(
        db,
        &StockMove {
            id: 0,
            period,
            biz_date: date,
            kind: StockKind::Adjust,
            item: item.to_string(),
            warehouse: warehouse.to_string(),
            batch_no: String::new(),
            qty: Money::ZERO,
            price: Money::ZERO,
            amount: delta,
            voucher_id: None,
            memo: memo.to_string(),
        },
    )
}

/// 期间存货收发存汇总
#[derive(Clone, Debug, Default)]
pub struct StockSummary {
    pub item: String,
    pub in_qty: Money,
    pub in_amount: Money,
    pub out_qty: Money,
    /// 出库成本（按计价方式算）
    pub out_amount: Money,
    pub end_qty: Money,
    pub end_amount: Money,
    pub unit_cost: Money,
}

/// 计算本期各存货的收发存，并把出库成本回写到流水
/// 存货收发存汇总——**结转口径：仅统计销售出库（kind=sale，含退货正行抵减）**。
/// 领料/形态转换/组装/调拨等非销售出库不属主营业务成本，已在行级过滤排除
///（否则制造业账下领料会被虚增结转进 6401）。
pub fn stock_summary(db: &Db, period: Period, method: CostMethod) -> DbResult<Vec<StockSummary>> {
    let mut items = stock_items(db)?;
    items.sort();
    let mut out = Vec::new();
    for item in items {
        let rows = stock_list_item(db, &item, period)?;
        // 全月一次平均：期末统一计价（期初 + 本期入库 → 一个单价）
        if method == CostMethod::MonthAverage {
            let (opening, this_period): (Vec<_>, Vec<_>) = rows
                .iter()
                .filter(|r| r.kind != StockKind::Adjust)
                .partition(|r| r.period.ymm() < period.ymm());
            let opening_moves: Vec<StockMoveIn> = opening
                .iter()
                .map(|r| StockMoveIn {
                    qty: r.qty,
                    price: if r.price.is_zero() { None } else { Some(r.price) },
                })
                .collect();
            // 期初结存 = 重放期前全部流水
            let (_, opening_state) =
                fincore::engine::costing::run(&opening_moves, CostMethod::MovingAverage)?;
            let period_moves: Vec<StockMoveIn> = this_period
                .iter()
                .map(|r| StockMoveIn {
                    qty: r.qty,
                    price: if r.price.is_zero() { None } else { Some(r.price) },
                })
                .collect();
            let (_, end_st) =
                fincore::engine::costing::run_month_average(&opening_state, &period_moves)?;
            let in_qty: Money = this_period.iter().filter(|r| r.qty.is_positive()).map(|r| r.qty).sum();
            let in_amount: Money = this_period
                .iter()
                .filter(|r| r.qty.is_positive())
                .map(|r| if r.amount.is_zero() { (r.qty * r.price).round2() } else { r.amount })
                .sum();
            let out_qty: Money = this_period
                .iter()
                .filter(|r| r.qty.is_negative() && r.kind == StockKind::Sale)
                .map(|r| r.qty.abs())
                .sum();
            // 期初结存金额（含历史调整）
            let opening_adj: Money = rows
                .iter()
                .filter(|r| r.kind == StockKind::Adjust && r.period.ymm() < period.ymm())
                .map(|r| r.amount)
                .sum();
            let end_amount = end_st.amount + opening_adj;
            // 全月一次单价 = (期初金额 + 本期入库) / (期初数量 + 本期入库)，不提前舍入
            let unit2 = {
                let denom = opening_state.qty + in_qty;
                if denom.is_zero() {
                    Money::ZERO
                } else {
                    (opening_state.amount + in_amount)
                        .checked_div(denom)
                        .expect("denom 已判非零")
                        .round_dp(6)
                }
            };
            // 出库成本按统一单价（口径与 run_month_average 一致）
            let out_amount = (out_qty * unit2).round2();
            out.push(StockSummary {
                item: item.clone(),
                in_qty,
                in_amount,
                out_qty,
                out_amount,
                end_qty: end_st.qty,
                end_amount,
                unit_cost: unit2.round2(),
            });
            continue;
        }

        let mut st = StockState::new();
        let mut s = StockSummary {
            item: item.clone(),
            ..Default::default()
        };
        for r in &rows {
            let mv = StockMoveIn {
                qty: r.qty,
                price: if r.price.is_zero() { None } else { Some(r.price) },
            };
            // 引擎跑全部行（含领料/转换/调拨），保证成本序列与单价正确
            let cost = st.apply(&mv, method)?;
            if r.qty.is_positive() {
                s.in_qty += r.qty;
                s.in_amount += if r.amount.is_zero() {
                    (r.qty * r.price).round2()
                } else {
                    r.amount
                };
            } else if r.kind == StockKind::Sale {
                // 结转口径：仅销售出库计入发出量/成本并回写（非销售出库不进 6401）
                s.out_qty += r.qty.abs();
                let c = cost.unwrap_or(Money::ZERO);
                s.out_amount += c;
                // 回写本期的出库成本（失败必须上抛：静默吞错会留下"汇总已改、流水未改"的错账）
                if r.period.ymm() == period.ymm() {
                    stock_update_amount(
                        db,
                        r.id,
                        if s.out_qty.is_zero() {
                            Money::ZERO
                        } else {
                            // 数量为 0 时单位成本按 0（原"除零返 0"口径显式化，守卫与旧行为一致）
                            c.checked_div(r.qty.abs())
                                .unwrap_or(Money::ZERO)
                                .round2()
                        },
                        c,
                    )?;
                }
            }
        }
        s.end_qty = st.qty;
        s.end_amount = st.amount;
        s.unit_cost = st.unit_cost();
        out.push(s);
    }
    Ok(out)
}

/// 生成销售成本结转凭证：借 主营业务成本 / 贷 库存商品
pub fn stock_cost_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    method: CostMethod,
    cost_account: &str,
    asset_account: &str,
    who: &str,
) -> DbResult<Option<i64>> {
    // 先开写事务再算汇总：`stock_summary` 会把出库成本回写到 stock_move，
    // 事务外回写一旦后续保存凭证失败（如已结账被拒），成本已被悄悄改掉。
    // 同一连接上直接执行也在事务内，`stock_summary`/`save_in` 无需改成 tx 版本。
    let tx = db.write_tx()?;
    let sum = stock_summary(db, period, method)?;
    let total: Money = sum.iter().map(|s| s.out_amount).sum();
    if total.is_zero() {
        return Ok(None);
    }
    // 回写目标先在事务内读好；取号 + 存凭证 + 回写流水原子提交，
    // 避免"凭证已存、回写一半"的半成品（save 自带事务，跨事务拆分必留窗口）。
    let out_ids: Vec<i64> = stock_list(db, period)?
        .iter()
        .filter(|m| m.qty.is_negative())
        .map(|m| m.id)
        .collect();
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    // 防重：同期间已有未作废的「结转销售成本」凭证 → 拒绝重复结转
    let dup: i64 = tx.query_row(
        "SELECT COUNT(*) FROM voucher WHERE period=?1 AND memo='结转销售成本' AND status<>'void'",
        [period.ymm()],
        |r| r.get(0),
    )?;
    if dup > 0 {
        return Err(
            fincore::FinError::state("本期已生成销售成本结转凭证，请勿重复结转（如需重做请先作废原凭证）")
                .into(),
        );
    }
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = "结转销售成本".to_string();
    v.push_entry(Entry {
        debit: total,
        ..Entry::new(1, cost_account, "结转销售成本")
    });
    for (idx, s) in sum.iter().filter(|s| !s.out_amount.is_zero()).enumerate() {
        // 显式带发出单价（amount/qty）：存货若曾有 0 价入账，缺 price 会触发
        // 「首次入库必须指定单价」校验；单价与数量金额三者恒自洽。
        let unit = if s.out_qty.is_zero() {
            Money::ZERO
        } else {
            s.out_amount
                .checked_div(s.out_qty)
                .unwrap_or(Money::ZERO)
                .round2()
        };
        v.push_entry(Entry {
            credit: s.out_amount,
            aux: AuxRef {
                item: Some(s.item.clone()),
                ..Default::default()
            },
            qty: Some(s.out_qty),
            price: Some(unit),
            ..Entry::new(idx as i32 + 2, asset_account, "结转销售成本")
        });
    }
    v.renumber();
    let id = crate::vouchers::save_in(&tx, &mut v)?;
    // 回写流水（同事务）
    for mid in out_ids {
        tx.execute(
            "UPDATE stock_move SET voucher_id=?2 WHERE id=?1",
            rusqlite::params![mid, id],
        )?;
    }
    tx.commit()?;
    Ok(Some(id))
}

// ===========================================================================
// 存货计价配置 + 期末结价
// ===========================================================================

/// 读取某存货的计价方式（未配置时默认移动加权平均）
pub fn item_cost_method(db: &Db, item: &str) -> DbResult<CostMethod> {
    let m: Option<String> = db
        .conn()
        .query_row(
            "SELECT method FROM item_cost_method WHERE item=?1",
            rusqlite::params![item],
            |r| r.get(0),
        )
        .optional()?;
    Ok(CostMethod::parse(m.as_deref().unwrap_or("moving_average")))
}

/// 存货是否启用来料检验（aux props.qc_required = "1"/true）——到货入库标记待检
pub fn item_qc_required(db: &Db, item: &str) -> bool {
    db.conn()
        .query_row(
            "SELECT props_json FROM aux_entity WHERE kind='item' AND code=?1",
            [item],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|p| serde_json::from_str::<std::collections::BTreeMap<String, String>>(&p).ok())
        .and_then(|m| m.get("qc_required").cloned())
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// 读取某存货的标准成本单价（未配置为 0）
pub fn item_standard_cost(db: &Db, item: &str) -> DbResult<Money> {
    let v: Option<String> = db
        .conn()
        .query_row(
            "SELECT standard_cost FROM item_cost_method WHERE item=?1",
            rusqlite::params![item],
            |r| r.get(0),
        )
        .optional()?;
    Ok(Money::parse_or_zero(v.as_deref().unwrap_or("0")))
}

/// 保存某存货的计价方式（method 为空表示恢复默认）
pub fn item_cost_method_set(
    db: &Db,
    item: &str,
    method: Option<&str>,
    standard_cost: Money,
) -> DbResult<()> {
    let code = method.unwrap_or("moving_average");
    db.conn().execute(
        "INSERT INTO item_cost_method(item,method,standard_cost) VALUES(?1,?2,?3)
         ON CONFLICT(item) DO UPDATE SET method=excluded.method, standard_cost=excluded.standard_cost",
        rusqlite::params![item, code, crate::exact_param(standard_cost)],
    )?;
    Ok(())
}

/// 删除某存货的计价配置（恢复默认）
pub fn item_cost_method_clear(db: &Db, item: &str) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM item_cost_method WHERE item=?1", rusqlite::params![item])?;
    Ok(())
}

/// 列出所有已配置计价方式的存货
#[derive(Clone, Debug, serde::Serialize)]
pub struct CostConfigRow {
    pub item: String,
    pub method: String,
    pub method_label: String,
    pub standard_cost: Money,
}

pub fn cost_configs(db: &Db) -> DbResult<Vec<CostConfigRow>> {
    let mut st = db
        .conn()
        .prepare("SELECT item, method, standard_cost FROM item_cost_method ORDER BY item")?;
    let rows = st
        .query_map([], |r| {
            let m: String = r.get(1)?;
            Ok(CostConfigRow {
                item: r.get(0)?,
                method: m.clone(),
                method_label: CostMethod::parse(&m).label().to_string(),
                standard_cost: Money::parse_or_zero(&r.get::<_, String>(2)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 期末结价：按各存货配置的计价方式计算期末结存成本，
/// 并生成"成本调整"流水（差额入账，保持数量不变）。
///
/// 返回每个存货的调整明细。全月一次平均在这里统一重算出库成本，
/// 再与流水上已记成本比较，把差额作为调整。
#[derive(Clone, Debug, serde::Serialize)]
pub struct PeriodEndCostRow {
    pub item: String,
    pub method: String,
    /// 结存数量
    pub end_qty: Money,
    /// 按计价方式算出的期末结存金额
    pub end_amount: Money,
    /// 单价
    pub unit_cost: Money,
    /// 需要调整的金额（正=调增，负=调减，0=无需调整）
    pub adjust: Money,
}

/// 期末结价。`period` 为本期，方法按各存货配置读取；
/// `apply` 为 true 时把调整写入 stock_move（kind=adjust）。
pub fn period_end_cost(db: &Db, period: Period, apply: bool) -> DbResult<Vec<PeriodEndCostRow>> {
    // 落地的多条调整必须在同一事务内：中途失败不能留下半批调整
    let tx = if apply { Some(db.write_tx()?) } else { None };
    let mut items = stock_items(db)?;
    items.sort();
    let mut out = Vec::new();
    for item in items {
        let method = item_cost_method(db, &item)?;
        // 期初结存（上一期期末）
        let prev = period.prev();
        let prev_state = stock_state(db, &item, prev, method)?;
        // 本期流水（含期末计价）
        let rows = stock_list_item(db, &item, period)?;
        let moves: Vec<StockMoveIn> = rows
            .iter()
            .filter(|r| r.kind != StockKind::Adjust)
            .map(|r| StockMoveIn {
                qty: r.qty,
                price: if r.price.is_zero() { None } else { Some(r.price) },
            })
            .collect();
        let (_, end_st) = if method == CostMethod::MonthAverage {
            fincore::engine::costing::run_month_average(&prev_state, &moves)?
        } else {
            let mut st = prev_state.clone();
            for mv in &moves {
                st.apply(mv, method)?;
            }
            (Vec::new(), st)
        };
        // 本期已入账的成本调整
        let adj: Money = rows
            .iter()
            .filter(|r| r.kind == StockKind::Adjust)
            .map(|r| r.amount)
            .sum();
        let end_amount = end_st.amount + adj;
        let unit_cost = end_st.unit_cost();
        // 调整差额：把"按计价方式应有的结存"与"流水累计的结存"对齐。
        // 流水口径：期初 + 本期入库 − 本期出库（出库按流水价）
        let flow_end: Money = {
            let mut st = prev_state.clone();
            for mv in &moves {
                st.apply(mv, method)?;
            }
            st.amount + adj
        };
        let adjust = (end_amount - flow_end).round2();
        if apply && !adjust.is_zero() {
            stock_adjust(
                db,
                period,
                period.last_day(),
                &item,
                "",
                adjust,
                "期末结价调整",
            )?;
        }
        out.push(PeriodEndCostRow {
            item,
            method: method.code().to_string(),
            end_qty: end_st.qty,
            end_amount,
            unit_cost,
            adjust,
        });
    }
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(out)
}

// ===========================================================================
// 工资与个税
// ===========================================================================

/// 工资单行
#[derive(Clone, Debug, serde::Serialize)]
pub struct Payroll {
    pub id: i64,
    pub period: Period,
    /// 职员档案 code
    pub employee: String,
    pub dept: String,
    /// 应发合计
    pub gross: Money,
    /// 社保个人部分
    pub social: Money,
    /// 公积金个人部分
    pub housing: Money,
    /// 其他扣款
    pub deduction: Money,
    /// 专项附加扣除（子女教育、房贷等）
    pub additional: Money,
    /// 计税基数
    pub tax_base: Money,
    /// 个人所得税
    pub tax: Money,
    /// 实发
    pub net: Money,
    /// 社保企业部分
    pub social_co: Money,
    /// 公积金企业部分
    pub housing_co: Money,
    pub voucher_id: Option<i64>,
    /// 发放凭证（工资发放动作回链）
    pub paid_voucher_id: Option<i64>,
    /// 社保公积金缴纳凭证
    pub social_voucher_id: Option<i64>,
    pub memo: String,
}

fn map_pay(r: &rusqlite::Row) -> rusqlite::Result<Payroll> {
    Ok(Payroll {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        employee: r.get(2)?,
        dept: r.get(3)?,
        gross: Money::parse_or_zero(&r.get::<_, String>(4)?),
        social: Money::parse_or_zero(&r.get::<_, String>(5)?),
        housing: Money::parse_or_zero(&r.get::<_, String>(6)?),
        deduction: Money::parse_or_zero(&r.get::<_, String>(7)?),
        additional: Money::parse_or_zero(&r.get::<_, String>(8)?),
        tax_base: Money::parse_or_zero(&r.get::<_, String>(9)?),
        tax: Money::parse_or_zero(&r.get::<_, String>(10)?),
        net: Money::parse_or_zero(&r.get::<_, String>(11)?),
        social_co: Money::parse_or_zero(&r.get::<_, String>(12)?),
        housing_co: Money::parse_or_zero(&r.get::<_, String>(13)?),
        voucher_id: r.get(14)?,
        paid_voucher_id: r.get(16)?,
        social_voucher_id: r.get(17)?,
        memo: r.get(15)?,
    })
}

const PAY_COLS: &str = "id,period,employee,dept,gross,social,housing,deduction,additional,
     tax_base,tax,net,social_co,housing_co,voucher_id,memo,paid_voucher_id,social_voucher_id";

pub fn payroll_list(db: &Db, period: Period) -> DbResult<Vec<Payroll>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {PAY_COLS} FROM payroll WHERE period=?1 ORDER BY employee"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], map_pay)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn payroll_get(db: &Db, period: Period, employee: &str) -> DbResult<Option<Payroll>> {
    db.conn()
        .query_row(
            &format!("SELECT {PAY_COLS} FROM payroll WHERE period=?1 AND employee=?2"),
            rusqlite::params![period.ymm(), employee],
            map_pay,
        )
        .optional()
        .map_err(Into::into)
}

/// 新增或覆盖某员工某期工资（UNIQUE(period,employee)）
pub fn payroll_upsert(db: &Db, p: &Payroll) -> DbResult<i64> {
    payroll_upsert_on(db.conn(), p)
}

/// 同 `payroll_upsert`，但只依赖连接，可在调用方的事务内执行（批量生成整体原子）
pub fn payroll_upsert_on(conn: &rusqlite::Connection, p: &Payroll) -> DbResult<i64> {
    conn.execute(
        "INSERT INTO payroll(period,employee,dept,gross,social,housing,deduction,additional,
             tax_base,tax,net,social_co,housing_co,voucher_id,memo)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
         ON CONFLICT(period,employee) DO UPDATE SET
            dept=excluded.dept, gross=excluded.gross, social=excluded.social,
            housing=excluded.housing, deduction=excluded.deduction,
            additional=excluded.additional, tax_base=excluded.tax_base, tax=excluded.tax,
            net=excluded.net, social_co=excluded.social_co, housing_co=excluded.housing_co,
            voucher_id=excluded.voucher_id, memo=excluded.memo",
        rusqlite::params![
            p.period.ymm(),
            p.employee,
            p.dept,
            crate::money_param(p.gross),
            crate::money_param(p.social),
            crate::money_param(p.housing),
            crate::money_param(p.deduction),
            crate::money_param(p.additional),
            crate::money_param(p.tax_base),
            crate::money_param(p.tax),
            crate::money_param(p.net),
            crate::money_param(p.social_co),
            crate::money_param(p.housing_co),
            p.voucher_id,
            p.memo
        ],
    )?;
    // 冲突更新时 last_insert_rowid 不保证，回查一次
    let id: i64 = conn.query_row(
        "SELECT id FROM payroll WHERE period=?1 AND employee=?2",
        rusqlite::params![p.period.ymm(), p.employee],
        |r| r.get(0),
    )?;
    Ok(id)
}

pub fn payroll_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM payroll WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 按主键取一条工资行（web 删除前校验凭证占用用）
pub fn payroll_get_by_id(db: &Db, id: i64) -> DbResult<Option<Payroll>> {
    db.conn()
        .query_row(
            &format!("SELECT {PAY_COLS} FROM payroll WHERE id=?1"),
            rusqlite::params![id],
            map_pay,
        )
        .optional()
        .map_err(Into::into)
}

/// 当年截至 `period` 前一个月，该员工的累计工资数据
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct YtdPayroll {
    pub income: Money,
    pub special: Money,
    pub additional: Money,
    pub withheld: Money,
    pub months: i32,
}

pub fn payroll_ytd(db: &Db, period: Period, employee: &str) -> DbResult<YtdPayroll> {
    let year = period.year();
    let from = Period::new(year, 1).unwrap().ymm();
    let to = period.ymm();
    let mut st = db.conn().prepare(
        "SELECT gross, social, housing, additional, tax FROM payroll
         WHERE period>=?1 AND period<?2 AND employee=?3",
    )?;
    let rows = st
        .query_map(rusqlite::params![from, to, employee], |r| {
            Ok((
                Money::parse_or_zero(&r.get::<_, String>(0)?),
                Money::parse_or_zero(&r.get::<_, String>(1)?),
                Money::parse_or_zero(&r.get::<_, String>(2)?),
                Money::parse_or_zero(&r.get::<_, String>(3)?),
                Money::parse_or_zero(&r.get::<_, String>(4)?),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut y = YtdPayroll::default();
    for (g, s, h, a, t) in rows {
        y.income += g;
        y.special += s + h;
        y.additional += a;
        y.withheld += t;
        y.months += 1;
    }
    Ok(y)
}

/// 计算一条工资：累计预扣预缴法算个税
///
/// `additional` 是专项附加扣除（子女教育 / 房贷 / 赡养老人等），按年累计。
pub fn payroll_calc(
    db: &Db,
    period: Period,
    employee: &str,
    dept: &str,
    gross: Money,
    social: Money,
    housing: Money,
    deduction: Money,
    additional: Money,
    social_co: Money,
    housing_co: Money,
    memo: &str,
) -> DbResult<Payroll> {
    let y = payroll_ytd(db, period, employee)?;
    let months = (y.months + 1).min(12);
    let c = fincore::engine::tax::Cumulative {
        income: y.income + gross,
        special: y.special + social + housing,
        additional: y.additional + additional,
        other: Money::ZERO,
        withheld: y.withheld,
        months,
    };
    let tax = fincore::engine::tax::current_tax(&c)?;
    let tax_base = c.taxable();
    let net = gross - social - housing - deduction - tax;
    Ok(Payroll {
        id: 0,
        period,
        employee: employee.to_string(),
        dept: dept.to_string(),
        gross,
        social,
        housing,
        deduction,
        additional,
        tax_base,
        tax,
        net: net.max(Money::ZERO),
        social_co,
        housing_co,
        voucher_id: None,
        paid_voucher_id: None,
        social_voucher_id: None,
        memo: memo.to_string(),
    })
}

/// 生成本月工资表（批量），返回写入条数
pub fn payroll_generate(
    db: &Db,
    period: Period,
    rows: &[(String, String, Money, Money, Money, Money, Money, Money, Money)],
    // (employee, dept, gross, social, housing, deduction, additional, social_co, housing_co)
) -> DbResult<usize> {
    // 整批同事务：中途失败整体回滚，避免"工资表生成了一半"
    let tx = db.write_tx()?;
    let mut n = 0;
    for (emp, dept, gross, social, housing, ded, add, sco, hco) in rows {
        let p = payroll_calc(
            db, period, emp, dept, *gross, *social, *housing, *ded, *add, *sco, *hco, "",
        )?;
        payroll_upsert_on(&tx, &p)?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// 工资类业务凭证的幂等闸：同一期间 + 同一摘要的非作废凭证只允许存在一张。
/// 摘要由系统按「计提/缴纳/发放 + 期间」固定生成，重复点击/重试会生成重复入账凭证。
pub(crate) fn ensure_unique_biz_voucher(
    tx: &rusqlite::Connection,
    period: Period,
    memo: &str,
) -> DbResult<()> {
    let n: i64 = tx.query_row(
        // 注意：source 经 serde snake_case 落库为小写（business），此前写成 'Business'
        // 导致幂等闸从未命中、重复生成不被拦截——由 payroll 重复生成测试暴露。
        "SELECT COUNT(*) FROM voucher
         WHERE period=?1 AND source='business' AND memo=?2 AND status != 'void'",
        rusqlite::params![period.ymm(), memo],
        |r| r.get(0),
    )?;
    if n > 0 {
        return Err(fincore::FinError::state(format!(
            "「{memo}」的凭证已经生成过，请勿重复生成"
        ))
        .into());
    }
    Ok(())
}

/// 工资计提凭证
///
/// 三条腿各自对应不同明细科目，混进一个"应付职工薪酬"会导致后面缴社保时对不上账：
/// - 应发工资 → 贷 `wage_payable`（221101）
/// - 企业承担社保 → 贷 `social_payable`（221103）
/// - 企业承担公积金 → 贷 `housing_payable`（221104）
///
/// 借方为单一费用科目，按部门拆分「应发 + 企业社保 + 企业公积金」——
/// 部门人工成本全额进费用；费用科目（如 6602）启用的部门辅助核算也因此始终有值。
pub fn payroll_accrue_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    expense_account: &str,
    wage_payable: &str,
    social_payable: &str,
    housing_payable: &str,
    who: &str,
) -> DbResult<Option<i64>> {
    let rows = payroll_list(db, period)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let gross: Money = rows.iter().map(|r| r.gross).sum();
    let sco: Money = rows.iter().map(|r| r.social_co).sum();
    let hco: Money = rows.iter().map(|r| r.housing_co).sum();

    // 取号 + 存凭证 + 回写工资单在同一写事务里原子提交。
    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("计提 {} 工资及社保", period.label());
    let mut i = 1;
    // 借方按部门拆分「应发 + 企业社保 + 企业公积金」，便于后续做部门损益；
    // 同时费用科目（如 6602 管理费用）通常启用部门辅助核算且必填，
    // 三段若各自单独出借方行，社保/公积金两行没有部门会过不了引擎校验。
    let mut by_dept: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for r in &rows {
        *by_dept.entry(r.dept.clone()).or_insert(Money::ZERO) += r.gross + r.social_co + r.housing_co;
    }
    for (dept, amt) in &by_dept {
        v.push_entry(Entry {
            debit: *amt,
            aux: AuxRef {
                dept: if dept.is_empty() { None } else { Some(dept.clone()) },
                ..Default::default()
            },
            ..Entry::new(i, expense_account, "计提工资及社保公积金（企业）")
        });
        i += 1;
    }
    v.push_entry(Entry {
        credit: gross,
        ..Entry::new(i, wage_payable, "计提工资")
    });
    i += 1;
    if !sco.is_zero() {
        v.push_entry(Entry {
            credit: sco,
            ..Entry::new(i, social_payable, "计提社保（企业）")
        });
        i += 1;
    }
    if !hco.is_zero() {
        v.push_entry(Entry {
            credit: hco,
            ..Entry::new(i, housing_payable, "计提公积金（企业）")
        });
    }
    ensure_unique_biz_voucher(&tx, period, &v.memo)?;
    v.renumber();
    let id = crate::vouchers::save_in(&tx, &mut v)?;
    for r in &rows {
        tx.execute(
            "UPDATE payroll SET voucher_id=?2 WHERE id=?1",
            rusqlite::params![r.id, id],
        )?;
    }
    tx.commit()?;
    Ok(Some(id))
}

/// 缴纳社保公积金凭证：借 应付社保（企业）+ 应付公积金（企业）+ 其他应付款（个人）
/// / 贷 银行存款
pub fn payroll_social_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    social_payable: &str,
    housing_payable: &str,
    personal_payable: &str,
    bank_account: &str,
    who: &str,
) -> DbResult<Option<i64>> {
    let rows = payroll_list(db, period)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let sco: Money = rows.iter().map(|r| r.social_co).sum();
    let hco: Money = rows.iter().map(|r| r.housing_co).sum();
    let personal: Money = rows.iter().map(|r| r.social + r.housing).sum();
    let total = sco + hco + personal;
    if total.is_zero() {
        return Ok(None);
    }
    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("缴纳 {} 社保公积金", period.label());
    let mut i = 1;
    if !sco.is_zero() {
        v.push_entry(Entry {
            debit: sco,
            ..Entry::new(i, social_payable, "缴纳社保")
        });
        i += 1;
    }
    if !hco.is_zero() {
        v.push_entry(Entry {
            debit: hco,
            ..Entry::new(i, housing_payable, "缴纳公积金")
        });
        i += 1;
    }
    if !personal.is_zero() {
        v.push_entry(Entry {
            debit: personal,
            ..Entry::new(i, personal_payable, "代缴社保公积金（个人）")
        });
        i += 1;
    }
    v.push_entry(Entry {
        credit: total,
        aux: AuxRef {
            bank: Some("BANK01".into()),
            ..Default::default()
        },
        ..Entry::new(i, bank_account, "缴纳社保公积金")
    });
    ensure_unique_biz_voucher(&tx, period, &v.memo)?;
    v.renumber();
    let id = crate::vouchers::save_in(&tx, &mut v)?;
    // 回链社保缴纳凭证（发放状态可见）
    for r in &rows {
        tx.execute(
            "UPDATE payroll SET social_voucher_id=?2 WHERE id=?1",
            rusqlite::params![r.id, id],
        )?;
    }
    tx.commit()?;
    Ok(Some(id))
}

/// 工资发放凭证：借 应付职工薪酬 / 贷 银行存款 + 应交个人所得税 + 其他应付款（社保个人）
pub fn payroll_pay_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    payable_account: &str,
    bank_account: &str,
    tax_account: &str,
    social_account: &str,
    who: &str,
) -> DbResult<Option<i64>> {
    let rows = payroll_list(db, period)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let net: Money = rows.iter().map(|r| r.net).sum();
    let tax: Money = rows.iter().map(|r| r.tax).sum();
    let social: Money = rows.iter().map(|r| r.social + r.housing + r.deduction).sum();
    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("发放 {} 工资", period.label());
    v.push_entry(Entry {
        debit: net + tax + social,
        ..Entry::new(1, payable_account, "发放工资")
    });
    let mut i = 2;
    if !net.is_zero() {
        v.push_entry(Entry {
            credit: net,
            aux: AuxRef {
                bank: Some("BANK01".into()),
                ..Default::default()
            },
            ..Entry::new(i, bank_account, "实发工资")
        });
        i += 1;
    }
    if !tax.is_zero() {
        v.push_entry(Entry {
            credit: tax,
            ..Entry::new(i, tax_account, "代扣个税")
        });
        i += 1;
    }
    if !social.is_zero() {
        v.push_entry(Entry {
            credit: social,
            ..Entry::new(i, social_account, "代扣社保公积金")
        });
    }
    ensure_unique_biz_voucher(&tx, period, &v.memo)?;
    v.renumber();
    let id = crate::vouchers::save_in(&tx, &mut v)?;
    // 回链发放凭证（发放状态可见）
    for r in &rows {
        tx.execute(
            "UPDATE payroll SET paid_voucher_id=?2 WHERE id=?1",
            rusqlite::params![r.id, id],
        )?;
    }
    tx.commit()?;
    Ok(Some(id))
}

// ===========================================================================
// 费用报销
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    Draft,
    Submitted,
    Approved,
    Rejected,
    Paid,
}

impl ClaimStatus {
    pub fn label(self) -> &'static str {
        match self {
            ClaimStatus::Draft => "草稿",
            ClaimStatus::Submitted => "待审批",
            ClaimStatus::Approved => "已批准",
            ClaimStatus::Rejected => "已驳回",
            ClaimStatus::Paid => "已付款",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            ClaimStatus::Draft => "draft",
            ClaimStatus::Submitted => "submitted",
            ClaimStatus::Approved => "approved",
            ClaimStatus::Rejected => "rejected",
            ClaimStatus::Paid => "paid",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "submitted" => ClaimStatus::Submitted,
            "approved" => ClaimStatus::Approved,
            "rejected" => ClaimStatus::Rejected,
            "paid" => ClaimStatus::Paid,
            _ => ClaimStatus::Draft,
        }
    }
    /// 该状态下还能改单据内容吗
    pub fn editable(self) -> bool {
        matches!(self, ClaimStatus::Draft | ClaimStatus::Rejected)
    }
    pub const ALL: &'static [ClaimStatus] = &[
        ClaimStatus::Draft,
        ClaimStatus::Submitted,
        ClaimStatus::Approved,
        ClaimStatus::Rejected,
        ClaimStatus::Paid,
    ];
}

/// 报销明细行
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ClaimItem {
    pub expense_account: String,
    pub amount: Money,
    pub memo: String,
}

/// 费用报销单
#[derive(Clone, Debug, serde::Serialize)]
pub struct Claim {
    pub id: i64,
    pub period: Period,
    pub no: String,
    pub biz_date: NaiveDate,
    pub applicant: String,
    pub dept: String,
    pub reason: String,
    pub amount: Money,
    pub status: ClaimStatus,
    pub items: Vec<ClaimItem>,
    pub approver: String,
    pub approved_at: Option<String>,
    pub payer: String,
    pub paid_at: Option<String>,
    pub voucher_id: Option<i64>,
    pub created_at: String,
}

fn map_claim(r: &rusqlite::Row) -> rusqlite::Result<Claim> {
    let d: String = r.get(3)?;
    let items_s: String = r.get(9)?;
    Ok(Claim {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        no: r.get(2)?,
        biz_date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        applicant: r.get(4)?,
        dept: r.get(5)?,
        reason: r.get(6)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
        status: ClaimStatus::parse(&r.get::<_, String>(8)?),
        items: serde_json::from_str(&items_s).unwrap_or_default(),
        approver: r.get(10)?,
        approved_at: r.get(11)?,
        payer: r.get(12)?,
        paid_at: r.get(13)?,
        voucher_id: r.get(14)?,
        created_at: r.get(15)?,
    })
}

const CL_COLS: &str = "id,period,no,biz_date,applicant,dept,reason,amount,status,items_json,
     approver,approved_at,payer,paid_at,voucher_id,created_at";

pub fn claim_list(db: &Db, period: Period, status: Option<ClaimStatus>) -> DbResult<Vec<Claim>> {
    let sql = match status {
        Some(_) => format!("SELECT {CL_COLS} FROM expense_claim WHERE period=?1 AND status=?2 ORDER BY no"),
        None => format!("SELECT {CL_COLS} FROM expense_claim WHERE period=?1 ORDER BY no"),
    };
    let mut st = db.conn().prepare(&sql)?;
    let rows = match status {
        Some(s) => st
            .query_map(rusqlite::params![period.ymm(), s.code()], map_claim)?
            .collect::<Result<Vec<_>, _>>()?,
        None => st
            .query_map(rusqlite::params![period.ymm()], map_claim)?
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

pub fn claim_get(db: &Db, id: i64) -> DbResult<Option<Claim>> {
    db.conn()
        .query_row(
            &format!("SELECT {CL_COLS} FROM expense_claim WHERE id=?1"),
            rusqlite::params![id],
            map_claim,
        )
        .optional()
        .map_err(Into::into)
}

/// 下一个单据号：`BX` + 期间 + 3 位序号
pub fn claim_next_no(db: &Db, period: Period) -> DbResult<String> {
    let max: i64 = db
        .conn()
        .query_row(
            "SELECT COALESCE(MAX(CAST(SUBSTR(no,11) AS INTEGER)),0) FROM expense_claim
             WHERE period=?1 AND no GLOB 'BX' || ?2 || '-[0-9]*'",
            rusqlite::params![period.ymm(), period.ymm().to_string()],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(format!("BX{}-{:03}", period.ymm(), max + 1))
}

pub fn claim_insert(db: &Db, c: &Claim) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO expense_claim(period,no,biz_date,applicant,dept,reason,amount,status,
             items_json,approver,approved_at,payer,paid_at,voucher_id,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        rusqlite::params![
            c.period.ymm(),
            c.no,
            c.biz_date.format("%Y-%m-%d").to_string(),
            c.applicant,
            c.dept,
            c.reason,
            crate::money_param(c.amount),
            c.status.code(),
            serde_json::to_string(&c.items)?,
            c.approver,
            c.approved_at,
            c.payer,
            c.paid_at,
            c.voucher_id,
            c.created_at
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn claim_update(db: &Db, c: &Claim) -> DbResult<()> {
    claim_update_cas(db, c, None)?;
    Ok(())
}

/// 按 id 全量更新报销单。`expect_status` 非空时是条件更新（CAS）：库里状态已被
/// 别人改走就一条都不动，返回受影响行数让调用方判定自己是否抢输。
fn claim_update_cas(db: &Db, c: &Claim, expect_status: Option<ClaimStatus>) -> DbResult<usize> {
    let expect = expect_status.map(|s| s.code().to_string());
    let n = db.conn().execute(
        "UPDATE expense_claim SET no=?2,biz_date=?3,applicant=?4,dept=?5,reason=?6,amount=?7,
             status=?8,items_json=?9,approver=?10,approved_at=?11,payer=?12,paid_at=?13,
             voucher_id=?14 WHERE id=?1 AND (?15 IS NULL OR status=?15)",
        rusqlite::params![
            c.id,
            c.no,
            c.biz_date.format("%Y-%m-%d").to_string(),
            c.applicant,
            c.dept,
            c.reason,
            crate::money_param(c.amount),
            c.status.code(),
            serde_json::to_string(&c.items)?,
            c.approver,
            c.approved_at,
            c.payer,
            c.paid_at,
            c.voucher_id,
            expect.as_deref()
        ],
    )?;
    Ok(n)
}

pub fn claim_delete(db: &Db, id: i64) -> DbResult<()> {
    let c = match claim_get(db, id)? {
        Some(c) => c,
        None => return Ok(()),
    };
    if c.voucher_id.is_some() {
        return Err(fincore::FinError::msg("该报销单已生成凭证，请先删除凭证").into());
    }
    db.conn()
        .execute("DELETE FROM expense_claim WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 状态流转，带前置校验
pub fn claim_transition(
    db: &Db,
    id: i64,
    to: ClaimStatus,
    who: &str,
) -> DbResult<()> {
    let mut c = match claim_get(db, id)? {
        Some(c) => c,
        None => return Err(fincore::FinError::not_found("报销单").into()),
    };
    // 记住读到的原状态：末尾按它做条件更新，避免两个人同时审批/付款时互相覆盖
    let from = c.status;
    match to {
        ClaimStatus::Submitted => {
            if c.status != ClaimStatus::Draft && c.status != ClaimStatus::Rejected {
                return Err(fincore::FinError::state("只有草稿或已驳回的单据能提交").into());
            }
            if c.amount <= Money::ZERO {
                return Err(fincore::FinError::validate("报销金额必须大于零").into());
            }
        }
        ClaimStatus::Approved => {
            if c.status != ClaimStatus::Submitted {
                return Err(fincore::FinError::state("只有待审批的单据能批准").into());
            }
            c.approver = who.to_string();
            c.approved_at = Some(now_str());
        }
        ClaimStatus::Rejected => {
            if c.status != ClaimStatus::Submitted {
                return Err(fincore::FinError::state("只有待审批的单据能驳回").into());
            }
            c.approver = who.to_string();
            c.approved_at = Some(now_str());
        }
        ClaimStatus::Paid => {
            if c.status != ClaimStatus::Approved {
                return Err(fincore::FinError::state("只有已批准的单据能付款").into());
            }
            c.payer = who.to_string();
            c.paid_at = Some(now_str());
        }
        ClaimStatus::Draft => {
            if c.status != ClaimStatus::Rejected {
                return Err(fincore::FinError::state("只有已驳回的单据能退回草稿").into());
            }
        }
    }
    c.status = to;
    if claim_update_cas(db, &c, Some(from))? == 0 {
        return Err(fincore::FinError::state(
            "该单据状态刚被他人变更，请刷新后重试",
        )
        .into());
    }
    // 支付即落账：付款凭证草稿随支付生成（默认支付科目=银行 100201；H-3 草稿不入余额）。
    // 生成失败不回滚支付（状态已生效），界面仍可经「生成凭证」幂等重试。
    if to == ClaimStatus::Paid && c.voucher_id.is_none() {
        claim_voucher(db, id, "100201", who)?;
    }
    Ok(())
}

/// 报销单生成凭证：借 各费用科目（按明细）/ 贷 支付科目
pub fn claim_voucher(
    db: &Db,
    id: i64,
    pay_account: &str,
    who: &str,
) -> DbResult<i64> {
    let c = match claim_get(db, id)? {
        Some(c) => c,
        None => return Err(fincore::FinError::not_found("报销单").into()),
    };
    if c.status != ClaimStatus::Paid {
        return Err(fincore::FinError::state("只有已付款的报销单能生成凭证").into());
    }
    if c.items.is_empty() {
        return Err(fincore::FinError::validate("报销明细为空").into());
    }
    let total: Money = c.items.iter().map(|i| i.amount).sum();
    if total != c.amount {
        return Err(fincore::FinError::validate(format!(
            "明细合计 {total} 与单据金额 {} 不一致",
            c.amount
        ))
        .into());
    }
    // 幂等（校验之后）：支付时已自动出过凭证（或此前手动出过）→ 返回同一张，不再新增；
    // 放在校验后保证"金额失配"等拦截在重复请求上仍然生效。
    if let Some(existing) = c.voucher_id {
        return Ok(existing);
    }
    let period = c.period;
    let date = c.biz_date;
    // 取号 → 写凭证 → 把单据标记为已挂凭证，三步同一个事务。分次提交时任何一步
    // 失败都会留下孤儿凭证或「已付款但无凭证」的单据；合并后抢输的一方整个回滚，
    // 连凭证号都不消耗。
    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("报销 {}", c.no);
    let mut i = 1;
    for it in &c.items {
        // 明细备注为空时用事由兜底：凭证引擎要求摘要必填，
        // 报销单的"备注"在界面上是可选项，直接用它会导致生成凭证被拒。
        let summary = if it.memo.trim().is_empty() {
            if c.reason.trim().is_empty() { "报销" } else { c.reason.trim() }
        } else {
            it.memo.trim()
        };
        v.push_entry(Entry {
            debit: it.amount,
            aux: AuxRef {
                dept: if c.dept.is_empty() { None } else { Some(c.dept.clone()) },
                employee: Some(c.applicant.clone()),
                ..Default::default()
            },
            ..Entry::new(i, &it.expense_account, summary)
        });
        i += 1;
    }
    v.push_entry(Entry {
        credit: total,
        aux: if pay_account.starts_with("1002") {
            AuxRef {
                bank: Some("BANK01".into()),
                ..Default::default()
            }
        } else {
            AuxRef::default()
        },
        ..Entry::new(i, pay_account, &c.reason)
    });
    v.renumber();
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    // 条件更新防并发重复出凭证：只有仍没挂凭证的单据能被本次结果占上。
    // 与凭证写入同事务，抢输的一方整体回滚——不再需要先出凭证再删的补丁。
    let taken = tx.execute(
        "UPDATE expense_claim SET voucher_id=?2 WHERE id=?1 AND voucher_id IS NULL",
        rusqlite::params![id, vid],
    )?;
    if taken == 0 {
        return Err(fincore::FinError::state("该报销单已生成过凭证").into());
    }
    tx.commit()?;
    Ok(vid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_biz_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }
    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn d(y: i32, mo: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, mo, dd).unwrap()
    }

    fn mv(period: Period, date: NaiveDate, kind: StockKind, qty: &str, price: &str) -> StockMove {
        StockMove {
            id: 0,
            period,
            biz_date: date,
            kind,
            item: "P001".into(),
            warehouse: "主仓".into(),
            batch_no: String::new(),
            qty: m(qty),
            price: m(price),
            amount: m(qty).abs() * m(price).abs(),
            voucher_id: None,
            memo: String::new(),
        }
    }

    #[test]
    fn stock_moving_average() {
        let db = tmpdb("stock");
        let p = Period::new(2026, 1).unwrap();
        stock_insert(&db, &mv(p, d(2026, 1, 5), StockKind::Purchase, "100", "10")).unwrap();
        stock_insert(&db, &mv(p, d(2026, 1, 10), StockKind::Purchase, "50", "12")).unwrap();
        stock_insert(&db, &mv(p, d(2026, 1, 20), StockKind::Sale, "-80", "0")).unwrap();

        let sum = stock_summary(&db, p, CostMethod::MovingAverage).unwrap();
        assert_eq!(sum.len(), 1);
        assert_eq!(sum[0].in_qty, m("150"));
        assert_eq!(sum[0].out_qty, m("80"));
        assert_eq!(sum[0].out_amount, m("853.33")); // 80 × 10.6667
        assert_eq!(sum[0].end_qty, m("70"));
        assert_eq!(sum[0].end_amount, m("746.67"));

        let st = stock_state(&db, "P001", p, CostMethod::MovingAverage).unwrap();
        assert_eq!(st.qty, m("70"));
    }

    #[test]
    fn stock_cost_voucher_balanced() {
        let db = tmpdb("stockv");
        let p = Period::new(2026, 1).unwrap();
        stock_insert(&db, &mv(p, d(2026, 1, 5), StockKind::Purchase, "10", "5")).unwrap();
        stock_insert(&db, &mv(p, d(2026, 1, 20), StockKind::Sale, "-4", "0")).unwrap();
        let id = stock_cost_voucher(&db, p, d(2026, 1, 31), CostMethod::MovingAverage, "6401", "140501", "u")
            .unwrap()
            .unwrap();
        let v = crate::vouchers::get(&db, id).unwrap().unwrap();
        assert!(v.balanced());
        assert_eq!(v.debit_total(), m("20"));
    }

    #[test]
    fn stock_adjust_changes_amount_not_qty() {
        let db = tmpdb("adj");
        let p = Period::new(2026, 1).unwrap();
        stock_insert(&db, &mv(p, d(2026, 1, 5), StockKind::Purchase, "10", "8")).unwrap();
        // 调增 20 元
        stock_adjust(&db, p, d(2026, 1, 31), "P001", "主仓", m("20"), "涨价").unwrap();
        let st = stock_state(&db, "P001", p, CostMethod::MovingAverage).unwrap();
        assert_eq!(st.qty, m("10"));
        assert_eq!(st.amount, m("100")); // 80 + 20
        assert_eq!(st.unit_cost(), m("10"));
        // 调减 10 元
        stock_adjust(&db, p, d(2026, 1, 31), "P001", "主仓", m("-10"), "降价").unwrap();
        let st = stock_state(&db, "P001", p, CostMethod::MovingAverage).unwrap();
        assert_eq!(st.amount, m("90"));
        // 零调整报错
        assert!(stock_adjust(&db, p, d(2026, 1, 31), "P001", "主仓", m("0"), "").is_err());
    }

    #[test]
    fn payroll_cumulative_tax() {
        let db = tmpdb("pay");
        let rows: Vec<(String, String, Money, Money, Money, Money, Money, Money, Money)> = vec![
            ("E01".into(), "D01".into(), m("15000"), m("2000"), m("0"), m("0"), m("0"), m("0"), m("0")),
        ];
        // 1 月
        payroll_generate(&db, Period::new(2026, 1).unwrap(), &rows).unwrap();
        let p1 = payroll_get(&db, Period::new(2026, 1).unwrap(), "E01").unwrap().unwrap();
        // 累计：15000 - 5000 - 2000 = 8000 → 年税率 3% → 240
        assert_eq!(p1.tax_base, m("8000"));
        assert_eq!(p1.tax, m("240"));
        assert_eq!(p1.net, m("12760"));

        // 2 月同样，累计 30000-10000-4000=16000 → 480，本期补 240
        payroll_generate(&db, Period::new(2026, 2).unwrap(), &rows).unwrap();
        let p2 = payroll_get(&db, Period::new(2026, 2).unwrap(), "E01").unwrap().unwrap();
        assert_eq!(p2.tax_base, m("16000"));
        assert_eq!(p2.tax, m("240"));

        // 5 月跨档：累计 75000-25000-10000=40000 → 年税率 10%-2520 = 1480，前 4 月已交 960 → 520
        for mo in 3..=5 {
            payroll_generate(&db, Period::new(2026, mo).unwrap(), &rows).unwrap();
        }
        let p5 = payroll_get(&db, Period::new(2026, 5).unwrap(), "E01").unwrap().unwrap();
        assert_eq!(p5.tax, m("520"));
    }

    #[test]
    fn payroll_vouchers() {
        let db = tmpdb("payv");
        let p = Period::new(2026, 1).unwrap();
        let rows: Vec<(String, String, Money, Money, Money, Money, Money, Money, Money)> = vec![
            ("E01".into(), "D01".into(), m("10000"), m("1000"), m("500"), m("0"), m("0"), m("2000"), m("500")),
        ];
        payroll_generate(&db, p, &rows).unwrap();
        let a = payroll_accrue_voucher(
            &db, p, d(2026, 1, 31), "660201", "221101", "221103", "221104", "u",
        )
            .unwrap()
            .unwrap();
        let va = crate::vouchers::get(&db, a).unwrap().unwrap();
        // 应发 10000 + 企业社保 2000 + 公积金 500 = 12500
        assert_eq!(va.debit_total(), m("12500"));
        assert!(va.balanced());

        let b = payroll_pay_voucher(&db, p, d(2026, 1, 31), "221101", "100201", "222107", "2241", "u")
            .unwrap()
            .unwrap();
        let vb = crate::vouchers::get(&db, b).unwrap().unwrap();
        assert!(vb.balanced());
        assert_eq!(vb.debit_total(), m("10000")); // 净 + 税 + 社保个人 = 应发全额
    }

    #[test]
    fn claim_flow_and_voucher() {
        let db = tmpdb("claim");
        let p = Period::new(2026, 1).unwrap();
        let no = claim_next_no(&db, p).unwrap();
        assert_eq!(no, "BX202601-001");
        let c = Claim {
            id: 0,
            period: p,
            no: no.clone(),
            biz_date: d(2026, 1, 15),
            applicant: "E01".into(),
            dept: "D01".into(),
            reason: "差旅费".into(),
            amount: m("800"),
            status: ClaimStatus::Draft,
            items: vec![
                ClaimItem {
                    expense_account: "660203".into(),
                    amount: m("500"),
                    memo: "机票".into(),
                },
                ClaimItem {
                    expense_account: "660204".into(),
                    amount: m("300"),
                    memo: "住宿".into(),
                },
            ],
            approver: String::new(),
            approved_at: None,
            payer: String::new(),
            paid_at: None,
            voucher_id: None,
            created_at: now_str(),
        };
        let id = claim_insert(&db, &c).unwrap();

        // 状态机：跳过待审批直接批准要拦
        assert!(claim_transition(&db, id, ClaimStatus::Approved, "boss").is_err());
        claim_transition(&db, id, ClaimStatus::Submitted, "E01").unwrap();
        claim_transition(&db, id, ClaimStatus::Approved, "boss").unwrap();
        claim_transition(&db, id, ClaimStatus::Paid, "cashier").unwrap();
        // 支付即自动落账：付款凭证随支付生成
        assert!(
            claim_get(&db, id).unwrap().unwrap().voucher_id.is_some(),
            "支付应自动生成付款凭证"
        );
        // 明细与金额不符要拦
        let mut c2 = claim_get(&db, id).unwrap().unwrap();
        c2.amount = m("900");
        claim_update(&db, &c2).unwrap();
        assert!(claim_voucher(&db, id, "100201", "u").is_err());
        c2.amount = m("800");
        claim_update(&db, &c2).unwrap();

        let vid = claim_voucher(&db, id, "100201", "u").unwrap();
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert!(v.balanced());
        assert_eq!(v.debit_total(), m("800"));
        // 已生成凭证不能删单据
        assert!(claim_delete(&db, id).is_err());
        // 幂等：重复请求返回同一张
        let vid2 = claim_voucher(&db, id, "100201", "u").unwrap();
        assert_eq!(vid2, vid, "重复请求应返回同一凭证");
    }

    /// 条件更新（CAS）：库里状态与期望值不符时一条都不动。
    /// 单据状态流转靠它拦住「两人同时审批 / 付款」互相覆盖。
    #[test]
    fn claim_update_cas_guards_on_expected_status() {
        let db = tmpdb("claimcas");
        let p = Period::new(2026, 1).unwrap();
        let mut c = Claim {
            id: 0,
            period: p,
            no: claim_next_no(&db, p).unwrap(),
            biz_date: d(2026, 1, 15),
            applicant: "E01".into(),
            dept: "D01".into(),
            reason: "差旅费".into(),
            amount: m("800"),
            status: ClaimStatus::Draft,
            items: vec![ClaimItem {
                expense_account: "660203".into(),
                amount: m("800"),
                memo: "机票".into(),
            }],
            approver: String::new(),
            approved_at: None,
            payer: String::new(),
            paid_at: None,
            voucher_id: None,
            created_at: now_str(),
        };
        let id = claim_insert(&db, &c).unwrap();
        c.id = id;

        // 期望状态与库里一致 → 正常生效
        c.status = ClaimStatus::Submitted;
        assert_eq!(claim_update_cas(&db, &c, Some(ClaimStatus::Draft)).unwrap(), 1);

        // 库里已是 submitted，仍按 draft 去更新 → 抢输，0 行且任何列都不动
        c.reason = "并发覆盖进来的内容".into();
        assert_eq!(claim_update_cas(&db, &c, Some(ClaimStatus::Draft)).unwrap(), 0);
        let after = claim_get(&db, id).unwrap().unwrap();
        assert_eq!(after.status, ClaimStatus::Submitted);
        assert_eq!(after.reason, "差旅费", "CAS 抢输时不能改动任何列");

        // 不传期望状态即无条件更新，保持 claim_update 的既有语义
        assert_eq!(claim_update_cas(&db, &c, None).unwrap(), 1);
        assert_eq!(
            claim_get(&db, id).unwrap().unwrap().reason,
            "并发覆盖进来的内容"
        );
    }

    #[test]
    fn cost_config_and_period_end_pricing() {
        let db = tmpdb("costcfg");
        // 未配置默认移动加权
        assert_eq!(item_cost_method(&db, "P001").unwrap(), CostMethod::MovingAverage);
        // 配置为全月一次平均 + 标准成本
        item_cost_method_set(&db, "P001", Some("month_average"), m("10")).unwrap();
        assert_eq!(item_cost_method(&db, "P001").unwrap(), CostMethod::MonthAverage);
        assert_eq!(item_standard_cost(&db, "P001").unwrap(), m("10"));
        let cfg = cost_configs(&db).unwrap();
        assert_eq!(cfg.len(), 1);
        assert_eq!(cfg[0].method_label, "全月一次加权平均");
        // 恢复默认
        item_cost_method_clear(&db, "P001").unwrap();
        assert_eq!(item_cost_method(&db, "P001").unwrap(), CostMethod::MovingAverage);
    }

    #[test]
    fn month_average_summary_and_pricing() {
        let db = tmpdb("ma");
        let p = Period::new(2026, 1).unwrap();
        // 1 月：入 100@10、入 50@12、出 80
        stock_insert(&db, &mv(p, d(2026, 1, 5), StockKind::Purchase, "100", "10")).unwrap();
        stock_insert(&db, &mv(p, d(2026, 1, 10), StockKind::Purchase, "50", "12")).unwrap();
        stock_insert(&db, &mv(p, d(2026, 1, 20), StockKind::Sale, "-80", "0")).unwrap();

        // 全月一次：单价 = (0 + 1000 + 600) / 150 = 10.6667；出库 80 × 10.6667 = 853.33
        let sum = stock_summary(&db, p, CostMethod::MonthAverage).unwrap();
        assert_eq!(sum.len(), 1);
        assert_eq!(sum[0].out_qty, m("80"));
        assert_eq!(sum[0].out_amount, m("853.33"));
        assert_eq!(sum[0].end_qty, m("70"));
        assert_eq!(sum[0].end_amount, m("746.67"));

        // 期末结价：不 apply 时 adjust 应为 0（流水成本与计价一致）
        let rows = period_end_cost(&db, p, false).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].end_qty, m("70"));
        assert_eq!(rows[0].adjust, m("0"));
    }
}
