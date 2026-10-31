//! 受托加工的三件事：客供料台账、加工费规则、计件工资
//!
//! 三块数据分别在 `findb::consign` / `findb::processfee` / `findb::piecepay`，
//! 桌面端只做「录入、展示、删除」。数量与金额的算术一律留在 findb：
//! 报价顺序（数量 × 单价 → 最低收费兜底 → 加急费）和计件汇总（按期间、按人）
//! 只要在两处各写一遍，早晚会有一处跟另一处差一分钱，而对客户报价这种事
//! 差一分钱就是要重新谈的。

use chrono::NaiveDate;
use egui::{RichText, Ui};
use fincore::{Money, Period, Perm};
use findb::consign::{self, ConsignBalance, ConsignDoc, ConsignStatementRow};
use findb::piecepay::{self, PiecePay, PieceRate};
use findb::processfee::{self, FeeQuote, PriceRule};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

/// 表格单元格里写一行文字。
///
/// `Ui::label` 返回 `Response`，直接写进 `match` 分支会让「有的分支返回 Response、
/// 有的返回 ()」而编译不过；这里统一丢掉返回值，表格代码才写得干净。
fn txt(ui: &mut Ui, s: impl Into<egui::WidgetText>) {
    ui.label(s);
}

// ===========================================================================
// ① 客供料台账
// ===========================================================================

/// 单据录入时的一行缓冲
#[derive(Clone, Default)]
pub struct LineBuf {
    pub item: String,
    pub item_name: String,
    pub unit: String,
    pub qty: String,
    pub batch_no: String,
}

pub struct ConsignView {
    /// 客户过滤（空 = 全部）
    pub cust: String,
    pub docs: Vec<ConsignDoc>,
    pub bals: Vec<ConsignBalance>,
    /// 展开查看的那张单据（含行）
    pub open: Option<ConsignDoc>,
    pub err: String,
    pub dirty: bool,
    key: String,
    pub new_open: bool,
    pub nb_customer: String,
    pub nb_kind: String,
    pub nb_date: String,
    pub nb_memo: String,
    pub nb_lines: Vec<LineBuf>,
    pub st_open: bool,
    pub st_customer: String,
    pub st_from: String,
    pub st_to: String,
    pub st_rows: Vec<ConsignStatementRow>,
}

impl Default for ConsignView {
    fn default() -> Self {
        Self {
            cust: String::new(),
            docs: Vec::new(),
            bals: Vec::new(),
            open: None,
            err: String::new(),
            dirty: true,
            key: String::new(),
            new_open: false,
            nb_customer: String::new(),
            nb_kind: "in".to_string(),
            nb_date: String::new(),
            nb_memo: String::new(),
            nb_lines: vec![LineBuf::default()],
            st_open: false,
            st_customer: String::new(),
            st_from: String::new(),
            st_to: String::new(),
            st_rows: Vec::new(),
        }
    }
}

