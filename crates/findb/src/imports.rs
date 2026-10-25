//! 从其他软件（金蝶 / 用友 / Excel 导出）导入数据
//!
//! 提供两种通用导入，复用账套既有校验保证数据一致：
//! - [`import_begin`]：期初余额表（科目编码, 方向, 金额）
//! - [`import_vouchers`]：凭证（日期, 凭证字, 摘要, 科目编码, 借方, 贷方）
//!
//! 支持 CSV 文本粘贴与 Excel 文件直接读取（.xlsx/.xls/.ods）。
//! 分隔符自动识别逗号 / 制表符 / 分号，支持引号包裹。
//! 提供"来源模板"（金蝶 / 用友 / 通用），自动按对应列顺序解析。

use std::io::Cursor;

use fincore::{AuxRef, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::OptionalExtension;

use crate::balances::{self, BeginRow};
use crate::vouchers;
use crate::{Db, DbResult};

/// 导入来源模板：不同软件导出的列顺序不同，自动适配
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ImportTemplate {
    /// 通用 CSV（当前默认格式）
    #[default]
    Generic,
    /// 金蝶导出格式
    Kingdee,
    /// 用友导出格式
    Yonyou,
}

impl ImportTemplate {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "kingdee" | "金蝶" | "kd" => ImportTemplate::Kingdee,
            "yonyou" | "用友" | "yy" => ImportTemplate::Yonyou,
            _ => ImportTemplate::Generic,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ImportTemplate::Kingdee => "金蝶",
            ImportTemplate::Yonyou => "用友",
            ImportTemplate::Generic => "通用",
        }
    }

    pub const ALL: &'static [ImportTemplate] = &[
        ImportTemplate::Generic,
        ImportTemplate::Kingdee,
        ImportTemplate::Yonyou,
    ];
}

/// 期初余额表行：统一提取后的标准化行（与来源模板无关）
struct BeginLine {
    code: String,
    direction: String,
    amount: Money,
    /// 累计借方（金蝶/用友有此列，通用格式无）
    debit_accum: Option<Money>,
    /// 累计贷方
    credit_accum: Option<Money>,
    /// 数量期初（数量核算科目才有，v1.8.22 起可从表里带）
    qty_begin: Option<Money>,
    /// 辅助核算维度（分客户/分存货的期初），v1.8.22 起可从表里带
    aux: AuxRef,
    /// 金额列无法识别的提示；非空表示本行金额不可信，调用方应丢弃该行
    warn: Vec<String>,
}

/// 凭证行：统一提取后的标准化行
struct VoucherLine {
    date: chrono::NaiveDate,
    word: String,
    no: Option<i32>,
    summary: String,
    code: String,
    debit: Money,
    credit: Money,
    /// 辅助核算（客户/银行等，CSV 里按列约定提取）
    aux: fincore::AuxRef,
    /// 金额列无法识别的提示；非空表示本行金额不可信，调用方应丢弃该行
    warn: Vec<String>,
}

/// 按模板从 CSV 行提取期初余额行
fn extract_begin_line(
    tmpl: ImportTemplate,
    f: &[String],
    map: &HeadMap,
) -> Option<BeginLine> {
    let code = f.first()?.trim().to_string();
    if code.is_empty() || !code.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut w = MoneyWarn::default();
    let mut line = match tmpl {
        ImportTemplate::Generic if map.has_header => {
            // 有表头就按列名取，不再靠「第 2 列是不是借/贷」猜是布局 A 还是 B。
            // 猜布局只对「没有表头的老文件」有意义 —— 文件里明明有列名，却还要
            // 靠位置猜，等于把用户能改的东西（列顺序）当成不能改的。
            let amt = w.money("期初余额", &map.cell(f, &["期初余额", "金额"], 3));
            let mut acc_of = |aliases: &[&str], pos: usize| -> Option<Money> {
                let v = map.cell(f, aliases, pos);
                if v.is_empty() {
                    None
                } else {
                    Some(w.money(aliases[0], &v))
                }
            };
            let debit_accum = acc_of(&["累计借方"], 4);
            let credit_accum = acc_of(&["累计贷方"], 5);
            BeginLine {
                code,
                direction: map.cell(f, &["方向", "余额方向"], 2),
                amount: amt,
                debit_accum,
                credit_accum,
                qty_begin: None,
                aux: AuxRef::default(),
                warn: Vec::new(),
            }
        }
        ImportTemplate::Generic => {
            // 通用模板实际有两种写法，**都必须认**：
            //   A) 科目编码, 方向, 金额
            //   B) 科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方
            //
            // 历史遗留：解析器最早只认 A，后来 `/api/import/template?kind=begin`
            // 导出的「通用」模板改成了 B（多一列科目名称、带上累计发生额，更像
            // 一张能核对的余额表），两边就一直错着。后果不是「少几列」而是
            // **整批丢行**：按 B 的表去喂 A 的解析器，方向读到「库存现金」、
            // 金额读到「借」，`MoneyWarn` 把「借」判成无法识别 → 每一行都进
            // warnings 被跳过。用户下载了官方模板、照着填、一条也导不进去，
            // 而提示是「期初余额「借」无法识别为金额」—— 看不出是列错位。
            //
            // 判据只看第 2 列是不是借/贷：B 的第 2 列是科目名称，不可能只有
            // 一个字「借」或「贷」；A 的第 2 列本来就是方向。不会误判。
            let c1 = f.get(1).map(|s| s.trim()).unwrap_or("");
            let c2 = f.get(2).map(|s| s.trim()).unwrap_or("");
            let looks_dir = |s: &str| {
                matches!(s, "借" | "贷" | "平" | "借方" | "贷方")
                    || s.eq_ignore_ascii_case("debit")
                    || s.eq_ignore_ascii_case("credit")
            };
            if !looks_dir(c1) && looks_dir(c2) {
                // 布局 B：科目名称在第 2 列
                let amt = w.money("期初余额", f.get(3).map(|s| s.as_str()).unwrap_or(""));
                let debit_accum = opt_field(f, 4).map(|s| w.money("累计借方", &s));
                let credit_accum = opt_field(f, 5).map(|s| w.money("累计贷方", &s));
                BeginLine {
                    code,
                    direction: c2.to_string(),
                    amount: amt,
                    debit_accum,
                    credit_accum,
                    qty_begin: None,
                    aux: AuxRef::default(),
                    warn: Vec::new(),
                }
            } else {
                // 布局 A
                if f.len() < 2 {
                    return None;
                }
                let amt = w.money("期初余额", f.get(2).map(|s| s.as_str()).unwrap_or(""));
                BeginLine {
                    code,
                    direction: c1.to_string(),
                    amount: amt,
                    debit_accum: None,
                    credit_accum: None,
                    qty_begin: None,
                    aux: AuxRef::default(),
                    warn: Vec::new(),
                }
            }
        }
        ImportTemplate::Kingdee => {
            // 科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方
            if f.len() < 4 {
                return None;
            }
            let amt = w.money("期初余额", f.get(3).map(|s| s.as_str()).unwrap_or(""));
            let debit_accum = f.get(4).map(|s| w.money("累计借方", s));
            let credit_accum = f.get(5).map(|s| w.money("累计贷方", s));
            BeginLine {
                code,
                direction: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                amount: amt,
                debit_accum,
                credit_accum,
                qty_begin: None,
                aux: AuxRef::default(),
                warn: Vec::new(),
            }
        }
        ImportTemplate::Yonyou => {
            // 科目编码, 科目名称, 期初借方, 期初贷方, 累计借方, 累计贷方
            if f.len() < 4 {
                return None;
            }
            let d = w.money("期初借方", f.get(2).map(|s| s.as_str()).unwrap_or(""));
            let c = w.money("期初贷方", f.get(3).map(|s| s.as_str()).unwrap_or(""));
            let debit_accum = f.get(4).map(|s| w.money("累计借方", s));
            let credit_accum = f.get(5).map(|s| w.money("累计贷方", s));
            let dir = if d > Money::ZERO { "借" } else { "贷" };
            let amt = if d > Money::ZERO { d } else { c };
            BeginLine {
                code,
                direction: dir.to_string(),
                amount: amt,
                debit_accum,
                credit_accum,
                qty_begin: None,
                aux: AuxRef::default(),
                warn: Vec::new(),
            }
        }
    };
    line.warn = std::mem::take(&mut w.0);
    // 数量期初与辅助核算维度（v1.8.22）。这两块原来一律丢：导出的期初表只有
    // 金额，导回来就是 `qty_begin: None`、`aux: AuxRef::default()`。数量核算
    // 科目（原材料/库存商品）搬过去数量期初归零，而金额是对的 —— 只有去看
    // 「数量金额式」明细才会发现数量是空的。
    let qty = map.cell(f, &["数量", "期初数量", "数量期初"], usize::MAX);
    if !qty.is_empty() {
        let mut wq = MoneyWarn::default();
        let m = wq.money("数量", &qty);
        if wq.0.is_empty() {
            line.qty_begin = Some(m);
        }
    }
    line.aux = AuxRef {
        customer: opt_aux(map.cell(f, &["客户编码", "客户"], usize::MAX)),
        supplier: opt_aux(map.cell(f, &["供应商编码", "供应商"], usize::MAX)),
        dept: opt_aux(map.cell(f, &["部门编码", "部门"], usize::MAX)),
        employee: opt_aux(map.cell(f, &["职员编码", "职员", "员工编码"], usize::MAX)),
        project: opt_aux(map.cell(f, &["项目编码", "项目"], usize::MAX)),
        item: opt_aux(map.cell(f, &["存货编码", "存货"], usize::MAX)),
        cash_flow: opt_aux(map.cell(f, &["现金流量项目编码"], usize::MAX)),
        bank: opt_aux(map.cell(f, &["银行账户", "银行账户编码"], usize::MAX)),
    };
    Some(line)
}

/// 空串 → None。辅助维度就是这样：表里留空表示「不分这个维度」。
fn opt_aux(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 取 CSV 第 `i` 列（0 起），空串返回 None
fn opt_field(f: &[String], i: usize) -> Option<String> {
    f.get(i)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 表头归一化：去掉空格、全角空格、必填星号、冒号，英文统一小写。
///
/// 「税点%」和「税点 %」要认成同一列；「编码*」（模板里标必填的写法）也要认。
fn normalize_header(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '*' && *c != '：' && *c != ':')
        .collect::<String>()
        .to_lowercase()
}

/// 整行是不是表头：只要有一格是「编码」类词就算。
///
/// 不能只看某一列 —— 表头的列顺序用户是可以改的（这正是 `HeadMap` 的意义）。
/// 数据行里不会有一格恰好写着「编码」两个字。
fn row_is_header(f: &[String]) -> bool {
    f.iter().any(|c| is_header_code(c.trim()))
}

/// 这一格是不是「编码」类的表头词（用来认出表头行）。
fn is_header_code(s: &str) -> bool {
    matches!(s, "编码" | "科目编码" | "存货编码" | "代码" | "编号")
        || s.eq_ignore_ascii_case("code")
}

/// 列名 → 列下标。
///
/// 基础资料的四个导入器（辅助档案 / 存货 / 科目 / 期初）最早都按**列下标**
/// 取值。于是只要导出端多加一列，或者用户在 Excel 里删列、调换顺序，数据就
/// 开始错位 —— 而错位通常不报错：备注被写进上级编码，税点被当成信用额度。
/// 账套看着搬成功了，对账时才发现。
///
/// 现在先认表头再退回位置，于是：
/// - 没有表头的老文件（四列/六列）行为完全不变，照旧能导；
/// - 我们自己导出的表，列顺序随便换、列可以删，都不会错位；
/// - 用户插一列自己的「备注2」也不会把后面的列顶歪。
#[derive(Default)]
struct HeadMap {
    idx: std::collections::HashMap<String, usize>,
    /// 这份文件到底有没有表头。
    ///
    /// 这个字段是必需的，不是冗余：**有表头**时缺列必须当成「空」，缺列就说明
    /// 用户删了它；**没有表头**时才能退回按位置猜。混起来会出真错 —— 曾经
    /// 一个把「备注」挪到第一列的表，因为少了上级编码这一列，退回按位置取到了
    /// 编码列，于是「上级编码 = 自己」，导进去报「会形成环，层级汇总无法收敛」。
    has_header: bool,
}

impl HeadMap {
    /// 同一列名出现多次取最左边那个（用户在 Excel 里复制出一列时，左边是他
    /// 原本填的那列）。
    fn from_row(row: &[String]) -> Self {
        let mut idx = std::collections::HashMap::new();
        for (i, raw) in row.iter().enumerate() {
            let k = normalize_header(raw);
            if k.is_empty() {
                continue;
            }
            idx.entry(k).or_insert(i);
        }
        Self {
            idx,
            has_header: true,
        }
    }

    /// 按别名取单元格。
    ///
    /// - 表里有表头：只认列名，**找不到就是空**（用户删了这一列）；
    /// - 表里没表头：退回 `pos`（老格式的位置），行为与升级前一致。
    fn cell(&self, f: &[String], aliases: &[&str], pos: usize) -> String {
        for a in aliases {
            if let Some(i) = self.idx.get(&normalize_header(a)) {
                return master_cell(f, *i);
            }
        }
        if self.has_header {
            return String::new();
        }
        master_cell(f, pos)
    }
}

/// 「是 / 否」类单元格：认得的是、Y、yes、1、√。空 = 否。
fn is_yes(s: &str) -> bool {
    matches!(
        s.trim(),
        "是" | "有" | "Y" | "y" | "yes" | "YES" | "1" | "√" | "true" | "TRUE"
    )
}

/// 辅助核算维度文字 → `AuxKind`。认中文标签也认英文码，认不出返回 None。
fn aux_kind_of_dim(s: &str) -> Option<fincore::AuxKind> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    match t {
        "客户" => Some(fincore::AuxKind::Customer),
        "供应商" => Some(fincore::AuxKind::Supplier),
        "部门" => Some(fincore::AuxKind::Dept),
        "职员" | "员工" => Some(fincore::AuxKind::Employee),
        "项目" => Some(fincore::AuxKind::Project),
        "存货" | "商品" => Some(fincore::AuxKind::Item),
        "现金流量" | "现金流量项目" => Some(fincore::AuxKind::CashFlow),
        "银行账户" | "银行" => Some(fincore::AuxKind::Bank),
        _ => aux_kind_of(t),
    }
}

