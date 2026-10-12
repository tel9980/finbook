//! 辅助档案批量粘贴解析
//!
//! 为什么要它：一家公司 50 个客户 + 30 个供应商 + 20 个员工，按「新增」按钮
//! 一个个敲是 100 次点开、填编码、填名称、保存。而财务手上几乎一定已经有一份
//! Excel/WPS 的名单 —— 让他们把两列复制过来粘贴一次，比让他们重打一遍便宜得多。
//!
//! 支持两种写法（都能从 Excel 直接复制）：
//!   1. `编码,名称` 两列 —— 有自己的编码体系时用
//!   2. 只写名称      —— 编码按类型前缀自动递增（C01/C02…、S01/S02…），
//!      小白多半不知道怎么给档案编编码，让他自己填反而是个坎
//!
//! 分隔符容忍逗号/中文逗号/制表符/全角空格：Excel 复制出来的是 `\t`，
//! WPS 有时给中文逗号，全角逗号也见过。标题行（含「编码」「名称」等）自动跳过。

use egui::RichText;
use fincore::{AuxEntity, AuxKind};

/// 解析结果里的一行
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRow {
    pub code: String,
    pub name: String,
    /// 该行被跳过的原因（None = 可导入）
    pub skip: Option<SkipReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkipReason {
    /// 与本批次里前面的行重复
    DuplicateInBatch,
    /// 库里已有这个编码
    ExistsInBook,
}

/// 解析结果
pub struct Parsed {
    pub rows: Vec<ParsedRow>,
    /// 没能解析出任何内容的行（比如空行已被清理，但整段都是垃圾）
    pub empty: bool,
}

impl Parsed {
    pub fn importable(&self) -> Vec<ParsedRow> {
        self.rows.iter().filter(|r| r.skip.is_none()).cloned().collect()
    }
}

/// 各类档案的编码前缀。与项目里既有惯例一致（示例账套就是 C01/S01/D01/E01/B01）。
pub fn code_prefix(kind: AuxKind) -> &'static str {
    match kind {
        AuxKind::Customer => "C",
        AuxKind::Supplier => "S",
        AuxKind::Dept => "D",
        AuxKind::Employee => "E",
        AuxKind::Item => "I",
        AuxKind::Bank => "B",
        _ => "X",
    }
}

fn is_header_row(cols: &[&str]) -> bool {
    // 表头通常是「编码 名称」或「名称 编码」，命中任意一个关键字就算
    let hit = cols.iter().any(|c| {
        let c = c.trim();
        c == "编码" || c == "代码" || c == "编号" || c == "code" || c == "名称" || c == "name"
    });
    hit
}

/// 把一行拆成若干列。
///
/// 优先用 tab（Excel 复制的真实分隔符），没有 tab 再退到逗号/中文逗号/顿号，
/// 最后按连续空格切 —— 这样从记事本贴「C01 杭州明远贸易」也能work。
fn split_cols(line: &str) -> Vec<String> {
    if line.contains('\t') {
        return line.split('\t').map(|s| s.trim().to_string()).collect();
    }
    if line.contains(',') || line.contains('，') || line.contains('、') {
        return line
            .split([',', '，', '、'])
            .map(|s| s.trim().to_string())
            .collect();
    }
    line.split_whitespace().map(|s| s.to_string()).collect()
}

/// 解析粘贴文本。
///
/// `existing` 是库里已有的编码集合，用来标出「这个编码已经有了」；
/// 传空集合就只做批次内去重（便于纯解析的单元测试）。
pub fn parse(text: &str, kind: AuxKind, existing: &[String]) -> Parsed {
    let prefix = code_prefix(kind);
    let mut rows: Vec<ParsedRow> = Vec::new();
    let mut seen_in_batch: Vec<String> = Vec::new();
    // 自动编号从「库里最大号 +1」接着往下走，避免撞上已有的 C01
    let mut next_num = 1 + existing
        .iter()
        .filter_map(|c| {
            let rest = c.strip_prefix(prefix)?;
            rest.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0);
    // 批次内自动生成的编码也要接着排，别和显式写的 C05 撞车
    let mut auto_taken: Vec<String> = Vec::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let cols = split_cols(line);
        let cols: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
        if cols.is_empty() {
            continue;
        }
        // 首个非空行像表头就跳过（编码/名称 名称/编码）
        if lineno == 0 && is_header_row(&cols) {
            continue;
        }

        // 只有一列 → 名称，编码自动生成
        // 两列 → 编码,名称（容忍顺序反了：纯中文的一律当名称）
        let (code, name) = match cols.len() {
            1 => {
                let name = cols[0].to_string();
                let mut code = format!("{}{:02}", prefix, next_num);
                while auto_taken.iter().any(|c| *c == code) || existing.iter().any(|c| *c == code) {
                    next_num += 1;
                    code = format!("{}{:02}", prefix, next_num);
                }
                next_num += 1;
                auto_taken.push(code.clone());
                (code, name)
            }
            _ => {
                let (a, b) = (cols[0], cols[1]);
                let looks_like_code = |s: &str| {
                    !s.is_empty()
                        && s.chars().count() <= 12
                        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                        && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                };
                if looks_like_code(a) && !looks_like_code(b) {
                    (a.to_string(), b.to_string())
                } else if looks_like_code(b) && !looks_like_code(a) {
                    (b.to_string(), a.to_string())
                } else {
                    // 两列都不是编码形状 → 当成「名称,名称」，仍自动编号
                    (String::new(), cols.join(" "))
                }
            }
        };

        if name.trim().is_empty() {
            continue;
        }
        let code = if code.trim().is_empty() {
            format!("{}{:02}", prefix, next_num)
        } else {
            code.trim().to_string()
        };

        let skip = if seen_in_batch.iter().any(|c| *c == code) {
            Some(SkipReason::DuplicateInBatch)
        } else if existing.iter().any(|c| *c == code) {
            Some(SkipReason::ExistsInBook)
        } else {
            seen_in_batch.push(code.clone());
            None
        };

        rows.push(ParsedRow { code, name: name.trim().to_string(), skip });
    }

    Parsed { empty: rows.is_empty(), rows }
}

