//! # finui —— FinBook 桌面界面层
//!
//! 主窗口由四块组成：顶栏（期间与用户）、左侧功能树、中央工作区、底部状态栏。
//! 所有业务数据都在渲染时从 SQLite 直接取，配合各界面自带的 key 缓存避免重复查询。

pub mod platform;
pub mod state;
pub mod theme;
pub mod views;
pub mod widgets;

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Align, Color32, Layout, RichText, Ui};
use findb::Db;
use fincore::{AuxKind, Period, Perm};

use state::{AppCtx, AppState, ConfirmAction, NavItem};

/// 需要跨会话记住的东西：最近账套、上次登录名、精简模式开关
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct Persist {
    pub recent: Vec<PathBuf>,
    pub last_user: String,
    /// 精简模式：侧栏只显示日常财务要用的菜单，隐藏生产/供应链等重模块。
    ///
    /// 存这里（而不是账套的 BookOptions）是因为它是**这台设备上我的界面偏好**，
    /// 不是账套的属性 —— 存进 .fbk 会跟着备份走，换台电脑打开别人的账套也会被套上。
    ///
    /// 编码用 Option 而非裸 bool：要区分「用户没这个偏好」和「用户明确要关」。
    /// 直接写 `#[serde(default = "…")] pub simple_nav: bool` 做不到 ——
    /// 老版本升上来时字段缺失会落进默认值，而**全新安装**的用户同样落进这个分支，
    /// 于是第一次打开就是 47 项菜单，恰恰是最需要精简的那批人看不到它。
    ///
    /// 所以：`None` = 还没决定，`Some(b)` = 用户明确的开/关。
    /// None 时再看 recent：没打开过任何账套 = 新用户 → 开；
    /// 有历史 = 老用户 → 保持原样（他们的菜单已经用熟，忽然少一半反而不适应）。
    #[serde(default)]
    pub simple_nav: Option<bool>,
}

impl Default for Persist {
    fn default() -> Self {
        Self { recent: Vec::new(), last_user: String::new(), simple_nav: None }
    }
}

/// 实际生效的精简模式取值。
///
/// 一句话规则：**新用户默认精简，老用户保持原样，用户的选择永远优先。**
pub fn effective_simple_nav(p: &Persist) -> bool {
    match p.simple_nav {
        Some(b) => b,
        None => p.recent.is_empty(),
    }
}

/// 启动桌面应用
pub fn run() -> eframe::Result<()> {
    FinBookApp::run()
}

pub struct FinBookApp {
    pub st: AppState,
    pub views: views::Views,
    pub login: views::login::LoginView,
    pub persist: Persist,
}

