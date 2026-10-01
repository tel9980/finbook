//! 存货批次与库位（对标金蝶批号/保质期/货位管理）
//!
//! - **批次主数据**：批号（空则按 `BT+年月日+序号` 自动生成）、生产日期、失效日期
//!   （= 生产日期 + 存货档案 `shelf_life_days` 保质期天数，未配置则不带效期）、仓库、初始库位；
//! - **批次余额**：派生自 `stock_move` 按 (item, batch_no) 汇总——批次出入登记直接写库存流水
//!   （其他入库/其他出库 + batch_no），**与普通库存同一本账**，不产生双轨数量；
//! - **FEFO 推荐**：按失效日期近者先出（无失效期的批次排最后），其次按生产日期；
//! - **临期预警**：失效日期 ≤ 今天 + N 天且余额 > 0；
//! - **库位主数据**：存储 / 拣货 / 隔离 三类（批次记录初始库位，调拨级库位联动后续迭代）。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 批次主数据
#[derive(Clone, Debug, serde::Serialize)]
pub struct StockBatch {
    pub id: i64,
    pub item: String,
    pub batch_no: String,
    pub production_date: String,
    pub expiry_date: String,
    pub warehouse: String,
    pub location: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
    /// 批次余额（= 库存流水汇总；list 时填充）
    #[serde(default)]
    pub balance: Money,
}

/// 库位
#[derive(Clone, Debug, serde::Serialize)]
pub struct StockLocation {
    pub id: i64,
    pub code: String,
    pub name: String,
    /// storage / pick / quarantine
    pub kind: String,
    pub memo: String,
}

const B_COLS: &str =
    "id,item,batch_no,production_date,expiry_date,warehouse,location,memo,created_by,created_at";

fn map_batch(r: &rusqlite::Row) -> rusqlite::Result<StockBatch> {
    Ok(StockBatch {
        id: r.get(0)?,
        item: r.get(1)?,
        batch_no: r.get(2)?,
        production_date: r.get(3)?,
        expiry_date: r.get(4)?,
        warehouse: r.get(5)?,
        location: r.get(6)?,
        memo: r.get(7)?,
        created_by: r.get(8)?,
        created_at: r.get(9)?,
        balance: Money::ZERO,
    })
}

/// 存货档案保质期天数（aux props.shelf_life_days；未配置/非法 = 0）
pub fn shelf_life_days(db: &Db, item: &str) -> i64 {
    let props: Option<String> = db
        .conn()
        .query_row(
            "SELECT props_json FROM aux_entity WHERE kind='item' AND code=?1",
            [item],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);
    let Some(props) = props else { return 0 };
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&props).unwrap_or_default();
    map.get("shelf_life_days")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(0)
}

/// 自动批号：BT + yymmdd + 3 位当日序号
fn next_batch_no(db: &Db, date: NaiveDate) -> DbResult<String> {
    let prefix = format!("BT{}", date.format("%y%m%d"));
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM stock_batch WHERE batch_no LIKE ?1",
        [format!("{prefix}%")],
        |r| r.get(0),
    )?;
    Ok(format!("{prefix}{:03}", n + 1))
}

/// 批次余额（库存流水按 item+batch 汇总）
pub fn batch_balance(db: &Db, item: &str, batch_no: &str) -> DbResult<Money> {
    let mut st = db.conn().prepare(
        "SELECT qty FROM stock_move WHERE item=?1 AND batch_no=?2",
    )?;
    let rows = st
        .query_map(rusqlite::params![item, batch_no], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.iter().map(|s| Money::parse_or_zero(s)).sum())
}

