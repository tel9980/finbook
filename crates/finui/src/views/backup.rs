//! 备份与恢复

use std::path::PathBuf;

use egui::{RichText, Ui};
use findb::BOOK_EXT;

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

pub struct BackupView {
    pub check: Option<Vec<String>>,
    pub msg: String,
    /// 自动备份保留份数（QSpinBox 风格输入）
    pub keep: u32,
}

impl Default for BackupView {
    fn default() -> Self {
        Self {
            check: None,
            msg: String::new(),
            keep: 10,
        }
    }
}

impl BackupView {
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::page_header(ui, "备份与恢复", |ui| {
            ui.label(
                RichText::new("一个账套就是一个文件，备份即复制，恢复即覆盖")
                    .weak(),
            );
        });

        // 备份/恢复是数据外带通道，仅系统管理员可用。
        // 普通账户（含财务主管）看不到任何备份按钮，无法获取账套文件副本。
        if !ctx.user().can(fincore::Perm::Backup) {
            ui.colored_label(
                palette::WARN,
                "备份与恢复仅限系统管理员。普通账户无法获取账套文件副本，如需备份请联系管理员。",
            );
            return;
        }

        widgets::card(ui, "备份", |ui| {
            ui.label("把当前账套完整另存为一份新文件（内含全部科目、凭证、期初与用户）。");
            ui.horizontal(|ui| {
                if ui.button("备份到…").clicked() && ctx.can(fincore::Perm::Backup) {
                    self.do_backup(ctx);
                }
                if ui.button("备份到同目录").clicked() && ctx.can(fincore::Perm::Backup) {
                    self.quick_backup(ctx);
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("自动备份（保留 N 份）").clicked() && ctx.can(fincore::Perm::Backup) {
                    self.auto_backup(ctx);
                }
                ui.label("保留份数");
                let r = ui.add(egui::DragValue::new(&mut self.keep).range(1..=99));
                if r.changed() {
                    // 就地生效，无需额外确认
                }
                ui.label(RichText::new("自动备份写入账套同目录 auto_*.fbk，超出份数自动删除最旧").weak());
            });
        });

        widgets::card(ui, "恢复", |ui| {
            ui.colored_label(
                palette::WARN,
                "恢复会用备份文件整体覆盖当前账套，当前账套中的数据将全部丢失。",
            );
            if ui.button("从备份文件恢复…").clicked() && ctx.can(fincore::Perm::Backup) {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("FinBook 账套", &[BOOK_EXT])
                    .pick_file()
                {
                    ctx.confirm_dangerous(
                        "恢复账套",
                        &format!(
                            "将用\n{}\n覆盖当前账套，当前数据不可恢复。确定继续吗？",
                            p.display()
                        ),
                        ConfirmAction::RestoreBook(p),
                        true,
                    );
                }
            }
        });

        widgets::card(ui, "维护", |ui| {
            ui.horizontal(|ui| {
                if ui.button("整理数据库").clicked() {
                    let r = ctx.db().vacuum();
                    if ctx.handle(r).is_some() {
                        ctx.info("已整理，文件体积已优化");
                    }
                }
                if ui.button("完整性检查").clicked() {
                    match ctx.db().integrity_check() {
                        Ok(v) => self.check = Some(v),
                        Err(e) => ctx.error(e.to_string()),
                    }
                }
            });
            if let Some(v) = &self.check {
                ui.add_space(6.0);
                if v.len() == 1 && v[0].eq_ignore_ascii_case("ok") {
                    ui.label(RichText::new("✔ 数据库完整性检查通过").color(palette::OK));
                } else {
                    for line in v {
                        ui.colored_label(palette::CREDIT, line);
                    }
                }
            }
        });

        widgets::card(ui, "账套概况", |ui| {
            let (v, e, a) = ctx.db().stats().unwrap_or((0, 0, 0));
            widgets::kv(ui, "凭证", &v.to_string());
            widgets::kv(ui, "分录", &e.to_string());
            widgets::kv(ui, "科目", &a.to_string());
            if let Some(p) = &ctx.st.book_path {
                widgets::kv(ui, "当前文件", &p.display().to_string());
                if let Ok(meta) = std::fs::metadata(p) {
                    widgets::kv(ui, "文件大小", &format!("{} KB", meta.len() / 1024));
                }
            }
        });

        if !self.msg.is_empty() {
            ui.colored_label(palette::OK, &self.msg);
        }
    }

    fn default_name(&self, ctx: &AppCtx<'_>) -> String {
        let p = ctx.period();
        let company = ctx.db().options().company;
        let base = if company.is_empty() {
            "账套".to_string()
        } else {
            company
        };
        format!("{}_{}{:02}备份", base, p.year(), p.month())
    }

    fn do_backup(&mut self, ctx: &mut AppCtx<'_>) {
        let name = self.default_name(ctx);
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("FinBook 账套", &[BOOK_EXT])
            .set_file_name(&format!("{name}.{BOOK_EXT}"))
            .save_file()
        {
            self.run_backup(ctx, p);
        }
    }

    fn quick_backup(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(cur) = ctx.st.book_path.clone() else {
            ctx.error("当前账套没有文件路径，请使用「备份到…」");
            return;
        };
        let name = self.default_name(ctx);
        let p = cur.with_file_name(format!("{name}.{BOOK_EXT}"));
        self.run_backup(ctx, p);
    }

    /// 自动备份到账套同目录，带保留份数轮转
    fn auto_backup(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(cur) = ctx.st.book_path.clone() else {
            ctx.error("当前账套没有文件路径，请使用「备份到…」");
            return;
        };
        let dir = cur
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let keep = self.keep.max(1) as usize;
        let r = ctx.db().backup_auto(&dir, keep);
        match r {
            Ok(p) => {
                let msg = format!("已自动备份到 {}（保留最近 {keep} 份）", p.display());
                ctx.log("账套", "自动备份", &p.display().to_string());
                ctx.info(&msg);
                self.msg = msg;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn run_backup(&mut self, ctx: &mut AppCtx<'_>, p: PathBuf) {
        let r = ctx.db().backup(&p);
        if ctx.handle(r).is_some() {
            let msg = format!("已备份到 {}", p.display());
            ctx.log("账套", "备份", &p.display().to_string());
            ctx.info(&msg);
            self.msg = msg;
        }
    }
}
