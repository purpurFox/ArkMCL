//! 实例分组索引。
//!
//! 保存用户显式创建的分组，避免只从实例字段推导导致空分组丢失。

use std::fmt;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fs::FileSystem;

/// 分组索引错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
    /// JSON 解析错误。
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    groups: Vec<String>,
}

/// 分组存储。
pub struct Store<F: FileSystem> {
    path: PathBuf,
    fs: F,
}

impl<F: FileSystem> Store<F> {
    /// 创建分组存储。
    #[must_use]
    pub fn new(path: PathBuf, fs: F) -> Self {
        Self { path, fs }
    }

    /// 列出全部分组。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn list(&self) -> Result<Vec<String>, Error> {
        if !self.fs.exists(&self.path) {
            return Ok(Vec::new());
        }
        let bytes = self.fs.read(&self.path).map_err(|source| Error::Io {
            path: self.path.clone(),
            source,
        })?;
        let file: File = serde_json::from_slice(&bytes).map_err(|source| Error::Json {
            path: self.path.clone(),
            source,
        })?;
        Ok(file.groups)
    }

    /// 添加分组；已存在时保持幂等。
    ///
    /// # Errors
    ///
    /// 读取或写入失败时返回错误。
    pub fn add(&self, group: &str) -> Result<(), Error> {
        let group = group.trim();
        if group.is_empty() {
            return Ok(());
        }
        let mut groups = self.list()?;
        if groups.iter().any(|existing| existing == group) {
            return Ok(());
        }
        groups.push(group.to_owned());
        groups.sort();
        self.save(&groups)
    }

    /// 移除分组。
    ///
    /// # Errors
    ///
    /// 读取或写入失败时返回错误。
    pub fn remove(&self, group: &str) -> Result<(), Error> {
        let mut groups = self.list()?;
        groups.retain(|existing| existing != group);
        self.save(&groups)
    }

    fn save(&self, groups: &[String]) -> Result<(), Error> {
        let file = File {
            groups: groups.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|source| Error::Json {
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
    use super::Store;
    use crate::fs::LocalFileSystem;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("arkmcl-groups-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn adds_lists_and_removes_groups() {
        let directory = temp_dir("store");
        let store = Store::new(directory.join("groups.json"), LocalFileSystem);
        store.add("Vanilla").expect("add");
        store.add("Modded").expect("add");
        store.add("Vanilla").expect("idempotent");
        assert_eq!(store.list().expect("list"), vec!["Modded", "Vanilla"]);
        store.remove("Vanilla").expect("remove");
        assert_eq!(store.list().expect("list"), vec!["Modded"]);
        let _ = fs::remove_dir_all(&directory);
    }
}
