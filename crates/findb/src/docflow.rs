//! 单据下推与追溯（对标金蝶 源单 → 目标单）
//!
//! - **下推**：请购单（已审批）→ 一键生成采购订单草稿（单行、单价留空待补），同记录
//!   `doc_link` 勾稽、请购置 `ordered`；允许多次下推（拆单场景），前端以状态标签防呆。
//! - **追溯**：`doc_chain(kind, id)` 合并两类边——`doc_link` 的跨单勾稽（上游/下游）+
//!   该单据固有的执行子单据（采购订单 → 到货/退货/付款流水；销售订单 → 发货/退货/收款），
//!   统一为 `DocNode` 列表供前端渲染单据链面板。
//! - 执行进度以子流水累计为准（`po_receipt` / `so_shipment`），不落冗余字段。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 单据链节点（上下游合并视图）
#[derive(Clone, Debug, Serialize)]
pub struct DocNode {
    /// req / po / so / receipt / payment / shipment / so_payment
    pub kind: String,
    pub id: i64,
    pub no: String,
    pub date: String,
    /// 摘要（请购品名 / 供应商 / 客户 / 备注）
    pub title: String,
    pub qty: String,
    pub amount: String,
    pub memo: String,
    /// up = 上游源单 / down = 下游或执行单据
    pub dir: String,
}

fn d(s: &NaiveDate) -> String {
    s.format("%Y-%m-%d").to_string()
}

/// 记录一条下推勾稽（重复调用幂等：UNIQUE(src,dst)）
pub fn link_add(
    db: &Db,
    src_type: &str,
    src_id: i64,
    dst_type: &str,
    dst_id: i64,
    memo: &str,
) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO doc_link(src_type,src_id,dst_type,dst_id,memo,created_at)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(src_type,src_id,dst_type,dst_id) DO NOTHING",
        rusqlite::params![src_type, src_id, dst_type, dst_id, memo, now()],
    )?;
    Ok(())
}

/// 是否已存在某类下游边（下推幂等检查：同源单同目标类型只允许一条）
pub fn has_link(db: &Db, src_type: &str, src_id: i64, dst_type: &str) -> DbResult<bool> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM doc_link WHERE src_type=?1 AND src_id=?2 AND dst_type=?3",
        rusqlite::params![src_type, src_id, dst_type],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 请购单下推采购订单：
/// - 仅 已审批(approved) / 已下推(ordered) 可推（草稿/已取消拒绝）；
/// - 生成采购订单草稿：单行 = 请购品名与数量，**单价留空待补**（请购无价），供应商留空；
/// - 记录 doc_link；请购首次下推置 ordered。
/// 返回 (采购订单 id, 单号)。
/// 注：po_save 自带写事务，本函数不再嵌套外层事务（后续步骤为单行幂等写，失败可直接重推）。
pub fn req_push_po(db: &Db, req_id: i64, period: Period, who: &str) -> DbResult<(i64, String)> {
    let r = crate::procurement::pr_get(db, req_id)?
        .ok_or_else(|| fincore::FinError::not_found("请购单不存在"))?;
    match r.status.as_str() {
        "draft" => {
            return Err(
                fincore::FinError::state("请购单未审批，审批后才能下推采购订单").into(),
            )
        }
        "cancelled" => return Err(fincore::FinError::state("请购单已取消，不能下推").into()),
        _ => {}
    }
    let mut po = crate::scm::PurchaseOrder::new(period, r.date, "", "", who);
    po.lines = vec![crate::scm::PoLine {
        id: 0,
        po_id: 0,
        item_code: r.item_code.clone(),
        item_name: r.item_name.clone(),
        qty_ordered: r.qty,
        qty_received: Money::ZERO,
        unit_price: Money::ZERO,
        tax_rate: Money::ZERO,
        amount: Money::ZERO,
        tax_amount: Money::ZERO,
        memo: format!("下推自请购 #{}", r.no),
    }];
    po.no = crate::scm::po_next_no(db, period)?;
    po.memo = format!("源：请购 #{}", r.no);
    let po_id = crate::scm::po_save(db, &mut po)?;
    link_add(db, "req", req_id, "po", po_id, &format!("请购 #{} 下推采购订单", r.no))?;
    if r.status == "approved" {
        db.conn().execute(
            "UPDATE purchase_req SET status='ordered' WHERE id=?1 AND status='approved'",
            [req_id],
        )?;
    }
    let _ = who;
    Ok((po_id, po.no))
}

