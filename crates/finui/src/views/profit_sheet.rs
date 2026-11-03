//! 利润分析表：照抄厂里那张表（序号 / 科目 / 行次 / 金额 / 备注）。
//!
//! 厂里那张表的名目在标准科目表里对不上（「化工」不是一个科目），所以**行是数据**：
//! 每行挂若干科目编码，金额 = 这些科目本期发生额之和。改口径就改这一行，不用改代码。
//!
//! 没挂科目的行给一个按钮「按这张表生成科目」，在 `5101 制造费用` 下各建一个同名末级科目，
//! 以后录凭证按它们记，这张表就自动出数——先有表、后有科目，小厂本来就是这个顺序。

use egui::{RichText, Ui};
use fincore::{Money, Period};
use findb::profit_sheet::{self, AutoMap, Row, Sheet};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

pub struct ProfitSheetView {
    pub period_text: String,
    pub sheet: Option<Sheet>,
    /// 行口径（可编辑）
    pub rows: Vec<Row>,
    /// 正在编辑的行下标
    pub sel: Option<usize>,
    pub ed_seq: String,
    pub ed_label: String,
    pub ed_hint: String,
    pub ed_accounts: String,
    pub ed_revenue: bool,
    pub err: String,
    pub dirty: bool,
    /// 自动挂科目的预览（Some 时开着窗口）
    pub auto: Option<AutoMap>,
    key: String,
}

impl Default for ProfitSheetView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            sheet: None,
            rows: Vec::new(),
            sel: None,
            ed_seq: String::new(),
            ed_label: String::new(),
            ed_hint: String::new(),
            ed_accounts: String::new(),
            ed_revenue: false,
            err: String::new(),
            dirty: true,
            auto: None,
            key: String::new(),
        }
    }
}

