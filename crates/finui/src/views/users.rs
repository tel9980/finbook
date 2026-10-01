//! 用户与权限

use egui::{Color32, RichText, Ui};
use fincore::{Perm, Role, User};

use crate::state::{AppCtx, ConfirmAction};
use crate::theme::palette;
use crate::widgets;

pub struct UsersView {
    pub rows: Vec<User>,
    pub dirty: bool,
    pub editing: Option<User>,
    pub editing_new: bool,
    pub pwd: String,
    /// 编辑窗口中的「部门范围」临时文本（逗号分隔）
    pub depts_text: String,
    pub err: String,
    /// 查看某角色的权限矩阵
    pub show_perms: Option<Role>,
}

impl Default for UsersView {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            dirty: true,
            editing: None,
            editing_new: false,
            pwd: String::new(),
            depts_text: String::new(),
            err: String::new(),
            show_perms: None,
        }
    }
}

impl UsersView {
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn reload(&mut self, ctx: &mut AppCtx<'_>) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.rows = findb::users::list(ctx.db()).unwrap_or_default();
    }

    /// 最近登录时间只显示日期部分，方便表格对齐
    fn short_date(s: &str) -> &str {
        s.get(..10).unwrap_or(s)
    }

    pub fn show(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        self.reload(ctx);

        widgets::page_header(ui, "用户与权限", |ui| {
            ui.label(RichText::new(format!("共 {} 个用户", self.rows.len())).weak());
        });

        widgets::toolbar(ui, |ui| {
            if ui.button("新增用户").clicked() {
                self.editing = Some(User::new("", "", Role::Accountant));
                self.editing_new = true;
                self.pwd.clear();
                self.depts_text.clear();
                self.err.clear();
            }
            if ui.button("刷新").clicked() {
                self.dirty = true;
            }
            ui.separator();
            ui.label("角色权限：");
            for r in Role::all() {
                if ui.button(r.label()).clicked() {
                    self.show_perms = Some(*r);
                }
            }
        });

        let rows = self.rows.clone();
        let cols = [
            widgets::TCol::new("用户名", 110.0).fixed(),
            widgets::TCol::new("姓名", 90.0).fixed(),
            widgets::TCol::new("角色", 90.0).fixed(),
            widgets::TCol::new("最近登录", 90.0).fixed(),
            widgets::TCol::new("锁定", 50.0).fixed(),
            widgets::TCol::new("强制改密", 70.0).fixed(),
            widgets::TCol::new("状态", 50.0).fixed(),
            widgets::TCol::new("备注", 180.0),
            widgets::TCol::new("操作", 150.0).fixed(),
        ];
        let me = ctx.user().username.clone();
        let mut del: Option<i64> = None;
        let mut unlock: Option<String> = None;
        widgets::grid(ui, "user_list", &cols, rows.len(), 24.0, |i, c, ui| {
            let u = &rows[i];
            match c {
                0 => {
                    let mut t = RichText::new(&u.username).monospace();
                    if u.username == me {
                        t = t.strong();
                    }
                    ui.label(t);
                }
                1 => { ui.label(&u.display_name); }
                2 => { ui.label(u.role_labels()); }
                3 => {
                    if u.last_login_at.is_empty() {
                        ui.label(RichText::new("从未登录").weak());
                    } else {
                        ui.label(RichText::new(Self::short_date(&u.last_login_at)).weak());
                    }
                }
                4 => {
                    if u.locked_until.is_some() {
                        ui.label(RichText::new("已锁定").color(palette::CREDIT));
                    } else {
                        ui.label(RichText::new("—").weak());
                    }
                }
                5 => {
                    if u.must_change_pwd {
                        ui.label(RichText::new("是").color(palette::WARN));
                    } else {
                        ui.label(RichText::new("否").weak());
                    }
                }
                6 => {
                    ui.label(if u.disabled {
                        RichText::new("停用").color(palette::CREDIT)
                    } else {
                        RichText::new("启用").color(palette::OK)
                    });
                }
                7 => { ui.label(RichText::new(&u.memo).weak()); }
                8 => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ui.small_button("改").clicked() {
                            self.editing = Some(u.clone());
                            self.editing_new = false;
                            self.pwd.clear();
                            self.depts_text = u.data_scope.depts.join(",");
                            self.err.clear();
                        }
                        if u.locked_until.is_some() && ui.small_button("解锁").clicked() {
                            unlock = Some(u.username.clone());
                        }
                        if ui.small_button("删").clicked() {
                            del = Some(u.id);
                        }
                    });
                }
                _ => {}
            }
        });
        if let Some(id) = del {
            ctx.confirm_dangerous(
                "删除用户",
                "删除后该用户无法登录，其历史操作日志保留。确定删除吗？",
                ConfirmAction::DeleteUser(id),
                true,
            );
        }
        if let Some(username) = unlock {
            match findb::security::unlock_user(ctx.db(), &username) {
                Ok(()) => {
                    ctx.log("用户", "解锁用户", &username);
                    ctx.info("已解锁");
                    self.dirty = true;
                }
                Err(e) => ctx.error(e.to_string()),
            }
        }

        self.edit_window(ctx, ui);
        self.perm_window(ui);
    }

    fn edit_window(&mut self, ctx: &mut AppCtx<'_>, ui: &mut Ui) {
        let Some(u) = self.editing.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut close = false;
        let is_new = self.editing_new;

        egui::Window::new(if is_new { "新增用户" } else { "修改用户" })
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.err);
                }
                egui::Grid::new("user_edit")
                    .num_columns(2)
                    .spacing([10.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("用户名：");
                        if is_new {
                            ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut u.username));
                        } else {
                            // 用户名一旦创建不可改（后端按 username 关联），只读展示避免误导
                            ui.label(RichText::new(&u.username).monospace().weak());
                        }
                        ui.end_row();
                        ui.label("姓名：");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut u.display_name));
                        ui.end_row();
                        ui.label("角色：");
                        egui::ComboBox::from_id_salt("user_role")
                            .selected_text(u.role_labels())
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for r in Role::all() {
                                    ui.selectable_value(&mut u.role, *r, r.label());
                                }
                            });
                        ui.end_row();
                        ui.label("兼任岗位：");
                        ui.horizontal_wrapped(|ui| {
                            for r in Role::all() {
                                if *r == u.role {
                                    continue;
                                }
                                let mut on = u.roles.contains(r);
                                if ui.checkbox(&mut on, r.label()).changed() {
                                    if on {
                                        if !u.roles.contains(r) {
                                            u.roles.push(*r);
                                        }
                                    } else {
                                        u.roles.retain(|x| x != r);
                                    }
                                }
                            }
                        });
                        ui.end_row();
                        ui.label(if is_new { "初始密码：" } else { "重置密码：" });
                        ui.add_sized(
                            [220.0, 22.0],
                            egui::TextEdit::singleline(&mut self.pwd)
                                .password(true)
                                .hint_text(if is_new { "至少 6 位（必填）" } else { "留空表示不修改" }),
                        );
                        ui.end_row();
                        ui.label("备注：");
                        ui.add_sized([220.0, 22.0], egui::TextEdit::singleline(&mut u.memo));
                        ui.end_row();
                        ui.label("部门范围：");
                        ui.add_sized(
                            [220.0, 22.0],
                            egui::TextEdit::singleline(&mut self.depts_text)
                                .hint_text("逗号分隔，留空不限"),
                        );
                        ui.end_row();
                        ui.label("科目范围：");
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [100.0, 22.0],
                                egui::TextEdit::singleline(&mut u.data_scope.account_from)
                                    .hint_text("起"),
                            );
                            ui.label("至");
                            ui.add_sized(
                                [100.0, 22.0],
                                egui::TextEdit::singleline(&mut u.data_scope.account_to)
                                    .hint_text("止"),
                            );
                        });
                        ui.end_row();
                    });
                ui.add_space(6.0);
                ui.label(RichText::new("数据范围").weak());
                ui.checkbox(&mut u.data_scope.own_voucher_only, "仅看本人填制的凭证");
                ui.checkbox(&mut u.data_scope.own_doc_only, "仅看本人经手的业务单据");
                ui.add_space(6.0);
                ui.checkbox(&mut u.disabled, "停用该用户");
                ui.checkbox(&mut u.must_change_pwd, "下次登录强制改密");
                ui.add_space(8.0);

                // 权限逐项覆盖：角色预设基础上，每一项可改为「强制开启 / 强制关闭」
                if u.is_admin() {
                    ui.label(RichText::new("系统管理员默认拥有全部权限，不受逐项开关限制。").weak());
                } else {
                    ui.label(RichText::new("权限覆盖（角色基础上逐项开关）").weak());
                    ui.add_space(4.0);
                    // 主岗位 + 兼任岗位权限并集（身兼多职）
                    let mut role_perms: Vec<Perm> = u.role.perms().to_vec();
                    for rc in &u.roles {
                        for p in rc.perms() {
                            if !role_perms.contains(p) {
                                role_perms.push(*p);
                            }
                        }
                    }
                    let extra = u.extra_perms.clone();
                    let deny = u.deny_perms.clone();
                    // -1=强制关闭 0=跟随角色 1=强制开启
                    let mut changed: Option<(Perm, i8)> = None;
                    egui::ScrollArea::vertical()
                        .id_salt("user_perm_overrides")
                        .max_height(180.0)
                        .show(ui, |ui| {
                            egui::Grid::new("perm_overrides")
                                .num_columns(2)
                                .spacing([10.0, 6.0])
                                .show(ui, |ui| {
                                    for p in Perm::all() {
                                        let base = role_perms.contains(p);
                                        let force_on = extra.contains(p);
                                        let force_off = deny.contains(p);
                                        let cur = if force_off {
                                            "强制关闭".to_string()
                                        } else if force_on {
                                            "强制开启".to_string()
                                        } else {
                                            format!("跟随角色{}", if base { " ✔" } else { " ✖" })
                                        };
                                        ui.label(format!("{}：", p.label()));
                                        egui::ComboBox::from_id_salt(("perm_ovr", p))
                                            .selected_text(cur)
                                            .width(170.0)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_label(!force_off && !force_on, "跟随角色")
                                                    .clicked()
                                                    .then(|| changed = Some((*p, 0)));
                                                ui.selectable_label(force_on, "强制开启")
                                                    .clicked()
                                                    .then(|| changed = Some((*p, 1)));
                                                ui.selectable_label(force_off, "强制关闭")
                                                    .clicked()
                                                    .then(|| changed = Some((*p, -1)));
                                            });
                                        ui.end_row();
                                    }
                                });
                        });
                    // 应用选择：跟随角色 → 清空覆盖；强制开启 → 加 extra；强制关闭 → 加 deny
                    if let Some((p, mode)) = changed {
                        u.extra_perms.retain(|x| *x != p);
                        u.deny_perms.retain(|x| *x != p);
                        match mode {
                            1 => u.extra_perms.push(p),
                            -1 => u.deny_perms.push(p),
                            _ => {}
                        }
                    }
                    ui.add_space(6.0);
                }

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
            self.err.clear();
            return;
        }
        if save {
            let mut u = self.editing.clone().unwrap();
            self.err.clear();
            // 权限调整收归管理员：非管理员即使能看到本页，也不能建号或改权限
            if !ctx.user().is_admin() {
                self.err = "只有管理员可以创建账号或调整权限".to_string();
                return;
            }
            if u.username.trim().is_empty() {
                self.err = "用户名不能为空".to_string();
                return;
            }
            // 部门范围：从逗号分隔文本解析回数组
            u.data_scope.depts = self
                .depts_text
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if is_new {
                let pwd = self.pwd.trim().to_string();
                if pwd.len() < 6 {
                    self.err = "初始密码至少 6 位（必填）".to_string();
                    return;
                }
                u.set_password(&pwd);
                // 管理员开的号应强制首次登录改密（与 Web 一致）
                u.must_change_pwd = true;
                // 凭证可见性默认放开（多岗位协作）；按账号收紧在数据范围勾选
                match findb::users::insert(ctx.db(), &u) {
                    Ok(_) => {
                        ctx.log("用户", "新增用户", &u.username);
                        ctx.info("已新增用户");
                        self.dirty = true;
                        self.editing = None;
                    }
                    Err(e) => self.err = e.to_string(),
                }
            } else {
                if !self.pwd.trim().is_empty() {
                    let pwd = self.pwd.trim().to_string();
                    if pwd.len() < 6 {
                        self.err = "重置密码至少 6 位".to_string();
                        return;
                    }
                    // 直接在此落地：设新口令 + 强制改密，再统一 update，避免双重写库
                    u.set_password(&pwd);
                    u.must_change_pwd = true;
                }
                match findb::users::update(ctx.db(), &u) {
                    Ok(()) => {
                        ctx.log("用户", "修改用户", &u.username);
                        ctx.info("已保存");
                        self.dirty = true;
                        self.editing = None;
                    }
                    Err(e) => self.err = e.to_string(),
                }
            }
        }
    }

    fn perm_window(&mut self, ui: &mut Ui) {
        let Some(r) = self.show_perms else { return };
        let mut open = true;
        egui::Window::new(format!("角色权限 — {}", r.label()))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                let owned: Vec<Perm> = r.perms().to_vec();
                ui.horizontal_wrapped(|ui| {
                    for p in Perm::all() {
                        let on = owned.contains(p);
                        let txt = if on {
                            format!("✔ {}", p.label())
                        } else {
                            format!("✖ {}", p.label())
                        };
                        ui.label(
                            RichText::new(txt).color(if on {
                                Color32::BLACK
                            } else {
                                ui.visuals().weak_text_color()
                            }),
                        );
                        ui.add_space(6.0);
                    }
                });
            });
        if !open {
            self.show_perms = None;
        }
    }
}
