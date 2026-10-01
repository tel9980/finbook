//! 银行对账：导入对账单 → 自动 / 手工勾对 → 余额调节表
//!
//! 勾对只存"银行流水 ↔ 凭证分录"的关联（写在 bank_statement.entry_id 上），
//! 不动凭证本身，所以反悔成本很低：取消勾对只是把外键清空。

use std::collections::HashSet;

use egui::{RichText, Ui};
use findb::bank::{BookEntry, MatchResult, Reconciliation, Statement};
use fincore::{Money, Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BankTab {
    Statement,
    Matching,
    Reconcile,
}

impl BankTab {
    pub fn label(self) -> &'static str {
        match self {
            BankTab::Statement => "对账单",
            BankTab::Matching => "自动勾对",
            BankTab::Reconcile => "余额调节表",
        }
    }
}

/// 调节表两侧的未达账项，银行流水与账面分录取同一套列，便于并列查看
#[derive(Clone)]
struct DiffRow {
    date: String,
    /// 收 / 付
    dir: &'static str,
    summary: String,
    settle_no: String,
    debit: Money,
    credit: Money,
}

pub struct BankRecView {
    pub tab: BankTab,
    pub period_text: String,
    /// 当前对账的银行存款科目
    pub account: String,
    pub accounts: Vec<String>,
    pub stmts: Vec<Statement>,
    pub books: Vec<BookEntry>,
    /// 账面侧已被勾对的分录 id
    linked: HashSet<i64>,
    /// 余额调节表按需计算（只在切到该页时取一次）
    pub rec: Option<Reconciliation>,
    /// 勾对页选中的银行流水 / 账面分录
    pub sel_stmt: Option<i64>,
    pub sel_book: Option<i64>,
    /// 对账单页勾选的行（供"删除选中"）
    pub checked: HashSet<i64>,
    /// 未勾对列表里是否一并显示已勾对的流水（取消勾对时要先能选中它）
    pub show_matched: bool,
    /// 自动勾对的日期容差天数
    pub tolerance: i64,
    pub last_match: Option<MatchResult>,
    pub import_open: bool,
    pub import_text: String,
    pub dirty: bool,
    key: String,
}

