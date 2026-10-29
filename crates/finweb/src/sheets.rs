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

/// 辅助核算档案：列头与 `import_aux_rows` 一致。
///
/// 除了六个公共列，**每种档案还带上它自己在页面上能编辑的扩展属性**：
/// 客户/供应商有「税点%」「开票情况」「信用额度」，银行账户有开户行和账号，
/// 存货有规格型号和计量单位。这些列原来一个都不导 —— 于是「导出去批量改」这条
/// 路走不通：59 个客户的税点只能一个个点开改，存了半年才发现改不过来。
///
/// 为什么按档案种类给不同列、而不是给一张大宽表：大宽表里 90% 的格子是空的，
/// 老板填表时要先判断「这列跟我有没有关系」。列名就是接口，多余的列也是噪音。
/// 解析器现在先认表头（见 `imports::HeadMap`），所以列集不同、顺序不同都能导回。
/// 《期初收集表》的空模板 —— **已预填当前账套的科目、客户、供应商清单**。
///
/// 为什么值得做：小工厂的期初要填 200 多个科目、86 个往来单位。以前是发一张白表，
/// 工厂照着科目表手抄编码，抄错一个编码就等于「这笔期初消失了」——`opening_book`
/// 对不在科目表里的编码只会往 warnings 里塞一句就跳过，不报错。
///
/// 所以模板直接从账套里取：编码与名称都是现成的，工厂只填金额。
///
/// 四张纸的列名与 `findb::opening_book` 的解析器一一对应（按列名找，不靠位置）：
///   ① 客户应收：客户编码 / 客户名称 / 欠款金额 / 单据号 / 备注
///   ② 供应商应付：供应商编码 / 供应商名称 / 欠款金额 / 单据号 / 备注
///   ③ 账户余额：科目编码 / 科目名称 / 余额
///   ④ 基本资料：项目 / 金额（只认「实收资本」与「本年利润」）
pub fn opening_collection_template(
    accounts: &[&Account],
    customers: &[AuxEntity],
    suppliers: &[AuxEntity],
) -> Vec<Sheet> {
    let mut out = Vec::new();

    // 0) 怎么填 —— 放在第一张，工厂打开就看到规则
    out.push(
        Sheet::titled(
            "怎么填",
            "期初收集表",
            "只填你确实有的科目/往来；空着的行不用删，也不影响导入",
            &["项目", "说明"],
            &[ColFmt::Text, ColFmt::Text],
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
        Sheet::titled(
            name,
            title,
            // 副标题里刻意不出现「编码」「科目」「项目」—— opener 靠这几个词找表头
            "金额填正数（欠多少填多少）；单据号留空则自动生成",
            &[code_head, name_head, "欠款金额", "单据号", "备注"],
            &[
                ColFmt::Text,
                ColFmt::Text,
                ColFmt::Money,
                ColFmt::Text,
                ColFmt::Text,
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
        Sheet::titled(
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
                ColFmt::Text,
                ColFmt::Text,
                ColFmt::Money,
                ColFmt::Money,
                ColFmt::Text,
                ColFmt::Text,
                ColFmt::Text,
            ],
        )
        .with_rows(bal_rows),
    );

    // ④ 基本资料
    out.push(
        Sheet::titled(
            "④ 基本资料",
            "基本资料",
            "实收资本照营业执照填写；本年利润填不出来就留空，导入时按差额轧",
            &["项目", "金额"],
            &[ColFmt::Text, ColFmt::Money],
        )
        .with_rows(vec![
            vec!["实收资本".to_string(), String::new()],
            vec!["其中：本年利润（没有就留空）".to_string(), String::new()],
        ]),
    );

    out
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