/// 「客户,存货」→ AuxMask。分隔符认中英文逗号、顿号、斜杠、分号。
fn parse_aux_mask(s: &str) -> fincore::account::AuxMask {
    let mut m = fincore::account::AuxMask::NONE;
    for part in s.split(|c: char| c == ',' || c == '，' || c == '、' || c == '/' || c == ';' || c == '；') {
        if let Some(k) = aux_kind_of_dim(part) {
            m = m.with(k);
        }
    }
    m
}

/// 按模板从 CSV 行提取凭证行
fn extract_voucher_line(tmpl: ImportTemplate, f: &[String]) -> Option<VoucherLine> {
    if f.len() < 4 {
        return None;
    }
    let date = parse_date(&f[0])?;
    let mut aux = fincore::AuxRef::default();
    let mut w = MoneyWarn::default();
    let mut line = match tmpl {
        ImportTemplate::Generic => {
            // 日期, 凭证字, 摘要, 科目编码, 借方, 贷方[, 客户, 银行]
            aux.customer = opt_field(f, 6);
            aux.bank = opt_field(f, 7);
            let debit = w.money("借方", f.get(4).map(|s| s.as_str()).unwrap_or(""));
            let credit = w.money("贷方", f.get(5).map(|s| s.as_str()).unwrap_or(""));
            VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: None,
                summary: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                debit,
                credit,
                aux,
                warn: Vec::new(),
            }
        }
        ImportTemplate::Kingdee => {
            // 日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方[, 客户, 银行]
            aux.customer = opt_field(f, 8);
            aux.bank = opt_field(f, 9);
            let debit = w.money("借方", f.get(6).map(|s| s.as_str()).unwrap_or(""));
            let credit = w.money("贷方", f.get(7).map(|s| s.as_str()).unwrap_or(""));
            VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: f.get(2).and_then(|s| s.trim().parse().ok()),
                summary: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(4).unwrap_or(&String::new()).trim().to_string(),
                debit,
                credit,
                aux,
                warn: Vec::new(),
            }
        }
        ImportTemplate::Yonyou => {
            // 日期, 凭证字号, 摘要, 科目编码, 借方, 贷方[, 客户, 银行]
            aux.customer = opt_field(f, 6);
            aux.bank = opt_field(f, 7);
            let debit = w.money("借方", f.get(4).map(|s| s.as_str()).unwrap_or(""));
            let credit = w.money("贷方", f.get(5).map(|s| s.as_str()).unwrap_or(""));
            VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: None,
                summary: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                debit,
                credit,
                aux,
                warn: Vec::new(),
            }
        }
    };
    line.warn = std::mem::take(&mut w.0);
    Some(line)
}

/// 读取 Excel 文件（.xlsx/.xls/.ods）第一个 sheet，返回所有行（每行是单元格列表）
pub fn read_xlsx(path: &std::path::Path) -> Result<Vec<Vec<String>>, fincore::FinError> {
    let bytes =
        std::fs::read(path).map_err(|e| fincore::FinError::io(format!("读取文件失败：{e}")))?;
    read_xlsx_bytes(&bytes)
}

/// xlsx/ods 本质是 zip：解压前先看各条目声明的解压后大小合计，拒绝解压炸弹
/// （上传限制只看压缩后体积，一个 2MB 的 xlsx 可膨胀到 GB 级内存）。
fn check_zip_bomb(bytes: &[u8]) -> Result<(), fincore::FinError> {
    const MAX_UNCOMPRESSED: u64 = 128 * 1024 * 1024;
    let is_zip = bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x06\x06");
    if !is_zip {
        return Ok(()); // .xls（OLE2）等非 zip 格式交给 calamine
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| fincore::FinError::io(format!("打开 Excel 失败：{e}")))?;
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let f = zip
            .by_index(i)
            .map_err(|e| fincore::FinError::io(format!("读取 Excel 失败：{e}")))?;
        total = total.saturating_add(f.size());
        if total > MAX_UNCOMPRESSED {
            return Err(fincore::FinError::validate(format!(
                "Excel 解压后体积过大（超过 {} MB），已拒绝导入",
                MAX_UNCOMPRESSED / 1024 / 1024
            )));
        }
    }
    Ok(())
}

/// 从字节读取 Excel（.xlsx/.xls/.ods）第一个 sheet，返回所有行（每行是单元格列表）
///
/// Web 端可直接把上传的文件字节交给本函数，无需落盘。
pub fn read_xlsx_bytes(bytes: &[u8]) -> Result<Vec<Vec<String>>, fincore::FinError> {
    check_zip_bomb(bytes)?;
    use calamine::{open_workbook_auto_from_rs, Data, Reader};
    let mut wb = open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|e| fincore::FinError::io(format!("打开 Excel 失败：{e}")))?;
    let sheet_name = wb
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| fincore::FinError::msg("Excel 无 sheet"))?;
    let range = wb
        .worksheet_range(&sheet_name)
        .map_err(|e| fincore::FinError::io(format!("读取 sheet 失败：{e}")))?;
    // 解析后再兜一道行数/单元格数上限：即使解压体积没超，超大表也没有导入意义
    const MAX_IMPORT_ROWS: usize = 200_000;
    const MAX_IMPORT_CELLS: usize = 2_000_000;
    if range.height() > MAX_IMPORT_ROWS
        || range.height().saturating_mul(range.width()) > MAX_IMPORT_CELLS
    {
        return Err(fincore::FinError::validate(format!(
            "Excel 规模过大（{} 行 × {} 列），最多支持 {MAX_IMPORT_ROWS} 行 / {MAX_IMPORT_CELLS} 个单元格",
            range.height(),
            range.width()
        )));
    }
    let mut rows = Vec::new();
    for row in range.rows() {
        let cells: Vec<String> = row
            .iter()
            .map(|c| match c {
                Data::String(s) => s.clone(),
                Data::Int(n) => n.to_string(),
                Data::Float(f) => format!("{f}"),
                Data::Bool(b) => b.to_string(),
                Data::DateTime(d) => d.to_string(),
                Data::DateTimeIso(s) => s.clone(),
                Data::DurationIso(s) => s.clone(),
                _ => String::new(),
            })
            .collect();
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    Ok(rows)
}

