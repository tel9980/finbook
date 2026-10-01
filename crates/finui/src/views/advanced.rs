//! 高级功能界面：多栏账 / 摘要汇总表 / 财务指标 / 制造管理（工艺路线·MRP）/
//! 审批中心 / 报表附注 / 电子档案

use egui::{RichText, Ui};
use findb::advanced;
use fincore::{AuxKind, Money, Period};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

pub struct MultiColumnView {
    pub main: String,
    pub cols_text: String,
    pub period_text: String,
    pub rows: Vec<advanced::MultiColRow>,
    pub dirty: bool,
    key: String,
}

impl Default for MultiColumnView {
    fn default() -> Self {
        Self {
            main: "6602".into(),
            cols_text: "660201,660202,660203".into(),
            period_text: String::new(),
            rows: Vec::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl MultiColumnView {
    pub fn invalidate(&mut self) { self.dirty = true; }
    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() { self.period_text = ctx.period().code(); }
        self.dirty = true;
    }
    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }
    fn cols(&self) -> Vec<String> {
        self.cols_text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    }
    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}|{}|{}", p.ymm(), self.main, self.cols_text);
        if !self.dirty && self.key == key { return; }
        self.key = key;
        self.dirty = false;
        let cols = self.cols();
        if self.main.trim().is_empty() || cols.is_empty() { self.rows.clear(); return; }
        match advanced::multi_column_table(ctx.db(), self.main.trim(), &cols, p, p, Some(ctx.user())) {
            Ok(rows) => self.rows = rows,
            Err(e) => { ctx.error(e.to_string()); self.rows.clear(); }
        }
    }
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        widgets::page_header(ui, "多栏账", |ui| { ui.label(RichText::new(p.label()).weak()); });
        widgets::toolbar(ui, |ui| {
            ui.label("主科目");
            let r = ui.add_sized([80.0, 22.0], egui::TextEdit::singleline(&mut self.main));
            if r.changed() { self.dirty = true; }
            ui.label("栏目(逗号分隔)");
            let r = ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut self.cols_text));
            if r.changed() { self.dirty = true; }
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() { self.dirty = true; }
            if ui.button("刷新").clicked() { self.dirty = true; }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new("以主科目发生额为主线，按凭证把对方发生额拆到各栏目；金额带符号（借正贷负）。").weak());
        ui.add_space(6.0);
        let cols = self.cols();
        let rows = self.rows.clone();
        let mut tcols = vec![
            widgets::TCol::new("日期", 90.0).fixed(),
            widgets::TCol::new("凭证号", 90.0).fixed(),
            widgets::TCol::new("摘要", 180.0),
            widgets::TCol::new("发生额", 110.0).right(),
        ];
        for c in &cols { tcols.push(widgets::TCol::new(c, 110.0).right()); }
        tcols.push(widgets::TCol::new("余额", 110.0).right());
        let ncols = cols.len();
        widgets::grid(ui, "multi_col", &tcols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(&r.date); }
                1 => { ui.label(RichText::new(&r.voucher_no).monospace()); }
                2 => { ui.label(&r.summary); }
                3 => { widgets::amount_label(ui, r.amount); }
                x if x >= 4 && x < 4 + ncols => {
                    let v = r.cols.get(x - 4).copied().unwrap_or(Money::ZERO);
                    if v.is_zero() { ui.label("—"); } else { widgets::amount_label(ui, v); }
                }
                _ => { widgets::amount_label(ui, r.balance); }
            }
        });
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let cols = self.cols();
        let mut headers = vec!["日期".to_string(), "凭证号".to_string(), "摘要".to_string(), "发生额".to_string()];
        headers.extend(cols.iter().cloned());
        headers.push("余额".to_string());
        let mut sh = crate::views::export::Sheet::new("多栏账", headers);
        for r in &self.rows {
            let mut row = vec![
                r.date.clone(),
                r.voucher_no.clone(),
                r.summary.clone(),
                r.amount.fmt_plain(),
            ];
            row.extend(r.cols.iter().map(|v| v.fmt_plain()));
            row.push(r.balance.fmt_plain());
            sh.push(row);
        }
        let title = format!("多栏账（{}-{}）", self.main, self.period_text);
        match crate::views::export::run_export(&sh, "多栏账", &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}

