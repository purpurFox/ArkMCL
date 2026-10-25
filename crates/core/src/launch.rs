//! 启动计划构建。
//!
//! 纯逻辑：不访问文件系统、网络或进程。输入版本元数据与路径根，
//! 输出可直接交给进程启动器的参数。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::meta::{Argument, ArgumentValue, Context, Version, allowed, maven_path};
use crate::{APP_NAME, VERSION};

/// 资源路径根。
#[derive(Debug, Clone)]
pub struct Roots {
    /// 库目录。
    pub libraries: PathBuf,
    /// 版本目录。
    pub versions: PathBuf,
    /// 资源目录。
    pub assets: PathBuf,
    /// 本地库目录。
    pub natives: PathBuf,
}

/// 启动用户。
#[derive(Debug, Clone)]
pub struct User {
    /// 玩家名。
    pub name: String,
    /// 玩家 UUID。
    pub uuid: String,
    /// 访问令牌。
    pub access_token: String,
    /// 用户类型。
    pub user_type: String,
}

/// 启动选项。
#[derive(Debug, Clone)]
pub struct Options {
    /// Java 可执行文件。
    pub java: PathBuf,
    /// 启动用户。
    pub user: User,
    /// 最大堆内存（MiB）。
    pub memory_mb: u32,
    /// 额外 JVM 参数。
    pub jvm_args: Vec<String>,
    /// 额外游戏参数。
    pub game_args: Vec<String>,
    /// 特性开关。
    pub features: HashMap<String, bool>,
    /// 系统名，例如 `linux`。
    pub os: String,
    /// 架构，例如 `x86_64`。
    pub arch: String,
    /// 窗口宽度。
    pub width: u32,
    /// 窗口高度。
    pub height: u32,
}

/// 启动计划。
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    /// Java 可执行文件。
    pub java: PathBuf,
    /// 服务端 jar；设置时用 `-jar` 启动而不是主类。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jar: Option<PathBuf>,
    /// 参数文件；设置时用 `@文件` 启动（Forge/NeoForge 服务端）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_file: Option<PathBuf>,
    /// 主类。
    pub main_class: String,
    /// 类路径。
    pub classpath: Vec<PathBuf>,
    /// 需要解压的本地库。
    pub natives: Vec<PathBuf>,
    /// JVM 参数。
    pub jvm_args: Vec<String>,
    /// 游戏参数。
    pub game_args: Vec<String>,
    /// 本地库目录。
    pub natives_dir: PathBuf,
    /// 工作目录。
    pub working_dir: PathBuf,
    /// 资源目录。
    pub assets_dir: PathBuf,
    /// 资源索引 id。
    pub asset_index: String,
}

impl Plan {
    /// 生成完整命令行，Java 可执行文件在首位。
    #[must_use]
    pub fn command(&self) -> Vec<String> {
        let mut command = Vec::with_capacity(self.jvm_args.len() + self.game_args.len() + 4);
        command.push(self.java.display().to_string());
        command.extend(self.jvm_args.iter().cloned());
        if let Some(jar) = &self.jar {
            command.push("-jar".to_owned());
            command.push(jar.display().to_string());
        } else if let Some(args_file) = &self.args_file {
            command.push(format!("@{}", args_file.display()));
        } else {
            command.push(self.main_class.clone());
        }
        command.extend(self.game_args.iter().cloned());
        command
    }

    /// 以平台分隔符连接类路径。
    #[must_use]
    pub fn classpath_string(&self) -> String {
        join_classpath(&self.classpath)
    }
}

/// 启动计划错误。
#[derive(Debug)]
pub enum Error {
    /// 缺少主类。
    MissingMainClass,
    /// 无法解析库坐标。
    Library(String),
    /// 无法解析本地库分类器。
    Native(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMainClass => formatter.write_str("version has no main class"),
            Self::Library(name) => write!(formatter, "cannot resolve library: {name}"),
            Self::Native(name) => write!(formatter, "cannot resolve natives library: {name}"),
        }
    }
}

impl std::error::Error for Error {}

/// 启动计划构建器。
pub struct Planner {
    roots: Roots,
}

impl Planner {
    /// 创建构建器。
    #[must_use]
    pub fn new(roots: Roots) -> Self {
        Self { roots }
    }

