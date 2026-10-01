//! 主题与字体
//!
//! egui 内置字体不含中日韩字形，不加载中文字体的话界面会显示成一片"豆腐块"。
//! 这里按平台依次探测常见中文字体，找到就注入；都找不到时退化为内置字体并在状态栏给出提示。

use std::sync::Arc;

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, Visuals};

/// 应用配色。财务软件用低饱和的蓝灰，长时间看不刺眼。
pub mod palette {
    use egui::Color32;

    /// 主色：沉稳蓝
    pub const PRIMARY: Color32 = Color32::from_rgb(30, 90, 160);
    /// 主色（hover）
    pub const PRIMARY_HOVER: Color32 = Color32::from_rgb(24, 74, 133);
    /// 借方/正数
    pub const DEBIT: Color32 = Color32::from_rgb(20, 20, 20);
    /// 贷方/负数：财务惯例用红色
    pub const CREDIT: Color32 = Color32::from_rgb(200, 40, 40);
    /// 警示
    pub const WARN: Color32 = Color32::from_rgb(214, 138, 20);
    /// 成功
    pub const OK: Color32 = Color32::from_rgb(30, 140, 80);
    /// 表头底色
    pub const HEADER_BG: Color32 = Color32::from_rgb(240, 243, 247);
    /// 小计行底色
    pub const SUBTOTAL_BG: Color32 = Color32::from_rgb(248, 250, 252);
    /// 合计行底色
    pub const TOTAL_BG: Color32 = Color32::from_rgb(232, 238, 246);
    /// 选中行底色
    pub const SELECTED_BG: Color32 = Color32::from_rgb(214, 231, 248);
    /// 侧边栏底色
    pub const SIDEBAR_BG: Color32 = Color32::from_rgb(38, 46, 58);
    /// 侧边栏文字
    pub const SIDEBAR_FG: Color32 = Color32::from_rgb(214, 220, 229);
    /// 侧边栏选中
    pub const SIDEBAR_ACTIVE: Color32 = Color32::from_rgb(30, 90, 160);
    /// 网格线
    pub const GRID: Color32 = Color32::from_rgb(214, 219, 226);
}

/// 已加载的中文字体名（用于状态栏提示）
pub static FONT_STATUS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 初始化字体与样式
pub fn setup(ctx: &Context) {
    let mut fonts = FontDefinitions::default();

    if let Some((name, bytes)) = load_cjk_font() {
        fonts
            .font_data
            .insert("cjk".to_owned(), Arc::new(FontData::from_owned(bytes)));
        // 放在首位，优先用中文字体渲染
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "cjk".to_owned());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "cjk".to_owned());
        if let Ok(mut g) = FONT_STATUS.lock() {
            *g = Some(name);
        }
    }

    ctx.set_fonts(fonts);
    apply_style(ctx);
}

/// 应用视觉样式
pub fn apply_style(ctx: &Context) {
    let mut visuals = Visuals::light();
    visuals.override_text_color = None;
    visuals.hyperlink_color = palette::PRIMARY;
    visuals.selection.bg_fill = palette::SELECTED_BG;
    visuals.selection.stroke.color = palette::PRIMARY;
    visuals.widgets.active.bg_fill = palette::PRIMARY;
    visuals.widgets.hovered.bg_fill = palette::PRIMARY_HOVER;
    visuals.widgets.noninteractive.bg_fill = Color32::WHITE;
    visuals.faint_bg_color = palette::HEADER_BG;
    visuals.extreme_bg_color = Color32::from_rgb(250, 251, 253);
    visuals.panel_fill = Color32::WHITE;
    visuals.window_fill = Color32::WHITE;
    visuals.striped = true;

    ctx.style_mut(|s| {
        s.visuals = visuals;
        // 财务界面信息密度高，控件收紧一点
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
        // 字号分级：标题醒目、正文易读、辅助小字紧凑，避免"满屏一个号"
        use egui::TextStyle;
        let sizes = [
            (TextStyle::Heading, 20.0),
            (TextStyle::Body, 14.0),
            (TextStyle::Button, 14.0),
            (TextStyle::Small, 12.0),
            (TextStyle::Monospace, 13.0),
        ];
        for (t, size) in sizes {
            if let Some(id) = s.text_styles.get_mut(&t) {
                id.size = size;
            }
        }
        // 交互命中区高度抬高，按钮/输入框更易点中
        s.spacing.interact_size.y = 26.0;
    });
}

/// 按平台探测中文字体
fn load_cjk_font() -> Option<(String, Vec<u8>)> {
    let candidates: Vec<(&str, String)> = if cfg!(windows) {
        vec![
            ("微软雅黑", "C:\\Windows\\Fonts\\msyh.ttc".to_string()),
            ("微软雅黑 Bold", "C:\\Windows\\Fonts\\msyhbd.ttc".to_string()),
            ("黑体", "C:\\Windows\\Fonts\\simhei.ttf".to_string()),
            ("宋体", "C:\\Windows\\Fonts\\simsun.ttc".to_string()),
            ("等线", "C:\\Windows\\Fonts\\Deng.ttf".to_string()),
            ("楷体", "C:\\Windows\\Fonts\\simkai.ttf".to_string()),
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            ("苹方", "/System/Library/Fonts/PingFang.ttc".to_string()),
            (
                "Heiti SC",
                "/System/Library/Fonts/STHeiti Medium.ttc".to_string(),
            ),
            (
                "Songti SC",
                "/Library/Fonts/Arial Unicode.ttf".to_string(),
            ),
        ]
    } else {
        vec![
            (
                "Noto Sans CJK",
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".to_string(),
            ),
            (
                "Noto Sans CJK",
                "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf".to_string(),
            ),
            (
                "文泉驿正黑",
                "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc".to_string(),
            ),
            (
                "文泉驿微米黑",
                "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".to_string(),
            ),
            (
                "Noto Sans SC",
                "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".to_string(),
            ),
            (
                "Source Han Sans",
                "/usr/share/fonts/opentype/source-han-sans/SourceHanSansSC-Regular.otf"
                    .to_string(),
            ),
        ]
    };

    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(&path) {
            if !bytes.is_empty() {
                return Some((format!("{name}（{path}）"), bytes));
            }
        }
    }
    None
}

/// 金额文本颜色：负数为红字
pub fn amount_color(v: fincore::Money) -> Color32 {
    if v.is_negative() {
        palette::CREDIT
    } else {
        palette::DEBIT
    }
}

/// 状态色
pub fn status_color(ok: bool) -> Color32 {
    if ok {
        palette::OK
    } else {
        palette::CREDIT
    }
}
