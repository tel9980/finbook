//! 打印版式（纸张、字号、页边距、页码、落款）。
//!
//! ## 为什么放在 fincore 而不是各自的界面层
//!
//! 桌面端（`finui::views::export`）和网页端（`finweb::report_html`）各有自己的
//! HTML 拼装代码，但**版式规则必须只有一份**。分叉的后果不是编译错误，而是
//! 「网页打印有页码、桌面打印没有」「网页横向、桌面竖向」这种谁也说不清的差异 ——
//! 没有人会同时打开两处源码比对，而用户只会说「打印出来怪怪的」。
//!
//! 所以：设置结构（[`PrintOptions`]）+ CSS 生成（[`page_css`]）+ 落款（[`signature_html`]）
//! 都在这里，两端只负责拼表体。

use serde::{Deserialize, Serialize};

/// 列数超过这个值就默认改横向：A4 纵向扣掉页边距后约 186mm，
/// 一列内容（日期/摘要/数量金额）平均要占 20mm 以上，8 列就已经挤到折行。
pub const AUTO_LANDSCAPE_COLS: usize = 8;

/// 套打用纸：凭证 / 账簿 / 单据三种用途各留一档。
///
/// 为什么不是「一个全局纸型」：小厂的现实是**三种纸同时在用**——凭证是 A5 或
/// 241×140 连续纸、账簿是 A4、收付款单是 A5 或三等分联。一个全局纸型必然有一头
/// 打歪，而打歪的表现是「内容压在线上的格子里」，不像字小一点那样能将就。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperUse {
    /// 记账凭证
    Voucher,
    /// 明细账 / 总账
    Ledger,
    /// 订单 / 收付款单
    Doc,
}

/// 可选纸型：`(值, 界面标签)`。桌面端与网页端都从这里取，避免两端各写一份清单。
///
/// `""` 只对单据有意义（= 跟随单据模板自带的纸型），凭证/账簿选了它按 A4 走。
pub fn paper_choices() -> [(&'static str, &'static str); 5] {
    [
        ("A4", "A4（210×297）"),
        ("A5", "A5（210×148）"),
        ("third", "三等分联（99×210）"),
        ("241x140", "241×140 连续纸（针式）"),
        ("", "跟随单据模板"),
    ]
}

/// 把纸型值解析成 CSS `@page size` 片段；`None` = 空或认不出来。
///
/// 认不出来时**不报错也不猜**，由 [`PrintOptions::problems`] 在保存时挡住，
/// 打印这一刻按兜底值走（脏配置不该让人打不出纸）。
pub fn paper_size_css(value: &str, landscape: bool) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    let base = if v.eq_ignore_ascii_case("a4") {
        "A4".to_string()
    } else if v.eq_ignore_ascii_case("a5") {
        "A5".to_string()
    } else if v.eq_ignore_ascii_case("third") {
        // 三等分联纸不是标准名，只能给尺寸
        return Some("99mm 210mm".to_string());
    } else if let Some((w, h)) = v.split_once('x').or_else(|| v.split_once('X')) {
        let w: i32 = w.trim().parse().ok()?;
        let h: i32 = h.trim().parse().ok()?;
        if !(50..=600).contains(&w) || !(50..=600).contains(&h) {
            return None;
        }
        // 自定义尺寸必须显式写死方向，否则浏览器按自己的规则猜
        return Some(format!("{w}mm {h}mm"));
    } else {
        return None;
    };
    Some(if landscape {
        format!("{base} landscape")
    } else {
        base
    })
}

/// 打印设置（存在账套参数里，跟随账套而不是跟随某台电脑）。
///
/// 跟随账套的理由：小工厂常常「办公室打报表、宿舍看电脑」，跟着机器走会让同一份
/// 报表在两台机器上打出两种版式；而账套本来就是「这套账的口径」。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintOptions {
    /// 纸张方向：`auto`（按列数决定）/ `portrait` / `landscape`
    pub orientation: String,
    /// 正文字号（pt），9–14
    pub font_pt: u8,
    /// 页边距（mm），5–30
    pub margin_mm: u8,
    /// 显示「编制单位」
    pub show_company: bool,
    /// 显示「打印时间」
    pub show_printed_at: bool,
    /// 显示页码（第 N 页 / 共 M 页）
    pub show_page_no: bool,
    /// 显示落款（制表 / 审核 / 负责人）
    pub show_signatures: bool,
    /// 落款文字，可自行改（比如加上店名或「复核」）
    pub sign_line: String,
    /// 套打横向偏移（毫米，正=向右）：针式打印机走纸偏左/偏右时用来对位
    pub offset_x_mm: i16,
    /// 套打纵向偏移（毫米，正=向下）
    pub offset_y_mm: i16,
    /// 凭证套打纸型（空 = A4）
    pub voucher_paper: String,
    /// 账簿套打纸型（空 = A4）
    pub ledger_paper: String,
    /// 单据套打纸型（空 = 跟随单据模板自带的纸型）
    pub doc_paper: String,
}

