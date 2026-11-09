//! 加工费规则（按面积/件计价 + 最低收费 + 加急费）

use super::*;

#[derive(Deserialize)]
pub struct PriceRuleReq {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub item_code: String,
    #[serde(default)]
    pub customer_code: String,
    #[serde(default)]
    pub unit: String,
    pub unit_price: String,
    #[serde(default)]
    pub min_charge: String,
    #[serde(default)]
    pub rush_fee: String,
    #[serde(default)]
    pub rush_rate: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub memo: String,
    /// 生效日期 `YYYY-MM-DD`（空 = 立即）
    #[serde(default)]
    pub effective_from: String,
    /// 工艺档（如「硬质氧化」，空 = 不分档）
    #[serde(default)]
    pub process: String,
    /// 规格档：`max` / `min` / `first` / `area`，空 = 不分档
    #[serde(default)]
    pub band_dim: String,
    /// 区间下限（cm，空 = 不限）
    #[serde(default)]
    pub band_from: String,
    /// 区间上限（cm，空 = 不限，含端点）
    #[serde(default)]
    pub band_to: String,
    /// 数量档下限（计价数量，空 = 不限；下限不含）
    #[serde(default)]
    pub qty_from: String,
    /// 数量档上限（空 = 不限，含端点）
    #[serde(default)]
    pub qty_to: String,
}

fn money_or_zero(s: &str) -> Result<Money, AppError> {
    if s.trim().is_empty() {
        return Ok(Money::ZERO);
    }
    parse_money_checked(s)
}

/// 规则列表
pub(super) async fn list_price_rules(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceView)?;
    let rules = findb::processfee::rule_list(&user.db)?;
    Ok(Json(json!({ "rules": rules })))
}

/// 新建/修改规则
pub(super) async fn save_price_rule(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PriceRuleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let db = &user.db;
    let mut r = findb::processfee::PriceRule {
        id: req.id,
        name: req.name.trim().to_string(),
        item_code: req.item_code.trim().to_string(),
        customer_code: req.customer_code.trim().to_string(),
        unit: req.unit.trim().to_string(),
        unit_price: parse_money_checked(&req.unit_price)?,
        min_charge: money_or_zero(&req.min_charge)?,
        rush_fee: money_or_zero(&req.rush_fee)?,
        rush_rate: money_or_zero(&req.rush_rate)?,
        disabled: req.disabled,
        memo: req.memo,
        effective_from: req.effective_from.trim().to_string(),
        process: req.process.trim().to_string(),
        band_dim: req.band_dim.trim().to_string(),
        band_from: req.band_from.trim().to_string(),
        band_to: req.band_to.trim().to_string(),
        qty_from: req.qty_from.trim().to_string(),
        qty_to: req.qty_to.trim().to_string(),
    };
    let id = findb::processfee::rule_save(db, &mut r)?;
    db.log(
        user.username(),
        "销售",
        "加工费规则",
        &format!(
            "{} 单价 {}/{}{} 起步 {} 加急 {}+{}%",
            r.name,
            r.unit_price,
            r.unit,
            if r.process.is_empty() {
                String::new()
            } else {
                format!("（{}）", r.process)
            },
            r.min_charge,
            r.rush_fee,
            r.rush_rate
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 删除规则
pub(super) async fn delete_price_rule(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let db = &user.db;
    findb::processfee::rule_delete(db, id)?;
    db.log(user.username(), "销售", "删除加工费规则", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

/// 报价：按物料/客户匹配规则，算出基础价、起步补差、加急加价与合计
/// 「按规格算单价」请求体：一个费率 + 一串规格
#[derive(serde::Deserialize)]
pub struct SpecCalcReq {
    pub rate: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub density: String,
    #[serde(default)]
    pub specs: Vec<String>,
}

/// 按一个费率把一批规格折成每件单价。
///
/// 这是「2.5 元一米，规格 1200*80 / 920*60 各多少钱一件」的直接答案：
/// 不落库、不改单价表，就是一个算价器 —— 客户发来一张规格清单，粘进来就出价。
pub(super) async fn calc_spec_prices(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SpecCalcReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceView)?;
    let rate = parse_money_checked(&req.rate)?;
    if !rate.is_positive() {
        return Err(AppError::BadRequest("单价要大于 0".to_string()));
    }
    let unit = if req.unit.trim().is_empty() {
        return Err(AppError::BadRequest(
            "先选口径（按米还是按 ㎡）".to_string(),
        ));
    } else {
        req.unit.trim().to_string()
    };
    let density = money_or_zero(&req.density)?;
    if req.specs.len() > 500 {
        return Err(AppError::BadRequest("一次最多算 500 个规格".to_string()));
    }
    let rows = findb::processfee::spec_prices(rate, &unit, &req.specs, density);
    let out: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "spec": r.spec,
                "dims": r.dims,
                "factor": r.factor.fmt_dp(4),
                "price": r.price.fmt_money(),
                "note": r.note,
            })
        })
        .collect();
    Ok(Json(
        json!({ "unit": unit, "rate": rate.fmt_money(), "rows": out }),
    ))
}

