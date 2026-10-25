//! `CurseForge` API 客户端。
//!
//! 需要 `CURSEFORGE_API_KEY` 环境变量；只负责搜索、文件列表与下载地址解析，
//! 实际下载由 `download` 模块完成。

use std::fmt::Write as _;
use std::io::Read;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

const BASE: &str = "https://api.curseforge.com/v1";
const MINECRAFT_GAME: u32 = 432;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
const KEY_ENV: &str = "CURSEFORGE_API_KEY";
const ATTEMPTS: u32 = 4;

/// `CurseForge` 错误。
#[derive(Debug)]
pub enum Error {
    /// 未设置 API key。
    MissingKey,
    /// I/O 错误。
    Io(std::io::Error),
    /// HTTP 状态码错误。
    Status {
        /// 状态码。
        code: u16,
    },
    /// 传输错误。
    Http(String),
    /// JSON 解析错误。
    Json(serde_json::Error),
    /// 响应超过大小上限。
    TooLarge,
    /// 项目 ID 不是数字。
    InvalidProject(String),
    /// 文件没有下载地址。
    NoDownloadUrl {
        /// 项目 ID。
        project: u32,
        /// 文件 ID。
        file: u32,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingKey => write!(formatter, "{KEY_ENV} is not set"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Status { code } => write!(formatter, "CurseForge returned HTTP {code}"),
            Self::Http(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::TooLarge => formatter.write_str("response exceeds size limit"),
            Self::InvalidProject(project) => {
                write!(
                    formatter,
                    "CurseForge project id must be numeric: {project}"
                )
            }
            Self::NoDownloadUrl { project, file } => write!(
                formatter,
                "CurseForge file {file} of project {project} has no download URL"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::MissingKey
            | Self::Status { .. }
            | Self::Http(_)
            | Self::TooLarge
            | Self::InvalidProject(_)
            | Self::NoDownloadUrl { .. } => None,
        }
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

/// 搜索结果条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// 项目 ID。
    pub id: u32,
    /// URL slug。
    pub slug: String,
    /// 标题。
    pub title: String,
    /// 简介。
    pub description: String,
    /// 下载量。
    pub downloads: u64,
    /// 创建时间。
    pub date_created: String,
    /// 项目类型。
    pub kind: String,
    /// 图标地址。
    pub icon_url: Option<String>,
}

/// 项目文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// 文件 ID。
    pub id: u32,
    /// 显示名称。
    pub name: String,
    /// 文件名。
    pub filename: String,
    /// 字节数。
    pub size: u64,
    /// SHA-1。
    pub sha1: Option<String>,
    /// 发布时间。
    pub date: String,
    /// 支持的游戏版本。
    pub game_versions: Vec<String>,
    /// 直接下载地址；可能为空。
    pub download_url: Option<String>,
}

/// 搜索排序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// 相关度（不发送排序字段）。
    Relevance,
    /// 最后更新。
    Updated,
    /// 总下载量。
    Downloads,
    /// 名称。
    Name,
}

impl Sort {
    fn field(self) -> Option<u32> {
        match self {
            Self::Relevance => None,
            Self::Updated => Some(3),
            Self::Downloads => Some(6),
            Self::Name => Some(4),
        }
    }
}

/// `CurseForge` 客户端。
pub struct Client {
    agent: ureq::Agent,
    key: String,
}

impl Client {
    /// 创建客户端。
    ///
    /// # Errors
    ///
    /// 未设置 `CURSEFORGE_API_KEY` 时返回 `MissingKey`。
    pub fn new() -> Result<Self, Error> {
        let key = std::env::var(KEY_ENV)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(Error::MissingKey)?;
        let agent = ureq::AgentBuilder::new()
            .timeout(TIMEOUT)
            .user_agent(concat!("arkmcl/", env!("CARGO_PKG_VERSION")))
            .build();
        Ok(Self { agent, key })
    }

