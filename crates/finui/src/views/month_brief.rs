//! 月度简报：一页纸，零新增录入。
//!
//! 赚了多少、钱在哪、干了多少——十来个数字，老板每月都要看，但原来分散在
//! 利润表、资产负债表、应收应付、生产订单、返工单五六个页面上，凑一遍半小时。
//!
//! 数字全部来自 [`findb::brief::month_brief`]，界面只负责排版与上色。
//! 方向（变大是好事还是坏事）由数据层的 `better_high` 决定，
//! 界面不猜——猜错会把「应收暴涨」标成绿色。

use egui::{RichText, Ui};
use fincore::Period;
use findb::brief::{self, BriefRow};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

pub struct MonthBriefView {
    pub period_text: String,
    pub rows: Vec<BriefRow>,
    pub has_prev: bool,
    pub prev_period: String,
    pub dirty: bool,
    key: String,
}

impl Default for MonthBriefView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            rows: Vec::new(),
            has_prev: false,
            prev_period: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl MonthBriefView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        self.dirty = true;
    }

    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}", p.ymm());
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        match brief::month_brief(ctx.db(), p) {
            Ok(b) => {
                self.has_prev = b.has_prev;
                self.prev_period = b.prev_period.code();
                self.rows = b.rows;
            }
            Err(e) => {
                ctx.error(e.to_string());
                self.rows.clear();
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "月度简报", |ui| {
            ui.label(RichText::new(p.label()).weak());
        });

        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized(
                [84.0, 22.0],
                egui::TextEdit::singleline(&mut self.period_text),
            );
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("上期").clicked() {
                self.period_text = p.prev().code();
                self.dirty = true;
            }
            if ui.button("下期").clicked() {
                self.period_text = p.next().code();
                self.dirty = true;
            }
            ui.separator();
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                let mut sh = crate::views::export::Sheet::new(
                    "月度简报",
                    vec![
                        "项目".to_string(),
                        "本期".to_string(),
                        "上月".to_string(),
                        "增减".to_string(),
                        "增减%".to_string(),
                    ],
                );
                for r in &self.rows {
                    let f = |m: fincore::Money| {
                        if r.is_qty {
                            m.fmt_qty()
                        } else {
                            m.fmt_plain()
                        }
                    };
                    sh.push(vec![
                        r.label.to_string(),
                        f(r.value),
                        r.prev.map(f).unwrap_or_default(),
                        r.delta.map(f).unwrap_or_default(),
                        r.delta_pct.clone().unwrap_or_default(),
                    ]);
                }
                let title = format!("月度简报（{}）", p.label());
                match crate::views::export::run_export(
                    &sh,
                    "月度简报",
                    &title,
                    mode,
                    &ctx.db().options(),
                ) {
                    Ok(m) => ctx.info(m),
                    Err(e) => ctx.error(e),
                }
            }
        });

        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "一页纸看三件事：赚了多少、钱在哪、干了多少。所有数字都从既有单据算出来，不用另外录。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        // 把「分组标题」与「数据行」压成一维，好复用同一张表（列宽才对得齐）。
        // None = 分组标题行。
        let mut lines: Vec<Option<BriefRow>> = Vec::with_capacity(self.rows.len() + 3);
        let mut last_group = "";
        for r in &self.rows {
            if r.group != last_group {
                last_group = r.group;
                lines.push(None);
            }
            lines.push(Some(r.clone()));
        }

        let cols = [
            widgets::TCol::new("项目", 220.0),
            widgets::TCol::new("本期", 150.0).right(),
            widgets::TCol::new("上月", 150.0).right(),
            widgets::TCol::new("增减", 150.0).right(),
            widgets::TCol::new("增减%", 110.0).right(),
        ];
        widgets::grid(ui, "month_brief", &cols, lines.len(), 24.0, |i, c, ui| {
            let Some(r) = lines.get(i).and_then(|x| x.as_ref()) else {
                // 分组标题：只占第一列，其余留空
                if c == 0 {
                    if let Some(Some(next)) = lines.get(i + 1) {
                        ui.label(RichText::new(next.group).strong().color(palette::PRIMARY));
                    }
                }
                return;
            };
            match c {
                0 => {
                    ui.label(r.label);
                }
                1 => {
                    if r.is_qty {
                        ui.label(r.value.fmt_qty());
                    } else {
                        widgets::amount_label(ui, r.value);
                    }
                }
                2 => match r.prev {
                    None => {
                        ui.label(RichText::new("—").weak());
                    }
                    Some(v) => {
                        if r.is_qty {
                            ui.label(v.fmt_qty());
                        } else {
                            widgets::amount_label(ui, v);
                        }
                    }
                },
                3 => match r.delta {
                    None => {
                        ui.label(RichText::new("—").weak());
                    }
                    Some(d) => {
                        if d.is_zero() {
                            ui.label("0");
                        } else {
                            // 变好标绿、变差标红。方向由数据层给，界面不猜。
                            let good = if r.better_high {
                                d.is_positive()
                            } else {
                                d.is_negative()
                            };
                            let color = if good { palette::OK } else { palette::CREDIT };
                            let sign = if d.is_positive() { "+" } else { "" };
                            ui.colored_label(
                                color,
                                format!(
                                    "{sign}{}",
                                    if r.is_qty { d.fmt_qty() } else { d.fmt_money() }
                                ),
                            );
                        }
                    }
                },
                4 => match (&r.delta, &r.delta_pct) {
                    (Some(d), Some(pct)) => {
                        let good = if r.better_high {
                            d.is_positive()
                        } else {
                            d.is_negative()
                        };
                        let color = if good { palette::OK } else { palette::CREDIT };
                        let sign = if d.is_positive() { "+" } else { "" };
                        ui.colored_label(color, format!("{sign}{pct}%"));
                    }
                    _ => {
                        ui.label(RichText::new("—").weak());
                    }
                },
                _ => {}
            }
        });

        if !self.has_prev {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "上月（{}）没有账，同比一栏显示「—」——不是持平，是没有可比数据。",
                    self.prev_period
                ))
                .weak(),
            );
        }
    }
}
