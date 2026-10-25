//! `Minecraft` 版本元数据模型。
//!
//! 仅覆盖启动计划需要的字段；未知字段会被忽略。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 版本清单。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    /// 最新版本。
    pub latest: Latest,
    /// 全部版本。
    #[serde(default)]
    pub versions: Vec<Summary>,
}

/// 最新版本引用。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Latest {
    /// 最新正式版。
    pub release: String,
    /// 最新快照。
    pub snapshot: String,
}

/// 清单中的版本摘要。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    /// 版本 id。
    pub id: String,
    /// 版本类型。
    #[serde(rename = "type")]
    pub kind: String,
    /// 版本 JSON 地址。
    pub url: String,
    /// 校验和。
    #[serde(default)]
    pub sha1: String,
    /// 字节数。
    #[serde(default)]
    pub size: u64,
    /// 发布时间。
    #[serde(default)]
    pub release_time: String,
    /// 最后更新时间。
    #[serde(default)]
    pub time: String,
}

/// 单个版本元数据。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    /// 版本 id。
    pub id: String,
    /// 版本类型。
    #[serde(rename = "type", default)]
    pub kind: String,
    /// 启动主类。
    pub main_class: String,
    /// 继承的版本 id。
    #[serde(default)]
    pub inherits_from: Option<String>,
    /// 客户端 jar 所属版本；合并 loader 后仍指向基础版本。
    #[serde(default, skip_serializing)]
    pub client_version: Option<String>,
    /// 资源索引 id。
    #[serde(default)]
    pub assets: Option<String>,
    /// 资源索引。
    #[serde(default)]
    pub asset_index: Option<AssetIndex>,
    /// 下载信息。
    #[serde(default)]
    pub downloads: Option<Downloads>,
    /// Java 版本要求。
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    /// 依赖库。
    #[serde(default)]
    pub libraries: Vec<Library>,
    /// 结构化参数。
    #[serde(default)]
    pub arguments: Option<Arguments>,
    /// 旧版参数字符串。
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
}

/// 资源索引。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndex {
    /// 索引 id；下载的索引文件本身不包含该字段。
    #[serde(default)]
    pub id: String,
    /// 校验和。
    #[serde(default)]
    pub sha1: String,
    /// 字节数。
    #[serde(default)]
    pub size: u64,
    /// 解压后总大小。
    #[serde(default)]
    pub total_size: u64,
    /// 下载地址。
    #[serde(default)]
    pub url: String,
    /// 资源对象表。
    #[serde(default)]
    pub objects: HashMap<String, AssetObject>,
    /// 是否为 virtual 资源布局。
    #[serde(rename = "virtual", default)]
    pub is_virtual: bool,
    /// 是否映射到 resources 目录。
    #[serde(default)]
    pub map_to_resources: bool,
}

/// 资源对象。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssetObject {
    /// 内容哈希。
    pub hash: String,
    /// 字节数。
    #[serde(default)]
    pub size: u64,
}

/// 下载信息。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Downloads {
    /// 客户端 jar。
    #[serde(default)]
    pub client: Option<Artifact>,
    /// 服务端 jar。
    #[serde(default)]
    pub server: Option<Artifact>,
}

/// 可下载文件。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Artifact {
    /// 相对路径。
    #[serde(default)]
    pub path: Option<String>,
    /// 校验和。
    #[serde(default)]
    pub sha1: String,
    /// 字节数。
    #[serde(default)]
    pub size: u64,
    /// 下载地址。
    #[serde(default)]
    pub url: String,
}

/// Java 版本要求。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    /// 组件名。
    #[serde(default)]
    pub component: String,
    /// 主版本。
    pub major_version: u32,
}

/// 依赖库。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Library {
    /// Maven 坐标。
    pub name: String,
    /// Maven 基础地址。
    #[serde(default)]
    pub url: Option<String>,
    /// 下载信息。
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    /// 本地库分类器。
    #[serde(default)]
    pub natives: Option<HashMap<String, String>>,
    /// 平台规则。
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// 解压规则。
    #[serde(default)]
    pub extract: Option<Extract>,
}

/// 库下载信息。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LibraryDownloads {
    /// 主 artifact。
    #[serde(default)]
    pub artifact: Option<Artifact>,
    /// 分类器 artifact。
    #[serde(default)]
    pub classifiers: Option<HashMap<String, Artifact>>,
}

