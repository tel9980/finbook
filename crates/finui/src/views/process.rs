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
use findb::fixture::{self, ToolBalance, ToolItem, ToolMove};
use findb::piecepay::{self, PiecePay, PieceRate};
use findb::processfee::{self, FeeQuote, PriceRule};
use findb::rework;
use findb::scm;

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
    /// 挂的销售订单（0 = 不挂）
    pub nb_so: i64,
    /// 当期销售订单（下拉用）
    pub orders: Vec<findb::scm::SalesOrder>,
    /// 交货进度（按客户）
    pub progress: Vec<consign::SoProgress>,
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
            nb_so: 0,
            orders: Vec::new(),
            progress: Vec::new(),
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
        self.progress = if cust.is_empty() {
            Vec::new()
        } else {
            consign::so_progress(ctx.db(), &cust).unwrap_or_default()
        };
        self.orders = findb::scm::so_list(ctx.db(), ctx.period(), None).unwrap_or_default();
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
                        .num_columns(7)
                        .show(ui, |ui| {
                            for h in ["单号", "日期", "客户", "类型", "销售订单", "备注", ""]
                            {
                                txt(ui, RichText::new(h).strong());
                            }
                            ui.end_row();
                            for d in &docs {
                                txt(ui, &d.no);
                                txt(ui, d.date.format("%Y-%m-%d").to_string());
                                txt(ui, &d.customer);
                                txt(ui, d.kind_label());
                                txt(ui, if d.so_no.is_empty() { "—" } else { &d.so_no });
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

        // 交货进度（填了客户才查）
        if !self.progress.is_empty() {
            ui.add_space(8.0);
            txt(ui, RichText::new("销售订单交货进度").strong());
            let cols = [
                widgets::TCol::new("订单号", 150.0),
                widgets::TCol::new("物料", 120.0),
                widgets::TCol::new("名称", 150.0),
                widgets::TCol::new("订购", 100.0).right(),
                widgets::TCol::new("已交", 100.0).right(),
                widgets::TCol::new("未交", 100.0).right(),
            ];
            let rows = self.progress.clone();
            widgets::grid(
                ui,
                "consign_so_progress",
                &cols,
                rows.len(),
                22.0,
                |i, c, ui| {
                    let p = &rows[i];
                    match c {
                        0 => txt(ui, &p.so_no),
                        1 => txt(ui, &p.item),
                        2 => txt(
                            ui,
                            if p.item_name.is_empty() {
                                "—".to_string()
                            } else {
                                p.item_name.clone()
                            },
                        ),
                        3 => txt(ui, RichText::new(p.ordered.fmt_qty()).monospace()),
                        4 => txt(ui, RichText::new(p.delivered.fmt_qty()).monospace()),
                        5 => {
                            if p.remain.is_positive() {
                                ui.colored_label(
                                    palette::CREDIT,
                                    RichText::new(p.remain.fmt_qty()).monospace(),
                                );
                            } else {
                                txt(ui, "—");
                            }
                        }
                        _ => {}
                    }
                },
            );
            txt(
                ui,
                RichText::new("已交只算交货单；退料 / 损耗不抵成品交付")
                    .weak()
                    .small(),
            );
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
                    txt(ui, "销售订单");
                    let sel = self
                        .orders
                        .iter()
                        .find(|o| o.id == self.nb_so)
                        .map(|o| format!("{} · {}", o.no, o.customer_code))
                        .unwrap_or_else(|| "不挂订单".to_string());
                    let mut pick = self.nb_so;
                    egui::ComboBox::from_id_salt("consign_so")
                        .selected_text(sel)
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut pick, 0, "不挂订单");
                            for o in &self.orders {
                                if !self.nb_customer.trim().is_empty()
                                    && o.customer_code.trim() != self.nb_customer.trim()
                                {
                                    continue;
                                }
                                ui.selectable_value(
                                    &mut pick,
                                    o.id,
                                    format!("{} · {}", o.no, o.customer_code),
                                );
                            }
                        });
                    self.nb_so = pick;
                    txt(
                        ui,
                        RichText::new("挂了订单，交货进度能看每单还差多少")
                            .weak()
                            .small(),
                    );
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
        d.so_id = self.nb_so;
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
    /// 生效日期 `YYYY-MM-DD`（空 = 立即）。网页端「单价矩阵」里提前录的下一版价，
    /// 在这里编辑时必须原样带回来 —— 丢了就等于把「什么时候调价」删了。
    pub effective_from: String,
    /// 工艺档（「硬质氧化」这类表面处理分档；空 = 不分档）。
    /// 网页端按档录入的价，在这里编辑时要原样带回去，否则一编辑就把档位抹平了。
    pub process: String,
    /// 规格档：按哪一维比（max/min/first/area，空 = 不分档）
    pub band_dim: String,
    /// 区间下限（厘米，空 = 不限）
    pub band_from: String,
    /// 区间上限（厘米，空 = 不限，含端点）
    pub band_to: String,
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
            effective_from: r.effective_from.clone(),
            process: r.process.clone(),
            band_dim: r.band_dim.clone(),
            band_from: r.band_from.clone(),
            band_to: r.band_to.clone(),
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
            effective_from: self.effective_from.trim().to_string(),
            process: self.process.trim().to_string(),
            band_dim: self.band_dim.trim().to_string(),
            band_from: self.band_from.trim().to_string(),
            band_to: self.band_to.trim().to_string(),
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
    /// 规格（毫米）：`1250*250*3`。没录折算系数的工件靠它算折合量
    pub q_spec: String,
    /// 计价口径（空 = 老算法：数量就是计价数量）
    pub q_unit: String,
    /// 密度（只有按 kg 算才用；钢 7.85）
    pub q_density: String,
    /// 工艺档（空 = 不分档）
    pub q_proc: String,
    pub q_rush: bool,
    pub quote: Option<FeeQuote>,
    /// 折算过程的说明（口径为空时为 None）
    pub conv_text: Option<String>,
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
            q_spec: String::new(),
            q_unit: String::new(),
            q_density: "7.85".to_string(),
            q_proc: String::new(),
            q_rush: false,
            quote: None,
            conv_text: None,
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
                widgets::TCol::new("工艺", 90.0),
                widgets::TCol::new("规格段", 110.0),
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
                        if r.process.trim().is_empty() {
                            "—".to_string()
                        } else {
                            r.process.clone()
                        },
                    ),
                    4 => {
                        // 「≤200cm」「>200cm」「200–300cm」—— 单位厘米，客户嘴里的「200 以下」
                        let dim = processfee::band_dim_label(&r.band_dim);
                        let span = if r.band_dim.trim().is_empty() {
                            "—".to_string()
                        } else if r.band_from.trim().is_empty() {
                            format!("≤ {}", r.band_to.trim())
                        } else if r.band_to.trim().is_empty() {
                            format!("> {}", r.band_from.trim())
                        } else {
                            format!("{}–{}", r.band_from.trim(), r.band_to.trim())
                        };
                        txt(
                            ui,
                            if r.band_dim.trim().is_empty() {
                                span
                            } else {
                                format!("{dim} {span}cm")
                            },
                        )
                    }
                    6 => txt(
                        ui,
                        if r.unit.is_empty() {
                            "—".to_string()
                        } else {
                            r.unit.clone()
                        },
                    ),
                    7 => txt(
                        ui,
                        if r.effective_from.trim().is_empty() {
                            r.unit_price.fmt_qty()
                        } else {
                            format!(
                                "{}（{} 起）",
                                r.unit_price.fmt_qty(),
                                r.effective_from.trim()
                            )
                        },
                    ),
                    8 => txt(
                        ui,
                        if r.min_charge.is_zero() {
                            "—".to_string()
                        } else {
                            r.min_charge.fmt_qty()
                        },
                    ),
                    9 => txt(
                        ui,
                        if r.rush_fee.is_zero() && r.rush_rate.is_zero() {
                            "—".to_string()
                        } else {
                            format!("{}+{}%", r.rush_fee.fmt_qty(), r.rush_rate.fmt_qty())
                        },
                    ),
                    10 => txt(
                        ui,
                        if r.disabled {
                            RichText::new("停用").weak()
                        } else {
                            RichText::new("启用").strong()
                        },
                    ),
                    11 => {
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
            txt(ui, "工艺");
            widgets::text_input(ui, &mut self.ed.process, 100.0, "留空=不分档");
        });
        ui.horizontal(|ui| {
            txt(ui, "规格档");
            egui::ComboBox::from_id_salt("rule_band_dim")
                .width(96.0)
                .selected_text(if self.ed.band_dim.trim().is_empty() {
                    "不分档".to_string()
                } else {
                    processfee::band_dim_label(&self.ed.band_dim).to_string()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.ed.band_dim, String::new(), "不分档");
                    for (k, label) in [
                        ("max", "最长边"),
                        ("min", "最短边"),
                        ("first", "第一维"),
                        ("area", "面积"),
                    ] {
                        ui.selectable_value(&mut self.ed.band_dim, k.to_string(), label);
                    }
                });
            txt(ui, "下限(cm)");
            widgets::text_input(ui, &mut self.ed.band_from, 64.0, "留空=不限");
            txt(ui, "上限(cm)");
            widgets::text_input(ui, &mut self.ed.band_to, 64.0, "留空=不限");
            txt(
                ui,
                RichText::new("含端点：填 200 表示 200cm 及以下")
                    .size(11.0)
                    .color(palette::GRID),
            );
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
            widgets::text_input(ui, &mut self.ed.memo, 240.0, "选填");
            txt(ui, "生效日");
            widgets::text_input(ui, &mut self.ed.effective_from, 100.0, "留空=立即");
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
        });
        ui.horizontal(|ui| {
            txt(ui, "规格");
            widgets::text_input(ui, &mut self.q_spec, 120.0, "1250*250*3（毫米）");
            txt(ui, "口径");
            widgets::text_input(ui, &mut self.q_unit, 60.0, "㎡ / 米 / kg");
            txt(ui, "密度");
            widgets::text_input(ui, &mut self.q_density, 50.0, "7.85");
            txt(ui, "工艺");
            widgets::text_input(ui, &mut self.q_proc, 90.0, "硬质氧化（可空）");
            if ui.button("算一下").clicked() {
                run = true;
            }
        });
        txt(
            ui,
            RichText::new(
                "口径留空的按老算法（数量就是计价数量）。填口径 + 规格的，没录折算系数也能算 —— \
                 1250×250×3 的板一块 0.3125 ㎡；密度只有按 kg 算才用（钢 7.85、铝 2.70）。",
            )
            .size(11.0)
            .color(palette::GRID),
        );
        if run {
            let qty = widgets::parse_money(&self.q_qty);
            let unit = self.q_unit.trim().to_string();
            let proc = self.q_proc.trim().to_string();
            let res = if unit.is_empty() && !self.q_spec.trim().is_empty() {
                // 填了规格但没选口径：让「按规格分段的价」自己定口径 ——
                // 「200cm 以下按件 0.2、以上按米 1.5」这种价，口径本来就是档位决定的。
                let density = widgets::parse_money(&self.q_density);
                processfee::quote_with_spec(
                    ctx.db(),
                    self.q_item.trim(),
                    self.q_customer.trim(),
                    &self.q_spec,
                    qty,
                    "",
                    density,
                    &proc,
                    self.q_rush,
                )
                .map(|sq| (sq.quote.clone(), Some(sq)))
            } else if unit.is_empty() {
                // 没填口径也没填规格：老算法（数量就是计价数量）。填了工艺档就必须按档位匹配 ——
                // quote_by 不认档位，会把「硬质氧化 18 元」报成普通档的 12 元。
                if proc.is_empty() {
                    processfee::quote_by(
                        ctx.db(),
                        self.q_item.trim(),
                        self.q_customer.trim(),
                        qty,
                        self.q_rush,
                    )
                    .map(|q| (q, None))
                } else {
                    processfee::match_rule_for_process(
                        ctx.db(),
                        self.q_item.trim(),
                        self.q_customer.trim(),
                        "",
                        &proc,
                    )
                    .and_then(|hit| match hit {
                        Some((rule, _)) => Ok((
                            processfee::quote_on(&rule, qty, self.q_rush, &processfee::today()),
                            None,
                        )),
                        None => Err(fincore::FinError::msg(format!(
                            "没有「{proc}」这一档的单价 —— 先在单价表里给它单开一格"
                        ))
                        .into()),
                    })
                }
            } else {
                let density = widgets::parse_money(&self.q_density);
                processfee::quote_with_spec(
                    ctx.db(),
                    self.q_item.trim(),
                    self.q_customer.trim(),
                    &self.q_spec,
                    qty,
                    &unit,
                    density,
                    &proc,
                    self.q_rush,
                )
                .map(|sq| (sq.quote.clone(), Some(sq)))
            };
            match res {
                Ok((q, conv)) => {
                    self.quote = Some(q);
                    self.conv_text = conv.map(|sq| {
                        let basis = if sq.basis == "spec" {
                            format!("按规格 {}）", sq.dims)
                        } else {
                            "按折算系数）".to_string()
                        };
                        format!(
                            "折算（{}：{} {} × {} = {} {}",
                            basis,
                            sq.stock_qty.fmt_qty(),
                            sq.stock_unit,
                            sq.factor.fmt_qty(),
                            sq.calc_qty.fmt_qty(),
                            sq.calc_unit
                        )
                    });
                    self.err.clear();
                }
                Err(e) => {
                    self.quote = None;
                    self.conv_text = None;
                    self.err = e.to_string();
                }
            }
        }
        if let Some(cv) = self.conv_text.clone() {
            if self.quote.is_some() {
                txt(ui, RichText::new(cv).size(11.5).color(palette::GRID));
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
    pub version: String,
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
            version: r.version.clone(),
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
            version: self.version.trim().to_string(),
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
                    widgets::TCol::new("版本", 60.0),
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
                        3 => txt(
                            ui,
                            if l.version.is_empty() {
                                "—".to_string()
                            } else {
                                l.version.clone()
                            },
                        ),
                        4 => txt(ui, l.qty.fmt_qty()),
                        5 => txt(
                            ui,
                            if l.matched {
                                l.rate.fmt_qty()
                            } else {
                                "（无单价）".to_string()
                            },
                        ),
                        6 => widgets::amount_label(ui, l.amount),
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
                widgets::TCol::new("版本", 60.0),
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
                        if r.version.is_empty() {
                            "不限".to_string()
                        } else {
                            r.version.clone()
                        },
                    ),
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
                        if r.op_name.is_empty() {
                            "任意".to_string()
                        } else {
                            r.op_name.clone()
                        },
                    ),
                    3 => txt(ui, r.rate.fmt_qty()),
                    4 => txt(
                        ui,
                        if r.disabled {
                            RichText::new("停用").weak()
                        } else {
                            RichText::new("启用").strong()
                        },
                    ),
                    5 => txt(
                        ui,
                        if r.memo.is_empty() {
                            "—".to_string()
                        } else {
                            r.memo.clone()
                        },
                    ),
                    6 => {
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
            txt(ui, "版本");
            widgets::text_input(ui, &mut self.ed.version, 80.0, "留空=不限");
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
                    &format!(
                        "{} {} {} @ {}",
                        if r.version.is_empty() {
                            "不限版本"
                        } else {
                            &r.version
                        },
                        r.item_code,
                        r.op_name,
                        r.rate
                    ),
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

// ===========================================================================
// ④ 挂具台账
// ===========================================================================

pub struct ToolLedgerView {
    /// 挂具编号过滤（空 = 全部）
    pub filter: String,
    pub items: Vec<ToolItem>,
    pub rows: Vec<ToolBalance>,
    pub moves: Vec<ToolMove>,
    pub err: String,
    pub dirty: bool,
    key: String,
    // 新增档案
    pub item_open: bool,
    pub ni_code: String,
    pub ni_name: String,
    pub ni_kind: String,
    pub ni_spec: String,
    pub ni_unit: String,
    pub ni_life: String,
    pub ni_memo: String,
    // 登记收发
    pub move_open: bool,
    pub nm_code: String,
    pub nm_kind: String,
    pub nm_date: String,
    pub nm_cust: String,
    pub nm_qty: String,
    pub nm_times: String,
    pub nm_memo: String,
}

impl Default for ToolLedgerView {
    fn default() -> Self {
        Self {
            filter: String::new(),
            items: Vec::new(),
            rows: Vec::new(),
            moves: Vec::new(),
            err: String::new(),
            dirty: true,
            key: String::new(),
            item_open: false,
            ni_code: String::new(),
            ni_name: String::new(),
            ni_kind: "挂具".to_string(),
            ni_spec: String::new(),
            ni_unit: "套".to_string(),
            ni_life: "0".to_string(),
            ni_memo: String::new(),
            move_open: false,
            nm_code: String::new(),
            nm_kind: "issue".to_string(),
            nm_date: String::new(),
            nm_cust: String::new(),
            nm_qty: String::new(),
            nm_times: String::new(),
            nm_memo: String::new(),
        }
    }
}

impl ToolLedgerView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, _ctx: &mut AppCtx<'_>) {
        if self.nm_date.is_empty() {
            self.nm_date = chrono::Local::now()
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
        }
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let filter = self.filter.trim().to_string();
        let key = format!("tool|{filter}");
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        match fixture::item_list(ctx.db()) {
            Ok(v) => self.items = v,
            Err(e) => {
                self.items.clear();
                self.err = e.to_string();
            }
        }
        match fixture::balances(ctx.db()) {
            Ok(v) => self.rows = v,
            Err(e) => {
                self.rows.clear();
                self.err = e.to_string();
            }
        }
        let f = if filter.is_empty() {
            None
        } else {
            Some(filter.as_str())
        };
        match fixture::moves(ctx.db(), f, None) {
            Ok(v) => self.moves = v,
            Err(e) => {
                self.moves.clear();
                self.err = e.to_string();
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "挂具台账", |ui| {
            txt(
                ui,
                RichText::new("挂具只记数量与使用次数：不进存货、不出凭证；报废只从在库扣").weak(),
            );
        });

        let mut want_item = false;
        let mut want_move = false;
        let over = self.rows.iter().filter(|b| b.needs_replace()).count();
        widgets::toolbar(ui, |ui| {
            txt(ui, "挂具");
            if widgets::text_input(ui, &mut self.filter, 130.0, "编号，留空 = 全部").changed()
            {
                self.dirty = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if ui
                .add_enabled(ctx.can(Perm::ProductionOps), egui::Button::new("登记收发"))
                .clicked()
            {
                want_move = true;
            }
            if ui
                .add_enabled(ctx.can(Perm::ProductionOps), egui::Button::new("新增档案"))
                .clicked()
            {
                want_item = true;
            }
            if over > 0 {
                ui.separator();
                txt(
                    ui,
                    RichText::new(format!("{over} 个挂具该换新了")).color(palette::WARN),
                );
            }
        });
        if want_move {
            if self.items.is_empty() {
                txt(ui, RichText::new("先建挂具档案").color(palette::WARN));
            } else {
                self.nm_code = self.items[0].code.clone();
                self.move_open = true;
            }
        }
        if want_item {
            self.item_open = true;
        }
        if !self.err.is_empty() {
            let msg = self.err.clone();
            ui.colored_label(palette::CREDIT, msg);
        }

        // ---------------- 结存 ----------------
        let cols = [
            widgets::TCol::new("挂具", 90.0),
            widgets::TCol::new("名称", 130.0),
            widgets::TCol::new("规格", 90.0),
            widgets::TCol::new("单位", 50.0).fixed(),
            widgets::TCol::new("在库", 70.0).right(),
            widgets::TCol::new("在车间", 70.0).right(),
            widgets::TCol::new("累计报废", 80.0).right(),
            widgets::TCol::new("使用次数", 80.0).right(),
            widgets::TCol::new("额定", 60.0).right(),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("", 60.0).fixed(),
        ];
        let rows = self.rows.clone();
        if rows.is_empty() {
            widgets::empty_hint(ui, "还没有挂具档案，先「新增档案」");
        }
        let mut del: Option<String> = None;
        widgets::grid(ui, "tool_bal", &cols, rows.len(), 24.0, |i, c, ui| {
            let b = &rows[i];
            match c {
                0 => txt(ui, RichText::new(&b.tool_code).monospace()),
                1 => txt(ui, &b.name),
                2 => txt(ui, &b.spec),
                3 => txt(ui, &b.unit),
                4 => widgets::amount_label(ui, b.in_stock),
                5 => widgets::amount_label(ui, b.in_use),
                6 => widgets::amount_label(ui, b.scraped),
                7 => txt(ui, (b.used_times).to_string()),
                8 => txt(
                    ui,
                    if b.life_times > 0 {
                        b.life_times.to_string()
                    } else {
                        "不限".to_string()
                    },
                ),
                9 => {
                    if b.disabled {
                        txt(ui, RichText::new("停用").weak());
                    } else if b.needs_replace() {
                        txt(ui, RichText::new("该换新").color(palette::WARN));
                    } else {
                        txt(ui, "");
                    }
                }
                10 => {
                    let hit = ui
                        .add_enabled(
                            ctx.can(Perm::ProductionOps),
                            egui::Button::new("删档").small(),
                        )
                        .clicked();
                    if hit {
                        del = Some(b.tool_code.clone());
                    }
                }
                _ => {}
            }
        });
        if let Some(code) = del {
            let msg = format!("确定删除挂具 {code} 的档案？有收发记录的会被拦下（停用更安全）。");
            ctx.confirm("删除挂具档案", &msg, ConfirmAction::DeleteToolItem(code));
        }

        // ---------------- 流水 ----------------
        ui.separator();
        txt(ui, RichText::new("收发流水").strong());
        let mcols = [
            widgets::TCol::new("单号", 130.0),
            widgets::TCol::new("日期", 90.0),
            widgets::TCol::new("类型", 60.0).fixed(),
            widgets::TCol::new("挂具", 90.0),
            widgets::TCol::new("客户", 90.0),
            widgets::TCol::new("数量", 80.0).right(),
            widgets::TCol::new("次数", 60.0).right(),
            widgets::TCol::new("备注", 140.0),
            widgets::TCol::new("", 60.0).fixed(),
        ];
        let mvs = self.moves.clone();
        if mvs.is_empty() {
            widgets::empty_hint(ui, "还没有收发记录");
        }
        let mut mdel: Option<i64> = None;
        widgets::grid(ui, "tool_move", &mcols, mvs.len(), 24.0, |i, c, ui| {
            let m = &mvs[i];
            match c {
                0 => txt(ui, RichText::new(&m.no).monospace()),
                1 => txt(ui, m.date.format("%Y-%m-%d").to_string()),
                2 => txt(ui, m.kind_label()),
                3 => txt(ui, &m.tool_code),
                4 => txt(
                    ui,
                    if m.customer.is_empty() {
                        "—".to_string()
                    } else {
                        m.customer.clone()
                    },
                ),
                5 => widgets::amount_label(ui, m.qty),
                6 => txt(ui, m.times.to_string()),
                7 => txt(ui, &m.memo),
                8 => {
                    let hit = ui
                        .add_enabled(
                            ctx.can(Perm::ProductionOps),
                            egui::Button::new("删").small(),
                        )
                        .clicked();
                    if hit {
                        mdel = Some(m.id);
                    }
                }
                _ => {}
            }
        });
        if let Some(id) = mdel {
            ctx.confirm(
                "删除收发记录",
                "确定删除这条收发记录？结存会跟着重算。",
                ConfirmAction::DeleteToolMove(id),
            );
        }

        self.show_item_window(ctx, ui);
        self.show_move_window(ctx, ui);
    }

    fn show_item_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if !self.item_open {
            return;
        }
        let mut open = self.item_open;
        egui::Window::new("新增挂具档案")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                egui::Grid::new("tool_item_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        txt(ui, "编号 *");
                        widgets::text_input(ui, &mut self.ni_code, 140.0, "如 GJ-001");
                        ui.end_row();
                        txt(ui, "名称");
                        widgets::text_input(ui, &mut self.ni_name, 200.0, "");
                        ui.end_row();
                        txt(ui, "类型");
                        widgets::text_input(ui, &mut self.ni_kind, 120.0, "挂具/挂篮/夹具");
                        ui.end_row();
                        txt(ui, "规格");
                        widgets::text_input(ui, &mut self.ni_spec, 140.0, "如 600×400");
                        ui.end_row();
                        txt(ui, "单位");
                        widgets::text_input(ui, &mut self.ni_unit, 80.0, "套");
                        ui.end_row();
                        txt(ui, "额定次数");
                        widgets::text_input(ui, &mut self.ni_life, 80.0, "0 = 不限");
                        ui.end_row();
                        txt(ui, "备注");
                        widgets::text_input(ui, &mut self.ni_memo, 200.0, "");
                        ui.end_row();
                    });
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        self.save_item(ctx);
                    }
                    if ui.button("关闭").clicked() {
                        self.item_open = false;
                    }
                });
            });
        if !open {
            self.item_open = false;
        }
    }

    fn save_item(&mut self, ctx: &mut AppCtx<'_>) {
        let mut it = ToolItem {
            code: self.ni_code.trim().to_string(),
            name: self.ni_name.trim().to_string(),
            tool_kind: self.ni_kind.trim().to_string(),
            spec: self.ni_spec.trim().to_string(),
            unit: self.ni_unit.trim().to_string(),
            life_times: self.ni_life.trim().parse::<i64>().unwrap_or(0),
            disabled: false,
            memo: self.ni_memo.trim().to_string(),
            ..Default::default()
        };
        match fixture::item_save(ctx.db(), &mut it) {
            Ok(_) => {
                ctx.log("挂具", "保存档案", &it.code);
                ctx.info("已保存挂具档案");
                self.item_open = false;
                self.ni_code.clear();
                self.ni_name.clear();
                self.ni_spec.clear();
                self.ni_memo.clear();
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn show_move_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if !self.move_open {
            return;
        }
        let mut open = self.move_open;
        let codes: Vec<String> = self.items.iter().map(|i| i.code.clone()).collect();
        egui::Window::new("登记挂具收发")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                egui::Grid::new("tool_move_grid")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        txt(ui, "挂具 *");
                        egui::ComboBox::from_id_salt("tool_mv_code")
                            .selected_text(self.nm_code.clone())
                            .show_ui(ui, |ui| {
                                for c in &codes {
                                    ui.selectable_value(&mut self.nm_code, c.clone(), c);
                                }
                            });
                        ui.end_row();
                        txt(ui, "类型 *");
                        egui::ComboBox::from_id_salt("tool_mv_kind")
                            .selected_text(fixture::kind_label(&self.nm_kind).to_string())
                            .show_ui(ui, |ui| {
                                for (k, l) in fixture::KINDS {
                                    ui.selectable_value(&mut self.nm_kind, (*k).to_string(), *l);
                                }
                            });
                        ui.end_row();
                        txt(ui, "日期 *");
                        widgets::text_input(ui, &mut self.nm_date, 100.0, "YYYY-MM-DD");
                        ui.end_row();
                        txt(ui, "客户");
                        widgets::text_input(ui, &mut self.nm_cust, 120.0, "专用挂具才填");
                        ui.end_row();
                        txt(ui, "数量 *");
                        widgets::money_input(ui, &mut self.nm_qty, 100.0);
                        ui.end_row();
                        txt(ui, "使用次数");
                        widgets::text_input(ui, &mut self.nm_times, 80.0, "归还留空按套数");
                        ui.end_row();
                        txt(ui, "备注");
                        widgets::text_input(ui, &mut self.nm_memo, 200.0, "");
                        ui.end_row();
                    });
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        self.save_move(ctx);
                    }
                    if ui.button("关闭").clicked() {
                        self.move_open = false;
                    }
                });
            });
        if !open {
            self.move_open = false;
        }
    }

    fn save_move(&mut self, ctx: &mut AppCtx<'_>) {
        let date = match NaiveDate::parse_from_str(self.nm_date.trim(), "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => {
                self.err = "日期格式应为 YYYY-MM-DD".to_string();
                return;
            }
        };
        let mut m = ToolMove::new(self.nm_code.trim(), self.nm_kind.trim(), date);
        m.customer = self.nm_cust.trim().to_string();
        m.qty = widgets::parse_money(&self.nm_qty);
        m.times = self.nm_times.trim().parse::<i64>().unwrap_or(0);
        m.memo = self.nm_memo.trim().to_string();
        let who = ctx.user().display_name.clone();
        match fixture::move_save(ctx.db(), &mut m, &who) {
            Ok(_) => {
                ctx.log("挂具", "收发", &format!("{} {}", m.no, m.tool_code));
                ctx.info(format!("已保存 {}", m.no));
                self.move_open = false;
                self.nm_qty.clear();
                self.nm_times.clear();
                self.nm_memo.clear();
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }
}

