//! 应收应付对账（桌面端）：先录对方报来的数，对好了再落账。
//!
//! 与 Web 端 `viewArapRecon` 是同一个后端（`findb::arap_recon`），口径不会分叉。
//! 三条底线照旧：
//!   ① 批次停在「对账中」时账上一分钱没动，也不进任何报表；
//!   ② 只有差额才生成凭证，而且一定是**草稿**，记不记账由人决定；
//!   ③ 名单按账面自动列（有余额的户先摆出来），人只改对方报的数，差额自己冒出来。

use egui::{RichText, Ui};
use findb::arap_recon::{self, Batch, Detail, KIND_AP, KIND_AR, ST_OPEN};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

pub struct ArapReconView {
    /// 应收 / 应付
    pub is_ap: bool,
    pub period_text: String,
    pub batches: Vec<Batch>,
    pub sel: i64,
    pub detail: Option<Detail>,
    /// 账面有余额、但这一批没录的户
    pub book_only: Vec<(String, String, fincore::Money)>,
    /// 档案里找不到的编码
    pub unknown: Vec<String>,
    /// 每户「对方报来的数」的输入缓冲（line id → 文本）
    pub claims: std::collections::BTreeMap<i64, String>,
    /// 粘贴导入的原文
    pub paste: String,
    /// 差额科目
    pub adjust: String,
    /// 新增一户用
    pub one_code: String,
    pub one_amt: String,
    pub err: String,
    pub dirty: bool,
}

impl Default for ArapReconView {
    fn default() -> Self {
        Self {
            is_ap: false,
            period_text: String::new(),
            batches: Vec::new(),
            sel: 0,
            detail: None,
            book_only: Vec::new(),
            unknown: Vec::new(),
            claims: Default::default(),
            paste: String::new(),
            adjust: "660305".to_string(),
            one_code: String::new(),
            one_amt: String::new(),
            err: String::new(),
            dirty: true,
        }
    }
}