/// 解压规则。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Extract {
    /// 排除的路径前缀。
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// 参数集合。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Arguments {
    /// 游戏参数。
    #[serde(default)]
    pub game: Vec<Argument>,
    /// JVM 参数。
    #[serde(default)]
    pub jvm: Vec<Argument>,
}

/// 单条参数。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Argument {
    /// 无条件字符串。
    Plain(String),
    /// 带规则的条件参数。
    Conditional(Conditional),
}

/// 条件参数。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Conditional {
    /// 平台规则。
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// 参数值。
    pub value: ArgumentValue,
}

/// 条件参数值。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ArgumentValue {
    /// 单个字符串。
    One(String),
    /// 多个字符串。
    Many(Vec<String>),
}

/// 平台规则。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rule {
    /// `allow` 或 `disallow`。
    pub action: String,
    /// 操作系统条件。
    #[serde(default)]
    pub os: Option<Os>,
    /// 特性条件。
    #[serde(default)]
    pub features: Option<HashMap<String, bool>>,
}

/// 操作系统条件。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Os {
    /// 系统名。
    #[serde(default)]
    pub name: Option<String>,
    /// 架构。
    #[serde(default)]
    pub arch: Option<String>,
    /// 系统版本。
    #[serde(default)]
    pub version: Option<String>,
}

/// Fabric loader 版本。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Loader {
    /// loader 版本。
    pub version: String,
    /// 是否为稳定版。
    #[serde(default)]
    pub stable: bool,
    /// Maven 坐标。
    #[serde(default)]
    pub maven: String,
}

/// 合并基础版本与 loader profile。
///
/// loader profile 覆盖主类、追加库和参数；客户端 jar 仍使用基础版本。
#[must_use]
pub fn merge(base: &Version, overlay: &Version) -> Version {
    Version {
        id: overlay.id.clone(),
        kind: if overlay.kind.is_empty() {
            base.kind.clone()
        } else {
            overlay.kind.clone()
        },
        main_class: overlay.main_class.clone(),
        inherits_from: None,
        client_version: Some(
            base.client_version
                .clone()
                .unwrap_or_else(|| base.id.clone()),
        ),
        assets: overlay.assets.clone().or_else(|| base.assets.clone()),
        asset_index: overlay
            .asset_index
            .clone()
            .or_else(|| base.asset_index.clone()),
        downloads: base.downloads.clone(),
        java_version: overlay
            .java_version
            .clone()
            .or_else(|| base.java_version.clone()),
        libraries: base
            .libraries
            .iter()
            .chain(overlay.libraries.iter())
            .cloned()
            .collect(),
        arguments: merge_arguments(base.arguments.as_ref(), overlay.arguments.as_ref()),
        minecraft_arguments: base.minecraft_arguments.clone(),
    }
}

fn merge_arguments(base: Option<&Arguments>, overlay: Option<&Arguments>) -> Option<Arguments> {
    match (base, overlay) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(overlay)) => Some(overlay.clone()),
        (Some(base), Some(overlay)) => Some(Arguments {
            game: base
                .game
                .iter()
                .chain(overlay.game.iter())
                .cloned()
                .collect(),
            jvm: base.jvm.iter().chain(overlay.jvm.iter()).cloned().collect(),
        }),
    }
}

/// 规则求值上下文。
#[derive(Debug, Clone)]
pub struct Context {
    /// 系统名，例如 linux。
    pub os: String,
    /// 架构，例如 `x86_64`。
    pub arch: String,
    /// 特性开关。
    pub features: HashMap<String, bool>,
}

impl Context {
    /// 当前运行环境的上下文。
    #[must_use]
    pub fn current() -> Self {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            other => other,
        };
        Self {
            os: std::env::consts::OS.to_owned(),
            arch: arch.to_owned(),
            features: HashMap::new(),
        }
    }
}

/// 判断规则集合是否允许。
#[must_use]
pub fn allowed(rules: &[Rule], context: &Context) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for rule in rules {
        if matches(rule, context) {
            allowed = rule.action.eq_ignore_ascii_case("allow");
        }
    }
    allowed
}

fn matches(rule: &Rule, context: &Context) -> bool {
    if let Some(os) = &rule.os {
        if let Some(name) = &os.name {
            if !os_name_matches(name, &context.os) {
                return false;
            }
        }
        if let Some(arch) = &os.arch {
            if arch != &context.arch {
                return false;
            }
        }
        if os.version.is_some() {
            return false;
        }
    }
    if let Some(features) = &rule.features {
        for (name, expected) in features {
            if context.features.get(name).copied().unwrap_or(false) != *expected {
                return false;
            }
        }
    }
    true
}

