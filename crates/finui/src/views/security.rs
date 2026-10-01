//! 安全中心：口令策略、账户锁定、登录审计、数据范围
//!
//! 单机财务软件真正的安全风险通常不是网络攻击，而是"谁都能坐到这台电脑前记账"。
//! 所以这里把四件事放在一个界面里：口令有多严、谁被锁了、谁在什么时候试着登录过、
//! 以及每个人到底能看到哪些数据——管理员不用在菜单里来回跳就能回答这些问题。

use egui::{RichText, Ui};
use findb::security::Attempt;
use fincore::user::{DataScope, PasswordPolicy, User};
use fincore::Perm;

use crate::state::AppCtx;
use crate::theme::palette;
use crate::widgets;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SecTab {
    Policy,
    Lock,
    Audit,
    Scope,
}

/// 账户锁定 Tab 的行操作
#[derive(Clone, Copy, PartialEq, Eq)]
enum UserOp {
    Unlock,
    Lock15,
    Reset,
    ResetDevice,
}

/// 自建确认弹窗。
///
/// 没有现成的 `ConfirmAction` 能表达"清空登录日志"这类操作（也不该为了界面去改
/// state.rs 的枚举），所以这里用 egui::Window 自己做二次确认。
#[derive(Clone)]
pub struct LocalConfirm {
    title: String,
    msg: String,
}

pub struct SecurityView {
    pub tab: SecTab,
    /// 正在编辑的口令策略（进入界面时从账套参数载入）
    pub policy: PasswordPolicy,
    /// 强度预览用的口令，只留在内存里，绝不落库
    pub probe: String,
    pub users: Vec<User>,
    /// 用户名 → 最近 1 小时连续失败次数
    pub fails: Vec<(String, i64)>,
    pub attempts: Vec<Attempt>,
    /// 登录审计筛选条件
    pub audit_user: String,
    pub only_fail: bool,
    pub limit_text: String,
    pub stats_total: usize,
    pub stats_fail: usize,
    pub stats_fail_1h: i64,
    /// 数据范围 Tab 选中的用户名
    pub scope_user: Option<String>,
    /// 选中用户的数据范围副本（保存时写回 User）
    pub scope: DataScope,
    /// 重置口令弹窗
    pub reset_user: Option<String>,
    pub reset_pwd: String,
    pub reset_err: String,
    /// 自服务修改本人口令弹窗
    pub my_pwd_open: bool,
    pub my_old: String,
    pub my_new: String,
    pub my_confirm: String,
    pub my_err: String,
    pub confirm: Option<LocalConfirm>,
    pub dirty: bool,
    key: String,
}

impl Default for SecurityView {
    fn default() -> Self {
        Self {
            tab: SecTab::Policy,
            policy: PasswordPolicy::default(),
            probe: String::new(),
            users: Vec::new(),
            fails: Vec::new(),
            attempts: Vec::new(),
            audit_user: String::new(),
            only_fail: false,
            limit_text: "500".to_string(),
            stats_total: 0,
            stats_fail: 0,
            stats_fail_1h: 0,
            scope_user: None,
            scope: DataScope::default(),
            reset_user: None,
            reset_pwd: String::new(),
            reset_err: String::new(),
            my_pwd_open: false,
            my_old: String::new(),
            my_new: String::new(),
            my_confirm: String::new(),
            my_err: String::new(),
            confirm: None,
            dirty: true,
            key: String::new(),
        }
    }
}

