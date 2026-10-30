//! 生成《期初收集表.xlsx》—— 工厂把 10/31 的数填进来，再录进系统。
//!
//! 用法: cargo run -p findb --example opening_sheet --release -- <输出.xlsx> <账套.fbk>
//!
//! 表里直接列出账套中已建好的客户/供应商编码，工厂照着填金额即可，
//! 避免"名字对不上"这种最费时间的返工。

use fincore::AuxKind;
use findb::Db;
use rust_xlsxwriter::{Format, Workbook, XlsxAlign, XlsxBorder, XlsxColor};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args
        .first()
        .cloned()
        .unwrap_or_else(|| "期初收集表.xlsx".into());
    let fbk = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "小工厂账套.fbk".into());

    let db = Db::open(&fbk).expect("打开账套");
    let mut cust = findb::auxs::list(&db, &fincore::AuxQuery::kind(AuxKind::Customer))
        .expect("客户")
        .into_iter()
        .map(|e| (e.code, e.name))
        .collect::<Vec<_>>();
    let mut supp = findb::auxs::list(&db, &fincore::AuxQuery::kind(AuxKind::Supplier))
        .expect("供应商")
        .into_iter()
        .map(|e| {
            (
                e.code,
                e.name,
                e.props.get("品类").cloned().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    cust.sort();
    supp.sort();

    let mut wb = Workbook::new();

    let title = Format::new()
        .set_bold()
        .set_font_size(14)
        .set_font_color(XlsxColor::White)
        .set_background_color(XlsxColor::RGB(0x1F4E79))
        .set_align(XlsxAlign::Center)
        .set_align(XlsxAlign::VerticalCenter);

    let head = Format::new()
        .set_bold()
        .set_font_color(XlsxColor::White)
        .set_background_color(XlsxColor::RGB(0x4472C4))
        .set_align(XlsxAlign::Center)
        .set_border(XlsxBorder::Thin);

    let text = Format::new().set_border(XlsxBorder::Thin);
    let money = Format::new()
        .set_border(XlsxBorder::Thin)
        .set_num_format("#,##0.00")
        .set_align(XlsxAlign::Right);
    let note = Format::new()
        .set_font_color(XlsxColor::RGB(0x808080))
        .set_italic();
    let sect = Format::new()
        .set_bold()
        .set_font_size(11)
        .set_font_color(XlsxColor::RGB(0x1F4E79));

    // ---------- Sheet 1: 怎么填 ----------
    let s = wb.add_worksheet();
    s.set_name("怎么填").expect("sheet name");
    s.set_column_width(0, 4).ok();
    s.set_column_width(1, 20).ok();
    s.set_column_width(2, 60).ok();

    s.merge_range(
        0,
        0,
        0,
        2,
        "小工厂 · 期初数据收集表（截止 2026-10-31）",
        &title,
    )
    .expect("title");
    s.set_row_height(0, 28).ok();

    let lines: &[(&str, &str)] = &[
        ("", ""),
        ("一、要填哪三块", ""),
        ("", "① 客户欠你的钱（应收）：每个客户一行"),
        ("", "② 你欠供应商的钱（应付）：每个供应商一行"),
        (
            "",
            "③ 各账户的余额：G 行、N 行、微信、现金、电表里还躺着的钱",
        ),
        ("", ""),
        ("二、填完怎么检查", ""),
        ("", "① 应收合计 + 账户余额合计 + 存货 + 设备 = 资产合计"),
        ("", "② 应付合计 + 欠薪 + 欠税 = 负债合计"),
        ("", "③ 资产 − 负债 = 所有者权益（老板投的本钱 + 之前赚的）"),
        (
            "",
            "差额填在「基本资料」表的实收资本/本年利润里，两边必须相等",
        ),
        ("", ""),
        ("三、几个注意", ""),
        ("", "· 金额填正数，不要带负号，也不要带「元」「¥」"),
        (
            "",
            "· 客户/供应商编码已经建好，照着下拉选名字即可，不要改编码",
        ),
        ("", "· 没有欠款的客户留空，不用填 0"),
        (
            "",
            "· 电费是「先充后用」：账户余额里要算上电表里还躺着的钱，",
        ),
        (
            "",
            "  这笔钱在账上记在 122101 备用金（电费）下，不是电费费用",
        ),
        ("", "· 抹零的钱不用单独填，按实收金额填就行"),
        ("", ""),
        ("四、填完之后", ""),
        (
            "",
            "把这张表发回来，我帮你转成系统的期初数据；或者你自己照着录进",
        ),
        ("", "系统的「期初余额」和「往来核销 → 期初明细」里。"),
    ];
    let mut r: u32 = 2;
    for (k, v) in lines {
        if !k.is_empty() {
            s.write_string(r, 1, *k, &sect).ok();
        }
        if !v.is_empty() {
            s.write_string(r, 1, *v, &text).ok();
            s.merge_range(r, 1, r, 2, *v, &text).ok();
        }
        r += 1;
    }

    // ---------- Sheet 2: 客户应收 ----------
    let s = wb.add_worksheet();
    s.set_name("① 客户应收").expect("sheet");
    s.set_column_width(0, 10).ok();
    s.set_column_width(1, 22).ok();
    s.set_column_width(2, 16).ok();
    s.set_column_width(3, 14).ok();
    s.set_column_width(4, 30).ok();

    s.merge_range(0, 0, 0, 4, "客户欠款（应收）· 截止 2026-10-31", &title)
        .expect("t");
    s.set_row_height(0, 26).ok();
    s.write_string(1, 0, "客户编码", &head).ok();
    s.write_string(1, 1, "客户名称", &head).ok();
    s.write_string(1, 2, "欠款金额", &head).ok();
    s.write_string(1, 3, "单据号（可空）", &head).ok();
    s.write_string(1, 4, "备注", &head).ok();
    for (i, (code, name)) in cust.iter().enumerate() {
        let row = 2 + i as u32;
        s.write_string(row, 0, code, &text).ok();
        s.write_string(row, 1, name, &text).ok();
        s.write_number(row, 2, 0.0, &money).ok();
        s.write_string(row, 3, "", &text).ok();
        s.write_string(row, 4, "", &text).ok();
    }
    let last = 2 + cust.len() as u32;
    s.write_string(last + 1, 1, "应收合计", &sect).ok();
    s.write_formula(
        last + 1,
        2,
        &format!("=SUM(C3:C{})", last),
        &Format::new().set_bold().set_num_format("#,##0.00"),
    )
    .ok();
    s.write_string(last + 3, 1, "只填有欠款的，没欠款留空即可", &note)
        .ok();

    // ---------- Sheet 3: 供应商应付 ----------
    let s = wb.add_worksheet();
    s.set_name("② 供应商应付").expect("sheet");
    s.set_column_width(0, 10).ok();
    s.set_column_width(1, 24).ok();
    s.set_column_width(2, 12).ok();
    s.set_column_width(3, 16).ok();
    s.set_column_width(4, 14).ok();
    s.set_column_width(5, 30).ok();

    s.merge_range(0, 0, 0, 5, "欠供应商的钱（应付）· 截止 2026-10-31", &title)
        .expect("t");
    s.set_row_height(0, 26).ok();
    for (c, h) in [
        "供应商编码",
        "供应商名称",
        "品类",
        "欠款金额",
        "单据号（可空）",
        "备注",
    ]
    .iter()
    .enumerate()
    {
        s.write_string(1, c as u16, *h, &head).ok();
    }
    for (i, (code, name, cat)) in supp.iter().enumerate() {
        let row = 2 + i as u32;
        s.write_string(row, 0, code, &text).ok();
        s.write_string(row, 1, name, &text).ok();
        s.write_string(row, 2, cat, &text).ok();
        s.write_number(row, 3, 0.0, &money).ok();
        s.write_string(row, 4, "", &text).ok();
        s.write_string(row, 5, "", &text).ok();
    }
    let last = 2 + supp.len() as u32;
    s.write_string(last + 1, 2, "应付合计", &sect).ok();
    s.write_formula(
        last + 1,
        3,
        &format!("=SUM(D3:D{})", last),
        &Format::new().set_bold().set_num_format("#,##0.00"),
    )
    .ok();

    // ---------- Sheet 4: 账户余额 ----------
    let s = wb.add_worksheet();
    s.set_name("③ 账户余额").expect("sheet");
    s.set_column_width(0, 14).ok();
    s.set_column_width(1, 30).ok();
    s.set_column_width(2, 16).ok();
    s.set_column_width(3, 44).ok();

    s.merge_range(0, 0, 0, 3, "各账户余额 · 截止 2026-10-31", &title)
        .expect("t");
    s.set_row_height(0, 26).ok();
    for (c, h) in ["科目编码", "账户", "余额", "说明"].iter().enumerate() {
        s.write_string(1, c as u16, *h, &head).ok();
    }
    let accts: &[(&str, &str, &str)] = &[
        ("100201", "G银行对公户（要票）", "银行对账单上的余额"),
        ("100202", "N银行结算户（当现金用）", "银行对账单上的余额"),
        ("100203", "微信钱包", "微信零钱里的余额"),
        ("1001", "库存现金", "手上现钞"),
        (
            "122101",
            "备用金 — 电费充值余额",
            "电表里还躺着多少钱（先充后用）",
        ),
        ("1123", "预付账款", "预付给供应商、还没收到货的钱（可空）"),
        (
            "140302",
            "辅助材料（存货）",
            "仓库里化工料、辅料的成本价金额（可空）",
        ),
        ("140305", "包装材料（存货）", "包装物金额（可空）"),
        ("1601", "固定资产原值", "设备、车、厂房原值合计（可空）"),
        ("1602", "累计折旧", "到 10 月底已提折旧（填正数，可空）"),
    ];
    for (i, (code, name, m)) in accts.iter().enumerate() {
        let row = 2 + i as u32;
        s.write_string(row, 0, *code, &text).ok();
        s.write_string(row, 1, *name, &text).ok();
        s.write_number(row, 2, 0.0, &money).ok();
        s.write_string(row, 3, *m, &note).ok();
    }

    // ---------- Sheet 5: 基本资料 ----------
    let s = wb.add_worksheet();
    s.set_name("④ 基本资料").expect("sheet");
    s.set_column_width(0, 22).ok();
    s.set_column_width(1, 20).ok();
    s.set_column_width(2, 52).ok();

    s.merge_range(0, 0, 0, 2, "基本资料与平衡检查", &title)
        .expect("t");
    s.set_row_height(0, 26).ok();
    s.write_string(1, 0, "项目", &head).ok();
    s.write_string(1, 1, "金额", &head).ok();
    s.write_string(1, 2, "说明", &head).ok();

    let rows: &[(&str, &str)] = &[
        ("应收合计", "抄「① 客户应收」的合计数"),
        ("账户余额合计", "抄「③ 账户余额」的 100201~1602 之和"),
        ("资产合计", "= 应收 + 账户余额合计"),
        ("应付合计", "抄「② 供应商应付」的合计数"),
        ("欠薪（应付职工薪酬 221101）", "还没发的工资"),
        ("欠税（应交税费 2221）", "还没缴的税"),
        ("负债合计", "= 应付 + 欠薪 + 欠税"),
        ("所有者权益（轧差）", "= 资产合计 − 负债合计"),
        ("其中：实收资本 4001", "老板实际投进去的本钱"),
        (
            "其中：本年利润 4103",
            "= 所有者权益 − 实收资本（前期赚的没分掉的）",
        ),
        ("平衡检查", "应为 0：实收资本 + 本年利润 − 所有者权益"),
    ];
    for (i, (k, m)) in rows.iter().enumerate() {
        let row = 2 + i as u32;
        s.write_string(row, 0, *k, &text).ok();
        s.write_number(row, 1, 0.0, &money).ok();
        s.write_string(row, 2, *m, &note).ok();
    }

    wb.save(&out).expect("保存");
    println!("已生成 {out}");
    println!(
        "  客户 {} 家、供应商 {} 家已预填编码与名称",
        cust.len(),
        supp.len()
    );
}
