//! `ArkMCL` 守护进程。
//!
//! 通过 Unix domain socket 暴露 JSON-RPC 2.0，当前提供：
//! `server.info`、`server.shutdown`，以及实例、启动和账户接口。

#[cfg(unix)]
mod app;

#[cfg(unix)]
pub use app::{Error, main_entry, run};
