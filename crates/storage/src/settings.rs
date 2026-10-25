//! 全局默认项。

use std::fmt;
use std::io;
use std::path::PathBuf;

use arkmcl_core::model::DEFAULT_JVM_ARGS;
use serde::{Deserialize, Serialize};

use crate::fs::FileSystem;

/// 实例默认项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceDefaults {
    /// 默认 Minecraft 版本；为空时使用最新稳定版。
    #[serde(default)]
    pub version: Option<String>,
    /// 默认内存（MiB）。
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    /// 默认 JVM 参数。
    #[serde(default = "default_jvm_args")]
    pub jvm_args: Vec<String>,
    /// 默认分组；为空时使用 `<loader>`。
    #[serde(default)]
    pub group: Option<String>,
    /// 创建时的默认链接策略：`all` 或 `none`。
    #[serde(default = "default_linking")]
    pub linking: String,
}

impl Default for InstanceDefaults {
    fn default() -> Self {
        Self {
            version: None,
            memory_mb: default_memory(),
            jvm_args: default_jvm_args(),
            group: None,
            linking: default_linking(),
        }
    }
}

fn default_memory() -> u32 {
    4096
}

fn default_linking() -> String {
    "all".to_owned()
}

fn default_jvm_args() -> Vec<String> {
    DEFAULT_JVM_ARGS
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect()
}

/// 全局设置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// 默认实例。
    #[serde(default)]
    pub default_instance: Option<String>,
    /// 默认账户。
    #[serde(default)]
    pub default_account: Option<String>,
    /// 实例默认项。
    #[serde(default)]
    pub instance: InstanceDefaults,
}

/// 设置错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
    /// JSON 错误。
    Json {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: serde_json::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Json { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
        }
    }
}

/// 设置存储。
pub struct Store<F: FileSystem> {
    path: PathBuf,
    fs: F,
}

impl<F: FileSystem> Store<F> {
    /// 创建设置存储。
    #[must_use]
    pub fn new(path: PathBuf, fs: F) -> Self {
        Self { path, fs }
    }

    /// 读取设置；文件缺失时返回默认值。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn load(&self) -> Result<Settings, Error> {
        if !self.fs.exists(&self.path) {
            return Ok(Settings::default());
        }
        let bytes = self.fs.read(&self.path).map_err(|source| Error::Io {
            path: self.path.clone(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| Error::Json {
            path: self.path.clone(),
            source,
        })
    }

    /// 保存设置。
    ///
    /// # Errors
    ///
    /// 写入失败时返回错误。
    pub fn save(&self, settings: &Settings) -> Result<(), Error> {
        let bytes = serde_json::to_vec_pretty(settings).map_err(|source| Error::Json {
            path: self.path.clone(),
            source,
        })?;
        self.fs
            .write(&self.path, &bytes)
            .map_err(|source| Error::Io {
                path: self.path.clone(),
                source,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Settings, Store};
    use crate::fs::LocalFileSystem;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("arkmcl-settings-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn missing_file_returns_defaults() {
        let directory = temp_dir("missing");
        let store = Store::new(directory.join("settings.json"), LocalFileSystem);
        assert_eq!(store.load().expect("load"), Settings::default());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn round_trips_defaults() {
        let directory = temp_dir("roundtrip");
        let store = Store::new(directory.join("settings.json"), LocalFileSystem);
        let settings = Settings {
            default_instance: Some("test".to_owned()),
            default_account: Some("uuid".to_owned()),
            instance: super::InstanceDefaults::default(),
        };
        store.save(&settings).expect("save");
        assert_eq!(store.load().expect("load"), settings);
        let _ = fs::remove_dir_all(&directory);
    }
}
