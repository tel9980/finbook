//! 常用凭证模板 / 常用摘要
//!
//! 模板分两类：手工模板（录凭证时一键调出改金额）和周期性分录（房租、摊销这类每月
//! 一样的分录，标了频率后由期末批量生成）。模板分录允许金额留空——留空表示"录凭证时再填"，
//! 所以保存时要么全填要么全空，混着填八成是漏输了。

use egui::{RichText, Ui};
use findb::summaries::Summary;
use findb::template::{Freq, Template, TemplateEntry};
use fincore::{Period, Perm};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

const FREQS: [Freq; 4] = [Freq::Manual, Freq::Monthly, Freq::Quarterly, Freq::Yearly];
const DIRS: [&str; 2] = ["借", "贷"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Template,
    Summary,
}

pub struct TemplateView {
    pub tab: Tab,
    // ---- Tab 1 ----
    pub rows: Vec<Template>,
    editing: Option<Template>,
    editing_new: bool,
    /// 频率下拉的显示值：egui 下拉要的是字符串，Freq 只在保存时转换
    freq_txt: String,
    start_txt: String,
    end_txt: String,
    // ---- Tab 2 ----
    pub kw: String,
    pub sums: Vec<Summary>,
    pub new_sum: String,
    /// 改名中的摘要：(旧文本, 新文本)
    renaming: Option<(String, String)>,
    pub dirty: bool,
    key: String,
}

impl Default for TemplateView {
    fn default() -> Self {
        Self {
            tab: Tab::Template,
            rows: Vec::new(),
            editing: None,
            editing_new: false,
            freq_txt: Freq::Manual.label().to_string(),
            start_txt: String::new(),
            end_txt: String::new(),
            kw: String::new(),
            sums: Vec::new(),
            new_sum: String::new(),
            renaming: None,
            dirty: true,
            key: String::new(),
        }
    }
}

