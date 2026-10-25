//! 应用路径解析。
//!
//! 优先级：显式环境变量 > 便携模式 > XDG 默认。
//! 便携模式默认禁用，由可执行文件旁的 `portable` 文件或
//! `ARKMCL_PORTABLE=1` 启用。

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const PACKAGE: &str = "com.purft.arkmcl";

/// 路径解析错误。
#[derive(Debug)]
pub enum Error {
    /// 无法确定 `HOME`。
    HomeMissing,
    /// 创建或读取路径失败。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层 I/O 错误。
        source: io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeMissing => formatter.write_str("cannot determine HOME directory"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HomeMissing => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

/// 解析后的应用路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// 配置根。
    pub config: PathBuf,
    /// 数据根。
    pub data: PathBuf,
    /// 游戏资源根。
    pub minecraft: PathBuf,
    /// 状态根。
    pub state: PathBuf,
    /// 运行时根。
    pub runtime: PathBuf,
    /// 是否处于便携模式。
    pub portable: bool,
    /// 是否退化为状态目录下的 `run`。
    pub runtime_fallback: bool,
}

impl Paths {
    /// 从当前进程环境解析路径。
    ///
    /// # Errors
    ///
    /// `HOME` 缺失且没有显式路径时返回错误。
    pub fn discover() -> Result<Self, Error> {
        let executable = std::env::current_exe().map_err(|source| Error::Io {
            path: PathBuf::from("current_exe"),
            source,
        })?;
        let executable_dir = executable.parent().ok_or(Error::HomeMissing)?;
        Self::resolve(executable_dir, |key| std::env::var_os(key))
    }

