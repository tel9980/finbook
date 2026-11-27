//! 电子发票离线识别
//!
//! 把一张「文件」变成一条可入账的发票草稿。全程本地解析，不发任何网络请求。
//!
//! 通道按文件形态选择：
//! - `.xml`  结构化 XML（税务 UKey / 全电发票导出）
//! - `.json` 结构化 JSON（第三方工具导出）
//! - `.pdf`  先抽文本层，再复用纯文本正则；没有文本层（扫描件）直接拒
//! - 其它   按纯文本走正则
//! - 图片（png/jpg/bmp/gif）走 [`parse_image`]：二维码优先，OCR 不可用则降级
//!
//! 设计取舍：
//! - **金额不做四舍五入自愈**。解析到的价税合计与「不含税+税额」不一致时只标记
//!   复核，不改数；发票上的数字是法律凭证，宁可疑也不许悄悄抹平差额。
//! - **抽不出发票号码一律拒收**。号码是唯一的业务主键；没号码的草稿进台账后
//!   既不能查重也没法对账，等于制造垃圾数据（参照实现同为硬失败）。
//! - 置信度与复核标记只在**图片通道**产出。结构化通道是确定性解析，不需要
//!   「猜得准不准」这一维度。

use std::io::Cursor;

use fincore::{FinError, Money};
use regex::Regex;

use crate::invoices::{Invoice, InvoiceParse};

/// 图片通道判定「需要人工复核」的置信度阈值。
///
/// 0.75 来自参照实现的实测口径：纯二维码（无品名/交易对手）约 0.6~0.7，
/// 二维码 + 完整 OCR 约 0.9+。低于 0.75 一律要求人工看一眼。
pub const OCR_CONFIDENCE_THRESHOLD: f64 = 0.75;

/// 原文留痕上限。发票版面文本通常几百字，超过这个量基本是整页 PDF 或
/// 误传的文件，截断够用，也避免把整本 PDF 塞进 TEXT 列。
const RAW_TEXT_LIMIT: usize = 5000;

/// 解析出的发票草稿（还没落库）。
#[derive(Clone, Debug)]
pub struct ParsedInvoice {
    pub invoice: Invoice,
    /// 命中的通道：xml / json / pdf / text / image
    pub channel: String,
    /// 实际生效的引擎：xml / json / pdf / qr / qr+ocr / none
    pub engine: String,
    /// 0~1，仅图片通道有意义的置信度（结构化通道恒为 "1"）
    pub confidence: String,
    pub needs_review: bool,
    /// 解析过程中值得让用户看见的提示（如「PDF 无文本层」已被拒，不会走到这里）
    pub warnings: Vec<String>,
}

/// 发票类型代码 → 名称（税局口径，与二维码载荷前两位一致）。
const INVOICE_TYPE_CODES: &[(&str, &str)] = &[
    ("01", "增值税专用发票"),
    ("02", "货物运输业增值税专用发票"),
    ("03", "机动车销售统一发票"),
    ("04", "增值税普通发票"),
    ("10", "增值税普通发票(电子)"),
    ("11", "增值税普通发票(卷式)"),
    ("14", "增值税电子普通发票"),
    ("20", "增值税电子专用发票"),
];

fn type_name(code: &str) -> Option<&'static str> {
    INVOICE_TYPE_CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| *n)
}

/// 归一化日期到 `YYYY-MM-DD`；认不出返回空串（不猜）。
fn normalize_date(raw: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return String::new();
    }
    // 纯 8 位数字
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 8 {
        return format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8]);
    }
    // 2026-3-5 / 2026/3/5 / 2026.3.5
    let parts: Vec<&str> = s.split(['-', '/', '.']).map(|p| p.trim()).collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(m), Ok(d)) = (
            parts[0].parse::<i32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            if (1900..=2200).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d) {
                return format!("{y:04}-{m:02}-{d:02}");
            }
        }
    }
    String::new()
}

/// 抽取一个金额字符串 → Money。认不出返回 None（不猜 0）。
fn pick_money(raw: Option<&str>) -> Option<Money> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    Money::parse(s).ok()
}

