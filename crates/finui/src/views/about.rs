//! 关于

use egui::{RichText, Ui};

use crate::state::AppCtx;
use crate::theme;
use crate::theme::palette;
use crate::widgets;

pub struct AboutView;

impl Default for AboutView {
    fn default() -> Self {
        Self
    }
}

impl AboutView {
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.label(RichText::new("FinBook 财务管理系统").size(26.0).strong());
            ui.label(RichText::new("账套 → 凭证 → 账簿 → 报表，一条链路").weak());
            ui.label(
                RichText::new(format!("版本 {}（内核 {}）", env!("CARGO_PKG_VERSION"), fincore::CORE_VERSION))
                    .weak(),
            );
        });
        ui.add_space(20.0);

        ui.columns(2, |cols| {
            egui::Frame::NONE
                .stroke(egui::Stroke::new(1.0, palette::GRID))
                .corner_radius(6.0)
                .inner_margin(14.0)
                .show(&mut cols[0], |ui| {
                    ui.label(RichText::new("技术实现").strong());
                    ui.separator();
                    for (k, v) in [
                        ("语言", "Rust 2021"),
                        ("界面", "egui / eframe（纯 Rust，无 Web 壳）"),
                        ("数据库", "SQLite 单文件账套（.fbk）"),
                        ("金额", "rust_decimal 定点十进制，全程无浮点"),
                        ("并发", "单线程 GUI 线程内直连数据库，零锁竞争"),
                    ] {
                        widgets::kv(ui, k, v);
                    }
                });

            egui::Frame::NONE
                .stroke(egui::Stroke::new(1.0, palette::GRID))
                .corner_radius(6.0)
                .inner_margin(14.0)
                .show(&mut cols[1], |ui| {
                    ui.label(RichText::new("运行环境").strong());
                    ui.separator();
                    let font = theme::FONT_STATUS
                        .lock()
                        .ok()
                        .and_then(|g| g.clone())
                        .unwrap_or_else(|| "未找到中文字体，界面可能显示方块".to_string());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("中文字体：").weak());
                        if font.starts_with("未找到") {
                            ui.colored_label(palette::CREDIT, font);
                        } else {
                            ui.label(&font);
                        }
                    });
                    widgets::kv(ui, "账套文件", &ctx.st.book_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "—".to_string()));
                    let o = ctx.db().options();
                    widgets::kv(ui, "企业名称", &o.company);
                    widgets::kv(ui, "启用期间", &o.start_period.label());
                    widgets::kv(ui, "本位币", &o.base_currency);
                    if let Some(u) = ctx.st.user.as_ref() {
                        widgets::kv(ui, "当前用户", &format!("{}（{}）", u.display_name, u.role_labels()));
                    }
                });
        });

        ui.add_space(20.0);
        ui.separator();
        ui.label(
            RichText::new(
                "本软件遵循《企业会计准则》的科目体系与报表格式设计：七大类科目、\
                 借贷记账法、凭证审核与记账分离、期末结转损益与结账控制。\n\
                 金额一律以定点十进制运算并保留两位小数，杜绝二进制浮点造成的分位误差。",
            )
            .weak(),
        );
    }
}
