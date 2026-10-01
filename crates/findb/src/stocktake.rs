//! 存货盘点（对标金蝶库存盘点）：盘点单按仓库**快照账面** + 录入实盘 → 应用时
//! 生成 其他入库 / 其他出库 流水，并按「差异 × 标准价」生成盘盈盘亏凭证
//! （借/贷 1901 待处理财产损溢）。金额口径 = 计价配置的标准价：未配置标准价时
//! 只调库存流水、不出凭证（响应里说明）。v1：草稿可删；**已应用盘点单不可删**
//! （冲回请做反向盘点单）；已应用单的凭证删除后单据仍保持 applied（流水已生效）。

use chrono::NaiveDate;
use fincore::{Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 盘点单
#[derive(Clone, Debug, serde::Serialize)]
pub struct StockCount {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    /// 空 = 全部仓库合计
    pub warehouse: String,
    pub memo: String,
    /// draft=草稿 / applied=已应用（流水与凭证已生成）
    pub status: String,
    pub voucher_id: Option<i64>,
    pub applied_at: String,
    pub created_by: String,
    pub created_at: String,
    /// 明细（list/get 填充）
    #[serde(default)]
    pub lines: Vec<StockCountLine>,
}

/// 盘点明细行
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct StockCountLine {
    pub id: i64,
    pub count_id: i64,
    pub item: String,
    /// 批次号（空 = 整仓口径，v25 批次盘点）
    pub batch_no: String,
    pub book_qty: Money,
    pub count_qty: Money,
    pub memo: String,
}

impl StockCountLine {
    /// 差异 = 实盘 − 账面（正=盘盈，负=盘亏）
    pub fn diff(&self) -> Money {
        self.count_qty - self.book_qty
    }
}

const C_COLS: &str =
    "id,no,period,date,warehouse,memo,status,voucher_id,applied_at,created_by,created_at";
const L_COLS: &str = "id,count_id,item,batch_no,book_qty,count_qty,memo";

fn map_count(r: &rusqlite::Row) -> rusqlite::Result<StockCount> {
    let d: String = r.get(3)?;
    Ok(StockCount {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1977, 1, 1).unwrap()),
        warehouse: r.get(4)?,
        memo: r.get(5)?,
        status: r.get(6)?,
        voucher_id: r.get(7)?,
        applied_at: r.get(8)?,
        created_by: r.get(9)?,
        created_at: r.get(10)?,
        lines: Vec::new(),
    })
}

fn map_line(r: &rusqlite::Row) -> rusqlite::Result<StockCountLine> {
    Ok(StockCountLine {
        id: r.get(0)?,
        count_id: r.get(1)?,
        item: r.get(2)?,
        batch_no: r.get(3)?,
        book_qty: Money::parse_or_zero(&r.get::<_, String>(4)?),
        count_qty: Money::parse_or_zero(&r.get::<_, String>(5)?),
        memo: r.get(6)?,
    })
}

/// 某存货在指定仓库的账面数量（warehouse 空 = 全部仓库合计）
fn book_qty(db: &Db, item: &str, warehouse: &str) -> DbResult<Money> {
    let rows = crate::inventory2::warehouse_stock(db, item)?;
    if warehouse.is_empty() {
        Ok(rows.iter().map(|w| w.qty).sum())
    } else {
        Ok(rows
            .iter()
            .find(|w| w.warehouse == warehouse)
            .map(|w| w.qty)
            .unwrap_or(Money::ZERO))
    }
}

/// 某存货+批次在指定仓库的账面数量（warehouse 空 = 全部仓库合计）。
/// 出负入正的净额即余额——逐行取文本再汇总（与 batch_balance 同模式，避免 SUM 类型亲和问题）。
fn batch_book_qty(db: &Db, item: &str, batch_no: &str, warehouse: &str) -> DbResult<Money> {
    let mut st = db.conn().prepare(
        "SELECT qty FROM stock_move WHERE item=?1 AND batch_no=?2 AND (?3='' OR warehouse=?3)",
    )?;
    let rows = st
        .query_map(rusqlite::params![item, batch_no, warehouse], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| Money::parse_or_zero(s)).sum())
}

