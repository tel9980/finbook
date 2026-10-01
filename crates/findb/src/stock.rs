//! 库存盘点与批次管理
//!
//! 对标金蝶/用友的库存模块：盘点单（盘盈盘亏）、批次跟踪。
//! 金额一律 TEXT 存储、Rust 侧 Decimal 累加，不走 SQL 浮点。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn read_m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 库存盘点
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CountLine {
    pub id: i64,
    pub item: String,
    /// 账面数量
    pub book_qty: Money,
    /// 实盘数量
    pub count_qty: Money,
    pub memo: String,
}

impl CountLine {
    /// 盘盈(+) / 盘亏(-)
    pub fn diff(&self) -> Money {
        self.count_qty - self.book_qty
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StockCount {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub warehouse: String,
    pub status: String, // draft / posted
    pub prepared_by: String,
    pub memo: String,
    pub created_at: String,
    pub lines: Vec<CountLine>,
}

fn map_line(r: &rusqlite::Row) -> rusqlite::Result<CountLine> {
    Ok(CountLine {
        id: r.get(0)?,
        item: r.get(2)?,
        book_qty: read_m(&r.get::<_, String>(3)?),
        count_qty: read_m(&r.get::<_, String>(4)?),
        memo: r.get(5)?,
    })
}

const L_COLS: &str = "id,sc_id,item,book_qty,count_qty,memo";

fn map_count(db: &Db, r: &rusqlite::Row) -> rusqlite::Result<StockCount> {
    let id: i64 = r.get(0)?;
    let lines: Vec<CountLine> = {
        let mut st = db.conn().prepare(&format!(
            "SELECT {L_COLS} FROM stock_count_line WHERE sc_id=?1 ORDER BY id"
        ))?;
        let rows = st
            .query_map(rusqlite::params![id], map_line)?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    Ok(StockCount {
        id,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&r.get::<_, String>(3)?, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        warehouse: r.get(4)?,
        status: r.get(5)?,
        prepared_by: r.get(6)?,
        memo: r.get(7)?,
        created_at: r.get(8)?,
        lines,
    })
}

const C_COLS: &str = "id,no,period,date,warehouse,status,prepared_by,memo,created_at";

pub fn sc_next_no(db: &Db, period: Period) -> DbResult<String> {
    let prefix = format!("PD{:04}{:02}", period.year(), period.month());
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM stock_count WHERE no LIKE ?1",
        rusqlite::params![format!("{prefix}%")],
        |r| r.get(0),
    )?;
    Ok(format!("{prefix}-{:03}", n + 1))
}

pub fn sc_save(db: &Db, c: &mut StockCount) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let id = if c.id > 0 {
        tx.execute(
            "UPDATE stock_count SET period=?2, date=?3, warehouse=?4, status=?5,
             prepared_by=?6, memo=?7 WHERE id=?1",
            rusqlite::params![
                c.id,
                c.period.ymm(),
                c.date.format("%Y-%m-%d").to_string(),
                c.warehouse,
                c.status,
                c.prepared_by,
                c.memo
            ],
        )?;
        c.id
    } else {
        tx.execute(
            "INSERT INTO stock_count(no,period,date,warehouse,status,prepared_by,memo,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                c.no,
                c.period.ymm(),
                c.date.format("%Y-%m-%d").to_string(),
                c.warehouse,
                c.status,
                c.prepared_by,
                c.memo,
                now()
            ],
        )?;
        tx.last_insert_rowid()
    };
    // 重建明细
    tx.execute("DELETE FROM stock_count_line WHERE sc_id=?1", rusqlite::params![id])?;
    for l in &c.lines {
        tx.execute(
            "INSERT INTO stock_count_line(sc_id,item,book_qty,count_qty,memo) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![id, l.item, crate::exact_param(l.book_qty), crate::exact_param(l.count_qty), l.memo],
        )?;
    }
    tx.commit()?;
    c.id = id;
    Ok(id)
}

pub fn sc_get(db: &Db, id: i64) -> DbResult<Option<StockCount>> {
    db.conn()
        .query_row(
            &format!("SELECT {C_COLS} FROM stock_count WHERE id=?1"),
            rusqlite::params![id],
            |r| map_count(db, r),
        )
        .optional()
        .map_err(Into::into)
}

