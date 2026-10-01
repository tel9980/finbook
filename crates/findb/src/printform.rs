//! 会计凭证 / 账簿套打（模板打印）HTML 生成
//!
//! 会计档案要求「凭证 + 账簿」按标准版式打印装订。本模块把已经算好的
//! 业务数据（凭证、账簿行）渲染成一张自带打印样式的 HTML——
//! - 桌面端：写临时文件用系统浏览器打开，Ctrl/Cmd+P 打印；
//! - Web 端：直接返回 HTML，浏览器打印。
//!
//! 双端复用同一份模板，保证打印版式一致。版式对齐《会计基础工作规范》：
//! - 记账凭证：摘要 / 总账科目 / 明细科目 / 借 / 贷，附单据张数、会计主管·记账·出纳·制单签章栏；
//! - 账簿（明细账 / 总账 / 日记账）：日期 / 凭证字号 / 摘要 / 借 / 贷 / 借或贷 / 余额，
//!   含期初余额、本期合计，页尾可接续。

use fincore::{Money, Period};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 由业务凭证 + 科目表转换成套打行数据。
/// - `gen_name`：总账（一级）科目路径名；`detail_name`：包含辅助核算的明细科目全名。
pub fn voucher_print_from(
    v: &fincore::Voucher,
    chart: &fincore::Chart,
    aux_label: &dyn Fn(&fincore::AuxRef) -> String,
) -> VoucherPrint {
    let mut rows = Vec::with_capacity(v.entries.len());
    let mut debit_total = Money::ZERO;
    let mut credit_total = Money::ZERO;
    for e in &v.entries {
        if e.debit.is_zero() && e.credit.is_zero() {
            continue;
        }
        let full = chart.full_name(&e.account_code);
        let gen = full
            .split('/')
            .next()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| full.clone());
        let mut detail = full;
        let a = aux_label(&e.aux);
        if !a.trim().is_empty() {
            if detail.is_empty() {
                detail = a.clone();
            } else {
                detail = format!("{detail}（{a}）");
            }
        }
        debit_total += e.debit;
        credit_total += e.credit;
        rows.push(VoucherPrintRow {
            summary: e.summary.clone(),
            gen_name: gen,
            detail_name: detail,
            debit: e.debit,
            credit: e.credit,
        });
    }
    VoucherPrint {
        word: v.word.clone(),
        no: v.no,
        date: v.date.format("%Y-%m-%d").to_string(),
        attachments: v.attachments,
        rows,
        debit_total,
        credit_total,
    }
}

/// 把某个期间、某凭证字下的业务凭证转成套打输入。
/// 只取未作废的凭证，随着 `vouchers::list` 的返回顺序。
pub fn vouchers_to_print(
    vouchers: &[fincore::Voucher],
    chart: &fincore::Chart,
    aux_label: &dyn Fn(&fincore::AuxRef) -> String,
) -> Vec<VoucherPrint> {
    vouchers
        .iter()
        .filter(|v| v.status != fincore::VoucherStatus::Void)
        .map(|v| voucher_print_from(v, chart, aux_label))
        .collect()
}

// ===========================================================================
// 记账凭证套打
// ===========================================================================

/// 凭证套打的一行分录
pub struct VoucherPrintRow {
    pub summary: String,
    /// 总账科目（一级）名称
    pub gen_name: String,
    /// 明细科目全路径名称
    pub detail_name: String,
    pub debit: Money,
    pub credit: Money,
}

/// 凭证套打的整张凭证
pub struct VoucherPrint {
    pub word: String,
    pub no: i32,
    pub date: String,
    pub attachments: i32,
    pub rows: Vec<VoucherPrintRow>,
    pub debit_total: Money,
    pub credit_total: Money,
}

impl VoucherPrint {
    pub fn voucher_no(&self) -> String {
        format!("{}-{:04}", self.word, self.no)
    }
}

/// 生成一页「记账凭证」套打 HTML（可含多张凭证，逐张分页）。
/// 标准版式：摘要 / 总账科目 / 明细科目 / 借方金额 / 贷方金额 + 合计 + 签章栏。
pub fn voucher_form_html(
    company: &str,
    period_label: &str,
    vouchers: &[VoucherPrint],
    page_from_1: bool,
) -> String {
    let forms: String = vouchers
        .iter()
        .enumerate()
        .map(|(i, v)| one_voucher_form(company, period_label, v, page_from_1, i + 1, vouchers.len()))
        .collect();
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<title>记账凭证</title>
<style>{VOUCHER_CSS}</style>
<script>window.onload=function(){{setTimeout(function(){{window.print();}},300);}};</script>
</head><body>{forms}</body></html>"#
    )
}

