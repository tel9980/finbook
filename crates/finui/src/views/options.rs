//! 账套参数

use egui::{RichText, Ui};
use fincore::{BookOptions, Period};

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

pub struct OptionsView {
    pub o: Option<BookOptions>,
    pub scheme: String,
    pub start: String,
    pub words: String,
    pub dirty: bool,
}

impl Default for OptionsView {
    fn default() -> Self {
        Self {
            o: None,
            scheme: String::new(),
            start: String::new(),
            words: String::new(),
            dirty: true,
        }
    }
}

impl OptionsView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let o = ctx.db().options();
        self.scheme = o
            .code_scheme
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join("-");
        self.start = o.start_period.code();
        self.words = o.voucher_words.join(",");
        self.o = Some(o);
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);
        let Some(o) = self.o.as_mut() else { return };

        widgets::page_header(ui, "账套参数", |ui| {
            ui.label(RichText::new("修改后需保存才生效").weak());
        });

        let mut do_save = false;
        let mut do_reload = false;
        let mut do_export = false;
        let mut do_import = false;
        let mut do_reset = false;
        widgets::toolbar(ui, |ui| {
            if ui.button("保存").clicked() {
                do_save = true;
            }
            if ui.button("重新载入").clicked() {
                do_reload = true;
            }
            ui.separator();
            if ui
                .button("导出配置")
                .on_hover_text("把整套账套参数存成 JSON 文件（换机器时带走）")
                .clicked()
            {
                do_export = true;
            }
            if ui
                .button("导入配置")
                .on_hover_text("从 JSON 文件恢复参数；编码级长/启用期间与当前账套不同会被拒绝")
                .clicked()
            {
                do_import = true;
            }
            if ui
                .button("恢复默认")
                .on_hover_text("只恢复偏好类设置（打印版式/预算控制/单据前缀/生产模块）；企业名称、编码级长、启用期间、默认科目不动")
                .clicked()
            {
                do_reset = true;
            }
        });
        if do_reload {
            self.dirty = true;
            return;
        }
        if do_reset {
            // 只动偏好：这几项与已经记过的账没关系，改了不会让存量凭证变味
            o.reset_preferences();
            self.save(ctx);
            return;
        }
        if do_export {
            let o2 = o.clone();
            self.export_config(ctx, &o2);
            return;
        }
        if do_import {
            self.import_config(ctx);
            return;
        }
        if do_save {
            self.save(ctx);
            return;
        }

        egui::Grid::new("book_options")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("企业名称：");
                ui.add_sized([320.0, 22.0], egui::TextEdit::singleline(&mut o.company));
                ui.end_row();

                ui.label("纳税识别号：");
                ui.add_sized([320.0, 22.0], egui::TextEdit::singleline(&mut o.tax_no));
                ui.end_row();

                ui.label("本位币：");
                ui.add_sized(
                    [120.0, 22.0],
                    egui::TextEdit::singleline(&mut o.base_currency),
                );
                ui.end_row();

                ui.label("启用期间：");
                ui.horizontal(|ui| {
                    ui.add_sized([120.0, 22.0], egui::TextEdit::singleline(&mut self.start));
                    ui.label(RichText::new("格式 2026-01，已有凭证后不建议修改").weak());
                });
                ui.end_row();

                ui.label("科目级长：");
                ui.horizontal(|ui| {
                    ui.add_sized([120.0, 22.0], egui::TextEdit::singleline(&mut self.scheme));
                    ui.label(RichText::new("如 4-2-2-2").weak());
                });
                ui.end_row();

                ui.label("凭证字：");
                ui.horizontal(|ui| {
                    ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut self.words));
                    ui.label(RichText::new("多个用逗号分隔，如 记,收,付,转").weak());
                });
                ui.end_row();
            });

        ui.add_space(10.0);
        ui.separator();
        ui.label(RichText::new("业务控制").strong());
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut o.enable_audit, "启用审核环节（未审核不能记账）");
            ui.checkbox(
                &mut o.require_cashier,
                "出纳签字（涉及现金/银行的凭证记账前须签字）",
            );
            ui.checkbox(&mut o.enable_qty, "启用数量核算");
            ui.checkbox(&mut o.enable_foreign, "启用外币核算");
            ui.checkbox(
                &mut o.enable_production,
                "启用生产制造（MRP / 排产 / 工序报工）",
            );
        });

        // 预算控制 + 单据前缀：网页端早就有，桌面端一直缺。
        // 缺的后果不是「不能用」，而是同一套账在两处看到不同的设置项 ——
        // 在网页上关掉的预算强控，回到桌面端根本找不到开关。
        ui.add_space(6.0);
        egui::Grid::new("biz_control_misc")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("预算控制");
                ui.horizontal(|ui| {
                    for (val, label) in [
                        ("", "关闭"),
                        ("warn", "超预算提醒（放行）"),
                        ("strong", "超预算强控（拒绝保存）"),
                    ] {
                        ui.radio_value(&mut o.budget_control, val.to_string(), label);
                    }
                });
                ui.end_row();

                ui.label("单据前缀");
                ui.horizontal(|ui| {
                    for (key, hint) in [
                        ("po", "采购订单 CG"),
                        ("so", "销售订单 XS"),
                        ("req", "请购单 QG"),
                        ("quo", "报价单 BJ"),
                        ("prod", "生产订单 SC"),
                    ] {
                        let e = o.doc_prefixes.entry(key.to_string()).or_default();
                        ui.add_sized([48.0, 20.0], egui::TextEdit::singleline(e))
                            .on_hover_text(hint);
                    }
                });
                ui.end_row();
            });
        ui.label(
            RichText::new("前缀只能用字母或数字、最多 8 个字符；留空用默认值。")
                .weak(),
        );

        ui.add_space(6.0);
        ui.separator();
        ui.label(RichText::new("业务凭证默认科目").strong());
        ui.horizontal_wrapped(|ui| {
            ui.label("应收");
            ui.add_sized(
                [86.0, 22.0],
                egui::TextEdit::singleline(&mut o.biz_accounts.ar),
            );
            ui.label("应付");
            ui.add_sized(
                [86.0, 22.0],
                egui::TextEdit::singleline(&mut o.biz_accounts.ap),
            );
            ui.label("收入");
            ui.add_sized(
                [86.0, 22.0],
                egui::TextEdit::singleline(&mut o.biz_accounts.income),
            );
            ui.label("销项税");
            ui.add_sized(
                [86.0, 22.0],
                egui::TextEdit::singleline(&mut o.biz_accounts.tax_sales),
            );
            ui.label("暂估材料");
            ui.add_sized(
                [86.0, 22.0],
                egui::TextEdit::singleline(&mut o.biz_accounts.material),
            );
            ui.label("资金");
            ui.add_sized(
                [86.0, 22.0],
                egui::TextEdit::singleline(&mut o.biz_accounts.fund),
            );
        });
        ui.label(
            RichText::new("须为末级科目；收付款单 / 发货收入 / 暂估等自动生成凭证的默认取此")
                .weak(),
        );

        ui.add_space(14.0);
        ui.separator();
        ui.label(RichText::new("打印预览").strong());
        ui.label(
            RichText::new("这些是账套级设置：改完保存，本账套所有报表打印都用同一套版式")
                .weak(),
        );
        egui::Grid::new("print_options")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("纸张方向");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut o.print.orientation, "auto".to_string(), "自动（列多转横向）");
                    ui.radio_value(&mut o.print.orientation, "portrait".to_string(), "纵向");
                    ui.radio_value(&mut o.print.orientation, "landscape".to_string(), "横向");
                });
                ui.end_row();

                ui.label("正文字号");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut o.print.font_pt)
                            .range(9..=14)
                            .suffix(" pt"),
                    );
                    ui.label(
                        RichText::new("9–14；列多或内容长时调小些，能少折几行")
                            .weak(),
                    );
                });
                ui.end_row();

                ui.label("页边距");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut o.print.margin_mm)
                            .range(5..=30)
                            .suffix(" mm"),
                    );
                    ui.label(RichText::new("针式打印机一般要留宽些").weak());
                });
                ui.end_row();

                ui.label("页眉页脚");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut o.print.show_company, "编制单位");
                    ui.checkbox(&mut o.print.show_printed_at, "打印时间");
                    ui.checkbox(&mut o.print.show_page_no, "页码");
                    ui.checkbox(&mut o.print.show_signatures, "落款");
                });
                ui.end_row();

                ui.label("落款文字");
                ui.add_sized(
                    [380.0, 22.0],
                    egui::TextEdit::singleline(&mut o.print.sign_line),
                );
                ui.end_row();

                ui.label("套打偏移");
                ui.horizontal(|ui| {
                    ui.label("左右");
                    ui.add(
                        egui::DragValue::new(&mut o.print.offset_x_mm)
                            .range(-20..=20)
                            .suffix(" mm"),
                    );
                    ui.label("上下");
                    ui.add(
                        egui::DragValue::new(&mut o.print.offset_y_mm)
                            .range(-20..=20)
                            .suffix(" mm"),
                    );
                    ui.label(
                        RichText::new("正数=向右/向下；针式打印机走纸偏时用来对位")
                            .weak(),
                    );
                });
                ui.end_row();

                // 三种纸同时在小厂里用：凭证 A5/241 连续纸、账簿 A4、单据 A5/三等分。
                // 一个全局纸型必然有一头打歪，所以分三档。
                ui.label("套打纸型");
                ui.vertical(|ui| {
                    let choices = fincore::print::paper_choices();
                    for (label, val) in [
                        ("凭证", &mut o.print.voucher_paper),
                        ("账簿", &mut o.print.ledger_paper),
                        ("单据", &mut o.print.doc_paper),
                    ] {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            let cur = if val.trim().is_empty() {
                                "跟随单据模板".to_string()
                            } else {
                                choices
                                    .iter()
                                    .find(|(v, _)| v.eq_ignore_ascii_case(val.trim()))
                                    .map(|(_, l)| l.to_string())
                                    .unwrap_or_else(|| val.clone())
                            };
                            egui::ComboBox::from_id_salt(format!("paper_{label}"))
                                .selected_text(cur)
                                .show_ui(ui, |ui| {
                                    for (v, l) in choices.iter() {
                                        // 「跟随单据模板」只对单据有意义
                                        if v.is_empty() && label != "单据" {
                                            continue;
                                        }
                                        if ui
                                            .selectable_label(
                                                val.eq_ignore_ascii_case(v),
                                                *l,
                                            )
                                            .clicked()
                                        {
                                            *val = v.to_string();
                                        }
                                    }
                                });
                        });
                    }
                    ui.label(
                        RichText::new("凭证多为 A5 或 241×140 连续纸；账簿 A4；单据跟随模板")
                            .weak(),
                    );
                });
                ui.end_row();
            });
        ui.label(
            RichText::new(
                "页码靠浏览器的「页边距盒子」实现（第 N 页/共 M 页）。\
                 旧内核（Chrome 131 之前等）会整块忽略而不报错 —— 表现为勾了页码但纸上没有，\
                 这时候改用新版浏览器，或者把页码关掉。",
            )
            .weak(),
        );
        let probs = o.print.problems();
        if !probs.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(0xB4, 0x6A, 0x00), probs.join("；"));
        }

        ui.add_space(14.0);
        ui.separator();
        ui.label(RichText::new("账套信息").strong());
        let path = ctx
            .st
            .book_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "（未保存为文件）".to_string());
        widgets::kv(ui, "文件位置", &path);
        let (v, e, a) = ctx.db().stats().unwrap_or((0, 0, 0));
        widgets::kv(ui, "凭证数", &v.to_string());
        widgets::kv(ui, "分录数", &e.to_string());
        widgets::kv(ui, "科目数", &a.to_string());

        ui.add_space(10.0);
        ui.label(RichText::new("科目表维护").strong());
        ui.horizontal(|ui| {
            if ui.button("补齐新版科目表").clicked() {
                match findb::accounts::fill_missing_defaults(ctx.db()) {
                    Ok(0) => ctx.info("科目表已完整，无需补齐"),
                    Ok(n) => {
                        ctx.log("科目", "补齐科目表", &format!("补入 {n} 个内置科目"));
                        ctx.info(format!("已补入 {n} 个内置科目"));
                        ctx.reload_chart();
                        // 直接落库，无需经过账套参数的「保存」
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
            ui.label(
                RichText::new(
                    "旧账套一键补入新版默认科目表中缺少的科目（当前内置 199 个），不影响已有科目。",
                )
                .weak(),
            );
        });
    }

    /// 导出整套参数到 JSON 文件
    ///
    /// 整个对象原样写出，不挑字段：少导出一个字段，导入后它就静默回到默认值，
    /// 而这种「换了机器某个设置不一样」的问题极难自查。
    fn export_config(&mut self, ctx: &mut AppCtx<'_>, o: &BookOptions) {
        let name = format!(
            "{}_账套参数_{}.json",
            if o.company.trim().is_empty() { "账套" } else { o.company.trim() },
            chrono::Local::now().format("%Y%m%d")
        );
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&name)
            .add_filter("JSON", &["json"])
            .save_file()
        else {
            return;
        };
        match serde_json::to_string_pretty(o) {
            Ok(text) => match std::fs::write(&path, text) {
                Ok(_) => {
                    ctx.log("账套", "导出账套参数", &path.display().to_string());
                    ctx.info(format!("已导出到 {}", path.display()));
                }
                Err(e) => ctx.toast(format!("写文件失败：{e}")),
            },
            Err(e) => ctx.toast(format!("序列化参数失败：{e}")),
        }
    }

    /// 从 JSON 文件导入参数
    ///
    /// 结构项（编码级长 / 启用期间）不同就整体拒绝且一个字段都不改 —— 见
    /// `BookOptions::adopt_config`。拒绝理由原样显示，用户要知道是哪一项不一样。
    fn import_config(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                ctx.toast(format!("读文件失败：{e}"));
                return;
            }
        };
        let incoming: BookOptions = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                ctx.toast(format!("这个文件不是账套参数（{}）：{e}", path.display()));
                return;
            }
        };
        let mut o = match self.o.clone() {
            Some(v) => v,
            None => return,
        };
        if let Err(problems) = o.adopt_config(&incoming) {
            ctx.toast(problems.join("；"));
            return;
        }
        self.o = Some(o);
        self.save(ctx);
        ctx.info(format!("已从 {} 导入配置", path.display()));
    }

    fn save(&mut self, ctx: &mut AppCtx<'_>) {
        let mut o = self.o.clone().unwrap();
        match Period::parse(&self.start) {
            Ok(p) => o.start_period = p,
            Err(e) => {
                ctx.error(format!("启用期间格式不正确：{e}"));
                return;
            }
        }
        match self
            .scheme
            .split(['-', ' ', ',', '.'])
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().parse::<u8>())
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(v) if !v.is_empty() && v.iter().all(|x| *x > 0) => o.code_scheme = v,
            _ => {
                ctx.error("科目级长格式不正确，应形如 4-2-2-2");
                return;
            }
        }
        o.voucher_words = self
            .words
            .split([',', '，', ' '])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if o.voucher_words.is_empty() {
            o.voucher_words = fincore::chart::default_voucher_words();
        }
        o.company = o.company.trim().to_string();
        o.tax_no = o.tax_no.trim().to_string();
        o.base_currency = o.base_currency.trim().to_uppercase();

        let r = ctx.db().set_options(&o);
        if ctx.handle(r).is_some() {
            ctx.log("账套", "修改账套参数", &o.company);
            ctx.info("账套参数已保存");
            self.dirty = true;
        }
    }
}

/// 供状态栏显示
pub fn options_hint(o: &BookOptions) -> String {
    let mut s = Vec::new();
    if o.require_cashier {
        s.push("出纳签字");
    }
    if o.enable_audit {
        s.push("需审核");
    }
    if o.enable_qty {
        s.push("数量核算");
    }
    if o.enable_foreign {
        s.push("外币核算");
    }
    if s.is_empty() {
        "无特殊控制".to_string()
    } else {
        s.join(" · ")
    }
}

/// 未通过校验时的提示色
pub fn warn_color() -> egui::Color32 {
    palette::WARN
}
