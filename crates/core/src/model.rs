//! 领域模型：实例、配置与组件。

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 实例标识。
///
/// 允许 ASCII 可见字符，但禁止通配符、路径分隔符、平台保留字符与空白；
/// 不得为 `.` 或 `..`，且最长 64 字符。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(String);

/// 实例标识错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// 空标识。
    Empty,
    /// 超过长度上限。
    TooLong,
    /// 含非法字符。
    Invalid(char),
    /// 保留名称。
    Reserved,
}

impl fmt::Display for IdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("instance id is empty"),
            Self::TooLong => formatter.write_str("instance id is longer than 64 characters"),
            Self::Invalid(character) => {
                write!(
                    formatter,
                    "instance id contains invalid character '{character}'"
                )
            }
            Self::Reserved => formatter.write_str("instance id is reserved"),
        }
    }
}

impl std::error::Error for IdError {}

impl Id {
    /// 校验并构造标识。
    ///
    /// # Errors
    ///
    /// 为空、过长、为保留名称或包含非法字符时返回错误。
    pub fn new(value: impl Into<String>) -> Result<Self, IdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IdError::Empty);
        }
        if value.len() > 64 {
            return Err(IdError::TooLong);
        }
        if value == "." || value == ".." {
            return Err(IdError::Reserved);
        }
        if let Some(character) = value.chars().find(|character| {
            !character.is_ascii()
                || character.is_ascii_control()
                || character.is_ascii_whitespace()
                || matches!(
                    character,
                    '*' | '?' | '/' | '\\' | ':' | '"' | '<' | '>' | '|'
                )
        }) {
            return Err(IdError::Invalid(character));
        }
        Ok(Self(value))
    }

    /// 从名称生成候选标识；非 ASCII 字符折叠为连字符。
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        let mut slug = String::new();
        for character in name.chars() {
            if character.is_ascii_alphanumeric() {
                slug.push(character.to_ascii_lowercase());
            } else if !slug.ends_with('-') {
                slug.push('-');
            }
        }
        let trimmed = slug.trim_matches('-');
        let base = if trimmed.is_empty() {
            "instance"
        } else {
            trimmed
        };
        let truncated: String = base.chars().take(48).collect();
        Self(truncated.trim_end_matches('-').to_owned())
    }

    /// 字符串形式。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 默认 JVM 参数（Aikar's flags）。
pub const DEFAULT_JVM_ARGS: [&str; 20] = [
    "-XX:+AlwaysPreTouch",
    "-XX:+DisableExplicitGC",
    "-XX:+ParallelRefProcEnabled",
    "-XX:+PerfDisableSharedMem",
    "-XX:+UnlockExperimentalVMOptions",
    "-XX:+UseG1GC",
    "-XX:G1HeapRegionSize=8M",
    "-XX:G1HeapWastePercent=5",
    "-XX:G1MaxNewSizePercent=40",
    "-XX:G1MixedGCCountTarget=4",
    "-XX:G1MixedGCLiveThresholdPercent=90",
    "-XX:G1NewSizePercent=30",
    "-XX:G1RSetUpdatingPauseTimePercent=5",
    "-XX:G1ReservePercent=20",
    "-XX:InitiatingHeapOccupancyPercent=15",
    "-XX:MaxGCPauseMillis=200",
    "-XX:MaxTenuringThreshold=1",
    "-XX:SurvivorRatio=32",
    "-Dusing.aikars.flags=https://mcflags.emc.gs",
    "-Daikars.new.flags=true",
];

/// 实例配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// 最大堆内存（MiB）。
    pub memory_mb: u32,
    /// 覆盖全局 Java 路径（JDK 根目录或 java 可执行文件）。
    pub java_path: Option<PathBuf>,
    /// 指定 Java 主版本；为空时使用游戏版本建议的版本。
    #[serde(default)]
    pub java_version: Option<u32>,
    /// 覆盖 LWJGL 版本；为空时使用版本 JSON 中的默认值。
    #[serde(default)]
    pub lwjgl: Option<String>,
    /// 额外 JVM 参数。
    pub jvm_args: Vec<String>,
    /// 额外游戏参数。
    pub game_args: Vec<String>,
    /// 窗口宽度。
    pub window_width: u32,
    /// 是否全屏。
    pub fullscreen: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            memory_mb: 4096,
            java_path: None,
            java_version: None,
            lwjgl: None,
            jvm_args: DEFAULT_JVM_ARGS
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
            game_args: Vec::new(),
            window_width: 854,
            fullscreen: false,
        }
    }
}

