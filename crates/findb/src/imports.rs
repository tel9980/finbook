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
    /// 源文件里的科目名称（导入不落库，仅用于报错/提示回显，
    /// 让用户看得到"1002 是什么"而不是只对着编码猜）
    name: String,
    direction: String,
    amount: Money,
    /// 累计借方（金蝶/用友有此列，通用格式无）
    debit_accum: Option<Money>,
    /// 累计贷方
    credit_accum: Option<Money>,
    /// 期初数量（数量金额式科目的期初数量，可选列；无则 None）
    qty: Option<Money>,
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
}

/// 按模板从 CSV 行提取期初余额行
fn extract_begin_line(tmpl: ImportTemplate, f: &[String]) -> Option<BeginLine> {
    let code = f.first()?.trim().to_string();
    if code.is_empty() || !code.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    match tmpl {
        ImportTemplate::Generic => {
            // 通用模板 3 列：科目编码, 方向, 金额（历史格式，保持不变）
            // 若第 2 列是"借/贷"以外的文本，则按 5 列格式
            //（科目编码, 科目名称, 方向, 期初余额, [累计借方, 累计贷方]）解析，
            // 取名称仅用于报错回显，不影响金额列的判定。
            if f.len() < 2 {
                return None;
            }
            let col1 = f.get(1).unwrap_or(&String::new()).trim().to_string();
            let looks_like_5col = !col1.is_empty()
                && col1 != "借"
                && col1 != "贷"
                && col1 != "debit"
                && col1 != "credit"
                && f.len() >= 3;
            let (name, dir_col, amt_col) = if looks_like_5col {
                (col1, 2, 3)
            } else {
                (String::new(), 1, 2)
            };
            let amt = parse_money(f.get(amt_col).unwrap_or(&String::new()));
            Some(BeginLine {
                code,
                name,
                direction: f.get(dir_col).unwrap_or(&String::new()).trim().to_string(),
                amount: amt,
                debit_accum: if looks_like_5col {
                    f.get(4).map(|s| parse_money(s))
                } else {
                    None
                },
                credit_accum: if looks_like_5col {
                    f.get(5).map(|s| parse_money(s))
                } else {
                    None
                },
                qty: if looks_like_5col {
                    f.get(6).map(|s| parse_money(s))
                } else {
                    None
                },
            })
        }
        ImportTemplate::Kingdee => {
            // 科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方
            if f.len() < 4 {
                return None;
            }
            let amt = parse_money(f.get(3).unwrap_or(&String::new()));
            Some(BeginLine {
                code,
                name: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                direction: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                amount: amt,
                debit_accum: f.get(4).map(|s| parse_money(s)),
                credit_accum: f.get(5).map(|s| parse_money(s)),
                qty: f.get(6).map(|s| parse_money(s)),
            })
        }
        ImportTemplate::Yonyou => {
            // 科目编码, 科目名称, 期初借方, 期初贷方, 累计借方, 累计贷方
            if f.len() < 4 {
                return None;
            }
            let d = parse_money(f.get(2).unwrap_or(&String::new()));
            let c = parse_money(f.get(3).unwrap_or(&String::new()));
            let dir = if d > Money::ZERO { "借" } else { "贷" };
            let amt = if d > Money::ZERO { d } else { c };
            Some(BeginLine {
                code,
                name: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                direction: dir.to_string(),
                amount: amt,
                debit_accum: f.get(4).map(|s| parse_money(s)),
                credit_accum: f.get(5).map(|s| parse_money(s)),
                qty: f.get(6).map(|s| parse_money(s)),
            })
        }
    }
}

