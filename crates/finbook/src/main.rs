//! FinBook 财务管理系统 —— 桌面版入口
//!
//! 启动约定：
//! - 不带参数：进入图形界面，从登录页选择或新建账套
//! - 带账套路径：构造时直接 attach 该账套（再走用户登录页）

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Warn)
        .try_init();

    let arg = std::env::args().nth(1).unwrap_or_default();
    let initial_book = if arg.is_empty() {
        None
    } else {
        let p = PathBuf::from(&arg);
        if p.exists() { Some(p) } else { None }
    };

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
        Box::new(move |cc| {
            // 主题
            finui::theme::setup(&cc.egui_ctx);
            // 读 Persist
            let mut persist: finui::Persist = cc
                .storage
                .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
                .unwrap_or_default();
            // 命令行给的路径如果不在 recent, 插到最前 (这样 LoginView 会自动 attach)
            if let Some(p) = &initial_book {
                persist.recent.retain(|x| x != p);
                persist.recent.insert(0, p.clone());
                persist.recent.truncate(10);
            }
            let mut app = finui::FinBookApp {
                st: finui::state::AppState::default(),
                views: finui::views::Views::default(),
                login: finui::views::login::LoginView::default(),
                persist: persist.clone(),
            };
            // 默认用户名
            if let Some(p) = persist.recent.first() {
                app.login.new_path = p.to_string_lossy().to_string();
            }
            if !app.persist.last_user.is_empty() {
                app.login.username = app.persist.last_user.clone();
            }
            // 如果命令行带了账套, 构造时直接 attach
            if let Some(p) = initial_book.clone() {
                if let Ok(db) = findb::Db::open(&p) {
                    let st = &mut app.st;
                    let _ = st.attach(Arc::new(db), Some(p));
                    app.login.loaded_book = None; // 让 show_login 重新拉用户
                }
            }
            app.persist = persist;
            Ok(Box::new(app))
        }),
    )
}
