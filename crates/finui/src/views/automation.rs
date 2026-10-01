//! 月末自动化：期末调汇、自动转账、月度检查清单、一键月末处理
//!
//! 这四块是每月固定要跑一遍的动作，塞在一个界面里按顺序走完，省得在几个菜单之间来回跳。
//! 界面只做三件事：取数、试算、把结果交给 `findb::automation` 生成凭证。
//! 像折旧计提、结转损益这些别处已经实现的功能，这里只显示金额 + 跳转，不重复造一遍。

use egui::{RichText, Ui};
use findb::automation::{
    at_delete, at_insert, at_list, at_preview_all, at_run, at_update, checklist, fx_adjust, fx_calc,
    fx_delete, fx_list, fx_missing, fx_set, AutoTransfer, CheckItem, EntryDir, FxAdjLine, FxRate,
    SrcKind, TransferPreview, FX_GAIN_ACCOUNT,
};
use findb::balances::BalanceQuery;
use fincore::{Money, Period, Perm};

use crate::state::{AppCtx, NavItem};
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Fx,
    Transfer,
    Checklist,
    Wizard,
}

/// 汇率编辑表单。汇率在文本框里是"半截数字"很常见（刚敲了 `7.`），
/// 所以用字符串缓冲，点保存时再解析，不让半成品进库。
#[derive(Clone)]
struct FxForm {
    period: String,
    currency: String,
    rate: String,
    is_new: bool,
}

impl FxForm {
    fn new(p: Period) -> Self {
        Self {
            period: p.code(),
            currency: String::new(),
            rate: String::new(),
            is_new: true,
        }
    }
    fn of(r: &FxRate) -> Self {
        Self {
            period: r.period.code(),
            currency: r.currency.clone(),
            rate: r.rate.fmt_plain(),
            is_new: false,
        }
    }
}

/// 自动转账规则编辑表单。
/// 下拉框在 egui 里要的是字符串，枚举值只在保存时来回转换一次。
#[derive(Clone)]
struct AtForm {
    id: i64,
    is_new: bool,
    name: String,
    summary: String,
    memo: String,
    sort: i32,
    active: bool,
    src_account: String,
    src_aux: String,
    kind_txt: String,
    src_dir_txt: String,
    /// true=按比例，false=固定金额
    by_ratio: bool,
    ratio_txt: String,
    dst_account: String,
    dst_aux: String,
    dst_dir_txt: String,
    offset_account: String,
}

impl AtForm {
    fn new(sort: i32) -> Self {
        Self {
            id: 0,
            is_new: true,
            name: String::new(),
            summary: String::new(),
            memo: String::new(),
            sort,
            active: true,
            src_account: String::new(),
            src_aux: String::new(),
            kind_txt: SrcKind::End.label().to_string(),
            src_dir_txt: EntryDir::Auto.label().to_string(),
            by_ratio: true,
            ratio_txt: "1".to_string(),
            dst_account: String::new(),
            dst_aux: String::new(),
            dst_dir_txt: EntryDir::Debit.label().to_string(),
            offset_account: String::new(),
        }
    }

    fn of(r: &AutoTransfer) -> Self {
        Self {
            id: r.id,
            is_new: false,
            name: r.name.clone(),
            summary: r.summary.clone(),
            memo: r.memo.clone(),
            sort: r.sort,
            active: r.active,
            src_account: r.src_account.clone(),
            src_aux: r.src_aux.clone(),
            kind_txt: r.src_kind.label().to_string(),
            src_dir_txt: r.src_dir.label().to_string(),
            by_ratio: r.ratio_mode_is_ratio,
            ratio_txt: r.ratio.fmt_plain(),
            dst_account: r.dst_account.clone(),
            dst_aux: r.dst_aux.clone(),
            dst_dir_txt: r.dst_dir.label().to_string(),
            offset_account: r.offset_account.clone(),
        }
    }

