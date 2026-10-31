//! 客供料台账（受托加工的客户来料）
//!
//! 来料加工厂的料是客户拉来的，**不进自己的存货、不产生金额、不出凭证**。
//! 但业务上必须能回答三个问题：
//!
//! 1. 某个客户现在还有多少料在我厂里？（[`balances`]）
//! 2. 这个月拉来多少、交回多少、退了多杀、损耗多少？（[`statement`]）
//! 3. 这次交货会不会超过现存数量？（[`doc_save`] 里的余额校验）
//!
//! 单据四类：[`KINDS`] —— `in` 来料（+）、`out` 交货（−）、`return` 退料（−）、
//! `loss` 损耗核销（−）。四个方向都会动余额，只有「来料」是加法。
//!
//! 口径约定（踩过才知道）：
//! - 数量一律 `Money`（Decimal）在 **Rust 侧**加减，绝不写 `SUM(l.qty)` ——
//!   SQLite 的 `SUM` 会退化成 REAL 浮点，0.1+0.2 就开始掉分。
//! - 批次号是余额维度的一部分：同一物料、不同批次的料不能互相顶账。
//!   批次留空 = 「不分批次」这一个维度，与「有批次」是两回事。
//! - 编辑单据时余额校验要**排除本单自己**（[`available`] 的 `exclude_doc`），
//!   否则「把 100 改成 100」这种原样保存都会被自己拦下。

use chrono::NaiveDate;
use fincore::{FinError, Money, Period};
use rusqlite::OptionalExtension;
use std::collections::HashMap;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 单据类型（数据值, 中文标签）
pub const KINDS: &[(&str, &str)] = &[
    ("in", "来料"),
    ("out", "交货"),
    ("return", "退料"),
    ("loss", "损耗核销"),
];

/// 单据类型的中文标签（不认识的原样返回，便于前端兜底显示）
pub fn kind_label(kind: &str) -> &str {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, l)| *l)
        .unwrap_or(kind)
}

/// 客供料单据（头 + 行）
#[derive(Clone, Debug, serde::Serialize)]
pub struct ConsignDoc {
    pub id: i64,
    /// 单号（按单据类型 + 期间自动生成，如 `LR-202611-001`）
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    /// 客户 code（辅助档案·客户）
    pub customer: String,
    /// in / out / return / loss
    pub kind: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
    #[serde(default)]
    pub lines: Vec<ConsignLine>,
}

impl ConsignDoc {
    /// 按类型建一张空单（期间由日期推）
    pub fn new(customer: &str, kind: &str, date: NaiveDate) -> Self {
        Self {
            id: 0,
            no: String::new(),
            period: Period::from_date(date),
            date,
            customer: customer.trim().to_string(),
            kind: kind.to_string(),
            memo: String::new(),
            created_by: String::new(),
            created_at: String::new(),
            lines: Vec::new(),
        }
    }

    /// 是否减少余额的单据（交货 / 退料 / 损耗核销）
    pub fn is_outbound(&self) -> bool {
        matches!(self.kind.as_str(), "out" | "return" | "loss")
    }

    /// 单据类型的中文标签
    pub fn kind_label(&self) -> &str {
        kind_label(&self.kind)
    }
}

/// 客供料单据行
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ConsignLine {
    pub id: i64,
    pub doc_id: i64,
    /// 客户来料的物料编码/名称（可以不在自己存货档案里）
    pub item: String,
    pub item_name: String,
    /// 计量单位（件 / ㎡ / kg，自由文本）
    pub unit: String,
    pub qty: Money,
    /// 批次号（空 = 不分批次）
    pub batch_no: String,
    pub memo: String,
}

/// 客供料结存（按 客户 + 物料 + 批次）
#[derive(Clone, Debug, serde::Serialize)]
pub struct ConsignBalance {
    pub customer: String,
    pub item: String,
    pub item_name: String,
    pub unit: String,
    pub batch_no: String,
    pub qty: Money,
}

