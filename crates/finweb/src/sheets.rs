//! 把账套数据拼成「导出用」的工作表。
//!
//! # 为什么列头要和 `findb::imports` 写在一起看
//!
//! 基础资料导出的列头必须与三处逐字一致：
//! 1. `crates/findb/src/imports.rs` 的解析器；
//! 2. `/api/import/template` 给出的模板（用户照它手填）；
//! 3. 本文件的导出（用户导出去改了再导回来）。
//!
//! 解析器现在**先认列名再退回列下标**（`imports::HeadMap`）。只按下标取值的
//! 时代，导出端多加一列、或者用户在 Excel 里删列换序，就会整体错位 —— 而且不
//! 报错：备注被写进上级编码、税点被当成信用额度，等对账才发现。
//!
//! 约定（改列时请守住）：**老列的位置一位不动，新列一律追加在末尾**。这样
//! 即便有人把表头行删了，退回按位置解析也还是对的。
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

/// 期初收集表（四张纸）——数据在 `findb::opening_book::collection_template`，
/// 这里只把它摊成网页端的 `Sheet`。
///
/// 为什么数据搬到了 `findb`：这张表同时是导出**和**导入的契约，桌面版和网页版
/// 必须吐出完全一样的列头 —— 用户在桌面下载、在网页导入不能错位。放在两端各写
/// 一遍，第一列一改就有一边悄悄错位，而 xlsx 不会报错。
pub fn opening_collection_template(
    accounts: &[&Account],
    customers: &[AuxEntity],
    suppliers: &[AuxEntity],
) -> Vec<Sheet> {
    let owned: Vec<Account> = accounts.iter().map(|a| (*a).clone()).collect();
    findb::opening_book::collection_template(&owned, customers, suppliers)
        .into_iter()
        .map(|c| {
            let cols: Vec<ColFmt> = c
                .cols
                .iter()
                .map(|k| match k {
                    findb::opening_book::Col::Money => ColFmt::Money,
                    findb::opening_book::Col::Text => ColFmt::Text,
                })
                .collect();
            let headers: Vec<&str> = c.headers.iter().map(|h| h.as_str()).collect();
            Sheet::titled(&c.name, &c.title, &c.subtitle, &headers, &cols).with_rows(c.rows)
        })
        .collect()
}
pub fn aux_sheet(rows: &[AuxEntity], kind_label: &str) -> Sheet {
    let kind = rows.first().map(|e| e.kind);
    // (列头, 取值) —— 顺序即导出顺序；缺失的列在导回时不会被误认成别的字段。
    let mut extra: Vec<(&str, fn(&AuxEntity) -> String)> = Vec::new();
    match kind {
        Some(fincore::AuxKind::Customer) | Some(fincore::AuxKind::Supplier) => {
            extra.push((
                "税点%",
                |e: &AuxEntity| e.prop(fincore::auxiliary::prop::TAX_RATE).cloned().unwrap_or_default(),
            ));
            extra.push((
                "开票情况",
                |e: &AuxEntity| {
                    fincore::auxiliary::tax_policy_label(
                        e.prop(fincore::auxiliary::prop::TAX_RATE_POLICY)
                            .map(|s| s.as_str())
                            .unwrap_or(""),
                    )
                    .to_string()
                },
            ));
            extra.push((
                "信用额度",
                |e: &AuxEntity| {
                    e.prop(fincore::auxiliary::prop::CREDIT_LIMIT).cloned().unwrap_or_default()
                },
            ));
        }
        Some(fincore::AuxKind::Bank) => {
            extra.push((
                "开户行",
                |e: &AuxEntity| e.prop(fincore::auxiliary::prop::BANK_NAME).cloned().unwrap_or_default(),
            ));
            extra.push((
                "银行账号",
                |e: &AuxEntity| {
                    e.prop(fincore::auxiliary::prop::BANK_ACCOUNT).cloned().unwrap_or_default()
                },
            ));
        }
        Some(fincore::AuxKind::Item) => {
            extra.push((
                "规格型号",
                |e: &AuxEntity| e.prop(fincore::auxiliary::prop::SPEC).cloned().unwrap_or_default(),
            ));
            extra.push((
                "计量单位",
                |e: &AuxEntity| e.prop(fincore::auxiliary::prop::UNIT).cloned().unwrap_or_default(),
            ));
        }
        _ => {}
    }

    let body = rows
        .iter()
        .map(|e| {
            let mut r = vec![
                e.kind.label().to_string(),
                e.code.clone(),
                e.name.clone(),
                e.memo.clone(),
                e.parent_code.clone().unwrap_or_default(),
                if e.disabled { "停用" } else { "启用" }.to_string(),
            ];
            for (_, f) in &extra {
                r.push(f(e));
            }
            r
        })
        .collect();

    let mut headers: Vec<&str> = vec!["类型", "编码", "名称", "备注", "上级编码", "状态"];
    headers.extend(extra.iter().map(|(h, _)| *h));
    let mut cols = vec![T; headers.len()];
    // 「税点%」「信用额度」是数字列，单元格要能被 Excel 排序/求和
    for (i, h) in headers.iter().enumerate() {
        if *h == "税点%" || *h == "信用额度" {
            cols[i] = M;
        }
    }
    Sheet::plain(&format!("{kind_label}档案"), &headers, &cols).with_rows(body)
}

