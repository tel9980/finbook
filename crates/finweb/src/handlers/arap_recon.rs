//! 应收应付对账批次：建批次 / 粘数字 / 逐户对照 / 确认落账。
//!
//! 口径与 `findb::arap_recon` 一致：批次「对账中」时完全不进报表，确认后只生成
//! 一张**草稿**调整凭证（要人工审核/记账）。前端 `app-analysis.js` 的对账页签调这里。

use super::*;

fn batch_json(b: &findb::arap_recon::Batch) -> serde_json::Value {
    json!({
        "id": b.id,
        "kind": b.kind,
        "account": b.account,
        "post_account": b.post_account,
        "title": b.title,
        "ymm": b.period.ymm(),
        "period": b.period.label(),
        "status": b.status,
        "memo": b.memo,
        "adjust_account": b.adjust_account,
        "voucher_id": b.voucher_id,
        "created_by": b.created_by,
        "created_at": b.created_at,
        "confirmed_by": b.confirmed_by,
        "confirmed_at": b.confirmed_at,
        "party_count": b.party_count,
        "claim_total": b.claim_total.fmt_money(),
        "diff_total": b.diff_total.fmt_money(),
    })
}

fn line_json(l: &findb::arap_recon::Line) -> serde_json::Value {
    json!({
        "id": l.id,
        "party_code": l.party_code,
        "party_name": l.party_name,
        "claim": l.claim.fmt_money(),
        "book": l.book.fmt_money(),
        "diff": l.diff.fmt_money(),
        "disputed": l.disputed,
        "agreed": l.agreed(),
        "note": l.note,
    })
}

#[derive(Deserialize)]
pub(super) struct ReconBatchNewReq {
    #[serde(default)]
    pub kind: String,
    /// 对账期间（YYYYMM）；0 表示当前期间
    #[serde(default)]
    pub ymm: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub account: String,
    #[serde(default)]
    pub post_account: String,
}

#[derive(Deserialize)]
pub(super) struct ReconImportReq {
    pub text: String,
}

#[derive(Deserialize)]
pub(super) struct ReconLineReq {
    pub party: String,
    pub claim: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize)]
pub(super) struct ReconLineSetReq {
    pub claim: String,
    #[serde(default)]
    pub disputed: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize)]
pub(super) struct ReconConfirmReq {
    #[serde(default)]
    pub adjust_account: String,
}

#[derive(Deserialize)]
pub(super) struct ReconListQuery {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
}

/// 批次列表
pub(super) async fn list_recon_batches(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<ReconListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let kind = if q.kind.trim().is_empty() {
        None
    } else {
        Some(q.kind.trim())
    };
    let status = if q.status.trim().is_empty() {
        None
    } else {
        Some(q.status.trim())
    };
    let batches = findb::arap_recon::list(db, kind, status)?;
    Ok(Json(json!({
        "batches": batches.iter().map(batch_json).collect::<Vec<_>>(),
    })))
}

/// 新建批次
pub(super) async fn create_recon_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReconBatchNewReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let kind = if req.kind.trim().is_empty() {
        findb::arap_recon::KIND_AR
    } else {
        req.kind.trim()
    };
    let period = if req.ymm > 0 {
        Period::from_ymm(req.ymm as i32)
    } else {
        periods::current_period(db)?
    };
    let id = findb::arap_recon::create(
        db,
        kind,
        period,
        &req.title,
        &req.account,
        &req.post_account,
        user.username(),
    )?;
    let b = findb::arap_recon::get_batch(db, id)?
        .ok_or_else(|| AppError::bad_request("批次创建失败".to_string()))?;
    Ok(Json(
        json!({ "ok": true, "id": id, "batch": batch_json(&b) }),
    ))
}

/// 批次详情：明细 + 体检数 + 账面有余额但没录进来的户
pub(super) async fn get_recon_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let d = findb::arap_recon::get(db, id)?
        .ok_or_else(|| AppError::bad_request(format!("对账批次 #{id} 不存在")))?;
    let book_only = findb::arap_recon::book_only(db, id)?;
    let unknown = findb::arap_recon::unknown_parties(db, id)?;
    Ok(Json(json!({
        "batch": batch_json(&d.batch),
        "lines": d.lines.iter().map(line_json).collect::<Vec<_>>(),
        "summary": {
            "party_count": d.summary.party_count,
            "agreed": d.summary.agreed,
            "disputed": d.summary.disputed,
            "need_adjust": d.summary.need_adjust,
            "claim_total": d.summary.claim_total.fmt_money(),
            "book_total": d.summary.book_total.fmt_money(),
            "diff_total": d.summary.diff_total.fmt_money(),
        },
        "book_only": book_only
            .iter()
            .map(|(code, name, amt)| json!({
                "party_code": code,
                "party_name": name,
                "book": amt.fmt_money(),
            }))
            .collect::<Vec<_>>(),
        "unknown_parties": unknown,
    })))
}

/// 粘贴导入（每行 `客商编码, 对账金额[, 备注]`）
pub(super) async fn import_recon_lines(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ReconImportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let r = findb::arap_recon::import_lines(db, id, &req.text, user.username())?;
    Ok(Json(json!({
        "ok": true,
        "imported": r.ok,
        "skipped": r.skipped,
        "warnings": r.warnings,
    })))
}

/// 加/改一户（同一户重复提交 = 覆盖金额）
pub(super) async fn upsert_recon_line(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ReconLineReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let claim = parse_money_checked(&req.claim)?;
    let line_id = findb::arap_recon::upsert_line(db, id, &req.party, claim, &req.note)?;
    Ok(Json(json!({ "ok": true, "id": line_id })))
}

/// 改一行金额 / 争议标记 / 备注
pub(super) async fn set_recon_line(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(line_id): Path<i64>,
    Json(req): Json<ReconLineSetReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let claim = parse_money_checked(&req.claim)?;
    findb::arap_recon::set_line(db, line_id, claim, req.disputed, &req.note)?;
    Ok(Json(json!({ "ok": true })))
}

/// 删一行
pub(super) async fn delete_recon_line(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(line_id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::arap_recon::delete_line(db, line_id)?;
    Ok(Json(json!({ "ok": true })))
}

/// 按账面余额把有来往的户先列进批次（金额先填账面数）
pub(super) async fn seed_recon_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let n = findb::arap_recon::seed_from_book(db, id, user.username())?;
    Ok(Json(json!({ "ok": true, "seeded": n })))
}

/// 重算账面余额
pub(super) async fn sync_recon_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let n = findb::arap_recon::sync_book(db, id)?;
    Ok(Json(json!({ "ok": true, "synced": n })))
}

/// 确认这一批（差额汇总成一张草稿调整凭证）
pub(super) async fn confirm_recon_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ReconConfirmReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let r = findb::arap_recon::confirm(db, id, user.username(), &req.adjust_account)?;
    Ok(Json(json!({
        "ok": true,
        "voucher_id": r.voucher_id,
        "adjusted": r.adjusted,
        "skipped_disputed": r.skipped_disputed,
        "diff_total": r.diff_total.fmt_money(),
    })))
}

/// 打回重对
pub(super) async fn reopen_recon_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::arap_recon::reopen(db, id, user.username())?;
    Ok(Json(json!({ "ok": true })))
}

/// 撤销批次
pub(super) async fn cancel_recon_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::arap_recon::cancel(db, id, user.username())?;
    Ok(Json(json!({ "ok": true })))
}

/// 删批次
pub(super) async fn delete_recon_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::arap_recon::delete(db, id, user.username())?;
    Ok(Json(json!({ "ok": true })))
}
