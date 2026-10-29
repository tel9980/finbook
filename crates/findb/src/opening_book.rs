//! 期初收集表一次性导入。
//!
//! 《期初收集表.xlsx》是给工厂填的（由 `crates/findb/examples/opening_sheet.rs` 生成），
//! 一个工作簿四张纸：
//!
//! | 纸 | 内容 | 落到哪里 |
//! |----|------|----------|
//! | ① 客户应收 | 客户编码/名称/欠款金额/单据号/备注 | 往来期初（ar）**且** 112201 科目期初合计 |
//! | ② 供应商应付 | 供应商编码/名称/品类/欠款金额/单据号/备注 | 往来期初（ap）**且** 220201 科目期初合计 |
//! | ③ 账户余额 | 科目编码/账户/余额 | 对应科目的期初余额 |
//! | ④ 基本资料 | 实收资本 / 本年利润 | 4001 / 4103 的期初余额 |
//!
//! 为什么往来期初要同时写科目期初：`arap_opening` 是**台账**（用来算账龄、出对账单），
//! 资产负债表的应收数是 `112201` 的科目余额。只导台账不导科目，报表里一分应收都没有，
//! 而账龄表上明明有欠款 —— 这种「两处都对、合起来不对」最难查。
//!
//! 平衡：期初借贷必须相等，否则试算平衡一开局就不平。所以这里**先算完再落库**，
//! 不平就整批拒绝并报出差额（填错表比写进半个账套好收拾）。

use crate::{Db, DbResult};
use fincore::account::Account;
use fincore::auxiliary::AuxEntity;
use fincore::money::Money;


/// 导出用的列类型（只有「文本」与「金额」两种，够用且不把 xlsx 细节漏进数据层）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Col {
    Text,
    Money,
}

/// 一张「收集表」的纯数据（不含任何 xlsx 细节）
///
/// 放在 `findb` 而不是各端，是因为这张表既是**导出**也是**导入**的契约：
/// 桌面版和网页版必须吐出完全一样的列头，否则用户拿桌面下的表到网页导就错位。
#[derive(Debug, Clone, Default)]
pub struct CollectionSheet {
    pub name: String,
    pub title: String,
    pub subtitle: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub cols: Vec<Col>,
}

impl CollectionSheet {
    pub fn titled(
        name: &str,
        title: &str,
        subtitle: &str,
        headers: &[&str],
        cols: &[Col],
    ) -> Self {
        Self {
            name: name.to_string(),
            title: title.to_string(),
            subtitle: subtitle.to_string(),
            headers: headers.iter().map(|h| h.to_string()).collect(),
            rows: Vec::new(),
            cols: cols.to_vec(),
        }
    }

    pub fn with_rows(mut self, rows: Vec<Vec<String>>) -> Self {
        self.rows = rows;
        self
    }
}

