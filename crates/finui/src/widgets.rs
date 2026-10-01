//! 通用界面控件
//!
//! 财务软件界面高度重复：工具栏、过滤条、表格、金额列、弹窗选择器。
//! 这里统一实现，各功能模块只关心业务列与数据。

use egui::{Align, Color32, Context, Id, Layout, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use fincore::{Account, Chart, Direction, Money};

use crate::theme::palette;

// ---------------------------------------------------------------------------
// 文本与数值
// ---------------------------------------------------------------------------

/// 金额显示：负数红字、千分位
pub fn amount_label(ui: &mut Ui, v: Money) {
    let txt = if v.is_zero() {
        // 财务报表惯例：零值留白，减少视觉噪音
        RichText::new("").color(Color32::GRAY)
    } else {
        RichText::new(v.fmt_money()).color(crate::theme::amount_color(v))
    };
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.label(txt);
    });
}

/// 带借贷方向的金额显示，如 `借 1,000.00`
pub fn dir_amount_label(ui: &mut Ui, dir: Direction, v: Money) {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let color = if dir == Direction::Credit && !v.is_zero() {
            palette::CREDIT
        } else {
            palette::DEBIT
        };
        if v.is_zero() {
            ui.label(RichText::new("平").weak());
        } else {
            ui.label(RichText::new(format!("{} {}", dir.label(), v.fmt_money())).color(color));
        }
    });
}

/// 金额输入框（右对齐，编辑时显示原始文本）
pub fn money_input(ui: &mut Ui, buf: &mut String, width: f32) -> egui::Response {
    ui.add_sized(
        [width, ui.available_height()],
        egui::TextEdit::singleline(buf)
            .horizontal_align(Align::RIGHT)
            .hint_text("0.00"),
    )
}

/// 可空的金额输入框（空表示不填）
pub fn money_input_opt(ui: &mut Ui, buf: &mut Option<String>, width: f32) -> egui::Response {
    let mut text = buf.clone().unwrap_or_default();
    let r = ui.add_sized(
        [width, ui.available_height()],
        egui::TextEdit::singleline(&mut text).horizontal_align(Align::RIGHT),
    );
    if r.changed() {
        *buf = if text.trim().is_empty() { None } else { Some(text) };
    }
    r
}

/// 从编辑缓冲区解析金额
pub fn parse_money(buf: &str) -> Money {
    Money::parse_or_zero(buf)
}

/// 文本输入框
pub fn text_input(ui: &mut Ui, buf: &mut String, width: f32, hint: &str) -> egui::Response {
    ui.add_sized(
        [width, ui.available_height()],
        egui::TextEdit::singleline(buf).hint_text(hint),
    )
}

// ---------------------------------------------------------------------------
// 布局
// ---------------------------------------------------------------------------

/// 页面标题 + 右侧操作区
pub fn page_header<R>(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.heading(RichText::new(title).strong());
        ui.add_space(8.0);
        ui.with_layout(Layout::right_to_left(Align::Center), right)
            .inner
    })
    .inner
}

/// 工具栏
pub fn toolbar<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgb(246, 248, 250))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(224, 228, 234)))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                content(ui)
            })
            .inner
        })
        .inner
}

/// 键值行
pub fn kv(ui: &mut Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).weak());
        ui.label(RichText::new(value).strong());
    });
}

/// 分组卡片。折叠时内容不执行（`body_returned` 为 None），因此不返回值；
/// 业务数据回传请用外部 `&mut`/`Rc<RefCell<_>>` 收集。
pub fn card<R>(ui: &mut Ui, title: &str, content: impl FnOnce(&mut Ui) -> R) {
    egui::CollapsingHeader::new(RichText::new(title).strong())
        .default_open(true)
        .show(ui, content);
}

/// 空数据提示
pub fn empty_hint(ui: &mut Ui, text: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space(24.0);
        ui.label(RichText::new(text).weak().size(15.0));
        ui.add_space(24.0);
    });
}

/// 搜索框
pub fn search_field(ui: &mut Ui, buf: &mut String, hint: &str) -> egui::Response {
    let r = ui.add(
        egui::TextEdit::singleline(buf)
            .hint_text(hint)
            .desired_width(180.0),
    );
    if ui.button("×").on_hover_text("清空").clicked() {
        buf.clear();
    }
    r
}

/// 分页控件
pub struct Paging {
    pub page: usize,
    pub size: usize,
}

impl Default for Paging {
    fn default() -> Self {
        Self { page: 0, size: 200 }
    }
}

