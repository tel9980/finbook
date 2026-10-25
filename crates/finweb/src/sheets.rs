//! 把账套数据拼成「导出用」的工作表。
//!
//! # 为什么列头要和 `findb::imports` 写在一起看
//!
//! 基础资料导出的列头必须与三处逐字一致：
//! 1. `crates/findb/src/imports.rs` 的解析器（按**列下标**取值，不看列名）；
//! 2. `/api/import/template` 给出的模板（用户照它手填）；
//! 3. 本文件的导出（用户导出去改了再导回来）。
//!
//! 三者任何一处改了而另两处没跟上，用户拿到的就是「导得出来、导不回去」的文件，
//! 而且症状通常不是报错，是**少了几条**或**数值整体错位**。把取数放在这里，
//! 至少改动时能一眼看到相邻的两处注释，不用在 1.6 万行的 handlers.rs 里翻。
//!
//! 报表类的导出没有回导要求，所以用带标题行的排版（`Sheet::titled`）；
//! 基础资料类**不能**带标题行 —— 解析器靠「第 1 列是不是表头」跳行，
//! 多一行标题会被当成一条编码为空的脏数据。

use fincore::account::Account;
use fincore::balance::{BalanceRow, TrialBalance};
use fincore::Money;
use fincore::report::ReportTable;
use fincore::AuxEntity;
use findb::balances::BeginRow;

use crate::xlsxout::{ColFmt, Sheet};

const T: ColFmt = ColFmt::Text;
const M: ColFmt = ColFmt::Money;
const I: ColFmt = ColFmt::Int;

/// 辅助核算档案：列头与 `import_aux_rows` 一致（类型, 编码, 名称, 备注）
pub fn aux_sheet(rows: &[AuxEntity], kind_label: &str) -> Sheet {
    let rows = rows
        .iter()
        .map(|e| {
            vec![
                e.kind.label().to_string(),
                e.code.clone(),
                e.name.clone(),
                e.memo.clone(),
            ]
        })
        .collect();
    Sheet::plain(
        &format!("{kind_label}档案"),
        &["类型", "编码", "名称", "备注"],
        &[T, T, T, T],
    )
    .with_rows(rows)
}

/// 存货档案：列头与 `import_items_rows` 一致（编码, 名称, 保质期天, 安全库存）
///
/// `safety` 是 `item_plan` 里的安全库存（低库存预警口径），与档案分开存在两张表，
/// 导出时合起来；导回时 `import_items_rows` 也会拆回去。
pub fn items_sheet(rows: &[AuxEntity], safety: &dyn Fn(&str) -> Option<Money>) -> Sheet {
    let rows = rows
        .iter()
        .map(|e| {
            vec![
                e.code.clone(),
                e.name.clone(),
                e.props.get("shelf_life_days").cloned().unwrap_or_default(),
                safety(&e.code).map(|m| m.fmt_plain()).unwrap_or_default(),
            ]
        })
        .collect();
    Sheet::plain(
        "存货档案",
        &["编码", "名称", "保质期天", "安全库存"],
        &[T, T, I, M],
    )
    .with_rows(rows)
}

/// 科目建档：列头与 `import_accounts_rows` 一致（编码, 名称, 类别, 方向, 备注）
///
/// 第 6 列「辅助核算维度」是**只读提示**：导入器读的是下标 0~4，这一列不进库。
/// 标出来是因为辅助核算维度决定了这个科目记账时要不要填客户/存货，
/// 搬账套的人需要看到「哪些科目是核算维度科目」，否则会以为科目搬全了。
pub fn accounts_sheet(accounts: &[&Account]) -> Sheet {
    let rows = accounts
        .iter()
        .filter(|a| !a.disabled)
        .map(|a| {
            let dims = fincore::AuxKind::ALL
                .iter()
                .filter(|k| a.aux.contains(**k))
                .map(|k| k.label())
                .collect::<Vec<_>>()
                .join("、");
            vec![
                a.code.clone(),
                a.name.clone(),
                a.category.label().to_string(),
                a.dir.label().to_string(),
                a.memo.clone(),
                dims,
            ]
        })
        .collect();
    Sheet::plain(
        "会计科目",
        &[
            "编码",
            "名称",
            "类别",
            "方向",
            "备注",
            "辅助核算维度（只读提示，导回时不生效）",
        ],
        &[T, T, T, T, T, T],
    )
    .with_rows(rows)
}