impl ConsignView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.nb_date.is_empty() {
            self.nb_date = chrono::Local::now()
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
        }
        let code = ctx.period().code();
        if self.st_from.is_empty() {
            self.st_from = code.clone();
        }
        if self.st_to.is_empty() {
            self.st_to = code;
        }
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let cust = self.cust.trim().to_string();
        let key = format!("consign|{cust}");
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        let filter = if cust.is_empty() {
            None
        } else {
            Some(cust.as_str())
        };
        match consign::doc_list(ctx.db(), filter) {
            Ok(v) => self.docs = v,
            Err(e) => {
                self.docs.clear();
                self.err = e.to_string();
            }
        }
        match consign::balances(ctx.db(), filter) {
            Ok(v) => self.bals = v,
            Err(e) => {
                self.bals.clear();
                self.err = e.to_string();
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "客供料台账", |ui| {
            txt(
                ui,
                RichText::new("客户来料只记数量：不进自己存货、不产生金额、不出凭证").weak(),
            );
        });

        let mut want_new = false;
        let mut want_stmt = false;
        widgets::toolbar(ui, |ui| {
            txt(ui, "客户");
            if widgets::text_input(ui, &mut self.cust, 140.0, "留空 = 全部").changed() {
                self.dirty = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if ui
                .add_enabled(ctx.can(Perm::OrderOps), egui::Button::new("登记单据"))
                .clicked()
            {
                want_new = true;
            }
            if ui.button("客户对账单").clicked() {
                want_stmt = true;
            }
        });
        if want_new {
            self.new_open = true;
        }
        if want_stmt {
            self.st_open = true;
        }

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, self.err.clone());
        }

        // 单据列表
        let mut open_id: Option<i64> = None;
        let mut del_id: Option<i64> = None;
        let docs = self.docs.clone();
        ui.add_space(4.0);
        if docs.is_empty() {
            widgets::empty_hint(ui, "还没有客供料单据。点「登记单据」把客户拉来的货记上。");
        } else {
            egui::ScrollArea::vertical()
                .id_salt("consign_docs")
                .max_height(220.0)
                .show(ui, |ui| {
                    egui::Grid::new("consign_docs_grid")
                        .striped(true)
                        .num_columns(6)
                        .show(ui, |ui| {
                            for h in ["单号", "日期", "客户", "类型", "备注", ""] {
                                txt(ui, RichText::new(h).strong());
                            }
                            ui.end_row();
                            for d in &docs {
                                txt(ui, &d.no);
                                txt(ui, d.date.format("%Y-%m-%d").to_string());
                                txt(ui, &d.customer);
                                txt(ui, d.kind_label());
                                txt(ui, if d.memo.is_empty() { "—" } else { &d.memo });
                                ui.horizontal(|ui| {
                                    if ui.button("明细").clicked() {
                                        open_id = Some(d.id);
                                    }
                                    if ui
                                        .add_enabled(
                                            ctx.can(Perm::OrderOps),
                                            egui::Button::new("删除"),
                                        )
                                        .clicked()
                                    {
                                        del_id = Some(d.id);
                                    }
                                });
                                ui.end_row();
                            }
                        });
                });
        }
        if let Some(id) = open_id {
            self.open = consign::doc_get(ctx.db(), id).ok().flatten();
        }
        if let Some(id) = del_id {
            ctx.confirm(
                "删除客供料单据",
                "删掉之后结存会跟着变；单据不占期间结账，删错了只能重新登记。",
                ConfirmAction::DeleteConsignDoc(id),
            );
        }

        // 展开的单据
        if let Some(d) = self.open.clone() {
            ui.add_space(6.0);
            txt(
                ui,
                RichText::new(format!("{}（{}）", d.no, d.kind_label())).strong(),
            );
            let mut close = false;
            egui::Grid::new("consign_open_grid")
                .striped(true)
                .num_columns(5)
                .show(ui, |ui| {
                    for h in ["物料", "名称", "单位", "数量", "批次"] {
                        txt(ui, RichText::new(h).strong());
                    }
                    ui.end_row();
                    for l in &d.lines {
                        txt(ui, &l.item);
                        txt(
                            ui,
                            if l.item_name.is_empty() {
                                "—"
                            } else {
                                &l.item_name
                            },
                        );
                        txt(ui, if l.unit.is_empty() { "—" } else { &l.unit });
                        txt(ui, l.qty.fmt_qty());
                        txt(
                            ui,
                            if l.batch_no.is_empty() {
                                "—"
                            } else {
                                &l.batch_no
                            },
                        );
                        ui.end_row();
                    }
                });
            if ui.button("收起").clicked() {
                close = true;
            }
            if close {
                self.open = None;
            }
        }

        // 结存
        ui.add_space(8.0);
        txt(ui, RichText::new("结存（客户 + 物料 + 批次）").strong());
        let bals = self.bals.clone();
        if bals.is_empty() {
            widgets::empty_hint(ui, "还没有结存。");
        } else {
            let cols = [
                widgets::TCol::new("客户", 140.0),
                widgets::TCol::new("物料", 120.0),
                widgets::TCol::new("名称", 160.0),
                widgets::TCol::new("批次", 100.0),
                widgets::TCol::new("单位", 70.0),
                widgets::TCol::new("结存数量", 110.0).right(),
            ];
            widgets::grid(ui, "consign_bal", &cols, bals.len(), 22.0, |i, c, ui| {
                let b = &bals[i];
                match c {
                    0 => txt(ui, &b.customer),
                    1 => txt(ui, &b.item),
                    2 => txt(
                        ui,
                        if b.item_name.is_empty() {
                            "—".to_string()
                        } else {
                            b.item_name.clone()
                        },
                    ),
                    3 => txt(
                        ui,
                        if b.batch_no.is_empty() {
                            "—".to_string()
                        } else {
                            b.batch_no.clone()
                        },
                    ),
                    4 => txt(
                        ui,
                        if b.unit.is_empty() {
                            "—".to_string()
                        } else {
                            b.unit.clone()
                        },
                    ),
                    5 => txt(ui, RichText::new(b.qty.fmt_qty()).monospace()),
                    _ => {}
                }
            });
        }

        if self.new_open {
            self.show_new_window(ctx, ui);
        }
        if self.st_open {
            self.show_stmt_window(ctx, ui);
        }
    }

    fn show_new_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let mut open = self.new_open;
        egui::Window::new("登记客供料单据")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(680.0)
            .show(ui.ctx(), |ui| {
                txt(
                    ui,
                    RichText::new(
                        "来料 = 客户把货送来（+）；交货 = 加工完送回（−）；\
                         退料 = 没用完退回去（−）；损耗核销 = 加工中损耗掉（−）。",
                    )
                    .weak(),
                );
                ui.horizontal(|ui| {
                    txt(ui, "客户");
                    widgets::text_input(ui, &mut self.nb_customer, 140.0, "客户编码");
                    txt(ui, "类型");
                    let mut kind = self.nb_kind.clone();
                    egui::ComboBox::from_id_salt("consign_kind")
                        .selected_text(consign::kind_label(&kind).to_string())
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for (k, label) in consign::KINDS {
                                ui.selectable_value(&mut kind, k.to_string(), *label);
                            }
                        });
                    self.nb_kind = kind;
                    txt(ui, "日期");
                    widgets::text_input(ui, &mut self.nb_date, 96.0, "2026-01-31");
                });
                ui.horizontal(|ui| {
                    txt(ui, "备注");
                    widgets::text_input(ui, &mut self.nb_memo, 400.0, "选填");
                });

                ui.add_space(4.0);
                txt(ui, RichText::new("明细").strong());
                let mut del_line: Option<usize> = None;
                egui::Grid::new("consign_new_lines")
                    .num_columns(6)
                    .show(ui, |ui| {
                        for h in ["物料", "名称", "单位", "数量", "批次", ""] {
                            txt(ui, RichText::new(h).strong());
                        }
                        ui.end_row();
                        for (i, l) in self.nb_lines.iter_mut().enumerate() {
                            widgets::text_input(ui, &mut l.item, 110.0, "编码");
                            widgets::text_input(ui, &mut l.item_name, 130.0, "名称");
                            widgets::text_input(ui, &mut l.unit, 56.0, "㎡");
                            widgets::money_input(ui, &mut l.qty, 90.0);
                            widgets::text_input(ui, &mut l.batch_no, 80.0, "选填");
                            if ui.button("删").clicked() {
                                del_line = Some(i);
                            }
                            ui.end_row();
                        }
                    });
                if let Some(i) = del_line {
                    if self.nb_lines.len() > 1 {
                        self.nb_lines.remove(i);
                    } else {
                        self.nb_lines[0] = LineBuf::default();
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("加一行").clicked() {
                        self.nb_lines.push(LineBuf::default());
                    }
                    ui.separator();
                    if ui.button("保存").clicked() {
                        self.save_new(ctx);
                    }
                    if ui.button("取消").clicked() {
                        self.new_open = false;
                    }
                });
                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, self.err.clone());
                }
            });
        self.new_open = open;
    }

    fn save_new(&mut self, ctx: &mut AppCtx<'_>) {
        self.err.clear();
        let date = match NaiveDate::parse_from_str(self.nb_date.trim(), "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => {
                self.err = "日期要写成 2026-01-31 这样".to_string();
                return;
            }
        };
        let mut d = ConsignDoc::new(&self.nb_customer, &self.nb_kind, date);
        d.memo = self.nb_memo.trim().to_string();
        for l in &self.nb_lines {
            let blank = l.item.trim().is_empty()
                && l.qty.trim().is_empty()
                && l.item_name.trim().is_empty();
            if blank {
                continue;
            }
            d.lines.push(consign::ConsignLine {
                id: 0,
                doc_id: 0,
                item: l.item.trim().to_string(),
                item_name: l.item_name.trim().to_string(),
                unit: l.unit.trim().to_string(),
                qty: widgets::parse_money(&l.qty),
                batch_no: l.batch_no.trim().to_string(),
                memo: String::new(),
            });
        }
        let who = ctx.user().display_name.clone();
        match consign::doc_save(ctx.db(), &mut d, &who) {
            Ok(_) => {
                let no = d.no.clone();
                let lines = d.lines.len();
                ctx.log("客供料", "保存单据", &format!("{no} 共 {lines} 行"));
                ctx.info(format!("已保存 {no}"));
                self.new_open = false;
                self.nb_lines = vec![LineBuf::default()];
                self.nb_memo.clear();
                self.err.clear();
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn show_stmt_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let mut open = self.st_open;
        egui::Window::new("客户对账单")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(720.0)
            .show(ui.ctx(), |ui| {
                let mut run = false;
                ui.horizontal(|ui| {
                    txt(ui, "客户");
                    widgets::text_input(ui, &mut self.st_customer, 140.0, "客户编码");
                    txt(ui, "起");
                    widgets::text_input(ui, &mut self.st_from, 84.0, "2026-01");
                    txt(ui, "止");
                    widgets::text_input(ui, &mut self.st_to, 84.0, "2026-01");
                    if ui.button("出表").clicked() {
                        run = true;
                    }
                });
                if run {
                    let from = Period::parse(self.st_from.trim()).unwrap_or_else(|_| ctx.period());
                    let to = Period::parse(self.st_to.trim()).unwrap_or(from);
                    match consign::statement(ctx.db(), self.st_customer.trim(), from, to) {
                        Ok(v) => self.st_rows = v,
                        Err(e) => {
                            self.st_rows.clear();
                            self.err = e.to_string();
                        }
                    }
                }
                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, self.err.clone());
                }
                if self.st_rows.is_empty() {
                    widgets::empty_hint(ui, "填客户和期间，点「出表」。");
                } else {
                    let rows = self.st_rows.clone();
                    let cols = [
                        widgets::TCol::new("物料", 110.0),
                        widgets::TCol::new("名称", 150.0),
                        widgets::TCol::new("批次", 90.0),
                        widgets::TCol::new("期初", 100.0).right(),
                        widgets::TCol::new("来料", 100.0).right(),
                        widgets::TCol::new("交货", 100.0).right(),
                        widgets::TCol::new("退料", 90.0).right(),
                        widgets::TCol::new("损耗", 90.0).right(),
                        widgets::TCol::new("期末", 100.0).right(),
                    ];
                    widgets::grid(ui, "consign_stmt", &cols, rows.len(), 22.0, |i, c, ui| {
                        let r = &rows[i];
                        match c {
                            0 => txt(ui, &r.item),
                            1 => txt(
                                ui,
                                if r.item_name.is_empty() {
                                    "—".to_string()
                                } else {
                                    r.item_name.clone()
                                },
                            ),
                            2 => txt(
                                ui,
                                if r.batch_no.is_empty() {
                                    "—".to_string()
                                } else {
                                    r.batch_no.clone()
                                },
                            ),
                            3 => txt(ui, r.begin.fmt_qty()),
                            4 => txt(ui, r.inbound.fmt_qty()),
                            5 => txt(ui, r.delivered.fmt_qty()),
                            6 => txt(ui, r.returned.fmt_qty()),
                            7 => txt(ui, r.loss.fmt_qty()),
                            8 => txt(ui, RichText::new(r.end.fmt_qty()).monospace().strong()),
                            _ => {}
                        }
                    });
                }
            });
        self.st_open = open;
    }
}