impl Paging {
    pub fn reset(&mut self) {
        self.page = 0;
    }
    pub fn total_pages(&self, total: usize) -> usize {
        (total + self.size - 1) / self.size.max(1)
    }
    pub fn slice<'a, T>(&self, items: &'a [T]) -> &'a [T] {
        let start = (self.page * self.size).min(items.len());
        let end = (start + self.size).min(items.len());
        &items[start..end]
    }
    /// 底部翻页条
    pub fn bar(&mut self, ui: &mut Ui, total: usize) {
        ui.horizontal(|ui| {
            let pages = self.total_pages(total).max(1);
            ui.label(format!("共 {total} 条 / {pages} 页"));
            if ui.button("⏮").clicked() {
                self.page = 0;
            }
            if ui.button("◀").clicked() && self.page > 0 {
                self.page -= 1;
            }
            ui.add(
                egui::DragValue::new(&mut self.page)
                    .range(0..=pages.saturating_sub(1))
                    .suffix(" 页"),
            );
            if ui.button("▶").clicked() && self.page + 1 < pages {
                self.page += 1;
            }
            if ui.button("⏭").clicked() {
                self.page = pages.saturating_sub(1);
            }
            ui.add_space(12.0);
            ui.label("每页");
            ui.add(egui::DragValue::new(&mut self.size).range(20..=2000));
            if self.page >= pages {
                self.page = pages.saturating_sub(1);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// 表格
// ---------------------------------------------------------------------------

/// 表格列定义
pub struct TCol {
    pub name: String,
    pub width: f32,
    /// 是否右对齐（金额列）
    pub right: bool,
    /// 是否可伸缩
    pub resizable: bool,
}

impl TCol {
    pub fn new(name: &str, width: f32) -> Self {
        Self {
            name: name.to_string(),
            width,
            right: false,
            resizable: true,
        }
    }
    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }
    pub fn fixed(mut self) -> Self {
        self.resizable = false;
        self
    }
}

/// 数据表格。render_cell 接收 (行号, 列号, ui)。
pub fn grid(
    ui: &mut Ui,
    id: &str,
    cols: &[TCol],
    row_count: usize,
    row_height: f32,
    mut render_cell: impl FnMut(usize, usize, &mut Ui),
) {
    if row_count == 0 {
        empty_hint(ui, "没有符合条件的数据");
        return;
    }
    let mut b = TableBuilder::new(ui)
        .id_salt(id)
        .striped(true)
        .resizable(true)
        .min_scrolled_height(0.0);

    for c in cols {
        let col = Column::initial(c.width).at_least(if c.right { 60.0 } else { 40.0 });
        let col = if c.resizable { col.resizable(true) } else { col };
        b = b.column(col);
    }

    b.header(26.0, |mut header| {
        for c in cols {
            header.col(|ui| {
                let layout = if c.right {
                    Layout::right_to_left(Align::Center)
                } else {
                    Layout::left_to_right(Align::Center)
                };
                ui.with_layout(layout, |ui| {
                    ui.label(RichText::new(&c.name).strong());
                });
            });
        }
    })
    .body(|body| {
        body.rows(row_height, row_count, |mut row| {
            let i = row.index();
            for ci in 0..cols.len() {
                row.col(|ui| {
                    if cols[ci].right {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            render_cell(i, ci, ui);
                        });
                    } else {
                        render_cell(i, ci, ui);
                    }
                });
            }
        });
    });
}

// ---------------------------------------------------------------------------
// 选择器
// ---------------------------------------------------------------------------

/// 下拉选择（字符串列表）
pub fn combo(
    ui: &mut Ui,
    id: &str,
    current: &mut String,
    options: &[String],
    width: f32,
) -> egui::Response {
    let selected = current.clone();
    let r = egui::ComboBox::from_id_salt(id)
        .selected_text(if selected.is_empty() {
            "（请选择）".to_string()
        } else {
            selected
        })
        .width(width)
        .show_ui(ui, |ui| {
            for o in options {
                ui.selectable_value(current, o.clone(), o);
            }
        });
    r.response
}

/// 科目下拉（只列末级科目时可用 leaf_only 过滤）
pub fn account_combo(
    ui: &mut Ui,
    id: &str,
    current: &mut String,
    chart: &Chart,
    leaf_only: bool,
    width: f32,
) {
    let label = match chart.get(current) {
        Some(a) => format!("{} {}", a.code, a.name),
        None => "（请选择科目）".to_string(),
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .width(width)
        .show_ui(ui, |ui| {
            for a in chart.all() {
                if leaf_only && !chart.is_leaf(&a.code) {
                    continue;
                }
                let indent = "　".repeat((a.level(&chart.scheme().0) as usize).saturating_sub(1));
                let text = format!("{indent}{} {}", a.code, a.name);
                ui.selectable_value(current, a.code.clone(), text);
            }
        });
}

/// 科目选择弹窗状态
#[derive(Default, Clone)]
pub struct AccountPickerState {
    pub open: bool,
    pub search: String,
    /// 打开时针对的行号
    pub row: usize,
    /// 是否只显示末级
    pub leaf_only: bool,
}

impl AccountPickerState {
    /// 显示窗口，返回用户选中的科目编码
    pub fn show(&mut self, ctx: &Context, chart: &Chart) -> Option<String> {
        if !self.open {
            return None;
        }
        let mut picked: Option<String> = None;
        let mut open = true;
        let kw = self.search.trim().to_lowercase();

        egui::Window::new("选择会计科目")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([520.0, 460.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("查找：");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("输入科目编码或名称")
                            .desired_width(260.0),
                    );
                    if ui.button("清空").clicked() {
                        self.search.clear();
                    }
                    ui.checkbox(&mut self.leaf_only, "只显示末级");
                });
                ui.separator();

                let matches: Vec<&Account> = chart
                    .all()
                    .into_iter()
                    .filter(|a| {
                        if self.leaf_only && !chart.is_leaf(&a.code) {
                            return false;
                        }
                        if kw.is_empty() {
                            return true;
                        }
                        a.code.to_lowercase().contains(&kw)
                            || a.name.to_lowercase().contains(&kw)
                    })
                    .collect();

                ui.label(format!("匹配 {} 个科目", matches.len()));
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    grid(
                        ui,
                        "acct_picker",
                        &[TCol::new("科目编码", 120.0), TCol::new("科目名称", 200.0)],
                        matches.len(),
                        22.0,
                        |i, c, ui| {
                            let a = matches[i];
                            if c == 0 {
                                if ui
                                    .selectable_label(false, RichText::new(&a.code).monospace())
                                    .clicked()
                                {
                                    picked = Some(a.code.clone());
                                }
                            } else if c == 1 {
                                if ui.selectable_label(false, &a.name).clicked() {
                                    picked = Some(a.code.clone());
                                }
                            }
                        },
                    );
                });
            });

        if picked.is_some() || !open {
            self.open = false;
            self.search.clear();
        }
        picked
    }
}