    /// 从给定可执行文件目录和取值函数解析路径，便于测试。
    ///
    /// # Errors
    ///
    /// 必需目录无法确定时返回错误。
    pub fn resolve<F>(executable_dir: &Path, get: F) -> Result<Self, Error>
    where
        F: Fn(&str) -> Option<OsString>,
    {
        let portable =
            is_truthy(get("ARKMCL_PORTABLE")) || executable_dir.join("portable").exists();
        let env_path = |key: &str| {
            get(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let home = env_path("HOME");

        let config = env_path("ARKMCL_CONFIG_HOME")
            .or_else(|| portable.then(|| executable_dir.join(".arkmcl")))
            .or_else(|| {
                env_path("XDG_CONFIG_HOME")
                    .or_else(|| home.as_ref().map(|path| path.join(".config")))
                    .map(|base| base.join(PACKAGE))
            })
            .ok_or(Error::HomeMissing)?;

        let data = env_path("ARKMCL_DATA_HOME")
            .or_else(|| portable.then(|| executable_dir.join(".arkmcl")))
            .or_else(|| {
                env_path("XDG_DATA_HOME")
                    .or_else(|| home.as_ref().map(|path| path.join(".local/share")))
                    .map(|base| base.join(PACKAGE))
            })
            .ok_or(Error::HomeMissing)?;

        let state = env_path("ARKMCL_STATE_HOME")
            .or_else(|| portable.then(|| executable_dir.join(".arkmcl")))
            .or_else(|| {
                env_path("XDG_STATE_HOME")
                    .or_else(|| home.as_ref().map(|path| path.join(".local/state")))
                    .map(|base| base.join(PACKAGE))
            })
            .ok_or(Error::HomeMissing)?;

        let (runtime, runtime_fallback) = if let Some(path) = env_path("ARKMCL_RUNTIME_DIR") {
            (path, false)
        } else if portable {
            (executable_dir.join(".arkmcl"), false)
        } else if let Some(path) = env_path("XDG_RUNTIME_DIR") {
            (path.join(PACKAGE), false)
        } else {
            (state.join("run"), true)
        };

        let minecraft = env_path("MINECRAFT_HOME")
            .or_else(|| portable.then(|| executable_dir.join(".minecraft")))
            .or_else(|| home.map(|path| path.join(".minecraft")))
            .ok_or(Error::HomeMissing)?;

        Ok(Self {
            config,
            data,
            minecraft,
            state,
            runtime,
            portable,
            runtime_fallback,
        })
    }

    /// 创建应用目录；应用目录权限为 `0700`。
    ///
    /// # Errors
    ///
    /// 目录无法创建或权限无法设置时返回错误。
    pub fn ensure(&self) -> Result<(), Error> {
        for path in [&self.config, &self.data, &self.state, &self.runtime] {
            create_dir(path, true)?;
        }
        create_dir(&self.minecraft, false)?;
        Ok(())
    }

    /// Unix socket 路径。
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("arkmcl.sock")
    }

    /// pid 文件路径。
    #[must_use]
    pub fn pid(&self) -> PathBuf {
        self.runtime.join("arkmcl.pid")
    }
}

fn is_truthy(value: Option<OsString>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.to_string_lossy().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn create_dir(path: &Path, private: bool) -> Result<(), Error> {
    if path.exists() {
        return Ok(());
    }
    fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            Error::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PACKAGE, Paths};
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    fn resolve(pairs: &[(&str, &str)]) -> Result<Paths, super::Error> {
        let env: HashMap<String, OsString> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect();
        Paths::resolve(Path::new("/opt/arkmcl"), |key| env.get(key).cloned())
    }

    #[test]
    fn portable_layout_uses_executable_directory() {
        let paths = resolve(&[("ARKMCL_PORTABLE", "1"), ("HOME", "/home/user")]).expect("paths");
        assert!(paths.portable);
        assert_eq!(paths.config, PathBuf::from("/opt/arkmcl/.arkmcl"));
        assert_eq!(paths.data, PathBuf::from("/opt/arkmcl/.arkmcl"));
        assert_eq!(paths.state, PathBuf::from("/opt/arkmcl/.arkmcl"));
        assert_eq!(paths.runtime, PathBuf::from("/opt/arkmcl/.arkmcl"));
        assert_eq!(paths.minecraft, PathBuf::from("/opt/arkmcl/.minecraft"));
    }

    #[test]
    fn xdg_defaults_use_package_directory() {
        let paths = resolve(&[
            ("HOME", "/home/user"),
            ("XDG_CONFIG_HOME", "/cfg"),
            ("XDG_DATA_HOME", "/data"),
            ("XDG_STATE_HOME", "/state"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ])
        .expect("paths");
        assert!(!paths.portable);
        assert_eq!(paths.config, PathBuf::from(format!("/cfg/{PACKAGE}")));
        assert_eq!(paths.data, PathBuf::from(format!("/data/{PACKAGE}")));
        assert_eq!(paths.state, PathBuf::from(format!("/state/{PACKAGE}")));
        assert_eq!(
            paths.runtime,
            PathBuf::from(format!("/run/user/1000/{PACKAGE}"))
        );
        assert_eq!(paths.minecraft, PathBuf::from("/home/user/.minecraft"));
        assert!(!paths.runtime_fallback);
    }

    #[test]
    fn explicit_environment_wins_over_portable() {
        let paths = resolve(&[
            ("ARKMCL_PORTABLE", "1"),
            ("ARKMCL_DATA_HOME", "/custom/data"),
            ("MINECRAFT_HOME", "/custom/minecraft"),
            ("HOME", "/home/user"),
        ])
        .expect("paths");
        assert_eq!(paths.data, PathBuf::from("/custom/data"));
        assert_eq!(paths.minecraft, PathBuf::from("/custom/minecraft"));
    }

    #[test]
    fn runtime_falls_back_to_state_directory() {
        let paths = resolve(&[("HOME", "/home/user")]).expect("paths");
        assert!(paths.runtime_fallback);
        assert_eq!(
            paths.runtime,
            PathBuf::from(format!("/home/user/.local/state/{PACKAGE}/run"))
        );
    }
}
