//! JSON 实例仓储。
//!
//! 布局：
//! ```text
//! <root>/<id>/.arkmcl/instance.cfg
//! <root>/<id>/.arkmcl/pack.json
//! <root>/<id>/.arkmcl/state.json
//! ```

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use arkmcl_core::model::{
    Config, ConfigError, Id, IdError, Instance, Kind, NewInstance, Profile, State,
};
use arkmcl_core::ports::Repository;

use crate::fs::FileSystem;

/// 仓储错误。
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
    /// 实例标识非法。
    Id(IdError),
    /// 配置非法。
    Config(ConfigError),
    /// 名称为空、过长或包含换行。
    InvalidName,
    /// 配置文件格式非法。
    InvalidFormat(String),
    /// 实例不存在。
    NotFound(String),
    /// 实例标识已存在。
    Conflict(String),
    /// 实例正在运行，已加锁。
    Locked,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Json { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Id(error) => write!(formatter, "{error}"),
            Self::Config(error) => write!(formatter, "{error}"),
            Self::InvalidName => {
                formatter.write_str("instance name is empty, too long, or contains a newline")
            }
            Self::InvalidFormat(message) => write!(formatter, "invalid instance.cfg: {message}"),
            Self::NotFound(id) => write!(formatter, "instance not found: {id}"),
            Self::Conflict(id) => write!(formatter, "instance already exists: {id}"),
            Self::Locked => formatter.write_str("instance is running and cannot be modified"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Id(error) => Some(error),
            Self::Config(error) => Some(error),
            Self::InvalidName
            | Self::InvalidFormat(_)
            | Self::NotFound(_)
            | Self::Conflict(_)
            | Self::Locked => None,
        }
    }
}

impl From<IdError> for Error {
    fn from(error: IdError) -> Self {
        Self::Id(error)
    }
}

impl From<ConfigError> for Error {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

/// 基于文件系统的实例仓储。
pub struct JsonInstanceRepository<F: FileSystem> {
    root: PathBuf,
    fs: F,
}

impl<F: FileSystem> JsonInstanceRepository<F> {
    /// 在给定实例根下创建仓储。
    #[must_use]
    pub fn new(root: PathBuf, fs: F) -> Self {
        Self { root, fs }
    }

    fn instance_root(&self, id: &Id) -> PathBuf {
        self.root.join(id.as_str())
    }

    fn meta_root(&self, id: &Id) -> PathBuf {
        self.instance_root(id).join(".arkmcl")
    }

