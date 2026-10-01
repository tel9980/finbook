//! 多维损益：按辅助核算维度（客户 / 供应商 / 部门 / 职员 / 项目 / 存货）看利润
//!
//! 分摊规则每家公司都不一样：房租按面积、总部费用按人数、共用设备按工时……
//! 与其编一个谁都不满意的分摊率，这里只做"直接归属"——
//! 分录上挂了哪个维度就算哪个维度的，没挂的汇总成「未分配」行摆在那里，
//! 让用户自己决定要不要分、怎么分。

use egui::{RichText, Ui};
use findb::balances::{BalanceQuery, BalanceSnapshot};
use findb::mgmt::{self, DimProfit};
use fincore::account::{AcctCategory, AuxKind};
use fincore::{Money, Period};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

/// 可分析的维度（现金流量与银行账户不是损益分析维度，不提供）
const DIMS: &[AuxKind] = &[
    AuxKind::Customer,
    AuxKind::Supplier,
    AuxKind::Dept,
    AuxKind::Employee,
    AuxKind::Project,
    AuxKind::Item,
];

#[derive(Clone, Copy, PartialEq, Eq)]
#[derive(Debug)]
pub enum SortBy {
    Profit,
    Revenue,
}

pub struct DimProfitView {
    pub period_text: String,
    pub dim: AuxKind,
    pub sort_by: SortBy,
    /// 已排序、并追加了「未分配」行的数据
    pub rows: Vec<DimProfit>,
    pub sum_revenue: Money,
    pub sum_cost: Money,
    pub sum_expense: Money,
    pub sum_tax: Money,
    pub sum_profit: Money,
    pub dirty: bool,
    key: String,
}

impl Default for DimProfitView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            dim: AuxKind::Dept,
            sort_by: SortBy::Profit,
            rows: Vec::new(),
            sum_revenue: Money::ZERO,
            sum_cost: Money::ZERO,
            sum_expense: Money::ZERO,
            sum_tax: Money::ZERO,
            sum_profit: Money::ZERO,
            dirty: true,
            key: String::new(),
        }
    }
}

impl DimProfitView {
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
        let key = format!("{}|{}|{:?}", p.ymm(), self.dim.code(), self.sort_by);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        match mgmt::dim_profit(ctx.db(), p, self.dim) {
            Ok(mut rows) => {
                match self.sort_by {
                    SortBy::Profit => rows.sort_by(|a, b| b.profit.cmp(&a.profit)),
                    SortBy::Revenue => rows.sort_by(|a, b| b.revenue.cmp(&a.revenue)),
                }
                // 数据层只归集"挂了维度"的分录，剩余部分在这里补成「未分配」行
                if let Some(u) = unallocated(ctx, p, &rows) {
                    rows.push(u);
                }
                self.rows = rows;
            }
            Err(e) => {
                ctx.error(e.to_string());
                self.rows.clear();
            }
        }