    /// 根据版本元数据构建启动计划。
    ///
    /// # Errors
    ///
    /// 主类为空、库坐标非法或本地库分类器缺失时返回错误。
    pub fn plan(
        &self,
        instance_root: &Path,
        version: &Version,
        options: &Options,
    ) -> Result<Plan, Error> {
        if version.main_class.trim().is_empty() {
            return Err(Error::MissingMainClass);
        }
        let context = Context {
            os: options.os.clone(),
            arch: options.arch.clone(),
            features: options.features.clone(),
        };
        let (mut classpath, natives) = self.libraries(version, options, &context)?;
        let client_version = version.client_version.as_deref().unwrap_or(&version.id);
        classpath.push(
            self.roots
                .versions
                .join(client_version)
                .join(format!("{client_version}.jar")),
        );
        let natives_dir = self.roots.natives.join(&version.id).join(&options.os);
        let asset_index = version.asset_index.as_ref().map_or_else(
            || version.assets.clone().unwrap_or_default(),
            |index| index.id.clone(),
        );
        let values = self.values(
            instance_root,
            version,
            options,
            &natives_dir,
            &classpath,
            &asset_index,
        );
        let jvm_args = Self::jvm(version, options, &context, &values);
        let game_args = Self::game(version, options, &context, &values);
        Ok(Plan {
            java: options.java.clone(),
            jar: None,
            args_file: None,
            main_class: version.main_class.clone(),
            classpath,
            natives,
            jvm_args,
            game_args,
            natives_dir,
            working_dir: instance_root.to_path_buf(),
            assets_dir: self.roots.assets.clone(),
            asset_index,
        })
    }

    fn libraries(
        &self,
        version: &Version,
        options: &Options,
        context: &Context,
    ) -> Result<(Vec<PathBuf>, Vec<PathBuf>), Error> {
        let mut classpath = Vec::new();
        let mut natives = Vec::new();
        for library in &version.libraries {
            if !allowed(&library.rules, context) {
                continue;
            }
            if let Some(natives_map) = &library.natives {
                if let Some(classifier) = natives_map.get(&options.os) {
                    let path = artifact_path(library, Some(classifier))
                        .or_else(|| maven_path(&library.name, Some(classifier)));
                    match path {
                        Some(path) => natives.push(self.roots.natives.join(path)),
                        None => return Err(Error::Native(library.name.clone())),
                    }
                }
            }
            if let Some(path) =
                artifact_path(library, None).or_else(|| maven_path(&library.name, None))
            {
                classpath.push(self.roots.libraries.join(path));
            }
        }
        Ok((classpath, natives))
    }

    fn values(
        &self,
        instance_root: &Path,
        version: &Version,
        options: &Options,
        natives_dir: &Path,
        classpath: &[PathBuf],
        asset_index: &str,
    ) -> HashMap<&'static str, String> {
        let mut values: HashMap<&'static str, String> = HashMap::new();
        values.insert("classpath", join_classpath(classpath));
        values.insert("natives_directory", natives_dir.display().to_string());
        values.insert("game_directory", instance_root.display().to_string());
        values.insert("assets_root", self.roots.assets.display().to_string());
        values.insert("assets_index_name", asset_index.to_owned());
        values.insert("version_name", version.id.clone());
        values.insert("version_type", version.kind.clone());
        values.insert("auth_player_name", options.user.name.clone());
        values.insert("auth_uuid", options.user.uuid.clone());
        values.insert("auth_access_token", options.user.access_token.clone());
        values.insert("user_type", options.user.user_type.clone());
        values.insert("launcher_name", APP_NAME.to_owned());
        values.insert("launcher_version", VERSION.to_owned());
        values.insert("resolution_width", options.width.to_string());
        values.insert("resolution_height", options.height.to_string());
        values.insert("user_properties", "{}".to_owned());
        values.insert("clientid", String::new());
        values.insert("auth_xuid", String::new());
        values
    }

    fn jvm(
        version: &Version,
        options: &Options,
        context: &Context,
        values: &HashMap<&'static str, String>,
    ) -> Vec<String> {
        let mut jvm_args = version
            .arguments
            .as_ref()
            .map_or_else(Vec::new, |arguments| {
                collect(&arguments.jvm, context, values)
            });
        if !has_flag(&jvm_args, "-cp") && !has_flag(&jvm_args, "-classpath") {
            jvm_args.push("-cp".to_owned());
            jvm_args.push(values.get("classpath").cloned().unwrap_or_default());
        }
        if !jvm_args
            .iter()
            .chain(options.jvm_args.iter())
            .any(|argument| argument.starts_with("-Xmx"))
        {
            jvm_args.push(format!("-Xmx{}M", options.memory_mb));
        }
        jvm_args.extend(options.jvm_args.iter().cloned());
        jvm_args
    }

    fn game(
        version: &Version,
        options: &Options,
        context: &Context,
        values: &HashMap<&'static str, String>,
    ) -> Vec<String> {
        let mut game_args = match &version.arguments {
            Some(arguments) => collect(&arguments.game, context, values),
            None => version
                .minecraft_arguments
                .as_ref()
                .map_or_else(Vec::new, |legacy| {
                    legacy
                        .split_whitespace()
                        .map(|argument| substitute(argument, values))
                        .collect()
                }),
        };
        game_args.extend(options.game_args.iter().cloned());
        game_args
    }
}

