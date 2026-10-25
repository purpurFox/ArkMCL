//! Forge / `NeoForge` loader metadata and installer.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

const FORGE_METADATA: &str =
    "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml";
const NEOFORGE_METADATA: &str =
    "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";
const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/net/minecraftforge/forge";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/net/neoforged/neoforge";
const TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_METADATA: u64 = 8 * 1024 * 1024;
const MAX_INSTALLER: u64 = 128 * 1024 * 1024;

/// Loader family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Minecraft Forge.
    Forge,
    /// `NeoForge`.
    NeoForge,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Forge => "forge",
            Self::NeoForge => "neoforge",
        }
    }
}

/// Loader version entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoaderVersion {
    /// Loader version.
    pub version: String,
    /// Whether this is the recommended/stable build.
    pub stable: bool,
}

/// Forge / `NeoForge` error.
#[derive(Debug)]
pub enum Error {
    /// I/O error.
    Io(std::io::Error),
    /// HTTP error.
    Http(String),
    /// JSON error.
    Json(serde_json::Error),
    /// Non-HTTPS URL.
    Url(String),
    /// No matching version.
    Missing(String),
    /// Installer failed.
    Installer(String),
    /// Installer exceeded the timeout.
    Timeout,
    /// Response exceeded the size limit.
    TooLarge,
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Http(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Url(url) => write!(formatter, "refusing non-HTTPS URL: {url}"),
            Self::Missing(value) => write!(formatter, "no matching version: {value}"),
            Self::Installer(message) => write!(formatter, "installer failed: {message}"),
            Self::Timeout => formatter.write_str("installer timed out"),
            Self::TooLarge => formatter.write_str("response exceeds size limit"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Http(_)
            | Self::Url(_)
            | Self::Missing(_)
            | Self::Installer(_)
            | Self::Timeout
            | Self::TooLarge => None,
        }
    }
}

/// `NeoForge` version prefix for a Minecraft version.
#[must_use]
pub fn neoforge_prefix(minecraft: &str) -> String {
    let mut parts = minecraft.split('.');
    let _ = parts.next();
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    format!("{major}.{minor}.")
}

/// List loader versions for a Minecraft version.
///
/// # Errors
///
/// Network, HTTP or JSON errors are returned.
pub fn versions(kind: Kind, minecraft: &str) -> Result<Vec<LoaderVersion>, Error> {
    match kind {
        Kind::Forge => forge_versions(minecraft),
        Kind::NeoForge => neoforge_versions(minecraft),
    }
}

/// Resolve a requested loader version, or choose the newest/recommended one.
///
/// # Errors
///
/// Network errors or no matching version.
pub fn resolve(kind: Kind, minecraft: &str, requested: Option<&str>) -> Result<String, Error> {
    if let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(requested.to_owned());
    }
    let versions = versions(kind, minecraft)?;
    match kind {
        Kind::Forge => versions
            .iter()
            .filter(|entry| entry.stable)
            .max_by_key(|entry| version_key(&entry.version))
            .or_else(|| {
                versions
                    .iter()
                    .max_by_key(|entry| version_key(&entry.version))
            })
            .map(|entry| entry.version.clone()),
        Kind::NeoForge => versions
            .iter()
            .filter(|entry| entry.stable)
            .max_by_key(|entry| version_key(&entry.version))
            .or_else(|| {
                versions
                    .iter()
                    .max_by_key(|entry| version_key(&entry.version))
            })
            .map(|entry| entry.version.clone()),
    }
    .ok_or_else(|| Error::Missing(minecraft.to_owned()))
}

/// Installer URL for a loader version.
///
/// # Errors
///
/// The loader version is invalid for the selected family.
pub fn installer_url(kind: Kind, minecraft: &str, loader: &str) -> Result<String, Error> {
    match kind {
        Kind::Forge => {
            if loader.is_empty() {
                return Err(Error::Missing("forge loader version".to_owned()));
            }
            Ok(format!(
                "{FORGE_MAVEN}/{minecraft}-{loader}/forge-{minecraft}-{loader}-installer.jar"
            ))
        }
        Kind::NeoForge => {
            if loader.is_empty() {
                return Err(Error::Missing("neoforge loader version".to_owned()));
            }
            Ok(format!(
                "{NEOFORGE_MAVEN}/{loader}/neoforge-{loader}-installer.jar"
            ))
        }
    }
}