pub fn collection_template(
    accounts: &[Account],
    customers: &[AuxEntity],
    suppliers: &[AuxEntity],
) -> Vec<CollectionSheet> {
    let mut out = Vec::new();

    // 0) 怎么填 —— 放在第一张，工厂打开就看到规则
    out.push(
        CollectionSheet::titled(
            "怎么填",
            "期初收集表",
            "只填你确实有的科目/往来；空着的行不用删，也不影响导入",
            &["项目", "说明"],
            &[Col::Text, Col::Text],
        )
        .with_rows(vec![
            vec!["① 客户应收".to_string(), "客户欠你的钱（按客户一行，金额填正数）".to_string()],
            vec!["② 供应商应付".to_string(), "你欠供应商的钱（按供应商一行，金额填正数）".to_string()],
            vec!["③ 账户余额".to_string(), "现金、各银行账户、固定/无形资产的账面数；借款、应交税费也填这里".to_string()],
            vec!["④ 基本资料".to_string(), "实收资本照营业执照上的填；「本年利润」不填就按借贷差额自动轧".to_string()],
            vec![
                "权益类不填在③".to_string(),
                "实收资本/本年利润只填在④ —— ③ 里再填一遍会双计，而且看起来还是平的".to_string(),
            ],
            vec![
                "不要填的两类".to_string(),
                "1122 应收账款 / 2202 应付账款 的科目余额不用填 —— 由 ①② 两张纸自动生成"
                    .to_string(),
            ],
            vec![
                "客户预交的钱（预收）".to_string(),
                "填 ③：2203 预收账款 的余额，并在「客户/供应商编码」列写上是哪个客户".to_string(),
            ],
            vec![
                "先付给供应商的钱（预付）".to_string(),
                "填 ③：1123 预付账款 的余额，并写上是哪个供应商".to_string(),
            ],
            vec![
                "存货".to_string(),
                "③ 里填金额，同时填「数量」和「存货编码」—— 只填金额的话数量是 0，以后出库结存会算错"
                    .to_string(),
            ],
            vec![
                "银行".to_string(),
                "③ 里填银行科目的余额，并在「银行账户」列写上账户编码（如 B01）—— 不填就落不到账户上，银行日记账没有期初"
                    .to_string(),
            ],
            vec![
                "不平怎么办".to_string(),
                "导入会先算平：不平整批拒绝，并告诉你差多少。差额补到「④ 本年利润」一般就平"
                    .to_string(),
            ],
            vec![
                "不确定就先预检".to_string(),
                "填一部分也能预检：只算平、不落库，改完再预检，满意了再正式导入".to_string(),
            ],
        ]),
    );

    // ①② 往来：编码与名称都预填，金额留空
    let parties = |name: &str, title: &str, code_head: &str, name_head: &str, rows: &[AuxEntity]| {
        CollectionSheet::titled(
            name,
            title,
            // 副标题里刻意不出现「编码」「科目」「项目」—— opener 靠这几个词找表头
            "金额填正数（欠多少填多少）；单据号留空则自动生成",
            &[code_head, name_head, "欠款金额", "单据号", "备注"],
            &[
                Col::Text,
                Col::Text,
                Col::Money,
                Col::Text,
                Col::Text,
            ],
        )
        .with_rows(
            rows.iter()
                .filter(|e| !e.disabled)
                .map(|e| {
                    vec![
                        e.code.clone(),
                        e.name.clone(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ]
                })
                .collect(),
        )
    };
    out.push(parties("① 客户应收", "客户应收（客户欠我的钱）", "客户编码", "客户名称", customers));
    out.push(parties(
        "② 供应商应付",
        "供应商应付（我欠供应商的钱）",
        "供应商编码",
        "供应商名称",
        suppliers,
    ));

    // ③ 账户余额：只列资产 / 负债类的末级科目。
    //
    // 剔掉两类，否则会**双计**：
    //   - 1122 / 2202：由 ①② 两张纸的往来合计自动生成
    //   - 4xxx 权益类：由 ④「实收资本 / 本年利润」生成
    // 双计的后果不是「多一行」，而是借贷两边各多一份、看起来还是平的 —— 最难查。
    let bal_rows: Vec<Vec<String>> = accounts
        .iter()
        .filter(|a| a.code.chars().next().is_some_and(|c| c == '1' || c == '2'))
        .filter(|a| !a.code.starts_with("1122") && !a.code.starts_with("2202"))
        .filter(|a| !a.disabled)
        .map(|a| {
            let mut row = vec![a.code.clone(), a.name.clone(), String::new()];
            // 后四列平时留空，只有特定科目才填 —— 见「怎么填」页。
            //
            // 没有这四列时，期初的「分户 / 分存货 / 分银行账户 / 数量」全都进不来：
            // 存货有金额没数量 → 数量金额式明细里数量是 0，出库结存必错；
            // 银行科目没账户 → 期初挂在不存在的账户上，银行日记账一分没有。
            row.extend(std::iter::repeat_n(String::new(), 4));
            row
        })
        .collect();
    out.push(
        CollectionSheet::titled(
            "③ 账户余额",
            "账户余额（资产 / 负债）",
            "余额一律填正数；后四列只有对应的科目才填（见「怎么填」）",
            &[
                "科目编码",
                "科目名称",
                "余额",
                "数量",
                "客户/供应商编码",
                "存货编码",
                "银行账户",
            ],
            &[
                Col::Text,
                Col::Text,
                Col::Money,
                Col::Money,
                Col::Text,
                Col::Text,
                Col::Text,
            ],
        )
        .with_rows(bal_rows),
    );

    // ④ 基本资料
    out.push(
        CollectionSheet::titled(
            "④ 基本资料",
            "基本资料",
            "实收资本照营业执照填写；本年利润填不出来就留空，导入时按差额轧",
            &["项目", "金额"],
            &[Col::Text, Col::Money],
        )
        .with_rows(vec![
            vec!["实收资本".to_string(), String::new()],
            vec!["其中：本年利润（没有就留空）".to_string(), String::new()],
        ]),
    );

    out
}

/// 一张纸的导入结果。
#[derive(Debug, Clone, Default)]
pub struct SheetOut {
    pub sheet: String,
    pub ok: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
    pub total: Money,
}

/// 整本收集表的导入结果。
#[derive(Debug, Clone, Default)]
pub struct CollectionImport {
    pub ar: SheetOut,
    pub ap: SheetOut,
    pub balances: SheetOut,
    pub equity: SheetOut,
    /// 借方合计（应收 + 资产类账户）
    pub debit_total: Money,
    /// 贷方合计（应付 + 欠薪欠税 + 实收资本 + 本年利润）
    pub credit_total: Money,
    /// 自动补写的本年利润（用户没填、靠轧差补齐时非零）
    pub auto_profit: Money,
    pub arap_ok: usize,
    pub begin_ok: usize,
}

/// 表名/列名归一：只留字母与汉字（去掉序号 ①、空格、括号、标点、数字），
/// 于是「① 客户应收」「客户应收」「1-客户应收」都归到同一个 key。
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphabetic()).collect()
}

