//! 管理员 · 账目总览（只读视角）
//!
//! 管理员的职责是查看账目全貌而不是记账：本页把资产 / 负债 / 权益 / 损益、
//! 营业收入 / 营业成本 / 净利润的逐月走势与内因构成、财务指标集中在一页，
//! 只读展示，不做任何录入。

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui};
use findb::advanced::FinancialAnalysis;
use findb::reports::Overview;
use fincore::{Money, Period};

use crate::state::AppCtx;
use crate::theme;
use crate::theme::palette;
use crate::widgets;

const C_REV: Color32 = Color32::from_rgb(37, 99, 235); // 营业收入 · 蓝
const C_COST: Color32 = Color32::from_rgb(245, 158, 11); // 营业成本 · 橙
const C_NP: Color32 = Color32::from_rgb(22, 163, 74); // 净利润 · 绿
const C_ERR: Color32 = Color32::from_rgb(220, 38, 38); // 异常 · 红
const SUBTLE: Color32 = Color32::from_rgb(120, 130, 145); // 次要文字

pub struct OverviewView {
    pub period_text: String,
    data: Option<Overview>,
    analysis: Option<FinancialAnalysis>,
    dirty: bool,
    key: String,
}

impl Default for OverviewView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            data: None,
            analysis: None,
            dirty: true,
            key: String::new(),
        }
    }
}

