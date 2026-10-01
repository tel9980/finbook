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
        widgets::toolbar(ui, |ui| {
            if ui.button("保存").clicked() {
                do_save = true;
            }
            if ui.button("重新载入").clicked() {
                do_reload = true;
            }
        });
        if do_reload {
            self.dirty = true;
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
        });

        ui.add_space(6.0);
        ui.separator();
        ui.label(RichText::new("业务凭证默认科目").strong());
        ui.horizontal_wrapped(|ui| {
            ui.label("应收");
            ui.add_sized([86.0, 22.0], egui::TextEdit::singleline(&mut o.biz_accounts.ar));
            ui.label("应付");
            ui.add_sized([86.0, 22.0], egui::TextEdit::singleline(&mut o.biz_accounts.ap));
            ui.label("收入");
            ui.add_sized([86.0, 22.0], egui::TextEdit::singleline(&mut o.biz_accounts.income));
            ui.label("销项税");
            ui.add_sized([86.0, 22.0], egui::TextEdit::singleline(&mut o.biz_accounts.tax_sales));
            ui.label("暂估材料");
            ui.add_sized(
                [86.0, 22.0],
                egui::TextEdit::singleline(&mut o.biz_accounts.material),
            );
            ui.label("资金");
            ui.add_sized([86.0, 22.0], egui::TextEdit::singleline(&mut o.biz_accounts.fund));
        });
        ui.label(
            RichText::new("须为末级科目；收付款单 / 发货收入 / 暂估等自动生成凭证的默认取此").weak(),
        );

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
                RichText::new("旧账套一键补入新版默认科目表中缺少的科目（当前内置 199 个），不影响已有科目。")
                    .weak(),
            );
        });
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
