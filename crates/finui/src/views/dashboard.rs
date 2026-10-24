//! 首页工作台

use egui::{Color32, RichText, Ui};
use fincore::{Money, Period, Voucher};
use findb::balances::BalanceQuery;

use crate::state::{AppCtx, NavItem};
use crate::theme::palette;
use crate::widgets;

pub struct Dashboard {
    key: String,
    draft: i64,
    audited: i64,
    posted: i64,
    void: i64,
    accounts: i64,
    entries: i64,
    begin_debit: Money,
    begin_credit: Money,
    debit: Money,
    credit: Money,
    end_debit: Money,
    end_credit: Money,
    recent: Vec<Voucher>,
    /// 待办事项
    todos: Vec<(String, String)>,
    /// 本期营业收入
    revenue: Money,
    /// 本期营业成本
    cost: Money,
    /// 本期净利润
    net_profit: Money,
    /// 本年期初起的累计净利润（年初至今）
    net_profit_ytd: Money,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            key: String::new(),
            draft: 0,
            audited: 0,
            posted: 0,
            void: 0,
            accounts: 0,
            entries: 0,
            begin_debit: Money::ZERO,
            begin_credit: Money::ZERO,
            debit: Money::ZERO,
            credit: Money::ZERO,
            end_debit: Money::ZERO,
            end_credit: Money::ZERO,
            recent: Vec::new(),
            todos: Vec::new(),
            revenue: Money::ZERO,
            cost: Money::ZERO,
            net_profit: Money::ZERO,
            net_profit_ytd: Money::ZERO,
        }
    }
}