impl OverviewView {
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
        let key = format!("{}|{}", p.ymm(), ctx.db().path().display());
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        match findb::reports::overview(ctx.db(), p) {
            Ok(o) => self.data = Some(o),
            Err(e) => {
                ctx.error(e.to_string());
                self.data = None;
            }
        }
        match findb::advanced::financial_analysis(ctx.db(), p, Some(ctx.user())) {
            Ok(a) => self.analysis = Some(a),
            Err(e) => {
                ctx.error(e.to_string());
                self.analysis = None;
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        widgets::page_header(ui, "账目总览", |ui| {
            ui.label(RichText::new("管理员只读视角 · 账目全貌").weak());
        });
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        let Some(o) = self.data.clone() else {
            return;
        };
        let t = &o.totals;

        // ---------------- 财务概况 ----------------
        widgets::card(ui, "财务概况（年初至今）", |ui| {
            ui.columns(4, |cols| {
                stat(&mut cols[0], "资产总额", &t.total_asset);
                stat(&mut cols[1], "负债总额", &t.total_liab);
                stat(&mut cols[2], "所有者权益", &t.equity);
                stat(&mut cols[3], "净利润", &t.net_profit);
            });
            ui.add_space(6.0);
            ui.columns(2, |cols| {
                stat(&mut cols[0], "营业收入", &t.revenue);
                stat(&mut cols[1], "营业成本", &t.cost);
            });
        });

        // ---------------- 财务走势 ----------------
        if let Some(a) = self.analysis.as_ref() {
            widgets::card(ui, "财务走势 · 年初至今累计（收入 / 成本 / 净利润）", |ui| {
                if !a.trend.is_empty() {
                    draw_trend(ui, a);
                } else {
                    ui.label(RichText::new("本年度尚无数据").weak());
                }
            });

            widgets::card(ui, "当月发生额 · 逐月对比", |ui| {
                if !a.trend.is_empty() {
                    draw_bars(ui, a);
                } else {
                    ui.label(RichText::new("本年度尚无数据").weak());
                }
            });

            // ---------------- 内因构成 ----------------
            widgets::card(ui, "指标内因构成（本月 vs 上月）", |ui| {
                ui.columns(3, |cols| {
                    driver_panel(&mut cols[0], "营业收入构成", &a.revenue_drivers, false);
                    driver_panel(&mut cols[1], "营业成本构成", &a.cost_drivers, false);
                    driver_panel(&mut cols[2], "净利润构成", &a.profit_drivers, true);
                });
            });

            // ---------------- 财务状况分析 ----------------
            widgets::card(ui, "财务状况分析", |ui| {
                ui.horizontal_wrapped(|ui| {
                    for r in &a.ratios {
                        ratio_chip(ui, r);
                    }
                });
                if !a.anomaly_notes.is_empty() {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(RichText::new("异常提示").strong().color(C_ERR));
                    for n in a.anomaly_notes.iter().take(8) {
                        ui.label(RichText::new(format!("• {n}")).color(palette::WARN));
                    }
                    if a.anomaly_notes.len() > 8 {
                        ui.label(RichText::new(format!("…共 {} 条", a.anomaly_notes.len())).weak());
                    }
                }
            });
        }

        // ---------------- 业务概况 ----------------
        widgets::card(ui, "业务概况", |ui| {
            ui.horizontal_wrapped(|ui| {
                kv(ui, "公司", &o.company);
                kv(
                    ui,
                    "已结账至",
                    &o.closed_upto
                        .map(|p| p.label())
                        .unwrap_or_else(|| "未结账".to_string()),
                );
            });
            ui.horizontal_wrapped(|ui| {
                kv(ui, "凭证总数（全账套）", &o.vouchers.to_string());
                kv(ui, "分录总数（全账套）", &o.entries.to_string());
                kv(ui, "科目数（全账套）", &o.accounts.to_string());
            });
            ui.horizontal_wrapped(|ui| {
                kv(ui, "当期未记账", &o.unposted.to_string());
                kv(ui, "当期已记账", &o.posted.to_string());
                kv(
                    ui,
                    "进项发票价税合计",
                    &format!("{}（{} 张）", o.invoice_in.0.fmt_money(), o.invoice_in.1),
                );
                kv(
                    ui,
                    "销项发票价税合计",
                    &format!("{}（{} 张）", o.invoice_out.0.fmt_money(), o.invoice_out.1),
                );
            });
        });

        // ---------------- 最近凭证 ----------------
        ui.add_space(6.0);
        ui.label(RichText::new("最近凭证（全账套，只读）").strong());
        let rows = o.recent.len();
        let cols = [
            widgets::TCol::new("期间", 76.0).fixed(),
            widgets::TCol::new("日期", 88.0).fixed(),
            widgets::TCol::new("凭证号", 88.0).fixed(),
            widgets::TCol::new("摘要", 280.0),
            widgets::TCol::new("借方", 110.0).right(),
            widgets::TCol::new("贷方", 110.0).right(),
            widgets::TCol::new("状态", 70.0).fixed(),
            widgets::TCol::new("制单", 80.0).fixed(),
        ];
        widgets::grid(ui, "overview_recent", &cols, rows, 24.0, |i, c, ui| {
            let v = &o.recent[i];
            match c {
                0 => { ui.label(v.period.code()); }
                1 => { ui.label(v.date.format("%Y-%m-%d").to_string()); }
                2 => { ui.label(RichText::new(v.voucher_no()).monospace()); }
                3 => { ui.label(v.first_summary()); }
                4 => { widgets::amount_label(ui, v.debit_total()); }
                5 => { widgets::amount_label(ui, v.credit_total()); }
                6 => {
                    ui.label(
                        RichText::new(v.status.label())
                            .color(theme::status_color(v.status.counts())),
                    );
                }
                7 => { ui.label(&v.prepared_by); }
                _ => {}
            }
        });
    }
}

// ---------------------------------------------------------------------------
// 图表绘制（egui 手绘，无第三方图表库）
// ---------------------------------------------------------------------------

fn f64_of(m: Money) -> f64 {
    m.to_f64()
}

/// 折线图：累计收入 / 成本 / 净利润
fn draw_trend(ui: &mut Ui, a: &FinancialAnalysis) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 200.0), Sense::hover());
    let p = ui.painter_at(rect);
    let plot = plot_rect(rect);
    p.rect_filled(rect, 6.0, palette::SUBTOTAL_BG);

    let labels: Vec<String> = a.trend.iter().map(|t| t.period.month().to_string()).collect();
    let series: [(&str, Color32, Vec<f64>, Vec<bool>); 3] = [
        ("累计营业收入", C_REV, a.trend.iter().map(|t| f64_of(t.cum_revenue)).collect(), a.trend.iter().map(|t| t.anomaly_revenue).collect()),
        ("累计营业成本", C_COST, a.trend.iter().map(|t| f64_of(t.cum_cost)).collect(), a.trend.iter().map(|t| t.anomaly_cost).collect()),
        ("累计净利润", C_NP, a.trend.iter().map(|t| f64_of(t.cum_net_profit)).collect(), a.trend.iter().map(|t| t.anomaly_net_profit).collect()),
    ];
    draw_lines(&p, plot, &labels, &series);
    draw_legend(ui, &series);
}