    fn allocate(&self, name: &str) -> Result<Id, Error> {
        let base = Id::from_name(name);
        if !self.fs.exists(&self.instance_root(&base)) {
            return Ok(base);
        }
        for suffix in 2..1000 {
            let candidate = format!("{}-{suffix}", base.as_str());
            let Ok(id) = Id::new(candidate) else {
                continue;
            };
            if !self.fs.exists(&self.instance_root(&id)) {
                return Ok(id);
            }
        }
        Err(Error::InvalidName)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        self.fs.write(path, bytes).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn read(&self, path: &Path) -> Result<Vec<u8>, Error> {
        self.fs.read(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn read_json<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<T, Error> {
        let bytes = self.read(path)?;
        serde_json::from_slice(&bytes).map_err(|source| Error::Json {
            path: path.to_path_buf(),
            source,
        })
    }
}

impl<F: FileSystem> Repository for JsonInstanceRepository<F> {
    type Error = Error;

    fn list(&self) -> Result<Vec<Instance>, Self::Error> {
        if !self.fs.exists(&self.root) {
            return Ok(Vec::new());
        }
        let entries = self.fs.read_dir(&self.root).map_err(|source| Error::Io {
            path: self.root.clone(),
            source,
        })?;
        let mut instances = Vec::new();
        for entry in entries {
            if !self.fs.is_dir(&entry) {
                continue;
            }
            let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Ok(id) = Id::new(name) else {
                continue;
            };
            if let Ok(instance) = self.load(&id) {
                instances.push(instance);
            }
        }
        instances.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        Ok(instances)
    }

    fn load(&self, id: &Id) -> Result<Instance, Self::Error> {
        let root = self.instance_root(id);
        if !self.fs.exists(&root) {
            return Err(Error::NotFound(id.to_string()));
        }
        let meta = self.meta_root(id);
        let cfg = self.read(&meta.join("instance.cfg"))?;
        let text = String::from_utf8(cfg)
            .map_err(|error| Error::InvalidFormat(format!("instance.cfg is not UTF-8: {error}")))?;
        let parsed = parse_cfg(&text)?;
        let profile: Profile = self.read_json(&meta.join("pack.json"))?;
        let state: State = self.read_json(&meta.join("state.json"))?;
        Ok(Instance {
            id: id.clone(),
            name: parsed.name,
            group: parsed.group,
            account: parsed.account,
            root,
            kind: parsed.kind,
            config: parsed.config,
            profile,
            state,
            created_at: parsed.created_at,
            last_launched_at: parsed.last_launched_at,
            run_seconds: parsed.run_seconds,
            last_exit_code: parsed.last_exit_code,
            last_crashed: parsed.last_crashed,
            last_crash_report: parsed.last_crash_report,
        })
    }

    fn create(&self, spec: &NewInstance) -> Result<Instance, Self::Error> {
        validate_name(&spec.name)?;
        spec.config.validate_for(spec.kind)?;
        let id = if let Some(id) = &spec.id {
            if self.fs.exists(&self.instance_root(id)) {
                return Err(Error::Conflict(id.to_string()));
            }
            id.clone()
        } else {
            self.allocate(&spec.name)?
        };
        let root = self.instance_root(&id);
        let meta = self.meta_root(&id);
        self.fs.create_dir_all(&meta).map_err(|source| Error::Io {
            path: meta.clone(),
            source,
        })?;
        let instance = Instance {
            id,
            kind: spec.kind,
            name: spec.name.trim().to_owned(),
            group: spec.group.clone(),
            account: spec.account.clone(),
            root: root.clone(),
            config: spec.config.clone(),
            profile: spec.profile.clone(),
            state: State::NotInstalled,
            created_at: now_unix(),
            last_launched_at: None,
            run_seconds: 0,
            last_exit_code: None,
            last_crashed: false,
            last_crash_report: None,
        };
        if let Err(error) = self.save(&instance) {
            let _ = self.fs.remove_dir_all(&root);
            return Err(error);
        }
        Ok(instance)
    }

    fn save(&self, instance: &Instance) -> Result<(), Self::Error> {
        validate_name(&instance.name)?;
        instance.config.validate_for(instance.kind)?;
        let meta = self.meta_root(&instance.id);
        self.fs.create_dir_all(&meta).map_err(|source| Error::Io {
            path: meta.clone(),
            source,
        })?;
        self.write(&meta.join("instance.cfg"), render_cfg(instance).as_bytes())?;
        let profile =
            serde_json::to_vec_pretty(&instance.profile).map_err(|source| Error::Json {
                path: meta.join("pack.json"),
                source,
            })?;
        self.write(&meta.join("pack.json"), &profile)?;
        let state = serde_json::to_vec_pretty(&instance.state).map_err(|source| Error::Json {
            path: meta.join("state.json"),
            source,
        })?;
        self.write(&meta.join("state.json"), &state)?;
        Ok(())
    }

    fn delete(&self, id: &Id) -> Result<(), Self::Error> {
        let root = self.instance_root(id);
        if !self.fs.exists(&root) {
            return Err(Error::NotFound(id.to_string()));
        }
        match crate::lock::Guard::check(&self.fs, &root) {
            Ok(()) => {}
            Err(crate::lock::Error::Locked) => return Err(Error::Locked),
            Err(crate::lock::Error::Io { path, source }) => {
                return Err(Error::Io { path, source });
            }
        }
        self.fs
            .remove_dir_all(&root)
            .map_err(|source| Error::Io { path: root, source })
    }

    fn rename(&self, from: &Id, to: &Id) -> Result<Instance, Self::Error> {
        let source = self.instance_root(from);
        if !self.fs.exists(&source) {
            return Err(Error::NotFound(from.to_string()));
        }
        let target = self.instance_root(to);
        if self.fs.exists(&target) {
            return Err(Error::Conflict(to.to_string()));
        }
        match crate::lock::Guard::check(&self.fs, &source) {
            Ok(()) => {}
            Err(crate::lock::Error::Locked) => return Err(Error::Locked),
            Err(crate::lock::Error::Io { path, source }) => {
                return Err(Error::Io { path, source });
            }
        }
        self.fs
            .rename(&source, &target)
            .map_err(|error| Error::Io {
                path: source.clone(),
                source: error,
            })?;
        self.load(to)
    }
}

fn validate_name(name: &str) -> Result<(), Error> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 128 || trimmed.contains(['\n', '\r']) {
        return Err(Error::InvalidName);
    }
    Ok(())
}

fn render_cfg(instance: &Instance) -> String {
    let account = instance.account.as_deref().unwrap_or("");
    let fullscreen = instance.config.fullscreen;
    let game_args =
        serde_json::to_string(&instance.config.game_args).unwrap_or_else(|_| "[]".to_owned());
    let group = instance.group.as_deref().unwrap_or("");
    let java_path = instance
        .config
        .java_path
        .as_ref()
        .map_or_else(String::new, |path| path.display().to_string());
    let java_version = instance
        .config
        .java_version
        .map_or_else(String::new, |version| version.to_string());
    let jvm_args =
        serde_json::to_string(&instance.config.jvm_args).unwrap_or_else(|_| "[]".to_owned());
    let last_crash_report = instance.last_crash_report.as_deref().unwrap_or("");
    let last_crashed = instance.last_crashed;
    let last_exit_code = instance
        .last_exit_code
        .map_or_else(String::new, |code| code.to_string());
    let lwjgl = instance.config.lwjgl.as_deref().unwrap_or("");
    let memory_mb = instance.config.memory_mb;
    let name = &instance.name;
    let kind = match instance.kind {
        Kind::Client => "client",
        Kind::Server => "server",
    };
    let window_width = instance.config.window_width;
    let created_at = instance.created_at;
    let last_launched_at = instance
        .last_launched_at
        .map_or_else(String::new, |value| value.to_string());
    let run_seconds = instance.run_seconds;
    format!(
        "account={account}\nfullscreen={fullscreen}\ngame_args={game_args}\ngroup={group}\njava_path={java_path}\njava_version={java_version}\njvm_args={jvm_args}\nlast_crash_report={last_crash_report}\nlast_crashed={last_crashed}\nlast_exit_code={last_exit_code}\nlwjgl={lwjgl}\nmemory_mb={memory_mb}\nname={name}\ntype={kind}\nwindow_width={window_width}\ncreated_at={created_at}\nlast_launched_at={last_launched_at}\nrun_seconds={run_seconds}\n"
    )
}

struct Parsed {
    kind: Kind,
    name: String,
    group: Option<String>,
    account: Option<String>,
    config: Config,
    created_at: u64,
    last_launched_at: Option<u64>,
    run_seconds: u64,
    last_exit_code: Option<i32>,
    last_crashed: bool,
    last_crash_report: Option<String>,
}

fn parse_cfg(text: &str) -> Result<Parsed, Error> {
    let mut values: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(Error::InvalidFormat(format!("line without '=': {line}")));
        };
        values.insert(key.trim().to_owned(), value.trim().to_owned());
    }

