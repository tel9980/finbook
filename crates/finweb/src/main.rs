//! FinBook Web 服务端入口
//!
//! 多租户模式：全局账号库（realm.db）+ 多账套目录。
//! - 管理员：管理普通用户账号、查看全部账套（本身不需要建账套）。
//! - 普通用户：登录后自建账套（每个账套独立 `.fbk` 文件，彼此隔离）。
//!
//! 业务代码在 lib 目标（finweb::handlers / finweb::state），本文件只做启动装配。

use std::path::PathBuf;

use axum::Router;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::TraceLayer;

use finweb::handlers;
use finweb::realm::RealmDb;
use finweb::state::{BookRegistry, SessionStore, WebState};
use tracing::{error, info};

/// 初始化日志：默认 info 级（含 HTTP 访问日志），可用 RUST_LOG 调级
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let listen = std::env::var("FINBOOK_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    // 账号库（全局账号 + 账套目录）
    let realm_path =
        std::env::var("FINBOOK_REALM").unwrap_or_else(|_| "./data/realm.db".to_string());
    // 用户自建账套的存放目录
    let books_dir =
        std::env::var("FINBOOK_BOOKS_DIR").unwrap_or_else(|_| "./data/books".to_string());
    let books_dir = PathBuf::from(&books_dir);
    // 数据目录是否必须**事先存在**。
    //
    // 容器部署必须置 FINBOOK_REQUIRE_BOOKS_DIR=true（Dockerfile 已设）：
    // `--data-volume` 挂错路径、或 bind mount 因宿主机目录不存在而没挂上时，
    // `create_dir_all` 会在**容器可写层**里凭空造一个 books 目录。服务照常启动、
    // 照常记账、备份脚本也照常把 .fbk 写进同一层 —— 一切看起来正常，
    // 直到容器重建：账套、备份全没，而运维从头到尾没收到任何信号。
    // 那是静默数据丢失，比启动失败糟得多。
    //
    // 本机/开发场景保留自动创建：首次跑起来不该要求人先 mkdir。
    let require_books_dir = std::env::var("FINBOOK_REQUIRE_BOOKS_DIR")
        .map(|v| v != "0" && v != "false")
        .unwrap_or(false);
    if require_books_dir && !books_dir.is_dir() {
        return Err(format!(
            "账套目录不存在：{}\n\
             这通常是数据卷没挂上（--volume / -v 路径写错，或宿主机目录不存在导致 bind mount 失败）。\n\
             继续启动会在容器可写层里新建一个目录开始记账，容器一重建数据就没了。\n\
             请先创建并挂载该目录；确认确实不需要持久化时，显式设 FINBOOK_REQUIRE_BOOKS_DIR=0。",
            books_dir.display()
        )
        .into());
    }
    std::fs::create_dir_all(&books_dir)?;

    // 兼容旧版单账套环境变量：若设置且目录里有账套文件，则注册为可选账套
    let legacy_dir = std::env::var("FINBOOK_DIR").ok().map(PathBuf::from);

    // 账号库：打开（首次自动建表）并引导管理员
    let realm = RealmDb::open(&realm_path)?;
    let bootstrap = realm.ensure_bootstrap(
        &std::env::var("FINBOOK_ADMIN_USER").unwrap_or_else(|_| "admin".to_string()),
        &std::env::var("FINBOOK_ADMIN_PASS").unwrap_or_default(),
        std::env::var("FINBOOK_ADMIN_MUST_CHANGE")
            .map(|v| v != "0" && v != "false")
            .unwrap_or(false),
        &realm.policy().unwrap_or_default(),
    )?;

    // 账套注册表：启动时把平台账套目录全量载入（key → path）
    let books = BookRegistry::new();
    for p in realm.load_all_book_paths()? {
        if p.exists() {
            books.register(&p, 16);
        }
    }
    // 旧版单账套目录里的文件也一并注册（便于迁移；归属仍以 realm_book 为准）
    if let Some(dir) = &legacy_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "fbk").unwrap_or(false) {
                    books.register(&p, 16);
                }
            }
        }
    }

    let state = WebState::new(
        books,
        SessionStore::new(),
        realm,
        books_dir.clone(),
        env!("CARGO_PKG_VERSION").to_string(),
        default_period(),
        static_dir(),
        asset_version(),
    );

    // 账套归属迁移（账号模型二元化，幂等）：普通账号名下的存量账套 → 管理员名下
    let _ = state.migrate_book_owners_to_admin();

    // 启动时把**所有**账套的 schema 迁到当前版本。
    //
    // 原本迁移是懒执行的（账套第一次被打开时才跑 `schema::init`），后果是：
    // 部署完成、readiness 探针全绿，而用户第一次点「收货」才报
    // `no such column: item_code` —— 故障点与原因隔了几小时，部署的人
    // 不会联想到 schema 版本。生产上真实踩到过。
    //
    // 分两步，顺序不能反：
    //   ① `arm_migration` 同步把状态标成「迁移中」—— 否则从开始监听到后台
    //     任务拉起之间有个窗口，readiness 会看到「默认 = 没在迁移」而**误报就绪**，
    //     正好是要消灭的那种假绿。
    //   ② 真正的迁移放 `spawn_blocking`：迁移是文件 I/O + 写事务，耗时随账套数
    //     线性增长。放进 liveness（`/api/health`）的路径会让迁移期间探针不响应，
    //     编排器据此判 unhealthy 并重启 —— **放大抖动**。
    //     readiness 会如实汇报进度与失败原因。
    state.books.arm_migration();
    {
        let st = state.clone();
        tokio::task::spawn_blocking(move || {
            let total = st.books.list().len();
            if total > 0 {
                info!(books = total, "开始批量迁移账套 schema");
            }
            let r = st.books.migrate_all();
            if r.has_failures() {
                for (k, e) in &r.failed {
                    error!(book = %k, error = %e, "账套 schema 迁移失败");
                }
                error!(migrated = r.ok, total = r.total, "有账套未能迁移到当前版本");
            } else if r.total > 0 {
                info!(migrated = r.ok, total = r.total, "账套 schema 迁移完成");
            }
        });
    }

    // 导出计划任务：每 60s 轮询（到期即写 books_dir/exports/，同日去重）
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                tick.tick().await;
                let now = chrono::Local::now();
                let hhmm = now.format("%H:%M").to_string();
                let today = now.format("%Y-%m-%d").to_string();
                let keys: Vec<String> = st.books.list().into_iter().map(|(k, _)| k).collect();
                for key in keys {
                    if let Ok(db) = st.db_for(&key) {
                        if let Ok(due) = findb::exports::sched_due(&db, &hhmm, &today) {
                            for s in due {
                                if let Err(e) = findb::exports::sched_run(
                                    &db,
                                    s.id,
                                    &st.books_dir.join("exports"),
                                ) {
                                    error!(schedule = s.id, error = %e, "导出计划任务执行失败");
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    let app = build_app(state.clone());

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let book_count = state.books.list().len();
    println!(
        "\n  FinBook Web 已启动（多租户模式）\n  访问地址   : http://{listen}\n  账号库     : {}\n  账套目录   : {}\n  已注册账套 : {book_count} 个\n",
        state.realm.path().display(),
        books_dir.display(),
    );
    if let Some((user, pass)) = bootstrap {
        println!("  ┌─────────────────────────────────────────────┐");
        println!("  │ 已初始化管理员（请立即保存，仅显示一次）│");
        println!("  │   账号：{user:<40}│");
        println!("  │   口令：{pass:<40}│");
        println!("  └─────────────────────────────────────────────┘\n");
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// 监听 SIGTERM / SIGINT，触发优雅关闭
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("注册 Ctrl+C 处理器失败");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("注册 SIGTERM 处理器失败")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    println!("收到关闭信号，等待进行中的请求完成...");
}

/// 默认工作期间：当前月份（ymm）
fn default_period() -> i32 {
    use chrono::Datelike;
    let now = chrono::Local::now();
    now.year() * 100 + now.month() as i32
}

/// 组装应用：API 路由 + 访问日志 + panic 兜底
///
/// 静态资源 fallback 已内聚进 `handlers::router`（spa_fallback）：/api/* 未匹配
/// 的请求必须按登录态回 401/404（M-9），不能落到静态 404 泄露"路径不存在"；
/// handlers::router 先于本函数的 layer 拿到 fallback，访问日志对静态资源同样生效。
fn build_app(state: std::sync::Arc<WebState>) -> Router {
    handlers::router(state)
        .layer(TraceLayer::new_for_http())
        // 最外层兜 panic：即使某条请求路径上的 unwrap 触发，也只返回 500，不拖垮进程
        .layer(CatchPanicLayer::new())
}

/// 定位静态资源目录：优先环境变量，其次可执行文件同级的 static/，最后当前目录的 static/
fn static_dir() -> PathBuf {
    if let Ok(d) = std::env::var("FINWEB_STATIC_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("static");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("static")
}

/// 由前端静态文件的最新修改时间生成资源版本号。
/// 任何 JS/CSS 变更都会使版本号变化 → 首页 `?v=` 随之变化 → 浏览器缓存自动失效，
/// 避免用户长期拿到旧版前端（曾因固定 `?v=20260101` 导致行为与样式不同步）。
fn asset_version() -> String {
    let dir = static_dir();
    let mut latest: u128 = 0;
    // 必须与 index.html 里带 `?v=` 的**每一个**脚本/样式表对应上。
    // 漏一个，那个文件单独改动时版本号不变 → 浏览器按旧 URL 拿到旧的缓存副本
    // （静态资源现在带一年期 `immutable`，漏项不再是「下次回源就对了」而是
    // 「一直错到别的文件也被改」）。`icons.js` 曾经就漏在这份清单外。
    for name in ["app.js", "style.css", "util.js", "icons.js"] {
        if let Ok(meta) = std::fs::metadata(dir.join(name)) {
            if let Ok(m) = meta.modified() {
                if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                    latest = latest.max(d.as_nanos());
                }
            }
        }
    }
    if latest == 0 {
        "dev".to_string()
    } else {
        latest.to_string()
    }
}