/// 把可导入的行转成待写入的实体
pub fn to_entities(kind: AuxKind, rows: &[ParsedRow]) -> Vec<AuxEntity> {
    rows.iter()
        .filter(|r| r.skip.is_none())
        .map(|r| AuxEntity::new(kind, r.code.clone(), r.name.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// 批量粘贴窗口的 UI 状态
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Ui {
    pub open: bool,
    pub text: String,
    /// 解析预览（点「预览」后填充）
    pub preview: Option<Parsed>,
    /// 导入完成后的结果提示
    pub done: Option<String>,
    pub err: String,
}

impl Ui {
    /// 展示批量粘贴窗口。`existing` 是库里已有的编码（用来标出重复）。
    /// 导入成功后返回 true，调用方据此刷新列表。
    pub fn window(
        &mut self,
        ctx: &mut crate::state::AppCtx<'_>,
        kind: AuxKind,
        existing: &[String],
        egui_ui: &mut egui::Ui,
    ) -> bool {
        if !self.open {
            return false;
        }
        let mut keep_open = true;
        let mut do_preview = false;
        let mut do_import = false;
        let mut close = false;

        egui::Window::new("批量新增档案")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(true)
            .default_width(580.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(egui_ui.ctx(), |ui| {
                ui.label(
                    RichText::new(
                        "把 Excel 里的名单复制过来粘贴，每行一条。\n\
                         两种写法都行：\n\
                         • 两列「编码,名称」—— 有自己的编码体系时用\n\
                         • 只写名称 —— 编码自动编号（C01、C02…），不用自己想编码",
                    )
                    .weak(),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("粘贴到这里：").strong());
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .desired_rows(10)
                        .hint_text("C01,杭州明远贸易有限公司\nC02,宁波华信电子有限公司"),
                );

                if !self.err.is_empty() {
                    ui.colored_label(crate::theme::palette::CREDIT, &self.err);
                }

                ui.horizontal(|ui| {
                    if ui.button("预览").clicked() {
                        do_preview = true;
                    }
                    if ui.button("确认导入").clicked() {
                        do_import = true;
                    }
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });

                if do_preview {
                    let p = parse(&self.text, kind, existing);
                    if p.empty {
                        self.preview = None;
                        self.err =
                            "没解析出内容。每行一条，编码和名称之间用逗号或 Tab 隔开就行。".to_string();
                    } else {
                        self.err.clear();
                        self.preview = Some(p);
                    }
                }

                if let Some(p) = &self.preview {
                    let ok = p.rows.iter().filter(|r| r.skip.is_none()).count();
                    let dup = p.rows.len() - ok;
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("解析到 {} 条，其中 {} 条将导入", p.rows.len(), ok)).strong());
                    if dup > 0 {
                        ui.colored_label(
                            crate::theme::palette::CREDIT,
                            &format!("{dup} 条编码已存在，将跳过（不会覆盖原有档案）"),
                        );
                    }
                    let cols = [
                        crate::widgets::TCol::new("编码", 110.0).fixed(),
                        crate::widgets::TCol::new("名称", 260.0),
                        crate::widgets::TCol::new("状态", 130.0).fixed(),
                    ];
                    let rows = p.rows.clone();
                    crate::widgets::grid(ui, "aux_batch_preview", &cols, rows.len(), 22.0, |i, c, ui| {
                        match c {
                            0 => {
                                ui.label(&rows[i].code);
                            }
                            1 => {
                                ui.label(&rows[i].name);
                            }
                            _ => {
                                let txt = match &rows[i].skip {
                                    None => "将导入",
                                    Some(SkipReason::ExistsInBook) => "跳过：已存在",
                                    Some(SkipReason::DuplicateInBatch) => "跳过：本批内重复",
                                };
                                ui.label(RichText::new(txt).weak());
                            }
                        }
                    });
                }

                if let Some(msg) = &self.done {
                    ui.add_space(6.0);
                    ui.colored_label(crate::theme::palette::OK, msg);
                }
            });

        if !keep_open || close {
            self.open = false;
            self.text.clear();
            self.preview = None;
            self.done = None;
            self.err.clear();
        }

        if do_import {
            // 先要求点过「预览」：直接点确认就把一坨文本当数据写库，
            // 粘贴错位（多带表头、带上了合计行）会静默变成脏档案。
            let Some(p) = &self.preview else {
                self.err = "请先点「预览」确认要导入哪些".to_string();
                return false;
            };
            let items = to_entities(kind, &p.importable());
            if items.is_empty() {
                self.err = "没有可导入的新档案（编码都已存在）".to_string();
            } else {
                match findb::auxs::insert_many_skip_existing(ctx.db(), &items) {
                    Ok((n, skipped)) => {
                        let mut msg = format!("已导入 {n} 条");
                        if !skipped.is_empty() {
                            msg.push_str(&format!("，跳过 {} 条（编码已存在）", skipped.len()));
                        }
                        self.done = Some(msg);
                        self.text.clear();
                        self.preview = None;
                        return true;
                    }
                    Err(e) => self.err = format!("导入失败：{e}"),
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(p: &Parsed) -> Vec<String> {
        p.rows.iter().map(|r| r.code.clone()).collect()
    }

    #[test]
    fn 解析两列_编码加名称() {
        let p = parse("C01,杭州明远\nC02,宁波华信", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01", "C02"]);
        assert_eq!(p.rows[1].name, "宁波华信");
        assert!(p.rows.iter().all(|r| r.skip.is_none()));
    }

    #[test]
    fn 解析Excel制表符() {
        // Excel 复制两列出来就是 tab 分隔
        let p = parse("C01\t杭州明远\nC02\t宁波华信", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01", "C02"]);
        assert_eq!(p.rows[0].name, "杭州明远");
    }

    #[test]
    fn 只给名称时自动编号() {
        let p = parse("杭州明远\n宁波华信", AuxKind::Customer, &[]);
        assert_eq!(codes(&p), vec!["C01", "C02"]);
        assert_eq!(p.rows[0].name, "杭州明远");
    }

    /// 小白最容易踩的：编号要从库里的最大值接着排，不能从 01 重来撞车
    #[test]
    fn 自动编号接着库里的最大号() {
        let existing = vec!["C01".to_string(), "C02".to_string(), "C07".to_string()];
        let p = parse("新客户甲\n新客户乙", AuxKind::Customer, &existing);
        assert_eq!(codes(&p), vec!["C08", "C09"]);
    }

    /// 不同档案用不同前缀，别全成 C01
    #[test]
    fn 各类型前缀不同() {
        for (k, want) in [
            (AuxKind::Customer, "C01"),
            (AuxKind::Supplier, "S01"),
            (AuxKind::Dept, "D01"),
            (AuxKind::Employee, "E01"),
            (AuxKind::Bank, "B01"),
        ] {
            let p = parse("某某", k, &[]);
            assert_eq!(p.rows[0].code, want, "{:?} 的前缀不对", k);
        }
    }

    /// 从 Excel 整块复制过来第一行通常是表头，不能当成一条数据
    #[test]
    fn 跳过表头行() {
        let p = parse("编码,名称\nC01,杭州明远", AuxKind::Customer, &[]);
        assert_eq!(p.rows.len(), 1, "表头不该被当成数据");
        assert_eq!(p.rows[0].code, "C01");
    }

    /// 冲突必须**报告出来**而不是静默覆盖 —— 覆盖会悄悄改掉客户名
    #[test]
    fn 库里已有的编码标为跳过() {
        let existing = vec!["C01".to_string()];
        let p = parse("C01,新名字\nC02,新客户", AuxKind::Customer, &existing);
        assert_eq!(p.rows[0].skip, Some(SkipReason::ExistsInBook));
        assert_eq!(p.rows[1].skip, None);
        assert_eq!(p.importable().len(), 1, "只有新的那条该被导入");
    }

    #[test]
    fn 批次内重复也标出() {
        let p = parse("C01,甲\nC01,乙", AuxKind::Customer, &[]);
        assert_eq!(p.rows[0].skip, None);
        assert_eq!(p.rows[1].skip, Some(SkipReason::DuplicateInBatch));
    }

    /// 从 WPS/记事本贴过来的空行和首尾空格不能变成空档案
    #[test]
    fn 清理空行与空白() {
        let p = parse("\n  \nC01,杭州明远\n\n  C02 , 宁波华信  \n", AuxKind::Customer, &[]);
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].name, "杭州明远");
        assert_eq!(p.rows[1].code, "C02");
        assert_eq!(p.rows[1].name, "宁波华信");
    }

    #[test]
    fn 全空输入不报错() {
        let p = parse("", AuxKind::Customer, &[]);
        assert!(p.empty);
        assert_eq!(p.importable().len(), 0);
    }
}