impl SecurityView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn enter(&mut self, ctx: &mut AppCtx<'_>) {
        // 策略只在进入时载入一次：否则每次 dirty 重载都会把用户正在编辑的值冲掉
        self.policy = findb::security::password_policy(ctx.db());
        self.probe.clear();
        self.dirty = true;
    }

    fn limit(&self) -> i64 {
        self.limit_text.trim().parse::<i64>().unwrap_or(500).max(1)
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        // 缓存 key 按"期间 + 当前 Tab"组：切 Tab 时必定重取，同 Tab 内重复渲染不查库
        let key = format!("{}|{:?}", ctx.period().ymm(), self.tab);
        if !self.dirty && self.key == key {
            return;
        }
        self.key = key;
        self.dirty = false;

        self.users = findb::users::list(ctx.db()).unwrap_or_default();

        self.fails.clear();
        for u in &self.users {
            let n = findb::security::recent_fails(ctx.db(), &u.username, 60).unwrap_or(0);
            self.fails.push((u.username.clone(), n));
            self.stats_fail_1h += n;
        }

        let all = findb::security::attempts(ctx.db(), None, 2000).unwrap_or_default();
        self.stats_total = all.len();
        self.stats_fail = all.iter().filter(|a| !a.ok).count();

        let user = if self.audit_user.trim().is_empty() {
            None
        } else {
            Some(self.audit_user.trim())
        };
        let mut rows = findb::security::attempts(ctx.db(), user, self.limit()).unwrap_or_default();
        if self.only_fail {
            rows.retain(|a| !a.ok);
        }
        self.attempts = rows;
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "安全中心", |ui| {
            ui.label(RichText::new(format!("共 {} 个用户", self.users.len())).weak());
        });

        widgets::toolbar(ui, |ui| {
            ui.selectable_value(&mut self.tab, SecTab::Policy, "口令策略");
            ui.selectable_value(&mut self.tab, SecTab::Lock, "账户锁定");
            ui.selectable_value(&mut self.tab, SecTab::Audit, "登录审计");
            ui.selectable_value(&mut self.tab, SecTab::Scope, "数据范围");
            ui.separator();
            if ui.button("修改我的口令").clicked() {
                self.my_pwd_open = true;
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        match self.tab {
            SecTab::Policy => self.show_policy(ctx, ui),
            SecTab::Lock => self.show_lock(ctx, ui),
            SecTab::Audit => self.show_audit(ctx, ui),
            SecTab::Scope => self.show_scope(ctx, ui),
        }

        self.reset_window(ctx, ui);
        self.confirm_window(ctx, ui);
        self.my_pwd_window(ctx, ui);
    }

    /// 自服务修改当前登录用户的口令（验证旧口令 + 走策略校验）
    fn my_pwd_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        if !self.my_pwd_open {
            return;
        }
        let mut open = true;
        egui::Window::new("修改我的口令")
            .open(&mut open)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.label("请输入当前口令并设置新口令（需满足口令策略）。");
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("当前口令");
                    if ui
                        .add_sized(
                            [200.0, 24.0],
                            egui::TextEdit::singleline(&mut self.my_old).password(true),
                        )
                        .changed()
                    {
                        self.my_err.clear();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("新口令　");
                    if ui
                        .add_sized(
                            [200.0, 24.0],
                            egui::TextEdit::singleline(&mut self.my_new).password(true),
                        )
                        .changed()
                    {
                        self.my_err.clear();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("确认新口令");
                    if ui
                        .add_sized(
                            [200.0, 24.0],
                            egui::TextEdit::singleline(&mut self.my_confirm).password(true),
                        )
                        .changed()
                    {
                        self.my_err.clear();
                    }
                });
                if !self.my_err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.my_err);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("确定修改").clicked() {
                        if self.my_new != self.my_confirm {
                            self.my_err = "两次输入的新口令不一致".to_string();
                        } else {
                            let name = ctx.user().username.clone();
                            let pol = findb::security::password_policy(ctx.db());
                            match findb::security::change_password_checked(
                                ctx.db(),
                                &name,
                                &self.my_old,
                                &self.my_new,
                                &pol,
                            ) {
                                Ok(Ok(())) => {
                                    ctx.info("口令已修改");
                                    ctx.log("安全", "修改口令", &name);
                                    self.my_pwd_open = false;
                                    self.my_old.clear();
                                    self.my_new.clear();
                                    self.my_confirm.clear();
                                    self.my_err.clear();
                                    ctx.st.must_change_pwd = false;
                                }
                                Ok(Err(e)) => self.my_err = e,
                                Err(e) => self.my_err = e.to_string(),
                            }
                        }
                    }
                    if ui.button("取消").clicked() {
                        self.my_pwd_open = false;
                        self.my_old.clear();
                        self.my_new.clear();
                        self.my_confirm.clear();
                        self.my_err.clear();
                    }
                });
            });
        if !open {
            self.my_pwd_open = false;
        }
    }

    // ------------------------- 口令策略 -------------------------
    fn show_policy(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.columns(2, |cols| {
            self.show_policy_form(ctx, &mut cols[0]);
            self.show_strength(&mut cols[1]);
        });
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "默认 8 位 + 字母 + 数字，不强制特殊字符——过严的策略只会让人把口令写在便利贴上。",
            )
            .weak(),
        );
    }

    fn show_policy_form(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        widgets::card(ui, "口令策略", |ui| {
            ui.horizontal(|ui| {
                ui.label("最小长度");
                ui.add(
                    egui::DragValue::new(&mut self.policy.min_len)
                        .range(4..=64)
                        .suffix(" 位"),
                );
            });
            ui.checkbox(&mut self.policy.need_letter, "必须包含字母");
            ui.checkbox(&mut self.policy.need_digit, "必须包含数字");
            ui.checkbox(&mut self.policy.need_symbol, "必须包含特殊字符");

            ui.horizontal(|ui| {
                ui.label("有效期");
                ui.add(
                    egui::DragValue::new(&mut self.policy.max_age_days)
                        .range(0..=730)
                        .suffix(" 天"),
                );
                ui.label(RichText::new("0 = 永不过期").weak());
            });
            ui.horizontal(|ui| {
                ui.label("连续失败");
                ui.add(
                    egui::DragValue::new(&mut self.policy.max_fail)
                        .range(1..=20)
                        .suffix(" 次后锁定"),
                );
                ui.add(
                    egui::DragValue::new(&mut self.policy.lock_minutes)
                        .range(1..=1440)
                        .suffix(" 分钟"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("会话空闲");
                ui.add(
                    egui::DragValue::new(&mut self.policy.idle_minutes)
                        .range(0..=1440)
                        .suffix(" 分钟后自动登出"),
                );
                ui.label(RichText::new("0 = 不自动登出").weak());
            });

            ui.add_space(6.0);
            let mut save = false;
            ui.horizontal(|ui| {
                if ui.button("保存").clicked() && ctx.can(Perm::UserManage) {
                    save = true;
                }
                if ui.button("恢复默认值").clicked() {
                    self.policy = PasswordPolicy::default();
                }
            });
            if save {
                let p = self.policy.clone();
                if ctx
                    .handle(findb::security::set_password_policy(ctx.db(), &p))
                    .is_some()
                {
                    ctx.log(
                        "安全",
                        "修改口令策略",
                        &format!("{}位/{}天/失败{}次锁{}分钟", p.min_len, p.max_age_days, p.max_fail, p.lock_minutes),
                    );
                    ctx.info("口令策略已保存");
                }
            }
        });
    }

    fn show_strength(&mut self, ui: &mut Ui) {
        widgets::card(ui, "实时强度预览", |ui| {
            ui.horizontal(|ui| {
                ui.label("试一个口令");
                ui.add_sized(
                    [200.0, 22.0],
                    egui::TextEdit::singleline(&mut self.probe)
                        .password(true)
                        .hint_text("仅本地测算，不保存"),
                );
            });
            // fincore 的 strength 返回 0~4，界面按 0~100 展示更直观
            let pct = self.policy.strength(&self.probe) as f32 * 25.0;
            let color = if pct < 40.0 {
                palette::CREDIT
            } else if pct < 70.0 {
                palette::WARN
            } else {
                palette::OK
            };
            ui.add(
                egui::ProgressBar::new(pct / 100.0)
                    .fill(color)
                    .text(format!("{pct:.0} / 100")),
            );
            if self.probe.is_empty() {
                ui.label(RichText::new("输入口令后即时显示强度与未满足的规则").weak());
            } else {
                match self.policy.check(&self.probe) {
                    Ok(()) => {
                        ui.colored_label(palette::OK, "✔ 满足当前策略");
                    }
                    Err(e) => {
                        ui.colored_label(palette::CREDIT, format!("✖ {e}"));
                    }
                }
            }
        });
    }

    // ------------------------- 账户锁定 -------------------------
    fn show_lock(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.label(
            RichText::new("连续输错达到策略上限后账户会自动锁定；管理员可提前解锁或手动锁定。")
                .weak(),
        );
        ui.add_space(6.0);

        let rows = self.users.clone();
        let fails = self.fails.clone();
        let cols = [
            widgets::TCol::new("用户名", 120.0).fixed(),
            widgets::TCol::new("姓名", 110.0).fixed(),
            widgets::TCol::new("角色", 100.0).fixed(),
            widgets::TCol::new("状态", 140.0).fixed(),
            widgets::TCol::new("最近失败", 90.0).right(),
            widgets::TCol::new("最近登录", 160.0),
            widgets::TCol::new("绑定设备", 130.0),
            widgets::TCol::new("操作", 290.0).fixed(),
        ];
        // 表格闭包里只能收集动作，真正改库要等闭包结束（否则同时借两次 ctx）
        let mut acts: Vec<(String, UserOp)> = Vec::new();
        widgets::grid(ui, "sec_users", &cols, rows.len(), 26.0, |i, c, ui| {
            let u = &rows[i];
            match c {
                0 => {
                    ui.label(RichText::new(&u.username).monospace());
                }
                1 => {
                    ui.label(&u.display_name);
                }
                2 => {
                    ui.label(u.role_labels());
                }
                3 => {
                    if u.disabled {
                        ui.label(RichText::new("已停用").color(palette::CREDIT));
                    } else if u.is_locked_out() {
                        ui.label(
                            RichText::new(format!("已锁定 {} 分钟", u.lock_remaining_min()))
                                .color(palette::CREDIT),
                        );
                    } else {
                        ui.label(RichText::new("正常").color(palette::OK));
                    }
                }
                4 => {
                    let n = fails
                        .iter()
                        .find(|(n, _)| n == &u.username)
                        .map(|(_, n)| *n)
                        .unwrap_or(0);
                    let t = RichText::new(n.to_string()).strong();
                    ui.label(if n > 0 { t.color(palette::WARN) } else { t });
                }
                5 => {
                    ui.label(RichText::new(u.last_login_at.clone()).weak());
                }
                6 => {
                    if u.is_admin() {
                        ui.label(RichText::new("不限").weak());
                    } else if u.device_id.is_empty() {
                        ui.label(RichText::new("未绑定").weak());
                    } else {
                        ui.label(&u.device_name);
                    }
                }
                7 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("立即解锁").clicked() {
                            acts.push((u.username.clone(), UserOp::Unlock));
                        }
                        if ui.small_button("锁定 15 分钟").clicked() {
                            acts.push((u.username.clone(), UserOp::Lock15));
                        }
                        if ui.small_button("重置口令").clicked() {
                            acts.push((u.username.clone(), UserOp::Reset));
                        }
                        // 管理员不受设备限制，无需重置；只对已绑定的普通账号提供重置
                        if !u.is_admin() && !u.device_id.is_empty()
                            && ui.small_button("重置绑定").clicked()
                        {
                            acts.push((u.username.clone(), UserOp::ResetDevice));
                        }
                    });
                }
                _ => {}
            }
        });

        for (name, op) in acts {
            match op {
                UserOp::Reset => {
                    if ctx.can(Perm::UserManage) {
                        self.reset_user = Some(name);
                        self.reset_pwd.clear();
                        self.reset_err.clear();
                    }
                }
                UserOp::Unlock => {
                    if ctx.can(Perm::UserManage) {
                        let r = findb::security::unlock_user(ctx.db(), &name);
                        if ctx.handle(r).is_some() {
                            ctx.log("安全", "解锁账户", &name);
                            ctx.info(format!("已解锁 {name}"));
                            self.dirty = true;
                        }
                    }
                }
                UserOp::Lock15 => {
                    if ctx.can(Perm::UserManage) {
                        let r = findb::security::lock_user(ctx.db(), &name, 15);
                        if ctx.handle(r).is_some() {
                            ctx.log("安全", "锁定账户", &format!("{name} 15 分钟"));
                            ctx.info(format!("已锁定 {name} 15 分钟"));
                            self.dirty = true;
                        }
                    }
                }
                UserOp::ResetDevice => {
                    if ctx.can(Perm::UserManage) {
                        // 走统一确认弹窗：重置后旧设备立即失效，误操作有挽回提示
                        ctx.confirm(
                            "重置设备绑定",
                            &format!(
                                "确定解除「{name}」的设备绑定吗？重置后该账号可以在任意设备重新登录绑定，原设备将无法继续使用。"
                            ),
                            crate::state::ConfirmAction::ResetDevice(name),
                        );
                    }
                }
            }
        }
    }

    /// 重置口令弹窗：管理员不知道用户的原口令，只能强制其下次登录时修改
    fn reset_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(name) = self.reset_user.clone() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let policy = self.policy.clone();

        egui::Window::new(format!("重置口令 — {name}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label("新口令需满足当前口令策略，重置后该用户下次登录必须修改口令。");
                if !self.reset_err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.reset_err);
                }
                ui.horizontal(|ui| {
                    ui.label("新口令");
                    ui.add_sized(
                        [220.0, 22.0],
                        egui::TextEdit::singleline(&mut self.reset_pwd)
                            .password(true)
                            .hint_text("输入新口令"),
                    );
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui.button("确定重置").clicked() {
                            save = true;
                        }
                    });
                });
            });

        if close || !open {
            self.reset_user = None;
            self.reset_pwd.clear();
            self.reset_err.clear();
            return;
        }
        if !save {
            return;
        }
        let pwd = self.reset_pwd.clone();
        self.reset_err.clear();
        match findb::security::admin_reset_password(ctx.db(), &name, &pwd, &policy) {
            Ok(Ok(())) => {
                ctx.log("安全", "重置口令", &name);
                ctx.info(format!("已重置 {name} 的口令，已强制该用户下次登录时修改口令"));
                self.reset_user = None;
                self.reset_pwd.clear();
                self.dirty = true;
            }
            Ok(Err(msg)) => self.reset_err = msg,
            Err(e) => self.reset_err = e.to_string(),
        }
    }

    // ------------------------- 登录审计 -------------------------
    fn show_audit(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let user_opts: Vec<String> = std::iter::once(String::new())
            .chain(self.users.iter().map(|u| u.username.clone()))
            .collect();
        let limit_opts: Vec<String> = ["100", "500", "2000"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        widgets::toolbar(ui, |ui| {
            ui.label("用户");
            let before = self.audit_user.clone();
            widgets::combo(ui, "sec_audit_user", &mut self.audit_user, &user_opts, 130.0);
            if self.audit_user != before {
                self.dirty = true;
            }
            if ui
                .checkbox(&mut self.only_fail, "只看失败")
                .changed()
            {
                self.dirty = true;
            }
            ui.label("条数");
            let before = self.limit_text.clone();
            widgets::combo(ui, "sec_audit_limit", &mut self.limit_text, &limit_opts, 80.0);
            if self.limit_text != before {
                self.dirty = true;
            }
            ui.separator();
            if ui.button("清空").clicked() && ctx.can(Perm::UserManage) {
                let who = if self.audit_user.trim().is_empty() {
                    "全部用户".to_string()
                } else {
                    self.audit_user.trim().to_string()
                };
                self.confirm = Some(LocalConfirm {
                    title: "清空登录日志".to_string(),
                    msg: format!("确定清空 {who} 的登录尝试记录吗？清空后无法恢复。"),
                });
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            widgets::kv(ui, "总尝试数", &self.stats_total.to_string());
            widgets::kv(ui, "失败数", &self.stats_fail.to_string());
            widgets::kv(ui, "最近 1 小时失败数", &self.stats_fail_1h.to_string());
        });
        ui.label(
            RichText::new("注：登录日志只保留最近 500 条，总尝试数为库内现有记录数。").weak(),
        );
        ui.add_space(6.0);

        let rows = std::mem::take(&mut self.attempts);
        let cols = [
            widgets::TCol::new("时间", 180.0).fixed(),
            widgets::TCol::new("用户名", 160.0).fixed(),
            widgets::TCol::new("结果", 100.0).fixed(),
        ];
        widgets::grid(ui, "sec_attempts", &cols, rows.len(), 24.0, |i, c, ui| {
            let a = &rows[i];
            match c {
                0 => {
                    ui.label(RichText::new(&a.ts).weak());
                }
                1 => {
                    ui.label(RichText::new(&a.username).monospace());
                }
                2 => {
                    if a.ok {
                        ui.label(RichText::new("成功").color(palette::OK));
                    } else {
                        ui.label(RichText::new("失败").color(palette::CREDIT));
                    }
                }
                _ => {}
            }
        });
        self.attempts = rows;
    }

    fn confirm_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(c) = self.confirm.clone() else {
            return;
        };
        let mut open = true;
        let mut close = false;
        let mut yes = false;
        egui::Window::new(c.title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.add_space(6.0);
                ui.label(RichText::new(&c.msg).color(palette::CREDIT));
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                        if ui
                            .add(egui::Button::new(RichText::new("确定").color(egui::Color32::WHITE)).fill(palette::CREDIT))
                            .clicked()
                        {
                            yes = true;
                        }
                    });
                });
            });

        if yes {
            self.confirm = None;
            let user = if self.audit_user.trim().is_empty() {
                None
            } else {
                Some(self.audit_user.trim().to_string())
            };
            let r = findb::security::clear_attempts(ctx.db(), user.as_deref());
            if let Some(n) = ctx.handle(r) {
                ctx.log("安全", "清空登录日志", &format!("{n} 条"));
                ctx.info(format!("已清空 {n} 条登录记录"));
                self.dirty = true;
            }
        } else if close || !open {
            self.confirm = None;
        }
    }

    // ------------------------- 数据范围 -------------------------
    fn show_scope(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        ui.label(
            RichText::new(
                "角色决定「能做什么操作」，数据范围决定「能看到哪些数据」：可以限制某个人只能看 \
                 指定部门、指定科目区间，或者只能看自己做的凭证与单据。",
            )
            .weak(),
        );
        ui.add_space(6.0);

        let users = self.users.clone();
        ui.columns(2, |cols| {
            cols[0].label(RichText::new("用户").strong());
            egui::ScrollArea::vertical()
                .id_salt("sec_scope_users")
                .show(&mut cols[0], |ui| {
                    for u in &users {
                        let sel = self.scope_user.as_deref() == Some(u.username.as_str());
                        let txt = if u.disabled {
                            RichText::new(format!("{}（已停用）", u.display_name)).weak()
                        } else {
                            RichText::new(&u.display_name)
                        };
                        if ui.selectable_label(sel, txt).clicked() {
                            // 切换用户时把库里的值重新灌进编辑区，避免上一个人的配置被误存
                            self.scope_user = Some(u.username.clone());
                            self.scope = u.data_scope.clone();
                        }
                    }
                });

            cols[1].label(RichText::new("可查看范围").strong());
            self.show_scope_editor(ctx, &mut cols[1]);
        });

        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "本版本的数据范围已写入用户档案，并提供 security::visible_dept / visible_account \
                 判断函数，但各账簿界面的实际过滤是逐模块接入的，还未全量生效。",
            )
            .weak(),
        );
    }

    fn show_scope_editor(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(name) = self.scope_user.clone() else {
            widgets::empty_hint(ui, "请从左侧选择一个用户");
            return;
        };

        let mut save = false;
        widgets::card(ui, &format!("数据范围 — {name}"), |ui| {
            ui.label(RichText::new("可查看部门（留空表示不限制）").weak());
            let mut remove: Option<usize> = None;
            for (i, d) in self.scope.depts.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [220.0, 22.0],
                        egui::TextEdit::singleline(d).hint_text("部门编码或名称"),
                    );
                    if ui.small_button("删除").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                self.scope.depts.remove(i);
            }
            if ui.button("+ 添加部门").clicked() {
                self.scope.depts.push(String::new());
            }

            ui.add_space(6.0);
            ui.label(RichText::new("可查看科目区间（留空表示不限，上界含其所有下级）").weak());
            ui.horizontal(|ui| {
                ui.add_sized(
                    [120.0, 22.0],
                    egui::TextEdit::singleline(&mut self.scope.account_from).hint_text("起始科目"),
                );
                ui.label("~");
                ui.add_sized(
                    [120.0, 22.0],
                    egui::TextEdit::singleline(&mut self.scope.account_to).hint_text("截止科目"),
                );
            });

            ui.add_space(6.0);
            ui.checkbox(&mut self.scope.own_voucher_only, "只能查看自己填制的凭证");
            ui.checkbox(&mut self.scope.own_doc_only, "只能查看自己经手的单据");

            ui.add_space(6.0);
            ui.label(if self.scope.is_unrestricted() {
                RichText::new("当前配置：不限制").color(palette::OK)
            } else {
                RichText::new("当前配置：已设限制").color(palette::WARN)
            });
            if ui.button("保存").clicked() && ctx.can(Perm::UserManage) {
                save = true;
            }
        });

        if !save {
            return;
        }
        let Some(base) = self.users.iter().find(|u| u.username == name).cloned() else {
            ctx.error("用户不存在，请刷新后重试");
            return;
        };
        let mut u = base;
        // 编辑区里可能留下空行，落库前去掉，否则"空字符串部门"会被当成一种限制
        // 数据范围 = 权限调整的一种，收归管理员
        if !ctx.user().is_admin() {
            ctx.error("只有管理员可以调整数据范围权限");
            return;
        }
        self.scope.depts.retain(|d| !d.trim().is_empty());
        u.data_scope = self.scope.clone();
        if ctx.handle(findb::users::update(ctx.db(), &u)).is_some() {
            ctx.log("安全", "修改数据范围", &name);
            ctx.info(format!("已保存 {name} 的数据范围"));
            self.dirty = true;
        }
    }
}
