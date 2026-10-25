//! 元数据 HTTP 抓取与校验。
//!
//! 只接受 HTTPS；下载前检查缓存，下载后按 `SHA-1` 校验。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arkmcl_core::meta::{AssetIndex, Loader, Manifest, Version};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha1::{Digest, Sha1};

use crate::fs::FileSystem;
use crate::meta::{Cache, Error as CacheError, Stamp};

const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
/// 可变元数据的默认生存时间。
const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// 覆盖 TTL 的环境变量（秒；`0` 表示永不过期）。
const TTL_ENV: &str = "ARKMCL_META_TTL";

/// 抓取错误。
#[derive(Debug)]
pub enum Error {
    /// 缓存错误。
    Cache(CacheError),
    /// 网络 I/O 错误。
    Io(std::io::Error),
    /// JSON 解析错误。
    Json(serde_json::Error),
    /// HTTP 错误。
    Http(String),
    /// 非 HTTPS 地址。
    Url(String),
    /// 清单中没有该版本。
    MissingVersion(String),
    /// 版本没有资源索引。
    MissingIndex(String),
    /// 校验和不匹配。
    Checksum {
        /// 下载地址。
        url: String,
        /// 期望值。
        expected: String,
        /// 实际值。
        actual: String,
    },
    /// 响应超过大小上限。
    TooLarge,
    /// 服务器返回非成功状态码。
    Status {
        /// 请求地址。
        url: String,
        /// HTTP 状态码。
        code: u16,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cache(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Http(error) => write!(formatter, "{error}"),
            Self::Url(url) => write!(formatter, "refusing non-HTTPS URL: {url}"),
            Self::MissingVersion(id) => write!(formatter, "version not found in manifest: {id}"),
            Self::MissingIndex(id) => write!(formatter, "version has no asset index: {id}"),
            Self::Checksum {
                url,
                expected,
                actual,
            } => write!(
                formatter,
                "checksum mismatch for {url}: expected {expected}, got {actual}"
            ),
            Self::TooLarge => formatter.write_str("response exceeds size limit"),
            Self::Status { url, code } => write!(formatter, "HTTP {code} for {url}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cache(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Http(_)
            | Self::Url(_)
            | Self::MissingVersion(_)
            | Self::MissingIndex(_)
            | Self::Checksum { .. }
            | Self::TooLarge
            | Self::Status { .. } => None,
        }
    }
}

impl From<CacheError> for Error {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
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

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct LoaderEntry {
    loader: Loader,
}

#[derive(Debug, serde::Deserialize)]
struct InstallerEntry {
    version: String,
    #[serde(default)]
    stable: bool,
}

/// 带缓存的元数据抓取器。
pub struct Fetcher<F: FileSystem> {
    cache: Cache<F>,
    agent: ureq::Agent,
    stale: AtomicBool,
}

impl<F: FileSystem> Fetcher<F> {
    /// 创建抓取器。
    #[must_use]
    pub fn new(minecraft: PathBuf, fs: F) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(TIMEOUT)
            .user_agent(concat!("arkmcl/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            cache: Cache::new(minecraft, fs),
            agent,
            stale: AtomicBool::new(false),
        }
    }

    /// 最近一次抓取是否回退到了过期缓存。
    #[must_use]
    pub fn stale(&self) -> bool {
        self.stale.load(Ordering::Relaxed)
    }

    /// 读取并清除 stale 标记。
    pub fn take_stale(&self) -> bool {
        self.stale.swap(false, Ordering::Relaxed)
    }

    /// 获取版本清单。
    ///
    /// 未超过 TTL 时直接使用缓存；过期后带 `If-None-Match` / `If-Modified-Since`
    /// 条件请求校验，网络失败时回退到旧缓存并置 stale 标记。
    ///
    /// # Errors
    ///
    /// 网络失败且无可用缓存、JSON 非法或缓存写入失败时返回错误。
    pub fn manifest(&self) -> Result<Manifest, Error> {
        let path = self.cache.manifest_path();
        // 旧版把清单放在 assets/ 根目录；首次读取时迁移到 assets/indexes/。
        if !self.cache.manifest_exists() {
            if let Some(legacy) = self.cache.legacy_manifest()? {
                self.cache.save_manifest(&legacy)?;
            }
        }
        self.cached_json(MANIFEST_URL, &path, Some(ttl()))
    }

