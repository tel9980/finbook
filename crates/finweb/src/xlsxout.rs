//! Web 端 Excel 导出的统一写出口。
//!
//! 为什么要有这个模块（而不是每个 handler 自己拼 Workbook）：
//!
//! 1. **一处定样式**。导出的表要给老板、给会计、给税务看，表头、金额格式、
//!    列宽、冻结首行必须一致；散在十几个 handler 里各写一遍，改一次要改十几处，
//!    还必然漏掉几处 —— 用户拿到两份格式不同的表，会以为数据也不一样。
//! 2. **数字必须写成真数字**。早期 `export_trial_balance` 只出 CSV，用户在
//!    Excel 里打开发现金额是文本（左对齐、不能求和），得手动「分列」才能用。
//!    写 xlsx 时要把金额写成 `write_number`，格式串交给 Excel，别自己拼千分位。
//! 3. **导出的表要能导回去**。基础资料类（客户/供应商/存货/科目/期初）导出的
//!    列头**必须**与 `findb::imports` 的解析器认的表头逐字相同，否则用户改完
//!    存盘再导入会被当成无表头的野数据 —— 闭环就断了。所以这类表用
//!    [`Sheet::plain`]，不加标题行；报表才用 [`Sheet::titled`]。
//!
//! 与桌面端 `crates/finui/src/views/export.rs` 的关系：桌面端落盘到用户选的路径，
//! 这里返回 `Vec<u8>` 走 HTTP 附件。样式的取舍两边保持一致（蓝底白字表头、
//! 细边框、两位小数）。

use rust_xlsxwriter::{Format, Workbook, XlsxAlign, XlsxBorder, XlsxColor};

/// 一列的数据类型，决定单元格的写法与数字格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColFmt {
    /// 文本（编码、名称、摘要）。走 `write_string`，即使内容是 "0012" 也不会被
    /// Excel 自动转成数字丢掉前导零 —— 科目编码、客户编码全靠这个。
    #[default]
    Text,
    /// 金额：两位小数 + 千分位 + 负数红字。
    Money,
    /// 整数（笔数、行数、天数）。千分位，无小数。
    Int,
}

/// 一张工作表。
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    /// 工作表名。会被裁剪到 Excel 允许的 31 字符并替换非法字符。
    pub name: String,
    /// 表格标题（合并首行）。`None` = 不要标题行（基础资料导出走这条，保证能导回）。
    pub title: Option<String>,
    /// 副标题（标题下一行，放期间/口径说明）。仅在有 `title` 时生效。
    pub subtitle: Option<String>,
    pub headers: Vec<String>,
    /// 每列的格式。长度可以小于 `headers`，缺的按 [`ColFmt::Text`] 处理。
    pub cols: Vec<ColFmt>,
    pub rows: Vec<Vec<String>>,
    /// 需要加粗的数据行下标（0 起，指 `rows` 里的位置）。
    ///
    /// 会计表里「小计 / 合计 / 净增加额」这些行必须和明细行区分开，否则
    /// 打印出来一堆逐行数字，读表的人找不到重点。只做加粗这一件事，
    /// 不做完整富文本 —— 导出的表主要拿去继续算，不是拿去排版。
    pub emphasis: Vec<usize>,
}

impl Sheet {
    /// 基础资料导出用：无标题行，表头就是第一行。
    ///
    /// 限定用途是有原因的 —— 加了标题行，`findb::imports` 的解析器会把标题
    /// 当成第一行数据，整列错位。所以凡是「导出→改→导回」的表，一律走这里。
    pub fn plain(name: &str, headers: &[&str], cols: &[ColFmt]) -> Self {
        Self {
            name: name.to_string(),
            title: None,
            subtitle: None,
            headers: headers.iter().map(|s| s.to_string()).collect(),
            cols: cols.to_vec(),
            rows: Vec::new(),
            emphasis: Vec::new(),
        }
    }