// ===========================================================================
// ② 加工费规则
// ===========================================================================

/// 规则编辑缓冲（金额用文本，保存时才解析）
#[derive(Clone, Default)]
pub struct RuleBuf {
    pub id: i64,
    pub name: String,
    pub item_code: String,
    pub customer_code: String,
    pub unit: String,
    pub unit_price: String,
    pub min_charge: String,
    pub rush_fee: String,
    pub rush_rate: String,
    pub disabled: bool,
    pub memo: String,
}

impl RuleBuf {
    fn from_rule(r: &PriceRule) -> Self {
        Self {
            id: r.id,
            name: r.name.clone(),
            item_code: r.item_code.clone(),
            customer_code: r.customer_code.clone(),
            unit: r.unit.clone(),
            unit_price: r.unit_price.fmt_qty(),
            min_charge: if r.min_charge.is_zero() {
                String::new()
            } else {
                r.min_charge.fmt_qty()
            },
            rush_fee: if r.rush_fee.is_zero() {
                String::new()
            } else {
                r.rush_fee.fmt_qty()
            },
            rush_rate: if r.rush_rate.is_zero() {
                String::new()
            } else {
                r.rush_rate.fmt_qty()
            },
            disabled: r.disabled,
            memo: r.memo.clone(),
        }
    }

