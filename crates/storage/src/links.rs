//! 实例软链接。
//!
//! 新实例默认把全局存档、截图、资源包、光影、数据包等链接到实例目录。
//! 所有链接一律使用相对路径，便于整体迁移；软链接不可用时禁用共享，
//! 不复制全局内容。

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use arkmcl_platform::paths::Paths;

use arkmcl_core::model::Kind as InstanceKind;

use crate::fs::FileSystem;

/// 客户端可链接项；顺序即默认创建顺序。
pub const CLIENT_LINKS: [&str; 8] = [
    "usercache",
    "saves",
    "screenshots",
    "servers",
    "realms",
    "resourcepacks",
    "resourcepacks-server",
    "shaderpacks",
];

/// 服务端可链接项；顺序即默认创建顺序。
pub const SERVER_LINKS: [&str; 9] = [
    "usercache",
    "cache",
    "libraries",
    "versions",
    "eula",
    "ops",
    "whitelist",
    "banned-players",
    "banned-ips",
];

/// 某类型实例支持的全部链接名。
#[must_use]
pub fn link_names(kind: InstanceKind) -> &'static [&'static str] {
    match kind {
        InstanceKind::Client => &CLIENT_LINKS,
        InstanceKind::Server => &SERVER_LINKS,
    }
}

/// 单个链接的当前状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 指向全局目标的符号链接。
    Linked,
    /// 真实文件或目录。
    Real,
    /// 不存在。
    Missing,
    /// 指向别处的符号链接。
    Foreign,
}

/// 链接状态条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkState {
    /// 逻辑名称。
    pub name: &'static str,
    /// 实例内路径。
    pub path: &'static str,
    /// 当前状态。
    pub status: Status,
}

/// 链接结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 新建的链接。
    pub created: Vec<PathBuf>,
    /// 已存在、未覆盖的路径。
    pub skipped: Vec<PathBuf>,
    /// 已移除的链接。
    pub removed: Vec<PathBuf>,
    /// 文件系统不支持软链接时为 `true`。
    pub sharing_disabled: bool,
}

/// 链接错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
    /// 要链接的全局文件不存在。
    Missing(PathBuf),
    /// 未知的链接名称。
    Unknown(String),
    /// 当前文件系统不支持软链接。
    Unsupported,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Missing(path) => write!(formatter, "missing link target: {}", path.display()),
            Self::Unknown(name) => write!(formatter, "unknown link name: {name}"),
            Self::Unsupported => formatter.write_str("symbolic links are not supported"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Missing(_) | Self::Unknown(_) | Self::Unsupported => None,
        }
    }
}

/// 实例与全局目录之间的链接管理器。
pub struct Linker<F: FileSystem> {
    paths: Paths,
    fs: F,
}

#[derive(Debug, Clone, Copy)]
enum LinkKind {
    Dir,
    File,
}

impl<F: FileSystem> Linker<F> {
    /// 创建链接管理器。
    #[must_use]
    pub fn new(paths: Paths, fs: F) -> Self {
        Self { paths, fs }
    }

    /// 为客户端实例创建默认链接。
    ///
    /// # Errors
    ///
    /// 目录无法创建或链接无法建立时返回错误。
    pub fn create(&self, instance_root: &Path) -> Result<Report, Error> {
        self.create_for(instance_root, InstanceKind::Client)
    }

    /// 按实例类型创建默认链接。
    ///
    /// # Errors
    ///
    /// 目录无法创建或链接无法建立时返回错误。
    pub fn create_for(&self, instance_root: &Path, kind: InstanceKind) -> Result<Report, Error> {
        self.create_selected(instance_root, link_names(kind))
    }

    /// 为实例创建指定链接。
    ///
    /// # Errors
    ///
    /// 名称未知、目录无法创建或链接无法建立时返回错误。
    pub fn create_selected(&self, instance_root: &Path, names: &[&str]) -> Result<Report, Error> {
        if !self.fs.supports_symlink(instance_root) {
            return Ok(Report {
                sharing_disabled: true,
                ..Report::default()
            });
        }
        let mut report = Report::default();
        for name in names {
            for (link_name, target, kind) in self.entries(name)? {
                self.link(&target, &instance_root.join(link_name), kind, &mut report)?;
            }
        }
        Ok(report)
    }