/// 存货档案：列头与 `import_items_rows` 一致。
///
/// `safety` 是 `item_plan` 里的安全库存（低库存预警口径），与档案分存两张表，
/// 导出时合起来；导回时 `import_items_rows` 会拆回去。
///
/// 后三列（备注/计量单位/状态）是 v1.8.22 补的：网页存货页面一直在显示这三列，
/// 导出的表里却没有 —— 用户在表里改完导回，单位没了、停用的又恢复启用。
///
/// 老的四列位置**一位没动**（编码, 名称, 保质期天, 安全库存），新列全部追加在
/// 后面。这样即便有人把表头行删了，退回按位置解析也不会错位。
pub fn items_sheet(rows: &[AuxEntity], safety: &dyn Fn(&str) -> Option<Money>) -> Sheet {
    let body = rows
        .iter()
        .map(|e| {
            vec![
                e.code.clone(),
                e.name.clone(),
                e.props.get("shelf_life_days").cloned().unwrap_or_default(),
                safety(&e.code).map(|m| m.fmt_plain()).unwrap_or_default(),
                e.memo.clone(),
                e.prop(fincore::auxiliary::prop::UNIT).cloned().unwrap_or_default(),
                if e.disabled { "停用" } else { "启用" }.to_string(),
            ]
        })
        .collect();
    Sheet::plain(
        "存货档案",
        &["编码", "名称", "保质期天", "安全库存", "备注", "计量单位", "状态"],
        &[T, T, I, M, T, T, T],
    )
    .with_rows(body)
}

/// 科目建档：列头与 `import_accounts_rows` 一致。
///
/// v1.8.22 起第 6 列往后是**真能导回的**，不再是只读提示。原来那一列的标题写着
/// 「只读提示，导回时不生效」—— 一句自相矛盾的话：页面上科目明明设了辅助核算，
/// 导出的表却告诉用户别改。搬账套时辅助核算维度、数量单位、外币、现金/银行标志
/// 全丢，余额表按辅助拆不开，只能一个个重设。
///
/// **停用科目也导出**。早先的版本把它们过滤掉了（注释说「停用科目不导出」），
/// 理由是「搬过去也没用」。可是账套搬家要看的是「像不像原来那本账」：一个科目
/// 在旧账里停用，搬到新账里变启用，做账时就会多出一堆不该能选的科目，而人根本
/// 想不起来是哪个。状态如实带过去，用不用是用户的事。
pub fn accounts_sheet(accounts: &[&Account]) -> Sheet {
    let body = accounts
        .iter()
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
                a.unit.clone().unwrap_or_default(),
                a.currency.clone().unwrap_or_default(),
                if a.is_cash { "是" } else { "" }.to_string(),
                if a.is_bank { "是" } else { "" }.to_string(),
                if a.disabled { "停用" } else { "启用" }.to_string(),
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
            "辅助核算维度",
            "数量单位",
            "币种",
            "现金科目",
            "银行科目",
            "状态",
        ],
        &[T, T, T, T, T, T, T, T, T, T, T],
    )
    .with_rows(body)
}