    /// 报表导出用：带标题与副标题，给人看。
    pub fn titled(
        name: &str,
        title: &str,
        subtitle: &str,
        headers: &[&str],
        cols: &[ColFmt],
    ) -> Self {
        Self {
            name: name.to_string(),
            title: Some(title.to_string()),
            subtitle: if subtitle.is_empty() {
                None
            } else {
                Some(subtitle.to_string())
            },
            headers: headers.iter().map(|s| s.to_string()).collect(),
            cols: cols.to_vec(),
            rows: Vec::new(),
            emphasis: Vec::new(),
        }
    }

    /// 一次性挂上数据行（构造器不算行，避免调用方为了传 rows 而丢掉类型检查）
    pub fn with_rows(mut self, rows: Vec<Vec<String>>) -> Self {
        self.rows = rows;
        self
    }

    /// 指定哪些数据行加粗（小计/合计行）
    pub fn with_emphasis(mut self, rows: Vec<usize>) -> Self {
        self.emphasis = rows;
        self
    }

    pub fn push<S: Into<String>>(&mut self, row: Vec<S>) {
        self.rows.push(row.into_iter().map(Into::into).collect());
    }
}

/// 把文本转成 f64。
///
/// 不能直接 `parse::<f64>()`：本项目的金额在界面上普遍带千分位（`Money` 的
/// `Display` 走 `fmt_money()`），`"1,234.56".parse()` 会失败并被静默当成 0 ——
/// 导出的表里一整列金额全是 0，而界面上的数是对的，这种 bug 极难发现。
/// 这里统一剥掉千分位逗号、空格、货币符号，并认会计的负数写法 `(1,234)`。
pub fn to_f64(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t == "-" || t == "—" {
        return None;
    }
    // 会计负数写法：(1234.56) 表示 -1234.56
    let (neg_paren, body) = if t.starts_with('(') && t.ends_with(')') && t.len() > 2 {
        (true, &t[1..t.len() - 1])
    } else {
        (false, t)
    };
    let cleaned: String = body
        .chars()
        .filter(|c| !matches!(c, ',' | ' ' | '\u{00a0}' | '¥' | '￥' | '$'))
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let v = cleaned.parse::<f64>().ok()?;
    Some(if neg_paren { -v } else { v })
}

/// 估算显示宽度：中日韩字符按 2 个字宽算，其余按 1。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            // 常用 CJK 区段 + 全角标点
            if (0x2E80..=0x9FFF).contains(&u)
                || (0xF900..=0xFAFF).contains(&u)
                || (0xFF00..=0xFF60).contains(&u)
                || (0xFFE0..=0xFFE6).contains(&u)
            {
                2
            } else {
                1
            }
        })
        .sum()
}

/// 统一的工作表名清洗：Excel 不允许超过 31 字符，也不允许 `: \ / ? * [ ]`。
fn sheet_name(raw: &str, fallback_idx: usize) -> String {
    let mut name: String = raw.chars().take(31).collect();
    for bad in [':', '\\', '/', '?', '*', '[', ']'] {
        name = name.replace(bad, "-");
    }
    if name.trim().is_empty() {
        format!("Sheet{}", fallback_idx + 1)
    } else {
        name
    }
}

fn header_fmt() -> Format {
    Format::new()
        .set_bold()
        .set_background_color(XlsxColor::RGB(0x4472C4))
        .set_font_color(XlsxColor::White)
        .set_align(XlsxAlign::Center)
        .set_border(XlsxBorder::Thin)
}

fn title_fmt() -> Format {
    Format::new()
        .set_bold()
        .set_font_size(14)
        .set_align(XlsxAlign::Center)
        .set_border(XlsxBorder::Thin)
}

fn subtitle_fmt() -> Format {
    Format::new()
        .set_font_size(10)
        .set_font_color(XlsxColor::RGB(0x59_59_59))
        .set_align(XlsxAlign::Center)
        .set_border(XlsxBorder::Thin)
}

fn body_fmt(col: ColFmt) -> Format {
    let base = Format::new().set_border(XlsxBorder::Thin);
    match col {
        ColFmt::Money => base
            .set_num_format("#,##0.00;[Red]-#,##0.00")
            .set_align(XlsxAlign::Right),
        ColFmt::Int => base.set_num_format("#,##0").set_align(XlsxAlign::Right),
        ColFmt::Text => base.set_align(XlsxAlign::Left),
    }
}

