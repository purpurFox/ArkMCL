//! `ArkMCL` 命令行入口。
//!
//! 约定：
//! - 正常结果写 stdout，诊断与错误写 stderr；
//! - 成功退出码 0，用法错误 2，运行时失败 1；
//! - `--lang` 优先于 `ARKMCL_LANG` 与系统 locale；
//! - 列表默认输出人类可读的分组文本，`--json` 输出机器可读 JSON。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, IsTerminal, Write};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use arkmcl_core::RuntimeInfo;
use arkmcl_core::model::DEFAULT_JVM_ARGS;
use arkmcl_l10n::{Catalog, Locale};
use arkmcl_platform::paths::Paths;
use arkmcl_storage::msa;
use serde_json::{Value, json};

mod completion;

/// `--json` 时禁止进度输出，避免污染机器可读结果。
static QUIET_PROGRESS: AtomicBool = AtomicBool::new(false);

/// 是否已完成与服务端的协议握手。
static PROTOCOL_CHECKED: AtomicBool = AtomicBool::new(false);

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    if args.iter().any(|argument| argument == "--json") {
        QUIET_PROGRESS.store(true, Ordering::Relaxed);
    }

    let Ok(lang) = extract_lang(&mut args) else {
        let catalog = load_catalog(Locale::detect(None));
        eprintln!("{}", catalog.text("cli.error.lang"));
        return ExitCode::from(2);
    };
    let locale = Locale::detect(lang.as_deref());
    let catalog = load_catalog(locale);

    if let Some(tag) = lang.as_deref() {
        if Locale::from_tag(tag).is_none() {
            eprintln!(
                "{}",
                catalog.format("cli.error.lang_unsupported", &[("tag", tag)])
            );
        }
    }

    dispatch(&mut args, &catalog, locale)
}

fn dispatch(args: &mut Vec<String>, catalog: &Catalog, locale: Locale) -> ExitCode {
    let Some(command) = args.first().cloned() else {
        print!("{}", catalog.text("cli.help"));
        return ExitCode::SUCCESS;
    };
    args.remove(0);

    match command.as_str() {
        "help" | "--help" | "-h" => {
            print!("{}", catalog.text("cli.help"));
            ExitCode::SUCCESS
        }
        "version" | "--version" | "-V" => {
            if let Some(extra) = args.first() {
                return unexpected(catalog, extra);
            }
            let info = RuntimeInfo::current();
            println!(
                "{}",
                catalog.format(
                    "cli.version",
                    &[("name", info.name), ("version", info.version)]
                )
            );
            ExitCode::SUCCESS
        }
        "info" => {
            if let Some(extra) = args.first() {
                return unexpected(catalog, extra);
            }
            info(catalog, locale)
        }
        "service" => service_command(args, catalog),
        "completion" => completion_command(args, catalog),
        "complete" => complete_command(args),
        "account" => account_command(args, catalog),
        "instance" => instance_command(args, catalog),
        "launch" => launch_command(args, catalog),
        "resource" => resource_command(args, catalog),
        other => unknown(catalog, other),
    }
}

