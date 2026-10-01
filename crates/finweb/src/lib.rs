//! FinBook Web 服务端库
//!
//! 把 Web 端拆成 lib + bin 两个目标：
//! - lib 导出路由、状态、DTO，供集成测试（tests/api.rs）直接引用；
//! - bin（main.rs）只负责启动参数与进程生命周期。

pub mod dto;
pub mod handlers;
pub mod realm;
pub mod pdf;
pub mod report_html;
pub mod state;