//! 期初建账（启用期之前的余额）

use egui::{Align, Color32, Layout, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use findb::balances::{BeginRow, BalanceQuery};
use fincore::{dir_amount_to_signed, signed_to_dir_amount, AuxRef, Direction, Money, Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets::{self, AccountPickerState};

#[derive(Clone)]
pub struct EditRow {
    pub id: i64,
    pub code: String,
    pub aux: AuxRef,
    /// 年初余额的方向（金额恒为正）
    pub dir: Direction,
    pub yb: String,
    pub ad: String,
    pub ac: String,
    pub qty: String,
    pub changed: bool,
}

impl EditRow {
    fn blank(code: String) -> Self {
        Self {
            id: 0,
            code,
            aux: AuxRef::default(),
            dir: Direction::Debit,
            yb: String::new(),
            ad: String::new(),
            ac: String::new(),
            qty: String::new(),
            changed: true,
        }
    }
    /// 启用期期初余额（带符号）
    fn begin_signed(&self) -> Money {
        let yb = dir_amount_to_signed(self.dir, Money::parse_or_zero(&self.yb));
        yb + Money::parse_or_zero(&self.ad) - Money::parse_or_zero(&self.ac)
    }
    fn to_begin_row(&self) -> BeginRow {
        BeginRow {
            id: self.id,
            account_code: self.code.trim().to_string(),
            aux: self.aux.clone(),
            year_begin: dir_amount_to_signed(self.dir, Money::parse_or_zero(&self.yb)),
            debit_accum: Money::parse_or_zero(&self.ad),
            credit_accum: Money::parse_or_zero(&self.ac),
            qty_begin: if self.qty.trim().is_empty() {
                None
            } else {
                Some(Money::parse_or_zero(&self.qty))
            },
        }
    }
}

pub struct BeginView {
    pub rows: Vec<EditRow>,
    pub dirty: bool,
    pub kw: String,
    pub only_nonzero: bool,
    pub picker: AccountPickerState,
    pub debits: Money,
    pub credits: Money,
    pub start_period: Period,
}

impl Default for BeginView {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            dirty: true,
            kw: String::new(),
            only_nonzero: false,
            picker: AccountPickerState::default(),
            debits: Money::ZERO,
            credits: Money::ZERO,
            start_period: Period::default(),
        }
    }
}