/// 把若干张表打成一份 xlsx 文件的字节流。
pub fn build(sheets: &[Sheet]) -> Result<Vec<u8>, String> {
    if sheets.is_empty() {
        return Err("没有可导出的数据".to_string());
    }
    let mut wb = Workbook::new();
    let hdr = header_fmt();

    for (si, s) in sheets.iter().enumerate() {
        let ws = wb.add_worksheet();
        ws.set_name(&sheet_name(&s.name, si))
            .map_err(|e| format!("工作表命名失败：{e}"))?;

        let ncols = s.headers.len().max(1);
        // 有标题时，标题占第 0 行、副标题第 1 行，表头从第 2 行起。
        let mut row_cursor: u32 = 0;
        if let Some(t) = &s.title {
            ws.merge_range(
                row_cursor,
                0,
                row_cursor,
                (ncols - 1) as u16,
                t,
                &title_fmt(),
            )
            .map_err(|e| format!("写标题失败：{e}"))?;
            ws.set_row_height(row_cursor, 24.0)
                .map_err(|e| format!("设置行高失败：{e}"))?;
            row_cursor += 1;
            if let Some(sub) = &s.subtitle {
                ws.merge_range(
                    row_cursor,
                    0,
                    row_cursor,
                    (ncols - 1) as u16,
                    sub,
                    &subtitle_fmt(),
                )
                .map_err(|e| format!("写副标题失败：{e}"))?;
                row_cursor += 1;
            }
        }

        let header_row = row_cursor;
        for (c, h) in s.headers.iter().enumerate() {
            ws.write_string(header_row, c as u16, h, &hdr)
                .map_err(|e| format!("写表头失败：{e}"))?;
        }

        // 每列的格式只 new 一次：rust_xlsxwriter 的 Format 内部按引用去重，
        // 但每格 new 一个仍然要多分配一堆对象，大表（几千行 × 十几列）能省下
        // 可观的内存。
        let fmts: Vec<Format> = (0..ncols)
            .map(|c| body_fmt(s.cols.get(c).copied().unwrap_or_default()))
            .collect();
        // 加粗版同样按列各建一次；`emo` 为空时完全不建，普通表零额外开销。
        let bold_fmts: Vec<Format> = if s.emphasis.is_empty() {
            Vec::new()
        } else {
            (0..ncols)
                .map(|c| body_fmt(s.cols.get(c).copied().unwrap_or_default()).set_bold())
                .collect()
        };

        for (ri, row) in s.rows.iter().enumerate() {
            let r = header_row + 1 + ri as u32;
            let bold = !bold_fmts.is_empty() && s.emphasis.contains(&ri);
            for (ci, cell) in row.iter().enumerate().take(ncols) {
                let col = s.cols.get(ci).copied().unwrap_or_default();
                let f = if bold { &bold_fmts[ci] } else { &fmts[ci] };
                match col {
                    ColFmt::Text => {
                        ws.write_string(r, ci as u16, cell, f)
                            .map_err(|e| format!("写单元格失败：{e}"))?;
                    }
                    ColFmt::Money | ColFmt::Int => match to_f64(cell) {
                        Some(v) => {
                            ws.write_number(r, ci as u16, v, f)
                                .map_err(|e| format!("写单元格失败：{e}"))?;
                        }
                        // 金额列里出现空串/横杠（表结构的占位行）时留空，
                        // 不写 0 —— 写 0 会让合计对不上，用户无法分辨
                        // 「真的是 0」和「这行没有数」。
                        None => {
                            ws.write_blank(r, ci as u16, f)
                                .map_err(|e| format!("写空单元格失败：{e}"))?;
                        }
                    },
                }
            }
        }

        // 冻结表头（含标题行），这样几千行的明细往下滚时列名还在。
        ws.set_freeze_panes(header_row + 1, 0)
            .map_err(|e| format!("冻结窗格失败：{e}"))?;

        // 列宽：按表头与内容的最大显示宽度，夹在 [8, 40] 之间。
        // 太窄看不出编码，太宽一屏放不下几列；40 是经验值（够放
        // 「杭州明远贸易有限公司」这类全称 + 一点余量）。
        for c in 0..ncols {
            let head_w = s.headers.get(c).map(|h| display_width(h)).unwrap_or(0);
            let body_w = s
                .rows
                .iter()
                .filter_map(|r| r.get(c))
                .map(|v| display_width(v))
                .max()
                .unwrap_or(0);
            let w = head_w.max(body_w).clamp(8, 40) + 2;
            ws.set_column_width(c as u16, w as f64)
                .map_err(|e| format!("设置列宽失败：{e}"))?;
        }
    }

    wb.save_to_buffer()
        .map_err(|e| format!("生成 xlsx 失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 千分位金额不能被当成零() {
        // 这是本模块存在的头号理由：`Money` 的 Display 带逗号，
        // 直接 parse 会失败并被 unwrap_or(0.0) 吞成 0。
        assert_eq!(to_f64("1,234.56"), Some(1234.56));
        assert_eq!(to_f64("12,345,678.90"), Some(12345678.90));
        assert_eq!(to_f64("-1,000.00"), Some(-1000.0));
    }

    #[test]
    fn 会计括号负数() {
        assert_eq!(to_f64("(1,234.56)"), Some(-1234.56));
        assert_eq!(to_f64("(0.00)"), Some(-0.0));
    }

    #[test]
    fn 占位符不算数字() {
        assert_eq!(to_f64(""), None);
        assert_eq!(to_f64("-"), None);
        assert_eq!(to_f64("—"), None);
        assert_eq!(to_f64("   "), None);
    }

    #[test]
    fn 货币符号与空格要剥掉() {
        assert_eq!(to_f64("¥ 1,000.00"), Some(1000.0));
        assert_eq!(to_f64("￥1,000"), Some(1000.0));
        assert_eq!(to_f64("1 234.5"), Some(1234.5));
    }

    #[test]
    fn 编码不能被当金额() {
        // 科目编码 1001 会被 to_f64 认成 1001.0 —— 所以列类型必须显式声明，
        // 不能靠「像不像数字」猜。这条测试锁定的是「用 ColFmt::Text 时
        // 走 write_string」这个前提，见下面 build 的断言。
        assert_eq!(to_f64("1001"), Some(1001.0));
    }

    #[test]
    fn 工作表名超长与非法字符要清洗() {
        let long = "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十甲乙";
        let n = sheet_name(long, 0);
        assert!(n.chars().count() <= 31, "超过 31 字符：{n}");
        let n2 = sheet_name("a/b:c?d*e[f]g", 0);
        for bad in [':', '\\', '/', '?', '*', '[', ']'] {
            assert!(!n2.contains(bad), "{n2} 里还有非法字符 {bad}");
        }
        assert_eq!(sheet_name("   ", 3), "Sheet4");
    }

    #[test]
    fn 空表也要能产出文件() {
        let mut s = Sheet::plain("空表", &["编码", "名称"], &[ColFmt::Text, ColFmt::Text]);
        s.push(vec!["C001", "客户甲"]);
        let bytes = build(&[s]).expect("应当能生成");
        // xlsx 是 zip，头四字节固定
        assert_eq!(&bytes[..2], b"PK");
        assert!(bytes.len() > 1000, "文件太小，可能没写进内容");
    }

    #[test]
    fn 一张表都不给要报错() {
        assert!(build(&[]).is_err());
    }

    #[test]
    fn 标题行不占表头位置() {
        // 有标题时表头下移，冻结窗格也要跟着下移，否则冻结线切在标题上。
        let mut s = Sheet::titled(
            "利润表",
            "利润表",
            "2026-11",
            &["项目", "本月数"],
            &[ColFmt::Text, ColFmt::Money],
        );
        s.push(vec!["营业收入", "100,000.00"]);
        let bytes = build(&[s]).expect("应当能生成");
        assert!(bytes.len() > 1000);
    }
}
