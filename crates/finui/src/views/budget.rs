//! 预算管理：预算编制 与 预算执行分析
//!
//! 预算是"没有标准答案"的典型：可以按部门编、按科目编、按季度编，
//! 所以这里只负责存（budget 表按 期间+科目+部门 唯一）和比，
//! 编什么、怎么比交给用户在界面上定，不替他做假设。

use egui::{Align2, RichText, Ui};
use findb::advanced::{self, BudgetVersion};
use findb::mgmt::{self, Budget, BudgetRow};
use fincore::account::AuxKind;
use fincore::{Money, Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

/// 预算管理的两个页签
#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)]
pub enum BudgetTab {
    /// 预算编制
    Edit,
    /// 预算执行分析
    Analysis,
}

pub struct BudgetView {
    pub tab: BudgetTab,
    /// 预算所属期间（`YYYY-MM`，Period::parse 容忍其它写法）
    pub period_text: String,
    /// 分析口径：false = 只看本期发生，true = 从 1 期累计到当前期
    pub yearly: bool,
    pub budgets: Vec<Budget>,
    /// 与 `budgets` 等长的科目名称缓存（渲染时不必逐行查科目表）
    pub names: Vec<String>,
    /// 与 `budgets` 等长的编辑缓冲，索引即行号
    pub amount_buf: Vec<String>,
    pub memo_buf: Vec<String>,
    pub sel: Option<usize>,
    /// 部门下拉选项（来自部门档案，空串表示"不指定部门"）
    pub dept_options: Vec<String>,
    pub new_account: String,
    pub new_dept: String,
    pub new_amount: String,
    pub new_memo: String,
    /// 「从实际数生成」弹窗
    pub gen_open: bool,
    pub gen_from: String,
    pub gen_to: String,
    pub gen_ratio: String,
    /// 预算版本（'' = 默认版本）
    pub version: String,
    /// 版本清单（切换/新建/复制用）
    pub versions: Vec<BudgetVersion>,
    /// 新建版本弹窗
    pub ver_open: bool,
    pub ver_key: String,
    pub ver_name: String,
    pub ver_memo: String,
    pub ver_copy_from: String,
    pub rows: Vec<BudgetRow>,
    pub sum_budget: Money,
    pub sum_actual: Money,
    pub sum_diff: Money,
    pub sum_rate: Money,
    pub dirty: bool,
    key: String,
}

impl Default for BudgetView {
    fn default() -> Self {
        Self {
            tab: BudgetTab::Edit,
            period_text: String::new(),
            yearly: false,
            budgets: Vec::new(),
            names: Vec::new(),
            amount_buf: Vec::new(),
            memo_buf: Vec::new(),
            sel: None,
            dept_options: Vec::new(),
            new_account: String::new(),
            new_dept: String::new(),
            new_amount: String::new(),
            new_memo: String::new(),
            gen_open: false,
            gen_from: String::new(),
            gen_to: String::new(),
            gen_ratio: "1".to_string(),
            version: String::new(),
            versions: Vec::new(),
            ver_open: false,
            ver_key: String::new(),
            ver_name: String::new(),
            ver_memo: String::new(),
            ver_copy_from: String::new(),
            rows: Vec::new(),
            sum_budget: Money::ZERO,
            sum_actual: Money::ZERO,
            sum_diff: Money::ZERO,
            sum_rate: Money::ZERO,
            dirty: true,
            key: String::new(),
        }
    }
}

impl BudgetView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        let p = ctx.period();
        if self.period_text.is_empty() {
            self.period_text = p.code();
        }
        // 默认以上年同期（这里退化为上一期）实际数为基数生成下期预算
        if self.gen_from.is_empty() {
            self.gen_from = p.prev().code();
        }
        if self.gen_to.is_empty() {
            self.gen_to = p.code();
        }
        self.dirty = true;
    }

    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("{}|{:?}|{}|{}", p.ymm(), self.tab, self.yearly, self.version);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        self.sel = None;

        self.dept_options = findb::auxs::codes(ctx.db(), AuxKind::Dept).unwrap_or_default();
        self.versions = advanced::bversion_list(ctx.db()).unwrap_or_default();

        match self.tab {
            BudgetTab::Edit => {
                self.rows.clear();
                match mgmt::budget_list_version(ctx.db(), Some(p), &self.version) {
                    Ok(v) => {
                        let chart = ctx.chart();
                        self.names = v
                            .iter()
                            .map(|b| {
                                chart
                                    .get(&b.account_code)
                                    .map(|a| a.name.clone())
                                    .unwrap_or_else(|| "⟨未知科目⟩".to_string())
                            })
                            .collect();
                        // 编辑缓冲按库里的值重建：保存后回读能立刻看到规范化后的金额
                        self.amount_buf = v.iter().map(|b| b.amount.fmt_plain()).collect();
                        self.memo_buf = v.iter().map(|b| b.memo.clone()).collect();
                        self.budgets = v;
                    }
                    Err(e) => {
                        ctx.error(e.to_string());
                        self.budgets.clear();
                        self.names.clear();
                        self.amount_buf.clear();
                        self.memo_buf.clear();
                    }
                }
            }
            BudgetTab::Analysis => {
                self.budgets.clear();
                self.names.clear();
                self.amount_buf.clear();
                self.memo_buf.clear();
                // 按年累计 = 从本年 1 期累计到当前期；按月 = 只取本期
                let from = if self.yearly {
                    Period::new(p.year(), 1).unwrap_or(p)
                } else {
                    p
                };
                match mgmt::budget_vs_actual(ctx.db(), p, from) {
                    Ok(v) => self.rows = v,
                    Err(e) => {
                        ctx.error(e.to_string());
                        self.rows.clear();
                    }
                }
                self.sum_budget = self.rows.iter().map(|r| r.budget).sum();
                self.sum_actual = self.rows.iter().map(|r| r.actual).sum();
                self.sum_diff = self.rows.iter().map(|r| r.diff).sum();
                // 总体执行率按绝对额口径算，避免正负抵消后得出无意义的比率
                self.sum_rate = if self.sum_budget.is_zero() {
                    Money::ZERO
                } else {
                    (self.sum_actual.abs() * Money::from_i64(100))
                        .checked_div(self.sum_budget.abs())
                        .expect("sum_budget 已判非零")
                        .round2()
                };
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "预算管理", |ui| {
            ui.label(RichText::new(format!("预算期间 {}", p.label())).weak());
        });

        let years = self.year_options(ctx);
        let was_yearly = self.yearly;
        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, BudgetTab::Edit, "预算编制");
            ui.selectable_value(&mut self.tab, BudgetTab::Analysis, "预算执行分析");
            ui.separator();
            ui.label("年份");
            // 年份下拉要连带把期间改到同年，widgets::combo 只认字符串列表，故直接用 ComboBox
            let mut year = p.year();
            egui::ComboBox::from_id_salt("bg_year_sel")
                .selected_text(format!("{} 年", p.year()))
                .width(96.0)
                .show_ui(ui, |ui| {
                    for y in &years {
                        ui.selectable_value(&mut year, *y, format!("{y} 年"));
                    }
                });
            if year != p.year() {
                self.period_text = Period::new(year, p.month()).unwrap_or(p).code();
                self.dirty = true;
            }
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
            if self.tab == BudgetTab::Analysis {
                ui.separator();
                ui.selectable_value(&mut self.yearly, false, "按月");
                ui.selectable_value(&mut self.yearly, true, "按年累计");
            }
            if self.tab == BudgetTab::Edit {
                ui.separator();
                ui.label("版本");
                let mut ver = self.version.clone();
                let ver_text = if ver.is_empty() {
                    "默认".to_string()
                } else {
                    self.versions
                        .iter()
                        .find(|v| v.key == ver)
                        .map(|v| v.name.clone())
                        .unwrap_or_else(|| ver.clone())
                };
                egui::ComboBox::from_id_salt("bg_ver_sel")
                    .selected_text(ver_text)
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut ver, String::new(), "默认");
                        for v in &self.versions {
                            ui.selectable_value(&mut ver, v.key.clone(), v.name.clone());
                        }
                    });
                if ver != self.version {
                    self.version = ver;
                    self.dirty = true;
                }
                if ui.button("版本管理").clicked() {
                    self.ver_open = true;
                }
            }
            ui.separator();
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });
        if self.yearly != was_yearly {
            self.dirty = true;
        }

        match self.tab {
            BudgetTab::Edit => self.show_edit(ctx, ui, p),
            BudgetTab::Analysis => self.show_analysis(ctx, ui),
        }
    }

    fn year_options(&self, ctx: &AppCtx<'_>) -> Vec<i32> {
        let start = ctx.db().options().start_period.year();
        let end = Period::default().year() + 1;
        if end < start {
            vec![start]
        } else {
            (start..=end).collect()
        }
    }

    // ------------------------- 预算编制 -------------------------
    fn show_edit(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        ui.label(
            RichText::new(
                "预算按「期间 + 科目 + 部门」唯一存储；金额与备注可直接改，光标离开单元格即保存。",
            )
            .weak(),
        );
        ui.add_space(4.0);

        let mut want_add = false;
        let mut want_gen = false;
        let mut want_del = false;
        let depts = self.dept_options.clone();
        widgets::toolbar(ui, |ui| {
            ui.label("科目");
            widgets::account_combo(ui, "bg_new_acct", &mut self.new_account, ctx.chart(), true, 200.0);
            ui.label("部门");
            let mut dept = self.new_dept.clone();
            egui::ComboBox::from_id_salt("bg_new_dept")
                .selected_text(if dept.is_empty() {
                    "（不指定）".to_string()
                } else {
                    dept.clone()
                })
                .width(120.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut dept, String::new(), "（不指定）");
                    for d in &depts {
                        ui.selectable_value(&mut dept, d.clone(), d);
                    }
                })
                .response
                .on_hover_text("留空表示全公司口径");
            self.new_dept = dept;
            ui.label("金额");
            widgets::money_input(ui, &mut self.new_amount, 110.0);
            ui.label("备注");
            widgets::text_input(ui, &mut self.new_memo, 120.0, "选填");
            want_add = ui.button("新增一行").clicked();
            ui.separator();
            want_del = ui.button("删除选中").clicked();
            want_gen = ui.button("从实际数生成").clicked();
        });

        if want_add {
            self.add_row(ctx, p);
        }
        if want_del {
            self.delete_selected(ctx);
        }
        if want_gen {
            self.gen_open = true;
        }

        let cols = [
            widgets::TCol::new("科目编码", 110.0).fixed(),
            widgets::TCol::new("科目名称", 200.0),
            widgets::TCol::new("部门", 100.0).fixed(),
            widgets::TCol::new("预算金额", 140.0).right(),
            widgets::TCol::new("备注", 220.0),
        ];
        if self.budgets.is_empty() {
            widgets::empty_hint(ui, "本期还没有预算，可先「新增一行」或「从实际数生成」");
        } else {
            // 编辑缓冲先搬出来，避免在 grid 的闭包里同时借用 self
            let mut amount_buf = std::mem::take(&mut self.amount_buf);
            let mut memo_buf = std::mem::take(&mut self.memo_buf);
            let mut sel = self.sel;
            let mut save: Vec<usize> = Vec::new();
            let codes: Vec<String> = self.budgets.iter().map(|b| b.account_code.clone()).collect();
            let names = self.names.clone();
            let depts: Vec<String> = self.budgets.iter().map(|b| b.dept.clone()).collect();
            widgets::grid(ui, "budget_rows", &cols, self.budgets.len(), 26.0, |i, c, ui| {
                match c {
                    0 => {
                        if ui
                            .selectable_label(sel == Some(i), RichText::new(&codes[i]).monospace())
                            .clicked()
                        {
                            sel = if sel == Some(i) { None } else { Some(i) };
                        }
                    }
                    1 => {
                        let n = &names[i];
                        ui.label(
                            RichText::new(n).color(if n.starts_with('⟨') {
                                palette::CREDIT
                            } else {
                                ui.visuals().text_color()
                            }),
                        );
                    }
                    2 => {
                        let d = &depts[i];
                        if d.is_empty() {
                            ui.label(RichText::new("—").weak());
                        } else {
                            ui.label(d);
                        }
                    }
                    3 => {
                        let r =
                            widgets::money_input(ui, &mut amount_buf[i], ui.available_width());
                        if r.lost_focus() {
                            save.push(i);
                        }
                    }
                    4 => {
                        let r = widgets::text_input(
                            ui,
                            &mut memo_buf[i],
                            ui.available_width(),
                            "备注",
                        );
                        if r.lost_focus() {
                            save.push(i);
                        }
                    }
                    _ => {}
                }
            });
            self.amount_buf = amount_buf;
            self.memo_buf = memo_buf;
            self.sel = sel;
            for i in save {
                self.save_row(ctx, i);
            }
        }

        ui.add_space(8.0);
        let mut want_clear = false;
        ui.horizontal(|ui| {
            widgets::kv(
                ui,
                "预算合计",
                &self
                    .budgets
                    .iter()
                    .fold(Money::ZERO, |a, b| a + b.amount)
                    .fmt_money(),
            );
            ui.add_space(10.0);
            widgets::kv(ui, "行数", &self.budgets.len().to_string());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("清空全部预算").clicked() {
                    want_clear = true;
                }
            });
        });
        if want_clear {
            ctx.confirm_dangerous(
                "清空全部预算",
                &format!("将删除 {} 的全部预算数据，且不可撤销。确定继续吗？", p.label()),
                ConfirmAction::ClearBudget,
                true,
            );
        }

        self.show_gen_window(ctx, ui);
        self.show_version_window(ctx, ui);
    }

    fn show_version_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if !self.ver_open {
            return;
        }
        let mut open = true;
        let mut close = false;
        let mut want_new = false;
        let mut want_copy = false;
        let mut want_activate: Option<String> = None;
        let mut want_delete: Option<String> = None;
        egui::Window::new("预算版本管理")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(RichText::new("每个版本独立存一套预算；「当前版本」用于执行分析，同时只能有一个生效。").weak());
                ui.add_space(6.0);
                let versions = self.versions.clone();
                for v in &versions {
                    ui.horizontal(|ui| {
                        let cur = advanced::bversion_current(ctx.db()).unwrap_or_default() == v.key;
                        if cur {
                            ui.label(RichText::new("●").color(palette::OK));
                        } else {
                            ui.label(RichText::new("○").weak());
                        }
                        ui.label(RichText::new(&v.name).strong());
                        ui.label(RichText::new(&v.key).monospace().weak());
                        ui.label(RichText::new(&v.created_at).weak());
                        if !cur && ui.button("设为当前").clicked() {
                            want_activate = Some(v.key.clone());
                        }
                        if ui.button("删除").clicked() {
                            want_delete = Some(v.key.clone());
                        }
                    });
                }
                if versions.is_empty() {
                    ui.label(RichText::new("暂无自定义版本，预算存于「默认」版本。").weak());
                }
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("版本编码");
                    widgets::text_input(ui, &mut self.ver_key, 100.0, "v2");
                    ui.label("版本名称");
                    widgets::text_input(ui, &mut self.ver_name, 140.0, "2026 调整版");
                    ui.label("备注");
                    widgets::text_input(ui, &mut self.ver_memo, 120.0, "选填");
                });
                ui.horizontal(|ui| {
                    ui.label("复制自");
                    let mut src = self.ver_copy_from.clone();
                    egui::ComboBox::from_id_salt("bg_ver_copy_from")
                        .selected_text(if src.is_empty() { "默认".to_string() } else { src.clone() })
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut src, String::new(), "默认");
                            for v in &self.versions {
                                ui.selectable_value(&mut src, v.key.clone(), v.name.clone());
                            }
                        });
                    self.ver_copy_from = src;
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("关闭").clicked() {
                            close = true;
                        }
                        if ui.button("新建版本(复制数据)").clicked() {
                            want_copy = true;
                        }
                        if ui.button("仅建空版本").clicked() {
                            want_new = true;
                        }
                    });
                });
            });
        if !open || close {
            self.ver_open = false;
        }
        // 操作在窗口外执行，避免在闭包里可变借用 self
        if let Some(key) = want_activate {
            self.activate_version(ctx, &key);
        }
        if let Some(key) = want_delete {
            self.delete_version(ctx, &key);
        }
        if want_new {
            self.new_version(ctx, false);
        }
        if want_copy {
            self.new_version(ctx, true);
        }
    }

    fn activate_version(&mut self, ctx: &mut AppCtx<'_>, key: &str) {
        if let Some(mut v) = self.versions.iter().find(|v| v.key == key).cloned() {
            v.is_current = true;
            if ctx.handle(advanced::bversion_save(ctx.db(), &v)).is_some() {
                ctx.info(format!("已把版本「{}」设为当前", v.name));
                self.dirty = true;
            }
        }
    }

    fn delete_version(&mut self, ctx: &mut AppCtx<'_>, key: &str) {
        let name = self
            .versions
            .iter()
            .find(|v| v.key == key)
            .map(|v| v.name.clone())
            .unwrap_or_else(|| key.to_string());
        if ctx.handle(advanced::bversion_delete(ctx.db(), key)).is_some() {
            ctx.info(format!("已删除版本「{name}」及其预算数据"));
            if self.version == key {
                self.version.clear();
            }
            self.dirty = true;
        }
    }

    fn new_version(&mut self, ctx: &mut AppCtx<'_>, copy_data: bool) {
        let key = self.ver_key.trim().to_string();
        let name = self.ver_name.trim().to_string();
        if key.is_empty() || name.is_empty() {
            ctx.error("版本编码与名称不能为空");
            return;
        }
        if self.versions.iter().any(|v| v.key == key) {
            ctx.error(format!("版本编码「{key}」已存在"));
            return;
        }
        let v = BudgetVersion {
            key: key.clone(),
            name: name.clone(),
            is_current: false,
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            memo: self.ver_memo.trim().to_string(),
        };
        if ctx.handle(advanced::bversion_save(ctx.db(), &v)).is_none() {
            return;
        }
        if copy_data {
            match advanced::bversion_copy(ctx.db(), &self.ver_copy_from, &key) {
                Ok(n) => ctx.info(format!("已建版本「{name}」并复制 {n} 行预算")),
                Err(e) => ctx.error(e.to_string()),
            }
        } else {
            ctx.info(format!("已建空版本「{name}」"));
        }
        self.version = key;
        self.ver_key.clear();
        self.ver_name.clear();
        self.ver_memo.clear();
        self.ver_copy_from.clear();
        self.ver_open = false;
        self.dirty = true;
    }

    fn add_row(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        if !ctx.can(Perm::Report) {
            return;
        }
        if self.new_account.trim().is_empty() {
            ctx.error("请先选择科目");
            return;
        }
        let rec = Budget {
            id: 0,
            period: p,
            account_code: self.new_account.trim().to_string(),
            dept: self.new_dept.trim().to_string(),
            amount: Money::parse_or_zero(&self.new_amount).round2(),
            memo: self.new_memo.trim().to_string(),
            version: self.version.clone(),
        };
        let r = mgmt::budget_upsert_version(ctx.db(), &rec);
        if ctx.handle(r).is_some() {
            ctx.log(
                "预算",
                "新增预算",
                &format!("{} {} {}", p.label(), rec.account_code, rec.amount.fmt_plain()),
            );
            ctx.info("已新增预算行");
            self.new_amount.clear();
            self.new_memo.clear();
            self.dirty = true;
        }
    }

    fn delete_selected(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(i) = self.sel else {
            ctx.error("请先点选要删除的预算行");
            return;
        };
        let Some(b) = self.budgets.get(i) else {
            return;
        };
        let id = b.id;
        let desc = format!("{} {}", b.account_code, b.dept);
        if !ctx.can(Perm::Report) {
            return;
        }
        let r = mgmt::budget_delete(ctx.db(), id);
        if ctx.handle(r).is_some() {
            ctx.log("预算", "删除预算", &desc);
            ctx.info("已删除预算行");
            self.dirty = true;
        }
    }

    /// 单元格失焦即存：以库里的「期间+科目+部门」为键 upsert
    fn save_row(&mut self, ctx: &mut AppCtx<'_>, i: usize) {
        let Some(b) = self.budgets.get(i) else {
            return;
        };
        let rec = Budget {
            id: b.id,
            period: b.period,
            account_code: b.account_code.clone(),
            dept: b.dept.clone(),
            amount: Money::parse_or_zero(self.amount_buf.get(i).map_or("", |s| s.as_str())).round2(),
            memo: self.memo_buf.get(i).cloned().unwrap_or_default(),
            version: b.version.clone(),
        };
        if rec.amount == b.amount && rec.memo == b.memo {
            return;
        }
        let r = mgmt::budget_upsert(ctx.db(), &rec);
        if ctx.handle(r).is_some() {
            ctx.log(
                "预算",
                "修改预算",
                &format!(
                    "{} {} → {}",
                    rec.period.label(),
                    rec.account_code,
                    rec.amount.fmt_plain()
                ),
            );
            self.dirty = true;
        }
    }

    fn show_gen_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if !self.gen_open {
            return;
        }
        let mut open = true;
        let mut go = false;
        let mut cancel = false;
        egui::Window::new("从实际数生成预算")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label("把起始期间各损益科目的实际发生额乘以放大系数，写入结束期间。");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("起始期间");
                    widgets::text_input(ui, &mut self.gen_from, 90.0, "2026-01");
                    ui.label("结束期间");
                    widgets::text_input(ui, &mut self.gen_to, 90.0, "2026-02");
                    ui.label("放大系数");
                    widgets::text_input(ui, &mut self.gen_ratio, 70.0, "1");
                });
                ui.label(
                    RichText::new("例：起始 2026-01、结束 2026-02、系数 1.1 = 按 1 月实际上浮 10% 编 2 月预算")
                        .weak(),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                        if ui.button("确定生成").clicked() {
                            go = true;
                        }
                    });
                });
            });
        if !open || cancel {
            self.gen_open = false;
        }
        if go {
            self.gen_open = false;
            self.do_generate(ctx);
        }
    }

    fn do_generate(&mut self, ctx: &mut AppCtx<'_>) {
        if !ctx.can(Perm::Report) {
            return;
        }
        let from = Period::parse(&self.gen_from).unwrap_or_else(|_| ctx.period());
        let to = Period::parse(&self.gen_to).unwrap_or_else(|_| ctx.period());
        let ratio = Money::parse(&self.gen_ratio).unwrap_or(Money::ONE);
        if ratio.is_zero() {
            ctx.error("放大系数不能为 0");
            return;
        }
        let r = mgmt::budget_from_actual(ctx.db(), from, to, ratio);
        if let Some(n) = ctx.handle(r) {
            ctx.log(
                "预算",
                "从实际数生成",
                &format!(
                    "{} → {} ×{}，{} 行",
                    from.label(),
                    to.label(),
                    ratio.fmt_plain(),
                    n
                ),
            );
            ctx.info(format!("已按 {} 实际数生成 {} 行预算", from.label(), n));
            // 生成结果写在结束期间，直接把界面切过去，省得用户再找
            self.period_text = to.code();
            self.tab = BudgetTab::Edit;
            self.dirty = true;
        }
    }

    // ------------------------- 预算执行分析 -------------------------
    fn show_analysis(&mut self, _ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.label(
            RichText::new(
                "超支判定按科目属性：费用/成本类实际大于预算算超支，收入类相反。执行率 = |实际| / |预算|。",
            )
            .weak(),
        );
        ui.add_space(4.0);

        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("科目编码", 110.0).fixed(),
            widgets::TCol::new("科目名称", 180.0),
            widgets::TCol::new("部门", 90.0).fixed(),
            widgets::TCol::new("预算数", 130.0).right(),
            widgets::TCol::new("实际数", 130.0).right(),
            widgets::TCol::new("差异", 130.0).right(),
            widgets::TCol::new("执行率(%)", 100.0).right(),
            widgets::TCol::new("是否超支", 80.0).fixed(),
        ];
        // 最后一行是合计行：复用同一张表，列宽自动对齐
        widgets::grid(ui, "budget_vs", &cols, rows.len() + 1, 24.0, |i, c, ui| {
            if i == rows.len() {
                match c {
                    0 => { ui.label(RichText::new("合计").strong()); }
                    3 => { ui.label(RichText::new(self.sum_budget.fmt_money()).strong()); }
                    4 => { ui.label(RichText::new(self.sum_actual.fmt_money()).strong()); }
                    5 => { ui.label(RichText::new(self.sum_diff.fmt_money()).strong()); }
                    6 => { ui.label(RichText::new(format!("{}%", self.sum_rate.fmt_money())).strong()); }
                    _ => {}
                }
                return;
            }
            let r = &rows[i];
            match c {
                0 => { ui.label(RichText::new(&r.account_code).monospace()); }
                1 => { ui.label(&r.account_name); }
                2 => {
                    if r.dept.is_empty() {
                        ui.label(RichText::new("—").weak());
                    } else {
                        ui.label(&r.dept);
                    }
                }
                3 => widgets::amount_label(ui, r.budget),
                4 => widgets::amount_label(ui, r.actual),
                5 => widgets::amount_label(ui, r.diff),
                6 => { ui.label(r.rate_pct()); }
                7 => {
                    if r.over {
                        ui.label(RichText::new("超支").color(palette::CREDIT).strong());
                    } else {
                        ui.label(RichText::new("正常").weak());
                    }
                }
                _ => {}
            }
        });

        let over: Vec<BudgetRow> = self.rows.iter().filter(|r| r.over).cloned().collect();
        widgets::card(ui, "超支清单", |ui| {
            if over.is_empty() {
                ui.label(RichText::new("✔ 没有超支科目").color(palette::OK));
            } else {
                ui.colored_label(
                    palette::CREDIT,
                    format!("共 {} 个科目超出预算：", over.len()),
                );
                let ocols = [
                    widgets::TCol::new("科目", 200.0),
                    widgets::TCol::new("部门", 90.0).fixed(),
                    widgets::TCol::new("预算数", 120.0).right(),
                    widgets::TCol::new("实际数", 120.0).right(),
                    widgets::TCol::new("超支额", 120.0).right(),
                ];
                widgets::grid(ui, "budget_over", &ocols, over.len(), 22.0, |i, c, ui| {
                    let r = &over[i];
                    match c {
                        0 => { ui.label(format!("{} {}", r.account_code, r.account_name)); }
                        1 => { ui.label(&r.dept); }
                        2 => widgets::amount_label(ui, r.budget),
                        3 => widgets::amount_label(ui, r.actual),
                        4 => widgets::amount_label(ui, r.diff),
                        _ => {}
                    }
                });
            }
        });
    }
}