impl BeginView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.start_period = ctx.db().options().start_period;
        let list = findb::balances::list_begin(ctx.db()).unwrap_or_default();
        self.rows = list
            .into_iter()
            .map(|b| {
                let (dir, amt) = signed_to_dir_amount(b.year_begin);
                EditRow {
                    id: b.id,
                    code: b.account_code,
                    aux: b.aux,
                    dir,
                    yb: if amt.is_zero() {
                        String::new()
                    } else {
                        amt.fmt_plain()
                    },
                    ad: if b.debit_accum.is_zero() {
                        String::new()
                    } else {
                        b.debit_accum.fmt_plain()
                    },
                    ac: if b.credit_accum.is_zero() {
                        String::new()
                    } else {
                        b.credit_accum.fmt_plain()
                    },
                    qty: b
                        .qty_begin
                        .map(|q| if q.is_zero() { String::new() } else { q.fmt_plain() })
                        .unwrap_or_default(),
                    changed: false,
                }
            })
            .collect();
        self.recount();
    }

    fn recount(&mut self) {
        let mut d = Money::ZERO;
        let mut c = Money::ZERO;
        for r in &self.rows {
            let s = r.begin_signed();
            if s.is_positive() {
                d += s;
            } else {
                c += s.negated();
            }
        }
        self.debits = d.round2();
        self.credits = c.round2();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let ectx = ui.ctx().clone();
        let can_edit = ctx.user().can(Perm::Opening);

        widgets::page_header(ui, "期初建账", |ui| {
            ui.label(
                RichText::new(format!("启用期间：{}", self.start_period.label()))
                    .weak(),
            );
        });

        widgets::toolbar(ui, |ui| {
            if ui.button("保存").clicked() && ctx.can(Perm::Opening) {
                self.save(ctx);
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if ui.button("添加科目").clicked() && can_edit {
                self.picker.open = true;
                self.picker.leaf_only = true;
            }
            if ui.button("按科目表补全").clicked() && can_edit {
                self.fill_from_chart(ctx);
            }
            ui.separator();
            ui.label("查找");
            ui.add_sized(
                [140.0, 22.0],
                egui::TextEdit::singleline(&mut self.kw).hint_text("编码或名称"),
            );
            ui.checkbox(&mut self.only_nonzero, "只看有余额的");
            ui.separator();
            if ui.button("按期末余额倒推").clicked() && can_edit {
                ctx.confirm_dangerous(
                    "按期末余额倒推期初",
                    "将用「当前期末余额 − 启用期以来发生额」重算并覆盖全部期初行。\n\
                     该操作会覆盖现有期初数据，确定继续吗？",
                    ConfirmAction::AutoFillBegin,
                    true,
                );
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        ui.label(
            RichText::new(
                "年初余额填正数并选择方向；「累计借方 / 累计贷方」填启用期之前的发生额。\
                 启用期期初 = 年初余额 + 累计借方 − 累计贷方。",
            )
            .weak()
            .size(12.0),
        );

        // ---------------- 表格 ----------------
        let kw = self.kw.trim().to_lowercase();
        let only_nonzero = self.only_nonzero;
        let indices: Vec<usize> = (0..self.rows.len())
            .filter(|&i| {
                let r = &self.rows[i];
                if only_nonzero && r.begin_signed().is_zero() {
                    return false;
                }
                if kw.is_empty() {
                    return true;
                }
                r.code.to_lowercase().contains(&kw)
                    || ctx
                        .chart()
                        .get(&r.code)
                        .map(|a| a.name.to_lowercase().contains(&kw))
                        .unwrap_or(false)
            })
            .collect();

        let names: Vec<String> = indices
            .iter()
            .map(|&i| {
                ctx.chart()
                    .get(&self.rows[i].code)
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| "⟨未知科目⟩".to_string())
            })
            .collect();
        let aux_names = &ctx.st.aux_names;
        let mut remove_idx: Option<usize> = None;
        let mut delete_id: Option<i64> = None;

        {
            let tb = TableBuilder::new(ui)
                .id_salt("begin_rows")
                .striped(true)
                .resizable(true)
                .min_scrolled_height(160.0)
                .column(Column::initial(110.0).resizable(false))
                .column(Column::initial(180.0).at_least(120.0))
                .column(Column::initial(150.0).at_least(90.0))
                .column(Column::initial(64.0).resizable(false))
                .column(Column::remainder().at_least(110.0))
                .column(Column::initial(120.0).at_least(90.0))
                .column(Column::initial(120.0).at_least(90.0))
                .column(Column::initial(130.0).at_least(90.0))
                .column(Column::initial(34.0).resizable(false));

            tb.header(26.0, |mut h| {
                for t in [
                    "科目编码",
                    "科目名称",
                    "辅助核算",
                    "方向",
                    "年初余额",
                    "累计借方",
                    "累计贷方",
                    "启用期期初",
                    "",
                ] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(26.0, indices.len(), |mut row| {
                    let k = row.index();
                    let i = indices[k];
                    let r = &mut self.rows[i];
                    row.col(|ui| {
                        ui.add_sized([ui.available_width(), 22.0], egui::TextEdit::singleline(&mut r.code));
                    });
                    row.col(|ui| {
                        let n = &names[k];
                        ui.label(RichText::new(n).color(if n.starts_with("⟨") {
                            palette::CREDIT
                        } else {
                            Color32::DARK_GRAY
                        }));
                    });
                    row.col(|ui| {
                        let txt = crate::views::voucher_edit::aux_text(&r.aux, aux_names);
                        ui.label(RichText::new(if txt.is_empty() { "—" } else { &txt }).weak());
                    });
                    row.col(|ui| {
                        let mut is_credit = r.dir == Direction::Credit;
                        if ui.checkbox(&mut is_credit, "贷").changed() {
                            r.dir = if is_credit {
                                Direction::Credit
                            } else {
                                Direction::Debit
                            };
                            r.changed = true;
                        }
                    });
                    row.col(|ui| {
                        let resp = ui.add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut r.yb)
                                .horizontal_align(Align::RIGHT)
                                .hint_text("0.00"),
                        );
                        if resp.changed() {
                            r.changed = true;
                        }
                    });
                    row.col(|ui| {
                        let resp = ui.add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut r.ad)
                                .horizontal_align(Align::RIGHT)
                                .hint_text("0.00"),
                        );
                        if resp.changed() {
                            r.changed = true;
                        }
                    });
                    row.col(|ui| {
                        let resp = ui.add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut r.ac)
                                .horizontal_align(Align::RIGHT)
                                .hint_text("0.00"),
                        );
                        if resp.changed() {
                            r.changed = true;
                        }
                    });
                    row.col(|ui| {
                        let v = r.begin_signed();
                        let (d, amt) = signed_to_dir_amount(v);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if v.is_zero() {
                                ui.label(RichText::new("平").weak());
                            } else {
                                ui.label(
                                    RichText::new(format!("{} {}", d.label(), amt.fmt_money()))
                                        .color(if d == Direction::Credit {
                                            palette::CREDIT
                                        } else {
                                            palette::DEBIT
                                        }),
                                );
                            }
                        });
                    });
                    row.col(|ui| {
                        if ui.small_button("×").clicked() {
                            if r.id > 0 {
                                delete_id = Some(r.id);
                            } else {
                                remove_idx = Some(i);
                            }
                        }
                    });
                });
            });
        }

        if let Some(id) = delete_id {
            ctx.confirm_dangerous(
                "删除期初行",
                "确定删除该科目（含其辅助核算明细）的期初余额吗？",
                ConfirmAction::DeleteBegin(id),
                true,
            );
        }
        if let Some(i) = remove_idx {
            self.rows.remove(i);
            self.recount();
        }

        // ---------------- 试算 ----------------
        ui.separator();
        self.recount();
        let diff = (self.debits - self.credits).round2();
        ui.horizontal(|ui| {
            ui.label(RichText::new("期初试算").strong());
            ui.add_space(16.0);
            ui.label(RichText::new(format!("借方合计 {}", self.debits.fmt_money())).strong());
            ui.add_space(16.0);
            ui.label(RichText::new(format!("贷方合计 {}", self.credits.fmt_money())).strong());
            ui.add_space(16.0);
            ui.label(
                RichText::new(format!("差额 {}", diff.fmt_money()))
                    .color(if diff.is_zero() {
                        palette::OK
                    } else {
                        palette::CREDIT
                    })
                    .strong(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(if diff.is_zero() {
                        "✔ 期初平衡，可以开始录凭证"
                    } else {
                        "✖ 期初不平衡，差额需为 0 才能正常出报表"
                    })
                    .color(if diff.is_zero() {
                        palette::OK
                    } else {
                        palette::CREDIT
                    }),
                );
            });
        });

        // ---------------- 科目选择 ----------------
        if let Some(code) = self.picker.show(&ectx, ctx.chart()) {
            self.rows.push(EditRow::blank(code));
            self.recount();
        }
    }

    fn save(&mut self, ctx: &mut AppCtx<'_>) {
        let mut ok = 0usize;
        let mut errs = Vec::new();
        let snapshot: Vec<EditRow> = self
            .rows
            .iter()
            .filter(|r| r.changed)
            .cloned()
            .collect();
        for r in snapshot {
            if r.code.trim().is_empty() {
                continue;
            }
            let br = r.to_begin_row();
            match findb::balances::upsert_begin(ctx.db(), &br) {
                Ok(()) => ok += 1,
                Err(e) => errs.push(format!("{}：{}", r.code, e)),
            }
        }
        // 回写 id 并清脏标记
        if errs.is_empty() {
            for r in self.rows.iter_mut() {
                r.changed = false;
            }
            ctx.log("期初", "保存期初余额", &format!("{ok} 行"));
            ctx.info(format!("已保存 {ok} 行期初余额"));
            self.dirty = true;
        } else {
            for e in errs.iter().take(5) {
                ctx.error(e.clone());
            }
        }
    }

    /// 把所有末级科目补齐一行（余额为零）
    fn fill_from_chart(&mut self, ctx: &mut AppCtx<'_>) {
        let codes: Vec<String> = ctx
            .chart()
            .all()
            .into_iter()
            .filter(|a| ctx.chart().is_leaf(&a.code))
            .map(|a| a.code.clone())
            .collect();
        let have: std::collections::HashSet<String> =
            self.rows.iter().map(|r| r.code.clone()).collect();
        let mut n = 0;
        for c in codes {
            if !have.contains(&c) {
                self.rows.push(EditRow::blank(c));
                n += 1;
            }
        }
        self.rows.sort_by(|a, b| a.code.cmp(&b.code));
        self.recount();
        ctx.info(format!("已补齐 {n} 个末级科目"));
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        if self.rows.is_empty() {
            ctx.error("没有可导出的数据");
            return;
        }
        let mut sh = crate::views::export::Sheet::new(
            "期初余额",
            vec![
                "科目编码".into(),
                "科目名称".into(),
                "方向".into(),
                "年初余额".into(),
                "累计借方".into(),
                "累计贷方".into(),
                "启用期期初".into(),
            ],
        );
        for r in &self.rows {
            let v = r.begin_signed();
            let (d, amt) = signed_to_dir_amount(v);
            sh.push(vec![
                r.code.clone(),
                ctx.chart()
                    .get(&r.code)
                    .map(|a| a.name.clone())
                    .unwrap_or_default(),
                r.dir.label().to_string(),
                Money::parse_or_zero(&r.yb).fmt_plain(),
                Money::parse_or_zero(&r.ad).fmt_plain(),
                Money::parse_or_zero(&r.ac).fmt_plain(),
                if v.is_zero() {
                    "0.00".to_string()
                } else {
                    format!("{}{}", if d == Direction::Credit { "-" } else { "" }, amt.fmt_plain())
                },
            ]);
        }
        match crate::views::export::run_export(&sh, "期初余额", "期初余额", mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}

/// 供确认动作调用：按当前期末余额倒推期初
pub fn auto_fill_begin(ctx: &mut AppCtx<'_>) -> Result<usize, String> {
    let start = ctx.db().options().start_period;
    let snap = findb::balances::BalanceSnapshot::load(
        ctx.db(),
        &BalanceQuery::range(start, Period::default()),
    )
    .map_err(|e| e.to_string())?;
    let rows = snap.raw_rows();
    let mut n = 0usize;
    for r in rows {
        if r.account_code.trim().is_empty() {
            continue;
        }
        // 期末余额 − 本期及以后发生额 = 启用期期初
        let begin = r.end() - (r.debit - r.credit);
        if begin.is_zero() && r.begin.is_zero() {
            continue;
        }
        let br = BeginRow {
            id: 0,
            account_code: r.account_code.clone(),
            aux: r.aux.clone(),
            year_begin: begin,
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        };
        findb::balances::upsert_begin(ctx.db(), &br).map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(n)
}