impl Default for BankRecView {
    fn default() -> Self {
        Self {
            tab: BankTab::Statement,
            period_text: String::new(),
            account: String::new(),
            accounts: Vec::new(),
            stmts: Vec::new(),
            books: Vec::new(),
            linked: HashSet::new(),
            rec: None,
            sel_stmt: None,
            sel_book: None,
            checked: HashSet::new(),
            show_matched: false,
            tolerance: 3,
            last_match: None,
            import_open: false,
            import_text: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl BankRecView {
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
        // 科目列表跟着科目表走，账套切换 / 科目维护后要能重新取
        let r = findb::automation::bank_accounts(ctx.db());
        self.accounts = ctx.handle(r).unwrap_or_default();
        if self.account.is_empty() {
            if let Some(c) = self.accounts.first() {
                self.account = c.clone();
            }
        }

        let p = self.period(ctx);
        let key = format!("{}|{}", p.ymm(), self.account);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        self.rec = None;
        self.checked.clear();
        self.sel_stmt = None;
        self.sel_book = None;

        if self.account.is_empty() {
            self.stmts.clear();
            self.books.clear();
            self.linked.clear();
            return;
        }
        let r = findb::bank::list(ctx.db(), p, &self.account);
        self.stmts = ctx.handle(r).unwrap_or_default();
        let r = findb::bank::book_side(ctx.db(), p, &self.account);
        self.books = ctx.handle(r).unwrap_or_default();
        let r = findb::bank::linked_entry_ids(ctx.db(), p, &self.account);
        self.linked = ctx.handle(r).unwrap_or_default().into_iter().collect();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);
        let matched = self.stmts.iter().filter(|s| s.matched()).count();

        widgets::page_header(ui, "银行对账", |ui| {
            ui.label(
                RichText::new(format!(
                    "{} · {} · 已勾对 {}/{} 条",
                    p.label(),
                    self.account,
                    matched,
                    self.stmts.len()
                ))
                .weak(),
            );
        });

        widgets::toolbar(ui, |ui| {
            for t in [BankTab::Statement, BankTab::Matching, BankTab::Reconcile] {
                ui.selectable_value(&mut self.tab, t, t.label());
            }
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
            ui.separator();
            self.account_combo(ctx, ui);
        });

        if self.account.is_empty() {
            widgets::empty_hint(
                ui,
                "科目表里没有末级的银行存款科目，请先到【会计科目】把银行账户科目勾上「银行科目」",
            );
            return;
        }

        match self.tab {
            BankTab::Statement => self.show_statement(ctx, ui, p),
            BankTab::Matching => self.show_matching(ctx, ui, p),
            BankTab::Reconcile => self.show_reconcile(ctx, ui, p),
        }

        self.import_window(ctx, ui, p);
    }

    fn account_combo(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.label("银行科目");
        let opts: Vec<(String, String)> = self
            .accounts
            .iter()
            .map(|c| {
                let n = ctx
                    .chart()
                    .get(c)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                (c.clone(), n)
            })
            .collect();
        let label = match ctx.chart().get(&self.account) {
            Some(a) if !self.account.is_empty() => format!("{} {}", a.code, a.name),
            _ => "（请选择）".to_string(),
        };
        let mut changed = false;
        egui::ComboBox::from_id_salt("bank_rec_account")
            .selected_text(label)
            .width(200.0)
            .show_ui(ui, |ui| {
                for (c, n) in &opts {
                    if ui
                        .selectable_value(&mut self.account, c.clone(), format!("{c} {n}"))
                        .clicked()
                    {
                        changed = true;
                    }
                }
            });
        if changed {
            self.dirty = true;
        }
    }

    // ------------------------- 对账单 -------------------------
    fn show_statement(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        let can_edit = ctx.user().can(Perm::VoucherNew);
        let mut import = false;
        let mut clear = false;
        let mut del_sel = false;
        widgets::toolbar(ui, |ui| {
            if ui.button("导入 CSV").clicked() && can_edit {
                import = true;
            }
            if ui.button("清空").clicked() && can_edit {
                clear = true;
            }
            if ui.button("删除选中").clicked() && can_edit {
                del_sel = true;
            }
            ui.label(
                RichText::new(format!(
                    "{} {} 共 {} 条流水",
                    p.label(),
                    self.account,
                    self.stmts.len()
                ))
                .weak(),
            );
        });
        if import {
            self.import_open = true;
            self.import_text.clear();
        }
        if clear {
            ctx.confirm_dangerous(
                "清空对账单",
                &format!(
                    "将删除 {} 科目 {} 的全部银行对账单流水（共 {} 条），已做的勾对一并丢失。",
                    self.account,
                    p.label(),
                    self.stmts.len()
                ),
                ConfirmAction::ClearBankStatement(self.account.clone()),
                true,
            );
        }
        if del_sel && !self.checked.is_empty() {
            let n = self.checked.len();
            for id in self.checked.iter() {
                if let Err(e) = findb::bank::delete(ctx.db(), *id) {
                    ctx.error(e.to_string());
                }
            }
            ctx.log("银行对账", "删除流水", &format!("{n} 条"));
            ctx.info(format!("已删除 {n} 条流水"));
            self.checked.clear();
            self.dirty = true;
        }

        if self.stmts.is_empty() {
            widgets::empty_hint(ui, "还没有导入银行对账单，点「导入 CSV」粘贴或选择文件");
            return;
        }

        let rows: Vec<(i64, String, String, String, Money, Money, Money, bool)> = self
            .stmts
            .iter()
            .map(|s| {
                (
                    s.id,
                    s.biz_date.format("%Y-%m-%d").to_string(),
                    s.summary.clone(),
                    s.settle_no.clone(),
                    s.debit,
                    s.credit,
                    s.balance,
                    s.matched(),
                )
            })
            .collect();
        let checked = self.checked.clone();
        let mut toggled: Option<i64> = None;
        let cols = [
            widgets::TCol::new("选", 34.0).fixed(),
            widgets::TCol::new("业务日期", 100.0).fixed(),
            widgets::TCol::new("摘要", 220.0),
            widgets::TCol::new("结算号", 120.0),
            widgets::TCol::new("借方", 120.0).right(),
            widgets::TCol::new("贷方", 120.0).right(),
            widgets::TCol::new("余额", 130.0).right(),
            widgets::TCol::new("勾对", 70.0).fixed(),
        ];
        widgets::grid(ui, "bank_stmts", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => {
                    let mut on = checked.contains(&r.0);
                    if ui.checkbox(&mut on, "").changed() {
                        toggled = Some(r.0);
                    }
                }
                1 => { ui.label(RichText::new(&r.1).monospace()); }
                2 => { ui.label(&r.2); }
                3 => { ui.label(RichText::new(&r.3).monospace()); }
                4 => widgets::amount_label(ui, r.4),
                5 => widgets::amount_label(ui, r.5),
                6 => widgets::amount_label(ui, r.6),
                7 => {
                    if r.7 {
                        ui.label(RichText::new("✔ 已勾对").color(palette::OK));
                    } else {
                        ui.label(RichText::new("未勾对").weak());
                    }
                }
                _ => {}
            }
        });
        if let Some(id) = toggled {
            if !self.checked.remove(&id) {
                self.checked.insert(id);
            }
        }
    }

    fn import_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        if !self.import_open {
            return;
        }
        let mut open = true;
        let mut do_import = false;
        let mut close = false;
        egui::Window::new("导入银行对账单")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 420.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(
                    RichText::new(
                        "每行一条流水，逗号 / 制表符 / 分号分隔。支持两种列顺序：\n\
                         日期,摘要,结算号,借方,贷方,余额\n\
                         日期,摘要,结算号,金额,余额（负数表示支出）\n\
                         首行是表头会自动跳过。",
                    )
                    .weak(),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_sized(
                        [ui.available_width(), 260.0],
                        egui::TextEdit::multiline(&mut self.import_text)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("2026-01-06,收到货款,SN001,1000.00,0.00,101000.00"),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui.button("导入").clicked() {
                            do_import = true;
                        }
                    });
                });
            });

        if close || !open {
            self.import_open = false;
            self.import_text.clear();
            return;
        }
        if do_import {
            let text = self.import_text.clone();
            let acct = self.account.clone();
            match findb::bank::import_csv(ctx.db(), p, &acct, &text) {
                Ok((n, warns)) => {
                    ctx.log("银行对账", "导入对账单", &format!("{acct} {n} 条"));
                    ctx.info(format!("已导入 {n} 条流水"));
                    for w in warns.iter().take(5) {
                        ctx.error(w.clone());
                    }
                    if warns.len() > 5 {
                        ctx.error(format!("……另有 {} 行未导入", warns.len() - 5));
                    }
                    self.import_open = false;
                    self.import_text.clear();
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    // ------------------------- 自动勾对 -------------------------
    fn show_matching(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        let can_edit = ctx.user().can(Perm::VoucherNew);
        let mut auto = false;
        widgets::toolbar(ui, |ui| {
            ui.label("日期容差");
            ui.add(egui::DragValue::new(&mut self.tolerance).range(0..=31).suffix(" 天"));
            if ui.button("自动勾对").clicked() && can_edit {
                auto = true;
            }
            ui.separator();
            if ui.button("手工勾对").clicked() && can_edit {
                let s = self.sel_stmt.and_then(|id| self.stmts.iter().find(|x| x.id == id));
                let b = self
                    .sel_book
                    .and_then(|id| self.books.iter().find(|x| x.entry_id == id));
                match (s, b) {
                    (Some(s), Some(b)) => {
                        let who = ctx.user().display_name.clone();
                        let r = findb::bank::link(ctx.db(), s.id, b.entry_id, &who);
                        if ctx.handle(r).is_some() {
                            ctx.log(
                                "银行对账",
                                "手工勾对",
                                &format!("流水#{} ↔ 凭证{}", s.id, b.voucher_label()),
                            );
                            ctx.info("已勾对");
                            self.sel_stmt = None;
                            self.sel_book = None;
                            self.dirty = true;
                        }
                    }
                    _ => ctx.error("请先在左边选一条银行流水、在右边选一条账面记录"),
                }
            }
            if ui.button("取消勾对").clicked() && can_edit {
                match self.sel_stmt {
                    Some(id) => {
                        let r = findb::bank::unlink(ctx.db(), id);
                        if ctx.handle(r).is_some() {
                            ctx.log("银行对账", "取消勾对", &format!("流水#{id}"));
                            ctx.info("已取消勾对");
                            self.dirty = true;
                        }
                    }
                    None => ctx.error("请先在左边选一条银行流水"),
                }
            }
            ui.checkbox(&mut self.show_matched, "显示已勾对");
        });

        if auto {
            let who = ctx.user().display_name.clone();
            let acct = self.account.clone();
            let tol = self.tolerance;
            let r = findb::bank::auto_match(ctx.db(), p, &acct, tol, &who);
            if let Some(res) = ctx.handle(r) {
                ctx.log(
                    "银行对账",
                    "自动勾对",
                    &format!("{} 勾对 {} 条", p.label(), res.matched),
                );
                self.last_match = Some(res);
                self.dirty = true;
            }
        }

        if let Some(m) = &self.last_match {
            ui.label(
                RichText::new(format!(
                    "自动勾对结果：累计已勾对 {} 条 —— 按结算号 {} 条、按金额+日期 {} 条、\
                     按金额 {} 条；{} 条存在多个候选，未自动处理，请手工勾对。",
                    m.matched, m.by_no, m.by_amount_date, m.by_amount, m.ambiguous
                ))
                .strong(),
            );
            if m.ambiguous > 0 {
                ui.colored_label(
                    palette::WARN,
                    "多候选的一律不自动勾：一笔对多笔通常是拆分收付款，勾错比不勾更麻烦。",
                );
            }
            ui.add_space(4.0);
        }

        let sel_stmt = self.sel_stmt;
        let sel_book = self.sel_book;
        let show_matched = self.show_matched;
        let stmt_total = self
            .stmts
            .iter()
            .filter(|s| show_matched || !s.matched())
            .count();
        let book_total = self
            .books
            .iter()
            .filter(|b| !self.linked.contains(&b.entry_id))
            .count();
        let (mut pick_stmt, mut pick_book) = (None, None);
        ui.columns(2, |cols| {
            // 左：银行对账单
            cols[0].label(
                RichText::new(format!(
                    "银行对账单{}（{}）",
                    if show_matched { "" } else { "未勾对" },
                    stmt_total
                ))
                .strong(),
            );
            let rows: Vec<(i64, String, String, Money, Money, bool)> = self
                .stmts
                .iter()
                .filter(|s| show_matched || !s.matched())
                .map(|s| {
                    (
                        s.id,
                        s.biz_date.format("%m-%d").to_string(),
                        s.summary.clone(),
                        s.debit,
                        s.credit,
                        s.matched(),
                    )
                })
                .collect();
            let scols = [
                widgets::TCol::new("日期", 54.0).fixed(),
                widgets::TCol::new("摘要", 150.0),
                widgets::TCol::new("借方", 90.0).right(),
                widgets::TCol::new("贷方", 90.0).right(),
            ];
            widgets::grid(&mut cols[0], "bank_pick_stmt", &scols, rows.len(), 22.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => {
                        if ui
                            .selectable_label(sel_stmt == Some(r.0), RichText::new(&r.1).monospace())
                            .clicked()
                        {
                            pick_stmt = Some(r.0);
                        }
                    }
                    1 => {
                        let t = if r.5 {
                            RichText::new(format!("✔ {}", r.2)).color(palette::OK)
                        } else {
                            RichText::new(&r.2)
                        };
                        if ui.selectable_label(sel_stmt == Some(r.0), t).clicked() {
                            pick_stmt = Some(r.0);
                        }
                    }
                    2 => widgets::amount_label(ui, r.3),
                    3 => widgets::amount_label(ui, r.4),
                    _ => {}
                }
            });

            // 右：企业日记账
            cols[1].label(RichText::new(format!("企业日记账未勾对（{book_total}）")).strong());
            let rows: Vec<(i64, String, String, String, Money, Money)> = self
                .books
                .iter()
                .filter(|b| !self.linked.contains(&b.entry_id))
                .map(|b| {
                    (
                        b.entry_id,
                        b.date.format("%m-%d").to_string(),
                        b.voucher_label(),
                        b.summary.clone(),
                        b.debit,
                        b.credit,
                    )
                })
                .collect();
            let bcols = [
                widgets::TCol::new("日期", 54.0).fixed(),
                widgets::TCol::new("凭证号", 76.0).fixed(),
                widgets::TCol::new("摘要", 130.0),
                widgets::TCol::new("借方", 90.0).right(),
                widgets::TCol::new("贷方", 90.0).right(),
            ];
            widgets::grid(&mut cols[1], "bank_pick_book", &bcols, rows.len(), 22.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => {
                        if ui
                            .selectable_label(sel_book == Some(r.0), RichText::new(&r.1).monospace())
                            .clicked()
                        {
                            pick_book = Some(r.0);
                        }
                    }
                    1 => {
                        if ui
                            .selectable_label(sel_book == Some(r.0), RichText::new(&r.2).monospace())
                            .clicked()
                        {
                            pick_book = Some(r.0);
                        }
                    }
                    2 => {
                        if ui.selectable_label(sel_book == Some(r.0), &r.3).clicked() {
                            pick_book = Some(r.0);
                        }
                    }
                    3 => widgets::amount_label(ui, r.4),
                    4 => widgets::amount_label(ui, r.5),
                    _ => {}
                }
            });
        });
        if let Some(id) = pick_stmt {
            self.sel_stmt = Some(id);
        }
        if let Some(id) = pick_book {
            self.sel_book = Some(id);
        }
    }

    // ------------------------- 余额调节表 -------------------------
    fn show_reconcile(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        if self.rec.is_none() {
            let acct = self.account.clone();
            let r = findb::bank::reconcile(ctx.db(), p, &acct);
            self.rec = ctx.handle(r);
        }
        let Some(rec) = self.rec.clone() else {
            widgets::empty_hint(ui, "余额调节表计算失败");
            return;
        };

        let book_in: Money = rec.book_only_in.iter().map(|b| b.signed()).sum();
        let book_out: Money = rec.book_only_out.iter().map(|b| b.signed().abs()).sum();
        let bank_in: Money = rec.bank_only_in.iter().map(|s| s.signed()).sum();
        let bank_out: Money = rec.bank_only_out.iter().map(|s| s.signed().abs()).sum();

        widgets::card(ui, "调节表", |ui| {
            widgets::kv(ui, "银行对账单余额：", &rec.bank_balance.fmt_money());
            widgets::kv(
                ui,
                "加：企业已收、银行未收：",
                &format!("{}（{} 笔）", book_in.fmt_money(), rec.book_only_in.len()),
            );
            widgets::kv(
                ui,
                "减：企业已付、银行未付：",
                &format!("{}（{} 笔）", book_out.fmt_money(), rec.book_only_out.len()),
            );
            widgets::kv(ui, "调整后银行余额：", &rec.bank_adjusted.fmt_money());
            ui.separator();
            widgets::kv(ui, "企业账面余额：", &rec.book_balance.fmt_money());
            widgets::kv(
                ui,
                "加：银行已收、企业未记：",
                &format!("{}（{} 笔）", bank_in.fmt_money(), rec.bank_only_in.len()),
            );
            widgets::kv(
                ui,
                "减：银行已付、企业未记：",
                &format!("{}（{} 笔）", bank_out.fmt_money(), rec.bank_only_out.len()),
            );
            widgets::kv(ui, "调整后账面余额：", &rec.book_adjusted.fmt_money());
            ui.separator();
            if rec.balanced() {
                ui.label(
                    RichText::new(format!("✔ 调节后双方余额一致：{}", rec.bank_adjusted.fmt_money()))
                        .color(palette::OK)
                        .strong(),
                );
            } else {
                ui.label(
                    RichText::new(format!(
                        "✖ 调节后仍不平衡，差额 {}（银行 {} ／ 账面 {}）",
                        rec.diff().fmt_money(),
                        rec.bank_adjusted.fmt_money(),
                        rec.book_adjusted.fmt_money()
                    ))
                    .color(palette::CREDIT)
                    .strong(),
                );
            }
        });

        let bank_rows = [
            rec.bank_only_in
                .iter()
                .map(|s| DiffRow {
                    date: s.biz_date.format("%Y-%m-%d").to_string(),
                    dir: "收",
                    summary: s.summary.clone(),
                    settle_no: s.settle_no.clone(),
                    debit: s.debit,
                    credit: s.credit,
                })
                .collect::<Vec<_>>(),
            rec.bank_only_out
                .iter()
                .map(|s| DiffRow {
                    date: s.biz_date.format("%Y-%m-%d").to_string(),
                    dir: "付",
                    summary: s.summary.clone(),
                    settle_no: s.settle_no.clone(),
                    debit: s.debit,
                    credit: s.credit,
                })
                .collect::<Vec<_>>(),
        ]
        .concat();
        let book_rows = [
            rec.book_only_in
                .iter()
                .map(|b| DiffRow {
                    date: b.date.format("%Y-%m-%d").to_string(),
                    dir: "收",
                    summary: format!("{} {}", b.voucher_label(), b.summary),
                    settle_no: b.settle_no.clone(),
                    debit: b.debit,
                    credit: b.credit,
                })
                .collect::<Vec<_>>(),
            rec.book_only_out
                .iter()
                .map(|b| DiffRow {
                    date: b.date.format("%Y-%m-%d").to_string(),
                    dir: "付",
                    summary: format!("{} {}", b.voucher_label(), b.summary),
                    settle_no: b.settle_no.clone(),
                    debit: b.debit,
                    credit: b.credit,
                })
                .collect::<Vec<_>>(),
        ]
        .concat();

        let cols = [
            widgets::TCol::new("日期", 100.0).fixed(),
            widgets::TCol::new("方向", 50.0).fixed(),
            widgets::TCol::new("摘要", 260.0),
            widgets::TCol::new("结算号", 120.0),
            widgets::TCol::new("借方", 120.0).right(),
            widgets::TCol::new("贷方", 120.0).right(),
        ];
        let cell = |r: &DiffRow, c: usize, ui: &mut Ui| match c {
            0 => {
                ui.label(RichText::new(&r.date).monospace());
            }
            1 => {
                ui.label(RichText::new(r.dir).color(if r.dir == "付" {
                    palette::CREDIT
                } else {
                    palette::OK
                }));
            }
            2 => {
                ui.label(&r.summary);
            }
            3 => {
                ui.label(RichText::new(&r.settle_no).monospace());
            }
            4 => widgets::amount_label(ui, r.debit),
            5 => widgets::amount_label(ui, r.credit),
            _ => {}
        };

        ui.add_space(6.0);
        ui.label(
            RichText::new(format!("银行有、企业没有（{} 笔）", bank_rows.len())).strong(),
        );
        let rows = bank_rows.clone();
        widgets::grid(ui, "bank_only", &cols, rows.len(), 22.0, |i, c, ui| {
            cell(&rows[i], c, ui);
        });

        ui.add_space(8.0);
        ui.label(
            RichText::new(format!("企业有、银行没有（{} 笔）", book_rows.len())).strong(),
        );
        let rows = book_rows.clone();
        widgets::grid(ui, "book_only", &cols, rows.len(), 22.0, |i, c, ui| {
            cell(&rows[i], c, ui);
        });
    }
}
