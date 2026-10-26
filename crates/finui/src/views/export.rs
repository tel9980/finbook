//! 导出：CSV / Excel / 打印预览
//!
//! 财务数据导出有两个坑：
//! 1. CSV 必须转义逗号、引号、换行，否则金额里的千分位会把列切错；
//! 2. 直接用 Excel 打开 CSV 时，中文会因为缺 BOM 变乱码，科目编码 `001` 会被吃掉前导零。
//!    所以 CSV 一律写 UTF-8 BOM，Excel 走真正的 xlsx。
//!
//! 打印预览：把二维表渲染成一张自带打印样式的 HTML，用系统默认浏览器打开，
//! 用户按 Ctrl/Cmd+P 即可打印。这样普通账户（无导出权限）也能拿到纸面报表，
//! 但数据不会以文件形式落地，符合"只能打印、不能导出"的管控要求。

use std::path::{Path, PathBuf};

use egui::{RichText, Ui};
use fincore::Perm;
use rust_xlsxwriter::Workbook;

use crate::platform::open_path;
use crate::state::AppCtx;

/// 一张二维表，首行为表头
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Sheet {
    pub fn new(name: &str, headers: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            headers,
            rows: Vec::new(),
        }
    }
    pub fn push(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// CSV 单元格转义（含防公式注入）
fn csv_cell(s: &str) -> String {
    // 以 = + - @ 开头的文本在 Excel/LibreOffice 打开时会被当公式执行；
    // 摘要、客户名等常来自导入数据，必须在导出侧统一加单引号前缀。
    let guarded =
        if s.starts_with('=') || s.starts_with('+') || s.starts_with('-') || s.starts_with('@') {
            format!("'{s}")
        } else {
            s.to_string()
        };
    if guarded.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

/// 生成带 BOM 的 CSV 文本
pub fn to_csv(s: &Sheet) -> String {
    let mut out = String::from("\u{feff}"); // UTF-8 BOM
    let mut line: Vec<String> = Vec::new();
    line.extend(s.headers.iter().map(|h| csv_cell(h)));
    out.push_str(&line.join(","));
    out.push('\n');
    for r in &s.rows {
        let cells: Vec<String> = r.iter().map(|c| csv_cell(c)).collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

pub fn write_csv(s: &Sheet, path: &Path) -> Result<usize, String> {
    let text = to_csv(s);
    std::fs::write(path, text).map_err(|e| format!("写入失败：{e}"))?;
    Ok(s.rows.len())
}

/// 判断是否按数字写：金额要能被 Excel 求和，但带前导零的编码不能丢零
fn as_number(s: &str) -> Option<f64> {
    let t = s.replace(',', "").trim().to_string();
    if t.is_empty() {
        return None;
    }
    if t.starts_with('0') && !t.starts_with("0.") {
        return None;
    }
    if t.chars()
        .any(|c| !c.is_ascii_digit() && c != '.' && c != '-')
    {
        return None;
    }
    t.parse::<f64>().ok()
}

pub fn write_xlsx(sheets: &[Sheet], path: &Path) -> Result<usize, String> {
    if sheets.is_empty() {
        return Err("没有可导出的数据".to_string());
    }
    let mut wb = Workbook::new();
    let mut total = 0usize;
    for (si, s) in sheets.iter().enumerate() {
        let ws = wb.add_worksheet();
        let name = if s.name.is_empty() {
            format!("Sheet{}", si + 1)
        } else {
            s.name.clone()
        };
        // Excel 工作表名不能超过 31 字符且不能含 []:*?/\
        let mut name: String = name.chars().take(31).collect();
        for bad in [':', '\\', '/', '?', '*', '[', ']'] {
            name = name.replace(bad, "-");
        }
        if name.trim().is_empty() {
            name = format!("Sheet{}", si + 1);
        }
        ws.set_name(&name)
            .map_err(|e| format!("工作表命名失败：{e}"))?;
        for (c, h) in s.headers.iter().enumerate() {
            ws.write_string(0, c as u16, h)
                .map_err(|e| format!("写表头失败：{e}"))?;
        }
        for (ri, row) in s.rows.iter().enumerate() {
            for (ci, cell) in row.iter().enumerate() {
                if let Some(v) = as_number(cell) {
                    ws.write_number((ri + 1) as u32, ci as u16, v)
                } else {
                    ws.write_string((ri + 1) as u32, ci as u16, cell)
                }
                .map_err(|e| format!("写单元格失败：{e}"))?;
            }
        }
        total += s.rows.len();
    }
    wb.save(path).map_err(|e| format!("保存 xlsx 失败：{e}"))?;
    Ok(total)
}

/// 弹出保存对话框
pub fn pick_save(default_name: &str, xlsx: bool) -> Option<PathBuf> {
    let (name, exts): (String, &[&str]) = if xlsx {
        (format!("{default_name}.xlsx"), &["xlsx"])
    } else {
        (format!("{default_name}.csv"), &["csv"])
    };
    let mut d = rfd::FileDialog::new().set_file_name(&name);
    for e in exts {
        d = d.add_filter(e.to_uppercase().as_str(), &[*e]);
    }
    d.save_file()
}

/// 弹出打开对话框
pub fn pick_open(exts: &[&str]) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("数据文件", exts)
        .pick_file()
}

/// 统一的导出入口：弹出路径选择并按扩展名落盘
pub fn export_sheet(sheet: &Sheet, default_name: &str, xlsx: bool) -> Result<String, String> {
    if sheet.is_empty() {
        return Err("没有可导出的数据".to_string());
    }
    let Some(path) = pick_save(default_name, xlsx) else {
        return Err("已取消".to_string());
    };
    let n = if xlsx {
        write_xlsx(std::slice::from_ref(sheet), &path)?
    } else {
        write_csv(sheet, &path)?
    };
    Ok(format!("已导出 {n} 行到 {}", path.display()))
}

/// 保存导入模板（.xlsx）。
///
/// 与 [`export_sheet`] 不同：模板是"空架子 + 示例行 + 填表说明"，不含账套数据，
/// 所以不走导出权限那一套 —— 调用方按旁边「批量粘贴」按钮同权限放行即可。
/// 返回 `Ok(None)` 表示用户在保存对话框点了取消（不算错，调用方直接无视）。
pub fn save_template(sheets: &[Sheet], default_name: &str) -> Result<Option<String>, String> {
    let Some(path) = pick_save(default_name, true) else {
        return Ok(None);
    };
    write_xlsx(sheets, &path)?;
    Ok(Some(format!(
        "模板已保存到 {}：用 Excel 填好后，回来点「从 Excel 文件载入」或复制粘贴导入",
        path.display()
    )))
}

/// 把 fincore 的批量模板转成两页 xlsx：第一页是数据区（表头 + 示例行），
/// 第二页是填表说明。
///
/// 分页是刻意的：Excel 读取只读第一页（见 `findb::imports::read_xlsx`），
/// 说明文字放第二页就不会被当成数据解析。
pub fn batch_template_sheets(data_name: &str, tpl: &fincore::batch::Template) -> Vec<Sheet> {
    let mut data = Sheet::new(data_name, tpl.headers.clone());
    for r in &tpl.examples {
        data.push(r.clone());
    }
    let mut help = Sheet::new(
        "填表说明",
        vec!["填表说明（本页不用导入，只看）".to_string()],
    );
    for h in &tpl.help {
        help.push(vec![h.clone()]);
    }
    vec![data, help]
}

/// 生成可打印的 HTML（自带打印样式，自动唤起打印对话框）
///
/// 版式规则全部来自 `fincore::print`（与网页端共用一份）：纸张方向、字号、页边距、
/// 页码、落款。这里只负责把 `Sheet` 拼成表体。
///
/// 对齐方式按**列内容**判断，而不是「最后一列右对齐」：凭证清单、明细账、档案列表
/// 的最后一列都是备注或摘要，硬性右对齐会把整列文字顶到右边，看着像排版坏了。
fn sheet_to_html(sheet: &Sheet, title: &str, opts: &fincore::account::BookOptions) -> String {
    let po = &opts.print;
    let title = escape_html(title);
    let cols = sheet.headers.len().max(1);
    // 每列是不是金额列（决定右对齐 + 等宽数字）
    let money_cols: Vec<bool> = (0..cols)
        .map(|c| fincore::print::is_money_column(sheet.rows.iter().filter_map(|r| r.get(c))))
        .collect();
    let mut rows = String::new();
    for r in &sheet.rows {
        rows.push_str("<tr>");
        for c in 0..cols {
            let cell = r.get(c).map(|s| s.as_str()).unwrap_or("");
            if money_cols[c] {
                rows.push_str(&format!("<td class='r'>{}</td>", escape_html(cell)));
            } else {
                rows.push_str(&format!("<td>{}</td>", escape_html(cell)));
            }
        }
        rows.push_str("</tr>\n");
    }
    let mut headers = String::new();
    for (i, h) in sheet.headers.iter().enumerate() {
        let cls = if money_cols.get(i).copied().unwrap_or(false) {
            " class='r'"
        } else {
            ""
        };
        headers.push_str(&format!("<th{cls}>{}</th>", escape_html(h)));
    }
    let company = if po.show_company {
        format!("<div class='sub'>编制单位：{}</div>", escape_html(&opts.company))
    } else {
        String::new()
    };
    let printed_at = if po.show_printed_at {
        format!(
            "<div class='sub'>打印时间：{}</div>",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        )
    } else {
        String::new()
    };
    let sig = po.signature_html();
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN"><head><meta charset="utf-8">
<title>{title}</title>
<style>
  body {{ font-family: "Microsoft YaHei","PingFang SC","Noto Sans CJK SC",sans-serif; color:#111; margin:24px; }}
  h1 {{ font-size:18px; text-align:center; margin:0 0 6px; }}
  .sub {{ text-align:center; color:#555; font-size:12px; }}
  table {{ border-collapse:collapse; width:100%; margin-top:10px; }}
  th, td {{ border:1px solid #999; padding:4px 8px; text-align:left; }}
  th {{ background:#eef3f7; }}
  td.r, th.r {{ text-align:right; font-variant-numeric:tabular-nums; }}
  .sig {{ margin-top:18px; color:#444; }}
  @media print {{ body {{ margin:0; }} }}
  {page}
</style>
<script>window.onload=function(){{ setTimeout(function(){{ window.print(); }}, 300); }};</script>
</head><body>
<h1>{title}</h1>
{company}{printed_at}
<table><thead><tr>{headers}</tr></thead><tbody>
{rows}</tbody></table>
{sig}
</body></html>"#,
        title = title,
        company = company,
        printed_at = printed_at,
        headers = headers,
        rows = rows,
        sig = sig,
        page = po.page_css(cols),
    )
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 打印预览：生成 HTML 用默认浏览器打开并自动弹出打印对话框。
/// 不落 Excel/CSV 文件，满足"普通账户只能打印、不能导出"的管控。
pub fn print_sheet(
    sheet: &Sheet,
    title: &str,
    opts: &fincore::account::BookOptions,
) -> Result<String, String> {
    if sheet.is_empty() {
        return Err("没有可打印的数据".to_string());
    }
    print_html_content(title, &sheet_to_html(sheet, title, opts))
}

/// 把一个现成的打印 HTML 写入零时文件并用系统默认浏览器打开（自动唤起打印）。
/// 供「凭证套打」「账簿套打」复用，保证会计档案版式统一。
pub fn print_html_content(title: &str, html: &str) -> Result<String, String> {
    let dir = std::env::temp_dir().join("finbook_print");
    let _ = std::fs::create_dir_all(&dir);
    let safe = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = dir.join(format!("{safe}.html"));
    std::fs::write(&path, html).map_err(|e| format!("生成打印页失败：{e}"))?;
    open_path(&path).map_err(|e| format!("打开打印预览失败：{e}"))?;
    Ok("已打开打印预览，请按 Ctrl/Cmd+P 打印（数据未以文件形式导出）".to_string())
}

/// 导出 / 打印的目标格式
#[derive(Clone, Copy, Debug)]
pub enum ExportMode {
    /// 导出 Excel（.xlsx）
    Excel,
    /// 导出 CSV
    Csv,
    /// 打印预览（HTML，不落地文件）
    Print,
}

/// 按模式执行导出 / 打印
pub fn run_export(
    sh: &Sheet,
    file_name: &str,
    print_title: &str,
    mode: ExportMode,
    opts: &fincore::account::BookOptions,
) -> Result<String, String> {
    match mode {
        ExportMode::Excel => export_sheet(sh, file_name, true),
        ExportMode::Csv => export_sheet(sh, file_name, false),
        ExportMode::Print => print_sheet(sh, print_title, opts),
    }
}

/// 渲染"导出 / 打印"按钮组并按权限门控。
///
/// - 导出 Excel / CSV 需要 `Export` 权限（仅管理员与财务主管），普通账户看不到这两个按钮；
/// - 打印预览人人可用（需 `Report` 权限），但数据只以 HTML 形式打印，不会落地成文件，
///   满足"普通账户只能打印、不能导出"的管控要求。
///
/// 返回被点击的模式；调用方据此执行实际的导出 / 打印。
pub fn export_print_controls(ui: &mut Ui, ctx: &AppCtx<'_>) -> Option<ExportMode> {
    if ctx.user().can(Perm::Export) {
        if ui.button("导出 Excel").clicked() {
            return Some(ExportMode::Excel);
        }
        if ui.button("导出 CSV").clicked() {
            return Some(ExportMode::Csv);
        }
    } else {
        ui.label(RichText::new("（无导出权限，仅可打印预览）").small().weak());
    }
    if ctx.user().can(Perm::Report) && ui.button("打印预览").clicked() {
        return Some(ExportMode::Print);
    }
    None
}
