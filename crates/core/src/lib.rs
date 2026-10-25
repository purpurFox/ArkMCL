//! `ArkMCL` 核心领域逻辑。
//!
//! 本 crate 必须保持平台无关：不依赖 CLI、GUI、守护进程或任何具体通信方式，
//! 供服务端、CLI 与 GUI 共同复用。

pub mod launch;
pub mod meta;
pub mod model;
pub mod ports;

/// 应用名称，用于日志、CLI 输出与 UI 展示。
pub const APP_NAME: &str = "ArkMCL";

/// 核心库版本，来自 Cargo 包版本。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 运行环境的结构化描述。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeInfo {
    /// 应用名称。
    pub name: &'static str,
    /// 语义化版本号。
    pub version: &'static str,
}

impl RuntimeInfo {
    /// 返回当前构建的应用信息。
    #[must_use]
    pub const fn current() -> Self {
        Self {
            name: APP_NAME,
            version: VERSION,
        }
    }

    /// 以固定字段顺序输出 JSON，便于脚本与 GUI 消费。
    ///
    /// 当前字段均为编译期常量，不含需要转义的字符；一旦允许外部输入，
    /// 应改用序列化库而不是手工拼接。
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(r#"{{"name":"{}","version":"{}"}}"#, self.name, self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::{APP_NAME, RuntimeInfo, VERSION};

    #[test]
    fn current_reports_app_identity() {
        let info = RuntimeInfo::current();
        assert_eq!(info.name, APP_NAME);
        assert_eq!(info.version, VERSION);
    }

    #[test]
    fn json_contains_name_and_version() {
        let json = RuntimeInfo::current().to_json();
        assert_eq!(
            json,
            format!(r#"{{"name":"{APP_NAME}","version":"{VERSION}"}}"#)
        );
    }
}