    fn to_rule(&self) -> PriceRule {
        PriceRule {
            id: self.id,
            name: self.name.trim().to_string(),
            item_code: self.item_code.trim().to_string(),
            customer_code: self.customer_code.trim().to_string(),
            unit: self.unit.trim().to_string(),
            unit_price: widgets::parse_money(&self.unit_price),
            min_charge: widgets::parse_money(&self.min_charge),
            rush_fee: widgets::parse_money(&self.rush_fee),
            rush_rate: widgets::parse_money(&self.rush_rate),
            disabled: self.disabled,
            memo: self.memo.trim().to_string(),
        }
    }
}

pub struct PriceRulesView {
    pub rules: Vec<PriceRule>,
    pub ed: RuleBuf,
    pub err: String,
    pub dirty: bool,
    key: String,
    /// 报价试算
    pub q_item: String,
    pub q_customer: String,
    pub q_qty: String,
    pub q_rush: bool,
    pub quote: Option<FeeQuote>,
}

impl Default for PriceRulesView {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            ed: RuleBuf {
                unit: "件".to_string(),
                ..Default::default()
            },
            err: String::new(),
            dirty: true,
            key: String::new(),
            q_item: String::new(),
            q_customer: String::new(),
            q_qty: String::new(),
            q_rush: false,
            quote: None,
        }
    }
}

