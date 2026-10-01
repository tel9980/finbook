//! 发票管理（进项/销项台账）
//!
//! 记录发票代码、号码、日期、购销双方、金额/税额/税价合计与认证状态。
//! 不校验真伪（需对接税局），仅做台账登记与状态流转。

use fincore::{FinError, Money};
use rusqlite::OptionalExtension;

use crate::{read_money, Db, DbResult};

/// 发票记录
#[derive(Clone, Debug)]
pub struct Invoice {
    pub id: i64,
    /// in = 进项，out = 销项
    pub kind: String,
    pub code: String,
    pub number: String,
    pub date: String,
    pub buyer: String,
    pub seller: String,
    /// 价税合计
    pub amount_tax: Money,
    /// 不含税金额
    pub amount: Money,
    /// 税额
    pub tax: Money,
    /// 税率（十进制字符串）
    pub tax_rate: String,
    /// pending=待认证 / verified=已认证 / rejected=已作废
    pub status: String,
    pub memo: String,
    pub attach_id: i64,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Invoice {
    pub fn status_label(&self) -> &'static str {
        match self.status.as_str() {
            "verified" => "已认证",
            "rejected" => "已作废",
            _ => "待认证",
        }
    }
}

const COLS: &str = "id,kind,code,number,date,buyer,seller,amount_tax,amount,tax,tax_rate,\
     status,memo,attach_id,created_by,created_at,updated_at";

fn map(r: &rusqlite::Row) -> rusqlite::Result<Invoice> {
    Ok(Invoice {
        id: r.get(0)?,
        kind: r.get(1)?,
        code: r.get(2)?,
        number: r.get(3)?,
        date: r.get(4)?,
        buyer: r.get(5)?,
        seller: r.get(6)?,
        amount_tax: read_money(r, 7)?,
        amount: read_money(r, 8)?,
        tax: read_money(r, 9)?,
        tax_rate: r.get(10)?,
        status: r.get(11)?,
        memo: r.get(12)?,
        attach_id: r.get(13)?,
        created_by: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
    })
}

/// 查询条件
#[derive(Clone, Debug, Default)]
pub struct InvoiceQuery {
    pub kind: Option<String>,
    pub status: Option<String>,
    pub keyword: Option<String>,
    pub limit: Option<i64>,
}

/// 列表（按开票日期倒序）
pub fn list(db: &Db, q: &InvoiceQuery) -> DbResult<Vec<Invoice>> {
    let mut sql = format!("SELECT {COLS} FROM invoice WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(k) = &q.kind {
        sql.push_str(" AND kind = ?");
        params.push(Box::new(k.clone()));
    }
    if let Some(s) = &q.status {
        sql.push_str(" AND status = ?");
        params.push(Box::new(s.clone()));
    }
    if let Some(kw) = &q.keyword {
        if !kw.trim().is_empty() {
            let k = format!("%{}%", crate::escape_like(kw.trim()));
            sql.push_str(
                " AND (number LIKE ? ESCAPE '\\' OR code LIKE ? ESCAPE '\\' OR buyer LIKE ? ESCAPE '\\' OR seller LIKE ? ESCAPE '\\')",
            );
            for _ in 0..4 {
                params.push(Box::new(k.clone()));
            }
        }
    }
    sql.push_str(" ORDER BY date DESC, id DESC");
    if let Some(l) = q.limit {
        sql.push_str(" LIMIT ?");
        params.push(Box::new(l));
    }
    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), map)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 按 id 取发票