    /// 表单回写规则（供 at_insert / at_update）
    fn apply_to(&self, r: &mut AutoTransfer) {
        r.name = self.name.trim().to_string();
        r.summary = self.summary.trim().to_string();
        r.memo = self.memo.trim().to_string();
        r.sort = self.sort;
        r.active = self.active;
        r.src_account = self.src_account.trim().to_string();
        r.src_aux = self.src_aux.trim().to_string();
        r.src_kind = SrcKind::ALL
            .iter()
            .copied()
            .find(|k| k.label() == self.kind_txt)
            .unwrap_or(SrcKind::End);
        r.src_dir = EntryDir::ALL
            .iter()
            .copied()
            .find(|d| d.label() == self.src_dir_txt)
            .unwrap_or(EntryDir::Auto);
        r.dst_account = self.dst_account.trim().to_string();
        r.dst_aux = self.dst_aux.trim().to_string();
        r.dst_dir = EntryDir::ALL
            .iter()
            .copied()
            .find(|d| d.label() == self.dst_dir_txt)
            .unwrap_or(EntryDir::Debit);
        r.offset_account = self.offset_account.trim().to_string();
        r.ratio_mode_is_ratio = self.by_ratio;
        r.ratio = Money::parse_or_zero(&self.ratio_txt);
    }
}

pub struct AutomationView {
    pub tab: Tab,
    pub period_text: String,
    // ---- Tab 1 期末调汇 ----
    pub fx_rates: Vec<FxRate>,
    pub fx_missing: Vec<String>,
    pub fx_lines: Vec<FxAdjLine>,
    pub fx_total: Money,
    fx_edit: Option<FxForm>,
    // ---- Tab 2 自动转账 ----
    pub rules: Vec<AutoTransfer>,
    pub previews: Vec<TransferPreview>,
    at_edit: Option<AtForm>,
    // ---- Tab 3 / 4 ----
    pub checks: Vec<CheckItem>,
    /// 本期尚未计提的折旧合计
    pub dep_amount: Money,
    pub dep_count: usize,
    /// 本期损益净额（向导第 4 步展示）
    pub pl_net: Money,
    /// 当前期间是否已结账
    pub closed: bool,
    pub dirty: bool,
    key: String,
}

impl Default for AutomationView {
    fn default() -> Self {
        Self {
            tab: Tab::Fx,
            period_text: String::new(),
            fx_rates: Vec::new(),
            fx_missing: Vec::new(),
            fx_lines: Vec::new(),
            fx_total: Money::ZERO,
            fx_edit: None,
            rules: Vec::new(),
            previews: Vec::new(),
            at_edit: None,
            checks: Vec::new(),
            dep_amount: Money::ZERO,
            dep_count: 0,
            pl_net: Money::ZERO,
            closed: false,
            dirty: true,
            key: String::new(),
        }
    }
}

impl AutomationView {
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