/// 取 CSV 第 `i` 列（0 起），空串返回 None
fn opt_field(f: &[String], i: usize) -> Option<String> {
    f.get(i).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// 按模板从 CSV 行提取凭证行
fn extract_voucher_line(tmpl: ImportTemplate, f: &[String]) -> Option<VoucherLine> {
    if f.len() < 4 {
        return None;
    }
    let date = parse_date(&f[0])?;
    let mut aux = fincore::AuxRef::default();
    match tmpl {
        ImportTemplate::Generic => {
            // 日期, 凭证字, 摘要, 科目编码, 借方, 贷方[, 客户, 银行]
            aux.customer = opt_field(f, 6);
            aux.bank = opt_field(f, 7);
            Some(VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: None,
                summary: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                debit: parse_money(f.get(4).unwrap_or(&String::new())),
                credit: parse_money(f.get(5).unwrap_or(&String::new())),
                aux,
            })
        }
        ImportTemplate::Kingdee => {
            // 日期, 凭证字, 凭证号, 摘要, 科目编码, 科目名称, 借方, 贷方[, 客户, 银行]
            aux.customer = opt_field(f, 8);
            aux.bank = opt_field(f, 9);
            Some(VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: f.get(2).and_then(|s| s.trim().parse().ok()),
                summary: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(4).unwrap_or(&String::new()).trim().to_string(),
                debit: parse_money(f.get(6).unwrap_or(&String::new())),
                credit: parse_money(f.get(7).unwrap_or(&String::new())),
                aux,
            })
        }
        ImportTemplate::Yonyou => {
            // 日期, 凭证字号, 摘要, 科目编码, 借方, 贷方[, 客户, 银行]
            aux.customer = opt_field(f, 6);
            aux.bank = opt_field(f, 7);
            Some(VoucherLine {
                date,
                word: f.get(1).unwrap_or(&String::new()).trim().to_string(),
                no: None,
                summary: f.get(2).unwrap_or(&String::new()).trim().to_string(),
                code: f.get(3).unwrap_or(&String::new()).trim().to_string(),
                debit: parse_money(f.get(4).unwrap_or(&String::new())),
                credit: parse_money(f.get(5).unwrap_or(&String::new())),
                aux,
            })
        }
    }
}

/// 读取 Excel 文件（.xlsx/.xls/.ods）第一个 sheet，返回所有行（每行是单元格列表）
pub fn read_xlsx(path: &std::path::Path) -> Result<Vec<Vec<String>>, fincore::FinError> {
    let bytes = std::fs::read(path)
        .map_err(|e| fincore::FinError::io(format!("读取文件失败：{e}")))?;
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
    if range.height() > MAX_IMPORT_ROWS || range.height().saturating_mul(range.width()) > MAX_IMPORT_CELLS
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

fn parse_money(s: &str) -> Money {
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
    // 去掉千分位逗号与货币符号，兼容 "1,234.56" / "¥1,234.56" / "−100"
    let cleaned: String = normalized
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    Money::parse(&cleaned).unwrap_or(Money::ZERO)
}

fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '-').collect();
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
fn collect_codes(rows: &[Vec<String>], tmpl: ImportTemplate, is_begin: bool) -> Vec<String> {
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
    let mut out: Vec<String> = map.keys().cloned().collect();
    out.sort();
    out
}