    /// 搜索项目，返回条目与总数。
    ///
    /// # Errors
    ///
    /// 网络、HTTP 或 JSON 错误时返回错误。
    #[allow(clippy::too_many_arguments)] // API 查询参数较多，保持扁平调用
    pub fn search(
        &self,
        query: &str,
        kind: Option<&str>,
        game_version: Option<&str>,
        loader: Option<&str>,
        sort: Sort,
        order: &str,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Hit>, u64), Error> {
        let mut url = format!("{BASE}/mods/search?gameId={MINECRAFT_GAME}");
        if let Some(class_id) = kind.and_then(class_id) {
            let _ = write!(url, "&classId={class_id}");
        }
        if !query.is_empty() {
            let _ = write!(url, "&searchFilter={}", encode(query));
        }
        if let Some(game_version) = game_version.filter(|value| !value.is_empty()) {
            let _ = write!(url, "&gameVersion={}", encode(game_version));
        }
        if let Some(loader) = loader.and_then(loader_type) {
            let _ = write!(url, "&modLoaderType={loader}");
        }
        if let Some(field) = sort.field() {
            let _ = write!(url, "&sortField={field}");
        }
        let _ = write!(
            url,
            "&sortOrder={}&pageSize={}&index={offset}",
            if order == "asc" { "asc" } else { "desc" },
            limit.clamp(1, 50)
        );

        let response: SearchResponse = self.get(&url)?;
        Ok((
            response.data.into_iter().map(Into::into).collect(),
            response.pagination.total_count,
        ))
    }

    /// 读取项目元数据。
    ///
    /// # Errors
    ///
    /// 网络、HTTP 或 JSON 错误时返回错误。
    pub fn project(&self, project: u32) -> Result<Hit, Error> {
        let response: Envelope<ModEntry> = self.get(&format!("{BASE}/mods/{project}"))?;
        Ok(response.data.into())
    }

    /// 列出项目文件。
    ///
    /// # Errors
    ///
    /// 网络、HTTP 或 JSON 错误时返回错误。
    pub fn files(
        &self,
        project: u32,
        game_version: Option<&str>,
        loader: Option<&str>,
    ) -> Result<Vec<File>, Error> {
        let mut url = format!("{BASE}/mods/{project}/files?pageSize=50");
        if let Some(game_version) = game_version.filter(|value| !value.is_empty()) {
            let _ = write!(url, "&gameVersion={}", encode(game_version));
        }
        if let Some(loader) = loader.and_then(loader_type) {
            let _ = write!(url, "&modLoaderType={loader}");
        }
        let response: Envelope<Vec<FileEntry>> = self.get(&url)?;
        Ok(response.data.into_iter().map(Into::into).collect())
    }

    /// 读取单个文件。
    ///
    /// # Errors
    ///
    /// 网络、HTTP 或 JSON 错误时返回错误。
    pub fn file(&self, project: u32, file: u32) -> Result<File, Error> {
        let response: Envelope<FileEntry> =
            self.get(&format!("{BASE}/mods/{project}/files/{file}"))?;
        Ok(response.data.into())
    }

    /// 解析下载地址；文件未直接提供地址时使用专用端点。
    ///
    /// # Errors
    ///
    /// 网络、HTTP、JSON 错误或没有可用地址时返回错误。
    pub fn download_url(&self, project: u32, file: u32) -> Result<String, Error> {
        let response: Envelope<Option<String>> =
            self.get(&format!("{BASE}/mods/{project}/files/{file}/download-url"))?;
        response
            .data
            .filter(|url| !url.is_empty())
            .ok_or(Error::NoDownloadUrl { project, file })
    }