pub struct SummaryTableView {
    pub period_text: String,
    pub rows: Vec<advanced::SummaryRow>,
    pub dirty: bool,
    key: String,
}

impl Default for SummaryTableView {
    fn default() -> Self {
        Self { period_text: String::new(), rows: Vec::new(), dirty: true, key: String::new() }
    }
}

impl SummaryTableView {
    pub fn invalidate(&mut self) { self.dirty = true; }
    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() { self.period_text = ctx.period().code(); }
        self.dirty = true;
    }
    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }
    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = p.ymm().to_string();
        if !self.dirty && self.key == key { return; }
        self.key = key;
        self.dirty = false;
        match advanced::summary_table(ctx.db(), p, p, Some(ctx.user())) {
            Ok(rows) => self.rows = rows,
            Err(e) => { ctx.error(e.to_string()); self.rows.clear(); }
        }
    }
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        widgets::page_header(ui, "摘要汇总表", |ui| { ui.label(RichText::new(p.label()).weak()); });
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() { self.dirty = true; }
            if ui.button("刷新").clicked() { self.dirty = true; }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });
        ui.add_space(6.0);
        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("摘要", 300.0),
            widgets::TCol::new("凭证张数", 100.0).right(),
            widgets::TCol::new("借方发生额", 140.0).right(),
            widgets::TCol::new("贷方发生额", 140.0).right(),
        ];
        widgets::grid(ui, "summary_table", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(&r.summary); }
                1 => { ui.label(r.voucher_count.to_string()); }
                2 => { widgets::amount_label(ui, r.debit); }
                3 => { widgets::amount_label(ui, r.credit); }
                _ => {}
            }
        });
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let mut sh = crate::views::export::Sheet::new(
            "摘要汇总表",
            vec!["摘要".to_string(), "凭证张数".to_string(), "借方发生额".to_string(), "贷方发生额".to_string()],
        );
        for r in &self.rows {
            sh.push(vec![
                r.summary.clone(),
                r.voucher_count.to_string(),
                r.debit.fmt_plain(),
                r.credit.fmt_plain(),
            ]);
        }
        let title = format!("摘要汇总表（{}）", self.period_text);
        match crate::views::export::run_export(&sh, "摘要汇总表", &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}

pub struct RatiosView {
    pub period_text: String,
    pub rows: Vec<advanced::FinRatio>,
    pub dirty: bool,
    key: String,
}

impl Default for RatiosView {
    fn default() -> Self {
        Self { period_text: String::new(), rows: Vec::new(), dirty: true, key: String::new() }
    }
}

impl RatiosView {
    pub fn invalidate(&mut self) { self.dirty = true; }
    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() { self.period_text = ctx.period().code(); }
        self.dirty = true;
    }
    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }
    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = p.ymm().to_string();
        if !self.dirty && self.key == key { return; }
        self.key = key;
        self.dirty = false;
        let from = Period::new(p.year(), 1).unwrap_or(p);
        match advanced::fin_ratios(ctx.db(), p, from, Some(ctx.user())) {
            Ok(rows) => self.rows = rows,
            Err(e) => { ctx.error(e.to_string()); self.rows.clear(); }
        }
    }
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        widgets::page_header(ui, "财务指标分析", |ui| { ui.label(RichText::new(p.label()).weak()); });
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() { self.dirty = true; }
            if ui.button("刷新").clicked() { self.dirty = true; }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });
        ui.add_space(6.0);
        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("指标", 220.0),
            widgets::TCol::new("数值", 120.0).right(),
            widgets::TCol::new("计算公式", 400.0),
        ];
        widgets::grid(ui, "ratios", &cols, rows.len(), 26.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.name).strong()); }
                1 => { ui.label(RichText::new(&r.display).strong()); }
                2 => { ui.label(RichText::new(&r.formula).weak()); }
                _ => {}
            }
        });
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let mut sh = crate::views::export::Sheet::new(
            "财务指标分析",
            vec!["指标".to_string(), "数值".to_string(), "计算公式".to_string()],
        );
        for r in &self.rows {
            sh.push(vec![r.name.clone(), r.display.clone(), r.formula.clone()]);
        }
        let title = format!("财务指标分析（{}）", self.period_text);
        match crate::views::export::run_export(&sh, "财务指标分析", &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}