    /// 获取指定版本 JSON；缓存命中时直接返回。
    ///
    /// # Errors
    ///
    /// 清单中不存在该版本、网络失败、校验失败或 JSON 非法时返回错误。
    pub fn version(&self, id: &str) -> Result<Version, Error> {
        if let Some(version) = self.cache.version(id)? {
            return Ok(version);
        }
        let manifest = self.manifest()?;
        let summary = manifest
            .versions
            .iter()
            .find(|summary| summary.id == id)
            .ok_or_else(|| Error::MissingVersion(id.to_owned()))?;
        let bytes = self.get(&summary.url)?;
        verify(&summary.sha1, &bytes, &summary.url)?;
        let version: Version = serde_json::from_slice(&bytes)?;
        self.cache.save_version(&version)?;
        Ok(version)
    }

    /// 获取版本对应的资源索引；缓存命中时直接返回。
    ///
    /// # Errors
    ///
    /// 版本没有资源索引、网络失败、校验失败或 JSON 非法时返回错误。
    pub fn index(&self, version: &Version) -> Result<AssetIndex, Error> {
        let index = version
            .asset_index
            .as_ref()
            .ok_or_else(|| Error::MissingIndex(version.id.clone()))?;
        if let Some(cached) = self.cache.index(&index.id)? {
            return Ok(cached);
        }
        let bytes = self.get(&index.url)?;
        verify(&index.sha1, &bytes, &index.url)?;
        let mut parsed: AssetIndex = serde_json::from_slice(&bytes)?;
        parsed.id.clone_from(&index.id);
        self.cache.save_index(&parsed)?;
        Ok(parsed)
    }

    /// 获取 Fabric loader 版本列表；与版本清单使用同一 TTL。
    ///
    /// # Errors
    ///
    /// 网络失败且无可用缓存，或 JSON 非法时返回错误。
    pub fn fabric_loaders(&self, game: &str) -> Result<Vec<Loader>, Error> {
        let url = format!("https://meta.fabricmc.net/v2/versions/loader/{game}");
        let path = self.cache.loader_list_path(game);
        let entries: Vec<LoaderEntry> = self.cached_json(&url, &path, Some(ttl()))?;
        Ok(entries.into_iter().map(|entry| entry.loader).collect())
    }

    /// 获取最新的稳定 Fabric installer 版本。
    ///
    /// # Errors
    ///
    /// 网络失败、JSON 非法或没有可用版本时返回错误。
    pub fn fabric_installer(&self) -> Result<String, Error> {
        let bytes = self.get("https://meta.fabricmc.net/v2/versions/installer")?;
        let entries: Vec<InstallerEntry> = serde_json::from_slice(&bytes)?;
        entries
            .iter()
            .find(|entry| entry.stable)
            .or_else(|| entries.first())
            .map(|entry| entry.version.clone())
            .ok_or_else(|| Error::Http("no Fabric installer version".to_owned()))
    }

    /// 获取 Fabric loader profile；缓存命中时直接返回。
    ///
    /// # Errors
    ///
    /// 网络失败或 JSON 非法时返回错误。
    pub fn fabric_profile(&self, game: &str, loader: &str) -> Result<Version, Error> {
        if let Some(profile) = self.cache.loader(game, loader)? {
            return Ok(profile);
        }
        let url =
            format!("https://meta.fabricmc.net/v2/versions/loader/{game}/{loader}/profile/json");
        let bytes = self.get(&url)?;
        let profile: Version = serde_json::from_slice(&bytes)?;
        self.cache.save_loader(game, loader, &profile)?;
        Ok(profile)
    }

