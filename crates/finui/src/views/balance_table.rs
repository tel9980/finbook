//! 科目余额表

use egui::{RichText, Ui};
use findb::balances::{BalanceQuery, BalanceSnapshot};
use fincore::{signed_to_dir_amount, BalanceRow, Money, Period};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

pub struct BalanceTableView {
    pub from: String,
    pub to: String,
    pub level: usize,
    pub leaf_only: bool,
    pub non_zero: bool,
    pub code_from: String,
    pub code_to: String,
    pub rows: Vec<BalanceRow>,
    pub trial: Option<(Money, Money, Money, Money, Money, Money)>,
    pub dirty: bool,
    /// 报表模式：0=科目余额表 1=辅助账 2=数量金额账
    pub mode: usize,
    /// 辅助账维度
    pub aux_kind: fincore::AuxKind,
    pub aux_rows: Vec<findb::balances::AuxBalanceRow>,
    pub qty_rows: Vec<findb::balances::QtyBalanceRow>,
    key: String,
}

const LEVELS: [&str; 7] = ["全部级次", "1 级", "2 级", "3 级", "4 级", "5 级", "6 级"];

impl Default for BalanceTableView {
    fn default() -> Self {
        Self {
            from: String::new(),
            to: String::new(),
            level: 0,
            leaf_only: false,
            non_zero: true,
            code_from: String::new(),
            code_to: String::new(),
            rows: Vec::new(),
            trial: None,
            dirty: true,
            mode: 0,
            aux_kind: fincore::AuxKind::Customer,
            aux_rows: Vec::new(),
            qty_rows: Vec::new(),
            key: String::new(),
        }
    }
}

