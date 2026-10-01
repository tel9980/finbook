//! 报表 PDF 导出（纯 Rust，无外部服务依赖）。
//!
//! 使用 `printpdf` 生成 PDF，中文字体在运行时从系统常见路径加载（微软雅黑 / 黑体 /
//! Noto Sans CJK / 苹方等），嵌入 PDF 后中文科目名可正常显示。

use std::io::{BufWriter, Cursor};

use printpdf::*;

use fincore::balance::{BalanceRow, TrialBalance};

/// 尝试从系统加载一个 CJK 字体，返回 (字体名, 字节)。
fn load_cjk_font() -> Option<(String, Vec<u8>)> {
    let candidates: Vec<(&str, &str)> = if cfg!(windows) {
        vec![
            ("微软雅黑", "C:\\Windows\\Fonts\\msyh.ttc"),
            ("黑体", "C:\\Windows\\Fonts\\simhei.ttf"),
            ("宋体", "C:\\Windows\\Fonts\\simsun.ttc"),
            ("等线", "C:\\Windows\\Fonts\\Deng.ttf"),
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            ("苹方", "/System/Library/Fonts/PingFang.ttc"),
            ("黑体-简", "/System/Library/Fonts/STHeiti Light.ttc"),
        ]
    } else {
        vec![
            (
                "Noto Sans CJK",
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            ),
            (
                "Noto Sans CJK SC",
                "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
            ),
            (
                "文泉驿微米黑",
                "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            ),
            (
                "文泉驿正黑",
                "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            ),
        ]
    };
    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() {
                return Some((name.to_string(), bytes));
            }
        }
    }
    None
}

/// 生成科目余额表 PDF。返回 PDF 字节。
pub fn trial_balance_pdf(
    company: &str,
    from: &str,
    to: &str,
    rows: &[BalanceRow],
    totals: &TrialBalance,
) -> Result<Vec<u8>, String> {
    let (doc, page0, layer0) = PdfDocument::new("科目余额表", Mm(297.0), Mm(210.0), "main");

    // 嵌入 CJK 字体；找不到时退化为内置字体（中文会缺字，但数字/编码仍可读）
    let cjk = load_cjk_font();
    let font = match &cjk {
        Some((_name, bytes)) => doc
            .add_external_font(Cursor::new(bytes.clone()))
            .map_err(|e| format!("字体加载失败：{e}"))?,
        None => doc
            .add_builtin_font(BuiltinFont::Helvetica)
            .map_err(|e| format!("内置字体加载失败：{e}"))?,
    };

    let col_x = [12.0_f32, 42.0, 122.0, 160.0, 190.0, 220.0, 250.0, 265.0];
    let heads = [
        "科目编码",
        "科目名称",
        "期初余额",
        "本期借方",
        "本期贷方",
        "期末余额",
        "本年累计借方",
        "本年累计贷方",
    ];

    // 画一页表头，返回正文起始 y
    let header = |doc: &PdfDocumentReference, page: PdfPageIndex, layer: PdfLayerIndex, first: bool| -> f32 {
        let l = doc.get_page(page).get_layer(layer);
        let mut y = 14.0_f32;
        if first {
            l.use_text(format!("{company} 科目余额表"), 16.0, Mm(12.0), Mm(16.0), &font);
            l.use_text(format!("期间：{from} 至 {to}"), 9.0, Mm(12.0), Mm(24.0), &font);
            y = 32.0;
        }
        for (x, h) in col_x.iter().zip(heads.iter()) {
            l.use_text(*h, 8.5, Mm(*x), Mm(y), &font);
        }
        y + 6.0
    };

    let mut page = page0;
    let mut layer = layer0;
    let mut y = header(&doc, page, layer, true);
    let mut count = 0usize;
    const ROWS_PER_PAGE: usize = 26;

    for r in rows {
        if count >= ROWS_PER_PAGE {
            let (p, l) = doc.add_page(Mm(297.0), Mm(210.0), "main");
            page = p;
            layer = l;
            y = header(&doc, page, layer, false);
            count = 0;
        }
        let l = doc.get_page(page).get_layer(layer);
        l.use_text(r.account_code.clone(), 7.5, Mm(col_x[0]), Mm(y), &font);
        l.use_text(r.account_name.clone(), 7.5, Mm(col_x[1]), Mm(y), &font);
        let (bdir, bamt) = r.begin_dir_amount();
        l.use_text(format!("{} {}", bdir.label(), bamt.fmt_money()), 7.5, Mm(col_x[2]), Mm(y), &font);
        l.use_text(r.debit.fmt_money(), 7.5, Mm(col_x[3]), Mm(y), &font);
        l.use_text(r.credit.fmt_money(), 7.5, Mm(col_x[4]), Mm(y), &font);
        let (edir, eamt) = r.end_dir_amount();
        l.use_text(format!("{} {}", edir.label(), eamt.fmt_money()), 7.5, Mm(col_x[5]), Mm(y), &font);
        l.use_text(r.ytd_debit.fmt_money(), 7.5, Mm(col_x[6]), Mm(y), &font);
        l.use_text(r.ytd_credit.fmt_money(), 7.5, Mm(col_x[7]), Mm(y), &font);
        y += 6.0;
        count += 1;
    }

    // 合计行
    let l = doc.get_page(page).get_layer(layer);
    l.use_text("合计", 8.5, Mm(col_x[1]), Mm(y), &font);
    l.use_text(
        format!("借 {} / 贷 {}", totals.begin_debit.fmt_money(), totals.begin_credit.fmt_money()),
        7.5,
        Mm(col_x[2]),
        Mm(y),
        &font,
    );
    l.use_text(totals.period_debit.fmt_money(), 7.5, Mm(col_x[3]), Mm(y), &font);
    l.use_text(totals.period_credit.fmt_money(), 7.5, Mm(col_x[4]), Mm(y), &font);
    l.use_text(
        format!("借 {} / 贷 {}", totals.end_debit.fmt_money(), totals.end_credit.fmt_money()),
        7.5,
        Mm(col_x[5]),
        Mm(y),
        &font,
    );

    // 页脚提示（若缺失 CJK 字体）
    if cjk.is_none() {
        let l = doc.get_page(page).get_layer(layer);
        l.use_text(
            "未找到中文字体，科目名称可能缺字；数字与编码不受影响。",
            8.0,
            Mm(12.0),
            Mm(204.0),
            &font,
        );
    }

    let mut buf = BufWriter::new(Vec::new());
    doc.save(&mut buf).map_err(|e| format!("PDF 生成失败：{e}"))?;
    let bytes = buf.into_inner().map_err(|e| format!("PDF 缓冲失败：{e}"))?;
    Ok(bytes)
}
