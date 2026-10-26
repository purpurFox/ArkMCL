//! 服务端实现。

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arkmcl_core::launch::{Options as LaunchOptions, Plan, Planner, Roots, User};
use arkmcl_core::meta::{self, Context, Version};
use arkmcl_core::model::{
    FABRIC, FORGE, Id, Instance, Kind as InstanceKind, MINECRAFT, NEOFORGE, NewInstance, PURPUR,
    State,
};
use arkmcl_core::ports::Repository;
use arkmcl_core::{APP_NAME, RuntimeInfo, VERSION};
use arkmcl_l10n::{Catalog, Locale};
use arkmcl_platform::ipc::{
    INTERNAL_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND, PARSE_ERROR, Request, Response,
};
use arkmcl_platform::paths::{self, Paths};
use arkmcl_storage::accounts::{Account, AccountId, Kind, Store as AccountStore, offline_uuid};
use arkmcl_storage::curseforge::{
    Client as CurseForgeClient, Error as CurseForgeError, Hit as CurseForgeHit,
    Sort as CurseForgeSort,
};
use arkmcl_storage::download::{
    Downloader, Mode as DownloadMode, Progress as DownloadProgress, Report as DownloadReport,
    Task as DownloadTask,
};
use arkmcl_storage::forge::{self, Kind as ForgeKind};
use arkmcl_storage::fs::LocalFileSystem;
use arkmcl_storage::groups::Store as GroupStore;
use arkmcl_storage::links::Linker;
use arkmcl_storage::modrinth::{Client as ModrinthClient, SearchHit, Version as ModrinthVersion};
use arkmcl_storage::mrpack;
use arkmcl_storage::msa;
use arkmcl_storage::natives::{Extractor, Report as NativesReport};
use arkmcl_storage::net::Fetcher;
use arkmcl_storage::purpur;
use arkmcl_storage::settings::Store as SettingsStore;
use arkmcl_storage::store::JsonInstanceRepository;
use arkmcl_storage::tokens::{Store as TokenStore, TokenSet};
use serde_json::{Value, json};

const MAX_MESSAGE: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const DEFAULT_DOWNLOAD_THREADS: usize = 8;
const MAX_DOWNLOAD_THREADS: usize = 64;

/// 服务端能力列表，供客户端做功能探测。
const CAPABILITIES: [&str; 5] = ["account", "instance", "launch", "resource", "settings"];

/// 服务端错误。
#[derive(Debug)]
pub enum Error {
    /// 路径解析或创建失败。
    Paths(paths::Error),
    /// I/O 失败。
    Io(io::Error),
    /// JSON 编解码失败。
    Json(serde_json::Error),
    /// 同一 socket 上已有服务。
    AlreadyRunning(PathBuf),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paths(error) => write!(formatter, "{error}"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::AlreadyRunning(path) => {
                write!(formatter, "already running at {}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Paths(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::AlreadyRunning(_) => None,
        }
    }
}

impl From<paths::Error> for Error {
    fn from(error: paths::Error) -> Self {
        Self::Paths(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// 运行时服务集合。
struct Services {
    repository: JsonInstanceRepository<LocalFileSystem>,
    linker: Linker<LocalFileSystem>,
    fetcher: Fetcher<LocalFileSystem>,
    downloader: Downloader,
    extractor: Extractor,
    minecraft: PathBuf,
    data: PathBuf,
    groups: GroupStore<LocalFileSystem>,
    sessions: Mutex<HashMap<Id, Session>>,
    finished: Mutex<HashMap<Id, Finished>>,
    accounts: AccountStore<LocalFileSystem>,
    tokens: Mutex<HashMap<AccountId, String>>,
    tokens_store: TokenStore,
    settings: SettingsStore<LocalFileSystem>,
}

#[derive(Debug, Clone, Default)]
struct Output {
    data: Vec<u8>,
    base: u64,
}

impl Output {
    fn append(&mut self, bytes: &[u8]) {
        self.data.extend_from_slice(bytes);
        if self.data.len() > OUTPUT_LIMIT {
            let drop = self.data.len() - OUTPUT_LIMIT;
            self.data.drain(..drop);
            self.base = self.base.saturating_add(drop as u64);
        }
    }

    fn read(&self, cursor: u64) -> (String, u64) {
        let start = usize::try_from(cursor.saturating_sub(self.base)).unwrap_or(usize::MAX);
        let start = start.min(self.data.len());
        (
            String::from_utf8_lossy(&self.data[start..]).into_owned(),
            self.base + self.data.len() as u64,
        )
    }
}

#[derive(Debug, Clone)]
struct Finished {
    output: Output,
    exit_code: Option<i32>,
    crashed: bool,
    crash_report: Option<String>,
    ended_at: u64,
}

struct Session {
    child: Arc<Mutex<Child>>,
    _guard: arkmcl_storage::lock::Guard<LocalFileSystem>,
    started_at: u64,
    account: Option<AccountId>,
    output: Arc<Mutex<Output>>,
    stopped: Arc<AtomicBool>,
}

impl Services {
    fn new(paths: &Paths) -> Self {
        Self {
            repository: JsonInstanceRepository::new(paths.data.join("instances"), LocalFileSystem),
            linker: Linker::new(paths.clone(), LocalFileSystem),
            fetcher: Fetcher::new(paths.minecraft.clone(), LocalFileSystem),
            downloader: Downloader::new(paths.minecraft.clone()),
            extractor: Extractor::new(paths.minecraft.clone()),
            minecraft: paths.minecraft.clone(),
            data: paths.data.clone(),
            groups: GroupStore::new(
                paths.data.join("instances").join("groups.json"),
                LocalFileSystem,
            ),
            sessions: Mutex::new(HashMap::new()),
            finished: Mutex::new(HashMap::new()),
            accounts: AccountStore::new(paths.config.join("accounts.json"), LocalFileSystem),
            tokens: Mutex::new(HashMap::new()),
            tokens_store: TokenStore::new(paths.config.join("tokens.json")),
            settings: SettingsStore::new(paths.config.join("settings.json"), LocalFileSystem),
        }
    }
}

/// 解析参数。
#[derive(Debug, Default)]
struct Options {
    lang: Option<String>,
    help: bool,
    version: bool,
}

/// 参数错误。
#[derive(Debug)]
enum ArgError {
    MissingLang,
    Unknown(String),
}

/// 服务端入口。
#[must_use]
pub fn main_entry() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    let options = match parse_args(&args) {
        Ok(options) => options,
        Err(error) => {
            let catalog = load_catalog(Locale::detect(None));
            return report_arg_error(&catalog, &error);
        }
    };

    let catalog = Arc::new(load_catalog(Locale::detect(options.lang.as_deref())));

    if options.help {
        print!("{}", catalog.text("service.help"));
        return ExitCode::SUCCESS;
    }
    if options.version {
        let info = RuntimeInfo::current();
        println!(
            "{}",
            catalog.format(
                "service.version",
                &[("name", info.name), ("version", info.version)]
            )
        );
        return ExitCode::SUCCESS;
    }

    let paths = match Paths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!(
                "{}",
                catalog.format("service.error.paths", &[("error", &error.to_string())])
            );
            return ExitCode::from(1);
        }
    };

    match run(paths, &catalog) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_runtime_error(catalog.as_ref(), &error),
    }
}

fn parse_args(args: &[String]) -> Result<Options, ArgError> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--lang" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(ArgError::MissingLang);
                };
                options.lang = Some(value.clone());
            }
            "--help" | "-h" => options.help = true,
            "--version" | "-V" => options.version = true,
            other => return Err(ArgError::Unknown(other.to_owned())),
        }
        index += 1;
    }
    Ok(options)
}

fn load_catalog(locale: Locale) -> Catalog {
    Catalog::load(locale).unwrap_or_else(|error| {
        eprintln!("localization error: {error}");
        std::process::exit(1);
    })
}

fn report_arg_error(catalog: &Catalog, error: &ArgError) -> ExitCode {
    match error {
        ArgError::MissingLang => {
            eprintln!("{}", catalog.text("service.error.lang"));
        }
        ArgError::Unknown(argument) => {
            eprintln!(
                "{}",
                catalog.format("service.error.usage", &[("arg", argument)])
            );
        }
    }
    ExitCode::from(2)
}

fn report_runtime_error(catalog: &Catalog, error: &Error) -> ExitCode {
    match error {
        Error::AlreadyRunning(path) => {
            eprintln!(
                "{}",
                catalog.format(
                    "service.error.already",
                    &[("socket", &path.display().to_string())]
                )
            );
        }
        Error::Paths(error) => {
            eprintln!(
                "{}",
                catalog.format("service.error.paths", &[("error", &error.to_string())])
            );
        }
        Error::Io(error) => {
            eprintln!(
                "{}",
                catalog.format("service.error.io", &[("error", &error.to_string())])
            );
        }
        Error::Json(error) => {
            eprintln!(
                "{}",
                catalog.format("service.error.io", &[("error", &error.to_string())])
            );
        }
    }
    ExitCode::from(1)
}

/// 运行服务端，直到收到 `server.stop`。
///
/// # Errors
///
/// 路径无法创建、socket 无法绑定、已有服务运行或 I/O 失败时返回错误。
pub fn run(paths: Paths, catalog: &Arc<Catalog>) -> Result<(), Error> {
    let catalog = Arc::clone(catalog);
    paths.ensure()?;
    let services = Arc::new(Services::new(&paths));
    refresh_tokens(&services, &catalog);
    let socket = paths.socket();
    prepare_socket(&socket)?;

    log(&catalog, "service.starting", &[]);
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;

    let pid_path = paths.pid();
    let _cleanup = Cleanup {
        socket: socket.clone(),
        pid: pid_path.clone(),
    };
    fs::write(&pid_path, std::process::id().to_string())?;

    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let paths = Arc::new(paths);

    log(
        &catalog,
        "service.listening",
        &[("socket", &socket.display().to_string())],
    );

    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _address)) => {
                let catalog = Arc::clone(&catalog);
                let paths = Arc::clone(&paths);
                let services = Arc::clone(&services);
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    if let Err(error) = handle_client(stream, &paths, &services, &catalog, &stop) {
                        log(
                            &catalog,
                            "service.error.io",
                            &[("error", &error.to_string())],
                        );
                    }
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(Error::Io(error)),
        }
    }

    log(&catalog, "service.stopped", &[]);
    Ok(())
}

fn prepare_socket(socket: &std::path::Path) -> Result<(), Error> {
    if !socket.exists() {
        return Ok(());
    }
    if UnixStream::connect(socket).is_ok() {
        return Err(Error::AlreadyRunning(socket.to_path_buf()));
    }
    fs::remove_file(socket)?;
    Ok(())
}

fn handle_client(
    stream: UnixStream,
    paths: &Paths,
    services: &Arc<Services>,
    catalog: &Catalog,
    stop: &AtomicBool,
) -> Result<(), Error> {
    let reader_stream = stream.try_clone()?;
    let mut reader = BufReader::new(reader_stream);
    let notifier = Mutex::new(stream.try_clone()?);
    let mut writer = stream;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if line.len() > MAX_MESSAGE {
            let response = Response::fault(None, PARSE_ERROR, catalog.text("rpc.parse").to_owned());
            write_response(&mut writer, &response)?;
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(request) => {
                let sink = |value: Value| {
                    if let Ok(mut stream) = notifier.lock() {
                        let _ = write_notification(&mut stream, &value);
                    }
                };
                dispatch(&request, paths, services, catalog, stop, &sink)
            }
            Err(_) => Response::fault(None, PARSE_ERROR, catalog.text("rpc.parse").to_owned()),
        };
        write_response(&mut writer, &response)?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    Ok(())
}

fn server_info(request: &Request, paths: &Paths, catalog: &Catalog) -> Response {
    Response::result(
        request.id.clone(),
        json!({
            "name": APP_NAME,
            "version": VERSION,
            "protocol": arkmcl_platform::ipc::PROTOCOL_VERSION,
            "capabilities": CAPABILITIES,
            "pid": std::process::id(),
            "locale": catalog.locale().tag(),
            "portable": paths.portable,
            "socket": paths.socket().display().to_string(),
        }),
    )
}

fn server_status(request: &Request, paths: &Paths, catalog: &Catalog) -> Response {
    server_info(request, paths, catalog)
}

fn server_stop(request: &Request, stop: &AtomicBool) -> Response {
    stop.store(true, Ordering::SeqCst);
    Response::result(request.id.clone(), json!({"stopping": true}))
}

fn dispatch(
    request: &Request,
    paths: &Paths,
    services: &Arc<Services>,
    catalog: &Catalog,
    stop: &AtomicBool,
    sink: &(dyn Fn(Value) + Sync),
) -> Response {
    match request.method.as_str() {
        "server.info" => server_info(request, paths, catalog),
        "server.status" => server_status(request, paths, catalog),
        "server.stop" => server_stop(request, stop),
        "instance.list" => instance_list(request, services, catalog),
        "instance.info" => instance_info(request, services, catalog),
        "instance.java" => instance_java(request, services, catalog),
        "instance.create" => instance_create(request, services, catalog),
        "instance.import" => instance_import(request, services, catalog, sink),
        "instance.update" => instance_update(request, services, catalog),
        "instance.link" => instance_link(request, services, catalog),
        "instance.links" => instance_links(request, services, catalog),
        "instance.move" => instance_move(request, services, catalog),
        "instance.group_move" => instance_group_move(request, services, catalog),
        "instance.remove" => instance_remove(request, services, catalog),
        "launch.prepare" => launch_prepare(request, services, catalog, sink),
        "launch.start" => launch_start(request, services, catalog),
        "launch.stop" => launch_stop(request, services, catalog),
        "launch.list" => launch_list(request, services, catalog),
        "launch.terminal" => launch_terminal(request, services, catalog),
        "launch.output" => launch_output(request, services, catalog),
        "launch.input" => launch_input(request, services, catalog),
        "account.info" => account_info(request, services, catalog),
        "account.list" => account_list(request, services, catalog),
        "account.add" => account_add(request, services, catalog),
        "account.remove" => account_remove(request, services, catalog),
        "account.default" => account_default(request, services, catalog),
        "account.refresh" => account_refresh(request, services, catalog),
        "resource.list_remote" => resource_list_remote(request, services, catalog),
        "resource.list" => resource_list(request, services, catalog),
        "resource.lookup" => resource_lookup(request, catalog),
        "resource.download" => resource_download(request, services, catalog, sink),
        "resource.list_remote_loaders" => resource_list_remote_loaders(request, services, catalog),
        "resource.search" => resource_search(request, catalog),
        "resource.autoremove" => resource_autoremove(request, services, catalog),
        "instance.default" => instance_default(request, services, catalog),
        "instance.groups" => instance_groups(request, services, catalog),
        "settings.get" => settings_get(request, services, catalog),
        "settings.update" => settings_update(request, services, catalog),
        method => Response::fault(
            request.id.clone(),
            METHOD_NOT_FOUND,
            catalog.format("rpc.method", &[("method", method)]),
        ),
    }
}

fn download_threads(request: &Request, catalog: &Catalog) -> Result<usize, (i32, String)> {
    match request.params.get("download_threads") {
        None => Ok(DEFAULT_DOWNLOAD_THREADS),
        Some(value) => {
            let threads = value
                .as_u64()
                .and_then(|threads| usize::try_from(threads).ok());
            match threads {
                Some(threads) if (1..=MAX_DOWNLOAD_THREADS).contains(&threads) => Ok(threads),
                _ => Err((
                    INVALID_PARAMS,
                    catalog.format("rpc.launch.threads", &[("value", &value.to_string())]),
                )),
            }
        }
    }
}

fn launch_prepare(
    request: &Request,
    services: &Services,
    catalog: &Catalog,
    sink: &(dyn Fn(Value) + Sync),
) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let instance = match services.repository.load(&id) {
        Ok(instance) => instance,
        Err(error) => return response_error(request, catalog, "rpc.launch.prepare", &error),
    };
    let threads = match download_threads(request, catalog) {
        Ok(threads) => threads,
        Err((code, message)) => return Response::fault(request.id.clone(), code, message),
    };
    match arkmcl_storage::lock::Guard::check(&LocalFileSystem, &instance.root) {
        Ok(()) => {}
        Err(arkmcl_storage::lock::Error::Locked) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.locked").to_owned(),
            );
        }
        Err(error) => return response_error(request, catalog, "rpc.launch.prepare", &error),
    }

    let (version_id, plan, downloads, natives) = if instance.kind == InstanceKind::Server {
        let (plan, downloads) = match server_plan(&instance, services, catalog, sink, threads) {
            Ok(result) => result,
            Err((code, message)) => return Response::fault(request.id.clone(), code, message),
        };
        let version_id = instance
            .profile
            .minecraft()
            .map(|component| component.version.clone())
            .unwrap_or_default();
        (version_id, plan, downloads, NativesReport::default())
    } else {
        let version = match resolve_version(&instance, services, catalog) {
            Ok(version) => version,
            Err((code, message)) => return Response::fault(request.id.clone(), code, message),
        };
        let (downloads, natives) = match prepare_content(&version, services, catalog, sink, threads)
        {
            Ok(result) => result,
            Err((code, message)) => return Response::fault(request.id.clone(), code, message),
        };
        let plan = match build_plan(
            &instance,
            &version,
            services,
            request.params.get("account").and_then(Value::as_str),
            catalog,
        ) {
            Ok((plan, _)) => plan,
            Err((code, message)) => return Response::fault(request.id.clone(), code, message),
        };
        (version.id, plan, downloads, natives)
    };

    Response::result(
        request.id.clone(),
        json!({
            "version": version_id,
            "plan": plan,
            "downloads": {
                "downloaded": downloads.downloaded,
                "skipped": downloads.skipped,
            },
            "natives": {
                "jars": natives.jars,
                "files": natives.files,
            },
            "stale": services.fetcher.take_stale(),
        }),
    )
}