/// 预检：找出文件中引用但账套里不存在的科目（供用户选择映射或忽略）
///
/// - `tmpl`：来源模板（决定凭证的科目列位置）
/// - `is_begin = true`：期初余额表（第 1 列是科目）；`false`：凭证
/// 类型别名 → AuxKind（客户/供应商/部门/职员/项目/银行/存货 + 英文）
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
    let mut res = ImportResult { ok: 0, skipped: 0, warnings: Vec::new() };
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 3 {
            continue; // 空行/说明行
        }
        let kind_s = master_cell(f, 0);
        let code = master_cell(f, 1);
        if code.is_empty() || code == "编码" || code.eq_ignore_ascii_case("code") {
            continue; // 表头
        }
        let Some(kind) = aux_kind_of(&kind_s) else {
            res.warnings.push(format!("第 {} 行：类型「{kind_s}」无法识别，已跳过", i + 1));
            res.skipped += 1;
            continue;
        };
        let name = master_cell(f, 2);
        if name.is_empty() {
            res.warnings.push(format!("第 {} 行：{code} 名称为空，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::auxs::get(db, kind, &code)?.is_some() {
            res.skipped += 1;
            continue; // 已存在静默跳过（重导常见）
        }
        let mut e = fincore::auxiliary::AuxEntity::new(kind, code.clone(), name);
        e.memo = master_cell(f, 3);
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
    let mut res = ImportResult { ok: 0, skipped: 0, warnings: Vec::new() };
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 2 {
            continue;
        }
        let code = master_cell(f, 0);
        if code.is_empty() || code == "编码" || code.eq_ignore_ascii_case("code") {
            continue;
        }
        let name = master_cell(f, 1);
        if name.is_empty() {
            res.warnings.push(format!("第 {} 行：{code} 名称为空，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::auxs::get(db, fincore::AuxKind::Item, &code)?.is_some() {
            res.skipped += 1;
            continue;
        }
        let mut e = fincore::auxiliary::AuxEntity::new(fincore::AuxKind::Item, code.clone(), name);
        let shelf = master_cell(f, 2);
        if !shelf.is_empty() {
            e.props.insert("shelf_life_days".into(), shelf);
        }
        crate::auxs::insert(db, &e)?;
        // 安全库存（列4）→ item_plan（MRP/低库存预警口径）
        let safety = master_cell(f, 3);
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
    let mut res = ImportResult { ok: 0, skipped: 0, warnings: Vec::new() };
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 2 {
            continue;
        }
        let code = master_cell(f, 0);
        if code.is_empty() || code == "编码" || code == "科目编码" || code.eq_ignore_ascii_case("code") {
            continue;
        }
        let name = master_cell(f, 1);
        if name.is_empty() {
            res.warnings.push(format!("第 {} 行：{code} 名称为空，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::accounts::get(db, &code)?.is_some() {
            res.skipped += 1;
            continue;
        }
        let cat = acct_category_of(&code, &master_cell(f, 2));
        let mut acc = fincore::account::Account::new(code.clone(), name, cat);
        match master_cell(f, 3).as_str() {
            "贷" | "credit" => acc.dir = fincore::Direction::Credit,
            "借" | "debit" => acc.dir = fincore::Direction::Debit,
            _ => {} // 空 = 类别默认方向
        }
        acc.memo = master_cell(f, 4);
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
    let mut res = ImportResult { ok: 0, skipped: 0, warnings: Vec::new() };
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
            res.warnings.push(format!("第 {} 行：仓库为空或数量非正，已跳过", i + 1));
            res.skipped += 1;
            continue;
        }
        if crate::auxs::get(db, fincore::AuxKind::Item, &item)?.is_none() {
            res.warnings.push(format!("第 {} 行：存货 {item} 未建档（先导存货档案），已跳过", i + 1));
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
                memo: if memo.is_empty() { "期初库存".into() } else { format!("期初 {memo}") },
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
                        (d + chrono::Duration::days(days)).format("%Y-%m-%d").to_string()
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
    let mut res = ImportResult { ok: 0, skipped: 0, warnings: Vec::new() };
    for (i, f) in rows.iter().enumerate() {
        if f.len() < 5 {
            continue;
        }
        let type_s = master_cell(f, 0);
        if type_s == "类型" || type_s.eq_ignore_ascii_case("type") {
            continue; // 表头
        }
        let Some(kind) = arap_kind_of(&type_s) else {
            res.warnings.push(format!("第 {} 行：类型「{type_s}」应为 应收/应付，已跳过", i + 1));
            res.skipped += 1;
            continue;
        };
        let party = master_cell(f, 1);
        let doc_no = master_cell(f, 2);
        let doc_date = master_cell(f, 3);
        let amount = Money::parse_or_zero(&master_cell(f, 4));
        if party.is_empty() || doc_no.is_empty() {
            res.warnings.push(format!("第 {} 行：客商编码或单据号为空，已跳过", i + 1));
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
            res.warnings.push(format!("第 {} 行：金额必须大于 0，已跳过", i + 1));
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

/// 执行导入：把源科目编码按 `mapping`（源编码 → 目标编码）替换后再写入。
/// 不在 mapping 里的缺失科目会跳过并警告；已存在科目不受影响。
pub fn apply_mapping(code: &str, mapping: &std::collections::HashMap<String, String>) -> String {
    mapping.get(code).cloned().unwrap_or_else(|| code.to_string())
}

/// 从源文件提取「科目编码 → 科目名称」去重集合（首次出现优先），供补建缺失科目用。
/// 科目列位置随来源模板不同：期初恒为第 1 列；凭证通用/用友为第 4 列，金蝶为第 5 列。
/// 名称列：期初 A 格式为第 2 列；凭证金蝶为第 6 列（索引 5），其余模板无名称列。
fn collect_code_names(
    rows: &[Vec<String>],
    tmpl: ImportTemplate,
    is_begin: bool,
) -> std::collections::BTreeMap<String, String> {
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for f in rows {
        let code = if is_begin {
            master_cell(f, 0)
        } else {
            match tmpl {
                ImportTemplate::Kingdee => master_cell(f, 4),
                _ => master_cell(f, 3),
            }
        };
        if code.is_empty() || !code.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let name = if is_begin {
            master_cell(f, 1)
        } else if tmpl == ImportTemplate::Kingdee {
            master_cell(f, 5)
        } else {
            String::new()
        };
        map.entry(code).or_insert(name);
    }
    map
}

/// 补建源文件中引用但账套不存在的科目：类别按编码首位推、方向按类别默认。
/// 复用 `import_accounts` 的建档口径（已存在跳过、逐条幂等），因此可安全重跑。
///
/// 这是「预检发现缺失 → 一键补建 → 重新导入」闭环里的第二步，
/// 让会计不用手动去科目表里逐个补科目。
pub fn autocreate_missing_accounts(
    db: &Db,
    text: &str,
    tmpl: ImportTemplate,
    is_begin: bool,
    who: &str,
) -> DbResult<ImportResult> {
    let rows = text_to_rows(text);
    let chart = crate::accounts::chart(db)?;
    let mut res = ImportResult {
        ok: 0,
        skipped: 0,
        warnings: Vec::new(),
    };
    for (code, name) in collect_code_names(&rows, tmpl, is_begin) {
        // 已存在（含父级/末级）一律跳过，不动已有科目
        if chart.get(&code).is_some() {
            continue;
        }
        let label = if name.trim().is_empty() { "" } else { name.trim() };
        let cat = acct_category_of(&code, label);
        let mut acc = fincore::account::Account::new(code.clone(), label.to_string(), cat);
        acc.memo = "导入期初时自动补建".to_string();
        crate::accounts::insert(db, &acc)?;
        res.ok += 1;
    }
    if res.ok > 0 {
        db.log(who, "导入", "补建缺失科目", &format!("成功 {} 条", res.ok))?;
    }
    Ok(res)
}

/// 期初导入快照 key（存在 meta 表，值 = `Vec<BeginRow>` 的 JSON）
const BEGIN_SNAPSHOT_KEY: &str = "begin_import_snapshot";

/// 导入期初前，把当前 `begin_balance` 全表快照到 meta，供「撤销上次导入」回滚。
/// 只存一份（最近一次导入前的状态），再次导入会覆盖上一份快照。
pub fn snapshot_begin(db: &Db) -> DbResult<()> {
    let rows = balances::list_begin(db)?;
    db.meta_set_json(BEGIN_SNAPSHOT_KEY, &rows)
}

/// 撤销上次期初导入：清空 `begin_balance` 并重建为导入前的快照。
/// 无快照时返回 0（无从撤销），有快照则重建并返回还原的行数。
///
/// 幂等：撤销后快照保留，可再次调用；重建前先整体删除，避免新旧混合。
pub fn undo_last_begin_import(db: &Db, who: &str) -> DbResult<usize> {
    let snapshot: Vec<BeginRow> = match db.meta_json(BEGIN_SNAPSHOT_KEY) {
        Some(v) => v,
        None => return Ok(0),
    };
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM begin_balance", [])?;
    for r in &snapshot {
        balances::upsert_begin_on(&tx, r)?;
    }
    tx.commit()?;
    db.log(who, "导入", "撤销上次期初导入", &format!("还原 {} 条", snapshot.len()))?;
    Ok(snapshot.len())
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
        e.price = Some(if e.debit.is_positive() { e.debit } else { e.credit });
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
    // 导入前先把当前期初快照到 meta，供「撤销上次导入」一键回滚（覆盖上一份快照）
    snapshot_begin(db)?;
    let tx = db.write_tx()?;
    for (i, f) in rows.iter().enumerate() {
        let Some(line) = extract_begin_line(tmpl, f) else {
            continue; // 表头 / 说明行
        };
        // 源科目 → 目标科目（用户映射）
        let src_code = line.code.clone();
        let code = apply_mapping(&src_code, mapping);
        let amt = line.amount;
        // 错误提示里的科目标签：优先用源文件带的科目名称，没有才退回纯编码
        let label = if line.name.trim().is_empty() {
            String::new()
        } else {
            format!("源名称「{}」，", line.name.trim())
        };
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
                        aux: AuxRef::default(),
                        year_begin: signed,
                        debit_accum: line.debit_accum.unwrap_or(Money::ZERO),
                        credit_accum: line.credit_accum.unwrap_or(Money::ZERO),
                        qty_begin: line.qty,
                    },
                )?;
                res.ok += 1;
            }
            Some(_) => {
                res.warnings.push(format!(
                    "第 {} 行：科目 {code}（{label}源 {src_code}）非末级，不能记期初，已跳过",
                    i + 1
                ));
                res.skipped += 1;
            }
            None => {
                res.warnings.push(format!(
                    "第 {} 行：科目 {code}（{label}源 {src_code}）不存在，已跳过",
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
                res.warnings.push(format!(
                    "凭证 {} 借贷不平衡，已跳过",
                    v.voucher_no()
                ));
                res.skipped += 1;
                return Ok(());
            }
            match vouchers::save_in(&tx, &mut v) {
                Ok(_) => res.ok += 1,
                Err(e) => {
                    res.warnings.push(format!("凭证 {} 导入失败：{e}", v.voucher_no()));
                    res.skipped += 1;
                }
            }
            let _ = who;
        }
        Ok(())
    };

    for f in rows.iter() {
        let Some(line) = extract_voucher_line(tmpl, f) else {
            continue; // 表头 / 说明行
        };
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
            let word = if line.word.trim().is_empty() { "记" } else { line.word.trim() };
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
        let csv = "\u{feff}科目,方向,金额\n1001,借,10000\n100201,贷,2000\n600101,借,5000\n9999,借,1\n";
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
        let res =
            import_begin(&db, csv, "u1", &std::collections::HashMap::new(), ImportTemplate::Kingdee)
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
        let res =
            import_begin(&db, csv, "u1", &std::collections::HashMap::new(), ImportTemplate::Yonyou)
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
        assert!(all.iter().all(|v| v.entries.is_empty() || v.entries.len() >= 2));
    }

    #[test]
    fn voucher_line_extracts_aux() {
        use fincore::AuxKind;
        // 通用：日期,凭证字,摘要,科目,借,贷[,客户,银行]
        let f = split_csv("2026-01-05,记,收货款,100201,0,1000,C01,B01");
        let line = extract_voucher_line(ImportTemplate::Generic, &f).unwrap();
        assert_eq!(line.aux.get(AuxKind::Customer).map(|s| s.as_str()), Some("C01"));
        assert_eq!(line.aux.get(AuxKind::Bank).map(|s| s.as_str()), Some("B01"));
        // 金蝶：日期,凭证字,凭证号,摘要,科目,科目名,借,贷[,客户,银行]
        let f = split_csv("2026-01-05,记,1,收货款,100201,银行存款,0,1000,C01,B01");
        let line = extract_voucher_line(ImportTemplate::Kingdee, &f).unwrap();
        assert_eq!(line.aux.get(AuxKind::Customer).map(|s| s.as_str()), Some("C01"));
        assert_eq!(line.aux.get(AuxKind::Bank).map(|s| s.as_str()), Some("B01"));
        // 用友：日期,凭证字号,摘要,科目,借,贷[,客户,银行]
        let f = split_csv("2026-01-05,记,收货款,100201,0,1000,C01,B01");
        let line = extract_voucher_line(ImportTemplate::Yonyou, &f).unwrap();
        assert_eq!(line.aux.get(AuxKind::Customer).map(|s| s.as_str()), Some("C01"));
        assert_eq!(line.aux.get(AuxKind::Bank).map(|s| s.as_str()), Some("B01"));
        // 无辅助列时保持空
        let line = extract_voucher_line(ImportTemplate::Generic, &split_csv("2026-01-05,记,收货款,100201,0,1000")).unwrap();
        assert!(line.aux.is_empty());
    }

    #[test]
    fn read_xlsx_fixture_parses_rows() {
        // fixture：用友模板期初表（科目编码,科目名称,期初借方,期初贷方,累计借方,累计贷方）
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/yonyou_begin.xlsx");
        let rows = read_xlsx(std::path::Path::new(path)).expect("读取 fixture xlsx 应成功");
        assert_eq!(rows.len(), 3, "应为表头 + 2 行数据");
        assert_eq!(rows[0][0], "科目编码");
        assert_eq!(rows[1][0], "1001");
        assert_eq!(rows[1][2], "10000");
        assert_eq!(rows[2][0], "100201");
        assert_eq!(rows[2][3], "2000");
    }

    #[test]
    fn import_begin_from_excel_bytes() {
        let db = mem();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/yonyou_begin.xlsx");
        let bytes = std::fs::read(path).unwrap();
        let res = import_begin_bytes(&db, &bytes, "u1", &std::collections::HashMap::new(), ImportTemplate::Yonyou)
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
        assert_eq!(parse_money("１，２３４．５６"), Money::parse("1234.56").unwrap());
        assert_eq!(parse_money("－１２．３０"), Money::parse("-12.30").unwrap());
        // 全角字母不应变成数字（旧的 0xFFEE 偏移会把 Ａ 映射成 3）
        assert_eq!(parse_money("ＡＢＣ"), Money::ZERO);
    }
}