impl Default for PrintOptions {
    fn default() -> Self {
        Self {
            orientation: "auto".to_string(),
            font_pt: 11,
            margin_mm: 12,
            show_company: true,
            show_printed_at: true,
            show_page_no: true,
            show_signatures: true,
            sign_line: "制表：　　　　　审核：　　　　　负责人：".to_string(),
            offset_x_mm: 0,
            offset_y_mm: 0,
            voucher_paper: "A4".to_string(),
            ledger_paper: "A4".to_string(),
            doc_paper: String::new(),
        }
    }
}

impl PrintOptions {
    /// 实际是否横向打印；`cols` 是这张表的列数（用于 `auto`）
    pub fn landscape(&self, cols: usize) -> bool {
        match self.orientation.as_str() {
            "landscape" => true,
            "portrait" => false,
            // 认不出来的值按 auto 走：脏配置不该让打印直接报错
            _ => cols > AUTO_LANDSCAPE_COLS,
        }
    }

    pub fn orientation_label(&self) -> &'static str {
        match self.orientation.as_str() {
            "landscape" => "横向",
            "portrait" => "纵向",
            _ => "自动",
        }
    }

    /// 结构校验（不查库）：返回空表示通过。
    ///
    /// 只挡「一定是错的」：范围越界、方向拼不出来。至于科目存不存在、是不是末级，
    /// 那是查库才能判断的语义校验，放在 findb 层（见 `findb::accounts::check_biz_accounts`）。
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !matches!(self.orientation.as_str(), "auto" | "portrait" | "landscape") {
            out.push(format!(
                "打印方向只能是 自动/纵向/横向，当前是「{}」",
                self.orientation
            ));
        }
        if !(9..=14).contains(&self.font_pt) {
            out.push(format!("打印字号应在 9–14 之间，当前是 {}", self.font_pt));
        }
        if !(5..=30).contains(&self.margin_mm) {
            out.push(format!(
                "页边距应在 5–30 毫米之间，当前是 {}",
                self.margin_mm
            ));
        }
        if self.show_signatures && self.sign_line.trim().is_empty() {
            out.push("选了显示落款，但落款文字是空的".to_string());
        }
        if !(-20..=20).contains(&self.offset_x_mm) {
            out.push(format!(
                "套打横向偏移应在 −20 到 20 毫米之间，当前是 {}",
                self.offset_x_mm
            ));
        }
        if !(-20..=20).contains(&self.offset_y_mm) {
            out.push(format!(
                "套打纵向偏移应在 −20 到 20 毫米之间，当前是 {}",
                self.offset_y_mm
            ));
        }
        for (label, value, allow_empty) in [
            ("凭证", &self.voucher_paper, false),
            ("账簿", &self.ledger_paper, false),
            ("单据", &self.doc_paper, true),
        ] {
            if value.trim().is_empty() && allow_empty {
                continue;
            }
            if paper_size_css(value, true).is_none() {
                out.push(format!(
                    "{label}套打纸型无法识别：「{value}」—— 可选 A4 / A5 / third / 241x140"
                ));
            }
        }
        out
    }

    /// 某种用途的实际纸型值（已兜底：凭证/账簿空值按 A4，单据空值返回空＝跟随模板）
    pub fn paper_of(&self, use_: PaperUse) -> &str {
        match use_ {
            PaperUse::Voucher => {
                if self.voucher_paper.trim().is_empty() {
                    "A4"
                } else {
                    &self.voucher_paper
                }
            }
            PaperUse::Ledger => {
                if self.ledger_paper.trim().is_empty() {
                    "A4"
                } else {
                    &self.ledger_paper
                }
            }
            PaperUse::Doc => &self.doc_paper,
        }
    }

    /// 某种用途的 `@page size` 片段；认不出来时按 A4 横向兜底。
    pub fn paper_css(&self, use_: PaperUse) -> String {
        paper_size_css(self.paper_of(use_), true)
            .or_else(|| paper_size_css("A4", true))
            .unwrap_or_else(|| "A4 landscape".to_string())
    }

    /// 只生成 `@page`（纸张 + 四边页边距 + 页码），**不含字号**。
    ///
    /// 套打页（凭证/账簿/单据）自己有以 px 计的排版（预印纸的格子是固定的），
    /// 被 `font-size:Npt` 一冲就会溢格。所以纸张规则与字号规则分开：
    /// 报表用 [`Self::page_css`]（两者都要），套打只取这一份。
    ///
    /// `base_margin_mm` 是这类纸的**基础**页边距（报表用设置里的 `margin_mm`，
    /// 套打用纸型自带的值），套打偏移在此基础上加减。
    pub fn page_box_css(&self, size: &str, base_margin_mm: u8) -> String {
        let base = base_margin_mm as i32;
        // 偏移是「内容整体往右下推」，等价于左/上边距变大、右/下变小。
        // 减到负数会被浏览器当成非法值整条忽略（不是变成 0），所以先夹住。
        let l = (base + self.offset_x_mm as i32).max(0);
        let r = (base - self.offset_x_mm as i32).max(0);
        let t = (base + self.offset_y_mm as i32).max(0);
        let b = (base - self.offset_y_mm as i32).max(0);
        let mut css = format!("@page{{size:{size};margin:{t}mm {r}mm {b}mm {l}mm;");
        if self.show_page_no {
            css.push_str(
                "@bottom-right{content:\"第 \" counter(page) \" 页 / 共 \" counter(pages) \" 页\";\
                 font-size:9pt;color:#666;}",
            );
        }
        css.push('}');
        css
    }

    /// 生成 `@page` 与基础排版 CSS（两端共用），供**报表**打印使用。
    ///
    /// 页码用 `@page` 的页边距盒子（`@bottom-right`）实现：这是浏览器里唯一能拿到
    /// `counter(page)` 的地方。旧浏览器（Chrome < 131 等）不认识页边距盒子，
    /// 会**整块忽略**而不报错 —— 表现为「设置里勾了页码但纸上没有」，所以界面上
    /// 的说明文字要讲清楚这个前提，别让人以为是功能没做。
    pub fn page_css(&self, cols: usize) -> String {
        let size = if self.landscape(cols) {
            "A4 landscape"
        } else {
            "A4 portrait"
        };
        format!(
            "{}{}",
            self.page_box_css(size, self.margin_mm),
            format!(
                "body{{font-size:{pt}pt;}}\
                 table{{border-collapse:collapse;width:100%;font-size:{pt}pt;}}\
                 thead{{display:table-header-group;}}\
                 tr,td,th{{break-inside:avoid;page-break-inside:avoid;}}",
                pt = self.font_pt
            )
        )
    }

    /// 落款 HTML；关闭时返回空串（调用方直接拼接即可，不会多出空行）
    pub fn signature_html(&self) -> String {
        if !self.show_signatures {
            return String::new();
        }
        format!(
            "<div class='sig'>{}</div>",
            crate::print::esc(&self.sign_line)
        )
    }
}