fn info(catalog: &Catalog, locale: Locale) -> ExitCode {
    let paths = match Paths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!(
                "{}",
                catalog.format("cli.error.paths", &[("error", &error.to_string())])
            );
            return ExitCode::from(1);
        }
    };
    let runtime = RuntimeInfo::current();
    let payload = serde_json::json!({
        "name": runtime.name,
        "version": runtime.version,
        "locale": locale.tag(),
        "portable": paths.portable,
        "socket": paths.socket().display().to_string(),
    });
    match serde_json::to_string(&payload) {
        Ok(text) => println!("{text}"),
        Err(error) => {
            eprintln!(
                "{}",
                catalog.format("cli.error.rpc", &[("message", &error.to_string())])
            );
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn service_command(args: &[String], catalog: &Catalog) -> ExitCode {
    let Some(subcommand) = args.first() else {
        eprintln!("{}", catalog.text("cli.hint"));
        return ExitCode::from(2);
    };
    if let Some(extra) = args.get(1) {
        return unexpected(catalog, extra);
    }

    match subcommand.as_str() {
        "status" => service_status(catalog),
        "stop" => service_stop(catalog),
        other => unknown(catalog, &format!("service {other}")),
    }
}

#[cfg(not(unix))]
fn service_command(_args: &[String], catalog: &Catalog) -> ExitCode {
    eprintln!("{}", catalog.text("cli.error.unsupported"));
    ExitCode::from(1)
}

#[cfg(unix)]
fn service_stop(catalog: &Catalog) -> ExitCode {
    call_simple(catalog, "server.stop", "cli.stop")
}

fn call_simple(catalog: &Catalog, method: &str, message_key: &str) -> ExitCode {
    match request_with(catalog, method, json!({}), false) {
        Ok(response) => {
            if let Some(fault) = response.error {
                eprintln!(
                    "{}",
                    catalog.format("cli.error.rpc", &[("message", &fault.message)])
                );
                return ExitCode::from(1);
            }
            println!("{}", catalog.text(message_key));
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

#[cfg(unix)]
fn service_status(catalog: &Catalog) -> ExitCode {
    let response = match request_with(catalog, "server.status", json!({}), false) {
        Ok(response) => response,
        Err(code) => return code,
    };
    if let Some(fault) = response.error {
        eprintln!(
            "{}",
            catalog.format("cli.error.rpc", &[("message", &fault.message)])
        );
        return ExitCode::from(1);
    }
    let result = response.result.unwrap_or_default();
    let pid = result
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .map_or_else(|| "?".to_owned(), |value| value.to_string());
    let locale = result
        .get("locale")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    let socket = result
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("?");
    println!(
        "{}",
        catalog.format(
            "cli.status",
            &[("pid", &pid), ("locale", locale), ("socket", socket)]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
/// 在 TTY 上绘制 pacman 风格进度；非 TTY 或 `--json` 时保持安静。
struct ProgressRenderer<'a> {
    catalog: &'a Catalog,
    enabled: bool,
    tty: bool,
    total: usize,
    finished: usize,
    current: String,
    done: u64,
    file_total: Option<u64>,
    last: Option<std::time::Instant>,
}

impl<'a> ProgressRenderer<'a> {
    fn new(catalog: &'a Catalog, enabled: bool) -> Self {
        Self {
            catalog,
            enabled,
            tty: std::io::stdout().is_terminal(),
            total: 0,
            finished: 0,
            current: String::new(),
            done: 0,
            file_total: None,
            last: None,
        }
    }

    fn update(&mut self, event: &Value) {
        if !self.enabled {
            return;
        }
        let Some(kind) = event.get("event").and_then(Value::as_str) else {
            return;
        };
        match kind {
            "start" => {
                self.total =
                    usize::try_from(event.get("total").and_then(Value::as_u64).unwrap_or(0))
                        .unwrap_or(0);
            }
            "task" => {
                if let Some(path) = event.get("path").and_then(Value::as_str) {
                    path.clone_into(&mut self.current);
                }
                self.file_total = event.get("total").and_then(Value::as_u64);
                self.done = 0;
            }
            "bytes" => {
                self.done = event.get("done").and_then(Value::as_u64).unwrap_or(0);
                self.render();
            }
            "done" => {
                self.finished += 1;
                self.done = 0;
                let path = event.get("path").and_then(Value::as_str).unwrap_or("");
                let skipped = event
                    .get("skipped")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if self.tty {
                    let suffix = if skipped { " (skipped)" } else { "" };
                    let mut out = std::io::stdout();
                    let _ = write!(out, "\r\x1b[K[{}] {path}{suffix}\n", self.finished);
                    let _ = out.flush();
                } else {
                    println!(
                        "{}",
                        self.catalog.format(
                            "cli.progress.done",
                            &[
                                ("done", &self.finished.to_string()),
                                ("total", &self.total.to_string()),
                                ("path", path),
                            ]
                        )
                    );
                }
            }
            "retry" => {
                let attempt = event.get("attempt").and_then(Value::as_u64).unwrap_or(0);
                let error = event.get("error").and_then(Value::as_str).unwrap_or("");
                eprintln!(
                    "{}",
                    self.catalog.format(
                        "cli.progress.retry",
                        &[("attempt", &attempt.to_string()), ("error", error)]
                    )
                );
            }
            _ => {}
        }
    }

    fn render(&mut self) {
        if !self.tty {
            return;
        }
        let now = std::time::Instant::now();
        if let Some(last) = self.last {
            if now.duration_since(last).as_millis() < 120 {
                return;
            }
        }
        self.last = Some(now);
        let index = (self.finished + 1).min(self.total.max(1));
        let (bar, percent) = match self.file_total {
            Some(total) if total > 0 => (
                progress_bar(self.done, total),
                format!("{:>3}%", self.done.saturating_mul(100) / total),
            ),
            _ => ("[--------------------]".to_owned(), "  ?%".to_owned()),
        };
        let mut out = std::io::stdout();
        let _ = write!(
            out,
            "\r\x1b[K[{index}/{}] {} {bar} {percent} {}\x1b[K",
            self.total.max(index),
            short_path(&self.current),
            human_bytes(self.done)
        );
        let _ = out.flush();
    }
}

fn progress_bar(done: u64, total: u64) -> String {
    const WIDTH: usize = 20;
    let filled = usize::try_from(done.saturating_mul(WIDTH as u64) / total)
        .unwrap_or(WIDTH)
        .min(WIDTH);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(WIDTH - filled))
}

#[allow(clippy::cast_precision_loss)] // 仅用于展示，字节级精度不影响可读性
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit('/').take(2).collect();
    let joined = parts.into_iter().rev().collect::<Vec<_>>().join("/");
    if joined.chars().count() <= 42 {
        return joined;
    }
    let tail: String = joined
        .chars()
        .rev()
        .take(41)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

fn request(
    catalog: &Catalog,
    method: &str,
    params: Value,
) -> Result<arkmcl_platform::ipc::Response, ExitCode> {
    request_with(catalog, method, params, true)
}

fn request_with(
    catalog: &Catalog,
    method: &str,
    params: Value,
    autostart: bool,
) -> Result<arkmcl_platform::ipc::Response, ExitCode> {
    if method != "server.info" {
        ensure_protocol(catalog, autostart)?;
    }
    send_request(catalog, method, params, autostart)
}

fn ensure_protocol(catalog: &Catalog, autostart: bool) -> Result<(), ExitCode> {
    if PROTOCOL_CHECKED.load(Ordering::Relaxed) {
        return Ok(());
    }
    let response = send_request(catalog, "server.info", json!({}), autostart)?;
    if let Some(fault) = response.error {
        eprintln!(
            "{}",
            catalog.format("cli.error.rpc", &[("message", &fault.message)])
        );
        return Err(ExitCode::from(1));
    }
    let protocol = response
        .result
        .as_ref()
        .and_then(|value| value.get("protocol"))
        .and_then(Value::as_u64);
    match protocol {
        Some(version) if version == u64::from(arkmcl_platform::ipc::PROTOCOL_VERSION) => {
            PROTOCOL_CHECKED.store(true, Ordering::Relaxed);
            Ok(())
        }
        Some(version) => {
            eprintln!(
                "{}",
                catalog.format(
                    "cli.error.protocol",
                    &[
                        ("server", &version.to_string()),
                        (
                            "client",
                            &arkmcl_platform::ipc::PROTOCOL_VERSION.to_string()
                        ),
                    ]
                )
            );
            Err(ExitCode::from(1))
        }
        None => {
            eprintln!("{}", catalog.text("cli.error.protocol_missing"));
            Err(ExitCode::from(1))
        }
    }
}

fn send_request(
    catalog: &Catalog,
    method: &str,
    params: Value,
    autostart: bool,
) -> Result<arkmcl_platform::ipc::Response, ExitCode> {
    let paths = match Paths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!(
                "{}",
                catalog.format("cli.error.paths", &[("error", &error.to_string())])
            );
            return Err(ExitCode::from(1));
        }
    };
    let socket = paths.socket();
    let request = arkmcl_platform::ipc::Request::new(1, method, params);
    let mut renderer = ProgressRenderer::new(catalog, !QUIET_PROGRESS.load(Ordering::Relaxed));
    let mut call_once = |request: &arkmcl_platform::ipc::Request| {
        arkmcl_platform::ipc::call_with(&socket, request, &mut |event| renderer.update(event))
    };
    let mut error = match call_once(&request) {
        Ok(response) => return Ok(response),
        Err(error) => error,
    };
    if autostart && is_not_running(&error) && start_service() {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            match call_once(&request) {
                Ok(response) => return Ok(response),
                Err(next) => error = next,
            }
        }
    }
    eprintln!(
        "{}",
        catalog.format(
            "cli.error.connect",
            &[
                ("socket", &socket.display().to_string()),
                ("error", &error.to_string()),
            ]
        )
    );
    Err(ExitCode::from(1))
}

fn is_not_running(error: &arkmcl_platform::ipc::Error) -> bool {
    matches!(
        error,
        arkmcl_platform::ipc::Error::Io(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
    )
}

fn start_service() -> bool {
    if std::env::var_os("ARKMCL_NO_AUTOSTART").is_some() {
        return false;
    }
    let binary = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(|dir| dir.join("arkmcl-service")));
    let mut command = match binary {
        Some(path) if path.is_file() => Command::new(path),
        _ => Command::new("arkmcl-service"),
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(unix)]
fn call(catalog: &Catalog, method: &str, params: Value) -> Result<Value, ExitCode> {
    let response = request(catalog, method, params)?;
    if let Some(fault) = response.error {
        eprintln!(
            "{}",
            catalog.format("cli.error.rpc", &[("message", &fault.message)])
        );
        return Err(ExitCode::from(1));
    }
    Ok(response.result.unwrap_or(Value::Null))
}

#[derive(Default)]
struct CommandArgs {
    positionals: Vec<String>,
    values: HashMap<String, Vec<String>>,
    flags: HashSet<String>,
    help: bool,
}

impl CommandArgs {
    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .and_then(|values| values.last())
            .map(String::as_str)
    }

    fn all(&self, name: &str) -> &[String] {
        self.values.get(name).map_or(&[], Vec::as_slice)
    }
}

fn parse_command_args(
    args: &[String],
    value_flags: &[&str],
    optional_value_flags: &[&str],
    bool_flags: &[&str],
    catalog: &Catalog,
) -> Result<CommandArgs, ExitCode> {
    let mut parsed = CommandArgs::default();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if token == "--help" || token == "-h" {
            parsed.help = true;
            index += 1;
            continue;
        }
        let Some(raw) = token.strip_prefix("--") else {
            if token.starts_with('-') && token != "-" {
                return Err(unexpected(catalog, token));
            }
            parsed.positionals.push(token.clone());
            index += 1;
            continue;
        };
        let (name, inline) = match raw.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (raw, None),
        };
        let flag = format!("--{name}");
        if bool_flags.contains(&name) {
            if inline.is_some() {
                return Err(unexpected(catalog, token));
            }
            parsed.flags.insert(name.to_owned());
            index += 1;
        } else if value_flags.contains(&name) {
            let value = if let Some(value) = inline {
                value
            } else {
                let Some(value) = args.get(index + 1) else {
                    return Err(missing_flag(catalog, &flag));
                };
                index += 1;
                value.clone()
            };
            parsed
                .values
                .entry(name.to_owned())
                .or_default()
                .push(value);
            index += 1;
        } else if optional_value_flags.contains(&name) {
            let value = if let Some(value) = inline {
                Some(value)
            } else if let Some(next) = args.get(index + 1) {
                if next.starts_with('-') {
                    None
                } else {
                    index += 1;
                    Some(next.clone())
                }
            } else {
                None
            };
            if let Some(value) = value {
                parsed
                    .values
                    .entry(name.to_owned())
                    .or_default()
                    .push(value);
            }
            parsed.flags.insert(name.to_owned());
            index += 1;
        } else {
            return Err(unexpected(catalog, token));
        }
    }
    Ok(parsed)
}

fn parse_jvm_args(raw: &str) -> Result<Vec<String>, String> {
    match raw {
        "default" => Ok(DEFAULT_JVM_ARGS
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect()),
        "none" => Ok(Vec::new()),
        other => split_args(other),
    }
}

fn split_args(input: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            started = true;
            continue;
        }
        match quote {
            Some(active) => {
                if character == '\\' && active == '"' {
                    escaped = true;
                } else if character == active {
                    quote = None;
                } else {
                    current.push(character);
                }
            }
            None => match character {
                '\'' | '"' => {
                    quote = Some(character);
                    started = true;
                }
                '\\' => escaped = true,
                character if character.is_whitespace() => {
                    if started {
                        args.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                character => {
                    current.push(character);
                    started = true;
                }
            },
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".to_owned());
    }
    if escaped {
        return Err("trailing escape".to_owned());
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

fn wildcard(pattern: &str, value: &str) -> bool {
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

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn sort_name(value: &Value) -> &str {
    value
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| value.get("id").and_then(Value::as_str))
        .unwrap_or("")
}

fn sort_values(
    values: &mut Vec<&Value>,
    key: &str,
    ascending: bool,
    catalog: &Catalog,
) -> Result<(), ExitCode> {
    if !matches!(key, "alphabet" | "create" | "launch" | "run") {
        eprintln!(
            "{}",
            catalog.format("cli.error.sort_key", &[("value", key)])
        );
        return Err(ExitCode::from(2));
    }
    values.sort_by(|left, right| match key {
        "alphabet" => sort_name(left).cmp(sort_name(right)),
        "create" => numeric_stat(left, "created_at").cmp(&numeric_stat(right, "created_at")),
        "launch" => launch_stat(left).cmp(&launch_stat(right)),
        "run" => numeric_stat(left, "run_seconds").cmp(&numeric_stat(right, "run_seconds")),
        _ => std::cmp::Ordering::Equal,
    });
    if !ascending {
        values.reverse();
    }
    Ok(())
}

fn numeric_stat(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .or_else(|| value.get("started_at").and_then(Value::as_u64))
        .unwrap_or(0)
}

fn launch_stat(value: &Value) -> u64 {
    value
        .get("last_launched_at")
        .and_then(Value::as_u64)
        .or_else(|| value.get("last_used_at").and_then(Value::as_u64))
        .or_else(|| value.get("started_at").and_then(Value::as_u64))
        .unwrap_or(0)
}

fn parse_order(args: &CommandArgs, catalog: &Catalog) -> Result<bool, ExitCode> {
    match args.value("list-sort").unwrap_or("asc") {
        "asc" => Ok(true),
        "desc" => Ok(false),
        other => {
            eprintln!("{}", catalog.format("cli.error.sort", &[("value", other)]));
            Err(ExitCode::from(2))
        }
    }
}

fn select<'a>(
    values: &'a [Value],
    patterns: &[String],
    fields: &[&str],
    catalog: &Catalog,
) -> Result<Vec<&'a Value>, ExitCode> {
    if patterns.is_empty() {
        return Ok(values.iter().collect());
    }
    let mut seen = HashSet::new();
    let mut selected = Vec::new();
    for value in values {
        let matched = patterns.iter().any(|pattern| {
            fields
                .iter()
                .any(|name| wildcard(pattern, field(value, name)))
        });
        if matched {
            let key = field(value, "id");
            if seen.insert(key.to_owned()) {
                selected.push(value);
            }
        }
    }
    if selected.is_empty() {
        eprintln!(
            "{}",
            catalog.format("cli.error.selection", &[("value", &patterns.join(", "))])
        );
        return Err(ExitCode::from(2));
    }
    Ok(selected)
}

fn confirm(count: usize, catalog: &Catalog, assume_yes: bool) -> bool {
    if assume_yes {
        return true;
    }
    eprint!(
        "{}",
        catalog.format("cli.confirm", &[("count", &count.to_string())])
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn print_json(value: &Value) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("serialization error: {error}");
            ExitCode::from(1)
        }
    }
}

fn component_version(value: &Value, uid: &str) -> Option<String> {
    value
        .get("profile")?
        .get("components")?
        .as_array()?
        .iter()
        .find(|component| field(component, "uid") == uid)
        .and_then(|component| component.get("version"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn print_accounts(values: &[Value], catalog: &Catalog) {
    if values.is_empty() {
        println!("{}", catalog.text("cli.account.empty"));
        return;
    }
    for kind in ["microsoft", "external", "offline"] {
        let group: Vec<&Value> = values
            .iter()
            .filter(|value| field(value, "kind") == kind)
            .collect();
        if group.is_empty() {
            continue;
        }
        println!("[{kind}]");
        for account in group {
            let server = account.get("server").and_then(Value::as_str).unwrap_or("-");
            println!(
                "  {}  {}  {server}",
                field(account, "name"),
                field(account, "id")
            );
        }
    }
}

fn print_instances(values: &[Value], catalog: &Catalog) {
    if values.is_empty() {
        println!("{}", catalog.text("cli.instance.empty"));
        return;
    }
    let mut groups: Vec<String> = values
        .iter()
        .map(|value| field(value, "group").to_owned())
        .filter(|group| !group.is_empty())
        .collect();
    groups.sort();
    groups.dedup();
    let ungrouped = values.iter().any(|value| field(value, "group").is_empty());
    if ungrouped {
        println!("[{}]", catalog.text("cli.instance.ungrouped"));
        print_instance_rows(values, "");
    }
    for group in groups {
        println!("[{group}]");
        print_instance_rows(values, &group);
    }
}

fn print_instance_rows(values: &[Value], group: &str) {
    for instance in values {
        if field(instance, "group") != group {
            continue;
        }
        let version =
            component_version(instance, "net.minecraft").unwrap_or_else(|| "-".to_owned());
        let loader = component_version(instance, "net.fabricmc.fabric-loader")
            .map_or_else(|| "-".to_owned(), |version| format!("fabric {version}"));
        println!(
            "  {}  {}  {version}  {loader}  {}",
            field(instance, "id"),
            field(instance, "name"),
            field(instance, "state")
        );
    }
}

fn print_sessions(values: &[Value], catalog: &Catalog) {
    if values.is_empty() {
        println!("{}", catalog.text("cli.launch.empty"));
        return;
    }
    for session in values {
        let pid = session
            .get("pid")
            .and_then(Value::as_u64)
            .map_or_else(|| "?".to_owned(), |value| value.to_string());
        let started = session
            .get("started_at")
            .and_then(Value::as_u64)
            .map_or_else(|| "?".to_owned(), |value| value.to_string());
        println!("  {}  {pid}  {started}", field(session, "id"));
    }
}

#[cfg(unix)]
fn account_command(args: &[String], catalog: &Catalog) -> ExitCode {
    let parsed = match parse_command_args(
        args,
        &["server", "list-sort", "list-sort-by"],
        &[],
        &[
            "add", "remove", "default", "refresh", "info", "list", "json", "yes",
        ],
        catalog,
    ) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    if parsed.help {
        print!("{}", catalog.text("cli.help"));
        return ExitCode::SUCCESS;
    }
    if parsed.flag("list")
        && (parsed.flag("add")
            || parsed.flag("remove")
            || parsed.flag("default")
            || parsed.flag("refresh")
            || parsed.flag("info"))
    {
        return unexpected(catalog, "--list");
    }
    if parsed.flag("info")
        && (parsed.flag("add")
            || parsed.flag("remove")
            || parsed.flag("default")
            || parsed.flag("refresh"))
    {
        return unexpected(catalog, "--info");
    }
    if parsed.flag("add")
        || parsed.flag("remove")
        || parsed.flag("default")
        || parsed.flag("refresh")
        || parsed.flag("info")
    {
        if let Some(code) = reject_flags(&parsed, catalog, &["list-sort", "list-sort-by"]) {
            return code;
        }
    }
    if parsed.flag("add") && parsed.flag("remove") {
        return unexpected(catalog, "--remove");
    }
    if parsed.flag("refresh")
        && (parsed.flag("add") || parsed.flag("remove") || parsed.flag("default"))
    {
        return unexpected(catalog, "--refresh");
    }
    if parsed.flag("remove") && parsed.flag("default") {
        return unexpected(catalog, "--default");
    }
    if parsed.value("server").is_some() && !parsed.flag("add") {
        return missing_flag(catalog, "--add");
    }
    if parsed.flag("add") && parsed.flag("default") {
        return account_add_action(&parsed, catalog, true);
    }
    if parsed.flag("default") {
        return account_default_action(&parsed, catalog);
    }
    if parsed.flag("add") {
        return account_add_action(&parsed, catalog, false);
    }
    if parsed.flag("remove") {
        return account_remove_action(&parsed, catalog);
    }
    if parsed.flag("refresh") {
        return account_refresh_action(&parsed, catalog);
    }
    if parsed.flag("info") {
        return account_info_action(&parsed, catalog);
    }
    account_list_action(&parsed, catalog)
}

#[cfg(unix)]
fn account_info_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        let value = match call(catalog, "account.default", json!({})) {
            Ok(value) => value,
            Err(code) => return code,
        };
        if value.is_null() {
            if parsed.flag("json") {
                return print_json(&Value::Null);
            }
            println!("{}", catalog.text("cli.account.default_none"));
            return ExitCode::SUCCESS;
        }
        return print_json(&value);
    }
    let values = match call(catalog, "account.list", json!({})) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let values = values.as_array().cloned().unwrap_or_default();
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    let mut results = Vec::new();
    for account in &selected {
        match call(
            catalog,
            "account.info",
            json!({ "id": field(account, "id") }),
        ) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if results.len() == 1 {
        return print_json(&results[0]);
    }
    print_json(&Value::Array(results))
}

#[cfg(unix)]
fn account_list_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let values = match call(catalog, "account.list", json!({})) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let values = values.as_array().cloned().unwrap_or_default();
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    let mut selected = selected;
    let ascending = match parse_order(parsed, catalog) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let key = parsed.value("list-sort-by").unwrap_or("run");
    if let Err(code) = sort_values(&mut selected, key, ascending, catalog) {
        return code;
    }
    if parsed.flag("json") {
        let owned: Vec<Value> = selected.into_iter().cloned().collect();
        return print_json(&Value::Array(owned));
    }
    print_accounts(
        &selected.into_iter().cloned().collect::<Vec<Value>>(),
        catalog,
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn account_default_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        let value = match call(catalog, "account.default", json!({})) {
            Ok(value) => value,
            Err(code) => return code,
        };
        if value.is_null() {
            if parsed.flag("json") {
                return print_json(&Value::Null);
            }
            println!("{}", catalog.text("cli.account.default_none"));
            return ExitCode::SUCCESS;
        }
        return print_json(&value);
    }
    if parsed.positionals.len() != 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--default")])
        );
        return ExitCode::from(2);
    }
    let values = match call(catalog, "account.list", json!({})) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let values = values.as_array().cloned().unwrap_or_default();
    let pattern = &parsed.positionals[0];
    if !pattern.contains('*') && !pattern.contains('?') {
        let matches: Vec<&Value> = values
            .iter()
            .filter(|account| field(account, "name") == pattern)
            .collect();
        if matches.len() > 1 {
            let ids = matches
                .iter()
                .map(|account| field(account, "id"))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
                "{}",
                catalog.format("cli.error.ambiguous", &[("value", pattern), ("ids", &ids)])
            );
            return ExitCode::from(2);
        }
    }
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    if selected.len() != 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--default")])
        );
        return ExitCode::from(2);
    }
    finish(
        catalog,
        request(
            catalog,
            "account.default",
            json!({ "value": field(selected[0], "id") }),
        ),
    )
}

