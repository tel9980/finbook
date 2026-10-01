//! 凭证查询（会计凭证序时簿）

use std::collections::HashSet;

use egui::{RichText, Ui};
use fincore::{Period, Perm, Voucher, VoucherStatus};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme;
use crate::widgets::{self, Paging};

pub enum Action {
    None,
    Open(i64),
    New,
}

pub struct VoucherList {
    pub from: String,
    pub to: String,
    pub word: String,
    pub status: usize,
    pub keyword: String,
    pub account: String,
    pub rows: Vec<Voucher>,
    pub sel: HashSet<i64>,
    pub paging: Paging,
    /// 需要重新查询
    pub dirty: bool,
    key: String,
}

const STATUS_OPTS: [&str; 4] = ["全部", "未记账", "已记账", "已作废"];

impl Default for VoucherList {
    fn default() -> Self {
        Self {
            from: String::new(),
            to: String::new(),
            word: String::new(),
            status: 0,
            keyword: String::new(),
            account: String::new(),
            rows: Vec::new(),
            sel: HashSet::new(),
            paging: Paging::default(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl VoucherList {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// 进入界面时把默认期间同步为当前业务期间
    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        let p = ctx.period();
        if self.from.is_empty() {
            self.from = p.code();
        }
        if self.to.is_empty() {
            self.to = p.code();
        }
        self.dirty = true;
    }

    fn query(&self) -> findb::vouchers::VoucherQuery {
        let from = Period::parse(&self.from).ok();
        let to = Period::parse(&self.to).ok();
        let st = match self.status {
            1 => Some(VoucherStatus::Draft),
            2 => Some(VoucherStatus::Posted),
            3 => Some(VoucherStatus::Void),
            _ => None,
        };
        findb::vouchers::VoucherQuery {
            from,
            to,
            status: st,
            word: if self.word.trim().is_empty() {
                None
            } else {
                Some(self.word.trim().to_string())
            },
            keyword: if self.keyword.trim().is_empty() {
                None
            } else {
                Some(self.keyword.trim().to_string())
            },
            account_code: if self.account.trim().is_empty() {
                None
            } else {
                Some(self.account.trim().to_string())
            },
            asc: true,
            ..Default::default()
        }
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            self.from, self.to, self.word, self.status, self.keyword, self.account
        );
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        self.sel.clear();
        self.paging.reset();
        let mut q = self.query().with_data_scope(ctx.user());
        // 科目范围（数据范围）：凭证可能横跨多科目，查询层只挡了制单人，
        // 这里对结果逐张过滤
        let u = ctx.user().clone();
        let mut rows = findb::vouchers::list(ctx.db(), &q).unwrap_or_default();
        // 列表要展示摘要/借贷合计，且科目范围过滤依赖分录：批量补充分录后再过滤
        let _ = findb::vouchers::fill_entries(ctx.db(), &mut rows);
        if !u.data_scope.is_unrestricted() {
            rows.retain(|v| u.can_see_voucher(v));
        }
        self.rows = rows;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) -> Action {
        self.reload(ctx);
        let mut act = Action::None;

        widgets::page_header(ui, "凭证查询", |ui| {
            ui.label(RichText::new(format!("共 {} 张", self.rows.len())).weak());
        });

        // ---------------- 过滤区 ----------------
        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.from));
            ui.label("—");
            ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.to));
            ui.label("凭证字");
            ui.add_sized([50.0, 22.0], egui::TextEdit::singleline(&mut self.word));
            ui.label("状态");
            egui::ComboBox::from_id_salt("vl_status")
                .selected_text(STATUS_OPTS[self.status])
                .width(90.0)
                .show_ui(ui, |ui| {
                    for (i, s) in STATUS_OPTS.iter().enumerate() {
                        ui.selectable_value(&mut self.status, i, *s);
                    }
                });
            ui.label("科目");
            ui.add_sized([90.0, 22.0], egui::TextEdit::singleline(&mut self.account));
            ui.label("关键字");
            let kw = ui.add_sized(
                [140.0, 22.0],
                egui::TextEdit::singleline(&mut self.keyword).hint_text("摘要/凭证号"),
            );
            if ui.button("查询").clicked() || kw.lost_focus() {
                self.dirty = true;
            }
            if ui.button("重置").clicked() {
                self.keyword.clear();
                self.account.clear();
                self.word.clear();
                self.status = 0;
                let p = ctx.period();
                self.from = p.code();
                self.to = p.code();
                self.dirty = true;
            }
        });

        // ---------------- 操作区 ----------------
        widgets::toolbar(ui, |ui| {
            if ui.button("填制凭证").clicked() && ctx.can(Perm::VoucherNew) {
                act = Action::New;
            }
            // 与 Web 端一致：无审核环节，仅保留批量记账（未记账 → 已记账）
            if ui.button("批量记账").clicked() && ctx.can(Perm::VoucherPost) {
                let ids: Vec<i64> = self
                    .sel
                    .iter()
                    .copied()
                    .filter(|id| {
                        self.rows
                            .iter()
                            .any(|v| v.id == *id && v.status.can_post())
                    })
                    .collect();
                if ids.is_empty() {
                    ctx.error("请先勾选未记账的凭证");
                } else {
                    // posted_by 与 prepared_by 口径一致：存 username
                    let who = ctx.user().username.clone();
                    let r = findb::vouchers::post_many(ctx.db(), &ids, &who);
                    if let Some((n, errs)) = ctx.handle(r) {
                        if errs.is_empty() {
                            ctx.info(format!("已记账 {n} 张"));
                        } else {
                            ctx.error(format!("已记账 {n} 张，失败 {} 张", errs.len()));
                        }
                        for e in errs.iter().take(5) {
                            ctx.error(e.clone());
                        }
                        ctx.log("凭证", "批量记账", &format!("{n} 张"));
                        self.dirty = true;
                    }
                }
            }
            ui.separator();
            if ui.button("重排断号").clicked() && ctx.can(Perm::VoucherEdit) {
                let p = ctx.period();
                let word = "记";
                match findb::vouchers::renumber(ctx.db(), p, word) {
                    Ok(0) => ctx.info("当期无凭证，无需重排"),
                    Ok(n) => {
                        ctx.info(format!("已重排 {n} 张凭证号"));
                        ctx.log("凭证", "重排断号", &format!("{}-{} 共 {n} 张", p.label(), word));
                        self.dirty = true;
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
            ui.separator();
            if ui.button("全选本页").clicked() {
                for v in self.paging.slice(&self.rows) {
                    self.sel.insert(v.id);
                }
            }
            if ui.button("取消选择").clicked() {
                self.sel.clear();
            }
            if ui.button("删除").clicked() && ctx.can(Perm::VoucherDelete) {
                if self.sel.len() == 1 {
                    let id = *self.sel.iter().next().unwrap();
                    ctx.confirm_dangerous(
                        "删除凭证",
                        "删除后不可恢复，确定继续吗？",
                        ConfirmAction::DeleteVoucher(id),
                        true,
                    );
                } else if self.sel.is_empty() {
                    ctx.error("请先勾选凭证");
                } else {
                    ctx.error("一次只能删除一张凭证，请单独勾选");
                }
            }
            ui.separator();
            if let Some(mode) = crate::views::export::export_print_controls(ui, ctx) {
                self.export(ctx, mode);
            }
        });

        // 凭证套打：按会计档案版式整张打印（不落地数据文件，Report 权限即可）
        if ctx.user().can(Perm::Report) {
            widgets::toolbar(ui, |ui| {
                ui.label(RichText::new("套打").strong());
                if ui.button("凭证套打").clicked() {
                    self.print_taoda(ctx);
                }
            });
        }

        // ---------------- 列表 ----------------
        let page_rows = self.paging.slice(&self.rows);
        let shown: Vec<Voucher> = page_rows.to_vec();
        let start = self.paging.page * self.paging.size;
        let cols = [
            widgets::TCol::new("✓", 26.0).fixed(),
            widgets::TCol::new("日期", 92.0).fixed(),
            widgets::TCol::new("凭证号", 88.0).fixed(),
            widgets::TCol::new("摘要", 300.0),
            widgets::TCol::new("借方合计", 130.0).right(),
            widgets::TCol::new("贷方合计", 130.0).right(),
            widgets::TCol::new("状态", 70.0).fixed(),
            widgets::TCol::new("制单", 80.0).fixed(),
            widgets::TCol::new("审核", 80.0).fixed(),
            widgets::TCol::new("记账", 80.0).fixed(),
        ];
        let mut clicked: Option<i64> = None;
        widgets::grid(ui, "voucher_list", &cols, shown.len(), 24.0, |i, c, ui| {
            let v = &shown[i];
            match c {
                0 => {
                    let mut on = self.sel.contains(&v.id);
                    if ui.checkbox(&mut on, "").changed() {
                        if on {
                            self.sel.insert(v.id);
                        } else {
                            self.sel.remove(&v.id);
                        }
                    }
                }
                1 => {
                    ui.label(v.date.format("%Y-%m-%d").to_string());
                }
                2 => {
                    if ui.link(RichText::new(v.voucher_no()).monospace()).clicked() {
                        clicked = Some(v.id);
                    }
                }
                3 => {
                    ui.label(v.first_summary());
                }
                4 => widgets::amount_label(ui, v.debit_total()),
                5 => widgets::amount_label(ui, v.credit_total()),
                6 => {
                    ui.label(
                        RichText::new(v.status.label())
                            .color(theme::status_color(v.status.counts())),
                    );
                }
                7 => {
                    ui.label(&v.prepared_by);
                }
                8 => {
                    ui.label(v.audited_by.as_deref().unwrap_or(""));
                }
                9 => {
                    ui.label(v.posted_by.as_deref().unwrap_or(""));
                }
                _ => {}
            }
        });
        if let Some(id) = clicked {
            act = Action::Open(id);
        }

        // 合计
        let mut td = fincore::Money::ZERO;
        let mut tc = fincore::Money::ZERO;
        for v in &self.rows {
            td += v.debit_total();
            tc += v.credit_total();
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!(
                "查询结果合计：借 {} / 贷 {}（第 {} — {} 条，共 {} 条）",
                td.fmt_money(),
                tc.fmt_money(),
                start + 1,
                start + shown.len(),
                self.rows.len()
            )));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.paging.bar(ui, self.rows.len());
            });
        });

        act
    }

    fn print_taoda(&mut self, ctx: &mut AppCtx<'_>) {
        if self.rows.is_empty() {
            ctx.error("没有可打印的凭证");
            return;
        }
        let company = ctx.db().options().company.clone();
        let chart = ctx.chart().clone();
        let aux_names = &ctx.st.aux_names;
        let aux_label = |aux: &fincore::AuxRef| crate::views::voucher_edit::aux_text(aux, aux_names);
        let prints = findb::printform::vouchers_to_print(&self.rows, &chart, &aux_label);
        if prints.is_empty() {
            ctx.error("没有未作废的凭证可打印");
            return;
        }
        let period_label = format!("{}~{}", self.from, self.to);
        let html = findb::printform::voucher_form_html(&company, &period_label, &prints, true);
        let title = format!("凭证套打_{}_{}", self.from, self.to);
        match crate::views::export::print_html_content(&title, &html) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }

    fn export(&mut self, ctx: &mut AppCtx<'_>, mode: crate::views::export::ExportMode) {
        if self.rows.is_empty() {
            ctx.error("没有可导出的数据");
            return;
        }
        let mut sh = crate::views::export::Sheet::new(
            "凭证列表",
            vec![
                "期间".into(),
                "日期".into(),
                "凭证字".into(),
                "凭证号".into(),
                "摘要".into(),
                "借方合计".into(),
                "贷方合计".into(),
                "状态".into(),
                "制单人".into(),
                "审核人".into(),
                "记账人".into(),
            ],
        );
        for v in &self.rows {
            sh.push(vec![
                v.period.code(),
                v.date.format("%Y-%m-%d").to_string(),
                v.word.clone(),
                format!("{:04}", v.no),
                v.first_summary(),
                v.debit_total().fmt_plain(),
                v.credit_total().fmt_plain(),
                v.status.label().to_string(),
                v.prepared_by.clone(),
                v.audited_by.clone().unwrap_or_default(),
                v.posted_by.clone().unwrap_or_default(),
            ]);
        }
        let file_name = format!("凭证列表_{}_{}", self.from, self.to);
        let title = format!("凭证列表（{} ~ {}）", self.from, self.to);
        match crate::views::export::run_export(&sh, &file_name, &title, mode) {
            Ok(m) => ctx.info(m),
            Err(e) => ctx.error(e),
        }
    }
}