impl ProfitSheetView {
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
        let key = format!("{}", p.ymm());
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        match profit_sheet::report(ctx.db(), p) {
            Ok(s) => self.sheet = Some(s),
            Err(e) => {
                ctx.error(e.to_string());
                self.sheet = None;
            }
        }
        match profit_sheet::list(ctx.db()) {
            Ok(rows) => self.rows = rows,
            Err(e) => {
                ctx.error(e.to_string());
                self.rows.clear();
            }
        }
    }

    fn pick(&mut self, i: usize) {
        self.sel = Some(i);
        let r = &self.rows[i];
        self.ed_seq = r.seq.to_string();
        self.ed_label = r.label.clone();
        self.ed_hint = r.hint.clone();
        self.ed_accounts = r.accounts.clone();
        self.ed_revenue = r.is_revenue();
    }

    fn save_row(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(i) = self.sel else { return };
        let Some(old) = self.rows.get(i).cloned() else {
            return;
        };
        let mut row = old;
        row.seq = self.ed_seq.trim().parse::<i64>().unwrap_or(row.seq);
        row.label = self.ed_label.trim().to_string();
        row.hint = self.ed_hint.trim().to_string();
        row.accounts = self.ed_accounts.trim().to_string();
        row.kind = if self.ed_revenue {
            profit_sheet::KIND_REVENUE.to_string()
        } else {
            profit_sheet::KIND_EXPENSE.to_string()
        };
        match profit_sheet::save(ctx.db(), &mut row) {
            Ok(_) => {
                self.err.clear();
                ctx.info(format!("第 {} 行已保存", row.seq));
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn create_accounts(&mut self, ctx: &mut AppCtx<'_>) {
        match profit_sheet::create_accounts(ctx.db()) {
            Ok((created, touched)) => {
                self.err.clear();
                // 先认现有科目、认不出才新建：两个数都要说清，
                // 不然用户以为又被造了一堆重复科目
                ctx.info(format!(
                    "挂上 {touched} 行（其中新建 {created} 个科目，其余用的是账里已有的）"
                ));
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    /// 按现有科目名先算一遍（不写库）
    fn preview_auto(&mut self, ctx: &mut AppCtx<'_>) {
        match profit_sheet::auto_map(ctx.db(), false) {
            Ok(m) => {
                self.err.clear();
                self.auto = Some(m);
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn apply_auto(&mut self, ctx: &mut AppCtx<'_>) {
        match profit_sheet::auto_map(ctx.db(), true) {
            Ok(m) => {
                self.err.clear();
                ctx.info(format!(
                    "挂好 {} 行，还有 {} 行没找到科目",
                    m.touched,
                    m.unmatched.len()
                ));
                self.auto = None;
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    /// 预览窗口：一行一行摆出来，人看过再点应用。
    ///
    /// 猜错了不会报错，只会让某一行金额偏大、另一行偏小，合计还是对的——所以不能直接落地。
    fn show_auto_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(m) = self.auto.clone() else { return };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("自动挂科目（先看一眼）")
            .open(&mut open)
            .default_width(720.0)
            .show(ui.ctx(), |ui| {
                ui.label(
                    RichText::new(
                        "按你现有科目的名字去对这张表的行。同一科目只挂一行；标准科目表里没有的名字（化工、染料封闭剂…）会留空，等你自己建。",
                    )
                    .weak(),
                );
                ui.add_space(4.0);
                let cols = [
                    widgets::TCol::new("序号", 50.0).right(),
                    widgets::TCol::new("科目", 140.0),
                    widgets::TCol::new("将挂的科目", 260.0),
                    widgets::TCol::new("说明", 240.0),
                ];
                let rows = m.rows.clone();
                widgets::grid(ui, "profit_sheet_auto", &cols, rows.len(), 22.0, |i, c, ui| {
                    let Some(r) = rows.get(i) else { return };
                    match c {
                        0 => {
                            ui.label(r.seq.to_string());
                        }
                        1 => {
                            ui.label(&r.label);
                        }
                        2 => {
                            if r.added.is_empty() {
                                if r.kept.is_empty() {
                                    ui.label(RichText::new("—").weak());
                                } else {
                                    ui.label(RichText::new(format!("已有 {}", r.kept)).weak());
                                }
                            } else {
                                let t = r
                                    .added
                                    .iter()
                                    .map(|(c, n)| format!("{c} {n}"))
                                    .collect::<Vec<_>>()
                                    .join("、");
                                ui.label(RichText::new(t).monospace());
                            }
                        }
                        _ => {
                            ui.label(RichText::new(&r.note).weak());
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let n = m.rows.iter().filter(|r| !r.added.is_empty()).count();
                    if ui
                        .add_enabled(n > 0, egui::Button::new(format!("应用这 {n} 行")))
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                });
            });
        if apply {
            self.apply_auto(ctx);
        } else if cancel || !open {
            self.auto = None;
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "利润分析表", |ui| {
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
            if ui
                .button("按这张表生成科目")
                .on_hover_text("给还没挂科目的行，在 5101 制造费用下各建一个同名末级科目")
                .clicked()
            {
                self.create_accounts(ctx);
            }
            if ui
                .button("自动挂科目")
                .on_hover_text("按你现有科目的名字去对这张表的行：先给你看，再决定要不要落地")
                .clicked()
            {
                self.preview_auto(ctx);
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                let mut sh = crate::views::export::Sheet::new(
                    "利润分析表",
                    vec![
                        "序号".to_string(),
                        "科目".to_string(),
                        "行次".to_string(),
                        "金额（元）".to_string(),
                        "备注".to_string(),
                    ],
                );
                if let Some(s) = &self.sheet {
                    let mut rev = 0;
                    let mut exp = 0;
                    for l in &s.rows {
                        let is_rev = l.is_revenue;
                        let no = if is_rev {
                            rev += 1;
                            rev
                        } else {
                            exp += 1;
                            exp
                        };
                        sh.push(vec![
                            l.seq.to_string(),
                            l.label.clone(),
                            no.to_string(),
                            l.amount.fmt_plain(),
                            l.hint.clone(),
                        ]);
                    }
                    sh.push(vec![
                        String::new(),
                        "收入合计".to_string(),
                        String::new(),
                        s.revenue.fmt_plain(),
                        String::new(),
                    ]);
                    sh.push(vec![
                        String::new(),
                        "支出合计".to_string(),
                        String::new(),
                        s.expense.fmt_plain(),
                        String::new(),
                    ]);
                    sh.push(vec![
                        String::new(),
                        "净利润".to_string(),
                        String::new(),
                        s.profit.fmt_plain(),
                        String::new(),
                    ]);
                }
                let title = format!("利润分析表（{}）", p.label());
                match crate::views::export::run_export(
                    &sh,
                    "利润分析表",
                    &title,
                    mode,
                    &ctx.db().options(),
                ) {
                    Ok(m) => ctx.info(m),
                    Err(e) => ctx.error(e),
                }
            }
        });

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, &self.err);
        }
        if let Some(s) = &self.sheet {
            if s.unmapped > 0 {
                ui.colored_label(
                    palette::WARN,
                    format!(
                        "有 {} 行还没挂科目：你的账里这些费用记在哪个科目？点「按这张表生成科目」自动挂，或在下面手工填。",
                        s.unmapped
                    ),
                );
            }
            // 表外体检：账上还有费用没落到任何一行上。不提示的话，
            // 支出合计少算了钱，表却看起来完全正常——这种错只有对账时才发现。
            if !s.uncovered.is_zero() {
                let names: Vec<String> = s
                    .uncovered_rows
                    .iter()
                    .take(6)
                    .map(|u| format!("{} {}", u.code, u.name))
                    .collect();
                let more = if s.uncovered_rows.len() > 6 {
                    format!("等 {} 笔", s.uncovered_rows.len())
                } else {
                    String::new()
                };
                ui.colored_label(
                    palette::WARN,
                    format!(
                        "还有 {} 元费用没落到表上任何一行（{}{}）：要看得见就加一行，挂上这些科目。",
                        s.uncovered.fmt_money(),
                        names.join("、"),
                        more
                    ),
                );
            }
        }
        ui.add_space(6.0);

        // 主表：序号 / 科目 / 行次 / 金额 / 备注（照抄厂里那张表）
        let lines: Vec<_> = self
            .sheet
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default();
        let rev = self
            .sheet
            .as_ref()
            .map(|s| s.revenue)
            .unwrap_or(Money::ZERO);
        let exp = self
            .sheet
            .as_ref()
            .map(|s| s.expense)
            .unwrap_or(Money::ZERO);
        let profit = self.sheet.as_ref().map(|s| s.profit).unwrap_or(Money::ZERO);

        let cols = [
            widgets::TCol::new("序号", 60.0).right(),
            widgets::TCol::new("科目", 220.0),
            widgets::TCol::new("行次", 60.0).right(),
            widgets::TCol::new("金额（元）", 150.0).right(),
            widgets::TCol::new("备注", 320.0),
        ];
        // 行次：收入行从 1 起、支出行也从 1 起（厂里那张表就是这么编的）
        let mut rev_no = 0i64;
        let mut exp_no = 0i64;
        let mut nos: Vec<i64> = Vec::with_capacity(lines.len());
        for l in &lines {
            if l.is_revenue {
                rev_no += 1;
                nos.push(rev_no);
            } else {
                exp_no += 1;
                nos.push(exp_no);
            }
        }
        widgets::grid(
            ui,
            "profit_sheet",
            &cols,
            lines.len() + 3,
            24.0,
            |i, c, ui| {
                if i >= lines.len() {
                    let (label, amount, strong) = match i - lines.len() {
                        0 => ("收入合计", rev, false),
                        1 => ("支出合计", exp, false),
                        _ => ("净利润（亏损以「-」号填写）", profit, true),
                    };
                    match c {
                        0 | 2 => {}
                        1 => {
                            ui.label(RichText::new(label).strong());
                        }
                        3 => {
                            let t = amount.fmt_money();
                            ui.label(if strong {
                                RichText::new(t).strong()
                            } else {
                                RichText::new(t)
                            });
                        }
                        _ => {}
                    }
                    return;
                }
                let l = &lines[i];
                match c {
                    0 => {
                        ui.label(l.seq.to_string());
                    }
                    1 => {
                        if l.unmapped {
                            ui.colored_label(palette::WARN, format!("{}（未挂科目）", l.label));
                        } else {
                            ui.label(&l.label);
                        }
                    }
                    2 => {
                        ui.label(nos[i].to_string());
                    }
                    3 => {
                        widgets::amount_label(ui, l.amount);
                    }
                    4 => {
                        let mut hint = l.hint.clone();
                        if let Some(q) = l.qty {
                            if !hint.is_empty() {
                                hint.push('　');
                            }
                            hint.push_str(&format!("产量 {}", q.fmt_qty()));
                        }
                        ui.label(RichText::new(hint).weak());
                    }
                    _ => {}
                }
            },
        );

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("行口径（每行挂哪些科目）").strong());
        ui.label(
            RichText::new(
                "同一科目只能挂一行——挂两行合计会翻倍，而报表看起来完全正常，只有对账时才发现。",
            )
            .weak(),
        );
        ui.add_space(4.0);

        let rows = self.rows.clone();
        let rcols = [
            widgets::TCol::new("序号", 60.0).right(),
            widgets::TCol::new("科目", 200.0),
            widgets::TCol::new("类型", 60.0),
            widgets::TCol::new("挂的科目", 200.0),
            widgets::TCol::new("备注", 300.0),
            widgets::TCol::new("", 60.0),
        ];
        let mut pick: Option<usize> = None;
        widgets::grid(
            ui,
            "profit_sheet_rows",
            &rcols,
            rows.len(),
            24.0,
            |i, c, ui| {
                let Some(r) = rows.get(i) else { return };
                match c {
                    0 => {
                        ui.label(r.seq.to_string());
                    }
                    1 => {
                        ui.label(&r.label);
                    }
                    2 => {
                        ui.label(if r.is_revenue() { "收入" } else { "支出" });
                    }
                    3 => {
                        if r.codes().is_empty() {
                            ui.colored_label(palette::WARN, "—");
                        } else {
                            ui.label(RichText::new(&r.accounts).monospace());
                        }
                    }
                    4 => {
                        ui.label(RichText::new(&r.hint).weak());
                    }
                    _ => {
                        if ui.button("编辑").clicked() {
                            pick = Some(i);
                        }
                    }
                }
            },
        );
        if let Some(i) = pick {
            self.pick(i);
        }

        self.show_auto_window(ctx, ui);

        if self.sel.is_some() {
            ui.add_space(8.0);
            ui.label(RichText::new("编辑选中的行").strong());
            widgets::toolbar(ui, |ui| {
                ui.label("序号");
                ui.add_sized([60.0, 22.0], egui::TextEdit::singleline(&mut self.ed_seq));
                ui.label("科目");
                ui.add_sized(
                    [160.0, 22.0],
                    egui::TextEdit::singleline(&mut self.ed_label),
                );
                ui.label("类型");
                ui.selectable_value(&mut self.ed_revenue, true, "收入");
                ui.selectable_value(&mut self.ed_revenue, false, "支出");
            });
            widgets::toolbar(ui, |ui| {
                ui.label("挂的科目");
                ui.add_sized(
                    [220.0, 22.0],
                    egui::TextEdit::singleline(&mut self.ed_accounts).hint_text("510106,660209"),
                );
                ui.label("备注");
                ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(&mut self.ed_hint));
                if ui.button("保存").clicked() {
                    self.save_row(ctx);
                }
                if ui.button("取消").clicked() {
                    self.sel = None;
                }
                if let Some(i) = self.sel {
                    if let Some(r) = self.rows.get(i) {
                        if ui.button("删除这一行").clicked() {
                            let msg =
                                format!("删掉第 {} 行「{}」？出表时就不再显示。", r.seq, r.label);
                            ctx.confirm(
                                "删除利润分析表的行",
                                &msg,
                                crate::state::ConfirmAction::DeleteProfitSheetRow(r.id),
                            );
                        }
                    }
                }
            });
        }
    }
}