/// Download and run the installer, returning the generated version id.
///
/// # Errors
///
/// Network, installer, timeout or verification errors are returned.
pub fn install(
    kind: Kind,
    minecraft: &str,
    loader: &str,
    java: &Path,
    directory: &Path,
) -> Result<String, Error> {
    let url = installer_url(kind, minecraft, loader)?;
    let temp = directory.join("cache");
    fs::create_dir_all(&temp).map_err(Error::Io)?;
    let profiles = directory.join("launcher_profiles.json");
    if !profiles.exists() {
        let _ = fs::write(&profiles, r#"{"profiles":{},"settings":{},"version":3}"#);
    }
    let installer = temp.join(format!("arkmcl-{}-{loader}-installer.jar", kind.label()));
    download(&url, &installer)?;
    let log_path = temp.join(format!("arkmcl-{}-{loader}-installer.log", kind.label()));
    let log = fs::File::create(&log_path).map_err(Error::Io)?;
    let errors = log.try_clone().map_err(Error::Io)?;
    let started = now_unix();
    let mut child = Command::new(java)
        .arg("-jar")
        .arg(&installer)
        .arg("--installClient")
        .arg(directory)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errors))
        .spawn()
        .map_err(Error::Io)?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(Error::Installer(log_tail(&log_path)));
                }
                break;
            }
            Ok(None) => {
                if start.elapsed() > INSTALL_TIMEOUT {
                    let _ = child.kill();
                    return Err(Error::Timeout);
                }
                thread::sleep(Duration::from_millis(250));
            }
            Err(error) => return Err(Error::Io(error)),
        }
    }
    find_installed(directory, minecraft, started)
        .ok_or_else(|| Error::Installer(log_tail(&log_path)))
}

/// 安装服务端并返回 `unix_args.txt` 路径。
///
/// # Errors
///
/// 网络、installer、超时或产物缺失时返回错误。
pub fn install_server(
    kind: Kind,
    minecraft: &str,
    loader: &str,
    java: &Path,
    directory: &Path,
) -> Result<PathBuf, Error> {
    let url = installer_url(kind, minecraft, loader)?;
    let temp = directory.join("cache");
    fs::create_dir_all(&temp).map_err(Error::Io)?;
    let installer = temp.join(format!(
        "arkmcl-{}-{loader}-server-installer.jar",
        kind.label()
    ));
    download(&url, &installer)?;
    let log_path = temp.join(format!(
        "arkmcl-{}-{loader}-server-installer.log",
        kind.label()
    ));
    let log = fs::File::create(&log_path).map_err(Error::Io)?;
    let errors = log.try_clone().map_err(Error::Io)?;
    let mut child = Command::new(java)
        .arg("-jar")
        .arg(&installer)
        .arg("--installServer")
        .arg(directory)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errors))
        .spawn()
        .map_err(Error::Io)?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(Error::Installer(log_tail(&log_path)));
                }
                break;
            }
            Ok(None) => {
                if start.elapsed() > INSTALL_TIMEOUT {
                    let _ = child.kill();
                    return Err(Error::Timeout);
                }
                thread::sleep(Duration::from_millis(250));
            }
            Err(error) => return Err(Error::Io(error)),
        }
    }
    let args = server_args_file(kind, minecraft, loader, directory);
    if args.is_file() {
        Ok(args)
    } else {
        Err(Error::Installer(log_tail(&log_path)))
    }
}

/// Forge/NeoForge 服务端参数文件路径。
#[must_use]
pub fn server_args_file(kind: Kind, minecraft: &str, loader: &str, directory: &Path) -> PathBuf {
    match kind {
        Kind::Forge => directory
            .join("libraries/net/minecraftforge/forge")
            .join(format!("{minecraft}-{loader}"))
            .join("unix_args.txt"),
        Kind::NeoForge => directory
            .join("libraries/net/neoforged/neoforge")
            .join(loader)
            .join("unix_args.txt"),
    }
}