/// 期初余额：列头与 `/api/import/template?kind=begin` 及 Generic 解析器
/// 的「布局 B」一致（科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方）
///
/// `year_begin` 在库里是带符号的（借正贷负），导出的「方向 + 正数金额」是给人看的
/// 写法 —— 会计不写负号，写成「贷 3000」。
pub fn begin_sheet(rows: &[BeginRow], name_of: &dyn Fn(&str) -> String) -> Sheet {
    let rows = rows
        .iter()
        .map(|r| {
            let (dir, amt) = if r.year_begin < Money::ZERO {
                ("贷", -r.year_begin)
            } else {
                ("借", r.year_begin)
            };
            vec![
                r.account_code.clone(),
                name_of(&r.account_code),
                dir.to_string(),
                amt.fmt_plain(),
                r.debit_accum.fmt_plain(),
                r.credit_accum.fmt_plain(),
            ]
        })
        .collect();
    Sheet::plain(
        "期初余额",
        &["科目编码", "科目名称", "方向", "期初余额", "累计借方", "累计贷方"],
        &[T, T, T, M, M, M],
    )
    .with_rows(rows)
}

/// 科目余额表：逐科目一行，期初/发生/期末都带方向列
///
/// 金额列给的是**绝对值 + 方向**（和页面表格一致），不是带符号数 ——
/// 余额表上「借 3000」和「贷 3000」是两件事，用负号区分在 Excel 里会被
/// 当成「负的借方」，做透视表时和非负数混不到一列。
pub fn trial_balance_sheet(
    rows: &[BalanceRow],
    totals: &TrialBalance,
    from: &str,
    to: &str,
) -> Sheet {
    let mut out: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let (b_dir, b_amt) = r.begin_dir_amount();
            let (e_dir, e_amt) = r.end_dir_amount();
            vec![
                r.account_code.clone(),
                r.account_name.clone(),
                b_dir.label().to_string(),
                b_amt.fmt_plain(),
                r.debit.fmt_plain(),
                r.credit.fmt_plain(),
                e_dir.label().to_string(),
                e_amt.fmt_plain(),
                r.ytd_debit.fmt_plain(),
                r.ytd_credit.fmt_plain(),
            ]
        })
        .collect();
    // 合计行：只填借贷两列为金额，方向列留空 —— 合计没有「方向」
    let total_at = out.len();
    out.push(vec![
        String::new(),
        "合计".to_string(),
        String::new(),
        String::new(), // 期初净额无意义，期初借贷分列见下方两行
        totals.period_debit.fmt_plain(),
        totals.period_credit.fmt_plain(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    ]);
    Sheet::titled(
        "科目余额表",
        "科目余额表",
        &format!("{from} 至 {to}　　单位：元"),
        &[
            "科目编码",
            "科目名称",
            "期初方向",
            "期初余额",
            "本期借方",
            "本期贷方",
            "期末方向",
            "期末余额",
            "本年累计借方",
            "本年累计贷方",
        ],
        &[T, T, T, M, M, M, T, M, M, M],
    )
    .with_rows(out)
    .with_emphasis(vec![total_at])
}

/// 三大报表（资产负债表 / 利润表 / 现金流量表 / 所有者权益变动表）
///
/// 直接吃 `ReportTable`，所以报表定义改了导出自动跟着改，不用维护第二份列表。
pub fn report_sheet(table: &ReportTable, unit: &str) -> Sheet {
    // 第 1 列是「项目」（文本），其余各列都是金额 —— 列数要按「1 + 报表列数」
    // 算。少算一列会让最后一列退回 Text，金额在 Excel 里变成左对齐的文本，
    // 求和也求不了。
    let mut cols: Vec<ColFmt> = vec![T];
    cols.extend(table.columns.iter().map(|_| M));
    let rows = table
        .rows
        .iter()
        .map(|r| {
            // 缩进用两个全角空格表示，Excel 里不要靠单元格缩进设置 ——
            // 用户复制到别处时缩进会丢，层级关系就没了。
            let indent = "　".repeat(r.indent as usize);
            let mut row = vec![format!("{indent}{}", r.name)];
            for v in &r.values {
                // 空值（报表上没有这一列的数）留空，不写 0
                row.push(if v.is_zero() {
                    String::new()
                } else {
                    v.fmt_plain()
                });
            }
            row
        })
        .collect();
    // 表头第 1 列是「项目」，其余列名由报表定义给出
    let mut headers: Vec<String> = vec!["项目".to_string()];
    headers.extend(table.columns.iter().cloned());
    Sheet::titled(
        &sheet_name_of(&table.title),
        &table.title,
        &format!("{}　　单位：{unit}", table.subtitle),
        &headers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &cols,
    )
    .with_rows(rows)
}