const VOUCHER_CSS: &str = r#"
@page{size:A4 landscape;margin:12mm 10mm;}
body{font-family:"宋体","SimSun","Noto Serif CJK SC",serif;color:#111;margin:0;font-size:12px;}
.form{width:100%;page-break-after:always;box-sizing:border-box;}
.form:last-child{page-break-after:auto;}
.head{display:flex;align-items:center;justify-content:space-between;
  border:1px solid #000;border-bottom:none;padding:4px 10px;background:#fafafa;}
.head .company{font-weight:bold;font-size:14px;}
.head .title{font-weight:bold;font-size:16px;letter-spacing:4px;}
.head .no{white-space:nowrap;}
.meta{display:flex;border:1px solid #000;border-bottom:none;font-size:11px;}
.meta>div{padding:3px 10px;border-right:1px dotted #999;}
table{border-collapse:collapse;width:100%;}
th,td{border:1px solid #000;padding:2px 4px;}
th{background:#f2f2f2;text-align:center;}
td.num{text-align:right;font-variant-numeric:tabular-nums;}
td.sel{background:#fafafa;}
tr.l{height:21px;}
.total-row td{font-weight:bold;background:#f7f7f7;}
.foot{display:flex;justify-content:space-between;border:1px solid #000;border-top:none;
  padding:6px 10px;font-size:11px;}
.foot div{width:22%;}
.foot .lbl{display:block;color:#666;margin-bottom:2px;}
@media print{body{font-size:11px;}}
"#;

fn one_voucher_form(
    company: &str,
    period_label: &str,
    v: &VoucherPrint,
    page_from_1: bool,
    idx: usize,
    total: usize,
) -> String {
    let mut body = String::new();
    let n_rows = v.rows.len().max(1);
    // 保证打印纸底部版式稳定：按固定行高补齐到至少 8 行
    let fill = 8usize.saturating_sub(n_rows);
    for (i, r) in v.rows.iter().enumerate() {
        body.push_str(&format!(
            "<tr class='l'><td class='c'>{}</td><td>{}</td><td>{}</td>\
             <td class='num'>{}</td><td class='num'>{}</td></tr>",
            i + 1,
            esc(&r.gen_name),
            esc(&r.detail_name),
            r.debit.fmt_money(),
            r.credit.fmt_money(),
        ));
    }
    for _ in 0..fill {
        body.push_str(
            "<tr class='l'><td class='c'></td><td class='sel'></td><td class='sel'></td>\
             <td class='num sel'></td><td class='num sel'></td></tr>",
        );
    }
    let page = if page_from_1 { format!("第 {idx} / 共 {total} 页") } else { String::new() };
    let caption = if v.debit_total.is_zero() {
        "".to_string()
    } else {
        v.debit_total.to_capital()
    };
    format!(
        r#"<div class="form">
<div class="head">
  <span class="company">{company}</span>
  <span class="title">记　账　凭　证</span>
  <span class="no">凭证字号：{word}</span>
</div>
<div class="meta">
  <div>日期：{date}</div>
  <div>期间：{period}</div>
  <div>附件：{att} 张</div>
  <div style="border-right:none;flex:1"></div>
  <div>{page}</div>
</div>
<table>
<thead><tr>
  <th style="width:4%">行</th>
  <th style="width:17%">摘要</th>
  <th style="width:22%">总账科目</th>
  <th style="width:27%">明细科目</th>
  <th style="width:15%">借方金额</th>
  <th style="width:15%">贷方金额</th>
</tr></thead>
<tbody>
{rows}
<tr class="total-row">
  <td colspan="2">合计</td>
  <td class="num" colspan="1" style="border-right:none"></td>
  <td style="border-left:none"></td>
  <td class="num">{di}</td>
  <td class="num">{ci}</td>
</tr>
</tbody></table>
<div class="foot">
  <div><span class="lbl">会计主管</span>&nbsp;</div>
  <div><span class="lbl">记账</span>&nbsp;</div>
  <div><span class="lbl">复核（审核）</span>&nbsp;</div>
  <div><span class="lbl">出纳</span>&nbsp;</div>
  <div><span class="lbl">制单</span>&nbsp;&nbsp;大写：{cap}</div>
</div>
</div>"#,
        company = esc(company),
        word = esc(&v.voucher_no()),
        date = esc(&v.date),
        period = esc(period_label),
        att = v.attachments,
        rows = body,
        page = esc(&page),
        di = v.debit_total.fmt_money(),
        ci = v.credit_total.fmt_money(),
        cap = esc(&caption),
    )
}

// ===========================================================================
// 账簿套打（总账 / 明细账 / 日记账）
// ===========================================================================

/// 账簿一行
#[derive(Clone)]
pub struct LedgerPrintRow {
    pub date: String,
    pub voucher_no: String,
    pub summary: String,
    pub debit: Money,
    pub credit: Money,
    /// 借 / 贷 / 平
    pub dir: String,
    pub balance: Money,
}

/// 账簿套打整页的输入
pub struct LedgerPrint {
    pub title: String,      // 总账 / 明细账 / 日记账
    pub account_name: String, // 科目（编码 + 全路径名）
    pub period_label: String,
    /// 期初余额方向（借 / 贷 / 平）与金额
    pub begin_dir: String,
    pub begin_balance: Money,
    pub rows: Vec<LedgerPrintRow>,
    pub page_from_1: bool,
}

/// 生成账簿套打 HTML。标准三栏账版式：日期 / 凭证字号 / 摘要 / 借 / 贷 / 借或贷 / 余额。
pub fn ledger_form_html(company: &str, ledger: &LedgerPrint) -> String {
    let mut body = String::new();
    for r in &ledger.rows {
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class='num'>{}</td>\
             <td class='num'>{}</td><td>{}</td><td class='num'>{}</td></tr>",
            esc(&r.date),
            esc(&r.voucher_no),
            esc(&r.summary),
            r.debit.fmt_money(),
            r.credit.fmt_money(),
            esc(&r.dir),
            r.balance.fmt_money(),
        ));
    }
    let page = if ledger.page_from_1 { "第 1 页 / 共 1 页".to_string() } else { String::new() };
    let head_row = format!(
        "<tr><td>{}</td><td>{}</td><td>{}</td><td></td><td></td><td class='c'>{}</td>\
         <td class='num'>{}</td></tr>",
        "",
        "",
        esc(&format!("期初余额：{}{}", ledger.begin_dir, ledger.begin_balance)),
        esc(&ledger.begin_dir),
        ledger.begin_balance.fmt_money(),
    );
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<title>{title}</title>
<style>{LEDGER_CSS}</style>
<script>window.onload=function(){{setTimeout(function(){{window.print();}},300);}};</script>
</head><body>
<div class="lhead">
  <span class="ltitle">{title}</span>
  <span class="lacct">{acct}</span>
</div>
<div class="lmeta">
  <span>编制单位：{company}</span>
  <span>期间：{period}</span>
  <span>{page}</span>
</div>
<table>
<thead><tr>
  <th style="width:9%">日期</th>
  <th style="width:11%">凭证字号</th>
  <th style="width:36%">摘要</th>
  <th style="width:12%">借方</th>
  <th style="width:12%">贷方</th>
  <th style="width:6%">借或贷</th>
  <th style="width:14%">余额</th>
</tr></thead>
<tbody>{head}{rows}</tbody></table>
</body></html>"#,
        title = esc(&ledger.title),
        acct = esc(&ledger.account_name),
        company = esc(company),
        period = esc(&ledger.period_label),
        page = esc(&page),
        head = head_row,
        rows = body,
    )
}

const LEDGER_CSS: &str = r#"
@page{size:A4 landscape;margin:10mm 10mm;}
body{font-family:"宋体","SimSun","Noto Serif CJK SC",serif;color:#111;font-size:12px;margin:0;}
.lhead{display:flex;justify-content:space-between;align-items:center;border:1px solid #000;
  border-bottom:none;padding:4px 10px;background:#fafafa;}
.ltitle{font-weight:bold;font-size:16px;}
.lacct{white-space:nowrap;}
.lmeta{display:flex;justify-content:space-between;border:1px solid #000;border-bottom:none;
  padding:3px 10px;font-size:11px;color:#333;}
table{border-collapse:collapse;width:100%;}
th,td{border:1px solid #000;padding:2px 4px;}
th{background:#f2f2f2;text-align:center;}
td.num{text-align:right;font-variant-numeric:tabular-nums;}
td.c{text-align:center;}
@media print{body{font-size:11px;}}
"#;

// ===========================================================================
// 便利函数：把账簿行 + 期初余额转成套打需要的结构
// ===========================================================================

/// 由一组账簿行计算本期合计与静态页元，供上层组装 [`LedgerPrint`]。
#[allow(clippy::too_many_arguments)]
pub fn summarize_ledger<'a>(
    title: &str,
    account_name: &str,
    period: Period,
    begin_dir: &str,
    begin_balance: Money,
    rows: &'a [LedgerPrintRow],
) -> LedgerPrint {
    LedgerPrint {
        title: title.to_string(),
        account_name: account_name.to_string(),
        period_label: period.code(),
        begin_dir: begin_dir.to_string(),
        begin_balance,
        rows: rows.to_vec(),
        page_from_1: true,
    }
}

// ===========================================================================
// 测试
// ===========================================================================

// ===========================================================================
// 业务单据套打（销售/采购订单、收付款单）：字段白名单可选 + 批量紧凑分页（充分利用 A4）
// ===========================================================================

/// 单据套打字段开关。
/// - `fields=no,date,...` 白名单解析（[`fields_from_tokens`]）：出现的 token 打开，未出现的关闭，空串 = 全部显示；
/// - `pack` 不走白名单，由 `pack=0/1` 单独控制（默认开：批量时多单紧凑排一页）。
#[derive(Clone, Copy, Debug)]
pub struct DocPrintFields {
    pub no: bool,
    pub date: bool,
    /// 订单状态 / 收付款类型
    pub status: bool,
    /// 客户·供应商 / 往来单位
    pub party: bool,
    pub memo: bool,
    pub col_code: bool,
    pub col_name: bool,
    pub col_qty: bool,
    pub col_price: bool,
    pub col_rate: bool,
    pub col_amount: bool,
    pub col_tax: bool,
    pub col_memo: bool,
    /// 制单人
    pub prepared: bool,
    /// 资金账户（收付款单）
    pub fund: bool,
    /// 关联凭证号（收付款单）
    pub voucher: bool,
    pub totals: bool,
    pub sign: bool,
    /// 批量时多单紧凑排一页（充分利用 A4）；false = 一单一页
    pub pack: bool,
}

impl Default for DocPrintFields {
    fn default() -> Self {
        Self {
            no: true,
            date: true,
            status: true,
            party: true,
            memo: true,
            col_code: true,
            col_name: true,
            col_qty: true,
            col_price: true,
            col_rate: true,
            col_amount: true,
            col_tax: true,
            col_memo: true,
            prepared: true,
            fund: true,
            voucher: true,
            totals: true,
            sign: true,
            pack: true,
        }
    }
}

fn all_off() -> DocPrintFields {
    DocPrintFields {
        no: false,
        date: false,
        status: false,
        party: false,
        memo: false,
        col_code: false,
        col_name: false,
        col_qty: false,
        col_price: false,
        col_rate: false,
        col_amount: false,
        col_tax: false,
        col_memo: false,
        prepared: false,
        fund: false,
        voucher: false,
        totals: false,
        sign: false,
        pack: false,
    }
}

/// 解析 `fields=no,date,...`：出现的 token 打开、未出现的关闭；**空串 = 全部默认显示**。
/// `pack` 不在此解析（保持默认 true，由调用方按 `pack=0/1` 覆盖）。
pub fn fields_from_tokens(csv: &str) -> DocPrintFields {
    if csv.trim().is_empty() {
        return DocPrintFields::default();
    }
    let mut f = all_off();
    f.pack = true;
    for t in csv.split(',') {
        match t.trim() {
            "no" => f.no = true,
            "date" => f.date = true,
            "status" => f.status = true,
            "party" => f.party = true,
            "memo" => f.memo = true,
            "code" => f.col_code = true,
            "name" => f.col_name = true,
            "qty" => f.col_qty = true,
            "price" => f.col_price = true,
            "rate" => f.col_rate = true,
            "amount" => f.col_amount = true,
            "tax" => f.col_tax = true,
            "linememo" => f.col_memo = true,
            "prepared" => f.prepared = true,
            "fund" => f.fund = true,
            "voucher" => f.voucher = true,
            "totals" => f.totals = true,
            "sign" => f.sign = true,
            _ => {}
        }
    }
    f
}

/// 一张订单的打印数据（销售订单 / 采购订单 共用）
#[derive(Clone, Debug)]
pub struct OrderPrint {
    pub title: String,
    pub no: String,
    pub date: String,
    /// 已翻译状态（草稿 / 已确认 …）
    pub status: String,
    /// 客户 / 供应商
    pub party_label: String,
    /// 编码 + 名称
    pub party: String,
    pub prepared_by: String,
    pub memo: String,
    pub rows: Vec<OrderPrintRow>,
    pub amount_total: Money,
    pub tax_total: Money,
}

/// 订单明细打印行
#[derive(Clone, Debug)]
pub struct OrderPrintRow {
    pub code: String,
    pub name: String,
    pub qty: Money,
    pub price: Money,
    pub rate: Money,
    pub amount: Money,
    pub tax: Money,
    pub memo: String,
}

/// 收付款单打印数据
#[derive(Clone, Debug)]
pub struct ReceiptPrint {
    pub no: String,
    pub date: String,
    /// 收款 / 付款
    pub kind_label: String,
    pub fund: String,
    pub party: String,
    pub amount: Money,
    pub memo: String,
    /// 关联凭证字号（空 = 无）
    pub voucher_no: String,
}

// 行单位排版预算：明细行打印高约 6.5mm，单据固定开销（标题+信息头+表头+
// 合计+签章 ≈ 38mm）折 6 个行单位。**每页预算随纸张尺寸计算**（page_setup），
// 批量时整张单据按预算塞入当前页剩余空间。
const DOC_OVERHEAD: i32 = 6;

/// 纸张 → (`.page` 规则, 行单位预算, 窄/小纸附加 CSS)。
/// 支持 `a4`(210×297) / `a5`(二等分 210×148) / `third`(三等分 99×210) /
/// 自定义 `"宽x高"`（mm，50..=600）；非法值回退 A4。
pub fn page_setup(size: &str) -> (String, i32, String) {
    let s = size.trim().to_ascii_lowercase();
    let custom = s
        .split_once('x')
        .and_then(|(a, b)| Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?)))
        .filter(|(w, h)| (50..=600).contains(w) && (50..=600).contains(h));
    let (w, h, m) = if s == "a5" {
        (210, 148, 8)
    } else if s == "third" {
        (99, 210, 8)
    } else if let Some((w, h)) = custom {
        (w, h, 8)
    } else {
        (210, 297, 10)
    };
    // 页高 − 上下边距，按 6.5mm/行 折行单位预算
    let budget = (((h - 2 * m) as f32) / 6.5) as i32;
    let page = match (w, h) {
        (210, 297) => "@page{size:A4;margin:10mm;}".to_string(),
        (210, 148) => "@page{size:A5;margin:8mm;}".to_string(),
        _ => format!("@page{{size:{w}mm {h}mm;margin:{m}mm;}}"),
    };
    // 窄纸（宽 < 120mm，如三等分联）压缩字号与行高；小纸（高 < 200mm）略紧
    let extra = if w < 120 {
        ".dtitle .t{font-size:14px;letter-spacing:4px;}th,td{font-size:9px;padding:1px 2px;}tr.l{height:18px;}"
            .to_string()
    } else if h < 200 {
        "tr.l{height:19px;}".to_string()
    } else {
        String::new()
    };
    (page, budget.max(6), extra)
}

const DOC_CSS: &str = r#"
body{font-family:"宋体","SimSun","Noto Serif CJK SC",serif;color:#111;margin:0;font-size:12px;}
.page{page-break-after:always;}
.page:last-child{page-break-after:auto;}
.cbar{display:flex;justify-content:space-between;border-bottom:2px solid #000;
  padding:0 0 4px;margin-bottom:6px;font-size:11px;}
.cbar .co{font-weight:bold;font-size:13px;}
.doc{margin-bottom:10px;break-inside:avoid;}
.dtitle{display:flex;justify-content:space-between;align-items:baseline;margin-bottom:3px;}
.dtitle .t{font-weight:bold;font-size:17px;letter-spacing:8px;}
.dtitle .no{font-size:11px;color:#333;}
.dmeta{font-size:11px;border:1px solid #000;border-bottom:none;padding:3px 8px;
  display:flex;flex-wrap:wrap;gap:2px 20px;background:#fafafa;}
table{border-collapse:collapse;width:100%;}
th,td{border:1px solid #000;padding:2px 4px;font-size:11px;}
th{background:#f2f2f2;text-align:center;}
td.n{text-align:right;font-variant-numeric:tabular-nums;}
td.c{text-align:center;}
td.lbl{background:#f7f7f7;text-align:center;width:14%;}
tr.l{height:21px;}
.tot td{font-weight:bold;background:#f7f7f7;}
.rmb td{background:#fafafa;font-size:11px;}
.rmb b{font-size:14px;}
.dsign{display:flex;border:1px solid #000;border-top:none;font-size:11px;}
.dsign div{flex:1;padding:5px 8px;border-right:1px dotted #999;}
.dsign div:last-child{border-right:none;}
.pbar{position:sticky;top:0;z-index:9;display:flex;gap:14px;align-items:center;background:#1a1a1a;
  color:#fff;padding:8px 14px;font-family:system-ui,sans-serif;font-size:12px;}
.pbar button{background:#fff;color:#111;border:0;border-radius:4px;padding:6px 14px;font-size:13px;cursor:pointer;}
@media print{.pbar{display:none;}}
@media print{body{font-size:11px;}}
"#;

fn doc_shell(title: &str, body: &str, size: &str, auto_print: bool) -> String {
    let (page, _, extra) = page_setup(size);
    // 默认渲染顶部「打印本页」工具条（先看后打、省纸）；auto=1 时进页自动弹打印对话框
    let js = if auto_print {
        "<script>window.onload=function(){setTimeout(function(){window.print();},300);};</script>"
            .to_string()
    } else {
        r#"<div class="pbar"><button onclick="window.print()">🖨 打印本页</button><span>纸张 / 份数在打印对话框中选择；关闭本页即取消</span></div>"#.to_string()
    };
    format!(
        r#"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<title>{title}</title>
<style>{page}{DOC_CSS}{extra}</style>
</head><body>{js}{body}</body></html>"#,
        title = esc(title),
        page = page,
        extra = extra,
        js = js,
        body = body,
    )
}

/// 可见列（th 文本, token, 基准宽度%）；基准宽度合计 100，子集渲染时归一化
fn order_columns(f: &DocPrintFields) -> Vec<(&'static str, &'static str, u32)> {
    let mut cols: Vec<(&'static str, &'static str, u32)> = Vec::new();
    if f.col_code { cols.push(("存货编码", "code", 10)); }
    if f.col_name { cols.push(("名称", "name", 24)); }
    if f.col_qty { cols.push(("数量", "qty", 8)); }
    if f.col_price { cols.push(("单价", "price", 11)); }
    if f.col_rate { cols.push(("税率", "rate", 6)); }
    if f.col_amount { cols.push(("金额", "amount", 13)); }
    if f.col_tax { cols.push(("税额", "tax", 10)); }
    if f.col_memo { cols.push(("备注", "linememo", 18)); }
    cols
}

fn order_units(f: &DocPrintFields, o: &OrderPrint) -> i32 {
    DOC_OVERHEAD + o.rows.len() as i32 + i32::from(f.totals) + i32::from(f.sign)
}

/// 订单套打 HTML（默认 A4；多尺寸见 [`order_forms_sized_html`]）
pub fn order_forms_html(company: &str, orders: &[OrderPrint], f: &DocPrintFields) -> String {
    order_forms_sized_html(company, orders, f, "a4", false)
}

/// 订单套打 HTML（指定纸张）：`fields` 控制显示字段/列；`pack` 时按**该纸张的
/// 行单位预算**把多张单据紧凑排进同页（放不下才换页；单张超一页自然续页）。
/// `auto_print=false` 渲染顶部「打印本页」工具条（先看后打）。
pub fn order_forms_sized_html(
    company: &str,
    orders: &[OrderPrint],
    f: &DocPrintFields,
    size: &str,
    auto_print: bool,
) -> String {
    let budget = page_setup(size).1;
    let mut pages: Vec<Vec<&OrderPrint>> = Vec::new();
    let mut cur: Vec<&OrderPrint> = Vec::new();
    let mut rem = budget;
    for o in orders {
        let cost = order_units(f, o);
        if f.pack && !cur.is_empty() && cost <= rem {
            cur.push(o);
            rem -= cost;
        } else {
            if !cur.is_empty() {
                pages.push(std::mem::take(&mut cur));
            }
            cur.push(o);
            rem = budget - cost;
        }
    }
    if !cur.is_empty() {
        pages.push(cur);
    }
    let today = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let total = pages.len();
    let body: String = pages
        .iter()
        .enumerate()
        .map(|(i, ps)| {
            let docs: String = ps.iter().map(|o| one_order_doc(o, f)).collect();
            format!(
                r#"<div class="page">
<div class="cbar"><span class="co">{company}</span><span>单据套打</span><span>共 {n} 单 · 第 {p} / 共 {t} 页 · {today}</span></div>
{docs}
</div>"#,
                company = esc(company),
                n = orders.len(),
                p = i + 1,
                t = total,
                today = esc(&today),
                docs = docs,
            )
        })
        .collect();
    doc_shell("单据套打", &body, size, auto_print)
}

fn one_order_doc(o: &OrderPrint, f: &DocPrintFields) -> String {
    // 标题区
    let mut no_bits: Vec<String> = Vec::new();
    if f.no {
        no_bits.push(format!("单号：{}", esc(&o.no)));
    }
    if f.date {
        no_bits.push(format!("日期：{}", esc(&o.date)));
    }
    // 信息头（flex gap 分隔）
    let mut meta_bits: Vec<String> = Vec::new();
    if f.party {
        meta_bits.push(format!("{}：{}", esc(&o.party_label), esc(&o.party)));
    }
    if f.status {
        meta_bits.push(format!("状态：{}", esc(&o.status)));
    }
    if f.prepared {
        meta_bits.push(format!("制单：{}", esc(&o.prepared_by)));
    }
    if f.memo && !o.memo.trim().is_empty() {
        meta_bits.push(format!("备注：{}", esc(o.memo.trim())));
    }
    let meta = if meta_bits.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="dmeta">{}</div>"#, meta_bits.join(""))
    };

    // 列头（可见列归一化宽度到 100%）
    let cols = order_columns(f);
    let ncol = cols.len().max(1);
    let sum: u32 = cols.iter().map(|c| c.2).sum::<u32>().max(1);
    let mut used = 0u32;
    let widths: Vec<u32> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if i + 1 == cols.len() {
                100 - used
            } else {
                let w = c.2 * 100 / sum;
                used += w;
                w
            }
        })
        .collect();
    let mut thead = String::from("<tr>");
    for ((h, _, _), w) in cols.iter().zip(widths.iter()) {
        thead.push_str(&format!(r#"<th style="width:{w}%">{h}</th>"#, w = w, h = h));
    }
    thead.push_str("</tr>");

    // 明细行
    let mut body = String::new();
    for r in &o.rows {
        body.push_str("<tr class='l'>");
        for (_, tok, _) in &cols {
            match *tok {
                "code" => body.push_str(&format!("<td class='c'>{}</td>", esc(&r.code))),
                "name" => body.push_str(&format!("<td>{}</td>", esc(&r.name))),
                "qty" => body.push_str(&format!("<td class='n'>{}</td>", r.qty.fmt_qty())),
                "price" => body.push_str(&format!("<td class='n'>{}</td>", r.price.fmt_money())),
                "rate" => {
                    let pct = (r.rate * Money::parse_or_zero("100")).round2();
                    body.push_str(&format!("<td class='c'>{}%</td>", pct.fmt_qty()));
                }
                "amount" => body.push_str(&format!("<td class='n'>{}</td>", r.amount.fmt_money())),
                "tax" => body.push_str(&format!("<td class='n'>{}</td>", r.tax.fmt_money())),
                "linememo" => body.push_str(&format!("<td>{}</td>", esc(&r.memo))),
                _ => body.push_str("<td></td>"),
            }
        }
        body.push_str("</tr>");
    }
    let totals = if f.totals {
        format!(
            r#"<tr class="tot"><td colspan="{n}">合计：不含税 {a}　税额 {t}　价税合计 {g}</td></tr>"#,
            n = ncol,
            a = o.amount_total.fmt_money(),
            t = o.tax_total.fmt_money(),
            g = (o.amount_total + o.tax_total).fmt_money(),
        )
    } else {
        String::new()
    };
    let sign = if f.sign {
        r#"<div class="dsign"><div><b>制单</b>：&nbsp;</div><div><b>客户/供应商签收</b>：&nbsp;</div><div><b>审核</b>：&nbsp;</div><div><b>日期</b>：&nbsp;</div></div>"#.to_string()
    } else {
        String::new()
    };

    format!(
        r#"<div class="doc">
<div class="dtitle"><span class="t">{title}</span><span class="no">{no}</span></div>
{meta}
<table><thead>{thead}</thead><tbody>{body}{totals}</tbody></table>
{sign}</div>"#,
        title = esc(&o.title),
        no = no_bits.join("　"),
        meta = meta,
        thead = thead,
        body = body,
        totals = totals,
        sign = sign,
    )
}

/// 收付款单套打 HTML（默认 A4；多尺寸见 [`receipt_forms_sized_html`]）
pub fn receipt_forms_html(company: &str, docs: &[ReceiptPrint], f: &DocPrintFields) -> String {
    receipt_forms_sized_html(company, docs, f, "a4", false)
}

/// 收付款单套打 HTML（指定纸张）：信息双列 + 金额大写 + 签章；`pack` 紧凑分页同订单
pub fn receipt_forms_sized_html(
    company: &str,
    docs: &[ReceiptPrint],
    f: &DocPrintFields,
    size: &str,
    auto_print: bool,
) -> String {
    const UNITS: i32 = DOC_OVERHEAD + 3;
    let budget = page_setup(size).1;
    let mut pages: Vec<Vec<&ReceiptPrint>> = Vec::new();
    let mut cur: Vec<&ReceiptPrint> = Vec::new();
    let mut rem = budget;
    for d in docs {
        if f.pack && !cur.is_empty() && UNITS <= rem {
            cur.push(d);
            rem -= UNITS;
        } else {
            if !cur.is_empty() {
                pages.push(std::mem::take(&mut cur));
            }
            cur.push(d);
            rem = budget - UNITS;
        }
    }
    if !cur.is_empty() {
        pages.push(cur);
    }
    let today = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let total = pages.len();
    let body: String = pages
        .iter()
        .enumerate()
        .map(|(i, ps)| {
            let blocks: String = ps.iter().map(|d| one_receipt_doc(d, f)).collect();
            format!(
                r#"<div class="page">
<div class="cbar"><span class="co">{company}</span><span>单据套打</span><span>共 {n} 单 · 第 {p} / 共 {t} 页 · {today}</span></div>
{blocks}
</div>"#,
                company = esc(company),
                n = docs.len(),
                p = i + 1,
                t = total,
                today = esc(&today),
                blocks = blocks,
            )
        })
        .collect();
    doc_shell("收付款单套打", &body, size, auto_print)
}

fn one_receipt_doc(d: &ReceiptPrint, f: &DocPrintFields) -> String {
    let mut no_bits: Vec<String> = Vec::new();
    if f.no {
        no_bits.push(format!("单号：{}", esc(&d.no)));
    }
    if f.date {
        no_bits.push(format!("日期：{}", esc(&d.date)));
    }
    let mut meta_bits: Vec<String> = Vec::new();
    if f.status {
        meta_bits.push(format!("类型：{}单", esc(&d.kind_label)));
    }
    if f.prepared {
        meta_bits.push(format!("凭证：{}", esc(if d.voucher_no.is_empty() { "未生成" } else { &d.voucher_no })));
    }

    // 双列信息格（label:value ×2/行，节省竖向空间）
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if f.fund {
        pairs.push(("资金账户", esc(&d.fund)));
    }
    if f.party {
        pairs.push(("往来单位", esc(&d.party)));
    }
    if f.memo && !d.memo.trim().is_empty() {
        pairs.push(("备注", esc(d.memo.trim())));
    }
    if f.voucher && !d.voucher_no.is_empty() {
        pairs.push(("关联凭证", esc(&d.voucher_no)));
    }
    let mut rows = String::new();
    let mut it = pairs.into_iter();
    loop {
        match (it.next(), it.next()) {
            (Some(a), Some(b)) => rows.push_str(&format!(
                r#"<tr><td class="lbl">{}</td><td>{}</td><td class="lbl">{}</td><td>{}</td></tr>"#,
                a.0, a.1, b.0, b.1
            )),
            (Some(a), None) => rows.push_str(&format!(
                r#"<tr><td class="lbl">{}</td><td colspan="3">{}</td></tr>"#,
                a.0, a.1
            )),
            (None, _) => break,
        }
    }
    if f.col_amount {
        rows.push_str(&format!(
            r#"<tr class="rmb"><td class="lbl"><b>金额</b></td><td colspan="3"><b>{}</b>　人民币（大写）{}</td></tr>"#,
            d.amount.fmt_money(),
            d.amount.to_capital()
        ));
    }
    let sign = if f.sign {
        r#"<div class="dsign"><div><b>制单</b>：&nbsp;</div><div><b>出纳</b>：&nbsp;</div><div><b>复核</b>：&nbsp;</div><div><b>日期</b>：&nbsp;</div></div>"#.to_string()
    } else {
        String::new()
    };
    let meta = if meta_bits.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="dmeta">{}</div>"#, meta_bits.join(""))
    };
    format!(
        r#"<div class="doc">
<div class="dtitle"><span class="t">{title}</span><span class="no">{no}</span></div>
{meta}
<table><tbody>{rows}</tbody></table>
{sign}</div>"#,
        title = format!("{}单", esc(&d.kind_label)),
        no = no_bits.join("　"),
        meta = meta,
        rows = rows,
        sign = sign,
    )
}

#[cfg(test)]
mod tests {
    // ---- 业务单据套打：字段白名单 / 批量紧凑分页（A4 空间最大化） ----
    fn mk_order(no: &str, lines: usize) -> super::OrderPrint {
        super::OrderPrint {
            title: "销售订单".to_string(),
            no: no.to_string(),
            date: "2026-01-05".to_string(),
            status: "已确认".to_string(),
            party_label: "客户".to_string(),
            party: "C01 甲公司".to_string(),
            prepared_by: "张三".to_string(),
            memo: "备注A".to_string(),
            rows: (0..lines)
                .map(|i| super::OrderPrintRow {
                    code: format!("14050{}", i % 10),
                    name: format!("成品{i}"),
                    qty: fincore::Money::parse("10").unwrap(),
                    price: fincore::Money::parse("20").unwrap(),
                    rate: fincore::Money::parse("0.13").unwrap(),
                    amount: fincore::Money::parse("200").unwrap(),
                    tax: fincore::Money::parse("26").unwrap(),
                    memo: String::new(),
                })
                .collect(),
            amount_total: fincore::Money::parse(&format!("{}", lines * 200)).unwrap(),
            tax_total: fincore::Money::parse(&format!("{}", lines * 26)).unwrap(),
        }
    }

    #[test]
    fn doc_fields_tokens() {
        assert!(super::fields_from_tokens("").col_rate, "空 fields = 全部默认显示");
        let f = super::fields_from_tokens("no,date,sign");
        assert!(f.no && f.date && f.sign);
        assert!(!f.col_rate && !f.totals && !f.col_amount, "白名单未列的字段应关闭");
        assert!(f.pack, "pack 不受 fields 白名单影响，默认开启");
        let f = super::fields_from_tokens("fund,voucher");
        assert!(f.fund && f.voucher);
    }

    #[test]
    fn order_forms_pack_and_fields() {
        let two = vec![mk_order("XS001", 2), mk_order("XS002", 2)];
        let html = super::order_forms_html("甲公司", &two, &Default::default());
        assert_eq!(html.matches("class=\"page\"").count(), 1, "两张小单应紧凑同页");
        assert_eq!(html.matches("class=\"doc\"").count(), 2, "同页两张单据");
        assert!(html.contains("销售订单") && html.contains("税率") && html.contains("签收"));

        // 超页长单 + 小单：长单占满首页，小单换页
        let mixed = vec![mk_order("XS003", 40), mk_order("XS004", 2)];
        let html = super::order_forms_html("甲公司", &mixed, &Default::default());
        assert_eq!(html.matches("class=\"page\"").count(), 2, "超页单据应换页");

        // pack 关闭 → 一单一页
        let f = super::DocPrintFields { pack: false, ..Default::default() };
        let html = super::order_forms_html("甲公司", &two, &f);
        assert_eq!(html.matches("class=\"page\"").count(), 2, "pack=0 应一单一页");

        // 字段收窄：不打税率列与合计，公司抬头始终保留
        let f = super::fields_from_tokens("no,date,code,qty,price,amount");
        let html = super::order_forms_html("甲公司", &two, &f);
        assert!(html.contains("单号") && html.contains("存货编码"));
        assert!(!html.contains("税率") && !html.contains("合计"), "未勾选字段不应出现");
        assert!(html.contains("甲公司"), "公司抬头始终保留");
    }

    #[test]
    fn receipt_forms_fields_and_capital() {
        let d = super::ReceiptPrint {
            no: "SK26010501".to_string(),
            date: "2026-01-05".to_string(),
            kind_label: "收款".to_string(),
            fund: "100201".to_string(),
            party: "C01 甲公司".to_string(),
            amount: fincore::Money::parse("1234.56").unwrap(),
            memo: "回款".to_string(),
            voucher_no: "记-0007".to_string(),
        };
        let html = super::receipt_forms_html("甲公司", &[d.clone()], &Default::default());
        assert!(html.contains("收款单"), "标题应为收款单");
        assert!(html.contains("壹仟贰佰叁拾肆元伍角陆分"), "金额大写");
        assert!(html.contains("记-0007") && html.contains("资金账户"));

        let f = super::fields_from_tokens("no,amount");
        let html = super::receipt_forms_html("甲公司", &[d], &f);
        assert!(!html.contains("资金账户"), "未勾选资金账户不应出现");
        assert!(html.contains("金额"), "金额行应保留");
    }

    #[test]
    fn page_sizes_recalc_budget() {
        let (_, b4, _) = super::page_setup("a4");
        let (_, b5, _) = super::page_setup("a5");
        let (_, bt, extra_t) = super::page_setup("third");
        let (_, bc, _) = super::page_setup("140x210");
        let (_, bf, _) = super::page_setup("abc");
        assert_eq!(b4, 42, "A4 预算约 42 行");
        assert!((15..b4).contains(&b5), "A5 预算应小于 A4：{b5}");
        assert!((22..b4).contains(&bt), "三等分预算：{bt}");
        assert!((22..b4).contains(&bc), "自定义 140x210 预算：{bc}");
        assert_eq!(bf, b4, "非法尺寸回退 A4");
        assert!(extra_t.contains("font-size:9px"), "三等分窄版应压缩字号");

        // 尺寸影响分页：两张 8 行单（单张成本 = 6+8+1+1 = 16 行单位）
        let two = vec![mk_order("XS101", 8), mk_order("XS102", 8)];
        let a4 = super::order_forms_sized_html("甲", &two, &Default::default(), "a4", false);
        let a5 = super::order_forms_sized_html("甲", &two, &Default::default(), "a5", false);
        assert_eq!(a4.matches("class=\"page\"").count(), 1, "A4 两单同页");
        assert_eq!(a5.matches("class=\"page\"").count(), 2, "A5 只装得下一单");
        assert!(a5.contains("size:A5"), "应输出 A5 @page");
        let d = super::ReceiptPrint {
            no: "SK1".to_string(),
            date: "2026-01-05".to_string(),
            kind_label: "收款".to_string(),
            fund: "100201".to_string(),
            party: "C01".to_string(),
            amount: fincore::Money::parse("10.00").unwrap(),
            memo: String::new(),
            voucher_no: String::new(),
        };
        let html = super::receipt_forms_sized_html("甲", &[d], &Default::default(), "third", false);
        assert!(html.contains("99mm 210mm"), "三等分应输出自定义 @page 尺寸");
    }

    use super::*;
    use chrono::NaiveDate;
    use fincore::{Account, AcctCategory, AuxRef, Chart, CodeScheme, Money, Period};

    fn sample_chart() -> Chart {
        let mut chart = Chart::new(CodeScheme::default());
        chart.insert(Account::new("1001", "库存现金", AcctCategory::Asset));
        chart.insert(Account::new("5001", "生产成本", AcctCategory::Cost));
        chart.insert(Account::new("500101", "直接材料", AcctCategory::Cost));
        chart
    }

    fn sample_voucher() -> fincore::Voucher {
        let p = Period::new(2026, 9).unwrap();
        let mut v = fincore::Voucher::new(p, NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(), "记", 1);
        v.attachments = 2;
        {
            let mut e = fincore::Entry::new(1, "500101", "领用原材料");
            e.debit = Money::from_cents(120_000);
            v.entries.push(e);
        }
        {
            let mut e = fincore::Entry::new(2, "1001", "结转成本");
            e.credit = Money::from_cents(120_000);
            v.entries.push(e);
        }
        v
    }

    #[test]
    fn voucher_print_skips_zero_and_total() {
        let chart = sample_chart();
        let v = sample_voucher();
        let aux = |_: &fincore::AuxRef| "".to_string();
        let p = voucher_print_from(&v, &chart, &aux);
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].gen_name, "生产成本");
        assert_eq!(p.debit_total, Money::from_cents(120_000));
        assert_eq!(p.credit_total, Money::from_cents(120_000));
        assert_eq!(p.voucher_no(), "记-0001");
    }

    #[test]
    fn voucher_form_html_has_key_structure() {
        let chart = sample_chart();
        let v = sample_voucher();
        let aux = |_: &fincore::AuxRef| "".to_string();
        let prints = vec![voucher_print_from(&v, &chart, &aux)];
        let html = voucher_form_html("某某公司", "2026-09", &prints, true);
        assert!(html.contains("记　账　凭　证"));
        assert!(html.contains("某某公司"));
        assert!(html.contains("记-0001"));
        assert!(html.contains("借方金额"));
        assert!(html.contains("合计"));
        assert!(html.contains("1,200.00"));
    }

    #[test]
    fn ledger_form_html_renders_begin_balance() {
        let ledger = LedgerPrint {
            title: "明细账".into(),
            account_name: "1001 库存现金".into(),
            period_label: "2026-09".into(),
            begin_dir: "借".into(),
            begin_balance: Money::from_cents(50_000),
            rows: vec![LedgerPrintRow {
                date: "2026-09-04".into(),
                voucher_no: "记-0001".into(),
                summary: "收款".into(),
                debit: Money::from_cents(10_000),
                credit: Money::ZERO,
                dir: "借".into(),
                balance: Money::from_cents(60_000),
            }],
            page_from_1: true,
        };
        let html = ledger_form_html("某某公司", &ledger);
        assert!(html.contains("明细账"));
        assert!(html.contains("期初余额：借500.00"));
        assert!(html.contains("借或贷"));
        assert!(html.contains("600.00"));
    }
}