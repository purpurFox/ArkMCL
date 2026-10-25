//! mrpack（Modrinth 整合包）读取与覆盖文件解压。

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
const MAX_OVERRIDE_FILES: usize = 10_000;
const MAX_OVERRIDE_BYTES: u64 = 256 * 1024 * 1024;

/// 一条待下载文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// 实例内相对路径。
    pub path: PathBuf,
    /// 下载地址。
    pub url: String,
    /// SHA-1。
    pub sha1: String,
    /// 字节数。
    pub size: u64,
    /// 客户端是否需要。
    pub client: bool,
    /// 服务端是否需要。
    pub server: bool,
}

/// 解析后的整合包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    /// 名称。
    pub name: String,
    /// 包版本号。
    pub version_id: String,
    /// Minecraft 版本。
    pub minecraft: String,
    /// 加载器：种类与版本。
    pub loader: Option<(String, String)>,
    /// 文件列表。
    pub files: Vec<File>,
}

/// mrpack 错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io(io::Error),
    /// ZIP 错误。
    Zip(zip::result::ZipError),
    /// JSON 错误。
    Json(serde_json::Error),
    /// 缺少 `modrinth.index.json`。
    MissingIndex,
    /// 缺少 Minecraft 依赖。
    MissingMinecraft,
    /// 非法文件路径。
    InvalidPath(String),
    /// 文件没有下载地址。
    MissingUrl(String),
    /// 不支持的 `formatVersion`。
    UnsupportedFormat(u32),
    /// 内容超过限制。
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Zip(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::MissingIndex => formatter.write_str("mrpack has no modrinth.index.json"),
            Self::MissingMinecraft => formatter.write_str("mrpack has no minecraft dependency"),
            Self::InvalidPath(path) => write!(formatter, "invalid mrpack path: {path}"),
            Self::MissingUrl(path) => write!(formatter, "mrpack file has no download URL: {path}"),
            Self::UnsupportedFormat(version) => {
                write!(formatter, "unsupported mrpack formatVersion: {version}")
            }
            Self::TooLarge => formatter.write_str("mrpack content exceeds limits"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Zip(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::MissingIndex
            | Self::MissingMinecraft
            | Self::InvalidPath(_)
            | Self::MissingUrl(_)
            | Self::UnsupportedFormat(_)
            | Self::TooLarge => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Zip(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// 读取整合包索引。
///
/// # Errors
///
/// 文件无法读取、ZIP 或 JSON 非法、路径非法时返回错误。
pub fn read(path: &Path) -> Result<Pack, Error> {
    let file = fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut index = String::new();
    archive
        .by_name("modrinth.index.json")
        .map_err(|_| Error::MissingIndex)?
        .take(MAX_INDEX_BYTES + 1)
        .read_to_string(&mut index)?;
    if index.len() as u64 > MAX_INDEX_BYTES {
        return Err(Error::TooLarge);
    }
    let raw: RawIndex = serde_json::from_str(&index)?;
    if raw.format_version != 1 {
        return Err(Error::UnsupportedFormat(raw.format_version));
    }
    let Some(minecraft) = raw.dependencies.get("minecraft").cloned() else {
        return Err(Error::MissingMinecraft);
    };
    let loader = ["fabric-loader", "quilt-loader", "forge", "neoforge"]
        .iter()
        .find_map(|key| {
            raw.dependencies
                .get(*key)
                .map(|version| (loader_kind(key), version.clone()))
        });
    let mut files = Vec::new();
    for file in raw.files {
        let Some(path) = safe_relative(&file.path) else {
            return Err(Error::InvalidPath(file.path));
        };
        let Some(url) = file
            .downloads
            .into_iter()
            .next()
            .filter(|url| !url.is_empty())
        else {
            return Err(Error::MissingUrl(file.path));
        };
        let (client, server) = file.env.map_or((true, true), |env| {
            (
                env.client.as_deref() != Some("unsupported"),
                env.server.as_deref() != Some("unsupported"),
            )
        });
        files.push(File {
            path,
            url,
            sha1: file.hashes.sha1,
            size: file.file_size,
            client,
            server,
        });
    }
    Ok(Pack {
        name: raw.name,
        version_id: raw.version_id,
        minecraft,
        loader,
        files,
    })
}

/// 解压 `overrides/` 与对应端的覆盖目录。
///
/// # Errors
///
/// ZIP 或 I/O 错误、路径非法时返回错误。
pub fn extract_overrides(archive_path: &Path, target: &Path, client: bool) -> Result<usize, Error> {
    let file = fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut count = 0_usize;
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let name = name.to_string_lossy().replace('\\', "/");
        let relative = if let Some(relative) = name.strip_prefix("overrides/") {
            relative
        } else if client {
            match name.strip_prefix("client-overrides/") {
                Some(relative) => relative,
                None => continue,
            }
        } else {
            match name.strip_prefix("server-overrides/") {
                Some(relative) => relative,
                None => continue,
            }
        };
        if relative.is_empty() {
            continue;
        }
        let destination = target.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
            continue;
        }
        total = total.saturating_add(entry.size());
        if count >= MAX_OVERRIDE_FILES || total > MAX_OVERRIDE_BYTES {
            return Err(Error::TooLarge);
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = fs::File::create(&destination)?;
        let written = io::copy(&mut entry, &mut output)?;
        total = total.saturating_add(written);
        if total > MAX_OVERRIDE_BYTES {
            return Err(Error::TooLarge);
        }
        count += 1;
    }
    Ok(count)
}

fn loader_kind(key: &str) -> String {
    match key {
        "fabric-loader" => "fabric",
        "quilt-loader" => "quilt",
        "forge" => "forge",
        "neoforge" => "neoforge",
        other => other,
    }
    .to_owned()
}

fn safe_relative(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return None;
    }
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if relative.as_os_str().is_empty() {
        None
    } else {
        Some(relative)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIndex {
    #[serde(default = "default_format_version")]
    format_version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version_id: String,
    #[serde(default)]
    files: Vec<RawFile>,
    #[serde(default)]
    dependencies: HashMap<String, String>,
}

fn default_format_version() -> u32 {
    1
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    path: String,
    #[serde(default)]
    hashes: RawHashes,
    #[serde(default)]
    env: Option<RawEnv>,
    #[serde(default)]
    downloads: Vec<String>,
    #[serde(default)]
    file_size: u64,
}

#[derive(Deserialize, Default)]
struct RawHashes {
    #[serde(default)]
    sha1: String,
}

#[derive(Deserialize)]
struct RawEnv {
    #[serde(default)]
    client: Option<String>,
    #[serde(default)]
    server: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{Error, extract_overrides, read};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("arkmcl-mrpack-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn write_pack(path: &PathBuf, index: &str) {
        let file = fs::File::create(path).expect("pack");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file("modrinth.index.json", options)
            .expect("index");
        writer.write_all(index.as_bytes()).expect("write index");
        writer
            .start_file("overrides/config/example.txt", options)
            .expect("override");
        writer.write_all(b"hello").expect("write override");
        writer.finish().expect("finish");
    }

    #[test]
    fn reads_index_and_overrides() {
        let directory = temp_dir("read");
        let pack_path = directory.join("pack.mrpack");
        write_pack(
            &pack_path,
            r#"{
              "formatVersion": 1,
              "game": "minecraft",
              "name": "Test Pack",
              "versionId": "1.0.0",
              "files": [
                {"path": "mods/example.jar", "hashes": {"sha1": "abc"}, "downloads": ["https://example.com/example.jar"], "fileSize": 10}
              ],
              "dependencies": {"minecraft": "1.20.4", "fabric-loader": "0.15.0"}
            }"#,
        );

        let pack = read(&pack_path).expect("read");
        assert_eq!(pack.name, "Test Pack");
        assert_eq!(pack.minecraft, "1.20.4");
        assert_eq!(
            pack.loader,
            Some(("fabric".to_owned(), "0.15.0".to_owned()))
        );
        assert_eq!(pack.files.len(), 1);
        assert_eq!(pack.files[0].path, PathBuf::from("mods/example.jar"));
        assert_eq!(pack.files[0].sha1, "abc");

        let target = directory.join("instance");
        fs::create_dir_all(&target).expect("target");
        let count = extract_overrides(&pack_path, &target, true).expect("overrides");
        assert_eq!(count, 1);
        assert_eq!(
            fs::read_to_string(target.join("config/example.txt")).expect("override"),
            "hello"
        );

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn rejects_unknown_format_version() {
        let directory = temp_dir("format");
        let pack_path = directory.join("pack.mrpack");
        write_pack(
            &pack_path,
            r#"{"formatVersion": 2, "name": "Bad", "files": [], "dependencies": {"minecraft": "1.20.4"}}"#,
        );
        assert!(matches!(read(&pack_path), Err(Error::UnsupportedFormat(2))));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn rejects_unsafe_paths() {
        let directory = temp_dir("unsafe");
        let pack_path = directory.join("pack.mrpack");
        write_pack(
            &pack_path,
            r#"{
              "name": "Bad",
              "files": [{"path": "../escape.jar", "downloads": ["https://example.com/x"], "hashes": {}}],
              "dependencies": {"minecraft": "1.20.4"}
            }"#,
        );
        assert!(matches!(read(&pack_path), Err(Error::InvalidPath(_))));
        let _ = fs::remove_dir_all(&directory);
    }
}
