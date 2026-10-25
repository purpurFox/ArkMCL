//! 版本元数据缓存。
//!
//! 所有 `ArkMCL` 管理的元数据统一放在 `assets/indexes/`：
//! - `$MINECRAFT_HOME/assets/indexes/version_manifest_v2.json`
//! - `$MINECRAFT_HOME/assets/indexes/versions/<id>/<id>.json`
//! - `$MINECRAFT_HOME/assets/indexes/<index-id>.json`
//! - `$MINECRAFT_HOME/assets/indexes/loaders/fabric/<game>-<loader>.json`
//!
//! 安装器写入的 `versions/<id>/<id>.json` 与旧版清单路径只作为回退读取。

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use arkmcl_core::meta::{AssetIndex, Manifest, Version};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::fs::FileSystem;

/// 缓存错误。
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

/// 缓存条目的校验信息。
///
/// 与缓存文件并排存放为 `<文件>.meta.json`；`fetched_at` 为 Unix 秒。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// 服务器返回的 `ETag`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// 服务器返回的 `Last-Modified`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// 最近一次确认内容有效的时间。
    #[serde(default)]
    pub fetched_at: u64,
}

/// 元数据缓存。
pub struct Cache<F: FileSystem> {
    minecraft: PathBuf,
    fs: F,
}

impl<F: FileSystem> Cache<F> {
    /// 创建缓存。
    #[must_use]
    pub fn new(minecraft: PathBuf, fs: F) -> Self {
        Self { minecraft, fs }
    }

