//! 期末处理：结转损益、期末结账、反结账

use egui::{RichText, Ui};
use findb::balances::BalanceQuery;
use fincore::engine::{
    generate_carry_forward, generate_year_end_carry, is_year_end, PROFIT_ACCOUNT,
    UNDISTRIBUTED_ACCOUNT,
};
use fincore::{BalanceRow, Money, Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Carry,
    Close,
    Unclose,
    Maintain,
    Reconcile,
}

pub struct PeriodEndView {
    pub tab: Tab,
    pub period_text: String,
    pub require_carry: bool,
    pub issues: Vec<String>,
    pub pl_rows: Vec<BalanceRow>,
    pub pl_net: Money,
    pub profit_balance: Money,
    pub closed: Vec<Period>,
    pub closed_upto: Option<Period>,
    pub reconcile: Vec<findb::reports::ReconcileItem>,
    pub dirty: bool,
    key: String,
}

impl Default for PeriodEndView {
    fn default() -> Self {
        Self {
            tab: Tab::Carry,
            period_text: String::new(),
            require_carry: true,
            issues: Vec::new(),
            pl_rows: Vec::new(),
            pl_net: Money::ZERO,
            profit_balance: Money::ZERO,
            closed: Vec::new(),
            closed_upto: None,
            reconcile: Vec::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl PeriodEndView {
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
        let key = format!("{}|{}", p.ymm(), self.require_carry);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        self.closed_upto = findb::periods::closed_upto(ctx.db()).unwrap_or(None);
        self.closed = findb::periods::list_closed(ctx.db()).unwrap_or_default();
        self.issues =
            findb::periods::precheck(ctx.db(), p, self.require_carry).unwrap_or_default();
        self.reconcile = findb::reports::period_reconcile(ctx.db(), p).unwrap_or_default();

        // 含草稿取数：与 Web 结转一致——结转凭证本身是草稿，年末结转靠
        // "4103 已清零"判重，按已记账口径会重复生成结转凭证（H-3 口径适用于报表）
        let snap = findb::balances::BalanceSnapshot::load(
            ctx.db(),
            &BalanceQuery::period(p).with_posted_only(false),
        );
        match snap {
            Ok(s) => {
                let chart = ctx.chart();
                self.pl_rows = s.profit_loss_rows(chart);
                self.pl_net = self
                    .pl_rows
                    .iter()
                    .map(|r| r.end())
                    .fold(Money::ZERO, |a, b| a + b);
                self.profit_balance = s.for_account(PROFIT_ACCOUNT, None).end();
            }
            Err(_) => {
                self.pl_rows.clear();
                self.pl_net = Money::ZERO;
                self.profit_balance = Money::ZERO;
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "期末处理", |ui| {
            if let Some(u) = self.closed_upto {
                ui.label(RichText::new(format!("已结账至：{}", u.label())).weak());
            } else {
                ui.label(RichText::new("尚未结账").weak());
            }
        });

        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, Tab::Carry, "结转损益");
            ui.selectable_value(&mut self.tab, Tab::Close, "期末结账");
            ui.selectable_value(&mut self.tab, Tab::Unclose, "反结账");
            ui.selectable_value(&mut self.tab, Tab::Reconcile, "期末对账");
            ui.selectable_value(&mut self.tab, Tab::Maintain, "数据维护");
            ui.separator();
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
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
            Tab::Carry => self.show_carry(ctx, ui, p),
            Tab::Close => self.show_close(ctx, ui, p),
            Tab::Unclose => self.show_unclose(ctx, ui, p),
            Tab::Maintain => self.show_maintain(ctx, ui, p),
            Tab::Reconcile => self.show_reconcile(ui),
        }
    }

    // ------------------------- 期末对账 -------------------------
    fn show_reconcile(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new(
                "期末对账：检查试算平衡、余额方向、银行未达账项与未生成凭证的业务单据。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        let rows = self.reconcile.clone();
        let cols = [
            widgets::TCol::new("检查项", 220.0),
            widgets::TCol::new("结果", 80.0).fixed(),
            widgets::TCol::new("说明", 400.0),
        ];
        widgets::grid(ui, "reconcile", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => {
                    ui.label(RichText::new(&r.name).strong());
                }
                1 => {
                    if r.ok {
                        ui.colored_label(crate::theme::palette::OK, "通过");
                    } else {
                        ui.colored_label(crate::theme::palette::CREDIT, "异常");
                    }
                }
                2 => {
                    ui.label(RichText::new(&r.detail).weak());
                }
                _ => {}
            }
        });
        if rows.is_empty() {
            widgets::empty_hint(ui, "暂无对账数据");
        }
    }