fn os_name_matches(expected: &str, actual: &str) -> bool {
    let expected = if expected == "osx" { "macos" } else { expected };
    expected == actual
}

/// 根据 Maven 坐标推导相对路径。
/// 把版本中的 LWJGL 库切换到指定版本。
///
/// 只改写 `org.lwjgl` 与 `org.lwjgl.lwjgl` 组；下载地址改为 Maven Central，
/// 并清空校验和（新版本未知）。非 LWJGL 库保持不变。
pub fn set_lwjgl(version: &mut Version, lwjgl: &str) {
    const MAVEN: &str = "https://repo1.maven.org/maven2";
    for library in &mut version.libraries {
        let Some((group, artifact)) = lwjgl_library(&library.name) else {
            continue;
        };
        library.name = format!("{group}:{artifact}:{lwjgl}");
        library.url = Some(MAVEN.to_owned());
        let Some(downloads) = &mut library.downloads else {
            continue;
        };
        if let Some(item) = &mut downloads.artifact {
            let path = maven_path(&library.name, None).unwrap_or_default();
            item.url = format!("{MAVEN}/{path}");
            item.path = Some(path);
            item.sha1.clear();
            item.size = 0;
        }
        if let Some(classifiers) = &mut downloads.classifiers {
            for (classifier, item) in classifiers.iter_mut() {
                let path = maven_path(&library.name, Some(classifier)).unwrap_or_default();
                item.url = format!("{MAVEN}/{path}");
                item.path = Some(path);
                item.sha1.clear();
                item.size = 0;
            }
        }
    }
}

fn lwjgl_library(name: &str) -> Option<(&str, &str)> {
    let mut parts = name.split(':');
    let group = parts.next()?;
    let artifact = parts.next()?;
    if group == "org.lwjgl" || group == "org.lwjgl.lwjgl" {
        Some((group, artifact))
    } else {
        None
    }
}

#[must_use]
pub fn maven_path(name: &str, classifier: Option<&str>) -> Option<String> {
    let parts: Vec<&str> = name.split(':').collect();
    let (group, artifact, version) = match parts.as_slice() {
        [group, artifact, version] | [group, artifact, version, _] => (*group, *artifact, *version),
        _ => return None,
    };
    let file = if let Some(classifier) = classifier {
        format!("{artifact}-{version}-{classifier}.jar")
    } else {
        format!("{artifact}-{version}.jar")
    };
    Some(format!(
        "{}/{}/{}/{}",
        group.replace('.', "/"),
        artifact,
        version,
        file
    ))
}

#[cfg(test)]
mod tests {
    use super::{Version, set_lwjgl};

    #[test]
    fn overrides_lwjgl_version() {
        let mut version: Version = serde_json::from_str(
            r#"{
              "id": "1.20.4",
              "type": "release",
              "mainClass": "net.minecraft.client.main.Main",
              "libraries": [
                {
                  "name": "org.lwjgl:lwjgl:3.3.2",
                  "downloads": {
                    "artifact": {"path": "org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2.jar", "sha1": "abc", "size": 10, "url": "https://libraries.minecraft.net/x"},
                    "classifiers": {"natives-linux": {"path": "org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2-natives-linux.jar", "sha1": "def", "size": 5, "url": "https://libraries.minecraft.net/y"}}
                  }
                },
                {"name": "com.example:lib:1.0"}
              ]
            }"#,
        )
        .expect("version");

        set_lwjgl(&mut version, "3.3.3");

        assert_eq!(version.libraries[0].name, "org.lwjgl:lwjgl:3.3.3");
        let downloads = version.libraries[0].downloads.as_ref().expect("downloads");
        let artifact = downloads.artifact.as_ref().expect("artifact");
        assert_eq!(artifact.sha1, "");
        assert_eq!(
            artifact.path.as_deref(),
            Some("org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar")
        );
        assert!(artifact.url.starts_with("https://repo1.maven.org/maven2/"));
        let classifier = downloads
            .classifiers
            .as_ref()
            .and_then(|items| items.get("natives-linux"))
            .expect("classifier");
        assert_eq!(
            classifier.path.as_deref(),
            Some("org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar")
        );
        assert_eq!(version.libraries[1].name, "com.example:lib:1.0");
    }
}