/// 把 Excel 行列表转成 CSV 文本（每行用逗号连接），复用 CSV 解析逻辑
pub fn xlsx_to_csv_text(rows: &[Vec<String>]) -> String {
    rows.iter()
        .map(|r| {
            r.iter()
                .map(|c| {
                    if c.contains(',') || c.contains('"') {
                        format!("\"{}\"", c.replace('"', "\"\""))
                    } else {
                        c.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析一行 CSV：自动识别 `,` `\t` `;`，支持 `"` 引号
pub fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_q && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_q = !in_q;
                }
            }
            ',' | '\t' | ';' if !in_q => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// 宽松金额解析：无法识别时按 0 处理（调用方自行决定是否可接受）。
///
/// 导入的金额列一律走 [`parse_money_strict`]——静默归零的金额比报错更难查；
/// 这里保留宽松版给不需要报错、只要个数的场景。
#[allow(dead_code)]
fn parse_money(s: &str) -> Money {
    parse_money_strict(s).unwrap_or(Money::ZERO)
}

/// 严格金额解析：无法识别时返回原因，交由调用方上报警告。
///
/// 括号是会计惯例里的负数：`(1234.56)` = -1234.56（金蝶/用友导出的红字就长这样）。
/// 以前直接把 `(` `)` 连同其它非数字字符一起过滤掉，`(1234.56)` 变成了 **+1234.56**，
/// 导入期初/凭证时方向整个反过来；此处与 `bank::parse_money` 的括号口径保持一致。
fn parse_money_strict(s: &str) -> Result<Money, String> {
    // 先规范化 Unicode 字符：全角数字、Unicode 负号（U+2212）、全角括号等
    let normalized: String = s
        .chars()
        .map(|c| match c {
            '\u{ff0d}' => '-', // 全角减号
            '\u{2212}' => '-', // Unicode 减号
            // 全角 ASCII（U+FF01–U+FF5E）→ 半角（U+0021–U+007E）。
            // 偏移量是 0xFEE0；写成 0xFFEE 会下溢（debug panic）且把全角数字映射成乱码。
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect();
    let t = normalized.trim();
    // 括号负数：先剥括号再取反（过滤字符会把括号直接吃掉，负号就丢了）
    let (inner, neg) = match (t.starts_with('('), t.ends_with(')')) {
        (true, true) => (&t[1..t.len() - 1], true),
        _ => (t, false),
    };
    // 去掉千分位逗号与货币符号，兼容 "1,234.56" / "¥1,234.56" / "−100"
    let cleaned: String = inner
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    if cleaned.is_empty() {
        if t.is_empty() {
            return Ok(Money::ZERO);
        }
        return Err("金额格式不正确".to_string());
    }
    let v = Money::parse(&cleaned).map_err(|_| "金额格式不正确".to_string())?;
    Ok(if neg { v.negated() } else { v })
}

/// 金额列解析的失败收集器：把"无法识别"的金额攒起来，随 `ImportResult.warnings` 上报，
/// 而不是静默按 0 记账（期初按 0 记会让余额表凭空少一笔钱）。
#[derive(Default)]
struct MoneyWarn(Vec<String>);

impl MoneyWarn {
    /// 空串按 0；无法识别时记一条警告并返回 0（调用方见 `warn` 非空即丢行）
    fn money(&mut self, label: &str, s: &str) -> Money {
        match parse_money_strict(s) {
            Ok(v) => v,
            Err(e) => {
                self.0.push(format!("{label}「{s}」{e}"));
                Money::ZERO
            }
        }
    }
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    chrono::NaiveDate::parse_from_str(&cleaned, "%Y-%m-%d")
        .ok()
        .or_else(|| chrono::NaiveDate::parse_from_str(&cleaned, "%Y/%m/%d").ok())
        .or_else(|| {
            if cleaned.len() == 8 && cleaned.chars().all(|c| c.is_ascii_digit()) {
                chrono::NaiveDate::parse_from_str(&cleaned, "%Y%m%d").ok()
            } else {
                None
            }
        })
}

/// 解析方向词："借" / "debit" / "d" / 正数 → 借；"贷" / "credit" / "c" / 负数 → 贷
fn parse_direction(s: &str, signed: Money) -> fincore::Direction {
    let t = s.trim().to_lowercase();
    match t.as_str() {
        "借" | "debit" | "d" | "借方" => fincore::Direction::Debit,
        "贷" | "credit" | "c" | "贷方" => fincore::Direction::Credit,
        _ => {
            if signed.is_negative() {
                fincore::Direction::Credit
            } else {
                fincore::Direction::Debit
            }
        }
    }
}

/// 导入结果：成功条数 + 警告列表（逐行失败不中断，全部尝试）
#[derive(Clone, Debug)]
pub struct ImportResult {
    pub ok: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

/// 预检出的缺失科目（供前端展示、让用户选择映射或忽略）
#[derive(Clone, Debug)]
pub struct MissingAccount {
    /// 源文件里的科目编码（如 "1002"）
    pub code: String,
    /// 对应名称（若有）
    pub name: String,
    /// 出现次数（用于排序提示）
    pub count: usize,
}

/// 预检问题：一条一个，按「会不会让导入直接失败」分 error / warn。
#[derive(Clone, Debug)]
pub struct ImportIssue {
    /// `error` = 导不进去；`warn` = 能进但要留意
    pub level: &'static str,
    /// 问题类别（前端按它分组显示）
    pub category: &'static str,
    /// 源文件行号（1-based）。给行号是刻意的：几百行里错 3 行时，
    /// 「借贷不平 ×3」用户无从下手，「第 42–45 行」才能直接定位。
    pub row: Option<usize>,
    /// 人类可读的具体说明
    pub detail: String,
}

/// 预检报告。
///
/// 为什么要从「只查缺失科目」扩成这个：原来的预检只回答「有没有不存在的科目」，
/// 而导入真正会失败的常见原因还有好几个 —— 期间已结账、科目已停用、
/// 编码在文件里重复。用户点完「执行导入」才发现，**错了一半的数据已经写进去了**
/// （档案类是逐条幂等写入、中断可重跑，凭证类更麻烦）。预检存在的意义就是把
/// 这些在写入前一次性说清楚。
#[derive(Clone, Debug, Default)]
pub struct ImportPreview {
    /// 参与检查的数据行数
    pub total_rows: usize,
    /// 缺失科目（保持原字段，前端映射 UI 直接用）
    pub missing_accounts: Vec<MissingAccount>,
    /// 逐条问题
    pub issues: Vec<ImportIssue>,
}

impl ImportPreview {
    pub fn errors(&self) -> usize {
        self.issues.iter().filter(|i| i.level == "error").count()
    }
    pub fn warns(&self) -> usize {
        self.issues.iter().filter(|i| i.level == "warn").count()
    }
    /// 能不能直接导入：只要有一条 error 就不该让用户点执行。
    ///
    /// 注意这不是硬门 —— 真正把关仍在写入路径（这里是「早告诉」，
    /// 不是「唯一拦点」）。缺失科目例外：那个可以靠映射解决，不算阻断。
    pub fn blocking(&self) -> bool {
        self.issues
            .iter()
            .any(|i| i.level == "error" && i.category != CATEGORY_MISSING)
    }
}

pub const CATEGORY_MISSING: &str = "科目缺失";
pub const CATEGORY_CLOSED: &str = "期间已结账";
pub const CATEGORY_DISABLED: &str = "科目已停用";
pub const CATEGORY_DUP: &str = "编码重复";
pub const CATEGORY_UNBALANCED: &str = "凭证借贷不平";
pub const CATEGORY_BAD_AMOUNT: &str = "金额无法识别";
pub const CATEGORY_EMPTY_VOUCHER: &str = "空凭证会被丢弃";

/// 把 CSV 文本拆成行列表（每行是单元格列表），供 Excel / 文本统一处理
fn text_to_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let f = split_csv(line);
        if !f.is_empty() {
            rows.push(f);
        }
    }
    rows
}

/// 提取文件中引用的所有科目编码（去重、带次数），供预检使用
/// 收集科目编码并**保留出现次数**。
///
/// 单独拆出来是因为 [`collect_codes`] 会去重（只返回 key），重复信息在那里
/// 就丢了 —— 而「同一科目在文件里出现多次」正是预检要提示用户的。
fn collect_code_counts(
    rows: &[Vec<String>],
    tmpl: ImportTemplate,
    is_begin: bool,
) -> std::collections::HashMap<String, usize> {
    let mut map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in rows {
        // 科目列位置随来源模板不同：期初恒为第 1 列；凭证通用/用友为第 4 列，金蝶为第 5 列
        let col = if is_begin {
            0
        } else {
            match tmpl {
                ImportTemplate::Kingdee => 4,
                _ => 3,
            }
        };
        let code = f.get(col).unwrap_or(&String::new()).trim().to_string();
        if !code.is_empty() && code.chars().any(|c| c.is_ascii_digit()) {
            *map.entry(code).or_insert(0) += 1;
        }
    }
    map
}

fn collect_codes(rows: &[Vec<String>], tmpl: ImportTemplate, is_begin: bool) -> Vec<String> {
    let mut out: Vec<String> = collect_code_counts(rows, tmpl, is_begin)
        .into_keys()
        .collect();
    out.sort();
    out
}

/// 预检：找出文件中引用但账套里不存在的科目（供用户选择映射或忽略）
///
/// - `tmpl`：来源模板（决定凭证的科目列位置）
/// - `is_begin = true`：期初余额表（第 1 列是科目）；`false`：凭证
///   类型别名 → AuxKind（客户/供应商/部门/职员/项目/银行/存货 + 英文）
fn aux_kind_of(s: &str) -> Option<fincore::AuxKind> {
    let t = s.trim().to_ascii_lowercase();
    Some(match t.as_str() {
        "客户" | "customer" | "c" => fincore::AuxKind::Customer,
        "供应商" | "supplier" | "s" => fincore::AuxKind::Supplier,
        "部门" | "dept" | "d" => fincore::AuxKind::Dept,
        "职员" | "员工" | "employee" | "e" => fincore::AuxKind::Employee,
        "项目" | "project" | "p" => fincore::AuxKind::Project,
        "银行" | "bank" | "b" => fincore::AuxKind::Bank,
        "存货" | "商品" | "item" | "i" => fincore::AuxKind::Item,
        // 现金流量项目也能建档案（`AuxKind::CashFlow`），但这里原来漏了 ——
        // 「导出全部辅助档案 → 导进另一个账套」这条闭环里，现金流量项目必然被
        // 当成「类型无法识别」跳过。少几条档案不会报错，搬账套的人发现不了。
        "现金流量" | "现金流量项目" | "现金流" | "cashflow" | "cf" => fincore::AuxKind::CashFlow,
        _ => return None,
    })
}

/// 科目类别解析：类别列空或无法识别 → 按编码首位推（1资产 2负债 3权益 4成本 5/6损益）
fn acct_category_of(code: &str, label: &str) -> fincore::AcctCategory {
    use fincore::AcctCategory as C;
    let from_code = || match code.chars().next().unwrap_or('1') {
        '1' => C::Asset,
        '2' => C::Liability,
        '3' => C::Equity,
        '4' => C::Cost,
        '5' | '6' => C::Expense,
        _ => C::Asset,
    };
    match label.trim() {
        "" => from_code(),
        "资产" | "asset" => C::Asset,
        "负债" | "liability" => C::Liability,
        "共同" | "common" => C::Common,
        "权益" | "所有者权益" | "equity" => C::Equity,
        "成本" | "cost" => C::Cost,
        "收入" | "income" | "收益" => C::Income,
        "费用" | "expense" | "损益" => C::Expense,
        _ => from_code(),
    }
}

fn master_cell(f: &[String], i: usize) -> String {
    f.get(i).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// 导入辅助核算档案（通用列：类型,编码,名称[,备注]；表头/短行自动跳过）。
/// **编码已存在 → 跳过计数不覆盖**（避免误伤业务引用；改名请用页面编辑）。
/// 逐条幂等写入：中断后重跑会跳过已导入行（与期初的整批事务不同——档案可增量）。
pub fn import_aux(db: &Db, text: &str, who: &str) -> DbResult<ImportResult> {
    import_aux_rows(db, &text_to_rows(text), who)
}

pub fn import_aux_bytes(db: &Db, bytes: &[u8], who: &str) -> DbResult<ImportResult> {
    import_aux_rows(db, &read_xlsx_bytes(bytes)?, who)
}

fn import_aux_rows(db: &Db, rows: &[Vec<String>], who: &str) -> DbResult<ImportResult> {
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    // 表头映射：有表头就按列名取，没有就按老位置取（见 `HeadMap`）
    let mut map = HeadMap::default();
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 3 {
            continue; // 空行/说明行
        }
        if row_is_header(f) {
            map = HeadMap::from_row(f);
            continue;
        }
        // 「类型」「编码」也走表头映射：用户完全可以把类型挪到最后一列
        let kind_s = map.cell(f, &["类型", "档案类型"], 0);
        let code = map.cell(f, &["编码", "档案编码", "代码", "编号"], 1);
        if code.is_empty() {
            continue;
        }
        let Some(kind) = aux_kind_of(&kind_s) else {
            res.warnings
                .push(format!("第 {} 行：类型「{kind_s}」无法识别，已跳过", i + 1));
            res.skipped += 1;
            continue;
        };
        let name = master_cell(f, 2);
        if name.is_empty() {
            res.warnings
                .push(format!("第 {} 行：{code} 名称为空，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::auxs::get(db, kind, &code)?.is_some() {
            res.skipped += 1;
            continue; // 已存在静默跳过（重导常见）
        }
        let mut e = fincore::auxiliary::AuxEntity::new(kind, code.clone(), name);
        e.memo = map.cell(f, &["备注"], 3);
        // 第 5、6 列（上级编码、状态）是 v1.8.21 才加进模板的。老模板只有前四列，
        // `master_cell` 越界返回空串，所以老文件照旧能导 —— 升级不会让用户
        // 手上那份表突然导不进去。
        let parent = map.cell(f, &["上级编码", "上级", "父级编码"], 4);
        if !parent.is_empty() {
            e.parent_code = Some(parent);
        }
        e.disabled = map.cell(f, &["状态"], 5) == "停用";

        // 扩展属性（v1.8.22）。原来这一整块都不导：客户档案上的「税点%」「开票
        // 情况」在桌面端能改、在网页端还改不了，导出表里又没有这两列 —— 想批量
        // 把 59 个客户的税点填一遍，只能一个个点开改。现在它们跟着表走。
        //
        // 空单元格 = 不动，**不是**清空：导出的表如果客户本来没填税点，导回来
        // 也不该凭空给它写个空值；而用户想清空某一格，删掉内容导回来仍然是空，
        // 效果一样。
        for (aliases, key) in [
            (&["税点%", "税点", "税率%", "税率"][..], fincore::auxiliary::prop::TAX_RATE),
            (&["信用额度", "信用额度（0=不限）"][..], fincore::auxiliary::prop::CREDIT_LIMIT),
            (&["开户行", "开户银行"][..], fincore::auxiliary::prop::BANK_NAME),
            (&["银行账号", "账号"][..], fincore::auxiliary::prop::BANK_ACCOUNT),
            (&["规格型号", "规格"][..], fincore::auxiliary::prop::SPEC),
            (&["计量单位", "单位"][..], fincore::auxiliary::prop::UNIT),
        ] {
            let v = map.cell(f, aliases, usize::MAX);
            if !v.is_empty() {
                e.set_prop(key, v);
            }
        }
        // 开票情况要中文标签 ↔ 存储码互转；认不出的自由文字**保留原值不写**
        let pol = map.cell(f, &["开票情况", "开票"], usize::MAX);
        if let Some(v) = fincore::auxiliary::tax_policy_value(&pol) {
            e.set_prop(fincore::auxiliary::prop::TAX_RATE_POLICY, v.to_string());
        }
        crate::auxs::insert(db, &e)?;
        res.ok += 1;
    }
    if res.ok > 0 {
        db.log(who, "导入", "导入基础档案", &format!("成功 {} 条", res.ok))?;
    }
    Ok(res)
}

/// 导入存货档案（列：编码,名称[,保质期天,安全库存]）＝ aux(item) + props保质期 + item_plan安全库存。
/// 单位换算请到「多单位换算」页维护；已存在跳过。
pub fn import_items(db: &Db, text: &str, who: &str) -> DbResult<ImportResult> {
    import_items_rows(db, &text_to_rows(text), who)
}

pub fn import_items_bytes(db: &Db, bytes: &[u8], who: &str) -> DbResult<ImportResult> {
    import_items_rows(db, &read_xlsx_bytes(bytes)?, who)
}

fn import_items_rows(db: &Db, rows: &[Vec<String>], who: &str) -> DbResult<ImportResult> {
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    let mut map = HeadMap::default();
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 2 {
            continue;
        }
        if row_is_header(f) {
            map = HeadMap::from_row(f);
            continue;
        }
        let code = map.cell(f, &["编码", "存货编码", "代码", "编号"], 0);
        if code.is_empty() {
            continue;
        }
        let name = map.cell(f, &["名称", "存货名称"], 1);
        if name.is_empty() {
            res.warnings
                .push(format!("第 {} 行：{code} 名称为空，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::auxs::get(db, fincore::AuxKind::Item, &code)?.is_some() {
            res.skipped += 1;
            continue;
        }
        let mut e = fincore::auxiliary::AuxEntity::new(fincore::AuxKind::Item, code.clone(), name);
        let shelf = map.cell(f, &["保质期天", "保质期"], 2);
        if !shelf.is_empty() {
            e.props
                .insert("shelf_life_days".into(), shelf);
        }
        // 「单位」「状态」「备注」是 v1.8.22 才进表的：网页存货页面一直在显示
        // 这三列，导出的表里却没有 —— 改完导回去，单位就没了。
        let unit = map.cell(f, &["计量单位", "单位"], usize::MAX);
        if !unit.is_empty() {
            e.set_prop(fincore::auxiliary::prop::UNIT, unit);
        }
        e.memo = map.cell(f, &["备注"], usize::MAX);
        e.disabled = map.cell(f, &["状态"], usize::MAX) == "停用";
        crate::auxs::insert(db, &e)?;
        // 安全库存（列4）→ item_plan（MRP/低库存预警口径）
        let safety = map.cell(f, &["安全库存"], 3);
        if !safety.is_empty() {
            crate::advanced::item_plan_upsert(
                db,
                &crate::advanced::ItemPlan {
                    item_code: code.clone(),
                    safety_stock: Money::parse_or_zero(&safety),
                    lead_days: 0,
                    lot_size: Money::ZERO,
                },
            )?;
        }
        res.ok += 1;
    }
    if res.ok > 0 {
        db.log(who, "导入", "导入存货档案", &format!("成功 {} 条", res.ok))?;
    }
    Ok(res)
}

/// 导入科目建档（列：编码,名称[,类别,方向,备注]）：类别空按编码首位推；已存在跳过。
/// 逐条幂等（中断可重导）；辅助核算维度请到科目编辑器补配。
pub fn import_accounts(db: &Db, text: &str, who: &str) -> DbResult<ImportResult> {
    import_accounts_rows(db, &text_to_rows(text), who)
}

pub fn import_accounts_bytes(db: &Db, bytes: &[u8], who: &str) -> DbResult<ImportResult> {
    import_accounts_rows(db, &read_xlsx_bytes(bytes)?, who)
}

fn import_accounts_rows(db: &Db, rows: &[Vec<String>], who: &str) -> DbResult<ImportResult> {
    let mut map = HeadMap::default();
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 2 {
            continue;
        }
        if row_is_header(f) {
            map = HeadMap::from_row(f);
            continue;
        }
        let code = map.cell(f, &["编码", "科目编码", "代码", "编号"], 0);
        if code.is_empty() {
            continue;
        }
        let name = map.cell(f, &["名称", "科目名称"], 1);
        if name.is_empty() {
            res.warnings
                .push(format!("第 {} 行：{code} 名称为空，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::accounts::get(db, &code)?.is_some() {
            res.skipped += 1;
            continue;
        }
        let cat = acct_category_of(&code, &map.cell(f, &["类别", "科目类别"], 2));
        let mut acc = fincore::account::Account::new(code.clone(), name, cat);
        match map.cell(f, &["方向", "余额方向"], 3).as_str() {
            "贷" | "credit" => acc.dir = fincore::Direction::Credit,
            "借" | "debit" => acc.dir = fincore::Direction::Debit,
            _ => {} // 空 = 类别默认方向
        }
        acc.memo = map.cell(f, &["备注"], 4);

        // 下面这些是 v1.8.22 才认的列。原来导出表里第 6 列写着「辅助核算维度
        // （只读提示，导回时不生效）」—— 一句自相矛盾的说明：页面上明明有这一
        // 项，导出的表却告诉用户「别改，改了没用」。搬账套时科目上的辅助核算、
        // 数量单位、外币、现金/银行标志全丢，余额表按辅助拆不开，只能一个个
        // 重设。
        let dims = map.cell(f, &["辅助核算维度", "辅助核算"], usize::MAX);
        acc.aux = parse_aux_mask(&dims);
        let unit = map.cell(f, &["数量单位", "计量单位", "单位"], usize::MAX);
        if !unit.is_empty() {
            acc.unit = Some(unit);
            acc.has_qty = true;
        }
        let cur = map.cell(f, &["币种", "外币币种"], usize::MAX);
        if !cur.is_empty() {
            acc.currency = Some(cur);
        }
        acc.is_cash = is_yes(&map.cell(f, &["现金科目"], usize::MAX));
        acc.is_bank = is_yes(&map.cell(f, &["银行科目"], usize::MAX));
        acc.disabled = map.cell(f, &["状态"], usize::MAX) == "停用";
        crate::accounts::insert(db, &acc)?;
        res.ok += 1;
    }
    if res.ok > 0 {
        db.log(who, "导入", "导入科目", &format!("成功 {} 条", res.ok))?;
    }
    Ok(res)
}

/// 导入期初库存（列：存货编码,仓库,数量[,单价,批次号,生产日期,备注]）。
/// **口径声明**：只入数量流水（其他入库，账期=建账首期），**不生成凭证**——
/// 金额与存货科目期初由「科目期初」导入负责，双侧各管一段避免重复记账。
/// 存货须已建档（先导存货档案）；批次号非空则自动建档（生产日期+档案保质期推失效日）。
pub fn import_opening_stock(db: &Db, text: &str, who: &str) -> DbResult<ImportResult> {
    import_opening_stock_rows(db, &text_to_rows(text), who)
}

pub fn import_opening_stock_bytes(db: &Db, bytes: &[u8], who: &str) -> DbResult<ImportResult> {
    import_opening_stock_rows(db, &read_xlsx_bytes(bytes)?, who)
}

fn import_opening_stock_rows(db: &Db, rows: &[Vec<String>], who: &str) -> DbResult<ImportResult> {
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    let period = db.options().start_period;
    let date = period.first_day();
    let tx = db.write_tx()?;
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 3 {
            continue;
        }
        let item = master_cell(f, 0);
        if item.is_empty() || item == "存货编码" || item.eq_ignore_ascii_case("code") {
            continue;
        }
        let warehouse = master_cell(f, 1);
        let qty = Money::parse_or_zero(&master_cell(f, 2));
        if warehouse.is_empty() || !qty.is_positive() {
            res.warnings
                .push(format!("第 {} 行：仓库为空或数量非正，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::auxs::get(db, fincore::AuxKind::Item, &item)?.is_none() {
            res.warnings.push(format!(
                "第 {} 行：存货 {item} 未建档（先导存货档案），已跳过",
                i + 1
            ));
            res.skipped += 1;
            continue;
        }
        let price = Money::parse_or_zero(&master_cell(f, 3));
        let batch_no = master_cell(f, 4);
        let pdate = master_cell(f, 5);
        let memo = master_cell(f, 6);
        // 数量流水（amount=0 由计价引擎结算；金额侧不入账——口径见 doc）
        crate::business::stock_insert_of(
            &tx,
            &crate::business::StockMove {
                id: 0,
                period,
                biz_date: date,
                kind: crate::business::StockKind::OtherIn,
                item: item.clone(),
                warehouse: warehouse.clone(),
                batch_no: batch_no.clone(),
                qty,
                price,
                amount: Money::ZERO,
                voucher_id: None,
                memo: if memo.is_empty() {
                    "期初库存".into()
                } else {
                    format!("期初 {memo}")
                },
            },
        )?;
        // 批次建档（OR IGNORE；失效日=生产日期+档案保质期）
        if !batch_no.is_empty() {
            let pdate_s = if pdate.is_empty() {
                date.format("%Y-%m-%d").to_string()
            } else {
                pdate.clone()
            };
            let expiry = match chrono::NaiveDate::parse_from_str(&pdate_s, "%Y-%m-%d") {
                Ok(d) => {
                    let days = crate::batch::shelf_life_days(db, &item);
                    if days > 0 {
                        (d + chrono::Duration::days(days))
                            .format("%Y-%m-%d")
                            .to_string()
                    } else {
                        String::new()
                    }
                }
                Err(_) => String::new(),
            };
            tx.execute(
                "INSERT OR IGNORE INTO stock_batch(item,batch_no,production_date,expiry_date,warehouse,memo,created_by,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![
                    item,
                    batch_no,
                    pdate_s,
                    expiry,
                    warehouse,
                    "期初批次",
                    who,
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
                ],
            )?;
        }
        res.ok += 1;
    }
    if res.ok > 0 {
        db.log(who, "导入", "导入期初库存", &format!("成功 {} 条", res.ok))?;
    }
    tx.commit()?;
    Ok(res)
}

/// 导入往来期初明细（列：类型(应收/应付/ar/ap),客商编码,单据号,单据日期,金额[,客商名,备注]）。
/// 同 kind+客商+单据号已存在 → 静默跳过（幂等重导）；**影子挂账不生成凭证**
/// （金额与客商总额由「科目期初」负责，本表只承载逐单欠款信息供账龄与管理）。
pub fn import_arap_opening(db: &Db, text: &str, who: &str) -> DbResult<ImportResult> {
    import_arap_opening_rows(db, &text_to_rows(text), who)
}

pub fn import_arap_opening_bytes(db: &Db, bytes: &[u8], who: &str) -> DbResult<ImportResult> {
    import_arap_opening_rows(db, &read_xlsx_bytes(bytes)?, who)
}

fn arap_kind_of(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "应收" | "ar" | "receivable" | "accounts_receivable" => Some("ar"),
        "应付" | "ap" | "payable" | "accounts_payable" => Some("ap"),
        _ => None,
    }
}

fn import_arap_opening_rows(db: &Db, rows: &[Vec<String>], who: &str) -> DbResult<ImportResult> {
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 5 {
            continue;
        }
        let type_s = master_cell(f, 0);
        if type_s == "类型" || type_s.eq_ignore_ascii_case("type") {
            continue; // 表头
        }
        let Some(kind) = arap_kind_of(&type_s) else {
            res.warnings.push(format!(
                "第 {} 行：类型「{type_s}」应为 应收/应付，已跳过",
                i + 1
            ));
            res.skipped += 1;
            continue;
        };
        let party = master_cell(f, 1);
        let doc_no = master_cell(f, 2);
        let doc_date = master_cell(f, 3);
        let amount = Money::parse_or_zero(&master_cell(f, 4));
        if party.is_empty() || doc_no.is_empty() {
            res.warnings
                .push(format!("第 {} 行：客商编码或单据号为空，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if chrono::NaiveDate::parse_from_str(&doc_date, "%Y-%m-%d").is_err() {
            res.warnings.push(format!(
                "第 {} 行：单据日期「{doc_date}」应为 YYYY-MM-DD，已跳过",
                i + 1
            ));
            res.skipped += 1;
            continue;
        }
        if !amount.is_positive() {
            res.warnings
                .push(format!("第 {} 行：金额必须大于 0，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::settle::arap_opening_exists(db, kind, &party, &doc_no)? {
            res.skipped += 1;
            continue; // 幂等
        }
        crate::settle::arap_opening_insert(
            db,
            &crate::settle::ArapOpening {
                id: 0,
                kind: kind.to_string(),
                party_code: party,
                party_name: master_cell(f, 5),
                doc_no,
                doc_date,
                amount,
                memo: master_cell(f, 6),
                created_by: String::new(),
            },
            who,
        )?;
        res.ok += 1;
    }
    if res.ok > 0 {
        db.log(who, "导入", "导入往来期初", &format!("成功 {} 条", res.ok))?;
    }
    Ok(res)
}

pub fn analyze_missing(
    db: &Db,
    text: &str,
    tmpl: ImportTemplate,
    is_begin: bool,
) -> DbResult<Vec<MissingAccount>> {
    let chart = crate::accounts::chart(db)?;
    let rows = text_to_rows(text);
    let codes = collect_codes(&rows, tmpl, is_begin);
    let mut out = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for code in &codes {
        *seen.entry(code.clone()).or_insert(0) += 1;
    }
    for code in &codes {
        if chart.get(code).is_none() {
            out.push(MissingAccount {
                code: code.clone(),
                name: String::new(),
                count: seen.get(code).copied().unwrap_or(0),
            });
        }
    }
    Ok(out)
}

/// 凭证导入的预检：借贷不平衡 / 金额不可解析 / 会变成空凭证。
///
/// 复用**真实导入器用的同一套**行提取与分组规则（`extract_voucher_line` +
/// 与 `import_vouchers_rows` 相同的「新凭证边界」判定）。这一点很重要：
/// 预检如果自己另写一套解析，预检说「平」而导入说「跳过」，用户会完全
/// 不知道该信哪个 —— 那是比没有预检更糟的结果。
///
/// 顺带修掉一个「静默丢弃」：导入时 `flush` 遇到 `entries.is_empty()` 直接
/// `return Ok(())` —— 既不计入 `skipped`，也不产生凭证级警告。于是
/// 一张所有分录金额都不可解析的凭证会**人间蒸发**：用户看到「成功 N 条」，
/// 以为全导进去了。这里把这种情况显式报出来。
fn analyze_vouchers(
    rows: &[Vec<String>],
    tmpl: ImportTemplate,
    mapping: &std::collections::HashMap<String, String>,
) -> Vec<ImportIssue> {
    let mut issues = Vec::new();
    // 按与导入器相同的边界规则把行分组成「一张张凭证」
    struct Open {
        start: usize,
        end: usize,
        date: chrono::NaiveDate,
        word: String,
        debit: Money,
        credit: Money,
        entries: usize,
    }
    let mut pending: Option<Open> = None;
    let mut pending_no: Option<i32> = None;

    fn close(pending: &mut Option<Open>, issues: &mut Vec<ImportIssue>) {
        if let Some(v) = pending.take() {
            let tag = if v.word.trim().is_empty() {
                "记"
            } else {
                v.word.trim()
            };
            if v.entries == 0 {
                // 整张凭证一条分录都没进来 —— 导入时会**静默丢弃**
                issues.push(ImportIssue {
                    level: "error",
                    category: CATEGORY_EMPTY_VOUCHER,
                    row: Some(v.start + 1),
                    detail: format!(
                        "第 {}–{} 行这张凭证一条分录都没解析出来（金额列全不可识别？），\
                         导入时会被**静默丢弃**且不计入跳过数",
                        v.start + 1,
                        v.end + 1
                    ),
                });
                return;
            }
            if v.debit != v.credit {
                issues.push(ImportIssue {
                    level: "error",
                    category: CATEGORY_UNBALANCED,
                    row: Some(v.start + 1),
                    detail: format!(
                        "第 {}–{} 行的{tag}字凭证借贷不平：借 {} / 贷 {}，差 {}（导入时整张跳过）",
                        v.start + 1,
                        v.end + 1,
                        v.debit.fmt_money(),
                        v.credit.fmt_money(),
                        (v.debit - v.credit).fmt_money()
                    ),
                });
            }
        }
    }

    for (i, f) in rows.iter().enumerate() {
        let Some(line) = extract_voucher_line(tmpl, f) else {
            continue; // 表头 / 说明行
        };
        if !line.warn.is_empty() {
            // 金额不可识别的行：导入器是「丢弃该分录，整张凭证随后因不平衡被跳过」。
            // 预检要报出来，否则用户只看到「成功 N 条」，不知道哪张被丢了。
            if pending.is_none() {
                pending = Some(Open {
                    start: i,
                    end: i,
                    date: line.date,
                    word: String::new(),
                    debit: Money::ZERO,
                    credit: Money::ZERO,
                    entries: 0,
                });
            }
            if let Some(v) = pending.as_mut() {
                v.end = i;
            }
            for w in &line.warn {
                issues.push(ImportIssue {
                    level: "error",
                    category: CATEGORY_BAD_AMOUNT,
                    row: Some(i + 1),
                    detail: format!("第 {} 行：{w}（该分录会被丢弃）", i + 1),
                });
            }
            continue;
        }
        // ↓ 与 import_vouchers_rows 里的判定逐字对齐
        let new_voucher = match &pending {
            Some(v) => {
                let date_changed = v.date != line.date;
                let no_changed = tmpl == ImportTemplate::Kingdee && pending_no != line.no;
                let balanced_boundary =
                    tmpl != ImportTemplate::Kingdee && v.entries > 0 && v.debit == v.credit;
                date_changed || no_changed || balanced_boundary
            }
            None => true,
        };
        if new_voucher {
            close(&mut pending, &mut issues);
            pending_no = line.no;
            pending = Some(Open {
                start: i,
                end: i,
                date: line.date,
                word: line.word.clone(),
                debit: Money::ZERO,
                credit: Money::ZERO,
                entries: 0,
            });
        }
        if let Some(v) = pending.as_mut() {
            v.end = i;
            if !line.word.trim().is_empty() {
                v.word = line.word.trim().to_string();
            }
            v.debit += line.debit;
            v.credit += line.credit;
            v.entries += 1;
        }
    }
    close(&mut pending, &mut issues);
    let _ = mapping;
    issues
}
///
/// `text` 传空串表示「本次不按科目列检查」（主数据类如科目表本身没有科目引用），
/// 与旧 `analyze_missing` 的约定一致。
pub fn analyze_import(
    db: &Db,
    text: &str,
    tmpl: ImportTemplate,
    is_begin: bool,
    period: Option<Period>,
) -> DbResult<ImportPreview> {
    analyze_import_mapped(db, text, tmpl, is_begin, period, &Default::default())
}

/// [`analyze_import`] 的带科目映射版本。
///
/// 映射只影响「科目缺失」那部分：映射后能落到真实科目上的，就不该再报缺失。
/// 借贷平衡 / 金额不可解析这些**与映射无关**（金额列认不出来，映射也救不了），
/// 所以不重复计算。
pub fn analyze_import_mapped(
    db: &Db,
    text: &str,
    tmpl: ImportTemplate,
    is_begin: bool,
    period: Option<Period>,
    mapping: &std::collections::HashMap<String, String>,
) -> DbResult<ImportPreview> {
    let mut pv = ImportPreview::default();
    let rows = if text.trim().is_empty() {
        Vec::new()
    } else {
        text_to_rows(text)
    };
    // 数据行数（跳过表头与空行），用于告诉用户「这份文件一共多少行」
    pv.total_rows = rows.len().saturating_sub(1);

    // ① 期间已结账 —— 这是最该提前拦的一条：导入写的是草稿没错，但凭证一旦
    //    要记账就会撞「期间不能已结账」，而那时用户已经导完并以为完事了。
    if let Some(p) = period {
        let closed: Option<i32> = db
            .conn()
            .query_row(
                "SELECT 1 FROM period_state WHERE period=?1 AND closed=1",
                [p.ymm()],
                |r| r.get(0),
            )
            .optional()?;
        if closed.is_some() {
            pv.issues.push(ImportIssue {
                level: "error",
                category: CATEGORY_CLOSED,
                row: None,
                detail: format!("期间 {} 已结账，数据导进去也无法记账（需先反结账）", p),
            });
        }
    }

    if rows.is_empty() {
        return Ok(pv);
    }

    // ② 缺失科目 + 停用科目 + 文件内重复编码
    let chart = crate::accounts::chart(db)?;
    // 用带次数的版本：collect_codes 会去重，重复信息在那里就丢了
    let counts = collect_code_counts(&rows, tmpl, is_begin);
    let mut codes: Vec<&String> = counts.keys().collect();
    codes.sort();
    for (code, n) in counts.iter() {
        if *n > 1 {
            pv.issues.push(ImportIssue {
                level: "warn",
                category: CATEGORY_DUP,
                // 重复不等于错：期初表里同一科目按方向分两行是常见写法。
                // 所以只提示，让用户自己判断，而不是拦下来。
                row: None,
                detail: format!("编码 {code} 在文件里出现 {n} 次（期初表同科目分方向是正常的）"),
            });
        }
    }
    for code in codes {
        // 映射能把源编码落到真实科目上的，就不该再报缺失 —— 否则用户刚选完
        // 映射，预检还是说「缺科目」，会开始不信这个功能。
        if mapping.contains_key(code) {
            continue;
        }
        match chart.get(code) {
            None => pv.missing_accounts.push(MissingAccount {
                code: code.clone(),
                name: String::new(),
                count: counts.get(code).copied().unwrap_or(0),
            }),
            Some(a) if a.disabled => pv.issues.push(ImportIssue {
                level: "error",
                category: CATEGORY_DISABLED,
                row: None,
                detail: format!("科目 {code}（{}）已停用，导入的凭证过不了记账校验", a.name),
            }),
            _ => {}
        }
    }

    // ③ 凭证专属：借贷不平 / 金额不可识别 / 会变成空凭证
    //    复用真实导入器的行提取与分组规则，避免「预检说平、导入说跳过」，
    //    那种不一致比没有预检更糟。
    if !is_begin {
        pv.issues.extend(analyze_vouchers(&rows, tmpl, mapping));
    }
    Ok(pv)
}

/// 执行导入：把源科目编码按 `mapping`（源编码 → 目标编码）替换后再写入。
/// 不在 mapping 里的缺失科目会跳过并警告；已存在科目不受影响。
pub fn apply_mapping(code: &str, mapping: &std::collections::HashMap<String, String>) -> String {
    mapping
        .get(code)
        .cloned()
        .unwrap_or_else(|| code.to_string())
}

/// 按科目表的辅助核算要求补齐分录的必填辅助字段（导入 CSV 常省略，需自动填默认值）
fn fill_required(db: &Db, e: &mut Entry) {
    let Ok(chart) = crate::accounts::chart(db) else {
        return;
    };
    let Some(a) = chart.get(&e.account_code) else {
        return;
    };
    for k in a.aux.list() {
        if e.aux.get(k).is_some() {
            continue;
        }
        let v = match k {
            fincore::AuxKind::Bank => "B01",
            fincore::AuxKind::Customer => "C01",
            fincore::AuxKind::Supplier => "S01",
            fincore::AuxKind::Item => "I01",
            fincore::AuxKind::Dept => "D01",
            fincore::AuxKind::Employee => "E01",
            fincore::AuxKind::Project => "P01",
            fincore::AuxKind::CashFlow => continue,
        };
        e.aux.set(k, Some(v.to_string()));
    }
    if a.has_qty && e.qty.is_none() {
        e.qty = Some(Money::ONE);
        e.price = Some(if e.debit.is_positive() {
            e.debit
        } else {
            e.credit
        });
    }
}

/// 凭证导入模板（桌面端「下载模板」用，通用列布局）。
/// 金蝶/用友列见各模板解析；桌面窗口内有三套写法的文字说明。
pub fn voucher_template() -> fincore::batch::Template {
    fincore::batch::Template {
        headers: vec![
            "日期".to_string(),
            "凭证字".to_string(),
            "摘要".to_string(),
            "科目编码".to_string(),
            "借方".to_string(),
            "贷方".to_string(),
        ],
        examples: vec![
            vec![
                "2026-11-03".to_string(),
                "记".to_string(),
                "收宏远汽配加工费".to_string(),
                "100201".to_string(),
                "68000".to_string(),
                "0".to_string(),
            ],
            vec![
                "2026-11-03".to_string(),
                "记".to_string(),
                "收宏远汽配加工费".to_string(),
                "112201".to_string(),
                "0".to_string(),
                "68000".to_string(),
            ],
        ],
        help: vec![
            "第一页第一行是表头，从第二行开始填你自己的凭证。".to_string(),
            "先把两行示例删掉，再填。".to_string(),
            "通用写法：日期,凭证字,摘要,科目编码,借方,贷方（后可加 客户编码,银行编码 两列）。"
                .to_string(),
            "同一张凭证写多行：日期相同、借贷合计相等的连续行会被并成一张。".to_string(),
            "日期必须在当前期间内；跨月分两次导。".to_string(),
            "账套里没有的科目整行跳过（不新建）：先在「会计科目」建好或改编码一致再导。"
                .to_string(),
            "导进来全是草稿：检查无误后记账。".to_string(),
        ],
    }
}

/// 导入期初余额表（CSV 文本）
///
/// - 通用列：`科目编码, 方向(借/贷), 金额`；方向省略时按金额正负推断
/// - 金蝶列：`科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方`
/// - 用友列：`科目编码, 科目名称, 期初借方, 期初贷方, 累计借方, 累计贷方`
///
/// 金额语义为"启用期初余额"（正 = 借，负 = 贷）。
/// 科目需为末级科目；非末级或不存在（且未在 `mapping` 中映射）则跳过并警告。
pub fn import_begin(
    db: &Db,
    text: &str,
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let rows = text_to_rows(text);
    import_begin_rows(db, &rows, who, mapping, tmpl)
}

/// 导入期初余额表（Excel 文件字节，.xlsx/.xls/.ods）
pub fn import_begin_bytes(
    db: &Db,
    bytes: &[u8],
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let rows = read_xlsx_bytes(bytes)?;
    import_begin_rows(db, &rows, who, mapping, tmpl)
}

/// 导入期初余额表核心：按模板解析每一行（文本与 Excel 共用）
fn import_begin_rows(
    db: &Db,
    rows: &[Vec<String>],
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let chart = crate::accounts::chart(db)?;
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    // 整批导入同事务：中途任何数据库错误都整体回滚，不留"导了一半"的期初
    let tx = db.write_tx()?;
    // 期初表第一行可能是表头（「科目编码,科目名称,方向,…」）。`extract_begin_line`
    // 靠「第一格必须有数字」把表头挡掉；但表头本身对 `HeadMap` 有用 —— 先认列名，
    // 后面每行就按名字取，列顺序怎么换都不会错位。
    let mut map = HeadMap::default();
    for (i, f) in rows.iter().enumerate() {
        if row_is_header(f) {
            map = HeadMap::from_row(f);
            continue;
        }
        let Some(line) = extract_begin_line(tmpl, f, &map) else {
            continue; // 表头 / 说明行
        };
        if !line.warn.is_empty() {
            // 金额无法识别：宁可丢行也不能按 0 记期初（余额表凭空少一笔钱最难查）
            for w in line.warn {
                res.warnings.push(format!("第 {} 行：{w}", i + 1));
            }
            res.skipped += 1;
            continue;
        }
        // 源科目 → 目标科目（用户映射）
        let src_code = line.code.clone();
        let code = apply_mapping(&src_code, mapping);
        let amt = line.amount;
        let dir = parse_direction(&line.direction, amt);
        let signed = if dir == fincore::Direction::Debit {
            amt.abs()
        } else {
            -amt.abs()
        };
        // 校验科目
        match chart.get(&code) {
            Some(_) if chart.is_leaf(&code) => {
                balances::upsert_begin_on(
                    &tx,
                    &BeginRow {
                        id: 0,
                        account_code: code.clone(),
                        aux: line.aux.clone(),
                        year_begin: signed,
                        debit_accum: line.debit_accum.unwrap_or(Money::ZERO),
                        credit_accum: line.credit_accum.unwrap_or(Money::ZERO),
                        qty_begin: line.qty_begin,
                    },
                )?;
                res.ok += 1;
            }
            Some(_) => {
                res.warnings.push(format!(
                    "第 {} 行：科目 {code}（源 {src_code}）非末级，不能记期初，已跳过",
                    i + 1
                ));
                res.skipped += 1;
            }
            None => {
                res.warnings.push(format!(
                    "第 {} 行：科目 {code}（源 {src_code}）不存在，已跳过",
                    i + 1
                ));
                res.skipped += 1;
            }
        }
    }
    if res.ok > 0 {
        db.log(who, "导入", "导入期初余额", &format!("成功 {} 条", res.ok))?;
    }
    tx.commit()?;
    Ok(res)
}

/// 导入凭证（CSV 文本）
///
/// - 通用列：`日期, 凭证字, 摘要, 科目编码, 借方, 贷方[, 客户, 银行]`
/// - 金蝶列：`日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方[, 客户, 银行]`
/// - 用友列：`日期, 凭证字号, 摘要, 科目编码, 借方, 贷方[, 客户, 银行]`
///
/// - 凭证字省略时按"记"
/// - 同一期间内凭证号自动连续分配（按出现顺序）
/// - 借方/贷方必有一方非零；一行分录借贷必须平衡的凭证由校验把关
/// - 辅助核算可选列：后面追加的客户/银行编码会按行分配到分录；带辅助核算的
///   科目若未提供辅助列则由科目表按默认值补齐
/// - `mapping`：源科目 → 目标科目映射（用户在前端选择），缺失科目按映射替换
pub fn import_vouchers(
    db: &Db,
    period: Period,
    text: &str,
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let rows = text_to_rows(text);
    import_vouchers_rows(db, period, &rows, who, mapping, tmpl)
}

/// 导入凭证（Excel 文件字节，.xlsx/.xls/.ods）
pub fn import_vouchers_bytes(
    db: &Db,
    period: Period,
    bytes: &[u8],
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let rows = read_xlsx_bytes(bytes)?;
    import_vouchers_rows(db, period, &rows, who, mapping, tmpl)
}

/// 在调用方事务内保存一张凭证，失败时**整张回滚**。
///
/// `vouchers::save_in` 是「先插表头、再逐条插分录」，分录写到一半报错（例如某行
/// 触发了 SQLite 约束）时表头已经留在调用方的事务里；导入循环只是记一条警告就
/// 继续，最后 `commit()` 把这些**没通过任何校验的半张凭证**一起提交进账套。
/// SAVEPOINT 把每张凭证圈成独立保存点，失败只回滚这一张。
fn save_voucher_in_tx(tx: &rusqlite::Transaction, v: &mut Voucher) -> DbResult<i64> {
    tx.execute("SAVEPOINT one_voucher", [])?;
    match vouchers::save_in(tx, v) {
        Ok(id) => {
            tx.execute("RELEASE one_voucher", [])?;
            Ok(id)
        }
        Err(e) => {
            tx.execute("ROLLBACK TO one_voucher", [])?;
            tx.execute("RELEASE one_voucher", [])?;
            Err(e)
        }
    }
}

/// 导入凭证核心：按模板解析每一行（文本与 Excel 共用）
fn import_vouchers_rows(
    db: &Db,
    period: Period,
    rows: &[Vec<String>],
    who: &str,
    mapping: &std::collections::HashMap<String, String>,
    tmpl: ImportTemplate,
) -> DbResult<ImportResult> {
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    let mut pending: Option<Voucher> = None;
    // 当前待提交凭证对应的源凭证号（金蝶模板有；通用/用友无，恒为 None）
    let mut pending_no: Option<i32> = None;
    // 整批导入同事务：中途任何数据库错误都整体回滚，不留"导了一半"的凭证
    let tx = db.write_tx()?;

    // 收尾提交
    let flush = |v: &mut Option<Voucher>, res: &mut ImportResult, who: &str| -> DbResult<()> {
        if let Some(mut v) = v.take() {
            if v.entries.is_empty() {
                return Ok(());
            }
            if !v.balanced() {
                res.warnings
                    .push(format!("凭证 {} 借贷不平衡，已跳过", v.voucher_no()));
                res.skipped += 1;
                return Ok(());
            }
            match save_voucher_in_tx(&tx, &mut v) {
                Ok(_) => res.ok += 1,
                Err(e) => {
                    res.warnings
                        .push(format!("凭证 {} 导入失败：{e}", v.voucher_no()));
                    res.skipped += 1;
                }
            }
            let _ = who;
        }
        Ok(())
    };

    for (i, f) in rows.iter().enumerate() {
        let Some(line) = extract_voucher_line(tmpl, f) else {
            continue; // 表头 / 说明行
        };
        if !line.warn.is_empty() {
            // 金额无法识别：丢掉该分录（整张凭证随后会因借贷不平衡被跳过），
            // 绝不按 0 写——半张凭证会静默进入账套
            for w in line.warn {
                res.warnings.push(format!("第 {} 行：{w}", i + 1));
            }
            continue;
        }
        let date = line.date;
        // 同一（日期 + 凭证号）内共一张凭证：金蝶模板带凭证号，同日期多张凭证号应分开；
        // 通用/用友没有凭证号，靠"上一张已借贷平衡"识别同一天多张凭证的边界，
        // 否则同一天的多张凭证会被合并成一张，凭证字号/张数丢失。
        let new_voucher = match &pending {
            Some(v) => {
                let date_changed = v.date != date;
                let no_changed = tmpl == ImportTemplate::Kingdee && pending_no != line.no;
                let balanced_boundary =
                    tmpl != ImportTemplate::Kingdee && !v.entries.is_empty() && v.balanced();
                date_changed || no_changed || balanced_boundary
            }
            None => true,
        };
        if new_voucher {
            flush(&mut pending, &mut res, who)?;
            pending_no = line.no;
            let word = if line.word.trim().is_empty() {
                "记"
            } else {
                line.word.trim()
            };
            let no = vouchers::next_no_of(&tx, period, word)?;
            let mut v = Voucher::new(period, date, word.to_string(), no);
            v.source = VoucherSource::Import; // 导入后为未记账，与录入一致，核对后在期末处理批量记账
            pending = Some(v);
        }
        let v = pending.as_mut().unwrap();
        let src_code = line.code.clone();
        let account_code = apply_mapping(&src_code, mapping);
        let mut entry = Entry::new(v.entries.len() as i32 + 1, account_code, line.summary);
        entry.debit = line.debit;
        entry.credit = line.credit;
        // 辅助核算（客户/银行等，CSV 里带了的直接填入）
        if line.aux.customer.is_some() {
            entry.aux.customer = line.aux.customer.clone();
        }
        if line.aux.bank.is_some() {
            entry.aux.bank = line.aux.bank.clone();
        }
        // 未填的必填辅助核算按科目表补齐（如银行科目必须填银行账户）
        fill_required(db, &mut entry);
        v.entries.push(entry);
    }
    flush(&mut pending, &mut res, who)?;
    if res.ok > 0 {
        db.log(who, "导入", "导入凭证", &format!("成功 {} 张", res.ok))?;
    }
    tx.commit()?;
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use fincore::VoucherStatus;

    #[test]
    fn csv_split_handles_quotes() {
        let f = split_csv(r#""a,1",b,"c,d""#);
        assert_eq!(f, vec!["a,1", "b", "c,d"]);
    }

    #[test]
    fn import_begin_basic() {
        let db = mem();
        let csv =
            "\u{feff}科目,方向,金额\n1001,借,10000\n100201,贷,2000\n600101,借,5000\n9999,借,1\n";
        let empty = std::collections::HashMap::new();
        let res = import_begin(&db, csv, "u1", &empty, ImportTemplate::Generic).unwrap();
        assert_eq!(res.ok, 3, "应导入 3 条：{:?}", res.warnings);
        assert_eq!(res.skipped, 1, "9999 不存在应跳过：{:?}", res.warnings);
        // 验证期初写入
        let rows = balances::list_begin(&db).unwrap();
        assert_eq!(rows.len(), 3);
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank.year_begin, Money::parse("-2000").unwrap());
    }

    #[test]
    fn analyze_missing_lists_unknown_codes() {
        let db = mem();
        let csv = "\u{feff}科目,方向,金额\n1001,借,10000\n9999,借,1\n8888,借,2\n";
        let missing = analyze_missing(&db, csv, ImportTemplate::Generic, true).unwrap();
        let codes: Vec<&str> = missing.iter().map(|m| m.code.as_str()).collect();
        assert!(codes.contains(&"9999"), "应列出 9999：{codes:?}");
        assert!(codes.contains(&"8888"), "应列出 8888：{codes:?}");
        assert!(!codes.contains(&"1001"), "已存在科目不应列出：{codes:?}");
    }

    /// 完整预检必须在**写入之前**把会失败的原因说清楚。
    ///
    /// 加这条的起因：原来的预检只回答「有没有不存在的科目」，而导入真正会失败的
    /// 常见原因还有几个 —— 期间已结账、科目已停用。用户点完「执行导入」才发现，
    /// 错的那部分数据可能已经写进去了（档案类是逐条幂等写入、凭证类更麻烦）。
    #[test]
    fn analyze_import_flags_closed_period_and_disabled_account() {
        let db = mem();

        // 干净账套：不该有任何阻断项
        let csv = "科目,方向,金额\n1001,借,10000\n";
        let pv = analyze_import(
            &db,
            csv,
            ImportTemplate::Generic,
            true,
            Some(Period::new(2026, 1).unwrap()),
        )
        .unwrap();
        assert!(!pv.blocking(), "干净账套不该阻断：{:?}", pv.issues);
        assert_eq!(pv.total_rows, 1, "应数出 1 行数据（不含表头）：{:?}", pv);
        assert!(
            pv.missing_accounts.is_empty(),
            "1001 存在：{:?}",
            pv.missing_accounts
        );

        // ① 期间已结账 → 必须拦
        db.conn()
            .execute(
                "INSERT INTO period_state(period, closed, closed_at, closed_by)
                 VALUES(202601, 1, '2026-02-01', 'u1')",
                [],
            )
            .unwrap();
        let pv = analyze_import(
            &db,
            csv,
            ImportTemplate::Generic,
            true,
            Some(Period::new(2026, 1).unwrap()),
        )
        .unwrap();
        assert!(
            pv.blocking(),
            "期间已结账必须阻断（导进去也记账不了）：{:?}",
            pv.issues
        );
        assert!(
            pv.issues.iter().any(|i| i.category == CATEGORY_CLOSED),
            "应报出「期间已结账」：{:?}",
            pv.issues
        );
        // 没结账的期间不该被误伤
        let pv_ok = analyze_import(
            &db,
            csv,
            ImportTemplate::Generic,
            true,
            Some(Period::new(2026, 2).unwrap()),
        )
        .unwrap();
        assert!(
            !pv_ok.issues.iter().any(|i| i.category == CATEGORY_CLOSED),
            "未结账期间不该报闭期：{:?}",
            pv_ok.issues
        );

        // ② 科目已停用 → 必须拦
        db.conn()
            .execute("UPDATE account SET disabled=1 WHERE code='1001'", [])
            .unwrap();
        let pv2 = analyze_import(
            &db,
            csv,
            ImportTemplate::Generic,
            true,
            Some(Period::new(2026, 2).unwrap()),
        )
        .unwrap();
        assert!(
            pv2.blocking(),
            "科目停用必须阻断（凭证过不了记账校验）：{:?}",
            pv2.issues
        );
        assert!(
            pv2.issues.iter().any(|i| i.category == CATEGORY_DISABLED),
            "应报出「科目已停用」：{:?}",
            pv2.issues
        );

        // ③ 文件内重复编码 → 只提示不阻断
        //    （期初表里同一科目按借贷方向分两行是常见写法，报 error 会误伤）
        let dup = "科目,方向,金额\n1001,借,10000\n1001,贷,2000\n";
        let pv3 = analyze_import(
            &db,
            dup,
            ImportTemplate::Generic,
            true,
            Some(Period::new(2026, 2).unwrap()),
        )
        .unwrap();
        assert!(
            pv3.issues.iter().any(|i| i.category == CATEGORY_DUP),
            "应提示编码重复：{:?}",
            pv3.issues
        );
        let dup_issue = pv3
            .issues
            .iter()
            .find(|i| i.category == CATEGORY_DUP)
            .unwrap();
        assert_eq!(dup_issue.level, "warn", "重复只应是 warn，不该阻断");
        // 期末初表的停用科目仍在，所以这里只断言「重复没被当成 error」
        assert!(
            pv3.issues
                .iter()
                .filter(|i| i.category == CATEGORY_DUP)
                .all(|i| i.level == "warn"),
            "重复编码不得是 error：{:?}",
            pv3.issues
        );

        // ④ 缺失科目可以靠映射解决 → 不算阻断
        let miss = "科目,方向,金额\n9999,借,1\n";
        let pv4 = analyze_import(
            &db,
            miss,
            ImportTemplate::Generic,
            true,
            Some(Period::new(2026, 2).unwrap()),
        )
        .unwrap();
        assert_eq!(pv4.missing_accounts.len(), 1, "应列出 9999：{:?}", pv4);
        assert!(
            !pv4.blocking(),
            "缺失科目可映射，不该阻断导入：{:?}",
            pv4.issues
        );
    }

    /// 凭证预检必须报出：借贷不平、金额不可解析、以及**会被静默丢弃的空凭证**。
    ///
    /// 最后一条是这里最要紧的：导入时 `flush` 遇到 `entries.is_empty()` 直接
    /// `return Ok(())` —— 既不计入 `skipped`，也不产生凭证级警告。一张所有分录
    /// 金额都不可解析的凭证会**人间蒸发**，用户看到「成功 N 条」以为全导进去了。
    ///
    /// 预检复用真实导入器的行提取与分组规则（`extract_voucher_line` + 同样的
    /// 新凭证边界判定），避免「预检说平、导入说跳过」那种不一致 ——
    /// 那比没有预检更糟，因为用户会不知道该信哪个。
    #[test]
    fn analyze_vouchers_flags_unbalanced_and_empty() {
        let db = mem();
        let csv = "\
日期,凭证字,摘要,科目编码,借方,贷方
2026-01-15,记,平的一笔,1001,100,0
2026-01-15,记,平的一笔,2001,0,100
2026-01-15,记,不平的一笔,1001,50,0
2026-01-15,记,不平的一笔,2001,0,30
2026-01-15,记,金额坏了,1001,abc,0
";
        let pv = analyze_import(
            &db,
            csv,
            ImportTemplate::Generic,
            false,
            Some(Period::new(2026, 1).unwrap()),
        )
        .unwrap();

        let cats: Vec<&str> = pv.issues.iter().map(|i| i.category).collect();
        assert!(
            cats.contains(&CATEGORY_UNBALANCED),
            "应报出借贷不平：{:?}",
            pv.issues
        );
        assert!(
            cats.contains(&CATEGORY_BAD_AMOUNT),
            "应报出金额无法识别：{:?}",
            pv.issues
        );
        // 带行号，用户才能定位
        let unbal = pv
            .issues
            .iter()
            .find(|i| i.category == CATEGORY_UNBALANCED)
            .unwrap();
        assert!(unbal.row.is_some(), "借贷不平必须带行号：{unbal:?}");
        assert_eq!(unbal.level, "error");
        assert!(
            pv.blocking(),
            "借贷不平 / 金额坏行会让导入丢凭证，必须阻断：{:?}",
            pv.issues
        );

        // 整张凭证的分录全被丢弃 → 静默蒸发，必须被预检抓到
        let all_bad = "\
日期,凭证字,摘要,科目编码,借方,贷方
2026-01-20,记,全是坏金额,1001,xx,0
2026-01-20,记,全是坏金额,2001,0,yy
";
        let pv2 = analyze_import(
            &db,
            all_bad,
            ImportTemplate::Generic,
            false,
            Some(Period::new(2026, 1).unwrap()),
        )
        .unwrap();
        assert!(
            pv2.issues
                .iter()
                .any(|i| i.category == CATEGORY_EMPTY_VOUCHER),
            "会被静默丢弃的空凭证必须报出来（导入时不计入 skipped、也不报警告）：{:?}",
            pv2.issues
        );
        assert!(
            pv2.blocking(),
            "空凭证蒸发是静默数据丢失，必须阻断：{:?}",
            pv2.issues
        );
    }

    /// 映射能消掉「科目缺失」：用户选完映射后预检不该还在说缺科目。
    #[test]
    fn analyze_import_respects_account_mapping() {
        let db = mem();
        let csv = "科目,方向,金额\n9999,借,100\n";
        let mut mapping = std::collections::HashMap::new();
        mapping.insert("9999".to_string(), "1001".to_string());

        // 不带映射：应报缺失
        let pv = analyze_import(&db, csv, ImportTemplate::Generic, true, None).unwrap();
        assert_eq!(pv.missing_accounts.len(), 1, "未映射时应报缺失：{:?}", pv);

        // 带映射：不该再报缺失（否则用户刚选完映射，预检还是说缺，会开始不信它）
        let pv2 =
            analyze_import_mapped(&db, csv, ImportTemplate::Generic, true, None, &mapping).unwrap();
        assert!(
            pv2.missing_accounts.is_empty(),
            "已映射的科目不该再报缺失：{:?}",
            pv2.missing_accounts
        );
    }

    #[test]
    fn import_begin_with_mapping() {
        let db = mem();
        let csv = "9999,借,10000\n";
        let mut mapping = std::collections::HashMap::new();
        mapping.insert("9999".to_string(), "1001".to_string());
        let res = import_begin(&db, csv, "u1", &mapping, ImportTemplate::Generic).unwrap();
        assert_eq!(res.ok, 1, "映射后应导入：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        assert_eq!(rows[0].account_code, "1001");
        assert_eq!(rows[0].year_begin, Money::parse("10000").unwrap());
    }

    #[test]
    fn import_begin_kingdee_template() {
        let db = mem();
        // 金蝶：科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方
        let csv = "\u{feff}科目编码,科目名称,方向,期初余额,累计借方,累计贷方\n\
                   1001,库存现金,借,10000,50000,30000\n\
                   100201,银行存款-工行,贷,2000,0,2000\n\
                   600101,主营业务收入,贷,0,0,0\n";
        let res = import_begin(
            &db,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Kingdee,
        )
        .unwrap();
        assert_eq!(res.ok, 3, "金蝶模板应导入 3 条：{:?}", res.warnings);
        assert_eq!(res.skipped, 0, "全部存在：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        assert_eq!(cash.debit_accum, Money::parse("50000").unwrap());
        assert_eq!(cash.credit_accum, Money::parse("30000").unwrap());
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank.year_begin, Money::parse("-2000").unwrap());
    }

    #[test]
    fn import_begin_yonyou_template() {
        let db = mem();
        // 用友：科目编码, 科目名称, 期初借方, 期初贷方, 累计借方, 累计贷方
        let csv = "\u{feff}科目编码,科目名称,期初借方,期初贷方,累计借方,累计贷方\n\
                   1001,库存现金,10000,0,50000,0\n\
                   100201,银行存款-工行,0,2000,0,2000\n";
        let res = import_begin(
            &db,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Yonyou,
        )
        .unwrap();
        assert_eq!(res.ok, 2, "用友模板应导入 2 条：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        assert_eq!(cash.debit_accum, Money::parse("50000").unwrap());
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank.year_begin, Money::parse("-2000").unwrap());
        assert_eq!(bank.credit_accum, Money::parse("2000").unwrap());
    }

    #[test]
    fn import_vouchers_balanced() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let csv = "\u{feff}日期,凭证字,摘要,科目,借方,贷方,客户\n\
                  2026-01-05,记,收到货款,100201,0,1000,C01\n\
                  2026-01-05,记,收到货款,600101,1000,0,\n\
                  2026-01-08,记,提现,1001,500,0,\n\
                  2026-01-08,记,提现,100201,0,500,\n";
        let res = import_vouchers(
            &db,
            p,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!(res.ok, 2, "应导入 2 张：{:?}", res.warnings);
        assert_eq!(res.skipped, 0);
        // 验证已落库并参与汇总（导入后为未记账，与录入一致）
        let all = vouchers::list(&db, &vouchers::VoucherQuery::period(p)).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].status, VoucherStatus::Draft);
    }

    #[test]
    fn import_vouchers_generic_same_day_splits_on_balance() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 同一天两张通用模板凭证（无凭证号列）：以借贷平衡为边界拆分
        let csv = "\u{feff}日期,凭证字,摘要,科目,借方,贷方\n\
                   2026-01-05,记,收到货款,100201,0,1000\n\
                   2026-01-05,记,收到货款,600101,1000,0\n\
                   2026-01-05,记,提现,1001,500,0\n\
                   2026-01-05,记,提现,100201,0,500\n";
        let res = import_vouchers(
            &db,
            p,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!(res.ok, 2, "同一天的两张凭证不应被合并：{:?}", res.warnings);
        let all = vouchers::list(&db, &vouchers::VoucherQuery::period(p)).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn import_vouchers_kingdee_template() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 金蝶：日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方
        let csv = "\u{feff}日期,凭证字,凭证号,摘要,科目编码,科目名称,借方,贷方\n\
                   2026-01-05,记,1,收到货款,100201,银行存款-工行,0,1000\n\
                   2026-01-05,记,1,收到货款,600101,主营业务收入,1000,0\n\
                   2026-01-05,记,2,提现,1001,库存现金,500,0\n\
                   2026-01-05,记,2,提现,100201,银行存款-工行,0,500\n";
        let res = import_vouchers(
            &db,
            p,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Kingdee,
        )
        .unwrap();
        assert_eq!(res.ok, 2, "金蝶模板应导入 2 张：{:?}", res.warnings);
        let all = vouchers::list(&db, &vouchers::VoucherQuery::period(p)).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all
            .iter()
            .all(|v| v.entries.is_empty() || v.entries.len() >= 2));
    }

    #[test]
    fn voucher_line_extracts_aux() {
        use fincore::AuxKind;
        // 通用：日期,凭证字,摘要,科目,借,贷[,客户,银行]
        let f = split_csv("2026-01-05,记,收货款,100201,0,1000,C01,B01");
        let line = extract_voucher_line(ImportTemplate::Generic, &f).unwrap();
        assert_eq!(
            line.aux.get(AuxKind::Customer).map(|s| s.as_str()),
            Some("C01")
        );
        assert_eq!(line.aux.get(AuxKind::Bank).map(|s| s.as_str()), Some("B01"));
        // 金蝶：日期,凭证字,凭证号,摘要,科目,科目名,借,贷[,客户,银行]
        let f = split_csv("2026-01-05,记,1,收货款,100201,银行存款,0,1000,C01,B01");
        let line = extract_voucher_line(ImportTemplate::Kingdee, &f).unwrap();
        assert_eq!(
            line.aux.get(AuxKind::Customer).map(|s| s.as_str()),
            Some("C01")
        );
        assert_eq!(line.aux.get(AuxKind::Bank).map(|s| s.as_str()), Some("B01"));
        // 用友：日期,凭证字号,摘要,科目,借,贷[,客户,银行]
        let f = split_csv("2026-01-05,记,收货款,100201,0,1000,C01,B01");
        let line = extract_voucher_line(ImportTemplate::Yonyou, &f).unwrap();
        assert_eq!(
            line.aux.get(AuxKind::Customer).map(|s| s.as_str()),
            Some("C01")
        );
        assert_eq!(line.aux.get(AuxKind::Bank).map(|s| s.as_str()), Some("B01"));
        // 无辅助列时保持空
        let line = extract_voucher_line(
            ImportTemplate::Generic,
            &split_csv("2026-01-05,记,收货款,100201,0,1000"),
        )
        .unwrap();
        assert!(line.aux.is_empty());
    }

    /// 凭证模板出去的列，预检必须认得（表头跳过、两行并成一张平的凭证、零阻断）。
    #[test]
    fn 凭证模板列能被预检认回来() {
        let t = voucher_template();
        let mut text = t.headers.join(",");
        for r in &t.examples {
            text.push('\n');
            text.push_str(&r.join(","));
        }
        let db = mem();
        let pv = analyze_import(&db, &text, ImportTemplate::Generic, false, None).unwrap();
        assert_eq!(pv.total_rows, t.examples.len(), "应为 2 行数据");
        assert_eq!(pv.errors(), 0, "模板自身不应有阻断错误：{:?}", pv.issues);
    }

    #[test]
    fn read_xlsx_fixture_parses_rows() {
        // fixture：用友模板期初表（科目编码,科目名称,期初借方,期初贷方,累计借方,累计贷方）
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/yonyou_begin.xlsx"
        );
        let rows = read_xlsx(std::path::Path::new(path)).expect("读取 fixture xlsx 应成功");
        assert_eq!(rows.len(), 3, "应为表头 + 2 行数据");
        assert_eq!(rows[0][0], "科目编码");
        assert_eq!(rows[1][0], "1001");
        assert_eq!(rows[1][2], "10000");
        assert_eq!(rows[2][0], "100201");
        assert_eq!(rows[2][3], "2000");
    }

    #[test]
    fn xlsx转文本可喂给粘贴解析() {
        // 桌面端「从 Excel 文件载入」链路：读 xlsx → 转 CSV 文本 → 粘贴解析。
        // 转出来的文本必须是行列完整的 CSV，首行是表头、后跟数据行。
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/yonyou_begin.xlsx"
        );
        let rows = read_xlsx(std::path::Path::new(path)).expect("读取 fixture xlsx 应成功");
        let text = xlsx_to_csv_text(&rows);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "应为表头 + 2 行数据：{text}");
        assert!(lines[0].starts_with("科目编码"), "表头行应在：{text}");
        assert!(lines[1].starts_with("1001,"), "数据行列不能错位：{text}");
        assert!(lines[1].contains("10000"), "金额不能丢：{text}");
    }

    #[test]
    fn import_begin_from_excel_bytes() {
        let db = mem();
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/yonyou_begin.xlsx"
        );
        let bytes = std::fs::read(path).unwrap();
        let res = import_begin_bytes(
            &db,
            &bytes,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Yonyou,
        )
        .unwrap();
        assert_eq!(res.ok, 2, "Excel 用友模板应导入 2 条：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        assert_eq!(cash.debit_accum, Money::parse("50000").unwrap());
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank.year_begin, Money::parse("-2000").unwrap());
        assert_eq!(bank.credit_accum, Money::parse("2000").unwrap());
    }

    #[test]
    fn parse_money_fullwidth_digits() {
        // 全角数字是中文用户最常粘贴的格式：偏移写错会导致金额静默归零/错乱
        assert_eq!(parse_money("１００"), Money::parse("100").unwrap());
        assert_eq!(
            parse_money("１，２３４．５６"),
            Money::parse("1234.56").unwrap()
        );
        assert_eq!(parse_money("－１２．３０"), Money::parse("-12.30").unwrap());
        // 全角字母不应变成数字（旧的 0xFFEE 偏移会把 Ａ 映射成 3）
        assert_eq!(parse_money("ＡＢＣ"), Money::ZERO);
    }

    /// 回归：`(1234.56)` 是会计惯例的**负数**。旧的 `parse_money` 把 `(` `)` 当
    /// 无效字符过滤掉，`(1234.56)` 变成 +1234.56，导入期初/凭证时方向整个反过来。
    #[test]
    fn parse_money_parenthesised_is_negative() {
        assert_eq!(
            parse_money("(1234.56)"),
            Money::parse("-1234.56").unwrap(),
            "半角括号应解析为负数"
        );
        assert_eq!(
            parse_money("（1234.56）"),
            Money::parse("-1234.56").unwrap(),
            "全角括号应解析为负数"
        );
        assert_eq!(
            parse_money("( 1,234.56 )"),
            Money::parse("-1234.56").unwrap()
        );
        assert_eq!(
            parse_money("(0)"),
            Money::ZERO,
            "红字 0 不应变成 -0 影响比较"
        );
        // 括号未闭合时不做取反，按原样过滤
        assert_eq!(parse_money("(1234.56"), Money::parse("1234.56").unwrap());
    }

    /// 回归：方向列留空/无法识别时，方向靠金额正负推断，红字括号必须推成**贷方**。
    ///
    /// 旧的 `parse_money` 把 `(2000)` 过滤成 +2000，`parse_direction` 判成借方，
    /// `import_begin_rows` 再 `abs()` 落成正 2000 —— 借贷整个反了。
    #[test]
    fn import_begin_paren_amount_keeps_credit_direction() {
        let db = mem();
        // 通用模板：方向列留空 → 方向由金额正负推断
        let csv = "\u{feff}科目,方向,金额\n100201,,(2000)\n";
        let res = import_begin(
            &db,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!(res.ok, 1, "应导入 1 条：{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        let bank = rows.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(
            bank.year_begin,
            Money::parse("-2000").unwrap(),
            "红字括号应落成贷方期初，回归前会记成 +2000（借方）"
        );

        // 金蝶模板：方向列写了无法识别的词（导出软件常写成"红字"之类）同样按金额正负推
        let db2 = mem();
        let csv2 = "\u{feff}科目编码,科目名称,方向,期初余额,累计借方,累计贷方\n\
                    100201,银行存款-工行,红字,(2000),0,0\n";
        let res2 = import_begin(
            &db2,
            csv2,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Kingdee,
        )
        .unwrap();
        assert_eq!(res2.ok, 1, "应导入 1 条：{:?}", res2.warnings);
        let rows2 = balances::list_begin(&db2).unwrap();
        let bank2 = rows2.iter().find(|r| r.account_code == "100201").unwrap();
        assert_eq!(bank2.year_begin, Money::parse("-2000").unwrap());
    }

    /// 回归：`/api/import/template?kind=begin` 导出的「通用」模板是 6 列
    /// （科目编码,科目名称,方向,期初余额,累计借方,累计贷方），而 Generic 解析器
    /// 原本按 3 列读（科目编码,方向,金额）。两边对不上时后果是**整批丢行**：
    /// 方向读到「库存现金」、金额读到「借」，`MoneyWarn` 判「借」不是金额，
    /// 每一行都进 warnings 被跳过。用户下载官方模板照着填、一条也导不进去。
    #[test]
    fn import_begin_generic_accepts_template_shape_with_account_name() {
        let db = mem();
        // 逐字照 `/api/import/template?kind=begin` 的列头与示例行
        let csv = "\u{feff}科目编码,科目名称,方向,期初余额,累计借方,累计贷方\n\
                   1001,库存现金,借,1000,0,0\n\
                   220201,应付账款,贷,3000,0,0\n";
        let res = import_begin(
            &db,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!(
            (res.ok, res.skipped),
            (2, 0),
            "通用模板 6 列形状应全部导入：{:?}",
            res.warnings
        );
        let rows = balances::list_begin(&db).unwrap();
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        let ap = rows.iter().find(|r| r.account_code == "220201").unwrap();
        assert_eq!(cash.year_begin, Money::parse("1000").unwrap(), "借方期初");
        assert_eq!(
            ap.year_begin,
            Money::parse("-3000").unwrap(),
            "贷方期初要落成负数，方向列不能被当成科目名称忽略"
        );
    }

    /// 3 列老形状（科目编码,方向,金额）不能被上一条的列错位修正带跑偏。
    #[test]
    fn import_begin_generic_still_accepts_three_column_shape() {
        let db = mem();
        let csv = "\u{feff}科目编码,方向,期初余额\n1001,借,1000\n220201,贷,3000\n";
        let res = import_begin(
            &db,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!((res.ok, res.skipped), (2, 0), "{:?}", res.warnings);
        let rows = balances::list_begin(&db).unwrap();
        assert_eq!(
            rows.iter()
                .find(|r| r.account_code == "220201")
                .unwrap()
                .year_begin,
            Money::parse("-3000").unwrap()
        );
    }

    /// 回归：现金流量项目档案原来无法经数据导入迁移（`aux_kind_of` 不认这个类型）。
    /// 列头就是接口：列顺序换掉、中间删一列，都不能错位。
    ///
    /// 这一条守的是 `HeadMap`。按列下标取值时，用户把「备注」挪到第一列、
    /// 「编码」挪到最后，导进去就会变成：名称写进编码、税点写进名称 —— 不报错，
    /// 数据全歪。现实里这种事天天发生：老板觉得备注碍眼就拖到最右边。
    #[test]
    fn 表头认列名所以列顺序可以换() {
        let db = mem();
        // 故意打乱顺序，并删掉「上级编码」「状态」两列
        let text = "备注\t税点%\t名称\t类型\t编码\n月结30天\t13\t客户甲\t客户\tC001\n";
        let res = import_aux(&db, text, "tester").unwrap();
        assert_eq!((res.ok, res.skipped), (1, 0), "{:?}", res.warnings);
        let e = crate::auxs::get(&db, fincore::AuxKind::Customer, "C001")
            .unwrap()
            .expect("客户没导进去 —— 列顺序换了就认不出编码了");
        assert_eq!(e.name, "客户甲");
        assert_eq!(e.memo, "月结30天", "备注被写到了别处");
        assert_eq!(
            e.prop(fincore::auxiliary::prop::TAX_RATE).map(|s| s.as_str()),
            Some("13"),
            "税点跟着列名走，不该看位置"
        );
    }

    /// 老格式（没有表头、按位置写的四列）行为必须一点不变。
    #[test]
    fn 没有表头的老文件仍按位置解析() {
        let db = mem();
        let text = "客户\tC002\t客户乙\t现金结算\n";
        let res = import_aux(&db, text, "tester").unwrap();
        assert_eq!((res.ok, res.skipped), (1, 0), "{:?}", res.warnings);
        let e = crate::auxs::get(&db, fincore::AuxKind::Customer, "C002")
            .unwrap()
            .expect("老格式导不进去了");
        assert_eq!(e.name, "客户乙");
        assert_eq!(e.memo, "现金结算");
    }

    /// 期初的分户（客户编码）与数量也要能从表里带回来。
    #[test]
    fn 期初表能带分户与数量() {
        let db = mem();
        let text = "科目编码\t科目名称\t方向\t期初余额\t累计借方\t累计贷方\t数量\t客户编码\n\
                    112201\t应收账款-货款\t借\t50000\t0\t0\t\tC001\n\
                    4001\t实收资本\t贷\t50000\t0\t0\t\t\n";
        let res = import_begin(
            &db,
            text,
            "tester",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!((res.ok, res.skipped), (2, 0), "{:?}", res.warnings);
        let rows = crate::balances::list_begin(&db).unwrap();
        let ar = rows
            .iter()
            .find(|r| r.account_code == "112201")
            .expect("应收账款期初没进去");
        assert_eq!(ar.aux.customer.as_deref(), Some("C001"), "分户丢了");
    }

    /// 期初表的列顺序也能换：有表头就别再靠「第 2 列是不是借/贷」猜布局。
    ///
    /// 这条守的是 `extract_begin_line` 里那个按位置猜 A/B 布局的老逻辑 ——
    /// 一旦文件带表头，猜就不该再发生。把「期初余额」和「方向」对调，
    /// 按位置猜会把金额当成方向、方向当成金额，整批进 warnings 被丢掉。
    #[test]
    fn 期初表列顺序换了也认得() {
        let db = mem();
        let text = "科目编码\t期初余额\t方向\t科目名称\t累计借方\t累计贷方\n\
                    1001\t10000\t借\t库存现金\t0\t0\n\
                    4001\t10000\t贷\t实收资本\t0\t0\n";
        let res = import_begin(
            &db,
            text,
            "tester",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!((res.ok, res.skipped), (2, 0), "{:?}", res.warnings);
        let rows = crate::balances::list_begin(&db).unwrap();
        let cash = rows.iter().find(|r| r.account_code == "1001").unwrap();
        assert_eq!(cash.year_begin, Money::parse("10000").unwrap());
        let cap = rows.iter().find(|r| r.account_code == "4001").unwrap();
        assert_eq!(
            cap.year_begin,
            Money::parse("-10000").unwrap(),
            "方向列换了位置后贷方要被认出来，不能当成借方"
        );
    }

    #[test]
    fn import_aux_accepts_cashflow_kind() {
        let db = mem();
        let csv = "\u{feff}类型,编码,名称,备注\n\
                   客户,C001,客户甲,\n\
                   现金流量,CF01,销售商品收到的现金,\n";
        let res = import_aux(&db, csv, "u1").unwrap();
        assert_eq!((res.ok, res.skipped), (2, 0), "{:?}", res.warnings);
        let cf = crate::auxs::get(&db, fincore::AuxKind::CashFlow, "CF01")
            .unwrap()
            .expect("现金流量项目应已建档");
        assert_eq!(cf.name, "销售商品收到的现金");
    }

    /// 回归：金额列无法识别必须丢行 + 报警告，而不是静默按 0 记账。
    #[test]
    fn import_begin_bad_amount_warns_and_skips_row() {
        let db = mem();
        let csv = "\u{feff}科目,方向,金额\n1001,借,待定\n100201,借,2000\n";
        let res = import_begin(
            &db,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!(res.ok, 1, "只应导入金额正常的一行：{:?}", res.warnings);
        assert_eq!(res.skipped, 1);
        assert!(
            res.warnings.iter().any(|w| w.contains("待定")),
            "应报出无法识别的金额：{:?}",
            res.warnings
        );
        let rows = balances::list_begin(&db).unwrap();
        assert_eq!(rows.len(), 1, "坏行不应落库：{:?}", rows);
        assert!(rows.iter().all(|r| r.account_code != "1001"));
    }

    /// 回归：一张凭证在写分录中途失败时，不能留下"表头 + 半张分录"。
    ///
    /// `vouchers::save_in` 先插表头再逐条插分录；导入循环此前只是记一条警告就继续，
    /// 末尾的 `commit()` 会把没通过任何校验的半张凭证一起提交进账套。
    /// 回归用一条唯一索引制造"分录写到一半必失败"的局面（同一张凭证里两条同科目分录）。
    #[test]
    fn import_vouchers_mid_failure_leaves_no_partial_voucher() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        db.conn()
            .execute_batch(
                "CREATE UNIQUE INDEX ux_test_entry ON voucher_entry(voucher_id, account_code)",
            )
            .unwrap();
        // 第 1 张（两科目）正常；第 2 张两条同科目分录 → 第二条必触发唯一约束
        let csv = "\u{feff}日期,凭证字,摘要,科目,借方,贷方\n\
                  2026-01-05,记,收货款,100201,0,1000\n\
                  2026-01-05,记,收货款,600101,1000,0\n\
                  2026-01-06,记,冲销,1001,0,500\n\
                  2026-01-06,记,冲销,1001,500,0\n";
        let res = import_vouchers(
            &db,
            p,
            csv,
            "u1",
            &std::collections::HashMap::new(),
            ImportTemplate::Generic,
        )
        .unwrap();
        assert_eq!(res.ok, 1, "只有第一张应成功：{:?}", res.warnings);
        assert_eq!(res.skipped, 1, "失败的第二张应计跳过：{:?}", res.warnings);
        assert!(
            res.warnings.iter().any(|w| w.contains("导入失败")),
            "应记录失败原因：{:?}",
            res.warnings
        );
        let cnt = |sql: &str| -> i64 { db.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            cnt("SELECT COUNT(*) FROM voucher"),
            1,
            "失败凭证的表头不应留下"
        );
        assert_eq!(
            cnt("SELECT COUNT(*) FROM voucher_entry"),
            2,
            "只应留下第一张的 2 条分录（半张凭证已落库）"
        );
        // 逐张核对借贷合计，确保账上没有任何不平衡的凭证
        let mut st = db
            .conn()
            .prepare("SELECT voucher_id, debit, credit FROM voucher_entry ORDER BY id")
            .unwrap();
        let rows: Vec<(i64, String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(st);
        for id in rows
            .iter()
            .map(|(id, _, _)| *id)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let d: Money = rows
                .iter()
                .filter(|(i, _, _)| *i == id)
                .map(|(_, d, _)| Money::parse_or_zero(d))
                .sum();
            let c: Money = rows
                .iter()
                .filter(|(i, _, _)| *i == id)
                .map(|(_, _, c)| Money::parse_or_zero(c))
                .sum();
            assert_eq!(d, c, "凭证 #{id} 借贷不平衡（半张凭证落库）");
        }
    }
}