        self.sum_revenue = self.rows.iter().map(|r| r.revenue).sum();
        self.sum_cost = self.rows.iter().map(|r| r.cost).sum();
        self.sum_expense = self.rows.iter().map(|r| r.expense).sum();
        self.sum_tax = self.rows.iter().map(|r| r.tax).sum();
        self.sum_profit = self.rows.iter().map(|r| r.profit).sum();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "多维损益", |ui| {
            ui.label(RichText::new(format!("{} · {}", p.label(), self.dim.label())).weak());
        });

        widgets::toolbar(ui, |ui| {
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
            ui.separator();
            ui.label("分析维度");
            let mut dim = self.dim;
            egui::ComboBox::from_id_salt("dim_kind")
                .selected_text(self.dim.label())
                .width(110.0)
                .show_ui(ui, |ui| {
                    for k in DIMS {
                        ui.selectable_value(&mut dim, *k, k.label());
                    }
                });
            if dim != self.dim {
                self.dim = dim;
                self.dirty = true;
            }
            ui.separator();
            ui.label("排序");
            let mut sort = self.sort_by;
            ui.selectable_value(&mut sort, SortBy::Profit, "按利润");
            ui.selectable_value(&mut sort, SortBy::Revenue, "按收入");
            if sort != self.sort_by {
                self.sort_by = sort;
                self.dirty = true;
            }
            ui.separator();
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                let mut sh = crate::views::export::Sheet::new(
                    "多维损益",
                    vec!["维度".to_string(), "收入".to_string(), "成本".to_string(), "费用".to_string(), "税金".to_string(), "利润".to_string()],
                );
                for r in &self.rows {
                    sh.push(vec![
                        r.name.clone(),
                        r.revenue.fmt_plain(),
                        r.cost.fmt_plain(),
                        r.expense.fmt_plain(),
                        r.tax.fmt_plain(),
                        r.profit.fmt_plain(),
                    ]);
                }
                let title = format!("多维损益（{} · {}）", p.label(), self.dim.label());
                match crate::views::export::run_export(&sh, "多维损益", &title, mode) {
                    Ok(m) => ctx.info(m),
                    Err(e) => ctx.error(e),
                }
            }
        });

        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "按辅助核算维度直接归集——只有分录上挂了该维度的才计入；未分配的公共费用会显示为「未分配」行，本版本不做分摊率分摊。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("编码", 100.0).fixed(),
            widgets::TCol::new("名称", 160.0),
            widgets::TCol::new("收入", 130.0).right(),
            widgets::TCol::new("成本", 130.0).right(),
            widgets::TCol::new("费用", 130.0).right(),
            widgets::TCol::new("税金", 120.0).right(),
            widgets::TCol::new("利润", 140.0).right(),
            widgets::TCol::new("利润占比", 100.0).right(),
        ];
        // 最后一行是合计行，复用同一张表保证列宽一致
        widgets::grid(ui, "dim_profit", &cols, rows.len() + 1, 24.0, |i, c, ui| {
            if i == rows.len() {
                match c {
                    0 => { ui.label(RichText::new("合计").strong()); }
                    2 => { ui.label(RichText::new(self.sum_revenue.fmt_money()).strong()); }
                    3 => { ui.label(RichText::new(self.sum_cost.fmt_money()).strong()); }
                    4 => { ui.label(RichText::new(self.sum_expense.fmt_money()).strong()); }
                    5 => { ui.label(RichText::new(self.sum_tax.fmt_money()).strong()); }
                    6 => { ui.label(RichText::new(self.sum_profit.fmt_money()).strong()); }
                    7 => { ui.label(RichText::new("100.00%").strong()); }
                    _ => {}
                }
                return;
            }
            let r = &rows[i];
            let unassigned = r.key.is_empty();
            match c {
                0 => {
                    if unassigned {
                        ui.label(RichText::new("—").weak());
                    } else {
                        ui.label(RichText::new(&r.key).monospace());
                    }
                }
                1 => {
                    if unassigned {
                        ui.colored_label(palette::WARN, &r.name);
                    } else {
                        ui.label(&r.name);
                    }
                }
                2 => widgets::amount_label(ui, r.revenue),
                3 => widgets::amount_label(ui, r.cost),
                4 => widgets::amount_label(ui, r.expense),
                5 => widgets::amount_label(ui, r.tax),
                6 => widgets::amount_label(ui, r.profit),
                7 => {
                    // 除零保护：利润合计为 0 时不显示比率，避免出现无意义的 0.00%
                    if self.sum_profit.is_zero() {
                        ui.label(RichText::new("—").weak());
                    } else {
                        let rate = (r.profit * Money::from_i64(100))
                            .checked_div(self.sum_profit)
                            .expect("sum_profit 已判非零")
                            .round2();
                        ui.label(format!("{}%", rate.fmt_money()));
                    }
                }
                _ => {}
            }
        });
    }
}

/// 把"没有挂任何维度值"的损益金额汇总成一行
///
/// 口径必须与 `mgmt::dim_profit` 完全一致（同样的科目前缀归类、同样的借贷符号），
/// 否则合计行会对不上——所以这里照抄了它的归类规则。
fn unallocated(ctx: &AppCtx<'_>, p: Period, allocated: &[DimProfit]) -> Option<DimProfit> {
    let snap = BalanceSnapshot::load(ctx.db(), &BalanceQuery::period(p).with_leaf_only(true)).ok()?;
    let chart = ctx.chart();

    let mut rev = Money::ZERO;
    let mut cost = Money::ZERO;
    let mut tax = Money::ZERO;
    let mut exp = Money::ZERO;
    for a in chart.all() {
        if !a.category.is_profit_loss() || !chart.is_leaf(&a.code) {
            continue;
        }
        let r = snap.for_account(&a.code, None);
        let signed = r.debit - r.credit;
        if signed.is_zero() {
            continue;
        }
        // 损益科目：收入类贷方为正，成本费用类借方为正
        let amount = if a.category == AcctCategory::Income {
            -signed
        } else {
            signed
        };
        match a.code.as_str() {
            c if c.starts_with("6001") || c.starts_with("6051") || c.starts_with("6301") => {
                rev += amount
            }
            c if c.starts_with("6401") || c.starts_with("6402") => cost += amount,
            c if c.starts_with("6403") || c.starts_with("6801") => tax += amount,
            _ => exp += amount,
        }
    }
    for d in allocated {
        rev -= d.revenue;
        cost -= d.cost;
        tax -= d.tax;
        exp -= d.expense;
    }
    if rev.is_zero() && cost.is_zero() && tax.is_zero() && exp.is_zero() {
        return None;
    }
    Some(DimProfit {
        key: String::new(),
        name: "未分配（公共费用未分摊）".to_string(),
        revenue: rev,
        cost,
        expense: exp,
        tax,
        profit: rev - cost - exp - tax,
    })
}