/// 客供料对账行（按 物料 + 批次）
#[derive(Clone, Debug, serde::Serialize)]
pub struct ConsignStatementRow {
    pub item: String,
    pub item_name: String,
    pub unit: String,
    pub batch_no: String,
    /// 期初结存（from 之前）
    pub begin: Money,
    pub inbound: Money,
    pub delivered: Money,
    pub returned: Money,
    pub loss: Money,
    /// 期末结存 = 期初 + 来料 − 交货 − 退料 − 损耗
    pub end: Money,
}

const D_COLS: &str = "id,no,period,date,customer,kind,memo,created_by,created_at";
const L_COLS: &str = "id,doc_id,item,item_name,unit,qty,batch_no,memo";

fn map_doc(r: &rusqlite::Row) -> rusqlite::Result<ConsignDoc> {
    Ok(ConsignDoc {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: parse_date(&r.get::<_, String>(3)?),
        customer: r.get(4)?,
        kind: r.get(5)?,
        memo: r.get(6)?,
        created_by: r.get(7)?,
        created_at: r.get(8)?,
        lines: Vec::new(),
    })
}

fn map_line(r: &rusqlite::Row) -> rusqlite::Result<ConsignLine> {
    Ok(ConsignLine {
        id: r.get(0)?,
        doc_id: r.get(1)?,
        item: r.get(2)?,
        item_name: r.get(3)?,
        unit: r.get(4)?,
        qty: crate::read_money(r, 5)?,
        batch_no: r.get(6)?,
        memo: r.get(7)?,
    })
}

fn parse_date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 一定合法"))
}

/// 单据列表（可按客户过滤），行一并带出
pub fn doc_list(db: &Db, customer: Option<&str>) -> DbResult<Vec<ConsignDoc>> {
    let sql = format!(
        "SELECT {D_COLS} FROM consign_doc
         WHERE (?1 IS NULL OR customer = ?1)
         ORDER BY date DESC, id DESC"
    );
    let mut st = db.conn().prepare(&sql)?;
    let mut rows: Vec<ConsignDoc> = st
        .query_map(rusqlite::params![customer], map_doc)?
        .collect::<Result<_, _>>()?;
    for d in &mut rows {
        d.lines = doc_lines(db, d.id)?;
    }
    Ok(rows)
}

fn doc_lines(db: &Db, doc_id: i64) -> DbResult<Vec<ConsignLine>> {
    let sql = format!("SELECT {L_COLS} FROM consign_line WHERE doc_id=?1 ORDER BY id");
    let mut st = db.conn().prepare(&sql)?;
    let out = st
        .query_map([doc_id], map_line)?
        .collect::<Result<_, _>>()?;
    Ok(out)
}

/// 取单张单据（含行）
pub fn doc_get(db: &Db, id: i64) -> DbResult<Option<ConsignDoc>> {
    let sql = format!("SELECT {D_COLS} FROM consign_doc WHERE id=?1");
    let mut d: Option<ConsignDoc> = db.conn().query_row(&sql, [id], map_doc).optional()?;
    if let Some(doc) = d.as_mut() {
        doc.lines = doc_lines(db, id)?;
    }
    Ok(d)
}

