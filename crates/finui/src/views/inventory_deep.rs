//! 库存深度：序列号 / 多单位 / 账龄 / ABC / 组装拆卸 / 分仓库 / 调拨报表
//!
//! 对标金蝶/用友库存管理。数据层 `findb::inventory2` 已就绪，这里只做展示与录入。

use chrono::NaiveDate;
use egui::{RichText, Ui};
use findb::business::{self, StockMove};
use findb::inventory2::{self, AbcRow, InvAging, ItemUnit, Serial, WhStock};
use fincore::{Money, Perm, Period};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets::{self, Paging};

const ALL_ITEMS: &str = "全部";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Serial,
    Unit,
    Aging,
    Abc,
    Assemble,
    Warehouse,
    Transfer,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Serial => "序列号",
            Tab::Unit => "多单位",
            Tab::Aging => "账龄分析",
            Tab::Abc => "ABC 分析",
            Tab::Assemble => "组装拆卸",
            Tab::Warehouse => "分仓库库存",
            Tab::Transfer => "调拨报表",
        }
    }
    const ALL: &'static [Tab] = &[
        Tab::Serial,
        Tab::Unit,
        Tab::Aging,
        Tab::Abc,
        Tab::Assemble,
        Tab::Warehouse,
        Tab::Transfer,
    ];
}

pub struct InventoryDeepView {
    pub tab: Tab,
    pub period_text: String,
    pub item: String,
    pub items: Vec<String>,

    // 序列号
    pub serial_batch: String,
    pub serial_date: String,
    pub serial_in_text: String,
    pub serial_out_text: String,
    pub serials: Vec<Serial>,

    // 多单位
    pub unit_base: String,
    pub unit_alt: String,
    pub unit_factor: String,
    pub unit_current: Option<ItemUnit>,
    pub unit_conv: String,

    // 账龄 / ABC
    pub aging: Vec<InvAging>,
    pub abc: Vec<AbcRow>,

    // 组装拆卸
    pub asm_parent: String,
    pub asm_children: String,
    pub asm_memo: String,
    pub asm_date: String,

    // 分仓库
    pub wh_item: String,
    pub wh_rows: Vec<WhStock>,

    // 调拨
    pub transfer: Vec<StockMove>,

    pub paging: Paging,
    pub err: String,
    pub dirty: bool,
    key: String,
}

impl Default for InventoryDeepView {
    fn default() -> Self {
        Self {
            tab: Tab::Serial,
            period_text: String::new(),
            item: String::new(),
            items: Vec::new(),
            serial_batch: String::new(),
            serial_date: String::new(),
            serial_in_text: String::new(),
            serial_out_text: String::new(),
            serials: Vec::new(),
            unit_base: String::new(),
            unit_alt: String::new(),
            unit_factor: String::new(),
            unit_current: None,
            unit_conv: String::new(),
            aging: Vec::new(),
            abc: Vec::new(),
            asm_parent: String::new(),
            asm_children: String::new(),
            asm_memo: String::new(),
            asm_date: String::new(),
            wh_item: String::new(),
            wh_rows: Vec::new(),
            transfer: Vec::new(),
            paging: Paging::default(),
            err: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl InventoryDeepView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        if self.item.is_empty() {
            self.item = ALL_ITEMS.to_string();
        }
        self.dirty = true;
    }

    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }

    fn today(&self) -> NaiveDate {
        chrono::Local::now().date_naive()
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}|{}|{}", p.ymm(), self.tab.name(), self.item);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        self.items = business::stock_items(ctx.db()).unwrap_or_default();
        match self.tab {
            Tab::Serial => {
                let item = if self.item == ALL_ITEMS { "" } else { &self.item };
                self.serials = inventory2::serial_list(ctx.db(), item).unwrap_or_default();
            }
            Tab::Unit => {
                self.unit_current = inventory2::unit_get(ctx.db(), &self.item).ok().flatten();
            }
            Tab::Aging => {
                self.aging = inventory2::inv_aging(ctx.db(), p).unwrap_or_default();
            }
            Tab::Abc => {
                self.abc = inventory2::abc_analysis(ctx.db(), p).unwrap_or_default();
            }
            Tab::Warehouse => {
                self.wh_rows = inventory2::warehouse_stock(ctx.db(), &self.wh_item).unwrap_or_default();
            }
            Tab::Transfer => {
                self.transfer = inventory2::transfer_report(ctx.db(), p).unwrap_or_default();
            }
            Tab::Assemble => {}
        }
        self.paging.reset();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "库存深度", |ui| {
            ui.label(RichText::new("序列号 / 多单位 / 账龄 / ABC / 组装拆卸 / 分仓库 / 调拨").weak());
        });

        widgets::toolbar(ui, |ui| {
            for t in Tab::ALL {
                ui.selectable_value(&mut self.tab, *t, t.name());
            }
            ui.separator();
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, &self.err);
        }

        match self.tab {
            Tab::Serial => self.show_serial(ctx, ui),
            Tab::Unit => self.show_unit(ctx, ui),
            Tab::Aging => self.show_aging(ui),
            Tab::Abc => self.show_abc(ui),
            Tab::Assemble => self.show_assemble(ctx, ui, p),
            Tab::Warehouse => self.show_warehouse(ui),
            Tab::Transfer => self.show_transfer(ui),
        }
    }

    // ------------------------- 序列号 -------------------------
    fn show_serial(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("存货");
            let mut opts = vec![ALL_ITEMS.to_string()];
            opts.extend(self.items.iter().cloned());
            if widgets::combo(ui, "invdeep_serial_item", &mut self.item, &opts, 160.0).changed() {
                self.dirty = true;
            }
        });

        widgets::card(ui, "入库登记", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("日期");
                if self.serial_date.is_empty() {
                    self.serial_date = self.today().format("%Y-%m-%d").to_string();
                }
                ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.serial_date));
                ui.label("批次");
                widgets::text_input(ui, &mut self.serial_batch, 120.0, "可留空");
                ui.label("序列号(逗号/换行分隔)");
                ui.add_sized(
                    [260.0, 44.0],
                    egui::TextEdit::multiline(&mut self.serial_in_text).hint_text("S001, S002, S003"),
                );
                if ui.button("批量入库").clicked() && ctx.can(Perm::AccountEdit) {
                    self.do_serial_in(ctx);
                }
            });
        });

        widgets::card(ui, "出库登记", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("日期");
                if self.serial_date.is_empty() {
                    self.serial_date = self.today().format("%Y-%m-%d").to_string();
                }
                ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.serial_date));
                ui.label("序列号");
                ui.add_sized(
                    [260.0, 44.0],
                    egui::TextEdit::multiline(&mut self.serial_out_text).hint_text("S001, S002"),
                );
                if ui.button("批量出库").clicked() && ctx.can(Perm::AccountEdit) {
                    self.do_serial_out(ctx);
                }
            });
        });

        let rows = self.serials.clone();
        let cols = [
            widgets::TCol::new("序列号", 160.0),
            widgets::TCol::new("存货", 140.0),
            widgets::TCol::new("批次", 100.0),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("入库日期", 110.0).fixed(),
            widgets::TCol::new("出库日期", 110.0).fixed(),
        ];
        widgets::grid(ui, "invdeep_serials", &cols, rows.len(), 22.0, |i, c, ui| {
            let s = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&s.serial).monospace()); }
                1 => { ui.label(&s.item); }
                2 => { ui.label(&s.batch_no); }
                3 => {
                    ui.label(
                        RichText::new(match s.status.as_str() {
                            "in" => "在库",
                            "out" => "已出库",
                            _ => "报废",
                        })
                        .color(if s.status == "in" { palette::OK } else { palette::CREDIT }),
                    );
                }
                4 => { ui.label(&s.in_date); }
                5 => { ui.label(s.out_date.clone().unwrap_or_else(|| "—".to_string())); }
                _ => {}
            }
        });
    }

    fn split_serials(text: &str) -> Vec<String> {
        text.split([',', '，', '\n', ' ', '\t'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    fn do_serial_in(&mut self, ctx: &mut AppCtx<'_>) {
        self.err.clear();
        let item = if self.item == ALL_ITEMS { "" } else { &self.item };
        if item.is_empty() {
            self.err = "请先在上方选择存货".to_string();
            return;
        }
        let date = NaiveDate::parse_from_str(self.serial_date.trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| self.today());
        let serials = Self::split_serials(&self.serial_in_text);
        if serials.is_empty() {
            self.err = "请输入序列号".to_string();
            return;
        }
        match inventory2::serial_in(ctx.db(), item, &serials, &self.serial_batch, date) {
            Ok(n) => {
                ctx.log("库存深度", "序列号入库", &format!("{item} {n} 个"));
                ctx.info(format!("已登记 {n} 个序列号入库"));
                self.serial_in_text.clear();
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn do_serial_out(&mut self, ctx: &mut AppCtx<'_>) {
        self.err.clear();
        let date = NaiveDate::parse_from_str(self.serial_date.trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| self.today());
        let serials = Self::split_serials(&self.serial_out_text);
        if serials.is_empty() {
            self.err = "请输入序列号".to_string();
            return;
        }
        match inventory2::serial_out(ctx.db(), &serials, date) {
            Ok(n) => {
                ctx.log("库存深度", "序列号出库", &format!("{n} 个"));
                ctx.info(format!("已出库 {n} 个序列号"));
                self.serial_out_text.clear();
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    // ------------------------- 多单位 -------------------------
    fn show_unit(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("存货");
            let mut opts = vec![ALL_ITEMS.to_string()];
            opts.extend(self.items.iter().cloned());
            if widgets::combo(ui, "invdeep_unit_item", &mut self.item, &opts, 160.0).changed() {
                self.dirty = true;
                self.unit_current = inventory2::unit_get(ctx.db(), &self.item).ok().flatten();
                if let Some(u) = &self.unit_current {
                    self.unit_base = u.base_unit.clone();
                    self.unit_alt = u.alt_unit.clone();
                    self.unit_factor = u.factor.fmt_qty();
                }
            }
        });

        widgets::card(ui, "单位换算设置", |ui| {
            let item = self.item.clone();
            egui::Grid::new("invdeep_unit_form")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("存货");
                    ui.label(RichText::new(&item).strong());
                    ui.end_row();
                    ui.label("主单位");
                    widgets::text_input(ui, &mut self.unit_base, 120.0, "如 个");
                    ui.end_row();
                    ui.label("辅助单位");
                    widgets::text_input(ui, &mut self.unit_alt, 120.0, "如 箱");
                    ui.end_row();
                    ui.label("换算系数(1主=系数辅)");
                    widgets::money_input(ui, &mut self.unit_factor, 120.0);
                    ui.end_row();
                });
            ui.add_space(6.0);
            if ui.button("保存换算").clicked() && ctx.can(Perm::AccountEdit) {
                self.do_save_unit(ctx);
            }
            if ui.button("测试换算(50 主单位)").clicked() {
                let factor = Money::parse_or_zero(&self.unit_factor);
                self.unit_conv = if factor.is_zero() {
                    "请先填写有效系数".to_string()
                } else {
                    format!("50 {} = {} {}", self.unit_base, Money::from_i64(50).checked_div(factor.inner()).expect("factor 已判非零").round_dp(fincore::money::QTY_DP).fmt_qty(), self.unit_alt)
                };
            }
            if !self.unit_conv.is_empty() {
                ui.label(RichText::new(&self.unit_conv).strong());
            }
        });
    }

    fn do_save_unit(&mut self, ctx: &mut AppCtx<'_>) {
        self.err.clear();
        if self.item == ALL_ITEMS || self.item.is_empty() {
            self.err = "请选择存货".to_string();
            return;
        }
        let u = ItemUnit {
            item: self.item.clone(),
            base_unit: self.unit_base.trim().to_string(),
            alt_unit: self.unit_alt.trim().to_string(),
            factor: Money::parse_or_zero(&self.unit_factor),
        };
        if u.base_unit.is_empty() || u.alt_unit.is_empty() {
            self.err = "请填写主单位与辅助单位".to_string();
            return;
        }
        match inventory2::unit_set(ctx.db(), &u) {
            Ok(()) => {
                ctx.log("库存深度", "单位换算", &format!("{} {}→{}", self.item, self.unit_base, self.unit_alt));
                ctx.info("已保存单位换算");
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    // ------------------------- 账龄 / ABC -------------------------
    fn show_aging(&self, ui: &mut Ui) {
        let rows = self.aging.clone();
        let cols = [
            widgets::TCol::new("存货", 180.0),
            widgets::TCol::new("最近入库", 110.0).fixed(),
            widgets::TCol::new("账龄(天)", 100.0).right(),
            widgets::TCol::new("结存数量", 110.0).right(),
            widgets::TCol::new("结存金额", 130.0).right(),
        ];
        widgets::grid(ui, "invdeep_aging", &cols, rows.len(), 24.0, |i, c, ui| {
            let a = &rows[i];
            match c {
                0 => { ui.label(&a.item); }
                1 => { ui.label(a.last_in.clone().unwrap_or_else(|| "—".to_string())); }
                2 => { ui.label(a.days.to_string()); }
                3 => { ui.label(a.qty.fmt_qty()); }
                4 => widgets::amount_label(ui, a.amount),
                _ => {}
            }
        });
    }

    fn show_abc(&self, ui: &mut Ui) {
        let rows = self.abc.clone();
        let cols = [
            widgets::TCol::new("存货", 180.0),
            widgets::TCol::new("结存金额", 130.0).right(),
            widgets::TCol::new("累计占比(%)", 110.0).right(),
            widgets::TCol::new("分类", 80.0).fixed(),
        ];
        widgets::grid(ui, "invdeep_abc", &cols, rows.len(), 24.0, |i, c, ui| {
            let a = &rows[i];
            match c {
                0 => { ui.label(&a.item); }
                1 => widgets::amount_label(ui, a.amount),
                2 => { ui.label(a.cum_pct.fmt_qty()); }
                3 => {
                    let color = match a.class.as_str() {
                        "A" => palette::CREDIT,
                        "B" => palette::WARN,
                        _ => palette::OK,
                    };
                    ui.label(RichText::new(&a.class).color(color).strong());
                }
                _ => {}
            }
        });
    }

    // ------------------------- 组装拆卸 -------------------------
    fn show_assemble(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        if self.asm_date.is_empty() {
            self.asm_date = self.today().format("%Y-%m-%d").to_string();
        }
        widgets::card(ui, "组装 / 拆卸", |ui| {
            ui.label(RichText::new(
                "组装：子件出库、成品入库（成品数量记 1）；拆卸：成品出库、子件入库。",
            ).weak());
            ui.add_space(4.0);
            egui::Grid::new("invdeep_asm_form")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("日期");
                    ui.add_sized([120.0, 22.0], egui::TextEdit::singleline(&mut self.asm_date));
                    ui.end_row();
                    ui.label("成品/母件");
                    let opts = self.items.clone();
                    if widgets::combo(ui, "invdeep_asm_parent", &mut self.asm_parent, &opts, 180.0).changed() {
                        // 无需额外处理，仅记录父件选择
                    }
                    ui.end_row();
                    ui.label("子件(名称:数量)");
                    ui.add_sized(
                        [280.0, 66.0],
                        egui::TextEdit::multiline(&mut self.asm_children).hint_text("RM1:2\nRM2:1"),
                    );
                    ui.end_row();
                    ui.label("备注");
                    widgets::text_input(ui, &mut self.asm_memo, 200.0, "可留空");
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("组装").clicked() && ctx.can(Perm::AccountEdit) {
                    self.do_assemble(ctx, p, false);
                }
                if ui.button("拆卸").clicked() && ctx.can(Perm::AccountEdit) {
                    self.do_assemble(ctx, p, true);
                }
            });
        });
    }

    fn parse_children(&self) -> Result<Vec<(String, Money)>, String> {
        let mut out = Vec::new();
        for line in self.asm_children.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (name, qty) = line
                .split_once([':', '：'])
                .ok_or_else(|| format!("子件行格式应为「名称:数量」：{line}"))?;
            let qty = Money::parse_or_zero(qty.trim());
            if qty.is_zero() {
                return Err(format!("子件数量不能为零：{line}"));
            }
            out.push((name.trim().to_string(), qty));
        }
        if out.is_empty() {
            return Err("请填写至少一个子件".to_string());
        }
        Ok(out)
    }

    fn do_assemble(&mut self, ctx: &mut AppCtx<'_>, p: Period, disassemble: bool) {
        self.err.clear();
        if self.asm_parent.trim().is_empty() {
            self.err = "请选择成品/母件".to_string();
            return;
        }
        let date = NaiveDate::parse_from_str(self.asm_date.trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| self.today());
        let children = match self.parse_children() {
            Ok(c) => c,
            Err(e) => {
                self.err = e;
                return;
            }
        };
        let r = if disassemble {
            inventory2::disassemble(ctx.db(), p, date, &self.asm_parent, &children, &self.asm_memo)
        } else {
            inventory2::assemble(ctx.db(), p, date, &self.asm_parent, &children, &self.asm_memo)
        };
        match r {
            Ok(()) => {
                let action = if disassemble { "拆卸" } else { "组装" };
                ctx.log("库存深度", action, &format!("{} {}", self.asm_parent, p.label()));
                ctx.info(format!("已{action}"));
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    // ------------------------- 分仓库 -------------------------
    fn show_warehouse(&mut self, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("存货");
            let opts = self.items.clone();
            if widgets::combo(ui, "invdeep_wh_item", &mut self.wh_item, &opts, 180.0).changed() {
                self.dirty = true;
            }
        });
        let rows = self.wh_rows.clone();
        let cols = [
            widgets::TCol::new("仓库", 180.0),
            widgets::TCol::new("存货", 180.0),
            widgets::TCol::new("结存数量", 130.0).right(),
        ];
        widgets::grid(ui, "invdeep_wh", &cols, rows.len(), 24.0, |i, c, ui| {
            let w = &rows[i];
            match c {
                0 => { ui.label(&w.warehouse); }
                1 => { ui.label(&w.item); }
                2 => { ui.label(w.qty.fmt_qty()); }
                _ => {}
            }
        });
    }

    // ------------------------- 调拨报表 -------------------------
    fn show_transfer(&mut self, ui: &mut Ui) {
        let rows = self.transfer.clone();
        let cols = [
            widgets::TCol::new("日期", 100.0).fixed(),
            widgets::TCol::new("存货", 170.0),
            widgets::TCol::new("仓库", 120.0),
            widgets::TCol::new("数量", 100.0).right(),
            widgets::TCol::new("备注", 220.0),
        ];
        widgets::grid(ui, "invdeep_transfer", &cols, rows.len(), 24.0, |i, c, ui| {
            let m = &rows[i];
            match c {
                0 => { ui.label(m.biz_date.format("%Y-%m-%d").to_string()); }
                1 => { ui.label(&m.item); }
                2 => { ui.label(&m.warehouse); }
                3 => { ui.label(m.qty.fmt_qty()); }
                4 => { ui.label(&m.memo); }
                _ => {}
            }
        });
    }
}