fn artifact_path(library: &crate::meta::Library, classifier: Option<&str>) -> Option<String> {
    let downloads = library.downloads.as_ref()?;
    if let Some(classifier) = classifier {
        downloads
            .classifiers
            .as_ref()?
            .get(classifier)
            .and_then(|artifact| artifact.path.clone())
    } else {
        downloads
            .artifact
            .as_ref()
            .and_then(|artifact| artifact.path.clone())
    }
}

fn collect(
    entries: &[Argument],
    context: &Context,
    values: &HashMap<&'static str, String>,
) -> Vec<String> {
    let mut arguments = Vec::new();
    for entry in entries {
        match entry {
            Argument::Plain(value) => arguments.push(substitute(value, values)),
            Argument::Conditional(conditional) => {
                if !allowed(&conditional.rules, context) {
                    continue;
                }
                match &conditional.value {
                    ArgumentValue::One(value) => arguments.push(substitute(value, values)),
                    ArgumentValue::Many(list) => {
                        arguments.extend(list.iter().map(|value| substitute(value, values)));
                    }
                }
            }
        }
    }
    arguments
}

fn substitute(value: &str, values: &HashMap<&'static str, String>) -> String {
    let mut output = value.to_owned();
    for (key, replacement) in values {
        output = output.replace(&format!("${{{key}}}"), replacement);
    }
    output
}

fn has_flag(arguments: &[String], flag: &str) -> bool {
    arguments.iter().any(|argument| argument == flag)
}