    fn get<T: DeserializeOwned>(&self, url: &str) -> Result<T, Error> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.get_once(url) {
                Ok(value) => return Ok(value),
                Err(error) if attempt < ATTEMPTS && retryable(&error) => {
                    std::thread::sleep(crate::retry::backoff(attempt));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn get_once<T: DeserializeOwned>(&self, url: &str) -> Result<T, Error> {
        if !url.starts_with("https://") {
            return Err(Error::Http(format!("refusing non-HTTPS URL: {url}")));
        }
        let response = match self
            .agent
            .get(url)
            .set("x-api-key", &self.key)
            .set("Accept", "application/json")
            .call()
        {
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
}

/// 资源类型 → `CurseForge` classId。
#[must_use]
pub fn class_id(kind: &str) -> Option<u32> {
    match kind {
        "mod" => Some(6),
        "plugin" => Some(5),
        "resourcepack" => Some(12),
        "world" => Some(17),
        "modpack" => Some(4471),
        "shader" => Some(6552),
        "datapack" => Some(6945),
        _ => None,
    }
}

/// 加载器名称 → `CurseForge` modLoaderType。
#[must_use]
pub fn loader_type(loader: &str) -> Option<u32> {
    match loader.to_ascii_lowercase().as_str() {
        "forge" => Some(1),
        "fabric" => Some(4),
        "quilt" => Some(5),
        "neoforge" => Some(6),
        _ => None,
    }
}

fn kind_from_class(class_id: Option<u32>) -> String {
    match class_id {
        Some(6) => "mod",
        Some(5) => "plugin",
        Some(12) => "resourcepack",
        Some(17) => "world",
        Some(4471) => "modpack",
        Some(6552) => "shader",
        Some(6945) => "datapack",
        _ => "",
    }
    .to_owned()
}

fn retryable(error: &Error) -> bool {
    match error {
        Error::Status { code } => crate::retry::retryable_status(*code),
        Error::Http(_) => true,
        Error::Io(source) => crate::retry::retryable_io(source.kind()),
        Error::MissingKey
        | Error::Json(_)
        | Error::TooLarge
        | Error::InvalidProject(_)
        | Error::NoDownloadUrl { .. } => false,
    }
}

fn encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct SearchResponse {
    data: Vec<ModEntry>,
    #[serde(default)]
    pagination: Pagination,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Pagination {
    #[serde(default)]
    total_count: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModEntry {
    id: u32,
    name: String,
    #[serde(default)]
    slug: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    download_count: f64,
    #[serde(default)]
    date_created: String,
    #[serde(default)]
    class_id: Option<u32>,
    #[serde(default)]
    logo: Option<Logo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Logo {
    #[serde(default)]
    thumbnail_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileEntry {
    id: u32,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    file_length: u64,
    #[serde(default)]
    file_date: String,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    hashes: Vec<HashEntry>,
    #[serde(default)]
    download_url: Option<String>,
}

#[derive(Deserialize)]
struct HashEntry {
    value: String,
    algo: u32,
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 下载量为非负有限值
impl From<ModEntry> for Hit {
    fn from(entry: ModEntry) -> Self {
        Self {
            id: entry.id,
            slug: entry.slug,
            title: entry.name,
            description: entry.summary,
            downloads: entry.download_count.max(0.0) as u64,
            date_created: entry.date_created,
            kind: kind_from_class(entry.class_id),
            icon_url: entry.logo.and_then(|logo| logo.thumbnail_url),
        }
    }
}

impl From<FileEntry> for File {
    fn from(entry: FileEntry) -> Self {
        let sha1 = entry
            .hashes
            .iter()
            .find(|hash| hash.algo == 1)
            .map(|hash| hash.value.clone());
        Self {
            id: entry.id,
            name: entry.display_name,
            filename: entry.file_name,
            size: entry.file_length,
            sha1,
            date: entry.file_date,
            game_versions: entry.game_versions,
            download_url: entry.download_url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{File, Hit, SearchResponse, class_id, kind_from_class, loader_type};

    #[test]
    fn maps_classes_and_loaders() {
        assert_eq!(class_id("mod"), Some(6));
        assert_eq!(class_id("modpack"), Some(4471));
        assert_eq!(class_id("plugin"), Some(5));
        assert_eq!(class_id("unknown"), None);
        assert_eq!(loader_type("fabric"), Some(4));
        assert_eq!(loader_type("NeoForge"), Some(6));
        assert_eq!(loader_type("paper"), None);
        assert_eq!(kind_from_class(Some(6945)), "datapack");
    }

    #[test]
    fn parses_search_response() {
        let response: SearchResponse = serde_json::from_str(
            r#"{
              "data": [
                {"id": 238222, "name": "JEI", "slug": "jei", "summary": "Item viewer", "downloadCount": 12345.0, "dateCreated": "2020-01-01T00:00:00Z", "classId": 6, "logo": {"thumbnailUrl": "https://example.com/icon.png"}}
              ],
              "pagination": {"totalCount": 42}
            }"#,
        )
        .expect("search");
        assert_eq!(response.pagination.total_count, 42);
        let hit: Hit = response.data.into_iter().next().expect("hit").into();
        assert_eq!(hit.id, 238_222);
        assert_eq!(hit.title, "JEI");
        assert_eq!(hit.kind, "mod");
        assert_eq!(hit.downloads, 12_345);
        assert_eq!(
            hit.icon_url.as_deref(),
            Some("https://example.com/icon.png")
        );
    }

    #[test]
    fn parses_file_entries() {
        let entry: super::FileEntry = serde_json::from_str(
            r#"{
              "id": 123, "displayName": "JEI 1.0", "fileName": "jei-1.0.jar",
              "fileLength": 1000, "fileDate": "2024-01-01T00:00:00Z",
              "gameVersions": ["1.20.4"],
              "hashes": [{"value": "abc", "algo": 1}, {"value": "def", "algo": 2}],
              "downloadUrl": null
            }"#,
        )
        .expect("file");
        let file: File = entry.into();
        assert_eq!(file.id, 123);
        assert_eq!(file.filename, "jei-1.0.jar");
        assert_eq!(file.sha1.as_deref(), Some("abc"));
        assert_eq!(file.size, 1000);
        assert!(file.download_url.is_none());
    }
}
