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
    can_edit: bool,
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
    page_shell(
        &table.title,
        company,
        &table.subtitle,
        unit,
        &format!("<table><thead><tr>{thead}</tr></thead><tbody>{body}</tbody></table>"),
        cols,
        po,
        can_edit,
    )
}

/// 现金流量表打印页（三类活动分行渲染）
pub fn cash_flow_html(
    cf: &fincore::report::cashflow::CashFlowStatement,
    company: &str,
    subtitle: &str,
    po: &PrintOptions,
    can_edit: bool,
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
    page_shell(
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
        can_edit,
    )
}

/// 所有者权益变动表打印页
pub fn equity_html(
    e: &fincore::report::equity::EquityStatement,
    company: &str,
    subtitle: &str,
    po: &PrintOptions,
    can_edit: bool,
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
    page_shell(
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
        can_edit,
    )
}


/// 预览页控制条：有「系统设置」权限时用这个 —— 每一项都是**立刻生效 + 存进账套**。
///
/// 为什么把控件做进打印预览页：版式调不调得对，只有在**看到那张纸**的时候才知道。
/// 原来是「去系统设置改 → 回来点打印 → 发现不对 → 再回去」，来回三五趟就放弃了，
/// 最后就那么歪着打。点一下、拖一下能当场看到、并且自动存住，才算真的能用。
const EDIT_BAR: &str = "\
<div class='pbar'>\
<button onclick='window.print()'>🖨 打印本页</button>\
<label>方向<select id='p-o'>\
<option value='auto'>自动（列多转横向）</option>\
<option value='portrait'>纵向</option>\
<option value='landscape'>横向</option></select></label>\
<label>字号<input type='range' id='p-f' min='9' max='14' step='1'><span id='f-v'></span></label>\
<label>页边距<input type='range' id='p-m' min='5' max='30' step='1'><span id='m-v'></span></label>\
<label><input type='checkbox' id='p-c'>编制单位</label>\
<label><input type='checkbox' id='p-t'>打印时间</label>\
<label><input type='checkbox' id='p-g'>页码</label>\
<label><input type='checkbox' id='p-s'>落款</label>\
<span class='st' id='p-st'></span>\
<span class='hint'>改这里就是改本账套的默认版式（所有人打印都生效）</span></div>";

/// 没权限时的控制条：只影响本次打印，并且明说。
const READONLY_BAR: &str = "\
<div class='pbar'>\
<button onclick='window.print()'>🖨 打印本页</button>\
<button onclick=\"live('landscape')\">横向</button>\
<button onclick=\"live('portrait')\">纵向</button>\
<button onclick='live(null,1)'>字大</button><button onclick='live(null,-1)'>字小</button>\
<span class='hint'>你的账号没有「系统设置」权限：这里的调整只对本次打印生效，不会保存</span></div>";

