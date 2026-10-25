//! Java 运行时探测。
//!
//! 从 `JAVA_HOME`、`JDK_HOME`、`PATH` 与常见 JDK 目录收集候选，
//! 通过 `java -XshowSettings:properties -version` 读取版本、架构与供应商标识。
//! 不自动下载 Java。

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 已探测的 Java 运行时。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    /// `java` 可执行文件。
    pub path: PathBuf,
    /// 主版本。
    pub major: u32,
    /// 规范化架构。
    pub arch: String,
    /// 供应商。
    pub vendor: String,
}

/// 探测错误。
#[derive(Debug)]
pub enum Error {
    /// 无法执行 java。
    Io {
        /// 可执行文件。
        path: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
    /// 输出中没有版本信息。
    MissingVersion(PathBuf),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::MissingVersion(path) => {
                write!(
                    formatter,
                    "cannot determine Java version: {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::MissingVersion(_) => None,
        }
    }
}

/// 探测全部可用运行时，按主版本降序。
#[must_use]
pub fn runtimes() -> Vec<Runtime> {
    let mut seen = HashSet::new();
    let mut runtimes = Vec::new();
    for candidate in candidates() {
        let key = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if !seen.insert(key) {
            continue;
        }
        if let Ok(runtime) = inspect(&candidate) {
            runtimes.push(runtime);
        }
    }
    runtimes.sort_by_key(|runtime| std::cmp::Reverse(runtime.major));
    runtimes
}

/// 选择满足最低主版本的运行时。
///
/// 优先精确匹配，其次选择大于要求的最小主版本。
#[must_use]
pub fn select(required: u32) -> Option<Runtime> {
    choose(&runtimes(), required)
}

/// 从给定列表中选择。
#[must_use]
pub fn choose(runtimes: &[Runtime], required: u32) -> Option<Runtime> {
    if let Some(runtime) = runtimes.iter().find(|runtime| runtime.major == required) {
        return Some(runtime.clone());
    }
    runtimes
        .iter()
        .filter(|runtime| runtime.major >= required)
        .min_by_key(|runtime| runtime.major)
        .cloned()
}

/// 读取单个 java 可执行文件的信息。
///
/// # Errors
///
/// 无法执行或输出中没有版本信息时返回错误。
pub fn inspect(path: &Path) -> Result<Runtime, Error> {
    let output = Command::new(path)
        .args(["-XshowSettings:properties", "-version"])
        .output()
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let version = property(&text, "java.version")
        .or_else(|| version_line(&text))
        .ok_or_else(|| Error::MissingVersion(path.to_path_buf()))?;
    let major = major(version).ok_or_else(|| Error::MissingVersion(path.to_path_buf()))?;
    let arch = property(&text, "os.arch")
        .map_or_else(|| std::env::consts::ARCH.to_owned(), normalize_arch);
    let vendor = property(&text, "java.vendor")
        .unwrap_or("unknown")
        .to_owned();
    Ok(Runtime {
        path: path.to_path_buf(),
        major,
        arch,
        vendor,
    })
}

fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("JDK_HOME") {
        paths.push(PathBuf::from(home).join("bin").join("java"));
    }
    paths.extend(java_home_paths(std::env::vars_os()));
    if let Some(value) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&value) {
            paths.push(directory.join("java"));
        }
    }
    for base in ["/usr/lib/jvm", "/usr/java", "/opt/java"] {
        push_jdks(&mut paths, Path::new(base));
    }
    if let Some(home) = std::env::var_os("HOME") {
        push_jdks(&mut paths, &PathBuf::from(home).join(".jdks"));
    }
    paths.retain(|path| path.is_file());
    paths
}

fn java_home_paths<I>(vars: I) -> Vec<PathBuf>
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    vars.into_iter()
        .filter_map(|(key, value)| {
            let key = key.to_string_lossy();
            (key.starts_with("JAVA") && key.ends_with("_HOME"))
                .then(|| PathBuf::from(value).join("bin").join("java"))
        })
        .collect()
}

fn push_jdks(paths: &mut Vec<PathBuf>, base: &Path) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        paths.push(entry.path().join("bin").join("java"));
    }
}

fn property<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| name.trim() == key)
        .map(|(_, value)| value.trim())
}

fn version_line(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let index = line.find("version \"")?;
        let rest = &line[index + 9..];
        let end = rest.find('"')?;
        Some(&rest[..end])
    })
}

fn major(version: &str) -> Option<u32> {
    let first = version.split(['.', '_', '-']).next()?;
    let first: u32 = first.parse().ok()?;
    if first == 1 {
        version.split('.').nth(1)?.parse().ok()
    } else {
        Some(first)
    }
}

fn normalize_arch(arch: &str) -> String {
    match arch {
        "amd64" | "x86_64" => "x86_64".to_owned(),
        "aarch64" | "arm64" => "arm64".to_owned(),
        "x86" | "i386" | "i686" => "x86".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Runtime, choose, inspect, java_home_paths, major};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    #[test]
    fn scans_java_home_variants() {
        let vars = vec![
            (
                std::ffi::OsString::from("JAVA8_HOME"),
                std::ffi::OsString::from("/j8"),
            ),
            (
                std::ffi::OsString::from("JAVA17_HOME"),
                std::ffi::OsString::from("/j17"),
            ),
            (
                std::ffi::OsString::from("PATH"),
                std::ffi::OsString::from("/bin"),
            ),
        ];
        assert_eq!(
            java_home_paths(vars),
            vec![
                PathBuf::from("/j8/bin/java"),
                PathBuf::from("/j17/bin/java")
            ]
        );
    }

    #[test]
    fn parses_major_versions() {
        assert_eq!(major("1.8.0_392"), Some(8));
        assert_eq!(major("17.0.9"), Some(17));
        assert_eq!(major("21"), Some(21));
        assert_eq!(major("bad"), None);
    }

    #[test]
    fn chooses_exact_then_closest() {
        let runtimes = vec![
            Runtime {
                path: PathBuf::from("/jdk8/bin/java"),
                major: 8,
                arch: "x86_64".to_owned(),
                vendor: "test".to_owned(),
            },
            Runtime {
                path: PathBuf::from("/jdk21/bin/java"),
                major: 21,
                arch: "x86_64".to_owned(),
                vendor: "test".to_owned(),
            },
        ];
        assert_eq!(choose(&runtimes, 21).expect("exact").major, 21);
        assert_eq!(choose(&runtimes, 17).expect("closest").major, 21);
        assert!(choose(&runtimes, 25).is_none());
    }

    #[test]
    fn inspects_a_fake_java() {
        let directory =
            std::env::temp_dir().join(format!("arkmcl-java-{}-inspect", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("temp dir");
        let script = directory.join("java");
        fs::write(
            &script,
            "#!/bin/sh\necho '    java.version = 21.0.2' >&2\necho '    os.arch = amd64' >&2\necho '    java.vendor = Test' >&2\n",
        )
        .expect("script");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("permissions");

        let runtime = inspect(&script).expect("inspect");
        assert_eq!(runtime.major, 21);
        assert_eq!(runtime.arch, "x86_64");
        assert_eq!(runtime.vendor, "Test");
        let _ = fs::remove_dir_all(&directory);
    }
}