    /// 按 TTL 读取缓存 JSON；`ttl` 为 `None` 时视为不可变内容。
    fn cached_json<T>(&self, url: &str, path: &Path, ttl: Option<Duration>) -> Result<T, Error>
    where
        T: DeserializeOwned + Serialize,
    {
        if let Some(cached) = self.cache.read_json(path)? {
            return match ttl {
                None => Ok(cached),
                Some(ttl) if self.fresh(path, ttl) => Ok(cached),
                Some(_) => match self.revalidate(url, path) {
                    Ok(Revalidated::NotModified) => Ok(cached),
                    Ok(Revalidated::Fresh {
                        bytes,
                        etag,
                        last_modified,
                    }) => {
                        let parsed: T = serde_json::from_slice(&bytes)?;
                        self.cache.write_json(path, &parsed)?;
                        self.cache.save_stamp(
                            path,
                            &Stamp {
                                etag,
                                last_modified,
                                fetched_at: now(),
                            },
                        )?;
                        Ok(parsed)
                    }
                    Err(error) => {
                        self.stale.store(true, Ordering::Relaxed);
                        eprintln!("arkmcl: using stale cache for {url} after {error}");
                        Ok(cached)
                    }
                },
            };
        }
        let Fetched::Body {
            bytes,
            etag,
            last_modified,
        } = self.fetch(url, None)?
        else {
            return Err(Error::Http(format!("unexpected 304 for {url}")));
        };
        let parsed: T = serde_json::from_slice(&bytes)?;
        self.cache.write_json(path, &parsed)?;
        self.cache.save_stamp(
            path,
            &Stamp {
                etag,
                last_modified,
                fetched_at: now(),
            },
        )?;
        Ok(parsed)
    }

    fn fresh(&self, path: &Path, ttl: Duration) -> bool {
        let Some(stamp) = self.cache.stamp(path) else {
            return false;
        };
        now().saturating_sub(stamp.fetched_at) < ttl.as_secs()
    }

    fn revalidate(&self, url: &str, path: &Path) -> Result<Revalidated, Error> {
        let stamp = self.cache.stamp(path);
        match self.fetch(url, stamp.as_ref())? {
            Fetched::NotModified => {
                let mut stamp = stamp.unwrap_or_default();
                stamp.fetched_at = now();
                self.cache.save_stamp(path, &stamp)?;
                Ok(Revalidated::NotModified)
            }
            Fetched::Body {
                bytes,
                etag,
                last_modified,
            } => Ok(Revalidated::Fresh {
                bytes,
                etag,
                last_modified,
            }),
        }
    }

    /// 无条件抓取（带重试）。
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        match self.fetch(url, None)? {
            Fetched::Body { bytes, .. } => Ok(bytes),
            Fetched::NotModified => Err(Error::Http(format!("unexpected 304 for {url}"))),
        }
    }