fn cell(row: &[String], i: usize) -> String {
    row.get(i).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// 数字串里去掉千分位、货币符号（Excel 里手填的钱常常是 `1,200.00` 或 `¥1200`）。
fn money_of(s: &str) -> Option<Money> {
    let t: String = s
        .chars()
        .filter(|c| !matches!(c, ',' | '，' | '¥' | '￥' | '$' | ' ' | '\u{3000}'))
        .collect();
    if t.is_empty() || t == "-" || t == "—" {
        return None;
    }
    Money::parse(&t).ok()
}

/// 找出表头所在行：返回 `(表头行号, 各列列名)`。
/// 找表头行。
///
/// 两个条件缺一不可：**至少两个非空格**、且含「编码 / 项目」这类列名。
///
/// 只看关键词是不够的：模板在表头上方还有标题行和一行说明文字，说明里随口提一句
/// 「科目」就能把这行认成表头。于是真正的表头变成「数据第一行」被跳过，而数据行
/// 用兜底列号解析 —— 编码和余额恰好还在原位，**多出来的列（数量/存货/银行）却全丢**，
/// 而且一声不响。v1.8.43 之前就是这么丢的。
fn find_header(rows: &[Vec<String>]) -> Option<(usize, Vec<String>)> {
    for (i, r) in rows.iter().enumerate().take(12) {
        let filled = r.iter().filter(|c| !c.trim().is_empty()).count();
        if filled < 2 {
            continue;
        }
        let joined: String = r.join("");
        if joined.contains("编码") || joined.contains("项目") {
            return Some((i, r.iter().map(|s| s.trim().to_string()).collect()));
        }
    }
    None
}

/// 按列名取列号；找不到就用兜底位置。表头被改过顺序也认。
fn col(head: &[String], names: &[&str], fallback: usize) -> usize {
    for (i, h) in head.iter().enumerate() {
        let hn = norm(h);
        if names.iter().any(|n| hn == norm(n)) {
            return i;
        }
    }
    fallback
}

/// 期初单据的日期：账套启用期间的前一天（11 月启用的账套，期初就是 10-31）。
fn cutoff_date(db: &Db) -> String {
    let start = db.options().start_period;
    let (y, m) = (start.year(), start.month());
    if m == 1 {
        format!("{}-12-31", y - 1)
    } else {
        // 上个月最后一天
        let (ny, nm) = (y, m - 1);
        let last = match nm {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            _ => {
                if (ny % 4 == 0 && ny % 100 != 0) || ny % 400 == 0 {
                    29
                } else {
                    28
                }
            }
        };
        format!("{ny}-{nm:02}-{last:02}")
    }
}

/// 一张「往来」纸（①/②）的解析结果。
struct PartySheets {
    lines: Vec<(String, String, Money, String, String)>,
    out: SheetOut,
}

fn parse_parties(rows: &[Vec<String>], sheet: &str, code_names: &[&str]) -> PartySheets {
    let mut out = SheetOut {
        sheet: sheet.to_string(),
        ..Default::default()
    };
    let mut lines = Vec::new();
    let Some((hr, head)) = find_header(rows) else {
        out.warnings.push(format!("「{sheet}」找不到表头，已跳过"));
        return PartySheets { lines, out };
    };
    let c_code = col(&head, code_names, 0);
    let c_name = col(&head, &["名称", "客户名称", "供应商名称"], 1);
    let c_amt = col(&head, &["欠款金额", "金额", "余额"], 2);
    let c_doc = col(&head, &["单据号", "单据号（可空）"], 3);
    let c_memo = col(&head, &["备注"], 4);
    for (i, r) in rows.iter().enumerate().skip(hr + 1) {
        let code = cell(r, c_code);
        let name = cell(r, c_name);
        let amt_s = cell(r, c_amt);
        let memo = cell(r, c_memo);
        // 合计行（"应收合计"在名称列）与提示行（"只填有欠款的…"）：编码列空 → 跳过且不抱怨
        if code.is_empty() || !code.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        if code.starts_with('=') || name.contains("合计") || name.contains("合计") {
            continue;
        }
        let Some(amt) = money_of(&amt_s) else {
            if !amt_s.is_empty() {
                out.warnings.push(format!(
                    "「{sheet}」第 {} 行：金额「{amt_s}」不是数字，已跳过",
                    i + 1
                ));
                out.skipped += 1;
            }
            continue;
        };
        if !amt.is_positive() {
            out.skipped += 1;
            continue;
        }
        let doc = {
            let d = cell(r, c_doc);
            if d.is_empty() {
                format!("QC-{code}")
            } else {
                d
            }
        };
        lines.push((code, name, amt, doc, memo));
        out.total += amt;
        out.ok += 1;
    }
    PartySheets { lines, out }
}

/// 要写进 `begin_balance` 的一行（含辅助核算与数量）。
struct BeginLine {
    code: String,
    signed: Money,
    aux: fincore::voucher::AuxRef,
    qty: Option<Money>,
}

/// ③ 账户余额的一行。
///
/// 后四个字段是 v1.8.43 才读的：没有它们，期初的「分存货数量 / 分银行账户 /
/// 分客户预收」全都进不了库 —— 表格里明明填了，导进去看不见，而且**不报错**。
struct BalLine {
    code: String,
    amount: Money,
    qty: Option<Money>,
    aux: fincore::voucher::AuxRef,
}

/// 一张「科目余额」纸（③）的解析结果。
fn parse_balances(rows: &[Vec<String>]) -> Vec<BalLine> {
    let mut out = Vec::new();
    let Some((hr, head)) = find_header(rows) else {
        return out;
    };
    let c_code = col(&head, &["科目编码", "编码"], 0);
    let c_amt = col(&head, &["余额", "金额"], 2);
    let c_qty = col(&head, &["数量", "期初数量", "数量期初"], usize::MAX);
    let c_cus = col(&head, &["客户编码", "客户"], usize::MAX);
    let c_sup = col(&head, &["供应商编码", "供应商"], usize::MAX);
    // 「客户/供应商编码」是一列两用：按科目方向决定它指的是哪一头。
    let c_party = col(
        &head,
        &["客户/供应商编码", "客商编码", "客户或供应商编码"],
        usize::MAX,
    );
    let c_item = col(&head, &["存货编码", "商品编码"], usize::MAX);
    let c_bank = col(&head, &["银行账户", "账户编码"], usize::MAX);
    for r in rows.iter().skip(hr + 1) {
        let code = cell(r, c_code);
        if code.is_empty() || !code.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let Some(m) = money_of(&cell(r, c_amt)) else {
            continue;
        };
        if m.is_zero() {
            continue;
        }
        let party = cell(r, c_party);
        let mut aux = fincore::voucher::AuxRef::default();
        if !cell(r, c_cus).is_empty() {
            aux.customer = Some(cell(r, c_cus));
        }
        if !cell(r, c_sup).is_empty() {
            aux.supplier = Some(cell(r, c_sup));
        }
        if !cell(r, c_item).is_empty() {
            aux.item = Some(cell(r, c_item));
        }
        if !cell(r, c_bank).is_empty() {
            aux.bank = Some(cell(r, c_bank));
        }
        // 一列两用，按**科目家族**判它指的是谁 —— 不能按「资产挂客户、负债挂供应商」：
        // 2203 预收账款是负债，可它挂的明明是客户；1123 预付账款是资产，挂的却是供应商。
        // 判错的后果不是报错，是把一个客户编码挂成供应商编码，然后对账时「档案里找不到」。
        if !party.is_empty() {
            let to_customer = code.starts_with("1122") || code.starts_with("2203");
            if to_customer {
                if aux.customer.is_none() {
                    aux.customer = Some(party);
                }
            } else if aux.supplier.is_none() {
                aux.supplier = Some(party);
            }
        }
        out.push(BalLine {
            code,
            amount: m,
            qty: money_of(&cell(r, c_qty)),
            aux,
        });
    }
    out
}

/// 一张「基本资料」纸（④）的解析结果：按项目名找金额。
fn parse_pairs(rows: &[Vec<String>]) -> Vec<(String, Money)> {
    let mut out = Vec::new();
    for r in rows.iter() {
        let label = cell(r, 0);
        if norm(&label).is_empty() {
            continue;
        }
        if let Some(m) = money_of(&cell(r, 1)) {
            out.push((label, m));
        }
    }
    out
}

/// 按表名关键字找纸；名字被改过就退回按顺序（第 1 张是「怎么填」，数据纸从索引 1 起）。
fn find_sheet<'a>(
    sheets: &'a [(String, Vec<Vec<String>>)],
    keys: &[&str],
    nth: usize,
) -> Option<&'a (String, Vec<Vec<String>>)> {
    if let Some(hit) = sheets
        .iter()
        .find(|(name, _)| keys.iter().any(|k| norm(name).contains(&norm(k))))
    {
        return Some(hit);
    }
    sheets.get(nth)
}