/// 保存单据（新建或修改），返回单据 id
///
/// 校验：客户必填、类型合法、至少一行、每行物料必填且数量 > 0；
/// **减少余额的单据（交货/退料/损耗）不能超过该客户现存客供料**。
pub fn doc_save(db: &Db, d: &mut ConsignDoc, who: &str) -> DbResult<i64> {
    let customer = d.customer.trim().to_string();
    if customer.is_empty() {
        return Err(FinError::msg("客供料单必须选客户").into());
    }
    if kind_label(&d.kind) == d.kind {
        return Err(FinError::msg(format!("单据类型不认识：{}", d.kind)).into());
    }
    if d.lines.is_empty() {
        return Err(FinError::msg("客供料单至少要一行").into());
    }
    for (i, l) in d.lines.iter().enumerate() {
        if l.item.trim().is_empty() {
            return Err(FinError::msg(format!("第 {} 行物料必填", i + 1)).into());
        }
        if !l.qty.is_positive() {
            return Err(FinError::msg(format!("第 {} 行数量必须大于 0", i + 1)).into());
        }
    }
    if d.is_outbound() {
        for (i, l) in d.lines.iter().enumerate() {
            let batch = l.batch_no.trim();
            let have = available(db, &customer, l.item.trim(), batch, d.id)?;
            if l.qty > have {
                let tail = if batch.is_empty() {
                    String::new()
                } else {
                    format!("（批次 {batch}）")
                };
                return Err(FinError::msg(format!(
                    "第 {} 行 {}{} 现存客供料 {}，本次 {} 超出可核销数量（先补登记来料）",
                    i + 1,
                    l.item.trim(),
                    tail,
                    have.fmt_qty(),
                    l.qty.fmt_qty()
                ))
                .into());
            }
        }
    }
    let tx = db.write_tx()?;
    let no = if d.no.trim().is_empty() {
        next_no(&tx, &d.kind, d.period)?
    } else {
        d.no.trim().to_string()
    };
    let id = if d.id > 0 {
        let n = tx.execute(
            "UPDATE consign_doc SET period=?2,date=?3,customer=?4,kind=?5,memo=?6 WHERE id=?1",
            rusqlite::params![
                d.id,
                d.period.ymm(),
                d.date.to_string(),
                customer,
                d.kind,
                d.memo
            ],
        )?;
        if n == 0 {
            return Err(FinError::msg(format!("客供料单 {0} 不存在", d.id)).into());
        }
        tx.execute("DELETE FROM consign_line WHERE doc_id=?1", [d.id])?;
        d.id
    } else {
        tx.execute(
            "INSERT INTO consign_doc(no,period,date,customer,kind,memo,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                no,
                d.period.ymm(),
                d.date.to_string(),
                customer,
                d.kind,
                d.memo,
                who,
                now()
            ],
        )?;
        tx.last_insert_rowid()
    };
    for l in &d.lines {
        tx.execute(
            "INSERT INTO consign_line(doc_id,item,item_name,unit,qty,batch_no,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                id,
                l.item.trim(),
                l.item_name.trim(),
                l.unit.trim(),
                crate::exact_param(l.qty),
                l.batch_no.trim(),
                l.memo
            ],
        )?;
    }
    tx.commit()?;
    d.id = id;
    d.no = no;
    d.customer = customer;
    Ok(id)
}

/// 删除单据（数量台账可直接删，删完余额自然回到删之前）
pub fn doc_delete(db: &Db, id: i64) -> DbResult<()> {
    let n = db
        .conn()
        .execute("DELETE FROM consign_doc WHERE id=?1", [id])?;
    if n == 0 {
        return Err(FinError::msg(format!("客供料单 {0} 不存在", id)).into());
    }
    Ok(())
}

/// 单号：按单据类型 + 期间流水，如 `LR-202611-001`
fn next_no(tx: &rusqlite::Transaction<'_>, kind: &str, period: Period) -> DbResult<String> {
    let prefix = match kind {
        "in" => "LR",
        "out" => "JH",
        "return" => "TL",
        _ => "SH",
    };
    let head = format!("{prefix}-{}", period.ymm());
    let mut seq: i64 = tx.query_row(
        "SELECT COUNT(*) FROM consign_doc WHERE no LIKE ?1 || '-%'",
        [&head],
        |r| r.get(0),
    )?;
    loop {
        seq += 1;
        let no = format!("{head}-{seq:03}");
        let exists: i64 =
            tx.query_row("SELECT COUNT(*) FROM consign_doc WHERE no=?1", [&no], |r| {
                r.get(0)
            })?;
        if exists == 0 {
            return Ok(no);
        }
    }
}

/// 一行「带单据头信息」的原始行，用于在 Rust 侧聚合
struct RawLine {
    customer: String,
    kind: String,
    item: String,
    item_name: String,
    unit: String,
    batch_no: String,
    qty: Money,
    period: i32,
}