#[cfg(unix)]
fn account_refresh_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let requests: Vec<Value> = if parsed.positionals.is_empty() {
        vec![json!({})]
    } else {
        parsed
            .positionals
            .iter()
            .map(|id| json!({"id": id}))
            .collect()
    };
    let mut results = Vec::new();
    for params in requests {
        match call(catalog, "account.refresh", params) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if parsed.flag("json") {
        if results.len() == 1 {
            return print_json(&results[0]);
        }
        return print_json(&Value::Array(results));
    }
    let mut refreshed = 0usize;
    let mut failed = 0usize;
    for result in &results {
        if let Some(items) = result.get("refreshed").and_then(Value::as_array) {
            refreshed += items.len();
        }
        if let Some(items) = result.get("failed").and_then(Value::as_array) {
            failed += items.len();
            for item in items {
                eprintln!(
                    "{}",
                    catalog.format(
                        "cli.account.refresh_failed",
                        &[
                            ("account", field(item, "name")),
                            ("error", field(item, "error"))
                        ]
                    )
                );
            }
        }
    }
    println!(
        "{}",
        catalog.format(
            "cli.account.refreshed",
            &[("count", &refreshed.to_string())]
        )
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(unix)]
fn account_remove_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        return missing_flag(catalog, "<id|name>");
    }
    let values = match call(catalog, "account.list", json!({})) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let values = values.as_array().cloned().unwrap_or_default();
    for pattern in &parsed.positionals {
        if pattern.contains('*') || pattern.contains('?') {
            continue;
        }
        let matches: Vec<&Value> = values
            .iter()
            .filter(|account| field(account, "name") == pattern)
            .collect();
        if matches.len() > 1 {
            let ids = matches
                .iter()
                .map(|account| field(account, "id"))
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
                "{}",
                catalog.format("cli.error.ambiguous", &[("value", pattern), ("ids", &ids)])
            );
            return ExitCode::from(2);
        }
    }
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    if !confirm(selected.len(), catalog, parsed.flag("yes")) {
        println!("{}", catalog.text("cli.confirm.cancelled"));
        return ExitCode::SUCCESS;
    }
    let mut results = Vec::new();
    for account in &selected {
        match call(
            catalog,
            "account.remove",
            json!({ "id": field(account, "id") }),
        ) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if parsed.flag("json") {
        return print_json(&Value::Array(results));
    }
    println!(
        "{}",
        catalog.format(
            "cli.account.removed",
            &[("count", &selected.len().to_string())]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn account_add_action(parsed: &CommandArgs, catalog: &Catalog, set_default: bool) -> ExitCode {
    if parsed.positionals.len() > 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--add")])
        );
        return ExitCode::from(2);
    }
    let name = parsed.positionals.first().cloned();
    if let Some(server) = parsed.value("server") {
        let Some(name) = name else {
            return missing_flag(catalog, "<name>");
        };
        let params = json!({"kind": "external", "name": name, "server": server});
        return submit_account(catalog, params, set_default);
    }
    if let Some(name) = name {
        return submit_account(
            catalog,
            json!({"kind": "offline", "name": name}),
            set_default,
        );
    }
    account_login_msa(catalog, set_default)
}

fn submit_account(catalog: &Catalog, params: serde_json::Value, set_default: bool) -> ExitCode {
    let added = match call(catalog, "account.add", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if !set_default {
        return print_json(&added);
    }
    let Some(id) = added.get("id").and_then(Value::as_str) else {
        eprintln!(
            "{}",
            catalog.format(
                "cli.error.rpc",
                &[("message", "account response has no id")]
            )
        );
        return ExitCode::from(1);
    };
    match call(catalog, "account.default", json!({ "value": id })) {
        Ok(value) => print_json(&value),
        Err(code) => code,
    }
}

#[cfg(unix)]
fn instance_command(args: &[String], catalog: &Catalog) -> ExitCode {
    let parsed = match parse_command_args(
        args,
        &["list-sort", "list-sort-by", "move", "group-move", "import"],
        &[
            "group",
            "link",
            "name",
            "memory",
            "jvm-args",
            "java",
            "version",
            "lwjgl",
            "loader",
            "loader-version",
            "type",
            "account",
            "create-defaults",
        ],
        &["list", "info", "default", "create", "remove", "json", "yes"],
        catalog,
    ) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    if parsed.help {
        print!("{}", catalog.text("cli.help"));
        return ExitCode::SUCCESS;
    }
    if let Some(flag) = action_conflict(
        &parsed,
        &[
            "create",
            "import",
            "remove",
            "default",
            "info",
            "list",
            "move",
            "group-move",
        ],
    ) {
        return unexpected(catalog, &format!("--{flag}"));
    }
    if let Some(code) = validate_instance_actions(&parsed, catalog) {
        return code;
    }
    if parsed.flag("create") {
        return instance_create_action(&parsed, catalog);
    }
    if parsed.value("import").is_some() {
        return instance_import_action(&parsed, catalog);
    }
    if parsed.flag("remove") {
        return instance_remove_action(&parsed, catalog);
    }
    if parsed.flag("default") {
        return instance_default_action(&parsed, catalog);
    }
    if parsed.flag("info") {
        return instance_info_action(&parsed, catalog);
    }
    if parsed.flag("list") {
        return instance_list_action(&parsed, catalog);
    }
    if parsed.flag("java") && parsed.value("java").is_none() {
        return instance_java_query(&parsed, catalog);
    }
    if instance_has_setters(&parsed) {
        return instance_update_action(&parsed, catalog);
    }
    if parsed.flag("group") && parsed.value("group").is_none() && parsed.positionals.is_empty() {
        return instance_groups_action(catalog);
    }
    if parsed.positionals.is_empty() {
        return instance_defaults_action(&parsed, catalog);
    }
    instance_info_action(&parsed, catalog)
}

#[cfg(unix)]
fn instance_defaults_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let value = match call(catalog, "settings.get", json!({})) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            println!("{key} = {value}");
        }
    }
    ExitCode::SUCCESS
}

fn instance_has_setters(parsed: &CommandArgs) -> bool {
    parsed.value("name").is_some()
        || parsed.value("type").is_some()
        || parsed.value("memory").is_some()
        || parsed.value("jvm-args").is_some()
        || parsed.value("java").is_some()
        || parsed.value("version").is_some()
        || parsed.value("lwjgl").is_some()
        || parsed.value("loader").is_some()
        || parsed.value("loader-version").is_some()
        || parsed.value("link").is_some()
        || (parsed.flag("link") && parsed.value("link").is_none())
        || parsed.value("group-move").is_some()
        || parsed.value("move").is_some()
        || parsed.value("account").is_some()
        || parsed.value("create-defaults").is_some()
        || (parsed.flag("create-defaults") && parsed.value("create-defaults").is_none())
        || (parsed.flag("group") && parsed.value("group").is_some())
        || instance_query_flags(parsed).next().is_some()
}

#[cfg(unix)]
fn instance_import_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let Some(path) = parsed.value("import") else {
        return missing_flag(catalog, "--import <path>");
    };
    if parsed.positionals.len() > 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--import")])
        );
        return ExitCode::from(2);
    }
    let mut params = json!({"path": path});
    if let Some(name) = parsed.value("name") {
        params["name"] = json!(name);
    }
    if let Some(kind) = parsed.value("type") {
        let kind = kind.to_ascii_lowercase();
        if !matches!(kind.as_str(), "client" | "server") {
            eprintln!(
                "{}",
                catalog.format("cli.error.type", &[("value", kind.as_str())])
            );
            return ExitCode::from(2);
        }
        params["type"] = json!(kind);
    }
    if let Some(linking) = parsed.value("link") {
        params["linking"] = json!(linking);
    }
    if let Some(group) = parsed.value("group") {
        params["group"] = json!(group);
    }
    let value = match call(catalog, "instance.import", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    let instance = value.get("instance").cloned().unwrap_or(Value::Null);
    let files = value
        .pointer("/files/downloaded")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let overrides = value.get("overrides").and_then(Value::as_u64).unwrap_or(0);
    println!(
        "{}",
        catalog.format(
            "cli.instance.imported",
            &[
                ("name", field(&instance, "name")),
                ("id", field(&instance, "id")),
                ("files", &files.to_string()),
                ("overrides", &overrides.to_string()),
            ]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
#[allow(clippy::too_many_lines)] // flag validation; split when more fields are added
fn instance_create_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.flag("java") && parsed.value("java").is_none() {
        return missing_flag(catalog, "--java");
    }
    if parsed.positionals.len() > 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--create")])
        );
        return ExitCode::from(2);
    }
    let name = parsed.value("name");
    let version = parsed.value("version");
    if parsed.positionals.is_empty() {
        if name.is_none() {
            return missing_flag(catalog, "--name");
        }
        if version.is_none() {
            return missing_flag(catalog, "--version");
        }
    }
    let create_defaults = match parsed.value("create-defaults") {
        None => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(value) => {
            eprintln!(
                "{}",
                catalog.format("cli.error.boolean", &[("value", value)])
            );
            return ExitCode::from(2);
        }
    };
    if parsed.flag("create-defaults") && create_defaults.is_none() {
        return missing_flag(catalog, "--create-defaults");
    }
    let requested_type = parsed.value("type").map(str::to_ascii_lowercase);
    let kind = match requested_type.as_deref() {
        None => None,
        Some("client") => Some("client"),
        Some("server") => Some("server"),
        Some(_) => {
            eprintln!(
                "{}",
                catalog.format(
                    "cli.error.type",
                    &[("value", requested_type.as_deref().unwrap_or(""))]
                )
            );
            return ExitCode::from(2);
        }
    };
    if parsed.value("loader-version").is_some()
        && matches!(parsed.value("loader"), None | Some("none"))
    {
        eprintln!("{}", catalog.text("cli.error.loader_version"));
        return ExitCode::from(2);
    }
    if let Some(loader) = parsed.value("loader") {
        if !matches!(loader, "none" | "fabric" | "forge" | "neoforge" | "purpur") {
            eprintln!(
                "{}",
                catalog.format("cli.error.loader", &[("loader", loader)])
            );
            return ExitCode::from(2);
        }
        if let Some(kind) = kind {
            let allowed = loader == "none"
                || if kind == "server" {
                    matches!(loader, "purpur" | "fabric" | "forge" | "neoforge")
                } else {
                    matches!(loader, "fabric" | "forge" | "neoforge")
                };
            if !allowed {
                eprintln!(
                    "{}",
                    catalog.format(
                        "cli.error.loader_kind",
                        &[("kind", kind), ("loader", loader)]
                    )
                );
                return ExitCode::from(2);
            }
        }
    }
    let memory = match parsed.value("memory") {
        Some(raw) => {
            if let Ok(value) = raw.parse::<u32>() {
                Some(value)
            } else {
                eprintln!("{}", catalog.format("cli.error.memory", &[("value", raw)]));
                return ExitCode::from(2);
            }
        }
        None => None,
    };
    let memory_minimum = match kind {
        Some("server") => 256,
        Some(_) => 512,
        None => match memory {
            Some(memory) if memory < 512 => {
                let settings = match call(catalog, "settings.get", json!({})) {
                    Ok(value) => value,
                    Err(code) => return code,
                };
                if settings.get("type").and_then(Value::as_str) == Some("server") {
                    256
                } else {
                    512
                }
            }
            _ => 512,
        },
    };
    if let Some(memory) = memory {
        if memory < memory_minimum {
            eprintln!(
                "{}",
                catalog.format("cli.error.memory", &[("value", &memory.to_string())])
            );
            return ExitCode::from(2);
        }
    }
    let mut params = json!({});
    if let Some(kind) = kind {
        params["type"] = json!(kind);
    }
    if let Some(defaults) = create_defaults {
        params["defaults"] = json!(defaults);
    }
    if let Some(name) = name {
        params["name"] = json!(name);
    }
    if let Some(version) = version {
        params["minecraft"] = json!(version);
    }
    if let Some(lwjgl) = parsed.value("lwjgl") {
        params["lwjgl"] = json!(lwjgl);
    }
    if let Some(java) = parsed.value("java") {
        params["java"] = json!(java);
    }
    if let Some(id) = parsed.positionals.first() {
        params["id"] = json!(id);
    }
    if let Some(loader) = parsed.value("loader") {
        params["loader"] = json!(loader);
    }
    if let Some(loader_version) = parsed.value("loader-version") {
        params["loader_version"] = json!(loader_version);
    }
    if let Some(account) = parsed.value("account") {
        params["account"] = json!(account);
    }
    if let Some(group) = parsed.value("group") {
        params["group"] = json!(group);
    }
    if let Some(linking) = parsed.value("link") {
        params["linking"] = json!(linking);
    }
    if let Some(memory) = memory {
        params["memory_mb"] = json!(memory);
    }
    if let Some(raw) = parsed.value("jvm-args") {
        match parse_jvm_args(raw) {
            Ok(args) => params["jvm_args"] = json!(args),
            Err(error) => {
                eprintln!(
                    "{}",
                    catalog.format("cli.error.jvm_args", &[("error", &error)])
                );
                return ExitCode::from(2);
            }
        }
    }
    finish(catalog, request(catalog, "instance.create", params))
}

