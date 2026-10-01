//! 资金 / 预算分析 / 成本核算 三个拓展视图
//!
//! - 资金管理：日报（按日）/ 票据 / 融资 / 现金盘点 / 支票簿 / 出纳日记账(日清) / 借支 / 资金预算 / 资金预测
//! - 预算分析：年度逐月预算 vs 实际（含部门维度）
//! - 成本核算：计价方式配置、期末结价
//!
//! 数据层都在 findb，这里只负责展示与交互。

use egui::{RichText, Ui};
use fincore::{Money, Period};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

// ===========================================================================
// 资金管理
// ===========================================================================

pub struct FundsView {
    pub tab: u8, // 0=资金日报 1=票据 2=融资 3=资金预测
    pub bill_kind: String,
    pub loan_kind: String,
    pub dirty: bool,
    /// 资金日报选中日期（None = 今天）
    pub daily_date: Option<chrono::NaiveDate>,
    // 现金盘点录入（日期文本 / 科目 / 实盘金额 / 备注）
    pub cc_date: String,
    pub cc_account: String,
    pub cc_counted: String,
    pub cc_memo: String,
    // 支票登记簿录入
    pub ck_no: String,
    pub ck_kind: String,
    pub ck_bank: String,
    pub ck_payee: String,
    pub ck_amount: String,
    pub ck_date: String,
    pub ck_memo: String,
    // 收付款单录入
    pub rc_date: String,
    pub rc_kind: String,
    pub rc_fund: String,
    pub rc_party: String,
    pub rc_amount: String,
    pub rc_memo: String,
    // 出纳日记账（科目 + 日清日期）
    pub j_account: String,
    pub j_date: String,
    // 员工借支录入 + 核销参数
    pub ad_date: String,
    pub ad_emp: String,
    pub ad_purpose: String,
    pub ad_amount: String,
    pub ad_account: String,
    pub ad_memo: String,
    pub ad_exp_account: String,
    pub ad_exp_amount: String,
}

impl Default for FundsView {
    fn default() -> Self {
        Self {
            tab: 0,
            bill_kind: String::new(),
            loan_kind: String::new(),
            dirty: true,
            daily_date: None,
            cc_date: String::new(),
            cc_account: "1001".to_string(),
            cc_counted: String::new(),
            cc_memo: String::new(),
            ck_no: String::new(),
            ck_kind: "transfer".to_string(),
            ck_bank: "100201".to_string(),
            ck_payee: String::new(),
            ck_amount: String::new(),
            ck_date: String::new(),
            ck_memo: String::new(),
            j_account: "1001".to_string(),
            j_date: String::new(),
            ad_date: String::new(),
            ad_emp: String::new(),
            ad_purpose: String::new(),
            ad_amount: String::new(),
            ad_account: "1001".to_string(),
            ad_memo: String::new(),
            ad_exp_account: "660201".to_string(),
            ad_exp_amount: String::new(),
            rc_date: String::new(),
            rc_kind: "receipt".to_string(),
            rc_fund: String::new(),
            rc_party: String::new(),
            rc_amount: String::new(),
            rc_memo: String::new(),
        }
    }
}