impl Dashboard {
    pub fn invalidate(&mut self) {
        self.key.clear();
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = ctx.period();
        // key 含数据量：新增科目 / 凭证后即使错过失效通知也能自动刷新
        let (v, e, a) = ctx.db().stats().unwrap_or((0, 0, 0));
        let key = format!(
            "{}|{}|{}|{}|{}",
            ctx.db().path().display(),
            p.ymm(),
            v,
            e,
            a
        );
        if self.key == key {
            return;
        }
        self.key = key;

        let db = ctx.db();
        let (draft, audited, posted, void) =
            findb::vouchers::status_summary(db, p).unwrap_or((0, 0, 0, 0));
        self.draft = draft;
        self.audited = audited;
        self.posted = posted;
        self.void = void;
        self.entries = e;
        self.accounts = a;

        let chart = ctx.chart();
        let tb = match findb::balances::BalanceSnapshot::load(db, &BalanceQuery::period(p)) {
            Ok(s) => s.trial_balance(chart),
            Err(_) => Default::default(),
        };
        // 本期经营结果：营业收入 / 营业成本 / 净利润。
        //
        // 首页前面几个指标卡都是"账记对没有"（试算平不平、几张没记账），
        // 但不回答老板最关心的那句"这个月赚没赚"。这里把利润口径也放上来，
        // 数据源与「报表 → 利润表」完全一致（financial_totals），不另算一套。
        let year_from = fincore::Period::new(p.year(), 1).unwrap_or(p);
        if let Ok(t) = findb::advanced::financial_totals(db, p, p, None) {
            self.revenue = t.revenue;
            self.cost = t.cost;
            self.net_profit = t.net_profit;
        }
        if let Ok(t) = findb::advanced::financial_totals(db, p, year_from, None) {
            self.net_profit_ytd = t.net_profit;
        }
        self.begin_debit = tb.begin_debit;
        self.begin_credit = tb.begin_credit;
        self.debit = tb.period_debit;
        self.credit = tb.period_credit;
        self.end_debit = tb.end_debit;
        self.end_credit = tb.end_credit;

        let mut q = findb::vouchers::VoucherQuery::period(p);
        q.asc = false;
        q.limit = Some(10);
        q = q.with_data_scope(ctx.user());
        let u = ctx.user().clone();
        let mut recent = findb::vouchers::list(db, &q).unwrap_or_default();
        // 最近凭证要展示摘要与借贷合计，且科目范围过滤依赖分录：先批量补充分录
        let _ = findb::vouchers::fill_entries(db, &mut recent);
        if !u.data_scope.is_unrestricted() {
            recent.retain(|v| u.can_see_voucher(v));
        }
        self.recent = recent;

        // 待办
        let mut todos = Vec::new();
        if draft + audited > 0 {
            todos.push((
                format!("{} 张凭证未记账", draft + audited),
                "到「凭证查询」勾选后批量记账，或在「期末处理」批量记账".to_string(),
            ));
        }
        if let Ok(Some(closed)) = findb::periods::closed_upto(db) {
            if closed < p {
                todos.push((
                    format!("{} 及之前的期间尚未结账", closed.next().label()),
                    "到「期末处理」执行结账".to_string(),
                ));
            }
        } else {
            todos.push((
                "账套尚未结账过任何期间".to_string(),
                "到「期末处理」执行结账".to_string(),
            ));
        }
        if let Ok(pl) = findb::balances::BalanceSnapshot::load(db, &BalanceQuery::period(p)) {
            let rows = pl.profit_loss_rows(chart);
            if !rows.is_empty() {
                todos.push((
                    "本期损益尚未结转".to_string(),
                    "到「期末处理」生成结转损益凭证".to_string(),
                ));
            }
        }
        let gaps = findb::vouchers::find_gaps(db, p, "记").unwrap_or_default();
        if !gaps.is_empty() {
            let show: Vec<String> = gaps.iter().take(5).map(|g| g.to_string()).collect();
            todos.push((
                format!("凭证断号 {} 个", gaps.len()),
                format!("缺号：{}", show.join("、")),
            ));
        }
        // 报税日历：与 Web 工作台同口径（申报窗口期内才出现）
        let taxpayer = findb::taxdecl::TaxOptions::load(db)
            .map(|o| o.taxpayer)
            .unwrap_or_default();
        let today = chrono::Local::now().date_naive();
        for t in findb::taxdecl::deadline_todos(today, &taxpayer) {
            todos.push((t.label, t.detail));
        }
        self.todos = todos;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        let p = ctx.period();
        widgets::page_header(ui, "首页", |ui| {
            ui.label(RichText::new(format!("当前期间：{}", p.label())).weak());
        });
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(4.0);

            // ---- 指标卡 ----
            ui.columns(4, |cols| {
                stat_card(
                    &mut cols[0],
                    "本期凭证",
                    &format!("{}", self.draft + self.audited + self.posted + self.void),
                    &format!("已记账 {}", self.posted),
                    palette::PRIMARY,
                );
                stat_card(
                    &mut cols[1],
                    "待处理",
                    &format!("{}", self.draft + self.audited),
                    "未记账凭证，核对后点「记账」",
                    if self.draft + self.audited > 0 {
                        palette::WARN
                    } else {
                        palette::OK
                    },
                );
                stat_card(
                    &mut cols[2],
                    "本期发生额",
                    &self.debit.fmt_money(),
                    &format!("贷方 {}", self.credit.fmt_money()),
                    palette::PRIMARY,
                );
                stat_card(
                    &mut cols[3],
                    "期末余额",
                    &self.end_debit.fmt_money(),
                    &format!("贷方 {}", self.end_credit.fmt_money()),
                    if (self.end_debit - self.end_credit).round2().is_zero() {
                        palette::OK
                    } else {
                        palette::CREDIT
                    },
                );
            });

            ui.add_space(12.0);

            // ---- 经营结果：这个月赚没赚钱 ----
            //
            // 放在试算平衡之前：老板每天打开系统最想看到的第一件事是"我赚了多少"，
            // 而不是"我账平不平"。账平不平是会计的事，赚钱与否是老板的事。
            egui::Frame::NONE
                .stroke(egui::Stroke::new(1.0_f32, palette::GRID))
                .corner_radius(6.0)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.label(RichText::new("本期经营结果").strong());
                    ui.separator();
                    ui.columns(3, |cols| {
                        let np_color = if self.net_profit.is_negative() {
                            palette::CREDIT
                        } else if self.net_profit.is_zero() {
                            palette::GRID
                        } else {
                            palette::OK
                        };
                        stat_card(
                            &mut cols[0],
                            "营业收入",
                            &self.revenue.fmt_money(),
                            "本月加工费等收入合计",
                            palette::PRIMARY,
                        );
                        stat_card(
                            &mut cols[1],
                            "营业成本",
                            &self.cost.fmt_money(),
                            "材料 + 人工 + 外发 + 折旧等",
                            palette::WARN,
                        );
                        stat_card(
                            &mut cols[2],
                            "净利润",
                            &self.net_profit.fmt_money(),
                            &format!("年初至今 {}", self.net_profit_ytd.fmt_money()),
                            np_color,
                        );
                    });
                });

            ui.add_space(12.0);

            // ---- 试算平衡 ----
            ui.columns(2, |cols| {
                egui::Frame::NONE
                    .stroke(egui::Stroke::new(1.0_f32, palette::GRID))
                    .corner_radius(6.0)
                    .inner_margin(12.0)
                    .show(&mut cols[0], |ui| {
                        ui.label(RichText::new("试算平衡").strong());
                        ui.separator();
                        let balanced = (self.end_debit - self.end_credit).round2().is_zero();
                        ui.horizontal(|ui| {
                            ui.label("期初：");
                            ui.label(RichText::new(format!(
                                "借 {} / 贷 {}",
                                self.begin_debit.fmt_money(),
                                self.begin_credit.fmt_money()
                            )));
                        });
                        ui.horizontal(|ui| {
                            ui.label("本期：");
                            ui.label(RichText::new(format!(
                                "借 {} / 贷 {}",
                                self.debit.fmt_money(),
                                self.credit.fmt_money()
                            )));
                        });
                        ui.horizontal(|ui| {
                            ui.label("期末：");
                            ui.label(RichText::new(format!(
                                "借 {} / 贷 {}",
                                self.end_debit.fmt_money(),
                                self.end_credit.fmt_money()
                            )));
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(if balanced {
                                "✔ 借贷平衡"
                            } else {
                                "✖ 借贷不平衡，请检查凭证"
                            })
                            .color(if balanced {
                                palette::OK
                            } else {
                                palette::CREDIT
                            })
                            .strong(),
                        );
                    });

                egui::Frame::NONE
                    .stroke(egui::Stroke::new(1.0_f32, palette::GRID))
                    .corner_radius(6.0)
                    .inner_margin(12.0)
                    .show(&mut cols[1], |ui| {
                        ui.label(RichText::new("待办事项").strong());
                        ui.separator();
                        if self.todos.is_empty() {
                            ui.label(RichText::new("✔ 本期账务已处理完毕").color(palette::OK));
                        } else {
                            for (t, d) in &self.todos {
                                ui.horizontal(|ui| {
                                    ui.colored_label(palette::WARN, "•");
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new(t).strong());
                                        ui.label(RichText::new(d).weak().size(12.0));
                                    });
                                });
                                ui.add_space(2.0);
                            }
                        }
                    });
            });

            ui.add_space(12.0);

            // ---- 最近凭证 ----
            ui.horizontal(|ui| {
                ui.label(RichText::new("最近凭证").strong().size(15.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("填制凭证").clicked() {
                        ctx.nav(NavItem::VoucherNew);
                    }
                    if ui.button("凭证查询").clicked() {
                        ctx.nav(NavItem::VoucherList);
                    }
                });
            });
            ui.separator();

            let rows = self.recent.len();
            let cols = [
                widgets::TCol::new("日期", 100.0).fixed(),
                widgets::TCol::new("凭证号", 100.0).fixed(),
                widgets::TCol::new("摘要", 300.0),
                widgets::TCol::new("借方合计", 130.0).right(),
                widgets::TCol::new("贷方合计", 130.0).right(),
                widgets::TCol::new("状态", 80.0).fixed(),
                widgets::TCol::new("制单人", 90.0).fixed(),
            ];
            widgets::grid(ui, "dash_recent", &cols, rows, 24.0, |i, c, ui| {
                let v = &self.recent[i];
                match c {
                    0 => {
                        ui.label(v.date.format("%Y-%m-%d").to_string());
                    }
                    1 => {
                        ui.label(RichText::new(v.voucher_no()).monospace());
                    }
                    2 => {
                        ui.label(v.first_summary());
                    }
                    3 => {
                        widgets::amount_label(ui, v.debit_total());
                    }
                    4 => {
                        widgets::amount_label(ui, v.credit_total());
                    }
                    5 => {
                        ui.label(
                            RichText::new(v.status.label())
                                .color(crate::theme::status_color(v.status.counts())),
                        );
                    }
                    6 => {
                        ui.label(&v.prepared_by);
                    }
                    _ => {}
                }
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "账套共 {} 个科目、{} 条分录",
                    self.accounts, self.entries
                ))
                .weak(),
            );
        });
    }
}

fn stat_card(ui: &mut Ui, title: &str, value: &str, sub: &str, color: Color32) {
    egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0_f32, palette::GRID))
        .corner_radius(6.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.label(RichText::new(title).weak());
            ui.label(RichText::new(value).size(24.0).color(color).strong());
            ui.label(RichText::new(sub).weak().size(12.0));
        });
}

/// 期间切换时由外壳调用
pub fn period_label(p: Period) -> String {
    p.label()
}
