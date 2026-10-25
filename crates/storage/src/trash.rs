//! 系统回收站（XDG Trash 规范）。
//!
//! 删除实例时移动到 `$XDG_DATA_HOME/Trash/files/`，并在 `info/` 写入
//! `.trashinfo`，以便文件管理器恢复。跨文件系统时不复制，直接报错。

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 回收站错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
    /// 无法确定回收站位置。
    NoHome,
    /// 目标与回收站位于不同文件系统。
    CrossDevice(PathBuf),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::NoHome => formatter.write_str("cannot determine trash directory"),
            Self::CrossDevice(path) => write!(
                formatter,
                "{} is on a different filesystem than the trash",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NoHome | Self::CrossDevice(_) => None,
        }
    }
}

/// 把路径移入回收站，返回回收站中的新路径。
///
/// # Errors
///
/// 无法确定回收站、写入元数据失败、移动失败或跨文件系统时返回错误。
pub fn trash(path: &Path) -> Result<PathBuf, Error> {
    match trash_in(&trash_root()?, path) {
        Err(Error::CrossDevice(_)) => {
            // XDG 规范：跨文件系统时使用该挂载点的 `.Trash-<uid>`。
            let root = mount_root(path).join(format!(".Trash-{}", uid()));
            trash_in(&root, path)
        }
        other => other,
    }
}

/// 路径所在文件系统的挂载点。
fn mount_root(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    let Ok(mut device) = fs::symlink_metadata(&current).map(|meta| meta.dev()) else {
        return path
            .parent()
            .map_or_else(|| path.to_path_buf(), Path::to_path_buf);
    };
    while let Some(parent) = current.parent() {
        let Ok(meta) = fs::symlink_metadata(parent) else {
            break;
        };
        if meta.dev() != device {
            break;
        }
        device = meta.dev();
        current = parent.to_path_buf();
    }
    current
}

fn uid() -> u32 {
    fs::metadata("/proc/self").map_or(0, |meta| meta.uid())
}

/// 把路径移入指定回收站根，便于测试。
fn trash_in(root: &Path, path: &Path) -> Result<PathBuf, Error> {
    let files = root.join("files");
    let info = root.join("info");

    for directory in [&files, &info] {
        fs::create_dir_all(directory).map_err(|source| Error::Io {
            path: directory.clone(),
            source,
        })?;
    }

    let name = path.file_name().map_or_else(
        || "instance".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let (target, entry) = unique_target(&files, &info, &name);

    let absolute = absolute_path(path);
    let contents = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        percent_encode(&absolute.to_string_lossy()),
        timestamp()
    );
    fs::write(&entry, contents).map_err(|source| Error::Io {
        path: entry.clone(),
        source,
    })?;

    match fs::rename(path, &target) {
        Ok(()) => Ok(target),
        Err(source) => {
            let _ = fs::remove_file(&entry);
            if source.raw_os_error() == Some(18) {
                return Err(Error::CrossDevice(path.to_path_buf()));
            }
            Err(Error::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

fn trash_root() -> Result<PathBuf, Error> {
    if let Some(base) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(base).join("Trash"));
    }
    let home = std::env::var_os("HOME").ok_or(Error::NoHome)?;
    Ok(PathBuf::from(home).join(".local/share/Trash"))
}

fn unique_target(files: &Path, info: &Path, name: &str) -> (PathBuf, PathBuf) {
    for index in 0..1000 {
        let candidate = if index == 0 {
            name.to_owned()
        } else {
            format!("{name}.{index}")
        };
        let target = files.join(&candidate);
        let entry = info.join(format!("{candidate}.trashinfo"));
        if !target.exists() && !entry.exists() {
            return (target, entry);
        }
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    (
        files.join(format!("{name}.{stamp}")),
        info.join(format!("{name}.{stamp}.trashinfo")),
    )
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
}

fn percent_encode(text: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        remainder / 3600,
        (remainder % 3600) / 60,
        remainder % 60
    )
}

/// Howard Hinnant 的 `civil_from_days`：把 Unix 天数转换为公历年月日。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, percent_encode, trash_in};
    use std::fs;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("arkmcl-trash-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn converts_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
    }

    #[test]
    fn encodes_paths() {
        assert_eq!(
            percent_encode("/tmp/a b/中文"),
            "/tmp/a%20b/%E4%B8%AD%E6%96%87"
        );
    }

    #[test]
    fn moves_directories_into_trash() {
        let home = temp_dir("move");
        let trash_root = home.join("Trash");
        let instance = home.join("instances/demo");
        fs::create_dir_all(&instance).expect("instance");
        fs::write(instance.join("file.txt"), b"data").expect("file");

        let trashed = trash_in(&trash_root, &instance).expect("trash");
        assert!(!instance.exists());
        assert!(trashed.join("file.txt").exists());
        let info = home.join("Trash/info/demo.trashinfo");
        let entry = fs::read_to_string(info).expect("trashinfo");
        assert!(entry.contains("[Trash Info]"));
        assert!(entry.contains("DeletionDate="));
        assert!(entry.contains("instances/demo"));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn avoids_name_collisions() {
        let home = temp_dir("collision");
        let trash_root = home.join("Trash");
        let first = home.join("a/demo");
        let second = home.join("b/demo");
        for path in [&first, &second] {
            fs::create_dir_all(path).expect("instance");
        }
        let trashed = trash_in(&trash_root, &first).expect("first");
        assert!(trashed.ends_with("demo"));
        let trashed = trash_in(&trash_root, &second).expect("second");
        assert!(trashed.ends_with("demo.1"));
        let _ = fs::remove_dir_all(&home);
    }
}