/// 导入整本《期初收集表》。返回每张纸的明细统计；不平衡直接报错不落库。
pub fn import_collection_sheet(db: &Db, bytes: &[u8], who: &str) -> DbResult<CollectionImport> {
    collection_sheet_impl(db, bytes, who, false)
}

/// 只体检、不落库。
///
/// 期初数据是**边整理边填**的：今天填了客户应收，后天补银行账户。整本填完之前
/// 谁也不知道平不平，而唯一能验证的办法以前只有「导一次试试」—— 导进去一半、
/// 发现不平、再想办法清掉，成本高到没人愿意试第二次。
///
/// 所以把「解析 + 校验 + 算平」和「落库」拆开：预检走同一条代码路径，只是不写。
pub fn preview_collection_sheet(db: &Db, bytes: &[u8]) -> DbResult<CollectionImport> {
    collection_sheet_impl(db, bytes, "", true)
}

fn collection_sheet_impl(
    db: &Db,
    bytes: &[u8],
    who: &str,
    dry_run: bool,
) -> DbResult<CollectionImport> {
    let sheets = crate::imports::read_xlsx_sheets(bytes)?;
    let mut res = CollectionImport::default();

    let ar_sheet = find_sheet(&sheets, &["客户应收", "应收"], 1);
    let ap_sheet = find_sheet(&sheets, &["供应商应付", "应付"], 2);
    let bal_sheet = find_sheet(&sheets, &["账户余额", "账户"], 3);
    let eq_sheet = find_sheet(&sheets, &["基本资料", "实收资本"], 4);

    // ---- 预检：四张纸是不是都在。缺纸就明说缺哪张，不要「导成功了但什么都没导」
    if ar_sheet.is_none() || ap_sheet.is_none() || bal_sheet.is_none() {
        return Err(fincore::FinError::validate(
            "这个工作簿不像《期初收集表》：应含「① 客户应收」「② 供应商应付」「③ 账户余额」三张纸",
        )
        .into());
    }

    // ---- ① ② 往来
    let ar = parse_parties(
        &ar_sheet.expect("已判非空").1,
        &ar_sheet.expect("已判非空").0,
        &["客户编码", "编码"],
    );
    let ap = parse_parties(
        &ap_sheet.expect("已判非空").1,
        &ap_sheet.expect("已判非空").0,
        &["供应商编码", "编码"],
    );
    res.ar = ar.out;
    res.ap = ap.out;

    let chart = crate::accounts::chart(db)?;

    // ---- ③ 账户余额；同时分资产/负债
    let bal_rows = bal_sheet.map(|(_, r)| parse_balances(r)).unwrap_or_default();
    let mut begin_rows: Vec<BeginLine> = Vec::new();
    for line in &bal_rows {
        let code = &line.code;
        let amt = &line.amount;
        if !chart.contains(code) {
            res.balances
                .warnings
                .push(format!("账户余额：科目 {code} 不在科目表里，已跳过"));
            res.balances.skipped += 1;
            continue;
        }
        if !chart.is_leaf(code) {
            res.balances
                .warnings
                .push(format!("账户余额：{code} 不是末级科目，期初要填在末级上，已跳过"));
            res.balances.skipped += 1;
            continue;
        }
        // 资产类（1 开头）记借方；负债/权益（2/3/4 开头）记贷方
        let signed = if code.starts_with('1') {
            *amt
        } else if code.starts_with('2') || code.starts_with('3') || code.starts_with('4') {
            -*amt
        } else {
            *amt
        };
        // 辅助核算编码必须在档案里，否则那一行会变成一个谁也查不到的孤儿期初。
        // 注意这里用 `bad` 跳整行：`continue` 只能跳出这个内层 for，行照样会写进去。
        let mut bad = false;
        for (label, code_opt, kind) in [
            ("客户编码", &line.aux.customer, fincore::AuxKind::Customer),
            (
                "供应商编码",
                &line.aux.supplier,
                fincore::AuxKind::Supplier,
            ),
            ("存货编码", &line.aux.item, fincore::AuxKind::Item),
            ("银行账户", &line.aux.bank, fincore::AuxKind::Bank),
        ] {
            let Some(c) = code_opt else { continue };
            let found = crate::auxs::list(
                db,
                &fincore::auxiliary::AuxQuery {
                    kind: Some(kind),
                    ..Default::default()
                },
            )?
            .into_iter()
            .any(|e| &e.code == c);
            if !found {
                res.balances.warnings.push(format!(
                    "账户余额：{code} 的{label}「{c}」在档案里找不到，这一行已跳过"
                ));
                res.balances.skipped += 1;
                bad = true;
                break;
            }
        }
        if bad {
            continue;
        }
        // 核算数量的科目，只填金额不填数量 → 以后出库结存必错
        if chart.get(code).is_some_and(|a| a.has_qty) && line.qty.is_none() {
            res.balances.warnings.push(format!(
                "账户余额：{code} 核算数量但没填数量，已按「只有金额」导入 —— 请补数量"
            ));
        }
        begin_rows.push(BeginLine {
            code: code.clone(),
            signed,
            aux: line.aux.clone(),
            qty: line.qty,
        });
        res.balances.ok += 1;
        res.balances.total += *amt;
    }

    // ---- ④ 基本资料：只认实收资本与本年利润
    let mut paid_in = Money::ZERO;
    let mut profit = Money::ZERO;
    let mut has_profit = false;
    if let Some((_, rows)) = eq_sheet {
        for (label, amt) in parse_pairs(rows) {
            let n = norm(&label);
            if n.contains("实收资本") {
                paid_in = amt;
                res.equity.ok += 1;
            } else if n.contains("本年利润") || n.contains("利润") {
                profit = amt;
                has_profit = !amt.is_zero();
                if has_profit {
                    res.equity.ok += 1;
                }
            }
        }
    }
    if !paid_in.is_zero() {
        begin_rows.push(BeginLine {
            code: "4001".to_string(),
            signed: -paid_in,
            aux: Default::default(),
            qty: None,
        });
    }
    if has_profit {
        begin_rows.push(BeginLine {
            code: "4103".to_string(),
            signed: -profit,
            aux: Default::default(),
            qty: None,
        });
    }

    // ---- 平衡：逐条把 begin_rows 按方向加总（资产类记借，其余记贷），
    //      再加上「应收台账合计」「应付台账合计」这两个还没进 begin_rows 的数。
    // 按**余额符号**分借贷，不能按科目首位。
    //
    // 4103 本年利润以「4」开头，但它完全可能是借方余额（亏了）—— 按首位一律当贷方，
    // 会把亏损加到贷方去，于是「借方 65,000 = 贷方 65,000」这种**两边都错、还能对上**的
    // 合计就打印出来了，而库里实际是 71,000。
    let tally = |rows: &[BeginLine]| {
        let mut d = res.ar.total;
        let mut c = res.ap.total;
        for line in rows {
            if line.signed.is_positive() {
                d += line.signed;
            } else {
                c += -line.signed;
            }
        }
        (d, c)
    };
    let (mut debit, mut credit) = tally(&begin_rows);

    // 若用户只填了实收资本、没填本年利润，用差额补齐 4103（收集表④本身也是这么轧的）
    if !has_profit && !paid_in.is_zero() {
        let diff = debit - credit;
        if !diff.is_zero() {
            profit += diff;
            res.auto_profit = diff;
            begin_rows.push(BeginLine {
                code: "4103".to_string(),
                signed: -profit,
                aux: Default::default(),
                qty: None,
            });
            res.equity.warnings.push(format!(
                "「本年利润」没填，已按轧差补 {} —— 请核对这个数是前期赚的（或亏的）",
                profit.fmt_money()
            ));
        }
    }

    // 轧差那一行进来之后必须重算：它是**借**方（亏损）还是**贷**方（利润）取决于
    // diff 的正负，靠「credit += diff」这种单边修补必然算错一边，于是明明已经平了
    // 却报「借贷不平」，或者回显的合计和库里的对不上。
    let (d2, c2) = tally(&begin_rows);
    debit = d2;
    credit = c2;

    res.debit_total = debit;
    res.credit_total = credit;
    if debit != credit {
        let diff = debit - credit;
        return Err(fincore::FinError::validate(format!(
            "期初借贷不平，整本没有导入。\n\
             借方合计 {}（应收 {} + 资产账户），贷方合计 {}（应付 {} + 负债 + 实收资本 + 本年利润），差额 {}。\n\
             常见原因：漏填某个银行账户余额、应收/应付填了合计但明细没填全、\
             或者「实收资本」与「本年利润」填了但资产那边没对应上。\
             把差额补到「④ 基本资料 → 其中：本年利润 4103」一般就能平。",
            debit.fmt_money(),
            res.ar.total.fmt_money(),
            credit.fmt_money(),
            res.ap.total.fmt_money(),
            diff.fmt_money()
        ))
        .into());
    }

    // ---- 预检到此为止：解析、校验、算平都做完了，但不写
    if dry_run {
        return Ok(res);
    }

    // ---- 全部通过，开始落库
    let cutoff = cutoff_date(db);
    let mut arap_ok = 0usize;
    let mut dup = 0usize;
    for (kind, lines) in [("ar", &ar.lines), ("ap", &ap.lines)] {
        for (code, name, amt, doc, memo) in lines.iter() {
            if crate::settle::arap_opening_exists(db, kind, code, doc)? {
                dup += 1;
                continue;
            }
            crate::settle::arap_opening_insert(
                db,
                &crate::settle::ArapOpening {
                    id: 0,
                    kind: kind.to_string(),
                    party_code: code.clone(),
                    party_name: name.clone(),
                    doc_no: doc.clone(),
                    doc_date: cutoff.clone(),
                    amount: *amt,
                    memo: memo.clone(),
                    created_by: String::new(),
                },
                who,
            )?;
            arap_ok += 1;
        }
    }
    res.arap_ok = arap_ok;
    // 单据号重复的落不到台账里，但科目期初照算（金额本来就在合计里）
    res.ar.skipped += dup;
    // 往来合计落到科目期初
    if res.ar.total.is_positive() {
        begin_rows.push(BeginLine {
            code: "112201".to_string(),
            signed: res.ar.total,
            aux: Default::default(),
            qty: None,
        });
    }
    if res.ap.total.is_positive() {
        begin_rows.push(BeginLine {
            code: "220201".to_string(),
            signed: -res.ap.total,
            aux: Default::default(),
            qty: None,
        });
    }

    for line in &begin_rows {
        crate::balances::upsert_begin(
            db,
            &crate::balances::BeginRow {
                id: 0,
                account_code: line.code.clone(),
                aux: line.aux.clone(),
                year_begin: line.signed,
                debit_accum: Money::ZERO,
                credit_accum: Money::ZERO,
                qty_begin: line.qty,
            },
        )?;
        res.begin_ok += 1;
    }

    db.log(
        who,
        "导入",
        "导入期初收集表",
        &format!(
            "应收 {} 条 / 应付 {} 条 / 科目期初 {} 条",
            res.ar.ok, res.ap.ok, res.begin_ok
        ),
    )?;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn money_tolerates_thousands_and_symbols() {
        assert_eq!(money_of("1,200.00"), Some(Money::parse("1200").unwrap()));
        assert_eq!(money_of("¥1200"), Some(Money::parse("1200").unwrap()));
        assert_eq!(money_of("  "), None);
        assert_eq!(money_of("—"), None);
        assert_eq!(money_of("1200元"), None);
    }

    /// ③ 的后四列（数量 / 客商 / 存货 / 银行账户）必须真的读进去。
    ///
    /// 这四列曾经是「模板上有、导入器不认」—— 表格里填得好好的，导进去看不见，
    /// 而且不报错：存货有金额没数量，第一次出库结存就是负数。
    #[test]
    fn balances_sheet_keeps_quantity_item_bank_and_party() {
        let rows = vec![
            s(&["科目编码", "科目名称", "余额", "数量", "客户/供应商编码", "存货编码", "银行账户"]),
            s(&["140501", "库存商品", "5000", "2.5", "", "HG001", ""]),
            s(&["100201", "工行对公", "8000", "", "", "", "B01"]),
            s(&["2203", "预收账款", "8000", "", "C002", "", ""]),
            s(&["1123", "预付账款", "3000", "", "S001", "", ""]),
        ];
        let out = parse_balances(&rows);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].qty, Some(Money::parse("2.5").unwrap()));
        assert_eq!(out[0].aux.item.as_deref(), Some("HG001"));
        assert_eq!(out[1].aux.bank.as_deref(), Some("B01"));
        // 2203 预收是负债，但它挂的明明是客户
        assert_eq!(out[2].aux.customer.as_deref(), Some("C002"));
        assert_eq!(out[2].aux.supplier, None, "预收账款不能把客户挂成供应商");
        // 1123 预付是资产，挂的却是供应商
        assert_eq!(out[3].aux.supplier.as_deref(), Some("S001"));
        assert_eq!(out[3].aux.customer, None, "预付账款不能把供应商挂成客户");
    }

    #[test]
    fn header_is_found_below_the_title_row() {
        let rows = vec![
            s(&["客户欠款（应收）· 截止 2026-10-31"]),
            s(&["客户编码", "客户名称", "欠款金额", "单据号（可空）", "备注"]),
            s(&["C001", "华达", "50000", "", ""]),
        ];
        let (hr, head) = find_header(&rows).expect("应找到表头");
        assert_eq!(hr, 1);
        assert_eq!(col(&head, &["客户编码", "编码"], 0), 0);
        assert_eq!(col(&head, &["欠款金额", "金额"], 2), 2);
    }

    #[test]
    fn columns_are_matched_by_name_not_position() {
        // 工厂把「单据号」和「备注」调了顺序，按位置取就会串
        let rows = vec![
            s(&["客户编码", "客户名称", "欠款金额", "备注", "单据号（可空）"]),
            s(&["C001", "华达", "50000", "月结", "YS-1"]),
        ];
        let p = parse_parties(&rows, "① 客户应收", &["客户编码", "编码"]);
        assert_eq!(p.lines.len(), 1);
        let (code, name, amt, doc, memo) = &p.lines[0];
        assert_eq!(code, "C001");
        assert_eq!(name, "华达");
        assert_eq!(*amt, Money::parse("50000").unwrap());
        assert_eq!(doc, "YS-1", "单据号必须按列名取");
        assert_eq!(memo, "月结");
    }

    #[test]
    fn blank_amount_rows_are_skipped_without_noise() {
        let rows = vec![
            s(&["客户编码", "客户名称", "欠款金额", "单据号（可空）", "备注"]),
            s(&["C001", "华达", "0", "", ""]),
            s(&["", "", "", "", ""]),
            s(&["", "应收合计", "0", "", ""]),
            s(&["", "只填有欠款的，没欠款留空即可", "", "", ""]),
        ];
        let p = parse_parties(&rows, "① 客户应收", &["客户编码", "编码"]);
        assert!(p.lines.is_empty(), "没有欠款不该产生单据");
        assert!(p.out.warnings.is_empty(), "空行不该报错：{:?}", p.out.warnings);
    }

    #[test]
    fn empty_doc_no_gets_a_generated_one() {
        let rows = vec![
            s(&["客户编码", "客户名称", "欠款金额", "单据号（可空）", "备注"]),
            s(&["C007", "鑫盛", "1234.56", "", ""]),
        ];
        let p = parse_parties(&rows, "① 客户应收", &["客户编码", "编码"]);
        assert_eq!(p.lines[0].3, "QC-C007", "单据号空着必须补一个，否则台账里分不出两笔");
    }

    #[test]
    fn cutoff_is_the_day_before_the_start_period() {
        // 纯算法校验：11 月启用 → 10-31；1 月启用 → 上年 12-31；3 月启用 → 2 月末（闰年 2-29）
        assert_eq!(last_day_of_prev(2026, 11), "2026-10-31");
        assert_eq!(last_day_of_prev(2026, 1), "2025-12-31");
        assert_eq!(last_day_of_prev(2026, 3), "2026-02-28");
        assert_eq!(last_day_of_prev(2024, 3), "2024-02-29");
    }

    fn last_day_of_prev(y: i32, m: u32) -> String {
        if m == 1 {
            return format!("{}-12-31", y - 1);
        }
        let (ny, nm) = (y, m - 1);
        let last = match nm {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            _ => {
                if (ny % 4 == 0 && ny % 100 != 0) || ny % 400 == 0 {
                    29
                } else {
                    28
                }
            }
        };
        format!("{ny}-{nm:02}-{last:02}")
    }
}

