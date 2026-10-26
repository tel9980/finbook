//! 报表打印 HTML 渲染（Web 端通用打印预览）
//!
//! 输入已算好的报表结构（`fincore::report::ReportTable` / 现金流量表 / 权益变动表），
//! 输出一张自带打印样式的 HTML 页，浏览器打开后自动唤起打印对话框。
//! 与桌面端 `finui::views::export::print_sheet` 同思路：只生成预览页，不落地文件。
//!
//! 版式（纸张方向/字号/页边距/页码/落款）不再写死在这里，而是从账套参数里取
//! `fincore::PrintOptions` —— 桌面端和网页端共用同一份规则，见 `fincore::print`。

use fincore::report::{LineStyle, ReportTable};
use fincore::PrintOptions;

fn esc(s: &str) -> String {
    fincore::print::esc(s)
}

/// 通用打印页壳：标题 + 编制单位 + 期间 + 表头 + 表体 + 合计
pub fn report_table_html(
    table: &ReportTable,
    company: &str,
    unit: &str,
    po: &PrintOptions,
) -> String {
    let mut thead = String::new();
    thead.push_str("<th style='width:40px'>行次</th><th>项目</th>");
    for c in &table.columns {
        thead.push_str(&format!("<th class='r'>{}</th>", esc(c)));
    }
    let mut body = String::new();
    for r in &table.rows {
        let indent = "　".repeat(r.indent as usize);
        let style = match r.style {
            LineStyle::Total => " style='font-weight:bold;background:#fafafa'",
            LineStyle::Subtotal => " style='font-weight:600'",
            LineStyle::Header => " style='font-weight:600;background:#f5f7fa'",
            LineStyle::Blank => " style='height:10px'",
            LineStyle::Normal => "",
        };
        let mut cells = String::new();
        for v in &r.values {
            // 零值也印 0.00，不再留空。
            //
            // 财务惯例的「零不印」只在纸面上成立；到了屏幕上，一片空白会被当成
            // 「金额列没取到数 / 接口返回缺列」——用户实际就是这么报障的。
            // 网页端（app.js）与接口（`table_json`）一直是 `0.00`，打印页原来
            // 单独留空，属于同一张表两套口径。这里统一成 0.00。
            cells.push_str(&format!(
                "<td class='r'{}>{}</td>",
                if v.is_negative() && r.show_negative_red {
                    " style='color:#c00'"
                } else {
                    ""
                },
                v.fmt_money()
            ));
        }
        body.push_str(&format!(
            "<tr{style}><td class='c'>{}</td><td>{}{}</td>{cells}</tr>",
            esc(&r.no),
            indent,
            esc(&r.name),
        ));
    }
    let cols = 2 + table.columns.len();
    shell(
        &table.title,
        company,
        &table.subtitle,
        unit,
        &format!("<table><thead><tr>{thead}</tr></thead><tbody>{body}</tbody></table>"),
        cols,
        po,
    )
}

/// 现金流量表打印页（三类活动分行渲染）
pub fn cash_flow_html(
    cf: &fincore::report::cashflow::CashFlowStatement,
    company: &str,
    subtitle: &str,
    po: &PrintOptions,
) -> String {
    let mut body = String::new();
    let mut section = |name: &str,
                       lines: &[fincore::report::cashflow::CashFlowLine],
                       net: fincore::Money| {
        body.push_str(&format!(
            "<tr style='font-weight:600;background:#f5f7fa'><td colspan='4'>{}</td></tr>",
            esc(name)
        ));
        for l in lines {
            body.push_str(&format!(
                "<tr><td>{}</td><td class='r'>{}</td><td class='r'>{}</td><td class='r'>{}</td></tr>",
                esc(&l.name),
                l.inflow.fmt_money(),
                l.outflow.fmt_money(),
                l.net.fmt_money()
            ));
        }
        body.push_str(&format!(
            "<tr style='font-weight:600'><td>小计</td><td></td><td></td><td class='r'>{}</td></tr>",
            net.fmt_money()
        ));
    };
    section("一、经营活动", &cf.operating, cf.operating_net);
    section("二、投资活动", &cf.investing, cf.investing_net);
    section("三、筹资活动", &cf.financing, cf.financing_net);
    body.push_str(&format!(
        "<tr style='font-weight:bold;background:#fafafa'><td>现金及现金等价物净增加额</td>\
         <td></td><td></td><td class='r'>{}</td></tr>",
        cf.net_increase.fmt_money()
    ));
    body.push_str(&format!(
        "<tr><td>加：期初现金及现金等价物余额</td><td></td><td></td><td class='r'>{}</td></tr>",
        cf.begin_cash.fmt_money()
    ));
    body.push_str(&format!(
        "<tr style='font-weight:bold'><td>期末现金及现金等价物余额</td>\
         <td></td><td></td><td class='r'>{}</td></tr>",
        cf.end_cash.fmt_money()
    ));
    let ties = cf.ties();
    body.push_str(&format!(
        "<tr><td colspan='4' style='color:{}'>{}</td></tr>",
        if ties { "#2e7d32" } else { "#c00" },
        if ties {
            "勾稽：期末现金 − 期初现金 = 净增加额 ✓"
        } else {
            "勾稽：净增加额与期初期末差不等，请检查未标注现金流量的凭证"
        }
    ));
    if cf.unassigned != fincore::Money::ZERO {
        body.push_str(&format!(
            "<tr><td colspan='4' style='color:#c00'>未标注现金流量项目的收付：{}</td></tr>",
            cf.unassigned.fmt_money()
        ));
    }
    shell(
        "现金流量表",
        company,
        subtitle,
        "元",
        &format!(
            "<table><thead><tr><th>项目</th><th class='r'>流入</th><th class='r'>流出</th>\
             <th class='r'>净额</th></tr></thead><tbody>{body}</tbody></table>"
        ),
        4,
        po,
    )
}