/// 有权限时的前端逻辑。
///
/// 两次防抖：拖动滑块会连续触发 `input`，等手停下来 400ms 再发一次请求；
/// 另外用 `seq` 丢掉过期响应 —— 否则快速拖几下，先发的慢请求后到，状态栏会
/// 显示成「保存失败」，而其实早就存好了。
const EDIT_JS: &str = r#"
var COLS=__COLS__, AUTO=__AUTOCOLS__, MG=__MG__, PT=__PT__, DIR='__DIR__', PG=__PG__===1;
var LV=document.getElementById('live');
function dirNow(){ if(DIR==='auto') return COLS>AUTO?'landscape':'portrait'; return DIR; }
// 与 fincore::print::page_css 保持同形；改一处要想着另一处
function cssNow(){ return '@page{size:A4 '+dirNow()+';margin:'+MG+'mm}body,table{font-size:'+PT+'pt}'; }
// 页码单独一条：它只在打印时看得见，开关时不需要重算纸张
var PGNUM='@page{@bottom-right{content:"第 " counter(page) " 页 / 共 " counter(pages) " 页"}}';
function paint(){
  LV.textContent=cssNow();
  document.getElementById('live2').textContent=PG?PGNUM:'';
  document.getElementById('q-co').style.display=document.getElementById('p-c').checked?'':'none';
  document.getElementById('q-tm').style.display=document.getElementById('p-t').checked?'':'none';
  document.getElementById('q-sg').style.display=document.getElementById('p-s').checked?'':'none';
  document.getElementById('f-v').textContent=PT+' pt';
  document.getElementById('m-v').textContent=MG+' mm';
}
var seq=0, timer=null;
function save(){
  var st=document.getElementById('p-st');
  st.textContent='保存中…'; st.className='st';
  var mine=++seq;
  if(timer) clearTimeout(timer);
  timer=setTimeout(function(){
    var body={font_pt:PT, margin_mm:MG, orientation:DIR,
              show_company:document.getElementById('p-c').checked,
              show_printed_at:document.getElementById('p-t').checked,
              show_page_no:PG,
              show_signatures:document.getElementById('p-s').checked};
    fetch('/api/reports/print-settings',{method:'POST',credentials:'same-origin',
        headers:{'Content-Type':'application/json'},body:JSON.stringify(body)})
      .then(function(r){
        if(r.ok) return r.json();
        return r.json().then(function(j){throw new Error(j.error||('HTTP '+r.status));},
                             function(){throw new Error('HTTP '+r.status);});
      })
      .then(function(){ if(mine!==seq) return; st.textContent='已保存 ✓'; st.className='st ok'; })
      .catch(function(e){ if(mine!==seq) return; st.textContent='保存失败：'+e.message; st.className='st bad'; });
  },400);
}
function bind(id, fn){
  var el=document.getElementById(id);
  el.addEventListener('input',function(){ fn(); paint(); save(); });
  el.addEventListener('change',function(){ fn(); paint(); save(); });
}
bind('p-f', function(){ PT=+document.getElementById('p-f').value; });
bind('p-m', function(){ MG=+document.getElementById('p-m').value; });
bind('p-c', function(){});
bind('p-t', function(){});
bind('p-s', function(){});
bind('p-g', function(){ PG=document.getElementById('p-g').checked; });
document.getElementById('p-o').addEventListener('change',function(){
  DIR=document.getElementById('p-o').value; paint(); save();
});
// 初值：把服务端当前设置写进控件，颜色/字号的显示才和纸上一致
document.getElementById('p-f').value=PT;
document.getElementById('p-m').value=MG;
document.getElementById('p-o').value=DIR;
document.getElementById('p-c').checked=document.getElementById('q-co').style.display!=='none';
document.getElementById('p-t').checked=document.getElementById('q-tm').style.display!=='none';
document.getElementById('p-s').checked=document.getElementById('q-sg').style.display!=='none';
document.getElementById('p-g').checked=PG;
paint();
"#;

/// 没权限时的临时调整（只改本次预览的样式，不发请求）。
const READONLY_JS: &str = r#"
var MG=__MG__, PT=__PT__, DIR='__DIR__';
function live(o,d){
  if(o) DIR=o;
  if(d) PT=Math.min(14,Math.max(9,PT+d));
  document.getElementById('live').textContent=
    '@page{size:A4 '+DIR+';margin:'+MG+'mm}body,table{font-size:'+PT+'pt}';
}
"#;

