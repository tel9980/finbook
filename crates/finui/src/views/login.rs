//! 登录与账套管理入口

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Align, Layout, RichText, Ui};
use findb::Db;
use fincore::{BookOptions, Period};

use crate::state::AppState;
use crate::theme::palette;

pub struct LoginView {
    pub username: String,
    pub password: String,
    pub users: Vec<String>,

    /// 正在新建账套
    pub creating: bool,
    pub new_path: String,
    pub new_company: String,
    pub new_tax: String,
    pub new_start: String,
    pub new_scheme: String,

    pub err: String,
    /// 已载入用户列表的账套（切换账套后要重新拉用户）
    pub loaded_book: Option<PathBuf>,
}

impl Default for LoginView {
    fn default() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            users: Vec::new(),
            creating: false,
            new_path: String::new(),
            new_company: String::new(),
            new_tax: String::new(),
            new_start: Period::default().code(),
            new_scheme: "4-2-2-2".to_string(),
            err: String::new(),
            loaded_book: None,
        }
    }
}

impl LoginView {
    pub fn show(&mut self, ui: &mut Ui, st: &mut AppState, recent: &mut Vec<PathBuf>) {
        ui.vertical_centered_justified(|ui| {
            ui.add_space(40.0);
            ui.label(RichText::new("FinBook 财务管理系统").size(30.0).strong());
            ui.label(
                RichText::new("账套 — 凭证 — 账簿 — 报表，一条链路跑通")
                    .weak()
                    .size(14.0),
            );
            ui.add_space(24.0);
        });

        ui.columns(3, |cols| {
            cols[1].set_max_width(520.0);
            cols[1].set_min_width(420.0);
            let ui = &mut cols[1];

            if self.creating {
                self.show_create(ui, st, recent);
            } else if st.db.is_none() {
                self.show_book_select(ui, st, recent);
            } else {
                self.show_login(ui, st);
            }
        });
    }

