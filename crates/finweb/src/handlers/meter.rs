//! 电费 / 水费充值台账接口。
//!
//! 三步：充值（走普通凭证或资金模块）→ 抄表登记（本页）→ 结转出凭证（本页一键）。
//! 充值与余额不在这里存：112301 的借方发生额就是本期充值、期末余额就是卡里剩的钱，
//! 从账上直接读，避免「台账一套数、总账另一套数」。

use super::*;

#[derive(Deserialize)]
pub(super) struct ReadingReq {
    /// electric / water，缺省 electric
    #[serde(default)]
    kind: String,
    /// 表号 / 户号
    meter_no: String,
    /// 抄表日期，留空 = 今天
    #[serde(default)]
    date: String,
    /// 本期用量（度 / 吨）
    #[serde(default)]
    qty: String,
    /// 含税单价
    #[serde(default)]
    unit_price: String,
    /// 本期金额（含税）。留空则按 用量 × 单价 算
    #[serde(default)]
    amount: String,
    /// 税率：0.13 / 0.09 / 0（无票）
    #[serde(default)]
    rate: String,
    #[serde(default)]
    memo: String,
    /// 归属期间 yyyy-MM，留空 = 当前账期
    #[serde(default)]
    period: String,
}

#[derive(Deserialize)]
pub(super) struct CloseReq {
    #[serde(default)]
    period: String,
    #[serde(default)]
    kind: String,
    /// 预付科目，留空自动找 112301 / 1123
    #[serde(default)]
    account: String,
    /// 预付科目核算供应商时要填
    #[serde(default)]
    supplier: String,
    /// 结转日期，留空 = 今天
    #[serde(default)]
    date: String,
}

fn reading_json(r: &findb::meter::Reading) -> serde_json::Value {
    let (net, tax) = r.split().unwrap_or((r.amount_incl, fincore::Money::ZERO));
    json!({
        "id": r.id,
        "kind": r.kind,
        "kind_label": findb::meter::kind_label(&r.kind),
        "meter_no": r.meter_no,
        "read_date": r.read_date,
        "qty": r.qty.fmt_qty(),
        "unit_price": r.unit_price.fmt_qty(),
        "amount": r.amount_incl.fmt_money(),
        "net": net.fmt_money(),
        "tax": tax.fmt_money(),
        "rate": r.tax_rate,
        "voucher_id": r.voucher_id,
        "memo": r.memo,
        "closed": r.is_closed(),
    })
}

/// 台账列表 + 概览：充了多少 / 用了多少 / 卡里还剩 / 待结转
pub(super) async fn list_meter(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(_state.as_ref(), &user, &q);
    let kind = q.get("kind").map(|s| s.as_str()).unwrap_or("");
    let kind_opt = if kind.is_empty() { None } else { Some(kind) };
    let rows = findb::meter::list(db, Some(period.ymm()), kind_opt)?;
    let sum = findb::meter::summary(db, period, kind_opt, None)?;
    let items: Vec<serde_json::Value> = rows.iter().map(reading_json).collect();
    Ok(Json(json!({
        "period": period.to_string(),
        "kind": kind,
        "summary": {
            "recharged": sum.recharged.fmt_money(),
            "consumed": sum.consumed.fmt_money(),
            "balance": sum.balance.fmt_money(),
            "unclosed": sum.unclosed.fmt_money(),
            "unclosed_net": sum.unclosed_net.fmt_money(),
            "unclosed_tax": sum.unclosed_tax.fmt_money(),
            "unclosed_count": sum.unclosed_count,
        },
        "items": items,
    })))
}

/// 登记一条抄表记录
pub(super) async fn add_meter(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReadingReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = if req.period.trim().is_empty() {
        query_period(_state.as_ref(), &user, &HashMap::new())
    } else {
        parse_period(&req.period)
            .ok_or_else(|| AppError::bad_request(format!("账期「{}」格式不对", req.period)))?
    };
    let date = prod_act_date(&req.date);
    let mut r = findb::meter::Reading {
        kind: if req.kind.trim().is_empty() {
            findb::meter::KIND_ELECTRIC.to_string()
        } else {
            req.kind.trim().to_string()
        },
        meter_no: req.meter_no.trim().to_string(),
        period: period.ymm(),
        read_date: date.to_string(),
        qty: parse_money_checked(&req.qty).unwrap_or(fincore::Money::ZERO),
        unit_price: parse_money_checked(&req.unit_price).unwrap_or(fincore::Money::ZERO),
        amount_incl: parse_money_checked(&req.amount).unwrap_or(fincore::Money::ZERO),
        tax_rate: req.rate.trim().to_string(),
        memo: req.memo.trim().to_string(),
        ..Default::default()
    };
    let id = findb::meter::add(db, &mut r, user.username())?;
    db.log(
        user.username(),
        "资金",
        "抄表登记",
        &format!(
            "{} {}{} 金额 {}",
            findb::meter::kind_label(&r.kind),
            r.meter_no,
            r.qty.fmt_qty(),
            r.amount_incl.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "id": id })))
}

/// 删除一条抄表记录（已结转的不许删）
pub(super) async fn delete_meter(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::meter::delete(db, id)?;
    db.log(user.username(), "资金", "删除抄表", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 结转：把本期未结转的抄表行做成一张凭证
pub(super) async fn close_meter(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CloseReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = if req.period.trim().is_empty() {
        query_period(_state.as_ref(), &user, &HashMap::new())
    } else {
        parse_period(&req.period)
            .ok_or_else(|| AppError::bad_request(format!("账期「{}」格式不对", req.period)))?
    };
    let date = prod_act_date(&req.date);
    let kind = if req.kind.trim().is_empty() {
        None
    } else {
        Some(req.kind.trim())
    };
    let account = if req.account.trim().is_empty() {
        None
    } else {
        Some(req.account.trim())
    };
    let supplier = if req.supplier.trim().is_empty() {
        None
    } else {
        Some(req.supplier.trim())
    };
    let vid = findb::meter::close(db, period, kind, account, supplier, date, user.username())?;
    match vid {
        Some(v) => {
            db.log(
                user.username(),
                "资金",
                "抄表结转",
                &format!("{period} 凭证#{v}"),
            )?;
            Ok(Json(json!({ "voucher_id": v })))
        }
        None => Ok(Json(
            json!({ "voucher_id": 0, "message": "本期没有待结转的抄表记录" }),
        )),
    }
}
