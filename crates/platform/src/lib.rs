//! 平台适配层。
//!
//! 当前提供 XDG/便携模式路径解析与 Unix domain socket 上的 JSON-RPC 传输。

pub mod ipc;
pub mod paths;