fn resolve_version(
    instance: &Instance,
    services: &Services,
    catalog: &Catalog,
) -> Result<Version, (i32, String)> {
    let Some(base) = instance.profile.minecraft() else {
        return Err((
            INTERNAL_ERROR,
            catalog.text("rpc.launch.minecraft").to_owned(),
        ));
    };
    let game = base.version.clone();
    let mut version = services.fetcher.version(&game).map_err(|error| {
        (
            INTERNAL_ERROR,
            catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
        )
    })?;
    let mut loaders = 0;
    for component in &instance.profile.components {
        if component.uid == MINECRAFT {
            continue;
        }
        if component.uid != FABRIC && component.uid != FORGE && component.uid != NEOFORGE {
            return Err((
                INTERNAL_ERROR,
                catalog.format("rpc.launch.loader", &[("uid", &component.uid)]),
            ));
        }
        loaders += 1;
    }
    if loaders > 1 {
        return Err((
            INTERNAL_ERROR,
            catalog.text("rpc.launch.multiple_loaders").to_owned(),
        ));
    }
    if let Some(fabric) = instance.profile.fabric() {
        let profile = services
            .fetcher
            .fabric_profile(&game, &fabric.version)
            .map_err(|error| {
                (
                    INTERNAL_ERROR,
                    catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
                )
            })?;
        if profile.inherits_from.as_deref() != Some(game.as_str()) {
            return Err((
                INTERNAL_ERROR,
                catalog.format(
                    "rpc.launch.prepare",
                    &[("error", "Fabric profile inherits a different version")],
                ),
            ));
        }
        version = meta::merge(&version, &profile);
    }
    if let Some(loader) = instance
        .profile
        .components
        .iter()
        .find(|component| component.uid == FORGE || component.uid == NEOFORGE)
    {
        let profile = services.fetcher.version(&loader.version).map_err(|error| {
            (
                INTERNAL_ERROR,
                catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
            )
        })?;
        if profile.inherits_from.as_deref() != Some(game.as_str()) {
            return Err((
                INTERNAL_ERROR,
                catalog.format(
                    "rpc.launch.prepare",
                    &[("error", "loader profile inherits a different version")],
                ),
            ));
        }
        version = meta::merge(&version, &profile);
    }
    if let Some(lwjgl) = &instance.config.lwjgl {
        meta::set_lwjgl(&mut version, lwjgl);
    }
    Ok(version)
}

fn prepare_content(
    version: &Version,
    services: &Services,
    catalog: &Catalog,
    sink: &(dyn Fn(Value) + Sync),
    threads: usize,
) -> Result<(DownloadReport, NativesReport), (i32, String)> {
    let wrap = |error: &dyn std::fmt::Display| {
        (
            INTERNAL_ERROR,
            catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
        )
    };
    let index = services
        .fetcher
        .index(version)
        .map_err(|error| wrap(&error))?;
    let context = Context::current();
    let tasks = services
        .downloader
        .tasks(version, &index, &context)
        .map_err(|error| wrap(&error))?;
    let progress = SinkProgress::new(sink);
    let downloads = services
        .downloader
        .run_with_threads(&tasks, DownloadMode::SkipExisting, &progress, threads)
        .map_err(|error| wrap(&error))?;
    let natives = services
        .extractor
        .extract(version, &context)
        .map_err(|error| wrap(&error))?;
    Ok((downloads, natives))
}

fn java_executable(path: &Path) -> PathBuf {
    path.join("bin")
        .join(format!("java{}", std::env::consts::EXE_SUFFIX))
}

#[allow(clippy::too_many_lines)] // orchestration; split when more steps are added
fn build_plan(
    instance: &Instance,
    version: &Version,
    services: &Services,
    account: Option<&str>,
    catalog: &Catalog,
) -> Result<(Plan, Option<AccountId>), (i32, String)> {
    let suggested = version
        .java_version
        .as_ref()
        .map_or(8, |java| java.major_version);
    let required = instance.config.java_version.unwrap_or(suggested);
    let java = if let Some(path) = instance.config.java_path.clone() {
        java_executable(&path)
    } else {
        match arkmcl_storage::java::select(required) {
            Some(runtime) => runtime.path,
            None => {
                return Err((
                    INTERNAL_ERROR,
                    catalog.format("rpc.launch.java", &[("major", &required.to_string())]),
                ));
            }
        }
    };
    let account_id = account
        .map(str::to_owned)
        .or_else(|| instance.account.clone())
        .or_else(|| {
            services
                .settings
                .load()
                .ok()
                .and_then(|settings| settings.default_account)
        });
    let (user, account_id) = if let Some(account_id) = account_id {
        let account = resolve_account(services, catalog, &account_id)
            .map_err(|message| (INTERNAL_ERROR, message))?;
        let access_token = if account.kind == Kind::Microsoft {
            services
                .tokens
                .lock()
                .ok()
                .and_then(|tokens| tokens.get(&account.id).cloned())
                .ok_or_else(|| {
                    (
                        INTERNAL_ERROR,
                        catalog.text("rpc.launch.account_token").to_owned(),
                    )
                })?
        } else {
            "0".to_owned()
        };
        let user_type = if account.kind == Kind::Microsoft {
            "msa"
        } else {
            "legacy"
        };
        (
            User {
                name: account.name,
                uuid: account.id.as_str().to_owned(),
                access_token,
                user_type: user_type.to_owned(),
            },
            Some(account.id),
        )
    } else {
        (
            User {
                name: instance.name.clone(),
                uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
                access_token: "0".to_owned(),
                user_type: "offline".to_owned(),
            },
            None,
        )
    };
    let options = LaunchOptions {
        java,
        user,
        memory_mb: instance.config.memory_mb,
        jvm_args: instance.config.jvm_args.clone(),
        game_args: instance.config.game_args.clone(),
        features: HashMap::new(),
        os: Context::current().os,
        arch: Context::current().arch,
        width: instance.config.window_width,
        height: 480,
    };
    let planner = Planner::new(Roots {
        libraries: services.minecraft.join("libraries"),
        versions: services.minecraft.join("versions"),
        assets: services.minecraft.join("assets"),
        natives: services.minecraft.join("natives"),
    });
    planner
        .plan(&instance.root, version, &options)
        .map(|plan| (plan, account_id))
        .map_err(|error| {
            (
                INTERNAL_ERROR,
                catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
            )
        })
}

#[allow(clippy::too_many_lines)] // orchestration; split when more steps are added
fn launch_start(request: &Request, services: &Arc<Services>, catalog: &Catalog) -> Response {
    let id = match instance_id(request) {
        Some(id) => id,
        None => match default_instance(services, catalog) {
            Ok(id) => id,
            Err((code, message)) => {
                return Response::fault(request.id.clone(), code, message);
            }
        },
    };
    let mut instance = match services.repository.load(&id) {
        Ok(instance) => instance,
        Err(error) => return response_error(request, catalog, "rpc.launch.start", &error),
    };
    if services
        .sessions
        .lock()
        .map_or(true, |sessions| sessions.contains_key(&id))
    {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.launch.running").to_owned(),
        );
    }

    let guard = match arkmcl_storage::lock::Guard::acquire(LocalFileSystem, &instance.root) {
        Ok(guard) => guard,
        Err(arkmcl_storage::lock::Error::Locked) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.running").to_owned(),
            );
        }
        Err(error) => return response_error(request, catalog, "rpc.launch.start", &error),
    };

    let threads = match download_threads(request, catalog) {
        Ok(threads) => threads,
        Err((code, message)) => return Response::fault(request.id.clone(), code, message),
    };
    let (plan, account_id, downloads, natives) = if instance.kind == InstanceKind::Server {
        let (plan, downloads) = match server_plan(&instance, services, catalog, &no_sink, threads) {
            Ok(result) => result,
            Err((code, message)) => return Response::fault(request.id.clone(), code, message),
        };
        (plan, None, downloads, NativesReport::default())
    } else {
        let version = match resolve_version(&instance, services, catalog) {
            Ok(version) => version,
            Err((code, message)) => return Response::fault(request.id.clone(), code, message),
        };
        let (downloads, natives) =
            match prepare_content(&version, services, catalog, &no_sink, threads) {
                Ok(result) => result,
                Err((code, message)) => return Response::fault(request.id.clone(), code, message),
            };
        let (plan, account_id) = match build_plan(
            &instance,
            &version,
            services,
            request.params.get("account").and_then(Value::as_str),
            catalog,
        ) {
            Ok((plan, account_id)) => (plan, account_id),
            Err((code, message)) => return Response::fault(request.id.clone(), code, message),
        };
        (plan, account_id, downloads, natives)
    };
    let stale = services.fetcher.take_stale();

    let output = Arc::new(Mutex::new(Output::default()));
    let stopped = Arc::new(AtomicBool::new(false));
    let mut child = match spawn_process(&plan, &instance, Arc::clone(&output)) {
        Ok(child) => child,
        Err(error) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.launch.spawn", &[("error", &error.to_string())]),
            );
        }
    };
    let pid = child.id();
    let started_at = now_unix();
    instance.state = State::Running;
    instance.last_launched_at = Some(started_at);
    instance.last_exit_code = None;
    instance.last_crashed = false;
    instance.last_crash_report = None;
    if let Err(error) = services.repository.save(&instance) {
        let _ = child.kill();
        return response_error(request, catalog, "rpc.launch.start", &error);
    }

    let child = Arc::new(Mutex::new(child));
    let session = Session {
        child: Arc::clone(&child),
        _guard: guard,
        started_at,
        account: account_id.clone(),
        output,
        stopped: Arc::clone(&stopped),
    };
    let Ok(mut sessions) = services.sessions.lock() else {
        let _ = child.lock().map(|mut child| child.kill());
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.launch.start").to_owned(),
        );
    };
    sessions.insert(id.clone(), session);
    drop(sessions);

    if let Some(account_id) = &account_id {
        touch_account(services, account_id, started_at);
    }

    let monitor_services = Arc::clone(services);
    let monitor_id = id.clone();
    thread::spawn(move || monitor(&monitor_services, &monitor_id));

    Response::result(
        request.id.clone(),
        json!({
            "pid": pid,
            "plan": plan,
            "downloads": {
                "downloaded": downloads.downloaded,
                "skipped": downloads.skipped,
            },
            "natives": {
                "jars": natives.jars,
                "files": natives.files,
            },
            "stale": stale,
        }),
    )
}

fn launch_stop(request: &Request, services: &Arc<Services>, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let child = {
        let Ok(sessions) = services.sessions.lock() else {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.stop").to_owned(),
            );
        };
        sessions
            .get(&id)
            .map(|session| (Arc::clone(&session.child), Arc::clone(&session.stopped)))
    };
    let Some((child, stopped)) = child else {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.launch.not_running").to_owned(),
        );
    };
    let result = {
        let Ok(mut child) = child.lock() else {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.stop").to_owned(),
            );
        };
        stopped.store(true, Ordering::SeqCst);
        child.kill()
    };
    match result {
        Ok(()) => Response::result(request.id.clone(), json!({"stopped": true})),
        Err(error) => response_error(request, catalog, "rpc.launch.stop", &error),
    }
}

fn launch_list(request: &Request, services: &Arc<Services>, catalog: &Catalog) -> Response {
    let Ok(sessions) = services.sessions.lock() else {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.launch.stop").to_owned(),
        );
    };
    let entries: Vec<Value> = sessions
        .iter()
        .map(|(id, session)| {
            let pid = session.child.lock().ok().map(|child| child.id());
            json!({
                "id": id,
                "pid": pid,
                "started_at": session.started_at,
            })
        })
        .collect();
    Response::result(request.id.clone(), json!(entries))
}

fn launch_terminal(request: &Request, services: &Arc<Services>, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let Ok(sessions) = services.sessions.lock() else {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.launch.stop").to_owned(),
        );
    };
    let Some(session) = sessions.get(&id) else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.launch.not_running").to_owned(),
        );
    };
    let pid = session.child.lock().ok().map(|child| child.id());
    Response::result(
        request.id.clone(),
        json!({
            "id": id.as_str(),
            "pid": pid,
            "started_at": session.started_at,
        }),
    )
}

fn launch_output(request: &Request, services: &Arc<Services>, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let cursor = request
        .params
        .get("cursor")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = {
        let Ok(sessions) = services.sessions.lock() else {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.output").to_owned(),
            );
        };
        sessions.get(&id).map(|session| Arc::clone(&session.output))
    };
    if let Some(output) = output {
        let Ok(output) = output.lock() else {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.output").to_owned(),
            );
        };
        let (text, next) = output.read(cursor);
        return Response::result(
            request.id.clone(),
            json!({"output": text, "cursor": next, "running": true}),
        );
    }
    let finished = {
        let Ok(finished) = services.finished.lock() else {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.output").to_owned(),
            );
        };
        finished.get(&id).cloned()
    };
    if let Some(finished) = finished {
        let (text, next) = finished.output.read(cursor);
        return Response::result(
            request.id.clone(),
            json!({
                "output": text,
                "cursor": next,
                "running": false,
                "exit_code": finished.exit_code,
                "crashed": finished.crashed,
                "crash_report": finished.crash_report,
            }),
        );
    }
    Response::result(
        request.id.clone(),
        json!({"output": "", "cursor": cursor, "running": false}),
    )
}

fn launch_input(request: &Request, services: &Arc<Services>, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let line = request
        .params
        .get("line")
        .and_then(Value::as_str)
        .unwrap_or("");
    let child = {
        let Ok(sessions) = services.sessions.lock() else {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.input").to_owned(),
            );
        };
        sessions.get(&id).map(|session| Arc::clone(&session.child))
    };
    let Some(child) = child else {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.launch.not_running").to_owned(),
        );
    };
    let Ok(mut child) = child.lock() else {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.launch.input").to_owned(),
        );
    };
    let Some(stdin) = child.stdin.as_mut() else {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.launch.input").to_owned(),
        );
    };
    let result = stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush());
    match result {
        Ok(()) => Response::result(request.id.clone(), json!({"written": true})),
        Err(error) => Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.format("rpc.launch.input", &[("error", &error.to_string())]),
        ),
    }
}

fn spawn_process(
    plan: &Plan,
    instance: &Instance,
    output: Arc<Mutex<Output>>,
) -> io::Result<Child> {
    let command = plan.command();
    let Some((program, arguments)) = command.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty launch command",
        ));
    };
    let log_directory = instance.root.join("logs");
    fs::create_dir_all(&log_directory)?;
    let log = fs::File::create(log_directory.join("latest.log"))?;
    let log = Arc::new(Mutex::new(log));
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir(&plan.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, Arc::clone(&output), Arc::clone(&log));
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, output, log);
    }
    Ok(child)
}

fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    output: Arc<Mutex<Output>>,
    log: Arc<Mutex<fs::File>>,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if let Ok(mut output) = output.lock() {
                        output.append(&buffer[..count]);
                    }
                    if let Ok(mut log) = log.lock() {
                        let _ = log.write_all(&buffer[..count]);
                        let _ = log.flush();
                    }
                }
            }
        }
    });
}

