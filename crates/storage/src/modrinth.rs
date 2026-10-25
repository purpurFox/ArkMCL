//! Modrinth API 客户端。
//!
//! 只负责搜索与版本解析；下载由 `download` 模块完成。

use std::io::Read;
use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

const BASE: &str = "https://api.modrinth.com/v2";
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

/// 搜索与版本查询错误。
#[derive(Debug)]
pub enum Error {
    /// 网络 I/O 错误。
    Io(std::io::Error),
    /// HTTP 错误。
    Http(String),
    /// JSON 解析错误。
    Json(serde_json::Error),
    /// 非 HTTPS 地址。
    Url(String),
    /// 响应超过大小上限。
    TooLarge,
    /// 项目没有可下载文件。
    NoFile(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Http(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::Url(url) => write!(formatter, "refusing non-HTTPS URL: {url}"),
            Self::TooLarge => formatter.write_str("response exceeds size limit"),
            Self::NoFile(project) => write!(formatter, "no downloadable file for {project}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Http(_) | Self::Url(_) | Self::TooLarge | Self::NoFile(_) => None,
        }
    }
}

/// 搜索结果条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// 项目 ID。
    pub project_id: String,
    /// URL slug。
    #[serde(default)]
    pub slug: String,
    /// 标题。
    pub title: String,
    /// 简介。
    #[serde(default)]
    pub description: String,
    /// 项目类型。
    #[serde(default)]
    pub project_type: String,
    /// 下载量。
    #[serde(default)]
    pub downloads: u64,
    /// 创建时间（ISO 8601）。
    #[serde(default)]
    pub date_created: String,
    /// 图标地址。
    #[serde(default)]
    pub icon_url: Option<String>,
}

/// 项目元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    /// 项目 ID。
    pub id: String,
    /// URL slug。
    #[serde(default)]
    pub slug: String,
    /// 标题。
    pub title: String,
    /// 简介。
    #[serde(default)]
    pub description: String,
    /// 项目类型。
    #[serde(rename = "project_type", default)]
    pub kind: String,
    /// 下载量。
    #[serde(default)]
    pub downloads: u64,
    /// 关注数。
    #[serde(default)]
    pub followers: u64,
    /// 创建时间。
    #[serde(default)]
    pub date_created: String,
    /// 图标地址。
    #[serde(default)]
    pub icon_url: Option<String>,
}

/// 搜索结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Search {
    /// 命中条目。
    pub hits: Vec<SearchHit>,
    /// 总命中数。
    #[serde(default)]
    pub total_hits: u64,
}

/// 文件哈希。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hashes {
    /// SHA-1。
    #[serde(default)]
    pub sha1: Option<String>,
    /// SHA-512。
    #[serde(default)]
    pub sha512: Option<String>,
}

/// 版本文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionFile {
    /// 下载地址。
    pub url: String,
    /// 文件名。
    pub filename: String,
    /// 是否主文件。
    #[serde(default)]
    pub primary: bool,
    /// 文件大小。
    #[serde(default)]
    pub size: u64,
    /// 哈希。
    #[serde(default)]
    pub hashes: Hashes,
}

/// 版本依赖。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dependency {
    /// 依赖项目 ID。
    #[serde(default)]
    pub project_id: Option<String>,
    /// 指定版本 ID；为空时选择最新兼容版本。
    #[serde(default)]
    pub version_id: Option<String>,
    /// 依赖类型：required、optional、incompatible、embedded。
    #[serde(default)]
    pub dependency_type: String,
}

/// 项目版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    /// 版本 ID。
    pub id: String,
    /// 所属项目 ID。
    #[serde(default)]
    pub project_id: String,
    /// 依赖项。
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    /// 显示名称。
    #[serde(default)]
    pub name: String,
    /// 版本号。
    #[serde(default)]
    pub version_number: String,
    /// 支持的游戏版本。
    #[serde(default)]
    pub game_versions: Vec<String>,
    /// 支持的加载器。
    #[serde(default)]
    pub loaders: Vec<String>,
    /// 发布时间。
    #[serde(default)]
    pub date_published: String,
    /// 文件列表。
    pub files: Vec<VersionFile>,
}