/// HTML 转义（打印内容里有客户名、摘要，可能带 `<` 和 `&`）
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 这一列是不是「金额列」——决定打印时的对齐方式。
///
/// 原来的写法是「最后一列右对齐」，那是个把报表结构硬编码进样式的假设：凭证清单、
/// 明细账、档案列表的最后一列都是备注或摘要，结果是**整列文字被推到右边**。
/// 改成按内容判断：非空单元格里有九成以上能解析成金额，才算金额列。
pub fn is_money_column<S: AsRef<str>>(cells: impl Iterator<Item = S>) -> bool {
    let mut total = 0usize;
    let mut numeric = 0usize;
    for c in cells {
        let t = c.as_ref().trim();
        if t.is_empty() {
            continue;
        }
        total += 1;
        if is_number_like(t) {
            numeric += 1;
        }
    }
    total > 0 && numeric * 10 >= total * 9
}

/// 单个单元格是否金钱样式：允许千分位、正负号、会计括号、货币符号。
pub fn is_number_like(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    let t = t
        .trim_start_matches(['¥', '￥', '$'])
        .trim_start_matches('+')
        .trim();
    let t = if let Some(inner) = t.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
        inner.trim()
    } else {
        t
    };
    let t = t.strip_prefix('-').unwrap_or(t).trim();
    if t.is_empty() {
        return false;
    }
    let mut seen_dot = false;
    for ch in t.chars() {
        match ch {
            ',' | ' ' => {}
            '.' if !seen_dot => seen_dot = true,
            c if c.is_ascii_digit() => {}
            _ => return false,
        }
    }
    // 至少要有一位数字，否则「...」也会被当成金额
    t.chars().any(|c| c.is_ascii_digit())
}