#[cfg(unix)]
fn instance_remove_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        return missing_flag(catalog, "<id>");
    }
    let values = match instance_list_values(catalog) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    if !confirm(selected.len(), catalog, parsed.flag("yes")) {
        println!("{}", catalog.text("cli.confirm.cancelled"));
        return ExitCode::SUCCESS;
    }
    let mut results = Vec::new();
    for instance in &selected {
        match call(
            catalog,
            "instance.remove",
            json!({ "id": field(instance, "id") }),
        ) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if parsed.flag("json") {
        return print_json(&Value::Array(results));
    }
    println!(
        "{}",
        catalog.format(
            "cli.instance.removed",
            &[("count", &selected.len().to_string())]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn instance_default_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        return finish(catalog, request(catalog, "instance.info", json!({})));
    }
    if parsed.positionals.len() != 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--default")])
        );
        return ExitCode::from(2);
    }
    let values = match instance_list_values(catalog) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    if selected.len() != 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--default")])
        );
        return ExitCode::from(2);
    }
    finish(
        catalog,
        request(
            catalog,
            "instance.default",
            json!({ "id": field(selected[0], "id") }),
        ),
    )
}

#[cfg(unix)]
fn instance_info_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        return finish(catalog, request(catalog, "instance.info", json!({})));
    }
    let values = match instance_list_values(catalog) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    let mut results = Vec::new();
    for instance in &selected {
        match call(
            catalog,
            "instance.info",
            json!({ "id": field(instance, "id") }),
        ) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if results.len() == 1 && !parsed.flag("json") {
        return print_json(&results[0]);
    }
    print_json(&Value::Array(results))
}

const INSTANCE_QUERY_FLAGS: [&str; 11] = [
    "name",
    "version",
    "memory",
    "jvm-args",
    "lwjgl",
    "loader",
    "loader-version",
    "type",
    "account",
    "group",
    "create-defaults",
];

fn instance_query_flags(parsed: &CommandArgs) -> impl Iterator<Item = &'static str> + '_ {
    INSTANCE_QUERY_FLAGS.iter().copied().filter(move |flag| {
        (*flag != "group" || !parsed.positionals.is_empty())
            && parsed.flag(flag)
            && parsed.value(flag).is_none()
    })
}