impl FundsView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::page_header(ui, "资金管理", |ui| {
            ui.label(RichText::new("现金/银行资金日报 · 票据 · 融资 · 资金预测").weak());
        });

        widgets::toolbar(ui, |ui| {
            for (i, label) in ["资金日报", "票据", "融资", "现金盘点", "支票簿", "日记账", "借支", "资金预算", "收付款", "资金预测"].iter().enumerate() {
                if ui.selectable_label(self.tab == i as u8, *label).clicked() {
                    self.tab = i as u8;
                    self.dirty = true;
                }
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        match self.tab {
            0 => self.show_daily(ctx, ui),
            1 => self.show_bills(ctx, ui),
            2 => self.show_loans(ctx, ui),
            3 => self.show_counts(ctx, ui),
            4 => self.show_checks(ctx, ui),
            5 => self.show_journal(ctx, ui),
            6 => self.show_advances(ctx, ui),
            7 => self.show_budget(ctx, ui),
            8 => self.show_receipts(ctx, ui),
            _ => self.show_forecast(ctx, ui),
        }
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let (name, sh) = match self.tab {
            0 => {
                let date = self
                    .daily_date
                    .unwrap_or_else(|| chrono::Local::now().date_naive());
                let rows = findb::funds::funds_daily_by_date(ctx.db(), date).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "资金日报",
                    vec!["科目".to_string(), "科目名称".to_string(), "上日结余".to_string(), "本日收入".to_string(), "本日支出".to_string(), "日末结存".to_string()],
                );
                for r in rows {
                    sh.push(vec![r.account_code.clone(), r.account_name.clone(), r.begin.fmt_plain(), r.income.fmt_plain(), r.expense.fmt_plain(), r.end.fmt_plain()]);
                }
                ("资金日报", sh)
            }
            1 => {
                let kind = if self.bill_kind.is_empty() { None } else { Some(self.bill_kind.as_str()) };
                let rows = findb::funds::bill_list(ctx.db(), kind).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "票据",
                    vec!["类型".to_string(), "票据号".to_string(), "出票日".to_string(), "到期日".to_string(), "对方单位".to_string(), "金额".to_string(), "状态".to_string()],
                );
                for b in rows {
                    sh.push(vec![
                        if b.kind == "receivable" { "应收".to_string() } else { "应付".to_string() },
                        b.no.clone(),
                        b.issue_date.format("%Y-%m-%d").to_string(),
                        b.due_date.format("%Y-%m-%d").to_string(),
                        b.counterpart.clone(),
                        b.amount.fmt_plain(),
                        findb::funds::BillStatus::parse(&b.status).label().to_string(),
                    ]);
                }
                ("票据", sh)
            }
            2 => {
                let kind = if self.loan_kind.is_empty() { None } else { Some(self.loan_kind.as_str()) };
                let rows = findb::funds::loan_list(ctx.db(), kind).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "融资",
                    vec!["类型".to_string(), "编号".to_string(), "机构".to_string(), "本金".to_string(), "年利率%".to_string(), "起息日".to_string(), "到期日".to_string(), "状态".to_string()],
                );
                for l in rows {
                    sh.push(vec![
                        if l.kind == "borrow" { "借款".to_string() } else { "放款".to_string() },
                        l.no.clone(),
                        l.bank.clone(),
                        l.principal.fmt_plain(),
                        l.rate_pct.fmt_qty(),
                        l.start_date.format("%Y-%m-%d").to_string(),
                        l.end_date.format("%Y-%m-%d").to_string(),
                        if l.status == "active" { "存续".to_string() } else { "已结清".to_string() },
                    ]);
                }
                ("融资", sh)
            }
            3 => {
                let rows = findb::funds::cash_count_list(ctx.db()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "现金盘点",
                    vec!["日期".to_string(), "科目".to_string(), "账面余额".to_string(), "实盘金额".to_string(), "差异".to_string(), "备注".to_string()],
                );
                for c in rows {
                    sh.push(vec![
                        c.date.format("%Y-%m-%d").to_string(),
                        c.account_code.clone(),
                        c.book_amount.fmt_plain(),
                        c.counted.fmt_plain(),
                        c.diff.fmt_plain(),
                        c.memo.clone(),
                    ]);
                }
                ("现金盘点", sh)
            }
            4 => {
                let rows = findb::funds::check_list(ctx.db()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "支票登记簿",
                    vec!["票号".to_string(), "类型".to_string(), "付款科目".to_string(), "收款人".to_string(), "金额".to_string(), "开出日".to_string(), "状态".to_string(), "备注".to_string()],
                );
                for c in rows {
                    sh.push(vec![
                        c.no.clone(),
                        if c.kind == "cash" { "现金支票".to_string() } else { "转账支票".to_string() },
                        c.bank_account.clone(),
                        c.payee.clone(),
                        c.amount.fmt_plain(),
                        c.issued_date.format("%Y-%m-%d").to_string(),
                        if c.status == "void" { "已作废".to_string() } else { "已开出".to_string() },
                        c.memo.clone(),
                    ]);
                }
                ("支票登记簿", sh)
            }
            5 => {
                let code = if self.j_account.trim().is_empty() {
                    "1001".to_string()
                } else {
                    self.j_account.trim().to_string()
                };
                let p = ctx.period();
                let q = findb::balances::LedgerQuery {
                    code: code.clone(),
                    include_children: false,
                    aux: None,
                    from: p,
                    to: p,
                    posted_only: true,
                    prepared_by: None,
                    code_from: None,
                    code_to: None,
                }
                .with_user_scope(ctx.user());
                let rows = findb::balances::journal(ctx.db(), ctx.chart(), &q).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "出纳日记账",
                    vec!["日期".to_string(), "凭证号".to_string(), "摘要".to_string(), "对方科目".to_string(), "借方".to_string(), "贷方".to_string(), "余额".to_string()],
                );
                for r in rows {
                    sh.push(vec![
                        r.date.format("%Y-%m-%d").to_string(),
                        r.voucher_no.clone(),
                        r.summary.clone(),
                        r.opposite_accounts.clone(),
                        r.debit.fmt_plain(),
                        r.credit.fmt_plain(),
                        r.balance.fmt_plain(),
                    ]);
                }
                ("出纳日记账", sh)
            }
            6 => {
                let rows = findb::funds::advance_list(ctx.db()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "员工借支",
                    vec!["编号".to_string(), "日期".to_string(), "借支人".to_string(), "事由".to_string(), "金额".to_string(), "支付账户".to_string(), "状态".to_string()],
                );
                for a in rows {
                    sh.push(vec![
                        a.no.clone(),
                        a.date.format("%Y-%m-%d").to_string(),
                        a.employee.clone(),
                        a.purpose.clone(),
                        a.amount.fmt_plain(),
                        a.pay_account.clone(),
                        match a.status.as_str() {
                            "approved" => "待支付",
                            "paid" => "已支付",
                            _ => "已核销",
                        }
                        .to_string(),
                    ]);
                }
                ("员工借支", sh)
            }
            7 => {
                let rows = findb::funds::funds_budget(ctx.db(), ctx.period()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "资金预算",
                    vec!["科目".to_string(), "科目名称".to_string(), "预算".to_string(), "实际".to_string(), "差异".to_string(), "执行率".to_string(), "状态".to_string()],
                );
                for r in rows {
                    sh.push(vec![
                        r.account_code.clone(),
                        r.account_name.clone(),
                        r.budget.fmt_plain(),
                        r.actual.fmt_plain(),
                        r.diff.fmt_plain(),
                        r.rate_pct(),
                        if r.over { "超预算".to_string() } else { "正常".to_string() },
                    ]);
                }
                ("资金预算", sh)
            }
            8 => {
                let rows = findb::receipt::receipt_list(ctx.db()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "收付款单",
                    vec!["单号".to_string(), "日期".to_string(), "类型".to_string(), "资金账户".to_string(), "往来单位".to_string(), "金额".to_string(), "凭证".to_string(), "备注".to_string()],
                );
                for d in rows {
                    sh.push(vec![
                        d.no.clone(),
                        d.date.format("%Y-%m-%d").to_string(),
                        if d.kind == "receipt" { "收款".to_string() } else { "付款".to_string() },
                        d.fund_account.clone(),
                        d.party.clone(),
                        d.amount.fmt_plain(),
                        d.voucher_id.map(|v| format!("#{v}")).unwrap_or_default(),
                        d.memo.clone(),
                    ]);
                }
                ("收付款单", sh)
            }
            _ => {
                let fc = findb::funds::funds_forecast(ctx.db(), ctx.period()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "资金预测",
                    vec!["项目".to_string(), "金额".to_string()],
                );
                sh.push(vec!["现金/银行结存".to_string(), fc.cash_balance.fmt_plain()]);
                sh.push(vec!["在库应收票据".to_string(), fc.receivable_bills.fmt_plain()]);
                sh.push(vec!["应付票据".to_string(), fc.payable_bills.fmt_plain()]);
                sh.push(vec!["放款可收回".to_string(), fc.lend.fmt_plain()]);
                sh.push(vec!["借款需偿还".to_string(), fc.borrow.fmt_plain()]);
                sh.push(vec!["预计资金头寸".to_string(), fc.position.fmt_plain()]);
                ("资金预测", sh)
            }
        };
        let title = format!("{name}（{}）", ctx.period().label());
        match crate::views::export::run_export(&sh, name, &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }

    fn show_daily(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.dirty {
            self.dirty = false;
        }
        // 按日资金日报（口径：仅已记账，与账簿/报表一致）：默认今天，可前后翻日
        let today = chrono::Local::now().date_naive();
        if self.daily_date.is_none() {
            self.daily_date = Some(today);
        }
        let date = self.daily_date.unwrap_or(today);
        widgets::toolbar(ui, |ui| {
            if ui.button("« 前一日").clicked() {
                self.daily_date = Some(date - chrono::Duration::days(1));
            }
            if ui.button("后一日 »").clicked() {
                self.daily_date = Some(date + chrono::Duration::days(1));
            }
            if ui.button("今天").clicked() {
                self.daily_date = Some(today);
            }
            ui.label(RichText::new(date.format("%Y-%m-%d").to_string()).strong());
        });
        let rows = findb::funds::funds_daily_by_date(ctx.db(), date).unwrap_or_default();
        let cols = [
            widgets::TCol::new("科目", 110.0).fixed(),
            widgets::TCol::new("科目名称", 160.0),
            widgets::TCol::new("上日结余", 130.0).right(),
            widgets::TCol::new("本日收入", 130.0).right(),
            widgets::TCol::new("本日支出", 130.0).right(),
            widgets::TCol::new("日末结存", 130.0).right(),
        ];
        widgets::grid(ui, "funds_daily", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.account_code).monospace()); }
                1 => { ui.label(&r.account_name); }
                2 => widgets::amount_label(ui, r.begin),
                3 => widgets::amount_label(ui, r.income),
                4 => widgets::amount_label(ui, r.expense),
                5 => widgets::amount_label(ui, r.end),
                _ => {}
            }
        });
    }

    fn show_bills(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("类型");
            egui::ComboBox::from_id_salt("bill_kind")
                .selected_text(if self.bill_kind.is_empty() {
                    "全部".to_string()
                } else {
                    self.bill_kind.clone()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.bill_kind, String::new(), "全部");
                    ui.selectable_value(&mut self.bill_kind, "receivable".to_string(), "应收票据");
                    ui.selectable_value(&mut self.bill_kind, "payable".to_string(), "应付票据");
                });
            if ui.button("新增票据").clicked() {
                let db = ctx.db();
                let who = ctx.user().username.clone();
                let mut b = findb::funds::Bill {
                    id: 0,
                    kind: "receivable".to_string(),
                    no: format!("PJ-{}", chrono::Local::now().format("%Y%m%d%H%M%S")),
                    period: ctx.period(),
                    issue_date: chrono::Local::now().date_naive(),
                    due_date: chrono::Local::now().date_naive(),
                    counterpart: String::new(),
                    bank: String::new(),
                    amount: Money::ZERO,
                    status: "in_hand".to_string(),
                    handled_date: None,
                    memo: String::new(),
                    created_by: who,
                    created_at: String::new(),
                    voucher_id: None,
                };
                match findb::funds::bill_save(db, &mut b) {
                    Ok(id) => {
                        ctx.log("资金", "新增票据", &format!("#{id} {}", b.no));
                        ctx.info("已新增票据");
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });

        let rows = findb::funds::bill_list(ctx.db(), if self.bill_kind.is_empty() { None } else { Some(&self.bill_kind) })
            .unwrap_or_default();
        let cols = [
            widgets::TCol::new("类型", 70.0).fixed(),
            widgets::TCol::new("票据号", 130.0).fixed(),
            widgets::TCol::new("出票日", 100.0).fixed(),
            widgets::TCol::new("到期日", 100.0).fixed(),
            widgets::TCol::new("对方单位", 140.0),
            widgets::TCol::new("金额", 130.0).right(),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("操作", 120.0).fixed(),
        ];
        let mut act: Option<(i64, String)> = None;
        let mut vact: Option<i64> = None;
        widgets::grid(ui, "bills", &cols, rows.len(), 24.0, |i, c, ui| {
            let b = &rows[i];
            match c {
                0 => { ui.label(if b.kind == "receivable" { "应收" } else { "应付" }); }
                1 => { ui.label(&b.no); }
                2 => { ui.label(b.issue_date.format("%Y-%m-%d").to_string()); }
                3 => { ui.label(b.due_date.format("%Y-%m-%d").to_string()); }
                4 => { ui.label(if b.counterpart.is_empty() { "—".to_string() } else { b.counterpart.clone() }); }
                5 => widgets::amount_label(ui, b.amount),
                6 => {
                    let s = findb::funds::BillStatus::parse(&b.status);
                    ui.label(RichText::new(s.label()).color(match s {
                        findb::funds::BillStatus::Settled => palette::OK,
                        findb::funds::BillStatus::InHand => palette::WARN,
                        _ => palette::CREDIT,
                    }));
                }
                7 => {
                    if b.status == "in_hand" {
                        ui.horizontal(|ui| {
                            if ui.small_button("背书").clicked() {
                                act = Some((b.id, "endorsed".to_string()));
                            }
                            if ui.small_button("贴现").clicked() {
                                act = Some((b.id, "discounted".to_string()));
                            }
                            if ui.small_button("兑付").clicked() {
                                act = Some((b.id, "settled".to_string()));
                            }
                        });
                    } else if matches!(b.status.as_str(), "endorsed" | "discounted" | "settled")
                        && b.voucher_id.is_none()
                    {
                        // 流转已自动生成台账凭证；此处供存量台账回填
                        if ui.small_button("生成凭证").clicked() {
                            vact = Some(b.id);
                        }
                    }
                }
                _ => {}
            }
        });
        if let Some((id, to)) = act {
            let st = findb::funds::BillStatus::parse(&to);
            let who = ctx.user().username.clone();
            match findb::funds::bill_transition(
                ctx.db(),
                id,
                st,
                chrono::Local::now().date_naive(),
                &who,
            ) {
                Ok(vid) => {
                    ctx.log("资金", "票据流转", &format!("#{id} → {}", st.label()));
                    ctx.info(match vid {
                        Some(v) => format!("已更新票据状态，生成凭证 #{v}"),
                        None => "已更新票据状态".to_string(),
                    });
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
        if let Some(id) = vact {
            let who = ctx.user().username.clone();
            match findb::funds::bill_voucher(ctx.db(), id, &who) {
                Ok(vid) => {
                    ctx.log("资金", "生成票据凭证", &format!("#{id} → 凭证 #{vid}"));
                    ctx.info(format!("已生成凭证 #{vid}"));
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn show_loans(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("类型");
            egui::ComboBox::from_id_salt("loan_kind")
                .selected_text(if self.loan_kind.is_empty() {
                    "全部".to_string()
                } else {
                    self.loan_kind.clone()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.loan_kind, String::new(), "全部");
                    ui.selectable_value(&mut self.loan_kind, "borrow".to_string(), "借款");
                    ui.selectable_value(&mut self.loan_kind, "lend".to_string(), "放款");
                });
            if ui.button("新增融资").clicked() {
                let db = ctx.db();
                let who = ctx.user().username.clone();
                let mut l = findb::funds::Loan {
                    id: 0,
                    kind: "borrow".to_string(),
                    no: format!("DK-{}", chrono::Local::now().format("%Y%m%d%H%M%S")),
                    bank: String::new(),
                    principal: Money::ZERO,
                    rate_pct: Money::ZERO,
                    start_date: chrono::Local::now().date_naive(),
                    end_date: chrono::Local::now().date_naive(),
                    status: "active".to_string(),
                    memo: String::new(),
                    created_by: who,
                    created_at: String::new(),
                    voucher_id: None,
                    settle_voucher_id: None,
                    settle_date: None,
                };
                match findb::funds::loan_save(db, &mut l) {
                    Ok(id) => {
                        ctx.log("资金", "新增融资", &format!("#{id} {}", l.no));
                        ctx.info("已新增融资");
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });

        let rows = findb::funds::loan_list(ctx.db(), if self.loan_kind.is_empty() { None } else { Some(&self.loan_kind) })
            .unwrap_or_default();
        let cols = [
            widgets::TCol::new("类型", 70.0).fixed(),
            widgets::TCol::new("编号", 130.0).fixed(),
            widgets::TCol::new("机构", 140.0),
            widgets::TCol::new("本金", 130.0).right(),
            widgets::TCol::new("年利率%", 90.0).right(),
            widgets::TCol::new("起息日", 100.0).fixed(),
            widgets::TCol::new("到期日", 100.0).fixed(),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("操作", 110.0).fixed(),
        ];
        let mut settle: Option<i64> = None;
        let mut gen: Option<i64> = None;
        widgets::grid(ui, "loans", &cols, rows.len(), 24.0, |i, c, ui| {
            let l = &rows[i];
            match c {
                0 => { ui.label(if l.kind == "borrow" { "借款" } else { "放款" }); }
                1 => { ui.label(&l.no); }
                2 => { ui.label(if l.bank.is_empty() { "—".to_string() } else { l.bank.clone() }); }
                3 => widgets::amount_label(ui, l.principal),
                4 => { ui.label(l.rate_pct.fmt_qty()); }
                5 => { ui.label(l.start_date.format("%Y-%m-%d").to_string()); }
                6 => { ui.label(l.end_date.format("%Y-%m-%d").to_string()); }
                7 => {
                    if l.status == "active" {
                        if ui.small_button("结清").clicked() {
                            settle = Some(l.id);
                        }
                    } else {
                        ui.label(RichText::new("已结清").color(palette::OK));
                    }
                }
                8 => {
                    if l.status == "active" && l.voucher_id.is_none() {
                        if ui.small_button("到账凭证").clicked() {
                            gen = Some(l.id);
                        }
                    } else if l.status != "active" && l.settle_voucher_id.is_none() {
                        if ui.small_button("还本凭证").clicked() {
                            gen = Some(l.id);
                        }
                    }
                }
                _ => {}
            }
        });
        if let Some(id) = settle {
            let who = ctx.user().username.clone();
            match findb::funds::loan_settle(ctx.db(), id, chrono::Local::now().date_naive(), &who)
            {
                Ok(vid) => {
                    ctx.log("资金", "结清融资", &format!("#{id}"));
                    ctx.info(match vid {
                        Some(v) => format!("已结清，生成还本凭证 #{v}"),
                        None => "已结清".to_string(),
                    });
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
        if let Some(id) = gen {
            let who = ctx.user().username.clone();
            match findb::funds::loan_voucher(ctx.db(), id, &who) {
                Ok(vid) => {
                    ctx.log("资金", "生成融资凭证", &format!("#{id} → 凭证 #{vid}"));
                    ctx.info(format!("已生成凭证 #{vid}"));
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn show_counts(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
        if self.cc_date.is_empty() {
            self.cc_date = today.clone();
        }
        widgets::toolbar(ui, |ui| {
            ui.label("日期");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.cc_date));
            ui.label("科目");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.cc_account));
            ui.label("实盘金额");
            ui.add_sized([100.0, 22.0], egui::TextEdit::singleline(&mut self.cc_counted));
            ui.label("备注");
            ui.add_sized([140.0, 22.0], egui::TextEdit::singleline(&mut self.cc_memo));
            if ui.button("新增盘点").clicked() {
                match chrono::NaiveDate::parse_from_str(&self.cc_date, "%Y-%m-%d") {
                    Ok(date) => {
                        let mut c = findb::funds::CashCount {
                            id: 0,
                            period: ctx.period(),
                            date,
                            account_code: if self.cc_account.trim().is_empty() {
                                "1001".to_string()
                            } else {
                                self.cc_account.trim().to_string()
                            },
                            book_amount: Money::ZERO,
                            counted: Money::parse_or_zero(&self.cc_counted),
                            diff: Money::ZERO,
                            memo: self.cc_memo.clone(),
                            voucher_id: None,
                            created_by: ctx.user().username.clone(),
                            created_at: String::new(),
                        };
                        match findb::funds::cash_count_save(ctx.db(), &mut c) {
                            Ok(_) => {
                                ctx.log(
                                    "资金",
                                    "现金盘点",
                                    &format!(
                                        "{} {} 实盘 {}",
                                        c.account_code,
                                        c.date.format("%Y-%m-%d"),
                                        c.counted.fmt_money()
                                    ),
                                );
                                ctx.info(format!(
                                    "账面 {}，差异 {}",
                                    c.book_amount.fmt_money(),
                                    c.diff.fmt_money()
                                ));
                                self.dirty = true;
                            }
                            Err(e) => ctx.error(e.to_string()),
                        }
                    }
                    Err(_) => ctx.error("日期格式应为 YYYY-MM-DD"),
                }
            }
        });

        let rows = findb::funds::cash_count_list(ctx.db()).unwrap_or_default();
        let cols = [
            widgets::TCol::new("日期", 100.0).fixed(),
            widgets::TCol::new("科目", 80.0).fixed(),
            widgets::TCol::new("账面余额", 120.0).right(),
            widgets::TCol::new("实盘金额", 120.0).right(),
            widgets::TCol::new("差异", 110.0).right(),
            widgets::TCol::new("备注", 150.0),
            widgets::TCol::new("操作", 150.0).fixed(),
        ];
        let mut gen: Option<i64> = None;
        let mut del: Option<i64> = None;
        widgets::grid(ui, "cash_count", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => {
                    ui.label(r.date.format("%Y-%m-%d").to_string());
                }
                1 => {
                    ui.label(RichText::new(&r.account_code).monospace());
                }
                2 => widgets::amount_label(ui, r.book_amount),
                3 => widgets::amount_label(ui, r.counted),
                4 => {
                    if r.diff.is_zero() {
                        ui.label(RichText::new(r.diff.fmt_money()).weak());
                    } else if r.diff.is_positive() {
                        ui.label(
                            RichText::new(format!("+{}", r.diff.fmt_money())).color(palette::OK),
                        );
                    } else {
                        ui.label(RichText::new(r.diff.fmt_money()).color(palette::WARN));
                    }
                }
                5 => {
                    ui.label(&r.memo);
                }
                6 => {
                    ui.horizontal(|ui| {
                        if r.voucher_id.is_none() && !r.diff.is_zero() {
                            if ui.small_button("生成凭证").clicked() {
                                gen = Some(r.id);
                            }
                        }
                        if r.voucher_id.is_some() {
                            ui.label(RichText::new("已出凭证").weak());
                        }
                        if ui.small_button("删除").clicked() {
                            del = Some(r.id);
                        }
                    });
                }
                _ => {}
            }
        });
        if let Some(id) = gen {
            let who = ctx.user().username.clone();
            match findb::funds::cash_count_voucher(ctx.db(), id, &who) {
                Ok(vid) => {
                    ctx.log("资金", "盘点差异出凭证", &format!("#{id} → 凭证 #{vid}"));
                    ctx.info(format!("已生成盘盈盘亏凭证 #{vid}"));
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
        if let Some(id) = del {
            match findb::funds::cash_count_delete(ctx.db(), id) {
                Ok(()) => {
                    ctx.log("资金", "删除盘点记录", &format!("#{id}"));
                    ctx.info("已删除盘点记录");
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn show_checks(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
        if self.ck_date.is_empty() {
            self.ck_date = today.clone();
        }
        widgets::toolbar(ui, |ui| {
            ui.label("票号");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.ck_no));
            ui.label("类型");
            egui::ComboBox::from_id_salt("ck_kind")
                .selected_text(if self.ck_kind == "cash" { "现金支票" } else { "转账支票" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.ck_kind, "transfer".to_string(), "转账支票");
                    ui.selectable_value(&mut self.ck_kind, "cash".to_string(), "现金支票");
                });
            ui.label("付款科目");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.ck_bank));
            ui.label("收款人");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.ck_payee));
            ui.label("金额");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.ck_amount));
            ui.label("开出日");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.ck_date));
            ui.label("备注");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.ck_memo));
            if ui.button("新增支票").clicked() {
                if self.ck_no.trim().is_empty() {
                    ctx.error("支票号必填");
                } else {
                    match chrono::NaiveDate::parse_from_str(&self.ck_date, "%Y-%m-%d") {
                        Ok(date) => {
                            let mut c = findb::funds::CheckRow {
                                id: 0,
                                no: self.ck_no.trim().to_string(),
                                kind: self.ck_kind.clone(),
                                bank_account: self.ck_bank.trim().to_string(),
                                payee: self.ck_payee.trim().to_string(),
                                amount: Money::parse_or_zero(&self.ck_amount),
                                issued_date: date,
                                status: "issued".to_string(),
                                memo: self.ck_memo.clone(),
                                created_by: ctx.user().username.clone(),
                                created_at: String::new(),
                            };
                            match findb::funds::check_save(ctx.db(), &mut c) {
                                Ok(_) => {
                                    ctx.log("资金", "登记支票", &format!("{} {}", c.no, c.amount.fmt_money()));
                                    ctx.info("已登记支票");
                                    self.ck_no.clear();
                                    self.ck_amount.clear();
                                    self.ck_payee.clear();
                                    self.ck_memo.clear();
                                }
                                Err(e) => ctx.error(e.to_string()),
                            }
                        }
                        Err(_) => ctx.error("开出日期格式应为 YYYY-MM-DD"),
                    }
                }
            }
        });

        let rows = findb::funds::check_list(ctx.db()).unwrap_or_default();
        let cols = [
            widgets::TCol::new("票号", 110.0).fixed(),
            widgets::TCol::new("类型", 90.0).fixed(),
            widgets::TCol::new("付款科目", 90.0).fixed(),
            widgets::TCol::new("收款人", 140.0),
            widgets::TCol::new("金额", 120.0).right(),
            widgets::TCol::new("开出日", 100.0).fixed(),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("操作", 140.0).fixed(),
        ];
        let mut st_act: Option<(i64, String)> = None;
        let mut del: Option<i64> = None;
        widgets::grid(ui, "check_register", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.no).monospace()); }
                1 => { ui.label(if r.kind == "cash" { "现金支票" } else { "转账支票" }); }
                2 => { ui.label(RichText::new(&r.bank_account).monospace()); }
                3 => { ui.label(if r.payee.is_empty() { "—".to_string() } else { r.payee.clone() }); }
                4 => widgets::amount_label(ui, r.amount),
                5 => { ui.label(r.issued_date.format("%Y-%m-%d").to_string()); }
                6 => {
                    ui.label(if r.status == "void" {
                        RichText::new("已作废").color(palette::WARN)
                    } else {
                        RichText::new("已开出").color(palette::OK)
                    });
                }
                7 => {
                    ui.horizontal(|ui| {
                        if r.status == "void" {
                            if ui.small_button("恢复").clicked() {
                                st_act = Some((r.id, "issued".to_string()));
                            }
                        } else if ui.small_button("作废").clicked() {
                            st_act = Some((r.id, "void".to_string()));
                        }
                        if ui.small_button("删除").clicked() {
                            del = Some(r.id);
                        }
                    });
                }
                _ => {}
            }
        });
        if let Some((id, status)) = st_act {
            match findb::funds::check_set_status(ctx.db(), id, &status) {
                Ok(()) => {
                    ctx.log("资金", "支票状态", &format!("#{id} → {status}"));
                    ctx.info(if status == "void" { "支票已作废" } else { "支票已恢复" });
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
        if let Some(id) = del {
            match findb::funds::check_delete(ctx.db(), id) {
                Ok(()) => {
                    ctx.log("资金", "删除支票", &format!("#{id}"));
                    ctx.info("已删除支票记录");
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    /// 出纳日记账：只读已记账逐笔滚动 + 日清标记 + 收付登记（跳凭证录入预填科目）
    fn show_journal(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
        if self.j_date.is_empty() {
            self.j_date = today.clone();
        }
        let p = ctx.period();
        let code = if self.j_account.trim().is_empty() {
            "1001".to_string()
        } else {
            self.j_account.trim().to_string()
        };
        widgets::toolbar(ui, |ui| {
            ui.label("科目");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.j_account));
            ui.separator();
            ui.label("日清日期");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.j_date));
        });
        widgets::toolbar(ui, |ui| {
            if ui.button("标记日清").clicked() {
                match chrono::NaiveDate::parse_from_str(&self.j_date, "%Y-%m-%d") {
                    Ok(d0) => {
                        let who = ctx.user().username.clone();
                        match findb::funds::day_clear_set(ctx.db(), &code, d0, true, &who) {
                            Ok(()) => {
                                ctx.log("资金", "日记账日清", &format!("{} {}", code, self.j_date));
                                ctx.info("已标记日清");
                            }
                            Err(e) => ctx.error(e.to_string()),
                        }
                    }
                    Err(_) => ctx.error("日清日期格式应为 YYYY-MM-DD"),
                }
            }
            if ui.button("取消日清").clicked() {
                match chrono::NaiveDate::parse_from_str(&self.j_date, "%Y-%m-%d") {
                    Ok(d0) => {
                        let who = ctx.user().username.clone();
                        match findb::funds::day_clear_set(ctx.db(), &code, d0, false, &who) {
                            Ok(()) => {
                                ctx.log("资金", "取消日清", &format!("{} {}", code, self.j_date));
                                ctx.info("已取消日清");
                            }
                            Err(e) => ctx.error(e.to_string()),
                        }
                    }
                    Err(_) => ctx.error("日清日期格式应为 YYYY-MM-DD"),
                }
            }
            ui.separator();
            if ui.button("收款登记").clicked() {
                ctx.info(format!("切到凭证录入：{code} 记借方"));
                ctx.st.pending_cash = Some(code.clone());
                ctx.st.nav = crate::state::NavItem::VoucherNew;
            }
            if ui.button("付款登记").clicked() {
                ctx.info(format!("切到凭证录入：{code} 记贷方"));
                ctx.st.pending_cash = Some(code.clone());
                ctx.st.nav = crate::state::NavItem::VoucherNew;
            }
        });

        let q = findb::balances::LedgerQuery {
            code: code.clone(),
            include_children: false,
            aux: None,
            from: p,
            to: p,
            posted_only: true,
            prepared_by: None,
            code_from: None,
            code_to: None,
        }
        .with_user_scope(ctx.user());
        let rows = findb::balances::journal(ctx.db(), ctx.chart(), &q).unwrap_or_default();
        let cleared =
            findb::funds::day_clear_dates(ctx.db(), &code, p.first_day(), p.last_day())
                .unwrap_or_default();
        let cols = [
            widgets::TCol::new("日期", 92.0).fixed(),
            widgets::TCol::new("凭证号", 88.0).fixed(),
            widgets::TCol::new("摘要", 220.0),
            widgets::TCol::new("对方科目", 220.0),
            widgets::TCol::new("借方", 120.0).right(),
            widgets::TCol::new("贷方", 120.0).right(),
            widgets::TCol::new("余额", 130.0).right(),
            widgets::TCol::new("日清", 80.0).fixed(),
        ];
        widgets::grid(ui, "cashier_journal", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(r.date.format("%Y-%m-%d").to_string()); }
                1 => { ui.label(RichText::new(&r.voucher_no).monospace()); }
                2 => { ui.label(&r.summary); }
                3 => { ui.label(RichText::new(&r.opposite_accounts).weak()); }
                4 => widgets::amount_label(ui, r.debit),
                5 => widgets::amount_label(ui, r.credit),
                6 => widgets::amount_label(ui, r.balance),
                7 => {
                    if cleared.contains(&r.date.format("%Y-%m-%d").to_string()) {
                        ui.label(RichText::new("✓ 已日清").color(palette::OK));
                    } else {
                        ui.label(RichText::new("—").weak());
                    }
                }
                _ => {}
            }
        });
        if rows.is_empty() {
            widgets::empty_hint(ui, "该科目本期无已记账记录");
        }
    }

    /// 员工借支：建单 → 支付（出凭证）→ 核销冲账（出凭证）
    fn show_advances(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
        if self.ad_date.is_empty() {
            self.ad_date = today.clone();
        }
        widgets::toolbar(ui, |ui| {
            ui.label("日期");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.ad_date));
            ui.label("借支人");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.ad_emp));
            ui.label("事由");
            ui.add_sized([140.0, 22.0], egui::TextEdit::singleline(&mut self.ad_purpose));
            ui.label("金额");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.ad_amount));
            ui.label("支付账户");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.ad_account));
            ui.label("备注");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.ad_memo));
            if ui.button("新增借支").clicked() {
                match chrono::NaiveDate::parse_from_str(&self.ad_date, "%Y-%m-%d") {
                    Ok(date) => {
                        let mut a = findb::funds::Advance {
                            id: 0,
                            no: format!("JZ-{}", chrono::Local::now().format("%Y%m%d%H%M%S")),
                            period: ctx.period(),
                            date,
                            employee: self.ad_emp.trim().to_string(),
                            purpose: self.ad_purpose.trim().to_string(),
                            amount: Money::parse_or_zero(&self.ad_amount),
                            pay_account: if self.ad_account.trim().is_empty() {
                                "1001".to_string()
                            } else {
                                self.ad_account.trim().to_string()
                            },
                            status: "approved".to_string(),
                            paid_date: None,
                            paid_voucher_id: None,
                            settle_date: None,
                            settle_voucher_id: None,
                            expense_account: "660201".to_string(),
                            memo: self.ad_memo.clone(),
                            created_by: ctx.user().username.clone(),
                            created_at: String::new(),
                        };
                        match findb::funds::advance_save(ctx.db(), &mut a) {
                            Ok(_) => {
                                ctx.log(
                                    "资金",
                                    "借支建单",
                                    &format!("{} {} {}", a.no, a.employee, a.amount.fmt_money()),
                                );
                                ctx.info("已新增借支单");
                                self.ad_emp.clear();
                                self.ad_purpose.clear();
                                self.ad_amount.clear();
                                self.ad_memo.clear();
                            }
                            Err(e) => ctx.error(e.to_string()),
                        }
                    }
                    Err(_) => ctx.error("日期格式应为 YYYY-MM-DD"),
                }
            }
        });
        widgets::toolbar(ui, |ui| {
            ui.label(RichText::new("核销参数（支付/核销按今天记账）：").weak());
            ui.label("冲账费用科目");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.ad_exp_account));
            ui.label("冲账金额");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.ad_exp_amount));
        });

        let rows = findb::funds::advance_list(ctx.db()).unwrap_or_default();
        let cols = [
            widgets::TCol::new("编号", 130.0).fixed(),
            widgets::TCol::new("日期", 100.0).fixed(),
            widgets::TCol::new("借支人", 90.0).fixed(),
            widgets::TCol::new("事由", 160.0),
            widgets::TCol::new("金额", 110.0).right(),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("操作", 150.0).fixed(),
        ];
        let mut pay: Option<i64> = None;
        let mut settle: Option<i64> = None;
        let mut del: Option<i64> = None;
        widgets::grid(ui, "advances", &cols, rows.len(), 24.0, |i, c, ui| {
            let a = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&a.no).monospace()); }
                1 => { ui.label(a.date.format("%Y-%m-%d").to_string()); }
                2 => { ui.label(&a.employee); }
                3 => { ui.label(&a.purpose); }
                4 => widgets::amount_label(ui, a.amount),
                5 => {
                    ui.label(match a.status.as_str() {
                        "approved" => RichText::new("待支付").color(palette::WARN),
                        "paid" => RichText::new("已支付").color(palette::CREDIT),
                        _ => RichText::new("已核销").color(palette::OK),
                    });
                }
                6 => {
                    ui.horizontal(|ui| {
                        if a.status == "approved" {
                            if ui.small_button("支付").clicked() {
                                pay = Some(a.id);
                            }
                            if ui.small_button("删除").clicked() {
                                del = Some(a.id);
                            }
                        } else if a.status == "paid" && ui.small_button("核销").clicked() {
                            settle = Some(a.id);
                        }
                    });
                }
                _ => {}
            }
        });
        let today_d = chrono::Local::now().date_naive();
        if let Some(id) = pay {
            let who = ctx.user().username.clone();
            match findb::funds::advance_pay(ctx.db(), id, today_d, &who) {
                Ok(vid) => {
                    ctx.log("资金", "借支支付", &format!("#{id}"));
                    ctx.info(match vid {
                        Some(v) => format!("已支付，生成凭证 #{v}"),
                        None => "该借支已支付过".to_string(),
                    });
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
        if let Some(id) = settle {
            if self.ad_exp_amount.trim().is_empty() {
                ctx.error("请先在上方填写冲账金额（全额退回填 0）");
            } else {
                let exp_acct = if self.ad_exp_account.trim().is_empty() {
                    "660201".to_string()
                } else {
                    self.ad_exp_account.trim().to_string()
                };
                let exp = Money::parse_or_zero(&self.ad_exp_amount);
                let who = ctx.user().username.clone();
                match findb::funds::advance_settle(ctx.db(), id, &exp_acct, exp, today_d, &who) {
                    Ok(vid) => {
                        ctx.log("资金", "借支核销", &format!("#{id} 冲账 {}", exp.fmt_money()));
                        ctx.info(match vid {
                            Some(v) => format!("已核销，生成凭证 #{v}"),
                            None => "该借支已核销过".to_string(),
                        });
                        self.ad_exp_amount.clear();
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        }
        if let Some(id) = del {
            match findb::funds::advance_delete(ctx.db(), id) {
                Ok(()) => {
                    ctx.log("资金", "删除借支单", &format!("#{id}"));
                    ctx.info("已删除借支单");
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    /// 资金预算 vs 执行（现金/银行科目；实际=当期已记账净额，H-3）
    fn show_budget(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let rows = findb::funds::funds_budget(ctx.db(), ctx.period()).unwrap_or_default();
        if rows.is_empty() {
            widgets::empty_hint(
                ui,
                "本期未编制现金/银行科目预算：到【预算分析】为 1001/1002 等科目新增预算行",
            );
            return;
        }
        let cols = [
            widgets::TCol::new("科目", 100.0).fixed(),
            widgets::TCol::new("科目名称", 150.0),
            widgets::TCol::new("预算", 120.0).right(),
            widgets::TCol::new("实际(净额)", 130.0).right(),
            widgets::TCol::new("差异", 120.0).right(),
            widgets::TCol::new("执行率", 90.0).right(),
            widgets::TCol::new("状态", 90.0).fixed(),
        ];
        widgets::grid(ui, "funds_budget", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.account_code).monospace()); }
                1 => { ui.label(&r.account_name); }
                2 => widgets::amount_label(ui, r.budget),
                3 => widgets::amount_label(ui, r.actual),
                4 => widgets::amount_label(ui, r.diff),
                5 => { ui.label(r.rate_pct()); }
                6 => {
                    ui.label(if r.over {
                        RichText::new("超预算").color(palette::WARN)
                    } else {
                        RichText::new("正常").color(palette::OK)
                    });
                }
                _ => {}
            }
        });
    }

    /// 收付款单：保存即出凭证 + 按往来单位 FIFO 自动核销（对标金蝶收款单/付款单）
    fn show_receipts(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
        if self.rc_date.is_empty() {
            self.rc_date = today.clone();
        }
        widgets::toolbar(ui, |ui| {
            ui.label("日期");
            ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut self.rc_date));
            ui.label("类型");
            egui::ComboBox::from_id_salt("rc_kind")
                .selected_text(if self.rc_kind == "payment" { "付款" } else { "收款" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.rc_kind, "receipt".to_string(), "收款");
                    ui.selectable_value(&mut self.rc_kind, "payment".to_string(), "付款");
                });
            ui.label("资金账户");
            ui.add_sized(
                [90.0, 22.0],
                egui::TextEdit::singleline(&mut self.rc_fund).hint_text("默认100201"),
            );
            ui.label("往来单位");
            ui.add_sized(
                [110.0, 22.0],
                egui::TextEdit::singleline(&mut self.rc_party).hint_text("辅助编码 C01/S01"),
            );
            ui.label("金额");
            ui.add_sized([100.0, 22.0], egui::TextEdit::singleline(&mut self.rc_amount));
            ui.label("备注");
            ui.add_sized([120.0, 22.0], egui::TextEdit::singleline(&mut self.rc_memo));
            if ui.button("新增收付款").clicked() {
                match chrono::NaiveDate::parse_from_str(&self.rc_date, "%Y-%m-%d") {
                    Ok(date) => {
                        let who = ctx.user().username.clone();
                        let amount = Money::parse_or_zero(&self.rc_amount);
                        match findb::receipt::receipt_create(
                            ctx.db(),
                            &self.rc_kind,
                            date,
                            &self.rc_fund,
                            &self.rc_party,
                            amount,
                            &self.rc_memo,
                            &who,
                        ) {
                            Ok(doc_id) => {
                                ctx.log(
                                    "资金",
                                    "新增收付款（待审核）",
                                    &format!("{} {} 单 #{doc_id}", self.rc_kind, amount.fmt_money()),
                                );
                                ctx.info("已保存为草稿；审核后生成凭证并自动核销");
                                self.rc_amount.clear();
                                self.rc_memo.clear();
                            }
                            Err(e) => ctx.error(e.to_string()),
                        }
                    }
                    Err(_) => ctx.error("日期格式应为 YYYY-MM-DD"),
                }
            }
            if ui.button("打印全部").clicked() {
                let rows = findb::receipt::receipt_list(ctx.db()).unwrap_or_default();
                if rows.is_empty() {
                    ctx.error("没有可打印的收付款单");
                } else {
                    let n = rows.len();
                    let company = ctx.db().options().company.clone();
                    let prints: Vec<findb::printform::ReceiptPrint> = rows
                        .into_iter()
                        .map(|d| findb::printform::ReceiptPrint {
                            no: d.no,
                            date: d.date.format("%Y-%m-%d").to_string(),
                            kind_label: if d.kind == "receipt" { "收款" } else { "付款" }
                                .to_string(),
                            fund: d.fund_account,
                            party: d.party,
                            amount: d.amount,
                            memo: d.memo,
                            voucher_no: d
                                .voucher_id
                                .map(|v| format!("记-{v:04}"))
                                .unwrap_or_default(),
                        })
                        .collect();
                    let html =
                        findb::printform::receipt_forms_html(&company, &prints, &Default::default());
                    match crate::views::export::print_html_content("收付款单套打", &html) {
                        Ok(m) => {
                            ctx.log("资金", "收付款单套打", &format!("{n} 张"));
                            ctx.info(m);
                        }
                        Err(e) => ctx.error(e),
                    }
                }
            }
        });
        ui.label(
            RichText::new("保存即生成记账凭证草稿（记账后入账），并按往来单位对未清挂账 FIFO 自动核销")
                .weak(),
        );

        let rows = findb::receipt::receipt_list(ctx.db()).unwrap_or_default();
        let cols = [
            widgets::TCol::new("单号", 140.0).fixed(),
            widgets::TCol::new("日期", 100.0).fixed(),
            widgets::TCol::new("类型", 70.0).fixed(),
            widgets::TCol::new("资金账户", 90.0).fixed(),
            widgets::TCol::new("往来单位", 110.0).fixed(),
            widgets::TCol::new("金额", 120.0).right(),
            widgets::TCol::new("凭证", 90.0).fixed(),
            widgets::TCol::new("状态", 64.0).fixed(),
            widgets::TCol::new("备注", 140.0),
            widgets::TCol::new("操作", 130.0).fixed(),
        ];
        let mut del: Option<i64> = None;
        let mut act: Option<(i64, bool)> = None; // (id, true=审核 / false=撤审)
        widgets::grid(ui, "receipt_docs", &cols, rows.len(), 24.0, |i, c, ui| {
            let d = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&d.no).monospace()); }
                1 => { ui.label(d.date.format("%Y-%m-%d").to_string()); }
                2 => { ui.label(if d.kind == "receipt" { "收款" } else { "付款" }); }
                3 => { ui.label(RichText::new(&d.fund_account).monospace()); }
                4 => { ui.label(&d.party); }
                5 => widgets::amount_label(ui, d.amount),
                6 => {
                    match d.voucher_id {
                        Some(v) => { ui.label(RichText::new(format!("#{v}")).color(palette::CREDIT)); }
                        None => { ui.label("—"); }
                    }
                }
                7 => {
                    if d.status == "audited" {
                        ui.label("已审核");
                    } else {
                        ui.label(RichText::new("待审核").weak());
                    }
                }
                8 => { ui.label(&d.memo); }
                9 => {
                    if d.status == "draft" && ctx.can(fincore::Perm::VoucherAudit) {
                        if ui.small_button("审核").clicked() {
                            act = Some((d.id, true));
                        }
                    } else if d.status == "audited" && ctx.can(fincore::Perm::VoucherAudit) {
                        if ui.small_button("撤审").clicked() {
                            act = Some((d.id, false));
                        }
                    }
                    if ui.small_button("删除").clicked() {
                        del = Some(d.id);
                    }
                }
                _ => {}
            }
        });
        if let Some((id, is_audit)) = act {
            if is_audit {
                let who = ctx.user().username.clone();
                match findb::receipt::receipt_audit(ctx.db(), id, &who) {
                    Ok((vid, settled)) => {
                        ctx.log("资金", "审核收付款单", &format!("#{id} 凭证 #{vid}"));
                        ctx.info(format!(
                            "已审核，凭证 #{vid}{}",
                            if settled > 0 {
                                format!("（自动核销 {settled} 笔）")
                            } else {
                                String::new()
                            }
                        ));
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            } else {
                match findb::receipt::receipt_unaudit(ctx.db(), id) {
                    Ok(()) => {
                        ctx.log("资金", "撤销审核收付款单", &format!("#{id}"));
                        ctx.info("已撤销审核，单据回到草稿");
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        }
        if let Some(id) = del {
            match findb::receipt::receipt_delete(ctx.db(), id) {
                Ok(()) => {
                    ctx.log("资金", "删除收付款单", &format!("#{id}"));
                    ctx.info("已删除收付款单");
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn show_forecast(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let fc = findb::funds::funds_forecast(ctx.db(), ctx.period()).unwrap_or_default();
        let items = [
            ("现金/银行结存", fc.cash_balance),
            ("在库应收票据", fc.receivable_bills),
            ("应付票据", fc.payable_bills),
            ("放款可收回", fc.lend),
            ("借款需偿还", fc.borrow),
            ("预计资金头寸", fc.position),
        ];
        widgets::grid(ui, "forecast", &[], items.len(), 30.0, |i, c, ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(items[i].0).strong());
                ui.label("：");
                widgets::amount_label(ui, items[i].1);
            });
            let _ = c;
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("头寸 = 结存 + 应收票据 − 应付票据 + 放款 − 借款（在库/存续口径）")
                .weak(),
        );
    }
}

// ===========================================================================
// 预算分析
// ===========================================================================

pub struct BudgetAnalysisView {
    pub year_text: String,
    pub version: String,
    pub dirty: bool,
}

impl Default for BudgetAnalysisView {
    fn default() -> Self {
        Self {
            year_text: String::new(),
            version: String::new(),
            dirty: true,
        }
    }
}

impl BudgetAnalysisView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.year_text.is_empty() {
            self.year_text = ctx.period().year().to_string();
        }
        widgets::page_header(ui, "预算分析", |ui| {
            ui.label(RichText::new("年度逐月预算 vs 实际，按科目 × 部门展开").weak());
        });
        widgets::toolbar(ui, |ui| {
            ui.label("年度");
            let r = ui.add_sized([60.0, 22.0], egui::TextEdit::singleline(&mut self.year_text));
            if r.changed() {
                self.dirty = true;
            }
            ui.label("版本");
            let r2 = ui.add_sized([100.0, 22.0], egui::TextEdit::singleline(&mut self.version).hint_text("留空=当前"));
            if r2.changed() {
                self.dirty = true;
            }
            if ui.button("查询").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                let year: i32 = self.year_text.trim().parse().unwrap_or_else(|_| ctx.period().year());
                let rows = findb::mgmt::budget_analysis_summary(ctx.db(), year, &self.version).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "预算分析",
                    vec!["科目".to_string(), "部门".to_string(), "预算".to_string(), "实际".to_string(), "执行率%".to_string()],
                );
                for r in rows {
                    sh.push(vec![
                        format!("{} {}", r.account_code, r.account_name),
                        if r.dept.is_empty() { "—".to_string() } else { r.dept.clone() },
                        r.budget.fmt_plain(),
                        r.actual.fmt_plain(),
                        r.rate.fmt_qty(),
                    ]);
                }
                let title = format!("预算分析（{} 年度）", year);
                match crate::views::export::run_export(&sh, "预算分析", &title, mode) {
                    Ok(m) => ctx.info(m),
                    Err(e) => ctx.error(e),
                }
            }
        });

        let year: i32 = self.year_text.trim().parse().unwrap_or_else(|_| ctx.period().year());
        let rows = findb::mgmt::budget_analysis(ctx.db(), year, &self.version, None).unwrap_or_default();
        let summary = findb::mgmt::budget_analysis_summary(ctx.db(), year, &self.version).unwrap_or_default();

        ui.separator();
        ui.label(RichText::new("年度汇总（科目 × 部门）").strong());
        let cols = [
            widgets::TCol::new("科目", 200.0),
            widgets::TCol::new("部门", 120.0),
            widgets::TCol::new("预算", 130.0).right(),
            widgets::TCol::new("实际", 130.0).right(),
            widgets::TCol::new("执行率%", 100.0).right(),
        ];
        widgets::grid(ui, "budget_ana_sum", &cols, summary.len(), 24.0, |i, c, ui| {
            let r = &summary[i];
            match c {
                0 => { ui.label(format!("{} {}", r.account_code, r.account_name)); }
                1 => { ui.label(if r.dept.is_empty() { "—".to_string() } else { r.dept.clone() }); }
                2 => widgets::amount_label(ui, r.budget),
                3 => widgets::amount_label(ui, r.actual),
                4 => {
                    let over = r.rate.to_f64() >= 100.0;
                    ui.label(RichText::new(r.rate.fmt_qty()).color(if over { palette::CREDIT } else { palette::OK }));
                }
                _ => {}
            }
        });

        if !rows.is_empty() {
            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new("逐月明细").strong());
            let cols2 = [
                widgets::TCol::new("期间", 90.0).fixed(),
                widgets::TCol::new("科目", 200.0),
                widgets::TCol::new("部门", 120.0),
                widgets::TCol::new("预算", 130.0).right(),
                widgets::TCol::new("实际", 130.0).right(),
                widgets::TCol::new("执行率%", 100.0).right(),
            ];
            widgets::grid(ui, "budget_ana_detail", &cols2, rows.len(), 24.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => { ui.label(r.period.label()); }
                    1 => { ui.label(format!("{} {}", r.account_code, r.account_name)); }
                    2 => { ui.label(if r.dept.is_empty() { "—".to_string() } else { r.dept.clone() }); }
                    3 => widgets::amount_label(ui, r.budget),
                    4 => widgets::amount_label(ui, r.actual),
                    5 => { ui.label(RichText::new(r.rate.fmt_qty()).color(if r.rate.to_f64() >= 100.0 { palette::CREDIT } else { palette::OK })); }
                    _ => {}
                }
            });
        }
    }
}