/// 配置错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// 内存低于 512 MiB。
    MemoryTooSmall,
    /// 窗口宽度低于 320。
    WindowTooSmall,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryTooSmall => formatter.write_str("memory_mb is below the required minimum"),
            Self::WindowTooSmall => formatter.write_str("window_width must be at least 320"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// 校验配置。
    ///
    /// # Errors
    ///
    /// 内存或窗口宽度越界时返回错误。
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.validate_for(Kind::Client)
    }

    /// 按实例类型校验：客户端至少 512 MiB，服务端至少 256 MiB。
    ///
    /// # Errors
    ///
    /// 内存或窗口宽度低于下限时返回错误。
    pub fn validate_for(&self, kind: Kind) -> Result<(), ConfigError> {
        let minimum = if kind == Kind::Server { 256 } else { 512 };
        if self.memory_mb < minimum {
            return Err(ConfigError::MemoryTooSmall);
        }
        if self.window_width < 320 {
            return Err(ConfigError::WindowTooSmall);
        }
        Ok(())
    }
}

/// 实例类型。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// 客户端。
    #[default]
    Client,
    /// 服务端。
    Server,
}

/// 实例状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// 尚未安装完整。
    #[default]
    NotInstalled,
    /// 就绪。
    Ready,
    /// 运行中。
    Running,
    /// 损坏，需要修复。
    Corrupted,
}

/// 组件引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    /// 组件类型标识，例如 `net.minecraft`。
    pub uid: String,
    /// 组件版本。
    pub version: String,
}

/// 原版组件 id。
pub const MINECRAFT: &str = "net.minecraft";

/// Fabric loader 组件 id。
pub const FABRIC: &str = "net.fabricmc.fabric-loader";

/// Forge 组件 id。
pub const FORGE: &str = "net.minecraftforge";

/// `NeoForge` 组件 id。
pub const NEOFORGE: &str = "net.neoforged";

/// Purpur 服务端组件 id。
pub const PURPUR: &str = "org.purpurmc.purpur";

/// 组件链。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// 有序组件列表。
    pub components: Vec<Component>,
}

impl Profile {
    /// 原版组件。
    #[must_use]
    pub fn minecraft(&self) -> Option<&Component> {
        self.components
            .iter()
            .find(|component| component.uid == MINECRAFT)
    }

    /// Fabric 组件。
    #[must_use]
    pub fn fabric(&self) -> Option<&Component> {
        self.components
            .iter()
            .find(|component| component.uid == FABRIC)
    }

    /// 设置原版版本。
    pub fn set_minecraft(&mut self, version: &str) {
        self.set(MINECRAFT, version);
    }

    /// 设置 Fabric loader 版本。
    pub fn set_fabric(&mut self, version: &str) {
        self.set(FABRIC, version);
    }

    /// 设置 Forge 版本。
    pub fn set_forge(&mut self, version: &str) {
        self.set(FORGE, version);
    }

    /// 设置 `NeoForge` 版本。
    pub fn set_neoforge(&mut self, version: &str) {
        self.set(NEOFORGE, version);
    }

    /// Purpur 组件。
    #[must_use]
    pub fn purpur(&self) -> Option<&Component> {
        self.components
            .iter()
            .find(|component| component.uid == PURPUR)
    }

    /// 设置 Purpur 构建号。
    pub fn set_purpur(&mut self, version: &str) {
        self.set(PURPUR, version);
    }

    /// 移除指定组件。
    pub fn remove(&mut self, uid: &str) {
        self.components.retain(|component| component.uid != uid);
    }

    fn set(&mut self, uid: &str, version: &str) {
        if let Some(component) = self
            .components
            .iter_mut()
            .find(|component| component.uid == uid)
        {
            version.clone_into(&mut component.version);
        } else {
            self.components.push(Component {
                uid: uid.to_owned(),
                version: version.to_owned(),
            });
        }
    }
}