/// 盘点单列表（含明细）
pub fn count_list(db: &Db) -> DbResult<Vec<StockCount>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {C_COLS} FROM inv_count ORDER BY date DESC, id DESC"))?;
    let mut items = st
        .query_map([], map_count)?
        .collect::<Result<Vec<_>, _>>()?;
    for c in &mut items {
        c.lines = count_lines(db, c.id)?;
    }
    Ok(items)
}

fn count_lines(db: &Db, count_id: i64) -> DbResult<Vec<StockCountLine>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {L_COLS} FROM inv_count_line WHERE count_id=?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map([count_id], map_line)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 新建盘点单：服务端按仓库快照每个存货的账面数量（快照与写入同一时刻点读取）。
/// 返回 (id, 单号)。
pub fn count_create(
    db: &Db,
    period: Period,
    date: NaiveDate,
    warehouse: &str,
    memo: &str,
    lines: &[(String, String, Money, String)],
    who: &str,
) -> DbResult<(i64, String)> {
    if lines.is_empty() {
        return Err(fincore::FinError::msg("盘点单至少一行存货").into());
    }
    // 先快照账面（事务外读，随后写入同一事务——时点一致）；批次行按流水净额快照
    let snaps: Vec<(String, String, Money, Money, String)> = lines
        .iter()
        .map(|(item, batch_no, count_qty, lmemo)| {
            let book = if batch_no.trim().is_empty() {
                book_qty(db, item, warehouse)?
            } else {
                batch_book_qty(db, item, batch_no.trim(), warehouse)?
            };
            Ok((
                item.clone(),
                batch_no.trim().to_string(),
                book,
                *count_qty,
                lmemo.clone(),
            ))
        })
        .collect::<DbResult<Vec<_>>>()?;
    let no = format!(
        "PD{}{}",
        date.format("%y%m%d"),
        chrono::Local::now().format("%H%M%S")
    );
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO inv_count(no,period,date,warehouse,memo,status,created_by,created_at)
         VALUES(?1,?2,?3,?4,?5,'draft',?6,?7)",
        rusqlite::params![
            no,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            warehouse,
            memo,
            who,
            now()
        ],
    )?;
    let id = tx.last_insert_rowid();
    for (item, batch_no, book, count_qty, lmemo) in &snaps {
        tx.execute(
            "INSERT INTO inv_count_line(count_id,item,batch_no,book_qty,count_qty,memo)
             VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                id,
                item,
                batch_no,
                crate::money_param(*book),
                crate::money_param(*count_qty),
                lmemo
            ],
        )?;
    }
    tx.commit()?;
    Ok((id, no))
}

/// 删除盘点单：仅草稿可删（已应用的流水与凭证已生效，冲回请做反向盘点）
pub fn count_delete(db: &Db, id: i64) -> DbResult<()> {
    let doc = count_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("盘点单"))?;
    if doc.status != "draft" {
        return Err(
            fincore::FinError::state("已应用的盘点单不可删除（如需冲回请做反向盘点单）").into(),
        );
    }
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM inv_count_line WHERE count_id=?1", [id])?;
    tx.execute("DELETE FROM inv_count WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(())
}

pub fn count_get(db: &Db, id: i64) -> DbResult<Option<StockCount>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {C_COLS} FROM inv_count WHERE id=?1"))?;
    let mut doc = st.query_row([id], map_count).optional()?;
    if let Some(c) = doc.as_mut() {
        c.lines = count_lines(db, c.id)?;
    }
    Ok(doc)
}

struct DiffRow {
    item: String,
    /// 批次号（空 = 整仓口径）
    batch_no: String,
    diff: Money,
    /// 差异 × 标准价（未配置标准价 = 0 → 不进凭证价值）
    value: Money,
}