/// 分组柱状图：当月收入 / 成本 / 净利润
fn draw_bars(ui: &mut Ui, a: &FinancialAnalysis) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 200.0), Sense::hover());
    let p = ui.painter_at(rect);
    let plot = plot_rect(rect);
    p.rect_filled(rect, 6.0, palette::SUBTOTAL_BG);

    let n = a.trend.len();
    let groups: [(&str, Color32, Vec<f64>, Vec<bool>); 3] = [
        ("营业收入", C_REV, a.trend.iter().map(|t| f64_of(t.revenue)).collect(), a.trend.iter().map(|t| t.anomaly_revenue).collect()),
        ("营业成本", C_COST, a.trend.iter().map(|t| f64_of(t.cost)).collect(), a.trend.iter().map(|t| t.anomaly_cost).collect()),
        ("净利润", C_NP, a.trend.iter().map(|t| f64_of(t.net_profit)).collect(), a.trend.iter().map(|t| t.anomaly_net_profit).collect()),
    ];

    let mut min = 0.0f64;
    let mut max = 0.0f64;
    for (_, _, vals, _) in &groups {
        for v in vals {
            min = min.min(*v);
            max = max.max(*v);
        }
    }
    if max == min {
        max = min + 1.0;
    }
    let span = max - min;
    let h = plot.height() as f64;
    let base = plot.bottom() as f64;
    let zero_y = base - ((0.0 - min) / span) * h;

    // 网格与刻度
    for k in 0..=4 {
        let v = min + span * k as f64 / 4.0;
        let gy = base - (v - min) / span * h;
        p.line_segment(
            [Pos2::new(plot.left(), gy as f32), Pos2::new(plot.right(), gy as f32)],
            Stroke::new(1.0, palette::GRID),
        );
        p.text(
            Pos2::new(plot.left() - 4.0, gy as f32),
            Align2::RIGHT_CENTER,
            fmt_k(v),
            FontId::proportional(10.0),
            SUBTLE,
        );
    }

    let slot = plot.width() / n.max(1) as f32;
    let bw = (slot * 0.7 / groups.len() as f32).min(16.0);
    for (gi, (_, color, vals, anoms)) in groups.iter().enumerate() {
        for (i, v) in vals.iter().enumerate() {
            let cx = plot.left() + slot * (i as f32 + 0.5) + (gi as f32 - 1.0) * (bw + 2.0);
            let vy = base - ((*v - min) / span) * h;
            let (top, bottom) = if vy >= zero_y { (vy, zero_y) } else { (zero_y, vy) };
            let r = Rect::from_min_max(
                Pos2::new(cx - bw / 2.0, top as f32),
                Pos2::new(cx + bw / 2.0, bottom as f32),
            );
            p.rect_filled(r, 1.5, *color);
            if anoms[i] {
                let ax = cx;
                let ay = if vy >= zero_y { vy - 8.0 } else { vy + 8.0 };
                p.line_segment(
                    [Pos2::new(ax, ay as f32), Pos2::new(ax + 4.0, (ay + 7.0) as f32)],
                    Stroke::new(1.6, C_ERR),
                );
                p.line_segment(
                    [Pos2::new(ax, ay as f32), Pos2::new(ax - 4.0, (ay + 7.0) as f32)],
                    Stroke::new(1.6, C_ERR),
                );
            }
        }
    }

    // X 轴标签（月份）
    for (i, t) in a.trend.iter().enumerate() {
        let cx = plot.left() + slot * (i as f32 + 0.5);
        p.text(
            Pos2::new(cx, plot.bottom() + 6.0),
            Align2::CENTER_TOP,
            t.period.month().to_string(),
            FontId::proportional(10.0),
            SUBTLE,
        );
    }
    draw_legend(ui, &groups);
}

