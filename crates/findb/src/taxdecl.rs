//! 税务申报表取数（增值税一般纳税人）
//!
//! ## 边界：不连网，只出「可手工申报」的数
//!
//! 本模块只做**取数与算表**，不做发票查验、不做电子税务局网报、不碰税控设备。
//! 产出的表与税局纸质申报表同构（行次、栏次一致），会计照着填即可。
//!
//! 这条边界是刻意的：网报要 CA 证书、税控盘（或全电发票的电子签名），是另一套
//! 基础设施，不该混在业务系统里。而「把数算对、把数给全、把数说得出来源」才是
//! ERP 该干的活——最后那一条尤其重要：税务局问「这期销项为什么比收入表多 3%」，
//! 系统要能当场答上来。
//!
//! ## 数据来源与口径（逐条列明，不留「大概是这个数」）
//!
//! | 报表 | 来源 | 口径 |
//! |---|---|---|
//! | 销项（附列一） | `invoice` kind=`out` | 排除 `rejected`；**含 `pending`** |
//! | 进项（附列二） | `invoice` kind=`in` | **只取 `verified`** |
//! | 附加税费 | 主表应纳税额 × 费率 | 费率来自账套税务设置 |
//!
//! 销项含 `pending` 而进项只取 `verified`，不是不对称，是两个方向的法定口径不同：
//! 销项义务在**开票时**发生，「待认证」是**进项方**的概念；进项抵扣则必须先
//! 认证。把 pending 计入进项就是让企业多缴税甚至被追缴。
//!
//! 两个刻意的「不自动算」：
//!
//! - **未开票收入不并入销项**。视同销售等情形要按会计口径逐笔认定，程序猜不出来。
//!   申报表里留独立行并提示人工填——比算一个数让会计以为是准的更安全。
//! - **进项税额转出（附列资料三）不做**。转出要按每张发票的**实际用途**逐张判定
//!   （集体福利/个人消费/免税项目…），靠科目和摘要猜必然出错，而报错方向是多缴税
//!   或被追缴。留手工填。
//!
//! ## 税率分档
//!
//! 申报表按**征收率**分栏，不能把 5% 征收率混进一般计税的 6% 档——两者在主表
//! 第 1 行（一般计税销售额）与第 3 行（简易计税销售额）分列。通行对应：
//! 13%/9%/6% 为一般计税税率；5%/3% 为简易计税征收率；0 为免税。

use std::collections::BTreeMap;

use fincore::Money;

use crate::{Db, DbResult};

/// 发票状态：已作废（不进任何申报数）
pub const ST_REJECTED: &str = "rejected";
/// 发票状态：已认证（进项可抵扣的必要条件）
pub const ST_VERIFIED: &str = "verified";

/// 账套税务设置（存 `meta` 表，不进 BookOptions）
///
/// 刻意放 meta 而不进 `BookOptions`：后者是**建账时**的参数集，而税务设置是
/// **期后**要按期调整的（附加税率、纳税人身份认定结果都可能变）。混在一起会让
/// 「建账参数」变成一张什么都往里塞的表。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaxOptions {
    /// general=一般纳税人 / small=小规模纳税人
    pub taxpayer: String,
    /// 城建税适用地区：city(市区 7%) / county(县城、镇 5%) / other(其他 1%)
    pub city_tax_zone: String,
    /// 教育费附加
    pub edu_rate: String,
    /// 地方教育附加
    pub local_edu_rate: String,
}

impl Default for TaxOptions {
    fn default() -> Self {
        Self {
            taxpayer: "general".into(),
            city_tax_zone: "city".into(),
            edu_rate: "0.03".into(),
            local_edu_rate: "0.02".into(),
        }
    }
}

impl TaxOptions {
    pub fn load(db: &Db) -> DbResult<Self> {
        let get = |k: &str| -> Option<String> {
            use rusqlite::OptionalExtension;
            db.conn()
                .query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0))
                .optional()
                .ok()
                .flatten()
        };
        let d = Self::default();
        Ok(Self {
            taxpayer: get("tax_taxpayer").unwrap_or(d.taxpayer),
            city_tax_zone: get("tax_city_zone").unwrap_or(d.city_tax_zone),
            edu_rate: get("tax_edu_rate").unwrap_or(d.edu_rate),
            local_edu_rate: get("tax_local_edu_rate").unwrap_or(d.local_edu_rate),
        })
    }

    pub fn save(&self, db: &Db) -> DbResult<()> {
        for (k, v) in [
            ("tax_taxpayer", &self.taxpayer),
            ("tax_city_zone", &self.city_tax_zone),
            ("tax_edu_rate", &self.edu_rate),
            ("tax_local_edu_rate", &self.local_edu_rate),
        ] {
            db.conn()
                .execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)", [k, v])?;
        }
        Ok(())
    }

    /// 城建税率：市区 7% / 县城镇 5% / 其他 1%
    ///
    /// 固定三档、不给自定义：城建税是法定税率，纳税人只能按适用地区选，不能自选
    /// 税率。给个自由输入框等于让会计填错、然后补税。
    pub fn city_tax_rate(&self) -> Money {
        Money::parse(match self.city_tax_zone.as_str() {
            "county" => "0.05",
            "other" => "0.01",
            _ => "0.07",
        })
        .unwrap_or(Money::ZERO)
    }
}