impl TemplateView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        self.dirty = true;
        // 模板界面不按期间取数，但期间变了要让缓存 key 失效（比如生效期间的默认值展示）
        let _ = ctx;
    }

    /// 按「期间 + Tab + 关键词」缓存
    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        let key = format!(
            "{}|{:?}|{}",
            ctx.period().ymm(),
            self.tab,
            self.kw.trim()
        );
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        match self.tab {
            Tab::Template => self.rows = findb::template::list(ctx.db()).unwrap_or_default(),
            Tab::Summary => {
                self.sums = findb::summaries::search(ctx.db(), &self.kw).unwrap_or_default()
            }
        }
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "凭证模板", |ui| {
            match self.tab {
                Tab::Template => ui.label(RichText::new(format!("共 {} 个模板", self.rows.len())).weak()),
                Tab::Summary => ui.label(RichText::new(format!("共 {} 条摘要", self.sums.len())).weak()),
            };
        });

        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, Tab::Template, "凭证模板");
            ui.selectable_value(&mut self.tab, Tab::Summary, "常用摘要");
            ui.separator();
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        match self.tab {
            Tab::Template => self.show_templates(ctx, ui),
            Tab::Summary => self.show_summaries(ctx, ui),
        }
    }

    // ------------------------- Tab 1 凭证模板 -------------------------
    fn show_templates(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            if ui.button("新增模板").clicked() && ctx.can(Perm::VoucherNew) {
                let mut t = Template::new("");
                // 一张凭证至少两行，直接给好空行省一次点击
                t.entries = vec![TemplateEntry::default(), TemplateEntry::default()];
                t.active = true;
                self.freq_txt = t.freq.label().to_string();
                self.start_txt.clear();
                self.end_txt.clear();
                self.editing = Some(t);
                self.editing_new = true;
            }
            ui.label(RichText::new("填制凭证界面可一键套用模板；周期性模板在期末自动出凭证").weak());
        });

        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("名称", 180.0),
            widgets::TCol::new("摘要/备注", 240.0),
            widgets::TCol::new("借贷合计", 260.0),
            widgets::TCol::new("生成频率", 110.0).fixed(),
            widgets::TCol::new("生效期间", 150.0),
            widgets::TCol::new("启用", 60.0).fixed(),
            widgets::TCol::new("操作", 110.0).fixed(),
        ];
        let mut edit: Option<i64> = None;
        let mut del: Option<i64> = None;
        widgets::grid(ui, "tpl_list", &cols, rows.len(), 26.0, |i, c, ui| {
            let t = &rows[i];
            let (d, cr) = t.totals();
            match c {
                0 => { ui.label(&t.name); }
                1 => { ui.label(RichText::new(&t.memo).weak()); }
                2 => {
                    let balanced = d == cr;
                    ui.label(
                        RichText::new(format!("借 {} / 贷 {}", d.fmt_money(), cr.fmt_money()))
                            .color(if balanced {
                                ui.visuals().text_color()
                            } else {
                                palette::CREDIT
                            }),
                    );
                }
                3 => { ui.label(t.freq.label()); }
                4 => { ui.label(period_range(t.start_period, t.end_period)); }
                5 => {
                    ui.label(if t.active {
                        RichText::new("启用").color(palette::OK)
                    } else {
                        RichText::new("停用").color(palette::CREDIT)
                    });
                }
                6 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改").clicked() {
                            edit = Some(t.id);
                        }
                        if ui.small_button("删").clicked() {
                            del = Some(t.id);
                        }
                    });
                }
                _ => {}
            }
        });

        if let Some(id) = edit {
            if let Some(t) = self.rows.iter().find(|t| t.id == id) {
                self.freq_txt = t.freq.label().to_string();
                self.start_txt = t.start_period.map(|p| p.code()).unwrap_or_default();
                self.end_txt = t.end_period.map(|p| p.code()).unwrap_or_default();
                self.editing = Some(t.clone());
                self.editing_new = false;
            }
        }
        if let Some(id) = del {
            let name = self
                .rows
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.name.clone())
                .unwrap_or_default();
            ctx.confirm_dangerous(
                "删除凭证模板",
                &format!("确定删除模板「{name}」吗？删除后不能恢复。"),
                ConfirmAction::DeleteTemplate(id),
                true,
            );
        }

        self.edit_window(ctx, ui);
    }

    fn edit_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(t) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;
        let freq_opts: Vec<String> = FREQS.iter().map(|f| f.label().to_string()).collect();
        let dir_opts: Vec<String> = DIRS.iter().map(|d| d.to_string()).collect();

        egui::Window::new(if is_new { "新增凭证模板" } else { "修改凭证模板" })
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([820.0, 540.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                egui::Grid::new("tpl_head")
                    .num_columns(4)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("名称：");
                        widgets::text_input(ui, &mut t.name, 220.0, "如：计提房租");
                        ui.label("备注：");
                        widgets::text_input(ui, &mut t.memo, 260.0, "可空");
                        ui.end_row();
                        ui.label("生成频率：");
                        widgets::combo(ui, "tpl_freq", &mut self.freq_txt, &freq_opts, 160.0);
                        ui.label("（仅手工调用=不自动生成）");
                        ui.end_row();
                        ui.label("生效期间：");
                        widgets::text_input(ui, &mut self.start_txt, 110.0, "空=不限");
                        ui.label("失效期间：");
                        widgets::text_input(ui, &mut self.end_txt, 110.0, "空=不限");
                        ui.end_row();
                    });
                ui.checkbox(&mut t.active, "启用");
                ui.add_space(6.0);
                ui.separator();

                ui.horizontal(|ui| {
                    ui.label(RichText::new("分录").strong());
                    if ui.button("加一行").clicked() {
                        t.entries.push(TemplateEntry::default());
                    }
                });
                let mut remove: Option<usize> = None;
                for (i, e) in t.entries.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}", i + 1)).weak().monospace());
                        widgets::text_input(ui, &mut e.summary, 150.0, "摘要");
                        widgets::account_combo(
                            ui,
                            &format!("tpl_acct_{i}"),
                            &mut e.account_code,
                            ctx.chart(),
                            true,
                            220.0,
                        );
                        let mut dir_txt = if e.dir == "credit" {
                            "贷".to_string()
                        } else {
                            "借".to_string()
                        };
                        widgets::combo(ui, &format!("tpl_dir_{i}"), &mut dir_txt, &dir_opts, 60.0);
                        e.dir = if dir_txt == "贷" {
                            "credit".to_string()
                        } else {
                            "debit".to_string()
                        };
                        // 金额留空 = 套用模板时再填，所以用可空文本输入而不是金额控件
                        widgets::text_input(ui, &mut e.amount, 120.0, "留空=待填");
                        if ui.small_button("×").on_hover_text("删除该行").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    t.entries.remove(i);
                }

                ui.separator();
                let (d, cr) = t.totals();
                ui.horizontal(|ui| {
                    ui.label(format!("借合计 {}", d.fmt_money()));
                    ui.label(format!("贷合计 {}", cr.fmt_money()));
                    if d != cr {
                        ui.colored_label(
                            palette::CREDIT,
                            format!("借贷不平衡，差额 {}", (d - cr).fmt_money()),
                        );
                    }
                });
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
            return;
        }
        if save {
            self.save_template(ctx);
        }
    }

    fn save_template(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(freq) = FREQS.iter().find(|f| f.label() == self.freq_txt).copied() else {
            ctx.error("生成频率不合法");
            return;
        };
        // 期间文本先解析完再动模板，解析失败时不要把半改的对象留在编辑态里
        let (start, end) = match (parse_period(&self.start_txt), parse_period(&self.end_txt)) {
            (Some(s), Some(e)) => (s, e),
            _ => {
                ctx.error("生效/失效期间格式应为 YYYYMM，留空表示不限");
                return;
            }
        };

        let Some(t) = self.editing.as_mut() else {
            return;
        };
        if t.name.trim().is_empty() {
            ctx.error("模板名称不能为空");
            return;
        }
        if t.entries.len() < 2 {
            ctx.error("至少需要两行分录");
            return;
        }
        if t.entries.iter().any(|e| e.account_code.trim().is_empty()) {
            ctx.error("每一行都必须选择科目");
            return;
        }
        // 半填半空多半是漏输，套用模板时会产生一张不平的凭证
        let filled = t
            .entries
            .iter()
            .filter(|e| !e.amount.trim().is_empty())
            .count();
        if filled != 0 && filled != t.entries.len() {
            ctx.error("金额要么全部填写，要么全部留空（留空表示套用时再填）");
            return;
        }

        t.name = t.name.trim().to_string();
        t.freq = freq;
        t.start_period = start;
        t.end_period = end;

        let is_new = self.editing_new;
        let r = if is_new {
            findb::template::insert(ctx.db(), t).map(|_| ())
        } else {
            findb::template::update(ctx.db(), t)
        };
        match r {
            Ok(()) => {
                ctx.log(
                    "模板",
                    if is_new { "新增模板" } else { "修改模板" },
                    &format!("{} {} 行", t.name, t.entries.len()),
                );
                ctx.info("已保存模板");
                self.dirty = true;
                self.editing = None;
            }
            Err(e) => ctx.error(e.to_string()),
        }
    }

    // ------------------------- Tab 2 常用摘要 -------------------------
    fn show_summaries(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::toolbar(ui, |ui| {
            let r = widgets::search_field(ui, &mut self.kw, "搜索摘要");
            if r.changed() {
                self.dirty = true;
            }
            ui.separator();
            widgets::text_input(ui, &mut self.new_sum, 220.0, "新增摘要文本");
            if ui.button("新增").clicked() && ctx.can(Perm::VoucherNew) {
                let t = self.new_sum.trim().to_string();
                if t.is_empty() {
                    ctx.error("请先输入摘要文本");
                } else {
                    match findb::summaries::insert(ctx.db(), &t) {
                        Ok(()) => {
                            ctx.log("摘要", "新增摘要", &t);
                            ctx.info("已新增摘要");
                            self.new_sum.clear();
                            self.dirty = true;
                        }
                        Err(e) => ctx.error(e.to_string()),
                    }
                }
            }
        });

        let rows = self.sums.clone();
        let cols = [
            widgets::TCol::new("摘要文本", 420.0),
            widgets::TCol::new("使用次数", 100.0).right(),
            widgets::TCol::new("操作", 110.0).fixed(),
        ];
        let mut rename: Option<String> = None;
        let mut del: Option<String> = None;
        widgets::grid(ui, "sum_list", &cols, rows.len(), 24.0, |i, c, ui| {
            let s = &rows[i];
            match c {
                0 => { ui.label(format!("{}（已用 {} 次）", s.text, s.use_count)); }
                1 => { ui.label(s.use_count.to_string()); }
                2 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改名").clicked() {
                            rename = Some(s.text.clone());
                        }
                        if ui.small_button("删").clicked() {
                            del = Some(s.text.clone());
                        }
                    });
                }
                _ => {}
            }
        });

        if let Some(old) = rename {
            self.renaming = Some((old.clone(), old));
        }
        if let Some(text) = del {
            match findb::summaries::delete(ctx.db(), &text) {
                Ok(()) => {
                    ctx.log("摘要", "删除摘要", &text);
                    ctx.info("已删除摘要");
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }

        ui.add_space(10.0);
        ui.label(
            RichText::new(
                "说明：凭证录入界面的摘要框会自动按使用次数倒序补全，输入即过滤；\
                 每保存一张凭证，其中出现过的摘要使用次数会 +1。",
            )
            .weak()
            .color(ui.visuals().weak_text_color()),
        );

        self.rename_window(ctx, ui);
    }

    fn rename_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some((old, new)) = self.renaming.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;

        egui::Window::new("修改摘要")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(format!("原摘要：{old}"));
                ui.horizontal(|ui| {
                    ui.label("新摘要：");
                    ui.add_sized(
                        [300.0, 22.0],
                        egui::TextEdit::singleline(new).hint_text("请输入新的摘要文本"),
                    );
                });
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
            self.renaming = None;
            return;
        }
        if save {
            let Some((old, new)) = self.renaming.clone() else {
                return;
            };
            let n = new.trim();
            if n.is_empty() {
                ctx.error("新摘要不能为空");
                return;
            }
            match findb::summaries::update(ctx.db(), &old, n) {
                Ok(()) => {
                    ctx.log("摘要", "修改摘要", &format!("{old} → {n}"));
                    ctx.info("已修改摘要");
                    self.dirty = true;
                    self.renaming = None;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }
    }
}

/// 空文本 = 不限期间，返回 None 表示解析失败
fn parse_period(s: &str) -> Option<Option<Period>> {
    let t = s.trim();
    if t.is_empty() {
        return Some(None);
    }
    Period::parse(t).ok().map(Some)
}

fn period_range(from: Option<Period>, to: Option<Period>) -> String {
    match (from, to) {
        (None, None) => "不限".to_string(),
        (Some(a), None) => format!("{} 起", a.code()),
        (None, Some(b)) => format!("至 {}", b.code()),
        (Some(a), Some(b)) => format!("{} ~ {}", a.code(), b.code()),
    }
}
