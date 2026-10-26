//! 内容下载器。
//!
//! 负责客户端 jar、库、natives jar、资源对象与社区资源文件。默认 4 个并发，
//! 支持断点续传、失败退避重试与进度回调；`SHA-1` 匹配的文件可跳过。

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use arkmcl_core::meta::{AssetIndex, Context, Library, Version, allowed, maven_path};
use sha1::{Digest, Sha1};

const RESOURCES: &str = "https://resources.download.minecraft.net";
const MAX_BYTES: u64 = 512 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);
const ATTEMPTS: u32 = 4;
const DEFAULT_THREADS: usize = 8;
const MAX_THREADS: usize = 64;

/// 同名文件冲突策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 已存在且校验通过时跳过。
    SkipExisting,
    /// 无条件写入，覆盖同名文件。
    Overwrite,
}

/// 下载进度回调。
///
/// 实现必须可跨线程共享；默认方法全部为空，便于只关心部分事件。
pub trait Progress: Sync {
    /// 任务总数已确定。
    fn start(&self, _total: usize) {}
    /// 单个任务开始。
    fn task_started(&self, _index: usize, _task: &Task, _total: Option<u64>) {}
    /// 单个任务累计写入字节数。
    fn task_bytes(&self, _index: usize, _done: u64, _total: Option<u64>) {}
    /// 单个任务完成；`skipped` 表示因校验通过而跳过。
    fn task_finished(&self, _index: usize, _task: &Task, _skipped: bool) {}
    /// 即将重试。
    fn retry(&self, _index: usize, _attempt: u32, _error: &str) {}
    /// 全部完成。
    fn finish(&self, _report: &Report) {}
}

/// 不报告进度。
pub struct NoProgress;

impl Progress for NoProgress {}

/// 一个下载任务。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// 下载地址。
    pub url: String,
    /// 目标路径。
    pub path: PathBuf,
    /// 期望的 `SHA-1`；为空时跳过校验。
    pub sha1: String,
}

/// 下载结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 实际下载数量。
    pub downloaded: usize,
    /// 已存在并跳过的数量。
    pub skipped: usize,
}

/// 下载错误。
#[derive(Debug)]
pub enum Error {
    /// I/O 错误。
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
    /// HTTP 错误。
    Http(String),
    /// 非 HTTPS 地址。
    Url(String),
    /// 校验和不匹配。
    Checksum {
        /// 文件路径。
        path: PathBuf,
        /// 期望值。
        expected: String,
        /// 实际值。
        actual: String,
    },
    /// 无法确定下载地址。
    NoSource(String),
    /// 文件超过大小上限。
    TooLarge(PathBuf),
    /// 服务器返回非成功状态码。
    Status {
        /// 下载地址。
        url: String,
        /// HTTP 状态码。
        code: u16,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Http(error) => write!(formatter, "{error}"),
            Self::Url(url) => write!(formatter, "refusing non-HTTPS URL: {url}"),
            Self::Checksum {
                path,
                expected,
                actual,
            } => write!(
                formatter,
                "checksum mismatch for {}: expected {expected}, got {actual}",
                path.display()
            ),
            Self::NoSource(name) => write!(formatter, "cannot determine download source: {name}"),
            Self::TooLarge(path) => write!(formatter, "file is too large: {}", path.display()),
            Self::Status { url, code } => write!(formatter, "HTTP {code} for {url}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Http(_)
            | Self::Url(_)
            | Self::Checksum { .. }
            | Self::NoSource(_)
            | Self::TooLarge(_)
            | Self::Status { .. } => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io {
            path: PathBuf::new(),
            source: error,
        }
    }
}

/// 内容下载器。
pub struct Downloader {
    minecraft: PathBuf,
    agent: ureq::Agent,
    threads: usize,
}

impl Downloader {
    /// 创建下载器；默认 8 个下载线程。
    #[must_use]
    pub fn new(minecraft: PathBuf) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(TIMEOUT)
            .user_agent(concat!("arkmcl/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            minecraft,
            agent,
            threads: DEFAULT_THREADS,
        }
    }

    /// 设置下载线程数；自动收敛到 `1..=64`。
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.clamp(1, MAX_THREADS);
        self
    }