/// 申报日历的一条提醒（纯日期逻辑，不读账套状态）。
#[derive(Clone, Debug)]
pub struct TaxTodo {
    pub key: &'static str,
    pub label: String,
    pub detail: String,
}

/// 申报日历：按今天算出"正在申报期"的事项。
///
/// 只报窗口期内（申报期之外不打扰）：每月 1–15 日是增值税/个税的申报期，
/// 1/4/7/10 月多一个企业所得税预缴，1–5 月是汇算清缴，1–6 月是工商年报。
/// 小规模增值税按季（1/4/7/10 月才报），一般纳税人按月 —— 报税期写错比
/// 不提醒更糟（会计白跑一趟税局）。
pub fn deadline_todos(today: chrono::NaiveDate, taxpayer: &str) -> Vec<TaxTodo> {
    use chrono::Datelike;
    let (m, d) = (today.month(), today.day());
    let small = taxpayer.trim() == "small";
    let quarter_month = matches!(m, 1 | 4 | 7 | 10);
    let mut out = Vec::new();
    if d <= 15 && (!small || quarter_month) {
        out.push(TaxTodo {
            key: "tax_vat",
            label: "增值税及附加税费申报（本月15日前）".to_string(),
            detail: if small {
                "小规模按季申报：查科目余额表 222102，未交多少报多少".to_string()
            } else {
                "一般纳税人按月申报：销项－进项＝应缴，税务申报页自动算表".to_string()
            },
        });
    }
    if d <= 15 {
        out.push(TaxTodo {
            key: "tax_iit",
            label: "个人所得税（工资薪金）申报（本月15日前）".to_string(),
            detail: "业务 → 工资 → 个税明细：本期应扣多少报多少".to_string(),
        });
    }
    if d <= 15 && quarter_month {
        out.push(TaxTodo {
            key: "tax_cit",
            label: "企业所得税预缴申报（本月15日前）".to_string(),
            detail: "按本年累计利润预缴，季度初的三个月".to_string(),
        });
    }
    if m <= 5 {
        out.push(TaxTodo {
            key: "tax_final",
            label: "企业所得税汇算清缴（5月31日前）".to_string(),
            detail: "上年度全年算总账，多退少补".to_string(),
        });
    }
    if m <= 6 {
        out.push(TaxTodo {
            key: "tax_annual",
            label: "工商年报（6月30日前）".to_string(),
            detail: "国家企业信用信息公示系统填报".to_string(),
        });
    }
    out
}

/// 税率规范化：`0.13` / `13%` / `13` / `13.0` → `13`
///
/// 不规范化就没法分档——`0.13` 与 `13%` 是同一税率却会落进两个桶。实际数据里两种
/// 写法都出现过（业务手工录入 vs 代码生成）。
///
/// **不能一律 ×100**：库里 `tax_rate` 既存小数（`0.13`）也存百分数（`13`），
/// 统一乘 100 会把 `13` 变成 `1300`，13% 的票落进一个不存在的档里、悄悄不进
/// 任何合计。所以按**量级**判：小于 1 视为小数、乘 100；否则已是百分数、原样取整。
pub fn normalize_rate(rate: &str) -> String {
    let r = rate.trim().trim_end_matches('%').trim();
    match r.parse::<f64>() {
        // round 消掉浮点尾数（0.13*100 = 13.000000000000002）
        Ok(v) => {
            let pct = if v.abs() < 1.0 { v * 100.0 } else { v };
            pct.round() as i64
        }
        .to_string(),
        // 解析不了就原样返回：悄悄变成 0 会被当成免税档，比报错更危险
        Err(_) => r.to_string(),
    }
}

/// 税率是否属一般计税（13/9/6）；5/3 是简易计税征收率，0 是免税
pub fn is_general_rate(rate: &str) -> bool {
    matches!(normalize_rate(rate).as_str(), "13" | "9" | "6")
}

