//! 计件工资（工序报工完工量 × 计件单价）
//!
//! 计件和月薪是两种不同的结算方式，但工资表是一张：`payroll` 里只有
//! 「应发合计」一个手填数（社保、公积金、专项附加、个税都是自动算的）。
//! 所以这里不去改工资表结构，而是做两件事：
//!
//! 1. **记流水**：每次工序报工（[`crate::advanced::prod_op_report`]）除了
//!    累加 `prod_op` 的累计值，还往 [`crate::piecepay`] 的 `prod_op_log`
//!    写一条流水（谁、哪天、哪道工序、做了多少、多少工时）。
//!    `prod_op` 只有累计值、没有期间，光靠它算不出「本月计件多少」。
//! 2. **算金额**：按期间把流水汇总到人，乘 [`PieceRate`] 里的计件单价，
//!    得到每个人本期的计件工资，供工资表「一键取计件」把应发提上去。
//!
//! 单价匹配（[`rate_match`]）：物料+工序 > 只限物料 > 只限工序 > 通配。
//! 匹配不到的行**照旧列出来**（`matched=false`、金额 0），让人看得见
//! 「这几道工序没有单价」，而不是静默丢掉——丢掉的工时就是少发的工资。

use fincore::{FinError, Money, Period};
use std::collections::HashMap;

use crate::{Db, DbResult};

/// 计件单价
#[derive(Clone, Debug, serde::Serialize)]
pub struct PieceRate {
    pub id: i64,
    /// 物料档案 code（空 = 适用任意物料）
    pub item_code: String,
    /// 工序名（空 = 适用任意工序）
    pub op_name: String,
    pub rate: Money,
    pub disabled: bool,
    pub memo: String,
}

impl Default for PieceRate {
    fn default() -> Self {
        Self {
            id: 0,
            item_code: String::new(),
            op_name: String::new(),
            rate: Money::ZERO,
            disabled: false,
            memo: String::new(),
        }
    }
}

/// 一条报工流水
#[derive(Clone, Debug, serde::Serialize)]
pub struct PieceLog {
    pub id: i64,
    pub op_id: i64,
    pub po_id: i64,
    pub po_no: String,
    pub item_code: String,
    pub item_name: String,
    pub op_name: String,
    pub worker: String,
    /// 期间 ymm（202611）
    pub period: i32,
    pub date: String,
    pub qty: Money,
    pub hours: Money,
}

/// 明细行（一个人一个月里的某道工序）
#[derive(Clone, Debug, serde::Serialize)]
pub struct PieceLine {
    pub po_no: String,
    pub item_code: String,
    pub item_name: String,
    pub op_name: String,
    pub qty: Money,
    pub rate: Money,
    pub amount: Money,
    /// 有没有匹配到单价（没匹配到金额记 0，但要显示出来）
    pub matched: bool,
}

/// 一个人的计件工资
#[derive(Clone, Debug, serde::Serialize)]
pub struct PiecePay {
    pub employee: String,
    pub qty: Money,
    pub amount: Money,
    pub lines: Vec<PieceLine>,
}

const RATE_COLS: &str = "id,item_code,op_name,rate,disabled,memo";

fn map_rate(r: &rusqlite::Row) -> rusqlite::Result<PieceRate> {
    Ok(PieceRate {
        id: r.get(0)?,
        item_code: r.get(1)?,
        op_name: r.get(2)?,
        rate: crate::read_money(r, 3)?,
        disabled: r.get::<_, i64>(4)? != 0,
        memo: r.get(5)?,
    })
}

