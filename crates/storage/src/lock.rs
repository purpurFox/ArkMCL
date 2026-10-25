//! 实例锁文件。
//!
//! 锁文件位于 `<instance>/.arkmcl/lock`，内容为 `{"pid":...,"token":"..."}`。
//! 进程退出或 `Drop` 时删除；陈旧锁会在下一次获取或检查时回收。

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::fs::FileSystem;

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    pid: u32,
    token: String,
}

/// 锁错误。
#[derive(Debug)]
pub enum Error {
    /// 实例正在运行。
    Locked,
    /// I/O 错误。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Locked => formatter.write_str("instance is running"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Locked => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

/// 实例锁。
pub struct Guard<F: FileSystem> {
    path: PathBuf,
    token: String,
    fs: F,
}

impl<F: FileSystem> Guard<F> {
    /// 获取实例锁。
    ///
    /// # Errors
    ///
    /// 实例已在运行或锁文件无法读写时返回错误。
    pub fn acquire(fs: F, instance_root: &Path) -> Result<Self, Error> {
        let meta = instance_root.join(".arkmcl");
        fs.create_dir_all(&meta).map_err(|source| Error::Io {
            path: meta.clone(),
            source,
        })?;
        let path = meta.join("lock");
        let token = token();
        let record = Record {
            pid: std::process::id(),
            token: token.clone(),
        };
        let payload = serde_json::to_vec(&record).map_err(|source| Error::Io {
            path: path.clone(),
            source: io::Error::other(source),
        })?;

        match fs.create_new(&path, &payload) {
            Ok(()) => Ok(Self { path, token, fs }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if is_stale(&fs, &path) {
                    let _ = fs.remove_file(&path);
                    match fs.create_new(&path, &payload) {
                        Ok(()) => Ok(Self { path, token, fs }),
                        Err(source) => Err(Error::Io { path, source }),
                    }
                } else {
                    Err(Error::Locked)
                }
            }
            Err(source) => Err(Error::Io { path, source }),
        }
    }

    /// 检查实例是否被锁定；陈旧锁会被清理。
    ///
    /// # Errors
    ///
    /// 实例正在运行或锁文件无法读取时返回错误。
    pub fn check(fs: &F, instance_root: &Path) -> Result<(), Error> {
        let path = instance_root.join(".arkmcl").join("lock");
        if !fs.exists(&path) {
            return Ok(());
        }
        if is_stale(fs, &path) {
            let _ = fs.remove_file(&path);
            Ok(())
        } else {
            Err(Error::Locked)
        }
    }

    /// 锁文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<F: FileSystem> Drop for Guard<F> {
    fn drop(&mut self) {
        if let Ok(bytes) = self.fs.read(&self.path) {
            if let Ok(record) = serde_json::from_slice::<Record>(&bytes) {
                if record.token == self.token {
                    let _ = self.fs.remove_file(&self.path);
                }
            }
        }
    }
}

fn is_stale<F: FileSystem>(fs: &F, path: &Path) -> bool {
    let Ok(bytes) = fs.read(path) else {
        return true;
    };
    let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
        return true;
    };
    !process_alive(record.pid)
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    true
}

fn token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{}:{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::{Error, Guard};
    use crate::fs::LocalFileSystem;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("arkmcl-lock-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn locks_and_releases() {
        let directory = temp_dir("basic");
        let guard = Guard::acquire(LocalFileSystem, &directory).expect("lock");
        assert!(directory.join(".arkmcl/lock").exists());
        assert!(matches!(
            Guard::acquire(LocalFileSystem, &directory),
            Err(Error::Locked)
        ));
        drop(guard);
        assert!(!directory.join(".arkmcl/lock").exists());
        let again = Guard::acquire(LocalFileSystem, &directory).expect("relock");
        drop(again);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn reclaims_stale_lock() {
        let directory = temp_dir("stale");
        let meta = directory.join(".arkmcl");
        fs::create_dir_all(&meta).expect("meta");
        fs::write(
            meta.join("lock"),
            json!({"pid": 999_999, "token": "stale"}).to_string(),
        )
        .expect("stale lock");
        let guard = Guard::acquire(LocalFileSystem, &directory).expect("reclaim");
        assert!(guard.path().exists());
        drop(guard);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn check_reports_live_lock() {
        let directory = temp_dir("check");
        let guard = Guard::acquire(LocalFileSystem, &directory).expect("lock");
        assert!(matches!(
            Guard::check(&LocalFileSystem, &directory),
            Err(Error::Locked)
        ));
        drop(guard);
        assert!(Guard::check(&LocalFileSystem, &directory).is_ok());
        let _ = fs::remove_dir_all(&directory);
    }
}