/// 登记批次出入：批次不存在则建档（批号空 = 自动 BT+日期+序号）。
/// 生产日期提供时按存货档案保质期推算失效日期。direction: "in" 入库 / "out" 出库。
/// 返回 (批次 id, 实际批号, 当前余额)。
pub fn batch_register(
    db: &Db,
    item: &str,
    batch_no: &str,
    production_date: &str,
    warehouse: &str,
    location: &str,
    qty: Money,
    direction: &str,
    memo: &str,
    who: &str,
) -> DbResult<(i64, String, Money)> {
    if item.trim().is_empty() {
        return Err(fincore::FinError::validate("存货编码必填").into());
    }
    if !qty.is_positive() {
        return Err(fincore::FinError::validate("数量必须大于 0").into());
    }
    if direction != "in" && direction != "out" {
        return Err(fincore::FinError::validate("方向只能是 in/out").into());
    }
    let today = chrono::Local::now().date_naive();
    let no = if batch_no.trim().is_empty() {
        next_batch_no(db, today)?
    } else {
        batch_no.trim().to_string()
    };
    // 生效日期：生产日期（今天缺省）；失效 = 生产 + 保质期
    let pdate = NaiveDate::parse_from_str(production_date.trim(), "%Y-%m-%d")
        .unwrap_or(today);
    let days = shelf_life_days(db, item);
    let expiry = if days > 0 {
        (pdate + chrono::Duration::days(days)).format("%Y-%m-%d").to_string()
    } else {
        String::new()
    };
    let period = Period::from_date(today);
    let signed = if direction == "in" { qty } else { qty.negated() };
    let kind = if direction == "in" {
        crate::business::StockKind::OtherIn
    } else {
        crate::business::StockKind::OtherOut
    };
    let tx = db.write_tx()?;
    // 建档（已存在则只更新库位/仓库为空的字段？v1：存在即跳过建档）
    tx.execute(
        "INSERT INTO stock_batch(item,batch_no,production_date,expiry_date,warehouse,location,memo,created_by,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(item, batch_no) DO NOTHING",
        rusqlite::params![
            item,
            no,
            pdate.format("%Y-%m-%d").to_string(),
            expiry,
            warehouse,
            location,
            memo,
            who,
            now()
        ],
    )?;
    // 库存流水带批号（与普通库存同一本账：余额 = 按 (item,batch) 汇总）
    crate::business::stock_insert_of(
        &tx,
        &crate::business::StockMove {
            id: 0,
            period,
            biz_date: today,
            kind,
            item: item.to_string(),
            warehouse: warehouse.to_string(),
            batch_no: no.clone(),
            qty: signed,
            price: Money::ZERO,
            amount: Money::ZERO,
            voucher_id: None,
            memo: if memo.is_empty() {
                format!("批次{} {}", if direction == "in" { "入库" } else { "出库" }, no)
            } else {
                memo.to_string()
            },
        },
    )?;
    tx.commit()?;
    let id: i64 = db.conn().query_row(
        "SELECT id FROM stock_batch WHERE item=?1 AND batch_no=?2",
        rusqlite::params![item, no],
        |r| r.get(0),
    )?;
    let bal = batch_balance(db, item, &no)?;
    Ok((id, no, bal))
}

/// 批次列表（含余额；item 可选过滤）
pub fn batch_list(db: &Db, item: &str) -> DbResult<Vec<StockBatch>> {
    let mut st = if item.trim().is_empty() {
        db.conn()
            .prepare(&format!("SELECT {B_COLS} FROM stock_batch ORDER BY item, batch_no"))?
    } else {
        db.conn().prepare(&format!(
            "SELECT {B_COLS} FROM stock_batch WHERE item=?1 ORDER BY batch_no"
        ))?
    };
    let mut rows: Vec<StockBatch> = if item.trim().is_empty() {
        st.query_map([], map_batch)?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        st.query_map([item.trim()], map_batch)?
            .collect::<Result<Vec<_>, _>>()?
    };
    for b in &mut rows {
        b.balance = batch_balance(db, &b.item, &b.batch_no)?;
    }
    Ok(rows)
}