impl PriceRulesView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, _ctx: &mut AppCtx<'_>) {
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.key = String::new();
        match processfee::rule_list(ctx.db()) {
            Ok(v) => self.rules = v,
            Err(e) => {
                self.rules.clear();
                self.err = e.to_string();
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "加工费规则", |ui| {
            txt(
                ui,
                RichText::new("按面积/件/kg 计价；不足最低收费补差；加急另加").weak(),
            );
        });

        let editable = ctx.can(Perm::PriceEdit);
        let mut want_new = false;
        let mut want_del: Option<i64> = None;
        widgets::toolbar(ui, |ui| {
            if ui
                .add_enabled(editable, egui::Button::new("新增规则"))
                .clicked()
            {
                want_new = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });
        if want_new {
            self.ed = RuleBuf {
                unit: "件".to_string(),
                ..Default::default()
            };
            self.err.clear();
        }

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, self.err.clone());
        }

        let rules = self.rules.clone();
        if rules.is_empty() {
            widgets::empty_hint(ui, "还没有加工费规则。一条不建的话，报价时谁都算不出钱。");
        } else {
            let cols = [
                widgets::TCol::new("名称", 140.0),
                widgets::TCol::new("物料", 100.0),
                widgets::TCol::new("客户", 100.0),
                widgets::TCol::new("单位", 60.0),
                widgets::TCol::new("单价", 90.0).right(),
                widgets::TCol::new("最低收费", 90.0).right(),
                widgets::TCol::new("加急", 110.0).right(),
                widgets::TCol::new("状态", 60.0),
                widgets::TCol::new("", 100.0),
            ];
            let mut edit_id: Option<i64> = None;
            widgets::grid(ui, "price_rules", &cols, rules.len(), 22.0, |i, c, ui| {
                let r = &rules[i];
                match c {
                    0 => txt(ui, &r.name),
                    1 => txt(
                        ui,
                        if r.item_code.is_empty() {
                            "任意".to_string()
                        } else {
                            r.item_code.clone()
                        },
                    ),
                    2 => txt(
                        ui,
                        if r.customer_code.is_empty() {
                            "任意".to_string()
                        } else {
                            r.customer_code.clone()
                        },
                    ),
                    3 => txt(
                        ui,
                        if r.unit.is_empty() {
                            "—".to_string()
                        } else {
                            r.unit.clone()
                        },
                    ),
                    4 => txt(ui, r.unit_price.fmt_qty()),
                    5 => txt(
                        ui,
                        if r.min_charge.is_zero() {
                            "—".to_string()
                        } else {
                            r.min_charge.fmt_qty()
                        },
                    ),
                    6 => txt(
                        ui,
                        if r.rush_fee.is_zero() && r.rush_rate.is_zero() {
                            "—".to_string()
                        } else {
                            format!("{}+{}%", r.rush_fee.fmt_qty(), r.rush_rate.fmt_qty())
                        },
                    ),
                    7 => txt(
                        ui,
                        if r.disabled {
                            RichText::new("停用").weak()
                        } else {
                            RichText::new("启用").strong()
                        },
                    ),
                    8 => {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(editable, egui::Button::new("编辑"))
                                .clicked()
                            {
                                edit_id = Some(r.id);
                            }
                            if ui
                                .add_enabled(editable, egui::Button::new("删除"))
                                .clicked()
                            {
                                want_del = Some(r.id);
                            }
                        });
                    }
                    _ => {}
                }
            });
            if let Some(id) = edit_id {
                if let Some(r) = rules.iter().find(|r| r.id == id) {
                    self.ed = RuleBuf::from_rule(r);
                    self.err.clear();
                }
            }
        }
        if let Some(id) = want_del {
            ctx.confirm(
                "删除加工费规则",
                "删掉之后用到这条规则的物料/客户就报不出价了。",
                ConfirmAction::DeletePriceRule(id),
            );
        }

        // 编辑区
        ui.add_space(8.0);
        txt(
            ui,
            RichText::new("规则内容（留空的物料/客户 = 任意）").strong(),
        );
        ui.horizontal(|ui| {
            txt(ui, "名称");
            widgets::text_input(ui, &mut self.ed.name, 140.0, "氧化-普通");
            txt(ui, "物料");
            widgets::text_input(ui, &mut self.ed.item_code, 100.0, "留空=任意");
            txt(ui, "客户");
            widgets::text_input(ui, &mut self.ed.customer_code, 100.0, "留空=任意");
        });
        ui.horizontal(|ui| {
            txt(ui, "单位");
            widgets::text_input(ui, &mut self.ed.unit, 56.0, "㎡");
            txt(ui, "单价");
            widgets::money_input(ui, &mut self.ed.unit_price, 90.0);
            txt(ui, "最低收费");
            widgets::money_input(ui, &mut self.ed.min_charge, 90.0);
            txt(ui, "加急固定");
            widgets::money_input(ui, &mut self.ed.rush_fee, 90.0);
            txt(ui, "加急比例(%)");
            widgets::money_input(ui, &mut self.ed.rush_rate, 70.0);
            ui.checkbox(&mut self.ed.disabled, "停用");
        });
        ui.horizontal(|ui| {
            txt(ui, "备注");
            widgets::text_input(ui, &mut self.ed.memo, 300.0, "选填");
            ui.separator();
            if ui
                .add_enabled(editable, egui::Button::new("保存"))
                .clicked()
                && editable
            {
                self.save(ctx);
            }
        });

        // 报价试算
        ui.add_space(10.0);
        txt(ui, RichText::new("试算报价").strong());
        let mut run = false;
        ui.horizontal(|ui| {
            txt(ui, "物料");
            widgets::text_input(ui, &mut self.q_item, 100.0, "留空=任意");
            txt(ui, "客户");
            widgets::text_input(ui, &mut self.q_customer, 100.0, "留空=任意");
            txt(ui, "数量");
            widgets::money_input(ui, &mut self.q_qty, 90.0);
            ui.checkbox(&mut self.q_rush, "加急");
            if ui.button("算一下").clicked() {
                run = true;
            }
        });
        if run {
            let qty = widgets::parse_money(&self.q_qty);
            match processfee::quote_by(
                ctx.db(),
                self.q_item.trim(),
                self.q_customer.trim(),
                qty,
                self.q_rush,
            ) {
                Ok(q) => {
                    self.quote = Some(q);
                    self.err.clear();
                }
                Err(e) => {
                    self.quote = None;
                    self.err = e.to_string();
                }
            }
        }
        if let Some(q) = self.quote.clone() {
            txt(
                ui,
                format!(
                    "规则「{}」：{} {} × {} = {}",
                    q.rule_name,
                    q.qty.fmt_qty(),
                    q.unit,
                    q.unit_price.fmt_qty(),
                    q.base.fmt_qty()
                ),
            );
            if q.floor_extra.is_positive() {
                txt(
                    ui,
                    format!(
                        "不足最低收费 {}，补差 {}",
                        q.min_charge.fmt_qty(),
                        q.floor_extra.fmt_qty()
                    ),
                );
            }
            if q.rush_extra.is_positive() {
                txt(
                    ui,
                    format!(
                        "加急：固定 {} + 比例 {}%（合计 {}）",
                        q.rush_fee.fmt_qty(),
                        q.rush_rate.fmt_qty(),
                        q.rush_extra.fmt_qty()
                    ),
                );
            }
            txt(
                ui,
                RichText::new(format!("合计 {}", q.total.fmt_money()))
                    .strong()
                    .size(15.0),
            );
        }
    }

    fn save(&mut self, ctx: &mut AppCtx<'_>) {
        self.err.clear();
        let mut r = self.ed.to_rule();
        match processfee::rule_save(ctx.db(), &mut r) {
            Ok(_) => {
                ctx.log("加工费", "保存规则", &format!("规则 {}", r.name));
                ctx.info(format!("已保存规则「{}」", r.name));
                self.ed = RuleBuf::from_rule(&r);
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }
}

// ===========================================================================
// ③ 计件工资
// ===========================================================================

/// 单价编辑缓冲
#[derive(Clone, Default)]
pub struct RateBuf {
    pub id: i64,
    pub item_code: String,
    pub op_name: String,
    pub rate: String,
    pub disabled: bool,
    pub memo: String,
}

impl RateBuf {
    fn from_rate(r: &PieceRate) -> Self {
        Self {
            id: r.id,
            item_code: r.item_code.clone(),
            op_name: r.op_name.clone(),
            rate: r.rate.fmt_qty(),
            disabled: r.disabled,
            memo: r.memo.clone(),
        }
    }

    fn to_rate(&self) -> PieceRate {
        PieceRate {
            id: self.id,
            item_code: self.item_code.trim().to_string(),
            op_name: self.op_name.trim().to_string(),
            rate: widgets::parse_money(&self.rate),
            disabled: self.disabled,
            memo: self.memo.trim().to_string(),
        }
    }
}

pub struct PiecePayView {
    pub period_text: String,
    pub rows: Vec<PiecePay>,
    pub total: Money,
    pub sel: Option<usize>,
    pub rates: Vec<PieceRate>,
    pub ed: RateBuf,
    pub err: String,
    pub dirty: bool,
    key: String,
}

impl Default for PiecePayView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            rows: Vec::new(),
            total: Money::ZERO,
            sel: None,
            rates: Vec::new(),
            ed: RateBuf::default(),
            err: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl PiecePayView {
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
        let key = format!("piece|{}", self.period_text);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        let p = self.period(ctx);
        match piecepay::compute(ctx.db(), p.ymm()) {
            Ok(v) => {
                self.total = v.iter().fold(Money::ZERO, |a, x| a + x.amount);
                self.rows = v;
                if let Some(i) = self.sel {
                    if i >= self.rows.len() {
                        self.sel = None;
                    }
                }
            }
            Err(e) => {
                self.rows.clear();
                self.total = Money::ZERO;
                self.err = e.to_string();
            }
        }
        match piecepay::rate_list(ctx.db()) {
            Ok(v) => self.rates = v,
            Err(e) => {
                self.rates.clear();
                self.err = e.to_string();
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "计件工资", |ui| {
            txt(
                ui,
                RichText::new("工序报工的完工量 × 计件单价，按期间汇总到人").weak(),
            );
        });

        let mut want_fill = false;
        let mut want_new_rate = false;
        let mut want_del_rate: Option<i64> = None;
        let editable = ctx.can(Perm::ProductionOps);
        widgets::toolbar(ui, |ui| {
            txt(ui, "期间");
            if widgets::text_input(ui, &mut self.period_text, 84.0, "2026-11").changed() {
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
            if ui.button("重算").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if ui
                .add_enabled(ctx.can(Perm::VoucherNew), egui::Button::new("回填工资表"))
                .clicked()
            {
                want_fill = true;
            }
        });
        if want_fill {
            self.fill(ctx, p);
        }

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, self.err.clone());
        }

        // 本期计件
        let rows = self.rows.clone();
        if rows.is_empty() {
            widgets::empty_hint(
                ui,
                "本期还没有报工流水。先在生产订单里报工（填完工数量与操作工），这里才会有人。",
            );
        } else {
            let cols = [
                widgets::TCol::new("员工", 140.0),
                widgets::TCol::new("数量", 110.0).right(),
                widgets::TCol::new("计件金额", 120.0).right(),
                widgets::TCol::new("明细", 260.0),
            ];
            let mut sel: Option<usize> = None;
            widgets::grid(ui, "piece_pay", &cols, rows.len(), 22.0, |i, c, ui| {
                let r = &rows[i];
                match c {
                    0 => {
                        if ui
                            .selectable_label(self.sel == Some(i), &r.employee)
                            .clicked()
                        {
                            sel = Some(i);
                        }
                    }
                    1 => txt(ui, r.qty.fmt_qty()),
                    2 => widgets::amount_label(ui, r.amount),
                    3 => {
                        let miss = r.lines.iter().filter(|l| !l.matched).count();
                        let text = if miss == 0 {
                            format!("{} 道工序", r.lines.len())
                        } else {
                            format!("{} 道工序，其中 {} 道无单价", r.lines.len(), miss)
                        };
                        txt(
                            ui,
                            if miss == 0 {
                                RichText::new(text)
                            } else {
                                RichText::new(text).color(palette::WARN)
                            },
                        );
                    }
                    _ => {}
                }
            });
            if sel.is_some() {
                self.sel = sel;
            }
            txt(
                ui,
                RichText::new(format!(
                    "共 {} 人，合计计件 {}",
                    rows.len(),
                    self.total.fmt_money()
                ))
                .weak(),
            );
        }

        // 选中人的明细
        if let Some(i) = self.sel {
            if let Some(r) = self.rows.get(i).cloned() {
                ui.add_space(6.0);
                txt(ui, RichText::new(format!("{} 的明细", r.employee)).strong());
                let cols = [
                    widgets::TCol::new("生产单", 130.0),
                    widgets::TCol::new("物料", 100.0),
                    widgets::TCol::new("工序", 100.0),
                    widgets::TCol::new("数量", 90.0).right(),
                    widgets::TCol::new("单价", 80.0).right(),
                    widgets::TCol::new("金额", 100.0).right(),
                ];
                let lines = r.lines.clone();
                widgets::grid(ui, "piece_lines", &cols, lines.len(), 22.0, |i, c, ui| {
                    let l = &lines[i];
                    match c {
                        0 => txt(ui, &l.po_no),
                        1 => txt(
                            ui,
                            if l.item_name.is_empty() {
                                l.item_code.clone()
                            } else {
                                format!("{} {}", l.item_code, l.item_name)
                            },
                        ),
                        2 => txt(ui, &l.op_name),
                        3 => txt(ui, l.qty.fmt_qty()),
                        4 => txt(
                            ui,
                            if l.matched {
                                l.rate.fmt_qty()
                            } else {
                                "（无单价）".to_string()
                            },
                        ),
                        5 => widgets::amount_label(ui, l.amount),
                        _ => {}
                    }
                });
            }
        }

        // 计件单价维护
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            txt(ui, RichText::new("计件单价").strong());
            if ui
                .add_enabled(editable, egui::Button::new("新增单价"))
                .clicked()
            {
                want_new_rate = true;
            }
        });
        if want_new_rate {
            self.ed = RateBuf::default();
            self.err.clear();
        }
        let rates = self.rates.clone();
        if rates.is_empty() {
            widgets::empty_hint(
                ui,
                "还没有计件单价。没有单价的行会显示「无单价」，金额记 0。",
            );
        } else {
            let cols = [
                widgets::TCol::new("物料", 110.0),
                widgets::TCol::new("工序", 110.0),
                widgets::TCol::new("单价", 90.0).right(),
                widgets::TCol::new("状态", 60.0),
                widgets::TCol::new("备注", 180.0),
                widgets::TCol::new("", 100.0),
            ];
            let mut edit_id: Option<i64> = None;
            widgets::grid(ui, "piece_rates", &cols, rates.len(), 22.0, |i, c, ui| {
                let r = &rates[i];
                match c {
                    0 => txt(
                        ui,
                        if r.item_code.is_empty() {
                            "任意".to_string()
                        } else {
                            r.item_code.clone()
                        },
                    ),
                    1 => txt(
                        ui,
                        if r.op_name.is_empty() {
                            "任意".to_string()
                        } else {
                            r.op_name.clone()
                        },
                    ),
                    2 => txt(ui, r.rate.fmt_qty()),
                    3 => txt(
                        ui,
                        if r.disabled {
                            RichText::new("停用").weak()
                        } else {
                            RichText::new("启用").strong()
                        },
                    ),
                    4 => txt(
                        ui,
                        if r.memo.is_empty() {
                            "—".to_string()
                        } else {
                            r.memo.clone()
                        },
                    ),
                    5 => {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(editable, egui::Button::new("编辑"))
                                .clicked()
                            {
                                edit_id = Some(r.id);
                            }
                            if ui
                                .add_enabled(editable, egui::Button::new("删除"))
                                .clicked()
                            {
                                want_del_rate = Some(r.id);
                            }
                        });
                    }
                    _ => {}
                }
            });
            if let Some(id) = edit_id {
                if let Some(r) = rates.iter().find(|r| r.id == id) {
                    self.ed = RateBuf::from_rate(r);
                    self.err.clear();
                }
            }
        }
        if let Some(id) = want_del_rate {
            ctx.confirm(
                "删除计件单价",
                "删掉之后这段时间的报工会变成「无单价」，金额记 0。",
                ConfirmAction::DeletePieceRate(id),
            );
        }
        ui.horizontal(|ui| {
            txt(ui, "物料");
            widgets::text_input(ui, &mut self.ed.item_code, 100.0, "留空=任意");
            txt(ui, "工序");
            widgets::text_input(ui, &mut self.ed.op_name, 100.0, "留空=任意");
            txt(ui, "单价");
            widgets::money_input(ui, &mut self.ed.rate, 90.0);
            ui.checkbox(&mut self.ed.disabled, "停用");
            if ui
                .add_enabled(editable, egui::Button::new("保存"))
                .clicked()
            {
                self.save_rate(ctx);
            }
        });
        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, self.err.clone());
        }
    }

    fn save_rate(&mut self, ctx: &mut AppCtx<'_>) {
        self.err.clear();
        let mut r = self.ed.to_rate();
        match piecepay::rate_save(ctx.db(), &mut r) {
            Ok(_) => {
                ctx.log(
                    "工资",
                    "保存计件单价",
                    &format!("{} {} @ {}", r.item_code, r.op_name, r.rate),
                );
                ctx.info("已保存计件单价");
                self.ed = RateBuf::from_rate(&r);
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn fill(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        self.err.clear();
        let rows = match piecepay::compute(ctx.db(), p.ymm()) {
            Ok(v) => v,
            Err(e) => {
                self.err = e.to_string();
                return;
            }
        };
        let (mut filled, mut skipped) = (0usize, 0usize);
        let who = ctx.user().display_name.clone();
        for r in &rows {
            if !r.amount.is_positive() {
                continue;
            }
            // 部门数据范围：看不见的人不该被这双手改写工资表
            let dept = findb::business::payroll_get(ctx.db(), p, &r.employee)
                .ok()
                .flatten()
                .map(|o| o.dept)
                .unwrap_or_default();
            if !ctx.user().data_scope.allows_dept(&dept) {
                skipped += 1;
                continue;
            }
            match piecepay::fill_one(ctx.db(), p, r) {
                Ok(Some(f)) => {
                    filled += 1;
                    ctx.log(
                        "工资",
                        "计件回填",
                        &format!(
                            "{} {} 应发 {} → {}",
                            p.label(),
                            f.employee,
                            f.before,
                            f.after
                        ),
                    );
                }
                Ok(None) => skipped += 1,
                Err(e) => {
                    self.err = e.to_string();
                    return;
                }
            }
        }
        if filled == 0 {
            ctx.info(format!("没有需要回填的人（跳过 {skipped}）"));
        } else {
            ctx.info(format!("已回填 {filled} 人，跳过 {skipped} 人"));
        }
        let _ = who;
        self.dirty = true;
    }
}