pub fn get(db: &Db, id: i64) -> DbResult<Option<Invoice>> {
    db.conn()
        .query_row(&format!("SELECT {COLS} FROM invoice WHERE id=?1"), [id], map)
        .optional()
        .map_err(Into::into)
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 校验必填
fn validate(inv: &Invoice) -> Result<(), FinError> {
    if inv.number.trim().is_empty() {
        return Err(FinError::msg("发票号码不能为空"));
    }
    if !matches!(inv.kind.as_str(), "in" | "out") {
        return Err(FinError::msg("发票类型只支持 in（进项）/ out（销项）"));
    }
    if inv.amount_tax.is_negative() || inv.amount.is_negative() || inv.tax.is_negative() {
        return Err(FinError::msg("发票金额不能为负"));
    }
    Ok(())
}

/// 新增发票，返回 id
pub fn insert(db: &Db, inv: &Invoice, who: &str) -> DbResult<i64> {
    validate(inv)?;
    let t = now();
    db.conn().execute(
        "INSERT INTO invoice(kind,code,number,date,buyer,seller,amount_tax,amount,tax,tax_rate,
            status,memo,attach_id,created_by,created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        rusqlite::params![
            inv.kind,
            inv.code,
            inv.number,
            inv.date,
            inv.buyer,
            inv.seller,
            inv.amount_tax.fmt_plain(),
            inv.amount.fmt_plain(),
            inv.tax.fmt_plain(),
            inv.tax_rate,
            if inv.status.is_empty() { "pending" } else { &inv.status },
            inv.memo,
            inv.attach_id,
            who,
            t,
            t,
        ],
    )?;
    let id = db.conn().last_insert_rowid();
    db.log(who, "发票", "新增", &format!("#{id} {}{}", inv.kind, inv.number))?;
    Ok(id)
}

/// 更新发票字段与状态
pub fn update(db: &Db, inv: &Invoice) -> DbResult<()> {
    validate(inv)?;
    let n = db.conn().execute(
        "UPDATE invoice SET kind=?2,code=?3,number=?4,date=?5,buyer=?6,seller=?7,
            amount_tax=?8,amount=?9,tax=?10,tax_rate=?11,status=?12,memo=?13,attach_id=?14,
            updated_at=?15 WHERE id=?1",
        rusqlite::params![
            inv.id,
            inv.kind,
            inv.code,
            inv.number,
            inv.date,
            inv.buyer,
            inv.seller,
            inv.amount_tax.fmt_plain(),
            inv.amount.fmt_plain(),
            inv.tax.fmt_plain(),
            inv.tax_rate,
            inv.status,
            inv.memo,
            inv.attach_id,
            now(),
        ],
    )?;
    if n == 0 {
        return Err(FinError::not_found(format!("发票 #{} 不存在", inv.id)).into());
    }
    db.log("", "发票", "更新", &format!("#{} {}", inv.id, inv.number))?;
    Ok(())
}

/// 状态流转：认证 / 作废
pub fn set_status(db: &Db, id: i64, status: &str, who: &str) -> DbResult<Invoice> {
    if !matches!(status, "pending" | "verified" | "rejected") {
        return Err(FinError::msg("非法发票状态").into());
    }
    let n = db.conn().execute(
        "UPDATE invoice SET status=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, status, now()],
    )?;
    if n == 0 {
        return Err(FinError::not_found("发票不存在").into());
    }
    db.log(who, "发票", if status == "verified" { "认证" } else { "标记状态" }, &format!("#{id} → {status}"))?;
    get(db, id)?.ok_or_else(|| FinError::not_found("发票不存在").into())
}

/// 删除发票
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    let n = db.conn().execute("DELETE FROM invoice WHERE id=?1", [id])?;
    if n == 0 {
        return Err(FinError::not_found(format!("发票 #{id} 不存在")).into());
    }
    db.log("", "发票", "删除", &format!("#{id}"))?;
    Ok(())
}

/// 下推发票的自动票号：FP+6位全局序号（撞号顺延——删除后重建的重号窗口）
fn next_invoice_no(db: &Db) -> DbResult<String> {
    let base: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM invoice", [], |r| r.get(0))?;
    let mut n = base + 1;
    loop {
        let no = format!("FP{n:06}");
        let exists: Option<i64> = db
            .conn()
            .query_row(
                "SELECT id FROM invoice WHERE number=?1",
                [&no],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Ok(no);
        }
        n += 1;
    }
}