    /// 按「期间 + Tab」缓存：切期间或切页签自动重取，其余帧不查库。
    /// 各 Tab 只取自己要的数据，避免每次进界面都把折旧、余额快照全跑一遍。
    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}|{:?}", p.ymm(), self.tab);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        match self.tab {
            Tab::Fx => {
                self.fx_rates = fx_list(ctx.db(), p).unwrap_or_default();
                self.fx_missing = fx_missing(ctx.db(), p).unwrap_or_default();
                self.fx_lines = fx_calc(ctx.db(), p).unwrap_or_default();
                self.fx_total = self
                    .fx_lines
                    .iter()
                    .map(|l| l.diff)
                    .fold(Money::ZERO, |a, b| a + b);
            }
            Tab::Transfer => {
                self.rules = at_list(ctx.db()).unwrap_or_default();
                self.previews = at_preview_all(ctx.db(), p).unwrap_or_default();
            }
            Tab::Checklist => {
                self.checks = checklist(ctx.db(), p).unwrap_or_default();
            }
            Tab::Wizard => {
                self.checks = checklist(ctx.db(), p).unwrap_or_default();
                self.rules = at_list(ctx.db()).unwrap_or_default();
                self.previews = at_preview_all(ctx.db(), p).unwrap_or_default();
                self.load_dep(ctx, p);
                self.load_pl(ctx, p);
                self.closed = match findb::periods::closed_upto(ctx.db()).unwrap_or(None) {
                    Some(u) => p.is_closed(u),
                    None => false,
                };
            }
        }
    }

    /// 本期待计提折旧：台账取数 + 用 findb 自己的计划折旧函数算金额，
    /// 已计提过的卡片排除掉，折旧算法本身不在这里重复实现。
    fn load_dep(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let done: std::collections::HashSet<i64> = findb::assets::dep_list_period(ctx.db(), p)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.asset_id)
            .collect();
        let mut amount = Money::ZERO;
        let mut count = 0usize;
        for row in findb::assets::ledger(ctx.db(), p).unwrap_or_default() {
            if done.contains(&row.asset.id) {
                continue;
            }
            if let Ok(Some(m)) = findb::assets::planned_dep(&row.asset, p) {
                if !m.is_zero() {
                    amount += m;
                    count += 1;
                }
            }
        }
        self.dep_amount = amount;
        self.dep_count = count;
    }

    /// 本期损益净额，和【期末处理】口径一致（正数为净亏损）——含草稿，
    /// 见 period_end.rs 的口径说明
    fn load_pl(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let q = BalanceQuery::period(p).with_posted_only(false);
        match findb::balances::BalanceSnapshot::load(ctx.db(), &q) {
            Ok(s) => {
                let rows = s.profit_loss_rows(ctx.chart());
                self.pl_net = rows.iter().map(|r| r.end()).fold(Money::ZERO, |a, b| a + b);
            }
            Err(_) => self.pl_net = Money::ZERO,
        }
    }

    fn check_ok(&self, key: &str) -> bool {
        self.checks
            .iter()
            .find(|c| c.key == key)
            .map(|c| c.ok)
            .unwrap_or(false)
    }

    // ------------------------------ 主框架 ------------------------------
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "月末自动化", |ui| {
            let done = self.checks.iter().filter(|c| c.ok).count();
            if !self.checks.is_empty() {
                ui.label(
                    RichText::new(format!("检查清单 {}/{} 项通过", done, self.checks.len())).weak(),
                );
            }
        });

        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, Tab::Fx, "期末调汇");
            ui.selectable_value(&mut self.tab, Tab::Transfer, "自动转账");
            ui.selectable_value(&mut self.tab, Tab::Checklist, "月度检查清单");
            ui.selectable_value(&mut self.tab, Tab::Wizard, "一键月末处理");
            ui.separator();
            ui.label("期间");
            let r =
                ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
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
        });

        match self.tab {
            Tab::Fx => self.show_fx(ctx, ui, p),
            Tab::Transfer => self.show_transfer(ctx, ui, p),
            Tab::Checklist => self.show_checklist(ctx, ui, p),
            Tab::Wizard => self.show_wizard(ctx, ui, p),
        }
    }

    // ------------------------- Tab 1 期末调汇 -------------------------
    fn show_fx(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        widgets::card(ui, "外币汇率（1 单位外币 = ? 单位本位币）", |ui| {
            ui.horizontal(|ui| {
                if ui.button("新增汇率").clicked() && ctx.can(Perm::CarryForward) {
                    self.fx_edit = Some(FxForm::new(p));
                }
                ui.label(RichText::new("汇率按期间维护，不同期间的期末汇率互不影响").weak());
            });
            if !self.fx_missing.is_empty() {
                ui.colored_label(
                    palette::WARN,
                    format!("本期用到但未维护汇率：{}", self.fx_missing.join("、")),
                );
            }

            let rows = self.fx_rates.clone();
            let cols = [
                widgets::TCol::new("币种", 100.0).fixed(),
                widgets::TCol::new("期间", 100.0).fixed(),
                widgets::TCol::new("汇率", 140.0).right(),
                widgets::TCol::new("操作", 100.0).fixed(),
            ];
            let mut edit: Option<String> = None;
            let mut del: Option<String> = None;
            widgets::grid(ui, "fx_rates", &cols, rows.len(), 24.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => {
                        ui.label(RichText::new(&r.currency).monospace());
                    }
                    1 => {
                        ui.label(r.period.code());
                    }
                    2 => widgets::amount_label(ui, r.rate),
                    3 => {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if ui.small_button("改").clicked() {
                                edit = Some(r.currency.clone());
                            }
                            if ui.small_button("删").clicked() {
                                del = Some(r.currency.clone());
                            }
                        });
                    }
                    _ => {}
                }
            });

            if let Some(cur) = edit {
                if let Some(r) = self.fx_rates.iter().find(|r| r.currency == cur) {
                    self.fx_edit = Some(FxForm::of(r));
                }
            }
            if let Some(cur) = del {
                match fx_delete(ctx.db(), p, &cur) {
                    Ok(()) => {
                        ctx.log("期末", "删除汇率", &format!("{} {}", p.label(), cur));
                        ctx.info(format!("已删除 {} 的汇率", cur));
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });

        // 试算：只算不写库，用户先看差额再决定要不要出凭证
        widgets::card(ui, "本期汇兑损益测算", |ui| {
            if self.fx_lines.is_empty() {
                widgets::empty_hint(ui, "本期没有需要调汇的外币余额（或尚未维护期末汇率）");
                return;
            }
            let rows = self.fx_lines.clone();
            let names: Vec<String> = rows
                .iter()
                .map(|l| {
                    ctx.chart()
                        .get(&l.account_code)
                        .map(|a| a.name.clone())
                        .unwrap_or_default()
                })
                .collect();
            let cols = [
                widgets::TCol::new("科目", 220.0),
                widgets::TCol::new("往来", 140.0),
                widgets::TCol::new("币种", 70.0).fixed(),
                widgets::TCol::new("原币", 130.0).right(),
                widgets::TCol::new("账面本位币", 150.0).right(),
                widgets::TCol::new("重述后本位币", 150.0).right(),
                widgets::TCol::new("差额", 140.0).right(),
            ];
            widgets::grid(ui, "fx_calc", &cols, rows.len(), 24.0, |i, c, ui| {
                let l = &rows[i];
                match c {
                    0 => { ui.label(format!("{} {}", l.account_code, names[i])); }
                    1 => { ui.label(&l.aux_key); }
                    2 => { ui.label(&l.currency); }
                    3 => widgets::amount_label(ui, l.foreign),
                    4 => widgets::amount_label(ui, l.book),
                    5 => widgets::amount_label(ui, l.restated),
                    6 => widgets::amount_label(ui, l.diff),
                    _ => {}
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("合计差额：{}", self.fx_total.fmt_money())).strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(format!("生成调汇凭证（计入 {}）", FX_GAIN_ACCOUNT))
                        .clicked()
                        && ctx.can(Perm::CarryForward)
                    {
                        self.do_fx_adjust(ctx, p);
                    }
                });
            });
        });

        self.fx_window(ctx, ui);
    }

    fn fx_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(f) = self.fx_edit.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = f.is_new;

        egui::Window::new(if is_new { "新增汇率" } else { "修改汇率" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                egui::Grid::new("fx_edit")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("币种：");
                        ui.add_sized([160.0, 22.0], egui::TextEdit::singleline(&mut f.currency).hint_text("USD"));
                        ui.end_row();
                        ui.label("期间：");
                        ui.add_sized([160.0, 22.0], egui::TextEdit::singleline(&mut f.period).hint_text("202601"));
                        ui.end_row();
                        ui.label("汇率：");
                        ui.add_sized([160.0, 22.0], egui::TextEdit::singleline(&mut f.rate).hint_text("7.2000"));
                        ui.end_row();
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui.button("保存").clicked() {
                            save = true;
                        }
                    });
                });
            });

        if close || !open {
            self.fx_edit = None;
            return;
        }
        if save {
            self.save_fx(ctx);
        }
    }

    fn save_fx(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(f) = self.fx_edit.clone() else {
            return;
        };
        let cur = f.currency.trim().to_uppercase();
        if cur.is_empty() {
            ctx.error("币种不能为空");
            return;
        }
        let p = match Period::parse(&f.period) {
            Ok(p) => p,
            Err(e) => {
                ctx.error(format!("期间格式应为 YYYYMM：{e}"));
                return;
            }
        };
        let rate = match Money::parse(&f.rate) {
            Ok(m) => m,
            Err(e) => {
                ctx.error(format!("汇率不是合法数字：{e}"));
                return;
            }
        };
        match fx_set(ctx.db(), p, &cur, rate) {
            Ok(()) => {
                ctx.log(
                    "期末",
                    "维护汇率",
                    &format!("{} {} = {}", p.label(), cur, rate.fmt_money()),
                );
                ctx.info(format!("已保存 {} {} 的汇率 {}", p.label(), cur, rate.fmt_money()));
                self.dirty = true;
                self.fx_edit = None;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn do_fx_adjust(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let who = ctx.user().display_name.clone();
        match fx_adjust(ctx.db(), p, p.last_day(), FX_GAIN_ACCOUNT, &who) {
            Ok(Some(id)) => {
                ctx.log(
                    "期末",
                    "期末调汇",
                    &format!("{} 凭证#{id} 差额 {}", p.label(), self.fx_total.fmt_money()),
                );
                ctx.info(format!("已生成期末调汇凭证 #{id}"));
                self.dirty = true;
            }
            Ok(None) => ctx.info("本期没有需要调汇的外币余额"),
            Err(e) => ctx.error(e.to_string()),
        }
    }

    // ------------------------- Tab 2 自动转账 -------------------------
    fn show_transfer(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        widgets::card(ui, "转账规则", |ui| {
            ui.horizontal(|ui| {
                if ui.button("新增规则").clicked() && ctx.can(Perm::CarryForward) {
                    let sort = (self.rules.len() as i32 + 1) * 10;
                    self.at_edit = Some(AtForm::new(sort));
                }
                ui.label(
                    RichText::new("执行顺序即列表顺序；对方科目留空表示把来源科目结平").weak(),
                );
            });

            let rows = self.rules.clone();
            let names: Vec<(String, String, String)> = rows
                .iter()
                .map(|r| {
                    let n = |code: &str| {
                        ctx.chart()
                            .get(code)
                            .map(|a| a.name.clone())
                            .unwrap_or_default()
                    };
                    (n(&r.src_account), n(&r.dst_account), n(&r.offset_account))
                })
                .collect();
            let cols = [
                widgets::TCol::new("序号", 60.0).fixed(),
                widgets::TCol::new("名称", 160.0),
                widgets::TCol::new("来源科目", 190.0),
                widgets::TCol::new("取数类型", 130.0),
                widgets::TCol::new("来源方向", 90.0).fixed(),
                widgets::TCol::new("比例", 90.0).right(),
                widgets::TCol::new("目标科目", 190.0),
                widgets::TCol::new("对方科目", 190.0),
                widgets::TCol::new("启用", 60.0).fixed(),
                widgets::TCol::new("操作", 160.0).fixed(),
            ];
            let mut edit: Option<i64> = None;
            let mut del: Option<i64> = None;
            let mut mv: Option<(usize, bool)> = None;
            widgets::grid(ui, "at_rules", &cols, rows.len(), 26.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => { ui.label(r.sort.to_string()); }
                    1 => { ui.label(&r.name); }
                    2 => { ui.label(format!("{} {}", r.src_account, names[i].0)); }
                    3 => { ui.label(r.src_kind.label()); }
                    4 => { ui.label(r.src_dir.label()); }
                    5 => {
                        let txt = if r.ratio_mode_is_ratio {
                            format!("{}%", (r.ratio * Money::from_i64(100)).fmt_money())
                        } else {
                            r.ratio.fmt_money()
                        };
                        ui.label(txt);
                    }
                    6 => { ui.label(format!("{} {}（{}）", r.dst_account, names[i].1, r.dst_dir.label())); }
                    7 => {
                        if r.offset_account.trim().is_empty() {
                            ui.label(RichText::new("同来源科目（结平）").weak());
                        } else {
                            ui.label(format!("{} {}", r.offset_account, names[i].2));
                        }
                    }
                    8 => {
                        ui.label(if r.active {
                            RichText::new("启用").color(palette::OK)
                        } else {
                            RichText::new("停用").color(palette::CREDIT)
                        });
                    }
                    9 => {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if ui.small_button("改").clicked() {
                                edit = Some(r.id);
                            }
                            if ui.small_button("删").clicked() {
                                del = Some(r.id);
                            }
                            if ui.small_button("↑").on_hover_text("上移").clicked() {
                                mv = Some((i, true));
                            }
                            if ui.small_button("↓").on_hover_text("下移").clicked() {
                                mv = Some((i, false));
                            }
                        });
                    }
                    _ => {}
                }
            });

            if let Some(id) = edit {
                if let Some(r) = self.rules.iter().find(|r| r.id == id) {
                    self.at_edit = Some(AtForm::of(r));
                }
            }
            if let Some(id) = del {
                match at_delete(ctx.db(), id) {
                    Ok(()) => {
                        ctx.log("期末", "删除转账规则", &format!("#{id}"));
                        ctx.info("已删除规则");
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
            if let Some((i, up)) = mv {
                self.move_rule(ctx, i, up);
            }
        });

        widgets::card(ui, "本期转账预览", |ui| {
            let rows = self.previews.clone();
            if rows.is_empty() || rows.iter().all(|r| r.amount.is_zero()) {
                widgets::empty_hint(ui, "本期没有启用的转账规则，或取数结果全部为零");
                return;
            }
            let cols = [
                widgets::TCol::new("规则名称", 220.0),
                widgets::TCol::new("取数基数", 160.0).right(),
                widgets::TCol::new("转账金额", 160.0).right(),
                widgets::TCol::new("说明", 300.0),
            ];
            widgets::grid(ui, "at_preview", &cols, rows.len(), 24.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => { ui.label(&r.rule.name); }
                    1 => widgets::amount_label(ui, r.base),
                    2 => widgets::amount_label(ui, r.amount),
                    3 => { ui.label(RichText::new(&r.note).weak()); }
                    _ => {}
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("生成自动转账凭证").clicked() && ctx.can(Perm::CarryForward) {
                        self.do_at_run(ctx, p);
                    }
                });
            });
        });

        self.at_window(ctx, ui);
    }

    /// 上移/下移：swap 后把 sort 重排成 10/20/30，
    /// 避免相邻两条 sort 相同（比如都是 0）时顺序随机。
    fn move_rule(&mut self, ctx: &mut AppCtx<'_>, idx: usize, up: bool) {
        let n = self.rules.len();
        if n < 2 {
            return;
        }
        let j = if up {
            if idx == 0 {
                return;
            }
            idx - 1
        } else {
            if idx + 1 >= n {
                return;
            }
            idx + 1
        };
        self.rules.swap(idx, j);
        let mut failed = 0usize;
        for (i, r) in self.rules.iter_mut().enumerate() {
            r.sort = (i as i32 + 1) * 10;
            if at_update(ctx.db(), r).is_err() {
                failed += 1;
            }
        }
        if failed > 0 {
            ctx.error(format!("有 {failed} 条规则的序号保存失败"));
        } else {
            ctx.info("已调整执行顺序");
        }
        ctx.log("期末", "调整转账规则顺序", &format!("{}↔{}", idx + 1, j + 1));
        self.dirty = true;
    }

    fn at_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(f) = self.at_edit.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = f.is_new;
        let kinds: Vec<String> = SrcKind::ALL.iter().map(|k| k.label().to_string()).collect();
        let dirs: Vec<String> = EntryDir::ALL.iter().map(|d| d.label().to_string()).collect();

        egui::Window::new(if is_new { "新增转账规则" } else { "修改转账规则" })
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 460.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                egui::Grid::new("at_edit")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("名称：");
                        widgets::text_input(ui, &mut f.name, 320.0, "如：结转制造费用");
                        ui.end_row();
                        ui.label("凭证摘要：");
                        widgets::text_input(ui, &mut f.summary, 320.0, "留空则取规则名");
                        ui.end_row();
                        ui.label("序号：");
                        ui.add(egui::DragValue::new(&mut f.sort).range(0..=9990));
                        ui.end_row();
                        ui.label("来源科目：");
                        widgets::account_combo(ui, "at_src", &mut f.src_account, ctx.chart(), true, 260.0);
                        ui.end_row();
                        ui.label("来源辅助：");
                        widgets::text_input(ui, &mut f.src_aux, 260.0, "可空，如 customer:C01");
                        ui.end_row();
                        ui.label("取数类型：");
                        widgets::combo(ui, "at_kind", &mut f.kind_txt, &kinds, 220.0);
                        ui.end_row();
                        ui.label("来源方向：");
                        widgets::combo(ui, "at_srcdir", &mut f.src_dir_txt, &dirs, 120.0);
                        ui.end_row();
                        ui.label("目标科目：");
                        widgets::account_combo(ui, "at_dst", &mut f.dst_account, ctx.chart(), true, 260.0);
                        ui.end_row();
                        ui.label("目标方向：");
                        widgets::combo(ui, "at_dstdir", &mut f.dst_dir_txt, &dirs, 120.0);
                        ui.end_row();
                        ui.label("对方科目：");
                        ui.horizontal(|ui| {
                            widgets::account_combo(
                                ui,
                                "at_off",
                                &mut f.offset_account,
                                ctx.chart(),
                                true,
                                230.0,
                            );
                            if ui.small_button("清空").on_hover_text("留空=结平来源科目").clicked() {
                                f.offset_account.clear();
                            }
                        });
                        ui.end_row();
                        ui.label("目标辅助：");
                        widgets::text_input(ui, &mut f.dst_aux, 260.0, "可空");
                        ui.end_row();
                    });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut f.by_ratio, "按比例");
                    if f.by_ratio {
                        ui.label("比例：");
                        widgets::text_input(ui, &mut f.ratio_txt, 120.0, "1 = 100%");
                    } else {
                        ui.label("固定金额：");
                        widgets::text_input(ui, &mut f.ratio_txt, 120.0, "0.00");
                    }
                });
                ui.checkbox(&mut f.active, "启用（不启用的规则不参与预览和执行）");
                ui.add_space(4.0);
                ui.label("备注：");
                widgets::text_input(ui, &mut f.memo, 480.0, "可空");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui.button("保存").clicked() {
                            save = true;
                        }
                    });
                });
            });

        if close || !open {
            self.at_edit = None;
            return;
        }
        if save {
            self.save_rule(ctx);
        }
    }

    fn save_rule(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(f) = self.at_edit.clone() else {
            return;
        };
        if f.name.trim().is_empty() {
            ctx.error("规则名称不能为空");
            return;
        }
        if f.src_account.trim().is_empty() || f.dst_account.trim().is_empty() {
            ctx.error("来源科目与目标科目都必须选择");
            return;
        }
        if Money::parse(&f.ratio_txt).is_err() {
            ctx.error("比例/金额不是合法数字");
            return;
        }

        let mut r = match self.rules.iter().find(|r| r.id == f.id) {
            Some(r) => r.clone(),
            None => AutoTransfer {
                id: 0,
                name: String::new(),
                sort: f.sort,
                active: true,
                src_account: String::new(),
                src_aux: String::new(),
                src_kind: SrcKind::End,
                src_dir: EntryDir::Auto,
                ratio: Money::ZERO,
                ratio_mode_is_ratio: true,
                dst_account: String::new(),
                dst_aux: String::new(),
                dst_dir: EntryDir::Debit,
                offset_account: String::new(),
                summary: String::new(),
                memo: String::new(),
            },
        };
        f.apply_to(&mut r);

        let res = if f.is_new {
            at_insert(ctx.db(), &r).map(|_| ())
        } else {
            at_update(ctx.db(), &r)
        };
        match res {
            Ok(()) => {
                ctx.log(
                    "期末",
                    if f.is_new { "新增转账规则" } else { "修改转账规则" },
                    &r.name,
                );
                ctx.info("已保存规则");
                self.dirty = true;
                self.at_edit = None;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn do_at_run(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let who = ctx.user().display_name.clone();
        match at_run(ctx.db(), p, p.last_day(), &who) {
            Ok((ids, skips)) => {
                if ids.is_empty() {
                    ctx.info("本期没有需要生成的自动转账凭证");
                } else {
                    ctx.log("期末", "自动转账", &format!("{} 生成 {} 张", p.label(), ids.len()));
                    ctx.info(format!("已生成 {} 张自动转账凭证", ids.len()));
                }
                // 跳过原因（取数为零 / 本期已生成 / 保存失败）逐条报出来，
                // 否则用户只看到"少了几张"却不知道为什么。
                for s in skips {
                    ctx.error(s);
                }
                self.dirty = true;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    // ------------------------- Tab 3 月度检查清单 -------------------------
    fn show_checklist(&mut self, _ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        let total = self.checks.len();
        let todo = self.checks.iter().filter(|c| !c.ok).count();
        if total == 0 {
            widgets::empty_hint(ui, "暂无检查项");
        } else if todo == 0 {
            ui.label(
                RichText::new(format!("✔ {} 全部 {} 项检查通过，可以结账", p.label(), total))
                    .color(palette::OK)
                    .strong(),
            );
        } else {
            ui.label(
                RichText::new(format!("{} 还有 {} 项待办（共 {} 项）", p.label(), todo, total))
                    .color(palette::WARN)
                    .strong(),
            );
        }
        ui.add_space(6.0);

        let rows = self.checks.clone();
        let cols = [
            widgets::TCol::new("状态", 60.0).fixed(),
            widgets::TCol::new("检查项", 420.0),
            widgets::TCol::new("处理入口", 300.0),
        ];
        widgets::grid(ui, "checklist", &cols, rows.len(), 26.0, |i, c, ui| {
            let it = &rows[i];
            match c {
                0 => {
                    if it.ok {
                        ui.label(RichText::new("✔").color(palette::OK).strong());
                    } else {
                        ui.label(RichText::new("✖").color(palette::WARN).strong());
                    }
                }
                1 => { ui.label(
                    RichText::new(&it.label).color(if it.ok {
                        palette::OK
                    } else {
                        palette::WARN
                    }),
                );
                }
                2 => { ui.label(RichText::new(&it.hint).weak()); }
                _ => {}
            }
        });

        ui.add_space(10.0);
        if ui.button("刷新").clicked() {
            self.dirty = true;
        }
    }

    // ------------------------- Tab 4 一键月末处理 -------------------------
    fn show_wizard(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        ui.label(
            RichText::new(
                "按顺序走完这五步即可完成月度结账。折旧与结转损益在各自功能界面执行，\
                 这里只做状态跟踪与跳转。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        let steps = self.steps();
        let mut act: Option<usize> = None;
        for (i, (name, done, detail)) in steps.iter().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("第 {} 步", i + 1))
                            .weak()
                            .monospace(),
                    );
                    ui.label(RichText::new(*name).strong());
                    ui.label(if *done {
                        RichText::new("✔ 已完成").color(palette::OK).strong()
                    } else {
                        RichText::new("○ 待处理").color(palette::WARN).strong()
                    });
                    ui.label(detail);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(wizard_button(i)).clicked() {
                            act = Some(i);
                        }
                    });
                });
            });
        }

        match act {
            // 折旧计提在【固定资产】界面，那里才有卡片级的明细与凭证生成
            Some(0) => ctx.nav(NavItem::Assets),
            Some(1) => {
                if ctx.can(Perm::CarryForward) {
                    self.do_fx_adjust(ctx, p);
                }
            }
            Some(2) => {
                if ctx.can(Perm::CarryForward) {
                    self.do_at_run(ctx, p);
                }
            }
            // 结转损益与期末结账都在【期末处理】，本界面不重复实现
            Some(3) | Some(4) => ctx.nav(NavItem::PeriodEnd),
            _ => {}
        }
    }

    /// 五步的（名称, 是否完成, 说明）
    fn steps(&self) -> Vec<(&'static str, bool, String)> {
        let pending = self.previews.iter().filter(|p| !p.amount.is_zero()).count();
        vec![
            (
                "计提折旧",
                self.check_ok("depreciation"),
                format!(
                    "本期待计提 {} 项，合计 {}",
                    self.dep_count,
                    self.dep_amount.fmt_money()
                ),
            ),
            (
                "期末调汇",
                self.check_ok("fx"),
                format!("差额计入 {} 汇兑损益", FX_GAIN_ACCOUNT),
            ),
            (
                "自动转账",
                pending == 0,
                if pending == 0 {
                    "本期没有待转账的规则".to_string()
                } else {
                    format!("待生成 {} 张转账凭证", pending)
                },
            ),
            (
                "结转损益",
                self.check_ok("carry"),
                format!("本期损益净额 {}", self.pl_net.fmt_money()),
            ),
            (
                "期末结账",
                self.closed,
                if self.closed {
                    "本期已结账".to_string()
                } else {
                    "尚未结账".to_string()
                },
            ),
        ]
    }
}

fn wizard_button(step: usize) -> &'static str {
    match step {
        0 => "前往固定资产",
        1 => "执行期末调汇",
        2 => "执行自动转账",
        3 => "前往期末处理",
        _ => "前往期末处理",
    }
}