// ===========================================================================
// 返工成本单：返工单独归集，看「这批返工亏了多少」
// ===========================================================================
pub struct ReworkView {
    pub period_text: String,
    /// 报表行（区间内每张返工单：三段成本 + 占原单比例）
    pub rows: Vec<rework::ReworkStat>,
    /// 选中的返工单
    pub sel: Option<i64>,
    pub costs: Vec<rework::ReworkCost>,
    pub err: String,
    pub dirty: bool,
    key: String,
    // 新建
    pub new_open: bool,
    pub nb_po: i64,
    pub nb_qty: String,
    pub nb_date: String,
    pub nb_reason: String,
    /// 原因分类编码（空 = 未分类）
    pub nb_code: String,
    /// 责任部门 / 班组
    pub nb_dept: String,
    pub nb_memo: String,
    /// 原因分类字典 + 本期按原因/按部门汇总
    pub reasons: Vec<rework::Reason>,
    pub analysis: Option<rework::Analysis>,
    // 追加成本
    pub nb_kind: String,
    pub nb_amount: String,
    pub nb_cost_memo: String,
}

impl Default for ReworkView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            rows: Vec::new(),
            sel: None,
            costs: Vec::new(),
            err: String::new(),
            dirty: true,
            key: String::new(),
            new_open: false,
            nb_po: 0,
            nb_qty: String::new(),
            nb_date: String::new(),
            nb_reason: String::new(),
            nb_code: String::new(),
            nb_dept: String::new(),
            nb_memo: String::new(),
            reasons: Vec::new(),
            analysis: None,
            nb_kind: "material".to_string(),
            nb_amount: String::new(),
            nb_cost_memo: String::new(),
        }
    }
}

