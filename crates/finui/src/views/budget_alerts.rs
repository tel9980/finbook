//! 预算预警：执行率超过阈值的科目
//!
//! 数据层 `findb::mgmt::budget_alerts` 已就绪，这里只做展示。

use egui::{RichText, Ui};
use findb::mgmt::{self, BudgetAlert};
use fincore::Period;

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

pub struct BudgetAlertsView {
    pub period_text: String,
    pub from_text: String,
    pub threshold: String,
    pub rows: Vec<BudgetAlert>,
    pub err: String,
    pub dirty: bool,
    key: String,
}

impl Default for BudgetAlertsView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            from_text: String::new(),
            threshold: "90".to_string(),
            rows: Vec::new(),
            err: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl BudgetAlertsView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        if self.from_text.is_empty() {
            self.from_text = format!("{}01", ctx.period().year());
        }
        self.dirty = true;
    }

    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }

    fn from_period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.from_text).unwrap_or_else(|_| self.period(ctx))
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let key = format!("{}|{}|{}", self.period_text, self.from_text, self.threshold);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        let p = self.period(ctx);
        let from = self.from_period(ctx);
        let thr = self.threshold.trim().parse::<i64>().unwrap_or(90);
        self.rows = mgmt::budget_alerts(ctx.db(), p, from, thr).unwrap_or_default();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "预算预警", |ui| {
            ui.label(RichText::new("执行率达到阈值的科目，按超支额降序").weak());
        });

        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            ui.label("起始期间");
            let r2 = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.from_text));
            if r2.changed() {
                self.dirty = true;
            }
            ui.label("阈值(%)");
            let r3 = ui.add_sized([60.0, 22.0], egui::TextEdit::singleline(&mut self.threshold));
            if r3.changed() {
                self.dirty = true;
            }
            if ui.button("查询").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                let mut sh = crate::views::export::Sheet::new(
                    "预算预警",
                    vec!["科目".to_string(), "部门".to_string(), "预算数".to_string(), "实际数".to_string(), "执行率(%)".to_string(), "超支额".to_string()],
                );
                for a in &self.rows {
                    sh.push(vec![
                        format!("{} {}", a.account_code, a.account_name),
                        if a.dept.is_empty() { "—".to_string() } else { a.dept.clone() },
                        a.budget.fmt_plain(),
                        a.actual.fmt_plain(),
                        a.rate.fmt_qty(),
                        a.over_amount.fmt_plain(),
                    ]);
                }
                match crate::views::export::run_export(&sh, "预算预警", "预算预警", mode) {
                    Ok(m) => ctx.info(m),
                    Err(e) => ctx.error(e),
                }
            }
        });

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, &self.err);
        }

        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("科目", 200.0),
            widgets::TCol::new("部门", 120.0),
            widgets::TCol::new("预算数", 130.0).right(),
            widgets::TCol::new("实际数", 130.0).right(),
            widgets::TCol::new("执行率(%)", 100.0).right(),
            widgets::TCol::new("超支额", 130.0).right(),
        ];
        widgets::grid(ui, "budget_alerts", &cols, rows.len(), 24.0, |i, c, ui| {
            let a = &rows[i];
            match c {
                0 => {
                    ui.label(format!("{} {}", a.account_code, a.account_name));
                }
                1 => {
                    ui.label(if a.dept.is_empty() { "—".to_string() } else { a.dept.clone() });
                }
                2 => widgets::amount_label(ui, a.budget),
                3 => widgets::amount_label(ui, a.actual),
                4 => {
                    ui.label(
                        RichText::new(a.rate.fmt_qty())
                            .color(if a.rate.to_f64() >= 100.0 { palette::CREDIT } else { palette::WARN }),
                    );
                }
                5 => widgets::amount_label(ui, a.over_amount),
                _ => {}
            }
        });

        if !rows.is_empty() {
            ui.separator();
            ui.label(
                RichText::new(format!(
                    "共 {} 个预警科目；执行率 = 实际数 ÷ 预算数（绝对值），阈值 {}%",
                    rows.len(),
                    self.threshold
                ))
                .weak(),
            );
        }
    }
}