#[cfg(unix)]
fn instance_query(parsed: &CommandArgs, flags: &[&str], catalog: &Catalog) -> ExitCode {
    if !parsed.positionals.is_empty() && flags.contains(&"create-defaults") {
        eprintln!(
            "{}",
            catalog.format("cli.error.global_setting", &[("flag", "--create-defaults")])
        );
        return ExitCode::from(2);
    }
    let json_output = parsed.flag("json");
    if parsed.positionals.is_empty() {
        let mut global = Vec::new();
        let mut unsupported = None;
        for flag in flags {
            if matches!(
                *flag,
                "version" | "memory" | "jvm-args" | "group" | "type" | "loader" | "create-defaults"
            ) {
                global.push(*flag);
            } else if unsupported.is_none() {
                unsupported = Some(*flag);
            }
        }
        if let Some(flag) = unsupported {
            eprintln!(
                "{}",
                catalog.format("cli.error.global_only", &[("flag", &format!("--{flag}"))])
            );
            return ExitCode::from(2);
        }
        let value = match call(catalog, "settings.get", json!({})) {
            Ok(value) => value,
            Err(code) => return code,
        };
        let mut out = serde_json::Map::new();
        for flag in global {
            let key = match flag {
                "memory" => "memory_mb",
                "jvm-args" => "jvm_args",
                "create-defaults" => "create_defaults",
                other => other,
            };
            out.insert(
                flag.to_owned(),
                value.get(key).cloned().unwrap_or(Value::Null),
            );
        }
        if json_output {
            return print_json(&Value::Object(out));
        }
        for (key, value) in out {
            println!("{key} = {value}");
        }
        return ExitCode::SUCCESS;
    }

    let values = match instance_list_values(catalog) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    let mut results = Vec::new();
    for instance in &selected {
        let value = match call(
            catalog,
            "instance.info",
            json!({"id": field(instance, "id")}),
        ) {
            Ok(value) => value,
            Err(code) => return code,
        };
        let mut out = serde_json::Map::new();
        out.insert("id".to_owned(), json!(field(instance, "id")));
        for flag in flags {
            out.insert((*flag).to_owned(), instance_field(&value, flag));
        }
        results.push(Value::Object(out));
    }
    if json_output {
        return print_json(&Value::Array(results));
    }
    for result in results {
        let id = result.get("id").and_then(Value::as_str).unwrap_or("");
        for flag in flags {
            let value = result.get(*flag).cloned().unwrap_or(Value::Null);
            println!("{id}: {flag} = {value}");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn instance_components(instance: &Value) -> &[Value] {
    instance
        .pointer("/profile/components")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

#[cfg(unix)]
fn query_component_version(instance: &Value, uid: &str) -> Value {
    instance_components(instance)
        .iter()
        .find(|component| component.get("uid").and_then(Value::as_str) == Some(uid))
        .and_then(|component| component.get("version"))
        .cloned()
        .unwrap_or(Value::Null)
}

#[cfg(unix)]
fn instance_field(instance: &Value, flag: &str) -> Value {
    match flag {
        "name" => instance.get("name").cloned().unwrap_or(Value::Null),
        "type" => instance.get("kind").cloned().unwrap_or(Value::Null),
        "group" => instance.get("group").cloned().unwrap_or(Value::Null),
        "account" => instance.get("account").cloned().unwrap_or(Value::Null),
        "memory" => instance
            .pointer("/config/memory_mb")
            .cloned()
            .unwrap_or(Value::Null),
        "jvm-args" => instance
            .pointer("/config/jvm_args")
            .cloned()
            .unwrap_or(Value::Null),
        "lwjgl" => instance
            .pointer("/config/lwjgl")
            .cloned()
            .unwrap_or(Value::Null),
        "version" => query_component_version(instance, "net.minecraft"),
        "loader-version" => {
            for uid in [
                "net.fabricmc.fabric-loader",
                "net.minecraftforge",
                "net.neoforged",
                "org.purpurmc.purpur",
            ] {
                let value = query_component_version(instance, uid);
                if !value.is_null() {
                    return value;
                }
            }
            Value::Null
        }
        "loader" => {
            for (uid, label) in [
                ("net.fabricmc.fabric-loader", "fabric"),
                ("net.minecraftforge", "forge"),
                ("net.neoforged", "neoforge"),
                ("org.purpurmc.purpur", "purpur"),
            ] {
                if !query_component_version(instance, uid).is_null() {
                    return Value::String(label.to_owned());
                }
            }
            Value::Null
        }
        _ => Value::Null,
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_lines)] // flag orchestration; split when more flags are added
fn instance_update_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.value("group-move").is_some() {
        return instance_group_move_action(parsed, catalog);
    }
    if let Some(to) = parsed.value("move") {
        if parsed.positionals.len() != 1 {
            eprintln!(
                "{}",
                catalog.format("cli.error.batch", &[("flag", "--move")])
            );
            return ExitCode::from(2);
        }
        let values = match instance_list_values(catalog) {
            Ok(values) => values,
            Err(code) => return code,
        };
        let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
            Ok(selected) => selected,
            Err(code) => return code,
        };
        if selected.len() != 1 {
            eprintln!(
                "{}",
                catalog.format("cli.error.batch", &[("flag", "--move")])
            );
            return ExitCode::from(2);
        }
        return finish(
            catalog,
            request(
                catalog,
                "instance.move",
                json!({"id": field(selected[0], "id"), "to": to}),
            ),
        );
    }
    if parsed.flag("link") && parsed.value("link").is_none() {
        return instance_link_query(parsed, catalog);
    }
    let queries: Vec<&str> = instance_query_flags(parsed).collect();
    if !queries.is_empty() {
        return instance_query(parsed, &queries, catalog);
    }
    if parsed.positionals.is_empty() {
        let mut update = serde_json::Map::new();
        if let Some(value) = parsed.value("version") {
            update.insert("version".to_owned(), json!(value));
        }
        if let Some(raw) = parsed.value("memory") {
            let Ok(memory) = raw.parse::<u32>() else {
                eprintln!("{}", catalog.format("cli.error.memory", &[("value", raw)]));
                return ExitCode::from(2);
            };
            if memory < 256 {
                eprintln!("{}", catalog.format("cli.error.memory", &[("value", raw)]));
                return ExitCode::from(2);
            }
            update.insert("memory_mb".to_owned(), json!(memory));
        }
        if let Some(raw) = parsed.value("jvm-args") {
            match parse_jvm_args(raw) {
                Ok(args) => {
                    update.insert("jvm_args".to_owned(), json!(args));
                }
                Err(error) => {
                    eprintln!(
                        "{}",
                        catalog.format("cli.error.jvm_args", &[("error", &error)])
                    );
                    return ExitCode::from(2);
                }
            }
        }
        if let Some(value) = parsed.value("group") {
            update.insert("group".to_owned(), json!(value));
        }
        if let Some(value) = parsed.value("link") {
            if !matches!(value, "all" | "none") {
                eprintln!(
                    "{}",
                    catalog.format("cli.error.global_only", &[("flag", "--link")])
                );
                return ExitCode::from(2);
            }
            update.insert("linking".to_owned(), json!(value));
        }
        if let Some(value) = parsed.value("java") {
            update.insert("java".to_owned(), json!(value));
        }
        if let Some(value) = parsed.value("type") {
            update.insert("type".to_owned(), json!(value.to_ascii_lowercase()));
        }
        if let Some(value) = parsed.value("loader") {
            update.insert("loader".to_owned(), json!(value));
        }
        if let Some(raw) = parsed.value("create-defaults") {
            let value = match raw {
                "true" => true,
                "false" => false,
                _ => {
                    eprintln!("{}", catalog.format("cli.error.boolean", &[("value", raw)]));
                    return ExitCode::from(2);
                }
            };
            update.insert("create_defaults".to_owned(), json!(value));
        }
        if !update.is_empty() {
            return finish(
                catalog,
                request(catalog, "settings.update", Value::Object(update)),
            );
        }
        for flag in ["name", "loader-version", "lwjgl", "account"] {
            if parsed.value(flag).is_some() {
                eprintln!(
                    "{}",
                    catalog.format("cli.error.global_only", &[("flag", &format!("--{flag}"))])
                );
                return ExitCode::from(2);
            }
        }
    }
    if parsed.positionals.is_empty() {
        return missing_flag(catalog, "<id>");
    }
    if parsed.value("create-defaults").is_some() {
        eprintln!(
            "{}",
            catalog.format("cli.error.global_setting", &[("flag", "--create-defaults")])
        );
        return ExitCode::from(2);
    }
    if parsed.value("type").is_some() {
        eprintln!("{}", catalog.text("cli.error.type_create"));
        return ExitCode::from(2);
    }
    let memory = match parsed.value("memory") {
        Some(raw) => match raw.parse::<u32>() {
            Ok(value) if value >= 256 => Some(value),
            Ok(_) | Err(_) => {
                eprintln!("{}", catalog.format("cli.error.memory", &[("value", raw)]));
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    if parsed.value("name").is_some() && parsed.positionals.len() > 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--name")])
        );
        return ExitCode::from(2);
    }
    let values = match instance_list_values(catalog) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    if let Some(memory) = memory {
        let minimum = selected
            .iter()
            .map(|instance| {
                if instance.get("kind").and_then(Value::as_str) == Some("server") {
                    256
                } else {
                    512
                }
            })
            .max()
            .unwrap_or(512);
        if memory < minimum {
            eprintln!(
                "{}",
                catalog.format(
                    "cli.error.memory",
                    &[("value", parsed.value("memory").unwrap_or_default())]
                )
            );
            return ExitCode::from(2);
        }
    }
    if selected.len() > 1 && !confirm(selected.len(), catalog, parsed.flag("yes")) {
        println!("{}", catalog.text("cli.confirm.cancelled"));
        return ExitCode::SUCCESS;
    }
    let mut params = json!({});
    if let Some(name) = parsed.value("name") {
        params["name"] = json!(name);
    }
    if let Some(group) = parsed.value("group") {
        params["group"] = json!(group);
    }
    if let Some(account) = parsed.value("account") {
        params["account"] = json!(account);
    }
    if let Some(version) = parsed.value("version") {
        params["minecraft"] = json!(version);
    }
    if let Some(loader) = parsed.value("loader") {
        let loader = loader.to_ascii_lowercase();
        if !matches!(
            loader.as_str(),
            "fabric" | "forge" | "neoforge" | "purpur" | "none"
        ) {
            eprintln!(
                "{}",
                catalog.format("cli.error.loader", &[("loader", loader.as_str())])
            );
            return ExitCode::from(2);
        }
        params["loader"] = json!(loader);
    }
    if let Some(loader_version) = parsed.value("loader-version") {
        params["loader_version"] = json!(loader_version);
    }
    if let Some(memory) = memory {
        params["memory_mb"] = json!(memory);
    }
    if let Some(lwjgl) = parsed.value("lwjgl") {
        params["lwjgl"] = json!(lwjgl);
    }
    if let Some(java) = parsed.value("java") {
        params["java"] = json!(java);
    }
    if let Some(raw) = parsed.value("jvm-args") {
        match parse_jvm_args(raw) {
            Ok(args) => params["jvm_args"] = json!(args),
            Err(error) => {
                eprintln!(
                    "{}",
                    catalog.format("cli.error.jvm_args", &[("error", &error)])
                );
                return ExitCode::from(2);
            }
        }
    }
    if let Some(raw) = parsed.value("link") {
        return instance_link_action(&selected, raw, catalog, parsed.flag("json"));
    }
    let mut results = Vec::new();
    for instance in &selected {
        let mut request_params = params.clone();
        request_params["id"] = json!(field(instance, "id"));
        match call(catalog, "instance.update", request_params) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if parsed.flag("json") {
        return print_json(&Value::Array(results));
    }
    println!(
        "{}",
        catalog.format(
            "cli.instance.updated",
            &[("count", &selected.len().to_string())]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn instance_link_query(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        let value = match call(catalog, "settings.get", json!({})) {
            Ok(value) => value,
            Err(code) => return code,
        };
        if parsed.flag("json") {
            return print_json(&value);
        }
        println!(
            "link = {}",
            value.get("linking").cloned().unwrap_or(Value::Null)
        );
        return ExitCode::SUCCESS;
    }
    let values = match instance_list_values(catalog) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    let mut results = Vec::new();
    for instance in &selected {
        match call(
            catalog,
            "instance.links",
            json!({"id": field(instance, "id")}),
        ) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if parsed.flag("json") {
        return print_json(&Value::Array(results));
    }
    for result in results {
        let id = result.get("id").and_then(Value::as_str).unwrap_or("");
        println!("[{id}]");
        for link in result
            .get("links")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = link.get("name").and_then(Value::as_str).unwrap_or("");
            let path = link.get("path").and_then(Value::as_str).unwrap_or("");
            let status = link.get("status").and_then(Value::as_str).unwrap_or("");
            let label = match status {
                "linked" => catalog.text("cli.instance.link_linked"),
                "real" => catalog.text("cli.instance.link_real"),
                "missing" => catalog.text("cli.instance.link_missing"),
                _ => catalog.text("cli.instance.link_foreign"),
            };
            println!("  {name} ({path}): {label}");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn instance_group_move_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let Some(from) = parsed.value("group-move") else {
        return missing_flag(catalog, "--group-move <from> <to>");
    };
    let Some(to) = parsed.positionals.first() else {
        return missing_flag(catalog, "<to>");
    };
    if let Some(extra) = parsed.positionals.get(1) {
        return unexpected(catalog, extra);
    }
    let value = match call(
        catalog,
        "instance.group_move",
        json!({ "from": from, "to": to }),
    ) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    let count = value
        .get("moved")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    println!(
        "{}",
        catalog.format(
            "cli.instance.group_moved",
            &[("count", &count.to_string()), ("from", from), ("to", to)]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn instance_link_action(
    selected: &[&Value],
    raw: &str,
    catalog: &Catalog,
    json_output: bool,
) -> ExitCode {
    let (links, remove) = match raw.trim() {
        "all" => (json!("all"), false),
        "none" => (json!("all"), true),
        value => {
            let names: Vec<&str> = value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect();
            if names.is_empty() {
                eprintln!("{}", catalog.format("cli.error.link", &[("value", value)]));
                return ExitCode::from(2);
            }
            (json!(names), false)
        }
    };
    let mut results = Vec::new();
    for instance in selected {
        let params = json!({
            "id": field(instance, "id"),
            "links": links,
            "remove": remove,
        });
        match call(catalog, "instance.link", params) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if json_output {
        return print_json(&Value::Array(results));
    }
    let count = |key: &str| -> usize {
        results
            .iter()
            .filter_map(|value| value.get(key))
            .filter_map(Value::as_array)
            .map(Vec::len)
            .sum()
    };
    println!(
        "{}",
        catalog.format(
            "cli.instance.linked",
            &[
                ("created", &count("created").to_string()),
                ("removed", &count("removed").to_string()),
                ("skipped", &count("skipped").to_string()),
            ]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn instance_groups_action(catalog: &Catalog) -> ExitCode {
    let value = match call(catalog, "instance.groups", json!({})) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let groups = value.as_array().cloned().unwrap_or_default();
    if groups.is_empty() {
        println!("{}", catalog.text("cli.instance.groups_empty"));
        return ExitCode::SUCCESS;
    }
    for group in groups {
        if let Some(group) = group.as_str() {
            println!("{group}");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn instance_list_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let values = match instance_list_values(catalog) {
        Ok(values) => values,
        Err(code) => return code,
    };
    let mut selected = match select(&values, &parsed.positionals, &["id", "name"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    if let Some(group) = parsed.value("group") {
        selected.retain(|value| field(value, "group") == group);
    }
    let ascending = match parse_order(parsed, catalog) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let key = parsed.value("list-sort-by").unwrap_or("create");
    if let Err(code) = sort_values(&mut selected, key, ascending, catalog) {
        return code;
    }
    if parsed.flag("json") {
        let owned: Vec<Value> = selected.into_iter().cloned().collect();
        return print_json(&Value::Array(owned));
    }
    print_instances(
        &selected.into_iter().cloned().collect::<Vec<Value>>(),
        catalog,
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn instance_java_query(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        let settings = match call(catalog, "settings.get", json!({})) {
            Ok(value) => value,
            Err(code) => return code,
        };
        let java = settings
            .get("java")
            .cloned()
            .unwrap_or_else(|| json!("default"));
        if parsed.flag("json") {
            return print_json(&json!({ "java": java }));
        }
        println!("java = {}", java.as_str().unwrap_or("default"));
        return ExitCode::SUCCESS;
    }
    if parsed.positionals.len() > 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--java")])
        );
        return ExitCode::from(2);
    }
    let mut params = json!({});
    if let Some(id) = parsed.positionals.first() {
        params["id"] = json!(id);
    }
    let value = match call(catalog, "instance.java", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    println!("java = {}", field(&value, "java"));
    if let Some(version) = value.get("version").and_then(Value::as_u64) {
        println!("version = {version}");
    }
    println!("source = {}", field(&value, "source"));
    ExitCode::SUCCESS
}

fn instance_list_values(catalog: &Catalog) -> Result<Vec<Value>, ExitCode> {
    let value = call(catalog, "instance.list", json!({}))?;
    Ok(value.as_array().cloned().unwrap_or_default())
}

#[cfg(unix)]
fn launch_command(args: &[String], catalog: &Catalog) -> ExitCode {
    let parsed = match parse_command_args(
        args,
        &["list-sort", "list-sort-by", "prepare-download-threads"],
        &[],
        &[
            "list", "terminal", "prepare", "start", "stop", "json", "yes",
        ],
        catalog,
    ) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    if parsed.help {
        print!("{}", catalog.text("cli.help"));
        return ExitCode::SUCCESS;
    }
    if let Some(flag) = action_conflict(&parsed, &["stop", "start", "prepare", "terminal", "list"])
    {
        return unexpected(catalog, &format!("--{flag}"));
    }
    if parsed.flag("stop") {
        if let Some(code) = reject_flags(
            &parsed,
            catalog,
            &[
                "account",
                "prepare-download-threads",
                "list-sort",
                "list-sort-by",
            ],
        ) {
            return code;
        }
    }
    if parsed.flag("terminal") {
        if let Some(code) = reject_flags(
            &parsed,
            catalog,
            &[
                "account",
                "prepare-download-threads",
                "list-sort",
                "list-sort-by",
            ],
        ) {
            return code;
        }
    }
    if parsed.flag("prepare") || parsed.flag("start") {
        if let Some(code) = reject_flags(&parsed, catalog, &["list-sort", "list-sort-by"]) {
            return code;
        }
    }
    if parsed.flag("list")
        || (!parsed.flag("stop")
            && !parsed.flag("start")
            && !parsed.flag("prepare")
            && !parsed.flag("terminal"))
    {
        if let Some(code) = reject_flags(&parsed, catalog, &["account", "prepare-download-threads"])
        {
            return code;
        }
    }
    if parsed.flag("stop") {
        return launch_stop_action(&parsed, catalog);
    }
    if parsed.flag("start") {
        return launch_start_action(&parsed, catalog);
    }
    if parsed.flag("prepare") {
        return launch_prepare_action(&parsed, catalog);
    }
    if parsed.flag("terminal") || !parsed.positionals.is_empty() {
        return launch_terminal_action(&parsed, catalog);
    }
    launch_list_action(&parsed, catalog)
}

#[cfg(unix)]
fn launch_list_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let value = match call(catalog, "launch.list", json!({})) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let values = value.as_array().cloned().unwrap_or_default();
    let mut selected = values.iter().collect::<Vec<&Value>>();
    if !parsed.positionals.is_empty() {
        let mut seen = HashSet::new();
        selected.retain(|session| {
            let id = field(session, "id");
            parsed
                .positionals
                .iter()
                .any(|pattern| wildcard(pattern, id))
                && seen.insert(id.to_owned())
        });
    }
    let ascending = match parse_order(parsed, catalog) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let key = parsed.value("list-sort-by").unwrap_or("launch");
    if let Err(code) = sort_values(&mut selected, key, ascending, catalog) {
        return code;
    }
    if parsed.flag("json") {
        let owned: Vec<Value> = selected.into_iter().cloned().collect();
        return print_json(&Value::Array(owned));
    }
    print_sessions(
        &selected.into_iter().cloned().collect::<Vec<Value>>(),
        catalog,
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn launch_stop_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    if parsed.positionals.is_empty() {
        return missing_flag(catalog, "<instance_id>");
    }
    let value = match call(catalog, "launch.list", json!({})) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let values = value.as_array().cloned().unwrap_or_default();
    let selected = match select(&values, &parsed.positionals, &["id"], catalog) {
        Ok(selected) => selected,
        Err(code) => return code,
    };
    if !confirm(selected.len(), catalog, parsed.flag("yes")) {
        println!("{}", catalog.text("cli.confirm.cancelled"));
        return ExitCode::SUCCESS;
    }
    let mut results = Vec::new();
    for session in &selected {
        match call(
            catalog,
            "launch.stop",
            json!({ "id": field(session, "id") }),
        ) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if parsed.flag("json") {
        return print_json(&Value::Array(results));
    }
    println!(
        "{}",
        catalog.format(
            "cli.launch.stopped",
            &[("count", &selected.len().to_string())]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn launch_single_id(
    parsed: &CommandArgs,
    catalog: &Catalog,
    flag: &str,
) -> Result<Option<String>, ExitCode> {
    match parsed.positionals.len() {
        0 => Ok(None),
        1 => Ok(Some(parsed.positionals[0].clone())),
        _ => {
            eprintln!("{}", catalog.format("cli.error.batch", &[("flag", flag)]));
            Err(ExitCode::from(2))
        }
    }
}

#[cfg(unix)]
fn default_instance_id(catalog: &Catalog) -> Result<String, ExitCode> {
    let value = call(catalog, "instance.info", json!({}))?;
    let id = field(&value, "id").to_owned();
    if id.is_empty() {
        eprintln!("{}", catalog.text("cli.error.default_instance"));
        return Err(ExitCode::from(2));
    }
    Ok(id)
}

#[cfg(unix)]
fn resolve_instance(id: Option<String>, catalog: &Catalog) -> Result<String, ExitCode> {
    match id {
        Some(id) => Ok(id),
        None => default_instance_id(catalog),
    }
}

#[cfg(unix)]
fn prepare_download_threads(parsed: &CommandArgs, catalog: &Catalog) -> Result<u64, ExitCode> {
    const DEFAULT: u64 = 8;
    const MAX: u64 = 64;
    match parsed.value("prepare-download-threads") {
        None => Ok(DEFAULT),
        Some(raw) => match raw.parse::<u64>() {
            Ok(value) if (1..=MAX).contains(&value) => Ok(value),
            _ => {
                eprintln!("{}", catalog.format("cli.error.threads", &[("value", raw)]));
                Err(ExitCode::from(2))
            }
        },
    }
}

fn launch_prepare_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let threads = match prepare_download_threads(parsed, catalog) {
        Ok(threads) => threads,
        Err(code) => return code,
    };
    let id = match launch_single_id(parsed, catalog, "--prepare") {
        Ok(id) => id,
        Err(code) => return code,
    };
    let id = match resolve_instance(id, catalog) {
        Ok(id) => id,
        Err(code) => return code,
    };
    let params = json!({ "id": id, "download_threads": threads });
    finish(catalog, request(catalog, "launch.prepare", params))
}

#[cfg(unix)]
fn launch_start_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let threads = match prepare_download_threads(parsed, catalog) {
        Ok(threads) => threads,
        Err(code) => return code,
    };
    let id = match launch_single_id(parsed, catalog, "--start") {
        Ok(id) => id,
        Err(code) => return code,
    };
    let id = match resolve_instance(id, catalog) {
        Ok(id) => id,
        Err(code) => return code,
    };
    let params = json!({ "id": id, "download_threads": threads });
    if let Err(code) = call(catalog, "launch.prepare", params.clone()) {
        return code;
    }
    let started = match call(catalog, "launch.start", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&started);
    }
    let pid = started
        .get("pid")
        .and_then(Value::as_u64)
        .map_or_else(|| "?".to_owned(), |value| value.to_string());
    println!(
        "{}",
        catalog.format("cli.launch.started", &[("id", &id), ("pid", &pid)])
    );
    terminal_follow(&id, catalog)
}

#[cfg(unix)]
fn launch_terminal_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let id = match launch_single_id(parsed, catalog, "--terminal") {
        Ok(id) => id,
        Err(code) => return code,
    };
    let id = match resolve_instance(id, catalog) {
        Ok(id) => id,
        Err(code) => return code,
    };
    if let Err(code) = call(catalog, "launch.terminal", json!({ "id": id })) {
        return code;
    }
    terminal_follow(&id, catalog)
}

#[cfg(unix)]
struct ConsoleExit {
    code: Option<i64>,
    crashed: bool,
    report: String,
    had_output: bool,
}

#[cfg(unix)]
struct TerminalGuard {
    saved: Option<String>,
    alternate: bool,
}

#[cfg(unix)]
impl TerminalGuard {
    fn enter(hint: &str, catalog: &Catalog) -> Self {
        let inactive = || Self {
            saved: None,
            alternate: false,
        };
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("{}", catalog.text("cli.launch.terminal"));
            return inactive();
        }
        let saved = Command::new("stty")
            .arg("-g")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|saved| !saved.is_empty());
        let Some(saved) = saved else {
            eprintln!("{}", catalog.text("cli.launch.terminal"));
            return inactive();
        };
        let applied = matches!(
            Command::new("stty")
                .args([
                    "-icanon", "-echo", "-isig", "-ixon", "-icrnl", "min", "1", "time", "0"
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
            Ok(status) if status.success()
        );
        if !applied {
            eprintln!("{}", catalog.text("cli.launch.terminal"));
            return inactive();
        }
        print!("\x1b[?1049h\x1b[2J\x1b[H");
        print_console_hint(hint);
        Self {
            saved: Some(saved),
            alternate: true,
        }
    }

    fn restore(&mut self) {
        if let Some(saved) = self.saved.take() {
            let _ = Command::new("stty")
                .arg(saved)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        if self.alternate {
            self.alternate = false;
            print!("\x1b[?1049l");
            let _ = std::io::stdout().flush();
        }
    }
}

#[cfg(unix)]
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
fn print_console_hint(hint: &str) {
    print!("\x1b[7m {hint} \x1b[0m\n\n> ");
    let _ = std::io::stdout().flush();
}

#[cfg(unix)]
fn send_console_input(id: &str, line: &str) {
    let Ok(paths) = Paths::discover() else {
        return;
    };
    let request =
        arkmcl_platform::ipc::Request::new(1, "launch.input", json!({"id": id, "line": line}));
    let _ = arkmcl_platform::ipc::call(&paths.socket(), &request);
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum ConsoleKey {
    Detach,
    Submit,
    Backspace,
    Clear,
    Insert(u8),
    Ignore,
}

#[cfg(unix)]
fn console_key(byte: u8) -> ConsoleKey {
    match byte {
        0x18 | 0x03 | 0x04 => ConsoleKey::Detach,
        b'\r' | b'\n' => ConsoleKey::Submit,
        0x7f | 0x08 => ConsoleKey::Backspace,
        0x0c => ConsoleKey::Clear,
        byte if byte >= 0x20 => ConsoleKey::Insert(byte),
        _ => ConsoleKey::Ignore,
    }
}

#[cfg(unix)]
fn terminal_input(id: &str, hint: &str, detach: &AtomicBool) {
    use std::io::Read;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let mut byte = [0u8; 1];
    let mut line: Vec<u8> = Vec::new();
    let mut skip = 0u8;
    loop {
        if detach.load(Ordering::SeqCst) {
            break;
        }
        match reader.read(&mut byte) {
            Ok(0) | Err(_) => {
                detach.store(true, Ordering::SeqCst);
                break;
            }
            Ok(_) => {}
        }
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if byte[0] == 0x1b {
            skip = 2;
            continue;
        }
        match console_key(byte[0]) {
            ConsoleKey::Detach => {
                detach.store(true, Ordering::SeqCst);
                break;
            }
            ConsoleKey::Submit => {
                let text = String::from_utf8_lossy(&line).trim().to_owned();
                line.clear();
                if !text.is_empty() {
                    send_console_input(id, &text);
                }
                print!("\n> ");
                let _ = std::io::stdout().flush();
            }
            ConsoleKey::Backspace => {
                if let Some(start) = line.iter().rposition(|byte| (*byte & 0xc0) != 0x80) {
                    line.truncate(start);
                    print!("\x08 \x08");
                    let _ = std::io::stdout().flush();
                }
            }
            ConsoleKey::Clear => {
                print!("\x1b[2J\x1b[H");
                print_console_hint(hint);
                let _ = std::io::stdout().write_all(&line);
                let _ = std::io::stdout().flush();
            }
            ConsoleKey::Insert(byte) => {
                line.push(byte);
                let _ = std::io::stdout().write_all(&[byte]);
                let _ = std::io::stdout().flush();
            }
            ConsoleKey::Ignore => {}
        }
    }
}

#[cfg(unix)]
fn terminal_input_lines(id: &str) {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        send_console_input(id, &line);
    }
}

#[cfg(unix)]
fn terminal_follow(id: &str, catalog: &Catalog) -> ExitCode {
    let hint = catalog.format("cli.launch.terminal_hint", &[("id", id)]);
    let guard = TerminalGuard::enter(&hint, catalog);
    let interactive = guard.alternate;
    let detach = std::sync::Arc::new(AtomicBool::new(false));
    let input_id = id.to_owned();
    let input_detach = std::sync::Arc::clone(&detach);
    thread::spawn(move || {
        if interactive {
            terminal_input(&input_id, &hint, &input_detach);
        } else {
            terminal_input_lines(&input_id);
        }
    });
    let mut cursor = 0u64;
    let mut failure = None;
    let mut exit = None;
    loop {
        if detach.load(Ordering::SeqCst) {
            break;
        }
        let value = match call(
            catalog,
            "launch.output",
            json!({"id": id, "cursor": cursor}),
        ) {
            Ok(value) => value,
            Err(code) => {
                failure = Some(code);
                break;
            }
        };
        let chunk = field(&value, "output");
        if !chunk.is_empty() {
            print!("{chunk}");
            let _ = std::io::stdout().flush();
        }
        cursor = value
            .get("cursor")
            .and_then(Value::as_u64)
            .unwrap_or(cursor);
        if !value
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            exit = Some(ConsoleExit {
                code: value.get("exit_code").and_then(Value::as_i64),
                crashed: value
                    .get("crashed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                report: field(&value, "crash_report").to_owned(),
                had_output: !chunk.is_empty(),
            });
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    let detached = detach.load(Ordering::SeqCst);
    drop(guard);
    if detached {
        println!("{}", catalog.text("cli.launch.detached"));
        return ExitCode::SUCCESS;
    }
    if let Some(code) = failure {
        return code;
    }
    let Some(exit) = exit else {
        return ExitCode::SUCCESS;
    };
    if exit.code.is_none() && !exit.had_output && !exit.crashed {
        eprintln!("{}", catalog.text("cli.launch.not_running"));
        return ExitCode::from(1);
    }
    if let Some(code) = exit.code {
        println!(
            "{}",
            catalog.format("cli.launch.exit", &[("code", &code.to_string())])
        );
    } else {
        println!("{}", catalog.text("cli.launch.exited"));
    }
    if exit.crashed {
        if exit.report.is_empty() {
            eprintln!("{}", catalog.text("cli.launch.crashed"));
        } else {
            eprintln!(
                "{}",
                catalog.format("cli.launch.crashed_report", &[("report", &exit.report)])
            );
        }
        return ExitCode::from(1);
    }
    if exit.code.unwrap_or(0) == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_lines)] // command dispatch; split by mode when it grows further
fn resource_command(args: &[String], catalog: &Catalog) -> ExitCode {
    let parsed = match parse_command_args(
        args,
        &[
            "filter-vanilla",
            "filter-version",
            "filter-loader",
            "type",
            "api",
            "lookup",
            "download",
            "download-version",
            "download-to-instance",
            "search",
            "list-limit",
            "list-sort",
            "list-sort-by",
            "game-version",
            "loader",
            "list-download-to-instance",
            "download-mod",
            "download-modpack",
            "download-resourcepack",
            "download-shader",
            "download-datapack",
            "download-world",
            "download-plugin",
        ],
        &[],
        &["list", "list-remote", "autoremove", "json", "yes"],
        catalog,
    ) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    if parsed.help {
        print!("{}", catalog.text("cli.help"));
        return ExitCode::SUCCESS;
    }
    if let Some(flag) = action_conflict(
        &parsed,
        &[
            "autoremove",
            "list-remote",
            "search",
            "lookup",
            "download",
            "download-mod",
            "download-modpack",
            "download-resourcepack",
            "download-shader",
            "download-datapack",
            "download-world",
            "download-plugin",
            "list",
        ],
    ) {
        return unexpected(catalog, &format!("--{flag}"));
    }
    if let Some(code) = validate_resource_actions(&parsed, catalog) {
        return code;
    }
    if parsed.flag("autoremove") {
        return resource_autoremove_action(&parsed, catalog);
    }
    if parsed.flag("list-remote") {
        let loaders = filtered_values(&parsed, "filter-loader");
        if loaders.is_empty() {
            return resource_list_remote_action(&parsed, catalog);
        }
        return resource_list_remote_loaders_action(&parsed, &loaders, catalog);
    }
    if let Some(query) = parsed.value("search") {
        let Some(kind) = parsed.value("type") else {
            return missing_flag(catalog, "--type");
        };
        let Some(kind) = search_kind(kind) else {
            eprintln!("{}", catalog.format("rpc.resource.kind", &[("kind", kind)]));
            return ExitCode::from(2);
        };
        return resource_search_new(&parsed, kind, query, catalog);
    }
    if let Some(id) = parsed.value("lookup") {
        return resource_lookup_new(&parsed, id, catalog);
    }
    if !parsed.all("download").is_empty() {
        return resource_download_new(&parsed, catalog);
    }
    let downloads = [
        ("mod", parsed.value("download-mod")),
        ("modpack", parsed.value("download-modpack")),
        ("resourcepack", parsed.value("download-resourcepack")),
        ("shader", parsed.value("download-shader")),
        ("datapack", parsed.value("download-datapack")),
        ("world", parsed.value("download-world")),
        ("plugin", parsed.value("download-plugin")),
    ];
    let selected: Vec<(&str, &str)> = downloads
        .into_iter()
        .filter_map(|(kind, id)| id.map(|id| (kind, id)))
        .collect();
    if selected.len() > 1 {
        eprintln!(
            "{}",
            catalog.format("cli.error.batch", &[("flag", "--download-*")])
        );
        return ExitCode::from(2);
    }
    if let Some((kind, id)) = selected.first() {
        return resource_download_action(&parsed, kind, id, catalog);
    }
    resource_local_list(&parsed, catalog)
}

#[cfg(unix)]
fn filtered_values(parsed: &CommandArgs, name: &str) -> Vec<String> {
    parsed
        .all(name)
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(unix)]
fn resource_list_remote_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let sort_by = parsed.value("list-sort-by").unwrap_or("create");
    if !matches!(sort_by, "create" | "alphabet") {
        eprintln!(
            "{}",
            catalog.format("cli.error.sort_key", &[("value", sort_by)])
        );
        return ExitCode::from(2);
    }
    let order = match parse_order(parsed, catalog) {
        Ok(order) => order,
        Err(code) => return code,
    };
    let mut params = json!({
        "sort_by": sort_by,
        "order": if order { "asc" } else { "desc" },
    });
    let vanilla = filtered_values(parsed, "filter-vanilla");
    if !vanilla.is_empty() {
        params["vanilla"] = json!(vanilla);
    }
    let versions = filtered_values(parsed, "filter-version");
    if !versions.is_empty() {
        params["versions"] = json!(versions);
    }
    if let Some(raw) = parsed.value("list-limit") {
        let Ok(limit) = raw.parse::<u64>() else {
            eprintln!(
                "{}",
                catalog.format("cli.resource.limit", &[("value", raw)])
            );
            return ExitCode::from(2);
        };
        if limit == 0 {
            eprintln!(
                "{}",
                catalog.format("cli.resource.limit", &[("value", raw)])
            );
            return ExitCode::from(2);
        }
        params["limit"] = json!(limit);
    }
    let value = match call(catalog, "resource.list_remote", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    let entries = value
        .get("versions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if entries.is_empty() {
        println!("{}", catalog.text("cli.resource.empty"));
        return ExitCode::SUCCESS;
    }
    for entry in entries {
        println!(
            "  {}  {}  {}",
            field(&entry, "id"),
            field(&entry, "type"),
            field(&entry, "release_time")
        );
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn resource_list_remote_loaders_action(
    parsed: &CommandArgs,
    loaders: &[String],
    catalog: &Catalog,
) -> ExitCode {
    let order = match parse_order(parsed, catalog) {
        Ok(order) => order,
        Err(code) => return code,
    };
    let mut results: Vec<(String, Value)> = Vec::new();
    for loader in loaders {
        if !matches!(
            loader.as_str(),
            "fabric" | "quilt" | "forge" | "neoforge" | "purpur"
        ) {
            eprintln!(
                "{}",
                catalog.format("rpc.resource.kind", &[("kind", loader.as_str())])
            );
            return ExitCode::from(2);
        }
        let mut params = json!({
            "loader": loader,
            "order": if order { "asc" } else { "desc" },
        });
        let versions = filtered_values(parsed, "filter-version");
        if !versions.is_empty() {
            params["versions"] = json!(versions);
        }
        if let Some(raw) = parsed.value("list-limit") {
            let Ok(limit) = raw.parse::<u64>() else {
                eprintln!(
                    "{}",
                    catalog.format("cli.resource.limit", &[("value", raw)])
                );
                return ExitCode::from(2);
            };
            if limit == 0 {
                eprintln!(
                    "{}",
                    catalog.format("cli.resource.limit", &[("value", raw)])
                );
                return ExitCode::from(2);
            }
            params["limit"] = json!(limit);
        }
        let value = match call(catalog, "resource.list_remote_loaders", params) {
            Ok(value) => value,
            Err(code) => return code,
        };
        results.push((loader.clone(), value));
    }

    if parsed.flag("json") {
        let mut entries = Vec::new();
        for (loader, value) in results {
            if let Some(items) = value.get("loaders").and_then(Value::as_array) {
                for item in items {
                    let mut entry = item.clone();
                    entry["loader"] = json!(loader);
                    entries.push(entry);
                }
            }
        }
        return print_json(&json!({"loaders": entries}));
    }

    let mut printed = false;
    for (loader, value) in results {
        let entries = value
            .get("loaders")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if entries.is_empty() {
            continue;
        }
        printed = true;
        println!("[{loader}]");
        for entry in entries {
            let stable = entry
                .get("stable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            println!(
                "  {}  {}",
                field(&entry, "version"),
                if stable { "stable" } else { "unstable" }
            );
        }
    }
    if !printed {
        println!("{}", catalog.text("cli.resource.empty"));
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn search_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "worlds" => Some("world"),
        "resourcepacks" => Some("resourcepack"),
        "datapacks" => Some("datapack"),
        "shaderpacks" => Some("shader"),
        "mods" => Some("mod"),
        "plugins" => Some("plugin"),
        "modpacks" => Some("modpack"),
        _ => None,
    }
}

#[cfg(unix)]
fn resource_search_new(
    parsed: &CommandArgs,
    kind: &str,
    query: &str,
    catalog: &Catalog,
) -> ExitCode {
    let limit = match parsed.value("list-limit") {
        Some(raw) => match raw.parse::<u64>() {
            Ok(value) if (1..=100).contains(&value) => value,
            _ => {
                eprintln!(
                    "{}",
                    catalog.format("cli.resource.limit", &[("value", raw)])
                );
                return ExitCode::from(2);
            }
        },
        None => 20,
    };
    let order = match parse_order(parsed, catalog) {
        Ok(order) => order,
        Err(code) => return code,
    };
    let sort_by = parsed.value("list-sort-by").unwrap_or("relevance");
    if !matches!(sort_by, "create" | "download" | "name" | "relevance") {
        eprintln!(
            "{}",
            catalog.format("cli.error.sort_key", &[("value", sort_by)])
        );
        return ExitCode::from(2);
    }
    let mut params = json!({
        "type": kind,
        "query": query,
        "limit": limit,
        "sort_by": sort_by,
        "order": if order { "asc" } else { "desc" },
    });
    if let Some(api) = parsed.value("api") {
        params["api"] = json!(api);
    }
    let versions = filtered_values(parsed, "filter-version");
    if !versions.is_empty() {
        params["game_versions"] = json!(versions);
    }
    let loaders = filtered_values(parsed, "filter-loader");
    if !loaders.is_empty() {
        params["loaders"] = json!(loaders);
    }
    let value = match call(catalog, "resource.search", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    print_search_hits(&value, catalog)
}

#[cfg(unix)]
fn print_search_hits(value: &Value, catalog: &Catalog) -> ExitCode {
    let hits = value
        .get("hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if hits.is_empty() {
        println!("{}", catalog.text("cli.resource.empty"));
        return ExitCode::SUCCESS;
    }
    for hit in hits {
        let downloads = hit.get("downloads").and_then(Value::as_u64).unwrap_or(0);
        let slug = field(&hit, "slug");
        let id = if slug.is_empty() {
            field(&hit, "project_id")
        } else {
            slug
        };
        println!(
            "  {}  {}  {downloads}  {}",
            field(&hit, "title"),
            id,
            field(&hit, "description")
        );
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn resource_lookup_new(parsed: &CommandArgs, id: &str, catalog: &Catalog) -> ExitCode {
    let mut params = json!({"id": id});
    if let Some(api) = parsed.value("api") {
        params["api"] = json!(api);
    }
    let versions = filtered_values(parsed, "filter-version");
    if !versions.is_empty() {
        params["game_versions"] = json!(versions);
    }
    let loaders = filtered_values(parsed, "filter-loader");
    if !loaders.is_empty() {
        params["loaders"] = json!(loaders);
    }
    let value = match call(catalog, "resource.lookup", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    let versions = value
        .get("versions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if versions.is_empty() {
        println!("{}", catalog.text("cli.resource.empty"));
        return ExitCode::SUCCESS;
    }
    for version in versions {
        let games = join_strings(version.get("game_versions"));
        let loaders = join_strings(version.get("loaders"));
        println!(
            "  {}  {}  {games}  {loaders}",
            field(&version, "version_number"),
            field(&version, "name")
        );
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn join_strings(value: Option<&Value>) -> String {
    value.and_then(Value::as_array).map_or_else(
        || "-".to_owned(),
        |items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        },
    )
}

#[cfg(unix)]
fn resource_download_new(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let ids = parsed.all("download");
    let versions = parsed.all("download-version");
    if !versions.is_empty() && versions.len() != ids.len() {
        eprintln!("{}", catalog.text("cli.resource.versions"));
        return ExitCode::from(2);
    }
    let instance = parsed.value("download-to-instance");
    let mut results = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let mut params = json!({"id": id});
        if let Some(kind) = parsed.value("type") {
            params["type"] = json!(kind);
        }
        if let Some(api) = parsed.value("api") {
            params["api"] = json!(api);
        }
        if let Some(version) = versions.get(index) {
            params["version"] = json!(version);
        }
        if let Some(instance) = instance {
            params["instance"] = json!(instance);
        }
        let games = filtered_values(parsed, "filter-version");
        if !games.is_empty() {
            params["game_versions"] = json!(games);
        }
        let loaders = filtered_values(parsed, "filter-loader");
        if !loaders.is_empty() {
            params["loaders"] = json!(loaders);
        }
        match call(catalog, "resource.download", params) {
            Ok(value) => results.push(value),
            Err(code) => return code,
        }
    }
    if parsed.flag("json") {
        return print_json(&Value::Array(results));
    }
    for result in results {
        println!(
            "{}",
            catalog.format(
                "cli.resource.downloaded",
                &[
                    ("file", field(&result, "file")),
                    ("path", field(&result, "path"))
                ]
            )
        );
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn resource_autoremove_action(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let value = match call(
        catalog,
        "resource.autoremove",
        json!({"apply": parsed.flag("yes")}),
    ) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    let files = value
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        println!("{}", catalog.text("cli.resource.autoremove_empty"));
        return ExitCode::SUCCESS;
    }
    for file in &files {
        if let Some(file) = file.as_str() {
            println!("  {file}");
        }
    }
    if value.get("apply").and_then(Value::as_bool).unwrap_or(false) {
        let removed = value.get("removed").and_then(Value::as_u64).unwrap_or(0);
        println!(
            "{}",
            catalog.format(
                "cli.resource.autoremoved",
                &[("count", &removed.to_string())]
            )
        );
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn resource_local_list(parsed: &CommandArgs, catalog: &Catalog) -> ExitCode {
    let kind = parsed.value("type");
    if let Some(kind) = kind {
        if !matches!(
            kind,
            "worlds"
                | "resourcepacks"
                | "datapacks"
                | "shaderpacks"
                | "mods"
                | "plugins"
                | "modpacks"
        ) {
            eprintln!("{}", catalog.format("rpc.resource.kind", &[("kind", kind)]));
            return ExitCode::from(2);
        }
    }
    let sort_by = parsed.value("list-sort-by").unwrap_or("create");
    if !matches!(sort_by, "create" | "launch" | "alphabet") {
        eprintln!(
            "{}",
            catalog.format("cli.error.sort_key", &[("value", sort_by)])
        );
        return ExitCode::from(2);
    }
    let order = match parse_order(parsed, catalog) {
        Ok(order) => order,
        Err(code) => return code,
    };
    let mut params = json!({
        "sort_by": sort_by,
        "order": if order { "asc" } else { "desc" },
    });
    if let Some(kind) = kind {
        params["type"] = json!(kind);
    }
    if let Some(raw) = parsed.value("list-limit") {
        let Ok(limit) = raw.parse::<u64>() else {
            eprintln!(
                "{}",
                catalog.format("cli.resource.limit", &[("value", raw)])
            );
            return ExitCode::from(2);
        };
        if limit == 0 {
            eprintln!(
                "{}",
                catalog.format("cli.resource.limit", &[("value", raw)])
            );
            return ExitCode::from(2);
        }
        params["limit"] = json!(limit);
    }
    let value = match call(catalog, "resource.list", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if resources.is_empty() {
        println!("{}", catalog.text("cli.resource.empty"));
        return ExitCode::SUCCESS;
    }
    for resource in resources {
        let path = resource.get("path").and_then(Value::as_str).unwrap_or("");
        println!(
            "  [{}]  {}  {path}",
            field(&resource, "type"),
            field(&resource, "name")
        );
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn resource_download_action(
    parsed: &CommandArgs,
    kind: &str,
    id: &str,
    catalog: &Catalog,
) -> ExitCode {
    let mut params = json!({"type": kind, "id": id});
    match parsed
        .value("download-to-instance")
        .or_else(|| parsed.value("list-download-to-instance"))
    {
        Some(instance) => params["instance"] = json!(instance),
        None if kind == "modpack" => {}
        None => {
            eprintln!("{}", catalog.text("cli.resource.instance"));
            return ExitCode::from(2);
        }
    }
    let versions = parsed.all("game-version");
    if !versions.is_empty() {
        params["game_versions"] = json!(versions);
    }
    let loaders = parsed.all("loader");
    if !loaders.is_empty() {
        params["loaders"] = json!(loaders);
    }
    let value = match call(catalog, "resource.download", params) {
        Ok(value) => value,
        Err(code) => return code,
    };
    if parsed.flag("json") {
        return print_json(&value);
    }
    println!(
        "{}",
        catalog.format(
            "cli.resource.downloaded",
            &[
                ("file", field(&value, "file")),
                ("path", field(&value, "path"))
            ]
        )
    );
    ExitCode::SUCCESS
}

#[cfg(not(unix))]
fn account_command(_args: &[String], catalog: &Catalog) -> ExitCode {
    eprintln!("{}", catalog.text("cli.error.unsupported"));
    ExitCode::from(1)
}

#[cfg(not(unix))]
fn instance_command(_args: &[String], catalog: &Catalog) -> ExitCode {
    eprintln!("{}", catalog.text("cli.error.unsupported"));
    ExitCode::from(1)
}

#[cfg(not(unix))]
fn launch_command(_args: &[String], catalog: &Catalog) -> ExitCode {
    eprintln!("{}", catalog.text("cli.error.unsupported"));
    ExitCode::from(1)
}

#[cfg(not(unix))]
fn resource_command(_args: &[String], catalog: &Catalog) -> ExitCode {
    eprintln!("{}", catalog.text("cli.error.unsupported"));
    ExitCode::from(1)
}

fn missing_flag(catalog: &Catalog, flag: &str) -> ExitCode {
    eprintln!("{}", catalog.format("cli.error.flag", &[("flag", flag)]));
    ExitCode::from(2)
}

#[cfg(unix)]
fn finish(catalog: &Catalog, result: Result<arkmcl_platform::ipc::Response, ExitCode>) -> ExitCode {
    let response = match result {
        Ok(response) => response,
        Err(code) => return code,
    };
    if let Some(fault) = response.error {
        eprintln!(
            "{}",
            catalog.format("cli.error.rpc", &[("message", &fault.message)])
        );
        return ExitCode::from(1);
    }
    let value = response.result.unwrap_or(Value::Null);
    match serde_json::to_string_pretty(&value) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "{}",
                catalog.format("cli.error.rpc", &[("message", &error.to_string())])
            );
            ExitCode::from(1)
        }
    }
}

#[cfg(unix)]
fn completion_command(args: &[String], catalog: &Catalog) -> ExitCode {
    let Some(shell) = args.first() else {
        eprintln!("{}", catalog.text("cli.completion.usage"));
        return ExitCode::from(2);
    };
    if let Some(extra) = args.get(1) {
        return unexpected(catalog, extra);
    }
    if let Some(script) = completion::script(shell) {
        print!("{script}");
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "{}",
            catalog.format("cli.completion.shell", &[("shell", shell)])
        );
        ExitCode::from(2)
    }
}

#[cfg(not(unix))]
fn completion_command(_args: &[String], catalog: &Catalog) -> ExitCode {
    eprintln!("{}", catalog.text("cli.error.unsupported"));
    ExitCode::from(1)
}

#[cfg(unix)]
fn complete_command(args: &[String]) -> ExitCode {
    let Some(topic) = args.first().map(String::as_str) else {
        return ExitCode::SUCCESS;
    };
    let values = match topic {
        "commands" => static_values(&[
            "account",
            "instance",
            "launch",
            "resource",
            "service",
            "info",
            "version",
            "help",
            "completion",
        ]),
        "resource-types" => static_values(&[
            "worlds",
            "resourcepacks",
            "datapacks",
            "shaderpacks",
            "mods",
            "plugins",
            "modpacks",
        ]),
        "loaders" => static_values(&["fabric", "forge", "neoforge", "purpur"]),
        "vanilla" => static_values(&["all", "stable", "snapshot", "foolsday", "legacy"]),
        "api" => static_values(&["modrinth", "curseforge"]),
        "sort-by" => static_values(&[
            "create",
            "launch",
            "run",
            "alphabet",
            "download",
            "name",
            "relevance",
        ]),
        "lang" => static_values(&["zh-CN", "en-US"]),
        "instance-ids" => complete_list("instance.list", &["id"]),
        "instance-names" => complete_list("instance.list", &["id", "name"]),
        "account-names" => complete_list("account.list", &["id", "name"]),
        "groups" => complete_groups(),
        _ => Vec::new(),
    };
    for value in values {
        println!("{value}");
    }
    ExitCode::SUCCESS
}

#[cfg(not(unix))]
fn complete_command(_args: &[String]) -> ExitCode {
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn static_values(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[cfg(unix)]
fn complete_list(method: &str, fields: &[&str]) -> Vec<String> {
    let Some(value) = complete_call(method) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for item in items {
        for field in fields {
            if let Some(text) = item.get(*field).and_then(Value::as_str) {
                if !text.is_empty() {
                    values.push(text.to_owned());
                }
            }
        }
    }
    values.sort();
    values.dedup();
    values
}

#[cfg(unix)]
fn complete_groups() -> Vec<String> {
    let Some(value) = complete_call("instance.groups") else {
        return Vec::new();
    };
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(unix)]
fn complete_call(method: &str) -> Option<Value> {
    let paths = Paths::discover().ok()?;
    let response = arkmcl_platform::ipc::call(
        &paths.socket(),
        &arkmcl_platform::ipc::Request::new(1, method, json!({})),
    )
    .ok()?;
    if response.error.is_some() {
        return None;
    }
    response.result
}

#[cfg(unix)]
fn account_login_msa(catalog: &Catalog, set_default: bool) -> ExitCode {
    let Ok(config) = msa::Config::from_env() else {
        eprintln!("{}", catalog.text("cli.msa.client"));
        return ExitCode::from(2);
    };
    let Ok(login) = msa::login(&config, |code| {
        eprintln!(
            "{}",
            catalog.format(
                "cli.msa.device",
                &[("uri", &code.verification_uri), ("code", &code.user_code)],
            )
        );
    }) else {
        eprintln!(
            "{}",
            catalog.format("cli.msa.failed", &[("error", "login failed")])
        );
        return ExitCode::from(1);
    };
    submit_account(
        catalog,
        json!({
            "kind": "microsoft",
            "name": login.profile.name,
            "id": login.profile.id,
            "access_token": login.access_token,
            "refresh_token": login.refresh_token,
        }),
        set_default,
    )
}

fn action_conflict(parsed: &CommandArgs, actions: &[&'static str]) -> Option<&'static str> {
    let has = |name: &str| parsed.flag(name) || parsed.value(name).is_some();
    if has("list") && actions.iter().any(|name| *name != "list" && has(name)) {
        return Some("list");
    }
    let mut found = actions.iter().copied().filter(|name| has(name));
    found.next()?;
    found.next()
}

fn reject_flags(parsed: &CommandArgs, catalog: &Catalog, flags: &[&str]) -> Option<ExitCode> {
    for flag in flags {
        if parsed.flag(flag) || parsed.value(flag).is_some() {
            return Some(unexpected(catalog, &format!("--{flag}")));
        }
    }
    None
}

#[allow(clippy::too_many_lines)] // action validation table; split if more flags are added
fn validate_instance_actions(parsed: &CommandArgs, catalog: &Catalog) -> Option<ExitCode> {
    let read_only = [
        "name",
        "type",
        "memory",
        "jvm-args",
        "java",
        "version",
        "lwjgl",
        "loader",
        "loader-version",
        "account",
        "group",
        "link",
        "create-defaults",
        "move",
        "group-move",
        "import",
        "list-sort",
        "list-sort-by",
    ];
    if parsed.flag("info") || parsed.flag("default") || parsed.flag("remove") {
        if let Some(code) = reject_flags(parsed, catalog, &read_only) {
            return Some(code);
        }
    }
    if parsed.flag("list") {
        let list_forbidden = [
            "name",
            "type",
            "memory",
            "jvm-args",
            "java",
            "version",
            "lwjgl",
            "loader",
            "loader-version",
            "account",
            "link",
            "create-defaults",
            "move",
            "group-move",
            "import",
        ];
        if let Some(code) = reject_flags(parsed, catalog, &list_forbidden) {
            return Some(code);
        }
    }
    let move_forbidden = [
        "name",
        "type",
        "memory",
        "jvm-args",
        "java",
        "version",
        "lwjgl",
        "loader",
        "loader-version",
        "account",
        "group",
        "link",
        "create-defaults",
        "import",
        "list-sort",
        "list-sort-by",
    ];
    if parsed.value("move").is_some() || parsed.value("group-move").is_some() {
        if let Some(code) = reject_flags(parsed, catalog, &move_forbidden) {
            return Some(code);
        }
    }
    if parsed.flag("create") {
        if let Some(code) = reject_flags(
            parsed,
            catalog,
            &["import", "move", "group-move", "list-sort", "list-sort-by"],
        ) {
            return Some(code);
        }
    }
    if parsed.value("import").is_some() {
        let import_forbidden = [
            "memory",
            "jvm-args",
            "java",
            "version",
            "lwjgl",
            "loader",
            "loader-version",
            "account",
            "create-defaults",
            "move",
            "group-move",
            "list-sort",
            "list-sort-by",
        ];
        if let Some(code) = reject_flags(parsed, catalog, &import_forbidden) {
            return Some(code);
        }
    }
    if !parsed.flag("list")
        && !parsed.flag("info")
        && !parsed.flag("default")
        && !parsed.flag("remove")
        && !parsed.flag("create")
        && parsed.value("import").is_none()
        && parsed.value("move").is_none()
        && parsed.value("group-move").is_none()
    {
        if let Some(code) = reject_flags(parsed, catalog, &["list-sort", "list-sort-by"]) {
            return Some(code);
        }
    }
    None
}

fn typed_download(parsed: &CommandArgs) -> bool {
    [
        "download-mod",
        "download-modpack",
        "download-resourcepack",
        "download-shader",
        "download-datapack",
        "download-world",
        "download-plugin",
    ]
    .iter()
    .any(|flag| parsed.value(flag).is_some())
}

#[allow(clippy::too_many_lines)] // flag validation table; split if more actions are added
fn validate_resource_actions(parsed: &CommandArgs, catalog: &Catalog) -> Option<ExitCode> {
    let all_values = [
        "filter-vanilla",
        "filter-version",
        "filter-loader",
        "type",
        "api",
        "lookup",
        "download",
        "download-version",
        "download-to-instance",
        "search",
        "list-limit",
        "list-sort",
        "list-sort-by",
        "game-version",
        "loader",
        "list-download-to-instance",
        "download-mod",
        "download-modpack",
        "download-resourcepack",
        "download-shader",
        "download-datapack",
        "download-world",
        "download-plugin",
    ];
    if parsed.flag("autoremove") {
        return reject_flags(parsed, catalog, &all_values);
    }
    if parsed.flag("list-remote") {
        if filtered_values(parsed, "filter-loader").is_empty() {
            return reject_flags(
                parsed,
                catalog,
                &[
                    "filter-loader",
                    "type",
                    "api",
                    "lookup",
                    "download",
                    "download-version",
                    "download-to-instance",
                    "search",
                    "game-version",
                    "loader",
                    "list-download-to-instance",
                    "download-mod",
                    "download-modpack",
                    "download-resourcepack",
                    "download-shader",
                    "download-datapack",
                    "download-world",
                    "download-plugin",
                    "yes",
                ],
            );
        }
        return reject_flags(
            parsed,
            catalog,
            &[
                "filter-vanilla",
                "list-sort-by",
                "type",
                "api",
                "lookup",
                "download",
                "download-version",
                "download-to-instance",
                "search",
                "game-version",
                "loader",
                "list-download-to-instance",
                "download-mod",
                "download-modpack",
                "download-resourcepack",
                "download-shader",
                "download-datapack",
                "download-world",
                "download-plugin",
                "yes",
            ],
        );
    }
    if parsed.value("search").is_some() {
        return reject_flags(
            parsed,
            catalog,
            &[
                "filter-vanilla",
                "lookup",
                "download",
                "download-version",
                "download-to-instance",
                "game-version",
                "loader",
                "list-download-to-instance",
                "download-mod",
                "download-modpack",
                "download-resourcepack",
                "download-shader",
                "download-datapack",
                "download-world",
                "download-plugin",
                "yes",
            ],
        );
    }
    if parsed.value("lookup").is_some() {
        return reject_flags(
            parsed,
            catalog,
            &[
                "filter-vanilla",
                "type",
                "download",
                "download-version",
                "download-to-instance",
                "search",
                "list-limit",
                "list-sort",
                "list-sort-by",
                "game-version",
                "loader",
                "list-download-to-instance",
                "download-mod",
                "download-modpack",
                "download-resourcepack",
                "download-shader",
                "download-datapack",
                "download-world",
                "download-plugin",
                "yes",
            ],
        );
    }
    if !parsed.all("download").is_empty() {
        return reject_flags(
            parsed,
            catalog,
            &[
                "filter-vanilla",
                "lookup",
                "search",
                "list-limit",
                "list-sort",
                "list-sort-by",
                "game-version",
                "loader",
                "list-download-to-instance",
                "download-mod",
                "download-modpack",
                "download-resourcepack",
                "download-shader",
                "download-datapack",
                "download-world",
                "download-plugin",
                "yes",
            ],
        );
    }
    if typed_download(parsed) {
        return reject_flags(
            parsed,
            catalog,
            &[
                "download",
                "download-version",
                "type",
                "api",
                "filter-vanilla",
                "filter-version",
                "filter-loader",
                "list-limit",
                "list-sort",
                "list-sort-by",
                "search",
                "lookup",
                "yes",
            ],
        );
    }
    reject_flags(
        parsed,
        catalog,
        &[
            "filter-vanilla",
            "filter-version",
            "filter-loader",
            "type",
            "api",
            "lookup",
            "download",
            "download-version",
            "download-to-instance",
            "search",
            "game-version",
            "loader",
            "list-download-to-instance",
            "download-mod",
            "download-modpack",
            "download-resourcepack",
            "download-shader",
            "download-datapack",
            "download-world",
            "download-plugin",
            "yes",
        ],
    )
}

fn unknown(catalog: &Catalog, command: &str) -> ExitCode {
    eprintln!(
        "{}",
        catalog.format("cli.error.unknown", &[("command", command)])
    );
    eprintln!("{}", catalog.text("cli.hint"));
    ExitCode::from(2)
}

fn unexpected(catalog: &Catalog, argument: &str) -> ExitCode {
    eprintln!(
        "{}",
        catalog.format("cli.error.unexpected", &[("arg", argument)])
    );
    eprintln!("{}", catalog.text("cli.hint"));
    ExitCode::from(2)
}

fn extract_lang(args: &mut Vec<String>) -> Result<Option<String>, ()> {
    let mut lang = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--lang" {
            if index + 1 >= args.len() {
                return Err(());
            }
            lang = Some(args.remove(index + 1));
            args.remove(index);
        } else if let Some(value) = args[index].strip_prefix("--lang=") {
            lang = Some(value.to_owned());
            args.remove(index);
        } else {
            index += 1;
        }
    }
    Ok(lang)
}

fn load_catalog(locale: Locale) -> Catalog {
    Catalog::load(locale).unwrap_or_else(|error| {
        eprintln!("localization error: {error}");
        std::process::exit(1);
    })
}

#[cfg(test)]
mod tests {
    use super::{field, sort_values, split_args};
    use arkmcl_l10n::{Catalog, Locale};
    use serde_json::json;

    #[test]
    fn splits_jvm_arguments() {
        assert_eq!(
            split_args("-Xmx4G -Dfoo=bar").expect("args"),
            vec!["-Xmx4G", "-Dfoo=bar"]
        );
        assert_eq!(
            split_args(r#"-Dfoo="bar baz" -Xss1m"#).expect("quoted"),
            vec!["-Dfoo=bar baz", "-Xss1m"]
        );
        assert!(split_args("-Dfoo=\"bar").is_err());
    }

    #[test]
    fn sorts_by_statistics() {
        let catalog = Catalog::load(Locale::EnUs).expect("catalog");
        let values = [
            json!({"id": "a", "created_at": 20, "run_seconds": 5}),
            json!({"id": "b", "created_at": 10, "run_seconds": 9}),
        ];
        let mut selected = values.iter().collect();
        sort_values(&mut selected, "create", true, &catalog).expect("create sort");
        assert_eq!(field(selected[0], "id"), "b");
        sort_values(&mut selected, "run", true, &catalog).expect("run sort");
        assert_eq!(field(selected[0], "id"), "a");
        sort_values(&mut selected, "create", false, &catalog).expect("desc sort");
        assert_eq!(field(selected[0], "id"), "a");
    }
}

#[cfg(all(test, unix))]
mod console_key_tests {
    use super::{ConsoleKey, console_key};

    #[test]
    fn detach_keys_match_nano_style_exit() {
        assert_eq!(console_key(0x18), ConsoleKey::Detach);
        assert_eq!(console_key(0x03), ConsoleKey::Detach);
        assert_eq!(console_key(0x04), ConsoleKey::Detach);
    }

    #[test]
    fn editing_and_submit_keys_are_stable() {
        assert_eq!(console_key(b'\r'), ConsoleKey::Submit);
        assert_eq!(console_key(b'\n'), ConsoleKey::Submit);
        assert_eq!(console_key(0x7f), ConsoleKey::Backspace);
        assert_eq!(console_key(0x08), ConsoleKey::Backspace);
        assert_eq!(console_key(0x0c), ConsoleKey::Clear);
        assert_eq!(console_key(b'a'), ConsoleKey::Insert(b'a'));
        assert_eq!(console_key(0xe4), ConsoleKey::Insert(0xe4));
        assert_eq!(console_key(0x01), ConsoleKey::Ignore);
        assert_eq!(console_key(0x1b), ConsoleKey::Ignore);
    }
}
