//! Purpur 服务端 API。
//!
//! 只负责构建号解析与下载；jar 写入实例根目录的 `server.jar`。

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::download::{Downloader, Mode, Progress, Task};

const BASE: &str = "https://api.purpurmc.org/v2/purpur";
const MAX_BYTES: u64 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
const ATTEMPTS: u32 = 4;

/// Purpur 错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io(std::io::Error),
    /// 传输错误。
    Http(String),
    /// HTTP 状态码错误。
    Status {
        /// 状态码。
        code: u16,
    },
    /// JSON 解析错误。
    Json(serde_json::Error),
    /// 响应超过大小上限。
    TooLarge,
    /// 该 Minecraft 版本没有可用构建。
    MissingBuild(String),
    /// 下载失败。
    Download(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Http(error) => write!(formatter, "{error}"),
            Self::Status { code } => write!(formatter, "Purpur returned HTTP {code}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::TooLarge => formatter.write_str("response exceeds size limit"),
            Self::MissingBuild(version) => {
                write!(formatter, "no Purpur build for Minecraft {version}")
            }
            Self::Download(error) => write!(formatter, "download failed: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Http(_)
            | Self::Status { .. }
            | Self::TooLarge
            | Self::MissingBuild(_)
            | Self::Download(_) => None,
        }
    }
}

/// 解析构建号；未指定时取最新构建。
///
/// # Errors
///
/// 网络、HTTP 或 JSON 错误时返回错误。
pub fn resolve(minecraft: &str, requested: Option<&str>) -> Result<String, Error> {
    if let Some(build) = requested.map(str::trim).filter(|build| !build.is_empty()) {
        return Ok(build.to_owned());
    }
    let response: VersionInfo = get(&format!("{BASE}/{minecraft}"))?;
    if response.builds.latest.is_empty() {
        return Err(Error::MissingBuild(minecraft.to_owned()));
    }
    Ok(response.builds.latest)
}

/// 列出某 Minecraft 版本的全部构建号（升序）。
///
/// # Errors
///
/// 网络、HTTP 或 JSON 错误时返回错误。
pub fn builds(minecraft: &str) -> Result<Vec<String>, Error> {
    let response: VersionInfo = get(&format!("{BASE}/{minecraft}"))?;
    let mut builds = response.builds.all;
    builds.sort_by_key(|build| build.parse::<u64>().unwrap_or(0));
    Ok(builds)
}

/// 下载并写入 `target`。
///
/// # Errors
///
/// 网络、写入或下载错误时返回错误。
pub fn install(
    minecraft: &str,
    build: &str,
    target: &Path,
    downloader: &Downloader,
    progress: &dyn Progress,
) -> Result<(), Error> {
    let url = format!("{BASE}/{minecraft}/{build}/download");
    let task = Task {
        url,
        path: target.to_path_buf(),
        sha1: String::new(),
    };
    downloader
        .run_with(&[task], Mode::Overwrite, progress)
        .map_err(|error| Error::Download(error.to_string()))?;
    Ok(())
}

fn get<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, Error> {
    let agent = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .user_agent(concat!("arkmcl/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut attempt = 0;
    loop {
        attempt += 1;
        match get_once(&agent, url) {
            Ok(value) => return Ok(value),
            Err(error) if attempt < ATTEMPTS && retryable(&error) => {
                std::thread::sleep(crate::retry::backoff(attempt));
            }
            Err(error) => return Err(error),
        }
    }
}

fn get_once<T: for<'de> Deserialize<'de>>(agent: &ureq::Agent, url: &str) -> Result<T, Error> {
    if !url.starts_with("https://") {
        return Err(Error::Http(format!("refusing non-HTTPS URL: {url}")));
    }
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) => return Err(Error::Status { code }),
        Err(error) => return Err(Error::Http(error.to_string())),
    };
    let mut reader = response.into_reader().take(MAX_BYTES + 1);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn retryable(error: &Error) -> bool {
    match error {
        Error::Status { code } => crate::retry::retryable_status(*code),
        Error::Http(_) => true,
        Error::Io(source) => crate::retry::retryable_io(source.kind()),
        Error::Json(_) | Error::TooLarge | Error::MissingBuild(_) | Error::Download(_) => false,
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Deserialize)]
struct VersionInfo {
    builds: Builds,
}

#[derive(Deserialize)]
struct Builds {
    #[serde(default)]
    latest: String,
    #[serde(default)]
    all: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::VersionInfo;

    #[test]
    fn resolves_explicit_build_without_network() {
        assert_eq!(
            super::resolve("1.21.1", Some("2324")).expect("build"),
            "2324"
        );
    }

    #[test]
    fn parses_builds() {
        let info: VersionInfo = serde_json::from_str(
            r#"{"project":"purpur","version":"1.21.1","builds":{"latest":"2324","all":["2324","2323"]}}"#,
        )
        .expect("purpur");
        assert_eq!(info.builds.latest, "2324");
        assert_eq!(info.builds.all.len(), 2);
    }
}