/// 报表标题 → 工作表名。Excel 工作表名不能有 `: \ / ? * [ ]`，
/// 且不超过 31 字（`xlsxout::build` 还会再兜一次底，这里先按语义给短名）。
fn sheet_name_of(title: &str) -> String {
    let t = title.trim();
    if t.is_empty() {
        return "报表".to_string();
    }
    let cleaned: String = t
        .chars()
        .map(|c| if ":\\/?*[]".contains(c) { '_' } else { c })
        .collect();
    cleaned.chars().take(31).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::account::{AcctCategory, Direction};

    fn acct(code: &str, name: &str, cat: AcctCategory) -> Account {
        Account::new(code, name, cat)
    }

    /// 导出的列头必须与导入器读的下标一一对应，否则「导出去改完再导回」会错位。
    /// 这里把解析器读的列名抄一遍当断言，改列头会立刻失败。
    #[test]
    fn 基础资料导出的列头与导入模板一致() {
        let a = acct("1001", "库存现金", AcctCategory::Asset);
        let s = accounts_sheet(&[&a]);
        assert_eq!(&s.headers[..5], &["编码", "名称", "类别", "方向", "备注"]);

        let e = AuxEntity::new(fincore::AuxKind::Customer, "C001", "客户甲");
        let s = aux_sheet(&[e], "客户");
        assert_eq!(s.headers, vec!["类型", "编码", "名称", "备注"]);

        let e = AuxEntity::new(fincore::AuxKind::Item, "P001", "产品A");
        let s = items_sheet(&[e], &|_| None);
        assert_eq!(s.headers, vec!["编码", "名称", "保质期天", "安全库存"]);
    }

    /// 科目类别与方向的标签必须能被 `imports::acct_category_of` /
    /// `import_accounts_rows` 认回来，否则导回去全变成资产类借方。
    #[test]
    fn 科目类别与方向的标签能被导回识别() {
        let cats = [
            (AcctCategory::Asset, "资产"),
            (AcctCategory::Liability, "负债"),
            (AcctCategory::Common, "共同"),
            (AcctCategory::Equity, "权益"),
            (AcctCategory::Cost, "成本"),
            (AcctCategory::Income, "收入"),
            (AcctCategory::Expense, "费用"),
        ];
        let accts: Vec<Account> = cats
            .iter()
            .enumerate()
            .map(|(i, (c, _))| acct(&format!("{}001", i + 1), "x", *c))
            .collect();
        let refs: Vec<&Account> = accts.iter().collect();
        let s = accounts_sheet(&refs);
        // 方向按类别默认值：资产/共同/成本/费用走借方，负债/权益/收入走贷方。
        // （「共同」类科目两个方向都可能，`default_dir` 取借方。）
        // 导出的是 `Account::new` 定下的方向，导回时方向列会覆盖类别默认值，
        // 所以这里断言的正是「默认方向能不能原样搬过去」。
        let expect_dir = ["借", "贷", "借", "贷", "借", "贷", "借"];
        for (i, (row, (_, label))) in s.rows.iter().zip(cats.iter()).enumerate() {
            assert_eq!(&row[2], label, "类别标签必须是导入器认的中文");
            assert_eq!(&row[3], expect_dir[i], "{label}类的默认方向");
        }
    }

    /// 期初导出：负数必须写成「贷 + 正数」，不能写成「借 -3000」。
    #[test]
    fn 期初贷方导出成正数加方向() {
        let r = BeginRow {
            id: 1,
            account_code: "220201".into(),
            aux: Default::default(),
            year_begin: Money::parse("-3000").unwrap(),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        };
        let s = begin_sheet(&[r], &|c| c.to_string());
        assert_eq!(s.rows[0][2], "贷");
        assert_eq!(s.rows[0][3], "3000.00");
    }

    /// 报表导出：空金额留白不写 0（0 和「没有这一列的数」在报表上是两回事）。
    #[test]
    fn 报表空金额留白() {
        use fincore::report::{LineStyle, ReportRow};
        let table = ReportTable {
            title: "利润表".into(),
            subtitle: "2026-01 至 2026-01".into(),
            company: "测试".into(),
            columns: vec!["本期".into(), "本年累计".into()],
            rows: vec![ReportRow {
                no: "1".into(),
                name: "营业收入".into(),
                indent: 0,
                style: LineStyle::Normal,
                values: vec![Money::parse("1000").unwrap(), Money::ZERO],
                show_negative_red: false,
            }],
        };
        let s = report_sheet(&table, "元");
        assert_eq!(s.headers, vec!["项目", "本期", "本年累计"]);
        assert_eq!(s.rows[0][1], "1000.00");
        assert_eq!(s.rows[0][2], "", "零值要留空");
        assert_eq!(s.cols, vec![ColFmt::Text, ColFmt::Money, ColFmt::Money]);
    }

    #[test]
    fn 报表标题里的非法字符要被替换掉() {
        assert_eq!(sheet_name_of("资产负债表:合并"), "资产负债表_合并");
        assert_eq!(sheet_name_of(""), "报表");
        assert!(sheet_name_of(&"x".repeat(60)).chars().count() <= 31);
    }

    #[test]
    fn 停用科目不导出() {
        let mut a = acct("1001", "库存现金", AcctCategory::Asset);
        a.disabled = true;
        let s = accounts_sheet(&[&a]);
        assert!(s.rows.is_empty());
    }
}

