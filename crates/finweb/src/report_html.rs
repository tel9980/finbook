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
    let mut section = |title: &str,
                       lines: &[fincore::report::cashflow::CashFlowLine],
                       net: &fincore::Money| {
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
         @media print{{body{{font-size:12px;margin:0;}}}}\
         .pbar{{position:sticky;top:0;z-index:9;display:flex;gap:14px;align-items:center;background:#1a1a1a;color:#fff;padding:8px 14px;font-family:system-ui,sans-serif;font-size:12px;}}\
         .pbar button{{background:#fff;color:#111;border:0;border-radius:4px;padding:6px 14px;font-size:13px;cursor:pointer;}}\
         @media print{{.pbar{{display:none;}}}}</style></head>\
         <body><h2>{}</h2>\
         <div class='meta'><span>编制单位：{}</span><span>单位：{}</span></div>\
         <div class='meta'><span></span><span>{}　打印时间：{}</span></div>\
         {}\
         <div class='pbar'><button onclick='window.print()'>🖨 打印本页</button><span>纸张 / 份数在打印对话框中选择；关闭本页即取消</span></div>\
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
        );
        assert!(
            html.contains("<td class='r'>0.00</td>"),
            "普通行零值应显示 0.00，实际 HTML：{html}"
        );
    }

    /// 负数的红字样式不能被这次改动顺带弄丢。
    #[test]
    fn print_negative_keeps_red_style() {
        let html = report_table_html(
            &table(LineStyle::Normal, vec![fincore::Money::from_cents(-123456)]),
            "测试公司",
            "元",
        );
        assert!(
            html.contains("<td class='r' style='color:#c00'>-1,234.56</td>"),
            "负数应保留红字样式，实际 HTML：{html}"
        );
    }

    /// 零值/负值都要经过千分位格式化，且两列表头都在。
    #[test]
    fn print_table_has_header_and_two_value_columns() {
        let html = report_table_html(
            &table(
                LineStyle::Total,
                vec![fincore::Money::ZERO, fincore::Money::from_cents(100000)],
            ),
            "测试公司",
            "元",
        );
        assert!(html.contains("<th class='r'>本期金额</th>"), "{html}");
        assert!(html.contains("<th class='r'>本年累计金额</th>"), "{html}");
        assert!(html.contains("<td class='r'>0.00</td>"), "{html}");
        assert!(html.contains("<td class='r'>1,000.00</td>"), "{html}");
    }
}