/// 应用盘点单：逐差异行生成 其他入库/其他出库 流水；价值（差异×标准价）≠0 时
/// 同事务生成盘盈盘亏凭证（借/贷 1901，存货腿逐行带 item 辅助与数量/单价）。
/// 返回 (id, 凭证 id?, 价值合计)。并发防重：仅 draft 可应用（条件更新）。
pub fn count_apply(db: &Db, id: i64, who: &str) -> DbResult<(i64, Option<i64>, Money)> {
    let doc = count_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("盘点单"))?;
    if doc.status != "draft" {
        return Err(fincore::FinError::state("盘点单已应用").into());
    }
    if doc.lines.is_empty() {
        return Err(fincore::FinError::msg("盘点单没有明细行").into());
    }
    let rows: Vec<DiffRow> = doc
        .lines
        .iter()
        .filter(|l| !l.diff().is_zero())
        .map(|l| {
            let std = crate::business::item_standard_cost(db, &l.item).unwrap_or(Money::ZERO);
            DiffRow {
                item: l.item.clone(),
                batch_no: l.batch_no.clone(),
                diff: l.diff(),
                value: (l.diff().abs() * std).round2(),
            }
        })
        .collect();
    if rows.is_empty() {
        return Err(fincore::FinError::msg("实盘与账面一致，无需应用").into());
    }
    let gain: Money = rows.iter().filter(|r| r.diff.is_positive()).map(|r| r.value).sum();
    let loss: Money = rows.iter().filter(|r| r.diff.is_negative()).map(|r| r.value).sum();

    // 盘盈盘亏凭证腿（价值≠0 才出；金额口径=标准价，与计价配置一致时无口径差）
    let memo = format!("盘点盈亏 {}", doc.no);
    let mut legs: Vec<Entry> = Vec::new();
    let mut ln: i32 = 1;
    for r in rows.iter().filter(|r| r.diff.is_positive()) {
        legs.push(crate::scm2::estimate_item_entry(
            db, &r.item, r.value, &memo, ln, true,
        ));
        ln += 1;
    }
    if !gain.is_zero() {
        legs.push(Entry {
            credit: gain,
            ..Entry::new(ln, "1901", &memo)
        });
        ln += 1;
    }
    if !loss.is_zero() {
        legs.push(Entry {
            debit: loss,
            ..Entry::new(ln, "1901", &memo)
        });
        ln += 1;
    }
    for r in rows.iter().filter(|r| r.diff.is_negative()) {
        legs.push(crate::scm2::estimate_item_entry(
            db, &r.item, r.value, &memo, ln, false,
        ));
        ln += 1;
    }

    let tx = db.write_tx()?;
    // 库存流水：盘盈=其他入库(qty 正) / 盘亏=其他出库(qty 负，出库行数量为负的全仓约定)
    for r in &rows {
        let kind = if r.diff.is_positive() {
            crate::business::StockKind::OtherIn
        } else {
            crate::business::StockKind::OtherOut
        };
        // 单价=标准价（amount=0 → 计价引擎按 qty×price 入账）
        let std = crate::business::item_standard_cost(db, &r.item).unwrap_or(Money::ZERO);
        crate::business::stock_insert_of(
            &tx,
            &crate::business::StockMove {
                id: 0,
                period: doc.period,
                biz_date: doc.date,
                kind,
                item: r.item.clone(),
                warehouse: doc.warehouse.clone(),
                batch_no: r.batch_no.clone(),
                qty: r.diff,
                price: std,
                amount: Money::ZERO,
                voucher_id: None,
                memo: format!("盘点 {}", doc.no),
            },
        )?;
        // 盘盈行带批次 → 批次不存在则建档（主仓 = 单据仓库；批次全仓唯一，OR IGNORE 防重）
        if !r.batch_no.is_empty() && r.diff.is_positive() {
            tx.execute(
                "INSERT OR IGNORE INTO stock_batch(item,batch_no,warehouse,memo,created_by,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    r.item,
                    r.batch_no,
                    doc.warehouse,
                    format!("盘点盘盈 {}", doc.no),
                    who,
                    now()
                ],
            )?;
        }
    }
    // 盘盈盘亏凭证（总价值>0 才出）
    let total_value = gain + loss;
    let vid = if total_value.is_positive() {
        let no = crate::vouchers::next_no_of(&tx, doc.period, "记")?;
        let mut v = Voucher::new(doc.period, doc.date, "记", no);
        v.prepared_by = who.to_string();
        v.source = VoucherSource::Business;
        v.memo = memo.clone();
        for leg in legs {
            v.push_entry(leg);
        }
        v.renumber();
        Some(crate::vouchers::save_in(&tx, &mut v)?)
    } else {
        None
    };
    let n = tx.execute(
        "UPDATE inv_count SET status='applied', voucher_id=?2, applied_at=?3
         WHERE id=?1 AND status='draft'",
        rusqlite::params![id, vid, now()],
    )?;
    if n == 0 {
        return Err(
            fincore::FinError::state("盘点单状态已变化（可能已被应用）").into(),
        );
    }
    tx.commit()?;
    Ok((id, vid, total_value))
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

    fn seed_stock(db: &Db, item: &str, qty: Money, price: Money) {
        crate::business::stock_insert(
            db,
            &crate::business::StockMove {
                id: 0,
                period: Period::new(2026, 1).unwrap(),
                biz_date: d(2026, 1, 2),
                kind: crate::business::StockKind::Purchase,
                item: item.to_string(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty,
                price,
                amount: (qty * price).round2(),
                voucher_id: None,
                memo: "期初入库".to_string(),
            },
        )
        .unwrap();
    }

    #[test]
    fn count_apply_loss_gain_and_guards() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // P001 期初20，标准价10
        seed_stock(&db, "P001", m("20"), m("10"));
        crate::business::item_cost_method_set(&db, "P001", Some("moving_average"), m("10"))
            .unwrap();

        // 盘亏：实盘15 → 差异-5 × 标准价10 = 50 → 借1901 / 贷存货(140301 回退材料科目)
        let (id, no) = count_create(
            &db,
            p,
            d(2026, 1, 10),
            "",
            "一月盘点",
            &[("P001".to_string(), String::new(), m("15"), String::new())],
            "u",
        )
        .unwrap();
        assert!(!no.is_empty());
        let doc = count_get(&db, id).unwrap().unwrap();
        assert_eq!(doc.lines[0].book_qty, m("20"), "账面应快照为20");
        let (id2, vid, value) = count_apply(&db, id, "u").unwrap();
        assert_eq!(id2, id);
        assert_eq!(value, m("50"), "盘亏价值 = 5×10");
        let vid = vid.expect("价值>0 应出凭证");
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "1901");
        assert_eq!(v.entries[0].debit, m("50"));
        assert_eq!(v.entries[1].account_code, "140301", "存货腿回退材料科目");
        assert_eq!(v.entries[1].credit, m("50"));
        assert_eq!(v.entries[1].aux.item.as_deref(), Some("P001"));
        assert!(v.entries[1].qty.is_some());
        // 库存流水已调：20 − 5 = 15
        let stock: Money = crate::inventory2::warehouse_stock(&db, "P001")
            .unwrap()
            .iter()
            .map(|w| w.qty)
            .sum();
        assert_eq!(stock, m("15"), "应用后账面应为15");
        // 已应用不可删
        assert!(count_delete(&db, id).is_err(), "已应用不可删");
        // 重复应用拒绝
        assert!(count_apply(&db, id, "u").is_err(), "重复应用应拒绝");

        // 盘盈：实盘25（基准15）→ +10×10=100 → 借存货 / 贷1901
        let (id3, _) = count_create(
            &db,
            p,
            d(2026, 1, 12),
            "",
            "复盘",
            &[("P001".to_string(), String::new(), m("25"), String::new())],
            "u",
        )
        .unwrap();
        let (_, vid3, value3) = count_apply(&db, id3, "u").unwrap();
        assert_eq!(value3, m("100"));
        let v = crate::vouchers::get(&db, vid3.unwrap()).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "140301");
        assert_eq!(v.entries[0].debit, m("100"));
        assert_eq!(v.entries[1].account_code, "1901");
        assert_eq!(v.entries[1].credit, m("100"));

        // 无差异 → 拒绝；草稿可删
        let (id4, _) = count_create(
            &db,
            p,
            d(2026, 1, 13),
            "",
            "无差异",
            &[("P001".to_string(), String::new(), m("25"), String::new())],
            "u",
        )
        .unwrap();
        assert!(count_apply(&db, id4, "u").is_err(), "无差异应拒绝");
        count_delete(&db, id4).unwrap();

        // 未配置标准价：只调流水、不出凭证（价值0）
        let (id5, _) = count_create(
            &db,
            p,
            d(2026, 1, 14),
            "",
            "无标准价",
            &[("NO_COST".to_string(), String::new(), m("3"), String::new())],
            "u",
        )
        .unwrap();
        let (_, vid5, value5) = count_apply(&db, id5, "u").unwrap();
        assert!(vid5.is_none(), "未配置标准价不出凭证");
        assert!(value5.is_zero());
    }
}