/// 字段键的归一化：去掉空白/下划线/连字符/冒号，转小写。
///
/// 目的是让 `InvoiceNumber`、`invoice_number`、`发票 号码`、`发票号码：`
/// 落到同一个可比较的形态。中文原样保留小写（中文没有大小写）。
fn norm_key(k: &str) -> String {
    k.chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '_' && *c != '-')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 从扁平键值表中按候选键名取第一个非空值。
///
/// 两轮匹配：
/// 1. 归一化后**完全相等**（`InvoiceNumber` == `invoice_number` == `发票号码`）。
/// 2. 归一化后**以分隔符边界结尾**（`root.InvoiceNumber` 命中 `InvoiceNumber`，
///    但 `totalamount` **不会**误命中 `amount` —— 后缀必须在 `.` / `[` 之后开始）。
///    这条边界判断是必须的：否则「不含税金额」会被「价税合计」的 `total_amount`
///    后缀匹配吃掉，金额直接错位。
fn pick<'a>(flat: &'a [(String, String)], candidates: &[&str]) -> Option<&'a str> {
    for cand in candidates {
        let key = norm_key(cand);
        for (k, v) in flat {
            if norm_key(k) == key && !v.trim().is_empty() {
                return Some(v.trim());
            }
        }
    }
    // 次轮：按原始键的段边界做后缀匹配
    for cand in candidates {
        let key = norm_key(cand);
        for (k, v) in flat {
            if v.trim().is_empty() {
                continue;
            }
            // 取最后一个分隔段（`.` 或 `[n]` 之后的部分）
            let last_seg = k.rsplit(['.', ']']).next().unwrap_or(k);
            if norm_key(last_seg) == key {
                return Some(v.trim());
            }
        }
    }
    None
}

/// XML 候选键（各省导出命名不一，英文键做常见变体，中文键兜底）。
const K_NO: &[&str] = &[
    "InvoiceNumber",
    "invoiceNumber",
    "invoice_no",
    "发票号码",
    "发票号",
    "number",
];
const K_CODE: &[&str] = &["InvoiceCode", "invoiceCode", "invoice_code", "发票代码"];
const K_DATE: &[&str] = &[
    "InvoiceDate",
    "invoiceDate",
    "invoice_date",
    "开票日期",
    "date",
];
const K_SELLER: &[&str] = &[
    "SellerName",
    "sellerName",
    "seller_name",
    "销售方名称",
    "销方名称",
    "seller",
];
const K_BUYER: &[&str] = &[
    "BuyerName",
    "buyerName",
    "buyer_name",
    "购买方名称",
    "购方名称",
    "buyer",
];
const K_SELLER_TAX: &[&str] = &[
    "SellerTaxNo",
    "sellerTaxNo",
    "seller_tax_no",
    "销售方纳税人识别号",
    "销方识别号",
];
const K_BUYER_TAX: &[&str] = &[
    "BuyerTaxNo",
    "buyerTaxNo",
    "buyer_tax_no",
    "购买方纳税人识别号",
    "购方识别号",
];
const K_ITEM: &[&str] = &[
    "ItemName",
    "itemName",
    "item_name",
    "货物名称",
    "项目名称",
    "品名",
    "goods",
];
const K_AMOUNT: &[&str] = &[
    "AmountWithoutTax",
    "amountWithoutTax",
    "amount",
    "不含税金额",
    "金额",
];
const K_TAX: &[&str] = &["TaxAmount", "taxAmount", "tax_amount", "税额", "tax"];
const K_TOTAL: &[&str] = &[
    "TotalAmount",
    "totalAmount",
    "total_amount",
    "价税合计",
    "含税金额",
    "total",
];
const K_TYPE: &[&str] = &["InvoiceType", "invoiceType", "invoice_type", "发票类型"];