/// FEFO 推荐：失效日期近者先出（无效期排最后），同效期按生产日期早者先；
/// 返回被推荐的 (批号, 失效日期, 建议分配数量)，数量不足部分余额留在返回值之后。
pub fn fefo_recommend(
    db: &Db,
    item: &str,
    qty: Money,
) -> DbResult<Vec<(String, String, Money)>> {
    let mut batches = batch_list(db, item)?;
    batches.retain(|b| b.balance.is_positive());
    // FEFO：有失效期的在前；组内失效期/生产日期早者在前；无失效期按生产日期早者在前
    batches.sort_by(|a, b| {
        match (a.expiry_date.is_empty(), b.expiry_date.is_empty()) {
            (false, true) => std::cmp::Ordering::Less,
            (true, false) => std::cmp::Ordering::Greater,
            (false, false) => a
                .expiry_date
                .cmp(&b.expiry_date)
                .then_with(|| a.production_date.cmp(&b.production_date)),
            (true, true) => a.production_date.cmp(&b.production_date),
        }
    });
    let mut rest = qty;
    let mut out = Vec::new();
    for b in batches {
        if !rest.is_positive() {
            break;
        }
        let take = rest.min(b.balance);
        out.push((b.batch_no, b.expiry_date, take));
        rest -= take;
    }
    Ok(out)
}

/// 临期批次：余额 > 0 且 0 < 失效日期 ≤ 今天 + days（expiry_days ≤0 用30）
pub fn expiring_batches(db: &Db, days: i64) -> DbResult<Vec<StockBatch>> {
    let days = if days > 0 { days } else { 30 };
    let today = chrono::Local::now().date_naive();
    let limit = (today + chrono::Duration::days(days)).format("%Y-%m-%d").to_string();
    let mut st = db.conn().prepare(&format!(
        "SELECT {B_COLS} FROM stock_batch WHERE expiry_date <> '' AND expiry_date <= ?1
         ORDER BY expiry_date, item"
    ))?;
    let mut rows = st
        .query_map([limit], map_batch)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.retain(|b| {
        batch_balance(db, &b.item, &b.batch_no)
            .map(|x| x.is_positive())
            .unwrap_or(false)
    });
    for b in &mut rows {
        b.balance = batch_balance(db, &b.item, &b.batch_no)?;
    }
    Ok(rows)
}

// ---------------- 库位主数据 ----------------

