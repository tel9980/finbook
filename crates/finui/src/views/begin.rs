//! 期初建账（启用期之前的余额）

use egui::{Align, Color32, Layout, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use findb::balances::{BeginRow, BalanceQuery};
use fincore::{dir_amount_to_signed, signed_to_dir_amount, AuxRef, Direction, Money, Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::views::begin_diag;
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
    pub(super) fn blank(code: String) -> Self {
        Self::blank_dir(code, Direction::Debit)
    }

    /// 该行的金额方向是否与科目方向不一致。
    ///
    /// 试算不平时，本函数找出的行就是**最优先的嫌疑人**：它的金额被放在了
    /// 与科目余额方向相反的一边。注意这不是「错误」—— 红字/负数、备抵科目
    /// 的反方向余额都是合法的，所以只在**试算不平**时才标出来，平时不打扰。
    ///
    /// 逻辑：年初余额（yb）是用户填的「那个科目的数」，它的方向是 r.dir；
    /// 科目的正常方向是 chart_dir。若两者相反，且累计借贷都是 0
    /// （即这行没有「期初之前还有发生额」的复杂情况），就标出来。
    pub(super) fn dir_mismatch(&self, chart_dir: Option<Direction>) -> bool {
        let Some(cd) = chart_dir else { return false };
        // 累计借贷非空时，期初数是三项合成的，不能简单按方向判断
        if !Money::parse_or_zero(&self.ad).is_zero() || !Money::parse_or_zero(&self.ac).is_zero() {
            return false;
        }
        let amt = Money::parse_or_zero(&self.yb);
        if amt.is_zero() {
            return false;
        }
        self.dir != cd
    }

    /// 按科目方向预填借贷。以前新建行一律借方 —— 资产确实是借，
    /// 但负债/权益/收入默认也是借，新手照着这个填，填完试算必不平，
    /// 他还不知道错在哪（方向错了，金额看着都对）。
    fn blank_dir(code: String, dir: Direction) -> Self {
        Self {
            id: 0,
            code,
            aux: AuxRef::default(),
            dir,
            yb: String::new(),
            ad: String::new(),
            ac: String::new(),
            qty: String::new(),
            changed: true,
        }
    }
    /// 启用期期初余额（带符号）。begin_diag 模块要用，改这行的方向/累计数
    /// 后它会变，所以只在内存里算，不落库。
    pub(super) fn begin_signed(&self) -> Money {
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
    /// 批量粘贴窗口
    pub batch: super::begin_batch::Ui,
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
            batch: Default::default(),
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
        self.recount();

        // 方向异常行的下标（试算不平时才算）。放在表格渲染之前，
        // 表格和试算区都要用它 —— 算一次，两处共用。
        let diff_now = (self.debits - self.credits).round2();
        let mismatched: Vec<usize> = if diff_now.is_zero() {
            Vec::new()
        } else {
            let chart = ctx.chart();
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    let cd = chart.get(&r.code).map(|a| a.dir);
                    r.dir_mismatch(cd)
                })
                .map(|(i, _)| i)
                .collect()
        };

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
            if ui.button("批量粘贴").clicked() && can_edit {
                self.batch = Default::default();
                self.batch.open = true;
            }
            if ui
                .button("下载模板")
                .on_hover_text("保存一个 .xlsx 空表（带示例和填表说明），用 Excel 填好后再载入")
                .clicked()
                && can_edit
            {
                let sheets = crate::views::export::batch_template_sheets(
                    "期初余额",
                    &fincore::batch::begin::template(),
                );
                match crate::views::export::save_template(&sheets, "期初余额导入模板") {
                    Ok(Some(m)) => ctx.info(m),
                    Ok(None) => {}
                    Err(e) => ctx.error(e),
                }
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
                // 前置检查：新账套（启用期以来没有任何发生额、也没有余额）点这个
                // 等于白点 —— auto_fill_begin 会算出 n=0，静默结束。
                // 用户刚点了「确定」，结果什么都没发生，他会以为按钮坏了。
                // 所以先数有没有可倒推的数据，没有就直接告诉他，别让他走确认流程。
                if !has_reversible_data(ctx.db()) {
                    ctx.error(
                        "账套里还没有任何余额和发生额，没有可倒推的数据。\n\
                         新账套请用「批量粘贴」从上期余额表导入，或逐行填写。",
                    );
                } else {
                    ctx.confirm_dangerous(
                        "按期末余额倒推期初",
                        "将用「当前期末余额 − 启用期以来发生额」重算并覆盖全部期初行。\n\
                         该操作会覆盖现有期初数据，确定继续吗？",
                        ConfirmAction::AutoFillBegin,
                        true,
                    );
                }
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
                    // 方向异常的行，编码框标红底 —— 200 行里一眼找到嫌疑人
                    let is_mismatch = mismatched.contains(&i);
                    row.col(|ui| {
                        let mut ed = egui::TextEdit::singleline(&mut r.code);
                        if is_mismatch {
                            ed = ed.text_color(palette::CREDIT);
                        }
                        ui.add_sized([ui.available_width(), 22.0], ed);
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

        // 嫌疑人提示：有方向异常的行才显示，没找到就直说（别让用户白找）
        if !diff.is_zero() && !mismatched.is_empty() {
            ui.horizontal(|ui| {
                ui.colored_label(
                    palette::CREDIT,
                    format!(
                        "⚠ 下面 {} 行的金额方向和科目方向相反，最可能是它们造成的，先检查：",
                        mismatched.len()
                    ),
                );
                for (k, &i) in mismatched.iter().enumerate() {
                    if k > 0 {
                        ui.label("、");
                    }
                    let code = self.rows[i].code.clone();
                    let name = ctx
                        .chart()
                        .get(&code)
                        .map(|a| a.name.clone())
                        .unwrap_or_default();
                    if ui.link(format!("{code} {name}")).clicked() {
                        // 跳到那一行：把搜索框设成该编码，列表自动过滤出来
                        self.kw = code.clone();
                        self.only_nonzero = false;
                    }
                }
            });
            ui.label(
                RichText::new("（备抵科目、红字余额等合法反方向也会被列出，请自行确认；点编码可直接跳到那一行）")
                    .weak()
                    .size(12.0),
            );
        }

        // ---------------- 不平的解法 ----------------
        //
        // 上面那条是「按科目表语义猜」的，命中不了几种情况：有累计数的行、
        // 科目表本身标错方向、压根不是方向问题（漏填了对方科目）。
        // 下面这层换成解方程 —— 只报**改完一定能平**的改法，用户改完当场验证，
        // 不用再猜。试算平了整块都不显示。
        let fixes = begin_diag::explain(diff, &self.rows);
        if !fixes.is_empty() {
            ui.add_space(2.0);
            ui.label(
                RichText::new("照下面改，试算就平了（这些改法都算过了，改完一定平）")
                    .color(palette::OK),
            );
            for f in fixes.iter() {
                if f.row >= self.rows.len() {
                    continue;
                }
                let code = self.rows[f.row].code.clone();
                let name = ctx
                    .chart()
                    .get(&code)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let label = if name.is_empty() {
                    code.clone()
                } else {
                    format!("{code} {name}")
                };
                ui.horizontal(|ui| {
                    let act = f.kind.action_label();
                    let clicked = if act.is_empty() {
                        false
                    } else if ui.button(format!("{label} — {act}")).clicked() {
                        true
                    } else {
                        ui.link(format!("{label}")).clicked()
                    };
                    if clicked {
                        match f.kind {
                            begin_diag::FixKind::FlipDir => {
                                let r = &mut self.rows[f.row];
                                r.dir = match r.dir {
                                    Direction::Debit => Direction::Credit,
                                    Direction::Credit => Direction::Debit,
                                };
                                r.changed = true;
                                self.recount();
                            }
                            begin_diag::FixKind::SwapAccum => {
                                let r = &mut self.rows[f.row];
                                std::mem::swap(&mut r.ad, &mut r.ac);
                                r.changed = true;
                                self.recount();
                            }
                            begin_diag::FixKind::MissingCounterpart => {
                                self.kw = code.clone();
                                self.only_nonzero = false;
                            }
                        }
                    }
                    ui.label(RichText::new(format!("：{}", f.detail)).weak());
                });
            }
        }

        // 兜底：上面解不出来时的通用出路 —— 补一行对方科目。
        // 金额和方向由差额直接算出，用户只需选科目，不用自己算该填多少。
        if let Some((dir, amt)) = begin_diag::counterpart_fill(diff) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if fixes.is_empty() {
                    "没找到能解释差额的行。通常是只填了一边 —— 补上对方科目："
                } else {
                    "都不对？补一笔对方的："
                })
                .weak());
                if ui
                    .button(format!(
                        "新增一行{} {}",
                        begin_diag::dir_word(dir),
                        amt.fmt_money()
                    ))
                    .on_hover_text("只是加一行空的，方向和金额已按差额填好，选个科目即可")
                    .clicked()
                {
                    // 预填好金额和方向，用户打开行时直接选科目就行
                    self.rows.push(EditRow::blank_dir(String::new(), dir));
                    self.recount();
                    self.kw = String::new();
                    self.only_nonzero = false;
                }
            });
            ui.label(
                RichText::new("（多填/漏填科目、数字抄错也会导致不平 —— 补完记得再看一眼试算）")
                    .weak()
                    .size(12.0),
            );
        }

        // ---------------- 科目选择 ----------------
        if let Some(code) = self.picker.show(&ectx, ctx.chart()) {
            let dir = ctx.chart().get(&code).map(|a| a.dir).unwrap_or(Direction::Debit);
            self.rows.push(EditRow::blank_dir(code, dir));
            self.recount();
        }

        // ---------------- 批量粘贴 ----------------
        if self.batch.open {
            let chart = ctx.chart().clone();
            let dir_of = |code: &str| chart.get(code).map(|a| a.dir);
            let leaf_codes: Vec<String> =
                chart.all().into_iter().filter(|a| chart.is_leaf(&a.code)).map(|a| a.code.clone()).collect();
            let existing: Vec<String> = self.rows.iter().map(|r| r.code.trim().to_string()).collect();
            let rows = &mut self.rows;
            let mut on_import = |items: Vec<super::begin_batch::ParsedRow>| {
                let mut n = 0;
                for it in items {
                    // 同一科目已在草稿里就不重复加（改那一行即可）
                    if rows.iter().any(|r| r.code.trim() == it.code) {
                        continue;
                    }
                    let mut e = EditRow::blank(it.code.clone());
                    e.dir = it.dir;
                    e.yb = it.amount.fmt_money().trim().to_string();
                    e.changed = true;
                    rows.push(e);
                    n += 1;
                }
                rows.sort_by(|a, b| a.code.cmp(&b.code));
                n
            };
            if self.batch.window(&dir_of, &leaf_codes, &existing, &mut on_import, ui) {
                self.recount();
            }
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
                let dir = ctx.chart().get(&c).map(|a| a.dir).unwrap_or(Direction::Debit);
                self.rows.push(EditRow::blank_dir(c, dir));
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
/// 账套里是否有可供「按期末余额倒推」的数据。
///
/// 倒推公式是 期末 − 本期发生 = 期初；两边都是 0 的行会被 auto_fill_begin 跳过。
/// 若**所有**行都可跳过，点按钮等于白点 —— 用户刚点了「确定」却什么都没发生，
/// 他会以为按钮坏了。所以点按钮时先调这个，没有就直接报错，别走确认流程。
pub fn has_reversible_data(db: &findb::Db) -> bool {
    let start = db.options().start_period;
    findb::balances::BalanceSnapshot::load(db, &BalanceQuery::range(start, Period::default()))
        .map(|snap| {
            snap.raw_rows().iter().any(|r| {
                !r.account_code.trim().is_empty() && (!r.end().is_zero() || !r.begin.is_zero())
            })
        })
        .unwrap_or(false)
}

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

#[cfg(test)]
mod mismatch_tests {
    use super::*;

    fn row(code: &str, dir: Direction, yb: &str) -> EditRow {
        let mut e = EditRow::blank(code.to_string());
        e.dir = dir;
        e.yb = yb.to_string();
        e
    }

    /// 方向与科目一致 → 不是嫌疑人
    #[test]
    fn 方向一致不标() {
        let r = row("1001", Direction::Debit, "5000");
        assert!(!r.dir_mismatch(Some(Direction::Debit)));
        let r = row("2001", Direction::Credit, "3000");
        assert!(!r.dir_mismatch(Some(Direction::Credit)));
    }

    /// 新手最常见的错：负债填到借方
    #[test]
    fn 方向相反标出() {
        let r = row("2001", Direction::Debit, "3000");
        assert!(r.dir_mismatch(Some(Direction::Credit)), "负债放借方必须被标出");
        let r = row("1001", Direction::Credit, "5000");
        assert!(r.dir_mismatch(Some(Direction::Debit)), "资产放贷方必须被标出");
    }

    /// 金额为 0 的行不标 —— 刚补全出来的空行全是 0，不该一上来就满屏红
    #[test]
    fn 零金额不标() {
        let r = row("2001", Direction::Debit, "");
        assert!(!r.dir_mismatch(Some(Direction::Credit)));
        let r = row("2001", Direction::Debit, "0");
        assert!(!r.dir_mismatch(Some(Direction::Credit)));
    }

    /// 有累计借贷时，期初数是三项合成的，不能简单按方向判断
    #[test]
    fn 有累计发生额时不误标() {
        let mut r = row("2001", Direction::Debit, "3000");
        r.ad = "100".to_string();
        assert!(!r.dir_mismatch(Some(Direction::Credit)), "有累计数时不该按方向标");
    }

    /// 科目表里没有这个编码 → 不标（未知科目那一列已经是红字提示了）
    #[test]
    fn 未知科目不标() {
        let r = row("9999", Direction::Debit, "100");
        assert!(!r.dir_mismatch(None));
    }
}

#[cfg(test)]
mod reversible_tests {
    use super::*;

    fn fresh_db(suffix: &str) -> findb::Db {
        let path = std::env::temp_dir().join(format!(
            "finui-reversible-test-{}-{}.fbk",
            std::process::id(),
            suffix
        ));
        let _ = std::fs::remove_file(&path);
        let opts = fincore::BookOptions {
            code_scheme: vec![4, 2, 2, 2],
            start_period: Period::parse("2026-01").unwrap(),
            base_currency: "CNY".into(),
            company: "测试".into(),
            tax_no: String::new(),
            enable_qty: false,
            enable_foreign: false,
            enable_audit: false,
            require_cashier: false,
            biz_accounts: Default::default(),
            budget_control: String::new(),
            doc_prefixes: Default::default(),
            require_audit: false,
            voucher_words: fincore::chart::default_voucher_words(),
        };
        findb::Db::create(&path, &opts).expect("建测试账套")
    }

    /// 空账套：没有余额也没有发生额 → 点倒推等于白点，必须提前拦住
    #[test]
    fn 空账套无可倒推数据() {
        let db = fresh_db("empty");
        let path = db.path().to_path_buf();
        let r = !has_reversible_data(&db);
        drop(db);
        let _ = std::fs::remove_file(&path);
        assert!(
            r,
            "空账套点「按期末余额倒推」应该被前置检查拦住，而不是走确认后静默 n=0"
        );
    }

    /// 有余额的账套 → 可以倒推，不该被拦
    #[test]
    fn 有余额时不拦() {
        use fincore::AuxRef;
        let db = fresh_db("withbal");
        let br = findb::balances::BeginRow {
            id: 0,
            account_code: "1001".to_string(),
            aux: AuxRef::default(),
            year_begin: Money::parse_or_zero("5000"),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        };
        findb::balances::upsert_begin(&db, &br).unwrap();
        let r = has_reversible_data(&db);
        let path = db.path().to_path_buf();
        drop(db);
        let _ = std::fs::remove_file(&path);
        assert!(r, "有余额时点倒推应该放行");
    }
}