fn monitor(services: &Arc<Services>, id: &Id) {
    loop {
        thread::sleep(Duration::from_millis(250));
        let child = {
            let Ok(sessions) = services.sessions.lock() else {
                return;
            };
            sessions.get(id).map(|session| Arc::clone(&session.child))
        };
        let Some(child) = child else {
            return;
        };
        let (exited, status) = {
            let Ok(mut child) = child.lock() else {
                return;
            };
            match child.try_wait() {
                Ok(Some(status)) => (true, Some(status)),
                Ok(None) => (false, None),
                Err(_) => (true, None),
            }
        };
        if !exited {
            continue;
        }
        let session = {
            let Ok(mut sessions) = services.sessions.lock() else {
                return;
            };
            sessions.remove(id)
        };
        let Some(session) = session else {
            return;
        };
        let elapsed = now_unix().saturating_sub(session.started_at);
        let stopped = session.stopped.load(Ordering::SeqCst);
        let exit_code = status
            .as_ref()
            .and_then(std::process::ExitStatus::code)
            .or_else(|| {
                status
                    .as_ref()
                    .and_then(|status| status.signal().map(|signal| 128 + signal))
            });
        let crash_report = services
            .repository
            .load(id)
            .ok()
            .and_then(|instance| find_crash_report(&instance.root, session.started_at));
        let crashed = !stopped
            && (status.as_ref().is_none_or(|status| !status.success()) || crash_report.is_some());
        if let Ok(mut instance) = services.repository.load(id) {
            instance.state = State::Ready;
            instance.run_seconds = instance.run_seconds.saturating_add(elapsed);
            instance.last_exit_code = exit_code;
            instance.last_crashed = crashed;
            instance.last_crash_report.clone_from(&crash_report);
            let _ = services.repository.save(&instance);
        }
        if let Some(account_id) = &session.account {
            if let Ok(Some(mut account)) = services.accounts.get(account_id) {
                account.run_seconds = account.run_seconds.saturating_add(elapsed);
                let _ = services.accounts.update(&account);
            }
        }
        if let Ok(mut output) = session.output.lock() {
            let label = exit_code.map_or_else(|| "signal".to_owned(), |code| code.to_string());
            output.append(format!("\n[arkmcl] process exited with code {label}\n").as_bytes());
        }
        let output = session
            .output
            .lock()
            .map(|output| output.clone())
            .unwrap_or_default();
        if let Ok(mut finished) = services.finished.lock() {
            finished.insert(
                id.clone(),
                Finished {
                    output,
                    exit_code,
                    crashed,
                    crash_report,
                    ended_at: now_unix(),
                },
            );
            while finished.len() > 8 {
                let Some(oldest) = finished
                    .iter()
                    .min_by_key(|(_, entry)| entry.ended_at)
                    .map(|(id, _)| id.clone())
                else {
                    break;
                };
                finished.remove(&oldest);
            }
        }
        drop(session);
        return;
    }
}

fn find_crash_report(root: &Path, since: u64) -> Option<String> {
    let entries = fs::read_dir(root.join("crash-reports")).ok()?;
    let mut best: Option<(u64, String)> = None;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs());
        if modified.saturating_add(1) < since {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if best
            .as_ref()
            .is_none_or(|(best_time, _)| modified >= *best_time)
        {
            best = Some((modified, name));
        }
    }
    best.map(|(_, name)| name)
}

/// 按 ID、UUID 或唯一名称解析账户；名称歧义时返回可解释的错误。
fn resolve_account(services: &Services, catalog: &Catalog, value: &str) -> Result<Account, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(catalog.text("rpc.account.id").to_owned());
    }
    let accounts = services
        .accounts
        .list()
        .map_err(|error| catalog.format("rpc.account.lookup", &[("error", &error.to_string())]))?;
    if let Some(account) = accounts.iter().find(|account| account.id.as_str() == value) {
        return Ok(account.clone());
    }
    let matches: Vec<&Account> = accounts
        .iter()
        .filter(|account| account.name == value)
        .collect();
    match matches.len() {
        0 => Err(catalog.format("rpc.account.not_found", &[("value", value)])),
        1 => Ok(matches[0].clone()),
        _ => {
            let ids = matches
                .iter()
                .map(|account| account.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(catalog.format("rpc.account.ambiguous", &[("ids", &ids)]))
        }
    }
}

#[allow(clippy::too_many_lines)] // inventory scan; split by type when more sources are added
fn resource_list(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let kind = request
        .params
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty());
    let sort_by = request
        .params
        .get("sort_by")
        .and_then(Value::as_str)
        .unwrap_or("create");
    if !matches!(sort_by, "create" | "launch" | "alphabet") {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("cli.error.sort_key", &[("value", sort_by)]),
        );
    }
    let order = request
        .params
        .get("order")
        .and_then(Value::as_str)
        .unwrap_or("asc");
    let limit = request
        .params
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(usize::MAX, |value| {
            usize::try_from(value).unwrap_or(usize::MAX)
        });
    let mut entries: Vec<Value> = Vec::new();

    if kind.is_none() || kind == Some("modpacks") {
        match services.repository.list() {
            Ok(instances) => {
                for instance in instances {
                    entries.push(json!({
                        "type": "modpacks",
                        "id": instance.id,
                        "name": instance.name,
                        "created_at": instance.created_at,
                        "last_launched_at": instance.last_launched_at,
                        "run_seconds": instance.run_seconds,
                    }));
                }
            }
            Err(error) => return response_error(request, catalog, "rpc.instance.list", &error),
        }
    }

    let directories = [
        ("worlds", "saves", false),
        ("resourcepacks", "resourcepacks", true),
        ("datapacks", "datapacks", false),
        ("shaderpacks", "shaderpacks", true),
        ("mods", "mods", true),
        ("plugins", "plugins", false),
        ("modpacks", "modpacks", false),
    ];
    for (label, directory, in_minecraft) in directories {
        if kind.is_some_and(|kind| kind != label) {
            continue;
        }
        let root = if in_minecraft {
            &services.minecraft
        } else {
            &services.data
        };
        let path = root.join(directory);
        let Ok(children) = fs::read_dir(&path) else {
            continue;
        };
        for child in children.flatten() {
            let metadata = child.metadata().ok();
            let modified = metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            entries.push(json!({
                "type": label,
                "name": child.file_name().to_string_lossy(),
                "path": child.path().display().to_string(),
                "size": metadata.map_or(0, |metadata| metadata.len()),
                "modified": modified,
            }));
        }
    }

    match sort_by {
        "alphabet" => entries.sort_by(|left, right| {
            left.get("name")
                .and_then(Value::as_str)
                .cmp(&right.get("name").and_then(Value::as_str))
        }),
        "launch" => entries.sort_by_key(|entry| {
            entry
                .get("last_launched_at")
                .and_then(Value::as_u64)
                .or_else(|| entry.get("modified").and_then(Value::as_u64))
                .unwrap_or(0)
        }),
        _ => entries.sort_by_key(|entry| {
            entry
                .get("created_at")
                .and_then(Value::as_u64)
                .or_else(|| entry.get("modified").and_then(Value::as_u64))
                .unwrap_or(0)
        }),
    }
    if order == "desc" {
        entries.reverse();
    }
    entries.truncate(limit);
    Response::result(request.id.clone(), json!({"resources": entries}))
}

fn resource_lookup(request: &Request, catalog: &Catalog) -> Response {
    let Some(project) = request
        .params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|project| !project.is_empty())
    else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.resource.project").to_owned(),
        );
    };
    let versions = string_array(request.params.get("game_versions"));
    let loaders = string_array(request.params.get("loaders"));
    if request.params.get("api").and_then(Value::as_str) == Some("curseforge") {
        return resource_lookup_curseforge(request, catalog, project, &versions, &loaders);
    }
    match ModrinthClient::new().versions(project, &versions, &loaders) {
        Ok(versions) => {
            let entries: Vec<Value> = versions
                .into_iter()
                .map(|version| {
                    json!({
                        "id": version.id,
                        "name": version.name,
                        "version_number": version.version_number,
                        "game_versions": version.game_versions,
                        "loaders": version.loaders,
                        "date_published": version.date_published,
                        "files": version.files.into_iter().map(|file| json!({
                            "filename": file.filename,
                            "url": file.url,
                            "primary": file.primary,
                            "size": file.size,
                            "sha1": file.hashes.sha1,
                        })).collect::<Vec<Value>>(),
                    })
                })
                .collect();
            Response::result(request.id.clone(), json!({"versions": entries}))
        }
        Err(error) => Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.format("rpc.resource.search", &[("error", &error.to_string())]),
        ),
    }
}

/// 只允许下载到全局目录的资源类型。
const GLOBAL_ONLY: [&str; 1] = ["modpack"];

/// 资源类型 →（目录名，是否位于 `$MINECRAFT_HOME`）。
fn resource_directory(kind: &str) -> Option<(&'static str, bool)> {
    Some(match kind {
        "mod" => ("mods", true),
        "resourcepack" => ("resourcepacks", true),
        "shader" => ("shaderpacks", true),
        "datapack" => ("datapacks", true),
        "world" => ("saves", true),
        "plugin" => ("plugins", true),
        "modpack" => ("modpacks", true),
        _ => return None,
    })
}

#[allow(clippy::too_many_lines)] // provider-specific install flow
fn resource_download_curseforge(
    request: &Request,
    services: &Services,
    catalog: &Catalog,
    sink: &(dyn Fn(Value) + Sync),
    project: &str,
) -> Response {
    let Ok(project_id) = project.parse::<u32>() else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.resource.curseforge_project").to_owned(),
        );
    };
    let client = match CurseForgeClient::new() {
        Ok(client) => client,
        Err(error) => return curseforge_fault(request, catalog, &error),
    };
    let kind = match request.params.get("type").and_then(Value::as_str) {
        Some(kind) if !kind.is_empty() => kind.to_owned(),
        _ => match client.project(project_id) {
            Ok(project) => project.kind,
            Err(error) => return curseforge_fault(request, catalog, &error),
        },
    };
    let Some((directory, in_minecraft)) = resource_directory(&kind) else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("rpc.resource.unsupported", &[("kind", kind.as_str())]),
        );
    };
    let requested_instance = request
        .params
        .get("instance")
        .and_then(Value::as_str)
        .filter(|instance| !instance.is_empty());
    if GLOBAL_ONLY.contains(&kind.as_str()) && requested_instance.is_some() {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("rpc.resource.global", &[("kind", kind.as_str())]),
        );
    }
    let instance = match requested_instance {
        Some(raw) => {
            let Ok(id) = Id::new(raw) else {
                return Response::fault(
                    request.id.clone(),
                    INVALID_PARAMS,
                    catalog.text("rpc.resource.instance").to_owned(),
                );
            };
            match services.repository.load(&id) {
                Ok(instance) => Some(instance),
                Err(error) => {
                    return response_error(request, catalog, "rpc.resource.download", &error);
                }
            }
        }
        None => None,
    };
    let guard = match &instance {
        Some(instance) => {
            match arkmcl_storage::lock::Guard::acquire(LocalFileSystem, &instance.root) {
                Ok(guard) => Some(guard),
                Err(arkmcl_storage::lock::Error::Locked) => {
                    return Response::fault(
                        request.id.clone(),
                        INTERNAL_ERROR,
                        catalog.text("rpc.launch.locked").to_owned(),
                    );
                }
                Err(error) => {
                    return response_error(request, catalog, "rpc.resource.download", &error);
                }
            }
        }
        None => None,
    };
    let root = if in_minecraft {
        &services.minecraft
    } else {
        &services.data
    };
    let target = instance.as_ref().map_or_else(
        || root.join(directory),
        |instance| instance.root.join(directory),
    );
    if let Err(error) = fs::create_dir_all(&target) {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.format("rpc.resource.download", &[("error", &error.to_string())]),
        );
    }

    let versions = string_array(request.params.get("game_versions"));
    let loaders = string_array(request.params.get("loaders"));
    let files = match client.files(
        project_id,
        versions.first().map(String::as_str),
        loaders.first().map(String::as_str),
    ) {
        Ok(files) => files,
        Err(error) => return curseforge_fault(request, catalog, &error),
    };
    let requested = request
        .params
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|version| !version.is_empty());
    let selected = match requested {
        Some(requested) => files
            .into_iter()
            .find(|file| file.id.to_string() == requested || file.name == requested),
        None => files.into_iter().next(),
    };
    let Some(file) = selected else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("rpc.resource.file", &[("project", project)]),
        );
    };
    let Some(filename) = std::path::Path::new(&file.filename)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.format("rpc.resource.file", &[("project", project)]),
        );
    };
    let url = match file.download_url.clone().filter(|url| !url.is_empty()) {
        Some(url) => url,
        None => match client.download_url(project_id, file.id) {
            Ok(url) => url,
            Err(error) => return curseforge_fault(request, catalog, &error),
        },
    };
    let path = target.join(filename);
    let task = DownloadTask {
        url,
        path: path.clone(),
        sha1: file.sha1.clone().unwrap_or_default(),
    };
    let progress = SinkProgress::new(sink);
    let report = match services
        .downloader
        .run_with(&[task], DownloadMode::Overwrite, &progress)
    {
        Ok(report) => report,
        Err(error) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.resource.download", &[("error", &error.to_string())]),
            );
        }
    };
    drop(guard);
    Response::result(
        request.id.clone(),
        json!({
            "project": project,
            "type": kind,
            "version": file.name,
            "file": filename,
            "path": path,
            "instance": instance.map(|instance| instance.id),
            "downloaded": report.downloaded,
            "skipped": report.skipped,
        }),
    )
}

#[allow(clippy::too_many_lines)] // download orchestration; split by type when more providers are added
fn resource_download(
    request: &Request,
    services: &Services,
    catalog: &Catalog,
    sink: &(dyn Fn(Value) + Sync),
) -> Response {
    let Some(project) = request
        .params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|project| !project.is_empty())
    else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.resource.project").to_owned(),
        );
    };
    if request.params.get("api").and_then(Value::as_str) == Some("curseforge") {
        return resource_download_curseforge(request, services, catalog, sink, project);
    }
    let client = ModrinthClient::new();
    let kind = match request.params.get("type").and_then(Value::as_str) {
        Some(kind) if !kind.is_empty() => kind.to_owned(),
        _ => match client.project(project) {
            Ok(project) => project.kind,
            Err(error) => {
                return Response::fault(
                    request.id.clone(),
                    INTERNAL_ERROR,
                    catalog.format("rpc.resource.download", &[("error", &error.to_string())]),
                );
            }
        },
    };
    let Some((directory, in_minecraft)) = resource_directory(&kind) else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("rpc.resource.unsupported", &[("kind", kind.as_str())]),
        );
    };
    let requested_instance = request
        .params
        .get("instance")
        .and_then(Value::as_str)
        .filter(|instance| !instance.is_empty());
    if GLOBAL_ONLY.contains(&kind.as_str()) && requested_instance.is_some() {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("rpc.resource.global", &[("kind", kind.as_str())]),
        );
    }

    let instance = match request
        .params
        .get("instance")
        .and_then(Value::as_str)
        .filter(|instance| !instance.is_empty())
    {
        Some(raw) => {
            let Ok(id) = Id::new(raw) else {
                return Response::fault(
                    request.id.clone(),
                    INVALID_PARAMS,
                    catalog.text("rpc.resource.instance").to_owned(),
                );
            };
            match services.repository.load(&id) {
                Ok(instance) => Some(instance),
                Err(error) => {
                    return response_error(request, catalog, "rpc.resource.download", &error);
                }
            }
        }
        None => None,
    };
    let guard = match &instance {
        Some(instance) => {
            match arkmcl_storage::lock::Guard::acquire(LocalFileSystem, &instance.root) {
                Ok(guard) => Some(guard),
                Err(arkmcl_storage::lock::Error::Locked) => {
                    return Response::fault(
                        request.id.clone(),
                        INTERNAL_ERROR,
                        catalog.text("rpc.launch.locked").to_owned(),
                    );
                }
                Err(error) => {
                    return response_error(request, catalog, "rpc.resource.download", &error);
                }
            }
        }
        None => None,
    };
    let root = if in_minecraft {
        &services.minecraft
    } else {
        &services.data
    };
    let target = instance.as_ref().map_or_else(
        || root.join(directory),
        |instance| instance.root.join(directory),
    );
    if let Err(error) = fs::create_dir_all(&target) {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.format("rpc.resource.download", &[("error", &error.to_string())]),
        );
    }

    let versions = string_array(request.params.get("game_versions"));
    let loaders = string_array(request.params.get("loaders"));
    let project_versions = match client.versions(project, &versions, &loaders) {
        Ok(versions) => versions,
        Err(error) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.resource.download", &[("error", &error.to_string())]),
            );
        }
    };
    let requested = request
        .params
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|version| !version.is_empty());
    let selected = match requested {
        Some(requested) => project_versions
            .iter()
            .find(|version| version.id == requested || version.version_number == requested)
            .cloned(),
        None => project_versions.first().cloned(),
    };
    let Some(version) = selected else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("rpc.resource.file", &[("project", project)]),
        );
    };
    let dependencies = match resolve_resource_dependencies(&client, &version, &versions, &loaders) {
        Ok(dependencies) => dependencies,
        Err(message) => {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format("rpc.resource.dependency", &[("error", &message)]),
            );
        }
    };
    let (primary, filename) = match version_task(&version, &target) {
        Ok(result) => result,
        Err(message) => {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format("rpc.resource.dependency", &[("error", &message)]),
            );
        }
    };
    let path = primary.path.clone();
    let mut tasks = vec![primary];
    let mut resolved = vec![json!({
        "project": version.project_id,
        "version": version.version_number,
        "file": filename,
    })];
    for dependency in &dependencies {
        match version_task(dependency, &target) {
            Ok((task, name)) => {
                resolved.push(json!({
                    "project": dependency.project_id,
                    "version": dependency.version_number,
                    "file": name,
                }));
                tasks.push(task);
            }
            Err(message) => {
                return Response::fault(
                    request.id.clone(),
                    INVALID_PARAMS,
                    catalog.format("rpc.resource.dependency", &[("error", &message)]),
                );
            }
        }
    }
    let progress = SinkProgress::new(sink);
    let report = match services
        .downloader
        .run_with(&tasks, DownloadMode::Overwrite, &progress)
    {
        Ok(report) => report,
        Err(error) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.resource.download", &[("error", &error.to_string())]),
            );
        }
    };
    drop(guard);
    Response::result(
        request.id.clone(),
        json!({
            "project": project,
            "type": kind,
            "version": version.version_number,
            "file": filename,
            "path": path,
            "instance": instance.map(|instance| instance.id),
            "downloaded": report.downloaded,
            "skipped": report.skipped,
            "resolved": resolved,
        }),
    )
}