pub fn location_list(db: &Db) -> DbResult<Vec<StockLocation>> {
    let mut st = db
        .conn()
        .prepare("SELECT id,code,name,kind,memo FROM stock_location ORDER BY code")?;
    let rows = st
        .query_map([], |r| {
            Ok(StockLocation {
                id: r.get(0)?,
                code: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                memo: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 新增/更新库位（按 code upsert）
pub fn location_save(
    db: &Db,
    code: &str,
    name: &str,
    kind: &str,
    memo: &str,
) -> DbResult<i64> {
    let code = code.trim();
    if code.is_empty() {
        return Err(fincore::FinError::validate("库位编码必填").into());
    }
    if !matches!(kind, "storage" | "pick" | "quarantine") {
        return Err(
            fincore::FinError::validate("库位类型只能是 storage/pick/quarantine").into(),
        );
    }
    db.conn().execute(
        "INSERT INTO stock_location(code,name,kind,memo) VALUES(?1,?2,?3,?4)
         ON CONFLICT(code) DO UPDATE SET name=excluded.name, kind=excluded.kind, memo=excluded.memo",
        rusqlite::params![code, name.trim(), kind, memo],
    )?;
    Ok(db.conn().query_row(
        "SELECT id FROM stock_location WHERE code=?1",
        [code],
        |r| r.get(0),
    )?)
}

pub fn location_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM stock_location WHERE id=?1", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    #[test]
    fn batch_register_fefo_and_locations() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let _ = p;

        // 配置保质期30天（存货档案 props）
        db.conn()
            .execute(
                "UPDATE aux_entity SET props_json='{\"shelf_life_days\":\"30\"}' WHERE kind='item' AND code='RM01'",
                [],
            )
            .unwrap_or(0); // 档案可能不存在 → 忽略（下面用直接建档验证自动效期）

        // 手工建档：RM02 无档案 → shelf_life=0 → 无失效日期
        let (_id, no1, bal) = batch_register(
            &db, "RM02", "", "2026-01-10", "", "", m("50"), "in", "", "u",
        )
        .unwrap();
        assert!(no1.starts_with("BT26"), "自动批号 BT+日期+序号：{no1}");
        assert_eq!(bal, m("50"), "入库后余额50");

        // 再入一个指定批号、带生产日期但无保质期
        let (_id2, no2, _b) = batch_register(
            &db, "RM02", "B-2026-001", "2026-01-05", "WH1", "L01", m("30"), "in", "早批", "u",
        )
        .unwrap();
        assert_eq!(no2, "B-2026-001");
        // 出库20 → 余额10
        let (_id3, _no3, bal3) = batch_register(
            &db, "RM02", "B-2026-001", "", "", "", m("20"), "out", "", "u",
        )
        .unwrap();
        assert_eq!(bal3, m("10"), "出库后余额10");

        // 余额与列表
        let rows = batch_list(&db, "RM02").unwrap();
        assert_eq!(rows.len(), 2);
        let b1 = rows.iter().find(|r| r.batch_no == no1).unwrap();
        assert_eq!(b1.balance, m("50"));

        // 临期：无保质期批次不出现；配置了保质期的才进
        assert!(expiring_batches(&db, 999).unwrap().is_empty(), "无失效日期不进临期");

        // FEFO：无失效日期按生产日期排序（01-05 早于01-10 → B-2026-001 先出）
        let rec = fefo_recommend(&db, "RM02", m("60")).unwrap();
        assert_eq!(rec.len(), 2);
        assert_eq!(rec[0].0, "B-2026-001", "生产日期早者先出：{:?}", rec);
        assert_eq!(rec[0].2, m("10"), "首批吃满余额10");
        assert_eq!(rec[1].0, no1);
        assert_eq!(rec[1].2, m("50"), "剩余需求50 由次批吃满");

        // 有保质期的批次进临期（999天窗口必中）
        // 先为 RM03 建档带保质期：直接更新 props（模拟档案配置）
        db.conn()
            .execute(
                "INSERT OR IGNORE INTO aux_entity(kind,code,name) VALUES('item','RM03','原料3')",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                "UPDATE aux_entity SET props_json='{\"shelf_life_days\":\"10\"}' WHERE kind='item' AND code='RM03'",
                [],
            )
            .unwrap();
        let (_id4, no4, _b4) = batch_register(
            &db, "RM03", "", "2026-01-10", "", "", m("10"), "in", "", "u",
        )
        .unwrap();
        let exp = expiring_batches(&db, 999).unwrap();
        assert!(exp.iter().any(|b| b.batch_no == no4), "配置保质期的批次应进临期列表");
        let expiry = exp.iter().find(|b| b.batch_no == no4).unwrap();
        assert_eq!(expiry.expiry_date, "2026-01-20", "失效 = 生产日期+10天");

        // 库位 CRUD
        let lid = location_save(&db, "L01", "A区货位", "storage", "").unwrap();
        assert!(lid > 0);
        let locs = location_list(&db).unwrap();
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].code, "L01");
        // 重复 code = 更新
        location_save(&db, "L01", "A区货位2", "pick", "").unwrap();
        assert_eq!(location_list(&db).unwrap()[0].name, "A区货位2");
        assert!(location_save(&db, "", "x", "storage", "").is_err(), "空编码拒绝");
        assert!(
            location_save(&db, "L02", "x", "bad", "").is_err(),
            "非法类型拒绝"
        );
        location_delete(&db, lid).unwrap();
        assert!(location_list(&db).unwrap().is_empty());
    }
}