    /// 当前下载线程数。
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// 生成版本与资源索引对应的下载任务。
    ///
    /// # Errors
    ///
    /// 允许的库缺少下载地址或 Maven 坐标非法时返回错误。
    pub fn tasks(
        &self,
        version: &Version,
        index: &AssetIndex,
        context: &Context,
    ) -> Result<Vec<Task>, Error> {
        let mut tasks = Vec::new();
        let mut seen = HashSet::new();
        self.client_task(version, &mut tasks, &mut seen);
        self.library_tasks(version, context, &mut tasks, &mut seen)?;
        self.asset_tasks(index, &mut tasks, &mut seen);
        Ok(tasks)
    }

    /// 执行任务列表，跳过已存在且校验通过的文件。
    ///
    /// # Errors
    ///
    /// 网络、写入或校验失败时返回错误。
    pub fn run(&self, tasks: &[Task]) -> Result<Report, Error> {
        self.run_with(tasks, Mode::SkipExisting, &NoProgress)
    }

    /// 按指定冲突策略并发执行任务列表。
    ///
    /// # Errors
    ///
    /// 任一任务在重试后仍失败时返回该错误。
    pub fn run_with(
        &self,
        tasks: &[Task],
        mode: Mode,
        progress: &dyn Progress,
    ) -> Result<Report, Error> {
        self.run_with_threads(tasks, mode, progress, self.threads)
    }