/// 判断是进项还是销项。
///
/// 依据本企业名称是否出现在销售方：销售方=本企业 → 这是开出去的销项票。
/// 本企业名称为空或两边都不匹配时按进项处理（采购取得发票更常见），
/// 并在 warnings 里留下提示让用户确认。
fn decide_kind(flat: &[(String, String)], company: &str, warns: &mut Vec<String>) -> String {
    let seller = pick(flat, K_SELLER).unwrap_or("");
    let buyer = pick(flat, K_BUYER).unwrap_or("");
    let company = company.trim();
    if !company.is_empty() {
        if seller.contains(company) {
            return "out".to_string();
        }
        if buyer.contains(company) {
            return "in".to_string();
        }
        warns.push(format!(
            "购销双方都不含本企业名称「{company}」，暂按进项票登记，请确认方向"
        ));
    }
    "in".to_string()
}

/// 从扁平键值表装配发票草稿。抽不出发票号或金额 → Err。
fn build_from_flat(
    flat: &[(String, String)],
    company: &str,
    channel: &str,
    engine: &str,
    raw_text: &str,
) -> Result<ParsedInvoice, FinError> {
    let mut warns: Vec<String> = Vec::new();

    let number = pick(flat, K_NO).unwrap_or("").to_string();
    if number.is_empty() {
        return Err(FinError::msg(
            "未能识别出发票号码（发票号是入账的必需字段），请改用手工录入",
        ));
    }

    let total = pick_money(pick(flat, K_TOTAL));
    let tax = pick_money(pick(flat, K_TAX));
    let amount = pick_money(pick(flat, K_AMOUNT));

    // 三者互相推导，规则与参照实现一致：缺哪一个就用另外两个补。
    let (amount, tax, amount_tax) = match (amount, tax, total) {
        (Some(a), Some(t), Some(tt)) => (a, t, tt),
        (Some(a), Some(t), None) => (a, t, a + t),
        (Some(a), None, Some(tt)) => {
            let t = tt - a;
            (a, t, tt)
        }
        (None, Some(t), Some(tt)) => (tt - t, t, tt),
        (Some(a), None, None) => (a, Money::ZERO, a),
        (None, Some(t), None) => (Money::ZERO, t, t),
        (None, None, Some(tt)) => {
            warns.push("只识别到价税合计，缺税额与不含税金额，请人工补齐后认证".to_string());
            (Money::ZERO, Money::ZERO, tt)
        }
        (None, None, None) => {
            return Err(FinError::msg(
                "未能识别出任何金额字段（价税合计/税额/不含税金额），请改用手工录入",
            ))
        }
    };

    // 价税合计应等于 不含税+税额；不一致只提醒，不改数。
    if !amount.is_zero() && !tax.is_zero() && amount_tax != amount + tax {
        warns.push(format!(
            "价税合计 {} 与「不含税 {} + 税额 {}」不一致，请核对票面",
            amount_tax.fmt_plain(),
            amount.fmt_plain(),
            tax.fmt_plain()
        ));
    }
    if amount_tax.is_negative() || amount.is_negative() || tax.is_negative() {
        return Err(FinError::msg("识别出负金额，票面异常，请手工录入核对"));
    }

    let kind = decide_kind(flat, company, &mut warns);
    let date = normalize_date(pick(flat, K_DATE).unwrap_or(""));
    if date.is_empty() {
        warns.push("开票日期未能识别，已留空，请补齐".to_string());
    }
    let invoice_type = pick(flat, K_TYPE).unwrap_or("电子普通发票").to_string();
    let seller = pick(flat, K_SELLER).unwrap_or("").to_string();
    let buyer = pick(flat, K_BUYER).unwrap_or("").to_string();
    if seller.is_empty() {
        warns.push("未识别到销售方名称，请补齐（影响往来核对）".to_string());
    }
    let item_summary = pick(flat, K_ITEM).unwrap_or("").to_string();

    let inv = Invoice {
        id: 0,
        kind,
        code: pick(flat, K_CODE).unwrap_or("").to_string(),
        number,
        date,
        buyer,
        seller,
        amount_tax,
        amount,
        tax,
        tax_rate: pick(flat, K_RATE)
            .map(|s| s.trim().trim_end_matches('%').to_string())
            .unwrap_or_default(),
        status: "pending".to_string(),
        memo: format!("识别导入（{invoice_type}）"),
        attach_id: 0,
        created_by: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
        parse: InvoiceParse {
            channel: channel.to_string(),
            raw_text: truncate_chars(raw_text, RAW_TEXT_LIMIT),
            engine: engine.to_string(),
            confidence: "1".to_string(),
            needs_review: false,
            buyer_tax_no: pick(flat, K_BUYER_TAX).unwrap_or("").to_string(),
            seller_tax_no: pick(flat, K_SELLER_TAX).unwrap_or("").to_string(),
            item_summary,
        },
    };

    Ok(ParsedInvoice {
        invoice: inv,
        channel: channel.to_string(),
        engine: engine.to_string(),
        confidence: "1".to_string(),
        needs_review: false,
        warnings: warns,
    })
}