/// 下推：采购订单 → 采购（进项）发票——金额=订单不含税/税额/价税合计，卖方=供应商、
/// 买方=本企业，状态 pending（待认证），票面代码留空、号码自动 FP 序号；记录 doc_link 勾稽。
pub fn push_from_po(db: &Db, po_id: i64, who: &str) -> DbResult<i64> {
    let po = crate::scm::po_get(db, po_id)?
        .ok_or_else(|| FinError::not_found("采购订单不存在"))?;
    if !po.total_amount.is_positive() {
        return Err(FinError::state("采购订单金额为 0，请先补价再下推发票").into());
    }
    let company = db.options().company.clone();
    let inv = Invoice {
        id: 0,
        kind: "in".to_string(),
        code: String::new(),
        number: next_invoice_no(db)?,
        date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        buyer: company,
        seller: po.supplier_name.clone(),
        amount_tax: po.total_amount + po.total_tax,
        amount: po.total_amount,
        tax: po.total_tax,
        tax_rate: String::new(),
        status: "pending".to_string(),
        memo: format!("下推自采购订单 {}", po.no),
        attach_id: 0,
        created_by: who.to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let id = insert(db, &inv, who)?;
    crate::docflow::link_add(db, "po", po_id, "invoice", id, "采购订单下推发票")?;
    Ok(id)
}

/// 下推：销售订单 → 销售（销项）发票——金额=订单整单（不含税/税额），买方=客户、
/// 卖方=本企业；状态 pending；记录 doc_link 勾稽。
pub fn push_from_so(db: &Db, so_id: i64, who: &str) -> DbResult<i64> {
    let so = crate::scm::so_get(db, so_id)?
        .ok_or_else(|| FinError::not_found("销售订单不存在"))?;
    if !so.total_amount.is_positive() {
        return Err(FinError::state("销售订单金额为 0，无法下推发票").into());
    }
    let company = db.options().company.clone();
    let inv = Invoice {
        id: 0,
        kind: "out".to_string(),
        code: String::new(),
        number: next_invoice_no(db)?,
        date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        buyer: so.customer_name.clone(),
        seller: company,
        amount_tax: so.total_amount + so.total_tax,
        amount: so.total_amount,
        tax: so.total_tax,
        tax_rate: String::new(),
        status: "pending".to_string(),
        memo: format!("下推自销售订单 {}", so.no),
        attach_id: 0,
        created_by: who.to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let id = insert(db, &inv, who)?;
    crate::docflow::link_add(db, "so", so_id, "invoice", id, "销售订单下推发票")?;
    Ok(id)
}

/// 汇总：进项/销项各自的金额与税额（用于发票台账统计）
/// 遵循全库约定：金额存 TEXT、不在 SQL 里 SUM，Rust 侧用 Decimal 累加。
pub fn summary(db: &Db) -> DbResult<Vec<(String, Money, Money, i64)>> {
    let mut stmt = db.conn().prepare(
        "SELECT kind, amount_tax, tax FROM invoice ORDER BY id",
    )?;
    let mut by_kind: std::collections::HashMap<String, (Money, Money, i64)> =
        std::collections::HashMap::new();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (kind, tax_total, tax_amt) in rows {
        let e = by_kind
            .entry(kind)
            .or_insert((Money::ZERO, Money::ZERO, 0));
        e.0 += Money::parse_or_zero(&tax_total);
        e.1 += Money::parse_or_zero(&tax_amt);
        e.2 += 1;
    }
    let mut out: Vec<(String, Money, Money, i64)> =
        by_kind.into_iter().map(|(k, v)| (k, v.0, v.1, v.2)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn sample(kind: &str, number: &str) -> Invoice {
        Invoice {
            id: 0,
            kind: kind.to_string(),
            code: "044001900111".to_string(),
            number: number.to_string(),
            date: "2026-03-10".to_string(),
            buyer: "甲公司".to_string(),
            seller: "乙公司".to_string(),
            amount_tax: Money::parse("11300").unwrap(),
            amount: Money::parse("10000").unwrap(),
            tax: Money::parse("1300").unwrap(),
            tax_rate: "0.13".to_string(),
            status: "pending".to_string(),
            memo: String::new(),
            attach_id: 0,
            created_by: "测试".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn crud_lifecycle() {
        let db = mem();
        // 新增
        let mut inv = sample("in", "12345678");
        let id = insert(&db, &inv, "u1").unwrap();
        assert!(id > 0);

        // 读取
        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(got.number, "12345678");
        assert_eq!(got.amount_tax, Money::parse("11300").unwrap());

        // 更新
        inv.id = id;
        inv.status = "verified".to_string();
        update(&db, &inv).unwrap();
        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(got.status, "verified");

        // 状态流转
        set_status(&db, id, "rejected", "u2").unwrap();
        let got = get(&db, id).unwrap().unwrap();
        assert_eq!(got.status, "rejected");

        // 删除
        delete(&db, id).unwrap();
        assert!(get(&db, id).unwrap().is_none());
    }

    #[test]
    fn list_and_summary() {
        let db = mem();
        insert(&db, &sample("in", "A001"), "u1").unwrap();
        insert(&db, &sample("in", "A002"), "u1").unwrap();
        insert(&db, &sample("out", "B001"), "u1").unwrap();

        // 按类型过滤
        let ins = list(&db, &InvoiceQuery { kind: Some("in".into()), ..Default::default() }).unwrap();
        assert_eq!(ins.len(), 2);
        // 关键字搜索
        let kw = list(&db, &InvoiceQuery { keyword: Some("B001".into()), ..Default::default() }).unwrap();
        assert_eq!(kw.len(), 1);
        assert_eq!(kw[0].kind, "out");

        // 汇总
        let sum = summary(&db).unwrap();
        let in_sum = sum.iter().find(|(k, _, _, _)| k == "in").unwrap();
        assert_eq!(in_sum.1, Money::parse("22600").unwrap()); // 两张进项 11300*2
        assert_eq!(in_sum.3, 2);
        let out_sum = sum.iter().find(|(k, _, _, _)| k == "out").unwrap();
        assert_eq!(out_sum.1, Money::parse("11300").unwrap());
    }

    #[test]
    fn validation_guards() {
        let db = mem();
        let inv = sample("in", "");
        assert!(insert(&db, &inv, "u1").is_err(), "发票号码必填");

        let inv2 = sample("bad", "X001");
        assert!(insert(&db, &inv2, "u1").is_err(), "非法类型应被拒");

        let mut inv3 = sample("in", "N001");
        inv3.amount_tax = Money::parse("-1").unwrap();
        assert!(insert(&db, &inv3, "u1").is_err(), "负金额应被拒");

        // 非法状态流转
        let id = insert(&db, &sample("in", "N002"), "u1").unwrap();
        assert!(set_status(&db, id, "whatever", "u1").is_err());
    }
}
