//! 操作日志

use egui::{RichText, Ui};
use fincore::user::AuditLog;

use crate::state::AppCtx;
use crate::widgets::{self, Paging};

pub struct LogsView {
    pub kw: String,
    pub rows: Vec<AuditLog>,
    pub paging: Paging,
    pub dirty: bool,
}

impl Default for LogsView {
    fn default() -> Self {
        Self {
            kw: String::new(),
            rows: Vec::new(),
            paging: Paging::default(),
            dirty: true,
        }
    }
}

impl LogsView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.paging.reset();
        self.rows = if self.kw.trim().is_empty() {
            ctx.db().recent_logs(2000).unwrap_or_default()
        } else {
            ctx.db()
                .search_logs(self.kw.trim(), 2000)
                .unwrap_or_default()
        };
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "操作日志", |ui| {
            ui.label(RichText::new(format!("共 {} 条", self.rows.len())).weak());
        });

        widgets::toolbar(ui, |ui| {
            ui.label("关键字");
            let r = ui.add_sized(
                [200.0, 22.0],
                egui::TextEdit::singleline(&mut self.kw).hint_text("用户 / 模块 / 动作 / 明细"),
            );
            if r.changed() || ui.button("查询").clicked() {
                self.dirty = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        let page = self.paging.slice(&self.rows).to_vec();
        let cols = [
            widgets::TCol::new("时间", 165.0).fixed(),
            widgets::TCol::new("用户", 100.0).fixed(),
            widgets::TCol::new("模块", 100.0).fixed(),
            widgets::TCol::new("动作", 120.0).fixed(),
            widgets::TCol::new("明细", 420.0),
        ];
        widgets::grid(ui, "logs", &cols, page.len(), 24.0, |i, c, ui| {
            let l = &page[i];
            match c {
                0 => { ui.label(RichText::new(&l.ts).weak()); }
                1 => { ui.label(&l.user); }
                2 => { ui.label(&l.module); }
                3 => { ui.label(&l.action); }
                4 => { ui.label(&l.detail); }
                _ => {}
            }
        });
        ui.separator();
        self.paging.bar(ui, self.rows.len());
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        if self.rows.is_empty() {
            ctx.error("没有可导出的数据");
            return;
        }
        let mut sh = crate::views::export::Sheet::new(
            "操作日志",
            vec![
                "时间".into(),
                "用户".into(),
                "模块".into(),
                "动作".into(),
                "明细".into(),
            ],
        );
        for l in &self.rows {
            sh.push(vec![
                l.ts.clone(),
                l.user.clone(),
                l.module.clone(),
                l.action.clone(),
                l.detail.clone(),
            ]);
        }
        match crate::views::export::run_export(&sh, "操作日志", "操作日志", mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}