// ===========================================================================
// 制造管理
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum MfgTab { Routing, Mrp, Report }

pub struct ManufacturingView {
    tab: MfgTab,
    pub item_text: String,
    pub ops: Vec<advanced::RoutingOp>,
    pub mrp_demand_item: String,
    pub mrp_demand_qty: String,
    pub mrp_rows: Vec<advanced::MrpRow>,
    pub mrp_run_at: String,
    // 报工
    pub po_no: String,
    pub po_id: i64,
    pub prod_ops: Vec<advanced::ProdOp>,
    pub report_qty: String,
    pub report_hours: String,
    pub report_sel: Option<usize>,
    pub dirty: bool,
    key: String,
}

impl Default for ManufacturingView {
    fn default() -> Self {
        Self {
            tab: MfgTab::Routing,
            item_text: String::new(),
            ops: Vec::new(),
            mrp_demand_item: String::new(),
            mrp_demand_qty: "10".into(),
            mrp_rows: Vec::new(),
            mrp_run_at: String::new(),
            po_no: String::new(),
            po_id: 0,
            prod_ops: Vec::new(),
            report_qty: String::new(),
            report_hours: String::new(),
            report_sel: None,
            dirty: true,
            key: String::new(),
        }
    }
}

impl ManufacturingView {
    pub fn invalidate(&mut self) { self.dirty = true; }
    pub fn enter(&mut self, _ctx: &mut AppCtx<'_>) { self.dirty = true; }
    fn item_codes(&self, ctx: &AppCtx<'_>) -> Vec<String> {
        findb::auxs::codes(ctx.db(), AuxKind::Item).unwrap_or_default()
    }
    fn reload_routing(&mut self, ctx: &mut AppCtx<'_>) {
        let key = format!("r|{}", self.item_text);
        if !self.dirty && self.key == key { return; }
        self.key = key;
        self.dirty = false;
        self.ops.clear();
        if self.item_text.trim().is_empty() { return; }
        match advanced::routing_list(ctx.db(), self.item_text.trim()) {
            Ok(ops) => self.ops = ops,
            Err(e) => ctx.error(e.to_string()),
        }
    }
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::page_header(ui, "制造管理", |ui| {
            ui.label(RichText::new("工艺路线 · MRP 运算 · 报工").weak());
        });
        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, MfgTab::Routing, "工艺路线");
            ui.selectable_value(&mut self.tab, MfgTab::Mrp, "MRP 运算");
            ui.selectable_value(&mut self.tab, MfgTab::Report, "工序报工");
        });
        ui.add_space(6.0);
        match self.tab {
            MfgTab::Routing => self.show_routing(ctx, ui),
            MfgTab::Mrp => self.show_mrp(ctx, ui),
            MfgTab::Report => self.show_report(ctx, ui),
        }
    }
    fn show_routing(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let codes = self.item_codes(ctx);
        widgets::toolbar(ui, |ui| {
            ui.label("产品(存货档案)");
            let mut idx = if self.item_text.is_empty() {
                0usize
            } else {
                codes.iter().position(|c| *c == self.item_text).unwrap_or(0)
            };
            if codes.is_empty() {
                ui.label(RichText::new("尚无存货档案").weak());
            } else {
                egui::ComboBox::from_id_salt("mfg_item")
                    .selected_text(codes.get(idx).cloned().unwrap_or_default())
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for (i, c) in codes.iter().enumerate() {
                            ui.selectable_value(&mut idx, i, c);
                        }
                    });
                let sel = codes.get(idx).cloned().unwrap_or_default();
                if sel != self.item_text {
                    self.item_text = sel;
                    self.dirty = true;
                }
            }
            if ui.button("加载").clicked() { self.dirty = true; }
            ui.separator();
            ui.label(RichText::new("工序清单（维护入口在 Web 端 /api/routing）").weak());
        });
        ui.add_space(4.0);
        self.reload_routing(ctx);
        let ops = self.ops.clone();
        let cols = [
            widgets::TCol::new("序号", 50.0).fixed(),
            widgets::TCol::new("工序编码", 110.0),
            widgets::TCol::new("工序名称", 160.0),
            widgets::TCol::new("工作中心", 120.0),
            widgets::TCol::new("标准工时", 90.0).right(),
            widgets::TCol::new("小时费率", 90.0).right(),
        ];
        widgets::grid(ui, "routing_ops", &cols, ops.len(), 24.0, |i, c, ui| {
            let op = &ops[i];
            match c {
                0 => { ui.label(op.seq.to_string()); }
                1 => { ui.label(RichText::new(&op.op_code).monospace()); }
                2 => { ui.label(&op.op_name); }
                3 => { ui.label(&op.work_center); }
                4 => { ui.label(op.std_hours.fmt_qty()); }
                5 => { widgets::amount_label(ui, op.rate); }
                _ => {}
            }
        });
        ui.add_space(6.0);
        ui.label(RichText::new("说明：工艺路线用于开工时生成报工清单、报工后按工时×费率归集人工成本。").weak());
    }
    fn show_mrp(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("需求产品");
            ui.add_sized([120.0, 22.0], egui::TextEdit::singleline(&mut self.mrp_demand_item));
            ui.label("数量");
            ui.add_sized([80.0, 22.0], egui::TextEdit::singleline(&mut self.mrp_demand_qty));
            if ui.button("运行 MRP").clicked() {
                let item = self.mrp_demand_item.trim().to_string();
                let qty = widgets::parse_money(&self.mrp_demand_qty);
                if item.is_empty() || qty.is_zero() {
                    ctx.error("请填写需求产品与数量");
                } else {
                    match advanced::mrp_run(ctx.db(), &[(item, qty, "手工".into())]) {
                        Ok(run_at) => {
                            self.mrp_run_at = run_at;
                            self.mrp_rows = advanced::mrp_by_run(ctx.db(), &self.mrp_run_at).unwrap_or_default();
                            ctx.info("MRP 运算完成");
                        }
                        Err(e) => ctx.error(e.to_string()),
                    }
                }
            }
            if ui.button("最近一次结果").clicked() {
                self.mrp_rows = advanced::mrp_latest(ctx.db()).unwrap_or_default();
                self.mrp_run_at = self.mrp_rows.first().map(|r| r.run_at.clone()).unwrap_or_default();
            }
        });
        ui.add_space(4.0);
        if !self.mrp_run_at.is_empty() {
            ui.label(RichText::new(format!("运算时间：{}", self.mrp_run_at)).weak());
        }
        ui.add_space(4.0);
        let rows = self.mrp_rows.clone();
        let cols = [
            widgets::TCol::new("层级", 50.0).fixed(),
            widgets::TCol::new("物料", 110.0),
            widgets::TCol::new("毛需求", 110.0).right(),
            widgets::TCol::new("现有库存", 100.0).right(),
            widgets::TCol::new("净需求", 110.0).right(),
            widgets::TCol::new("计划量", 110.0).right(),
            widgets::TCol::new("行动", 90.0),
            widgets::TCol::new("来源", 160.0),
        ];
        widgets::grid(ui, "mrp_rows", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(r.level.to_string()); }
                1 => { ui.label(RichText::new(&r.item_code).monospace()); }
                2 => { ui.label(r.gross_req.fmt_qty()); }
                3 => { ui.label(r.on_hand.fmt_qty()); }
                4 => { ui.label(r.net_req.fmt_qty()); }
                5 => { ui.label(r.planned_qty.fmt_qty()); }
                6 => {
                    let (txt, color) = match r.action.as_str() {
                        "produce" => ("生产", palette::OK),
                        "purchase" => ("采购", palette::WARN),
                        _ => ("无", palette::GRID),
                    };
                    ui.colored_label(color, txt);
                }
                7 => { ui.label(RichText::new(&r.source).weak()); }
                _ => {}
            }
        });
    }
    fn show_report(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("生产订单号");
            let r = ui.add_sized([140.0, 22.0], egui::TextEdit::singleline(&mut self.po_no));
            if r.changed() {
                self.po_id = 0;
                self.prod_ops.clear();
            }
            if ui.button("加载工序").clicked() {
                self.load_prod_ops(ctx);
            }
            ui.separator();
            ui.label("完工数量");
            ui.add_sized([80.0, 22.0], egui::TextEdit::singleline(&mut self.report_qty));
            ui.label("实际工时");
            ui.add_sized([80.0, 22.0], egui::TextEdit::singleline(&mut self.report_hours));
            if ui.button("报工").clicked() {
                self.do_report(ctx);
            }
            if ui.button("完工").clicked() {
                self.do_finish(ctx);
            }
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new("报工：按生产订单录入各工序的累计完工数量与实际工时；完工后可按工时×费率归集人工成本。").weak(),
        );
        ui.add_space(6.0);
        let ops = self.prod_ops.clone();
        let mut sel = self.report_sel;
        let cols = [
            widgets::TCol::new("", 30.0).fixed(),
            widgets::TCol::new("工序", 180.0),
            widgets::TCol::new("工作中心", 120.0),
            widgets::TCol::new("完工数量", 100.0).right(),
            widgets::TCol::new("实际工时", 100.0).right(),
            widgets::TCol::new("状态", 90.0),
        ];
        widgets::grid(ui, "prod_ops", &cols, ops.len(), 24.0, |i, c, ui| {
            let op = &ops[i];
            match c {
                0 => {
                    if ui.selectable_label(sel == Some(i), "").clicked() {
                        sel = if sel == Some(i) { None } else { Some(i) };
                    }
                }
                1 => { ui.label(&op.op_name); }
                2 => { ui.label(&op.work_center); }
                3 => { ui.label(op.qty_done.fmt_qty()); }
                4 => { ui.label(op.hours.fmt_qty()); }
                5 => {
                    let (txt, color) = match op.status.as_str() {
                        "done" => ("完工", palette::OK),
                        "in_progress" => ("进行中", palette::WARN),
                        _ => ("待开工", palette::GRID),
                    };
                    ui.colored_label(color, txt);
                }
                _ => {}
            }
        });
        self.report_sel = sel;
    }

    fn load_prod_ops(&mut self, ctx: &mut AppCtx<'_>) {
        let no = self.po_no.trim().to_string();
        if no.is_empty() {
            ctx.error("请输入生产订单号");
            return;
        }
        // 先找到生产订单 id
        let orders = findb::scm::prod_list(ctx.db(), ctx.period(), None).unwrap_or_default();
        let Some(order) = orders.into_iter().find(|o| o.no == no) else {
            ctx.error(format!("未找到生产订单「{no}」"));
            return;
        };
        self.po_id = order.id;
        match advanced::prod_op_list(ctx.db(), self.po_id) {
            Ok(ops) => {
                if ops.is_empty() {
                    // 首次加载：尝试按工艺路线生成工序清单
                    match advanced::prod_op_init_from_routing(ctx.db(), self.po_id, &order.item_code) {
                        Ok(n) if n > 0 => {
                            self.prod_ops = advanced::prod_op_list(ctx.db(), self.po_id).unwrap_or_default();
                            ctx.info(format!("已按工艺路线生成 {n} 道工序"));
                        }
                        _ => {
                            self.prod_ops.clear();
                            ctx.info("该订单暂无工序，请先维护产品工艺路线");
                        }
                    }
                } else {
                    self.prod_ops = ops;
                }
                self.report_sel = None;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn do_report(&mut self, ctx: &mut AppCtx<'_>) {
        if self.po_id == 0 {
            ctx.error("请先加载生产订单");
            return;
        }
        let Some(i) = self.report_sel else {
            ctx.error("请先点选要报工的工序");
            return;
        };
        let Some(op) = self.prod_ops.get(i) else {
            return;
        };
        let qty = widgets::parse_money(&self.report_qty);
        let hours = widgets::parse_money(&self.report_hours);
        if qty.is_zero() && hours.is_zero() {
            ctx.error("完工数量与工时不能同时为空");
            return;
        }
        match advanced::prod_op_report(ctx.db(), op.id, qty, hours) {
            Ok(()) => {
                ctx.info("已报工");
                self.report_qty.clear();
                self.report_hours.clear();
                self.prod_ops = advanced::prod_op_list(ctx.db(), self.po_id).unwrap_or_default();
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn do_finish(&mut self, ctx: &mut AppCtx<'_>) {
        if self.po_id == 0 {
            ctx.error("请先加载生产订单");
            return;
        }
        let Some(i) = self.report_sel else {
            ctx.error("请先点选要完工的工序");
            return;
        };
        let Some(op) = self.prod_ops.get(i) else {
            return;
        };
        match advanced::prod_op_finish(ctx.db(), op.id) {
            Ok(()) => {
                ctx.info("工序已完工");
                self.prod_ops = advanced::prod_op_list(ctx.db(), self.po_id).unwrap_or_default();
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }
}

// ===========================================================================
// 审批中心
// ===========================================================================

pub struct ApprovalView {
    pub rows: Vec<advanced::Approval>,
    pub dirty: bool,
    key: String,
}

impl Default for ApprovalView {
    fn default() -> Self {
        Self { rows: Vec::new(), dirty: true, key: String::new() }
    }
}

impl ApprovalView {
    pub fn invalidate(&mut self) { self.dirty = true; }
    pub fn enter(&mut self, _ctx: &mut AppCtx<'_>) { self.dirty = true; }
    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        if !self.dirty && self.key == "todo" { return; }
        self.key = "todo".into();
        self.dirty = false;
        match advanced::approval_todo(ctx.db(), ctx.user().username.as_str()) {
            Ok(rows) => self.rows = rows,
            Err(e) => { ctx.error(e.to_string()); self.rows.clear(); }
        }
    }
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        widgets::page_header(ui, "审批中心", |ui| { ui.label(RichText::new("待我审批").weak()); });
        widgets::toolbar(ui, |ui| {
            if ui.button("刷新").clicked() { self.dirty = true; }
            ui.separator();
            ui.label(RichText::new("说明：报销/采购/销售/生产单据可发起审批流，当前节点审批人可在此通过或驳回。").weak());
        });
        ui.add_space(6.0);
        let rows = self.rows.clone();
        if rows.is_empty() {
            widgets::empty_hint(ui, "没有待审批的单据");
            return;
        }
        for r in &rows {
            widgets::card(ui, &format!("{}（{}/{}）", r.title, r.biz_kind, r.biz_id), |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("申请人").weak());
                    ui.label(&r.applicant);
                    ui.separator();
                    ui.label(RichText::new("当前节点").weak());
                    let cur = r.steps.iter().find(|s| s.seq == r.current_node);
                    if let Some(s) = cur {
                        ui.label(format!("{}/{}（{}）", s.seq, r.steps.len(), s.approver));
                    }
                    ui.separator();
                    ui.label(RichText::new("发起时间").weak());
                    ui.label(&r.created_at);
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("通过").clicked() {
                        match advanced::approval_act(ctx.db(), r.id, ctx.user().username.as_str(), true, "同意") {
                            Ok(_) => { ctx.info("已通过"); self.dirty = true; }
                            Err(e) => ctx.error(e.to_string()),
                        }
                    }
                    if ui.button("驳回").clicked() {
                        match advanced::approval_act(ctx.db(), r.id, ctx.user().username.as_str(), false, "驳回") {
                            Ok(_) => { ctx.info("已驳回"); self.dirty = true; }
                            Err(e) => ctx.error(e.to_string()),
                        }
                    }
                });
            });
            ui.add_space(4.0);
        }
    }
}

// ===========================================================================
// 报表附注
// ===========================================================================

const REPORT_KEYS: &[(&str, &str)] = &[
    ("balance_sheet", "资产负债表"),
    ("income_statement", "利润表"),
    ("cash_flow", "现金流量表"),
];

fn report_key_label(k: &str) -> &'static str {
    match k {
        "income_statement" => "利润表",
        "cash_flow" => "现金流量表",
        _ => "资产负债表",
    }
}

pub struct ReportNotesView {
    pub report_key: String,
    pub period_text: String,
    pub rows: Vec<advanced::ReportNote>,
    pub new_title: String,
    pub new_content: String,
    pub dirty: bool,
    key: String,
}

impl Default for ReportNotesView {
    fn default() -> Self {
        Self {
            report_key: "balance_sheet".into(),
            period_text: String::new(),
            rows: Vec::new(),
            new_title: String::new(),
            new_content: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl ReportNotesView {
    pub fn invalidate(&mut self) { self.dirty = true; }
    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() { self.period_text = ctx.period().code(); }
        self.dirty = true;
    }
    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }
    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}|{}", self.report_key, p.ymm());
        if !self.dirty && self.key == key { return; }
        self.key = key;
        self.dirty = false;
        match advanced::note_list(ctx.db(), &self.report_key, p) {
            Ok(rows) => self.rows = rows,
            Err(e) => { ctx.error(e.to_string()); self.rows.clear(); }
        }
    }
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        widgets::page_header(ui, "报表附注", |ui| { ui.label(RichText::new(p.label()).weak()); });
        widgets::toolbar(ui, |ui| {
            ui.label("报表");
            let mut rk = self.report_key.clone();
            egui::ComboBox::from_id_salt("note_report_key")
                .selected_text(report_key_label(&rk))
                .width(180.0)
                .show_ui(ui, |ui| {
                    for (k, lbl) in REPORT_KEYS {
                        ui.selectable_value(&mut rk, k.to_string(), *lbl);
                    }
                });
            if rk != self.report_key {
                self.report_key = rk;
                self.dirty = true;
            }
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() { self.dirty = true; }
            if ui.button("刷新").clicked() { self.dirty = true; }
        });
        ui.add_space(6.0);
        widgets::card(ui, "新增附注", |ui| {
            ui.horizontal(|ui| {
                ui.label("标题");
                ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut self.new_title));
            });
            ui.horizontal(|ui| {
                ui.label("内容");
                ui.add_sized([400.0, 60.0], egui::TextEdit::multiline(&mut self.new_content));
                if ui.button("保存").clicked() {
                    let seq = (self.rows.len() as i32) + 1;
                    let mut n = advanced::ReportNote {
                        id: 0,
                        report_key: self.report_key.clone(),
                        period: p,
                        seq,
                        title: self.new_title.clone(),
                        content: self.new_content.clone(),
                        updated_by: ctx.user().username.clone(),
                        updated_at: String::new(),
                    };
                    match advanced::note_save(ctx.db(), &mut n) {
                        Ok(_) => {
                            ctx.info("已保存附注");
                            self.new_title.clear();
                            self.new_content.clear();
                            self.dirty = true;
                        }
                        Err(e) => ctx.error(e.to_string()),
                    }
                }
            });
        });
        ui.add_space(6.0);
        let rows = self.rows.clone();
        for (i, n) in rows.iter().enumerate() {
            widgets::card(ui, &format!("{} · {}", n.seq, n.title), |ui| {
                ui.label(&n.content);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("更新：{} {}", n.updated_by, n.updated_at)).weak());
                    if ui.button("删除").clicked() {
                        if let Err(e) = advanced::note_delete(ctx.db(), n.id) {
                            ctx.error(e.to_string());
                        } else {
                            ctx.info("已删除");
                            self.dirty = true;
                        }
                    }
                });
            });
            if i + 1 < rows.len() { ui.add_space(4.0); }
        }
        if rows.is_empty() { widgets::empty_hint(ui, "暂无附注"); }
    }
}

