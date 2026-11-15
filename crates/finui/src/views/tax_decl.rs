//! 增值税一般纳税人申报表（桌面端）。
//!
//! 数据口径、法定表式、哪些项刻意不算，全部写在 `findb::taxdecl` 的模块注释里——
//! 本视图只负责取数 + 摆表 + 导出，与 Web 端（`finweb` 的税务申报表）同一套后端。
//! 边界一致：只取数、算表、导出可手工填报的表，不做网报、不碰税控设备。
//!
//! 桌面端是**只读展示**（申报口径展示）：未开票销售额、留抵结转等人工填报项，
//! Web 端同样只展示不代算，这里保持一致，只把后端算出的数摊出来。

use egui::{RichText, Ui};
use fincore::{Money, Period};
use findb::taxdecl::{self, DetailTable, VatMainForm};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

pub struct TaxDeclView {
    pub period_text: String,
    pub form: Option<VatMainForm>,
    pub dirty: bool,
    key: String,
}

impl Default for TaxDeclView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            form: None,
            dirty: true,
            key: String::new(),
        }
    }
}

impl TaxDeclView {
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
        let key = p.ymm().to_string();
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        match taxdecl::vat_main_form(ctx.db(), p) {
            Ok(f) => self.form = Some(f),
            Err(e) => {
                ctx.error(e.to_string());
                self.form = None;
            }
        }
    }

    /// 导出 CSV：主表 + 附一 + 附二 + 附加税费，与 Web 端同一份数。
    fn export(&mut self, ctx: &mut AppCtx<'_>, p: Period, mode: crate::views::export::ExportMode) {
        let Some(f) = &self.form else { return };
        let mut sh = crate::views::export::Sheet::new(
            "增值税申报表",
            vec!["项目".to_string(), "金额（元）".to_string()],
        );
        let mut push = |label: &str, v: Money| {
            sh.push(vec![label.to_string(), v.fmt_plain()]);
        };
        push("一般计税方法计税销售额", f.sales_general);
        push("简易计税方法计税销售额", f.sales_simple);
        push("免征增值税销售额", f.sales_exempt);
        push("待归类销售额（税率未填/不可识别）", f.sales_unknown);
        push("销项税额", f.output_tax);
        push("本期认证相符的进项税额", f.input_tax);
        push("应纳税额（销项-进项）", f.payable);
        push("城市维护建设税", f.surcharge.city_tax);
        push("教育费附加", f.surcharge.edu);
        push("地方教育附加", f.surcharge.local_edu);
        push("附加税费合计", f.surcharge.total);

        let title = format!("增值税申报表（{}）", p.label());
        match crate::views::export::run_export(
            &sh,
            "增值税申报表",
            &title,
            mode,
            &ctx.db().options(),
        ) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "增值税申报表", |ui| {
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
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, p, mode);
            }
        });

        let Some(f) = &self.form else {
            widgets::empty_hint(ui, "本期暂无申报表数据");
            return;
        };

        // 数据完整性提示：后端给出的边界必须原样摆出来，不能因为「表看着正常」就吞掉
        if !f.warnings.is_empty() {
            ui.add_space(4.0);
            for w in &f.warnings {
                ui.colored_label(palette::WARN, format!("⚠ {w}"));
            }
            ui.add_space(4.0);
        }

        ui.add_space(6.0);
        self.show_main(ui, f);
        ui.add_space(10.0);
        self.show_detail(ui, &f.details_out);
        ui.add_space(6.0);
        self.show_detail(ui, &f.details_in);
    }

    /// 主表 + 附加税费：两列（项目 / 金额）。
    fn show_main(&self, ui: &mut Ui, f: &VatMainForm) {
        #[derive(Clone, Copy, PartialEq)]
        enum Style {
            Section,
            Normal,
            Strong,
        }
        // (标题, 金额, 样式)；金额为 None 的纯节标题行
        let rows: Vec<(String, Option<Money>, Style)> = vec![
            ("一、销项税额".into(), None, Style::Section),
            (
                "一般计税方法计税销售额".into(),
                Some(f.sales_general),
                Style::Normal,
            ),
            (
                "简易计税方法计税销售额".into(),
                Some(f.sales_simple),
                Style::Normal,
            ),
            (
                "免征增值税销售额".into(),
                Some(f.sales_exempt),
                Style::Normal,
            ),
            (
                "待归类销售额（税率未填/不可识别）".into(),
                Some(f.sales_unknown),
                Style::Normal,
            ),
            ("销项税额".into(), Some(f.output_tax), Style::Strong),
            ("二、进项税额".into(), None, Style::Section),
            (
                "本期认证相符的进项税额".into(),
                Some(f.input_tax),
                Style::Strong,
            ),
            ("三、应纳税额".into(), None, Style::Section),
            (
                "应纳税额（销项 - 进项）".into(),
                Some(f.payable),
                Style::Strong,
            ),
            ("四、附加税费".into(), None, Style::Section),
            (
                format!("城市维护建设税（{}）", f.surcharge.city_rate),
                Some(f.surcharge.city_tax),
                Style::Normal,
            ),
            (
                format!("教育费附加（{}）", f.surcharge.edu_rate),
                Some(f.surcharge.edu),
                Style::Normal,
            ),
            (
                format!("地方教育附加（{}）", f.surcharge.local_edu_rate),
                Some(f.surcharge.local_edu),
                Style::Normal,
            ),
            (
                "附加税费合计".into(),
                Some(f.surcharge.total),
                Style::Strong,
            ),
        ];

        let cols = [
            widgets::TCol::new("项目", 320.0),
            widgets::TCol::new("金额（元）", 180.0).right(),
        ];
        widgets::grid(ui, "tax_main", &cols, rows.len(), 24.0, |i, c, ui| {
            let (label, amount, style) = &rows[i];
            match c {
                0 => {
                    let t = RichText::new(label.as_str());
                    let t = match style {
                        Style::Section => t.strong(),
                        Style::Normal => t,
                        Style::Strong => t.strong(),
                    };
                    ui.label(t);
                }
                1 => {
                    if let Some(v) = amount {
                        // 待归类非 0 是异常：金额标红提醒，和 warning 呼应
                        if label.contains("待归类") && !v.is_zero() {
                            ui.colored_label(palette::WARN, v.fmt_money());
                        } else if *style == Style::Strong {
                            widgets::amount_label(ui, *v);
                        } else {
                            ui.label(v.fmt_money());
                        }
                    }
                }
                _ => {}
            }
        });
    }

    /// 附列资料（附一 / 附二）：分档表。
    fn show_detail(&self, ui: &mut Ui, t: &DetailTable) {
        ui.label(RichText::new(&t.title).strong());
        ui.add_space(2.0);
        let cols = [
            widgets::TCol::new("税率/征收率", 120.0),
            widgets::TCol::new("张数", 70.0).right(),
            widgets::TCol::new("销售额", 150.0).right(),
            widgets::TCol::new("税额", 150.0).right(),
            widgets::TCol::new("价税合计", 150.0).right(),
        ];
        let n = t.buckets.len() + 1;
        widgets::grid(ui, "tax_detail", &cols, n, 22.0, |i, c, ui| {
            if i >= t.buckets.len() {
                // 合计行
                match c {
                    0 => {
                        ui.label(RichText::new("合计").strong());
                    }
                    1 => {
                        ui.label(RichText::new(t.count.to_string()).strong());
                    }
                    2 => {
                        let v = t.net_total.fmt_money();
                        ui.label(RichText::new(v).strong());
                    }
                    3 => {
                        let v = t.tax_total.fmt_money();
                        ui.label(RichText::new(v).strong());
                    }
                    4 => {
                        let v = t.gross_total.fmt_money();
                        ui.label(RichText::new(v).strong());
                    }
                    _ => {}
                }
                return;
            }
            let b = &t.buckets[i];
            match c {
                0 => {
                    let mut tag = b.rate.clone();
                    if !b.general_method {
                        tag.push_str(if b.rate == "0" {
                            "（免税）"
                        } else {
                            "（简易）"
                        });
                    }
                    ui.label(tag);
                }
                1 => {
                    ui.label(b.count.to_string());
                }
                2 => {
                    ui.label(b.net.fmt_money());
                }
                3 => {
                    ui.label(b.tax.fmt_money());
                }
                4 => {
                    ui.label(b.gross.fmt_money());
                }
                _ => {}
            }
        });
    }
}
