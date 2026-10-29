//! 往来核销与账龄分析
//!
//! 核销是单据级的：一张应收单可以分多次收款核销，一笔收款也可以冲掉多张单。
//! 所以界面上永远是"挑一条借方 + 挑一条贷方 + 填本次金额"，而不是"整客户结清"。
//! 自动核销只做保守配对，剩下的交给人判断——错勾一笔比漏勾一笔麻烦得多。

use chrono::NaiveDate;
use egui::{RichText, Ui};
use fincore::engine::aging::{self, AgingBucket, AgingLine};
use fincore::{Money, Period, Perm};
use findb::offset::ArapOffset;
use findb::settle::{AutoSettleResult, OpenEntry, SettleRecord};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettleTab {
    Manual,
    Auto,
    Offset,
    Aging,
    /// 往来对账单：期初 + 本期流水 + 期末，发给客户回签
    Statement,
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
    /// 撤销核销时要撤的金额（留空 = 整条撤销该记录）
    pub unsettle_amount: String,
    /// 借方（被核销方）选中行
    pub sel_from: Option<i64>,
    /// 贷方（核销方）选中行
    pub sel_to: Option<i64>,
    pub amount: String,
    /// 差异（现金折扣 / 抹零）金额：手工核销时把零头记入差异科目
    pub diff: String,
    /// 差异科目（默认财务费用-其他；营业外支出等可改）
    pub diff_account: String,
    /// 跨单位/科目互抵：借方与贷方的科目或往来单位可以不同（代收代付 / 三方抵账），
    /// 核销时自动出一张过渡凭证把两边接上
    pub cross: bool,
    /// 互抵的对方往来科目（如 1122 ↔ 2202；同科目跨单位时留空）
    pub cross_account: String,
    /// 互抵过渡凭证日期（`YYYY-MM-DD`）
    pub cross_date: String,
    pub auto: Option<AutoSettleResult>,
    pub tolerance: String,
    /// 往来对冲单列表 + 录单字段
    pub offsets: Vec<ArapOffset>,
    pub of_date: String,
    pub of_customer: String,
    pub of_supplier: String,
    pub of_ar: String,
    pub of_ap: String,
    pub of_amount: String,
    pub of_memo: String,
    /// 账龄基准日（`YYYY-MM-DD`）
    pub as_of: String,
    pub scheme: AgingScheme,
    pub buckets: Vec<AgingBucket>,
    pub lines: Vec<AgingLine>,
    /// 6 档坏账计提比例（文本缓冲）
    pub rates: Vec<String>,
    /// 往来对账单
    pub statements: Vec<findb::statement::Statement>,
    /// 对账单类型：ar / ap
    pub st_kind: String,
    /// 对账单往来单位编码
    pub st_party: String,
    /// 对账单起始期间 / 截止期间
    pub st_from: String,
    pub st_to: String,
    pub st_memo: String,
    /// 账龄下钻：选中的往来对象（`customer=C01` 形式的 aux_key）
    pub drill_key: Option<String>,
    /// 账龄下钻：该对象还没核销完的分录
    pub drill_rows: Vec<OpenEntry>,
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
            unsettle_amount: String::new(),
            sel_from: None,
            sel_to: None,
            amount: String::new(),
            diff: String::new(),
            // 660305 财务费用-其他：内置科目表里带「部门」核算的是 6603 一级本身，
            // 明细 660305 不强制部门，抹零能直接出凭证，不用先补部门。
            diff_account: "660305".to_string(),
            cross: false,
            cross_account: String::new(),
            cross_date: String::new(),
            auto: None,
            tolerance: "0.01".to_string(),
            offsets: Vec::new(),
            of_date: String::new(),
            of_customer: String::new(),
            of_supplier: String::new(),
            of_ar: String::new(),
            of_ap: String::new(),
            of_amount: String::new(),
            of_memo: String::new(),
            as_of: String::new(),
            scheme: AgingScheme::ByDays,
            buckets: aging::buckets_by_days(),
            lines: Vec::new(),
            rates: aging::default_bad_debt_rates()
                .iter()
                .map(|m| m.fmt_plain())
                .collect(),
            drill_key: None,
            drill_rows: Vec::new(),
            confirm_auto: false,
            statements: Vec::new(),
            st_kind: "ar".to_string(),
            st_party: String::new(),
            st_from: String::new(),
            st_to: String::new(),
            st_memo: String::new(),
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
        if self.of_date.is_empty() {
            self.of_date = chrono::Local::now().date_naive().to_string();
        }
        if self.cross_date.is_empty() {
            self.cross_date = chrono::Local::now().date_naive().to_string();
        }
        if self.st_from.is_empty() {
            // 默认从建账期到这个月：小厂的账通常从启用月份开始
            self.st_from = ctx.db().options().start_period.code();
        }
        if self.st_to.is_empty() {
            self.st_to = ctx.period().code();
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
            "{}|{:?}|{}|{}|{}|{}|{}|{}|{}|{}",
            ctx.period().ymm(),
            self.tab,
            self.account,
            upto.ymm(),
            self.only_open,
            self.as_of,
            self.scheme.idx(),
            self.cross,
            self.cross_account,
            self.st_kind
        );
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        self.buckets = self.scheme.buckets();
        // include_all 与"只显示未核销完"正好相反
        let mut entries =
            findb::settle::open_entries(ctx.db(), &self.account, upto, !self.only_open)
                .unwrap_or_default();
        // 「跨单位/科目互抵」：把对方科目的未核销清单合并进来，才能同时挑到两侧
        // （同科目跨单位时对方科目留空即可——当前科目的借/贷两行本就在同一张表里）
        let a2 = self.cross_account.trim();
        if self.tab == SettleTab::Manual && self.cross && !a2.is_empty() && a2 != self.account {
            let more = findb::settle::open_entries(ctx.db(), a2, upto, !self.only_open)
                .unwrap_or_default();
            for e in more {
                if !entries.iter().any(|x| x.entry_id == e.entry_id) {
                    entries.push(e);
                }
            }
            entries.sort_by_key(|a| (a.date, a.no, a.line));
        }
        self.entries = entries;
        self.records = findb::settle::list(ctx.db(), &self.account).unwrap_or_default();
        self.offsets = findb::offset::offset_list(ctx.db()).unwrap_or_default();

        if self.tab == SettleTab::Aging {
            let as_of = self.as_of_date(upto);
            self.lines = findb::settle::aging(ctx.db(), &self.account, upto, as_of, &self.buckets, None)
                .unwrap_or_default();
        }
        if self.tab == SettleTab::Statement {
            let kind = self.st_kind.clone();
            self.statements = findb::statement::list(ctx.db(), Some(&kind), None).unwrap_or_default();
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
            ui.label(RichText::new(format!("{acct} {acct_name} · 截至 {}", upto.label())).weak());
        });

        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, SettleTab::Manual, "手工核销");
            ui.selectable_value(&mut self.tab, SettleTab::Auto, "自动核销");
            ui.selectable_value(&mut self.tab, SettleTab::Offset, "往来对冲");
            ui.selectable_value(&mut self.tab, SettleTab::Aging, "账龄分析");
            ui.selectable_value(&mut self.tab, SettleTab::Statement, "对账单");
            ui.separator();
            ui.label("科目");
            let before = self.account.clone();
            widgets::account_combo(
                ui,
                "settle_account",
                &mut self.account,
                ctx.chart(),
                false,
                240.0,
            );
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
            let r = ui.add_sized(
                [84.0, 22.0],
                egui::TextEdit::singleline(&mut self.upto_text),
            );
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
            SettleTab::Offset => self.show_offset(ctx, ui),
            SettleTab::Aging => self.show_aging(ctx, ui),
            SettleTab::Statement => self.show_statement(ctx, ui),
        }
    }

    fn clear_sel(&mut self) {
        self.sel_from = None;
        self.sel_to = None;
        self.amount.clear();
        self.diff.clear();
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
                 普通核销要求同科目、同往来单位、不超额、不自核自；\
                 若是「A 的付款冲 B 的货款」这类跨往来单位 / 跨科目的场景，勾选下面的「跨单位互抵」。",
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
                    if ui
                        .selectable_label(sel, if sel { "✔" } else { "选" })
                        .clicked()
                    {
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
            widgets::money_input(ui, &mut self.amount, 120.0);
            ui.separator();
            ui.label("差异(抹零)").on_hover_text(
                "实收/实付与挂账的零头。填了会自动出一张「借 差异科目 / 贷 应收」\
                  （或反向）的凭证，被核销方一次结清，零头落到损益。",
            );
            widgets::money_input(ui, &mut self.diff, 90.0);
            ui.label("差异科目");
            ui.add_sized(
                [90.0, 22.0],
                egui::TextEdit::singleline(&mut self.diff_account).hint_text("660305"),
            );
            ui.separator();
            ui.label(RichText::new(&info).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("核销").clicked() && ctx.can(Perm::VoucherNew) {
                    go = true;
                }
                ui.separator();
                ui.checkbox(&mut self.cross, "跨单位/科目互抵")
                    .on_hover_text(
                        "A 的付款冲 B 的货款、客户应收抵供应商应付（三方抵账）等——\
                     借方与贷方的科目或往来单位可以不同，核销时自动出一张过渡凭证把两边接上；\
                     同科目同往来单位时仍走普通核销、不出过渡凭证。",
                    );
                if self.cross {
                    ui.separator();
                    ui.label("对方科目");
                    ui.add_sized(
                        [80.0, 22.0],
                        egui::TextEdit::singleline(&mut self.cross_account).hint_text("如 2202"),
                    );
                    ui.label("过渡凭证日期");
                    ui.add_sized(
                        [96.0, 22.0],
                        egui::TextEdit::singleline(&mut self.cross_date).hint_text("YYYY-MM-DD"),
                    );
                }
            });
        });
        if go {
            if self.cross {
                self.do_cross(ctx);
            } else {
                self.do_settle(ctx);
            }
        }
    }

    /// 跨单位 / 跨科目互抵：数据层自动出过渡凭证并各自核销，这里只负责把话说清楚。
    fn do_cross(&mut self, ctx: &mut AppCtx<'_>) {
        let (Some(f), Some(t)) = (self.sel_from, self.sel_to) else {
            ctx.error("请先在借方和贷方各选一行");
            return;
        };
        if Money::parse(self.diff.trim())
            .map(|d| d.is_positive())
            .unwrap_or(false)
        {
            ctx.error("跨单位互抵不填差异；请先互抵，再单独用「差异核销」抹零");
            return;
        }
        let Ok(amt) = Money::parse(self.amount.trim()) else {
            ctx.error("互抵金额格式不正确");
            return;
        };
        let date = NaiveDate::parse_from_str(self.cross_date.trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| chrono::Local::now().date_naive());
        let who = ctx.user().display_name.clone();
        match findb::settle::settle_cross(ctx.db(), f, t, amt, date, &who) {
            Ok(r) => {
                match r.bridge_voucher_id {
                    Some(vid) => {
                        ctx.log(
                            "往来",
                            "跨单位互抵",
                            &format!("#{f} ↔ #{t} {amt}（过渡凭证 #{vid}）"),
                        );
                        ctx.info(format!(
                            "已互抵 {}，过渡凭证 #{vid} 请到凭证列表记账",
                            amt.fmt_money()
                        ));
                    }
                    None => {
                        ctx.log("往来", "手工核销", &format!("#{f} ↔ #{t} {amt}"));
                        ctx.info(format!("已核销 {}", amt.fmt_money()));
                    }
                }
                self.clear_sel();
                self.dirty = true;
            }
            Err(e) => ctx.error(e.to_string()),
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
        let diff = if self.diff.trim().is_empty() {
            Money::ZERO
        } else {
            match Money::parse(self.diff.trim()) {
                Ok(d) => d,
                Err(_) => {
                    ctx.error("差异金额格式不正确");
                    return;
                }
            }
        };
        let who = ctx.user().display_name.clone();
        let diff_acct = self.diff_account.trim().to_string();
        // 数据层会拒掉自核自、跨科目、跨往来单位、超额与差异校验，这里只负责把错误说出来
        match findb::settle::settle_with_diff(ctx.db(), f, t, amt, diff, &diff_acct, &who) {
            Ok(r) => {
                if diff.is_positive() {
                    ctx.log(
                        "往来",
                        "差异核销",
                        &format!(
                            "#{f} ↔ #{t} {amt}（差异 {diff} 记入 {diff_acct}，凭证 #{:?}）",
                            r.diff_voucher_id
                        ),
                    );
                    ctx.info(format!(
                        "已核销 {}，差异 {} 记入 {}",
                        amt.fmt_money(),
                        diff.fmt_money(),
                        diff_acct
                    ));
                } else {
                    ctx.log("往来", "手工核销", &format!("#{f} ↔ #{t} {amt}"));
                    ctx.info(format!("已核销 {}", amt.fmt_money()));
                }
                self.clear_sel();
                self.dirty = true;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn show_records(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("已核销记录").strong());
            ui.add_space(12.0);
            ui.label("撤销金额");
            ui.add(
                egui::TextEdit::singleline(&mut self.unsettle_amount)
                    .desired_width(110.0)
                    .hint_text("留空=整条"),
            );
            ui.label(RichText::new("先填金额再点「取消」，可只撤销一部分").weak());
        });
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
                7 if ui.small_button("取消").clicked() => {
                    drop = Some(r.id);
                }
                _ => {}
            }
        });
        self.records = rows;

        if let Some(id) = drop {
            if ctx.can(Perm::VoucherNew) {
                let raw = self.unsettle_amount.trim().to_string();
                if raw.is_empty() {
                    let r = findb::settle::unsettle(ctx.db(), id);
                    if ctx.handle(r).is_some() {
                        ctx.log("往来", "取消核销", &format!("#{id}"));
                        ctx.info("已取消该笔核销");
                        self.dirty = true;
                    }
                } else {
                    match Money::parse(&raw) {
                        Ok(amt) => {
                            let r = findb::settle::unsettle_part(ctx.db(), id, amt);
                            if let Some(left) = ctx.handle(r) {
                                ctx.log("往来", "部分撤销核销", &format!("#{id} 撤销 {amt}"));
                                ctx.info(format!("已撤销 {}，余 {}", amt.fmt_money(), left.fmt_money()));
                                self.dirty = true;
                            }
                        }
                        Err(e) => ctx.error(format!("撤销金额「{raw}」不是合法金额：{e}")),
                    }
                }
                self.unsettle_amount.clear();
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
    // ------------------------- 往来对冲 -------------------------
    fn show_offset(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let mut create = false;
        widgets::card(
            ui,
            "往来对冲单（应收 ↔ 应付相互抵销）",
            |ui| {
                ui.label(
                    RichText::new(
                        "某公司既欠我们（应收）、我们又欠它（应付）时，月末轧差只结净额。\
                     录单 → 审核：系统出「借 应付 / 贷 应收」凭证，并对两端各自按先进先出核销，\
                     两边往来同时变清，差额（净额）继续挂账。对冲额不得超过任一端未核销合计。",
                    )
                    .weak(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("日期");
                    ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.of_date));
                    ui.label("客户(应收)");
                    ui.add_sized(
                        [110.0, 22.0],
                        egui::TextEdit::singleline(&mut self.of_customer),
                    );
                    ui.label("供应商(应付)");
                    ui.add_sized(
                        [110.0, 22.0],
                        egui::TextEdit::singleline(&mut self.of_supplier),
                    );
                    ui.label("对冲金额");
                    widgets::money_input(ui, &mut self.of_amount, 110.0);
                });
                ui.horizontal(|ui| {
                    ui.label("应收科目");
                    widgets::account_combo(ui, "of_ar", &mut self.of_ar, ctx.chart(), false, 170.0);
                    ui.label("应付科目");
                    widgets::account_combo(ui, "of_ap", &mut self.of_ap, ctx.chart(), false, 170.0);
                    ui.label(RichText::new("（留空取账套默认应收 / 应付）").weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                ctx.can(Perm::VoucherNew),
                                egui::Button::new(
                                    RichText::new("新增对冲单").color(egui::Color32::WHITE),
                                )
                                .fill(palette::PRIMARY),
                            )
                            .clicked()
                        {
                            create = true;
                        }
                    });
                });
            },
        );

        if create {
            self.do_create_offset(ctx);
        }

        ui.add_space(8.0);
        self.show_offset_list(ctx, ui);
    }

    fn do_create_offset(&mut self, ctx: &mut AppCtx<'_>) {
        let Ok(date) = NaiveDate::parse_from_str(self.of_date.trim(), "%Y-%m-%d") else {
            ctx.error("日期格式应为 YYYY-MM-DD");
            return;
        };
        let Ok(amount) = Money::parse(self.of_amount.trim()) else {
            ctx.error("对冲金额格式不正确");
            return;
        };
        let who = ctx.user().display_name.clone();
        match findb::offset::offset_create(
            ctx.db(),
            date,
            &self.of_customer,
            &self.of_supplier,
            &self.of_ar,
            &self.of_ap,
            amount,
            &self.of_memo,
            &who,
        ) {
            Ok(id) => {
                ctx.log("往来", "新增往来对冲单（待审核）", &format!("#{id}"));
                ctx.info("已新增对冲单；审核后生成凭证并两端自动核销");
                self.of_customer.clear();
                self.of_supplier.clear();
                self.of_amount.clear();
                self.of_memo.clear();
                self.dirty = true;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn show_offset_list(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.label(RichText::new("对冲单").strong());
        let rows = std::mem::take(&mut self.offsets);
        let cols = [
            widgets::TCol::new("日期", 100.0).fixed(),
            widgets::TCol::new("单号", 150.0),
            widgets::TCol::new("客户(应收)", 120.0),
            widgets::TCol::new("供应商(应付)", 120.0),
            widgets::TCol::new("金额", 120.0).right(),
            widgets::TCol::new("状态", 80.0),
            widgets::TCol::new("操作", 220.0),
        ];
        let mut act: Option<(i64, &'static str)> = None;
        widgets::grid(ui, "arap_offsets", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => {
                    ui.label(r.date.to_string());
                }
                1 => {
                    ui.label(&r.no);
                }
                2 => {
                    ui.label(&r.customer);
                }
                3 => {
                    ui.label(&r.supplier);
                }
                4 => widgets::amount_label(ui, r.amount),
                5 => {
                    ui.label(if r.status == "audited" {
                        "已审核"
                    } else {
                        "待审核"
                    });
                }
                6 => {
                    if r.status == "draft"
                        && ui
                            .add_enabled(
                                ctx.can(Perm::VoucherNew),
                                egui::Button::new("审核").small(),
                            )
                            .clicked()
                    {
                        act = Some((r.id, "audit"));
                    }
                    if r.status == "audited"
                        && ui
                            .add_enabled(
                                ctx.can(Perm::VoucherAudit) || ctx.can(Perm::VoucherPost),
                                egui::Button::new("撤审").small(),
                            )
                            .clicked()
                    {
                        act = Some((r.id, "unaudit"));
                    }
                    if r.status != "audited"
                        && ui
                            .add_enabled(
                                ctx.can(Perm::VoucherNew),
                                egui::Button::new("删除").small(),
                            )
                            .clicked()
                    {
                        act = Some((r.id, "delete"));
                    }
                }
                _ => {}
            }
        });
        self.offsets = rows;

        if let Some((id, op)) = act {
            let who = ctx.user().display_name.clone();
            let res: Result<String, String> = match op {
                "audit" => match findb::offset::offset_audit(ctx.db(), id, &who) {
                    Ok((vid, n)) => Ok(format!("已审核：凭证 #{vid}，两端核销 {n} 笔")),
                    Err(e) => Err(e.to_string()),
                },
                "unaudit" => match findb::offset::offset_unaudit(ctx.db(), id) {
                    Ok(()) => Ok("已撤销审核（凭证与核销已回退）".to_string()),
                    Err(e) => Err(e.to_string()),
                },
                _ => match findb::offset::offset_delete(ctx.db(), id) {
                    Ok(()) => Ok("已删除".to_string()),
                    Err(e) => Err(e.to_string()),
                },
            };
            match res {
                Ok(msg) => {
                    let act_name = match op {
                        "audit" => "审核往来对冲单",
                        "unaudit" => "撤销审核往来对冲单",
                        _ => "删除往来对冲单",
                    };
                    ctx.log("往来", act_name, &format!("#{id}"));
                    ctx.info(msg);
                    self.dirty = true;
                }
                Err(e) => ctx.error(e),
            }
        }
    }

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
        // `AgingLine.key` 是**裸编码** `C01`，而 `aux_name` 吃的是 `customer=C01`
        // 形式的 aux_key —— 直接传会查不到名称，账龄表第一列全是编码。
        // 维度按科目的真实辅助核算取（2203 预收挂客户），见 `settle::party_kind_in`。
        let party = findb::settle::party_kind_in(ctx.db(), &self.account);
        let names: Vec<String> = lines
            .iter()
            .map(|l| Self::aux_name(ctx, &findb::balances::aux_key_frag(&l.key, party)))
            .collect();
        // 下钻要的是 `customer=C01` 形式的 aux_key，表格显示用的是名称；
        // 两列内容算一次，别在渲染循环里重复拼字符串。
        let aux_keys: Vec<String> = lines
            .iter()
            .map(|l| findb::balances::aux_key_frag(&l.key, party))
            .collect();
        let buckets = self.buckets.clone();
        let mut cols = vec![widgets::TCol::new("往来单位", 190.0)];
        for b in &buckets {
            cols.push(widgets::TCol::new(b.label, 120.0).right());
        }
        cols.push(widgets::TCol::new("合计", 130.0).right());
        cols.push(widgets::TCol::new("贷方余额（预收/预付）", 160.0).right());
        cols.push(widgets::TCol::new("最长账龄", 100.0).right());
        cols.push(widgets::TCol::new("逾期", 110.0).right());

        let bucket_n = buckets.len();
        widgets::grid(ui, "settle_aging", &cols, lines.len(), 24.0, |i, c, ui| {
            let l = &lines[i];
            if c == 0 {
                let picked = self.drill_key.as_deref() == Some(aux_keys[i].as_str());
                if ui
                    .selectable_label(picked, &names[i])
                    .on_hover_text("点一下看这个对象还没核销的单据")
                    .clicked()
                {
                    self.drill_key = Some(aux_keys[i].clone());
                    self.drill_rows = match findb::settle::open_entries_with(
                        ctx.db(),
                        &self.account,
                        upto,
                        false,
                        true,
                        Some(&aux_keys[i]),
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            ctx.toast(format!("取未核销单据失败：{e}"));
                            Vec::new()
                        }
                    };
                }
            } else if c <= bucket_n {
                widgets::amount_label(ui, l.amounts.get(c - 1).copied().unwrap_or(Money::ZERO));
            } else if c == bucket_n + 1 {
                widgets::amount_label(ui, l.total);
            } else if c == bucket_n + 2 {
                widgets::amount_label(ui, l.credit_total);
            } else if c == bucket_n + 3 {
                ui.label(format!("{} 天", l.max_days));
            } else if c == bucket_n + 4 {
                // 「逾期」与「账龄」不是一回事：账龄 54 天、账期 30 天 → 逾期 24 天。
                // 没填账期的客户只能说账龄，不能说逾期（红线提示会催错人）。
                match l.pay_days {
                    None => {
                        ui.label(RichText::new("未设账期").weak());
                    }
                    Some(_) if l.overdue_days == 0 => {
                        ui.label(RichText::new("未到期").color(palette::OK));
                    }
                    Some(_) => {
                        ui.label(
                            RichText::new(format!("逾期 {} 天", l.overdue_days))
                                .color(palette::WARN),
                        );
                    }
                }
            }
        });

        // 表尾列合计
        let totals = aging::column_totals(&lines, &buckets);
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("各区间合计：").strong());
            for (i, t) in totals.iter().enumerate() {
                ui.label(format!("{} {}", buckets[i].label, t.fmt_money()));
                ui.add_space(10.0);
            }
            let grand: Money = totals.iter().copied().fold(Money::ZERO, |a, b| a + b);
            ui.label(RichText::new(format!("借方合计 {}", grand.fmt_money())).strong());
        });

        ui.add_space(10.0);
        self.show_drill(ui, upto);
        ui.add_space(10.0);
        self.show_bad_debt(ctx, ui, &lines, &buckets);
    }

    /// 账龄下钻：某个往来对象还没核销的单据。
    ///
    /// 账龄表只回答「最老 148 天」，答不了「这 5 万是哪几张单、各挂多久」——
    /// 而这正是催款时第一句要问的。口径与核销页一致（只认已记账、未核销），
    /// 所以不会出现「账龄里有、核销页找不到」。
    fn show_drill(&self, ui: &mut Ui, upto: Period) {
        let Some(key) = self.drill_key.as_deref() else {
            return;
        };
        let as_of = upto.last_day();
        widgets::card(ui, &format!("未核销单据　{key}"), |ui| {
            if self.drill_rows.is_empty() {
                ui.label(
                    RichText::new("这笔没有未核销单据（可能已核销完，或只是期初挂账）").weak(),
                );
                return;
            }
            let total: Money = self.drill_rows.iter().map(|e| e.open()).sum();
            ui.label(
                RichText::new(format!(
                    "共 {} 笔，未核销合计 {}",
                    self.drill_rows.len(),
                    total.fmt_money()
                ))
                .strong(),
            );
            let cols = vec![
                widgets::TCol::new("日期", 100.0),
                widgets::TCol::new("凭证号", 110.0),
                widgets::TCol::new("摘要", 240.0),
                widgets::TCol::new("借方", 120.0).right(),
                widgets::TCol::new("贷方", 120.0).right(),
                widgets::TCol::new("已核销", 110.0).right(),
                widgets::TCol::new("未核销", 110.0).right(),
                widgets::TCol::new("账龄", 80.0).right(),
            ];
            let rows = &self.drill_rows;
            widgets::grid(ui, "settle_drill", &cols, rows.len(), 24.0, |i, c, ui| {
                let e = &rows[i];
                match c {
                    0 => {
                        ui.label(e.date.format("%Y-%m-%d").to_string());
                    }
                    1 => {
                        ui.label(format!("{}-{:04}", e.word, e.no));
                    }
                    2 => {
                        ui.label(&e.summary);
                    }
                    3 => widgets::amount_label(ui, e.debit),
                    4 => widgets::amount_label(ui, e.credit),
                    5 => widgets::amount_label(ui, e.settled),
                    6 => widgets::amount_label(ui, e.open()),
                    _ => {
                        ui.label(format!("{} 天", (as_of - e.date).num_days().max(0)));
                    }
                }
            });
        });
    }

    /// 坏账准备测算：比例可改，结果只用于参考
    fn show_bad_debt(
        &mut self,
        ctx: &mut AppCtx<'_>,
        ui: &mut Ui,
        lines: &[AgingLine],
        buckets: &[AgingBucket],
    ) {
        widgets::card(ui, "坏账准备测算", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, r) in self.rates.iter_mut().enumerate() {
                    let label = buckets.get(i).map(|b| b.label).unwrap_or("—");
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
                match findb::settle::bad_debt_provision_voucher(
                    ctx.db(),
                    period,
                    date,
                    &ctx.user().username,
                ) {
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

impl SettleView {
    /// 往来对账单：期初 + 本期流水 + 期末，发给客户回签。
    ///
    /// 与「催款单」的区别见 `findb::statement` 模块头：催款单是某一时点的欠款快照，
    /// 对账单是某期间的**完整流水**（含已核销），客户盖章确认后即为对账闭环。
    /// 小厂客户几个月才确认一次，靠的就是这个——每一期发出去、等回签，
    /// 没有回签的就一直挂在"已发出"里，一眼能看出跟谁还没对上。
    fn show_statement(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let mut create = false;

        widgets::card(ui, "往来对账单（发客户对账 → 回签确认）", |ui| {
            ui.label(
                RichText::new(
                    "对账单是某期间的完整流水：期初余额 + 本期增加 − 本期减少 = 期末余额，\
                     逐行带滚动余额。生成时快照落库，之后改凭证、再核销都不影响已发出的对账单。\
                     客户回签后才算对上——没回签的会一直挂在「已发出」里，提醒你去问。",
                )
                .weak(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("类型");
                ui.selectable_value(&mut self.st_kind, "ar".to_string(), "应收对账单");
                ui.selectable_value(&mut self.st_kind, "ap".to_string(), "应付对账单");
                ui.separator();
                ui.label(if self.st_kind == "ar" { "客户" } else { "供应商" });
                ui.add_sized(
                    [110.0, 22.0],
                    egui::TextEdit::singleline(&mut self.st_party)
                        .hint_text(if self.st_kind == "ar" { "C001" } else { "S001" }),
                    );
                ui.separator();
                ui.label("期间");
                ui.add_sized(
                    [84.0, 22.0],
                    egui::TextEdit::singleline(&mut self.st_from).hint_text("2026-11"),
                );
                ui.label("至");
                ui.add_sized(
                    [84.0, 22.0],
                    egui::TextEdit::singleline(&mut self.st_to).hint_text("2026-11"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("备注");
                ui.add_sized(
                    [300.0, 22.0],
                    egui::TextEdit::singleline(&mut self.st_memo)
                        .hint_text("如：截止 XX 月往来明细，请核对后签章回传"),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            ctx.can(Perm::VoucherNew),
                            egui::Button::new(
                                RichText::new("生成对账单").color(egui::Color32::WHITE),
                            )
                            .fill(palette::PRIMARY),
                        )
                        .clicked()
                    {
                        create = true;
                    }
                });
            });
            ui.label(
                RichText::new(
                    "客商编码在对账单里用的是档案编码（客户 C001…、供应商 S001…），\
                     科目留空则取账套默认应收 / 应付。",
                )
                .weak()
                .size(12.0),
            );
        });

        if create {
            self.do_create_statement(ctx);
        }

        ui.add_space(8.0);
        self.show_statement_list(ctx, ui);
    }

    fn do_create_statement(&mut self, ctx: &mut AppCtx<'_>) {
        let party = self.st_party.trim().to_string();
        if party.is_empty() {
            ctx.error(if self.st_kind == "ar" {
                "请填写客户编码（如 C001）"
            } else {
                "请填写供应商编码（如 S001）"
            });
            return;
        }
        let Ok(from) = Period::parse(self.st_from.trim()) else {
            ctx.error("起始期间格式应为 YYYY-MM");
            return;
        };
        let Ok(to) = Period::parse(self.st_to.trim()) else {
            ctx.error("截止期间格式应为 YYYY-MM");
            return;
        };
        // 名称从辅助档案取，取不到就退回编码——对账单上写编码总比空白好
        let kind = if self.st_kind == "ar" {
            fincore::AuxKind::Customer
        } else {
            fincore::AuxKind::Supplier
        };
        let party_name = findb::auxs::list(ctx.db(), &fincore::AuxQuery::kind(kind))
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.code == party)
            .map(|e| e.name)
            .unwrap_or_else(|| party.clone());
        let account = if self.st_kind == "ar" {
            ctx.db().options().biz_accounts.ar.clone()
        } else {
            ctx.db().options().biz_accounts.ap.clone()
        };
        let who = ctx.user().display_name.clone();
        match findb::statement::create(
            ctx.db(),
            &self.st_kind,
            &account,
            &party,
            &party_name,
            from,
            to,
            &self.st_memo,
            &who,
        ) {
            Ok(st) => {
                ctx.log(
                    "往来",
                    "生成往来对账单",
                    &format!("{} {} {}~{}", st.no, st.party_name, st.period_from.code(), st.period_to.code()),
                );
                ctx.info(format!(
                    "已生成对账单 {}：期末余额 {}（正=对方欠我方），发出后等客户回签",
                    st.no,
                    st.end_balance.fmt_money()
                ));
                self.st_party.clear();
                self.st_memo.clear();
                self.dirty = true;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn show_statement_list(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let mut action: Option<(i64, &'static str)> = None;
        let rows = std::mem::take(&mut self.statements);
        let mut detail: Option<findb::statement::Statement> = None;
        {
            let cols = [
                widgets::TCol::new("单号", 130.0).fixed(),
                widgets::TCol::new("往来单位", 140.0),
                widgets::TCol::new("期间", 130.0),
                widgets::TCol::new("期初", 100.0).right(),
                widgets::TCol::new("本期增加", 100.0).right(),
                widgets::TCol::new("本期减少", 100.0).right(),
                widgets::TCol::new("期末", 100.0).right(),
                widgets::TCol::new("状态", 90.0),
                widgets::TCol::new("回签人", 90.0),
                widgets::TCol::new("操作", 210.0),
            ];
            widgets::grid(ui, "statement_list", &cols, rows.len(), 26.0, |i, c, ui| {
                let Some(st) = rows.get(i) else { return };
                match c {
                    0 => {
                        ui.label(&st.no);
                    }
                    1 => {
                        ui.label(format!("{} {}", st.party_code, st.party_name));
                    }
                    2 => {
                        ui.label(format!("{}~{}", st.period_from.code(), st.period_to.code()));
                    }
                    3 => widgets::amount_label(ui, st.begin_balance),
                    4 => widgets::amount_label(ui, st.period_increase),
                    5 => widgets::amount_label(ui, st.period_decrease),
                    6 => widgets::report_amount(ui, st.end_balance, true),
                    7 => {
                        // 已发出未回签的用警示色，一眼看出跟谁还没对上
                        let txt = RichText::new(st.status_label());
                        ui.label(match st.status.as_str() {
                            "sent" => txt.color(palette::WARN),
                            "confirmed" => txt.color(palette::OK),
                            "cancelled" => txt.weak(),
                            _ => txt,
                        });
                    }
                    8 => {
                        ui.label(if st.confirmed_by.is_empty() {
                            "-".to_string()
                        } else {
                            st.confirmed_by.clone()
                        });
                    }
                    9 => {
                        ui.horizontal(|ui| {
                            if ui.small_button("明细").clicked() {
                                detail = Some(st.clone());
                            }
                            if st.status == "draft"
                                && ui
                                    .add_enabled(ctx.can(Perm::VoucherNew), egui::Button::new("发出").small())
                                    .clicked()
                            {
                                action = Some((st.id, "sent"));
                            }
                            if matches!(st.status.as_str(), "draft" | "sent")
                                && ui
                                    .add_enabled(
                                        ctx.can(Perm::VoucherNew),
                                        egui::Button::new("回签确认").small(),
                                    )
                                    .clicked()
                            {
                                action = Some((st.id, "confirmed"));
                            }
                            if matches!(st.status.as_str(), "draft" | "sent")
                                && ui.small_button("作废").clicked()
                            {
                                action = Some((st.id, "cancelled"));
                            }
                        });
                    }
                    _ => {}
                }
            });
        }
        self.statements = rows;

        if let Some((id, status)) = action {
            let who = ctx.user().display_name.clone();
            match findb::statement::set_status(ctx.db(), id, status, &who) {
                Ok(()) => {
                    ctx.info(format!(
                        "对账单已变更为「{}」",
                        findb::statement::status_label(status)
                    ));
                    ctx.log("往来", "对账单状态变更", &format!("#{id} → {status}"));
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }


        if let Some(st) = detail {
            self.show_statement_detail(ctx, ui, &st);
        }
    }
}

impl SettleView {
    /// 对账单明细弹窗：期初 + 逐笔流水（带滚动余额）+ 期末。
    ///
    /// 这张表就是打印给客户签字的那张——所以金额正负口径要一眼看懂：
    /// **正 = 对方欠我方**，负数 = 我方欠对方（预收/预付）。
    fn show_statement_detail(&mut self, _ctx: &AppCtx<'_>, ui: &mut Ui, st: &findb::statement::Statement) {
        let mut open = true;
        egui::Window::new(format!("{} {}", st.kind_label(), st.no))
            .collapsible(false)
            .resizable(true)
            .default_width(760.0)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                egui::Grid::new("st_head")
                    .num_columns(4)
                    .spacing([18.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("往来单位").weak());
                        ui.label(format!("{} {}", st.party_code, st.party_name));
                        ui.label(RichText::new("状态").weak());
                        ui.label(RichText::new(st.status_label()).strong());
                        ui.end_row();
                        ui.label(RichText::new("期间").weak());
                        ui.label(format!(
                            "{} 至 {}",
                            st.period_from.code(),
                            st.period_to.code()
                        ));
                        ui.label(RichText::new("科目").weak());
                        ui.label(&st.account);
                        ui.end_row();
                        ui.label(RichText::new("生成人").weak());
                        ui.label(format!("{} {}", st.created_by, st.created_at));
                        ui.label(RichText::new("回签").weak());
                        if st.confirmed_by.is_empty() {
                            ui.label(RichText::new("未回签").weak());
                        } else {
                            ui.label(format!("{} {}", st.confirmed_by, st.confirmed_at));
                        }
                        ui.end_row();
                    });
                if !st.memo.is_empty() {
                    ui.add_space(4.0);
                    ui.label(format!("备注：{}", st.memo));
                }
                ui.separator();

                let cols = [
                    widgets::TCol::new("日期", 100.0).fixed(),
                    widgets::TCol::new("凭证号", 140.0),
                    widgets::TCol::new("摘要", 240.0),
                    widgets::TCol::new("增加", 100.0).right(),
                    widgets::TCol::new("减少", 100.0).right(),
                    widgets::TCol::new("余额", 110.0).right(),
                ];
                // 明细行 + 首行期初 + 末行合计
                let lines = &st.lines;
                widgets::grid(
                    ui,
                    "st_detail_grid",
                    &cols,
                    lines.len() + 2,
                    24.0,
                    |i, c, ui| {
                        if i == 0 {
                            // 期初行
                            match c {
                                0 => {
                                    ui.label(RichText::new("期初").weak());
                                }
                                1 => {
                                    ui.label("");
                                }
                                2 => {
                                    ui.label(RichText::new("上期结转").weak());
                                }
                                3 => {
                                    ui.label("");
                                }
                                4 => {
                                    ui.label("");
                                }
                                5 => widgets::amount_label(ui, st.begin_balance),
                                _ => {}
                            }
                            return;
                        }
                        if i == lines.len() + 1 {
                            // 合计行
                            match c {
                                0 => {
                                    ui.label(RichText::new("合计").strong());
                                }
                                1 => {
                                    ui.label("");
                                }
                                2 => {
                                    ui.label(RichText::new(format!("共 {} 笔", st.line_count)).weak());
                                }
                                3 => widgets::report_amount(ui, st.period_increase, true),
                                4 => widgets::report_amount(ui, st.period_decrease, true),
                                5 => widgets::report_amount(ui, st.end_balance, true),
                                _ => {}
                            }
                            return;
                        }
                        let Some(ln) = lines.get(i - 1) else { return };
                        match c {
                            0 => {
                                ui.label(&ln.date);
                            }
                            1 => {
                                ui.label(&ln.doc_no);
                            }
                            2 => {
                                ui.label(&ln.summary).on_hover_text(&ln.summary);
                            }
                            3 => widgets::amount_label(ui, ln.increase),
                            4 => widgets::amount_label(ui, ln.decrease),
                            5 => widgets::amount_label(ui, ln.balance),
                            _ => {}
                        }
                    },
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "余额正数 = 对方欠我方，负数 = 我方欠对方（预收/预付）。\
                         本表为生成时快照，之后改凭证不影响。",
                    )
                    .weak()
                    .size(12.0),
                );
            });
    }
}