fn version_task(
    version: &ModrinthVersion,
    target: &Path,
) -> Result<(DownloadTask, String), String> {
    let Some(file) = version
        .files
        .iter()
        .find(|file| file.primary)
        .or_else(|| version.files.first())
    else {
        return Err(format!("no downloadable file for {}", version.id));
    };
    let Some(filename) = Path::new(&file.filename)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return Err(format!("invalid file name: {}", file.filename));
    };
    Ok((
        DownloadTask {
            url: file.url.clone(),
            path: target.join(filename),
            sha1: file.hashes.sha1.clone().unwrap_or_default(),
        },
        filename.to_owned(),
    ))
}

fn resolve_resource_dependencies(
    client: &ModrinthClient,
    root: &ModrinthVersion,
    game_versions: &[String],
    loaders: &[String],
) -> Result<Vec<ModrinthVersion>, String> {
    use std::collections::{HashSet, VecDeque};

    let mut seen = HashSet::new();
    if !root.project_id.is_empty() {
        seen.insert(root.project_id.clone());
    }
    let mut resolved = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(root.clone());
    while let Some(current) = queue.pop_front() {
        for dependency in &current.dependencies {
            match dependency.dependency_type.as_str() {
                "required" => {
                    let Some(project) = dependency.project_id.clone() else {
                        continue;
                    };
                    if !seen.insert(project.clone()) {
                        continue;
                    }
                    let version = if let Some(id) = &dependency.version_id {
                        client.version(id).map_err(|error| error.to_string())?
                    } else {
                        let mut candidates = client
                            .versions(&project, game_versions, loaders)
                            .map_err(|error| error.to_string())?;
                        candidates
                            .drain(..)
                            .next()
                            .ok_or_else(|| format!("no compatible version for {project}"))?
                    };
                    if resolved.len() >= 100 {
                        return Err("dependency limit exceeded".to_owned());
                    }
                    queue.push_back(version.clone());
                    resolved.push(version);
                }
                "incompatible" => {
                    if let Some(project) = &dependency.project_id {
                        if seen.contains(project) {
                            return Err(format!("incompatible dependency: {project}"));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(resolved)
}

fn resource_autoremove(request: &Request, services: &Services, _catalog: &Catalog) -> Response {
    let apply = request
        .params
        .get("apply")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut referenced = std::collections::HashSet::new();
    if let Ok(instances) = services.repository.list() {
        for instance in instances {
            for directory in ["mods", "resourcepacks", "shaderpacks", "datapacks"] {
                let path = instance.root.join(directory);
                let Ok(entries) = fs::read_dir(&path) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let Ok(target) = fs::read_link(entry.path()) else {
                        continue;
                    };
                    let resolved = if target.is_absolute() {
                        target
                    } else {
                        entry.path().parent().unwrap_or(Path::new("")).join(target)
                    };
                    if let Ok(canonical) = fs::canonicalize(resolved) {
                        referenced.insert(canonical);
                    }
                }
            }
        }
    }
    let mut candidates = Vec::new();
    for directory in ["mods", "resourcepacks", "shaderpacks", "datapacks"] {
        let path = services.minecraft.join(directory);
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().is_file() {
                continue;
            }
            let canonical = fs::canonicalize(entry.path()).unwrap_or_else(|_| entry.path());
            if referenced.contains(&canonical) {
                continue;
            }
            candidates.push(entry.path());
        }
    }
    let mut removed = 0;
    if apply {
        for path in &candidates {
            if arkmcl_storage::trash::trash(path).is_ok() {
                removed += 1;
            }
        }
    }
    Response::result(
        request.id.clone(),
        json!({
            "apply": apply,
            "removed": removed,
            "files": candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<String>>(),
        }),
    )
}

fn resource_list_remote(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let manifest = match services.fetcher.manifest() {
        Ok(manifest) => manifest,
        Err(error) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.resource.search", &[("error", &error.to_string())]),
            );
        }
    };
    let mut buckets = string_array(request.params.get("vanilla"));
    if buckets.is_empty() {
        buckets.push("stable".to_owned());
    }
    let patterns = string_array(request.params.get("versions"));
    let sort_by = request
        .params
        .get("sort_by")
        .and_then(Value::as_str)
        .unwrap_or("create");
    let order = request
        .params
        .get("order")
        .and_then(Value::as_str)
        .unwrap_or("asc");
    let limit = request
        .params
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(usize::MAX, |value| {
            usize::try_from(value).unwrap_or(usize::MAX)
        });
    let mut versions: Vec<&meta::Summary> = manifest
        .versions
        .iter()
        .filter(|summary| version_matches(summary, &buckets, &patterns))
        .collect();
    match sort_by {
        "create" => versions.sort_by(|left, right| left.release_time.cmp(&right.release_time)),
        "alphabet" => versions.sort_by(|left, right| left.id.cmp(&right.id)),
        other => {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format("cli.error.sort_key", &[("value", other)]),
            );
        }
    }
    if order == "desc" {
        versions.reverse();
    }
    versions.truncate(limit);
    let entries: Vec<Value> = versions
        .into_iter()
        .map(|summary| {
            json!({
                "id": summary.id,
                "type": summary.kind,
                "release_time": summary.release_time,
                "url": summary.url,
            })
        })
        .collect();
    Response::result(
        request.id.clone(),
        json!({"versions": entries, "latest": manifest.latest}),
    )
}

fn resource_lookup_curseforge(
    request: &Request,
    catalog: &Catalog,
    project: &str,
    versions: &[String],
    loaders: &[String],
) -> Response {
    let Ok(project_id) = project.parse::<u32>() else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.resource.curseforge_project").to_owned(),
        );
    };
    let client = match CurseForgeClient::new() {
        Ok(client) => client,
        Err(error) => return curseforge_fault(request, catalog, &error),
    };
    match client.files(
        project_id,
        versions.first().map(String::as_str),
        loaders.first().map(String::as_str),
    ) {
        Ok(files) => {
            let entries: Vec<Value> = files
                .into_iter()
                .map(|file| {
                    json!({
                        "id": file.id.to_string(),
                        "name": file.name,
                        "version_number": file.name,
                        "game_versions": file.game_versions,
                        "loaders": [],
                        "date_published": file.date,
                        "files": [{
                            "filename": file.filename,
                            "url": file.download_url.unwrap_or_default(),
                            "primary": true,
                            "size": file.size,
                            "sha1": file.sha1,
                        }],
                    })
                })
                .collect();
            Response::result(request.id.clone(), json!({"versions": entries}))
        }
        Err(error) => curseforge_fault(request, catalog, &error),
    }
}

#[allow(clippy::too_many_lines)] // loader matrix; split when more sources are added
fn resource_list_remote_loaders(
    request: &Request,
    services: &Services,
    catalog: &Catalog,
) -> Response {
    let Some(loader) = request
        .params
        .get("loader")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|loader| !loader.is_empty())
    else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.resource.loader").to_owned(),
        );
    };
    let patterns = string_array(request.params.get("versions"));
    let game = patterns
        .iter()
        .find(|pattern| !pattern.contains('*') && !pattern.contains('?'))
        .cloned()
        .or_else(|| {
            services
                .fetcher
                .manifest()
                .ok()
                .map(|manifest| manifest.latest.release)
        });
    let Some(game) = game else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.instance.minecraft").to_owned(),
        );
    };
    let mut entries: Vec<Value> = match loader {
        "fabric" => match services.fetcher.fabric_loaders(&game) {
            Ok(mut loaders) => {
                loaders.sort_by(|left, right| left.version.cmp(&right.version));
                loaders
                    .into_iter()
                    .map(|loader| {
                        json!({
                            "version": loader.version,
                            "stable": loader.stable,
                            "maven": loader.maven,
                            "game": game,
                        })
                    })
                    .collect()
            }
            Err(error) => {
                return Response::fault(
                    request.id.clone(),
                    INTERNAL_ERROR,
                    catalog.format("rpc.resource.search", &[("error", &error.to_string())]),
                );
            }
        },
        "forge" => match forge::versions(ForgeKind::Forge, &game) {
            Ok(versions) => versions
                .into_iter()
                .map(|version| {
                    json!({
                        "version": version.version,
                        "stable": version.stable,
                        "maven": "",
                        "game": game,
                    })
                })
                .collect(),
            Err(error) => {
                return Response::fault(
                    request.id.clone(),
                    INTERNAL_ERROR,
                    catalog.format("rpc.resource.search", &[("error", &error.to_string())]),
                );
            }
        },
        "neoforge" => match forge::versions(ForgeKind::NeoForge, &game) {
            Ok(versions) => versions
                .into_iter()
                .map(|version| {
                    json!({
                        "version": version.version,
                        "stable": version.stable,
                        "maven": "",
                        "game": game,
                    })
                })
                .collect(),
            Err(error) => {
                return Response::fault(
                    request.id.clone(),
                    INTERNAL_ERROR,
                    catalog.format("rpc.resource.search", &[("error", &error.to_string())]),
                );
            }
        },
        "purpur" => match purpur::builds(&game) {
            Ok(builds) => builds
                .into_iter()
                .map(|build| {
                    json!({
                        "version": build,
                        "stable": true,
                        "maven": "",
                        "game": game,
                    })
                })
                .collect(),
            Err(error) => {
                return Response::fault(
                    request.id.clone(),
                    INTERNAL_ERROR,
                    catalog.format("rpc.resource.search", &[("error", &error.to_string())]),
                );
            }
        },
        other => {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format("rpc.resource.unsupported", &[("kind", other)]),
            );
        }
    };
    if request.params.get("order").and_then(Value::as_str) == Some("desc") {
        entries.reverse();
    }
    let limit = request
        .params
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(usize::MAX, |value| {
            usize::try_from(value).unwrap_or(usize::MAX)
        });
    entries.truncate(limit);
    Response::result(request.id.clone(), json!({"loaders": entries}))
}

fn version_matches(summary: &meta::Summary, buckets: &[String], patterns: &[String]) -> bool {
    let bucket_matches = buckets.iter().any(|bucket| match bucket.as_str() {
        "all" => true,
        "stable" => summary.kind == "release",
        "snapshot" => summary.kind == "snapshot" && !is_foolsday(&summary.id),
        "foolsday" => is_foolsday(&summary.id),
        "legacy" => summary.kind == "old_beta" || summary.kind == "old_alpha",
        _ => false,
    });
    bucket_matches
        && (patterns.is_empty()
            || patterns
                .iter()
                .any(|pattern| wildcard_match(pattern, &summary.id)))
}

fn is_foolsday(id: &str) -> bool {
    matches!(
        id,
        "15w14a"
            | "1.RV-Pre1"
            | "3D Shareware v1.34"
            | "20w14infinite"
            | "22w13oneblockatatime"
            | "23w13a_or_b"
            | "24w14potato"
    )
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let value: Vec<char> = value.to_lowercase().chars().collect();
    let (mut pi, mut vi) = (0usize, 0usize);
    let mut star = None;
    let mut mark = 0usize;
    while vi < value.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == value[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = vi;
        } else if let Some(position) = star {
            pi = position + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn resource_search(request: &Request, catalog: &Catalog) -> Response {
    let kind = request
        .params
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty());
    if let Some(kind) = kind {
        if !matches!(
            kind,
            "mod" | "modpack" | "resourcepack" | "shader" | "datapack" | "plugin" | "world"
        ) {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format("rpc.resource.kind", &[("kind", kind)]),
            );
        }
    }
    let query = request
        .params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let limit = request
        .params
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(20, |value| value.clamp(1, 100));
    let limit = usize::try_from(limit).unwrap_or(100);
    let versions = string_array(request.params.get("game_versions"));
    let loaders = string_array(request.params.get("loaders"));
    let sort_by = request
        .params
        .get("sort_by")
        .and_then(Value::as_str)
        .unwrap_or("relevance");
    if request.params.get("api").and_then(Value::as_str) == Some("curseforge") {
        return resource_search_curseforge(
            request, catalog, kind, query, limit, &versions, &loaders, sort_by,
        );
    }
    let index = match sort_by {
        "create" => "newest",
        "download" => "downloads",
        "name" | "relevance" => "relevance",
        other => {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format("cli.error.sort_key", &[("value", other)]),
            );
        }
    };
    match ModrinthClient::new().search(query, kind, &versions, &loaders, index, limit) {
        Ok(mut search) => {
            let order = request
                .params
                .get("order")
                .and_then(Value::as_str)
                .unwrap_or("asc");
            sort_search(&mut search.hits, sort_by, order);
            Response::result(request.id.clone(), json!(search))
        }
        Err(error) => Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.format("rpc.resource.search", &[("error", &error.to_string())]),
        ),
    }
}

#[allow(clippy::too_many_arguments)] // 查询参数较多，保持扁平调用
fn resource_search_curseforge(
    request: &Request,
    catalog: &Catalog,
    kind: Option<&str>,
    query: &str,
    limit: usize,
    versions: &[String],
    loaders: &[String],
    sort_by: &str,
) -> Response {
    let sort = match sort_by {
        "create" => CurseForgeSort::Updated,
        "download" => CurseForgeSort::Downloads,
        "name" => CurseForgeSort::Name,
        "relevance" => CurseForgeSort::Relevance,
        other => {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format("cli.error.sort_key", &[("value", other)]),
            );
        }
    };
    let order = request
        .params
        .get("order")
        .and_then(Value::as_str)
        .unwrap_or("asc");
    let client = match CurseForgeClient::new() {
        Ok(client) => client,
        Err(error) => return curseforge_fault(request, catalog, &error),
    };
    match client.search(
        query,
        kind,
        versions.first().map(String::as_str),
        loaders.first().map(String::as_str),
        sort,
        order,
        limit,
        0,
    ) {
        Ok((hits, total)) => Response::result(
            request.id.clone(),
            json!({
                "hits": hits.iter().map(curseforge_hit).collect::<Vec<Value>>(),
                "total_hits": total,
            }),
        ),
        Err(error) => curseforge_fault(request, catalog, &error),
    }
}

fn curseforge_hit(hit: &CurseForgeHit) -> Value {
    json!({
        "project_id": hit.id.to_string(),
        "slug": hit.slug,
        "title": hit.title,
        "description": hit.description,
        "project_type": hit.kind,
        "downloads": hit.downloads,
        "date_created": hit.date_created,
        "icon_url": hit.icon_url,
    })
}

fn curseforge_fault(request: &Request, catalog: &Catalog, error: &CurseForgeError) -> Response {
    if matches!(error, CurseForgeError::MissingKey) {
        return Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.text("rpc.resource.curseforge_key").to_owned(),
        );
    }
    Response::fault(
        request.id.clone(),
        INTERNAL_ERROR,
        catalog.format("rpc.resource.curseforge", &[("error", &error.to_string())]),
    )
}

fn sort_search(hits: &mut [SearchHit], sort_by: &str, order: &str) {
    match sort_by {
        "create" => hits.sort_by(|left, right| left.date_created.cmp(&right.date_created)),
        "download" => hits.sort_by_key(|hit| hit.downloads),
        "name" => {
            hits.sort_by_key(|hit| hit.title.to_lowercase());
        }
        _ => return,
    }
    if order == "desc" {
        hits.reverse();
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn copy_instance_defaults(data: &std::path::Path, root: &std::path::Path) {
    let config = data.join("config");
    if config.is_dir() {
        let target = root.join("config");
        if !target.exists() {
            let _ = copy_dir(&config, &target);
        }
    }
    let options = data.join("options.txt");
    if options.is_file() {
        let target = root.join("options.txt");
        if !target.exists() {
            let _ = fs::copy(&options, &target);
        }
    }
}

fn copy_server_defaults(data: &std::path::Path, root: &std::path::Path) {
    for name in [
        "server.properties",
        "bukkit.yml",
        "spigot.yml",
        "purpur.yml",
    ] {
        let source = data.join(name);
        if source.is_file() {
            let target = root.join(name);
            if !target.exists() {
                let _ = fs::copy(&source, &target);
            }
        }
    }
    let config = data.join("config-server");
    if config.is_dir() {
        let target = root.join("config");
        if !target.exists() {
            let _ = copy_dir(&config, &target);
        }
    }
}

fn copy_dir(source: &std::path::Path, target: &std::path::Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &destination)?;
        } else {
            fs::copy(&path, &destination)?;
        }
    }
    Ok(())
}

