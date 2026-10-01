//! 往来核销与账龄分析
//!
//! 核销是单据级的：一张应收单可以分多次收款核销，一笔收款也可以冲掉多张单。
//! 所以界面上永远是"挑一条借方 + 挑一条贷方 + 填本次金额"，而不是"整客户结清"。
//! 自动核销只做保守配对，剩下的交给人判断——错勾一笔比漏勾一笔麻烦得多。

use chrono::NaiveDate;
use egui::{RichText, Ui};
use fincore::engine::aging::{self, AgingBucket, AgingLine};
use fincore::{Money, Period, Perm};
use findb::settle::{AutoSettleResult, OpenEntry, SettleRecord};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettleTab {
    Manual,
    Auto,
    Aging,
}

/// 账龄区间方案
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgingScheme {
    ByDays,
    ByYear,
}

impl AgingScheme {
    fn idx(self) -> usize {
        match self {
            AgingScheme::ByDays => 0,
            AgingScheme::ByYear => 1,
        }
    }
    fn label(self) -> &'static str {
        match self {
            AgingScheme::ByDays => "按天数分档",
            AgingScheme::ByYear => "按年度分档",
        }
    }
    fn buckets(self) -> Vec<AgingBucket> {
        match self {
            AgingScheme::ByDays => aging::buckets_by_days(),
            AgingScheme::ByYear => aging::buckets_by_year(),
        }
    }
}

/// 工具栏上的快捷科目（往来科目就那么几个，点一下比在下拉里翻快）
const COMMON: &[(&str, &str)] = &[
    ("1122", "应收账款"),
    ("2202", "应付账款"),
    ("1221", "其他应收款"),
    ("2203", "预收账款"),
];

pub struct SettleView {
    pub tab: SettleTab,
    pub account: String,
    /// 截止期间（只统计该期间及以前的已记账凭证）
    pub upto_text: String,
    /// 只显示未核销完的分录
    pub only_open: bool,
    pub entries: Vec<OpenEntry>,
    pub records: Vec<SettleRecord>,
    /// 借方（被核销方）选中行
    pub sel_from: Option<i64>,
    /// 贷方（核销方）选中行
    pub sel_to: Option<i64>,
    pub amount: String,
    pub auto: Option<AutoSettleResult>,
    pub tolerance: String,
    /// 账龄基准日（`YYYY-MM-DD`）
    pub as_of: String,
    pub scheme: AgingScheme,
    pub buckets: Vec<AgingBucket>,
    pub lines: Vec<AgingLine>,
    /// 6 档坏账计提比例（文本缓冲）
    pub rates: Vec<String>,
    /// 自动核销的二次确认
    pub confirm_auto: bool,
    pub dirty: bool,
    key: String,
}

impl Default for SettleView {
    fn default() -> Self {
        Self {
            tab: SettleTab::Manual,
            account: "1122".to_string(),
            upto_text: String::new(),
            only_open: true,
            entries: Vec::new(),
            records: Vec::new(),
            sel_from: None,
            sel_to: None,
            amount: String::new(),
            auto: None,
            tolerance: "0.01".to_string(),
            as_of: String::new(),
            scheme: AgingScheme::ByDays,
            buckets: aging::buckets_by_days(),
            lines: Vec::new(),
            rates: aging::default_bad_debt_rates()
                .iter()
                .map(|m| m.fmt_plain())
                .collect(),
            confirm_auto: false,
            dirty: true,
            key: String::new(),
        }
    }
}