impl FinBookApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::setup(&cc.egui_ctx);
        let persist: Persist = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        let mut login = views::login::LoginView::default();
        login.username = persist.last_user.clone();
        if let Some(p) = persist.recent.first() {
            login.new_path = p.to_string_lossy().to_string();
        }
        Self {
            st: AppState::default(),
            views: views::Views::default(),
            login,
            persist,
        }
    }

    pub fn run() -> eframe::Result<()> {
        let opts = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1440.0, 900.0])
                .with_min_inner_size([1180.0, 700.0])
                .with_title("FinBook 财务管理系统")
                .with_decorations(true),
            ..Default::default()
        };
        eframe::run_native(
            "FinBook 财务管理系统",
            opts,
            Box::new(|cc| Ok(Box::new(FinBookApp::new(cc)))),
        )
    }

    /// 危险 / 不可逆动作的统一执行入口
    fn run_action(&mut self, act: ConfirmAction) {
        let Self { st, views, .. } = self;
        let mut ctx = AppCtx { st, now: 0.0 };
        match act {
            ConfirmAction::DeleteVoucher(id) => {
                if !ctx.can(Perm::VoucherDelete) {
                    ctx.error(format!("没有「{}」权限", Perm::VoucherDelete.label()));
                    return;
                }
                // 与 web 端及引擎 validate_delete 口径一致：仅草稿可删、已结账期间不可删
                let db = ctx.db();
                let closed = findb::periods::closed_upto(db).unwrap_or(None);
                let guard = match findb::vouchers::get(db, id) {
                    Ok(Some(v)) => fincore::engine::validate_delete(&v, closed),
                    Ok(None) => {
                        ctx.error("凭证不存在");
                        return;
                    }
                    Err(e) => {
                        ctx.error(e.to_string());
                        return;
                    }
                };
                if let Err(e) = guard.into_result() {
                    ctx.error(e.to_string());
                    return;
                }
                let r = findb::vouchers::delete(ctx.db(), id);
                if ctx.handle(r).is_some() {
                    ctx.log("凭证", "删除凭证", &format!("凭证 #{id}"));
                    ctx.info("已删除");
                    views.data_changed(crate::state::DataKind::Voucher);
                    views.voucher_edit.load(&mut ctx, None);
                }
            }
            ConfirmAction::DeleteAccount(code) => {
                if !ctx.can(Perm::AccountEdit) {
                    ctx.error(format!("没有「{}」权限", Perm::AccountEdit.label()));
                    return;
                }
                let r = findb::accounts::delete(ctx.db(), &code);
                if ctx.handle(r).is_some() {
                    ctx.log("科目", "删除科目", &code);
                    ctx.info("已删除科目");
                    ctx.reload_chart();
                    views.data_changed(crate::state::DataKind::Account);
                }
            }
            ConfirmAction::DeleteAux(id) => {
                let r = findb::auxs::delete(ctx.db(), id);
                if ctx.handle(r).is_some() {
                    ctx.log("档案", "删除档案", &format!("#{id}"));
                    ctx.info("已删除");
                    ctx.reload_aux_names();
                    views.data_changed(crate::state::DataKind::Aux);
                }
            }
            ConfirmAction::DeleteBegin(id) => {
                let r = findb::balances::delete_begin(ctx.db(), id);
                if ctx.handle(r).is_some() {
                    ctx.log("期初", "删除期初", &format!("#{id}"));
                    ctx.info("已删除");
                    views.data_changed(crate::state::DataKind::Begin);
                }
            }
            ConfirmAction::DeleteUser(id) => {
                if !ctx.can(Perm::UserManage) {
                    ctx.error(format!("没有「{}」权限", Perm::UserManage.label()));
                    return;
                }
                // 删号 = 调整他人账号，收归管理员
                if !ctx.user().is_admin() {
                    ctx.error("只有管理员可以删除账号");
                    return;
                }
                let r = findb::users::delete(ctx.db(), id);
                if ctx.handle(r).is_some() {
                    ctx.log("用户", "删除用户", &format!("#{id}"));
                    ctx.info("已删除用户");
                    views.data_changed(crate::state::DataKind::User);
                }
            }
            ConfirmAction::CarryForward(p) => {
                views.period_end.run_carry_forward(&mut ctx, p);
                views.data_changed(crate::state::DataKind::Voucher);
            }
            ConfirmAction::ClosePeriod(p) => {
                let who = ctx.user().display_name.clone();
                let require = views.period_end.require_carry;
                let r = findb::periods::close(ctx.db(), p, &who, require);
                match r {
                    Ok(issues) if issues.is_empty() => {
                        ctx.log("期末", "结账", &p.label());
                        ctx.info(format!("{} 已结账", p.label()));
                        views.data_changed(crate::state::DataKind::Voucher);
                        // 结账后自动前进到下一期
                        ctx.set_period(p.next());
                        views.invalidate_all();
                    }
                    Ok(issues) => {
                        for e in issues.iter().take(8) {
                            ctx.error(e.clone());
                        }
                        views.period_end.invalidate();
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
            ConfirmAction::UnclosePeriod(p) => {
                let r = findb::periods::unclose(ctx.db(), p, &ctx.user().username);
                if ctx.handle(r).is_some() {
                    ctx.log("期末", "反结账", &p.label());
                    ctx.info(format!("{} 已反结账", p.label()));
                    views.data_changed(crate::state::DataKind::Voucher);
                    ctx.set_period(p);
                    views.invalidate_all();
                }
            }
            ConfirmAction::ClearVouchers => {
                // 清空全部凭证与期初是高危不可逆操作，不能只靠期末处理入口的
                // CarryForward 权限：要求备份权限（默认仅管理员）。
                if !ctx.can(Perm::Backup) {
                    ctx.error(format!("没有「{}」权限，无法清空业务数据", Perm::Backup.label()));
                    return;
                }
                let r = ctx.db().clear_vouchers();
                if ctx.handle(r).is_some() {
                    ctx.log("账套", "清空业务数据", "全部凭证与期初");
                    ctx.info("已清空全部凭证与期初余额");
                    views.invalidate_all();
                }
            }
            ConfirmAction::RestoreBook(src) => {
                let Some(dst) = ctx.st.book_path.clone() else {
                    ctx.error("当前账套没有文件路径，无法就地恢复");
                    return;
                };
                // 先断开连接，释放文件句柄
                ctx.st.detach();
                match restore_book_atomic(&src, &dst) {
                    Ok(()) => match Db::open(&dst) {
                        Ok(db) => {
                            let r = ctx.st.attach(Arc::new(db), Some(dst));
                            if let Err(e) = r {
                                ctx.error(e);
                            }
                            ctx.info("恢复完成，请重新登录");
                        }
                        Err(e) => ctx.error(format!("恢复后重新打开账套失败：{e}")),
                    },
                    Err(e) => {
                        // 失败时把原账套重新挂回，避免用户停留在"没有账套"的状态
                        if let Ok(db) = Db::open(&dst) {
                            let _ = ctx.st.attach(Arc::new(db), Some(dst));
                        }
                        ctx.error(format!("恢复失败（原账套未受影响）：{e}"));
                    }
                }
            }
            ConfirmAction::AutoFillBegin => match views::begin::auto_fill_begin(&mut ctx) {
                Ok(n) => {
                    ctx.log("期初", "按期末余额倒推期初", &format!("{n} 行"));
                    ctx.info(format!("已重算 {n} 行期初余额"));
                    views.data_changed(crate::state::DataKind::Begin);
                }
                Err(e) => ctx.error(e),
            },
            ConfirmAction::ImportAccounts => {
                let list = fincore::chart::default_accounts();
                let n = list.len();
                let r = findb::accounts::import_many(ctx.db(), &list);
                match r {
                    Ok(c) => {
                        ctx.log("科目", "导入内置科目表", &format!("{c}/{n}"));
                        ctx.info(format!("已导入 {c} 个科目（共 {n} 个）"));
                        ctx.reload_chart();
                        views.data_changed(crate::state::DataKind::Account);
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
            ConfirmAction::ImportCashFlowItems => {
                let r = findb::reports::reset_cash_flow_items(ctx.db());
                if let Some(n) = ctx.handle(r) {
                    ctx.log("报表", "恢复内置现金流量项目", &format!("{n} 个"));
                    ctx.info(format!("已恢复 {n} 个内置现金流量项目"));
                    views.reports.invalidate();
                }
            }
            ConfirmAction::PresetAux(kind) => {
                let label = kind.label().to_string();
                let r = findb::auxs::insert_presets(ctx.db(), kind);
                match r {
                    Ok((n, skipped)) => {
                        if n > 0 {
                            ctx.log(
                                "基础资料",
                                &format!("预置{label}常用档案"),
                                &format!("{n} 条"),
                            );
                        }
                        // 全跳过也要说话 —— 用户点了按钮却什么提示都没有，
                        // 他会以为按钮坏了，去反复点。
                        let mut msg = if n == 0 {
                            format!("{label}档案已经都有对应的了，没有新增。")
                        } else {
                            format!("已预置 {n} 条{label}档案。")
                        };
                        if !skipped.is_empty() {
                            msg.push_str(&format!(
                                "\n已有 {} 条被跳过（编码相同）：{}。你的原档案没动。",
                                skipped.len(),
                                skipped.join("、")
                            ));
                        }
                        if n > 0 {
                            msg.push_str("\n这些都是通用占位，请按你公司的情况改名或补全。");
                        }
                        ctx.info(msg);
                        views.data_changed(crate::state::DataKind::Aux);
                    }
                    Err(e) => ctx.error(e.to_string()),
                }
            }
            ConfirmAction::DeleteAsset(id) => {
                // 资产清理：置状态 + 同事务生成清理转销凭证（历史折旧记录保留）
                let p = ctx.st.period;
                let who = ctx.user().username.clone();
                let r = findb::assets::dispose(ctx.db(), id, p, fincore::Money::ZERO, &who);
                if ctx.handle(r).is_some() {
                    ctx.log("固定资产", "资产清理", &format!("#{id}（已生成清理转销凭证）"));
                    ctx.info("已清理并生成转销凭证草稿（变卖收款与净损益结转请另行制单）");
                    views.data_changed(crate::state::DataKind::Asset);
                }
            }
            ConfirmAction::DeleteDepreciation(period_ymm) => {
                let p = if period_ymm > 0 {
                    // L-2：确认框回传的期间也要校验，非法值不能静默构造成脏 Period 去删数据
                    match fincore::Period::from_ymm_checked(period_ymm as i32) {
                        Ok(p) => p,
                        Err(e) => {
                            ctx.error(e.to_string());
                            return;
                        }
                    }
                } else {
                    ctx.st.period
                };
                let r = findb::assets::dep_delete_period(ctx.db(), p);
                if let Some(n) = ctx.handle(r) {
                    ctx.log("固定资产", "删除折旧记录", &format!("{} {n} 条", p.label()));
                    ctx.info(format!("已删除 {} 期 {n} 条折旧记录", p.label()));
                    views.data_changed(crate::state::DataKind::Asset);
                }
            }
            ConfirmAction::ClearBankStatement(account) => {
                let p = ctx.st.period;
                let r = findb::bank::clear(ctx.db(), p, &account);
                if let Some(n) = ctx.handle(r) {
                    ctx.log("银行对账", "清空对账单", &format!("{account} {n} 条"));
                    ctx.info(format!("已清空 {n} 条对账单记录"));
                    views.data_changed(crate::state::DataKind::Business);
                }
            }
            ConfirmAction::DeletePayroll(id) => {
                let r = findb::business::payroll_delete(ctx.db(), id);
                if ctx.handle(r).is_some() {
                    ctx.log("工资", "删除工资记录", &format!("#{id}"));
                    ctx.info("已删除");
                    views.data_changed(crate::state::DataKind::Business);
                }
            }
            ConfirmAction::DeleteClaim(id) => {
                let r = findb::business::claim_delete(ctx.db(), id);
                if ctx.handle(r).is_some() {
                    ctx.log("报销", "删除报销单", &format!("#{id}"));
                    ctx.info("已删除");
                    views.data_changed(crate::state::DataKind::Business);
                }
            }
            ConfirmAction::DeleteTemplate(id) => {
                let r = findb::template::delete(ctx.db(), id);
                if ctx.handle(r).is_some() {
                    ctx.log("模板", "删除凭证模板", &format!("#{id}"));
                    ctx.info("已删除模板");
                    views.data_changed(crate::state::DataKind::Template);
                }
            }
            ConfirmAction::DeleteCustomReport(key) => {
                let r = findb::mgmt::custom_delete(ctx.db(), &key);
                if ctx.handle(r).is_some() {
                    ctx.log("报表", "删除自定义报表", &key);
                    ctx.info("已删除报表");
                    views.data_changed(crate::state::DataKind::Mgmt);
                }
            }
            ConfirmAction::ClearBudget => {
                let r = ctx
                    .db()
                    .conn()
                    .execute("DELETE FROM budget", []);
                if let Some(_) = ctx.handle(r.map(|n| n).map_err(findb::DbError::from)) {
                    ctx.log("预算", "清空预算", "全部");
                    ctx.info("已清空全部预算");
                    views.data_changed(crate::state::DataKind::Mgmt);
                }
            }
            ConfirmAction::ResetDevice(username) => {
                let r = findb::users::reset_device(ctx.db(), &username);
                if ctx.handle(r).is_some() {
                    ctx.log("安全", "重置设备绑定", &username);
                    ctx.info(format!("已重置「{username}」的设备绑定，该账号可在新设备重新绑定"));
                    views.data_changed(crate::state::DataKind::User);
                }
            }
        }
    }
}

impl eframe::App for FinBookApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        // 落盘时把推断值固化成 Some(b) —— 之后它就是「用户的明确选择」，
        // 不再走新用户/老用户的推断。
        self.persist.simple_nav = Some(effective_simple_nav(&self.persist));
        eframe::set_value(storage, eframe::APP_KEY, &self.persist);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);
        self.st.toasts.retain(now);

        // 精简模式的实际取值（Option → bool 的判定见 effective_simple_nav）。
        // 每帧从 Persist 重算；勾选框改的是这个局部量，save() 时才写回 Persist。
        let mut simple_nav = effective_simple_nav(&self.persist);

        if self.st.want_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.st.want_quit = false;
        }
        if self.st.want_logout {
            self.st.detach();
            self.views.invalidate_all();
            self.login.password.clear();
            self.login.loaded_book = None;
            self.st.want_logout = false;
        }

        // 会话空闲超时：代账会计离开工位忘记锁屏，是单机财务软件最常见的泄露场景
        if self.st.user.is_some() && self.st.session.expired() {
            let who = self.st.user.as_ref().map(|u| u.username.clone()).unwrap_or_default();
            if let Some(db) = self.st.db.clone() {
                let _ = db.log(&who, "系统", "自动登出", "会话空闲超时");
            }
            self.st.detach();
            self.views.invalidate_all();
            self.login.password.clear();
            self.login.loaded_book = None;
            self.login.err = "长时间未操作，已自动退出登录".to_string();
        }

        if self.st.user.is_some() {
            self.st.session.touch();
        }

        if self.st.user.is_none() || self.st.db.is_none() {
            egui::CentralPanel::default().show(ctx, |ui| {
                self.login
                    .show(ui, &mut self.st, &mut self.persist.recent);
            });
            if self.st.user.is_some() && self.st.db.is_some() {
                self.persist.last_user = self.st.user.as_ref().unwrap().username.clone();
                self.views.invalidate_all();
            }
            widgets::toasts(ctx, &mut self.st.toasts, now);
            return;
        }

        // ---------------- 顶栏 ----------------
        egui::TopBottomPanel::top("top_bar")
            .exact_height(46.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new("FinBook").size(19.0).strong().color(theme::palette::PRIMARY));
                    ui.add_space(14.0);
                    if ui.button("填制凭证").clicked() {
                        // 顶栏入口必须与侧栏同一套权限门槛，否则 Viewer/Auditor 可绕过导航限制
                        if self.st.user.as_ref().map(|u| u.can(Perm::VoucherNew)).unwrap_or(false) {
                            self.st.nav = NavItem::VoucherNew;
                        } else {
                            let now = ui.input(|i| i.time);
                            self.st
                                .toasts
                                .push(format!("没有「{}」权限", Perm::VoucherNew.label()), true, now);
                        }
                    }
                    if ui.button("凭证查询").clicked() {
                        self.st.nav = NavItem::VoucherList;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(10.0);
                        if ui.button("退出登录").clicked() {
                            self.st.want_logout = true;
                        }
                        if ui.button("切换账套").clicked() {
                            self.st.want_logout = true;
                        }
                        ui.separator();
                        if let Some(u) = &self.st.user {
                            ui.label(
                                RichText::new(format!("{}（{}）", u.display_name, u.role_labels()))
                                    .weak(),
                            );
                            ui.label(RichText::new("👤").size(16.0));
                        }
                        ui.separator();
                        period_selector(&mut self.st, &mut self.views, ui);
                    });
                });
            });

        // 强制改密横幅：口令过期或管理员标记了 must_change_pwd
        if self.st.must_change_pwd {
            egui::TopBottomPanel::top("force_pwd_bar")
                .frame(egui::Frame::NONE.fill(theme::palette::WARN.gamma_multiply(0.18)))
                .show(ctx, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.colored_label(theme::palette::WARN, "⚠");
                        ui.label(
                            RichText::new("口令已过期或被管理员要求重置，请到「安全中心 → 修改我的口令」尽快修改。")
                                .strong(),
                        );
                    });
                });
        }

        // ---------------- 状态栏 ----------------
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(24.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    if let Some((msg, is_err)) = &self.st.status {
                        ui.colored_label(
                            if *is_err {
                                theme::palette::CREDIT
                            } else {
                                theme::palette::OK
                            },
                            msg,
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(8.0);
                        if let Some(p) = &self.st.book_path {
                            ui.label(RichText::new(p.display().to_string()).weak().size(12.0));
                        }
                        ui.separator();
                        if let Some(db) = &self.st.db {
                            if let Ok(c) = findb::periods::closed_upto(db) {
                                let t = match c {
                                    Some(x) => format!("已结账至 {}", x.label()),
                                    None => "未结账".to_string(),
                                };
                                ui.label(RichText::new(t).weak().size(12.0));
                            }
                            if let Ok((v, e, _)) = db.stats() {
                                ui.label(
                                    RichText::new(format!("凭证 {v} · 分录 {e}"))
                                        .weak()
                                        .size(12.0),
                                );
                            }
                        }
                        ui.separator();
                        let font = theme::FONT_STATUS
                            .lock()
                            .ok()
                            .and_then(|g| g.clone())
                            .unwrap_or_default();
                        if font.is_empty() {
                            ui.colored_label(
                                theme::palette::CREDIT,
                                RichText::new("未找到中文字体").size(12.0),
                            );
                        } else {
                            ui.label(
                                RichText::new(font.split('（').next().unwrap_or("字体"))
                                    .weak()
                                    .size(12.0),
                            );
                        }
                    });
                });
            });

        // ---------------- 左侧功能树 ----------------
        egui::SidePanel::left("nav")
            .exact_width(168.0)
            .resizable(false)
            .show(ctx, |ui| {
                side_bar(&mut self.st, ui, &mut simple_nav);
            });

        // ---------------- 中央工作区 ----------------
        egui::CentralPanel::default().show(ctx, |ui| {
            let t = ui.input(|i| i.time);
            let Self { st, views, .. } = self;
            let mut actx = AppCtx { st, now: t };
            views.show(&mut actx, ui);
        });

        // ---------------- 确认框与提示 ----------------
        if let Some(c) = self.st.confirm.clone() {
            match widgets::confirm_window(ctx, &c) {
                Some(true) => {
                    let act = c.action;
                    self.st.confirm = None;
                    self.run_action(act);
                }
                Some(false) => self.st.confirm = None,
                None => {}
            }
        }
        widgets::toasts(ctx, &mut self.st.toasts, now);
    }
}