    // --------------------------------------------------------------
    // 选择账套
    // --------------------------------------------------------------
    fn show_book_select(&mut self, ui: &mut Ui, st: &mut AppState, recent: &mut Vec<PathBuf>) {
        egui::Frame::NONE
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, palette::GRID))
            .corner_radius(6.0)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.label(RichText::new("请选择账套").strong().size(16.0));
                ui.add_space(8.0);

                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.err);
                    ui.add_space(6.0);
                }

                if recent.is_empty() {
                    ui.label(RichText::new("暂无最近打开的账套，请新建或打开已有账套。").weak());
                } else {
                    let mut to_open: Option<PathBuf> = None;
                    let mut remove: Option<usize> = None;
                    for (i, p) in recent.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let exists = p.exists();
                            let label = format!("{}  {}", if exists { "📁" } else { "⚠" }, p.display());
                            if ui.button(&label).clicked() && exists {
                                to_open = Some(p.clone());
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("移除").clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                    }
                    if let Some(i) = remove {
                        recent.remove(i);
                    }
                    if let Some(p) = to_open {
                        self.open_book(st, &p, recent);
                    }
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    if ui.button("  新建账套  ").clicked() {
                        self.creating = true;
                        self.err.clear();
                        if self.new_path.is_empty() {
                            if let Some(dir) = default_book_dir() {
                                self.new_path = dir
                                    .join(format!("{}.fbk", chrono::Local::now().format("%Y")))
                                    .to_string_lossy()
                                    .to_string();
                            }
                        }
                    }
                    if ui.button("  打开已有账套  ").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("FinBook 账套", &["fbk"])
                            .pick_file()
                        {
                            self.open_book(st, &p, recent);
                        }
                    }
                });

                ui.add_space(10.0);
                ui.label(
                    RichText::new(
                        "一个账套就是一个 .fbk 文件，备份只需复制该文件。\n\
                         新账套内置管理员账号 admin，默认口令 admin123（首次登录须修改）。",
                    )
                    .weak(),
                );
            });
    }

    // --------------------------------------------------------------
    // 新建账套
    // --------------------------------------------------------------
    fn show_create(&mut self, ui: &mut Ui, st: &mut AppState, recent: &mut Vec<PathBuf>) {
        egui::Frame::NONE
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, palette::GRID))
            .corner_radius(6.0)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.label(RichText::new("新建账套").strong().size(16.0));
                ui.add_space(8.0);

                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.err);
                }

                ui.horizontal(|ui| {
                    ui.label("文件路径");
                    ui.add_sized(
                        [260.0, 22.0],
                        egui::TextEdit::singleline(&mut self.new_path)
                            .hint_text("D:\\财务\\2026.fbk"),
                    );
                    if ui.button("浏览…").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("FinBook 账套", &["fbk"])
                            .set_file_name("2026.fbk")
                            .save_file()
                        {
                            self.new_path = p.to_string_lossy().to_string();
                        }
                    }
                });

                labeled(ui, "企业名称", |ui| {
                    ui.add_sized(
                        [260.0, 22.0],
                        egui::TextEdit::singleline(&mut self.new_company).hint_text("必填"),
                    );
                });
                labeled(ui, "纳税识别号", |ui| {
                    ui.add_sized([260.0, 22.0], egui::TextEdit::singleline(&mut self.new_tax));
                });
                labeled(ui, "启用期间", |ui| {
                    ui.add_sized(
                        [120.0, 22.0],
                        egui::TextEdit::singleline(&mut self.new_start).hint_text("2026-01"),
                    );
                    ui.label(RichText::new("启用期间之前的业务通过「期初建账」录入").weak());
                });
                labeled(ui, "科目级长", |ui| {
                    ui.add_sized(
                        [120.0, 22.0],
                        egui::TextEdit::singleline(&mut self.new_scheme).hint_text("4-2-2-2"),
                    );
                    ui.label(RichText::new("如 4-2-2-2 表示 1001 / 100101 / 10010101").weak());
                });

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let ok = ui.button("  创建并打开  ").clicked();
                    if ok {
                        self.create_book(st, recent);
                    }
                    if ui.button("返回").clicked() {
                        self.creating = false;
                        self.err.clear();
                    }
                });
            });
    }

    // --------------------------------------------------------------
    // 登录
    // --------------------------------------------------------------
    fn show_login(&mut self, ui: &mut Ui, st: &mut AppState) {
        // 切换账套后重新拉用户列表；旧版「首登建号」遗留的空用户账套，补种默认管理员
        if self.loaded_book.as_ref() != st.book_path.as_ref() {
            let db = st.db.as_ref().unwrap();
            if findb::users::count(db).unwrap_or(1) == 0 {
                let mut u = fincore::User::new("admin", "系统管理员", fincore::Role::Admin);
                u.set_password("admin123");
                u.must_change_pwd = true;
                let _ = findb::users::insert(db, &u);
                let _ = db.log(
                    "系统",
                    "安全",
                    "初始化管理员",
                    "账套无任何账号，补种默认管理员 admin（首次登录强制改密）",
                );
            }
            self.users = findb::users::list(db)
                .unwrap_or_default()
                .into_iter()
                .filter(|u| !u.disabled)
                .map(|u| u.username)
                .collect();
            self.loaded_book = st.book_path.clone();
            if self.username.is_empty() {
                self.username = self.users.first().cloned().unwrap_or_default();
            }
        }

        let company = st.db.as_ref().map(|d| d.options().company).unwrap_or_default();
        let path = st
            .book_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        egui::Frame::NONE
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(1.0, palette::GRID))
            .corner_radius(6.0)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.label(RichText::new(if company.is_empty() {
                    "用户登录".to_string()
                } else {
                    company
                })
                .strong()
                .size(18.0));
                ui.label(RichText::new(path).weak().size(12.0));
                ui.add_space(14.0);

                if !self.err.is_empty() {
                    ui.colored_label(palette::CREDIT, &self.err);
                    ui.add_space(6.0);
                }

                labeled(ui, "用户名", |ui| {
                    if self.users.is_empty() {
                        ui.add_sized([200.0, 22.0], egui::TextEdit::singleline(&mut self.username));
                    } else {
                        egui::ComboBox::from_id_salt("login_user")
                            .selected_text(&self.username)
                            .width(200.0)
                            .show_ui(ui, |ui| {
                                for u in &self.users {
                                    ui.selectable_value(&mut self.username, u.clone(), u);
                                }
                            });
                    }
                });

                labeled(ui, "密码", |ui| {
                    let r = ui.add_sized(
                        [200.0, 22.0],
                        egui::TextEdit::singleline(&mut self.password).password(true),
                    );
                    r.request_focus();
                });

                ui.add_space(14.0);
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("  登  录  ").clicked() || enter {
                    self.do_login(st);
                }
                ui.add_space(8.0);
                if ui.small_button("切换账套").clicked() {
                    st.detach();
                    self.password.clear();
                    self.err.clear();
                    self.loaded_book = None;
                }
            });
    }

    // --------------------------------------------------------------
    // 动作
    // --------------------------------------------------------------
    fn open_book(&mut self, st: &mut AppState, path: &PathBuf, recent: &mut Vec<PathBuf>) {
        match Db::open(path) {
            Ok(db) => {
                match st.attach(Arc::new(db), Some(path.clone())) {
                    Ok(()) => {
                        self.err.clear();
                        self.password.clear();
                        push_recent(recent, path.clone());
                    }
                    Err(e) => self.err = e,
                }
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn create_book(&mut self, st: &mut AppState, recent: &mut Vec<PathBuf>) {
        self.err.clear();
        if self.new_path.trim().is_empty() {
            self.err = "请选择账套文件保存位置".to_string();
            return;
        }
        if self.new_company.trim().is_empty() {
            self.err = "请填写企业名称".to_string();
            return;
        }
        let start = match Period::parse(&self.new_start) {
            Ok(p) => p,
            Err(e) => {
                self.err = format!("启用期间格式不正确：{e}");
                return;
            }
        };
        let scheme: Vec<u8> = match self
            .new_scheme
            .split(['-', ' ', ',', '.'])
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().parse::<u8>())
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(v) if !v.is_empty() && v.iter().all(|x| *x > 0) => v,
            _ => {
                self.err = "科目级长格式不正确，应形如 4-2-2-2".to_string();
                return;
            }
        };

        let path = PathBuf::from(self.new_path.trim());
        let opts = BookOptions {
            code_scheme: scheme,
            start_period: start,
            base_currency: "CNY".to_string(),
            company: self.new_company.trim().to_string(),
            tax_no: self.new_tax.trim().to_string(),
            enable_qty: false,
            enable_foreign: false,
            enable_audit: false,
            require_cashier: false,
            biz_accounts: Default::default(),
            budget_control: String::new(),
            doc_prefixes: Default::default(),
            require_audit: false, // 审核环节已移除（未记账 → 记账两态）
            voucher_words: fincore::chart::default_voucher_words(),
        };
        // 内置默认管理员 admin（口令 admin123，首次登录强制改密），与 Web 端口径一致
        match Db::create(&path, &opts) {
            Ok(db) => {
                if let Err(e) = st.attach(Arc::new(db), Some(path.clone())) {
                    self.err = e;
                    return;
                }
                push_recent(recent, path);
                self.creating = false;
                self.password.clear();
            }
            Err(e) => self.err = e.to_string(),
        }
    }

    fn do_login(&mut self, st: &mut AppState) {
        let Some(db) = st.db.clone() else {
            self.err = "尚未选择账套".to_string();
            return;
        };
        // 完整登录流程：失败计数 → 锁定 → 设备绑定 → 口令策略 → 强制改密
        let policy = findb::security::password_policy(&db);
        let device = crate::platform::device_identity();
        match findb::security::login(&db, self.username.trim(), &self.password, &policy, Some(&device)) {
            Ok(findb::security::LoginResult::Ok(u))
            | Ok(findb::security::LoginResult::MustChangePassword(u)) => {
                let name = u.username.clone();
                st.must_change_pwd = u.must_change_pwd
                    || policy.is_expired(&u.pwd_changed_at, chrono::Local::now().date_naive());
                st.session = findb::security::Session::new(policy.idle_minutes);
                st.session.start();
                st.user = Some(u);
                self.err.clear();
                self.password.clear();
                let _ = findb::users::touch_login(&db, &name);
                let _ = db.log(&name, "系统", "登录", &format!("登录成功（设备：{}）", device.name));
            }
            Ok(findb::security::LoginResult::BadPassword { remaining }) => {
                self.err = format!("用户名或密码错误，还剩 {remaining} 次机会");
            }
            Ok(findb::security::LoginResult::Locked { minutes }) => {
                self.err = format!("连续失败次数过多，账户已锁定，请 {minutes} 分钟后再试");
            }
            Ok(findb::security::LoginResult::Disabled) => {
                self.err = "该用户已被停用，请联系管理员".to_string();
            }
            Ok(findb::security::LoginResult::NoSuchUser) => {
                self.err = "用户名或密码错误".to_string();
            }
            Ok(findb::security::LoginResult::DeviceBound { device_name }) => {
                self.err = format!(
                    "该账号已绑定设备「{device_name}」，一台账号只能在一台设备上使用；\
                     如需更换设备，请联系管理员重置设备绑定"
                );
            }
            Err(e) => self.err = e.to_string(),
        }
    }
}

fn labeled<R>(ui: &mut Ui, label: &str, content: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.set_min_width(90.0);
        ui.label(format!("{label}："));
        content(ui)
    })
    .inner
}

fn push_recent(recent: &mut Vec<PathBuf>, p: PathBuf) {
    recent.retain(|x| x != &p);
    recent.insert(0, p);
    recent.truncate(10);
}

fn default_book_dir() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|d| d.document_dir().map(|p| p.to_path_buf()))
}