fn plot_rect(rect: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(rect.left() + 52.0, rect.top() + 10.0),
        Pos2::new(rect.right() - 8.0, rect.bottom() - 22.0),
    )
}

fn draw_lines(
    p: &egui::Painter,
    plot: Rect,
    labels: &[String],
    series: &[(&str, Color32, Vec<f64>, Vec<bool>)],
) {
    let mut min = 0.0f64;
    let mut max = 0.0f64;
    for (_, _, vals, _) in series {
        for v in vals {
            min = min.min(*v);
            max = max.max(*v);
        }
    }
    if max == min {
        max = min + 1.0;
    }
    let span = max - min;
    let n = series[0].2.len().max(1);
    let h = plot.height() as f64;
    let base = plot.bottom() as f64;

    for k in 0..=4 {
        let v = min + span * k as f64 / 4.0;
        let gy = base - (v - min) / span * h;
        p.line_segment(
            [Pos2::new(plot.left(), gy as f32), Pos2::new(plot.right(), gy as f32)],
            Stroke::new(1.0, palette::GRID),
        );
        p.text(
            Pos2::new(plot.left() - 4.0, gy as f32),
            Align2::RIGHT_CENTER,
            fmt_k(v),
            FontId::proportional(10.0),
            SUBTLE,
        );
    }

    let x_at = |i: usize| {
        if n == 1 {
            plot.center().x
        } else {
            plot.left() + plot.width() * i as f32 / (n - 1) as f32
        }
    };
    let y_at = |v: f64| base - (v - min) / span * h;

    for (_, color, vals, anoms) in series {
        for i in 1..vals.len() {
            p.line_segment(
                [
                    Pos2::new(x_at(i - 1), y_at(vals[i - 1]) as f32),
                    Pos2::new(x_at(i), y_at(vals[i]) as f32),
                ],
                Stroke::new(2.0, *color),
            );
        }
        for (i, v) in vals.iter().enumerate() {
            let r = if anoms[i] { 4.5 } else { 2.4 };
            p.circle_filled(Pos2::new(x_at(i), y_at(*v) as f32), r, if anoms[i] { C_ERR } else { *color });
        }
    }

    // X 轴标签
    for (i, l) in labels.iter().enumerate() {
        if labels.len() > 14 && i % 2 != 0 {
            continue;
        }
        p.text(
            Pos2::new(x_at(i), plot.bottom() + 6.0),
            Align2::CENTER_TOP,
            l.clone(),
            FontId::proportional(10.0),
            SUBTLE,
        );
    }
}

fn draw_legend(ui: &mut Ui, series: &[(&str, Color32, Vec<f64>, Vec<bool>)]) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        for (name, color, _, _) in series {
            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, *color);
            ui.label(RichText::new(*name).size(12.0).weak());
            ui.add_space(12.0);
        }
        ui.label(RichText::new("红点/红三角 = 异常月份").size(12.0).color(C_ERR));
    });
}

fn fmt_k(v: f64) -> String {
    let a = v.abs();
    if a >= 1_0000_0000.0 {
        format!("{:.1}亿", v / 1_0000_0000.0)
    } else if a >= 1_0000.0 {
        format!("{:.1}万", v / 1_0000.0)
    } else {
        format!("{:.0}", v)
    }
}