    let kind = match values.get("type").map(String::as_str) {
        None | Some("" | "client") => Kind::Client,
        Some("server") => Kind::Server,
        Some(other) => {
            return Err(Error::InvalidFormat(format!(
                "unknown instance type: {other}"
            )));
        }
    };
    let name = values
        .get("name")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| Error::InvalidFormat("missing name".to_owned()))?
        .clone();
    let group = values
        .get("group")
        .filter(|group| !group.is_empty())
        .cloned();
    let account = values
        .get("account")
        .filter(|account| !account.is_empty())
        .cloned();
    let java_path = values
        .get("java_path")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let java_version = values
        .get("java_version")
        .filter(|value| !value.is_empty())
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| Error::InvalidFormat("invalid java_version".to_owned()))?;
    let config = Config {
        memory_mb: parse_value(&values, "memory_mb", 2048)?,
        java_path,
        java_version,
        lwjgl: values
            .get("lwjgl")
            .filter(|lwjgl| !lwjgl.is_empty())
            .cloned(),
        jvm_args: parse_list(&values, "jvm_args")?,
        game_args: parse_list(&values, "game_args")?,
        window_width: parse_value(&values, "window_width", 854)?,
        fullscreen: parse_value(&values, "fullscreen", false)?,
    };
    config.validate_for(kind)?;
    Ok(Parsed {
        kind,
        name,
        group,
        account,
        config,
        created_at: parse_value(&values, "created_at", 0u64)?,
        last_launched_at: parse_optional_u64(&values, "last_launched_at")?,
        run_seconds: parse_value(&values, "run_seconds", 0u64)?,
        last_exit_code: parse_optional_i32(&values, "last_exit_code")?,
        last_crashed: parse_value(&values, "last_crashed", false)?,
        last_crash_report: values
            .get("last_crash_report")
            .filter(|value| !value.is_empty())
            .cloned(),
    })
}

fn parse_value<T>(values: &HashMap<String, String>, key: &str, default: T) -> Result<T, Error>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    match values.get(key) {
        Some(raw) if !raw.is_empty() => raw
            .parse()
            .map_err(|error| Error::InvalidFormat(format!("{key}: {error}"))),
        _ => Ok(default),
    }
}

fn parse_optional_i32(values: &HashMap<String, String>, key: &str) -> Result<Option<i32>, Error> {
    match values.get(key) {
        Some(raw) if !raw.is_empty() => raw
            .parse()
            .map(Some)
            .map_err(|error| Error::InvalidFormat(format!("{key}: {error}"))),
        _ => Ok(None),
    }
}