/// 单价列表
pub fn rate_list(db: &Db) -> DbResult<Vec<PieceRate>> {
    let sql = format!("SELECT {RATE_COLS} FROM piece_rate ORDER BY id");
    let mut st = db.conn().prepare(&sql)?;
    let out = st.query_map([], map_rate)?.collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// 保存单价（id=0 新建）
pub fn rate_save(db: &Db, r: &mut PieceRate) -> DbResult<i64> {
    if r.item_code.trim().is_empty() && r.op_name.trim().is_empty() {
        return Err(FinError::msg("物料与工序至少要填一个，否则等于给所有人所有活定价").into());
    }
    if r.rate.is_negative() {
        return Err(FinError::msg("计件单价不能为负").into());
    }
    let tx = db.write_tx()?;
    let id = if r.id > 0 {
        let n = tx.execute(
            "UPDATE piece_rate SET item_code=?2,op_name=?3,rate=?4,disabled=?5,memo=?6 WHERE id=?1",
            rusqlite::params![
                r.id,
                r.item_code.trim(),
                r.op_name.trim(),
                crate::money_param(r.rate),
                r.disabled as i64,
                r.memo
            ],
        )?;
        if n == 0 {
            return Err(FinError::msg(format!("计件单价 {0} 不存在", r.id)).into());
        }
        r.id
    } else {
        tx.execute(
            "INSERT INTO piece_rate(item_code,op_name,rate,disabled,memo) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![
                r.item_code.trim(),
                r.op_name.trim(),
                crate::money_param(r.rate),
                r.disabled as i64,
                r.memo
            ],
        )?;
        tx.last_insert_rowid()
    };
    tx.commit()?;
    r.id = id;
    Ok(id)
}

/// 删除单价
pub fn rate_delete(db: &Db, id: i64) -> DbResult<()> {
    let n = db
        .conn()
        .execute("DELETE FROM piece_rate WHERE id=?1", [id])?;
    if n == 0 {
        return Err(FinError::msg(format!("计件单价 {id} 不存在")).into());
    }
    Ok(())
}

/// 按 (物料, 工序) 匹配单价：物料+工序 > 只限物料 > 只限工序 > 通配
pub fn rate_match<'a>(rates: &'a [PieceRate], item: &str, op: &str) -> Option<&'a PieceRate> {
    let item = item.trim();
    let op = op.trim();
    let mut best: Option<(u8, &PieceRate)> = None;
    for r in rates {
        if r.disabled {
            continue;
        }
        let hit_item = !r.item_code.is_empty() && r.item_code == item;
        let hit_op = !r.op_name.is_empty() && r.op_name == op;
        let rank = if hit_item && hit_op {
            0u8
        } else if hit_item {
            1
        } else if hit_op {
            2
        } else if r.item_code.is_empty() && r.op_name.is_empty() {
            3
        } else {
            continue;
        };
        let take = match &best {
            None => true,
            Some((r0, _)) => rank < *r0,
        };
        if take {
            best = Some((rank, r));
        }
    }
    best.map(|(_, r)| r)
}