pub(super) async fn quote_fee(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceView)?;
    let get = |k: &str| q.get(k).map(|s| s.trim()).unwrap_or("");
    let qty = parse_money_checked(get("qty"))?;
    let rush = matches!(get("rush"), "1" | "true" | "yes" | "on");
    // 新口径：给「库存单位数量 + 计价口径」，系统自己折算（300 只 → 7.02 ㎡）
    //
    // 带规格（`spec=1250*250*3`）时走按规格折算：没录过折算系数的工件也能报价 ——
    // 「80 元/㎡，规格 1250×250×3」这类问法在小厂里是日常，不该逼人先算好 0.3125 再录进去。
    //
    // 口径也可以留空：给了规格就让「规格档」来定口径 ——
    // 「200cm 以下按件 0.2 元、以上按米 1.5 元」这种价，口径本来就是档位决定的。
    if !get("spec").is_empty() {
        let stock_qty = parse_money_checked(get("stock_qty"))?;
        let density = money_or_zero(get("density"))?;
        let sq = findb::processfee::quote_with_spec(
            &user.db,
            get("item"),
            get("customer"),
            get("spec"),
            stock_qty,
            get("calc_unit"),
            density,
            get("process"),
            rush,
        )?;
        return Ok(Json(json!({
            "quote": sq.quote,
            "conv": {
                "stock_qty": sq.stock_qty.fmt_money(),
                "stock_unit": sq.stock_unit,
                // 折合量保留 4 位：1250×250 的板是 0.3125 ㎡，按金额口径取 2 位会显示成 0.31，
                // 而金额明明是按 0.3125 算的 —— 单子上「0.31 × 80 = 25.00」自己就对不上
                "factor": sq.factor.fmt_dp(4),
                "calc_qty": sq.calc_qty.fmt_money(),
                "calc_unit": sq.calc_unit,
                "dims": sq.dims,
                "basis": sq.basis,
            },
        })));
    }
    if !get("calc_unit").is_empty() {
        let stock_qty = parse_money_checked(get("stock_qty"))?;
        let uq = findb::processfee::quote_with_units(
            &user.db,
            get("item"),
            get("customer"),
            stock_qty,
            get("calc_unit"),
            get("process"),
            rush,
        )?;
        return Ok(Json(json!({ "quote": uq.quote, "conv": uq })));
    }
    // 老路径（只给计价数量、不给口径）：本来走 quote_by，但它不认工艺档 ——
    // 带了 process 就必须按档位匹配，否则「硬质 18 元」会被普通档的 12 元顶掉。
    if get("process").is_empty() {
        let quote = findb::processfee::quote_by(&user.db, get("item"), get("customer"), qty, rush)?;
        return Ok(Json(json!({ "quote": quote })));
    }
    let rule = findb::processfee::match_rule_for_process(
        &user.db,
        get("item"),
        get("customer"),
        "",
        get("process"),
    )?
    .map(|(r, _)| r)
    .ok_or_else(|| {
        AppError::BadRequest(format!(
            "没有「{}」这一档的单价（物料 {}／客户 {}）——先在单价表里给它单开一格",
            get("process"),
            get("item"),
            if get("customer").is_empty() {
                "任意"
            } else {
                get("customer")
            }
        ))
    })?;
    let quote = findb::processfee::quote_on(&rule, qty, rush, &findb::processfee::today());
    Ok(Json(json!({ "quote": quote })))
}

