//! 合并报表（跨账套汇总）——桌面端入口
//!
//! 对标 Web 端的 `GET /api/consolidate/preview`：**两端调同一个
//! `findb::consolidate::preview`**，所以口径不可能走偏。
//!
//! ## 桌面端与 Web 端唯一的差别：账套怎么选
//!
//! Web 端有「账套仓库」（`Realm`），能列出当前账号可见的账套；桌面端没有这个概念
//! （一次只开一个 `.fbk`，见 `AppState::book_path`）。所以这里让使用者自己多选
//! 文件：`rfd` 多选 → 逐个 `Db::open_no_migrate`。
//!
//! 为什么是 `open_no_migrate` 而不是 `open`：合并报表是**只读**操作，不该因为
//! 「打开了另一个账套看一眼」就触发那套账的 schema 迁移。迁移应该发生在真正
//! 进入那套账工作的时候。

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;
use egui::{RichText, Ui};
use fincore::Period;
use findb::consolidate::{ConsolReport, ConsolSource};
use std::path::PathBuf;

/// 已经打开参与合并的一个账套
struct OpenBook {
    /// 文件名（作为列标识）
    key: String,
    company: String,
    path: PathBuf,
    /// 注意：`Db` 不是 `Clone`，这里直接持有
    db: findb::Db,
}

#[derive(Default)]
pub struct ConsolidateView {
    /// 已选账套（保持顺序，报表列按这个顺序排）
    books: Vec<OpenBook>,
    /// 报表期间，形如 `2026-01`
    period: String,
    report: Option<ConsolReport>,
    /// 上次报错，显示在顶部而不是弹窗——选文件失败很常见，弹窗太吵
    err: String,
}

impl ConsolidateView {
    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        if self.period.trim().is_empty() {
            self.period = ctx.period().to_string();
        }
        self.reload();
    }

    pub fn invalidate(&mut self) {
        self.report = None;
    }

    /// 按当前账套与期间重算
    fn reload(&mut self) {
        if self.books.is_empty() {
            self.report = None;
            return;
        }
        let period = match parse_period(&self.period) {
            Some(p) => p,
            None => {
                self.err = "期间格式应为 YYYY-MM（例如 2026-01）".to_string();
                self.report = None;
                return;
            }
        };
        let sources: Vec<ConsolSource<'_>> = self
            .books
            .iter()
            .map(|b| ConsolSource {
                key: b.key.clone(),
                company: b.company.clone(),
                db: &b.db,
            })
            .collect();
        match findb::consolidate::preview(&sources, period) {
            Ok(r) => {
                self.err.clear();
                self.report = Some(r);
            }
            Err(e) => {
                self.err = e.to_string();
                self.report = None;
            }
        }
    }

    fn add_books(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("FinBook 账套", &["fbk"])
            .pick_files()
        else {
            return;
        };
        for p in paths {
            // 同一个文件选两次没有意义，直接跳过
            if self.books.iter().any(|b| b.path == p) {
                continue;
            }
            match findb::Db::open_no_migrate(&p) {
                Ok(db) => {
                    let key = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "未命名".to_string());
                    let company = db.options().company.clone();
                    self.books.push(OpenBook {
                        key,
                        company,
                        path: p,
                        db,
                    });
                }
                Err(e) => {
                    self.err = format!("打不开 {}：{e}", p.display());
                }
            }
        }
        ctx.info(format!("已选 {} 个账套", self.books.len()));
        self.reload();
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.button("选择账套…（可多选）").clicked() {
                self.add_books(ctx);
            }
            ui.label("期间");
            let r = ui.add_sized([80.0, 22.0], egui::TextEdit::singleline(&mut self.period));
            if r.lost_focus() {
                self.reload();
            }
            if ui.button("重新汇总").clicked() {
                self.reload();
            }
            if !self.books.is_empty() && ui.button("清空").clicked() {
                self.books.clear();
                self.report = None;
            }
        });
        ui.add_space(4.0);

        if !self.err.is_empty() {
            ui.colored_label(palette::CREDIT, &self.err);
            ui.add_space(4.0);
        }

        if self.books.is_empty() {
            widgets::empty_hint(ui, "先选择要合并的账套（可多选 .fbk 文件）");
            return;
        }

        // 已选账套列表：可以逐个移除
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("参与合并：").weak());
            let mut remove: Option<usize> = None;
            for (i, b) in self.books.iter().enumerate() {
                let label = if b.company.is_empty() {
                    b.key.clone()
                } else {
                    format!("{}（{}）", b.company, b.key)
                };
                if ui.small_button(format!("{label} ✕")).clicked() {
                    remove = Some(i);
                }
            }
            if let Some(i) = remove {
                self.books.remove(i);
                self.reload();
            }
        });
        ui.add_space(6.0);

        let Some(rep) = self.report.clone() else {
            return;
        };
        ui.label(RichText::new(findb::consolidate::CONSOL_NOTE).weak());
        ui.add_space(4.0);

        // 列：科目编码 / 科目名称 / 每个账套一列 / 合计
        let mut cols: Vec<widgets::TCol> = vec![
            widgets::TCol::new("科目编码", 100.0).fixed(),
            widgets::TCol::new("科目名称", 160.0),
        ];
        for (_, company) in &rep.books {
            cols.push(widgets::TCol::new(company, 120.0).right());
        }
        cols.push(widgets::TCol::new("合计", 130.0).right());

        let n_books = rep.books.len();
        let rows = rep.rows.clone();
        let last = rows.len().saturating_sub(1);
        widgets::grid(ui, "consolidate", &cols, rows.len(), 24.0, |i, c, ui| {
            let r = &rows[i];
            match c {
                0 => {
                    ui.label(RichText::new(&r.account_code).monospace());
                }
                1 => {
                    ui.label(&r.account_name);
                }
                _ if c < 2 + n_books => {
                    let v = r.values[c - 2];
                    // 零余额压暗，让有数的账套跳出来
                    if v.is_zero() {
                        ui.label(RichText::new("—").weak());
                    } else {
                        widgets::amount_label(ui, v);
                    }
                }
                _ => {
                    let txt = RichText::new(r.total.fmt_money()).strong();
                    ui.label(if i == last { txt.underline() } else { txt });
                }
            }
        });
        widgets::empty_hint(ui, "");
    }
}

/// `2026-01` → `Period`
fn parse_period(s: &str) -> Option<Period> {
    let s = s.trim();
    let (y, m) = s.split_once('-')?;
    let y: i32 = y.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    Period::new(y, m).ok()
}