// ---------------------------------------------------------------------------
// 顶栏 / 侧栏
// ---------------------------------------------------------------------------

fn period_selector(st: &mut AppState, views: &mut views::Views, ui: &mut Ui) {
    let Some(db) = st.db.clone() else { return };
    let start = db.options().start_period;
    let this_year = Period::default().year();
    let end = Period::new(this_year + 1, 12).unwrap_or(Period::default());
    let list = if end >= start {
        Period::range(start, end)
    } else {
        vec![start]
    };
    let closed = findb::periods::closed_upto(&db).unwrap_or(None);
    let cur = st.period;
    // L-2：哨兵值用命名常量 Period::ZERO（等价于 ymm=0，"从未结账"），不再走未校验构造
    let label = if cur.is_closed(closed.unwrap_or(Period::ZERO)) {
        format!("🔒 {}", cur.label())
    } else {
        cur.label()
    };
    ui.label(RichText::new("会计期间").weak());
    egui::ComboBox::from_id_salt("top_period")
        .selected_text(label)
        .width(150.0)
        .show_ui(ui, |ui| {
            for p in list {
                let locked = closed.map(|c| p.is_closed(c)).unwrap_or(false);
                let t = if locked {
                    format!("🔒 {}", p.label())
                } else {
                    p.label()
                };
                if ui
                    .selectable_label(p == cur, RichText::new(t).color(if locked {
                        ui.visuals().weak_text_color()
                    } else {
                        Color32::BLACK
                    }))
                    .clicked()
                    && p != cur
                {
                    st.period = p;
                    views.invalidate_all();
                }
            }
        });
}