impl ArapReconView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period_text.is_empty() {
            self.period_text = ctx.period().code();
        }
        self.dirty = true;
    }

    fn kind(&self) -> &'static str {
        if self.is_ap {
            KIND_AP
        } else {
            KIND_AR
        }
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let kind = self.kind();
        match arap_recon::list(ctx.db(), Some(kind), None) {
            Ok(bs) => {
                self.batches = bs;
                let keep = self
                    .batches
                    .iter()
                    .any(|b| b.id == self.sel && b.kind == kind);
                if !keep {
                    self.sel = self.batches.first().map(|b| b.id).unwrap_or(0);
                }
            }
            Err(e) => {
                ctx.error(e.to_string());
                self.batches.clear();
                self.sel = 0;
            }
        }
        self.load_detail(ctx);
        self.dirty = false;
    }

    fn load_detail(&mut self, ctx: &mut AppCtx<'_>) {
        if self.sel <= 0 {
            self.detail = None;
            self.book_only.clear();
            self.unknown.clear();
            self.claims.clear();
            return;
        }
        match arap_recon::get(ctx.db(), self.sel) {
            Ok(Some(d)) => {
                self.claims = d
                    .lines
                    .iter()
                    .map(|l| (l.id, l.claim.fmt_plain()))
                    .collect();
                if d.batch.adjust_account.trim().is_empty() {
                    // 上次用过的科目留空就沿用默认
                } else {
                    self.adjust = d.batch.adjust_account.clone();
                }
                self.detail = Some(d);
            }
            Ok(None) => {
                self.detail = None;
                self.sel = 0;
            }
            Err(e) => ctx.error(e.to_string()),
        }
        self.book_only = arap_recon::book_only(ctx.db(), self.sel).unwrap_or_default();
        self.unknown = arap_recon::unknown_parties(ctx.db(), self.sel).unwrap_or_default();
    }

    fn new_batch(&mut self, ctx: &mut AppCtx<'_>) {
        let p = fincore::Period::parse(&self.period_text).unwrap_or_else(|_| ctx.period());
        let kind = self.kind();
        match arap_recon::create(ctx.db(), kind, p, "", "", "", &ctx.user().username) {
            Ok(id) => {
                // 名单按账面先列一遍：对账的动作是「核对对方报的数」，
                // 不是「我凭空录二十个客户」。
                let n = arap_recon::seed_from_book(ctx.db(), id, &ctx.user().username).unwrap_or(0);
                if n > 0 {
                    ctx.info(format!("新建了对账批次，按账面列出 {n} 户，改对方的数就行"));
                } else {
                    ctx.info("新建了对账批次（账面暂时没有余额，可以手工加户）");
                }
                self.sel = id;
                self.dirty = true;
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn save_claims(&mut self, ctx: &mut AppCtx<'_>, changed: Vec<(i64, String)>) {
        let Some(d) = self.detail.clone() else { return };
        let mut n = 0;
        for (id, text) in changed {
            let Ok(amt) = fincore::Money::parse(text.trim()) else {
                self.err = format!("金额格式不对：{}", text.trim());
                continue;
            };
            let Some(l) = d.lines.iter().find(|l| l.id == id) else {
                continue;
            };
            match arap_recon::set_line(ctx.db(), id, amt, l.disputed, &l.note) {
                Ok(()) => n += 1,
                Err(e) => self.err = e.to_string(),
            }
        }
        if n > 0 {
            self.err.clear();
            ctx.info(format!("改了 {n} 户的金额"));
            self.load_detail(ctx);
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if self.dirty {
            self.reload(ctx);
        }

        ui.horizontal(|ui| {
            ui.label("对哪一边");
            let mut ap = self.is_ap;
            if ui.selectable_label(!ap, "应收（客户欠我）").clicked() {
                ap = false;
            }
            if ui.selectable_label(ap, "应付（我欠供应商）").clicked() {
                ap = true;
            }
            if ap != self.is_ap {
                self.is_ap = ap;
                self.sel = 0;
                self.dirty = true;
            }
            ui.separator();
            ui.label("期间");
            ui.add(
                egui::TextEdit::singleline(&mut self.period_text)
                    .desired_width(80.0)
                    .hint_text("202611"),
            );
            if ui.button("新建这次对账").clicked() {
                self.new_batch(ctx);
                self.reload(ctx);
            }
            ui.separator();
            ui.label("这一次");
            let mut pick = self.sel;
            let items: Vec<(i64, String)> = self
                .batches
                .iter()
                .map(|b| {
                    (
                        b.id,
                        format!(
                            "{} {}（{}·{}户）",
                            b.period.label(),
                            b.title,
                            status_label(&b.status),
                            b.party_count
                        ),
                    )
                })
                .collect();
            let cur = items
                .iter()
                .find(|(id, _)| *id == pick)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| "还没有对账批次".to_string());
            egui::ComboBox::from_id_salt("arap_recon_batch")
                .selected_text(cur)
                .width(320.0)
                .show_ui(ui, |ui| {
                    for (id, s) in &items {
                        ui.selectable_value(&mut pick, *id, s);
                    }
                });
            if pick != self.sel {
                self.sel = pick;
                self.load_detail(ctx);
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        ui.label(
            RichText::new(
                "对账不改账。对方报来的数记在这张单子上，对好了点「确认这一批」，只有差额才生成一张草稿凭证——要不要记账你说了算。",
            )
            .weak(),
        );

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, &self.err);
        }

        let Some(d) = self.detail.clone() else {
            ui.add_space(8.0);
            ui.label(RichText::new("先「新建这次对账」，名单会自动按账面列出来。").weak());
            return;
        };
        let open = d.batch.status == ST_OPEN;
        let s = &d.summary;

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} {}（{}）：共 {} 户 —— 对上 {} 户，有差额 {} 户，争议 {} 户",
                    d.batch.period.label(),
                    d.batch.title,
                    if d.batch.kind == KIND_AP {
                        "应付"
                    } else {
                        "应收"
                    },
                    s.party_count,
                    s.agreed,
                    s.need_adjust,
                    s.disputed
                ))
                .strong(),
            );
            ui.separator();
            ui.label(format!(
                "对方合计 {}　账面合计 {}",
                s.claim_total.fmt_money(),
                s.book_total.fmt_money()
            ));
            let diff = s.diff_total;
            ui.label(RichText::new(format!("差额 {}", diff.fmt_money())).color(
                if diff.is_zero() {
                    palette::OK
                } else {
                    palette::CREDIT
                },
            ));
            if let Some(v) = d.batch.voucher_id {
                ui.separator();
                ui.label(RichText::new(format!("差额凭证 #{v}（草稿）")).weak());
            }
        });

        if !self.book_only.is_empty() {
            ui.colored_label(
                palette::WARN,
                format!(
                    "账面还有 {} 户没录进这一批（对账最常漏的就是这几户）：{}",
                    self.book_only.len(),
                    self.book_only
                        .iter()
                        .take(5)
                        .map(|(c, n, a)| format!("{c} {n} {}", a.fmt_money()))
                        .collect::<Vec<_>>()
                        .join("、")
                ),
            );
        }
        if !self.unknown.is_empty() {
            ui.colored_label(
                palette::WARN,
                format!(
                    "这些编码在档案里没有，确认一下是不是打错了：{}",
                    self.unknown.join("、")
                ),
            );
        }

        ui.add_space(6.0);

        // 主表
        let lines = d.lines.clone();
        let cols = [
            widgets::TCol::new("客商编码", 100.0),
            widgets::TCol::new("名称", 180.0),
            widgets::TCol::new("对方报来的数", 130.0).right(),
            widgets::TCol::new("账面", 120.0).right(),
            widgets::TCol::new("差额", 110.0).right(),
            widgets::TCol::new("状态", 90.0),
            widgets::TCol::new("备注", 180.0),
            widgets::TCol::new("", 150.0),
        ];
        let mut claims = self.claims.clone();
        let mut changed: Vec<(i64, String)> = Vec::new();
        let mut flag: Option<i64> = None;
        let mut del: Option<i64> = None;
        let mut add_from_book: Option<(String, fincore::Money)> = None;

        widgets::grid(
            ui,
            "arap_recon_lines",
            &cols,
            lines.len(),
            24.0,
            |i, c, ui| {
                let Some(l) = lines.get(i) else { return };
                match c {
                    0 => {
                        ui.label(RichText::new(&l.party_code).monospace());
                    }
                    1 => {
                        ui.label(&l.party_name);
                    }
                    2 => {
                        if open {
                            let buf = claims.entry(l.id).or_insert_with(|| l.claim.fmt_plain());
                            let r = ui.add(
                                egui::TextEdit::singleline(buf)
                                    .desired_width(120.0)
                                    .hint_text("对方报的数"),
                            );
                            // 只有真改了才存：逐帧保存会把库写穿
                            if r.lost_focus() && claims.get(&l.id) != Some(&l.claim.fmt_plain()) {
                                changed
                                    .push((l.id, claims.get(&l.id).cloned().unwrap_or_default()));
                            }
                        } else {
                            ui.label(l.claim.fmt_money());
                        }
                    }
                    3 => {
                        widgets::amount_label(ui, l.book);
                    }
                    4 => {
                        let t = l.diff.fmt_money();
                        ui.label(RichText::new(t).color(if l.diff.is_zero() {
                            palette::OK
                        } else {
                            palette::CREDIT
                        }));
                    }
                    5 => {
                        if l.disputed {
                            ui.colored_label(palette::CREDIT, "有争议");
                        } else if l.agreed() {
                            ui.colored_label(palette::OK, "对上了");
                        } else {
                            ui.label(RichText::new("没对上").weak());
                        }
                    }
                    6 => {
                        ui.label(RichText::new(&l.note).weak());
                    }
                    _ => {
                        if open {
                            if ui
                                .button(if l.disputed {
                                    "取消争议"
                                } else {
                                    "标争议"
                                })
                                .clicked()
                            {
                                flag = Some(l.id);
                            }
                            if ui.button("删").clicked() {
                                del = Some(l.id);
                            }
                        }
                    }
                }
            },
        );

        if !self.book_only.is_empty() && open {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("漏掉的户：").weak());
                for (code, name, bal) in self.book_only.iter().take(6) {
                    if ui
                        .button(format!("+ {} {}", code, name))
                        .on_hover_text(format!("按账面 {} 加进来", bal.fmt_money()))
                        .clicked()
                    {
                        add_from_book = Some((code.clone(), *bal));
                    }
                }
            });
        }

        ui.add_space(8.0);
        ui.separator();

        if open {
            ui.horizontal(|ui| {
                if ui.button("按账面列名单").clicked() {
                    match arap_recon::seed_from_book(ctx.db(), self.sel, &ctx.user().username) {
                        Ok(0) => ctx.info("账面有余额的户都在这了"),
                        Ok(n) => {
                            ctx.info(format!("按账面补了 {n} 户"));
                            self.load_detail(ctx);
                        }
                        Err(e) => self.err = e.to_string(),
                    }
                }
                if ui.button("重算账面（你填的数不动）").clicked() {
                    match arap_recon::sync_book(ctx.db(), self.sel) {
                        Ok(n) => {
                            ctx.info(format!("重算了 {n} 户的账面数"));
                            self.load_detail(ctx);
                        }
                        Err(e) => self.err = e.to_string(),
                    }
                }
                ui.separator();
                ui.label("差额科目");
                ui.add(
                    egui::TextEdit::singleline(&mut self.adjust)
                        .desired_width(90.0)
                        .hint_text("660305"),
                );
                if ui.button("确认这一批（差额转草稿凭证）").clicked() {
                    let adjust = self.adjust.trim().to_string();
                    let need = !d.summary.diff_total.is_zero() && !adjust.is_empty();
                    if need || d.summary.diff_total.is_zero() {
                        match arap_recon::confirm(ctx.db(), self.sel, &ctx.user().username, &adjust)
                        {
                            Ok(r) => {
                                self.err.clear();
                                match r.voucher_id {
                                    Some(v) => ctx.info(format!(
                                        "已确认：差额 {} 生成草稿凭证 #{v}，去凭证查询审核记账",
                                        r.diff_total.fmt_money()
                                    )),
                                    None => ctx
                                        .info(format!("已确认：{} 户对得上，不用调账", r.adjusted)),
                                }
                                if r.skipped_disputed > 0 {
                                    ctx.info(format!(
                                        "有 {} 户标了争议，这次先跳过",
                                        r.skipped_disputed
                                    ));
                                }
                                self.dirty = true;
                            }
                            Err(e) => self.err = e.to_string(),
                        }
                    } else {
                        self.err = "还有差额，请先填差额科目".to_string();
                    }
                }
                if ui.button("撤销这一批").clicked() {
                    match arap_recon::cancel(ctx.db(), self.sel, &ctx.user().username) {
                        Ok(()) => {
                            ctx.info("已撤销");
                            self.dirty = true;
                        }
                        Err(e) => self.err = e.to_string(),
                    }
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("加一户");
                ui.add(
                    egui::TextEdit::singleline(&mut self.one_code)
                        .desired_width(110.0)
                        .hint_text("客商编码"),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.one_amt)
                        .desired_width(110.0)
                        .hint_text("对方报的数"),
                );
                if ui.button("加进来").clicked() {
                    let code = self.one_code.trim().to_string();
                    let amt = fincore::Money::parse(self.one_amt.trim());
                    match (code.is_empty(), amt) {
                        (true, _) => self.err = "编码要填".to_string(),
                        (_, Err(_)) => self.err = "金额格式不对".to_string(),
                        (false, Ok(a)) => {
                            match arap_recon::upsert_line(ctx.db(), self.sel, &code, a, "") {
                                Ok(_) => {
                                    self.err.clear();
                                    self.one_code.clear();
                                    self.one_amt.clear();
                                    self.load_detail(ctx);
                                }
                                Err(e) => self.err = e.to_string(),
                            }
                        }
                    }
                }
            });

            ui.add_space(6.0);
            ui.label(
                RichText::new("粘贴导入：一行一户 客商编码,金额[,备注]（从对方发来的对账单上抄）")
                    .weak(),
            );
            ui.add(
                egui::TextEdit::multiline(&mut self.paste)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("C01,12000,11月加工费"),
            );
            if ui.button("导入这些数").clicked() {
                match arap_recon::import_lines(
                    ctx.db(),
                    self.sel,
                    &self.paste,
                    &ctx.user().username,
                ) {
                    Ok(r) => {
                        self.err.clear();
                        ctx.info(format!(
                            "导入 {} 行{}",
                            r.ok,
                            if r.skipped > 0 {
                                format!("，跳过 {} 行", r.skipped)
                            } else {
                                String::new()
                            }
                        ));
                        if let Some(w) = r.warnings.first() {
                            ctx.info(w.clone());
                        }
                        self.paste.clear();
                        self.load_detail(ctx);
                    }
                    Err(e) => self.err = e.to_string(),
                }
            }
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "这一批已{}（{}）",
                        status_label(&d.batch.status),
                        if d.batch.confirmed_by.is_empty() {
                            "—".to_string()
                        } else {
                            format!("{} {}", d.batch.confirmed_by, d.batch.confirmed_at)
                        }
                    ))
                    .weak(),
                );
                if ui.button("打回重对").clicked() {
                    match arap_recon::reopen(ctx.db(), self.sel, &ctx.user().username) {
                        Ok(()) => {
                            ctx.info("已打回，可以接着改");
                            self.dirty = true;
                        }
                        Err(e) => self.err = e.to_string(),
                    }
                }
            });
        }

        // —— 表格里点出来的动作，统一在画完之后落库（免得边画边借） ——
        if !changed.is_empty() {
            self.save_claims(ctx, changed);
        }
        if let Some(id) = flag {
            if let Some(d) = self.detail.clone() {
                if let Some(l) = d.lines.iter().find(|l| l.id == id) {
                    match arap_recon::set_line(ctx.db(), id, l.claim, !l.disputed, &l.note) {
                        Ok(()) => {
                            self.err.clear();
                            self.load_detail(ctx);
                        }
                        Err(e) => self.err = e.to_string(),
                    }
                }
            }
        }
        if let Some(id) = del {
            match arap_recon::delete_line(ctx.db(), id) {
                Ok(()) => {
                    self.err.clear();
                    self.load_detail(ctx);
                }
                Err(e) => self.err = e.to_string(),
            }
        }
        if let Some((code, bal)) = add_from_book {
            match arap_recon::upsert_line(ctx.db(), self.sel, &code, bal, "按账面补录") {
                Ok(_) => {
                    self.err.clear();
                    self.load_detail(ctx);
                }
                Err(e) => self.err = e.to_string(),
            }
        }
    }
}

fn status_label(s: &str) -> &'static str {
    match s {
        arap_recon::ST_CONFIRMED => "已确认",
        arap_recon::ST_CANCELLED => "已撤销",
        _ => "对账中",
    }
}