    /// 用指定下载线程数执行任务列表。
    ///
    /// # Errors
    ///
    /// 任一任务在重试后仍失败时返回该错误。
    pub fn run_with_threads(
        &self,
        tasks: &[Task],
        mode: Mode,
        progress: &dyn Progress,
        threads: usize,
    ) -> Result<Report, Error> {
        let total = tasks.len();
        progress.start(total);
        if total == 0 {
            let report = Report::default();
            progress.finish(&report);
            return Ok(report);
        }
        let workers = threads.clamp(1, MAX_THREADS).min(total);
        let next = AtomicUsize::new(0);
        let outcomes: Mutex<Vec<Option<Result<bool, Error>>>> =
            Mutex::new((0..total).map(|_| None).collect());
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= total {
                            break;
                        }
                        let task = &tasks[index];
                        let outcome = if mode == Mode::SkipExisting {
                            match Self::verify(task) {
                                Ok(true) => Ok(true),
                                Ok(false) => {
                                    self.download_with(index, task, progress).map(|()| false)
                                }
                                Err(error) => Err(error),
                            }
                        } else {
                            self.download_with(index, task, progress).map(|()| false)
                        };
                        if let Ok(skipped) = &outcome {
                            progress.task_finished(index, task, *skipped);
                        }
                        if let Ok(mut guard) = outcomes.lock() {
                            guard[index] = Some(outcome);
                        }
                    }
                });
            }
        });
        let outcomes = match outcomes.into_inner() {
            Ok(outcomes) => outcomes,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut report = Report::default();
        for outcome in outcomes.into_iter().flatten() {
            match outcome {
                Ok(true) => report.skipped += 1,
                Ok(false) => report.downloaded += 1,
                Err(error) => return Err(error),
            }
        }
        progress.finish(&report);
        Ok(report)
    }

    fn client_task(&self, version: &Version, tasks: &mut Vec<Task>, seen: &mut HashSet<PathBuf>) {
        let Some(client) = version
            .downloads
            .as_ref()
            .and_then(|downloads| downloads.client.as_ref())
        else {
            return;
        };
        if client.url.is_empty() {
            return;
        }
        let client_version = version.client_version.as_deref().unwrap_or(&version.id);
        let path = self
            .minecraft
            .join("versions")
            .join(client_version)
            .join(format!("{client_version}.jar"));
        push(
            tasks,
            seen,
            Task {
                url: client.url.clone(),
                path,
                sha1: client.sha1.clone(),
            },
        );
    }

    fn library_tasks(
        &self,
        version: &Version,
        context: &Context,
        tasks: &mut Vec<Task>,
        seen: &mut HashSet<PathBuf>,
    ) -> Result<(), Error> {
        for library in &version.libraries {
            if !allowed(&library.rules, context) {
                continue;
            }
            if let Some((url, path, sha1)) = artifact_source(library, None)? {
                push(
                    tasks,
                    seen,
                    Task {
                        url,
                        path: self.minecraft.join("libraries").join(path),
                        sha1,
                    },
                );
            }
            if let Some(classifier) = library
                .natives
                .as_ref()
                .and_then(|natives| natives.get(&context.os))
            {
                match artifact_source(library, Some(classifier))? {
                    Some((url, path, sha1)) => push(
                        tasks,
                        seen,
                        Task {
                            url,
                            path: self.minecraft.join("libraries").join(path),
                            sha1,
                        },
                    ),
                    None => return Err(Error::NoSource(library.name.clone())),
                }
            }
        }
        Ok(())
    }

    fn asset_tasks(&self, index: &AssetIndex, tasks: &mut Vec<Task>, seen: &mut HashSet<PathBuf>) {
        for object in index.objects.values() {
            let Some(prefix) = object.hash.get(0..2) else {
                continue;
            };
            let path = self
                .minecraft
                .join("assets")
                .join("objects")
                .join(prefix)
                .join(&object.hash);
            push(
                tasks,
                seen,
                Task {
                    url: format!("{RESOURCES}/{prefix}/{}", object.hash),
                    path,
                    sha1: object.hash.clone(),
                },
            );
        }
    }

    fn verify(task: &Task) -> Result<bool, Error> {
        if !task.path.exists() {
            return Ok(false);
        }
        if task.sha1.is_empty() {
            return Ok(true);
        }
        let mut file = fs::File::open(&task.path).map_err(|source| Error::Io {
            path: task.path.clone(),
            source,
        })?;
        let mut hasher = Sha1::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|source| Error::Io {
                path: task.path.clone(),
                source,
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = format!("{:x}", hasher.finalize());
        Ok(task.sha1.eq_ignore_ascii_case(&actual))
    }

    fn download_with(
        &self,
        index: usize,
        task: &Task,
        progress: &dyn Progress,
    ) -> Result<(), Error> {
        if !task.url.starts_with("https://") {
            return Err(Error::Url(task.url.clone()));
        }
        let mut attempt = 0_u32;
        loop {
            attempt += 1;
            match self.download_once(index, task, progress) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if attempt >= ATTEMPTS || !retryable(&error) {
                        return Err(error);
                    }
                    progress.retry(index, attempt, &error.to_string());
                    std::thread::sleep(backoff(attempt));
                }
            }
        }
    }

    fn download_once(
        &self,
        index: usize,
        task: &Task,
        progress: &dyn Progress,
    ) -> Result<(), Error> {
        let parent = task.path.parent().ok_or_else(|| Error::Io {
            path: task.path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path has no parent directory",
            ),
        })?;
        fs::create_dir_all(parent).map_err(|source| Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let temporary = partial_path(&task.path);
        let resume_from = fs::metadata(&temporary).map_or(0, |metadata| metadata.len());
        let mut request = self.agent.get(&task.url);
        if resume_from > 0 {
            request = request.set("Range", &format!("bytes={resume_from}-"));
        }
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::Status(code, _)) => {
                return Err(Error::Status {
                    url: task.url.clone(),
                    code,
                });
            }
            Err(error) => return Err(Error::Http(error.to_string())),
        };
        let resumed = resume_from > 0 && response.status() == 206;
        let length = response
            .header("Content-Length")
            .and_then(|value| value.parse::<u64>().ok());
        let total = length.map(|length| {
            if resumed {
                length + resume_from
            } else {
                length
            }
        });
        progress.task_started(index, task, total);
        let result = (|| {
            let (mut hasher, mut done) = if resumed {
                hash_prefix(&temporary)?
            } else {
                (Sha1::new(), 0_u64)
            };
            let mut file = if resumed {
                fs::OpenOptions::new().append(true).open(&temporary)
            } else {
                fs::File::create(&temporary)
            }
            .map_err(|source| Error::Io {
                path: temporary.clone(),
                source,
            })?;
            let mut reader = response.into_reader().take(MAX_BYTES + 1);
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let read = reader.read(&mut buffer).map_err(|source| Error::Io {
                    path: temporary.clone(),
                    source,
                })?;
                if read == 0 {
                    break;
                }
                done += read as u64;
                if done > MAX_BYTES {
                    return Err(Error::TooLarge(task.path.clone()));
                }
                hasher.update(&buffer[..read]);
                file.write_all(&buffer[..read])
                    .map_err(|source| Error::Io {
                        path: temporary.clone(),
                        source,
                    })?;
                progress.task_bytes(index, done, total);
            }
            file.sync_all().map_err(|source| Error::Io {
                path: temporary.clone(),
                source,
            })?;
            drop(file);
            commit(&temporary, task, hasher)
        })();
        if matches!(&result, Err(Error::Checksum { .. } | Error::TooLarge(_))) {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn partial_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || std::ffi::OsString::from("download"),
        std::ffi::OsStr::to_os_string,
    );
    name.push(".part");
    path.with_file_name(name)
}