// ===========================================================================
// 电子档案
// ===========================================================================

pub struct ArchiveView {
    pub period_text: String,
    pub kind: String,
    pub rows: Vec<advanced::EArchive>,
    pub new_kind: String,
    pub new_title: String,
    pub new_payload: String,
    pub dirty: bool,
    key: String,
}

impl Default for ArchiveView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            kind: String::new(),
            rows: Vec::new(),
            new_kind: "voucher".into(),
            new_title: String::new(),
            new_payload: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl ArchiveView {
    pub fn invalidate(&mut self) { self.dirty = true; }
    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() { self.period_text = ctx.period().code(); }
        self.dirty = true;
    }
    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }
    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}|{}", p.ymm(), self.kind);
        if !self.dirty && self.key == key { return; }
        self.key = key;
        self.dirty = false;
        let kind = if self.kind.is_empty() { None } else { Some(self.kind.as_str()) };
        match advanced::archive_list(ctx.db(), p, kind) {
            Ok(rows) => self.rows = rows,
            Err(e) => { ctx.error(e.to_string()); self.rows.clear(); }
        }
    }
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        widgets::page_header(ui, "会计电子档案", |ui| { ui.label(RichText::new(p.label()).weak()); });
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() { self.dirty = true; }
            ui.label("类型(空=全部)");
            let r = ui.add_sized([100.0, 22.0], egui::TextEdit::singleline(&mut self.kind));
            if r.changed() { self.dirty = true; }
            if ui.button("刷新").clicked() { self.dirty = true; }
        });
        ui.add_space(6.0);
        widgets::card(ui, "新增归档", |ui| {
            ui.horizontal(|ui| {
                ui.label("类型");
                ui.add_sized([100.0, 22.0], egui::TextEdit::singleline(&mut self.new_kind));
                ui.label("标题");
                ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut self.new_title));
                if ui.button("归档").clicked() {
                    let file_no = match advanced::archive_next_no(ctx.db(), p, &self.new_kind) {
                        Ok(n) => n,
                        Err(e) => { ctx.error(e.to_string()); return; }
                    };
                    match advanced::archive_create(
                        ctx.db(),
                        p,
                        &self.new_kind,
                        &self.new_title,
                        &file_no,
                        &self.new_payload,
                        ctx.user().username.as_str(),
                    ) {
                        Ok(_) => {
                            ctx.info(format!("已归档 {file_no}"));
                            self.new_title.clear();
                            self.new_payload.clear();
                            self.dirty = true;
                        }
                        Err(e) => ctx.error(e.to_string()),
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("内容(JSON)");
                ui.add_sized([500.0, 60.0], egui::TextEdit::multiline(&mut self.new_payload));
            });
        });
        ui.add_space(6.0);
        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("档案号", 150.0),
            widgets::TCol::new("类型", 90.0),
            widgets::TCol::new("标题", 220.0),
            widgets::TCol::new("SHA-256", 160.0).fixed(),
            widgets::TCol::new("完整性", 80.0),
            widgets::TCol::new("归档人", 90.0),
            widgets::TCol::new("归档时间", 140.0),
        ];
        widgets::grid(ui, "archive_rows", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.file_no).monospace()); }
                1 => { ui.label(&r.kind); }
                2 => { ui.label(&r.title); }
                3 => {
                    let h = &r.content_hash;
                    ui.label(RichText::new(&h[..h.len().min(16)]).monospace());
                }
                4 => {
                    if advanced::archive_verify(r) {
                        ui.colored_label(palette::OK, "完好");
                    } else {
                        ui.colored_label(palette::CREDIT, "被篡改");
                    }
                }
                5 => { ui.label(&r.archived_by); }
                6 => { ui.label(&r.archived_at); }
                _ => {}
            }
        });
    }
}