/// 期初余额：列头与 `/api/import/template?kind=begin` 及 Generic 解析器
/// 的「布局 B」一致（科目编码, 科目名称, 方向, 期初余额, 累计借方, 累计贷方）。
///
/// `year_begin` 在库里是带符号的（借正贷负），导出的「方向 + 正数金额」是给人看的
/// 写法 —— 会计不写负号，写成「贷 3000」。
///
/// v1.8.22 追加了三类列，都在末尾，绝不动前六列的位置：
/// - `数量`：数量核算科目（原材料/库存商品）的期初数量；
/// - 七个辅助维度编码：分客户/分供应商/分存货的期初余额。小厂经常「华达还欠我
///   5 万」这种期初，金额记在 112201 上、分户靠辅助核算 —— 原来这一步搬不过去，
///   期初金额对了但按客户查是空的。
pub fn begin_sheet(rows: &[BeginRow], name_of: &dyn Fn(&str) -> String) -> Sheet {
    let body = rows
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
                r.qty_begin.map(|q| q.fmt_plain()).unwrap_or_default(),
                r.aux.customer.clone().unwrap_or_default(),
                r.aux.supplier.clone().unwrap_or_default(),
                r.aux.item.clone().unwrap_or_default(),
                r.aux.project.clone().unwrap_or_default(),
                r.aux.dept.clone().unwrap_or_default(),
                r.aux.employee.clone().unwrap_or_default(),
                r.aux.bank.clone().unwrap_or_default(),
            ]
        })
        .collect();
    Sheet::plain(
        "期初余额",
        &[
            "科目编码",
            "科目名称",
            "方向",
            "期初余额",
            "累计借方",
            "累计贷方",
            "数量",
            "客户编码",
            "供应商编码",
            "存货编码",
            "项目编码",
            "部门编码",
            "职员编码",
            "银行账户",
        ],
        &[T, T, T, M, M, M, M, T, T, T, T, T, T, T],
    )
    .with_rows(body)
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


    /// 对比表：上期有、本期没有的项目不能消失
    ///
    /// 「上个月买了设备、这个月没买」正是同比最该看见的事。按本期循环一遍就收工，
    /// 投资活动这一整块会凭空消失，而两张表看起来都挺对。
    #[test]
    fn cash_flow_compare_keeps_last_period_only_items() {
        use fincore::report::cashflow::{CashFlowLine, CashFlowStatement};
        let line = |code: &str, name: &str, net: &str| CashFlowLine {
            code: code.to_string(),
            name: name.to_string(),
            inflow: Money::parse(net).unwrap(),
            outflow: Money::ZERO,
            net: Money::parse(net).unwrap(),
        };
        let mk = |ops: Vec<CashFlowLine>, inv: Vec<CashFlowLine>| CashFlowStatement {
            operating: ops,
            operating_net: Money::parse("100").unwrap(),
            investing: inv,
            investing_net: Money::ZERO,
            financing: vec![],
            financing_net: Money::ZERO,
            net_increase: Money::parse("100").unwrap(),
            begin_cash: Money::ZERO,
            end_cash: Money::parse("100").unwrap(),
            unassigned: Money::ZERO,
            auto_assigned: 0,
            ambiguous: 0,
            transfers: 0,
        };
        let cur = mk(vec![line("0101", "销售商品收到的现金", "100")], vec![]);
        let prev = mk(
            vec![line("0101", "销售商品收到的现金", "80")],
            vec![line("0202", "购建固定资产支付的现金", "500")],
        );
        let sh = cash_flow_compare_sheet(&cur, &prev, "本期", "上期");
        let flat: String = sh.rows.iter().flatten().cloned().collect::<Vec<String>>().join("|");
        assert!(flat.contains("购建固定资产"), "上期独有的投资活动被吞了：{flat}");
        assert!(flat.contains("（上期有）"), "要标出来这是上期才有的事：{flat}");
        assert!(flat.contains("销售商品收到的现金"), "两期都有的项目");
    }

    /// 期初模板必须把低频科目也列出来
    ///
    /// 「我以为模板只列了常用的几个」是错的：模板按 1xxx/2xxx 末级科目**全量**生成。
    /// 这条测试把这件事钉住 —— 否则哪天有人为了「让表短一点」加个白名单，
    /// 在建工程、预付账款、预收账款这些不常动但一动就是大额的科目会静默消失，
    /// 而用户看不见「少了一行」，只会发现期末不平。
    #[test]
    fn opening_template_lists_low_frequency_accounts_too() {
        use fincore::account::Chart;
        let chart = Chart::with_accounts(
            fincore::CodeScheme(vec![4, 2, 2, 2, 2]),
            fincore::chart::default_accounts(),
        );
        let all: Vec<&fincore::account::Account> = chart.all().into_iter().collect();
        let sheets = opening_collection_template(&all, &[], &[]);
        let flat: String = sheets
            .iter()
            .flat_map(|s| s.rows.iter().flatten().cloned())
            .collect::<Vec<String>>()
            .join("\u{1f}");
        for (code, name) in [
            ("1123", "预付账款"),
            ("1604", "在建工程"),
            ("2203", "预收账款"),
            ("2501", "长期借款"),
        ] {
            assert!(
                flat.contains(code) && flat.contains(name),
                "期初模板里找不到 {code} {name} —— 低频科目也是资产/负债，漏了期末就不平"
            );
        }
        // 由 ①② 两张纸自动生成的这两个，反而**不能**出现在③账户余额表里
        // （「怎么填」那张说明纸上会提到它们，那是解释文字，不是可填行）
        let bal = sheets
            .iter()
            .find(|s| s.name == "③ 账户余额")
            .expect("应有账户余额这张纸");
        let bal_flat: String = bal.rows.iter().flatten().cloned().collect::<Vec<String>>().join("\u{1f}");
        assert!(
            !bal_flat.contains("应收账款"),
            "1122 的余额由客户应收那张纸生成，再让人填一遍会双计"
        );
        assert!(
            !bal_flat.contains("应付账款"),
            "2202 同理，由供应商应付那张纸生成"
        );
    }

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
        // 前五列是 v1.8.21 就有的老位置，**一位都不能动**：老文件退回按位置解析
        // 时靠的就是它们。后面的列都是 v1.8.22 追加的。
        assert_eq!(&s.headers[..5], &["编码", "名称", "类别", "方向", "备注"]);
        assert_eq!(
            &s.headers[5..],
            &[
                "辅助核算维度",
                "数量单位",
                "币种",
                "现金科目",
                "银行科目",
                "状态"
            ],
            "这几列原来只写「只读提示，导回时不生效」——列名就是接口，说了不生效等于断闭环"
        );

        let mut e = AuxEntity::new(fincore::AuxKind::Customer, "C001", "客户甲");
        e.parent_code = Some("C00".into());
        e.disabled = true;
        e.set_prop(
            fincore::auxiliary::prop::TAX_RATE,
            String::from("13"),
        );
        e.set_prop(
            fincore::auxiliary::prop::TAX_RATE_POLICY,
            String::from("sometimes"),
        );
        let s = aux_sheet(&[e], "客户");
        assert_eq!(
            s.headers,
            vec![
                "类型", "编码", "名称", "备注", "上级编码", "状态", "税点%", "开票情况",
                "信用额度"
            ]
        );
        assert_eq!(s.rows[0][4], "C00");
        assert_eq!(s.rows[0][5], "停用", "停用状态必须写出来");
        assert_eq!(s.rows[0][6], "13");
        assert_eq!(
            s.rows[0][7], "有时开有时不开",
            "库里存 sometimes，表里要写字 —— 老板看的是表，不是库"
        );

        let e = AuxEntity::new(fincore::AuxKind::Item, "P001", "产品A");
        let s = items_sheet(&[e], &|_| None);
        assert_eq!(
            s.headers,
            vec!["编码", "名称", "保质期天", "安全库存", "备注", "计量单位", "状态"]
        );
    }

    /// 档案的扩展列按种类给：客户身上不该出现「规格型号」，存货身上不该出现
    /// 「税点%」。宽表里 90% 的格子空着，填表的人要先判断「这列跟我有关系吗」。
    #[test]
    fn 档案扩展列按种类不同() {
        let c = AuxEntity::new(fincore::AuxKind::Customer, "C1", "客户");
        let s = aux_sheet(&[c], "客户");
        assert!(s.headers.iter().any(|h| h == "税点%"));
        assert!(!s.headers.iter().any(|h| h == "规格型号"));

        let i = AuxEntity::new(fincore::AuxKind::Item, "I1", "存货");
        let s = aux_sheet(&[i], "存货");
        assert!(s.headers.iter().any(|h| h == "规格型号"));
        assert!(!s.headers.iter().any(|h| h == "税点%"));

        let b = AuxEntity::new(fincore::AuxKind::Bank, "B1", "基本户");
        let s = aux_sheet(&[b], "银行");
        assert!(s.headers.iter().any(|h| h == "开户行"));
        assert!(!s.headers.iter().any(|h| h == "税点%"));

        let d = AuxEntity::new(fincore::AuxKind::Dept, "D1", "车间");
        let s = aux_sheet(&[d], "部门");
        assert_eq!(s.headers.len(), 6, "部门没有扩展列");
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

    /// 停用科目**要**导出（v1.8.22 改的口径）。
    ///
    /// 早先过滤掉它们，理由是「搬过去也没用」。可账套搬家要看的是「像不像原来
    /// 那本账」：旧账里停用的科目搬到新账里变启用，做账时就多出一堆不该能选的
    /// 科目，而人想不起来是哪个。
    #[test]
    fn 停用科目也导出并带上状态() {
        let mut a = acct("1001", "库存现金", AcctCategory::Asset);
        a.disabled = true;
        let s = accounts_sheet(&[&a]);
        assert_eq!(s.rows.len(), 1);
        assert_eq!(s.rows[0][10], "停用");
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
    // 归集情况一律用 fincore 的那一句话，三端同源
    rows.push(vec![
        format!("提示：{}", cf.assignment_note()),
        String::new(),
        String::new(),
        String::new(),
    ]);

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

/// 现金流量表的「本期 vs 上期」对比表
///
/// 单独一张表而不是给主表加两列：主表已经有流入/流出/净额三列，再加两列在 A4 横向
/// 也挤；而对比只需要看净额，一行一个项目最清楚。
pub fn cash_flow_compare_sheet(
    cur: &fincore::report::cashflow::CashFlowStatement,
    prev: &fincore::report::cashflow::CashFlowStatement,
    cur_label: &str,
    prev_label: &str,
) -> Sheet {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut emphasis: Vec<usize> = Vec::new();

    let mut push_block = |rows: &mut Vec<Vec<String>>,
                          emphasis: &mut Vec<usize>,
                          title: &str,
                          a: &[fincore::report::cashflow::CashFlowLine],
                          b: &[fincore::report::cashflow::CashFlowLine]| {
        rows.push(vec![title.to_string(), String::new(), String::new(), String::new()]);
        emphasis.push(rows.len() - 1);
        // 以本期出现的项目为主序，上期有而本期没有的补在后面 —— 不能只按本期循环，
        // 否则「上期买过设备、这期没买」这件事在对比表里会凭空消失
        let mut seen: Vec<&str> = Vec::new();
        for l in a.iter() {
            seen.push(l.code.as_str());
            let other = b.iter().find(|x| x.code == l.code).map(|x| x.net);
            let delta = match other {
                Some(o) => l.net - o,
                None => l.net,
            };
            rows.push(vec![
                format!("　{}", l.name),
                l.net.fmt_plain(),
                other.map(|o| o.fmt_plain()).unwrap_or_default(),
                delta.fmt_plain(),
            ]);
        }
        for l in b.iter() {
            if seen.contains(&l.code.as_str()) {
                continue;
            }
            rows.push(vec![
                format!("　{}（上期有）", l.name),
                String::new(),
                l.net.fmt_plain(),
                (Money::ZERO - l.net).fmt_plain(),
            ]);
        }
    };

    let mut totals = |rows: &mut Vec<Vec<String>>,
                      emphasis: &mut Vec<usize>,
                      label: &str,
                      a: &Money,
                      b: &Money| {
        rows.push(vec![
            label.to_string(),
            a.fmt_plain(),
            b.fmt_plain(),
            (*a - *b).fmt_plain(),
        ]);
        emphasis.push(rows.len() - 1);
    };

    push_block(
        &mut rows,
        &mut emphasis,
        "一、经营活动",
        &cur.operating,
        &prev.operating,
    );
    totals(
        &mut rows,
        &mut emphasis,
        "　经营活动净额",
        &cur.operating_net,
        &prev.operating_net,
    );
    push_block(
        &mut rows,
        &mut emphasis,
        "二、投资活动",
        &cur.investing,
        &prev.investing,
    );
    totals(
        &mut rows,
        &mut emphasis,
        "　投资活动净额",
        &cur.investing_net,
        &prev.investing_net,
    );
    push_block(
        &mut rows,
        &mut emphasis,
        "三、筹资活动",
        &cur.financing,
        &prev.financing,
    );
    totals(
        &mut rows,
        &mut emphasis,
        "　筹资活动净额",
        &cur.financing_net,
        &prev.financing_net,
    );
    totals(
        &mut rows,
        &mut emphasis,
        "现金及现金等价物净增加额",
        &cur.net_increase,
        &prev.net_increase,
    );

    Sheet::titled(
        "本期与上期对比",
        "现金流量表 · 本期与上期对比",
        &format!("{cur_label} 对比 {prev_label}"),
        &["项目", cur_label, prev_label, "变动（本期−上期）"],
        &[ColFmt::Text, ColFmt::Money, ColFmt::Money, ColFmt::Money],
    )
    .with_rows(rows)
    .with_emphasis(emphasis)
}