impl ReworkView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        if self.nb_date.is_empty() {
            self.nb_date = chrono::Local::now()
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
        }
        self.dirty = true;
    }

    fn period(&self, ctx: &AppCtx<'_>) -> fincore::Period {
        fincore::Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let key = format!("rework|{}", p.code());
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        match rework::report(ctx.db(), p, p) {
            Ok(v) => self.rows = v,
            Err(e) => {
                self.rows.clear();
                self.err = e.to_string();
            }
        }
        match rework::reason_list(ctx.db()) {
            Ok(v) => self.reasons = v,
            Err(e) => self.err = e.to_string(),
        }
        match rework::analyze(ctx.db(), p, p) {
            Ok(a) => self.analysis = Some(a),
            Err(e) => {
                self.analysis = None;
                self.err = e.to_string();
            }
        }
        if let Some(id) = self.sel {
            match rework::costs(ctx.db(), id) {
                Ok(v) => self.costs = v,
                Err(e) => self.err = e.to_string(),
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "返工成本单", |ui| {
            txt(
                ui,
                RichText::new("返工的材料/工时单独归集，不进原单 —— 原单单位成本才不虚高").weak(),
            );
        });

        let mut want_new = false;
        widgets::toolbar(ui, |ui| {
            txt(ui, "期间");
            if widgets::text_input(ui, &mut self.period_text, 110.0, "2026-01").changed() {
                self.dirty = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            if ui
                .add_enabled(
                    ctx.can(Perm::ProductionOps),
                    egui::Button::new("新建返工单"),
                )
                .clicked()
            {
                want_new = true;
            }
        });
        if want_new {
            self.new_open = true;
        }

        if !self.err.is_empty() {
            txt(ui, RichText::new(&self.err).color(palette::CREDIT));
        }

        let cols = [
            widgets::TCol::new("单号", 110.0),
            widgets::TCol::new("日期", 90.0),
            widgets::TCol::new("来源订单", 120.0),
            widgets::TCol::new("物料", 150.0),
            widgets::TCol::new("返工数量", 80.0).right(),
            widgets::TCol::new("分类", 80.0),
            widgets::TCol::new("原因", 140.0),
            widgets::TCol::new("部门", 90.0),
            widgets::TCol::new("状态", 70.0),
            widgets::TCol::new("材料", 90.0).right(),
            widgets::TCol::new("人工", 90.0).right(),
            widgets::TCol::new("制造费用", 90.0).right(),
            widgets::TCol::new("合计", 100.0).right(),
            widgets::TCol::new("占原单", 80.0).right(),
        ];
        let rows = self.rows.clone();
        let mut pick: Option<i64> = None;
        widgets::grid(ui, "rework_grid", &cols, rows.len(), 22.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => {
                    if ui
                        .selectable_label(self.sel == Some(r.rw_id), &r.no)
                        .clicked()
                    {
                        pick = Some(r.rw_id);
                    }
                }
                1 => txt(ui, &r.date),
                2 => txt(ui, &r.po_no),
                3 => txt(ui, format!("{} {}", r.item_code, r.item_name)),
                4 => widgets::amount_label(ui, r.qty),
                5 => txt(ui, &r.reason_label),
                6 => txt(ui, &r.reason),
                7 => txt(ui, if r.dept.is_empty() { "—" } else { &r.dept }),
                8 => txt(ui, rework::status_label(&r.status)),
                9 => widgets::amount_label(ui, r.material),
                10 => widgets::amount_label(ui, r.labor),
                11 => widgets::amount_label(ui, r.overhead),
                12 => widgets::amount_label(ui, r.total),
                13 => {
                    if r.ratio.is_empty() {
                        txt(ui, "—")
                    } else {
                        txt(ui, format!("{}%", r.ratio))
                    }
                }
                _ => {}
            }
        });
        // 按原因 / 按部门汇总：哪种原因最费钱、哪个班组返工最多
        if let Some(a) = self.analysis.clone() {
            ui.separator();
            txt(
                ui,
                RichText::new(format!(
                    "返工成本分析：本期 {} 张，合计 {}",
                    a.count,
                    a.total.fmt_money()
                ))
                .strong(),
            );
            for (title, list) in [("按原因", &a.by_reason), ("按责任部门", &a.by_dept)] {
                txt(ui, RichText::new(title).weak());
                let cols = [
                    widgets::TCol::new("分组", 120.0),
                    widgets::TCol::new("张数", 60.0).right(),
                    widgets::TCol::new("返工件数", 90.0).right(),
                    widgets::TCol::new("材料", 90.0).right(),
                    widgets::TCol::new("人工", 90.0).right(),
                    widgets::TCol::new("制造费用", 90.0).right(),
                    widgets::TCol::new("合计", 100.0).right(),
                    widgets::TCol::new("占比", 70.0).right(),
                ];
                let list = list.clone();
                let id = format!("rework_an_{title}");
                widgets::grid(ui, &id, &cols, list.len(), 22.0, |i, c, ui| {
                    let b = &list[i];
                    match c {
                        0 => txt(ui, &b.label),
                        1 => txt(ui, b.count.to_string()),
                        2 => widgets::amount_label(ui, b.qty),
                        3 => widgets::amount_label(ui, b.material),
                        4 => widgets::amount_label(ui, b.labor),
                        5 => widgets::amount_label(ui, b.overhead),
                        6 => widgets::amount_label(ui, b.total),
                        _ => {
                            if b.share.is_empty() {
                                txt(ui, "—")
                            } else {
                                txt(ui, format!("{}%", b.share))
                            }
                        }
                    }
                });
            }
        }

        if let Some(id) = pick {
            self.sel = Some(id);
            match rework::costs(ctx.db(), id) {
                Ok(v) => self.costs = v,
                Err(e) => self.err = e.to_string(),
            }
        }

        // 选中一张单：成本明细 + 追加/删除
        let Some(id) = self.sel else {
            return;
        };
        let closed = self
            .rows
            .iter()
            .find(|r| r.rw_id == id)
            .map(|r| r.status == "closed")
            .unwrap_or(false);
        ui.separator();
        let title = self
            .rows
            .iter()
            .find(|r| r.rw_id == id)
            .map(|r| format!("{} 的成本明细", r.no))
            .unwrap_or_else(|| "成本明细".to_string());
        txt(ui, RichText::new(title).strong());
        if closed {
            txt(
                ui,
                RichText::new("已结案：这批返工的钱已经认下来了，要改先把状态改回「进行中」")
                    .color(palette::WARN),
            );
        }

        let ccols = [
            widgets::TCol::new("成本项", 110.0),
            widgets::TCol::new("金额", 110.0).right(),
            widgets::TCol::new("备注", 240.0),
        ];
        let costs = self.costs.clone();
        let mut del_cost: Option<i64> = None;
        widgets::grid(
            ui,
            "rework_cost_grid",
            &ccols,
            costs.len(),
            22.0,
            |i, c, ui| {
                let x = &costs[i];
                match c {
                    0 => txt(ui, x.kind_label()),
                    1 => widgets::amount_label(ui, x.amount),
                    2 => {
                        ui.horizontal(|ui| {
                            txt(ui, &x.memo);
                            if !closed
                                && ui
                                    .add_enabled(
                                        ctx.can(Perm::ProductionOps),
                                        egui::Button::new("删").small(),
                                    )
                                    .clicked()
                            {
                                del_cost = Some(x.id);
                            }
                        });
                    }
                    _ => {}
                }
            },
        );
        if let Some(cid) = del_cost {
            match rework::delete_cost(ctx.db(), cid) {
                Ok(()) => {
                    ctx.log("生产", "返工成本", &format!("删除 {cid}"));
                    self.dirty = true;
                    self.invalidate();
                }
                Err(e) => self.err = e.to_string(),
            }
        }

        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("rework_cost_kind")
                .width(120.0)
                .selected_text(rework::cost_kind_label(&self.nb_kind))
                .show_ui(ui, |ui| {
                    for (k, l) in rework::COST_KINDS {
                        ui.selectable_value(&mut self.nb_kind, (*k).to_string(), *l);
                    }
                });
            widgets::money_input(ui, &mut self.nb_amount, 110.0);
            widgets::text_input(ui, &mut self.nb_cost_memo, 200.0, "备注");
            if ui
                .add_enabled(
                    ctx.can(Perm::ProductionOps) && !closed,
                    egui::Button::new("追加成本"),
                )
                .clicked()
            {
                let amount = widgets::parse_money(&self.nb_amount);
                if !amount.is_positive() {
                    self.err = "金额必须大于 0".to_string();
                } else {
                    match rework::add_cost(
                        ctx.db(),
                        id,
                        &self.nb_kind,
                        amount,
                        self.nb_cost_memo.trim(),
                    ) {
                        Ok(_) => {
                            ctx.log(
                                "生产",
                                "返工成本",
                                &format!(
                                    "{id} {} {}",
                                    rework::cost_kind_label(&self.nb_kind),
                                    amount
                                ),
                            );
                            self.nb_amount.clear();
                            self.nb_cost_memo.clear();
                            self.dirty = true;
                            self.invalidate();
                        }
                        Err(e) => self.err = e.to_string(),
                    }
                }
            }
            ui.separator();
            if ui
                .add_enabled(
                    ctx.can(Perm::ProductionOps),
                    egui::Button::new(if closed { "改回进行中" } else { "结案" }),
                )
                .clicked()
            {
                if let Some(mut o) = rework::get(ctx.db(), id).ok().flatten() {
                    o.status = if closed {
                        "open".into()
                    } else {
                        "closed".into()
                    };
                    match rework::save(ctx.db(), &mut o, &ctx.user().username) {
                        Ok(_) => {
                            ctx.log("生产", "返工单", &format!("{} {}", o.no, o.status_label()));
                            self.dirty = true;
                            self.invalidate();
                        }
                        Err(e) => self.err = e.to_string(),
                    }
                }
            }
            if ui
                .add_enabled(
                    ctx.can(Perm::ProductionOps),
                    egui::Button::new("删除此单").small(),
                )
                .clicked()
            {
                ctx.confirm(
                    "删除返工单",
                    "成本明细会一起删掉，确定？",
                    ConfirmAction::DeleteRework(id),
                );
            }
        });

        if self.new_open {
            self.show_new_window(ctx, ui);
        }
    }

    fn show_new_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let mut open = self.new_open;
        let mut pending: Option<(i64, String, String, String, String, String, String)> = None;
        egui::Window::new("新建返工单")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                let orders = scm::prod_list(ctx.db(), ctx.period(), None).unwrap_or_default();
                if orders.is_empty() {
                    txt(
                        ui,
                        RichText::new("本期还没有生产订单：先下生产订单再登记返工")
                            .color(palette::WARN),
                    );
                    return;
                }
                if self.nb_po == 0 {
                    self.nb_po = orders[0].id;
                }
                let cur = orders
                    .iter()
                    .find(|o| o.id == self.nb_po)
                    .map(|o| format!("{} {} ×{}", o.no, o.item_code, o.planned_qty))
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    txt(ui, "来源生产订单");
                    egui::ComboBox::from_id_salt("rework_po")
                        .width(240.0)
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            for o in &orders {
                                ui.selectable_value(
                                    &mut self.nb_po,
                                    o.id,
                                    format!("{} {} ×{}", o.no, o.item_code, o.planned_qty),
                                );
                            }
                        });
                });
                ui.horizontal(|ui| {
                    txt(ui, "返工数量");
                    widgets::money_input(ui, &mut self.nb_qty, 110.0);
                });
                ui.horizontal(|ui| {
                    txt(ui, "日期");
                    widgets::text_input(ui, &mut self.nb_date, 110.0, "2026-01-20");
                });
                ui.horizontal(|ui| {
                    txt(ui, "原因分类");
                    let cur = self
                        .reasons
                        .iter()
                        .find(|r| r.code == self.nb_code)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| rework::UNCLASSIFIED.to_string());
                    egui::ComboBox::from_id_salt("rework_reason_code")
                        .width(120.0)
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.nb_code,
                                String::new(),
                                rework::UNCLASSIFIED,
                            );
                            let list = self.reasons.clone();
                            for r in list.iter().filter(|r| !r.disabled) {
                                ui.selectable_value(&mut self.nb_code, r.code.clone(), &r.name);
                            }
                        });
                    txt(ui, "责任部门");
                    widgets::text_input(ui, &mut self.nb_dept, 120.0, "氧化一班");
                });
                ui.horizontal(|ui| {
                    txt(ui, "原因备注");
                    widgets::text_input(ui, &mut self.nb_reason, 220.0, "挂具印痕 / 膜厚不足");
                });
                ui.horizontal(|ui| {
                    txt(ui, "备注");
                    widgets::text_input(ui, &mut self.nb_memo, 240.0, "");
                });
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        pending = Some((
                            self.nb_po,
                            self.nb_qty.clone(),
                            self.nb_date.clone(),
                            self.nb_reason.clone(),
                            self.nb_code.clone(),
                            self.nb_dept.clone(),
                            self.nb_memo.clone(),
                        ));
                    }
                    if ui.button("取消").clicked() {
                        self.new_open = false;
                    }
                });
            });
        self.new_open = open;
        let Some((po_id, qty, date, reason, code, dept, memo)) = pending else {
            return;
        };
        let date = match NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => {
                self.err = "日期格式应为 YYYY-MM-DD".to_string();
                return;
            }
        };
        let mut o = rework::ReworkOrder::new(po_id, date);
        o.qty = widgets::parse_money(&qty);
        o.reason = reason;
        o.reason_code = code.trim().to_uppercase();
        o.dept = dept.trim().to_string();
        o.memo = memo;
        match rework::save(ctx.db(), &mut o, &ctx.user().username) {
            Ok(id) => {
                ctx.log(
                    "生产",
                    "返工单",
                    &format!("{} 来源 {} 数量 {}", o.no, o.po_no, o.qty),
                );
                self.sel = Some(id);
                self.new_open = false;
                self.nb_qty.clear();
                self.nb_reason.clear();
                self.nb_dept.clear();
                self.nb_memo.clear();
                self.dirty = true;
                self.invalidate();
            }
            Err(e) => self.err = e.to_string(),
        }
    }
}