fn forge_versions(minecraft: &str) -> Result<Vec<LoaderVersion>, Error> {
    let body = get(FORGE_METADATA)?;
    let prefix = format!("{minecraft}-");
    let mut versions: Vec<LoaderVersion> = extract_versions(&body)
        .into_iter()
        .filter_map(|version| version.strip_prefix(&prefix).map(str::to_owned))
        .map(|version| LoaderVersion {
            stable: !version.contains("-beta"),
            version,
        })
        .collect();
    versions.sort_by_key(|entry| version_key(&entry.version));
    versions.dedup_by(|left, right| left.version == right.version);
    Ok(versions)
}

fn neoforge_versions(minecraft: &str) -> Result<Vec<LoaderVersion>, Error> {
    let body = get(NEOFORGE_METADATA)?;
    let prefix = neoforge_prefix(minecraft);
    let mut versions: Vec<LoaderVersion> = extract_versions(&body)
        .into_iter()
        .filter(|version| version.starts_with(&prefix))
        .map(|version| LoaderVersion {
            stable: !version.contains("-beta"),
            version,
        })
        .collect();
    versions.sort_by_key(|entry| version_key(&entry.version));
    versions.dedup_by(|left, right| left.version == right.version);
    Ok(versions)
}

fn extract_versions(xml: &str) -> Vec<String> {
    let mut versions = Vec::new();
    for part in xml.split("<version>").skip(1) {
        if let Some(end) = part.find("</version>") {
            versions.push(part[..end].trim().to_owned());
        }
    }
    versions
}

fn version_key(version: &str) -> Vec<u64> {
    version
        .split(['.', '-'])
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn get(url: &str) -> Result<String, Error> {
    if !url.starts_with("https://") {
        return Err(Error::Url(url.to_owned()));
    }
    let response = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .user_agent(concat!("arkmcl/", env!("CARGO_PKG_VERSION")))
        .build()
        .get(url)
        .call()
        .map_err(|error| Error::Http(error.to_string()))?;
    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_METADATA + 1)
        .read_to_string(&mut body)
        .map_err(Error::Io)?;
    if body.len() as u64 > MAX_METADATA {
        return Err(Error::TooLarge);
    }
    Ok(body)
}

fn download(url: &str, path: &Path) -> Result<(), Error> {
    if !url.starts_with("https://") {
        return Err(Error::Url(url.to_owned()));
    }
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .user_agent(concat!("arkmcl/", env!("CARGO_PKG_VERSION")))
        .build()
        .get(url)
        .call()
        .map_err(|error| Error::Http(error.to_string()))?;
    let mut file = fs::File::create(path).map_err(Error::Io)?;
    let copied = std::io::copy(
        &mut response.into_reader().take(MAX_INSTALLER + 1),
        &mut file,
    )
    .map_err(Error::Io)?;
    file.flush().map_err(Error::Io)?;
    if copied > MAX_INSTALLER {
        let _ = fs::remove_file(path);
        return Err(Error::TooLarge);
    }
    Ok(())
}

fn find_installed(directory: &Path, minecraft: &str, since: u64) -> Option<String> {
    let entries = fs::read_dir(directory.join("versions")).ok()?;
    let mut newest: Option<(u64, String)> = None;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let json = entry.path().join(format!("{name}.json"));
        let Ok(bytes) = fs::read(&json) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if value.get("inheritsFrom").and_then(Value::as_str) != Some(minecraft) {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs());
        if modified.saturating_add(1) < since {
            continue;
        }
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&name)
            .to_owned();
        if newest.as_ref().is_none_or(|(best, _)| modified >= *best) {
            newest = Some((modified, id));
        }
    }
    newest.map(|(_, id)| id)
}

fn log_tail(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    let start = text.len().saturating_sub(2000);
    text[start..].to_owned()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::{Kind, installer_url, neoforge_prefix};

    #[test]
    fn maps_neoforge_prefixes() {
        assert_eq!(neoforge_prefix("1.20.4"), "20.4.");
        assert_eq!(neoforge_prefix("1.21"), "21.0.");
        assert_eq!(neoforge_prefix("1.21.1"), "21.1.");
    }

    #[test]
    fn builds_installer_urls() {
        assert_eq!(
            installer_url(Kind::Forge, "1.20.4", "49.0.30").expect("forge"),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.20.4-49.0.30/forge-1.20.4-49.0.30-installer.jar"
        );
        assert_eq!(
            installer_url(Kind::NeoForge, "1.20.4", "20.4.237").expect("neoforge"),
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/20.4.237/neoforge-20.4.237-installer.jar"
        );
    }
}
