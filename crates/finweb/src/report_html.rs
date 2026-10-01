//! 报表打印 HTML 渲染（Web 端通用打印预览）
//!
//! 输入已算好的报表结构（`fincore::report::ReportTable` / 现金流量表 / 权益变动表），
//! 输出一张自带打印样式的 HTML 页，浏览器打开后自动唤起打印对话框。
//! 与桌面端 `finui::views::export::print_sheet` 同思路：只生成预览页，不落地文件。

use fincore::report::{LineStyle, ReportTable};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 通用打印页壳：标题 + 编制单位 + 期间 + 表头 + 表体 + 合计
pub fn report_table_html(table: &ReportTable, company: &str, unit: &str) -> String {
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
        for (i, v) in r.values.iter().enumerate() {
            let text = if v.is_zero() && r.style == LineStyle::Normal {
                String::new()
            } else {
                v.fmt_money()
            };
            cells.push_str(&format!(
                "<td class='r'{}>{}</td>",
                if v.is_negative() && r.show_negative_red {
                    " style='color:#c00'"
                } else {
                    ""
                },
                text
            ));
            let _ = i;
        }
        body.push_str(&format!(
            "<tr{style}><td class='c'>{}</td><td>{}{}</td>{cells}</tr>",
            esc(&r.no),
            indent,
            esc(&r.name),
        ));
    }
    shell(
        &table.title,
        company,
        &table.subtitle,
        unit,
        &format!("<table><thead><tr>{thead}</tr></thead><tbody>{body}</tbody></table>"),
    )
}

/// 现金流量表打印页（三类活动分行渲染）
pub fn cash_flow_html(
    cf: &fincore::report::cashflow::CashFlowStatement,
    company: &str,
    subtitle: &str,
) -> String {
    let mut body = String::new();
    let mut section = |title: &str, lines: &[fincore::report::cashflow::CashFlowLine], net: &fincore::Money| {
        body.push_str(&format!(
            "<tr style='font-weight:600;background:#f5f7fa'><td colspan='3'>{}</td></tr>",
            esc(title)
        ));
        for l in lines {
            body.push_str(&format!(
                "<tr><td class='c'></td><td>{}</td><td class='r'>{}</td></tr>",
                esc(&l.name),
                if l.net.is_negative() {
                    format!("<span style='color:#c00'>{}</span>", l.net.fmt_money())
                } else {
                    l.net.fmt_money()
                }
            ));
        }
        body.push_str(&format!(
            "<tr style='font-weight:600'><td class='c'></td><td>{} 小计</td><td class='r'>{}</td></tr>",
            esc(title),
            net.fmt_money()
        ));
    };
    section("经营活动产生的现金流量", &cf.operating, &cf.operating_net);
    section("投资活动产生的现金流量", &cf.investing, &cf.investing_net);
    section("筹资活动产生的现金流量", &cf.financing, &cf.financing_net);
    body.push_str(&format!(
        "<tr style='font-weight:bold;background:#fafafa'><td class='c'></td><td>现金及现金等价物净增加额</td><td class='r'>{}</td></tr>",
        cf.net_increase.fmt_money()
    ));
    body.push_str(&format!(
        "<tr><td class='c'></td><td>加：期初现金及现金等价物余额</td><td class='r'>{}</td></tr>",
        cf.begin_cash.fmt_money()
    ));
    body.push_str(&format!(
        "<tr style='font-weight:bold;background:#fafafa'><td class='c'></td><td>期末现金及现金等价物余额</td><td class='r'>{}</td></tr>",
        cf.end_cash.fmt_money()
    ));
    body.push_str(&format!(
        "<tr><td colspan='3' style='color:#888'>{}</td></tr>",
        if cf.ties() {
            "✔ 净增加额与货币资金变动勾稽一致"
        } else {
            "✖ 勾稽不符：净增加额 ≠ 期末 − 期初货币资金"
        }
    ));
    shell(
        "现金流量表",
        company,
        subtitle,
        "元",
        &format!(
            "<table><thead><tr><th style='width:40px'>行次</th><th>项目</th><th class='r'>金额</th></tr></thead><tbody>{body}</tbody></table>"
        ),
    )
}

/// 权益变动表打印页
pub fn equity_html(
    e: &fincore::report::equity::EquityStatement,
    company: &str,
    subtitle: &str,
) -> String {
    let mut body = String::new();
    for l in &e.lines {
        body.push_str(&format!(
            "<tr><td class='c'></td><td>{}</td><td class='r'>{}</td><td class='r'>{}</td><td class='r'>{}</td></tr>",
            esc(&l.name),
            l.begin.fmt_money(),
            l.change.fmt_money(),
            l.end.fmt_money(),
        ));
    }
    let t = &e.total;
    body.push_str(&format!(
        "<tr style='font-weight:bold;background:#fafafa'><td class='c'></td><td>合计</td><td class='r'>{}</td><td class='r'>{}</td><td class='r'>{}</td></tr>",
        t.begin.fmt_money(),
        t.change.fmt_money(),
        t.end.fmt_money(),
    ));
    shell(
        "所有者权益变动表",
        company,
        subtitle,
        "元",
        &format!(
            "<table><thead><tr><th style='width:40px'>行次</th><th>项目</th>\
             <th class='r'>年初余额</th><th class='r'>本年增减</th><th class='r'>年末余额</th>\
             </tr></thead><tbody>{body}</tbody></table>"
        ),
    )
}

/// 打印页外壳（统一排版，打开即弹打印对话框）
fn shell(title: &str, company: &str, subtitle: &str, unit: &str, table: &str) -> String {
    format!(
        "<!doctype html><html lang='zh-CN'><head><meta charset='utf-8'>\
         <title>{}</title>\
         <style>body{{font-family:-apple-system,'Microsoft YaHei',sans-serif;color:#222;margin:16px;}}\
         h2{{text-align:center;margin:8px 0;}}\
         .meta{{display:flex;justify-content:space-between;color:#666;font-size:13px;margin-bottom:4px;}}\
         table{{border-collapse:collapse;width:100%;margin-top:8px;font-size:13px;}}\
         th,td{{border:1px solid #bbb;padding:4px 8px;}}\
         th{{background:#f0f3f7;}}td.r{{text-align:right;}}td.c{{text-align:center;color:#888;}}\
         @media print{{body{{font-size:12px;margin:0;}}}}</style></head>\
         <body><h2>{}</h2>\
         <div class='meta'><span>编制单位：{}</span><span>单位：{}</span></div>\
         <div class='meta'><span></span><span>{}　打印时间：{}</span></div>\
         {}\
         <script>window.onload=function(){{setTimeout(function(){{window.print();}},300);}};</script>\
         </body></html>",
        esc(title),
        esc(title),
        esc(company),
        esc(unit),
        esc(subtitle),
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        table,
    )
}