fn node_of(tx: &rusqlite::Connection, kind: &str, id: i64, dir: &str) -> DbResult<Option<DocNode>> {
    let row = match kind {
        "req" => tx
            .query_row(
                "SELECT no, date, item_name, qty, memo FROM purchase_req WHERE id=?1",
                [id],
                |r| {
                    Ok(DocNode {
                        kind: "req".into(),
                        id,
                        no: r.get(0)?,
                        date: r.get(1)?,
                        title: r.get(2)?,
                        qty: r.get::<_, String>(3)?,
                        amount: "0".into(),
                        memo: r.get(4)?,
                        dir: dir.to_string(),
                    })
                },
            )
            .optional()?,
        "po" => tx
            .query_row(
                "SELECT no, date, supplier_name, total_amount, status FROM purchase_order WHERE id=?1",
                [id],
                |r| {
                    Ok(DocNode {
                        kind: "po".into(),
                        id,
                        no: r.get(0)?,
                        date: r.get(1)?,
                        title: r.get::<_, String>(2)?,
                        qty: "0".into(),
                        amount: r.get(3)?,
                        memo: r.get::<_, String>(4)?,
                        dir: dir.to_string(),
                    })
                },
            )
            .optional()?,
        "so" => tx
            .query_row(
                "SELECT no, date, customer_name, total_amount, status FROM sales_order WHERE id=?1",
                [id],
                |r| {
                    Ok(DocNode {
                        kind: "so".into(),
                        id,
                        no: r.get(0)?,
                        date: r.get(1)?,
                        title: r.get::<_, String>(2)?,
                        qty: "0".into(),
                        amount: r.get(3)?,
                        memo: r.get::<_, String>(4)?,
                        dir: dir.to_string(),
                    })
                },
            )
            .optional()?,
        "quote" => tx
            .query_row(
                "SELECT no, date, customer_name, qty, status FROM quotation WHERE id=?1",
                [id],
                |r| {
                    Ok(DocNode {
                        kind: "quote".into(),
                        id,
                        no: r.get(0)?,
                        date: r.get(1)?,
                        title: r.get::<_, String>(2)?,
                        qty: r.get::<_, String>(3)?,
                        amount: "0".into(),
                        memo: r.get::<_, String>(4)?,
                        dir: dir.to_string(),
                    })
                },
            )
            .optional()?,
        "invoice" => tx
            .query_row(
                "SELECT number, date, buyer, seller, amount, status FROM invoice WHERE id=?1",
                [id],
                |r| {
                    Ok(DocNode {
                        kind: "invoice".into(),
                        id,
                        no: r.get(0)?,
                        date: r.get(1)?,
                        title: r.get::<_, String>(2)?,
                        qty: "0".into(),
                        amount: r.get(3)?,
                        memo: r.get::<_, String>(4)?,
                        dir: dir.to_string(),
                    })
                },
            )
            .optional()?,
        "receipt" => tx
            .query_row(
                "SELECT no, date, party, amount, status FROM receipt_doc WHERE id=?1",
                [id],
                |r| {
                    Ok(DocNode {
                        kind: "receipt".into(),
                        id,
                        no: r.get(0)?,
                        date: r.get(1)?,
                        title: r.get::<_, String>(2)?,
                        qty: "0".into(),
                        amount: r.get(3)?,
                        memo: r.get::<_, String>(4)?,
                        dir: dir.to_string(),
                    })
                },
            )
            .optional()?,
        "notice" => tx
            .query_row(
                "SELECT qty, date, memo FROM ship_notice WHERE id=?1",
                [id],
                |r| {
                    Ok(DocNode {
                        kind: "notice".into(),
                        id,
                        no: String::new(),
                        date: r.get(1)?,
                        title: "发货通知".into(),
                        qty: r.get(0)?,
                        amount: "0".into(),
                        memo: r.get::<_, String>(2)?,
                        dir: dir.to_string(),
                    })
                },
            )
            .optional()?,
        _ => None,
    };
    Ok(row)
}

fn q_rows(
    tx: &rusqlite::Connection,
    sql: &str,
    id: i64,
    kind: &str,
    is_qty: bool,
) -> DbResult<Vec<DocNode>> {
    let mut st = tx.prepare(sql)?;
    let rows = st
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for (rid, date, v, memo) in rows {
        out.push(DocNode {
            kind: kind.to_string(),
            id: rid,
            no: String::new(),
            date,
            title: String::new(),
            qty: if is_qty { v.clone() } else { "0".into() },
            amount: if is_qty { "0".into() } else { v },
            memo,
            dir: "down".into(),
        });
    }
    Ok(out)
}

