//! 存货核算：出入库单 / 收发存汇总 / 结转成本
//!
//! 存货与凭证模块最大的区别是**金额不都由用户录入**：入库按单据单价入账，
//! 出库成本由计价方式（移动加权平均 / FIFO）在 `fincore::engine::costing` 里算。
//! 界面只负责把流水喂进去、把算出的成本展示出来，并在期末一键生成结转凭证。

use chrono::NaiveDate;
use egui::{Align2, RichText, Ui};
use findb::business::{self, StockKind, StockMove, StockSummary};
use fincore::engine::costing::CostMethod;
use fincore::money::QTY_DP;
use fincore::{Money, Period, Perm};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets::{self, Paging};

/// 存货过滤下拉里表示"不过滤"的选项
const ALL_ITEMS: &str = "全部";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Moves,
    Summary,
    Cost,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Moves => "出入库单",
            Tab::Summary => "收发存汇总",
            Tab::Cost => "结转成本",
        }
    }
}

/// 出入库单编辑缓冲区（金额类字段一律用 String，保存时才解析成 Money）
#[derive(Clone)]
pub struct MoveDraft {
    id: i64,
    date: String,
    kind: StockKind,
    item: String,
    warehouse: String,
    batch_no: String,
    qty: String,
    price: String,
    memo: String,
}

pub struct InventoryView {
    pub tab: Tab,
    pub period_text: String,
    pub item: String,
    pub items: Vec<String>,
    pub moves: Vec<StockMove>,
    pub summary: Vec<StockSummary>,
    pub method: CostMethod,
    /// 本期发出成本合计（结转成本页签用）
    pub out_total: Money,
    pub paging: Paging,
    pub editing: Option<MoveDraft>,
    pub editing_new: bool,
    /// 待二次确认的删除（流水 id + 提示文字）
    pub pending_delete: Option<(i64, String)>,
    pub cost_date: String,
    pub cost_account: String,
    pub asset_account: String,
    pub err: String,
    pub dirty: bool,
    key: String,
}