/// 打印页外壳：**任何**一张表都能套（表体自己拼好传进来）。
///
/// `cols` 用来判自动方向（见 `PrintOptions::landscape`），`po` 提供纸张/字号/边距/页码/落款。
///
/// 公开的理由：科目余额表、明细账这些页面以前各自写了一份 `<style>`，于是
/// 「网页打印有页码、这张表没有」这类分叉反复出现。表体各不相同，**壳必须只有一个**。
pub fn page_shell(
    title: &str,
    company: &str,
    subtitle: &str,
    unit: &str,
    table: &str,
    cols: usize,
    po: &PrintOptions,
    can_edit: bool,
) -> String {
    let company_cell = if po.show_company {
        format!("<span id='q-co'>编制单位：{}</span>", esc(company))
    } else {
        "<span id='q-co' style='display:none'>编制单位：{}</span>".to_string()
    };
    let time_cell = format!(
        "<span id='q-tm'{}>打印时间：{}</span>",
        if po.show_printed_at { "" } else { " style='display:none'" },
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    );
    let sig_div = format!(
        "<div class='sig' id='q-sg'{}>{}</div>",
        if po.show_signatures { "" } else { " style='display:none'" },
        po.signature_html()
    );
    // 预览页的控制条分两种：
    //   有「系统设置」权限 → 真控件，改完就存进账套（点一下、拖一下都算）；
    //   没有权限 → 只给本次打印用的临时按钮，并写明不会保存。
    let (bar, js) = if can_edit {
        (
            EDIT_BAR,
            EDIT_JS
                .replace("__COLS__", &cols.to_string())
                .replace("__AUTOCOLS__", &fincore::print::AUTO_LANDSCAPE_COLS.to_string())
                .replace("__MG__", &po.margin_mm.to_string())
                .replace("__PT__", &po.font_pt.to_string())
                .replace("__DIR__", &po.orientation)
                .replace("__PG__", if po.show_page_no { "1" } else { "0" }),
        )
    } else {
        (
            READONLY_BAR,
            READONLY_JS
                .replace("__MG__", &po.margin_mm.to_string())
                .replace("__PT__", &po.font_pt.to_string())
                .replace("__DIR__", &po.orientation),
        )
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
         .pbar{{position:sticky;top:0;z-index:9;display:flex;flex-wrap:wrap;gap:10px 14px;\
         align-items:center;background:#1a1a1a;color:#fff;padding:8px 14px;\
         font-family:system-ui,sans-serif;font-size:12px;}}\
         .pbar button{{background:#fff;color:#111;border:0;border-radius:4px;padding:6px 12px;\
         font-size:13px;cursor:pointer;}}\
         .pbar label{{display:flex;gap:6px;align-items:center;cursor:pointer;}}\
         .pbar input[type=range]{{width:110px;}}\
         .pbar select{{border:0;border-radius:4px;padding:4px 6px;font-size:13px;}}\
         .pbar .hint{{opacity:.72;}}\
         .pbar .st{{min-width:76px;opacity:.9;}}\
         .pbar .st.ok{{color:#7ee787;}}.pbar .st.bad{{color:#ff9a8b;}}\
         @media print{{.pbar{{display:none;}}body{{margin:0;}}\
         tr,td,th{{break-inside:avoid;page-break-inside:avoid;}}}}\
         {page}</style>\
         <style id='live'></style><style id='live2'></style></head>\
         <body><h2>{title}</h2>\
         <div class='meta'>{company_cell}<span>单位：{unit}</span></div>\
         <div class='meta'><span>{subtitle}</span>{time_cell}</div>\
         {table}{sig_div}\
         {bar}\
         <script>{js}</script>\
         </body></html>",
        title = esc(title),
        company_cell = company_cell,
        unit = esc(unit),
        subtitle = esc(subtitle),
        time_cell = time_cell,
        table = table,
        sig_div = sig_div,
        bar = bar,
        js = js,
        page = po.page_css(cols),
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
            false,
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
            false,
        );
        assert!(html.contains("size:A4 landscape"), "设置里选了横向就该横向");
        assert!(html.contains("font-size:9pt"), "字号要跟着设置");
        assert!(html.contains("margin:20mm"), "页边距要跟着设置");
        assert!(html.contains("counter(page)"), "勾了页码就要有页码");
        assert!(html.contains("制表：老李"), "落款文字要出现");
    }

    /// 关掉的项目不能留个空壳在纸上
    #[test]
    fn print_switches_hide_their_blocks() {
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
            false,
        );
        // 这几个块现在**留在 DOM 里但 display:none**：预览页的勾选框要能当场打开/关掉，
        // 删掉节点就没法「改一下看一眼」。打印与屏幕都一样不显示，所以观感没变。
        assert!(html.contains("id='q-co' style='display:none'"), "{html}");
        assert!(html.contains("id='q-tm' style='display:none'"));
        assert!(html.contains("id='q-sg' style='display:none'"));
        assert!(!html.contains("counter(page)"), "关了页码不该有页码规则");
        // 「隐藏」而不是「不存在」的前提是：隐藏的块确实带 display:none，
        // 别哪天只删了文字、留下一个空 div 占位
        assert!(!html.contains("<span id='q-co'></span>"));
    }

    /// 有「系统设置」权限的预览页要给真控件（点一下、拖一下就能存）。
    #[test]
    fn editable_preview_offers_controls_that_save() {
        let po = PrintOptions::default();
        let html = report_table_html(
            &table(LineStyle::Normal, vec![fincore::Money::ZERO]),
            "测试公司",
            "元",
            &po,
            true,
        );
        for need in [
            "id='p-f'",              // 字号滑块
            "id='p-m'",              // 页边距滑块
            "id='p-o'",              // 方向
            "/api/reports/print-settings", // 保存地址
        ] {
            assert!(html.contains(need), "缺 {need}：预览页改不了也存不住");
        }
        assert!(!html.contains("只对本次打印生效"), "有权限就不该说这句话");
    }

    /// 没权限时：控件不存在，并且明说不会保存。
    #[test]
    fn readonly_preview_says_adjustments_are_not_saved() {
        let po = PrintOptions::default();
        let html = report_table_html(
            &table(LineStyle::Normal, vec![fincore::Money::ZERO]),
            "测试公司",
            "元",
            &po,
            false,
        );
        assert!(!html.contains("id='p-f'"), "没权限不该看到会保存的控件");
        assert!(!html.contains("/api/reports/print-settings"));
        assert!(html.contains("只对本次打印生效"), "{html}");
    }

    /// 列多自动横向：两端是同一套判定，不能只改桌面端
    #[test]
    fn wide_table_turns_landscape_by_itself() {
        let po = PrintOptions::default();
        let mut t = table(LineStyle::Normal, vec![fincore::Money::ZERO]);
        t.columns = (1..=10).map(|i| format!("列{i}")).collect();
        let html = report_table_html(&t, "测试公司", "元", &po, true);
        assert!(html.contains("size:A4 landscape"), "10 列在纵向放不下");
    }
}