/// 一个 Minecraft 实例。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    /// 实例标识。
    pub id: Id,
    /// 实例类型。
    #[serde(default)]
    pub kind: Kind,
    /// 显示名称。
    pub name: String,
    /// 所属分组。
    pub group: Option<String>,
    /// 实例使用的账户；为空时使用默认账户。
    #[serde(default)]
    pub account: Option<String>,
    /// 实例根目录；由仓储设置，不持久化。
    #[serde(skip)]
    pub root: PathBuf,
    /// 实例配置。
    pub config: Config,
    /// 组件链。
    pub profile: Profile,
    /// 当前状态。
    pub state: State,
    /// 创建时间（Unix 秒）。
    #[serde(default)]
    pub created_at: u64,
    /// 最近启动时间（Unix 秒）。
    #[serde(default)]
    pub last_launched_at: Option<u64>,
    /// 累计运行秒数。
    #[serde(default)]
    pub run_seconds: u64,
    /// 最近退出码。
    #[serde(default)]
    pub last_exit_code: Option<i32>,
    /// 最近一次是否崩溃。
    #[serde(default)]
    pub last_crashed: bool,
    /// 最近崩溃报告文件名。
    #[serde(default)]
    pub last_crash_report: Option<String>,
}

/// 创建实例的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewInstance {
    /// 指定标识；为空时自动生成。
    pub id: Option<Id>,
    /// 实例类型。
    pub kind: Kind,
    /// 显示名称。
    pub name: String,
    /// 所属分组。
    pub group: Option<String>,
    /// 指定账户。
    pub account: Option<String>,
    /// 实例配置。
    pub config: Config,
    /// 组件链。
    pub profile: Profile,
}

impl NewInstance {
    /// 用名称和默认配置构造。
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: None,
            kind: Kind::Client,
            name: name.into(),
            group: None,
            account: None,
            config: Config::default(),
            profile: Profile::default(),
        }
    }

    /// 用指定标识和默认配置构造。
    #[must_use]
    pub fn with_id(id: Id) -> Self {
        let name = id.as_str().to_owned();
        Self {
            id: Some(id),
            kind: Kind::Client,
            name,
            group: None,
            account: None,
            config: Config::default(),
            profile: Profile::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigError, Id, IdError};

    #[test]
    fn validates_ids() {
        assert!(Id::new("my-instance-1").is_ok());
        assert!(Id::new("test-1.17.1").is_ok());
        assert!(Id::new("Upper").is_ok());
        assert_eq!(Id::new(""), Err(IdError::Empty));
        assert_eq!(Id::new("a".repeat(65)), Err(IdError::TooLong));
        assert_eq!(Id::new("你好"), Err(IdError::Invalid('你')));
        assert_eq!(Id::new("a*b"), Err(IdError::Invalid('*')));
        assert_eq!(Id::new("a/b"), Err(IdError::Invalid('/')));
        assert_eq!(Id::new("a b"), Err(IdError::Invalid(' ')));
        assert_eq!(Id::new("."), Err(IdError::Reserved));
        assert_eq!(Id::new(".."), Err(IdError::Reserved));
    }

    #[test]
    fn slugifies_names() {
        assert_eq!(Id::from_name("My Instance").as_str(), "my-instance");
        assert_eq!(Id::from_name("你好").as_str(), "instance");
        assert_eq!(Id::from_name("  ").as_str(), "instance");
    }

    #[test]
    fn profile_tracks_minecraft_and_fabric() {
        let mut profile = super::Profile::default();
        profile.set_minecraft("1.20.4");
        profile.set_fabric("0.15.11");
        profile.set_fabric("0.15.12");
        assert_eq!(profile.minecraft().expect("minecraft").version, "1.20.4");
        assert_eq!(profile.fabric().expect("fabric").version, "0.15.12");
        assert_eq!(profile.components.len(), 2);
    }

    #[test]
    fn validates_config() {
        let mut config = Config::default();
        assert!(config.validate().is_ok());
        config.memory_mb = 256;
        assert_eq!(config.validate(), Err(ConfigError::MemoryTooSmall));
        config.memory_mb = 2048;
        config.window_width = 100;
        assert_eq!(config.validate(), Err(ConfigError::WindowTooSmall));
    }
}