impl Default for InventoryView {
    fn default() -> Self {
        Self {
            tab: Tab::Moves,
            period_text: String::new(),
            item: String::new(),
            items: Vec::new(),
            moves: Vec::new(),
            summary: Vec::new(),
            method: CostMethod::default(),
            out_total: Money::ZERO,
            paging: Paging::default(),
            editing: None,
            editing_new: false,
            pending_delete: None,
            cost_date: String::new(),
            cost_account: "6401".to_string(),
            asset_account: "1405".to_string(),
            err: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl InventoryView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        if self.item.is_empty() {
            self.item = ALL_ITEMS.to_string();
        }
        self.dirty = true;
    }

    fn period(&self, ctx: &AppCtx<'_>) -> Period {
        Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period())
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        // 缓存 key 覆盖期间 + 页签 + 存货过滤 + 计价方式，任一变化才重新取数
        let key = format!(
            "{}|{}|{}|{}",
            p.ymm(),
            self.tab.name(),
            self.item,
            self.method.code()
        );
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        self.items = business::stock_items(ctx.db()).unwrap_or_default();
        self.moves = business::stock_list(ctx.db(), p).unwrap_or_default();
        if self.item != ALL_ITEMS {
            self.moves.retain(|m| m.item == self.item);
        }

        self.summary.clear();
        self.out_total = Money::ZERO;
        // stock_summary 会把算出的出库成本回写进流水，只在真正需要它的页签调用，
        // 不能每帧都跑（否则界面刷一次就写一次库）
        if matches!(self.tab, Tab::Summary | Tab::Cost) {
            match business::stock_summary(ctx.db(), p, self.method) {
                Ok(s) => {
                    self.out_total = s.iter().map(|x| x.out_amount).sum();
                    self.summary = s;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
        self.paging.reset();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "存货核算", |ui| {
            ui.label(RichText::new(format!("计价方式：{}", self.method.label())).weak());
        });

        widgets::toolbar(ui, |ui| {
            for t in [Tab::Moves, Tab::Summary, Tab::Cost] {
                ui.selectable_value(&mut self.tab, t, t.name());
            }
            ui.separator();
            ui.label("期间");
            let r = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
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
        });

        match self.tab {
            Tab::Moves => self.show_moves(ctx, ui, p),
            Tab::Summary => self.show_summary(ui),
            Tab::Cost => self.show_cost(ctx, ui, p),
        }

        // 弹窗要在主体渲染之后再画，避免抢走同一帧的输入焦点
        self.move_window(ctx, ui, p);
        self.delete_window(ctx, ui);
    }

    // ------------------------- 出入库单 -------------------------
    fn show_moves(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        widgets::toolbar(ui, |ui| {
            ui.label("存货");
            let mut opts = vec![ALL_ITEMS.to_string()];
            opts.extend(self.items.iter().cloned());
            if widgets::combo(ui, "inv_item_filter", &mut self.item, &opts, 160.0).changed() {
                self.dirty = true;
            }
            ui.separator();
            if ui.button("新增出入库单").clicked() && ctx.can(Perm::AccountEdit) {
                self.open_new(p);
            }
        });

        let shown: Vec<StockMove> = self.paging.slice(&self.moves).to_vec();
        let cols = [
            widgets::TCol::new("日期", 96.0).fixed(),
            widgets::TCol::new("单据类型", 90.0).fixed(),
            widgets::TCol::new("存货", 170.0),
            widgets::TCol::new("仓库", 100.0),
            widgets::TCol::new("数量", 100.0).right(),
            widgets::TCol::new("单价", 100.0).right(),
            widgets::TCol::new("金额", 120.0).right(),
            widgets::TCol::new("凭证号", 80.0).fixed(),
            widgets::TCol::new("操作", 100.0).fixed(),
        ];
        let mut edit: Option<i64> = None;
        let mut del: Option<i64> = None;
        widgets::grid(ui, "inv_moves", &cols, shown.len(), 24.0, |i, c, ui| {
            let m = &shown[i];
            match c {
                0 => {
                    ui.label(m.biz_date.format("%Y-%m-%d").to_string());
                }
                1 => {
                    ui.label(
                        RichText::new(m.kind.label()).color(if m.kind.is_inbound() {
                            palette::OK
                        } else {
                            palette::CREDIT
                        }),
                    );
                }
                2 => {
                    ui.label(&m.item);
                }
                3 => {
                    ui.label(&m.warehouse);
                }
                4 => {
                    ui.label(m.qty.fmt_qty());
                }
                5 => {
                    ui.label(if m.price.is_zero() {
                        String::new()
                    } else {
                        m.price.fmt_qty()
                    });
                }
                6 => widgets::amount_label(ui, m.amount),
                7 => {
                    ui.label(match m.voucher_id {
                        Some(v) => RichText::new(format!("{v}")).monospace(),
                        None => RichText::new("—").weak(),
                    });
                }
                8 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改").clicked() {
                            edit = Some(m.id);
                        }
                        if ui.small_button("删").clicked() {
                            del = Some(m.id);
                        }
                    });
                }
                _ => {}
            }
        });
        if let Some(id) = edit {
            if let Some(m) = self.moves.iter().find(|x| x.id == id).cloned() {
                self.open_edit(&m);
            }
        }
        if let Some(id) = del {
            if let Some(m) = self.moves.iter().find(|x| x.id == id) {
                // 没有合适的 ConfirmAction 变体（DeleteVoucher 是删凭证不是删单据），
                // 这里自己开一个二次确认窗口，不新增枚举
                self.pending_delete = Some((
                    id,
                    format!(
                        "{} {} {} {}，删除后不可恢复。",
                        m.biz_date.format("%Y-%m-%d"),
                        m.kind.label(),
                        m.item,
                        m.qty.fmt_qty()
                    ),
                ));
            }
        }

        let in_amt: Money = self
            .moves
            .iter()
            .filter(|m| m.qty.is_positive())
            .map(|m| m.amount)
            .sum();
        let out_qty: Money = self
            .moves
            .iter()
            .filter(|m| m.qty.is_negative())
            .map(|m| m.qty)
            .sum();
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(format!(
                "共 {} 条；入库金额合计 {}；出库数量合计 {}",
                self.moves.len(),
                in_amt.fmt_money(),
                out_qty.fmt_qty()
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.paging.bar(ui, self.moves.len());
            });
        });
    }

    fn open_new(&mut self, p: Period) {
        let date = p
            .contains(chrono::Local::now().date_naive())
            .then(|| chrono::Local::now().date_naive())
            .unwrap_or_else(|| p.last_day());
        self.editing = Some(MoveDraft {
            id: 0,
            date: date.format("%Y-%m-%d").to_string(),
            kind: StockKind::Purchase,
            item: if self.item == ALL_ITEMS {
                String::new()
            } else {
                self.item.clone()
            },
            warehouse: String::new(),
            batch_no: String::new(),
            qty: String::new(),
            price: String::new(),
            memo: String::new(),
        });
        self.editing_new = true;
        self.err.clear();
    }

    fn open_edit(&mut self, m: &StockMove) {
        self.editing = Some(MoveDraft {
            id: m.id,
            date: m.biz_date.format("%Y-%m-%d").to_string(),
            kind: m.kind,
            item: m.item.clone(),
            warehouse: m.warehouse.clone(),
            batch_no: m.batch_no.clone(),
            qty: m.qty.abs().fmt_qty(),
            price: if m.price.is_zero() {
                String::new()
            } else {
                m.price.fmt_qty()
            },
            memo: m.memo.clone(),
        });
        self.editing_new = false;
        self.err.clear();
    }

    /// 编辑缓冲区 → 流水。数量按 `QTY_DP` 取整，符号由单据类型决定
    fn build_move(&self, d: &MoveDraft) -> Result<StockMove, String> {
        let date = NaiveDate::parse_from_str(d.date.trim(), "%Y-%m-%d")
            .map_err(|_| format!("日期格式不正确：{}（应为 2026-01-31）", d.date))?;
        if d.item.trim().is_empty() {
            return Err("请填写存货名称".to_string());
        }
        let qty = Money::parse_or_zero(&d.qty).round_dp(QTY_DP);
        if qty.is_zero() {
            return Err("数量不能为零".to_string());
        }
        let price = Money::parse_or_zero(&d.price);
        if price.is_negative() {
            return Err("单价不能为负数".to_string());
        }
        // 出库单价可以留空：留给计价引擎按移动加权平均 / FIFO 算
        if d.kind.is_inbound() && price.is_zero() {
            return Err(format!("{}必须填写单价", d.kind.label()));
        }
        let amount = if price.is_zero() {
            Money::ZERO
        } else {
            (qty * price).round2()
        };
        Ok(StockMove {
            id: d.id,
            period: Period::from_date(date),
            biz_date: date,
            kind: d.kind,
            item: d.item.trim().to_string(),
            warehouse: d.warehouse.trim().to_string(),
            batch_no: d.batch_no.trim().to_string(),
            qty: if d.kind.is_inbound() { qty } else { -qty },
            price,
            amount,
            voucher_id: None,
            memo: d.memo.trim().to_string(),
        })
    }

    fn move_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        let Some(d) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;
        let items = self.items.clone();
        let err = self.err.clone();
        let kind_label = d.kind.label().to_string();
        // 金额随数量/单价实时算出来给用户看，避免保存后才发现录错
        let preview = {
            let qty = Money::parse_or_zero(&d.qty);
            let price = Money::parse_or_zero(&d.price);
            if price.is_zero() {
                "（出库单价留空时由系统按计价方式计算）".to_string()
            } else {
                format!("金额 {}（{} × {}）", (qty * price).round2().fmt_money(), qty.fmt_qty(), price.fmt_qty())
            }
        };

        egui::Window::new(if is_new { "新增出入库单" } else { "修改出入库单" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if !err.is_empty() {
                    ui.colored_label(palette::CREDIT, &err);
                }
                egui::Grid::new("inv_move_edit")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("日期：");
                        ui.add_sized([120.0, 22.0], egui::TextEdit::singleline(&mut d.date));
                        ui.end_row();
                        ui.label("单据类型：");
                        egui::ComboBox::from_id_salt("inv_move_kind")
                            .selected_text(&kind_label)
                            .width(140.0)
                            .show_ui(ui, |ui| {
                                for k in StockKind::ALL {
                                    ui.selectable_value(&mut d.kind, *k, k.label());
                                }
                            });
                        ui.end_row();
                        ui.label("存货：");
                        ui.horizontal(|ui| {
                            widgets::text_input(ui, &mut d.item, 180.0, "存货名称");
                            // 已有项目做补全下拉，避免同一存货录出多个名字
                            widgets::combo(ui, "inv_move_item", &mut d.item, &items, 100.0);
                        });
                        ui.end_row();
                        ui.label("仓库：");
                        widgets::text_input(ui, &mut d.warehouse, 180.0, "可留空");
                        ui.end_row();
                        ui.label("批次：");
                        widgets::text_input(ui, &mut d.batch_no, 120.0, "可留空");
                        ui.end_row();
                        ui.label("数量：");
                        widgets::money_input(ui, &mut d.qty, 120.0);
                        ui.end_row();
                        ui.label("单价：");
                        widgets::money_input(ui, &mut d.price, 120.0);
                        ui.end_row();
                        ui.label("备注：");
                        widgets::text_input(ui, &mut d.memo, 260.0, "可留空");
                        ui.end_row();
                    });
                ui.label(RichText::new(preview).weak());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui.button("保存").clicked() {
                            save = true;
                        }
                    });
                });
            });

        if close || !open {
            self.editing = None;
            self.err.clear();
            return;
        }
        if !save {
            return;
        }

        let d = self.editing.clone().unwrap();
        self.err.clear();
        match self.build_move(&d) {
            Ok(mut m) => {
                let old_id = d.id;
                let r = if self.editing_new {
                    business::stock_insert(ctx.db(), &m).map(|_| ())
                } else {
                    // 数据层没有通用 update（只有改金额的 stock_update_amount），
                    // 所以修改走"先插新行、再删旧行"，顺序反过来一旦插入失败就会丢单据
                    m.voucher_id = self
                        .moves
                        .iter()
                        .find(|x| x.id == old_id)
                        .and_then(|x| x.voucher_id);
                    business::stock_insert(ctx.db(), &m)
                        .and_then(|_| business::stock_delete(ctx.db(), old_id))
                };
                match r {
                    Ok(()) => {
                        ctx.log(
                            "存货",
                            if self.editing_new { "新增出入库单" } else { "修改出入库单" },
                            &format!("{} {} {}", m.biz_date.format("%Y-%m-%d"), m.kind.label(), m.item),
                        );
                        ctx.info("已保存");
                        // 单据日期可能落在别的期间，提示一声免得用户以为没存上
                        if m.period != p {
                            ctx.info(format!("单据已按日期归入 {}，不在当前查看期间", m.period.label()));
                        }
                        self.dirty = true;
                        self.editing = None;
                    }
                    Err(e) => self.err = e.to_string(),
                }
            }
            Err(e) => self.err = e,
        }
    }

    fn delete_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some((id, msg)) = self.pending_delete.clone() else {
            return;
        };
        let mut open = true;
        let mut ok = false;
        let mut cancel = false;
        egui::Window::new("删除出入库单")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(RichText::new(&msg).color(palette::CREDIT));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                        let b = egui::Button::new(RichText::new("确定删除").color(egui::Color32::WHITE))
                            .fill(palette::CREDIT);
                        if ui.add(b).clicked() {
                            ok = true;
                        }
                    });
                });
            });
        if !open {
            self.pending_delete = None;
            return;
        }
        if ok {
            match business::stock_delete(ctx.db(), id) {
                Ok(()) => {
                    ctx.log("存货", "删除出入库单", &format!("#{id}"));
                    ctx.info("已删除");
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
            self.pending_delete = None;
        }
    }

    // ------------------------- 收发存汇总 -------------------------
    fn show_summary(&mut self, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            ui.label("计价方式");
            let old = self.method;
            for m in CostMethod::ALL {
                ui.selectable_value(&mut self.method, *m, m.label());
            }
            if self.method != old {
                self.dirty = true;
            }
        });

        let rows = self.summary.clone();
        let cols = [
            widgets::TCol::new("存货", 180.0),
            widgets::TCol::new("收入数量", 100.0).right(),
            widgets::TCol::new("收入金额", 120.0).right(),
            widgets::TCol::new("发出数量", 100.0).right(),
            widgets::TCol::new("发出金额", 120.0).right(),
            widgets::TCol::new("结存数量", 100.0).right(),
            widgets::TCol::new("结存金额", 120.0).right(),
            widgets::TCol::new("单位成本", 100.0).right(),
        ];
        widgets::grid(ui, "inv_summary", &cols, rows.len(), 24.0, |i, c, ui| {
            let s = &rows[i];
            match c {
                0 => {
                    ui.label(&s.item);
                }
                1 => {
                    ui.label(s.in_qty.fmt_qty());
                }
                2 => widgets::amount_label(ui, s.in_amount),
                3 => {
                    ui.label(s.out_qty.fmt_qty());
                }
                4 => widgets::amount_label(ui, s.out_amount),
                5 => {
                    ui.label(s.end_qty.fmt_qty());
                }
                6 => widgets::amount_label(ui, s.end_amount),
                7 => {
                    ui.label(s.unit_cost.fmt_qty());
                }
                _ => {}
            }
        });

        if rows.is_empty() {
            return;
        }
        let ti: Money = rows.iter().map(|s| s.in_amount).sum();
        let to: Money = rows.iter().map(|s| s.out_amount).sum();
        let te: Money = rows.iter().map(|s| s.end_amount).sum();
        ui.separator();
        ui.label(RichText::new(format!(
            "合计：收入 {} ／ 发出 {} ／ 结存 {}",
            ti.fmt_money(),
            to.fmt_money(),
            te.fmt_money()
        )).strong());
        ui.label(
            RichText::new(
                "说明：切换计价方式后出库成本会按新方式重算，并把单价与金额回写到本期出库流水。",
            )
            .weak(),
        );
    }

    // ------------------------- 结转成本 -------------------------
    fn show_cost(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, p: Period) {
        if self.cost_date.is_empty() {
            self.cost_date = p.last_day().format("%Y-%m-%d").to_string();
        }
        ui.label(
            RichText::new("按当前计价方式计算本期发出成本并生成凭证：借 成本科目 / 贷 存货科目。")
                .weak(),
        );
        ui.add_space(6.0);

        widgets::card(ui, "结转成本", |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("本期发出成本合计").strong());
                ui.label(
                    RichText::new(self.out_total.fmt_money())
                        .strong()
                        .size(18.0)
                        .color(palette::PRIMARY),
                );
                ui.label(RichText::new(format!("（计价方式：{}）", self.method.label())).weak());
            });
            if self.out_total.is_zero() {
                ui.colored_label(palette::WARN, "本期没有发出成本，无需结转");
            }
            ui.add_space(6.0);
            egui::Grid::new("inv_cost_form")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("凭证日期：");
                    ui.add_sized([120.0, 22.0], egui::TextEdit::singleline(&mut self.cost_date));
                    ui.end_row();
                    ui.label("成本科目：");
                    widgets::account_combo(
                        ui,
                        "inv_cost_acct",
                        &mut self.cost_account,
                        ctx.chart(),
                        true,
                        260.0,
                    );
                    ui.end_row();
                    ui.label("存货科目：");
                    widgets::account_combo(
                        ui,
                        "inv_asset_acct",
                        &mut self.asset_account,
                        ctx.chart(),
                        true,
                        260.0,
                    );
                    ui.end_row();
                });
            ui.add_space(8.0);
            if ui.button("生成结转成本凭证").clicked() && ctx.can(Perm::VoucherNew) {
                self.gen_cost_voucher(ctx, p);
            }
        });
    }

    fn gen_cost_voucher(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let date = NaiveDate::parse_from_str(self.cost_date.trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| p.last_day());
        let who = ctx.user().display_name.clone();
        let (cost, asset) = (self.cost_account.clone(), self.asset_account.clone());
        let r = business::stock_cost_voucher(
            ctx.db(),
            p,
            date,
            self.method,
            &cost,
            &asset,
            &who,
        );
        match r {
            Ok(Some(id)) => {
                let no = voucher_no(ctx.db(), id);
                ctx.log("存货", "结转成本", &format!("{} 凭证{no}", p.label()));
                ctx.info(format!("已生成结转成本凭证 {no}"));
                self.dirty = true;
            }
            // 没有出库流水时返回 None，这不算错误，只是无需结转
            Ok(None) => ctx.error("本期没有发出成本，无需结转"),
            Err(e) => ctx.error(e.to_string()),
        }
    }
}

/// 凭证号展示用：拿到"记-0005"这样的完整号码，取不到就退回 #id
fn voucher_no(db: &findb::Db, id: i64) -> String {
    match findb::vouchers::get(db, id) {
        Ok(Some(v)) => v.voucher_no(),
        _ => format!("#{id}"),
    }
}