/// 收付款单与源单已下推发票自动勾稽（订单页收款/付款动作后调用）：取源单 → invoice
/// 的发票中尚未与该收付款单建边的第一张，建立 (invoice→receipt) 边——票↔款链直达。
pub fn link_receipt_to_src_invoice(
    db: &Db,
    src_type: &str,
    src_id: i64,
    receipt_id: i64,
) -> DbResult<()> {
    let mut st = db.conn().prepare(
        "SELECT dst_id FROM doc_link WHERE src_type=?1 AND src_id=?2 AND dst_type='invoice'",
    )?;
    let invoices: Vec<i64> = st
        .query_map(rusqlite::params![src_type, src_id], |r| r.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for inv in invoices {
        let exists: Option<i64> = db
            .conn()
            .query_row(
                "SELECT id FROM doc_link
                 WHERE src_type='invoice' AND src_id=?1 AND dst_type='receipt' AND dst_id=?2",
                rusqlite::params![inv, receipt_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return link_add(db, "invoice", inv, "receipt", receipt_id, "收付款勾稽");
        }
    }
    Ok(())
}

/// 单据链：doc_link 上/下游 + 该单固有执行子单据，合并返回（up 在前）。
/// 支持 kind：req / po / so。
pub fn doc_chain(db: &Db, kind: &str, id: i64) -> DbResult<Vec<DocNode>> {
    let mut out: Vec<DocNode> = Vec::new();
    // 1) doc_link 勾稽边
    let mut st = db.conn().prepare(
        "SELECT src_type, src_id, dst_type, dst_id, memo FROM doc_link
         WHERE (dst_type=?1 AND dst_id=?2) OR (src_type=?1 AND src_id=?2)",
    )?;
    let links: Vec<(String, i64, String, i64, String)> = st
        .query_map(rusqlite::params![kind, id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (src_type, src_id, dst_type, dst_id, memo) in links {
        if dst_type == kind && dst_id == id {
            if let Some(mut n) = node_of(db.conn(), &src_type, src_id, "up")? {
                if n.memo.is_empty() {
                    n.memo = memo;
                }
                out.push(n);
            }
        } else if src_type == kind && src_id == id {
            if let Some(mut n) = node_of(db.conn(), &dst_type, dst_id, "down")? {
                if n.memo.is_empty() {
                    n.memo = memo;
                }
                out.push(n);
            }
        }
    }
    // 2) 固有执行子单据（外键直连）
    match kind {
        "po" => {
            out.extend(q_rows(
                db.conn(),
                "SELECT id, date, CAST(qty AS TEXT), memo FROM po_receipt WHERE po_id=?1 ORDER BY id",
                id,
                "receipt",
                true,
            )?);
            out.extend(q_rows(
                db.conn(),
                "SELECT id, date, CAST(amount AS TEXT), memo FROM po_payment WHERE po_id=?1 ORDER BY id",
                id,
                "payment",
                false,
            )?);
        }
        "so" => {
            out.extend(q_rows(
                db.conn(),
                "SELECT id, date, CAST(qty AS TEXT), memo FROM so_shipment WHERE so_id=?1 ORDER BY id",
                id,
                "shipment",
                true,
            )?);
            out.extend(q_rows(
                db.conn(),
                "SELECT id, date, CAST(amount AS TEXT), memo FROM so_payment WHERE so_id=?1 ORDER BY id",
                id,
                "so_payment",
                false,
            )?);
            out.extend(q_rows(
                db.conn(),
                "SELECT id, date, CAST(qty AS TEXT), memo FROM ship_notice WHERE so_id=?1 ORDER BY id",
                id,
                "notice",
                true,
            )?);
        }
        _ => {}
    }
    // 上游在前、下游在后
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn push_requires_approval_and_chain_links() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 单据不存在 → 拒
        assert!(req_push_po(&db, 999, p, "u").is_err());
        // 草稿请购 → 拒
        let mut req = crate::procurement::PurchaseReq {
            id: 0,
            no: String::new(),
            period: p,
            date: NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
            item_code: "RM01".into(),
            item_name: "原料".into(),
            qty: Money::parse("10").unwrap(),
            status: "draft".into(),
            requester: String::new(),
            memo: String::new(),
        };
        let rid = crate::procurement::pr_save(&db, &mut req).unwrap();
        assert!(req_push_po(&db, rid, p, "u").is_err(), "未审批不能下推");
        // 审批 → 下推成功
        crate::procurement::pr_approve(&db, rid).unwrap();
        let (po_id, po_no) = req_push_po(&db, rid, p, "u").unwrap();
        assert!(po_no.starts_with("CG"), "采购订单号 CG 前缀：{po_no}");
        // 请购置 ordered
        let after = crate::procurement::pr_get(&db, rid).unwrap().unwrap();
        assert_eq!(after.status, "ordered", "下推后请购应为已下推");
        // 订单行 = 请购数量、单价留空
        let po = crate::scm::po_get(&db, po_id).unwrap().unwrap();
        assert_eq!(po.lines.len(), 1);
        assert_eq!(po.lines[0].qty_ordered, m10());
        assert!(po.lines[0].unit_price.is_zero(), "单价留空待补");
        // 追溯：po 上游 = 该请购；req 下游 = 该订单
        let chain = doc_chain(&db, "po", po_id).unwrap();
        assert!(chain.iter().any(|n| n.kind == "req" && n.id == rid && n.dir == "up"), "PO 应见上游请购");
        let chain2 = doc_chain(&db, "req", rid).unwrap();
        assert!(chain2.iter().any(|n| n.kind == "po" && n.id == po_id && n.dir == "down"), "请购应见下游订单");
        // 拆单：再次下推生成第二张订单与第二条链
        let (po2, _) = req_push_po(&db, rid, p, "u").unwrap();
        assert_ne!(po2, po_id, "重复下推应生成新订单（拆单）");
        assert_eq!(doc_chain(&db, "req", rid).unwrap().iter().filter(|n| n.kind == "po").count(), 2);
    }

    fn m10() -> Money {
        Money::parse("10").unwrap()
    }
}