/// 打印版式的**局部**改动。
///
/// 预览页上「点一下方向、拖一下字号」发的是这个，而不是整个 [`PrintOptions`]：
/// 预览页只认识其中几个字段，整对象覆盖会把它不认识的设置（凭证/账簿/单据纸型、
/// 套打偏移）一起按默认值冲掉 —— 而且因为都带 `#[serde(default)]`，这种破坏
/// 一声不响，等下次打凭证才发现纸型没了。
///
/// 字段全是 `Option`：`None` = 「这次没动它」。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_pt: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin_mm: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_x_mm: Option<i16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_y_mm: Option<i16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_company: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_printed_at: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_page_no: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_signatures: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_line: Option<String>,
}

impl PrintOptions {
    /// 把一处局部改动合进来；`None` 的字段原样保留。
    ///
    /// 这里**不做范围检查** —— 非法值要留给 [`PrintOptions::problems`] 在保存时
    /// 报出来。悄悄夹回合法范围看着「更稳」，但用户明明填了 20pt 却打出 14pt，
    /// 而且没有任何提示，只会让人以为设置没生效。
    pub fn apply_patch(&mut self, p: &PrintPatch) {
        if let Some(v) = &p.orientation {
            self.orientation = v.trim().to_string();
        }
        if let Some(v) = p.font_pt {
            self.font_pt = v;
        }
        if let Some(v) = p.margin_mm {
            self.margin_mm = v;
        }
        if let Some(v) = p.offset_x_mm {
            self.offset_x_mm = v;
        }
        if let Some(v) = p.offset_y_mm {
            self.offset_y_mm = v;
        }
        if let Some(v) = p.show_company {
            self.show_company = v;
        }
        if let Some(v) = p.show_printed_at {
            self.show_printed_at = v;
        }
        if let Some(v) = p.show_page_no {
            self.show_page_no = v;
        }
        if let Some(v) = p.show_signatures {
            self.show_signatures = v;
        }
        if let Some(v) = &p.sign_line {
            self.sign_line = v.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 局部改动只动它带来的字段：预览页不认识的设置（纸型）必须原样留着。
    #[test]
    fn patch_only_touches_what_it_carries() {
        let mut po = PrintOptions {
            voucher_paper: "241x140".into(),
            ledger_paper: "A5".into(),
            offset_x_mm: -3,
            ..PrintOptions::default()
        };
        let patch = PrintPatch {
            font_pt: Some(13),
            orientation: Some("landscape".into()),
            ..PrintPatch::default()
        };
        po.apply_patch(&patch);
        assert_eq!(po.font_pt, 13);
        assert_eq!(po.orientation, "landscape");
        // 这三个一个都没在 patch 里
        assert_eq!(po.voucher_paper, "241x140");
        assert_eq!(po.ledger_paper, "A5");
        assert_eq!(po.offset_x_mm, -3);
        assert_eq!(po.margin_mm, PrintOptions::default().margin_mm);
    }

    /// 越界的值要留下来让保存报错，不能被悄悄夹回去。
    #[test]
    fn patch_keeps_out_of_range_values_so_saving_can_complain() {
        let mut po = PrintOptions::default();
        po.apply_patch(&PrintPatch {
            font_pt: Some(20),
            ..PrintPatch::default()
        });
        assert_eq!(po.font_pt, 20);
        let p = po.problems();
        assert!(p.iter().any(|m| m.contains("20")), "{p:?}");
    }

    /// 空 patch 等于什么都没发生——这是「页面只改了勾选框」时的常见形态。
    #[test]
    fn empty_patch_changes_nothing() {
        let before = PrintOptions::default();
        let mut after = before.clone();
        after.apply_patch(&PrintPatch::default());
        assert_eq!(before, after);
    }

    #[test]
    fn default_is_auto_and_sane() {
        let p = PrintOptions::default();
        assert_eq!(p.orientation_label(), "自动");
        assert!(p.problems().is_empty(), "{:?}", p.problems());
        assert!(!p.landscape(6), "6 列在 A4 纵向放得下");
        assert!(p.landscape(9), "9 列自动改横向");
    }

    #[test]
    fn explicit_orientation_wins_over_column_count() {
        let mut p = PrintOptions::default();
        p.orientation = "portrait".into();
        assert!(!p.landscape(30), "显式选纵向就该听用户的");
        p.orientation = "landscape".into();
        assert!(p.landscape(2));
    }

    #[test]
    fn page_css_carries_size_margin_font_and_break_rules() {
        let p = PrintOptions::default();
        let css = p.page_css(4);
        assert!(css.contains("size:A4 portrait"), "{css}");
        assert!(css.contains("margin:12mm"), "{css}");
        assert!(css.contains("font-size:11pt"), "{css}");
        // 跨页时行不能被切成两半，表头要重复
        assert!(css.contains("break-inside:avoid"), "{css}");
        assert!(css.contains("display:table-header-group"), "{css}");
    }

    #[test]
    fn page_number_can_be_turned_off() {
        let mut p = PrintOptions::default();
        assert!(p.page_css(3).contains("counter(page)"));
        p.show_page_no = false;
        assert!(
            !p.page_css(3).contains("counter(page)"),
            "关掉页码后不能再输出页边距盒子"
        );
    }

    #[test]
    fn signature_respects_toggle_and_escapes() {
        let mut p = PrintOptions::default();
        assert!(p.signature_html().contains("制表"));
        p.show_signatures = false;
        assert_eq!(p.signature_html(), "");
        p.show_signatures = true;
        p.sign_line = "<b>制表</b>".into();
        assert!(
            p.signature_html().contains("&lt;b&gt;"),
            "落款里的字符要转义"
        );
    }

    #[test]
    fn paper_offset_shifts_the_printable_area() {
        let mut p = PrintOptions::default();
        p.margin_mm = 12;
        p.offset_x_mm = 3;
        p.offset_y_mm = -2;
        let css = p.page_box_css("A4 landscape", 12);
        // 上右下左：向右推 3mm → 左 15、右 9；向上推 2mm → 上 10、下 14
        assert!(css.contains("margin:10mm 9mm 14mm 15mm"), "{css}");
    }

    #[test]
    fn paper_offset_never_produces_a_negative_margin() {
        let mut p = PrintOptions::default();
        p.offset_x_mm = -20;
        let css = p.page_box_css("A4", 10);
        // 负数边距会被浏览器整条丢弃（不是当 0），所以必须夹住
        assert!(!css.contains("--"), "{css}");
        assert!(css.contains("margin:10mm 30mm 10mm 0mm"), "{css}");
    }

    #[test]
    fn page_box_has_no_font_rules_so_taoda_keeps_its_grid() {
        let p = PrintOptions::default();
        let box_css = p.page_box_css("A4 landscape", 10);
        assert!(
            !box_css.contains("font-size:1"),
            "套打纸型不该被正文字号冲掉：{box_css}"
        );
        assert!(p.page_css(4).contains("font-size:11pt"), "报表才需要字号");
    }

    #[test]
    fn out_of_range_settings_are_reported_with_the_value() {
        let mut p = PrintOptions::default();
        p.font_pt = 30;
        p.margin_mm = 1;
        p.orientation = "sideways".into();
        p.offset_y_mm = 40;
        let probs = p.problems();
        assert!(probs.iter().any(|x| x.contains("30")), "{probs:?}");
        assert!(probs.iter().any(|x| x.contains("当前是 1")), "{probs:?}");
        assert!(probs.iter().any(|x| x.contains("sideways")), "{probs:?}");
        assert!(
            probs.iter().any(|x| x.contains("40")),
            "偏移越界也要报出来：{probs:?}"
        );
    }

    #[test]
    fn money_column_is_detected_by_content_not_position() {
        // 备注列：最后一列但全是文字 → 不能右对齐
        let notes = ["月结 30 天", "现结", ""];
        assert!(!is_money_column(notes.into_iter().map(|s| s.to_string())));
        // 金额列：带千分位与括号负数
        let money = ["1,234.56", "(1,000.00)", "0.00"];
        assert!(is_money_column(money.into_iter().map(|s| s.to_string())));
    }

    #[test]
    fn number_like_accepts_accounting_forms() {
        for s in ["1,234.56", "-5", "(1,000.00)", "¥88.00", "0.00", "12 "] {
            assert!(is_number_like(s), "{s} 应被认成金额");
        }
        for s in ["", "  ", "月结", "...", "12,34,5x", "2026-10-26"] {
            assert!(!is_number_like(s), "{s} 不该被认成金额");
        }
    }
}