// ---------------------------------------------------------------------------
// 提示与确认
// ---------------------------------------------------------------------------

/// 右下角浮动提示
pub fn toasts(ctx: &Context, queue: &mut crate::state::ToastQueue, now: f64) {
    queue.retain(now);
    if queue.0.is_empty() {
        return;
    }
    egui::Area::new(Id::new("toasts"))
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                for t in queue.0.iter().rev() {
                    let color = if t.is_error {
                        palette::CREDIT
                    } else {
                        palette::OK
                    };
                    egui::Frame::NONE
                        .fill(egui::Color32::from_rgb(252, 252, 253))
                        .stroke(egui::Stroke::new(1.5, color))
                        .corner_radius(4.0)
                        .inner_margin(8.0)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 2],
                            blur: 8,
                            spread: 0,
                            color: Color32::from_black_alpha(40),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(color, if t.is_error { "✖" } else { "✔" });
                                ui.label(&t.msg);
                            });
                        });
                    ui.add_space(4.0);
                }
            });
        });
}

/// 二次确认窗口
pub fn confirm_window(
    ctx: &Context,
    c: &crate::state::Confirm,
) -> Option<bool> {
    let mut result: Option<bool> = None;
    let mut open = true;
    let title = c.title.clone();
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.add_space(6.0);
                let txt = if c.dangerous {
                    RichText::new(&c.message).color(palette::CREDIT)
                } else {
                    RichText::new(&c.message)
                };
                ui.label(txt);
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            result = Some(false);
                        }
                        let ok = egui::Button::new(
                            RichText::new("确定").color(if c.dangerous {
                                Color32::WHITE
                            } else {
                                Color32::WHITE
                            }),
                        )
                        .fill(if c.dangerous {
                            palette::CREDIT
                        } else {
                            palette::PRIMARY
                        });
                        if ui.add(ok).clicked() {
                            result = Some(true);
                        }
                    });
                });
            });
        });
    if !open {
        result = Some(false);
    }
    result
}