/// 税率是否**可识别**（落在法定档位里）
///
/// 刻意与 `is_general_rate` 分开：不可识别的税率**不能**默认归到简易计税。
/// `push_from_so` / `push_from_po` 下推的发票 `tax_rate` 就是**空串**（订单行
/// 没存税率），若按「非一般计税即简易计税」处理，一张真实的销项发票会落进
/// 简易计税销售额 —— 主表第 1 行和第 3 行都填错，而申报表上看起来完全正常。
pub fn is_known_rate(rate: &str) -> bool {
    matches!(normalize_rate(rate).as_str(), "13" | "9" | "6" | "5" | "3" | "0")
}

/// 一个税率档的汇总
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RateBucket {
    /// 档位：13 / 9 / 6 / 5 / 3 / 0
    pub rate: String,
    pub count: i64,
    pub net: Money,
    pub tax: Money,
    pub gross: Money,
    /// true=一般计税税率，false=简易计税征收率或免税
    pub general_method: bool,
}

/// 申报表的一张分档表（附列资料一 / 二）
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct DetailTable {
    pub title: String,
    pub buckets: Vec<RateBucket>,
    pub net_total: Money,
    pub tax_total: Money,
    pub gross_total: Money,
    pub count: i64,
}

/// 附加税费
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Surcharge {
    /// 应纳税额（主表应纳税额行）
    pub vat_payable: Money,
    pub city_tax: Money,
    pub edu: Money,
    pub local_edu: Money,
    pub total: Money,
    pub city_rate: String,
    pub edu_rate: String,
    pub local_edu_rate: String,
}

/// 增值税一般纳税人申报表（主表 + 附一 + 附二 + 附加税费）
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct VatMainForm {
    pub period: String,
    /// 一、销项税额
    /// 一般计税方法计税销售额
    pub sales_general: Money,
    /// 简易计税方法计税销售额
    pub sales_simple: Money,
    /// 免征增值税销售额
    pub sales_exempt: Money,
    /// 税率未填/不可识别的销售额（**不进上面三行**，须人工归类）
    pub sales_unknown: Money,
    /// 上述部分对应的销项税额
    pub unknown_tax: Money,
    /// 销项税额
    pub output_tax: Money,
    /// 二、进项税额
    /// 本期认证相符的进项税额
    pub input_tax: Money,
    /// 应纳税额 = 销项 - 进项
    pub payable: Money,
    /// 未开票销售额（**程序不算，留表上人工填**）
    pub unbilled_sales: Money,
    /// 未开票收入销项税额（人工填）
    pub unbilled_tax: Money,
    pub details_out: DetailTable,
    pub details_in: DetailTable,
    pub surcharge: Surcharge,
    /// 数据完整性提示。非空就说明本期数据有洞或口径有边界，UI 必须显示
    pub warnings: Vec<String>,
}

/// 期间内的发票
#[derive(Clone)]
struct RawInv {
    date: String,
    number: String,
    /// 销项=购买方（客户），进项=销售方（供应商）
    party: String,
    rate: String,
    net: Money,
    tax: Money,
    gross: Money,
    status: String,
    memo: String,
}

impl RawInv {
    fn usable(&self) -> bool {
        self.status != ST_REJECTED
    }
}