fn account_info(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(raw) = request.params.get("id").and_then(Value::as_str) else {
        return account_id_error(request, catalog);
    };
    let Ok(id) = AccountId::new(raw) else {
        return account_id_error(request, catalog);
    };
    match services.accounts.get(&id) {
        Ok(Some(account)) => Response::result(request.id.clone(), json!(account)),
        Ok(None) => Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("rpc.account.not_found", &[("value", raw)]),
        ),
        Err(error) => response_error(request, catalog, "rpc.account.lookup", &error),
    }
}

fn account_list(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    match services.accounts.list() {
        Ok(accounts) => Response::result(request.id.clone(), json!(accounts)),
        Err(error) => response_error(request, catalog, "rpc.account.list", &error),
    }
}

#[allow(clippy::too_many_lines)] // account kind validation; split when more kinds are added
fn account_add(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(kind) = request
        .params
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
    else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.account.kind_missing").to_owned(),
        );
    };
    let Some(name) = account_name(request, catalog) else {
        return account_name_error(request, catalog);
    };
    let id = request
        .params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let server = request
        .params
        .get("server")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|server| !server.is_empty())
        .map(str::to_owned);
    let token = request
        .params
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let refresh = request
        .params
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let (kind, id, server, token) = match kind {
        "offline" => {
            for field in ["id", "server", "access_token", "refresh_token"] {
                if request.params.get(field).is_some() {
                    return account_field_error(request, catalog, kind, field);
                }
            }
            (Kind::Offline, offline_uuid(name), None, None)
        }
        "external" => {
            if request.params.get("access_token").is_some() {
                return account_field_error(request, catalog, kind, "access_token");
            }
            if request.params.get("refresh_token").is_some() {
                return account_field_error(request, catalog, kind, "refresh_token");
            }
            let Some(server) = server else {
                return Response::fault(
                    request.id.clone(),
                    INVALID_PARAMS,
                    catalog.text("rpc.account.server").to_owned(),
                );
            };
            (
                Kind::External,
                id.unwrap_or_else(|| offline_uuid(name)),
                Some(server),
                None,
            )
        }
        "microsoft" => {
            if request.params.get("server").is_some() {
                return account_field_error(request, catalog, kind, "server");
            }
            let Some(id) = id else {
                return account_id_error(request, catalog);
            };
            (Kind::Microsoft, id, None, token)
        }
        other => {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format("rpc.account.kind", &[("kind", other)]),
            );
        }
    };

    let Ok(id) = AccountId::new(id) else {
        return account_id_error(request, catalog);
    };
    let account = Account {
        id,
        kind,
        name: name.to_owned(),
        server,
        created_at: 0,
        last_used_at: None,
        run_seconds: 0,
    };
    add_account(request, services, catalog, &account, token, refresh)
}

fn account_field_error(request: &Request, catalog: &Catalog, kind: &str, field: &str) -> Response {
    Response::fault(
        request.id.clone(),
        INVALID_PARAMS,
        catalog.format("rpc.account.field", &[("kind", kind), ("field", field)]),
    )
}

fn add_account(
    request: &Request,
    services: &Services,
    catalog: &Catalog,
    account: &Account,
    token: Option<String>,
    refresh: Option<String>,
) -> Response {
    let mut account = account.clone();
    if account.created_at == 0 {
        account.created_at = now_unix();
    }
    if let Err(error) = services.accounts.add(&account) {
        return response_error(request, catalog, "rpc.account.add", &error);
    }
    if let Some(token) = token {
        if let Ok(mut tokens) = services.tokens.lock() {
            tokens.insert(account.id.clone(), token.clone());
        }
        if let Some(refresh) = refresh {
            let _ = services.tokens_store.set(
                account.id.as_str(),
                &TokenSet {
                    refresh_token: refresh,
                    access_token: Some(token),
                    expires_at: None,
                },
            );
        }
    } else if let Some(refresh) = refresh {
        let _ = services.tokens_store.set(
            account.id.as_str(),
            &TokenSet {
                refresh_token: refresh,
                access_token: None,
                expires_at: None,
            },
        );
    }
    Response::result(request.id.clone(), json!(account))
}

fn refresh_tokens(services: &Services, catalog: &Catalog) {
    let accounts = match services.accounts.list() {
        Ok(accounts) => accounts,
        Err(error) => {
            log(
                catalog,
                "service.token.store",
                &[("error", &error.to_string())],
            );
            return;
        }
    };
    let mut stored = Vec::new();
    for account in accounts {
        match services.tokens_store.get(account.id.as_str()) {
            Ok(Some(token)) => stored.push((account.id.as_str().to_owned(), token)),
            Ok(None) => {}
            Err(error) => log(
                catalog,
                "service.token.store",
                &[("error", &error.to_string())],
            ),
        }
    }
    if stored.is_empty() {
        return;
    }
    for (id, token) in &stored {
        if let Some(access) = &token.access_token {
            if let Ok(id) = AccountId::new(id.clone()) {
                if let Ok(mut tokens) = services.tokens.lock() {
                    tokens.insert(id, access.clone());
                }
            }
        }
    }
    let Ok(config) = msa::Config::from_env() else {
        log(catalog, "service.token.client", &[]);
        return;
    };
    let accounts = match services.accounts.list() {
        Ok(accounts) => accounts,
        Err(error) => {
            log(
                catalog,
                "service.token.store",
                &[("error", &error.to_string())],
            );
            return;
        }
    };
    for account in accounts {
        if account.kind != Kind::Microsoft {
            continue;
        }
        let Some(token) = stored
            .iter()
            .find(|(id, _)| id == account.id.as_str())
            .map(|(_, token)| token)
        else {
            continue;
        };
        match msa::refresh(&config, &token.refresh_token) {
            Ok(login) => {
                if let Ok(mut tokens) = services.tokens.lock() {
                    tokens.insert(account.id.clone(), login.access_token.clone());
                }
                let expires_at = login
                    .expires_in
                    .map(|seconds| now_unix().saturating_add(seconds));
                let refresh = login
                    .refresh_token
                    .unwrap_or_else(|| token.refresh_token.clone());
                let _ = services.tokens_store.set(
                    account.id.as_str(),
                    &TokenSet {
                        refresh_token: refresh,
                        access_token: Some(login.access_token),
                        expires_at,
                    },
                );
            }
            Err(error) => log(
                catalog,
                "service.token.refresh",
                &[
                    ("account", account.name.as_str()),
                    ("error", &error.to_string()),
                ],
            ),
        }
    }
}

fn touch_account(services: &Services, id: &AccountId, timestamp: u64) {
    if let Ok(Some(mut account)) = services.accounts.get(id) {
        account.last_used_at = Some(timestamp);
        let _ = services.accounts.update(&account);
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn account_remove(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(raw) = request.params.get("id").and_then(Value::as_str) else {
        return account_id_error(request, catalog);
    };
    let Ok(id) = AccountId::new(raw) else {
        return account_id_error(request, catalog);
    };
    match services.accounts.remove(&id) {
        Ok(()) => {
            if let Ok(mut tokens) = services.tokens.lock() {
                tokens.remove(&id);
            }
            let _ = services.tokens_store.remove(id.as_str());
            if let Ok(mut settings) = services.settings.load() {
                if settings.default_account.as_deref() == Some(id.as_str()) {
                    settings.default_account = None;
                    let _ = services.settings.save(&settings);
                }
            }
            Response::result(request.id.clone(), json!({"deleted": true}))
        }
        Err(error) => response_error(request, catalog, "rpc.account.remove", &error),
    }
}

fn account_name<'a>(request: &'a Request, _catalog: &Catalog) -> Option<&'a str> {
    request
        .params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

fn account_name_error(request: &Request, catalog: &Catalog) -> Response {
    Response::fault(
        request.id.clone(),
        INVALID_PARAMS,
        catalog.text("rpc.account.name").to_owned(),
    )
}

fn account_id_error(request: &Request, catalog: &Catalog) -> Response {
    Response::fault(
        request.id.clone(),
        INVALID_PARAMS,
        catalog.text("rpc.account.id").to_owned(),
    )
}

fn instance_default(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(raw) = request.params.get("id").and_then(Value::as_str) else {
        return invalid_id(request, catalog);
    };
    let Ok(id) = Id::new(raw) else {
        return invalid_id(request, catalog);
    };
    let mut settings = match services.settings.load() {
        Ok(settings) => settings,
        Err(error) => return response_error(request, catalog, "rpc.instance.default", &error),
    };
    settings.default_instance = Some(id.as_str().to_owned());
    match services.settings.save(&settings) {
        Ok(()) => Response::result(request.id.clone(), json!({"default_instance": id})),
        Err(error) => response_error(request, catalog, "rpc.instance.default", &error),
    }
}

fn account_refresh(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Ok(config) = msa::Config::from_env() else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("service.token.client").to_owned(),
        );
    };
    let target = request
        .params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let selected = if let Some(target) = target {
        match resolve_account(services, catalog, target) {
            Ok(account) => vec![account],
            Err(message) => {
                return Response::fault(request.id.clone(), INVALID_PARAMS, message);
            }
        }
    } else {
        match services.accounts.list() {
            Ok(accounts) => accounts
                .into_iter()
                .filter(|account| account.kind == Kind::Microsoft)
                .collect(),
            Err(error) => return response_error(request, catalog, "rpc.account.list", &error),
        }
    };
    let mut refreshed = Vec::new();
    let mut failed = Vec::new();
    for account in selected {
        if account.kind != Kind::Microsoft {
            failed.push(json!({
                "id": account.id,
                "name": account.name,
                "error": "not a Microsoft account",
            }));
            continue;
        }
        let token = match services.tokens_store.get(account.id.as_str()) {
            Ok(Some(token)) => token,
            Ok(None) => {
                failed.push(json!({
                    "id": account.id,
                    "name": account.name,
                    "error": "no stored refresh token",
                }));
                continue;
            }
            Err(error) => {
                failed.push(json!({
                    "id": account.id,
                    "name": account.name,
                    "error": error.to_string(),
                }));
                continue;
            }
        };
        match msa::refresh(&config, &token.refresh_token) {
            Ok(login) => {
                if let Ok(mut tokens) = services.tokens.lock() {
                    tokens.insert(account.id.clone(), login.access_token.clone());
                }
                let expires_at = login
                    .expires_in
                    .map(|seconds| now_unix().saturating_add(seconds));
                let refresh = login
                    .refresh_token
                    .unwrap_or_else(|| token.refresh_token.clone());
                let _ = services.tokens_store.set(
                    account.id.as_str(),
                    &TokenSet {
                        refresh_token: refresh,
                        access_token: Some(login.access_token),
                        expires_at,
                    },
                );
                refreshed.push(json!({
                    "id": account.id,
                    "name": account.name,
                    "expires_at": expires_at,
                }));
            }
            Err(error) => failed.push(json!({
                "id": account.id,
                "name": account.name,
                "error": error.to_string(),
            })),
        }
    }
    Response::result(
        request.id.clone(),
        json!({"refreshed": refreshed, "failed": failed}),
    )
}

fn account_default(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(value) = request
        .params
        .get("value")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return account_default_query(request, services, catalog);
    };
    let accounts = match services.accounts.list() {
        Ok(accounts) => accounts,
        Err(error) => return response_error(request, catalog, "rpc.account.default", &error),
    };
    let account = if let Some(account) = accounts
        .iter()
        .find(|account| account.id.as_str() == value)
        .cloned()
    {
        account
    } else {
        let matches: Vec<Account> = accounts
            .iter()
            .filter(|account| account.name == value)
            .cloned()
            .collect();
        match matches.len() {
            0 => {
                return Response::fault(
                    request.id.clone(),
                    INVALID_PARAMS,
                    catalog.format("rpc.account.not_found", &[("value", value)]),
                );
            }
            1 => matches.into_iter().next().expect("one match"),
            _ => {
                let ids = matches
                    .iter()
                    .map(|account| account.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Response::fault(
                    request.id.clone(),
                    INVALID_PARAMS,
                    catalog.format("rpc.account.ambiguous", &[("ids", &ids)]),
                );
            }
        }
    };

    let mut settings = match services.settings.load() {
        Ok(settings) => settings,
        Err(error) => return response_error(request, catalog, "rpc.account.default", &error),
    };
    settings.default_account = Some(account.id.as_str().to_owned());
    match services.settings.save(&settings) {
        Ok(()) => Response::result(request.id.clone(), json!(account)),
        Err(error) => response_error(request, catalog, "rpc.account.default", &error),
    }
}

fn account_default_query(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let settings = match services.settings.load() {
        Ok(settings) => settings,
        Err(error) => return response_error(request, catalog, "rpc.account.default", &error),
    };
    let Some(default) = settings.default_account else {
        return Response::result(request.id.clone(), Value::Null);
    };
    let Ok(id) = AccountId::new(default) else {
        return Response::result(request.id.clone(), Value::Null);
    };
    match services.accounts.get(&id) {
        Ok(Some(account)) => Response::result(request.id.clone(), json!(account)),
        Ok(None) => Response::result(request.id.clone(), Value::Null),
        Err(error) => response_error(request, catalog, "rpc.account.default", &error),
    }
}

fn instance_groups(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let mut groups = match services.groups.list() {
        Ok(groups) => groups,
        Err(error) => return response_error(request, catalog, "rpc.instance.groups", &error),
    };
    match services.repository.list() {
        Ok(instances) => {
            for instance in instances {
                if let Some(group) = instance.group {
                    groups.push(group);
                }
            }
        }
        Err(error) => return response_error(request, catalog, "rpc.instance.list", &error),
    }
    groups.sort();
    groups.dedup();
    Response::result(request.id.clone(), json!(groups))
}

fn default_instance(services: &Services, catalog: &Catalog) -> Result<Id, (i32, String)> {
    let settings = services
        .settings
        .load()
        .map_err(|error| (INTERNAL_ERROR, error.to_string()))?;
    let Some(raw) = settings.default_instance else {
        return Err((
            INVALID_PARAMS,
            catalog.text("rpc.instance.default_missing").to_owned(),
        ));
    };
    Id::new(raw).map_err(|error| (INVALID_PARAMS, error.to_string()))
}

fn instance_java(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let id = match instance_id(request) {
        Some(id) => id,
        None => match default_instance(services, catalog) {
            Ok(id) => id,
            Err((code, message)) => {
                return Response::fault(request.id.clone(), code, message);
            }
        },
    };
    let instance = match services.repository.load(&id) {
        Ok(instance) => instance,
        Err(error) => return response_error(request, catalog, "rpc.instance.info", &error),
    };
    let (source, java, major) = if let Some(path) = &instance.config.java_path {
        let java = java_executable(path);
        let major = arkmcl_storage::java::inspect(&java)
            .ok()
            .map(|runtime| runtime.major);
        ("path", java, major)
    } else {
        let suggested = if let Some(version) = instance.config.java_version {
            version
        } else {
            let Some(base) = instance.profile.minecraft() else {
                return Response::fault(
                    request.id.clone(),
                    INTERNAL_ERROR,
                    catalog.text("rpc.launch.minecraft").to_owned(),
                );
            };
            let version = match services.fetcher.version(&base.version) {
                Ok(version) => version,
                Err(error) => {
                    return Response::fault(
                        request.id.clone(),
                        INTERNAL_ERROR,
                        catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
                    );
                }
            };
            version.java_version.as_ref().map_or(
                if instance.kind == InstanceKind::Server {
                    21
                } else {
                    8
                },
                |java| java.major_version,
            )
        };
        let Some(runtime) = arkmcl_storage::java::select(suggested) else {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.launch.java", &[("major", &suggested.to_string())]),
            );
        };
        let source = if instance.config.java_version.is_some() {
            "version"
        } else {
            "default"
        };
        (source, runtime.path, Some(runtime.major))
    };
    Response::result(
        request.id.clone(),
        json!({
            "id": instance.id,
            "source": source,
            "java": java.display().to_string(),
            "version": major,
        }),
    )
}

fn instance_info(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let id = match instance_id(request) {
        Some(id) => id,
        None => match default_instance(services, catalog) {
            Ok(id) => id,
            Err((code, message)) => {
                return Response::fault(request.id.clone(), code, message);
            }
        },
    };
    match services.repository.load(&id) {
        Ok(instance) => Response::result(request.id.clone(), json!(instance)),
        Err(error) => response_error(request, catalog, "rpc.instance.info", &error),
    }
}

