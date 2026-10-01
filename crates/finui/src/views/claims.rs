//! 费用报销：单据录入、审批流转、生成记账凭证
//!
//! 报销是典型的「业务单据在前、会计凭证在后」：状态机走完（草稿 → 已提交 → 已审批 → 已支付）
//! 才允许生成凭证，且一张单据只能生成一次——这两条限制都由 `findb::business::claim_*` 把关，
//! 界面只负责把当前状态允许的按钮显示出来，不让用户点到必然报错的操作。

use chrono::NaiveDate;
use egui::{Align2, Color32, RichText, Ui};
use findb::business::{self, Claim, ClaimItem, ClaimStatus};
use fincore::{Money, Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets::{self, Paging};

/// 状态筛选下拉：0 为全部，其后与 `ClaimStatus::ALL` 一一对齐
const STATUS_OPTS: [&str; 6] = ["全部", "草稿", "已提交", "已审批", "已驳回", "已支付"];

/// 明细行编辑缓冲区
#[derive(Clone)]
struct ItemDraft {
    account: String,
    amount: String,
    memo: String,
}

/// 报销单编辑缓冲区。`base` 保留原单据的不可改字段（状态、审批人、凭证号等）
#[derive(Clone)]
pub struct ClaimDraft {
    base: Claim,
    is_new: bool,
    date: String,
    applicant: String,
    dept: String,
    reason: String,
    items: Vec<ItemDraft>,
}

pub struct ClaimsView {
    pub period_text: String,
    pub status: usize,
    pub kw: String,
    pub rows: Vec<Claim>,
    pub paging: Paging,
    /// 生成凭证时的贷方（支付）科目
    pub pay_account: String,
    pub editing: Option<ClaimDraft>,
    /// 待执行的状态流转（单据 id + 目标状态）
    pub pending_transition: Option<(i64, ClaimStatus)>,
    /// 待生成凭证的单据
    pub pending_voucher: Option<i64>,
    pub err: String,
    pub dirty: bool,
    key: String,
}

impl Default for ClaimsView {
    fn default() -> Self {
        Self {
            period_text: String::new(),
            status: 0,
            kw: String::new(),
            rows: Vec::new(),
            paging: Paging::default(),
            pay_account: "100201".to_string(),
            editing: None,
            pending_transition: None,
            pending_voucher: None,
            err: String::new(),
            dirty: true,
            key: String::new(),
        }
    }
}

impl ClaimsView {
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

    fn status_filter(&self) -> Option<ClaimStatus> {
        match self.status {
            0 => None,
            i => ClaimStatus::ALL.get(i - 1).copied(),
        }
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let p = self.period(ctx);
        let status = self.status_filter().map(|s| s.code()).unwrap_or("-");
        let key = format!("{}|{}|{}", p.ymm(), status, self.kw.trim());
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;
        // 「仅看本人经手的业务单据」：报销按申请人过滤
        let mut rows = business::claim_list(ctx.db(), p, self.status_filter()).unwrap_or_default();
        let u = ctx.user();
        if u.data_scope.own_doc_only {
            rows.retain(|c| c.applicant == u.display_name || c.applicant == u.username);
        }
        self.rows = rows;
        self.paging.reset();
    }

    /// 状态颜色：一眼区分单据走到哪一步
    fn status_color(s: ClaimStatus) -> Color32 {
        match s {
            ClaimStatus::Draft => Color32::GRAY,
            ClaimStatus::Submitted => palette::PRIMARY,
            ClaimStatus::Approved => palette::OK,
            ClaimStatus::Rejected => palette::CREDIT,
            ClaimStatus::Paid => palette::DEBIT,
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let p = self.period(ctx);

        widgets::page_header(ui, "费用报销", |ui| {
            ui.label(RichText::new(format!("共 {} 张", self.rows.len())).weak());
        });

        ui.label(
            RichText::new(
                "状态机：草稿 → 已提交 → 已审批 → 已支付；只有「已支付」能生成凭证，且同一张单据不能重复生成。",
            )
            .weak(),
        );

        widgets::toolbar(ui, |ui| {
            ui.label("期间");
            let rp = ui.add_sized([84.0, 22.0], egui::TextEdit::singleline(&mut self.period_text));
            if rp.changed() {
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
            ui.label("状态");
            egui::ComboBox::from_id_salt("claim_status")
                .selected_text(STATUS_OPTS[self.status])
                .width(90.0)
                .show_ui(ui, |ui| {
                    for (i, s) in STATUS_OPTS.iter().enumerate() {
                        ui.selectable_value(&mut self.status, i, *s);
                    }
                });
            let rk = widgets::search_field(ui, &mut self.kw, "单号/申请人/事由");
            if rk.changed() || rk.lost_focus() {
                self.dirty = true;
            }
            ui.separator();
            if ui.button("新增报销单").clicked() && ctx.can(Perm::VoucherNew) {
                self.open_new(ctx, p);
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            ui.label("支付科目");
            widgets::account_combo(
                ui,
                "claim_pay_acct",
                &mut self.pay_account,
                ctx.chart(),
                true,
                220.0,
            );
        });

        // 关键字在内存里过滤：单据量不大，没必要每次敲键都查库
        let kw = self.kw.trim().to_lowercase();
        let shown: Vec<Claim> = self
            .paging
            .slice(&self.rows)
            .iter()
            .filter(|c| {
                kw.is_empty()
                    || c.no.to_lowercase().contains(&kw)
                    || c.applicant.to_lowercase().contains(&kw)
                    || c.reason.to_lowercase().contains(&kw)
            })
            .cloned()
            .collect();

        let cols = [
            widgets::TCol::new("单号", 120.0).fixed(),
            widgets::TCol::new("日期", 96.0).fixed(),
            widgets::TCol::new("申请人", 100.0),
            widgets::TCol::new("部门", 90.0),
            widgets::TCol::new("事由", 220.0),
            widgets::TCol::new("金额", 120.0).right(),
            widgets::TCol::new("状态", 80.0).fixed(),
            widgets::TCol::new("凭证号", 80.0).fixed(),
            widgets::TCol::new("操作", 240.0).fixed(),
        ];
        let mut edit: Option<i64> = None;
        let mut del: Option<i64> = None;
        let mut trans: Option<(i64, ClaimStatus)> = None;
        let mut voucher: Option<i64> = None;
        widgets::grid(ui, "claim_rows", &cols, shown.len(), 24.0, |i, c, ui| {
            let r = &shown[i];
            match c {
                0 => {
                    ui.label(RichText::new(&r.no).monospace());
                }
                1 => {
                    ui.label(r.biz_date.format("%Y-%m-%d").to_string());
                }
                2 => {
                    ui.label(&r.applicant);
                }
                3 => {
                    ui.label(&r.dept);
                }
                4 => {
                    ui.label(&r.reason);
                }
                5 => widgets::amount_label(ui, r.amount),
                6 => {
                    ui.label(RichText::new(r.status.label()).color(Self::status_color(r.status)));
                }
                7 => {
                    ui.label(match r.voucher_id {
                        Some(v) => RichText::new(format!("{v}")).monospace(),
                        None => RichText::new("—").weak(),
                    });
                }
                8 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        // 已生成凭证或已流转过的单据不允许再改内容
                        if r.status.editable() && r.voucher_id.is_none() {
                            if ui.small_button("改").clicked() {
                                edit = Some(r.id);
                            }
                            if ui.small_button("删").clicked() {
                                del = Some(r.id);
                            }
                        }
                        for (label, to) in Self::actions(r) {
                            if ui.small_button(label).clicked() {
                                trans = Some((r.id, to));
                            }
                        }
                        if r.status == ClaimStatus::Paid && r.voucher_id.is_none() {
                            if ui.small_button("生成凭证").clicked() {
                                voucher = Some(r.id);
                            }
                        }
                    });
                }
                _ => {}
            }
        });

        if let Some(id) = edit {
            if let Ok(Some(c)) = business::claim_get(ctx.db(), id) {
                self.open_edit(&c);
            }
        }
        if let Some(id) = del {
            ctx.confirm_dangerous(
                "删除报销单",
                "删除后不可恢复（已生成凭证的单据需先删除凭证）。确定删除吗？",
                ConfirmAction::DeleteClaim(id),
                true,
            );
        }
        self.pending_transition = trans;
        self.pending_voucher = voucher;

        let total: Money = self.rows.iter().map(|r| r.amount).sum();
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!(
                "本期报销金额合计 {}",
                total.fmt_money()
            )).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.paging.bar(ui, self.rows.len());
            });
        });

        self.apply_pending(ctx);
        self.claim_window(ctx, ui);
    }

    /// 当前状态下可用的流转动作（已支付之后只能生成凭证，不在这里）
    fn actions(c: &Claim) -> Vec<(&'static str, ClaimStatus)> {
        match c.status {
            ClaimStatus::Draft => vec![("提交", ClaimStatus::Submitted)],
            ClaimStatus::Submitted => vec![
                ("审批通过", ClaimStatus::Approved),
                ("驳回", ClaimStatus::Rejected),
            ],
            ClaimStatus::Approved => vec![("支付", ClaimStatus::Paid)],
            ClaimStatus::Rejected => vec![("退回草稿", ClaimStatus::Draft)],
            ClaimStatus::Paid => vec![],
        }
    }

    fn apply_pending(&mut self, ctx: &mut AppCtx<'_>) {
        if let Some((id, to)) = self.pending_transition.take() {
            let who = ctx.user().display_name.clone();
            match business::claim_transition(ctx.db(), id, to, &who) {
                Ok(()) => {
                    ctx.log("报销", "状态流转", &format!("#{id} → {}", to.label()));
                    ctx.info(format!("已{}", to.label()));
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
        if let Some(id) = self.pending_voucher.take() {
            let who = ctx.user().display_name.clone();
            let pay = self.pay_account.clone();
            match business::claim_voucher(ctx.db(), id, &pay, &who) {
                Ok(vid) => {
                    let no = match findb::vouchers::get(ctx.db(), vid) {
                        Ok(Some(v)) => v.voucher_no(),
                        _ => format!("#{vid}"),
                    };
                    ctx.log("报销", "生成凭证", &format!("#{id} 凭证{no}"));
                    ctx.info(format!("已生成记账凭证 {no}"));
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }

    // ------------------------- 编辑弹窗 -------------------------
    fn open_new(&mut self, ctx: &mut AppCtx<'_>, p: Period) {
        let no = business::claim_next_no(ctx.db(), p).unwrap_or_else(|_| format!("BX{}-001", p.ymm()));
        let date = p
            .contains(chrono::Local::now().date_naive())
            .then(|| chrono::Local::now().date_naive())
            .unwrap_or_else(|| p.last_day());
        self.editing = Some(ClaimDraft {
            base: Claim {
                id: 0,
                period: p,
                no,
                biz_date: date,
                applicant: String::new(),
                dept: String::new(),
                reason: String::new(),
                amount: Money::ZERO,
                status: ClaimStatus::Draft,
                items: Vec::new(),
                approver: String::new(),
                approved_at: None,
                payer: String::new(),
                paid_at: None,
                voucher_id: None,
                created_at: date.format("%Y-%m-%d").to_string(),
            },
            is_new: true,
            date: date.format("%Y-%m-%d").to_string(),
            applicant: String::new(),
            dept: String::new(),
            reason: String::new(),
            items: vec![ItemDraft::default()],
        });
        self.err.clear();
    }

    fn open_edit(&mut self, c: &Claim) {
        self.editing = Some(ClaimDraft {
            base: c.clone(),
            is_new: false,
            date: c.biz_date.format("%Y-%m-%d").to_string(),
            applicant: c.applicant.clone(),
            dept: c.dept.clone(),
            reason: c.reason.clone(),
            items: c
                .items
                .iter()
                .map(|it| ItemDraft {
                    account: it.expense_account.clone(),
                    amount: if it.amount.is_zero() {
                        String::new()
                    } else {
                        it.amount.fmt_plain()
                    },
                    memo: it.memo.clone(),
                })
                .collect(),
        });
        self.err.clear();
    }

    fn claim_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(d) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let mut remove: Option<usize> = None;
        let is_new = d.is_new;
        let err = self.err.clone();
        // 明细合计实时算出来给用户看，单据金额由明细汇总，不让人手工填
        let total: Money = d.items.iter().map(|i| Money::parse_or_zero(&i.amount)).sum();

        egui::Window::new(if is_new { "新增报销单" } else { "修改报销单" })
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([720.0, 460.0])
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if !err.is_empty() {
                    ui.colored_label(palette::CREDIT, &err);
                }
                egui::Grid::new("claim_edit")
                    .num_columns(6)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("单号：");
                        ui.label(RichText::new(&d.base.no).monospace());
                        ui.label("日期：");
                        ui.add_sized([110.0, 22.0], egui::TextEdit::singleline(&mut d.date));
                        ui.label("申请人：");
                        widgets::text_input(ui, &mut d.applicant, 120.0, "职员编码");
                        ui.end_row();
                        ui.label("部门：");
                        widgets::text_input(ui, &mut d.dept, 120.0, "部门编码");
                        ui.label("事由：");
                        widgets::text_input(ui, &mut d.reason, 360.0, "如：差旅费");
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.label(RichText::new("费用明细").strong());
                let n = d.items.len();
                widgets::grid(
                    ui,
                    "claim_items",
                    &[
                        widgets::TCol::new("费用科目", 280.0),
                        widgets::TCol::new("金额", 110.0).right(),
                        widgets::TCol::new("备注", 240.0),
                        widgets::TCol::new("操作", 60.0).fixed(),
                    ],
                    n,
                    26.0,
                    |i, c, ui| {
                        let it = &mut d.items[i];
                        match c {
                            0 => widgets::account_combo(
                                ui,
                                &format!("claim_item_acct_{i}"),
                                &mut it.account,
                                ctx.chart(),
                                true,
                                270.0,
                            ),
                            1 => {
                                widgets::money_input(ui, &mut it.amount, 100.0);
                            }
                            2 => {
                                widgets::text_input(ui, &mut it.memo, 230.0, "摘要");
                            }
                            3 => {
                                if ui.small_button("删").clicked() {
                                    remove = Some(i);
                                }
                            }
                            _ => {}
                        }
                    },
                );
                ui.horizontal(|ui| {
                    if ui.button("+ 添加明细行").clicked() {
                        d.items.push(ItemDraft::default());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("合计 {}", total.fmt_money())).strong());
                    });
                });
                ui.add_space(10.0);
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

        if let Some(i) = remove {
            if let Some(d) = self.editing.as_mut() {
                if i < d.items.len() {
                    d.items.remove(i);
                }
            }
        }
        if close || !open {
            self.editing = None;
            self.err.clear();
            return;
        }
        if !save {
            return;
        }

        let (is_new, draft) = (self.editing.as_ref().unwrap().is_new, self.editing.clone().unwrap());
        match self.build_claim(&draft) {
            Ok(mut c) => {
                c.id = draft.base.id;
                c.no = draft.base.no.clone();
                // 状态、审批人、凭证号这些由状态机维护，编辑界面不碰
                c.status = draft.base.status;
                c.approver = draft.base.approver.clone();
                c.approved_at = draft.base.approved_at.clone();
                c.payer = draft.base.payer.clone();
                c.paid_at = draft.base.paid_at.clone();
                c.voucher_id = draft.base.voucher_id;
                c.created_at = draft.base.created_at.clone();
                let r = if is_new {
                    business::claim_insert(ctx.db(), &c).map(|_| ())
                } else {
                    business::claim_update(ctx.db(), &c)
                };
                match r {
                    Ok(()) => {
                        ctx.log(
                            "报销",
                            if is_new { "新增报销单" } else { "修改报销单" },
                            &format!("{} {} {}", c.no, c.applicant, c.amount.fmt_money()),
                        );
                        ctx.info("已保存");
                        self.dirty = true;
                        self.editing = None;
                    }
                    Err(e) => self.err = e.to_string(),
                }
            }
            Err(e) => self.err = e,
        }
    }

    fn build_claim(&self, d: &ClaimDraft) -> Result<Claim, String> {
        let date = NaiveDate::parse_from_str(d.date.trim(), "%Y-%m-%d")
            .map_err(|_| format!("日期格式不正确：{}（应为 2026-01-31）", d.date))?;
        if d.applicant.trim().is_empty() {
            return Err("请填写申请人".to_string());
        }
        let mut items: Vec<ClaimItem> = Vec::new();
        for it in &d.items {
            let amount = Money::parse_or_zero(&it.amount).round2();
            if it.account.trim().is_empty() && amount.is_zero() && it.memo.trim().is_empty() {
                continue; // 空行直接跳过，允许留几行空白方便连续录入
            }
            if it.account.trim().is_empty() {
                return Err("明细行的费用科目不能为空".to_string());
            }
            if amount <= Money::ZERO {
                return Err("明细行金额必须大于零".to_string());
            }
            items.push(ClaimItem {
                expense_account: it.account.trim().to_string(),
                amount,
                memo: it.memo.trim().to_string(),
            });
        }
        if items.is_empty() {
            return Err("请至少录入一行费用明细".to_string());
        }
        // 单据金额必须等于明细合计，否则后面生成凭证时会被数据层拦下
        let amount = items.iter().map(|i| i.amount).sum();
        Ok(Claim {
            id: d.base.id,
            period: Period::from_date(date),
            no: d.base.no.clone(),
            biz_date: date,
            applicant: d.applicant.trim().to_string(),
            dept: d.dept.trim().to_string(),
            reason: d.reason.trim().to_string(),
            amount,
            status: d.base.status,
            items,
            approver: d.base.approver.clone(),
            approved_at: d.base.approved_at.clone(),
            payer: d.base.payer.clone(),
            paid_at: d.base.paid_at.clone(),
            voucher_id: d.base.voucher_id,
            created_at: d.base.created_at.clone(),
        })
    }
}

impl Default for ItemDraft {
    fn default() -> Self {
        Self {
            account: String::new(),
            amount: String::new(),
            memo: String::new(),
        }
    }
}