const K_RATE: &[&str] = &["TaxRate", "taxRate", "tax_rate", "税率"];

fn truncate_chars(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    s.chars().take(limit).collect()
}

/// 从纯文本里用正则抓字段（也用于 PDF 文本层的二次抽取）。
fn flat_from_text(text: &str) -> Vec<(String, String)> {
    // (归一化键, 正则)。正则都用捕获组 1 取「值」。
    let specs: &[(&str, &str)] = &[
        (
            "发票号码",
            r"(?:发票号码|发票号|Invoice\s*Number|InvoiceNo)[:：\s]*([0-9]{8,20})",
        ),
        (
            "发票代码",
            r"(?:发票代码|Invoice\s*Code)[:：\s]*([0-9]{10,12})",
        ),
        (
            "开票日期",
            r"(?:开票日期|开具日期|Date)[:：\s]*([0-9]{4}\s*[年/\-\.]\s*[0-9]{1,2}\s*[月/\-\.]\s*[0-9]{1,2})",
        ),
        (
            "价税合计",
            r"(?:价税合计|含税金额|合计金额|小写|Total)[^0-9¥￥]{0,8}[¥￥]?\s*([0-9][0-9,]*\.?[0-9]{0,2})",
        ),
        (
            "税额",
            r"(?:税额|税金|Tax\s*Amount)[:：\s]*[¥￥]?\s*([0-9][0-9,]*\.?[0-9]{0,2})",
        ),
        (
            "不含税金额",
            r"(?:不含税金额|金额|Amount)[:：\s]*[¥￥]?\s*([0-9][0-9,]*\.?[0-9]{0,2})",
        ),
        (
            "销售方名称",
            r"(?:销售方名称|销方名称|销售方)[:：\s]*([\u4e00-\u9fa5A-Za-z0-9（）()·]{2,40})",
        ),
        (
            "购买方名称",
            r"(?:购买方名称|购方名称|购买方)[:：\s]*([\u4e00-\u9fa5A-Za-z0-9（）()·]{2,40})",
        ),
        (
            "货物名称",
            r"(?:货物或应税劳务、服务名称|货物名称|项目名称|品名)[:：\s]*([^\n\r]{1,60})",
        ),
    ];
    let mut out: Vec<(String, String)> = Vec::new();
    for (key, pat) in specs {
        if let Ok(re) = Regex::new(pat) {
            if let Some(cap) = re.captures(text) {
                if let Some(m) = cap.get(1) {
                    let v = m.as_str().trim().to_string();
                    if !v.is_empty() {
                        out.push((key.to_string(), v));
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// XML
// ---------------------------------------------------------------------------

/// 解析 XML：把每个「有文本的叶子元素」摊成 (元素名, 文本)，先中序遍历再拍平。
///
/// 电子发票 XML 的层级各省不同，但**叶子元素的标签名高度一致**（都叫
/// InvoiceNumber / 发票号码 之类），所以按标签名取值比按路径稳健得多。
fn flat_from_xml(raw: &[u8]) -> Result<Vec<(String, String)>, FinError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let text = std::str::from_utf8(raw).map_err(|_| FinError::msg("XML 不是合法的 UTF-8 编码"))?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    // 记录当前元素路径的标签栈
    let mut stack: Vec<String> = Vec::new();
    let mut out: Vec<(String, String)> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_string();
                stack.push(name);
            }
            Ok(Event::Text(t)) => {
                let decoded = t.xml10_content().into_owned();
                let val = quick_xml::escape::unescape(&decoded)
                    .map(|c| c.into_owned())
                    .unwrap_or(decoded);
                let val = val.trim();
                if !val.is_empty() {
                    // 用「父.子」路径 + 纯标签名各存一份，取字段时两种都能命中。
                    if let Some(tag) = stack.last() {
                        out.push((tag.clone(), val.to_string()));
                        if stack.len() >= 2 {
                            let path = stack.join(".");
                            out.push((path, val.to_string()));
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Ok(Event::Empty(e)) => {
                // 自闭合标签没有文本，忽略（属性里的一般不是我们要的字段）
                let _ = e;
            }
            Ok(_) => {}
            Err(e) => {
                return Err(FinError::msg(format!("XML 解析失败：{e}")));
            }
        }
    }
    if out.is_empty() {
        return Err(FinError::msg("XML 中没有可用的文本节点"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// JSON → 扁平 (路径, 值) 列表。递归拍平，数组用 `[i]` 标记。
fn flatten_json(value: &serde_json::Value, prefix: &str, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_json(v, &path, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let path = format!("{prefix}[{i}]");
                flatten_json(v, &path, out);
            }
        }
        serde_json::Value::String(s) => {
            if !s.trim().is_empty() {
                out.push((prefix.to_string(), s.clone()));
            }
        }
        serde_json::Value::Number(n) => {
            out.push((prefix.to_string(), n.to_string()));
        }
        serde_json::Value::Bool(b) => {
            out.push((prefix.to_string(), b.to_string()));
        }
        _ => {}
    }
}

fn flat_from_json(raw: &[u8]) -> Result<Vec<(String, String)>, FinError> {
    let v: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| FinError::msg(format!("JSON 解析失败：{e}")))?;
    let mut out = Vec::new();
    flatten_json(&v, "", &mut out);
    if out.is_empty() {
        return Err(FinError::msg("JSON 中没有可用的字段"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// PDF
// ---------------------------------------------------------------------------

/// 抽 PDF 文本层。抽出的文本为空或过短 → 明确报「疑似扫描件，无文本层」。
fn pdf_text_layer(raw: &[u8]) -> Result<String, FinError> {
    let text = pdf_extract::extract_text_from_mem(raw)
        .map_err(|e| FinError::msg(format!("PDF 文本层提取失败：{e}")))?;
    if text.trim().chars().count() < 10 {
        return Err(FinError::msg(
            "该 PDF 没有可用的文本层（疑似扫描件/图片版），请提供电子发票原文件或改用图片识别",
        ));
    }
    Ok(text)
}

// ---------------------------------------------------------------------------
// 图片：二维码优先
// ---------------------------------------------------------------------------

/// 二维码里编码的关键字段（税局标准载荷）。
#[derive(Clone, Debug, Default)]
struct QrPayload {
    invoice_code: String,
    invoice_no: String,
    amount: String,
    date: String,
    invoice_type: String,
}

/// 解析税局标准二维码文本载荷。
///
/// 标准格式（逗号分隔）：`版本,类型码,发票代码,发票号码,金额,开票日期,校验码`。
/// 兼容含/不含版本前缀两种排布；发票号码必须是纯数字，否则视为格式不符。
fn parse_qr_payload(payload: &str) -> Option<QrPayload> {
    if payload.trim().is_empty() {
        return None;
    }
    let cleaned = payload.trim().replace(['，', '\t'], ",");
    let parts: Vec<&str> = cleaned
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 5 {
        return None;
    }
    // 含版本前缀时前两位是 版本标识/类型码，整体后移两位。
    let offset = if parts[0] == "01" || parts[0] == "02" {
        2
    } else {
        0
    };
    if parts.len() < offset + 5 {
        return None;
    }
    let type_code = if offset > 0 { parts[offset - 1] } else { "" };
    let invoice_code = parts[offset].to_string();
    let invoice_no = parts[offset + 1].to_string();
    let amount = parts[offset + 2].to_string();
    let date = parts[offset + 3].to_string();
    if invoice_no.is_empty() || !invoice_no.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // 金额必须能解析，否则不是我们认识的发票码。
    // 用 Money::parse 而不是 parse::<f64>() —— 金额一律走 Decimal，
    // 既符合全仓「金额不得用浮点」的约定，也避免 0.1+0.2 这类二进制误差在
    // 后续「价税合计 vs 不含税+税额」比对里凭空造出一个差额。
    if Money::parse(&amount.replace(',', "")).is_err() {
        return None;
    }
    Some(QrPayload {
        invoice_code,
        invoice_no,
        amount,
        date,
        invoice_type: type_name(type_code).unwrap_or("增值税发票").to_string(),
    })
}

/// 从图片字节里解出第一个二维码文本。解码失败返回 None（不报错，交给上层降级）。
fn decode_qr_text(raw: &[u8]) -> Option<String> {
    // 1) 用 image 解码成 8 位灰度（我们只开了 png/jpeg/bmp/gif 四种格式）。
    let img = image::load_from_memory(raw).ok()?.to_luma8();
    let (w, h) = img.dimensions();
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return None;
    }
    // 2) 交给 rqrr 自己二值化并找码。像素来源用闭包按需取（不复制整幅到二次缓冲区）。
    let pixels = img.into_raw();
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| pixels[y * w + x]);
    let grids = prepared.detect_grids();
    for grid in grids {
        if let Ok((_meta, content)) = grid.decode() {
            let trimmed = content.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// 图片通道：二维码解码 → 组装草稿。二维码缺失/不识别 → 明确报错。
///
/// 本机没有离线 OCR 引擎（不引入 tesseract/ONNX），所以图片通道只走二维码。
/// 二维码只编码号码/金额/日期，缺品名与交易对手 → 一律标记 [`needs_review`]。
pub fn parse_image(raw: &[u8], company: &str) -> Result<ParsedInvoice, FinError> {
    let payload_text = match decode_qr_text(raw) {
        Some(t) => t,
        None => return Err(FinError::msg(
            "图片中未能识别出二维码（本机未安装离线 OCR 引擎），请提供电子发票 PDF/XML，或手工录入",
        )),
    };
    let qr = match parse_qr_payload(&payload_text) {
        Some(q) => q,
        None => {
            return Err(FinError::msg(
                "识别到二维码但内容不是标准发票载荷，请手工录入核对",
            ))
        }
    };
    let total =
        pick_money(Some(&qr.amount)).ok_or_else(|| FinError::msg("二维码金额字段无法解析"))?;
    if !total.is_positive() {
        return Err(FinError::msg("二维码金额为零或负数，票面异常"));
    }
    let date = normalize_date(&qr.date);
    let warns: Vec<String> =
        vec!["二维码只含号码/金额/日期，缺少购销双方与品名，请补齐后认证".to_string()];

    // 置信度：二维码命中即确定性解码，但缺交易对手/品名 → 一定需复核。
    // 与参照实现同口径：core 五要素里命中 2 项（号码+金额/日期），
    // engine 置信度取 1.0，综合 0.5*hit + 0.5*1.0。
    let core_hits = [true, !date.is_empty(), true, false, false]
        .iter()
        .filter(|b| **b)
        .count();
    let hit_ratio = core_hits as f64 / 5.0;
    let mut confidence = 0.5 * hit_ratio + 0.5 * 1.0;
    if hit_ratio >= 0.6 {
        confidence = confidence.max(0.9);
    }
    confidence = confidence.min(1.0);
    let needs_review = confidence < OCR_CONFIDENCE_THRESHOLD || true; // 缺交易对手，恒需复核
    let conf_str = format!("{confidence:.4}");

    let inv = Invoice {
        id: 0,
        kind: "in".to_string(),
        code: qr.invoice_code.clone(),
        number: qr.invoice_no.clone(),
        date,
        buyer: String::new(),
        seller: String::new(),
        amount_tax: total,
        amount: total,
        tax: Money::ZERO,
        tax_rate: String::new(),
        status: "pending".to_string(),
        memo: format!("二维码识别（{}）", qr.invoice_type),
        attach_id: 0,
        created_by: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
        parse: InvoiceParse {
            channel: "image".to_string(),
            raw_text: truncate_chars(&payload_text, RAW_TEXT_LIMIT),
            engine: "qr".to_string(),
            confidence: conf_str.clone(),
            needs_review,
            buyer_tax_no: String::new(),
            seller_tax_no: String::new(),
            item_summary: String::new(),
        },
    };
    let _ = company; // 图片通道没有购销信息，无法判方向，一律按进项登记
    Ok(ParsedInvoice {
        invoice: inv,
        channel: "image".to_string(),
        engine: "qr".to_string(),
        confidence: conf_str,
        needs_review,
        warnings: warns,
    })
}

// ---------------------------------------------------------------------------
// 统一入口
// ---------------------------------------------------------------------------

/// 按文件名扩展名选择通道解析。
///
/// `company` 是本企业名称，用于判断进项/销项；为空时按进项处理。
pub fn parse(filename: &str, raw: &[u8], company: &str) -> Result<ParsedInvoice, FinError> {
    let lower = filename.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");

    match ext {
        "xml" => {
            let flat = flat_from_xml(raw)?;
            let raw_text = String::from_utf8_lossy(raw).to_string();
            build_from_flat(&flat, company, "xml", "xml", &raw_text)
        }
        "json" => {
            let flat = flat_from_json(raw)?;
            let raw_text = String::from_utf8_lossy(raw).to_string();
            build_from_flat(&flat, company, "json", "json", &raw_text)
        }
        "pdf" => {
            let text = pdf_text_layer(raw)?;
            let flat = flat_from_text(&text);
            if flat.is_empty() {
                return Err(FinError::msg(
                    "PDF 有文本层但未匹配到发票字段，可能是非发票 PDF，请手工录入",
                ));
            }
            build_from_flat(&flat, company, "pdf", "pdf", &text)
        }
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" | "tif" | "tiff" => {
            parse_image(raw, company)
        }
        _ => {
            // 其它（txt / csv / 无扩展名）：把内容当纯文本走正则
            let text = String::from_utf8_lossy(raw).to_string();
            let flat = flat_from_text(&text);
            if flat.is_empty() {
                return Err(FinError::msg(
                    "文本中未匹配到发票字段，请确认文件内容或手工录入",
                ));
            }
            build_from_flat(&flat, company, "text", "text", &text)
        }
    }
}

/// 判断文件名是否是本模块支持的识别格式。
pub fn is_supported(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    matches!(
        ext,
        "xml"
            | "json"
            | "pdf"
            | "png"
            | "jpg"
            | "jpeg"
            | "bmp"
            | "gif"
            | "webp"
            | "tif"
            | "tiff"
            | "txt"
            | "csv"
    )
}

/// 供上层构造 `Cursor`（PDF 抽取内部也可能用到，这里导出以便测试复用）。
pub fn _cursor(raw: &[u8]) -> Cursor<&[u8]> {
    Cursor::new(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_payload_with_version_prefix() {
        let p =
            parse_qr_payload("01,10,044031900111,12345678,1000.00,20200101,12345678901234567890,")
                .unwrap();
        assert_eq!(p.invoice_code, "044031900111");
        assert_eq!(p.invoice_no, "12345678");
        assert_eq!(p.amount, "1000.00");
        assert_eq!(p.date, "20200101");
        assert_eq!(p.invoice_type, "增值税普通发票(电子)");
    }

    #[test]
    fn qr_payload_without_version_prefix() {
        let p = parse_qr_payload("044031900111,12345678,88.50,20240315,CHECKCODE").unwrap();
        assert_eq!(p.invoice_no, "12345678");
        assert_eq!(p.amount, "88.50");
    }

    #[test]
    fn qr_payload_rejects_garbage() {
        assert!(parse_qr_payload("").is_none());
        assert!(parse_qr_payload("hello world").is_none());
        // 号码非数字
        assert!(parse_qr_payload("01,10,044031900111,ABCDE,100.00,20200101,CHK").is_none());
    }

    #[test]
    fn normalize_date_variants() {
        assert_eq!(normalize_date("20200101"), "2020-01-01");
        assert_eq!(normalize_date("2026-3-5"), "2026-03-05");
        assert_eq!(normalize_date("2026/12/31"), "2026-12-31");
        assert_eq!(normalize_date(""), "");
        assert_eq!(normalize_date("乱码"), "");
    }

    #[test]
    fn json_invoice_parsed() {
        let json = r#"{"InvoiceNumber":"12345678","InvoiceCode":"044001900111",
            "InvoiceDate":"2026-03-10","SellerName":"乙公司","BuyerName":"甲公司",
            "TotalAmount":"11300.00","TaxAmount":"1300.00","AmountWithoutTax":"10000.00",
            "ItemName":"钢材"}"#;
        let p = parse("a.json", json.as_bytes(), "乙公司").unwrap();
        assert_eq!(p.invoice.number, "12345678");
        assert_eq!(p.invoice.kind, "out", "销售方是本企业 → 销项");
        assert_eq!(p.invoice.amount_tax.fmt_plain(), "11300.00");
        assert_eq!(p.invoice.amount.fmt_plain(), "10000.00");
        assert_eq!(p.invoice.tax.fmt_plain(), "1300.00");
        assert_eq!(p.invoice.date, "2026-03-10");
        assert_eq!(p.invoice.parse.engine, "json");
    }

    #[test]
    fn json_derives_amount_from_total_minus_tax() {
        let json = br#"{"invoice_no":"99990001","total_amount":"106.00","tax_amount":"6.00"}"#;
        let p = parse("a.json", json, "").unwrap();
        // 缺不含税金额 → 用 价税合计 - 税额 推出 100.00
        assert_eq!(p.invoice.amount.fmt_plain(), "100.00");
    }

    #[test]
    fn missing_number_is_rejected() {
        let json = br#"{"TotalAmount":"11300.00","TaxAmount":"1300.00"}"#;
        let err = parse("a.json", json, "").unwrap_err();
        assert!(err.to_string().contains("发票号码"), "{}", err);
    }

    #[test]
    fn xml_invoice_parsed() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <Invoice>
          <InvoiceNumber>87654321</InvoiceNumber>
          <InvoiceCode>044031900111</InvoiceCode>
          <InvoiceDate>2026-05-06</InvoiceDate>
          <SellerName>甲方商贸</SellerName>
          <BuyerName>我方公司</BuyerName>
          <TotalAmount>565.00</TotalAmount>
          <TaxAmount>65.00</TaxAmount>
          <AmountWithoutTax>500.00</AmountWithoutTax>
          <ItemName>办公用品</ItemName>
        </Invoice>"#;
        let p = parse("inv.xml", xml.as_bytes(), "我方公司").unwrap();
        assert_eq!(p.invoice.number, "87654321");
        assert_eq!(p.invoice.kind, "in", "购买方是本企业 → 进项");
        assert_eq!(p.invoice.amount_tax.fmt_plain(), "565.00");
        assert_eq!(p.invoice.seller, "甲方商贸");
    }

    #[test]
    fn text_channel_extracts_fields() {
        let text = "增值税电子普通发票\n发票号码：12345678\n开票日期：2026年03月10日\n\
                    价税合计：￥11300.00\n税额：1300.00\n销售方名称：乙公司\n货物名称：钢材";
        let flat = flat_from_text(text);
        let p = build_from_flat(&flat, "", "text", "text", text).unwrap();
        assert_eq!(p.invoice.number, "12345678");
        assert_eq!(p.invoice.date, "2026-03-10");
        assert_eq!(p.invoice.amount_tax.fmt_plain(), "11300.00");
        assert_eq!(p.invoice.seller, "乙公司");
    }

    #[test]
    fn total_mismatch_only_warns_not_fixed() {
        let json = br#"{"invoice_no":"11112222","total_amount":"11300.00","tax_amount":"1300.00","amount":"9000.00"}"#;
        let p = parse("a.json", json, "").unwrap();
        // 9000 + 1300 != 11300 → 只提醒，不改数
        assert_eq!(p.invoice.amount.fmt_plain(), "9000.00");
        assert!(p.warnings.iter().any(|w| w.contains("不一致")));
    }

    #[test]
    fn supported_extensions() {
        assert!(is_supported("a.pdf"));
        assert!(is_supported("a.XML"));
        assert!(is_supported("a.png"));
        assert!(!is_supported("a.docx"));
    }
}