// ===========================================================================
// 成本核算
// ===========================================================================

pub struct CostView {
    pub tab: u8, // 0=计价配置 1=期末结价 2=制造成本
    pub period_text: String,
    pub configs: Vec<findb::business::CostConfigRow>,
    pub close_rows: Vec<findb::business::PeriodEndCostRow>,
    pub dirty: bool,
    pub key: String,
    // 制造成本
    pub oh_amount: String,
    pub oh_base: String,
}

impl Default for CostView {
    fn default() -> Self {
        Self {
            tab: 0,
            period_text: String::new(),
            configs: Vec::new(),
            close_rows: Vec::new(),
            dirty: true,
            key: String::new(),
            oh_amount: String::new(),
            oh_base: "cost".to_string(),
        }
    }
}

impl CostView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        widgets::page_header(ui, "成本核算", |ui| {
            ui.label(RichText::new("存货计价方式配置 · 期末结价").weak());
        });
        widgets::toolbar(ui, |ui| {
            for (i, label) in ["计价方式", "期末结价", "制造成本"].iter().enumerate() {
                if ui.selectable_label(self.tab == i as u8, *label).clicked() {
                    self.tab = i as u8;
                    self.dirty = true;
                }
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        match self.tab {
            0 => self.show_configs(ctx, ui),
            1 => self.show_period_end(ctx, ui),
            _ => self.show_manufacturing(ctx, ui),
        }
    }

    fn show_manufacturing(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let p = Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period());
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            ui.separator();
            ui.label("分摊费用");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.oh_amount));
            for (code, label) in [("cost", "按成本占比"), ("labor", "按直接人工"), ("qty", "按计划产量")] {
                if ui.selectable_label(self.oh_base == code, label).clicked() {
                    self.oh_base = code.to_string();
                }
            }
            if ui.button("分摊并落地").clicked() {
                let amt = Money::parse_or_zero(&self.oh_amount);
                let base = findb::manufacturing::OverheadBase::parse(&self.oh_base);
                match findb::manufacturing::overhead_allocate_with(ctx.db(), p, amt, base, true) {
                    Ok(alloc) => {
                        ctx.log("成本", "制造费用分摊", &format!("{} 笔", alloc.len()));
                        ctx.info(format!("已分摊 {} 笔制造费用", alloc.len()));
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });

        // 在制品成本
        let wip = findb::manufacturing::wip_cost(ctx.db(), p).unwrap_or_default();
        widgets::card(ui, "在制品成本", |ui| {
            let cols = [
                widgets::TCol::new("订单号", 140.0).fixed(),
                widgets::TCol::new("产品", 150.0),
                widgets::TCol::new("材料", 90.0).right(),
                widgets::TCol::new("人工", 90.0).right(),
                widgets::TCol::new("制造费", 90.0).right(),
                widgets::TCol::new("合计", 110.0).right(),
            ];
            widgets::grid(ui, "cost_wip", &cols, wip.len(), 22.0, |i, c, ui| {
                let r = &wip[i];
                match c {
                    0 => { ui.label(RichText::new(&r.no).monospace()); }
                    1 => { ui.label(&r.item_name); }
                    2 => { widgets::amount_label(ui, r.material); }
                    3 => { widgets::amount_label(ui, r.labor); }
                    4 => { widgets::amount_label(ui, r.overhead); }
                    5 => { widgets::amount_label(ui, r.total); }
                    _ => {}
                }
            });
        });

        // 成本差异分析
        let var = findb::manufacturing::cost_variance_report(ctx.db(), p).unwrap_or_default();
        widgets::card(ui, "成本差异分析", |ui| {
            let cols = [
                widgets::TCol::new("订单号", 140.0).fixed(),
                widgets::TCol::new("产品", 140.0),
                widgets::TCol::new("实际成本", 100.0).right(),
                widgets::TCol::new("标准成本", 100.0).right(),
                widgets::TCol::new("差异", 100.0).right(),
                widgets::TCol::new("差异%", 70.0).right(),
            ];
            widgets::grid(ui, "cost_var", &cols, var.len(), 22.0, |i, c, ui| {
                let r = &var[i];
                match c {
                    0 => { ui.label(RichText::new(&r.no).monospace()); }
                    1 => { ui.label(&r.item_name); }
                    2 => { widgets::amount_label(ui, r.actual); }
                    3 => { widgets::amount_label(ui, r.standard); }
                    4 => { ui.label(r.variance.fmt_plain()); }
                    5 => { ui.label(format!("{:.1}%", r.variance_pct)); }
                    _ => {}
                }
            });
        });

        // 成本预测
        let fc = findb::manufacturing::cost_forecast_report(ctx.db(), p).unwrap_or_default();
        widgets::card(ui, "成本预测（BOM 参考）", |ui| {
            let cols = [
                widgets::TCol::new("订单号", 140.0).fixed(),
                widgets::TCol::new("产品", 140.0),
                widgets::TCol::new("计划量", 100.0).right(),
                widgets::TCol::new("预测成本", 120.0).right(),
            ];
            widgets::grid(ui, "cost_fc", &cols, fc.len(), 22.0, |i, c, ui| {
                let r = &fc[i];
                match c {
                    0 => { ui.label(RichText::new(&r.no).monospace()); }
                    1 => { ui.label(&r.item_name); }
                    2 => { widgets::amount_label(ui, r.planned_qty); }
                    3 => { widgets::amount_label(ui, r.forecast); }
                    _ => {}
                }
            });
        });
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        let (name, sh) = match self.tab {
            0 => {
                let rows = findb::business::cost_configs(ctx.db()).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "计价方式配置",
                    vec!["存货".to_string(), "计价方式".to_string(), "标准成本".to_string()],
                );
                for r in rows {
                    sh.push(vec![r.item.clone(), r.method_label.clone(), r.standard_cost.fmt_plain()]);
                }
                ("计价方式配置", sh)
            }
            _ => {
                let p = Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period());
                let rows = findb::business::period_end_cost(ctx.db(), p, false).unwrap_or_default();
                let mut sh = crate::views::export::Sheet::new(
                    "期末结价",
                    vec!["存货".to_string(), "计价方式".to_string(), "结存数量".to_string(), "结存金额".to_string(), "单价".to_string(), "调整额".to_string()],
                );
                for r in rows {
                    sh.push(vec![r.item.clone(), r.method.clone(), r.end_qty.fmt_qty(), r.end_amount.fmt_plain(), r.unit_cost.fmt_plain(), r.adjust.fmt_plain()]);
                }
                ("期末结价", sh)
            }
        };
        let title = format!("{name}（{}）", ctx.period().label());
        match crate::views::export::run_export(&sh, name, &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }

    fn show_configs(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.dirty {
            self.dirty = false;
            self.configs = findb::business::cost_configs(ctx.db()).unwrap_or_default();
        }
        widgets::toolbar(ui, |ui| {
            if ui.button("新增配置").clicked() {
                let item = "ITEM".to_string();
                let method = "moving_average".to_string();
                match findb::business::item_cost_method_set(ctx.db(), &item, Some(&method), Money::ZERO) {
                    Ok(()) => {
                        ctx.log("成本", "设置计价方式", &format!("{item} → {method}"));
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });
        let rows = self.configs.clone();
        let cols = [
            widgets::TCol::new("存货", 140.0).fixed(),
            widgets::TCol::new("计价方式", 160.0),
            widgets::TCol::new("标准成本", 130.0).right(),
            widgets::TCol::new("操作", 100.0).fixed(),
        ];
        let mut clear: Option<String> = None;
        widgets::grid(ui, "cost_configs", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.item).monospace()); }
                1 => { ui.label(&r.method_label); }
                2 => widgets::amount_label(ui, r.standard_cost),
                3 => {
                    if ui.small_button("清除").clicked() {
                        clear = Some(r.item.clone());
                    }
                }
                _ => {}
            }
        });
        if let Some(item) = clear {
            match findb::business::item_cost_method_clear(ctx.db(), &item) {
                Ok(()) => {
                    ctx.log("成本", "清除计价配置", &item);
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    fn show_period_end(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if r.changed() {
                self.dirty = true;
            }
            if ui.button("试算").clicked() {
                self.reload_close(ctx);
            }
            if ui.button("结价（写入调整）").clicked() {
                let p = Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period());
                match findb::business::period_end_cost(ctx.db(), p, true) {
                    Ok(rows) => {
                        self.close_rows = rows;
                        ctx.log("成本", "期末结价", &format!("{}", p.label()));
                        ctx.info("已写入成本调整");
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
        });

        if self.dirty {
            self.reload_close(ctx);
        }
        let rows = self.close_rows.clone();
        let cols = [
            widgets::TCol::new("存货", 140.0).fixed(),
            widgets::TCol::new("计价方式", 150.0),
            widgets::TCol::new("结存数量", 110.0).right(),
            widgets::TCol::new("结存金额", 130.0).right(),
            widgets::TCol::new("单价", 120.0).right(),
            widgets::TCol::new("调整额", 130.0).right(),
        ];
        widgets::grid(ui, "period_end", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.item).monospace()); }
                1 => { ui.label(&r.method); }
                2 => { ui.label(r.end_qty.fmt_qty()); }
                3 => widgets::amount_label(ui, r.end_amount),
                4 => widgets::amount_label(ui, r.unit_cost),
                5 => {
                    let neg = r.adjust.is_negative();
                    ui.label(
                        RichText::new(r.adjust.fmt_money())
                            .color(if neg { palette::CREDIT } else { palette::OK }),
                    );
                }
                _ => {}
            }
        });
        if !rows.is_empty() {
            let sum: Money = rows.iter().map(|r| r.adjust).sum();
            ui.separator();
            ui.label(RichText::new(format!("调整合计：{}", sum.fmt_money())).strong());
        }
    }

    fn reload_close(&mut self, ctx: &mut AppCtx<'_>) {
        let p = Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period());
        self.close_rows = findb::business::period_end_cost(ctx.db(), p, false).unwrap_or_default();
        self.dirty = false;
    }
}
