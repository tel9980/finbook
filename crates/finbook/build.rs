//! 构建脚本：Windows 目标下强制使用 GUI 子系统，
//! 这样双击 `finbook.exe` 运行时不会弹出黑色控制台窗口。
//!
//! `.cargo/config.toml` 里也配了 `-mwindows`，但 config 里的 target rustflags
//! 可能被全局配置覆盖；build script 产出的 `cargo:rustc-link-arg` 一定会被追加，
//! 作为兜底保证桌面双击体验。

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=-mwindows");
    }
}
