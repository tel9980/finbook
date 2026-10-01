//! 凭证填制
//!
//! 这是整个软件用得最多的界面，交互参照金蝶/用友：
//! - 摘要可复制上一行、科目支持编码直输与弹窗选择
//! - 借贷不平不能保存，差额一键补平
//! - 与 Web 端一致：无需审核，保存即记账（Posted），已作废凭证只读

use chrono::NaiveDate;
use egui::{Align, Color32, Layout, RichText, Ui};
use egui_extras::{Column, DatePickerButton, TableBuilder};
use fincore::engine::{validate_voucher, ValidateCtx};
use fincore::report::cashflow::CashFlowItem;
use fincore::{
    AuxKind, AuxRef, Chart, Direction, Entry, Money, Period, Perm, Voucher, VoucherStatus,
};

use crate::state::AppCtx;
use crate::theme;
use crate::theme::palette;
use crate::widgets::{self, AccountPickerState, TCol};

/// 编辑中的一行分录
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub summary: String,
    pub code: String,
    pub debit: String,
    pub credit: String,
    pub aux: AuxRef,
    pub qty: String,
    pub price: String,
    /// 原币币种（如 USD）
    pub currency: String,
    /// 汇率（1 外币 = ? 本位币）
    pub rate: String,
    /// 原币金额
    pub amount_for: String,
    /// 现金流量项目编码（现金/银行科目）
    pub cf: String,
}

impl Row {
    pub fn is_blank(&self) -> bool {
        self.code.trim().is_empty()
            && self.debit.trim().is_empty()
            && self.credit.trim().is_empty()
            && self.summary.trim().is_empty()
    }
    pub fn debit(&self) -> Money {
        Money::parse_or_zero(&self.debit)
    }
    pub fn credit(&self) -> Money {
        Money::parse_or_zero(&self.credit)
    }
}

/// 未保存修改确认后的载入目标
#[derive(Clone, Copy)]
enum PendingLoad {
    /// 载入一张空白新凭证
    New,
    /// 重新载入指定凭证
    Existing(i64),
}

pub struct VoucherEdit {
    pub id: i64,
    pub status: VoucherStatus,
    pub period: Period,
    pub date: String,
    pub word: String,
    pub no: String,
    pub attachments: String,
    pub memo: String,
    pub prepared_by: String,
    pub rows: Vec<Row>,
    pub loaded: bool,
    pub dirty: bool,
    /// 载入/保存时的表单指纹：与当前不一致即视为有未保存修改
    loaded_sig: u64,
    /// 有未保存修改时，等待用户选择是否放弃
    confirm_discard: Option<PendingLoad>,
    pub picker: AccountPickerState,
    /// 正在编辑辅助核算的行号
    pub aux_row: Option<usize>,
    pub print_open: bool,
    pub words: Vec<String>,
    /// 常用摘要弹窗
    pub summary_popup: bool,
    /// 摘要弹窗搜索词
    pub summary_search: String,
    /// 当前焦点所在的摘要行（弹窗选中的摘要填进这一行）
    pub focus_row: Option<usize>,
}

impl Default for VoucherEdit {
    fn default() -> Self {
        Self {
            id: 0,
            status: VoucherStatus::Draft,
            period: Period::default(),
            date: chrono::Local::now().date_naive().format("%Y-%m-%d").to_string(),
            word: "记".to_string(),
            no: "1".to_string(),
            attachments: String::new(),
            memo: String::new(),
            prepared_by: String::new(),
            rows: Vec::new(),
            loaded: false,
            dirty: false,
            loaded_sig: 0,
            confirm_discard: None,
            picker: AccountPickerState::default(),
            aux_row: None,
            print_open: false,
            words: Vec::new(),
            summary_popup: false,
            summary_search: String::new(),
            focus_row: None,
        }
    }
}

impl VoucherEdit {
    pub fn invalidate(&mut self) {
        self.loaded = false;
    }