fn hash_prefix(path: &Path) -> Result<(Sha1, u64), Error> {
    let mut existing = fs::File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha1::new();
    let mut done = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = existing.read(&mut buffer).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        done += read as u64;
    }
    Ok((hasher, done))
}

fn commit(temporary: &Path, task: &Task, hasher: Sha1) -> Result<(), Error> {
    let actual = format!("{:x}", hasher.finalize());
    if !task.sha1.is_empty() && !task.sha1.eq_ignore_ascii_case(&actual) {
        return Err(Error::Checksum {
            path: task.path.clone(),
            expected: task.sha1.clone(),
            actual,
        });
    }
    fs::rename(temporary, &task.path).map_err(|source| Error::Io {
        path: task.path.clone(),
        source,
    })
}

fn retryable(error: &Error) -> bool {
    match error {
        Error::Status { code, .. } => crate::retry::retryable_status(*code),
        Error::Http(_) | Error::Checksum { .. } => true,
        Error::Io { source, .. } => crate::retry::retryable_io(source.kind()),
        Error::Url(_) | Error::NoSource(_) | Error::TooLarge(_) => false,
    }
}

fn backoff(attempt: u32) -> Duration {
    crate::retry::backoff(attempt)
}

fn artifact_source(
    library: &Library,
    classifier: Option<&str>,
) -> Result<Option<(String, String, String)>, Error> {
    if let Some(downloads) = &library.downloads {
        let artifact = if let Some(classifier) = classifier {
            downloads
                .classifiers
                .as_ref()
                .and_then(|classifiers| classifiers.get(classifier))
        } else {
            downloads.artifact.as_ref()
        };
        if let Some(artifact) = artifact {
            if !artifact.url.is_empty() {
                let path = artifact
                    .path
                    .clone()
                    .or_else(|| maven_path(&library.name, classifier))
                    .ok_or_else(|| Error::NoSource(library.name.clone()))?;
                return Ok(Some((artifact.url.clone(), path, artifact.sha1.clone())));
            }
        }
    }
    if let Some(base) = &library.url {
        let path = maven_path(&library.name, classifier)
            .ok_or_else(|| Error::NoSource(library.name.clone()))?;
        let url = format!("{}/{}", base.trim_end_matches('/'), path);
        return Ok(Some((url, path, String::new())));
    }
    Ok(None)
}

fn push(tasks: &mut Vec<Task>, seen: &mut HashSet<PathBuf>, task: Task) {
    if seen.insert(task.path.clone()) {
        tasks.push(task);
    }
}

#[cfg(test)]
mod tests {
    use super::{Downloader, Error, Mode, NoProgress, Progress, Report, Task, partial_path};
    use arkmcl_core::meta::{AssetIndex, Context, Version};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn temp_dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("arkmcl-download-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn context() -> Context {
        Context {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            features: HashMap::new(),
        }
    }