fn side_bar(st: &mut AppState, ui: &mut Ui, simple_nav: &mut bool) {
    // 借出来会和其他 &mut st 字段冲突，所以取局部副本，改完再写回
    let mut show_all_hint = st.show_all_hint;
    egui::Frame::NONE
        .fill(theme::palette::SIDEBAR_BG)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(6.0);
                let mut last_group = String::new();
                for item in NavItem::menu() {
                    // 管理员专属入口：非管理员连分组标题一起隐藏
                    if item.admin_only() && !st.user.as_ref().map(|u| u.is_admin()).unwrap_or(false) {
                        continue;
                    }
                    // 精简模式：只留日常财务够用的菜单，其余（生产制造 / 供应链 / 成本 /
                    // 预算 / 多维损益…）先藏起来。功能一项没少，取消勾选立刻全回来。
                    if *simple_nav && !show_all_hint && !item.daily_core() {
                        continue;
                    }
                    let g = item.group();
                    if g != last_group {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(g)
                                .size(11.5)
                                .color(Color32::from_rgb(140, 152, 168)),
                        );
                        last_group = g.to_string();
                    }
                    let selected = st.nav == *item;
                    let permitted = match item.required_perm() {
                        Some(p) => st.user.as_ref().map(|u| u.can(p)).unwrap_or(false),
                        None => true,
                    };
                    let text = RichText::new(format!("  {}", item.label()))
                        .size(14.0)
                        .color(if selected {
                            Color32::WHITE
                        } else if permitted {
                            theme::palette::SIDEBAR_FG
                        } else {
                            Color32::from_rgb(105, 115, 130)
                        });
                    let btn = egui::Button::new(text)
                        .fill(if selected {
                            theme::palette::SIDEBAR_ACTIVE
                        } else {
                            Color32::TRANSPARENT
                        })
                        .min_size(egui::vec2(ui.available_width() - 8.0, 30.0));
                    if ui.add(btn).clicked() {
                        if permitted {
                            st.nav = *item;
                        } else if let Some(p) = item.required_perm() {
                            let now = ui.input(|i: &egui::InputState| i.time);
                            st.toasts
                                .push(format!("没有「{}」权限", p.label()), true, now);
                        }
                    }
                }

                // ---- 精简模式开关 ----
                // 放在侧栏底部而不是「账套参数」里：这是个高频微调的界面偏好，
                // 藏在深层设置里等于没有。勾上只显示日常财务那 12 项，取消即全回来。
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                let mut simple = *simple_nav;
                if ui
                    .checkbox(&mut simple, "精简模式")
                    .on_hover_text(
                        "只显示日常财务要用的 12 个菜单\n\
                         （工作台/填制凭证/凭证查询/账簿/余额表/报表/期末处理/\n\
                           银行对账/往来核销/科目/期初建账/备份）\n\
                         隐藏生产制造、供应链、成本等模块。功能一项没少，取消勾选即恢复。",
                    )
                    .changed()
                {
                    *simple_nav = simple;
                    if simple && st.nav != NavItem::Dashboard && !st.nav.daily_core() {
                        // 当前停在被藏起来的页面上：那页继续显示，但侧栏没有高亮项，
                        // 用户会以为「界面坏了」。先把他带回工作台。
                        st.nav = NavItem::Dashboard;
                    }
                }

                // 勾上精简模式后，被藏起来的页面在侧栏就没有入口了（顶栏只有
                // 填制凭证/凭证查询两个快捷键），那就等于「功能没了」。
                // 给一个临时的「临时看全部」：只影响这一屏，不动开关本身。
                if *simple_nav {
                    if show_all_hint {
                        if ui.link("← 回到精简模式").clicked() {
                            show_all_hint = false;
                        }
                    } else if ui.link("临时看全部功能…").clicked() {
                        show_all_hint = true;
                    }
                }
                // 回写：勾选框与临时展开都改在同一帧里统一落盘
                st.show_all_hint = show_all_hint;
                ui.add_space(10.0);
            });
        });
}

