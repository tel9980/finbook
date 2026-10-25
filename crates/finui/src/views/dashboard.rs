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
    /// 上月净利润（环比基准）
    net_profit_prev: Money,
    /// 成本构成 Top（科目名, 金额），按金额降序
    cost_top: Vec<(String, Money)>,
    /// 未记账凭证张数（数据完整性提示：有未记账凭证时利润不完整）
    unposted_count: i64,
    /// 钱在哪：应收应付合到一起看
    money: Option<findb::advanced::MoneyPosition>,
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
            net_profit_prev: Money::ZERO,
            cost_top: Vec::new(),
            unposted_count: 0,
            money: None,
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
        // 上月净利润：只为了算环比。"这个月赚 8 万"本身没有意义，
        // 老板真正想知道的是"比上个月好还是差"。
        let prev_p = p.prev();
        if let Ok(t) = findb::advanced::financial_totals(db, prev_p, prev_p, None) {
            self.net_profit_prev = t.net_profit;
        }
        // 成本构成 Top 3：老板一定会问"钱都花哪了"，把最大的几块直接摆出来，
        // 不用再点进报表翻。
        self.cost_top.clear();
        if let Ok(a) = findb::advanced::financial_analysis(db, p, None) {
            for d in a.cost_drivers.iter().take(3) {
                if d.amount.is_zero() {
                    continue;
                }
                self.cost_top.push((d.name.clone(), d.amount));
            }
        }
        self.unposted_count = draft + audited;

        // 「钱在哪」：利润是账面赚的，这个是真正压在外面的钱。
        // 很多小厂账面赚钱却倒在这里——所以它必须和净利润并排出现。
        let as_of = chrono::NaiveDate::from_ymd_opt(p.year(), p.month(), 1)
            .map(|d| {
                // 取当月最后一天作为账龄计算基准
                let next = if p.month() == 12 {
                    chrono::NaiveDate::from_ymd_opt(p.year() + 1, 1, 1)
                } else {
                    chrono::NaiveDate::from_ymd_opt(p.year(), p.month() + 1, 1)
                };
                next.and_then(|n| n.pred_opt()).unwrap_or(d)
            })
            .unwrap_or_else(|| chrono::Local::now().date_naive());
        self.money = findb::advanced::money_position(db, p, as_of, None).ok();
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
            let rows = pl.carry_forward_rows(chart);
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

                    let np_color = if self.net_profit.is_negative() {
                        palette::CREDIT
                    } else if self.net_profit.is_zero() {
                        palette::GRID
                    } else {
                        palette::OK
                    };

                    // ---- 净利润：唯一的视觉主角 ----
                    //
                    // 收入、成本、利润率都是陪衬，老板每天只想知道一个数：
                    // 这个月赚了多少、比上个月好还是差。所以净利润给最大字号，
                    // 其它信息全部退成小字挂在副标题里。
                    ui.vertical(|ui| {
                        ui.label(RichText::new("本月净利润").weak().size(13.0));
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(self.net_profit.fmt_money())
                                    .size(38.0)
                                    .color(np_color)
                                    .strong(),
                            );
                            // 环比：箭头 + 差额，比"上月 X 元"更直观
                            if !self.net_profit_prev.is_zero() || !self.net_profit.is_zero() {
                                let diff = (self.net_profit - self.net_profit_prev).round2();
                                if !diff.is_zero() {
                                    let (arrow, c) = if diff.is_positive() {
                                        ("▲", palette::OK)
                                    } else {
                                        ("▼", palette::CREDIT)
                                    };
                                    ui.label(
                                        RichText::new(format!(
                                            "{} 较上月 {}",
                                            arrow,
                                            diff.abs().fmt_money()
                                        ))
                                        .size(14.0)
                                        .color(c),
                                    );
                                }
                            }
                        });
                        // 副标题：利润率 + 年初至今
                        let margin = if self.revenue.is_zero() {
                            String::from("—")
                        } else {
                            let pct = (self.net_profit.to_f64() / self.revenue.to_f64()) * 100.0;
                            format!("{pct:.1}%")
                        };
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("利润率 {margin}"))
                                    .size(12.0)
                                    .weak(),
                            );
                            ui.label(RichText::new("·").weak().size(12.0));
                            ui.label(
                                RichText::new(format!(
                                    "年初至今 {}",
                                    self.net_profit_ytd.fmt_money()
                                ))
                                .size(12.0)
                                .weak(),
                            );
                        });
                    });

                    ui.add_space(8.0);
                    ui.separator();

                    // ---- 收入 / 成本：退为陪衬 ----
                    let cost_detail = if self.cost_top.is_empty() {
                        "本月尚无成本发生".to_string()
                    } else {
                        self.cost_top
                            .iter()
                            .map(|(n, v)| format!("{n} {}", v.fmt_money()))
                            .collect::<Vec<_>>()
                            .join(" · ")
                    };
                    ui.columns(2, |cols| {
                        cols[0].label("营业收入");
                        cols[0].label(
                            RichText::new(self.revenue.fmt_money())
                                .size(20.0)
                                .color(palette::PRIMARY),
                        );
                        cols[0].label(RichText::new("本月收入合计").weak().size(11.0));

                        cols[1].label("营业成本");
                        cols[1].label(
                            RichText::new(self.cost.fmt_money())
                                .size(20.0)
                                .color(palette::WARN),
                        );
                        // 成本构成 Top3，直接摊在成本下面
                        cols[1].label(RichText::new(cost_detail).weak().size(11.0));
                    });

                    // ---- 数据完整性提示 ----
                    //
                    // 有未记账凭证时利润一定是偏小的，但界面上看不出来。
                    // 老板拿着这个数做决定就会错，所以必须明说。
                    if self.unposted_count > 0 {
                        ui.add_space(6.0);
                        ui.colored_label(
                            palette::WARN,
                            RichText::new(format!(
                                "⚠ {} 张凭证还没记账，未计入上面的数字",
                                self.unposted_count
                            ))
                            .size(12.0),
                        );
                    }
                });

            ui.add_space(12.0);

            // ---- 钱在哪：应收应付合到一起 ----
            //
            // 利润回答"赚没赚"，这一块回答"钱在谁手上"。小厂倒闭多半不是因为
            // 不赚钱，而是赚的钱全压在客户那里、同时还要按月付房租工资。
            // 所以这两块必须并排，缺一个都做不出正确判断。
            if let Some(m) = self.money.clone() {
                egui::Frame::NONE
                    .stroke(egui::Stroke::new(1.0_f32, palette::GRID))
                    .corner_radius(6.0)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new("钱在哪").strong());
                        ui.separator();

                        // 四个数：别人欠我 / 我欠别人 / 手头现金 / 净头寸
                        ui.columns(4, |cols| {
                            cols[0].label("别人欠我");
                            cols[0].label(
                                RichText::new(m.receivable.fmt_money())
                                    .size(22.0)
                                    .color(palette::PRIMARY)
                                    .strong(),
                            );
                            let mut sub = format!("{} 家客户", m.top_customers.len());
                            if !m.overdue_90.is_zero() {
                                sub = format!("{} · 超90天 {}", sub, m.overdue_90.fmt_money());
                            }
                            cols[0].label(RichText::new(sub).weak().size(11.0));

                            cols[1].label("我欠别人");
                            cols[1].label(
                                RichText::new(m.payable.fmt_money())
                                    .size(22.0)
                                    .color(palette::WARN)
                                    .strong(),
                            );
                            cols[1].label(
                                RichText::new(format!("{} 家供应商", m.top_suppliers.len()))
                                    .weak()
                                    .size(11.0),
                            );

                            cols[2].label("手头现金");
                            cols[2].label(
                                RichText::new(m.cash.fmt_money())
                                    .size(22.0)
                                    .color(palette::OK)
                                    .strong(),
                            );
                            let mut cash_sub = "现金 + 银行".to_string();
                            if !m.advance_in.is_zero() {
                                cash_sub = format!("{} · 预收 {}", cash_sub, m.advance_in.fmt_money());
                            }
                            cols[2].label(RichText::new(cash_sub).weak().size(11.0));

                            // 净头寸：正=别人欠我的多，负=我欠别人的多
                            cols[3].label("净头寸");
                            let net_c = if m.net().is_negative() {
                                palette::CREDIT
                            } else {
                                palette::PRIMARY
                            };
                            cols[3].label(
                                RichText::new(m.net().fmt_money())
                                    .size(22.0)
                                    .color(net_c)
                                    .strong(),
                            );
                            cols[3].label(
                                RichText::new("应收 − 应付，不含现金").weak().size(11.0),
                            );
                        });

                        // ---- 明细：谁欠我、我欠谁 ----
                        if !m.top_customers.is_empty() || !m.top_suppliers.is_empty() {
                            ui.add_space(8.0);
                            ui.separator();
                            ui.columns(2, |cols| {
                                cols[0].label(RichText::new("欠我最多的客户").weak().size(12.0));
                                if m.top_customers.is_empty() {
                                    cols[0].label(
                                        RichText::new("没有未收款项").weak().size(11.0),
                                    );
                                }
                                for c in m.top_customers.iter() {
                                    let overdue = c.max_days >= 90;
                                    cols[0].horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!("{} {}", c.code, c.name))
                                                .size(12.0),
                                        );
                                        ui.label(
                                            RichText::new(c.amount.fmt_money())
                                                .size(12.0)
                                                .strong(),
                                        );
                                        // 账龄：超过 90 天标红，这才是要催的
                                        let (txt, col) = if overdue {
                                            (format!("{} 天", c.max_days), palette::CREDIT)
                                        } else if c.max_days >= 60 {
                                            (format!("{} 天", c.max_days), palette::WARN)
                                        } else {
                                            (format!("{} 天", c.max_days), palette::GRID)
                                        };
                                        ui.label(RichText::new(txt).size(11.0).color(col));
                                    });
                                    // 双向身份：他同时是我的供应商
                                    // 不合并成一行（两边金额、账龄都要能查），但必须
                                    // 把净额摆出来 —— 否则老板可能两头都付/两头都催。
                                    if !c.also.is_zero() {
                                        cols[0].horizontal(|ui| {
                                            ui.add_space(12.0);
                                            ui.label(
                                                RichText::new(format!(
                                                    "↔ 我欠他 {} → 净{} {}",
                                                    c.also.fmt_money(),
                                                    if c.net_is_receivable() { "他欠我" } else { "我欠他" },
                                                    c.net_with().fmt_money()
                                                ))
                                                .size(11.0)
                                                .color(palette::WARN),
                                            );
                                        });
                                    }
                                }

                                cols[1].label(RichText::new("我欠最多的供应商").weak().size(12.0));
                                if m.top_suppliers.is_empty() {
                                    cols[1].label(
                                        RichText::new("没有未付款项").weak().size(11.0),
                                    );
                                }
                                for sp in m.top_suppliers.iter() {
                                    cols[1].horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!("{} {}", sp.code, sp.name))
                                                .size(12.0),
                                        );
                                        ui.label(
                                            RichText::new(sp.amount.fmt_money())
                                                .size(12.0)
                                                .strong(),
                                        );
                                        ui.label(
                                            RichText::new(format!("{} 天", sp.max_days))
                                                .size(11.0)
                                                .weak(),
                                        );
                                    });
                                    if !sp.also.is_zero() {
                                        cols[1].horizontal(|ui| {
                                            ui.add_space(12.0);
                                            ui.label(
                                                RichText::new(format!(
                                                    "↔ 他欠我 {} → 净{} {}",
                                                    sp.also.fmt_money(),
                                                    if sp.net_is_receivable() { "他欠我" } else { "我欠他" },
                                                    sp.net_with().fmt_money()
                                                ))
                                                .size(11.0)
                                                .color(palette::WARN),
                                            );
                                        });
                                    }
                                }
                            });
                        }

                        // ---- 催收提示 ----
                        if m.overdue_90_count > 0 {
                            ui.add_space(6.0);
                            ui.colored_label(
                                palette::CREDIT,
                                RichText::new(format!(
                                    "⚠ {} 家客户的钱超过 90 天没收回，共 {}，该催了",
                                    m.overdue_90_count,
                                    m.overdue_90.fmt_money()
                                ))
                                .size(12.0),
                            );
                        }
                    });
            }

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