fn driver_panel(ui: &mut Ui, title: &str, items: &[findb::advanced::DriverItem], signed: bool) {
    ui.label(RichText::new(title).strong());
    if items.is_empty() {
        ui.label(RichText::new("暂无数据").weak());
        return;
    }
    let max_abs = items
        .iter()
        .map(|d| f64_of(d.amount).abs())
        .fold(1.0f64, f64::max);
    for d in items {
        let cur = f64_of(d.amount);
        let prev = f64_of(d.prev_amount);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&d.name).size(12.5));
            ui.add_space(4.0);
            let frac = (cur.abs() / max_abs) as f32;
            let (w, _) = ui.allocate_exact_size(egui::vec2(80.0 * frac.max(0.02), 8.0), Sense::hover());
            let color = if signed && cur < 0.0 { C_ERR } else { C_REV };
            ui.painter().rect_filled(w, 2.0, color);
            ui.label(RichText::new(d.amount.fmt_money()).size(12.5).monospace());
        });
        // 环比变化
        let change = if prev != 0.0 {
            let pct = (cur - prev) / prev.abs() * 100.0;
            let (arrow, c) = if pct > 0.5 {
                ("▲", palette::OK)
            } else if pct < -0.5 {
                ("▼", C_ERR)
            } else {
                ("—", SUBTLE)
            };
            format!("{arrow} {:.1}%", pct.abs())
        } else if cur != 0.0 {
            "▲ 新增".to_string()
        } else {
            "—".to_string()
        };
        ui.label(
            RichText::new(format!("  环比 {change}"))
                .size(11.5)
                .weak(),
        );
        ui.add_space(4.0);
    }
}

fn stat(ui: &mut Ui, label: &str, v: &Money) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).weak().size(12.0));
        ui.label(RichText::new(v.fmt_money()).size(17.0).strong());
    });
}

fn ratio_chip(ui: &mut Ui, r: &findb::advanced::FinRatio) {
    let verdict = ratio_verdict(&r.key, r.value.to_f64());
    let (tag, color) = match verdict {
        2 => ("健康", palette::OK),
        1 => ("关注", palette::WARN),
        _ => ("预警", C_ERR),
    };
    egui::Frame::NONE
        .fill(palette::SUBTOTAL_BG)
        .stroke(Stroke::new(1.0, palette::GRID))
        .corner_radius(8.0)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_min_width(150.0);
            ui.label(RichText::new(&r.name).weak().size(11.5));
            ui.horizontal(|ui| {
                ui.label(RichText::new(&r.display).size(19.0).strong());
                ui.add_space(6.0);
                ui.label(
                    RichText::new(tag)
                        .size(11.0)
                        .strong()
                        .color(color),
                );
            });
            ui.label(RichText::new(&r.formula).size(10.5).weak());
        });
}

/// 财务指标健康度：2 健康 / 1 关注 / 0 预警
fn ratio_verdict(key: &str, v: f64) -> i32 {
    match key {
        "current_ratio" => {
            if v >= 2.0 { 2 } else if v >= 1.0 { 1 } else { 0 }
        }
        "quick_ratio" => {
            if v >= 1.0 { 2 } else if v >= 0.5 { 1 } else { 0 }
        }
        "debt_ratio" => {
            if v <= 0.5 { 2 } else if v <= 0.7 { 1 } else { 0 }
        }
        "gross_margin" => {
            if v >= 0.3 { 2 } else if v >= 0.1 { 1 } else { 0 }
        }
        "net_margin" => {
            if v >= 0.1 { 2 } else if v >= 0.0 { 1 } else { 0 }
        }
        "roe" => {
            if v >= 0.1 { 2 } else if v >= 0.0 { 1 } else { 0 }
        }
        "roa" => {
            if v >= 0.05 { 2 } else if v >= 0.0 { 1 } else { 0 }
        }
        _ => 1,
    }
}

fn kv(ui: &mut Ui, label: &str, v: &str) {
    ui.label(RichText::new(format!("{label}：")).weak());
    ui.label(RichText::new(v).strong());
    ui.add_space(10.0);
}