#[allow(clippy::too_many_lines)] // orchestration; split when more steps are added
fn instance_update(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let mut instance = match services.repository.load(&id) {
        Ok(instance) => instance,
        Err(error) => return response_error(request, catalog, "rpc.instance.info", &error),
    };
    let previous_group = instance.group.clone();
    if let Some(value) = request.params.get("type") {
        let Some(kind) = parse_kind(value) else {
            return invalid_field(request, catalog, "type");
        };
        if kind != instance.kind {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.text("rpc.instance.type_fixed").to_owned(),
            );
        }
    }
    match arkmcl_storage::lock::Guard::check(&LocalFileSystem, &instance.root) {
        Ok(()) => {}
        Err(arkmcl_storage::lock::Error::Locked) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.instance.update_locked").to_owned(),
            );
        }
        Err(error) => return response_error(request, catalog, "rpc.instance.update", &error),
    }

    if let Some(name) = request.params.get("name").and_then(Value::as_str) {
        name.clone_into(&mut instance.name);
    }
    if let Some(group) = request.params.get("group") {
        if group.is_null() {
            instance.group = None;
        } else if let Some(group) = group.as_str() {
            let group = group.trim();
            instance.group = if group.is_empty() {
                None
            } else {
                Some(group.to_owned())
            };
        } else {
            return invalid_field(request, catalog, "group");
        }
    }
    if let Some(value) = request.params.get("account") {
        if value.is_null() {
            instance.account = None;
        } else if let Some(account) = value.as_str() {
            let account = account.trim();
            instance.account = if account.is_empty() {
                None
            } else {
                match resolve_account(services, catalog, account) {
                    Ok(account) => Some(account.id.as_str().to_owned()),
                    Err(message) => {
                        return Response::fault(request.id.clone(), INVALID_PARAMS, message);
                    }
                }
            };
        } else {
            return invalid_field(request, catalog, "account");
        }
    }
    if let Some(value) = request.params.get("memory_mb") {
        let Some(value) = value.as_u64().and_then(|value| u32::try_from(value).ok()) else {
            return invalid_field(request, catalog, "memory_mb");
        };
        instance.config.memory_mb = value;
    }
    if let Some(value) = request.params.get("window_width") {
        let Some(value) = value.as_u64().and_then(|value| u32::try_from(value).ok()) else {
            return invalid_field(request, catalog, "window_width");
        };
        instance.config.window_width = value;
    }
    if let Some(value) = request.params.get("fullscreen") {
        let Some(value) = value.as_bool() else {
            return invalid_field(request, catalog, "fullscreen");
        };
        instance.config.fullscreen = value;
    }
    if let Some(value) = request.params.get("java_path") {
        if value.is_null() {
            instance.config.java_path = None;
        } else if let Some(path) = value.as_str() {
            instance.config.java_path = Some(PathBuf::from(path));
        } else {
            return invalid_field(request, catalog, "java_path");
        }
    }
    if let Some(value) = request.params.get("java") {
        let Some(raw) = value.as_str() else {
            return invalid_field(request, catalog, "java");
        };
        match parse_java(raw, catalog) {
            Ok(JavaChoice::Default) => {
                instance.config.java_path = None;
                instance.config.java_version = None;
            }
            Ok(JavaChoice::Version(version)) => {
                instance.config.java_path = None;
                instance.config.java_version = Some(version);
            }
            Ok(JavaChoice::Path(path)) => {
                instance.config.java_version = None;
                instance.config.java_path = Some(path);
            }
            Err((code, message)) => return Response::fault(request.id.clone(), code, message),
        }
    }
    if let Some(value) = request.params.get("lwjgl") {
        if value.is_null() {
            instance.config.lwjgl = None;
        } else if let Some(text) = value.as_str() {
            let text = text.trim();
            instance.config.lwjgl = if text.is_empty() || text.eq_ignore_ascii_case("default") {
                None
            } else if valid_lwjgl(text) {
                Some(text.to_owned())
            } else {
                return invalid_field(request, catalog, "lwjgl");
            };
        } else {
            return invalid_field(request, catalog, "lwjgl");
        }
    }
    if let Some(value) = request.params.get("jvm_args") {
        let Some(args) = string_list(value) else {
            return invalid_field(request, catalog, "jvm_args");
        };
        instance.config.jvm_args = args;
    }
    if let Some(value) = request.params.get("game_args") {
        let Some(args) = string_list(value) else {
            return invalid_field(request, catalog, "game_args");
        };
        instance.config.game_args = args;
    }

    let mut probe = instance.clone();
    let mut profile_changed = false;
    if let Some(value) = request.params.get("minecraft") {
        let Some(version) = value
            .as_str()
            .map(str::trim)
            .filter(|version| !version.is_empty())
        else {
            return invalid_field(request, catalog, "minecraft");
        };
        let changed = probe
            .profile
            .minecraft()
            .is_none_or(|component| component.version != version);
        if changed {
            probe.profile.set_minecraft(version);
            profile_changed = true;
        }
    }

    let loader = request
        .params
        .get("loader")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|loader| !loader.is_empty());
    let loader_version = request
        .params
        .get("loader_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|version| !version.is_empty());
    let server_instance = instance.kind == InstanceKind::Server;
    if let Some(loader) = loader {
        if !loader.eq_ignore_ascii_case("none")
            && !loader_allowed(probe.kind, &loader.to_ascii_lowercase())
        {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format(
                    "rpc.instance.loader_kind",
                    &[("loader", loader), ("kind", kind_label(probe.kind))],
                ),
            );
        }
        match loader.to_ascii_lowercase().as_str() {
            "fabric" => {
                probe.profile.remove(FORGE);
                probe.profile.remove(NEOFORGE);
                probe.profile.remove(PURPUR);
                if let Err(message) = apply_fabric(&mut probe, services, catalog, loader_version) {
                    return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
                }
                profile_changed = true;
            }
            "forge" => {
                probe.profile.remove(FABRIC);
                probe.profile.remove(NEOFORGE);
                probe.profile.remove(PURPUR);
                if let Err(message) = apply_forge(
                    &mut probe,
                    services,
                    catalog,
                    loader_version,
                    ForgeKind::Forge,
                    server_instance,
                ) {
                    return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
                }
                profile_changed = true;
            }
            "neoforge" => {
                probe.profile.remove(FABRIC);
                probe.profile.remove(FORGE);
                probe.profile.remove(PURPUR);
                if let Err(message) = apply_forge(
                    &mut probe,
                    services,
                    catalog,
                    loader_version,
                    ForgeKind::NeoForge,
                    server_instance,
                ) {
                    return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
                }
                profile_changed = true;
            }
            "purpur" => {
                probe.profile.remove(FABRIC);
                probe.profile.remove(FORGE);
                probe.profile.remove(NEOFORGE);
                if let Err(message) = apply_purpur(&mut probe, services, catalog, loader_version) {
                    return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
                }
                profile_changed = true;
            }
            "none" => {
                probe.profile.remove(FABRIC);
                probe.profile.remove(FORGE);
                probe.profile.remove(NEOFORGE);
                probe.profile.remove(PURPUR);
                profile_changed = true;
            }
            other => {
                return Response::fault(
                    request.id.clone(),
                    INVALID_PARAMS,
                    catalog.format("rpc.instance.loader", &[("loader", other)]),
                );
            }
        }
    } else if let Some(loader_version) = loader_version {
        let fabric = probe
            .profile
            .fabric()
            .map(|component| component.version.clone());
        if fabric.is_some() {
            if let Err(message) = apply_fabric(&mut probe, services, catalog, Some(loader_version))
            {
                return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
            }
        } else if let Some(kind) = existing_forge_kind(&probe.profile) {
            if let Err(message) = apply_forge(
                &mut probe,
                services,
                catalog,
                Some(loader_version),
                kind,
                server_instance,
            ) {
                return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
            }
        } else {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.text("rpc.instance.loader_version").to_owned(),
            );
        }
        profile_changed = true;
    } else if profile_changed {
        let fabric = probe
            .profile
            .fabric()
            .map(|component| component.version.clone());
        if let Some(current) = fabric {
            if let Err(message) = apply_fabric(&mut probe, services, catalog, Some(&current)) {
                return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
            }
        } else if let Some(kind) = existing_forge_kind(&probe.profile) {
            let current = probe
                .profile
                .components
                .iter()
                .find(|component| component.uid == FORGE || component.uid == NEOFORGE)
                .map(|component| component.version.clone())
                .unwrap_or_default();
            if let Err(message) = apply_forge(
                &mut probe,
                services,
                catalog,
                Some(&current),
                kind,
                server_instance,
            ) {
                return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
            }
        }
    }

    if profile_changed {
        instance.profile = probe.profile;
        instance.state = State::NotInstalled;
    }

    match services.repository.save(&instance) {
        Ok(()) => {
            sync_group(
                services,
                previous_group.as_deref(),
                instance.group.as_deref(),
            );
            Response::result(request.id.clone(), json!(instance))
        }
        Err(error) => response_error(request, catalog, "rpc.instance.update", &error),
    }
}

/// 实例变更分组后同步 groups.json：进入即创建，最后一个离开即删除。
fn sync_group(services: &Services, previous: Option<&str>, current: Option<&str>) {
    if previous == current {
        return;
    }
    if let Some(group) = current {
        let _ = services.groups.add(group);
    }
    let Some(group) = previous else {
        return;
    };
    let still_used = services.repository.list().map_or(true, |instances| {
        instances
            .iter()
            .any(|instance| instance.group.as_deref() == Some(group))
    });
    if !still_used {
        let _ = services.groups.remove(group);
    }
}

fn settings_get(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    match services.settings.load() {
        Ok(settings) => Response::result(request.id.clone(), json!(settings.instance)),
        Err(error) => response_error(request, catalog, "rpc.settings.get", &error),
    }
}

#[allow(clippy::too_many_lines)] // 逐字段校验全局默认项；拆分反而降低可读性
fn settings_update(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let mut settings = match services.settings.load() {
        Ok(settings) => settings,
        Err(error) => return response_error(request, catalog, "rpc.settings.update", &error),
    };
    if let Some(value) = request.params.get("version") {
        if value.is_null() {
            settings.instance.version = None;
        } else if let Some(text) = value.as_str() {
            let text = text.trim();
            settings.instance.version = if text.is_empty() {
                None
            } else {
                Some(text.to_owned())
            };
        } else {
            return invalid_field(request, catalog, "version");
        }
    }
    if let Some(value) = request.params.get("memory_mb") {
        let Some(memory) = value.as_u64().and_then(|value| u32::try_from(value).ok()) else {
            return invalid_field(request, catalog, "memory_mb");
        };
        if memory < 256 {
            return invalid_field(request, catalog, "memory_mb");
        }
        settings.instance.memory_mb = memory;
    }
    if let Some(value) = request.params.get("jvm_args") {
        let Some(items) = value.as_array() else {
            return invalid_field(request, catalog, "jvm_args");
        };
        settings.instance.jvm_args = items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    if let Some(value) = request.params.get("group") {
        if value.is_null() {
            settings.instance.group = None;
        } else if let Some(text) = value.as_str() {
            let text = text.trim();
            settings.instance.group = if text.is_empty() {
                None
            } else {
                Some(text.to_owned())
            };
        } else {
            return invalid_field(request, catalog, "group");
        }
    }
    if let Some(value) = request.params.get("linking") {
        let Some(text) = value.as_str() else {
            return invalid_field(request, catalog, "linking");
        };
        let text = text.trim().to_ascii_lowercase();
        if !matches!(text.as_str(), "all" | "none") {
            return invalid_field(request, catalog, "linking");
        }
        settings.instance.linking = text;
    }
    if let Some(value) = request.params.get("java") {
        let Some(text) = value.as_str() else {
            return invalid_field(request, catalog, "java");
        };
        let text = text.trim();
        if text.is_empty() {
            return invalid_field(request, catalog, "java");
        }
        if text.eq_ignore_ascii_case("default") {
            "default".clone_into(&mut settings.instance.java);
        } else if let Ok(version) = text.parse::<u32>() {
            if version == 0 {
                return invalid_field(request, catalog, "java");
            }
            settings.instance.java = version.to_string();
        } else {
            text.clone_into(&mut settings.instance.java);
        }
    }
    if let Some(value) = request.params.get("type") {
        let Some(kind) = parse_kind(value) else {
            return invalid_field(request, catalog, "type");
        };
        settings.instance.kind = kind;
    }
    if let Some(value) = request.params.get("loader") {
        if value.is_null() {
            settings.instance.loader = None;
        } else if let Some(text) = value.as_str() {
            let text = text.trim().to_ascii_lowercase();
            if text.is_empty() || text == "none" {
                settings.instance.loader = None;
            } else if matches!(text.as_str(), "fabric" | "forge" | "neoforge" | "purpur") {
                settings.instance.loader = Some(text);
            } else {
                return invalid_field(request, catalog, "loader");
            }
        } else {
            return invalid_field(request, catalog, "loader");
        }
    }
    if let Some(value) = request.params.get("create_defaults") {
        let Some(value) = value.as_bool() else {
            return invalid_field(request, catalog, "create_defaults");
        };
        settings.instance.create_defaults = value;
    }
    if let Some(loader) = &settings.instance.loader {
        if !loader_allowed(settings.instance.kind, loader) {
            return invalid_field(request, catalog, "loader");
        }
    }
    match services.settings.save(&settings) {
        Ok(()) => Response::result(request.id.clone(), json!(settings.instance)),
        Err(error) => response_error(request, catalog, "rpc.settings.update", &error),
    }
}

fn instance_link(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let instance = match services.repository.load(&id) {
        Ok(instance) => instance,
        Err(error) => return response_error(request, catalog, "rpc.instance.info", &error),
    };
    let remove = request
        .params
        .get("remove")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let allowed = arkmcl_storage::links::link_names(instance.kind);
    let names: Vec<String> = match request.params.get("links") {
        Some(Value::String(value)) if value == "all" => {
            allowed.iter().map(|name| (*name).to_owned()).collect()
        }
        Some(value) => string_array(Some(value)),
        None => return invalid_field(request, catalog, "links"),
    };
    if names.is_empty() {
        return invalid_field(request, catalog, "links");
    }
    for name in &names {
        if !allowed.contains(&name.as_str()) {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format(
                    "rpc.instance.link_kind",
                    &[("name", name), ("kind", kind_label(instance.kind))],
                ),
            );
        }
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let result = if remove {
        services.linker.remove_selected(&instance.root, &names)
    } else {
        services.linker.create_selected(&instance.root, &names)
    };
    match result {
        Ok(report) => Response::result(
            request.id.clone(),
            json!({
                "id": instance.id,
                "created": paths_to_strings(report.created),
                "skipped": paths_to_strings(report.skipped),
                "removed": paths_to_strings(report.removed),
                "sharing_disabled": report.sharing_disabled,
            }),
        ),
        Err(error) => response_error(request, catalog, "rpc.instance.link", &error),
    }
}

fn instance_links(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let instance = match services.repository.load(&id) {
        Ok(instance) => instance,
        Err(error) => return response_error(request, catalog, "rpc.instance.info", &error),
    };
    let names = arkmcl_storage::links::link_names(instance.kind);
    match services.linker.inspect(&instance.root, names) {
        Ok(states) => Response::result(
            request.id.clone(),
            json!({
                "id": instance.id,
                "kind": kind_label(instance.kind),
                "links": states
                    .into_iter()
                    .map(|state| json!({
                        "name": state.name,
                        "path": state.path,
                        "status": link_status_label(state.status),
                    }))
                    .collect::<Vec<Value>>(),
            }),
        ),
        Err(error) => response_error(request, catalog, "rpc.instance.links", &error),
    }
}

fn link_status_label(status: arkmcl_storage::links::Status) -> &'static str {
    match status {
        arkmcl_storage::links::Status::Linked => "linked",
        arkmcl_storage::links::Status::Real => "real",
        arkmcl_storage::links::Status::Missing => "missing",
        arkmcl_storage::links::Status::Foreign => "foreign",
    }
}

fn instance_move(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let Some(raw) = request
        .params
        .get("to")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return invalid_field(request, catalog, "to");
    };
    let Ok(to) = Id::new(raw) else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.instance.id").to_owned(),
        );
    };
    match services.repository.rename(&id, &to) {
        Ok(instance) => {
            if let Ok(mut settings) = services.settings.load() {
                if settings.default_instance.as_deref() == Some(id.as_str()) {
                    settings.default_instance = Some(to.as_str().to_owned());
                    let _ = services.settings.save(&settings);
                }
            }
            Response::result(request.id.clone(), json!(instance))
        }
        Err(error) => response_error(request, catalog, "rpc.instance.move", &error),
    }
}

