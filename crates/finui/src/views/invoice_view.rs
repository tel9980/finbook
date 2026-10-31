//! 发票管理（进项 / 销项登记与认证）
//!
//! 为什么桌面端需要这一页：一般纳税人的进项抵扣要求**取得专票且已认证**，
//! `crates/findb/src/taxdecl.rs` 里只有 `status == "verified"` 的进项才计入可抵扣。
//! 以前登记与认证只能在网页端做，桌面版连入口都没有 —— 于是「记了账但没登记发票」
//! 这件事谁都不会发现，直到申报时进项凭空少一截。

use egui::{RichText, Ui};
use fincore::Money;
use findb::invoices::{Invoice, InvoiceQuery};
use rust_decimal::Decimal;

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

/// 新增 / 编辑发票的临时表单
#[derive(Clone, Debug, Default)]
pub struct InvoiceEdit {
    pub id: i64,
    pub kind: String,
    pub code: String,
    pub number: String,
    pub date: String,
    pub buyer: String,
    pub seller: String,
    pub amount_tax: String,
    pub tax_rate: String,
    pub memo: String,
}

#[derive(Default)]
pub struct InvoiceView {
    /// "" = 全部；"in" 进项；"out" 销项
    pub kind: String,
    /// "" = 全部；pending / verified / rejected
    pub status: String,
    pub kw: String,
    pub rows: Vec<Invoice>,
    /// (kind, status, 税额, 张数)
    pub summary: Vec<(String, String, Money, i64)>,
    pub edit: Option<InvoiceEdit>,
    dirty: bool,
}

/// 价税合计 + 税率(%) → (不含税金额, 税额)，都四舍五入到分
///
/// 单独抽成纯函数是为了能测：这段是「13% 的 10300 到底拆成多少」，
/// 拆错了发票上的两个数都不对，而界面上看不出来。
pub fn split_amount_tax(total: &str, rate_pct: &str) -> Result<(String, String), String> {
    let t = total.trim().trim_start_matches('¥').replace(',', "");
    if t.is_empty() {
        return Err("价税合计没填".to_string());
    }
    let total = Money::parse(&t).map_err(|_| format!("价税合计「{total}」不是金额"))?;
    if total < Money::ZERO {
        return Err("价税合计不能是负数（红字发票请按正数登记）".to_string());
    }
    let r = rate_pct.trim().trim_end_matches('%').trim();
    // 空税率 = 0%，不报错：不开票的收入、免税项目都是 0%
    let rate = if r.is_empty() {
        Money::ZERO
    } else {
        Money::parse(r).map_err(|_| format!("税率「{rate_pct}」不是数字"))?
    };
    if rate < Money::ZERO || rate > Money::parse_or_zero("100") {
        return Err(format!("税率「{rate_pct}」应在 0–100 之间"));
    }
    // 不含税 = 价税合计 / (1 + 税率/100)；税额 = 合计 − 不含税。
    // 先算不含税再相减，别各算一遍 —— 两条路径在分位上会差 1 分，
    // 而发票上「不含税 + 税额」必须刚好等于合计。
    let divisor = Decimal::ONE + rate.inner() / Decimal::from(100);
    let amount = (total.inner() / divisor).round_dp(2);
    let tax = (total.inner() - amount).round_dp(2);
    Ok((Money::new(amount).fmt_plain(), Money::new(tax).fmt_plain()))
}