fn join_classpath(paths: &[PathBuf]) -> String {
    let separator = if cfg!(windows) { ";" } else { ":" };
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use super::{Options, Planner, Roots, User};
    use crate::meta::Version;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn builds_server_command_with_args_file() {
        let plan = super::Plan {
            java: PathBuf::from("/usr/bin/java"),
            jar: None,
            args_file: Some(PathBuf::from(
                "libraries/net/minecraftforge/forge/1.20.4-49.0.0/unix_args.txt",
            )),
            main_class: String::new(),
            classpath: Vec::new(),
            natives: Vec::new(),
            jvm_args: vec!["-Xmx1G".to_owned()],
            game_args: vec!["nogui".to_owned()],
            natives_dir: PathBuf::new(),
            working_dir: PathBuf::new(),
            assets_dir: PathBuf::new(),
            asset_index: String::new(),
        };
        assert_eq!(
            plan.command(),
            vec![
                "/usr/bin/java",
                "-Xmx1G",
                "@libraries/net/minecraftforge/forge/1.20.4-49.0.0/unix_args.txt",
                "nogui"
            ]
        );
    }

    #[test]
    fn builds_server_command_with_jar() {
        let plan = super::Plan {
            java: PathBuf::from("/usr/bin/java"),
            jar: Some(PathBuf::from("server.jar")),
            args_file: None,
            main_class: String::new(),
            classpath: Vec::new(),
            natives: Vec::new(),
            jvm_args: vec!["-Xmx1G".to_owned()],
            game_args: vec!["--nogui".to_owned()],
            natives_dir: PathBuf::new(),
            working_dir: PathBuf::new(),
            assets_dir: PathBuf::new(),
            asset_index: String::new(),
        };
        assert_eq!(
            plan.command(),
            vec!["/usr/bin/java", "-Xmx1G", "-jar", "server.jar", "--nogui"]
        );
    }

    fn options() -> Options {
        Options {
            java: PathBuf::from("/usr/bin/java"),
            user: User {
                name: "Player".to_owned(),
                uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
                access_token: "token".to_owned(),
                user_type: "offline".to_owned(),
            },
            memory_mb: 2048,
            jvm_args: Vec::new(),
            game_args: Vec::new(),
            features: HashMap::new(),
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            width: 854,
            height: 480,
        }
    }

    fn roots() -> Roots {
        Roots {
            libraries: PathBuf::from("/root/libraries"),
            versions: PathBuf::from("/root/versions"),
            assets: PathBuf::from("/root/assets"),
            natives: PathBuf::from("/root/natives"),
        }
    }

    fn version() -> Version {
        serde_json::from_str(
            r#"{
              "id": "1.20.4",
              "type": "release",
              "mainClass": "net.minecraft.client.main.Main",
              "assets": "12",
              "assetIndex": {"id": "12", "sha1": "", "size": 0, "totalSize": 0, "url": ""},
              "libraries": [
                {
                  "name": "org.lwjgl:lwjgl:3.3.2",
                  "downloads": {"artifact": {"path": "org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2.jar", "sha1": "", "size": 0, "url": ""}}
                },
                {
                  "name": "org.lwjgl:lwjgl:3.3.2:natives-linux",
                  "downloads": {"classifiers": {"natives-linux": {"path": "org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2-natives-linux.jar", "sha1": "", "size": 0, "url": ""}}},
                  "natives": {"linux": "natives-linux"},
                  "rules": [{"action": "allow", "os": {"name": "linux"}}]
                },
                {
                  "name": "com.example:mac-only:1.0",
                  "downloads": {"artifact": {"path": "com/example/mac-only/1.0/mac-only-1.0.jar", "sha1": "", "size": 0, "url": ""}},
                  "rules": [{"action": "allow", "os": {"name": "osx"}}]
                }
              ],
              "arguments": {
                "jvm": ["-Djava.library.path=${natives_directory}", "-cp", "${classpath}"],
                "game": ["--username", "${auth_player_name}", "--version", "${version_name}", "--gameDir", "${game_directory}", "--assetsDir", "${assets_root}", "--assetIndex", "${assets_index_name}"]
              }
            }"#,
        )
        .expect("version")
    }

    #[test]
    fn builds_plan_with_rules_and_placeholders() {
        let plan = Planner::new(roots())
            .plan(Path::new("/instances/test"), &version(), &options())
            .expect("plan");

        assert_eq!(plan.main_class, "net.minecraft.client.main.Main");
        assert!(plan.classpath.contains(&PathBuf::from(
            "/root/libraries/org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2.jar"
        )));
        assert!(
            plan.classpath
                .contains(&PathBuf::from("/root/versions/1.20.4/1.20.4.jar"))
        );
        assert!(
            !plan
                .classpath
                .iter()
                .any(|path| path.to_string_lossy().contains("mac-only"))
        );
        assert!(plan.natives.contains(&PathBuf::from(
            "/root/natives/org/lwjgl/lwjgl/3.3.2/lwjgl-3.3.2-natives-linux.jar"
        )));
        assert!(
            plan.jvm_args
                .iter()
                .any(|argument| argument == "-Djava.library.path=/root/natives/1.20.4/linux")
        );
        assert!(plan.jvm_args.iter().any(|argument| argument == "-Xmx2048M"));
        let game = plan.game_args.join(" ");
        assert!(game.contains("--username Player"));
        assert!(game.contains("--gameDir /instances/test"));
        assert!(game.contains("--assetsDir /root/assets"));
        assert!(game.contains("--assetIndex 12"));
        assert_eq!(plan.command()[0], "/usr/bin/java");
    }

    #[test]
    fn supports_legacy_minecraft_arguments() {
        let version: Version = serde_json::from_str(
            r#"{
              "id": "1.5.2",
              "type": "release",
              "mainClass": "net.minecraft.client.Minecraft",
              "minecraftArguments": "--username ${auth_player_name} --gameDir ${game_directory}"
            }"#,
        )
        .expect("version");
        let plan = Planner::new(roots())
            .plan(Path::new("/instances/legacy"), &version, &options())
            .expect("plan");
        assert!(plan.game_args.contains(&"--username".to_owned()));
        assert!(plan.game_args.contains(&"Player".to_owned()));
        assert!(plan.game_args.contains(&"/instances/legacy".to_owned()));
    }

    #[test]
    fn rejects_missing_main_class() {
        let mut version = version();
        version.main_class.clear();
        assert!(
            Planner::new(roots())
                .plan(Path::new("/instances/test"), &version, &options())
                .is_err()
        );
    }
}