/// 侧栏未展示的辅助核算类型（供以后扩展模块挂接）
pub fn extra_aux_kinds() -> Vec<AuxKind> {
    vec![AuxKind::CashFlow]
}

/// 原子恢复账套：校验备份 → 复制到同目录临时文件 → 再次校验 →
/// 兜底备份当前账套 → rename 原子替换 → 清理旧 WAL/SHM。
///
/// 任何一步失败都不会动当前账套文件。
fn restore_book_atomic(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    // 健康检查：integrity_check 正常时返回 ["ok"]
    fn check_ok(db: &Db, what: &str) -> Result<(), String> {
        let v = db
            .integrity_check()
            .map_err(|e| format!("{what}完整性检查失败：{e}"))?;
        if v.iter().any(|m| m.trim() != "ok") {
            return Err(format!("{what}完整性检查未通过：{}", v.join("；")));
        }
        Ok(())
    }

    let dir = dst.parent().ok_or_else(|| "账套路径无效".to_string())?;
    let file_name = dst
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("book.fbk");
    let tmp = dir.join(format!("{file_name}.restore-tmp"));
    let bak = dir.join(format!("{file_name}.before-restore"));

    // 复制到同目录临时文件（同分区才能 rename 原子替换）
    std::fs::copy(src, &tmp).map_err(|e| format!("写入临时文件失败：{e}"))?;
    {
        let db = Db::open(&tmp).map_err(|e| format!("备份文件无法打开：{e}"))?;
        check_ok(&db, "备份文件")?;
    }
    // 兜底备份当前账套（用户误选文件时还能手工找回）
    if dst.exists() {
        std::fs::copy(dst, &bak).map_err(|e| format!("备份当前账套失败：{e}"))?;
    }
    // 原子替换
    std::fs::rename(&tmp, dst).map_err(|e| format!("替换账套失败：{e}"))?;
    // 清理旧日志侧车文件，避免旧 WAL 帧污染新库
    for ext in ["-wal", "-shm"] {
        let p = std::path::PathBuf::from(format!("{}{}", dst.display(), ext));
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

#[cfg(test)]
mod onboarding_tests {
    use super::*;

    fn p(simple: Option<bool>, n_recent: usize) -> Persist {
        Persist {
            recent: std::iter::repeat_with(|| std::path::PathBuf::from("/x.fbk")).take(n_recent).collect(),
            last_user: String::new(),
            simple_nav: simple,
        }
    }

    /// 新用户（没打开过任何账套）第一次进来就该是精简模式 ——
    /// 47 项菜单正是「小白看不懂」的根因，不能等他自己去发现那个勾选框。
    #[test]
    fn new_user_gets_simple_nav_by_default() {
        assert!(effective_simple_nav(&p(None, 0)), "新用户首次打开必须默认精简");
    }

    /// 老用户（升级前就有账套）不能突然少一半菜单 ——
    /// 他们的使用习惯是基于完整菜单建立的。
    #[test]
    fn existing_user_keeps_full_nav() {
        assert!(!effective_simple_nav(&p(None, 3)), "老用户升级后不该被突然精简");
    }

    /// 用户的明确选择永远优先，两个方向都要能覆盖默认推断。
    #[test]
    fn explicit_choice_overrides_inference() {
        // 新用户主动关掉 → 保持关
        assert!(!effective_simple_nav(&p(Some(false), 0)), "用户主动关掉后不能被默认值顶回来");
        // 老用户主动打开 → 保持开
        assert!(effective_simple_nav(&p(Some(true), 5)), "用户主动打开后要保持");
    }

    /// 反序列化老版本配置（没有 simple_nav 字段）不能崩，且落到 None。
    /// 这是升级路径：老用户的 eframe 存储里根本没有这个键。
    #[test]
    fn legacy_config_without_field_still_loads() {
        let legacy = r#"{"recent":["/a.fbk"],"last_user":"admin"}"#;
        let got: Persist = serde_json::from_str(legacy).expect("老配置必须能读");
        assert_eq!(got.simple_nav, None, "缺失字段应为 None（未决定）");
        assert!(!effective_simple_nav(&got), "有历史的账号按老用户处理");
    }

    /// round-trip：存了再读，值不变。
    #[test]
    fn persist_roundtrip_keeps_choice() {
        let saved = p(Some(false), 2);
        let text = serde_json::to_string(&saved).unwrap();
        let back: Persist = serde_json::from_str(&text).unwrap();
        assert_eq!(back.simple_nav, Some(false), "往返后不能丢用户的选择");
    }
}