    fn fixture() -> (Version, AssetIndex) {
        let version: Version = serde_json::from_str(
            r#"{
              "id": "1.20.4",
              "type": "release",
              "mainClass": "net.minecraft.client.main.Main",
              "downloads": {"client": {"url": "https://example.com/client.jar", "sha1": "", "size": 0}},
              "libraries": [
                {
                  "name": "org.example:lib:1.0",
                  "downloads": {"artifact": {"url": "https://example.com/lib.jar", "path": "org/example/lib/1.0/lib-1.0.jar", "sha1": "", "size": 0}}
                },
                {
                  "name": "net.fabricmc:fabric-loader:1.0",
                  "url": "https://maven.fabricmc.net/"
                },
                {
                  "name": "org.example:native:1.0",
                  "natives": {"linux": "natives-linux"},
                  "downloads": {"classifiers": {"natives-linux": {"url": "https://example.com/native.jar", "path": "org/example/native/1.0/native-1.0-natives-linux.jar", "sha1": "", "size": 0}}},
                  "rules": [{"action": "allow", "os": {"name": "linux"}}]
                },
                {
                  "name": "org.example:mac:1.0",
                  "downloads": {"artifact": {"url": "https://example.com/mac.jar", "path": "org/example/mac/1.0/mac-1.0.jar", "sha1": "", "size": 0}},
                  "rules": [{"action": "allow", "os": {"name": "osx"}}]
                }
              ]
            }"#,
        )
        .expect("version");
        let index: AssetIndex = serde_json::from_str(
            r#"{
              "objects": {
                "icons/icon_16x16.png": {"hash": "aabbccddeeff00112233445566778899aabbccdd", "size": 10}
              }
            }"#,
        )
        .expect("index");
        (version, index)
    }

    #[test]
    fn builds_tasks_with_rules_and_urls() {
        let directory = temp_dir("tasks");
        let (version, index) = fixture();
        let tasks = Downloader::new(directory.clone())
            .tasks(&version, &index, &context())
            .expect("tasks");
        assert_eq!(tasks.len(), 5);
        assert!(
            tasks
                .iter()
                .any(|task| task.path.ends_with("versions/1.20.4/1.20.4.jar"))
        );
        assert!(tasks.iter().any(|task| {
            task.url
                .contains("maven.fabricmc.net/net/fabricmc/fabric-loader/1.0")
        }));
        assert!(tasks.iter().any(|task| {
            task.path
                .ends_with("assets/objects/aa/aabbccddeeff00112233445566778899aabbccdd")
        }));
        assert!(
            !tasks
                .iter()
                .any(|task| task.path.to_string_lossy().contains("mac-1.0"))
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn clamps_threads_to_supported_range() {
        let directory = temp_dir("threads");
        assert_eq!(
            Downloader::new(directory.clone()).with_threads(0).threads(),
            1
        );
        assert_eq!(
            Downloader::new(directory.clone())
                .with_threads(999)
                .threads(),
            64
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn partial_files_keep_their_extension() {
        assert_eq!(
            partial_path(std::path::Path::new("/tmp/foo/1.20.4.jar")),
            PathBuf::from("/tmp/foo/1.20.4.jar.part")
        );
    }

    #[test]
    fn empty_task_list_reports_no_work() {
        let directory = temp_dir("empty");
        let report = Downloader::new(directory.clone())
            .run_with(&[], Mode::Overwrite, &NoProgress)
            .expect("run");
        assert_eq!(report, Report::default());
        let _ = fs::remove_dir_all(&directory);
    }

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<String>>,
    }

    impl Progress for Recorder {
        fn start(&self, total: usize) {
            self.events
                .lock()
                .expect("lock")
                .push(format!("start:{total}"));
        }

        fn finish(&self, report: &Report) {
            self.events
                .lock()
                .expect("lock")
                .push(format!("finish:{}/{}", report.downloaded, report.skipped));
        }
    }

    #[test]
    fn reports_start_and_finish() {
        let directory = temp_dir("progress");
        let recorder = Recorder::default();
        Downloader::new(directory.clone())
            .run_with(&[], Mode::SkipExisting, &recorder)
            .expect("run");
        assert_eq!(
            recorder.events.lock().expect("lock").as_slice(),
            ["start:0", "finish:0/0"]
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn skips_existing_matching_file() {
        let directory = temp_dir("skip");
        let path = directory.join("file.txt");
        fs::write(&path, b"hello").expect("file");
        let task = Task {
            url: "https://example.com/file".to_owned(),
            path,
            sha1: "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".to_owned(),
        };
        let report = Downloader::new(directory.clone())
            .run(&[task])
            .expect("run");
        assert_eq!(report.downloaded, 0);
        assert_eq!(report.skipped, 1);
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn rejects_non_https_urls() {
        let directory = temp_dir("http");
        let task = Task {
            url: "http://example.com/file".to_owned(),
            path: directory.join("file"),
            sha1: String::new(),
        };
        assert!(matches!(
            Downloader::new(directory.clone()).run(&[task]),
            Err(Error::Url(_))
        ));
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    #[ignore = "requires network access"]
    fn downloads_live_asset() {
        let directory = temp_dir("live");
        let fetcher = crate::net::Fetcher::new(directory.clone(), crate::fs::LocalFileSystem);
        let manifest = fetcher.manifest().expect("manifest");
        let version = fetcher.version(&manifest.latest.release).expect("version");
        let index = fetcher.index(&version).expect("index");
        let downloader = Downloader::new(directory.clone());
        let tasks = downloader
            .tasks(&version, &index, &context())
            .expect("tasks");
        let asset = tasks
            .iter()
            .find(|task| task.path.starts_with(directory.join("assets/objects")))
            .expect("asset task");
        let report = downloader
            .run(std::slice::from_ref(asset))
            .expect("download asset");
        assert_eq!(report.downloaded, 1);
        assert!(asset.path.exists());
        let _ = fs::remove_dir_all(&directory);
    }
}