impl BalanceTableView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        let p = ctx.period();
        if self.from.is_empty() {
            self.from = p.code();
        }
        if self.to.is_empty() {
            self.to = p.code();
        }
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let key = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{:?}",
            self.from, self.to, self.level, self.leaf_only, self.non_zero, self.code_from,
            self.code_to, self.mode, self.aux_kind
        );
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        let from = Period::parse(&self.from).unwrap_or_else(|_| ctx.period());
        let to = Period::parse(&self.to).unwrap_or_else(|_| ctx.period());
        if self.mode == 1 {
            match findb::balances::aux_balance(ctx.db(), self.aux_kind, from, to, Some(ctx.user())) {
                Ok(rows) => {
                    self.aux_rows = rows;
                    self.rows.clear();
                    self.trial = None;
                }
                Err(e) => {
                    ctx.error(e.to_string());
                    self.aux_rows.clear();
                }
            }
            return;
        }
        if self.mode == 2 {
            match findb::balances::qty_balance_sheet(ctx.db(), from, to, Some(ctx.user())) {
                Ok(rows) => {
                    self.qty_rows = rows;
                    self.rows.clear();
                    self.trial = None;
                }
                Err(e) => {
                    ctx.error(e.to_string());
                    self.qty_rows.clear();
                }
            }
            return;
        }
        let mut q = BalanceQuery::range(from, to)
            .with_leaf_only(self.leaf_only)
            .with_non_zero(self.non_zero);
        if self.level > 0 {
            q = q.with_max_level(Some(self.level as u8));
        }
        let cf = self.code_from.trim().to_string();
        let ct = self.code_to.trim().to_string();
        if !cf.is_empty() || !ct.is_empty() {
            q = q.with_code_range(
                if cf.is_empty() { None } else { Some(cf) },
                if ct.is_empty() { None } else { Some(ct) },
            );
        }
        // 数据范围（科目范围 + 仅本人凭证）：与用户手动选择取交集
        q = q.with_user_scope(ctx.user());

        match BalanceSnapshot::load(ctx.db(), &q) {
            Ok(s) => {
                let tb = s.trial_balance(ctx.chart());
                self.trial = Some((
                    tb.begin_debit,
                    tb.begin_credit,
                    tb.period_debit,
                    tb.period_credit,
                    tb.end_debit,
                    tb.end_credit,
                ));
                self.rows = s.account_table(ctx.chart(), &q);
            }
            Err(e) => {
                ctx.error(e.to_string());
                self.rows.clear();
                self.trial = None;
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        let mode_title = ["科目余额表", "辅助账", "数量金额账"][self.mode.min(2)];
        let row_count = match self.mode {
            1 => self.aux_rows.len(),
            2 => self.qty_rows.len(),
            _ => self.rows.len(),
        };
        widgets::page_header(ui, mode_title, |ui| {
            ui.label(RichText::new(format!("{} 行", row_count)).weak());
        });

        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.from));
            ui.label("—");
            ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.to));
            ui.label("报表");
            egui::ComboBox::from_id_salt("bt_mode")
                .selected_text(mode_title)
                .width(110.0)
                .show_ui(ui, |ui| {
                    for (i, s) in ["科目余额表", "辅助账", "数量金额账"].iter().enumerate() {
                        ui.selectable_value(&mut self.mode, i, *s);
                    }
                });
            if self.mode == 1 {
                ui.label("维度");
                egui::ComboBox::from_id_salt("bt_auxkind")
                    .selected_text(self.aux_kind.label())
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for k in fincore::AuxKind::BALANCE_DIMS {
                            ui.selectable_value(&mut self.aux_kind, *k, k.label());
                        }
                    });
            }
            ui.label("级次");
            egui::ComboBox::from_id_salt("bt_level")
                .selected_text(LEVELS[self.level])
                .width(100.0)
                .show_ui(ui, |ui| {
                    for (i, s) in LEVELS.iter().enumerate() {
                        ui.selectable_value(&mut self.level, i, *s);
                    }
                });
            ui.checkbox(&mut self.leaf_only, "只显示末级");
            ui.checkbox(&mut self.non_zero, "只显示有发生额/余额的");
            ui.label("科目范围");
            ui.add_sized([80.0, 22.0], egui::TextEdit::singleline(&mut self.code_from));
            ui.label("—");
            ui.add_sized([80.0, 22.0], egui::TextEdit::singleline(&mut self.code_to));
            if ui.button("查询").clicked() {
                self.dirty = true;
            }
            if ui.button("本年").clicked() {
                let p = ctx.period();
                self.from = Period::new(p.year(), 1).map(|x| x.code()).unwrap_or(p.code());
                self.to = p.code();
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        if self.mode == 1 {
            let rows = self.aux_rows.clone();
            let cols = [
                widgets::TCol::new(self.aux_kind.label(), 200.0),
                widgets::TCol::new("期初（带符号）", 150.0).right(),
                widgets::TCol::new("本期借方", 150.0).right(),
                widgets::TCol::new("本期贷方", 150.0).right(),
                widgets::TCol::new("期末（带符号）", 150.0).right(),
            ];
            widgets::grid(ui, "aux_balance", &cols, rows.len(), 24.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => { ui.label(&r.key); }
                    1 => widgets::amount_label(ui, r.begin),
                    2 => widgets::amount_label(ui, r.debit),
                    3 => widgets::amount_label(ui, r.credit),
                    4 => widgets::amount_label(ui, r.end),
                    _ => {}
                }
            });
            return;
        }
        if self.mode == 2 {
            let rows = self.qty_rows.clone();
            let cols = [
                widgets::TCol::new("科目编码", 110.0).fixed(),
                widgets::TCol::new("科目名称", 180.0),
                widgets::TCol::new("期初数量", 110.0).right(),
                widgets::TCol::new("入库数量", 110.0).right(),
                widgets::TCol::new("出库数量", 110.0).right(),
                widgets::TCol::new("期末数量", 110.0).right(),
                widgets::TCol::new("期初金额", 120.0).right(),
                widgets::TCol::new("借方金额", 120.0).right(),
                widgets::TCol::new("贷方金额", 120.0).right(),
                widgets::TCol::new("期末金额", 120.0).right(),
            ];
            widgets::grid(ui, "qty_balance", &cols, rows.len(), 24.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => { ui.label(RichText::new(&r.account_code).monospace()); }
                    1 => { ui.label(&r.account_name); }
                    2 => { ui.label(RichText::new(r.qty_begin.fmt_qty()).monospace()); }
                    3 => { ui.label(RichText::new(r.qty_in.fmt_qty()).monospace()); }
                    4 => { ui.label(RichText::new(r.qty_out.fmt_qty()).monospace()); }
                    5 => { ui.label(RichText::new(r.qty_end.fmt_qty()).monospace()); }
                    6 => widgets::amount_label(ui, r.amount_begin),
                    7 => widgets::amount_label(ui, r.amount_debit),
                    8 => widgets::amount_label(ui, r.amount_credit),
                    9 => widgets::amount_label(ui, r.amount_end),
                    _ => {}
                }
            });
            return;
        }
        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("科目编码", 110.0).fixed(),
            widgets::TCol::new("科目名称", 220.0),
            widgets::TCol::new("期初借方", 130.0).right(),
            widgets::TCol::new("期初贷方", 130.0).right(),
            widgets::TCol::new("本期借方", 130.0).right(),
            widgets::TCol::new("本期贷方", 130.0).right(),
            widgets::TCol::new("本年累计借", 130.0).right(),
            widgets::TCol::new("本年累计贷", 130.0).right(),
            widgets::TCol::new("期末借方", 130.0).right(),
            widgets::TCol::new("期末贷方", 130.0).right(),
        ];
        widgets::grid(ui, "balance_table", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            let (bd, ba) = signed_to_dir_amount(r.begin);
            let (ed, ea) = signed_to_dir_amount(r.end());
            match c {
                0 => { ui.label(RichText::new(&r.account_code).monospace()); }
                1 => { ui.label(&r.account_name); }
                2 => {
                    if bd == fincore::Direction::Debit && !ba.is_zero() {
                        widgets::amount_label(ui, ba);
                    }
                }
                3 => {
                    if bd == fincore::Direction::Credit && !ba.is_zero() {
                        widgets::amount_label(ui, ba);
                    }
                }
                4 => widgets::amount_label(ui, r.debit),
                5 => widgets::amount_label(ui, r.credit),
                6 => widgets::amount_label(ui, r.ytd_debit),
                7 => widgets::amount_label(ui, r.ytd_credit),
                8 => {
                    if ed == fincore::Direction::Debit && !ea.is_zero() {
                        widgets::amount_label(ui, ea);
                    }
                }
                9 => {
                    if ed == fincore::Direction::Credit && !ea.is_zero() {
                        widgets::amount_label(ui, ea);
                    }
                }
                _ => {}
            }
        });

        // 合计行
        let mut sbd = Money::ZERO;
        let mut sbc = Money::ZERO;
        let mut sd = Money::ZERO;
        let mut sc = Money::ZERO;
        let mut syd = Money::ZERO;
        let mut syc = Money::ZERO;
        let mut sed = Money::ZERO;
        let mut sec = Money::ZERO;
        for r in &rows {
            let (bd, ba) = signed_to_dir_amount(r.begin);
            let (ed, ea) = signed_to_dir_amount(r.end());
            if bd == fincore::Direction::Debit {
                sbd += ba;
            } else {
                sbc += ba;
            }
            sd += r.debit;
            sc += r.credit;
            syd += r.ytd_debit;
            syc += r.ytd_credit;
            if ed == fincore::Direction::Debit {
                sed += ea;
            } else {
                sec += ea;
            }
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("合计").strong());
            ui.add_space(10.0);
            for (t, v) in [
                ("期初借", sbd),
                ("期初贷", sbc),
                ("本期借", sd),
                ("本期贷", sc),
                ("累计借", syd),
                ("累计贷", syc),
                ("期末借", sed),
                ("期末贷", sec),
            ] {
                ui.label(format!("{} {}", t, v.fmt_money()));
                ui.add_space(8.0);
            }
            if let Some((bd, bc, _, _, ed, ec)) = self.trial {
                let ok1 = (bd - bc).round2().is_zero();
                let ok2 = (ed - ec).round2().is_zero();
                ui.label(
                    RichText::new(if ok1 && ok2 {
                        "✔ 试算平衡"
                    } else {
                        "✖ 试算不平衡"
                    })
                    .color(if ok1 && ok2 {
                        palette::OK
                    } else {
                        palette::CREDIT
                    })
                    .strong(),
                );
            }
        });
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        if self.mode == 1 {
            if self.aux_rows.is_empty() {
                ctx.error("没有可导出的数据");
                return;
            }
            let mut sh = crate::views::export::Sheet::new(
                "辅助账",
                vec![
                    self.aux_kind.label().into(),
                    "期初（带符号）".into(),
                    "本期借方".into(),
                    "本期贷方".into(),
                    "期末（带符号）".into(),
                ],
            );
            for r in &self.aux_rows {
                sh.push(vec![
                    r.key.clone(),
                    r.begin.fmt_plain(),
                    r.debit.fmt_plain(),
                    r.credit.fmt_plain(),
                    r.end.fmt_plain(),
                ]);
            }
            let file_name = format!("{}辅助账_{}_{}", self.aux_kind.label(), self.from, self.to);
            let title = format!("{}辅助账（{} ~ {}）", self.aux_kind.label(), self.from, self.to);
            match crate::views::export::run_export(&sh, &file_name, &title, mode) {
                Ok(m) => ctx.info(m),
                Err(e) => ctx.error(e),
            }
            return;
        }
        if self.mode == 2 {
            if self.qty_rows.is_empty() {
                ctx.error("没有可导出的数据");
                return;
            }
            let mut sh = crate::views::export::Sheet::new(
                "数量金额账",
                vec![
                    "科目编码".into(),
                    "科目名称".into(),
                    "期初数量".into(),
                    "入库数量".into(),
                    "出库数量".into(),
                    "期末数量".into(),
                    "期初金额".into(),
                    "借方金额".into(),
                    "贷方金额".into(),
                    "期末金额".into(),
                ],
            );
            for r in &self.qty_rows {
                sh.push(vec![
                    r.account_code.clone(),
                    r.account_name.clone(),
                    r.qty_begin.fmt_qty(),
                    r.qty_in.fmt_qty(),
                    r.qty_out.fmt_qty(),
                    r.qty_end.fmt_qty(),
                    r.amount_begin.fmt_plain(),
                    r.amount_debit.fmt_plain(),
                    r.amount_credit.fmt_plain(),
                    r.amount_end.fmt_plain(),
                ]);
            }
            let file_name = format!("数量金额账_{}_{}", self.from, self.to);
            let title = format!("数量金额账（{} ~ {}）", self.from, self.to);
            match crate::views::export::run_export(&sh, &file_name, &title, mode) {
                Ok(m) => ctx.info(m),
                Err(e) => ctx.error(e),
            }
            return;
        }
        if self.rows.is_empty() {
            ctx.error("没有可导出的数据");
            return;
        }
        let mut sh = crate::views::export::Sheet::new(
            "科目余额表",
            vec![
                "科目编码".into(),
                "科目名称".into(),
                "期初借方".into(),
                "期初贷方".into(),
                "本期借方".into(),
                "本期贷方".into(),
                "本年累计借方".into(),
                "本年累计贷方".into(),
                "期末借方".into(),
                "期末贷方".into(),
            ],
        );
        for r in &self.rows {
            let (bd, ba) = signed_to_dir_amount(r.begin);
            let (ed, ea) = signed_to_dir_amount(r.end());
            sh.push(vec![
                r.account_code.clone(),
                r.account_name.clone(),
                if bd == fincore::Direction::Debit {
                    ba.fmt_plain()
                } else {
                    String::new()
                },
                if bd == fincore::Direction::Credit {
                    ba.fmt_plain()
                } else {
                    String::new()
                },
                r.debit.fmt_plain(),
                r.credit.fmt_plain(),
                r.ytd_debit.fmt_plain(),
                r.ytd_credit.fmt_plain(),
                if ed == fincore::Direction::Debit {
                    ea.fmt_plain()
                } else {
                    String::new()
                },
                if ed == fincore::Direction::Credit {
                    ea.fmt_plain()
                } else {
                    String::new()
                },
            ]);
        }
        let file_name = format!("科目余额表_{}_{}", self.from, self.to);
        let title = format!("科目余额表（{} ~ {}）", self.from, self.to);
        match crate::views::export::run_export(&sh, &file_name, &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}