/// 取期间内的发票
///
/// `date` 是 `YYYY-MM-DD` 文本，**不能**直接 `substr(date,1,6)` —— 那样取到的是
/// `"2026-0"` 而不是 `"202601"`，一条都匹配不上，申报表会安静地全 0。先剥分隔符
/// 再取前 6 位；`/` 分隔也一并处理（历史导入数据里两种都出现过）。
fn invoices_in_period(db: &Db, kind: &str, period: fincore::Period) -> DbResult<Vec<RawInv>> {
    let party_col = if kind == "out" { "buyer" } else { "seller" };
    let sql = format!(
        "SELECT date, number, {party_col} AS party, tax_rate, amount, tax, amount_tax, status, memo
         FROM invoice
         WHERE kind=?1 AND substr(replace(replace(date,'-',''),'/',''),1,6)=?2
         ORDER BY date, number"
    );
    let mut st = db.conn().prepare(&sql)?;
    let rows = st.query_map(
        rusqlite::params![kind, period.ymm().to_string()],
        |r| {
            Ok(RawInv {
                date: r.get(0)?,
                number: r.get(1)?,
                party: r.get(2)?,
                rate: r.get(3)?,
                net: Money::parse_or_zero(&r.get::<_, String>(4)?),
                tax: Money::parse_or_zero(&r.get::<_, String>(5)?),
                gross: Money::parse_or_zero(&r.get::<_, String>(6)?),
                status: r.get(7)?,
                memo: r.get(8)?,
            })
        },
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 档位排序：法定顺序（13→9→6→5→3→0），不是字符串字典序
fn rate_rank(rate: &str) -> (u8, i64) {
    let v: i64 = rate.parse().unwrap_or(999);
    (0, -v)
}

fn bucketize(list: &[RawInv], title: &str) -> DetailTable {
    let mut map: BTreeMap<String, RateBucket> = BTreeMap::new();
    for inv in list {
        if !inv.usable() {
            continue;
        }
        let key = normalize_rate(&inv.rate);
        let b = map
            .entry(key.clone())
            .or_insert_with(|| RateBucket {
                rate: key.clone(),
                general_method: is_general_rate(&key),
                ..Default::default()
            });
        b.count += 1;
        b.net += inv.net;
        b.tax += inv.tax;
        b.gross += inv.gross;
    }
    let mut buckets: Vec<RateBucket> = map.into_values().collect();
    buckets.sort_by(|a, b| rate_rank(&a.rate).cmp(&rate_rank(&b.rate)));
    let mut t = DetailTable {
        title: title.into(),
        buckets,
        ..Default::default()
    };
    for b in &t.buckets {
        t.net_total += b.net;
        t.tax_total += b.tax;
        t.gross_total += b.gross;
        t.count += b.count;
    }
    t
}

/// 生成增值税一般纳税人申报表
pub fn vat_main_form(db: &Db, period: fincore::Period) -> DbResult<VatMainForm> {
    let opts = TaxOptions::load(db)?;

    let out_all = invoices_in_period(db, "out", period)?;
    let out_rejected = out_all.iter().filter(|i| !i.usable()).count() as i64;
    let details_out = bucketize(&out_all, "附列资料一：应税货物和劳务销项明细");

    let in_all = invoices_in_period(db, "in", period)?;
    let in_rejected = in_all.iter().filter(|i| !i.usable()).count() as i64;
    let in_verified: Vec<RawInv> = in_all
        .iter()
        .filter(|i| i.usable() && i.status == ST_VERIFIED)
        .cloned()
        .collect();
    let in_pending = in_all.len() as i64 - in_rejected - in_verified.len() as i64;
    let details_in = bucketize(&in_verified, "附列资料二：进项税额明细");

    // 主表三行销项：一般计税 / 简易计税 / 免税
    //
    // 不可识别的税率（空串、`免税` 这类非数字）**既不算一般计税、也不算简易计税**，
    // 单列成「待归类」。默认归到简易计税会让主表第 3 行凭空多出一块，而表面上
    // 数字看着完全正常 —— 这种错要等到税局退表才发现。
    let (mut sales_general, mut sales_simple, mut sales_exempt) = (Money::ZERO, Money::ZERO, Money::ZERO);
    let (mut sales_unknown, mut unknown_tax) = (Money::ZERO, Money::ZERO);
    for b in &details_out.buckets {
        if !is_known_rate(&b.rate) {
            sales_unknown += b.net;
            unknown_tax += b.tax;
            continue;
        }
        if b.general_method {
            sales_general += b.net;
        } else if b.rate == "0" {
            sales_exempt += b.net;
        } else {
            sales_simple += b.net;
        }
    }

    let output_tax = details_out.tax_total;
    let input_tax = details_in.tax_total;
    let payable = output_tax - input_tax;

    let city_rate = opts.city_tax_rate();
    // 附加税费按**分**取整（round2），不能保留乘出来的小数位。
    //
    // 130 × 5% 在 Decimal 里是 6.5000（4 位），而销项是 2 位——同一张申报表上
    // 两种小数位，会计看着别扭、税务机关录入系统也会按 2 位截。取整必须在这里做，
    // 越晚做越难说清「到底哪一位是算出来的」。
    let mut surcharge = Surcharge {
        vat_payable: payable,
        city_tax: (payable * city_rate).round2(),
        edu: (payable * Money::parse_or_zero(&opts.edu_rate)).round2(),
        local_edu: (payable * Money::parse_or_zero(&opts.local_edu_rate)).round2(),
        city_rate: city_rate.fmt_money(),
        edu_rate: Money::parse_or_zero(&opts.edu_rate).fmt_money(),
        local_edu_rate: Money::parse_or_zero(&opts.local_edu_rate).fmt_money(),
        ..Default::default()
    };
    surcharge.total =
        (surcharge.city_tax + surcharge.edu + surcharge.local_edu).round2();

    let mut warnings: Vec<String> = Vec::new();
    if in_pending > 0 {
        warnings.push(format!(
            "本期有 {in_pending} 张进项发票尚未认证，**未计入**进项税额。认证后需重算本期申报表"
        ));
    }
    if out_rejected > 0 {
        warnings.push(format!("本期有 {out_rejected} 张销项发票已作废，已排除"));
    }
    if in_rejected > 0 {
        warnings.push(format!("本期有 {in_rejected} 张进项发票已作废，已排除"));
    }
    if details_out.count == 0 {
        warnings.push(
            "本期没有销项发票。**未开票收入不自动并入销项**——视同销售等情形请在下方手工填入"
                .to_string(),
        );
    }
    if !sales_unknown.is_zero() || !unknown_tax.is_zero() {
        let rates: Vec<&str> = details_out
            .buckets
            .iter()
            .filter(|b| !is_known_rate(&b.rate))
            .map(|b| b.rate.as_str())
            .collect();
        warnings.push(format!(
            "有税率未填/不可识别的销项发票（税率档：{}），销售额 {} / 税额 {} **未计入上面三行**。\
             订单下推的发票默认不带税率，请到发票管理补全后重算",
            if rates.is_empty() { "（空）".to_string() } else { rates.join("、") },
            sales_unknown.fmt_money(),
            unknown_tax.fmt_money()
        ));
    }
    if payable.is_negative() {
        warnings.push(
            "进项大于销项，本表应纳税额为负，按规定形成留抵税额，须结转下期抵扣——\
             留抵额度与结转由人工填报"
                .to_string(),
        );
    }
    if opts.taxpayer != "general" {
        warnings.push(format!(
            "账套纳税人身份为「{}」，非一般纳税人，本表按一般纳税人表式生成，口径可能不适用",
            opts.taxpayer
        ));
    }
    warnings.push(
        "本期留抵税额、附加税费减免、进项税额转出（附列资料三）、不动产扣除均由人工填报，系统不代算"
            .to_string(),
    );

    Ok(VatMainForm {
        period: period.to_string(),
        sales_general,
        sales_simple,
        sales_exempt,
        sales_unknown,
        unknown_tax,
        output_tax,
        input_tax,
        payable,
        // 未开票收入与销项留空由人工填（见上方「不自动算」的理由）
        unbilled_sales: Money::ZERO,
        unbilled_tax: Money::ZERO,
        details_out,
        details_in,
        surcharge,
        warnings,
    })
}

/// 构成一张发票明细（可追溯：每个汇总数都要能答出「由哪几张发票构成」）
///
/// `kind` = `out` 查销项、`in` 查进项；进项只返回已认证的。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct BucketInvoice {
    pub date: String,
    pub number: String,
    pub party: String,
    pub rate: String,
    pub net: String,
    pub tax: String,
    pub gross: String,
    pub memo: String,
}

pub fn bucket_detail(
    db: &Db,
    kind: &str,
    period: fincore::Period,
    rate: &str,
) -> DbResult<Vec<BucketInvoice>> {
    let want = normalize_rate(rate);
    let list = invoices_in_period(db, kind, period)?;
    let mut out = Vec::new();
    for inv in list {
        if !inv.usable() || normalize_rate(&inv.rate) != want {
            continue;
        }
        if kind == "in" && inv.status != ST_VERIFIED {
            continue;
        }
        out.push(BucketInvoice {
            date: inv.date,
            number: inv.number,
            party: inv.party,
            rate: want.clone(),
            net: inv.net.fmt_money(),
            tax: inv.tax.fmt_money(),
            gross: inv.gross.fmt_money(),
            memo: inv.memo,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoices::Invoice;
    use crate::tests::mem;

    fn add_inv(
        db: &Db,
        kind: &str,
        date: &str,
        rate: &str,
        net: &str,
        tax: &str,
        status: &str,
    ) -> i64 {
        let net = Money::parse(net).unwrap();
        let tax = Money::parse(tax).unwrap();
        // 单号有 UNIQUE 约束，用行数当后缀即可（不引 rand，免得为一个后缀加依赖）
        let seq: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM invoice", [], |r| r.get::<_, i64>(0))
            .unwrap()
            + 1;
        crate::invoices::insert(
            db,
            &Invoice {
                id: 0,
                kind: kind.into(),
                code: "044001900111".into(),
                number: format!("FP{seq:06}"),
                date: date.into(),
                buyer: if kind == "out" { "客户甲".into() } else { "本企业".into() },
                seller: if kind == "out" { "本企业".into() } else { "供应商甲".into() },
                amount_tax: net + tax,
                amount: net,
                tax,
                tax_rate: rate.into(),
                status: status.into(),
                memo: String::new(),
                attach_id: 0,
                created_by: "测试".into(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            "测试",
        )
        .unwrap()
    }

    /// 税率写法必须归一：`0.13` / `13%` / `13` 是同一档
    ///
    /// 关键是**不能一律 ×100**：库里 `tax_rate` 既存小数（`0.13`）也存百分数
    /// （`13`）。若统一乘 100，`13` 会变成 `1300`，13% 的票落进一个不存在的档、
    /// 悄悄不进任何合计——申报表上就是一个查不出原因的少一块。
    #[test]
    fn rate_normalization_groups_equivalent_notations() {
        for s in ["0.13", "13%", "13", "13.0", " 0.13 ", "0.130"] {
            assert_eq!(normalize_rate(s), "13", "{s} 应归一到 13");
        }
        for s in ["0.06", "6%", "6", "6.0"] {
            assert_eq!(normalize_rate(s), "6", "{s} 应归一到 6");
        }
        for s in ["0.09", "9%", "9"] {
            assert_eq!(normalize_rate(s), "9", "{s} 应归一到 9");
        }
        for s in ["0.05", "5%", "5"] {
            assert_eq!(normalize_rate(s), "5", "{s} 应归一到 5");
        }
        assert_eq!(normalize_rate("0"), "0");
        // 解析不了的原样返回，不能悄悄变成 0 然后被当成免税档
        assert_eq!(normalize_rate("免税"), "免税");
    }

    /// 税率未填/不可识别的销项**不能**默认归到简易计税
    ///
    /// 回归背景：`push_from_so` / `push_from_po` 下推的发票 `tax_rate` 是**空串**
    /// （订单行不存税率）。按「非一般计税即简易计税」处理，一张真实的销项发票会
    /// 落进简易计税销售额 —— 主表第 3 行凭空多一块，而表面上数字完全正常，
    /// 要等税局退表才发现。
    #[test]
    fn unknown_rate_is_not_silently_simple_taxed() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        // 空税率（订单下推的默认状态）
        add_inv(&db, "out", "2026-01-05", "", "1000", "130", "verified");
        add_inv(&db, "out", "2026-01-06", "免税", "500", "0", "verified");
        // 一张正常的 13%
        add_inv(&db, "out", "2026-01-07", "0.13", "2000", "260", "verified");

        let f = vat_main_form(&db, p).unwrap();
        assert_eq!(f.sales_general.fmt_money(), "2,000.00", "13% 档该算一般计税");
        assert_eq!(
            f.sales_simple.fmt_money(),
            "0.00",
            "空税率/免税字样绝不能被当成简易计税：{}",
            f.sales_simple.fmt_money()
        );
        assert_eq!(f.sales_unknown.fmt_money(), "1,500.00", "未识别的应单列");
        assert_eq!(f.unknown_tax.fmt_money(), "130.00");
        // 销项税额总额仍要包含它们（只是不进三行分类）
        assert_eq!(f.output_tax.fmt_money(), "390.00");
        assert!(
            f.warnings.iter().any(|w| w.contains("未填") || w.contains("不可识别")),
            "必须提示税率缺失，否则会计以为分类完了：{:?}",
            f.warnings
        );
    }

    /// 一般计税 vs 简易计税征收率必须分开
    #[test]
    fn general_rate_classification() {
        for r in ["13", "0.13", "9%", "6"] {
            assert!(is_general_rate(r), "{r} 应算一般计税");
        }
        for r in ["5", "3", "0.05", "0"] {
            assert!(!is_general_rate(r), "{r} 不该算一般计税");
        }
        // 可识别性是另一回事：5/3/0 是法定档位，只是不是一般计税
        for r in ["13", "9", "6", "5", "3", "0", "0.13"] {
            assert!(is_known_rate(r), "{r} 是法定档位，应可识别");
        }
        // 空串与非数字不可识别
        for r in ["", "  ", "免税", "unknown"] {
            assert!(!is_known_rate(r), "{r:?} 不该被当成法定档位");
        }
    }

    /// 进项只认已认证：pending 的进项算进来就是让企业多缴税
    #[test]
    fn input_tax_only_counts_verified() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        add_inv(&db, "in", "2026-01-10", "0.13", "1000", "130", "verified");
        add_inv(&db, "in", "2026-01-11", "0.13", "500", "65", "pending");
        add_inv(&db, "in", "2026-01-12", "0.13", "300", "39", "rejected");

        let f = vat_main_form(&db, p).unwrap();
        assert_eq!(
            f.input_tax.to_string(),
            Money::parse("130").unwrap().to_string(),
            "只应计已认证的 130（当前 {}）",
            f.input_tax.fmt_money()
        );
        assert!(
            f.warnings.iter().any(|w| w.contains("尚未认证")),
            "未认证的发票必须在提示里说出来，否则会计以为数算全了：{:?}",
            f.warnings
        );
    }

    /// 销项含 pending：开票了销项义务就发生，「待认证」是进项方的概念
    #[test]
    fn output_tax_includes_pending() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        add_inv(&db, "out", "2026-01-10", "0.13", "1000", "130", "pending");
        add_inv(&db, "out", "2026-01-11", "0.13", "400", "52", "rejected");

        let f = vat_main_form(&db, p).unwrap();
        assert_eq!(
            f.output_tax.to_string(),
            Money::parse("130").unwrap().to_string(),
            "pending 的销项应计入，rejected 应排除（当前 {}）",
            f.output_tax.fmt_money()
        );
    }

    /// 税率分档 + 主表三行销售额：5% 简易计税不能混进一般计税销售额
    #[test]
    fn buckets_and_main_form_split_simple_vs_general() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        add_inv(&db, "out", "2026-01-05", "0.13", "1000", "130", "verified");
        add_inv(&db, "out", "2026-01-06", "0.13", "500", "65", "verified");
        add_inv(&db, "out", "2026-01-07", "0.06", "200", "12", "verified");
        add_inv(&db, "out", "2026-01-08", "0.05", "80", "4", "verified");
        add_inv(&db, "out", "2026-01-09", "0", "50", "0", "verified");

        let f = vat_main_form(&db, p).unwrap();
        // 一般计税 = 13% 档 1500 + 6% 档 200
        // 断言用 fmt_money（带千分位，申报表上就是千分位），字符串里也要写千分位
        assert_eq!(f.sales_general.fmt_money(), "1,700.00");
        // 简易计税 = 5% 档 80
        assert_eq!(f.sales_simple.fmt_money(), "80.00");
        // 免税 = 0% 档 50
        assert_eq!(f.sales_exempt.fmt_money(), "50.00");
        // 销项税额 = 130+65+12+4
        assert_eq!(f.output_tax.fmt_money(), "211.00");
        // 档位顺序必须是法定顺序
        let rates: Vec<&str> = f.details_out.buckets.iter().map(|b| b.rate.as_str()).collect();
        assert_eq!(rates, vec!["13", "6", "5", "0"], "档位顺序 {rates:?}");
    }

    /// 应纳税额与附加税费：市区 7% + 教育 3% + 地方教育 2% = 12%
    ///
    /// 顺带守住小数位：附加税费必须与销项一样是 2 位。130 × 5% 在 Decimal 里是
    /// 6.5000（4 位），同一张表上两种小数位，会计看着别扭、税务机关录入也按 2 位截。
    #[test]
    fn payable_and_surcharge() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        add_inv(&db, "out", "2026-01-05", "0.13", "1000", "130", "verified");
        add_inv(&db, "in", "2026-01-05", "0.13", "500", "65", "verified");

        let f = vat_main_form(&db, p).unwrap();
        assert_eq!(f.payable.fmt_money(), "65.00");
        assert_eq!(f.surcharge.city_tax.fmt_money(), "4.55"); // 65 × 7%
        assert_eq!(f.surcharge.edu.fmt_money(), "1.95"); // 65 × 3%
        assert_eq!(f.surcharge.local_edu.fmt_money(), "1.30"); // 65 × 2%
        assert_eq!(f.surcharge.total.fmt_money(), "7.80");

        // 5% 税率乘出来是 4 位小数，必须取整到分
        let db2 = mem();
        add_inv(&db2, "out", "2026-01-05", "0.13", "1000", "130", "verified");
        let mut o = TaxOptions::default();
        o.city_tax_zone = "county".into();
        o.save(&db2).unwrap();
        let f2 = vat_main_form(&db2, p).unwrap();
        assert_eq!(
            f2.surcharge.city_tax.fmt_money(),
            "6.50",
            "130 × 5% 应显示 6.50 而不是 6.5000"
        );
        assert_eq!(f2.output_tax.fmt_money().split('.').nth(1).map(str::len), Some(2));
        assert_eq!(
            f2.surcharge.city_tax.fmt_money().split('.').nth(1).map(str::len),
            Some(2),
            "同一张表上金额小数位必须一致"
        );
    }

    /// 城建税按适用地区取法定三档，不给自定义
    #[test]
    fn city_tax_zone_picks_legal_rate() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        add_inv(&db, "out", "2026-01-05", "0.13", "1000", "130", "verified");

        for (zone, want) in [("city", "9.10"), ("county", "6.50"), ("other", "1.30")] {
            let mut o = TaxOptions::default();
            o.city_tax_zone = zone.into();
            o.save(&db).unwrap();
            let f = vat_main_form(&db, p).unwrap();
            assert_eq!(
                f.surcharge.city_tax.fmt_money(),
                want,
                "{zone} 档城建税应为 {want}（当前 {}）",
                f.surcharge.city_tax.fmt_money()
            );
        }
    }

    /// 跨期数据不能串：只取本期
    ///
    /// 同时守住「期间过滤真的生效」这件事：`substr(date,1,6)` 在 `2026-01-05`
    /// 上取到的是 `"2026-0"`，一条都匹配不上、申报表全 0 而没有任何报错。
    /// 这个 bug 安静得很：数字全对，只是全等于零。
    #[test]
    fn only_current_period_invoices() {
        let db = mem();
        let jan = fincore::Period::new(2026, 1).unwrap();
        let feb = fincore::Period::new(2026, 2).unwrap();
        add_inv(&db, "out", "2026-01-10", "0.13", "1000", "130", "verified");
        add_inv(&db, "out", "2026-02-10", "0.13", "2000", "260", "verified");

        assert_eq!(vat_main_form(&db, jan).unwrap().output_tax.fmt_money(), "130.00");
        assert_eq!(vat_main_form(&db, feb).unwrap().output_tax.fmt_money(), "260.00");
    }

    /// 汇总数要能追到发票：档位明细之和必须等于档位汇总
    #[test]
    fn bucket_detail_reconciles_with_bucket() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        add_inv(&db, "out", "2026-01-05", "0.13", "1000", "130", "verified");
        add_inv(&db, "out", "2026-01-06", "0.13", "500", "65", "pending");

        let f = vat_main_form(&db, p).unwrap();
        let b = f
            .details_out
            .buckets
            .iter()
            .find(|b| b.rate == "13")
            .expect("应有 13% 档");
        let detail = bucket_detail(&db, "out", p, "13").unwrap();
        assert_eq!(detail.len() as i64, b.count, "明细张数与汇总张数对不上");
        let sum: Money = detail.iter().map(|d| Money::parse(&d.tax).unwrap()).sum();
        assert_eq!(
            sum.to_string(),
            b.tax.to_string(),
            "明细税额之和必须等于档位汇总（回答「这个数由哪几张票构成」）"
        );
    }

    /// 空账套也要出表 + 明确提示，不能报错也不能装作有数
    #[test]
    fn empty_book_yields_form_with_warnings() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        let f = vat_main_form(&db, p).unwrap();
        assert_eq!(f.output_tax.fmt_money(), "0.00");
        assert!(f.details_out.buckets.is_empty());
        assert!(
            f.warnings.iter().any(|w| w.contains("未开票收入")),
            "无销项时必须提示未开票收入要人工填：{:?}",
            f.warnings
        );
    }

    /// 留抵（进项 > 销项）必须提示人工处理
    #[test]
    fn credit_balance_is_flagged() {
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        add_inv(&db, "out", "2026-01-05", "0.13", "100", "13", "verified");
        add_inv(&db, "in", "2026-01-05", "0.13", "1000", "130", "verified");

        let f = vat_main_form(&db, p).unwrap();
        assert!(f.payable.is_negative(), "进项大于销项应为负数");
        assert!(
            f.warnings.iter().any(|w| w.contains("留抵")),
            "留抵必须提示人工结转：{:?}",
            f.warnings
        );
    }

    fn tax_keys(today: &str, taxpayer: &str) -> Vec<&'static str> {
        deadline_todos(
            chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d").unwrap(),
            taxpayer,
        )
        .iter()
        .map(|t| t.key)
        .collect()
    }

    /// 申报期边界：15 日含、16 日不含；小规模 11 月无增值税但有个税
    #[test]
    fn deadline_申报期边界() {
        assert!(tax_keys("2026-11-15", "general").contains(&"tax_vat"));
        assert!(!tax_keys("2026-11-16", "general").contains(&"tax_vat"));
        assert!(!tax_keys("2026-11-10", "small").contains(&"tax_vat"), "小规模非季月不报增值税");
        assert!(tax_keys("2026-11-10", "small").contains(&"tax_iit"), "个税按月人人都要报");
        assert!(tax_keys("2026-10-10", "small").contains(&"tax_vat"), "季月小规模要报");
    }

    /// 季度与年度事项只在窗口出现
    #[test]
    fn deadline_季度年度窗口() {
        assert!(tax_keys("2026-01-10", "general").contains(&"tax_cit"));
        assert!(!tax_keys("2026-02-10", "general").contains(&"tax_cit"));
        assert!(tax_keys("2026-05-31", "general").contains(&"tax_final"));
        assert!(!tax_keys("2026-06-01", "general").contains(&"tax_final"));
        assert!(tax_keys("2026-06-30", "general").contains(&"tax_annual"));
        assert!(!tax_keys("2026-07-01", "general").contains(&"tax_annual"));
    }
}