/// 所有者权益变动表打印页
pub fn equity_html(
    e: &fincore::report::equity::EquityStatement,
    company: &str,
    subtitle: &str,
    po: &PrintOptions,
) -> String {
    let mut body = String::new();
    for l in &e.lines {
        body.push_str(&format!(
            "<tr><td>{}</td><td class='r'>{}</td><td class='r'>{}</td><td class='r'>{}</td></tr>",
            esc(&l.name),
            l.begin.fmt_money(),
            l.change.fmt_money(),
            l.end.fmt_money()
        ));
    }
    body.push_str(&format!(
        "<tr style='font-weight:bold;background:#fafafa'><td>{}</td>\
         <td class='r'>{}</td><td class='r'>{}</td><td class='r'>{}</td></tr>",
        esc(&e.total.name),
        e.total.begin.fmt_money(),
        e.total.change.fmt_money(),
        e.total.end.fmt_money()
    ));
    shell(
        "所有者权益变动表",
        company,
        subtitle,
        "元",
        &format!(
            "<table><thead><tr><th>项目</th><th class='r'>年初余额</th>\
             <th class='r'>本年增减</th><th class='r'>年末余额</th>\
             </tr></thead><tbody>{body}</tbody></table>"
        ),
        4,
        po,
    )
}

/// 打印页外壳（统一排版，打开即弹打印对话框）
///
/// `cols` 用来判自动方向（见 `PrintOptions::landscape`），`po` 提供纸张/字号/边距/页码/落款。
fn shell(
    title: &str,
    company: &str,
    subtitle: &str,
    unit: &str,
    table: &str,
    cols: usize,
    po: &PrintOptions,
) -> String {
    let company_cell = if po.show_company {
        format!("<span>编制单位：{}</span>", esc(company))
    } else {
        "<span></span>".to_string()
    };
    let time_cell = if po.show_printed_at {
        format!(
            "<span>打印时间：{}</span>",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        )
    } else {
        "<span></span>".to_string()
    };
    let sig = po.signature_html();
    // 预览页上临时改方向/字号：只影响本次打印，不改账套设置（长期设置要进「系统设置」）
    let init_dir = if po.landscape(cols) {
        "landscape"
    } else {
        "portrait"
    };
    format!(
        "<!doctype html><html lang='zh-CN'><head><meta charset='utf-8'>\
         <title>{title}</title>\
         <style>body{{font-family:-apple-system,'Microsoft YaHei',sans-serif;color:#222;margin:16px;}}\
         h2{{text-align:center;margin:8px 0;}}\
         .meta{{display:flex;justify-content:space-between;color:#666;font-size:13px;margin-bottom:4px;}}\
         table{{border-collapse:collapse;width:100%;margin-top:8px;}}\
         th,td{{border:1px solid #bbb;padding:4px 8px;}}\
         th{{background:#f0f3f7;}}td.r{{text-align:right;}}td.c{{text-align:center;color:#888;}}\
         .sig{{margin-top:18px;color:#444;}}\
         .pbar{{position:sticky;top:0;z-index:9;display:flex;gap:10px;align-items:center;background:#1a1a1a;color:#fff;padding:8px 14px;font-family:system-ui,sans-serif;font-size:12px;}}\
         .pbar button{{background:#fff;color:#111;border:0;border-radius:4px;padding:6px 12px;font-size:13px;cursor:pointer;}}\
         .pbar .hint{{opacity:.72;}}\
         @media print{{.pbar{{display:none;}}body{{margin:0;}}\
         tr,td,th{{break-inside:avoid;page-break-inside:avoid;}}}}\
         {page}</style>\
         <style id='live'></style></head>\
         <body><h2>{title}</h2>\
         <div class='meta'>{company_cell}<span>单位：{unit}</span></div>\
         <div class='meta'><span>{subtitle}</span>{time_cell}</div>\
         {table}{sig}\
         <div class='pbar'><button onclick='window.print()'>🖨 打印本页</button>\
         <button onclick=\"live('landscape')\">横向</button>\
         <button onclick=\"live('portrait')\">纵向</button>\
         <button onclick='live(null,1)'>字大</button><button onclick='live(null,-1)'>字小</button>\
         <span class='hint'>这里的调整只影响本次打印；要长期生效请到「系统设置 → 打印预览」</span></div>\
         <script>\
         var MG={margin},PT={font},DIR='{init_dir}';\
         function live(o,d){{if(o)DIR=o;PT=Math.min(14,Math.max(9,PT+d));\
         document.getElementById('live').textContent=\
         '@page{{size:A4 '+DIR+';margin:'+MG+'mm}}body,table{{font-size:'+PT+'pt}}';}}\
         </script>\
         </body></html>",
        title = esc(title),
        company_cell = company_cell,
        unit = esc(unit),
        subtitle = esc(subtitle),
        time_cell = time_cell,
        table = table,
        sig = sig,
        page = po.page_css(cols),
        margin = po.margin_mm,
        font = po.font_pt,
        init_dir = init_dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::report::ReportRow;

    fn table(style: LineStyle, values: Vec<fincore::Money>) -> ReportTable {
        ReportTable {
            title: "利润表".to_string(),
            subtitle: "2026年11月 至 2026年11月".to_string(),
            company: "测试公司".to_string(),
            columns: vec!["本期金额".to_string(), "本年累计金额".to_string()],
            rows: vec![ReportRow {
                no: "1".to_string(),
                name: "一、营业收入".to_string(),
                indent: 0,
                style,
                values,
                show_negative_red: true,
            }],
        }
    }

    /// 回归：普通行零值必须印 `0.00`，不能留空。
    ///
    /// 旧实现按「财务惯例」把 `LineStyle::Normal` 的 0 打成空字符串，
    /// 打印预览整列空白，用户据此报「利润表金额列丢了」。
    #[test]
    fn print_zero_normal_row_shows_money() {
        let html = report_table_html(
            &table(LineStyle::Normal, vec![fincore::Money::ZERO]),
            "测试公司",
            "元",
            &PrintOptions::default(),
        );
        assert!(html.contains("0.00"), "零值必须印成 0.00");
    }

    /// 版式取账套设置：页码、落款、方向都要真的进 HTML
    #[test]
    fn print_page_honours_book_settings() {
        let po = PrintOptions {
            orientation: "landscape".into(),
            font_pt: 9,
            margin_mm: 20,
            show_page_no: true,
            sign_line: "制表：老李".into(),
            ..PrintOptions::default()
        };
        let html = report_table_html(
            &table(LineStyle::Normal, vec![fincore::Money::ZERO]),
            "测试公司",
            "元",
            &po,
        );
        assert!(html.contains("size:A4 landscape"), "设置里选了横向就该横向");
        assert!(html.contains("font-size:9pt"), "字号要跟着设置");
        assert!(html.contains("margin:20mm"), "页边距要跟着设置");
        assert!(html.contains("counter(page)"), "勾了页码就要有页码");
        assert!(html.contains("制表：老李"), "落款文字要出现");
    }

    /// 关掉的项目不能留个空壳在纸上
    #[test]
    fn print_switches_remove_their_blocks() {
        let po = PrintOptions {
            show_company: false,
            show_printed_at: false,
            show_page_no: false,
            show_signatures: false,
            ..PrintOptions::default()
        };
        let html = report_table_html(
            &table(LineStyle::Normal, vec![fincore::Money::ZERO]),
            "测试公司",
            "元",
            &po,
        );
        assert!(!html.contains("编制单位"), "关了编制单位还印出来就是没生效");
        assert!(!html.contains("打印时间"));
        assert!(!html.contains("counter(page)"));
        assert!(!html.contains("class='sig'"));
    }

    /// 列多自动横向：两端是同一套判定，不能只改桌面端
    #[test]
    fn wide_table_turns_landscape_by_itself() {
        let po = PrintOptions::default();
        let mut t = table(LineStyle::Normal, vec![fincore::Money::ZERO]);
        t.columns = (1..=10).map(|i| format!("列{i}")).collect();
        let html = report_table_html(&t, "测试公司", "元", &po);
        assert!(html.contains("size:A4 landscape"), "10 列在纵向放不下");
    }
}
