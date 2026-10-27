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
use fincore::money::Money;

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
fn find_header(rows: &[Vec<String>]) -> Option<(usize, Vec<String>)> {
    for (i, r) in rows.iter().enumerate().take(12) {
        let joined: String = r.join("");
        if joined.contains("编码") || joined.contains("科目") || joined.contains("项目") {
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

/// 一张「科目余额」纸（③）的解析结果。
fn parse_balances(rows: &[Vec<String>]) -> Vec<(String, Money)> {
    let mut out = Vec::new();
    let Some((hr, head)) = find_header(rows) else {
        return out;
    };
    let c_code = col(&head, &["科目编码", "编码"], 0);
    let c_amt = col(&head, &["余额", "金额"], 2);
    for r in rows.iter().skip(hr + 1) {
        let code = cell(r, c_code);
        if code.is_empty() || !code.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Some(m) = money_of(&cell(r, c_amt)) {
            if !m.is_zero() {
                out.push((code, m));
            }
        }
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
    let mut begin_rows: Vec<(String, Money)> = Vec::new();
    for (code, amt) in &bal_rows {
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
        begin_rows.push((code.clone(), signed));
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
        begin_rows.push(("4001".to_string(), -paid_in));
    }
    if has_profit {
        begin_rows.push(("4103".to_string(), -profit));
    }

    // ---- 平衡：逐条把 begin_rows 按方向加总（资产类记借，其余记贷），
    //      再加上「应收台账合计」「应付台账合计」这两个还没进 begin_rows 的数。
    let mut debit = res.ar.total;
    let mut credit = res.ap.total;
    for (code, signed) in &begin_rows {
        if code.starts_with('1') {
            debit += *signed;
        } else {
            credit += -*signed;
        }
    }

    // 若用户只填了实收资本、没填本年利润，用差额补齐 4103（收集表④本身也是这么轧的）
    if !has_profit && !paid_in.is_zero() {
        let diff = debit - credit;
        if !diff.is_zero() {
            profit += diff;
            res.auto_profit = diff;
            begin_rows.push(("4103".to_string(), -profit));
            credit += diff;
            res.equity.warnings.push(format!(
                "「本年利润」没填，已按轧差补 {} —— 请核对这个数是前期赚的（或亏的）",
                profit.fmt_money()
            ));
        }
    }

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
        begin_rows.push(("112201".to_string(), res.ar.total));
    }
    if res.ap.total.is_positive() {
        begin_rows.push(("220201".to_string(), -res.ap.total));
    }

    for (code, signed) in &begin_rows {
        crate::balances::upsert_begin(
            db,
            &crate::balances::BeginRow {
                id: 0,
                account_code: code.clone(),
                aux: fincore::voucher::AuxRef::default(),
                year_begin: *signed,
                debit_accum: Money::ZERO,
                credit_accum: Money::ZERO,
                qty_begin: None,
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