fn instance_group_move(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let from = request
        .params
        .get("from")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|group| !group.is_empty());
    let to = request
        .params
        .get("to")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|group| !group.is_empty());
    let (Some(from), Some(to)) = (from, to) else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.instance.group_args").to_owned(),
        );
    };
    if from == to {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.instance.group_same").to_owned(),
        );
    }
    let instances = match services.repository.list() {
        Ok(instances) => instances,
        Err(error) => return response_error(request, catalog, "rpc.instance.group", &error),
    };
    let mut moved = Vec::new();
    for mut instance in instances {
        if instance.group.as_deref() != Some(from) {
            continue;
        }
        instance.group = Some(to.to_owned());
        if let Err(error) = services.repository.save(&instance) {
            return response_error(request, catalog, "rpc.instance.group", &error);
        }
        moved.push(instance.id.as_str().to_owned());
    }
    if !moved.is_empty() {
        let _ = services.groups.add(to);
        sync_group(services, Some(from), None);
    }
    Response::result(
        request.id.clone(),
        json!({"from": from, "to": to, "moved": moved}),
    )
}

fn valid_lwjgl(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '+')
        })
}

fn allocate_id(services: &Services, base: &str) -> Option<Id> {
    let base = Id::from_name(base);
    if services.repository.load(&base).is_err() {
        return Some(base);
    }
    for suffix in 2..1000 {
        let candidate = format!("{}-{suffix}", base.as_str());
        let Ok(id) = Id::new(candidate) else {
            continue;
        };
        if services.repository.load(&id).is_err() {
            return Some(id);
        }
    }
    None
}

fn loader_allowed(kind: InstanceKind, loader: &str) -> bool {
    match kind {
        InstanceKind::Client => matches!(loader, "fabric" | "forge" | "neoforge"),
        InstanceKind::Server => matches!(loader, "purpur" | "fabric" | "forge" | "neoforge"),
    }
}

fn apply_purpur(
    instance: &mut Instance,
    services: &Services,
    catalog: &Catalog,
    requested: Option<&str>,
) -> Result<(), String> {
    let Some(base) = instance
        .profile
        .minecraft()
        .map(|component| component.version.clone())
    else {
        return Err(catalog.text("rpc.instance.minecraft").to_owned());
    };
    let build = purpur::resolve(&base, requested)
        .map_err(|error| catalog.format("rpc.instance.purpur", &[("error", &error.to_string())]))?;
    instance.profile.set_purpur(&build);
    let _ = services;
    Ok(())
}

fn instance_defaults(services: &Services) -> arkmcl_storage::settings::InstanceDefaults {
    services.settings.load().map_or_else(
        |_| arkmcl_storage::settings::InstanceDefaults::default(),
        |settings| settings.instance,
    )
}

fn apply_links(
    services: &Services,
    instance: &Instance,
    mode: &str,
) -> Result<arkmcl_storage::links::Report, arkmcl_storage::links::Error> {
    match mode {
        "none" => Ok(arkmcl_storage::links::Report::default()),
        "all" => services.linker.create_for(&instance.root, instance.kind),
        names => {
            let names: Vec<&str> = names
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect();
            services.linker.create_selected(&instance.root, &names)
        }
    }
}

#[allow(clippy::too_many_lines)] // server jar resolution by loader
fn server_plan(
    instance: &Instance,
    services: &Services,
    catalog: &Catalog,
    sink: &(dyn Fn(Value) + Sync),
    threads: usize,
) -> Result<(Plan, DownloadReport), (i32, String)> {
    let Some(base) = instance
        .profile
        .minecraft()
        .map(|component| component.version.clone())
    else {
        return Err((
            INTERNAL_ERROR,
            catalog.text("rpc.launch.minecraft").to_owned(),
        ));
    };
    let version = services.fetcher.version(&base).map_err(|error| {
        (
            INTERNAL_ERROR,
            catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
        )
    })?;
    let jar = instance.root.join("server.jar");
    let mut downloads = DownloadReport::default();
    let suggested = version
        .java_version
        .as_ref()
        .map_or(21, |java| java.major_version);
    let required = instance.config.java_version.unwrap_or(suggested);
    let java = if let Some(path) = &instance.config.java_path {
        java_executable(path)
    } else {
        arkmcl_storage::java::select(required)
            .map(|runtime| runtime.path)
            .ok_or_else(|| {
                (
                    INTERNAL_ERROR,
                    catalog.format("rpc.launch.java", &[("major", &required.to_string())]),
                )
            })?
    };

    let forge_kind = existing_forge_kind(&instance.profile);
    let forge_loader = instance
        .profile
        .components
        .iter()
        .find(|component| component.uid == FORGE || component.uid == NEOFORGE)
        .map(|component| component.version.clone());
    let fabric_loader = instance
        .profile
        .fabric()
        .map(|component| component.version.clone());

    let mut args_file = None;
    if let Some(kind) = forge_kind {
        let Some(loader) = forge_loader else {
            return Err((
                INTERNAL_ERROR,
                catalog.format(
                    "rpc.launch.prepare",
                    &[("error", "missing Forge loader version")],
                ),
            ));
        };
        let expected = forge::server_args_file(kind, &base, &loader, &instance.root);
        args_file = Some(if expected.is_file() {
            expected
        } else {
            forge::install_server(kind, &base, &loader, &java, &instance.root).map_err(|error| {
                (
                    INTERNAL_ERROR,
                    catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
                )
            })?
        });
    } else if !jar.is_file() {
        let progress = SinkProgress::new(sink);
        if let Some(component) = instance.profile.purpur() {
            purpur::install(
                &base,
                &component.version,
                &jar,
                &services.downloader,
                &progress,
                threads,
            )
            .map_err(|error| {
                (
                    INTERNAL_ERROR,
                    catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
                )
            })?;
        } else if let Some(loader) = &fabric_loader {
            let installer = services.fetcher.fabric_installer().map_err(|error| {
                (
                    INTERNAL_ERROR,
                    catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
                )
            })?;
            let url = format!(
                "https://meta.fabricmc.net/v2/versions/loader/{base}/{loader}/{installer}/server/jar"
            );
            let task = DownloadTask {
                url,
                path: jar.clone(),
                sha1: String::new(),
            };
            downloads = services
                .downloader
                .run_with_threads(&[task], DownloadMode::SkipExisting, &progress, threads)
                .map_err(|error| {
                    (
                        INTERNAL_ERROR,
                        catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
                    )
                })?;
        } else {
            let Some(server) = version
                .downloads
                .as_ref()
                .and_then(|downloads| downloads.server.clone())
            else {
                return Err((
                    INTERNAL_ERROR,
                    catalog.format(
                        "rpc.launch.prepare",
                        &[("error", "version has no server download")],
                    ),
                ));
            };
            let task = DownloadTask {
                url: server.url,
                path: jar,
                sha1: server.sha1,
            };
            downloads = services
                .downloader
                .run_with_threads(&[task], DownloadMode::SkipExisting, &progress, threads)
                .map_err(|error| {
                    (
                        INTERNAL_ERROR,
                        catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
                    )
                })?;
        }
    }
    let mut jvm_args = vec![format!("-Xmx{}M", instance.config.memory_mb)];
    jvm_args.extend(instance.config.jvm_args.iter().cloned());
    let plan = Plan {
        java,
        jar: args_file.is_none().then(|| PathBuf::from("server.jar")),
        args_file,
        main_class: String::new(),
        classpath: Vec::new(),
        natives: Vec::new(),
        jvm_args,
        game_args: vec![if forge_kind.is_some() {
            "nogui".to_owned()
        } else {
            "--nogui".to_owned()
        }],
        natives_dir: instance.root.join("natives"),
        working_dir: instance.root.clone(),
        assets_dir: services.minecraft.join("assets"),
        asset_index: String::new(),
    };
    Ok((plan, downloads))
}

fn kind_label(kind: InstanceKind) -> &'static str {
    match kind {
        InstanceKind::Client => "client",
        InstanceKind::Server => "server",
    }
}

fn parse_kind(value: &Value) -> Option<InstanceKind> {
    match value.as_str()?.trim().to_ascii_lowercase().as_str() {
        "client" => Some(InstanceKind::Client),
        "server" => Some(InstanceKind::Server),
        _ => None,
    }
}

fn paths_to_strings(paths: Vec<PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .map(|path| path.display().to_string())
        .collect()
}

fn apply_fabric(
    instance: &mut Instance,
    services: &Services,
    catalog: &Catalog,
    requested: Option<&str>,
) -> Result<(), String> {
    let Some(base_version) = instance
        .profile
        .minecraft()
        .map(|component| component.version.clone())
    else {
        return Err(catalog.text("rpc.instance.minecraft").to_owned());
    };

    let loader = if let Some(loader) = requested.map(str::trim).filter(|loader| !loader.is_empty())
    {
        loader.to_owned()
    } else {
        match services.fetcher.fabric_loaders(&base_version) {
            Ok(loaders) => match loaders
                .iter()
                .find(|loader| loader.stable)
                .or_else(|| loaders.first())
            {
                Some(loader) => loader.version.clone(),
                None => return Err(catalog.text("rpc.instance.fabric_none").to_owned()),
            },
            Err(error) => {
                return Err(catalog.format("rpc.instance.fabric", &[("error", &error.to_string())]));
            }
        }
    };

    match services.fetcher.fabric_profile(&base_version, &loader) {
        Ok(profile) => {
            if profile.inherits_from.as_deref() != Some(base_version.as_str()) {
                return Err(catalog.format(
                    "rpc.instance.fabric",
                    &[("error", "profile inherits a different Minecraft version")],
                ));
            }
        }
        Err(error) => {
            return Err(catalog.format("rpc.instance.fabric", &[("error", &error.to_string())]));
        }
    }

    instance.profile.set_fabric(&loader);
    Ok(())
}

fn existing_forge_kind(profile: &arkmcl_core::model::Profile) -> Option<ForgeKind> {
    if profile
        .components
        .iter()
        .any(|component| component.uid == FORGE)
    {
        Some(ForgeKind::Forge)
    } else if profile
        .components
        .iter()
        .any(|component| component.uid == NEOFORGE)
    {
        Some(ForgeKind::NeoForge)
    } else {
        None
    }
}

fn apply_forge(
    instance: &mut Instance,
    services: &Services,
    catalog: &Catalog,
    requested: Option<&str>,
    kind: ForgeKind,
    server: bool,
) -> Result<(), String> {
    let Some(base) = instance
        .profile
        .minecraft()
        .map(|component| component.version.clone())
    else {
        return Err(catalog.text("rpc.instance.minecraft").to_owned());
    };
    let loader = forge::resolve(kind, &base, requested)
        .map_err(|error| catalog.format("rpc.instance.forge", &[("error", &error.to_string())]))?;
    if server {
        match kind {
            ForgeKind::Forge => instance.profile.set_forge(&loader),
            ForgeKind::NeoForge => instance.profile.set_neoforge(&loader),
        }
        return Ok(());
    }
    let mut required = services
        .fetcher
        .version(&base)
        .ok()
        .and_then(|version| version.java_version.map(|java| java.major_version))
        .unwrap_or(17);
    if kind == ForgeKind::NeoForge && !loader.starts_with("47.") {
        required = required.max(21);
    }
    let java = instance
        .config
        .java_path
        .clone()
        .or_else(|| arkmcl_storage::java::select(required).map(|runtime| runtime.path))
        .unwrap_or_else(|| PathBuf::from("java"));
    let installed = forge::install(kind, &base, &loader, &java, &services.minecraft)
        .map_err(|error| catalog.format("rpc.instance.forge", &[("error", &error.to_string())]))?;
    match kind {
        ForgeKind::Forge => instance.profile.set_forge(&installed),
        ForgeKind::NeoForge => instance.profile.set_neoforge(&installed),
    }
    Ok(())
}

fn instance_list(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    match services.repository.list() {
        Ok(instances) => Response::result(request.id.clone(), json!(instances)),
        Err(error) => response_error(request, catalog, "rpc.instance.list", &error),
    }
}

#[derive(Debug, Clone)]
enum JavaChoice {
    Default,
    Version(u32),
    Path(PathBuf),
}

struct CreateRequest {
    id: Option<Id>,
    kind: Option<InstanceKind>,
    name: Option<String>,
    defaults: Option<bool>,
    memory_mb: Option<u32>,
    jvm_args: Option<Vec<String>>,
    linking: Option<String>,
    lwjgl: Option<String>,
    java: Option<JavaChoice>,
    group: Option<String>,
    minecraft: Option<String>,
    loader: Option<String>,
    loader_version: Option<String>,
    account: Option<String>,
}

fn parse_java(raw: &str, catalog: &Catalog) -> Result<JavaChoice, (i32, String)> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("default") {
        return Ok(JavaChoice::Default);
    }
    if let Ok(version) = raw.parse::<u32>() {
        if version == 0 {
            return Err((
                INVALID_PARAMS,
                catalog.format("rpc.instance.java", &[("value", raw)]),
            ));
        }
        return Ok(JavaChoice::Version(version));
    }
    if raw.is_empty() {
        return Err((
            INVALID_PARAMS,
            catalog.format("rpc.instance.java", &[("value", raw)]),
        ));
    }
    Ok(JavaChoice::Path(PathBuf::from(raw)))
}

#[allow(clippy::too_many_lines)] // flag validation; split when more fields are added
fn parse_create(request: &Request, catalog: &Catalog) -> Result<CreateRequest, (i32, String)> {
    let name = request
        .params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let defaults = request.params.get("defaults").and_then(Value::as_bool);
    let memory_mb = request
        .params
        .get("memory_mb")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let jvm_args = request
        .params
        .get("jvm_args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<String>>()
        });
    let linking = request
        .params
        .get("linking")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    let id = request
        .params
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(Id::new)
        .transpose()
        .map_err(|error| (INVALID_PARAMS, error.to_string()))?;
    let kind = match request.params.get("type") {
        None => None,
        Some(value) => Some(
            parse_kind(value)
                .ok_or_else(|| (INVALID_PARAMS, catalog.text("rpc.instance.type").to_owned()))?,
        ),
    };
    let loader = request
        .params
        .get("loader")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .filter(|loader| !loader.is_empty());
    let loader_version = request
        .params
        .get("loader_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .map(str::to_owned);
    if loader_version.is_some() && loader.is_none() {
        return Err((
            INVALID_PARAMS,
            catalog.text("rpc.instance.loader_version").to_owned(),
        ));
    }
    if let Some(loader) = &loader {
        if !matches!(
            loader.as_str(),
            "fabric" | "forge" | "neoforge" | "purpur" | "none"
        ) {
            return Err((
                INVALID_PARAMS,
                catalog.format("rpc.instance.loader", &[("loader", loader)]),
            ));
        }
    }

    let minecraft = request
        .params
        .get("minecraft")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|minecraft| !minecraft.is_empty())
        .map(str::to_owned);

    let group = request
        .params
        .get("group")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_owned);
    let account = request
        .params
        .get("account")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|account| !account.is_empty())
        .map(str::to_owned);
    let java = match request.params.get("java") {
        None => None,
        Some(value) => {
            let raw = value.as_str().unwrap_or("");
            Some(parse_java(raw, catalog)?)
        }
    };
    let lwjgl = match request.params.get("lwjgl") {
        None => None,
        Some(value) => match value.as_str().map(str::trim) {
            Some(text) if text.is_empty() || text.eq_ignore_ascii_case("default") => None,
            Some(text) if valid_lwjgl(text) => Some(text.to_owned()),
            _ => {
                return Err((
                    INVALID_PARAMS,
                    catalog.text("rpc.instance.lwjgl").to_owned(),
                ));
            }
        },
    };

    Ok(CreateRequest {
        id,
        kind,
        name,
        defaults,
        memory_mb,
        jvm_args,
        linking,
        lwjgl,
        java,
        group,
        minecraft,
        loader,
        loader_version,
        account,
    })
}

struct CacheGuard(Option<std::path::PathBuf>);

impl Drop for CacheGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn ensure_eula(instance: &Instance) {
    if instance.kind == InstanceKind::Server {
        let path = instance.root.join("eula.txt");
        if !path.exists() {
            let _ = fs::write(path, b"");
        }
    }
}

fn download_mrpack(
    url: &str,
    services: &Services,
    sink: &(dyn Fn(Value) + Sync),
) -> Result<std::path::PathBuf, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let name = format!("mrpack-{:016x}.mrpack", hasher.finish());
    let path = services.minecraft.join("cache").join(name);
    let task = DownloadTask {
        url: url.to_owned(),
        path: path.clone(),
        sha1: String::new(),
    };
    let progress = SinkProgress::new(sink);
    services
        .downloader
        .run_with(&[task], DownloadMode::Overwrite, &progress)
        .map_err(|error| format!("download failed: {error}"))?;
    Ok(path)
}