pub fn sc_list(db: &Db, period: Period) -> DbResult<Vec<StockCount>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {C_COLS} FROM stock_count WHERE period=?1 ORDER BY id DESC"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], |r| map_count(db, r))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn sc_delete(db: &Db, id: i64) -> DbResult<()> {
    let c = sc_get(db, id)?.ok_or_else(|| fincore::FinError::msg("盘点单不存在"))?;
    if c.status == "posted" {
        return Err(fincore::FinError::msg("已过账的盘点单不能删除").into());
    }
    db.conn()
        .execute("DELETE FROM stock_count WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 盘点单过账：按盘盈盘亏生成其他出入库流水。
/// 盘盈（count>book）→ 其他入库；盘亏（count<book）→ 其他出库。
pub fn sc_post(db: &Db, id: i64) -> DbResult<usize> {
    let c = sc_get(db, id)?.ok_or_else(|| fincore::FinError::msg("盘点单不存在"))?;
    if c.status == "posted" {
        return Err(fincore::FinError::msg("盘点单已过账").into());
    }
    let mut n = 0usize;
    let tx = db.write_tx()?;
    for l in &c.lines {
        let diff = l.diff();
        if diff.is_zero() {
            continue;
        }
        let (kind, qty) = if diff.is_positive() {
            ("other_in", diff)
        } else {
            ("other_out", diff)
        };
        // 盘点调整按当前结存单价入账（价格 0 由计价引擎取结存价）
        tx.execute(
            "INSERT INTO stock_move(period,biz_date,kind,item,warehouse,batch_no,qty,price,amount,voucher_id,memo)
             VALUES(?1,?2,?3,?4,?5,'',?6,'0','0',NULL,?7)",
            rusqlite::params![
                c.period.ymm(),
                c.date.format("%Y-%m-%d").to_string(),
                kind,
                l.item,
                c.warehouse,
                crate::exact_param(qty),
                format!("盘点{}", if diff.is_positive() { "盘盈" } else { "盘亏" })
            ],
        )?;
        n += 1;
    }
    tx.execute(
        "UPDATE stock_count SET status='posted' WHERE id=?1",
        rusqlite::params![id],
    )?;
    tx.commit()?;
    Ok(n)
}

// ===========================================================================
// 批次管理
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BatchBalance {
    pub item: String,
    pub batch_no: String,
    pub qty: Money,
}

/// 按批次汇总结存数量（Rust 侧 Decimal 累加）
pub fn batch_balances(db: &Db, item: &str) -> DbResult<Vec<BatchBalance>> {
    let mut st = db.conn().prepare(
        "SELECT batch_no, qty FROM stock_move WHERE item=?1 AND batch_no <> ''",
    )?;
    let rows = st
        .query_map(rusqlite::params![item], |r| {
            Ok((r.get::<_, String>(0)?, read_m(&r.get::<_, String>(1)?)))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut map: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for (b, q) in rows {
        *map.entry(b).or_insert(Money::ZERO) += q;
    }
    Ok(map
        .into_iter()
        .filter(|(_, q)| !q.is_zero())
        .map(|(batch_no, qty)| BatchBalance {
            item: item.to_string(),
            batch_no,
            qty,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::{stock_insert, stock_state, StockKind, StockMove};
    use fincore::engine::costing::CostMethod;

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_stk_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }
    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn d(y: i32, mo: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, mo, dd).unwrap()
    }

    #[test]
    fn count_flow_and_post() {
        let db = tmpdb("count");
        let p = Period::new(2026, 1).unwrap();
        stock_insert(&db, &StockMove {
            id: 0, period: p, biz_date: d(2026, 1, 5), kind: StockKind::Purchase,
            item: "P001".into(), warehouse: "主仓".into(), batch_no: String::new(),
            qty: m("100"), price: m("10"), amount: m("1000"), voucher_id: None, memo: String::new(),
        }).unwrap();
        let mut c = StockCount {
            id: 0,
            no: sc_next_no(&db, p).unwrap(),
            period: p,
            date: d(2026, 1, 31),
            warehouse: "主仓".into(),
            status: "draft".into(),
            prepared_by: "张三".into(),
            memo: String::new(),
            created_at: String::new(),
            lines: vec![CountLine { id: 0, item: "P001".into(), book_qty: m("100"), count_qty: m("95"), memo: String::new() }],
        };
        let id = sc_save(&db, &mut c).unwrap();
        assert!(id > 0);
        assert_eq!(sc_list(&db, p).unwrap().len(), 1);
        // 盘亏 5 → 过账生成 1 条其他出库
        assert_eq!(sc_post(&db, id).unwrap(), 1);
        assert_eq!(sc_get(&db, id).unwrap().unwrap().status, "posted");
        // 重复过账报错
        assert!(sc_post(&db, id).is_err());
        // 结存 95
        let st = stock_state(&db, "P001", p, CostMethod::MovingAverage).unwrap();
        assert_eq!(st.qty, m("95"));
        // 已过账不能删
        assert!(sc_delete(&db, id).is_err());
    }

    #[test]
    fn batch_balances_sum() {
        let db = tmpdb("batch");
        let p = Period::new(2026, 1).unwrap();
        for (batch, qty) in [("B1", "100"), ("B2", "50"), ("B1", "-30")] {
            stock_insert(&db, &StockMove {
                id: 0, period: p, biz_date: d(2026, 1, 5),
                kind: if qty.starts_with('-') { StockKind::Sale } else { StockKind::Purchase },
                item: "P001".into(), warehouse: "主仓".into(), batch_no: batch.into(),
                qty: m(qty), price: m("10"), amount: m(qty).abs() * m("10"),
                voucher_id: None, memo: String::new(),
            }).unwrap();
        }
        let b = batch_balances(&db, "P001").unwrap();
        let b1 = b.iter().find(|x| x.batch_no == "B1").unwrap();
        assert_eq!(b1.qty, m("70"));
        let b2 = b.iter().find(|x| x.batch_no == "B2").unwrap();
        assert_eq!(b2.qty, m("50"));
    }
}