// ---------------------------------------------------------------------------
// 现金流量表 / 所有者权益变动表
//
// 这两张不是 `ReportTable`（结构特殊：一类活动一个分组、权益表是三个金额列），
// 所以单独拼。分组标题行与「小计」行靠 `emphasis` 加粗区分层次。
// ---------------------------------------------------------------------------

/// 现金流量表：项目 | 流入 | 流出 | 净额
pub fn cash_flow_sheet(
    cf: &fincore::report::cashflow::CashFlowStatement,
    subtitle: &str,
) -> Sheet {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut emphasis: Vec<usize> = Vec::new();

    let push_section =
        |rows: &mut Vec<Vec<String>>,
         emphasis: &mut Vec<usize>,
         title: &str,
         lines: &[fincore::report::cashflow::CashFlowLine],
         net: &Money| {
            emphasis.push(rows.len());
            rows.push(vec![title.to_string(), String::new(), String::new(), String::new()]);
            for l in lines {
                rows.push(vec![
                    format!("　{}", l.name),
                    l.inflow.fmt_plain(),
                    l.outflow.fmt_plain(),
                    l.net.fmt_plain(),
                ]);
            }
            emphasis.push(rows.len());
            rows.push(vec![
                format!("{title}小计"),
                String::new(),
                String::new(),
                net.fmt_plain(),
            ]);
            // 分组之间空一行，打印出来能断开
            rows.push(Vec::new());
        };

    push_section(&mut rows, &mut emphasis, "一、经营活动产生的现金流量", &cf.operating, &cf.operating_net);
    push_section(&mut rows, &mut emphasis, "二、投资活动产生的现金流量", &cf.investing, &cf.investing_net);
    push_section(&mut rows, &mut emphasis, "三、筹资活动产生的现金流量", &cf.financing, &cf.financing_net);
    // 上面每个分组末尾塞了一个空行，最后那个多余的去掉
    if rows.last().map(|r| r.is_empty()).unwrap_or(false) {
        rows.pop();
    }

    emphasis.push(rows.len());
    rows.push(vec![
        "现金及现金等价物净增加额".to_string(),
        String::new(),
        String::new(),
        cf.net_increase.fmt_plain(),
    ]);
    rows.push(vec![
        "加：期初现金及现金等价物余额".to_string(),
        String::new(),
        String::new(),
        cf.begin_cash.fmt_plain(),
    ]);
    emphasis.push(rows.len());
    rows.push(vec![
        "期末现金及现金等价物余额".to_string(),
        String::new(),
        String::new(),
        cf.end_cash.fmt_plain(),
    ]);

    // 勾稽情况写在表里而不是只在页面上提示：导出的表是拿来发的，
    // 收表的人看不到页面上的那行绿字。
    rows.push(vec![
        if cf.ties() {
            "勾稽：净增加额 = 期末 − 期初货币资金，一致".to_string()
        } else {
            "勾稽不符：净增加额 ≠ 期末 − 期初货币资金，请检查".to_string()
        },
        String::new(),
        String::new(),
        String::new(),
    ]);
    if !cf.unassigned.is_zero() {
        rows.push(vec![
            format!(
                "提示：{} 元现金收支未标注现金流量项目，未计入上面任何一类",
                cf.unassigned.fmt_plain()
            ),
            String::new(),
            String::new(),
            String::new(),
        ]);
    }

    Sheet::titled(
        "现金流量表",
        "现金流量表",
        subtitle,
        &["项目", "流入", "流出", "净额"],
        &[T, M, M, M],
    )
    .with_rows(rows)
    .with_emphasis(emphasis)
}

/// 所有者权益变动表：项目 | 年初余额 | 本年增减 | 年末余额
pub fn equity_sheet(e: &fincore::report::equity::EquityStatement, subtitle: &str) -> Sheet {
    let mut rows: Vec<Vec<String>> = e
        .lines
        .iter()
        .map(|l| {
            vec![
                l.name.clone(),
                l.begin.fmt_plain(),
                l.change.fmt_plain(),
                l.end.fmt_plain(),
            ]
        })
        .collect();
    let total_at = rows.len();
    let t = &e.total;
    rows.push(vec![
        "所有者权益合计".to_string(),
        t.begin.fmt_plain(),
        t.change.fmt_plain(),
        t.end.fmt_plain(),
    ]);
    Sheet::titled(
        "所有者权益变动表",
        "所有者权益变动表",
        subtitle,
        &["项目", "年初余额", "本年增减", "年末余额"],
        &[T, M, M, M],
    )
    .with_rows(rows)
    .with_emphasis(vec![total_at])
}