#[cfg(test)]
mod template_tests {
    use super::*;
    use fincore::account::{AcctCategory, Account, Chart, CodeScheme};
    use fincore::AuxKind;

    fn acct(code: &str, name: &str, cat: AcctCategory) -> Account {
        let mut a = Account::new(code, name, cat);
        a.dir = cat.default_dir();
        a
    }

    fn chart_accounts() -> Vec<Account> {
        vec![
            acct("1001", "库存现金", AcctCategory::Asset),
            acct("1122", "应收账款", AcctCategory::Asset),
            acct("112201", "应收账款-客户", AcctCategory::Asset),
            acct("1123", "预付账款", AcctCategory::Asset),
            acct("1405", "库存商品", AcctCategory::Asset),
            acct("2202", "应付账款", AcctCategory::Liability),
            acct("220201", "应付账款-供应商", AcctCategory::Liability),
            acct("2203", "预收账款", AcctCategory::Liability),
            acct("2501", "长期借款", AcctCategory::Liability),
            acct("4001", "实收资本", AcctCategory::Equity),
            acct("5101", "制造费用", AcctCategory::Cost),
        ]
    }

    /// 四张纸的名字与列头是**导入契约**：客户端按名字找表、按列位取值。
    /// 改这里等于改接口，必须同步改 `import_collection_sheet` 的解析。
    #[test]
    fn collection_template_has_the_four_sheets_and_expected_headers() {
        let accounts = chart_accounts();
        let sheets = collection_template(&accounts, &[], &[]);
        let names: Vec<&str> = sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["怎么填", "① 客户应收", "② 供应商应付", "③ 账户余额", "④ 基本资料"]);