/// Modrinth 客户端。
pub struct Client {
    agent: ureq::Agent,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// 创建客户端。
    #[must_use]
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(TIMEOUT)
            .user_agent(concat!("arkmcl/", env!("CARGO_PKG_VERSION")))
            .build();
        Self { agent }
    }

    /// 搜索项目。
    ///
    /// # Errors
    ///
    /// 网络、HTTP 或 JSON 错误时返回错误。
    pub fn search(
        &self,
        query: &str,
        kind: Option<&str>,
        versions: &[String],
        loaders: &[String],
        index: &str,
        limit: usize,
    ) -> Result<Search, Error> {
        let mut url = format!(
            "{BASE}/search?query={}&limit={}&index={}",
            encode(query),
            limit,
            encode(index)
        );
        let facets = facets(kind, versions, loaders);
        if facets != "[]" {
            url.push_str("&facets=");
            url.push_str(&encode(&facets));
        }
        self.get(&url)
    }

    /// 查询项目元数据。
    ///
    /// # Errors
    ///
    /// 网络、HTTP 或 JSON 错误时返回错误。
    pub fn project(&self, project: &str) -> Result<Project, Error> {
        self.get(&format!("{BASE}/project/{}", encode(project)))
    }

    /// 查询项目版本。
    ///
    /// # Errors
    ///
    /// 网络、HTTP 或 JSON 错误时返回错误。
    pub fn versions(
        &self,
        project: &str,
        versions: &[String],
        loaders: &[String],
    ) -> Result<Vec<Version>, Error> {
        let mut url = format!("{BASE}/project/{}/version", encode(project));
        if !versions.is_empty() {
            url.push_str("?game_versions=");
            url.push_str(&encode(
                &serde_json::to_string(versions).map_err(Error::Json)?,
            ));
        }
        if !loaders.is_empty() {
            url.push_str(if versions.is_empty() { "?" } else { "&" });
            url.push_str("loaders=");
            url.push_str(&encode(
                &serde_json::to_string(loaders).map_err(Error::Json)?,
            ));
        }
        self.get(&url)
    }

    /// 读取指定版本。
    ///
    /// # Errors
    ///
    /// 网络、HTTP 或 JSON 错误时返回错误。
    pub fn version(&self, id: &str) -> Result<Version, Error> {
        self.get(&format!("{BASE}/version/{}", encode(id)))
    }

    /// 选择最新且可下载的版本文件。
    ///
    /// # Errors
    ///
    /// 查询失败或没有文件时返回错误。
    pub fn best(
        &self,
        project: &str,
        versions: &[String],
        loaders: &[String],
    ) -> Result<(Version, VersionFile), Error> {
        let versions = self.versions(project, versions, loaders)?;
        for version in versions {
            let file = version
                .files
                .iter()
                .find(|file| file.primary)
                .or_else(|| version.files.first())
                .cloned();
            if let Some(file) = file {
                return Ok((version, file));
            }
        }
        Err(Error::NoFile(project.to_owned()))
    }

    fn get<T: DeserializeOwned>(&self, url: &str) -> Result<T, Error> {
        if !url.starts_with("https://") {
            return Err(Error::Url(url.to_owned()));
        }
        let response = self
            .agent
            .get(url)
            .call()
            .map_err(|error| Error::Http(error.to_string()))?;
        let mut body = String::new();
        response
            .into_reader()
            .take(MAX_BYTES + 1)
            .read_to_string(&mut body)
            .map_err(Error::Io)?;
        if body.len() as u64 > MAX_BYTES {
            return Err(Error::TooLarge);
        }
        serde_json::from_str(&body).map_err(Error::Json)
    }
}

fn facets(kind: Option<&str>, versions: &[String], loaders: &[String]) -> String {
    let mut groups: Vec<Vec<String>> = Vec::new();
    if let Some(kind) = kind {
        groups.push(vec![format!("project_type:{kind}")]);
    }
    if !versions.is_empty() {
        groups.push(
            versions
                .iter()
                .map(|value| format!("versions:{value}"))
                .collect(),
        );
    }
    if !loaders.is_empty() {
        groups.push(
            loaders
                .iter()
                .map(|value| format!("categories:{value}"))
                .collect(),
        );
    }
    serde_json::to_string(&groups).unwrap_or_else(|_| "[]".to_owned())
}

fn encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(char::from(byte));
            }
            b' ' => output.push_str("%20"),
            other => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                output.push('%');
                output.push(char::from(HEX[usize::from(other >> 4)]));
                output.push(char::from(HEX[usize::from(other & 0x0f)]));
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{encode, facets};

    #[test]
    fn encodes_reserved_characters() {
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("[\"x:y\"]"), "%5B%22x%3Ay%22%5D");
    }

    #[test]
    fn builds_facets_with_or_within_groups() {
        let text = facets(
            Some("mod"),
            &["1.20.4".to_owned(), "1.20.5".to_owned()],
            &["fabric".to_owned()],
        );
        assert!(text.contains("project_type:mod"));
        assert!(text.contains("versions:1.20.4"));
        assert!(text.contains("categories:fabric"));
        assert_eq!(facets(None, &[], &[]), "[]");
    }
}