    /// 读取版本清单。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn manifest(&self) -> Result<Option<Manifest>, Error> {
        if let Some(manifest) = self.read(&self.manifest_path())? {
            return Ok(Some(manifest));
        }
        self.read(&self.legacy_manifest_path())
    }

    /// 新位置是否已有版本清单。
    #[must_use]
    pub fn manifest_exists(&self) -> bool {
        self.fs.exists(&self.manifest_path())
    }

    /// 读取旧位置的版本清单，用于一次性迁移。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn legacy_manifest(&self) -> Result<Option<Manifest>, Error> {
        self.read(&self.legacy_manifest_path())
    }

    /// 写入版本清单。
    ///
    /// # Errors
    ///
    /// 文件无法写入时返回错误。
    pub fn save_manifest(&self, manifest: &Manifest) -> Result<(), Error> {
        self.write(&self.manifest_path(), manifest)
    }

    /// 读取指定版本。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn version(&self, id: &str) -> Result<Option<Version>, Error> {
        if let Some(version) = self.read(&self.version_path(id))? {
            return Ok(Some(version));
        }
        self.read(&self.installed_version_path(id))
    }

    /// 写入版本 JSON。
    ///
    /// # Errors
    ///
    /// 文件无法写入时返回错误。
    pub fn save_version(&self, version: &Version) -> Result<(), Error> {
        self.write(&self.version_path(&version.id), version)
    }

    /// 读取资源索引。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn index(&self, id: &str) -> Result<Option<AssetIndex>, Error> {
        self.read(&self.index_path(id))
    }

    /// 写入资源索引。
    ///
    /// # Errors
    ///
    /// 文件无法写入时返回错误。
    pub fn save_index(&self, index: &AssetIndex) -> Result<(), Error> {
        self.write(&self.index_path(&index.id), index)
    }

    /// 读取缓存条目的校验信息；缺失或非法时返回 `None`.
    #[must_use]
    pub fn stamp(&self, path: &Path) -> Option<Stamp> {
        let bytes = self.fs.read(&stamp_path(path)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// 写入缓存条目的校验信息。
    ///
    /// # Errors
    ///
    /// 文件无法写入时返回错误。
    pub fn save_stamp(&self, path: &Path, stamp: &Stamp) -> Result<(), Error> {
        let stamp_path = stamp_path(path);
        let bytes = serde_json::to_vec(stamp).map_err(|source| Error::Json {
            path: stamp_path.clone(),
            source,
        })?;
        self.fs
            .write(&stamp_path, &bytes)
            .map_err(|source| Error::Io {
                path: stamp_path,
                source,
            })
    }

    /// 读取任意缓存 JSON。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn read_json<T: DeserializeOwned>(&self, path: &Path) -> Result<Option<T>, Error> {
        self.read(path)
    }

    /// 写入任意缓存 JSON（原子写）。
    ///
    /// # Errors
    ///
    /// 文件无法写入时返回错误。
    pub fn write_json<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), Error> {
        self.write(path, value)
    }

    /// 读取 Fabric loader profile。
    ///
    /// # Errors
    ///
    /// 文件无法读取或 JSON 非法时返回错误。
    pub fn loader(&self, game: &str, loader: &str) -> Result<Option<Version>, Error> {
        self.read(&self.loader_path(game, loader))
    }

    /// 写入 Fabric loader profile。
    ///
    /// # Errors
    ///
    /// 文件无法写入时返回错误。
    pub fn save_loader(&self, game: &str, loader: &str, version: &Version) -> Result<(), Error> {
        self.write(&self.loader_path(game, loader), version)
    }

    fn indexes_root(&self) -> PathBuf {
        self.minecraft.join("assets").join("indexes")
    }

    /// 版本清单缓存路径。
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.indexes_root().join("version_manifest_v2.json")
    }

    /// 旧版版本清单路径，仅用于迁移读取。
    fn legacy_manifest_path(&self) -> PathBuf {
        self.minecraft
            .join("assets")
            .join("version_manifest_v2.json")
    }

    fn version_path(&self, id: &str) -> PathBuf {
        self.indexes_root()
            .join("versions")
            .join(id)
            .join(format!("{id}.json"))
    }

    /// 安装器写入的版本 JSON；作为回退读取。
    fn installed_version_path(&self, id: &str) -> PathBuf {
        self.minecraft
            .join("versions")
            .join(id)
            .join(format!("{id}.json"))
    }

    fn index_path(&self, id: &str) -> PathBuf {
        self.indexes_root().join(format!("{id}.json"))
    }

    fn loader_path(&self, game: &str, loader: &str) -> PathBuf {
        self.indexes_root()
            .join("loaders")
            .join("fabric")
            .join(format!("{game}-{loader}.json"))
    }

    /// Fabric loader 版本列表缓存路径。
    #[must_use]
    pub fn loader_list_path(&self, game: &str) -> PathBuf {
        self.indexes_root()
            .join("loaders")
            .join("fabric")
            .join(format!("{game}-loaders.json"))
    }

    fn read<T: DeserializeOwned>(&self, path: &Path) -> Result<Option<T>, Error> {
        if !self.fs.exists(path) {
            return Ok(None);
        }
        let bytes = self.fs.read(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| Error::Json {
                path: path.to_path_buf(),
                source,
            })
    }

    fn write<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), Error> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|source| Error::Json {
            path: path.to_path_buf(),
            source,
        })?;
        self.fs.write(path, &bytes).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn stamp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("cache"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".meta.json");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::Cache;
    use crate::fs::LocalFileSystem;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("arkmcl-meta-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    #[test]
    fn round_trips_manifest_version_and_index() {
        let directory = temp_dir("roundtrip");
        let cache = Cache::new(directory.clone(), LocalFileSystem);

        let manifest: arkmcl_core::meta::Manifest = serde_json::from_str(
            r#"{"latest":{"release":"1.20.4","snapshot":"24w01a"},"versions":[]}"#,
        )
        .expect("manifest");
        cache.save_manifest(&manifest).expect("save manifest");
        assert_eq!(
            cache
                .manifest()
                .expect("load manifest")
                .expect("present")
                .latest
                .release,
            "1.20.4"
        );

        let version: arkmcl_core::meta::Version = serde_json::from_str(
            r#"{"id":"1.20.4","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
        )
        .expect("version");
        cache.save_version(&version).expect("save version");
        assert_eq!(
            cache
                .version("1.20.4")
                .expect("load version")
                .expect("present")
                .id,
            "1.20.4"
        );
        assert!(
            directory
                .join("assets/indexes/versions/1.20.4/1.20.4.json")
                .exists()
        );

        let index: arkmcl_core::meta::AssetIndex =
            serde_json::from_str(r#"{"id":"12","sha1":"","size":0,"totalSize":0,"url":""}"#)
                .expect("index");
        cache.save_index(&index).expect("save index");
        assert_eq!(
            cache.index("12").expect("load index").expect("present").id,
            "12"
        );
        assert!(directory.join("assets/indexes/12.json").exists());

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn round_trips_stamp_sidecar() {
        let directory = temp_dir("stamp");
        let cache = Cache::new(directory.clone(), LocalFileSystem);
        let path = directory.join("assets/indexes/version_manifest_v2.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, b"{}").expect("cache file");

        assert!(cache.stamp(&path).is_none());
        let stamp = super::Stamp {
            etag: Some("\"abc\"".to_owned()),
            last_modified: None,
            fetched_at: 42,
        };
        cache.save_stamp(&path, &stamp).expect("save stamp");
        assert_eq!(cache.stamp(&path), Some(stamp));
        assert!(
            directory
                .join("assets/indexes/version_manifest_v2.json.meta.json")
                .exists()
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn falls_back_to_installed_version_json() {
        let directory = temp_dir("installed");
        let cache = Cache::new(directory.clone(), LocalFileSystem);
        let path = directory.join("versions/1.20.4/1.20.4.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(
            &path,
            r#"{"id":"1.20.4","type":"release","mainClass":"net.minecraft.client.main.Main"}"#,
        )
        .expect("version json");

        let version = cache.version("1.20.4").expect("version").expect("present");
        assert_eq!(version.id, "1.20.4");
        assert!(
            !directory
                .join("assets/indexes/versions/1.20.4/1.20.4.json")
                .exists()
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn missing_files_return_none() {
        let directory = temp_dir("missing");
        let cache = Cache::new(directory.clone(), LocalFileSystem);
        assert!(cache.manifest().expect("manifest").is_none());
        assert!(cache.version("nope").expect("version").is_none());
        assert!(cache.index("nope").expect("index").is_none());
        let _ = fs::remove_dir_all(&directory);
    }
}