// ---------------------------------------------------------------------------
// 计价口径与折算系数（v49）
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CalcUnitReq {
    #[serde(default)]
    pub id: i64,
    pub item_code: String,
    pub calc_unit: String,
    pub factor: String,
    #[serde(default)]
    pub scale: Option<i32>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub memo: String,
}

/// 折算系数列表（`?item=` 空 = 全部物料）
pub(super) async fn list_calc_units(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceView)?;
    let item = q.get("item").map(|s| s.trim()).unwrap_or("");
    let units = findb::processfee::calc_unit_list(&user.db, item)?;
    Ok(Json(json!({ "units": units })))
}

/// 保存一条折算系数
pub(super) async fn save_calc_unit(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CalcUnitReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let db = &user.db;
    let mut c = findb::processfee::CalcUnit {
        id: req.id,
        item_code: req.item_code.trim().to_string(),
        calc_unit: req.calc_unit.trim().to_string(),
        factor: parse_money_checked(&req.factor)?,
        scale: req.scale.unwrap_or(2),
        disabled: req.disabled,
        memo: req.memo,
    };
    let id = findb::processfee::calc_unit_save(db, &mut c)?;
    db.log(
        user.username(),
        "销售",
        "折算系数",
        &format!(
            "{}：1 {} = {} {}",
            c.item_code, c.item_code, c.factor, c.calc_unit
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 删除一条折算系数
pub(super) async fn delete_calc_unit(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let db = &user.db;
    findb::processfee::calc_unit_delete(db, id)?;
    db.log(user.username(), "销售", "删除折算系数", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

/// 单价矩阵：行 = 物料，列 = 计价口径，格 = 单价
pub(super) async fn get_price_matrix(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceView)?;
    let customer = q.get("customer").map(|s| s.trim()).unwrap_or("");
    let process = q.get("process").map(|s| s.trim()).unwrap_or("");
    let mx = findb::processfee::matrix(&user.db, customer, process)?;
    Ok(Json(json!(mx)))
}

/// 「按规格分段的价」：只列带规格档的规则（短件按件 / 长料按米这种）
pub(super) async fn list_price_bands(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceView)?;
    let customer = q.get("customer").map(|s| s.trim()).unwrap_or("");
    let rows = findb::processfee::band_list(&user.db, customer)?;
    Ok(Json(json!({ "customer": customer, "rows": rows })))
}

#[derive(Deserialize)]
pub struct MatrixCellReq {
    #[serde(default)]
    pub customer_code: String,
    #[serde(default)]
    pub item_code: String,
    pub calc_unit: String,
    pub unit_price: String,
    #[serde(default)]
    pub min_charge: String,
    #[serde(default)]
    pub effective_from: String,
    /// 工艺档（空 = 不分档）
    #[serde(default)]
    pub process: String,
}

/// 改矩阵里的一格：没有规则就建一条，有就改价
///
/// 名字自动生成（`MB01 ㎡（C01）`）——老板填矩阵时只想输数字，
/// 让他先起名字是最容易放弃的一步。
pub(super) async fn set_matrix_cell(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<MatrixCellReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let db = &user.db;
    let item = req.item_code.trim();
    let customer = req.customer_code.trim();
    let unit = req.calc_unit.trim();
    if unit.is_empty() {
        return Err(AppError::BadRequest("计价口径必填".into()));
    }
    let price = parse_money_checked(&req.unit_price)?;
    let min_charge = money_or_zero(&req.min_charge)?;
    let effective_from = req.effective_from.trim().to_string();
    let process = req.process.trim().to_string();

    // 先找现成的同格规则（同物料 + 同客户 + 同口径 + 同生效日）
    //
    // 生效日也是身份的一部分：3 月起的新价要和现在的价并存，
    // 只按「物料+客户+口径」找的话，第二版价会把第一版覆盖掉，
    // 「提前录下一版价」这件事就做不成了。
    let existing = findb::processfee::rule_list(db)?.into_iter().find(|r| {
        r.item_code.trim() == item
            && r.customer_code.trim() == customer
            && r.unit.trim() == unit
            && r.effective_from.trim() == effective_from
            && r.process.trim() == process
    });
    let mut r = match existing {
        Some(r) => r,
        None => {
            let item_label = if item.is_empty() {
                "任意物料"
            } else {
                item
            };
            let proc_label = if process.is_empty() {
                String::new()
            } else {
                format!("·{process}")
            };
            let name = if customer.is_empty() {
                format!("{item_label} 按{unit}{proc_label}")
            } else {
                format!("{item_label} 按{unit}（{customer}）{proc_label}")
            };
            findb::processfee::PriceRule {
                name,
                item_code: item.to_string(),
                customer_code: customer.to_string(),
                unit: unit.to_string(),
                process: process.clone(),
                ..Default::default()
            }
        }
    };
    r.unit_price = price;
    r.min_charge = min_charge;
    r.effective_from = effective_from;
    r.disabled = false;
    let id = findb::processfee::rule_save(db, &mut r)?;
    db.log(
        user.username(),
        "销售",
        "加工费规则",
        &format!(
            "矩阵改价 {} 按{}{} = {}",
            r.name,
            unit,
            if process.is_empty() {
                String::new()
            } else {
                format!("（{process}）")
            },
            price
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

// ---------------------------------------------------------------------------
// 批量导入（单价表 / 折算系数）
// ---------------------------------------------------------------------------

/// 粘贴 CSV 或上传 Excel 的请求体：两者给一个就行
#[derive(Deserialize)]
pub struct ImportPriceReq {
    /// 粘贴的 CSV/TSV 文本
    #[serde(default)]
    pub text: String,
    /// Excel(.xlsx) 的 base64；与 text 二选一
    #[serde(default)]
    pub file: Option<String>,
}

/// 把请求体变成 CSV 文本：有文件读文件（xlsx 也认），否则用粘贴的文本
fn import_text(req: &ImportPriceReq) -> Result<String, AppError> {
    let has_file = req
        .file
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if has_file {
        let bytes = super::b64_decode(req.file.as_deref().unwrap_or(""))?;
        let rows = findb::imports::read_xlsx_bytes(&bytes)?;
        return Ok(findb::imports::xlsx_to_csv_text(&rows));
    }
    if req.text.trim().is_empty() {
        return Err(AppError::bad_request(
            "请粘贴 CSV 内容，或者选一个 Excel 文件",
        ));
    }
    Ok(req.text.clone())
}

fn import_json(r: &findb::processfee::PriceImport) -> serde_json::Value {
    json!({
        "created": r.created,
        "updated": r.updated,
        "skipped": r.skipped,
        "warnings": r.warnings,
    })
}

/// 批量导入单价（列头见「下载模板」）
pub(super) async fn import_price_rules(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ImportPriceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let text = import_text(&req)?;
    let r = findb::processfee::import_rules(&user.db, &text, user.username())?;
    Ok(Json(import_json(&r)))
}

/// 批量导入折算系数
pub(super) async fn import_calc_units(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ImportPriceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let text = import_text(&req)?;
    let r = findb::processfee::import_calc_units(&user.db, &text, user.username())?;
    Ok(Json(import_json(&r)))
}

/// 下载导入模板（CSV 带 BOM，Excel 双击打开不乱码）
pub(super) async fn price_import_template(
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::PriceView)?;
    let kind = q.get("kind").map(String::as_str).unwrap_or("rules");
    let (body, file) = match kind {
        "rules" => (
            findb::processfee::rule_import_template(),
            "加工费单价-模板.csv",
        ),
        "units" => (
            findb::processfee::unit_import_template(),
            "折算系数-模板.csv",
        ),
        _ => {
            return Err(AppError::bad_request(
                "未知模板 kind：rules（单价）/ units（折算系数）",
            ))
        }
    };
    let mut csv = String::from("\u{feff}");
    csv.push_str(&body);
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{file}\"").as_str(),
            ),
        ],
        csv,
    )
        .into_response())
}