    fn fetch(&self, url: &str, stamp: Option<&Stamp>) -> Result<Fetched, Error> {
        if !url.starts_with("https://") {
            return Err(Error::Url(url.to_owned()));
        }
        let mut attempt = 0_u32;
        loop {
            attempt += 1;
            match self.fetch_once(url, stamp) {
                Ok(fetched) => return Ok(fetched),
                Err(error) if attempt < crate::retry::ATTEMPTS && retryable(&error) => {
                    std::thread::sleep(crate::retry::backoff(attempt));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn fetch_once(&self, url: &str, stamp: Option<&Stamp>) -> Result<Fetched, Error> {
        let mut request = self.agent.get(url);
        if let Some(stamp) = stamp {
            if let Some(etag) = &stamp.etag {
                request = request.set("If-None-Match", etag);
            }
            if let Some(modified) = &stamp.last_modified {
                request = request.set("If-Modified-Since", modified);
            }
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::Status(304, _)) => return Ok(Fetched::NotModified),
            Err(ureq::Error::Status(code, _)) => {
                return Err(Error::Status {
                    url: url.to_owned(),
                    code,
                });
            }
            Err(error) => return Err(Error::Http(error.to_string())),
        };
        let etag = response.header("ETag").map(str::to_owned);
        let last_modified = response.header("Last-Modified").map(str::to_owned);
        let mut reader = response.into_reader().take(MAX_BYTES + 1);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::TooLarge);
        }
        Ok(Fetched::Body {
            bytes,
            etag,
            last_modified,
        })
    }
}

enum Fetched {
    NotModified,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

enum Revalidated {
    NotModified,
    Fresh {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

fn ttl() -> Duration {
    match std::env::var(TTL_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        Some(0) => Duration::MAX,
        Some(seconds) => Duration::from_secs(seconds),
        None => DEFAULT_TTL,
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn retryable(error: &Error) -> bool {
    match error {
        Error::Status { code, .. } => crate::retry::retryable_status(*code),
        Error::Http(_) | Error::Checksum { .. } => true,
        Error::Io(source) => crate::retry::retryable_io(source.kind()),
        Error::Cache(_)
        | Error::Json(_)
        | Error::Url(_)
        | Error::MissingVersion(_)
        | Error::MissingIndex(_)
        | Error::TooLarge => false,
    }
}

fn verify(expected: &str, bytes: &[u8], url: &str) -> Result<(), Error> {
    if expected.is_empty() {
        return Ok(());
    }
    let actual = format!("{:x}", Sha1::digest(bytes));
    if expected.eq_ignore_ascii_case(&actual) {
        Ok(())
    } else {
        Err(Error::Checksum {
            url: url.to_owned(),
            expected: expected.to_owned(),
            actual,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, Fetcher, verify};
    use crate::fs::LocalFileSystem;
    use std::fs;

    #[test]
    fn verifies_checksums() {
        let bytes = b"hello";
        assert!(verify("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d", bytes, "x").is_ok());
        assert!(matches!(
            verify("deadbeef", bytes, "x"),
            Err(Error::Checksum { .. })
        ));
        assert!(verify("", bytes, "x").is_ok());
    }

    #[test]
    fn rejects_non_https_urls() {
        let directory =
            std::env::temp_dir().join(format!("arkmcl-net-{}-http", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let fetcher = Fetcher::new(directory, LocalFileSystem);
        assert!(matches!(
            fetcher.get("http://example.com"),
            Err(Error::Url(_))
        ));
    }

    #[test]
    #[ignore = "requires network access"]
    fn fetches_live_fabric_profile() {
        let directory =
            std::env::temp_dir().join(format!("arkmcl-net-{}-fabric", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let fetcher = Fetcher::new(directory.clone(), LocalFileSystem);
        let manifest = fetcher.manifest().expect("manifest");
        let game = manifest.latest.release;
        let loaders = fetcher.fabric_loaders(&game).expect("loaders");
        let loader = loaders
            .iter()
            .find(|loader| loader.stable)
            .or_else(|| loaders.first())
            .expect("loader");
        let profile = fetcher
            .fabric_profile(&game, &loader.version)
            .expect("profile");
        assert_eq!(profile.inherits_from.as_deref(), Some(game.as_str()));
        let base = fetcher.version(&game).expect("base version");
        let merged = arkmcl_core::meta::merge(&base, &profile);
        assert_eq!(merged.client_version.as_deref(), Some(game.as_str()));
        assert_eq!(merged.main_class, profile.main_class);
        assert!(merged.libraries.len() >= base.libraries.len());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    #[ignore = "requires network access"]
    fn fetches_live_metadata() {
        let directory =
            std::env::temp_dir().join(format!("arkmcl-net-{}-live", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let fetcher = Fetcher::new(directory.clone(), LocalFileSystem);
        let manifest = fetcher.manifest().expect("manifest");
        assert!(!manifest.versions.is_empty());
        let version = fetcher.version(&manifest.latest.release).expect("version");
        assert_eq!(version.id, manifest.latest.release);
        let index = fetcher.index(&version).expect("index");
        assert_ne!(index.id, "");
        let _ = fs::remove_dir_all(&directory);
    }
}