fn load_lines(db: &Db, customer: Option<&str>, exclude_doc: i64) -> DbResult<Vec<RawLine>> {
    let mut st = db.conn().prepare(
        "SELECT d.customer, d.kind, l.item, l.item_name, l.unit, l.batch_no, l.qty, d.period
         FROM consign_line l JOIN consign_doc d ON d.id = l.doc_id
         WHERE (?1 IS NULL OR d.customer = ?1) AND d.id <> ?2
         ORDER BY d.date, d.id, l.id",
    )?;
    let out = st
        .query_map(rusqlite::params![customer, exclude_doc], |r| {
            Ok(RawLine {
                customer: r.get(0)?,
                kind: r.get(1)?,
                item: r.get(2)?,
                item_name: r.get(3)?,
                unit: r.get(4)?,
                batch_no: r.get(5)?,
                qty: crate::read_money(r, 6)?,
                period: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// 来料为加、其余为减
fn signed(kind: &str, qty: Money) -> Money {
    if kind == "in" {
        qty
    } else {
        Money::ZERO - qty
    }
}

/// 某客户某物料（某批次）的现存客供料数量
///
/// `exclude_doc` 用于编辑场景：排除该单据自身的贡献。
pub fn available(
    db: &Db,
    customer: &str,
    item: &str,
    batch_no: &str,
    exclude_doc: i64,
) -> DbResult<Money> {
    let item = item.trim();
    let batch_no = batch_no.trim();
    let mut tot = Money::ZERO;
    for r in load_lines(db, Some(customer.trim()), exclude_doc)? {
        if r.item == item && r.batch_no == batch_no {
            tot += signed(&r.kind, r.qty);
        }
    }
    Ok(tot)
}

/// 客供料结存（只列非零行，按客户 + 物料 + 批次）
pub fn balances(db: &Db, customer: Option<&str>) -> DbResult<Vec<ConsignBalance>> {
    let mut map: HashMap<(String, String, String, String, String), Money> = HashMap::new();
    for r in load_lines(db, customer, 0)? {
        let key = (
            r.customer.clone(),
            r.item.clone(),
            r.item_name.clone(),
            r.unit.clone(),
            r.batch_no.clone(),
        );
        let e = map.entry(key).or_insert(Money::ZERO);
        *e += signed(&r.kind, r.qty);
    }
    let mut out: Vec<ConsignBalance> = map
        .into_iter()
        .filter(|(_, qty)| !qty.is_zero())
        .map(
            |((customer, item, item_name, unit, batch_no), qty)| ConsignBalance {
                customer,
                item,
                item_name,
                unit,
                batch_no,
                qty,
            },
        )
        .collect();
    out.sort_by(|a, b| {
        (&a.customer, &a.item, &a.batch_no).cmp(&(&b.customer, &b.item, &b.batch_no))
    });
    Ok(out)
}

/// 客户对账单：期初 + 本期来料/交货/退料/损耗 = 期末（按物料 + 批次）
pub fn statement(
    db: &Db,
    customer: &str,
    from: Period,
    to: Period,
) -> DbResult<Vec<ConsignStatementRow>> {
    if from.ymm() > to.ymm() {
        return Err(FinError::msg(format!(
            "起始期间 {} 不能晚于结束期间 {}",
            from.label(),
            to.label()
        ))
        .into());
    }
    struct Acc {
        item_name: String,
        unit: String,
        begin: Money,
        inbound: Money,
        delivered: Money,
        returned: Money,
        loss: Money,
    }
    let mut map: HashMap<(String, String), Acc> = HashMap::new();
    for r in load_lines(db, Some(customer.trim()), 0)? {
        if r.period > to.ymm() {
            // 晚于区间的单据不进对账单（必须在建行之前跳过，
            // 否则会凭空多出一行全 0 的物料）
            continue;
        }
        let key = (r.item.clone(), r.batch_no.clone());
        let a = map.entry(key).or_insert_with(|| Acc {
            item_name: r.item_name.clone(),
            unit: r.unit.clone(),
            begin: Money::ZERO,
            inbound: Money::ZERO,
            delivered: Money::ZERO,
            returned: Money::ZERO,
            loss: Money::ZERO,
        });
        if a.item_name.is_empty() {
            a.item_name = r.item_name.clone();
        }
        if a.unit.is_empty() {
            a.unit = r.unit.clone();
        }
        if r.period < from.ymm() {
            a.begin += signed(&r.kind, r.qty);
        } else {
            match r.kind.as_str() {
                "in" => a.inbound += r.qty,
                "out" => a.delivered += r.qty,
                "return" => a.returned += r.qty,
                _ => a.loss += r.qty,
            }
        }
    }
    let mut out: Vec<ConsignStatementRow> = map
        .into_iter()
        .map(|((item, batch_no), a)| {
            let end = a.begin + a.inbound - a.delivered - a.returned - a.loss;
            ConsignStatementRow {
                item,
                item_name: a.item_name,
                unit: a.unit,
                batch_no,
                begin: a.begin,
                inbound: a.inbound,
                delivered: a.delivered,
                returned: a.returned,
                loss: a.loss,
                end,
            }
        })
        .collect();
    out.sort_by(|a, b| (&a.item, &a.batch_no).cmp(&(&b.item, &b.batch_no)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn d(dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, dd).unwrap()
    }

    fn doc(customer: &str, kind: &str, day: u32, lines: &[(&str, &str, &str)]) -> ConsignDoc {
        let mut v = ConsignDoc::new(customer, kind, d(day));
        v.lines = lines
            .iter()
            .map(|(item, batch, qty)| ConsignLine {
                item: item.to_string(),
                item_name: format!("{item} 毛坯"),
                unit: "件".to_string(),
                qty: m(qty),
                batch_no: batch.to_string(),
                ..Default::default()
            })
            .collect();
        v
    }

    fn save(db: &Db, mut v: ConsignDoc) -> i64 {
        doc_save(db, &mut v, "u").unwrap()
    }

    #[test]
    fn 来料交货退料损耗的结存与对账单() {
        let db = mem();
        save(&db, doc("C001", "in", 1, &[("MB01", "B1", "100")]));
        save(&db, doc("C001", "out", 5, &[("MB01", "B1", "30")]));
        save(&db, doc("C001", "return", 8, &[("MB01", "B1", "10")]));
        save(&db, doc("C001", "loss", 9, &[("MB01", "B1", "5")]));

        let bal = balances(&db, Some("C001")).unwrap();
        assert_eq!(bal.len(), 1);
        assert_eq!(bal[0].qty, m("55"), "100 − 30 − 10 − 5 应剩 55");
        assert_eq!(bal[0].batch_no, "B1");

        let p = Period::new(2026, 11).unwrap();
        let st = statement(&db, "C001", p, p).unwrap();
        assert_eq!(st.len(), 1);
        let r = &st[0];
        assert_eq!(r.begin, Money::ZERO, "本期期初应为 0");
        assert_eq!(r.inbound, m("100"));
        assert_eq!(r.delivered, m("30"));
        assert_eq!(r.returned, m("10"));
        assert_eq!(r.loss, m("5"));
        assert_eq!(r.end, m("55"), "期末 = 期初 + 来料 − 交货 − 退料 − 损耗");
    }

    #[test]
    fn 交货超过现存数量要报错() {
        let db = mem();
        save(&db, doc("C001", "in", 1, &[("MB01", "", "40")]));
        save(&db, doc("C001", "out", 5, &[("MB01", "", "40")]));
        // 现存 0，再交 1 件必须被拦下
        let mut over = doc("C001", "out", 6, &[("MB01", "", "1")]);
        let err = doc_save(&db, &mut over, "u").unwrap_err().to_string();
        assert!(err.contains("超出可核销数量"), "应拦下超交，实际：{err}");
        // 损耗核销不受影响（余额已经 0，但损耗 0 也不许：数量必须 > 0）
        let mut bad = doc("C001", "loss", 6, &[("MB01", "", "0")]);
        assert!(doc_save(&db, &mut bad, "u").is_err(), "数量为 0 也要拦");
    }

    #[test]
    fn 批次不同不能互相顶账() {
        let db = mem();
        save(&db, doc("C001", "in", 1, &[("MB01", "B1", "10")]));
        save(&db, doc("C001", "in", 1, &[("MB01", "B2", "5")]));
        let mut over = doc("C001", "out", 5, &[("MB01", "B2", "6")]);
        let err = doc_save(&db, &mut over, "u").unwrap_err().to_string();
        assert!(err.contains("批次 B2"), "报错要指出批次，实际：{err}");
        // B1 的 10 件不能拿来交 B2 的 6 件
        let bal = balances(&db, Some("C001")).unwrap();
        assert_eq!(bal.len(), 2, "两个批次各自一行");
        assert_eq!(bal[0].qty, m("10"));
        assert_eq!(bal[1].qty, m("5"));
    }

    #[test]
    fn 单号按类型与期间生成且不重复() {
        let db = mem();
        let mut a = doc("C001", "in", 1, &[("MB01", "", "1")]);
        let mut b = doc("C001", "in", 2, &[("MB01", "", "1")]);
        let mut c = doc("C001", "out", 3, &[("MB01", "", "1")]);
        doc_save(&db, &mut a, "u").unwrap();
        doc_save(&db, &mut b, "u").unwrap();
        doc_save(&db, &mut c, "u").unwrap();
        assert_eq!(a.no, "LR-202611-001");
        assert_eq!(b.no, "LR-202611-002");
        assert_eq!(c.no, "JH-202611-001", "交货单独一条流水");
        assert_eq!(kind_label("loss"), "损耗核销");
        assert_eq!(kind_label("???"), "???");
    }

    #[test]
    fn 编辑单据要排除自身占用() {
        let db = mem();
        save(&db, doc("C001", "in", 1, &[("MB01", "", "100")]));
        let id = save(&db, doc("C001", "out", 5, &[("MB01", "", "100")]));
        // 原样改一下备注再存：余额校验必须排除本单，否则「现存 100 交 100」会自己拦自己
        let mut got = doc_get(&db, id).unwrap().unwrap();
        got.memo = "改个备注".to_string();
        doc_save(&db, &mut got, "u").unwrap();
        assert_eq!(
            available(&db, "C001", "MB01", "", 0).unwrap(),
            Money::ZERO,
            "交完之后现存为 0"
        );
        // 真要改到 101 就必须拦
        let mut over = doc_get(&db, id).unwrap().unwrap();
        over.lines[0].qty = m("101");
        assert!(doc_save(&db, &mut over, "u").is_err(), "改大到超量要拦");
    }

    #[test]
    fn 期初按上一期间累计() {
        let db = mem();
        let mut oct = doc("C001", "in", 20, &[("MB01", "", "70")]);
        oct.date = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        oct.period = Period::from_date(oct.date);
        doc_save(&db, &mut oct, "u").unwrap();
        save(&db, doc("C001", "in", 3, &[("MB01", "", "30")]));

        let p11 = Period::new(2026, 11).unwrap();
        let st = statement(&db, "C001", p11, p11).unwrap();
        assert_eq!(st[0].begin, m("70"), "10 月的来料是 11 月的期初");
        assert_eq!(st[0].inbound, m("30"));
        assert_eq!(st[0].end, m("100"));

        // 11 月对账单不把 12 月的单据算进来
        let mut dec = doc("C001", "in", 5, &[("MB01", "", "9")]);
        dec.date = NaiveDate::from_ymd_opt(2026, 12, 5).unwrap();
        dec.period = Period::from_date(dec.date);
        doc_save(&db, &mut dec, "u").unwrap();
        let st = statement(&db, "C001", p11, p11).unwrap();
        assert_eq!(st[0].end, m("100"), "12 月的单据不进 11 月对账单");
    }
}