impl SettleView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.upto_text.is_empty() {
            self.upto_text = ctx.period().code();
        }
        if self.account.is_empty() {
            self.account = "1122".to_string();
        }
        if self.as_of.is_empty() {
            self.as_of = self.upto(ctx).last_day().to_string();
        }
        self.dirty = true;
    }

    fn upto(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(self.upto_text.trim()).unwrap_or_else(|_| ctx.period())
    }

    fn as_of_date(&self, upto: Period) -> NaiveDate {
        NaiveDate::parse_from_str(self.as_of.trim(), "%Y-%m-%d").unwrap_or_else(|_| upto.last_day())
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let upto = self.upto(ctx);
        // 缓存 key 按"期间 + Tab + 科目 + 截止期间 + 过滤条件"组，避免每帧重查
        let key = format!(
            "{}|{:?}|{}|{}|{}|{}|{}",
            ctx.period().ymm(),
            self.tab,
            self.account,
            upto.ymm(),
            self.only_open,
            self.as_of,
            self.scheme.idx()
        );
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        self.buckets = self.scheme.buckets();
        // include_all 与"只显示未核销完"正好相反
        self.entries =
            findb::settle::open_entries(ctx.db(), &self.account, upto, !self.only_open)
                .unwrap_or_default();
        self.records = findb::settle::list(ctx.db(), &self.account).unwrap_or_default();

        if self.tab == SettleTab::Aging {
            let as_of = self.as_of_date(upto);
            self.lines =
                findb::settle::aging(ctx.db(), &self.account, upto, as_of, &self.buckets)
                    .unwrap_or_default();
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let upto = self.upto(ctx);
        let acct = self.account.clone();
        let acct_name = ctx
            .chart()
            .get(&acct)
            .map(|a| a.name.clone())
            .unwrap_or_default();

        widgets::page_header(ui, "往来核销", |ui| {
            ui.label(
                RichText::new(format!("{acct} {acct_name} · 截至 {}", upto.label())).weak(),
            );
        });

        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, SettleTab::Manual, "手工核销");
            ui.selectable_value(&mut self.tab, SettleTab::Auto, "自动核销");
            ui.selectable_value(&mut self.tab, SettleTab::Aging, "账龄分析");
            ui.separator();
            ui.label("科目");
            let before = self.account.clone();
            widgets::account_combo(ui, "settle_account", &mut self.account, ctx.chart(), false, 240.0);
            if self.account != before {
                self.clear_sel();
                self.dirty = true;
            }
            for (code, name) in COMMON {
                if ui.small_button(*code).on_hover_text(*name).clicked() {
                    self.account = (*code).to_string();
                    self.clear_sel();
                    self.dirty = true;
                }
            }
            ui.separator();
            ui.label("截止期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.upto_text));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("上期").clicked() {
                self.upto_text = upto.prev().code();
                self.dirty = true;
            }
            if ui.button("下期").clicked() {
                self.upto_text = upto.next().code();
                self.dirty = true;
            }
            ui.checkbox(&mut self.only_open, "只显示未核销完");
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        match self.tab {
            SettleTab::Manual => self.show_manual(ctx, ui),
            SettleTab::Auto => self.show_auto(ctx, ui),
            SettleTab::Aging => self.show_aging(ctx, ui),
        }
    }

    fn clear_sel(&mut self) {
        self.sel_from = None;
        self.sel_to = None;
        self.amount.clear();
        self.auto = None;
    }

    /// 往来单位显示名。`aux_key` 形如 `customer=C01`，而名称缓存的键是 `customer:C01`
    fn aux_name(ctx: &AppCtx<'_>, key: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        for seg in key.split('\u{1f}') {
            let Some((k, v)) = seg.split_once('=') else {
                continue;
            };
            let name = ctx
                .st
                .aux_names
                .get(&format!("{k}:{v}"))
                .cloned()
                .unwrap_or_default();
            parts.push(if name.is_empty() { v.to_string() } else { name });
        }
        if parts.is_empty() {
            key.to_string()
        } else {
            parts.join(" / ")
        }
    }

    // ------------------------- 手工核销 -------------------------
    fn show_manual(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.label(
            RichText::new(
                "上方挑一笔借方（应收 / 预付），下方挑一笔贷方（收款 / 付款），填好本次核销金额后核销。\
                 超核销、跨科目、跨往来单位、自核自都会被拒绝。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        ui.label(RichText::new("借方未核销").strong());
        self.show_open_table(ctx, ui, true);
        ui.add_space(10.0);
        ui.label(RichText::new("贷方未核销").strong());
        self.show_open_table(ctx, ui, false);
        ui.add_space(8.0);

        self.show_settle_bar(ctx, ui);
        ui.add_space(10.0);
        self.show_records(ctx, ui);
    }

    fn show_open_table(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, debit_side: bool) {
        let rows: Vec<OpenEntry> = self
            .entries
            .iter()
            .filter(|e| {
                if debit_side {
                    e.debit > Money::ZERO
                } else {
                    e.credit > Money::ZERO
                }
            })
            .cloned()
            .collect();
        // 名称查表需要借 ctx，而表格闭包要借 self，所以先把显示名算成一列平行数组
        let names: Vec<String> = rows
            .iter()
            .map(|e| Self::aux_name(ctx, &e.aux_key))
            .collect();

        let cols = [
            widgets::TCol::new("选", 44.0).fixed(),
            widgets::TCol::new("日期", 100.0).fixed(),
            widgets::TCol::new("凭证", 100.0).fixed(),
            widgets::TCol::new("摘要", 200.0),
            widgets::TCol::new("往来单位", 170.0),
            widgets::TCol::new("金额", 140.0).right(),
            widgets::TCol::new("已核销", 130.0).right(),
            widgets::TCol::new("未核销", 130.0).right(),
        ];
        let id = if debit_side {
            "settle_open_d"
        } else {
            "settle_open_c"
        };
        widgets::grid(ui, id, &cols, rows.len(), 24.0, |i, c, ui| {
            let e = &rows[i];
            match c {
                0 => {
                    let sel = if debit_side {
                        self.sel_from == Some(e.entry_id)
                    } else {
                        self.sel_to == Some(e.entry_id)
                    };
                    if ui.selectable_label(sel, if sel { "✔" } else { "选" }).clicked() {
                        let next = if sel { None } else { Some(e.entry_id) };
                        if debit_side {
                            self.sel_from = next;
                        } else {
                            self.sel_to = next;
                        }
                        self.sync_amount();
                    }
                }
                1 => {
                    ui.label(e.date.to_string());
                }
                2 => {
                    ui.label(format!("{}-{}", e.word, e.no));
                }
                3 => {
                    ui.label(&e.summary);
                }
                4 => {
                    ui.label(&names[i]);
                }
                5 => {
                    let v = if debit_side { e.debit } else { e.credit };
                    ui.label(format!("{} {}", e.dir_label(), v.fmt_money()));
                }
                6 => widgets::amount_label(ui, e.settled),
                7 => widgets::amount_label(ui, e.open()),
                _ => {}
            }
        });
    }

    /// 两边都选好时，默认核销额取较小的未核销额（不能超核销）
    fn sync_amount(&mut self) {
        let a = self
            .sel_from
            .and_then(|id| self.entries.iter().find(|e| e.entry_id == id))
            .map(OpenEntry::open);
        let b = self
            .sel_to
            .and_then(|id| self.entries.iter().find(|e| e.entry_id == id))
            .map(OpenEntry::open);
        if let (Some(x), Some(y)) = (a, b) {
            self.amount = x.min(y).fmt_plain();
        } else {
            self.amount.clear();
        }
    }

    fn show_settle_bar(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let from = self.sel_from;
        let to = self.sel_to;
        let info = match (from, to) {
            (Some(f), Some(t)) => {
                let fo = self
                    .entries
                    .iter()
                    .find(|e| e.entry_id == f)
                    .map(OpenEntry::open)
                    .unwrap_or(Money::ZERO);
                let to_open = self
                    .entries
                    .iter()
                    .find(|e| e.entry_id == t)
                    .map(OpenEntry::open)
                    .unwrap_or(Money::ZERO);
                format!(
                    "分录 #{f}（未核销 {}） ↔ #{t}（未核销 {}）",
                    fo.fmt_money(),
                    to_open.fmt_money()
                )
            }
            _ => "请在借方与贷方各选择一行".to_string(),
        };

        let mut go = false;
        widgets::toolbar(ui, |ui| {
            ui.label("本次核销金额");
            widgets::money_input(ui, &mut self.amount, 140.0);
            ui.separator();
            ui.label(RichText::new(&info).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("核销").clicked() && ctx.can(Perm::VoucherNew) {
                    go = true;
                }
            });
        });
        if go {
            self.do_settle(ctx);
        }
    }

    fn do_settle(&mut self, ctx: &mut AppCtx<'_>) {
        let (Some(f), Some(t)) = (self.sel_from, self.sel_to) else {
            ctx.error("请先在借方和贷方各选一行");
            return;
        };
        let Ok(amt) = Money::parse(self.amount.trim()) else {
            ctx.error("核销金额格式不正确");
            return;
        };
        let who = ctx.user().display_name.clone();
        // 数据层会拒掉自核自、跨科目、跨往来单位与超核销，这里只负责把错误说出来
        match findb::settle::settle(ctx.db(), f, t, amt, &who) {
            Ok(_) => {
                ctx.log("往来", "手工核销", &format!("#{f} ↔ #{t} {amt}"));
                ctx.info(format!("已核销 {}", amt.fmt_money()));
                self.clear_sel();
                self.dirty = true;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn show_records(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.separator();
        ui.label(RichText::new("已核销记录").strong());
        let rows = std::mem::take(&mut self.records);
        let names: Vec<String> = rows
            .iter()
            .map(|r| Self::aux_name(ctx, &r.aux_key))
            .collect();
        let cols = [
            widgets::TCol::new("期间", 90.0).fixed(),
            widgets::TCol::new("被核销分录", 110.0).right(),
            widgets::TCol::new("核销方分录", 110.0).right(),
            widgets::TCol::new("往来单位", 170.0),
            widgets::TCol::new("金额", 130.0).right(),
            widgets::TCol::new("操作人", 100.0),
            widgets::TCol::new("核销时间", 165.0),
            widgets::TCol::new("操作", 90.0).fixed(),
        ];
        let mut drop: Option<i64> = None;
        widgets::grid(ui, "settle_records", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => {
                    ui.label(r.period.label());
                }
                1 => {
                    ui.label(r.from_entry.to_string());
                }
                2 => {
                    ui.label(r.to_entry.to_string());
                }
                3 => {
                    ui.label(&names[i]);
                }
                4 => widgets::amount_label(ui, r.amount),
                5 => {
                    ui.label(&r.settled_by);
                }
                6 => {
                    ui.label(RichText::new(&r.settled_at).weak());
                }
                7 => {
                    if ui.small_button("取消").clicked() {
                        drop = Some(r.id);
                    }
                }
                _ => {}
            }
        });
        self.records = rows;

        if let Some(id) = drop {
            if ctx.can(Perm::VoucherNew) {
                let r = findb::settle::unsettle(ctx.db(), id);
                if ctx.handle(r).is_some() {
                    ctx.log("往来", "取消核销", &format!("#{id}"));
                    ctx.info("已取消该笔核销");
                    self.dirty = true;
                }
            }
        }
    }

    // ------------------------- 自动核销 -------------------------
    fn show_auto(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::card(ui, "自动核销", |ui| {
            ui.label(
                RichText::new(
                    "先按「同往来单位 + 金额完全相等」精确配对，再按「同往来单位 + 先进先出」逐笔冲销，\
                     尾差在容差内自动核销。匹配不上的留空，交给手工。",
                )
                .weak(),
            );
            ui.horizontal(|ui| {
                ui.label("容差金额");
                widgets::money_input(ui, &mut self.tolerance, 120.0);
                ui.label(RichText::new("余额小于该值的尾差直接抹平").weak());
            });
            ui.add_space(6.0);
            if ui.button("执行自动核销").clicked() && ctx.can(Perm::VoucherNew) {
                self.confirm_auto = true;
            }
            if let Some(r) = &self.auto {
                ui.add_space(8.0);
                ui.separator();
                widgets::kv(ui, "配对笔数", &r.pairs.to_string());
                widgets::kv(ui, "核销金额", &r.amount.fmt_money());
                widgets::kv(ui, "其中精确配对", &r.exact.to_string());
                widgets::kv(ui, "其中尾差核销", &r.written_off.to_string());
            }
        });

        if !self.confirm_auto {
            return;
        }
        let upto = self.upto(ctx);
        let acct = self.account.clone();
        let tol = Money::parse_or_zero(self.tolerance.trim());
        let mut open = true;
        let mut close = false;
        let mut yes = false;
        egui::Window::new("执行自动核销")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.add_space(6.0);
                ui.label(format!(
                    "将对 {acct} 截至 {} 的未核销分录执行自动核销（容差 {}）。\
                     建议先备份账套，核销记录可逐笔取消。",
                    upto.label(),
                    tol.fmt_money()
                ));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("确定执行").color(egui::Color32::WHITE),
                                )
                                .fill(palette::PRIMARY),
                            )
                            .clicked()
                        {
                            yes = true;
                        }
                    });
                });
            });
        if yes {
            self.confirm_auto = false;
            self.do_auto_settle(ctx, upto, &acct, tol);
        } else if close || !open {
            self.confirm_auto = false;
        }
    }

    fn do_auto_settle(&mut self, ctx: &mut AppCtx<'_>, upto: Period, acct: &str, tol: Money) {
        let who = ctx.user().display_name.clone();
        match findb::settle::auto_settle(ctx.db(), acct, upto, tol, &who) {
            Ok(r) => {
                ctx.log(
                    "往来",
                    "自动核销",
                    &format!("{acct} {} 配对{}笔 {}", upto.label(), r.pairs, r.amount),
                );
                ctx.info(format!(
                    "自动核销完成：{} 笔，共 {}",
                    r.pairs,
                    r.amount.fmt_money()
                ));
                self.auto = Some(r);
                self.clear_sel();
                self.dirty = true;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    // ------------------------- 账龄分析 -------------------------
    fn show_aging(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let upto = self.upto(ctx);
        widgets::toolbar(ui, |ui| {
            ui.label("账龄基准日");
            let r = ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.as_of));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("取期末最后一天").clicked() {
                self.as_of = upto.last_day().to_string();
                self.dirty = true;
            }
            ui.separator();
            ui.label("区间方案");
            let before = self.scheme;
            egui::ComboBox::from_id_salt("settle_aging_scheme")
                .selected_text(self.scheme.label())
                .width(140.0)
                .show_ui(ui, |ui| {
                    for s in [AgingScheme::ByDays, AgingScheme::ByYear] {
                        ui.selectable_value(&mut self.scheme, s, s.label());
                    }
                });
            if self.scheme != before {
                self.dirty = true;
            }
        });

        ui.label(
            RichText::new(
                "账龄按单据逐笔计算：同一往来单位的新单与老单分别落在不同区间，\
                 贷方余额（预收 / 预付）单独列示，不混进账龄区间。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        let lines = self.lines.clone();
        let names: Vec<String> = lines.iter().map(|l| Self::aux_name(ctx, &l.key)).collect();
        let buckets = self.buckets.clone();
        let mut cols = vec![widgets::TCol::new("往来单位", 190.0)];
        for b in &buckets {
            cols.push(widgets::TCol::new(b.label, 120.0).right());
        }
        cols.push(widgets::TCol::new("合计", 130.0).right());
        cols.push(widgets::TCol::new("贷方余额（预收/预付）", 160.0).right());
        cols.push(widgets::TCol::new("最长账龄", 100.0).right());

        let bucket_n = buckets.len();
        widgets::grid(ui, "settle_aging", &cols, lines.len(), 24.0, |i, c, ui| {
            let l = &lines[i];
            if c == 0 {
                ui.label(&names[i]);
            } else if c <= bucket_n {
                widgets::amount_label(ui, l.amounts.get(c - 1).copied().unwrap_or(Money::ZERO));
            } else if c == bucket_n + 1 {
                widgets::amount_label(ui, l.total);
            } else if c == bucket_n + 2 {
                widgets::amount_label(ui, l.credit_total);
            } else if c == bucket_n + 3 {
                ui.label(format!("{} 天", l.max_days));
            }
        });

        // 表尾列合计
        let totals = aging::column_totals(&lines, &buckets);
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("各区间合计：").strong());
            for (i, t) in totals.iter().enumerate() {
                ui.label(format!(
                    "{} {}",
                    buckets[i].label,
                    t.fmt_money()
                ));
                ui.add_space(10.0);
            }
            let grand: Money = totals.iter().copied().fold(Money::ZERO, |a, b| a + b);
            ui.label(RichText::new(format!("借方合计 {}", grand.fmt_money())).strong());
        });

        ui.add_space(10.0);
        self.show_bad_debt(ctx, ui, &lines, &buckets);
    }

    /// 坏账准备测算：比例可改，结果只用于参考
    fn show_bad_debt(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, lines: &[AgingLine], buckets: &[AgingBucket]) {
        widgets::card(ui, "坏账准备测算", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, r) in self.rates.iter_mut().enumerate() {
                    let label = buckets
                        .get(i)
                        .map(|b| b.label)
                        .unwrap_or("—");
                    ui.label(label);
                    ui.add_sized([70.0, 22.0], egui::TextEdit::singleline(r));
                    ui.add_space(8.0);
                }
            });
            let rates: Vec<Money> = self
                .rates
                .iter()
                .map(|s| Money::parse_or_zero(s.trim()))
                .collect();
            let prov = aging::bad_debt_provision(lines, &rates);
            widgets::kv(ui, "应计提坏账准备", &prov.fmt_money());
            ui.add_space(6.0);
            if ui.button("正式计提坏账准备").clicked() && ctx.can(Perm::VoucherNew) {
                let period = self.upto(ctx);
                let date = chrono::Local::now().date_naive();
                match findb::settle::bad_debt_provision_voucher(ctx.db(), period, date, &ctx.user().username) {
                    Ok(Some(id)) => {
                        ctx.info(format!("已生成坏账准备凭证 #{id}"));
                        ctx.log("往来", "计提坏账准备", &format!("凭证 #{id}"));
                        self.dirty = true;
                    }
                    Ok(None) => ctx.info("无可计提的坏账准备"),
                    Err(e) => ctx.error(e.to_string()),
                }
            }
            ui.label(
                RichText::new("按账龄区间比例测算；点击「正式计提」自动生成「借 资产减值损失 / 贷 坏账准备」凭证。").weak(),
            );
        });
    }
}
