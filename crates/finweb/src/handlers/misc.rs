use super::*;

pub(super) async fn download_attachment(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let a =
        findb::attach::get(&db, id)?.ok_or_else(|| AppError::NotFound("附件不存在".to_string()))?;
    let v = vouchers::get(&db, a.voucher_id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该附件"));
    }
    let data = findb::attach::read(&db, id)?;
    let ctype = match a.kind.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    };
    let encoded: String = a.name.bytes().map(|b| format!("%{b:02X}")).collect();
    let mut resp = data.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(ctype),
    );
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
        "inline; filename=\"{}\"; filename*=UTF-8''{}",
        a.name.replace('"', ""),
        encoded
    )) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

pub(super) async fn delete_attachment(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = &user.db;
    let a =
        findb::attach::get(&db, id)?.ok_or_else(|| AppError::NotFound("附件不存在".to_string()))?;
    let v = vouchers::get(&db, a.voucher_id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权删除该附件"));
    }
    findb::attach::delete(&db, id)?;
    db.log(
        user.username(),
        "凭证",
        "删除附件",
        &format!("{} 附件#{} {}", v.voucher_no(), id, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
}