impl InvoiceView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        let _ = ctx;
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &AppCtx<'_>) {
        let q = InvoiceQuery {
            kind: Some(self.kind.clone()).filter(|k| !k.is_empty()),
            status: Some(self.status.clone()).filter(|s| !s.is_empty()),
            keyword: Some(self.kw.clone()).filter(|s| !s.trim().is_empty()),
            limit: Some(500),
        };
        self.rows = findb::invoices::list(ctx.db(), &q).unwrap_or_default();
        self.summary = findb::invoices::tax_summary(ctx.db()).unwrap_or_default();
        self.dirty = false;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.dirty {
            self.reload(ctx);
        }
        widgets::page_header(ui, "发票管理", |ui| {
            ui.label(RichText::new("进项认证决定能不能抵扣").weak());
        });

        self.show_summary(ui);
        ui.add_space(8.0);

        let mut open_new = false;
        let mut set_status: Option<(i64, &'static str)> = None;
        let mut edit_id: Option<i64> = None;

        widgets::card(ui, "", |ui| {
            widgets::toolbar(ui, |ui| {
                ui.label("类型");
                ui.selectable_value(&mut self.kind, String::new(), "全部");
                ui.selectable_value(&mut self.kind, "in".to_string(), "进项");
                ui.selectable_value(&mut self.kind, "out".to_string(), "销项");
                ui.separator();
                ui.label("状态");
                ui.selectable_value(&mut self.status, String::new(), "全部");
                ui.selectable_value(&mut self.status, "pending".to_string(), "待认证");
                ui.selectable_value(&mut self.status, "verified".to_string(), "已认证");
                ui.selectable_value(&mut self.status, "rejected".to_string(), "已作废");
                ui.separator();
                ui.add_sized(
                    [180.0, 22.0],
                    egui::TextEdit::singleline(&mut self.kw).hint_text("号码/对方"),
                );
                if ui.button("刷新").clicked() {
                    self.dirty = true;
                }
                if ui
                    .button("登记发票")
                    .on_hover_text("进项记得登记后去「认证」，否则申报时抵不了")
                    .clicked()
                {
                    open_new = true;
                }
            });
        });

        ui.add_space(6.0);
        if self.rows.is_empty() {
            ui.label(RichText::new("没有发票记录。点「登记发票」开始。").weak());
        } else {
            egui::Grid::new("invoice_rows")
                .num_columns(8)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for h in [
                        "开票日期",
                        "类型",
                        "发票号码",
                        "对方",
                        "价税合计",
                        "税额",
                        "状态",
                        "操作",
                    ] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for r in &self.rows {
                        ui.label(&r.date);
                        ui.label(if r.kind == "in" { "进项" } else { "销项" });
                        ui.label(format!("{}{}", r.code, r.number));
                        ui.label(if r.kind == "in" { &r.seller } else { &r.buyer });
                        ui.label(r.amount_tax.fmt_money());
                        ui.label(r.tax.fmt_money());
                        let (txt, color) = match r.status.as_str() {
                            "verified" => ("已认证", palette::OK),
                            "rejected" => ("已作废", ui.visuals().weak_text_color()),
                            _ => ("待认证", palette::WARN),
                        };
                        ui.label(RichText::new(txt).color(color));
                        ui.horizontal(|ui| {
                            if r.status == "pending" && ui.small_button("认证").clicked() {
                                set_status = Some((r.id, "verified"));
                            }
                            if r.status != "rejected" && ui.small_button("作废").clicked() {
                                set_status = Some((r.id, "rejected"));
                            }
                            if ui.small_button("编辑").clicked() {
                                edit_id = Some(r.id);
                            }
                        });
                        ui.end_row();
                    }
                });
        }

        if let Some((id, st)) = set_status {
            let who = ctx.user().username.clone();
            match findb::invoices::set_status(ctx.db(), id, st, &who) {
                Ok(_) => self.dirty = true,
                Err(e) => ctx.toast(format!("操作失败：{e}")),
            }
        }
        if let Some(id) = edit_id {
            if let Ok(Some(inv)) = findb::invoices::get(ctx.db(), id) {
                self.edit = Some(InvoiceEdit {
                    id: inv.id,
                    kind: inv.kind,
                    code: inv.code,
                    number: inv.number,
                    date: inv.date,
                    buyer: inv.buyer,
                    seller: inv.seller,
                    amount_tax: inv.amount_tax.fmt_plain(),
                    tax_rate: inv.tax_rate,
                    memo: inv.memo,
                });
            }
        }
        if open_new {
            self.edit = Some(InvoiceEdit {
                kind: if self.kind == "out" { "out" } else { "in" }.to_string(),
                date: chrono::Local::now().format("%Y-%m-%d").to_string(),
                ..Default::default()
            });
        }

        self.show_edit(ctx, ui);
    }

    fn show_summary(&self, ui: &mut Ui) {
        let sum_of = |kind: &str, status: &str| -> (Money, i64) {
            self.summary
                .iter()
                .find(|r| r.0 == kind && r.1 == status)
                .map(|r| (r.2, r.3))
                .unwrap_or((Money::ZERO, 0))
        };
        let (pend_tax, pend_n) = sum_of("in", "pending");
        let (ver_tax, ver_n) = sum_of("in", "verified");
        let (out_tax, out_n) = sum_of("out", "verified");

        widgets::card(ui, "税额概览", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("待认证进项：");
                ui.label(
                    RichText::new(format!("{} 元", pend_tax.fmt_money()))
                        .strong()
                        .color(if pend_n > 0 {
                            palette::WARN
                        } else {
                            ui.visuals().weak_text_color()
                        }),
                );
                ui.label(RichText::new(format!("（{pend_n} 张）")).weak());
                ui.separator();
                ui.label("已认证进项：");
                ui.label(
                    RichText::new(format!("{} 元", ver_tax.fmt_money()))
                        .strong()
                        .color(palette::OK),
                );
                ui.label(RichText::new(format!("（{ver_n} 张）")).weak());
                ui.separator();
                ui.label("销项税额：");
                ui.label(RichText::new(format!("{} 元", out_tax.fmt_money())).strong());
                ui.label(RichText::new(format!("（{out_n} 张）")).weak());
            });
            if pend_n > 0 {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("⚠ 还有待认证的进项票：认证前不能抵扣，申报时会白白多缴这段税。")
                        .color(palette::WARN),
                );
            }
        });
    }

    fn show_edit(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(mut e) = self.edit.clone() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new(if e.id == 0 {
            "登记发票"
        } else {
            "编辑发票"
        })
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .show(ui.ctx(), |ui| {
            egui::Grid::new("invoice_edit")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("类型");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut e.kind, "in".to_string(), "进项（收到的）");
                        ui.selectable_value(&mut e.kind, "out".to_string(), "销项（开出的）");
                    });
                    ui.end_row();
                    ui.label("开票日期");
                    ui.add_sized(
                        [120.0, 22.0],
                        egui::TextEdit::singleline(&mut e.date).hint_text("2026-11-05"),
                    );
                    ui.end_row();
                    ui.label("发票代码");
                    ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut e.code));
                    ui.end_row();
                    ui.label("发票号码");
                    ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut e.number));
                    ui.end_row();
                    ui.label(if e.kind == "in" {
                        "销售方"
                    } else {
                        "购买方"
                    });
                    ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(&mut e.buyer));
                    ui.end_row();
                    ui.label("对方名称");
                    let (party_label, party) = if e.kind == "in" {
                        ("销售方", &mut e.seller)
                    } else {
                        ("购买方", &mut e.buyer)
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(party_label).weak());
                        ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(party));
                    });
                    ui.end_row();
                    ui.label("价税合计");
                    ui.add_sized(
                        [140.0, 22.0],
                        egui::TextEdit::singleline(&mut e.amount_tax).hint_text("10300"),
                    );
                    ui.end_row();
                    ui.label("税率 %");
                    ui.add_sized(
                        [80.0, 22.0],
                        egui::TextEdit::singleline(&mut e.tax_rate).hint_text("13"),
                    );
                    ui.end_row();
                    ui.label("备注");
                    ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(&mut e.memo));
                    ui.end_row();
                });
            // 实算一遍给用户看着：填完就能对上票面
            match split_amount_tax(&e.amount_tax, &e.tax_rate) {
                Ok((amount, tax)) => {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("不含税 {amount} ＋ 税额 {tax} = 价税合计")).weak(),
                    );
                }
                Err(msg) => {
                    ui.add_space(4.0);
                    ui.label(RichText::new(msg).color(palette::WARN));
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() {
                    save = true;
                }
                if ui.button("取消").clicked() {
                    cancel = true;
                }
            });
        });

        if cancel || !open {
            self.edit = None;
            return;
        }
        self.edit = Some(e.clone());
        if !save {
            return;
        }

        let (amount, tax) = match split_amount_tax(&e.amount_tax, &e.tax_rate) {
            Ok(v) => v,
            Err(msg) => {
                ctx.toast(msg);
                return;
            }
        };
        let who = ctx.user().username.clone();
        let inv = Invoice {
            id: e.id,
            kind: e.kind.clone(),
            code: e.code.trim().to_string(),
            number: e.number.trim().to_string(),
            date: e.date.trim().to_string(),
            buyer: e.buyer.trim().to_string(),
            seller: e.seller.trim().to_string(),
            amount_tax: Money::parse_or_zero(&e.amount_tax),
            amount: Money::parse_or_zero(&amount),
            tax: Money::parse_or_zero(&tax),
            tax_rate: e.tax_rate.trim().to_string(),
            status: "pending".to_string(),
            memo: e.memo.trim().to_string(),
            attach_id: 0,
            created_by: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        if inv.number.is_empty() {
            ctx.toast("发票号码不能为空");
            return;
        }
        let res = if e.id == 0 {
            findb::invoices::insert(ctx.db(), &inv, &who).map(|_| ())
        } else {
            findb::invoices::update(ctx.db(), &inv)
        };
        match res {
            Ok(()) => {
                ctx.toast("已保存");
                self.edit = None;
                self.dirty = true;
            }
            Err(err) => ctx.toast(format!("保存失败：{err}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::split_amount_tax;

    #[test]
    fn thirteen_percent_splits_cleanly() {
        let (a, t) = split_amount_tax("10300", "13").unwrap();
        assert_eq!(a, "9115.04");
        assert_eq!(t, "1184.96");
    }

    #[test]
    fn three_percent_and_zero_percent() {
        let (a, t) = split_amount_tax("10300", "3%").unwrap();
        assert_eq!(a, "10000.00");
        assert_eq!(t, "300.00");
        let (a, t) = split_amount_tax("10300", "0").unwrap();
        assert_eq!(a, "10300.00");
        assert_eq!(t, "0.00");
    }

    #[test]
    fn thousands_separator_and_currency_sign_are_tolerated() {
        let (a, _) = split_amount_tax("¥10,300.00", "13").unwrap();
        assert_eq!(a, "9115.04");
    }

    #[test]
    fn bad_input_names_the_original_value() {
        assert!(split_amount_tax("十万", "13").unwrap_err().contains("十万"));
        assert!(split_amount_tax("100", "abc").unwrap_err().contains("abc"));
        assert!(split_amount_tax("", "13").unwrap_err().contains("没填"));
        // 税率越界要拦住：填成 130 会算出负的税额，而票面上看不出来
        assert!(split_amount_tax("100", "130")
            .unwrap_err()
            .contains("0–100"));
    }
}