#[allow(clippy::too_many_lines)] // import orchestration; split when more formats are added
fn instance_import(
    request: &Request,
    services: &Services,
    catalog: &Catalog,
    sink: &(dyn Fn(Value) + Sync),
) -> Response {
    let Some(path) = request
        .params
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
    else {
        return invalid_field(request, catalog, "path");
    };
    let kind = match request.params.get("type") {
        None => InstanceKind::Client,
        Some(value) => match parse_kind(value) {
            Some(kind) => kind,
            None => return invalid_field(request, catalog, "type"),
        },
    };
    let local_path = if path.starts_with("https://") {
        match download_mrpack(path, services, sink) {
            Ok(path) => path,
            Err(error) => {
                return Response::fault(
                    request.id.clone(),
                    INTERNAL_ERROR,
                    catalog.format("rpc.instance.import", &[("error", &error)]),
                );
            }
        }
    } else if path.contains("://") {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format(
                "rpc.instance.import",
                &[("error", "only HTTPS URLs are supported")],
            ),
        );
    } else {
        std::path::PathBuf::from(path)
    };
    let _cache_guard = CacheGuard(if path.starts_with("https://") {
        Some(local_path.clone())
    } else {
        None
    });
    let pack = match mrpack::read(&local_path) {
        Ok(pack) => pack,
        Err(error) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.instance.import", &[("error", &error.to_string())]),
            );
        }
    };
    let loader_kind = pack.loader.as_ref().map(|(kind, _)| kind.clone());
    let loader_version = pack.loader.as_ref().map(|(_, version)| version.clone());
    if loader_kind.as_deref() == Some("quilt") {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.format("rpc.instance.loader", &[("loader", "quilt")]),
        );
    }
    if let Some(loader) = loader_kind.as_deref() {
        if !loader_allowed(kind, loader) {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format(
                    "rpc.instance.loader_kind",
                    &[("loader", loader), ("kind", kind_label(kind))],
                ),
            );
        }
    }
    let defaults = instance_defaults(services);
    let file_stem = local_path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("modpack");
    let name = request
        .params
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(
            || {
                if pack.name.is_empty() {
                    file_stem.to_owned()
                } else {
                    pack.name.clone()
                }
            },
            str::to_owned,
        );
    let loader_label = loader_kind.as_deref().unwrap_or("vanilla");
    let Some(id) = allocate_id(services, &name) else {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.instance.id").to_owned(),
        );
    };
    let mut spec = NewInstance::with_id(id);
    spec.kind = kind;
    spec.name = name;
    spec.group = request
        .params
        .get("group")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|group| !group.is_empty())
        .map(str::to_owned)
        .or(defaults.group.clone())
        .or_else(|| Some(loader_label.to_owned()));
    spec.config.memory_mb = defaults.memory_mb;
    spec.config.jvm_args.clone_from(&defaults.jvm_args);
    spec.profile.set_minecraft(&pack.minecraft);
    if let (Some(loader), Some(version)) = (loader_kind.as_deref(), loader_version.as_deref()) {
        match loader {
            "fabric" => spec.profile.set_fabric(version),
            "forge" => spec.profile.set_forge(version),
            "neoforge" => spec.profile.set_neoforge(version),
            _ => {}
        }
    }

    let mut instance = match services.repository.create(&spec) {
        Ok(instance) => instance,
        Err(error) => return response_error(request, catalog, "rpc.instance.import", &error),
    };
    let cleanup = |instance: &Instance| {
        let _ = services.repository.delete(&instance.id);
    };
    let overrides = match mrpack::extract_overrides(
        &local_path,
        &instance.root,
        kind == InstanceKind::Client,
    ) {
        Ok(count) => count,
        Err(error) => {
            cleanup(&instance);
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.instance.import", &[("error", &error.to_string())]),
            );
        }
    };

    let linking_mode = request
        .params
        .get("linking")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| defaults.linking.clone(), str::to_owned);
    let link_result = apply_links(services, &instance, &linking_mode).map(|_| ());
    if let Err(error) = link_result {
        cleanup(&instance);
        return response_error(request, catalog, "rpc.instance.import", &error);
    }

    let tasks: Vec<DownloadTask> = pack
        .files
        .iter()
        .filter(|file| {
            if kind == InstanceKind::Client {
                file.client
            } else {
                file.server
            }
        })
        .map(|file| DownloadTask {
            url: file.url.clone(),
            path: instance.root.join(&file.path),
            sha1: file.sha1.clone(),
        })
        .collect();
    let progress = SinkProgress::new(sink);
    let report = match services
        .downloader
        .run_with(&tasks, DownloadMode::SkipExisting, &progress)
    {
        Ok(report) => report,
        Err(error) => {
            cleanup(&instance);
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.format("rpc.instance.import", &[("error", &error.to_string())]),
            );
        }
    };

    if let (Some(loader), Some(version)) = (loader_kind.as_deref(), loader_version.as_deref()) {
        if matches!(loader, "forge" | "neoforge") {
            let forge_kind = if loader == "forge" {
                ForgeKind::Forge
            } else {
                ForgeKind::NeoForge
            };
            if let Err(message) = apply_forge(
                &mut instance,
                services,
                catalog,
                Some(version),
                forge_kind,
                kind == InstanceKind::Server,
            ) {
                cleanup(&instance);
                return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
            }
        }
    }
    ensure_eula(&instance);
    if let Err(error) = services.repository.save(&instance) {
        cleanup(&instance);
        return response_error(request, catalog, "rpc.instance.import", &error);
    }
    if let Err(error) = services.repository.load(&instance.id) {
        cleanup(&instance);
        return response_error(request, catalog, "rpc.instance.import", &error);
    }

    Response::result(
        request.id.clone(),
        json!({
            "instance": instance,
            "files": {
                "downloaded": report.downloaded,
                "skipped": report.skipped,
            },
            "overrides": overrides,
        }),
    )
}

#[allow(clippy::too_many_lines)] // create orchestration; split when more steps are added
fn instance_create(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let parsed = match parse_create(request, catalog) {
        Ok(parsed) => parsed,
        Err((code, message)) => return Response::fault(request.id.clone(), code, message),
    };
    let defaults = instance_defaults(services);
    let kind = parsed.kind.unwrap_or(defaults.kind);
    let loader = match parsed.loader.as_deref() {
        Some("none") => None,
        Some(loader) => Some(loader.to_owned()),
        None => defaults.loader.clone(),
    };
    if parsed.loader_version.is_some() && loader.is_none() {
        return Response::fault(
            request.id.clone(),
            INVALID_PARAMS,
            catalog.text("rpc.instance.loader_version").to_owned(),
        );
    }
    if let Some(loader) = &loader {
        if !loader_allowed(kind, loader) {
            return Response::fault(
                request.id.clone(),
                INVALID_PARAMS,
                catalog.format(
                    "rpc.instance.loader_kind",
                    &[("loader", loader), ("kind", kind_label(kind))],
                ),
            );
        }
    }
    let create_defaults = parsed.defaults.unwrap_or(defaults.create_defaults);
    let version = match parsed
        .minecraft
        .clone()
        .or_else(|| defaults.version.clone())
    {
        Some(version) => version,
        None => match services.fetcher.manifest() {
            Ok(manifest) => manifest.latest.release,
            Err(error) => {
                return Response::fault(
                    request.id.clone(),
                    INTERNAL_ERROR,
                    catalog.format("rpc.launch.prepare", &[("error", &error.to_string())]),
                );
            }
        },
    };
    let loader_label = loader.as_deref().unwrap_or("vanilla");
    let default_name = format!("{loader_label}-{version}");
    let name = parsed
        .name
        .clone()
        .or_else(|| parsed.id.as_ref().map(|id| id.as_str().to_owned()))
        .unwrap_or_else(|| default_name.clone());
    let id = if let Some(id) = parsed.id.clone() {
        id
    } else {
        let base = parsed
            .name
            .clone()
            .unwrap_or_else(|| format!("{loader_label}-{version}"));
        match allocate_id(services, &base) {
            Some(id) => id,
            None => {
                return Response::fault(
                    request.id.clone(),
                    INVALID_PARAMS,
                    catalog.text("rpc.instance.id").to_owned(),
                );
            }
        }
    };
    let mut spec = NewInstance::with_id(id);
    spec.kind = kind;
    spec.name = name;
    spec.group = parsed
        .group
        .clone()
        .or(defaults.group.clone())
        .or_else(|| Some(loader_label.to_owned()));
    spec.config.lwjgl = parsed.lwjgl;
    let java = parsed
        .java
        .clone()
        .or_else(|| parse_java(&defaults.java, catalog).ok());
    match java {
        Some(JavaChoice::Version(version)) => spec.config.java_version = Some(version),
        Some(JavaChoice::Path(path)) => spec.config.java_path = Some(path),
        Some(JavaChoice::Default) | None => {}
    }
    spec.config.memory_mb = parsed.memory_mb.unwrap_or(defaults.memory_mb);
    spec.config.jvm_args = parsed
        .jvm_args
        .clone()
        .unwrap_or_else(|| defaults.jvm_args.clone());
    spec.account = match parsed.account {
        Some(value) => match resolve_account(services, catalog, &value) {
            Ok(account) => Some(account.id.as_str().to_owned()),
            Err(message) => {
                return Response::fault(request.id.clone(), INVALID_PARAMS, message);
            }
        },
        None => None,
    };
    spec.profile.set_minecraft(&version);

    match services.repository.create(&spec) {
        Ok(mut instance) => {
            if let Some(group) = &instance.group {
                let _ = services.groups.add(group);
            }
            if create_defaults {
                match instance.kind {
                    InstanceKind::Client => copy_instance_defaults(&services.data, &instance.root),
                    InstanceKind::Server => copy_server_defaults(&services.data, &instance.root),
                }
            }
            let linking_mode = parsed
                .linking
                .clone()
                .unwrap_or_else(|| defaults.linking.clone());
            let link_result = apply_links(services, &instance, &linking_mode);
            let report = match link_result {
                Ok(report) => report,
                Err(error) => {
                    let _ = services.repository.delete(&instance.id);
                    return response_error(request, catalog, "rpc.instance.create", &error);
                }
            };
            ensure_eula(&instance);
            let applied = match loader.as_deref() {
                Some("fabric") => apply_fabric(
                    &mut instance,
                    services,
                    catalog,
                    parsed.loader_version.as_deref(),
                ),
                Some("forge") => apply_forge(
                    &mut instance,
                    services,
                    catalog,
                    parsed.loader_version.as_deref(),
                    ForgeKind::Forge,
                    kind == InstanceKind::Server,
                ),
                Some("neoforge") => apply_forge(
                    &mut instance,
                    services,
                    catalog,
                    parsed.loader_version.as_deref(),
                    ForgeKind::NeoForge,
                    kind == InstanceKind::Server,
                ),
                Some("purpur") => apply_purpur(
                    &mut instance,
                    services,
                    catalog,
                    parsed.loader_version.as_deref(),
                ),
                None => Ok(()),
                Some(other) => Err(catalog.format("rpc.instance.loader", &[("loader", other)])),
            };
            if let Err(message) = applied {
                let _ = services.repository.delete(&instance.id);
                return Response::fault(request.id.clone(), INTERNAL_ERROR, message);
            }
            if loader.is_some() {
                if let Err(error) = services.repository.save(&instance) {
                    let _ = services.repository.delete(&instance.id);
                    return response_error(request, catalog, "rpc.instance.create", &error);
                }
            }
            Response::result(
                request.id.clone(),
                json!({
                    "instance": instance,
                    "links": {
                        "created": report.created.len(),
                        "skipped": report.skipped.len(),
                        "sharing_disabled": report.sharing_disabled,
                    }
                }),
            )
        }
        Err(error) => response_error(request, catalog, "rpc.instance.create", &error),
    }
}

fn instance_remove(request: &Request, services: &Services, catalog: &Catalog) -> Response {
    let Some(id) = instance_id(request) else {
        return invalid_id(request, catalog);
    };
    let instance = match services.repository.load(&id) {
        Ok(instance) => instance,
        Err(error) => return response_error(request, catalog, "rpc.instance.remove", &error),
    };
    match arkmcl_storage::lock::Guard::check(&LocalFileSystem, &instance.root) {
        Ok(()) => {}
        Err(arkmcl_storage::lock::Error::Locked) => {
            return Response::fault(
                request.id.clone(),
                INTERNAL_ERROR,
                catalog.text("rpc.launch.locked").to_owned(),
            );
        }
        Err(error) => return response_error(request, catalog, "rpc.instance.remove", &error),
    }
    match arkmcl_storage::trash::trash(&instance.root) {
        Ok(trashed) => {
            sync_group(services, instance.group.as_deref(), None);
            Response::result(
                request.id.clone(),
                json!({
                    "deleted": true,
                    "trashed": trashed.display().to_string(),
                }),
            )
        }
        Err(error) => Response::fault(
            request.id.clone(),
            INTERNAL_ERROR,
            catalog.format("rpc.instance.remove", &[("error", &error.to_string())]),
        ),
    }
}

fn instance_id(request: &Request) -> Option<Id> {
    request
        .params
        .get("id")
        .and_then(Value::as_str)
        .and_then(|raw| Id::new(raw).ok())
}

fn invalid_id(request: &Request, catalog: &Catalog) -> Response {
    Response::fault(
        request.id.clone(),
        INVALID_PARAMS,
        catalog.text("rpc.instance.id").to_owned(),
    )
}

fn invalid_field(request: &Request, catalog: &Catalog, field: &str) -> Response {
    Response::fault(
        request.id.clone(),
        INVALID_PARAMS,
        catalog.format("rpc.instance.field", &[("field", field)]),
    )
}

fn string_list(value: &Value) -> Option<Vec<String>> {
    let items = value.as_array()?;
    if items.iter().all(Value::is_string) {
        Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        )
    } else {
        None
    }
}

fn response_error(
    request: &Request,
    catalog: &Catalog,
    key: &str,
    error: &impl std::fmt::Display,
) -> Response {
    Response::fault(
        request.id.clone(),
        INTERNAL_ERROR,
        catalog.format(key, &[("error", &error.to_string())]),
    )
}

struct Cleanup {
    socket: PathBuf,
    pid: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(&self.pid);
    }
}

fn log(catalog: &Catalog, key: &str, args: &[(&str, &str)]) {
    eprintln!("{}", catalog.format(key, args));
}

fn no_sink(_: Value) {}

/// 把下载进度转成 JSON-RPC 通知，便于 CLI/GUI 渲染。
struct SinkProgress<'a> {
    sink: &'a (dyn Fn(Value) + Sync),
    last: std::sync::atomic::AtomicU64,
}

impl<'a> SinkProgress<'a> {
    fn new(sink: &'a (dyn Fn(Value) + Sync)) -> Self {
        Self {
            sink,
            last: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn emit(&self, value: Value) {
        (self.sink)(value);
    }

    fn throttle(&self) -> bool {
        let now = now_millis();
        let last = self.last.load(Ordering::Relaxed);
        if now.saturating_sub(last) < 100 {
            return false;
        }
        self.last.store(now, Ordering::Relaxed);
        true
    }
}

impl DownloadProgress for SinkProgress<'_> {
    fn start(&self, total: usize) {
        self.emit(json!({"event": "start", "total": total}));
    }

    fn task_started(&self, index: usize, task: &DownloadTask, total: Option<u64>) {
        self.emit(json!({
            "event": "task",
            "index": index,
            "path": task.path.display().to_string(),
            "total": total,
        }));
    }

    fn task_bytes(&self, index: usize, done: u64, total: Option<u64>) {
        if self.throttle() {
            self.emit(json!({
                "event": "bytes",
                "index": index,
                "done": done,
                "total": total,
            }));
        }
    }

    fn task_finished(&self, index: usize, task: &DownloadTask, skipped: bool) {
        self.emit(json!({
            "event": "done",
            "index": index,
            "path": task.path.display().to_string(),
            "skipped": skipped,
        }));
    }

    fn retry(&self, index: usize, attempt: u32, error: &str) {
        self.emit(json!({
            "event": "retry",
            "index": index,
            "attempt": attempt,
            "error": error,
        }));
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn write_notification(writer: &mut UnixStream, params: &Value) -> Result<(), Error> {
    let message = json!({"jsonrpc": "2.0", "method": "progress", "params": params});
    let text = serde_json::to_string(&message)?;
    writer.write_all(text.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn write_response(writer: &mut UnixStream, response: &Response) -> Result<(), Error> {
    let text = serde_json::to_string(response)?;
    writer.write_all(text.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resource_directory;

    #[test]
    fn maps_resource_kinds_to_directories() {
        assert_eq!(resource_directory("mod"), Some(("mods", true)));
        assert_eq!(
            resource_directory("resourcepack"),
            Some(("resourcepacks", true))
        );
        assert_eq!(resource_directory("shader"), Some(("shaderpacks", true)));
        assert_eq!(resource_directory("datapack"), Some(("datapacks", true)));
        assert_eq!(resource_directory("world"), Some(("saves", true)));
        assert_eq!(resource_directory("plugin"), Some(("plugins", true)));
        assert_eq!(resource_directory("modpack"), Some(("modpacks", true)));
        assert_eq!(resource_directory("unknown"), None);
    }
}