        let bal = sheets.iter().find(|s| s.name == "③ 账户余额").unwrap();
        assert_eq!(
            bal.headers,
            vec![
                "科目编码", "科目名称", "余额", "数量", "客户/供应商编码", "存货编码", "银行账户"
            ]
        );
        let eq = sheets.iter().find(|s| s.name == "④ 基本资料").unwrap();
        assert_eq!(eq.headers, vec!["项目", "金额"]);
        // 金额列要被标出来，否则导出成文本 Excel 里没法求和
        assert_eq!(bal.cols[2], Col::Money);
        assert_eq!(eq.cols[1], Col::Money);
    }

    /// ③ 里**只能**有 1xxx / 2xxx 的末级科目，且必须排除 1122/2202 ——
    /// 那两个由 ①② 两张纸生成，再填一遍就是双计，而双计之后借贷两边
    /// 各多一份、看起来还是平的，最难查。
    #[test]
    fn balances_sheet_excludes_generated_ar_and_ap_but_keeps_the_rest() {
        let accounts = chart_accounts();
        let sheets = collection_template(&accounts, &[], &[]);
        let bal = sheets.iter().find(|s| s.name == "③ 账户余额").unwrap();
        let codes: Vec<&str> = bal.rows.iter().map(|r| r[0].as_str()).collect();
        assert!(!codes.contains(&"1122"), "1122 由①生成，不能再填：{codes:?}");
        assert!(!codes.contains(&"112201"), "112201 也是应收，不能填：{codes:?}");
        assert!(!codes.contains(&"2202"), "2202 由②生成：{codes:?}");
        assert!(!codes.contains(&"220201"), "220201 也是应付：{codes:?}");
        // 低频科目必须在 —— 「在建工程、预付账款没进去」曾经是我的错觉，
        // 这条断言就是防止哪天有人为了「让表短一点」加白名单
        for c in ["1123", "2203", "2501", "1001"] {
            assert!(codes.contains(&c), "{c} 应当出现在③里：{codes:?}");
        }
        // 权益类不出现在③（由④生成）
        assert!(!codes.contains(&"4001"), "4001 由④生成：{codes:?}");
        // 成本类不是 1/2 开头，也不该在
        assert!(!codes.contains(&"5101"), "5101 不该进期初账户表：{codes:?}");
    }

    /// 客户/供应商档案存在时，①② 要预填成行（编码、名称、空金额），
    /// 空表则只有表头 —— 两种都要能导入
    #[test]
    fn customer_and_supplier_rows_are_prefilled() {
        let accounts = chart_accounts();
        let c = AuxEntity::new(AuxKind::Customer, "C001", "华达");
        let s = AuxEntity::new(AuxKind::Supplier, "S001", "宏源");
        let sheets = collection_template(&accounts, &[c], &[s]);
        let ar = sheets.iter().find(|x| x.name == "① 客户应收").unwrap();
        assert!(ar.rows.iter().any(|r| r.contains(&"C001".to_string())));
        let ap = sheets.iter().find(|x| x.name == "② 供应商应付").unwrap();
        assert!(ap.rows.iter().any(|r| r.contains(&"S001".to_string())));
        let _ = Chart::with_accounts(CodeScheme(vec![4, 2, 2]), accounts);
    }
}