fn parse_optional_u64(values: &HashMap<String, String>, key: &str) -> Result<Option<u64>, Error> {
    match values.get(key) {
        Some(raw) if !raw.is_empty() => raw
            .parse()
            .map(Some)
            .map_err(|error| Error::InvalidFormat(format!("{key}: {error}"))),
        _ => Ok(None),
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn parse_list(values: &HashMap<String, String>, key: &str) -> Result<Vec<String>, Error> {
    match values.get(key) {
        Some(raw) if !raw.is_empty() => serde_json::from_str(raw)
            .map_err(|error| Error::InvalidFormat(format!("{key}: {error}"))),
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::JsonInstanceRepository;
    use crate::fs::LocalFileSystem;
    use arkmcl_core::model::{Config, Instance, NewInstance, Profile, State};
    use arkmcl_core::ports::Repository;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("arkmcl-store-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn repository(name: &str) -> (JsonInstanceRepository<LocalFileSystem>, PathBuf) {
        let directory = temp_dir(name);
        (
            JsonInstanceRepository::new(directory.clone(), LocalFileSystem),
            directory,
        )
    }

    #[test]
    fn creates_lists_loads_and_deletes() {
        let (repo, directory) = repository("crud");
        let mut spec = NewInstance::new("My Instance");
        spec.config.jvm_args = vec!["-Xmx2G".to_owned(), "-Dfoo=bar baz".to_owned()];
        spec.profile = Profile::default();

        let created = repo.create(&spec).expect("create");
        assert_eq!(created.id.as_str(), "my-instance");
        assert_eq!(created.state, State::NotInstalled);
        assert!(created.created_at > 0);
        assert_eq!(created.last_launched_at, None);
        assert_eq!(created.run_seconds, 0);
        assert!(created.root.join(".arkmcl/instance.cfg").exists());

        let listed = repo.list().expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);

        let loaded = repo.load(&created.id).expect("load");
        assert_eq!(loaded.name, "My Instance");
        assert_eq!(loaded.config.jvm_args[1], "-Dfoo=bar baz");

        repo.delete(&created.id).expect("delete");
        let remaining = repo.list().expect("list after delete");
        assert_eq!(remaining.len(), 0, "expected no instances after delete");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn allocates_unique_ids() {
        let (repo, directory) = repository("collision");
        let first = repo.create(&NewInstance::new("Test")).expect("first");
        let second = repo.create(&NewInstance::new("Test")).expect("second");
        assert_eq!(first.id.as_str(), "test");
        assert_eq!(second.id.as_str(), "test-2");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn renames_instances() {
        use arkmcl_core::model::Id;

        let (repo, directory) = repository("rename");
        let instance = repo.create(&NewInstance::new("Old")).expect("create");
        let to = Id::new("new").expect("id");
        let renamed = repo.rename(&instance.id, &to).expect("rename");
        assert_eq!(renamed.id.as_str(), "new");
        assert!(!directory.join("old").exists());
        assert!(directory.join("new/.arkmcl/instance.cfg").exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn saves_state_and_profile() {
        let (repo, directory) = repository("save");
        let mut instance = repo.create(&NewInstance::new("Save")).expect("create");
        instance.state = State::Ready;
        instance.config.memory_mb = 4096;
        repo.save(&instance).expect("save");
        let loaded = repo.load(&instance.id).expect("load");
        assert_eq!(loaded.state, State::Ready);
        assert_eq!(loaded.config.memory_mb, 4096);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn writes_instance_cfg_in_config_order() {
        let (repo, directory) = repository("order");
        let instance = repo.create(&NewInstance::new("Order")).expect("create");
        let text =
            fs::read_to_string(instance.root.join(".arkmcl/instance.cfg")).expect("read cfg");
        let keys: Vec<&str> = text
            .lines()
            .filter_map(|line| line.split_once('=').map(|(key, _)| key))
            .collect();
        assert_eq!(
            keys,
            [
                "account",
                "fullscreen",
                "game_args",
                "group",
                "java_path",
                "java_version",
                "jvm_args",
                "last_crash_report",
                "last_crashed",
                "last_exit_code",
                "lwjgl",
                "memory_mb",
                "name",
                "type",
                "window_width",
                "created_at",
                "last_launched_at",
                "run_seconds",
            ]
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn rejects_invalid_name_and_config() {
        let (repo, directory) = repository("invalid");
        let mut spec = NewInstance::new("   ");
        assert!(repo.create(&spec).is_err());
        spec.name = "Valid".to_owned();
        spec.config = Config {
            memory_mb: 128,
            ..Config::default()
        };
        assert!(repo.create(&spec).is_err());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn missing_instance_reports_not_found() {
        let (repo, directory) = repository("missing");
        let id = arkmcl_core::model::Id::new("none").expect("id");
        assert!(repo.load(&id).is_err());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn instance_root_is_set_by_repository() {
        let (repo, directory) = repository("root");
        let instance: Instance = repo.create(&NewInstance::new("Root")).expect("create");
        assert_eq!(instance.root, directory.join("root"));
        let _ = fs::remove_dir_all(&directory);
    }
}