    /// 表单指纹：用于判断是否有未保存的修改（切换页面/期间/凭证前确认）
    fn signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.id.hash(&mut h);
        format!("{:?}", self.status).hash(&mut h);
        self.period.ymm().hash(&mut h);
        self.date.hash(&mut h);
        self.word.hash(&mut h);
        self.no.hash(&mut h);
        self.attachments.hash(&mut h);
        self.memo.hash(&mut h);
        for r in &self.rows {
            r.summary.hash(&mut h);
            r.code.hash(&mut h);
            r.debit.hash(&mut h);
            r.credit.hash(&mut h);
            r.qty.hash(&mut h);
            r.price.hash(&mut h);
            r.currency.hash(&mut h);
            r.rate.hash(&mut h);
            r.amount_for.hash(&mut h);
            r.cf.hash(&mut h);
            r.aux.key().hash(&mut h);
        }
        h.finish()
    }

    fn has_unsaved(&self) -> bool {
        self.loaded_sig != self.signature()
    }

    /// 请求载入：有未保存修改时先弹确认条，不静默丢弃
    fn request_load(&mut self, ctx: &mut AppCtx<'_>, target: PendingLoad) {
        if self.has_unsaved() {
            self.confirm_discard = Some(target);
        } else {
            self.confirm_discard = None;
            match target {
                PendingLoad::New => self.load(ctx, None),
                PendingLoad::Existing(id) => self.load(ctx, Some(id)),
            }
        }
    }

    /// 载入凭证；id 为 None 表示新建
    pub fn load(&mut self, ctx: &mut AppCtx<'_>, id: Option<i64>) {
        self.words = ctx.db().voucher_words();
        if self.words.is_empty() {
            self.words = fincore::chart::default_voucher_words();
        }
        self.rows.clear();
        self.aux_row = None;

        match id {
            Some(vid) => match findb::vouchers::get(ctx.db(), vid) {
                Ok(Some(v)) => {
                    self.id = v.id;
                    self.status = v.status;
                    self.period = v.period;
                    self.date = v.date.format("%Y-%m-%d").to_string();
                    self.word = v.word.clone();
                    self.no = v.no.to_string();
                    self.attachments = if v.attachments > 0 {
                        v.attachments.to_string()
                    } else {
                        String::new()
                    };
                    self.memo = v.memo.clone();
                    self.prepared_by = v.prepared_by.clone();
                    for e in &v.entries {
                        self.rows.push(Row {
                            summary: e.summary.clone(),
                            code: e.account_code.clone(),
                            debit: if e.debit.is_zero() {
                                String::new()
                            } else {
                                e.debit.fmt_plain()
                            },
                            credit: if e.credit.is_zero() {
                                String::new()
                            } else {
                                e.credit.fmt_plain()
                            },
                            aux: e.aux.clone(),
                            qty: e.qty.map(|q| q.fmt_plain()).unwrap_or_default(),
                            price: e.price.map(|q| q.fmt_plain()).unwrap_or_default(),
                            currency: e.currency.clone().unwrap_or_default(),
                            rate: e.rate.map(|r| r.to_string()).unwrap_or_default(),
                            amount_for: e.amount_for.map(|m| m.fmt_plain()).unwrap_or_default(),
                            cf: e.aux.cash_flow.clone().unwrap_or_default(),
                        });
                    }
                }
                Ok(None) => {
                    ctx.error("凭证不存在，可能已被删除");
                    self.new_voucher(ctx);
                }
                Err(e) => {
                    ctx.error(e.to_string());
                    self.new_voucher(ctx);
                }
            },
            None => self.new_voucher(ctx),
        }

        while self.rows.len() < 4 {
            self.rows.push(Row::default());
        }
        self.loaded = true;
        self.dirty = false;
        self.loaded_sig = self.signature();
        self.confirm_discard = None;
    }

    fn new_voucher(&mut self, ctx: &mut AppCtx<'_>) {
        let p = ctx.period();
        let date = if p.contains(chrono::Local::now().date_naive()) {
            chrono::Local::now().date_naive()
        } else {
            p.last_day()
        };
        let no = findb::vouchers::next_no(ctx.db(), p, &self.word).unwrap_or(1);
        self.id = 0;
        self.status = VoucherStatus::Draft;
        self.period = p;
        self.date = date.format("%Y-%m-%d").to_string();
        self.no = no.to_string();
        self.attachments.clear();
        self.memo.clear();
        self.prepared_by = ctx.user().username.clone();
    }

    // ------------------------------------------------------------------
    // 组装与校验
    // ------------------------------------------------------------------
    fn parse_date(&self) -> Result<NaiveDate, String> {
        NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d")
            .map_err(|_| format!("日期格式不正确：{}（应为 2026-01-31）", self.date))
    }

    pub fn debit_total(&self) -> Money {
        self.rows.iter().map(|r| r.debit()).fold(Money::ZERO, |a, b| a + b)
    }
    pub fn credit_total(&self) -> Money {
        self.rows.iter().map(|r| r.credit()).fold(Money::ZERO, |a, b| a + b)
    }

    fn to_voucher(&self) -> Result<Voucher, String> {
        let date = self.parse_date()?;
        let period = Period::from_date(date);
        let no: i32 = self.no.trim().parse().map_err(|_| "凭证号必须是整数".to_string())?;
        if no <= 0 {
            return Err("凭证号必须大于 0".to_string());
        }
        let attachments: i32 = if self.attachments.trim().is_empty() {
            0
        } else {
            self.attachments
                .trim()
                .parse()
                .map_err(|_| "附件张数必须是整数".to_string())?
        };

        let mut v = Voucher::new(period, date, self.word.trim().to_string(), no);
        v.id = self.id;
        v.attachments = attachments;
        v.memo = self.memo.trim().to_string();
        v.prepared_by = self.prepared_by.clone();
        for r in &self.rows {
            if r.is_blank() {
                continue;
            }
            let mut e = Entry::new(0, r.code.trim().to_string(), r.summary.trim().to_string());
            e.debit = r.debit().round2();
            e.credit = r.credit().round2();
            e.aux = r.aux.clone();
            if !r.cf.trim().is_empty() {
                e.aux.cash_flow = Some(r.cf.trim().to_string());
            }
            if !r.qty.trim().is_empty() {
                e.qty = Some(Money::parse_or_zero(&r.qty));
            }
            if !r.price.trim().is_empty() {
                e.price = Some(Money::parse_or_zero(&r.price));
            }
            // 外币要素（科目启用外币时由引擎校验原币×汇率≈本位币金额）
            if !r.currency.trim().is_empty() {
                e.currency = Some(r.currency.trim().to_ascii_uppercase());
            }
            if !r.rate.trim().is_empty() {
                e.rate = Some(Money::parse_or_zero(&r.rate).inner());
            }
            if !r.amount_for.trim().is_empty() {
                e.amount_for = Some(Money::parse_or_zero(&r.amount_for));
            }
            v.push_entry(e);
        }
        v.renumber();
        Ok(v)
    }

    /// 保存。keep=false 表示保存后清空界面继续新增
    /// 常用摘要选择弹窗：按使用热度倒序，点一条填进焦点行（或第一个空行）
    fn show_summary_popup(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if !self.summary_popup {
            return;
        }
        let mut open = true;
        let mut picked: Option<String> = None;
        egui::Window::new("常用摘要")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([360.0, 420.0])
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    ui.label("查找：");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.summary_search)
                            .hint_text("输入关键词过滤")
                            .desired_width(220.0),
                    );
                });
                let kw = self.summary_search.trim();
                let items = if kw.is_empty() {
                    findb::summaries::top(ctx.db(), 50).unwrap_or_default()
                } else {
                    findb::summaries::search(ctx.db(), kw)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|s| s.text)
                        .collect()
                };
                ui.label(RichText::new(format!("{} 条，点击填入当前摘要行", items.len())).weak());
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for t in &items {
                        if ui.selectable_label(false, t).clicked() {
                            picked = Some(t.clone());
                        }
                    }
                });
            });
        if !open {
            self.summary_popup = false;
        }
        if let Some(t) = picked {
            let row = self.focus_row.unwrap_or_else(|| {
                self.rows
                    .iter()
                    .position(|r| r.summary.trim().is_empty())
                    .unwrap_or(0)
            });
            if let Some(r) = self.rows.get_mut(row) {
                r.summary = t;
            }
            self.summary_popup = false;
        }
    }

    /// 附件区：小于 256KB 内联存 SQLite，大文件落账套同目录 .attachments/
    fn show_attach_panel(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui, readonly: bool) {
        let id = self.id;
        let atts = if id > 0 {
            findb::attach::list(ctx.db(), id).unwrap_or_default()
        } else {
            Vec::new()
        };
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("附件").strong());
            ui.label(
                RichText::new(format!("{} 个（表头「附件」栏的数字表示原始凭证张数）", atts.len()))
                    .weak(),
            );
            if id == 0 {
                ui.label(RichText::new("保存凭证后才能上传附件").weak());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("📎 上传文件").clicked() && !readonly && id > 0 {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        match findb::attach::add_file(ctx.db(), id, &path, &ctx.user().username) {
                            Ok(_) => {
                                ctx.log("凭证", "上传附件", &path.display().to_string());
                                ctx.info("已添加附件");
                            }
                            Err(e) => ctx.error(e.to_string()),
                        }
                    }
                }
            });
        });
        if !atts.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for a in &atts {
                    ui.horizontal(|ui| {
                        let icon = if a.is_image() { "🖼" } else { "📄" };
                        ui.label(RichText::new(format!("{icon} {}", a.name)).weak());
                        ui.label(RichText::new(a.size_text()).weak().small());
                        if ui.small_button("开").on_hover_text("导出到临时目录并打开").clicked() {
                            match findb::attach::read(ctx.db(), a.id) {
                                Ok(data) => {
                                    let p = findb::attach::temp_export_path(&a.name);
                                    if let Some(dir) = p.parent() {
                                        let _ = std::fs::create_dir_all(dir);
                                    }
                                    match std::fs::write(&p, &data) {
                                        Ok(()) => {
                                            let _ = std::process::Command::new("xdg-open")
                                                .arg(&p)
                                                .spawn();
                                        }
                                        Err(e) => ctx.error(format!("写出附件失败：{e}")),
                                    }
                                }
                                Err(e) => ctx.error(e.to_string()),
                            }
                        }
                        if ui.small_button("删").clicked() && !readonly {
                            match findb::attach::delete(ctx.db(), a.id) {
                                Ok(()) => ctx.info("已删除附件"),
                                Err(e) => ctx.error(e.to_string()),
                            }
                        }
                        ui.add_space(6.0);
                    });
                }
            });
        }
    }

    fn save(&mut self, ctx: &mut AppCtx<'_>, keep: bool) {
        if !self.status.can_edit() {
            ctx.error("已作废的凭证不能修改");
            return;
        }
        // 保存入口必须自己做权限校验：顶栏可达本页，侧栏的门槛在这里不生效
        let needed = if self.id == 0 { Perm::VoucherNew } else { Perm::VoucherEdit };
        if !ctx.can(needed) {
            ctx.error(format!("没有「{}」权限", needed.label()));
            return;
        }
        let mut v = match self.to_voucher() {
            Ok(v) => v,
            Err(e) => {
                ctx.error(e);
                return;
            }
        };
        if v.entries.is_empty() {
            ctx.error("请至少录入一行有效分录");
            return;
        }
        if self.id == 0 {
            v.prepared_by = ctx.user().username.clone();
        }

        // 校验（含期间锁定、借贷平衡、科目末级、辅助核算必录等）
        let closed_res = findb::periods::closed_upto(ctx.db());
        let closed = match closed_res {
            Ok(c) => c,
            Err(e) => {
                ctx.error(format!("读取结账线失败：{e}"));
                return;
            }
        };
        let res = {
            let db = ctx.db();
            let opts = db.options();
            validate_voucher(&v, &ValidateCtx::new(ctx.chart(), &opts, closed)).into_result()
        };
        if let Err(e) = res {
            ctx.error(e.to_string());
            return;
        }

        // 凭证号重复检查
        if let Ok(true) = findb::vouchers::no_taken(ctx.db(), v.period, &v.word, v.no, v.id) {
            ctx.error(format!("{}-{:04} 已存在，请更换凭证号", v.word, v.no));
            return;
        }

        // 保存为「未记账」，核对无误后点「记账」确认入账（与 Web 端一致，无审核环节）
        match findb::vouchers::save(ctx.db(), &mut v) {
            Ok(id) => {
                let label = v.voucher_no();
                ctx.log(
                    "凭证",
                    if self.id == 0 { "新增凭证" } else { "修改凭证" },
                    &format!("{label} 借贷 {}", v.debit_total().fmt_money()),
                );
                ctx.info(format!("已保存 {label}"));
                // 摘要热度 +1，录入界面按热度倒序补全
                let sums: Vec<String> = self
                    .rows
                    .iter()
                    .map(|r| r.summary.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let _ = findb::summaries::bump_many(ctx.db(), &sums);
                self.id = id;
                if keep {
                    self.load(ctx, Some(id));
                } else {
                    let word = self.word.clone();
                    self.load(ctx, None);
                    self.word = word;
                }
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    fn reload_after(&mut self, ctx: &mut AppCtx<'_>) {
        let id = self.id;
        if id > 0 {
            self.request_load(ctx, PendingLoad::Existing(id));
        }
    }

    /// 补平差额
    fn balance(&mut self, ctx: &mut AppCtx<'_>) {
        let diff = (self.debit_total() - self.credit_total()).round2();
        if diff.is_zero() {
            ctx.info("借贷已经平衡");
            return;
        }
        let target = self
            .rows
            .iter_mut()
            .rev()
            .find(|r| !r.code.trim().is_empty());
        let need_new = match target {
            Some(r) => {
                if diff.is_positive() {
                    let cur = r.credit();
                    r.credit = (cur + diff).fmt_plain();
                } else {
                    let cur = r.debit();
                    r.debit = (cur + diff.negated()).fmt_plain();
                }
                false
            }
            None => true,
        };
        if need_new {
            let mut r = Row::default();
            if diff.is_positive() {
                r.credit = diff.fmt_plain();
            } else {
                r.debit = diff.negated().fmt_plain();
            }
            self.rows.push(r);
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------------
    // 界面
    // ------------------------------------------------------------------
    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        // 外部失效（切期间/切页面）时若表单有未保存修改，先让用户确认
        if !self.loaded && self.confirm_discard.is_none() && self.has_unsaved() {
            self.confirm_discard = Some(PendingLoad::New);
        }
        if let Some(target) = self.confirm_discard {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    palette::WARN,
                    "当前凭证有未保存的修改，继续将丢失这些内容。",
                );
                if ui.button("放弃修改").clicked() {
                    self.confirm_discard = None;
                    match target {
                        PendingLoad::New => self.load(ctx, None),
                        PendingLoad::Existing(id) => self.load(ctx, Some(id)),
                    }
                }
                if ui.button("继续编辑").clicked() {
                    self.confirm_discard = None;
                    // 保留当前编辑内容：把表单标记为已载入，避免下一帧被重新加载覆盖
                    self.loaded = true;
                }
            });
            ui.separator();
            // 等用户选择期间不渲染编辑表单，避免"看起来还能改"
            if self.confirm_discard.is_some() {
                return;
            }
        }
        // 出纳日记账「登记收付」预填：载入空白凭证并置入科目（金额由出纳填写）
        if let Some(code) = ctx.st.pending_cash.take() {
            self.load(ctx, None);
            if let Some(r) = self.rows.first_mut() {
                r.code = code.clone();
            }
            // 预填视为已载入快照，避免"有未保存修改"确认误触发
            self.loaded = true;
            ctx.info(format!("已预置科目 {code}，请填写金额与摘要"));
        }
        if !self.loaded {
            self.load(ctx, None);
        }
        self.dirty = self.has_unsaved();
        let readonly = !self.status.can_edit();
        let ectx = ui.ctx().clone();

        // ---------------- 表头 ----------------
        widgets::page_header(ui, if self.id == 0 { "填制凭证" } else { "修改凭证" }, |ui| {
            ui.label(
                RichText::new(format!("状态：{}", self.status.label()))
                    .color(theme::status_color(self.status.counts()))
                    .strong(),
            );
        });

        widgets::toolbar(ui, |ui| {
            let save_clicked = ui.button("保存").on_hover_text("Ctrl+S").clicked()
                || (ui.input(|i| {
                    i.modifiers.ctrl && i.key_pressed(egui::Key::S)
                }));
            if save_clicked && !readonly {
                self.save(ctx, true);
            }
            if ui.button("保存并新增").clicked() && !readonly {
                self.save(ctx, false);
            }
            ui.separator();
            if ui.button("新增").clicked() {
                self.request_load(ctx, PendingLoad::New);
            }
            if ui.button("增行").clicked() && !readonly {
                self.rows.push(Row::default());
            }
            if ui.button("常用摘要").on_hover_text("从摘要库选一条填入当前行").clicked() {
                self.summary_popup = !self.summary_popup;
            }
            if ui.button("删空行").clicked() && !readonly {
                self.rows.retain(|r| !r.is_blank());
                while self.rows.len() < 4 {
                    self.rows.push(Row::default());
                }
            }
            if ui.button("自动平衡").clicked() && !readonly {
                self.balance(ctx);
            }
            ui.separator();
            // 审核环节（可选）：启用后未记账 → 已审核 → 已记账；未启用时可直接记账
            if ui.button("审核").clicked()
                && self.id > 0
                && self.status == VoucherStatus::Draft
            {
                if ctx.can(Perm::VoucherAudit) {
                    do_status(ctx, self.id, "audit");
                    self.reload_after(ctx);
                }
            }
            if ui.button("反审核").clicked()
                && self.id > 0
                && self.status == VoucherStatus::Audited
            {
                if ctx.can(Perm::VoucherUnaudit) {
                    do_status(ctx, self.id, "unaudit");
                    self.reload_after(ctx);
                }
            }
            // 出纳签字（可选前置：账套开启 require_cashier 后，现金/银行凭证记账前须签字）
            if ui.button("出纳签字").clicked()
                && self.id > 0
                && matches!(self.status, VoucherStatus::Draft | VoucherStatus::Audited)
            {
                if ctx.can(Perm::CashierSign) {
                    do_status(ctx, self.id, "sign");
                    self.reload_after(ctx);
                }
            }
            if ui.button("取消签字").clicked()
                && self.id > 0
                && matches!(self.status, VoucherStatus::Draft | VoucherStatus::Audited)
            {
                if ctx.can(Perm::CashierSign) {
                    do_status(ctx, self.id, "unsign");
                    self.reload_after(ctx);
                }
            }
            // 与 Web 端一致：未审核（或未启用审核）的凭证核对后手动记账确认入账
            if ui.button("记账").clicked() && self.status.can_post() {
                if ctx.can(Perm::VoucherPost) {
                    do_status(ctx, self.id, "post");
                    self.reload_after(ctx);
                }
            }
            if ui.button("反记账").clicked() && self.status.can_unpost() {
                if ctx.can(Perm::VoucherUnpost) {
                    do_status(ctx, self.id, "unpost");
                    self.reload_after(ctx);
                }
            }
            if ui.button("红字冲销").clicked() && self.id > 0 && self.status != VoucherStatus::Void {
                if ctx.can(Perm::VoucherNew) {
                    let period = ctx.period();
                    let date = chrono::Local::now().date_naive();
                    match findb::vouchers::reverse(ctx.db(), self.id, &ctx.user().username, period, date) {
                        Ok(nid) => {
                            ctx.info(format!("已生成冲销凭证 #{nid}"));
                            ctx.log("凭证", "红字冲销", &format!("凭证 #{nid}"));
                            self.dirty = true;
                        }
                        Err(e) => ctx.error(e.to_string()),
                    }
                }
            }
            ui.separator();
            if ui.button("上一张").clicked() {
                self.step(ctx, -1);
            }
            if ui.button("下一张").clicked() {
                self.step(ctx, 1);
            }
            ui.separator();
            if ui.button("打印预览").clicked() {
                self.print_open = true;
            }
            if ui.button("删除").clicked() && self.id > 0 {
                if !ctx.can(Perm::VoucherDelete) {
                    ctx.error(format!("没有「{}」权限", Perm::VoucherDelete.label()));
                } else {
                    ctx.confirm_dangerous(
                        "删除凭证",
                        &format!("确定删除 {} 吗？删除后凭证号会留下断号。", self.no_text()),
                        crate::state::ConfirmAction::DeleteVoucher(self.id),
                        true,
                    );
                }
            }
        });

        // ---------------- 凭证头 ----------------
        ui.horizontal(|ui| {
            ui.label("日期");
            ui.add_sized(
                [100.0, 22.0],
                egui::TextEdit::singleline(&mut self.date).hint_text("YYYY-MM-DD"),
            );
            if !readonly {
                // 日历选择器：点击弹出日历，选中后回写日期字符串；仍可手动输入
                let mut d = NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d")
                    .unwrap_or_else(|_| chrono::Local::now().date_naive());
                if ui
                    .add(DatePickerButton::new(&mut d).id_salt("vch_date"))
                    .changed()
                {
                    self.date = d.format("%Y-%m-%d").to_string();
                }
                if ui.button("今天").clicked() {
                    self.date = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
                }
                // 跨期提示：所选日期与当前期间不一致时提前告知，保存将按日期归入对应期间
                if let Ok(d) = NaiveDate::parse_from_str(self.date.trim(), "%Y-%m-%d") {
                    let p = Period::from_date(d);
                    if p != ctx.period() {
                        ui.label(
                            RichText::new(format!("该日期属于 {} 期，保存后归入该期间", p.label()))
                                .color(palette::WARN)
                                .small(),
                        );
                    }
                }
            }
            ui.label("凭证字");
            if readonly || self.words.is_empty() {
                ui.add_sized(
                    [60.0, 22.0],
                    egui::TextEdit::singleline(&mut self.word),
                );
            } else {
                egui::ComboBox::from_id_salt("vch_word")
                    .selected_text(&self.word)
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for wd in &self.words {
                            ui.selectable_value(&mut self.word, wd.clone(), wd);
                        }
                    });
            }
            ui.label("凭证号");
            ui.add_sized([60.0, 22.0], egui::TextEdit::singleline(&mut self.no));
            ui.label("附件");
            ui.add_sized([50.0, 22.0], egui::TextEdit::singleline(&mut self.attachments));
            ui.label("制单");
            ui.label(RichText::new(&self.prepared_by).weak());
        });
        ui.horizontal(|ui| {
            ui.label("备注");
            ui.add_sized(
                [ui.available_width() - 8.0, 22.0],
                egui::TextEdit::singleline(&mut self.memo).hint_text("可选"),
            );
        });

        self.show_summary_popup(ctx, ui);
        self.show_attach_panel(ctx, ui, readonly);

        if readonly {
            ui.colored_label(
                palette::WARN,
                "该凭证已记账或已作废，处于只读状态（已记账请先反记账后再修改）。",
            );
        }

        ui.separator();

        // ---------------- 分录表 ----------------
        let n = self.rows.len();
        let chart = ctx.chart();
        let aux_names = &ctx.st.aux_names;
        let mut open_picker: Option<usize> = None;
        let mut open_aux: Option<usize> = None;
        let mut remove_row: Option<usize> = None;

        let tb = TableBuilder::new(ui)
            .id_salt("voucher_entries")
            .striped(false)
            .resizable(true)
            .min_scrolled_height(120.0)
            .column(Column::initial(32.0).resizable(false))
            .column(Column::remainder().at_least(150.0))
            .column(Column::initial(300.0).at_least(180.0))
            .column(Column::initial(150.0).at_least(90.0))
            .column(Column::initial(130.0).at_least(90.0))
            .column(Column::initial(130.0).at_least(90.0))
            .column(Column::initial(28.0).resizable(false));

        tb.header(26.0, |mut h| {
            let heads = ["#", "摘　要", "会计科目", "辅助核算", "借方金额", "贷方金额", ""];
            for t in heads {
                h.col(|ui| {
                    ui.label(RichText::new(t).strong());
                });
            }
        })
        .body(|body| {
            body.rows(26.0, n, |mut row| {
                let i = row.index();
                let r = &mut self.rows[i];
                row.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("{}", i + 1)).weak());
                    });
                });
                // 摘要
                row.col(|ui| {
                    let resp = ui.add_sized(
                        [ui.available_width(), 22.0],
                        egui::TextEdit::singleline(&mut r.summary).hint_text("摘要"),
                    );
                    if resp.has_focus() {
                        self.focus_row = Some(i);
                    }
                });
                // 科目
                row.col(|ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui
                            .add_sized(
                                [92.0, 22.0],
                                egui::TextEdit::singleline(&mut r.code).hint_text("编码"),
                            )
                            .changed()
                        {
                            // 输入完整编码后自动带出默认现金流量项目
                            if let Some(a) = chart.get(r.code.trim()) {
                                if let Some(cf) = &a.cash_flow_item {
                                    if r.cf.is_empty() {
                                        r.cf = cf.clone();
                                    }
                                }
                            }
                        }
                        let name = chart
                            .get(r.code.trim())
                            .map(|a| a.name.clone())
                            .unwrap_or_else(|| {
                                if r.code.trim().is_empty() {
                                    String::new()
                                } else {
                                    "⟨未知科目⟩".to_string()
                                }
                            });
                        let color = if name.starts_with("⟨") {
                            palette::CREDIT
                        } else {
                            Color32::DARK_GRAY
                        };
                        if ui
                            .add(
                                egui::Button::new(RichText::new(&name).color(color))
                                    .min_size(egui::vec2(
                                        (ui.available_width() - 30.0).max(60.0),
                                        22.0,
                                    )),
                            )
                            .clicked()
                        {
                            open_picker = Some(i);
                        }
                        if ui.small_button("…").clicked() {
                            open_picker = Some(i);
                        }
                    });
                });
                // 辅助核算
                row.col(|ui| {
                    let acct = chart.get(r.code.trim());
                    let need = acct
                        .map(|a| !a.aux.is_empty() || a.is_cash || a.is_bank)
                        .unwrap_or(false);
                    let txt = aux_text(&r.aux, &aux_names);
                    let label = if txt.is_empty() {
                        if need {
                            "（必录）".to_string()
                        } else {
                            String::new()
                        }
                    } else {
                        txt.clone()
                    };
                    let color = if need && txt.is_empty() {
                        palette::WARN
                    } else {
                        Color32::DARK_GRAY
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(&label).color(color))
                                .min_size(egui::vec2(ui.available_width().max(50.0), 22.0)),
                        )
                        .clicked()
                    {
                        open_aux = Some(i);
                    }
                });
                // 借方
                row.col(|ui| {
                    if ui
                        .add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut r.debit)
                                .horizontal_align(Align::RIGHT)
                                .hint_text("0.00"),
                        )
                        .changed()
                        && !r.debit.trim().is_empty()
                    {
                        r.credit.clear();
                    }
                });
                // 贷方
                row.col(|ui| {
                    if ui
                        .add_sized(
                            [ui.available_width(), 22.0],
                            egui::TextEdit::singleline(&mut r.credit)
                                .horizontal_align(Align::RIGHT)
                                .hint_text("0.00"),
                        )
                        .changed()
                        && !r.credit.trim().is_empty()
                    {
                        r.debit.clear();
                    }
                });
                // 删除行
                row.col(|ui| {
                    if ui.small_button("×").on_hover_text("删除该行").clicked() {
                        remove_row = Some(i);
                    }
                });
            });
        });

        if let Some(i) = remove_row {
            if self.rows.len() > 1 {
                self.rows.remove(i);
            }
            if self.aux_row == Some(i) {
                self.aux_row = None;
            }
        }
        if let Some(i) = open_picker {
            self.picker.row = i;
            self.picker.open = true;
            self.picker.leaf_only = true;
        }
        if let Some(i) = open_aux {
            self.aux_row = Some(i);
        }

        // ---------------- 合计 ----------------
        ui.separator();
        let d = self.debit_total();
        let c = self.credit_total();
        let diff = (d - c).round2();
        ui.horizontal(|ui| {
            ui.label(RichText::new("合计").strong());
            ui.add_space(20.0);
            ui.label(RichText::new(format!("借方 {}", d.fmt_money())).strong());
            ui.add_space(20.0);
            ui.label(RichText::new(format!("贷方 {}", c.fmt_money())).strong());
            ui.add_space(20.0);
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
                    RichText::new(format!("人民币大写：{}", d.to_capital()))
                        .weak()
                        .size(13.0),
                );
            });
        });

        // ---------------- 弹窗 ----------------
        if let Some(code) = self.picker.show(&ectx, ctx.chart()) {
            let i = self.picker.row;
            if let Some(r) = self.rows.get_mut(i) {
                r.code = code.clone();
                if let Some(a) = ctx.chart().get(&code) {
                    if let Some(cf) = &a.cash_flow_item {
                        r.cf = cf.clone();
                    }
                }
                self.dirty = true;
            }
        }
        self.aux_window(ctx, ui);
        self.print_window(ctx, ui);
    }

    fn no_text(&self) -> String {
        format!("{}-{:04}", self.word, self.no.parse::<i32>().unwrap_or(0))
    }

    // ---------------- 辅助核算编辑窗 ----------------
    fn aux_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(i) = self.aux_row else { return };
        if i >= self.rows.len() {
            self.aux_row = None;
            return;
        }
        let code = self.rows[i].code.trim().to_string();
        let mut open = true;
        let mut close = false;
        let title = format!("辅助核算 — 第 {} 行 {}", i + 1, code);
        let mut new_aux = self.rows[i].aux.clone();
        let mut new_cf = self.rows[i].cf.clone();

        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                let acct = ctx.chart().get(&code).cloned();
                let dims: Vec<AuxKind> = acct
                    .as_ref()
                    .map(|a| a.aux.list())
                    .unwrap_or_default();
                if dims.is_empty()
                    && !acct.as_ref().map(|a| a.is_cash || a.is_bank).unwrap_or(false)
                {
                    ui.label(RichText::new("该科目未启用辅助核算。").weak());
                }
                for k in dims {
                    ui.horizontal(|ui| {
                        ui.set_min_width(90.0);
                        ui.label(format!("{}：", k.label()));
                        let cur = new_aux.get(k).cloned();
                        let opts = findb::auxs::codes(ctx.db(), k).unwrap_or_default();
                        let mut sel = cur.clone().unwrap_or_default();
                        let label = match &cur {
                            Some(c) => format!("{} {}", c, aux_name(ctx.db(), k, c)),
                            None => "（不核算）".to_string(),
                        };
                        egui::ComboBox::from_id_salt(format!("aux_{:?}", k))
                            .selected_text(label)
                            .width(260.0)
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_value(&mut sel, String::new(), "（不核算）")
                                    .clicked()
                                {}
                                for c in &opts {
                                    let t = format!("{} {}", c, aux_name(ctx.db(), k, c));
                                    ui.selectable_value(&mut sel, c.clone(), t);
                                }
                            });
                        if sel.is_empty() {
                            new_aux.set(k, None);
                        } else if Some(&sel) != cur.as_ref() {
                            new_aux.set(k, Some(sel));
                        }
                    });
                }
                // 现金流量项目
                if acct.as_ref().map(|a| a.is_cash || a.is_bank).unwrap_or(false) {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.set_min_width(90.0);
                        ui.label("现金流量：");
                        let items: Vec<CashFlowItem> =
                            findb::reports::cash_flow_items(ctx.db()).unwrap_or_default();
                        egui::ComboBox::from_id_salt("aux_cf")
                            .selected_text(if new_cf.is_empty() {
                                "（未指定）".to_string()
                            } else {
                                items
                                    .iter()
                                    .find(|x| x.code == new_cf)
                                    .map(|x| format!("{} {}", x.code, x.name))
                                    .unwrap_or_else(|| new_cf.clone())
                            })
                            .width(300.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut new_cf, String::new(), "（未指定）");
                                for it in &items {
                                    if it.disabled {
                                        continue;
                                    }
                                    ui.selectable_value(
                                        &mut new_cf,
                                        it.code.clone(),
                                        format!("{} {}", it.code, it.name),
                                    );
                                }
                            });
                    });
                    ui.label(
                        RichText::new("现金/银行科目的现金流量项目用于生成现金流量表")
                            .weak()
                            .size(12.0),
                    );
                }
                // 数量金额式
                if acct.as_ref().map(|a| a.has_qty).unwrap_or(false) {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("数量");
                        ui.add_sized([100.0, 22.0], egui::TextEdit::singleline(&mut self.rows[i].qty));
                        ui.label("单价");
                        ui.add_sized(
                            [100.0, 22.0],
                            egui::TextEdit::singleline(&mut self.rows[i].price),
                        );
                    });
                }
                // 外币核算：科目配置了币种，或账套启用了外币
                let has_cur = acct.as_ref().map(|a| a.currency.is_some()).unwrap_or(false)
                    || ctx.db().options().enable_foreign;
                if has_cur {
                    if self.rows[i].currency.trim().is_empty() {
                        if let Some(c) = acct.as_ref().and_then(|a| a.currency.clone()) {
                            self.rows[i].currency = c;
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("币种");
                        ui.add_sized(
                            [70.0, 22.0],
                            egui::TextEdit::singleline(&mut self.rows[i].currency).hint_text("USD"),
                        );
                        ui.label("汇率");
                        ui.add_sized(
                            [90.0, 22.0],
                            egui::TextEdit::singleline(&mut self.rows[i].rate)
                                .hint_text("1 外币 = ?"),
                        );
                        ui.label("原币金额");
                        ui.add_sized(
                            [110.0, 22.0],
                            egui::TextEdit::singleline(&mut self.rows[i].amount_for),
                        );
                    });
                    ui.label(
                        RichText::new("原币金额 × 汇率 应等于本行本位币金额（允许 0.01 舍入差）")
                            .weak()
                            .size(12.0),
                    );
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("关闭").clicked() {
                            close = true;
                        }
                        if ui.button("清除全部").clicked() {
                            new_aux = AuxRef::default();
                            new_cf.clear();
                        }
                    });
                });
            });

        self.rows[i].aux = new_aux;
        self.rows[i].cf = new_cf;
        if close || !open {
            self.aux_row = None;
        }
    }

    // ---------------- 打印预览 ----------------
    fn print_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if !self.print_open {
            return;
        }
        let mut open = true;
        let v = self.to_voucher().ok();
        let company = ctx.db().options().company;
        let mut do_export = false;
        egui::Window::new("凭证打印预览")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([760.0, 560.0])
            .show(ui.ctx(), |ui| {
                egui::ScrollArea::both().show(ui, |ui| match &v {
                    None => {
                        ui.colored_label(palette::CREDIT, "当前内容不完整，无法打印预览");
                    }
                    Some(v) => {
                        print_body(ui, v, ctx.chart(), &company);
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("关闭").clicked() {
                            self.print_open = false;
                        }
                        // 凭证的 Excel 导出属于"数据落地"，仅管理员与财务主管可用；
                        // 屏幕内的打印预览（print_body）人人可见，满足"只能打印不能导出"。
                        if ctx.user().can(Perm::Export) {
                            if ui.button("导出 Excel").clicked() {
                                do_export = true;
                            }
                        }
                    });
                });
            });
        if !open {
            self.print_open = false;
        }
        if do_export {
            export_print(ctx, v.as_ref());
        }
    }

    // ---------------- 上下张 ----------------
    fn step(&mut self, ctx: &mut AppCtx<'_>, dir: i32) {
        let list = match findb::vouchers::list(ctx.db(), &findb::vouchers::VoucherQuery::period(self.period)) {
            Ok(l) => l,
            Err(e) => {
                ctx.error(e.to_string());
                return;
            }
        };
        if list.is_empty() {
            ctx.info("本期还没有凭证");
            return;
        }
        let pos = list.iter().position(|v| v.id == self.id);
        let next = match pos {
            None => {
                if dir > 0 {
                    0
                } else {
                    list.len() - 1
                }
            }
            Some(i) => {
                if dir > 0 {
                    (i + 1).min(list.len() - 1)
                } else {
                    i.saturating_sub(1)
                }
            }
        };
        let id = list[next].id;
        self.request_load(ctx, PendingLoad::Existing(id));
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 凭证状态流转（审核/记账/作废等），集中处理以便统一写日志
pub fn do_status(ctx: &mut AppCtx<'_>, id: i64, what: &str) {
    if id <= 0 {
        ctx.error("请先保存凭证");
        return;
    }
    // 与凭证 prepared_by/posted_by 的存储口径一致：登录账号 username
    let who = ctx.user().username.clone();
    let r = match what {
        "post" => findb::vouchers::post(ctx.db(), id, &who),
        "unpost" => findb::vouchers::unpost(ctx.db(), id),
        "audit" => findb::vouchers::audit(ctx.db(), id, &who),
        "unaudit" => findb::vouchers::unaudit(ctx.db(), id, &who),
        "sign" => findb::vouchers::sign(ctx.db(), id, &who),
        "unsign" => findb::vouchers::unsign(ctx.db(), id, &who),
        _ => return,
    };
    match r {
        Ok(()) => {
            let label: &str = match what {
                "post" => "记账",
                "unpost" => "反记账",
                "audit" => "审核",
                "unaudit" => "反审核",
                "sign" => "出纳签字",
                "unsign" => "取消签字",
                _ => "状态变更",
            };
            ctx.log("凭证", label, &format!("凭证 #{id}"));
            ctx.info("操作成功");
        }
        Err(e) => ctx.error(e.to_string()),
    }
}

fn aux_name(db: &findb::Db, k: AuxKind, code: &str) -> String {
    findb::auxs::get(db, k, code)
        .ok()
        .flatten()
        .map(|e| e.name)
        .unwrap_or_default()
}

/// 把 AuxRef 渲染成一行可读文本
pub fn aux_text(aux: &AuxRef, names: &std::collections::BTreeMap<String, String>) -> String {
    let mut parts = Vec::new();
    for k in AuxKind::ALL {
        if *k == AuxKind::CashFlow {
            continue;
        }
        if let Some(v) = aux.get(*k) {
            let name = names.get(&format!("{}:{}", k.code(), v)).cloned().unwrap_or_default();
            parts.push(if name.is_empty() {
                format!("{}:{}", k.label(), v)
            } else {
                format!("{}:{}", k.label(), name)
            });
        }
    }
    parts.join(" ")
}

/// 凭证打印样式
pub fn print_body(ui: &mut Ui, v: &Voucher, chart: &Chart, company: &str) {
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("记 账 凭 证").size(22.0).strong());
    });
    ui.horizontal(|ui| {
        ui.label(format!("单位：{company}"));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(format!("日期：{}", v.date.format("%Y-%m-%d")));
            ui.label(format!("凭证号：{}", v.voucher_no()));
        });
    });
    ui.separator();
    let rows = fincore::engine::entries_for_print(v, chart);
    let cols = [
        TCol::new("摘要", 260.0),
        TCol::new("会计科目", 300.0),
        TCol::new("借方金额", 140.0).right(),
        TCol::new("贷方金额", 140.0).right(),
    ];
    widgets::grid(ui, "print_entries", &cols, rows.len(), 24.0, |i, c, ui| {
        let e = &rows[i];
        match c {
            0 => {
                ui.label(&e.summary);
            }
            1 => {
                ui.label(format!("{} {}", e.account_display, e.aux_display));
            }
            2 => widgets::amount_label(ui, e.debit),
            3 => widgets::amount_label(ui, e.credit),
            _ => {}
        }
    });
    ui.separator();
    let (d, c, cap) = fincore::engine::voucher_totals(v);
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("合计：借 {} 贷 {}", d.fmt_money(), c.fmt_money())).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(format!("大写：{cap}")).weak());
        });
    });
    ui.horizontal(|ui| {
        ui.label(format!("制单：{}", v.prepared_by));
        ui.label(format!("审核：{}", v.audited_by.clone().unwrap_or_default()));
        ui.label(format!("记账：{}", v.posted_by.clone().unwrap_or_default()));
        ui.label(format!(
            "出纳：{}",
            v.cashier.clone().unwrap_or_default()
        ));
    });
}