    // ------------------------- 结转损益 -------------------------
    fn show_carry(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        ui.label(
            RichText::new(
                "结转损益会把本期所有损益类科目的净发生额转入「本年利润」，结转后损益类科目期末余额归零。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        if self.pl_rows.is_empty() {
            widgets::empty_hint(ui, "本期损益类科目没有发生额，无需结转");
        } else {
            let rows = self.pl_rows.clone();
            let cols = [
                widgets::TCol::new("科目编码", 110.0).fixed(),
                widgets::TCol::new("科目名称", 220.0),
                widgets::TCol::new("本期借方", 140.0).right(),
                widgets::TCol::new("本期贷方", 140.0).right(),
                widgets::TCol::new("期末余额（带符号）", 160.0).right(),
            ];
            widgets::grid(ui, "pl_rows", &cols, rows.len(), 24.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => { ui.label(RichText::new(&r.account_code).monospace()); }
                    1 => { ui.label(&r.account_name); }
                    2 => widgets::amount_label(ui, r.debit),
                    3 => widgets::amount_label(ui, r.credit),
                    4 => widgets::amount_label(ui, r.end()),
                    _ => {}
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("损益净额（正为净亏损）：{}", self.pl_net.fmt_money()))
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("生成结转损益凭证").clicked() && ctx.can(Perm::CarryForward) {
                        ctx.confirm(
                            "结转损益",
                            &format!(
                                "将为 {} 生成一张结转损益凭证（转入 {}本年利润），确定继续吗？",
                                p.label(),
                                PROFIT_ACCOUNT
                            ),
                            ConfirmAction::CarryForward(p),
                        );
                    }
                });
            });
        }

        // 年末结转
        ui.add_space(12.0);
        ui.separator();
        ui.label(RichText::new("年末结转").strong());
        ui.horizontal(|ui| {
            ui.label(format!(
                "「{}本年利润」期末余额：{}",
                PROFIT_ACCOUNT,
                self.profit_balance.fmt_money()
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(format!("结转至 {} 未分配利润", UNDISTRIBUTED_ACCOUNT))
                    .on_hover_text("一般在 12 期结账后执行")
                    .clicked()
                    && ctx.can(Perm::CarryForward)
                {
                    self.do_year_end(ctx, p);
                }
            });
        });
        if !is_year_end(p) {
            ui.label(
                RichText::new(format!("提示：当前期间 {} 不是 12 期，通常次年再做年末结转", p.label()))
                    .weak(),
            );
        }
    }

    fn do_year_end(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let date = p.last_day();
        let word = ctx
            .db()
            .voucher_words()
            .first()
            .cloned()
            .unwrap_or_else(|| "记".to_string());
        let no = findb::vouchers::next_no(ctx.db(), p, &word).unwrap_or(1);
        let who = ctx.user().display_name.clone();
        let r = generate_year_end_carry(
            p,
            date,
            &word,
            no,
            self.profit_balance,
            UNDISTRIBUTED_ACCOUNT,
            ctx.chart(),
            &who,
        );
        match r {
            Ok(mut v) => match findb::vouchers::save(ctx.db(), &mut v) {
                Ok(id) => {
                    ctx.log("期末", "年末结转", &format!("{} 凭证#{id}", p.label()));
                    ctx.info(format!("已生成年末结转凭证 {}", v.voucher_no()));
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            },
            Err(e) => ctx.error(e.to_string()),
        }
    }

    /// 由外壳确认后调用
    pub fn run_carry_forward(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let date = p.last_day();
        let word = ctx
            .db()
            .voucher_words()
            .first()
            .cloned()
            .unwrap_or_else(|| "记".to_string());
        let no = findb::vouchers::next_no(ctx.db(), p, &word).unwrap_or(1);
        let who = ctx.user().display_name.clone();
        let rows = self.pl_rows.clone();
        let r = generate_carry_forward(
            p,
            date,
            &word,
            no,
            &rows,
            ctx.chart(),
            PROFIT_ACCOUNT,
            &who,
        );
        match r {
            Ok(mut v) => match findb::vouchers::save(ctx.db(), &mut v) {
                Ok(id) => {
                    ctx.log("期末", "结转损益", &format!("{} 凭证#{id}", p.label()));
                    ctx.info(format!("已生成结转损益凭证 {}", v.voucher_no()));
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            },
            Err(e) => ctx.error(e.to_string()),
        }
    }

    // ------------------------- 期末结账 -------------------------
    fn show_close(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.require_carry, "要求先结转损益才能结账");
            if ui.button("重新检查").clicked() {
                self.dirty = true;
            }
        });
        ui.add_space(6.0);

        if self.issues.is_empty() {
            ui.label(
                RichText::new(format!("✔ {} 具备结账条件", p.label()))
                    .color(palette::OK)
                    .strong(),
            );
        } else {
            ui.label(RichText::new(format!("{} 存在以下问题：", p.label())).strong());
            for s in &self.issues {
                ui.horizontal(|ui| {
                    ui.colored_label(palette::CREDIT, "✖");
                    ui.label(s);
                });
            }
        }
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            if ui.button("批量记账本期已审核凭证").clicked() && ctx.can(Perm::VoucherPost) {
                let who = ctx.user().display_name.clone();
                let r = findb::periods::post_all(ctx.db(), p, &who);
                if let Some((n, errs)) = ctx.handle(r) {
                    ctx.info(format!("已记账 {n} 张"));
                    for e in errs.iter().take(5) {
                        ctx.error(e.clone());
                    }
                    ctx.log("期末", "批量记账", &format!("{} {n} 张", p.label()));
                    self.dirty = true;
                }
            }
            if ui.button("期末结账").clicked() && ctx.can(Perm::PeriodClose) {
                ctx.confirm(
                    "期末结账",
                    &format!(
                        "结账后 {} 不能再录入或修改凭证。确定对 {} 结账吗？",
                        p.label(),
                        p.label()
                    ),
                    ConfirmAction::ClosePeriod(p),
                );
            }
        });

        ui.add_space(14.0);
        ui.separator();
        ui.label(RichText::new("已结账期间").strong());
        if self.closed.is_empty() {
            ui.label(RichText::new("暂无").weak());
        } else {
            for c in &self.closed {
                ui.horizontal(|ui| {
                    ui.label("🔒");
                    ui.label(c.label());
                });
            }
        }
    }

    // ------------------------- 反结账 -------------------------
    fn show_unclose(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        ui.label(
            RichText::new(
                "反结账会解除该期间的锁定，允许再次录入凭证。只能从最后一个已结账期间开始反。",
            )
            .weak(),
        );
        ui.add_space(8.0);
        match self.closed_upto {
            None => widgets::empty_hint(ui, "当前没有任何已结账期间"),
            Some(u) => {
                ui.label(
                    RichText::new(format!("最后结账期间：{}", u.label()))
                        .strong(),
                );
                if p != u {
                    ui.colored_label(
                        palette::WARN,
                        format!("当前选择的 {} 不是最后结账期间，请先反结账 {}", p.label(), u.label()),
                    );
                }
                ui.add_space(8.0);
                if ui.button("反结账当前期间").clicked() && ctx.can(Perm::PeriodClose) {
                    ctx.confirm_dangerous(
                        "反结账",
                        &format!(
                            "确定对 {} 反结账吗？反结账后该期间可以重新录入凭证，\
                             已生成的报表口径会随之变化。",
                            p.label()
                        ),
                        ConfirmAction::UnclosePeriod(p),
                        true,
                    );
                }
            }
        }
    }

    // ------------------------- 数据维护 -------------------------
    fn show_maintain(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        widgets::card(ui, "批量记账", |ui| {
            ui.label("把指定期间内所有「已审核」凭证一次性记账。");
            if ui.button("执行批量记账").clicked() && ctx.can(Perm::VoucherPost) {
                let who = ctx.user().display_name.clone();
                let r = findb::periods::post_all(ctx.db(), p, &who);
                if let Some((n, errs)) = ctx.handle(r) {
                    ctx.info(format!("已记账 {n} 张"));
                    for e in errs.iter().take(5) {
                        ctx.error(e.clone());
                    }
                    self.dirty = true;
                }
            }
        });

        widgets::card(ui, "凭证断号检查", |ui| {
            let gaps = findb::vouchers::find_gaps(ctx.db(), p, "记").unwrap_or_default();
            if gaps.is_empty() {
                ui.label(RichText::new("✔ 凭证号连续").color(palette::OK));
            } else {
                let show: Vec<String> = gaps.iter().take(20).map(|g| g.to_string()).collect();
                ui.colored_label(
                    palette::WARN,
                    format!("{} 存在 {} 个断号：{}", p.label(), gaps.len(), show.join("、")),
                );
            }
        });

        widgets::card(ui, "危险操作", |ui| {
            ui.colored_label(
                palette::CREDIT,
                "清空业务数据会删除全部凭证与期初余额，且不可撤销，请先备份账套。",
            );
            if ui.button("清空全部凭证与期初").clicked() {
                ctx.confirm_dangerous(
                    "清空业务数据",
                    "将删除账套内全部凭证分录与期初余额（科目、档案、用户保留）。\n确定继续吗？",
                    ConfirmAction::ClearVouchers,
                    true,
                );
            }
        });
    }
}
