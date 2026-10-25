//! 文件系统端口与本地实现。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 文件系统端口。
pub trait FileSystem: Send + Sync {
    /// 读取整个文件。
    ///
    /// # Errors
    ///
    /// 文件不存在或无法读取时返回错误。
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// 原子写入：临时文件 + fsync + rename。
    ///
    /// # Errors
    ///
    /// 目录无法创建、写入失败或 rename 失败时返回错误。
    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;

    /// 以独占方式创建文件；文件已存在时返回 `AlreadyExists`。
    ///
    /// # Errors
    ///
    /// 文件已存在或无法创建时返回错误。
    fn create_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;

    /// 递归创建目录。
    ///
    /// # Errors
    ///
    /// 目录无法创建时返回错误。
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;

    /// 递归删除目录。
    ///
    /// # Errors
    ///
    /// 目录无法删除时返回错误。
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;

    /// 删除文件。
    ///
    /// # Errors
    ///
    /// 文件无法删除时返回错误。
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// 重命名文件或目录。
    ///
    /// # Errors
    ///
    /// 源不存在、目标已存在或移动失败时返回错误。
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// 列出目录项，按路径排序。
    ///
    /// # Errors
    ///
    /// 目录无法读取时返回错误。
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;

    /// 路径是否存在。
    fn exists(&self, path: &Path) -> bool;

    /// 路径是否为目录。
    fn is_dir(&self, path: &Path) -> bool;

    /// 创建符号链接。
    ///
    /// # Errors
    ///
    /// 平台不支持、目标存在或权限不足时返回错误。
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()>;

    /// 读取符号链接目标。
    ///
    /// # Errors
    ///
    /// 路径不是链接或无法读取时返回错误。
    fn read_link(&self, path: &Path) -> io::Result<PathBuf>;

    /// 在给定目录中探测符号链接能力。
    fn supports_symlink(&self, directory: &Path) -> bool;
}

/// 本地文件系统实现。
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFileSystem;

impl FileSystem for LocalFileSystem {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        atomic_write(path, bytes)
    }

    fn create_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
        })?;
        fs::create_dir_all(parent)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir_all(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort();
        Ok(entries)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    #[cfg(unix)]
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(not(unix))]
    fn symlink(&self, _target: &Path, _link: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symbolic links are not supported on this platform",
        ))
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        fs::read_link(path)
    }

    fn supports_symlink(&self, directory: &Path) -> bool {
        #[cfg(unix)]
        {
            if !directory.is_dir() {
                return false;
            }
            let target = temp_path(directory);
            let link = temp_path(directory);
            if fs::write(&target, b"").is_err() {
                return false;
            }
            let created = std::os::unix::fs::symlink(&target, &link).is_ok();
            let verified = created && fs::read_link(&link).is_ok();
            let _ = fs::remove_file(&link);
            let _ = fs::remove_file(&target);
            verified
        }
        #[cfg(not(unix))]
        {
            let _ = directory;
            false
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    fs::create_dir_all(parent)?;
    let temp = temp_path(parent);
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn temp_path(parent: &Path) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".tmp-{}-{counter}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::{FileSystem, LocalFileSystem};
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("arkmcl-fs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn writes_atomically_and_reads_back() {
        let directory = temp_dir("write");
        let path = directory.join("nested/file.txt");
        let fs = LocalFileSystem;
        fs.write(&path, b"hello").expect("write");
        assert_eq!(fs.read(&path).expect("read"), b"hello");
        let _ = fs::remove_dir_all(&directory);
    }

    #[cfg(unix)]
    #[test]
    fn probes_symlink_support() {
        let directory = temp_dir("probe");
        let fs = LocalFileSystem;
        assert!(fs.supports_symlink(&directory));
        assert!(!fs.supports_symlink(&directory.join("missing")));
        let _ = fs::remove_dir_all(&directory);
    }

    #[cfg(unix)]
    #[test]
    fn creates_and_reads_symlink() {
        let directory = temp_dir("link");
        let target = directory.join("target");
        let link = directory.join("link");
        fs::write(&target, b"data").expect("target");
        let fs = LocalFileSystem;
        fs.symlink(&target, &link).expect("symlink");
        assert_eq!(fs.read_link(&link).expect("read_link"), target);
        let _ = fs::remove_dir_all(&directory);
    }
}