fn export_print(ctx: &mut AppCtx<'_>, v: Option<&Voucher>) {
    let Some(v) = v else {
        ctx.error("当前内容不完整，无法导出");
        return;
    };
    let chart = ctx.chart();
    let rows = fincore::engine::entries_for_print(v, chart);
    let mut sh = crate::views::export::Sheet::new(
        "记账凭证",
        vec![
            "日期".into(),
            "凭证号".into(),
            "摘要".into(),
            "会计科目".into(),
            "辅助核算".into(),
            "借方金额".into(),
            "贷方金额".into(),
        ],
    );
    for e in &rows {
        sh.push(vec![
            v.date.format("%Y-%m-%d").to_string(),
            v.voucher_no(),
            e.summary.clone(),
            e.account_display.clone(),
            e.aux_display.clone(),
            e.debit.fmt_plain(),
            e.credit.fmt_plain(),
        ]);
    }
    match crate::views::export::export_sheet(&sh, &format!("凭证{}", v.voucher_no()), true) {
        Ok(m) => ctx.info(m),
        Err(e) => ctx.error(e),
    }
}

/// 余额方向的小工具：给定科目方向，判断某金额是否异常
pub fn abnormal(dir: Direction, v: Money) -> bool {
    (dir == Direction::Debit && v.is_negative()) || (dir == Direction::Credit && v.is_positive())
}