    /// 移除指向全局目标的指定链接；真实目录或指向别处的链接不动。
    ///
    /// # Errors
    ///
    /// 名称未知或删除失败时返回错误。
    pub fn remove_selected(&self, instance_root: &Path, names: &[&str]) -> Result<Report, Error> {
        let mut report = Report::default();
        for name in names {
            for (link_name, target, _) in self.entries(name)? {
                let link = instance_root.join(link_name);
                let Ok(existing) = self.fs.read_link(&link) else {
                    report.skipped.push(link);
                    continue;
                };
                let resolved = link
                    .parent()
                    .map_or_else(|| existing.clone(), |parent| parent.join(&existing));
                let matches = existing == target
                    || resolved == target
                    || existing == relative_path(link.parent().unwrap_or(Path::new("")), &target);
                if !matches {
                    report.skipped.push(link);
                    continue;
                }
                self.fs.remove_file(&link).map_err(|source| Error::Io {
                    path: link.clone(),
                    source,
                })?;
                report.removed.push(link);
            }
        }
        Ok(report)
    }

    /// 检查指定逻辑名称的当前状态。
    ///
    /// # Errors
    ///
    /// 名称未知时返回错误。
    pub fn inspect(
        &self,
        instance_root: &Path,
        names: &[&'static str],
    ) -> Result<Vec<LinkState>, Error> {
        let mut link_states = Vec::new();
        for name in names {
            for (path, target, _) in self.entries(name)? {
                let link = instance_root.join(path);
                let status = if let Ok(existing) = self.fs.read_link(&link) {
                    let resolved = link
                        .parent()
                        .map_or_else(|| existing.clone(), |parent| parent.join(&existing));
                    if existing == target
                        || resolved == target
                        || existing
                            == relative_path(link.parent().unwrap_or(Path::new("")), &target)
                    {
                        Status::Linked
                    } else {
                        Status::Foreign
                    }
                } else if self.fs.exists(&link) {
                    Status::Real
                } else {
                    Status::Missing
                };
                link_states.push(LinkState { name, path, status });
            }
        }
        Ok(link_states)
    }

    /// 把全局 mods 中的单个文件链接到实例。
    ///
    /// # Errors
    ///
    /// 全局文件不存在或链接无法建立时返回错误。
    pub fn link_mod(&self, instance_root: &Path, name: &str) -> Result<(), Error> {
        if !self.fs.supports_symlink(instance_root) {
            return Err(Error::Unsupported);
        }
        let target = self.paths.minecraft.join("mods").join(name);
        if !self.fs.exists(&target) {
            return Err(Error::Missing(target));
        }
        let link = instance_root.join("mods").join(name);
        if self.fs.exists(&link) {
            return Ok(());
        }
        self.ensure_parent(&link)?;
        let relative = relative_path(link.parent().unwrap_or(Path::new("")), &target);
        self.fs
            .symlink(&relative, &link)
            .map_err(|source| Error::Io { path: link, source })
    }

    fn entries(&self, name: &str) -> Result<Vec<(&'static str, PathBuf, LinkKind)>, Error> {
        let data = &self.paths.data;
        let minecraft = &self.paths.minecraft;
        Ok(match name {
            "usercache" => vec![
                (
                    "usercache.json",
                    data.join("usercache.json"),
                    LinkKind::File,
                ),
                (
                    "usernamecache.json",
                    data.join("usernamecache.json"),
                    LinkKind::File,
                ),
            ],
            "saves" => vec![("saves", data.join("saves"), LinkKind::Dir)],
            "screenshots" => vec![("screenshots", data.join("screenshots"), LinkKind::Dir)],
            "servers" => vec![("servers.dat", data.join("servers.dat"), LinkKind::File)],
            "realms" => vec![(
                "realms_persistence.json",
                data.join("realms_persistence.json"),
                LinkKind::File,
            )],
            "resourcepacks" => vec![(
                "resourcepacks",
                minecraft.join("resourcepacks"),
                LinkKind::Dir,
            )],
            "resourcepacks-server" => vec![(
                "server-resource-packs",
                minecraft.join("server-resource-packs"),
                LinkKind::Dir,
            )],
            "shaderpacks" => vec![("shaderpacks", minecraft.join("shaderpacks"), LinkKind::Dir)],
            "cache" => vec![("cache", minecraft.join("cache"), LinkKind::Dir)],
            "libraries" => vec![("libraries", minecraft.join("libraries"), LinkKind::Dir)],
            "versions" => vec![("versions", minecraft.join("versions"), LinkKind::Dir)],
            "eula" => vec![("eula.txt", data.join("eula.txt"), LinkKind::File)],
            "ops" => vec![("ops.json", data.join("ops.json"), LinkKind::File)],
            "whitelist" => vec![(
                "whitelist.json",
                data.join("whitelist.json"),
                LinkKind::File,
            )],
            "banned-players" => vec![(
                "banned-players.json",
                data.join("banned-players.json"),
                LinkKind::File,
            )],
            "banned-ips" => vec![(
                "banned-ips.json",
                data.join("banned-ips.json"),
                LinkKind::File,
            )],
            other => return Err(Error::Unknown(other.to_owned())),
        })
    }

    fn link(
        &self,
        target: &Path,
        link: &Path,
        kind: LinkKind,
        report: &mut Report,
    ) -> Result<(), Error> {
        self.ensure_target(target, kind)?;
        let parent = link.parent().unwrap_or(Path::new(""));
        let relative = relative_path(parent, target);
        if self.fs.exists(link) {
            if let Ok(existing) = self.fs.read_link(link) {
                let resolved = parent.join(&existing);
                if existing == relative || existing == target || resolved == target {
                    report.skipped.push(link.to_path_buf());
                    return Ok(());
                }
            }
            report.skipped.push(link.to_path_buf());
            return Ok(());
        }
        self.ensure_parent(link)?;
        self.fs
            .symlink(&relative, link)
            .map_err(|source| Error::Io {
                path: link.to_path_buf(),
                source,
            })?;
        report.created.push(link.to_path_buf());
        Ok(())
    }

    fn ensure_target(&self, target: &Path, kind: LinkKind) -> Result<(), Error> {
        if self.fs.exists(target) {
            return Ok(());
        }
        match kind {
            LinkKind::Dir => self.fs.create_dir_all(target).map_err(|source| Error::Io {
                path: target.to_path_buf(),
                source,
            }),
            LinkKind::File => {
                self.ensure_parent(target)?;
                self.fs.write(target, b"").map_err(|source| Error::Io {
                    path: target.to_path_buf(),
                    source,
                })
            }
        }
    }

    fn ensure_parent(&self, path: &Path) -> Result<(), Error> {
        if let Some(parent) = path.parent() {
            self.fs.create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        Ok(())
    }
}

/// 计算从 `from` 目录到 `to` 的相对路径。
fn relative_path(from: &Path, to: &Path) -> PathBuf {
    use std::path::Component;
    let from_parts: Vec<Component<'_>> = from.components().collect();
    let to_parts: Vec<Component<'_>> = to.components().collect();
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    let mut result = PathBuf::new();
    for _ in common..from_parts.len() {
        result.push("..");
    }
    for component in &to_parts[common..] {
        result.push(component.as_os_str());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{Linker, Status};
    use crate::fs::LocalFileSystem;
    use arkmcl_platform::paths::Paths;
    use std::fs;
    use std::path::PathBuf;

    fn temp_paths(name: &str) -> (Paths, PathBuf) {
        let root = std::env::temp_dir().join(format!("arkmcl-links-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temp dir");
        let paths = Paths {
            config: root.join("config"),
            data: root.join("data"),
            minecraft: root.join("minecraft"),
            state: root.join("state"),
            runtime: root.join("run"),
            portable: true,
            runtime_fallback: false,
        };
        (paths, root)
    }

    #[cfg(unix)]
    #[test]
    fn creates_default_links_and_preserves_targets() {
        let (paths, root) = temp_paths("default");
        let instance = root.join("instances/test");
        fs::create_dir_all(&instance).expect("instance");

        let linker = Linker::new(paths.clone(), LocalFileSystem);
        let report = linker.create(&instance).expect("links");
        assert!(!report.sharing_disabled);
        assert_eq!(report.created.len(), 9);

        let saves = instance.join("saves");
        assert_eq!(
            fs::read_link(&saves).expect("read link"),
            PathBuf::from("../../data/saves")
        );
        assert!(!instance.join("datapacks").exists());

        fs::remove_dir_all(&instance).expect("remove instance");
        assert!(paths.data.join("saves").is_dir());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn creates_server_links() {
        use arkmcl_core::model::Kind;

        let (paths, root) = temp_paths("server");
        let instance = root.join("servers/test");
        fs::create_dir_all(&instance).expect("instance");

        let linker = Linker::new(paths.clone(), LocalFileSystem);
        let report = linker.create_for(&instance, Kind::Server).expect("links");
        assert!(!report.sharing_disabled);
        assert_eq!(report.created.len(), 10);
        assert_eq!(
            fs::read_link(instance.join("cache")).expect("cache link"),
            PathBuf::from("../../minecraft/cache")
        );
        assert!(instance.join("eula.txt").exists());
        assert!(!instance.join("saves").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn inspects_link_state() {
        let (paths, root) = temp_paths("inspect");
        let instance = root.join("instances/test");
        fs::create_dir_all(instance.join("shaderpacks")).expect("real dir");

        let linker = Linker::new(paths.clone(), LocalFileSystem);
        linker.create(&instance).expect("links");

        let link_states = linker
            .inspect(&instance, &["saves", "shaderpacks", "screenshots"])
            .expect("inspect");
        let status_of = |name: &str| {
            link_states
                .iter()
                .find(|state| state.name == name)
                .map(|state| state.status)
        };
        assert_eq!(status_of("saves"), Some(Status::Linked));
        assert_eq!(status_of("shaderpacks"), Some(Status::Real));
        assert_eq!(status_of("screenshots"), Some(Status::Linked));
        assert_eq!(status_of("accounts"), None);

        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn skips_existing_real_directories() {
        let (paths, root) = temp_paths("existing");
        let instance = root.join("instances/test");
        fs::create_dir_all(instance.join("saves")).expect("local saves");

        let linker = Linker::new(paths, LocalFileSystem);
        let report = linker.create(&instance).expect("links");
        assert!(report.skipped.contains(&instance.join("saves")));
        assert!(instance.join("saves").is_dir());
        assert!(
            fs::symlink_metadata(instance.join("saves"))
                .expect("metadata")
                .is_dir()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn links_individual_mods() {
        let (paths, root) = temp_paths("mods");
        let instance = root.join("instances/test");
        fs::create_dir_all(&instance).expect("instance");
        fs::create_dir_all(paths.minecraft.join("mods")).expect("mods");
        fs::write(paths.minecraft.join("mods/example.jar"), b"jar").expect("mod");

        let linker = Linker::new(paths.clone(), LocalFileSystem);
        linker.link_mod(&instance, "example.jar").expect("link mod");
        assert_eq!(
            fs::read_link(instance.join("mods/example.jar")).expect("read link"),
            PathBuf::from("../../../minecraft/mods/example.jar")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn removes_only_links_to_global_targets() {
        let (paths, root) = temp_paths("remove");
        let instance = root.join("instances/test");
        fs::create_dir_all(instance.join("shaderpacks")).expect("local shaderpacks");

        let linker = Linker::new(paths.clone(), LocalFileSystem);
        linker.create(&instance).expect("links");
        let report = linker
            .remove_selected(&instance, &["saves", "shaderpacks"])
            .expect("remove");

        assert_eq!(report.removed, vec![instance.join("saves")]);
        assert!(!instance.join("saves").exists());
        assert!(instance.join("shaderpacks").is_dir());
        assert!(
            fs::symlink_metadata(instance.join("shaderpacks"))
                .expect("metadata")
                .is_dir()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_missing_mod_target() {
        let (paths, root) = temp_paths("missing-mod");
        let instance = root.join("instances/test");
        fs::create_dir_all(&instance).expect("instance");
        let linker = Linker::new(paths, LocalFileSystem);
        assert!(linker.link_mod(&instance, "missing.jar").is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