/// 记一条报工流水（正常由 `advanced::prod_op_report` 调用）
pub fn log_add(db: &Db, log: &PieceLog) -> DbResult<i64> {
    if log.qty.is_negative() || log.hours.is_negative() {
        return Err(FinError::msg("报工数量与工时不能为负").into());
    }
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO prod_op_log(op_id,po_id,po_no,item_code,item_name,op_name,worker,period,date,qty,hours)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        rusqlite::params![
            log.op_id,
            log.po_id,
            log.po_no.trim(),
            log.item_code.trim(),
            log.item_name.trim(),
            log.op_name.trim(),
            log.worker.trim(),
            log.period,
            log.date.trim(),
            crate::exact_param(log.qty),
            crate::exact_param(log.hours)
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

/// 某期间的报工流水（按人、按 id 排序）
pub fn log_list(db: &Db, period: i32) -> DbResult<Vec<PieceLog>> {
    let mut st = db.conn().prepare(
        "SELECT id,op_id,po_id,po_no,item_code,item_name,op_name,worker,period,date,qty,hours
         FROM prod_op_log WHERE period=?1 ORDER BY worker,id",
    )?;
    let rows = st
        .query_map(rusqlite::params![period], |r| {
            Ok(PieceLog {
                id: r.get(0)?,
                op_id: r.get(1)?,
                po_id: r.get(2)?,
                po_no: r.get(3)?,
                item_code: r.get(4)?,
                item_name: r.get(5)?,
                op_name: r.get(6)?,
                worker: r.get(7)?,
                period: r.get(8)?,
                date: r.get(9)?,
                qty: crate::read_money(r, 10)?,
                hours: crate::read_money(r, 11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 按期间算计件工资（按人汇总，金额在 Rust 侧用 Decimal 算）
///
/// 没记工人的流水不计入（报工单上没填人，发不出工资，但流水留着可查）。
pub fn compute(db: &Db, period: i32) -> DbResult<Vec<PiecePay>> {
    let rates = rate_list(db)?;
    let logs = log_list(db, period)?;
    let mut bucket: Vec<PiecePay> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for log in logs {
        let worker = log.worker.trim();
        if worker.is_empty() {
            continue;
        }
        let rate = rate_match(&rates, &log.item_code, &log.op_name);
        let unit = rate.map(|r| r.rate).unwrap_or(Money::ZERO);
        let amount = (log.qty * unit).round2();
        let line = PieceLine {
            po_no: log.po_no.clone(),
            item_code: log.item_code.clone(),
            item_name: log.item_name.clone(),
            op_name: log.op_name.clone(),
            qty: log.qty,
            rate: unit,
            amount,
            matched: rate.is_some(),
        };
        let i = match index.get(worker) {
            Some(i) => *i,
            None => {
                bucket.push(PiecePay {
                    employee: worker.to_string(),
                    qty: Money::ZERO,
                    amount: Money::ZERO,
                    lines: Vec::new(),
                });
                index.insert(worker.to_string(), bucket.len() - 1);
                bucket.len() - 1
            }
        };
        bucket[i].qty += log.qty;
        bucket[i].amount += amount;
        bucket[i].lines.push(line);
    }
    bucket.sort_by(|a, b| a.employee.cmp(&b.employee));
    Ok(bucket)
}

/// 某个人某期间的计件金额（工资表「一键取计件」用）
pub fn amount_of(db: &Db, period: i32, employee: &str) -> DbResult<Money> {
    let who = employee.trim();
    Ok(compute(db, period)?
        .into_iter()
        .find(|p| p.employee == who)
        .map(|p| p.amount)
        .unwrap_or(Money::ZERO))
}

/// 一次回填的结果（一个人）
#[derive(Clone, Debug, serde::Serialize)]
pub struct PieceFill {
    pub employee: String,
    pub qty: Money,
    /// 本期的计件金额
    pub piece: Money,
    /// 回填前的应发
    pub before: Money,
    /// 回填后的应发
    pub after: Money,
}

/// 把一个人的计件额回填进工资表
///
/// 口径只有一条：**应发低于计件额时才抬到计件额**。工资表上的应发可能是
/// 「底薪 + 计件」或者手工调过的数，无条件覆盖会把底薪抹掉；而不覆盖又会让
/// 纯计件的工人漏发。所以只向上补，返回 `None` 表示不用动。
///
/// 抬应发之后必须重算社保与个税（走 `business::payroll_calc`），并且要过
/// `payroll_upsert` 的结账闸门——已结账月份不许再改工资表。
pub fn fill_one(db: &Db, period: Period, p: &PiecePay) -> DbResult<Option<PieceFill>> {
    let old = crate::business::payroll_get(db, period, &p.employee)?;
    let (dept, gross, social, housing, deduction, additional, social_co, housing_co, memo) =
        match &old {
            Some(o) => (
                o.dept.clone(),
                o.gross,
                o.social,
                o.housing,
                o.deduction,
                o.additional,
                o.social_co,
                o.housing_co,
                o.memo.clone(),
            ),
            None => (
                String::new(),
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
                String::new(),
            ),
        };
    if gross >= p.amount {
        return Ok(None);
    }
    let memo = if memo.trim().is_empty() {
        format!("计件 {} 件 / {}", p.qty, p.amount)
    } else {
        format!("{}｜计件 {}", memo, p.amount)
    };
    let row = crate::business::payroll_calc(
        db,
        period,
        &p.employee,
        &dept,
        p.amount,
        social,
        housing,
        deduction,
        additional,
        social_co,
        housing_co,
        &memo,
    )?;
    crate::business::payroll_upsert(db, &row)?;
    Ok(Some(PieceFill {
        employee: p.employee.clone(),
        qty: p.qty,
        piece: p.amount,
        before: gross,
        after: row.gross,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    fn log(op_id: i64, item: &str, op: &str, worker: &str, period: i32, qty: &str) -> PieceLog {
        PieceLog {
            id: 0,
            op_id,
            po_id: 1,
            po_no: "SC-202611-001".to_string(),
            item_code: item.to_string(),
            item_name: "铝件".to_string(),
            op_name: op.to_string(),
            worker: worker.to_string(),
            period,
            date: "2026-11-05".to_string(),
            qty: m(qty),
            hours: Money::ZERO,
        }
    }

    #[test]
    fn 计件工资按期间汇总到人() {
        let db = crate::tests::mem();
        let mut r = PieceRate {
            item_code: "MB01".to_string(),
            op_name: "氧化".to_string(),
            rate: m("1.5"),
            ..Default::default()
        };
        rate_save(&db, &mut r).unwrap();

        log_add(&db, &log(1, "MB01", "氧化", "张三", 202611, "100")).unwrap();
        log_add(&db, &log(2, "MB01", "氧化", "张三", 202611, "60")).unwrap();
        log_add(&db, &log(3, "MB01", "氧化", "李四", 202611, "40")).unwrap();
        // 另一个期间的流水不能算进来
        log_add(&db, &log(4, "MB01", "氧化", "张三", 202612, "999")).unwrap();

        let pays = compute(&db, 202611).unwrap();
        assert_eq!(pays.len(), 2);
        assert_eq!(pays[0].employee, "张三");
        assert_eq!(pays[0].qty, m("160"));
        assert_eq!(pays[0].amount, m("240"), "160 × 1.5");
        assert_eq!(pays[0].lines.len(), 2);
        assert_eq!(pays[1].employee, "李四");
        assert_eq!(pays[1].amount, m("60"));

        assert_eq!(amount_of(&db, 202611, "张三").unwrap(), m("240"));
        assert_eq!(amount_of(&db, 202611, "王五").unwrap(), Money::ZERO);
    }

    #[test]
    fn 计件单价匹配优先级() {
        let db = crate::tests::mem();
        let mut wild = PieceRate {
            rate: m("0.8"),
            ..Default::default()
        };
        // 通配也要过一个「物料或工序」的校验，这里用只限工序的表达
        wild.op_name = String::new();
        wild.item_code = String::new();
        assert!(
            rate_save(&db, &mut wild).is_err(),
            "两个都空等于给所有人定价，拒收"
        );

        let mut by_op = PieceRate {
            op_name: "氧化".to_string(),
            rate: m("1"),
            ..Default::default()
        };
        rate_save(&db, &mut by_op).unwrap();
        let mut by_item = PieceRate {
            item_code: "MB01".to_string(),
            rate: m("2"),
            ..Default::default()
        };
        rate_save(&db, &mut by_item).unwrap();
        let mut both = PieceRate {
            item_code: "MB01".to_string(),
            op_name: "氧化".to_string(),
            rate: m("3"),
            ..Default::default()
        };
        rate_save(&db, &mut both).unwrap();

        let rates = rate_list(&db).unwrap();
        assert_eq!(rate_match(&rates, "MB01", "氧化").unwrap().rate, m("3"));
        assert_eq!(rate_match(&rates, "MB01", "包装").unwrap().rate, m("2"));
        assert_eq!(rate_match(&rates, "MB02", "氧化").unwrap().rate, m("1"));
        assert!(rate_match(&rates, "MB02", "包装").is_none());

        // 停用以后退到下一档
        both.disabled = true;
        rate_save(&db, &mut both).unwrap();
        let rates = rate_list(&db).unwrap();
        assert_eq!(rate_match(&rates, "MB01", "氧化").unwrap().rate, m("2"));
    }

    #[test]
    fn 没维护单价的行保留但金额为零() {
        let db = crate::tests::mem();
        let mut r = PieceRate {
            item_code: "MB01".to_string(),
            op_name: "氧化".to_string(),
            rate: m("1.5"),
            ..Default::default()
        };
        rate_save(&db, &mut r).unwrap();
        log_add(&db, &log(1, "MB01", "氧化", "张三", 202611, "100")).unwrap();
        // 没有单价：金额 0，但行要在，人要看得见
        // （物料和工序都得换个都对不上的：只限物料或只限工序的单价都会命中）
        log_add(&db, &log(2, "MB02", "包装", "张三", 202611, "50")).unwrap();

        let pays = compute(&db, 202611).unwrap();
        assert_eq!(pays.len(), 1);
        assert_eq!(pays[0].lines.len(), 2);
        assert_eq!(pays[0].amount, m("150"), "只有氧化那 100 件有单价");
        assert_eq!(pays[0].qty, m("150"), "数量照旧全算");
        assert!(!pays[0].lines[1].matched);
        assert_eq!(pays[0].lines[1].rate, Money::ZERO);
    }
    #[test]
    fn 计件回填只向上补并重算个税() {
        let db = crate::tests::mem();
        let mut r = PieceRate {
            item_code: "MB03".to_string(),
            op_name: "氧化".to_string(),
            rate: m("1.5"),
            ..Default::default()
        };
        rate_save(&db, &mut r).unwrap();
        log_add(&db, &log(901, "MB03", "氧化", "李四", 202611, "100")).unwrap();

        let p = Period::from_ymm(202611);
        let pays = compute(&db, 202611).unwrap();
        assert_eq!(pays.len(), 1);
        assert_eq!(pays[0].amount, m("150"));

        let f = fill_one(&db, p, &pays[0])
            .unwrap()
            .expect("应发 0 < 150，要回填");
        assert_eq!(f.before, Money::ZERO);
        assert_eq!(f.after, m("150"));
        let row = crate::business::payroll_get(&db, p, "李四")
            .unwrap()
            .unwrap();
        assert_eq!(row.gross, m("150"));
        // 个税按 150 重算：低于起征点，应交 0
        assert_eq!(row.tax, Money::ZERO);
        assert!(row.net == m("150"));

        // 再回填一次：应发已不低于计件额，不动（否则会重复加钱）
        assert!(fill_one(&db, p, &pays[0]).unwrap().is_none());
    }

    #[test]
    fn 计件回填保留比计件额高的应发() {
        let db = crate::tests::mem();
        let mut r = PieceRate {
            item_code: "MB04".to_string(),
            op_name: "包装".to_string(),
            rate: m("2"),
            ..Default::default()
        };
        rate_save(&db, &mut r).unwrap();
        log_add(&db, &log(902, "MB04", "包装", "王五", 202611, "50")).unwrap();

        let p = Period::from_ymm(202611);
        // 底薪 500 已经在工资表里
        let seed = crate::business::payroll_calc(
            &db,
            p,
            "王五",
            "",
            m("500"),
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            Money::ZERO,
            "底薪",
        )
        .unwrap();
        crate::business::payroll_upsert(&db, &seed).unwrap();

        let pays = compute(&db, 202611).unwrap();
        assert_eq!(pays[0].amount, m("100"));
        assert!(fill_one(&db, p, &pays[0]).unwrap().is_none());
        let row = crate::business::payroll_get(&db, p, "王五")
            .unwrap()
            .unwrap();
        assert_eq!(row.gross, m("500"), "不能把底薪压到计件额");
    }
}
