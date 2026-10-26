#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use arkmcl_l10n::{Catalog, Locale};
use arkmcl_platform::ipc::{self, Request, Response};
use arkmcl_platform::paths::Paths;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

struct Service {
    directory: PathBuf,
    socket: PathBuf,
    handle: JoinHandle<Result<(), arkmcl_service::Error>>,
}

impl Service {
    fn start(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("arkmcl-service-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("temp directory");
        let paths = Paths {
            config: directory.clone(),
            data: directory.clone(),
            minecraft: directory.clone(),
            state: directory.clone(),
            runtime: directory.clone(),
            portable: true,
            runtime_fallback: false,
        };
        let catalog = Arc::new(Catalog::load(Locale::EnUs).expect("catalog"));
        let socket = directory.join("arkmcl.sock");
        let handle = thread::spawn(move || arkmcl_service::run(paths, &catalog));
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(socket.exists(), "socket was not created");
        Self {
            directory,
            socket,
            handle,
        }
    }

    fn call(&self, id: u64, method: &str, params: Value) -> Response {
        ipc::call(&self.socket, &Request::new(id, method, params)).expect("rpc call")
    }

    fn finish(self) {
        let response = self.call(99, "server.stop", json!({}));
        assert_eq!(response.result.expect("stop")["stopping"], true);
        self.handle
            .join()
            .expect("server thread")
            .expect("server run");
        assert!(!self.socket.exists(), "socket was not cleaned up");
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn serves_info_and_instance_crud() {
    let service = Service::start("crud");

    let info = service
        .call(2, "server.info", json!({}))
        .result
        .expect("info");
    assert_eq!(info["name"], "ArkMCL");
    assert_eq!(info["locale"], "en-US");
    assert_eq!(
        info["protocol"].as_u64(),
        Some(u64::from(ipc::PROTOCOL_VERSION))
    );
    let capabilities = info["capabilities"].as_array().expect("capabilities");
    assert!(capabilities.iter().any(|value| value == "instance"));
    assert!(capabilities.iter().any(|value| value == "launch"));

    let status = service
        .call(20, "server.status", json!({}))
        .result
        .expect("status");
    assert_eq!(status["name"], "ArkMCL");

    let created = service
        .call(
            3,
            "instance.create",
            json!({"name": "Test", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["id"], "test");
    assert_eq!(
        created["instance"]["profile"]["components"][0]["uid"],
        "net.minecraft"
    );
    let root = service.directory.join("instances/test");
    assert!(root.join(".arkmcl/instance.cfg").exists());
    assert!(
        fs::symlink_metadata(root.join("saves"))
            .expect("saves link")
            .file_type()
            .is_symlink()
    );

    let got = service
        .call(4, "instance.info", json!({"id": "test"}))
        .result
        .expect("get");
    assert_eq!(got["name"], "Test");

    let updated = service
        .call(
            5,
            "instance.update",
            json!({"id": "test", "name": "Renamed", "memory_mb": 4096, "jvm_args": ["-Xmx2G"]}),
        )
        .result
        .expect("update");
    assert_eq!(updated["name"], "Renamed");
    assert_eq!(updated["config"]["memory_mb"], 4096);
    assert_eq!(updated["config"]["jvm_args"][0], "-Xmx2G");

    let list = service
        .call(6, "instance.list", json!({}))
        .result
        .expect("list");
    let entries = list.as_array().expect("array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "Renamed");

    let deleted = service
        .call(11, "instance.remove", json!({"id": "test"}))
        .result
        .expect("delete");
    assert_eq!(deleted["deleted"], true);
    assert!(!root.exists());
    assert!(
        service.directory.join("saves").is_dir(),
        "global target must survive"
    );

    service.finish();
}

#[test]
fn lock_blocks_updates_and_deletes() {
    let service = Service::start("lock");
    let created = service
        .call(1, "instance.create", json!({"name": "Test"}))
        .result
        .expect("create");
    assert_eq!(created["instance"]["id"], "test");
    let root = service.directory.join("instances/test");
    let lock_path = root.join(".arkmcl/lock");
    fs::write(
        &lock_path,
        json!({"pid": std::process::id(), "token": "test"}).to_string(),
    )
    .expect("lock");

    let response = service.call(
        2,
        "instance.update",
        json!({"id": "test", "memory_mb": 8192}),
    );
    assert!(response.error.is_some(), "update must be blocked by lock");

    let response = service.call(3, "instance.remove", json!({"id": "test"}));
    assert!(response.error.is_some(), "delete must be blocked by lock");
    assert!(root.exists());

    fs::remove_file(&lock_path).expect("unlock");
    let deleted = service
        .call(4, "instance.remove", json!({"id": "test"}))
        .result
        .expect("delete");
    assert_eq!(deleted["deleted"], true);
    assert!(!root.exists());
    assert!(service.directory.join("saves").is_dir());

    service.finish();
}

#[test]
fn explicit_id_sets_default_name() {
    let service = Service::start("create-id-name");
    let created = service
        .call(
            1,
            "instance.create",
            json!({"id": "my-id", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["id"], "my-id");
    assert_eq!(created["instance"]["name"], "my-id");
    service.finish();
}

#[test]
fn missing_id_keeps_template_name() {
    let service = Service::start("create-template-name");
    let created = service
        .call(1, "instance.create", json!({"minecraft": "1.20.4"}))
        .result
        .expect("create");
    assert_eq!(created["instance"]["id"], "vanilla-1-20-4");
    assert_eq!(created["instance"]["name"], "vanilla-1.20.4");
    service.finish();
}

#[test]
fn allows_dots_in_instance_id() {
    let service = Service::start("create-dotted-id");
    let created = service
        .call(
            1,
            "instance.create",
            json!({"id": "test-1.17.1", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["id"], "test-1.17.1");
    assert_eq!(created["instance"]["name"], "test-1.17.1");
    service.finish();
}

#[test]
fn creates_instance_with_explicit_id_and_fabric() {
    let service = Service::start("create-fabric");
    write_fabric_profile(&service.directory);
    let created = service
        .call(
            1,
            "instance.create",
            json!({
                "id": "custom",
                "name": "Custom",
                "minecraft": "1.20.4",
                "loader": "fabric",
                "loader_version": "0.15.11"
            }),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["id"], "custom");
    let components = created["instance"]["profile"]["components"]
        .as_array()
        .expect("components");
    assert!(components.iter().any(|component| {
        component["uid"] == "net.fabricmc.fabric-loader" && component["version"] == "0.15.11"
    }));
    service.finish();
}

fn write_fabric_profile(directory: &std::path::Path) {
    let loaders = directory.join("assets/loaders/fabric");
    fs::create_dir_all(&loaders).expect("loaders");
    fs::write(
        loaders.join("1.20.4-0.15.11.json"),
        json!({
            "id": "fabric-loader-0.15.11-1.20.4",
            "inheritsFrom": "1.20.4",
            "type": "release",
            "mainClass": "net.fabricmc.loader.impl.launch.knot.KnotClient",
            "libraries": []
        })
        .to_string(),
    )
    .expect("fabric profile");
}

#[test]
fn prepares_launch_from_cached_metadata() {
    let service = Service::start("prepare");
    let created = service
        .call(
            1,
            "instance.create",
            json!({"id": "prepare", "name": "Prepare", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["id"], "prepare");

    let jar = b"jar";
    let hash = format!("{:x}", Sha1::digest(jar));
    let version = json!({
        "id": "1.20.4",
        "type": "release",
        "mainClass": "net.minecraft.client.main.Main",
        "assetIndex": {"id": "12", "sha1": "", "size": 0, "totalSize": 0, "url": "https://example.com/index.json"},
        "downloads": {"client": {"url": "https://example.com/client.jar", "sha1": hash, "size": 3}},
        "libraries": []
    });
    let versions = service.directory.join("versions/1.20.4");
    fs::create_dir_all(&versions).expect("versions");
    fs::write(versions.join("1.20.4.json"), version.to_string()).expect("version json");
    fs::write(versions.join("1.20.4.jar"), jar).expect("client jar");
    let indexes = service.directory.join("assets/indexes");
    fs::create_dir_all(&indexes).expect("indexes");
    fs::write(indexes.join("12.json"), json!({"objects": {}}).to_string()).expect("index");

    let response = service.call(2, "launch.prepare", json!({"id": "prepare"}));
    assert!(
        response.error.is_none(),
        "prepare error: {:?}",
        response.error
    );
    let result = response.result.expect("result");
    assert_eq!(
        result["plan"]["main_class"],
        "net.minecraft.client.main.Main"
    );
    assert_eq!(result["downloads"]["skipped"], 1);
    assert_eq!(result["natives"]["files"], 0);
    service.finish();
}

#[test]
fn starts_and_stops_process() {
    let service = Service::start("process");
    service
        .call(
            1,
            "instance.create",
            json!({"id": "proc", "name": "Proc", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    write_launch_cache(&service.directory);

    let jdk = service.directory.join("fake-jdk");
    let script = jdk.join("bin").join("java");
    fs::create_dir_all(script.parent().expect("bin")).expect("bin");
    fs::write(&script, "#!/bin/sh\necho ready\nsleep 5\n").expect("script");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("permissions");
    service
        .call(
            2,
            "instance.update",
            json!({"id": "proc", "java_path": jdk.to_string_lossy()}),
        )
        .result
        .expect("update java path");

    let started = service.call(3, "launch.start", json!({"id": "proc"}));
    assert!(started.error.is_none(), "start error: {:?}", started.error);
    let pid = started.result.expect("start")["pid"].as_u64().expect("pid");
    assert!(pid > 0);

    let terminal = service
        .call(11, "launch.terminal", json!({"id": "proc"}))
        .result
        .expect("terminal");
    assert_eq!(terminal["id"], "proc");

    let mut output = String::new();
    for _ in 0..20 {
        let value = service
            .call(10, "launch.output", json!({"id": "proc", "cursor": 0}))
            .result
            .expect("output");
        output = value["output"].as_str().unwrap_or("").to_owned();
        if output.contains("ready") {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(output.contains("ready"), "output: {output}");

    let status = service
        .call(4, "launch.list", json!({}))
        .result
        .expect("status");
    assert_eq!(status.as_array().expect("array").len(), 1);

    let blocked = service.call(5, "instance.remove", json!({"id": "proc"}));
    assert!(
        blocked.error.is_some(),
        "delete must be blocked while running"
    );

    let stopped = service.call(6, "launch.stop", json!({"id": "proc"}));
    assert!(stopped.error.is_none(), "stop error: {:?}", stopped.error);

    for _ in 0..20 {
        let status = service
            .call(7, "launch.list", json!({}))
            .result
            .expect("status");
        if status.as_array().expect("array").is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let status = service
        .call(8, "launch.list", json!({}))
        .result
        .expect("status");
    assert_eq!(
        status.as_array().expect("array").len(),
        0,
        "expected no running sessions"
    );

    let terminal = service.call(12, "launch.terminal", json!({"id": "proc"}));
    assert!(
        terminal.error.is_some(),
        "terminal requires a running instance"
    );

    let instance = service
        .call(9, "instance.info", json!({"id": "proc"}))
        .result
        .expect("get");
    assert_eq!(instance["state"], "ready");
    assert!(
        instance["created_at"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert!(instance["last_launched_at"].as_u64().is_some());
    assert!(instance["run_seconds"].as_u64().is_some());
    assert_eq!(instance["last_crashed"], false);
    service.finish();
}

#[test]
fn records_exit_code_and_crash() {
    let service = Service::start("crash");
    service
        .call(
            1,
            "instance.create",
            json!({"id": "crash", "name": "Crash", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    write_launch_cache(&service.directory);

    let jdk = service.directory.join("fake-jdk-crash");
    let script = jdk.join("bin").join("java");
    fs::create_dir_all(script.parent().expect("bin")).expect("bin");
    fs::write(&script, "#!/bin/sh\nexit 3\n").expect("script");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("permissions");
    service
        .call(
            2,
            "instance.update",
            json!({"id": "crash", "java_path": jdk.to_string_lossy()}),
        )
        .result
        .expect("update java path");

    let started = service.call(3, "launch.start", json!({"id": "crash"}));
    assert!(started.error.is_none(), "start error: {:?}", started.error);
    for _ in 0..30 {
        let status = service
            .call(4, "launch.list", json!({}))
            .result
            .expect("status");
        if status.as_array().expect("array").is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let instance = service
        .call(5, "instance.info", json!({"id": "crash"}))
        .result
        .expect("get");
    assert_eq!(instance["last_exit_code"], 3);
    assert_eq!(instance["last_crashed"], true);
    service.finish();
}

fn write_launch_cache(directory: &std::path::Path) -> PathBuf {
    let jar = b"jar";
    let hash = format!("{:x}", Sha1::digest(jar));
    let version = json!({
        "id": "1.20.4",
        "type": "release",
        "mainClass": "net.minecraft.client.main.Main",
        "assetIndex": {"id": "12", "sha1": "", "size": 0, "totalSize": 0, "url": "https://example.com/index.json"},
        "downloads": {"client": {"url": "https://example.com/client.jar", "sha1": hash, "size": 3}},
        "libraries": []
    });
    let versions = directory.join("versions/1.20.4");
    fs::create_dir_all(&versions).expect("versions");
    fs::write(versions.join("1.20.4.json"), version.to_string()).expect("version json");
    let client = versions.join("1.20.4.jar");
    fs::write(&client, jar).expect("client jar");
    let indexes = directory.join("assets/indexes");
    fs::create_dir_all(&indexes).expect("indexes");
    fs::write(indexes.join("12.json"), json!({"objects": {}}).to_string()).expect("index");
    client
}

#[test]
fn updates_instance_lwjgl() {
    let service = Service::start("lwjgl");
    service
        .call(
            1,
            "instance.create",
            json!({"name": "Test", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");

    let updated = service
        .call(
            2,
            "instance.update",
            json!({"id": "test", "lwjgl": "3.3.3"}),
        )
        .result
        .expect("update");
    assert_eq!(updated["config"]["lwjgl"], "3.3.3");
    let cfg = fs::read_to_string(
        service
            .directory
            .join("instances/test/.arkmcl/instance.cfg"),
    )
    .expect("cfg");
    assert!(cfg.contains("lwjgl=3.3.3"));

    let cleared = service
        .call(
            3,
            "instance.update",
            json!({"id": "test", "lwjgl": "default"}),
        )
        .result
        .expect("clear");
    assert!(cleared["config"]["lwjgl"].is_null());

    service.finish();
}

#[test]
fn creates_purpur_server_instance() {
    let service = Service::start("create-purpur");
    let created = service
        .call(
            1,
            "instance.create",
            json!({
                "name": "Purpur",
                "minecraft": "1.21.1",
                "type": "server",
                "loader": "purpur",
                "loader_version": "2324"
            }),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["kind"], "server");
    let pack = fs::read_to_string(service.directory.join("instances/purpur/.arkmcl/pack.json"))
        .expect("pack");
    assert!(pack.contains("org.purpurmc.purpur"));
    assert!(pack.contains("2324"));
    service.finish();
}

#[test]
fn updates_instance_defaults() {
    let service = Service::start("settings");
    let defaults = service
        .call(1, "settings.get", json!({}))
        .result
        .expect("get");
    assert_eq!(defaults["memory_mb"], 4096);
    assert_eq!(defaults["jvm_args"].as_array().expect("jvm args").len(), 20);

    service
        .call(
            2,
            "settings.update",
            json!({"memory_mb": 3072, "jvm_args": [], "group": "custom", "version": "1.20.4", "linking": "none"}),
        )
        .result
        .expect("update");

    let created = service
        .call(3, "instance.create", json!({"id": "custom"}))
        .result
        .expect("create");
    assert_eq!(created["instance"]["config"]["memory_mb"], 3072);
    assert_eq!(created["instance"]["group"], "custom");
    assert_eq!(created["instance"]["name"], "custom");
    assert_eq!(
        created["instance"]["config"]["jvm_args"]
            .as_array()
            .expect("jvm args")
            .len(),
        0
    );

    let links = service
        .call(4, "instance.links", json!({"id": "custom"}))
        .result
        .expect("links");
    assert!(
        links["links"]
            .as_array()
            .expect("array")
            .iter()
            .all(|link| link["status"] == "missing")
    );
    service.finish();
}

#[test]
fn creates_instance_with_default_values() {
    let service = Service::start("defaults");
    let created = service
        .call(
            1,
            "instance.create",
            json!({"id": "custom", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["name"], "custom");
    assert_eq!(created["instance"]["group"], "vanilla");
    assert_eq!(created["instance"]["config"]["memory_mb"], 4096);
    assert_eq!(
        created["instance"]["config"]["jvm_args"]
            .as_array()
            .expect("jvm args")
            .len(),
        20
    );
    service.finish();
}

#[test]
fn updates_java_selection() {
    let service = Service::start("instance-java");
    let created = service
        .call(
            1,
            "instance.create",
            json!({"id": "java", "minecraft": "1.20.4", "java": "21"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["config"]["java_version"], 21);
    assert!(created["instance"]["config"]["java_path"].is_null());

    let updated = service
        .call(
            2,
            "instance.update",
            json!({"id": "java", "java": "/opt/jdk-21"}),
        )
        .result
        .expect("update");
    assert_eq!(updated["config"]["java_path"], "/opt/jdk-21");
    assert!(updated["config"]["java_version"].is_null());

    let cleared = service
        .call(
            3,
            "instance.update",
            json!({"id": "java", "java": "default"}),
        )
        .result
        .expect("update");
    assert!(cleared["config"]["java_path"].is_null());
    assert!(cleared["config"]["java_version"].is_null());

    let invalid = service.call(4, "instance.update", json!({"id": "java", "java": "0"}));
    assert_eq!(invalid.error.expect("invalid java").code, -32602);
    service.finish();
}

#[test]
fn queries_instance_java() {
    let service = Service::start("instance-java-query");
    service
        .call(
            1,
            "instance.create",
            json!({"id": "java", "minecraft": "1.20.4", "java": "/opt/jdk-21"}),
        )
        .result
        .expect("create");
    let value = service
        .call(2, "instance.java", json!({"id": "java"}))
        .result
        .expect("java");
    assert_eq!(value["source"], "path");
    assert_eq!(value["java"], "/opt/jdk-21/bin/java");
    assert!(value["version"].is_null());
    service.finish();
}

#[test]
fn global_java_default_applies_to_create() {
    let service = Service::start("global-java");
    let settings = service
        .call(1, "settings.update", json!({"java": "21"}))
        .result
        .expect("settings");
    assert_eq!(settings["java"], "21");

    let created = service
        .call(
            2,
            "instance.create",
            json!({"id": "j", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["config"]["java_version"], 21);

    let explicit = service
        .call(
            3,
            "instance.create",
            json!({"id": "j2", "minecraft": "1.20.4", "java": "default"}),
        )
        .result
        .expect("create");
    assert!(explicit["instance"]["config"]["java_version"].is_null());

    service
        .call(4, "settings.update", json!({"java": "default"}))
        .result
        .expect("reset");
    let reset = service
        .call(
            5,
            "instance.create",
            json!({"id": "j3", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    assert!(reset["instance"]["config"]["java_version"].is_null());

    service
        .call(6, "settings.update", json!({"java": "/opt/jdk-21"}))
        .result
        .expect("path");
    let path = service
        .call(
            7,
            "instance.create",
            json!({"id": "j4", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    assert_eq!(path["instance"]["config"]["java_path"], "/opt/jdk-21");
    service.finish();
}

#[test]
fn global_instance_defaults_apply_to_create() {
    let service = Service::start("global-instance-defaults");
    fs::write(service.directory.join("server.properties"), "motd=test\n").expect("defaults");
    let defaults = service
        .call(
            1,
            "settings.update",
            json!({"type": "server", "create_defaults": false}),
        )
        .result
        .expect("settings");
    assert_eq!(defaults["type"], "server");
    assert_eq!(defaults["create_defaults"], false);
    assert!(defaults["loader"].is_null());

    let server = service
        .call(
            2,
            "instance.create",
            json!({"id": "srv", "minecraft": "1.21.1"}),
        )
        .result
        .expect("create server");
    assert_eq!(server["instance"]["kind"], "server");
    assert!(
        !service
            .directory
            .join("instances/srv/server.properties")
            .exists()
    );

    let rejected = service.call(
        3,
        "settings.update",
        json!({"type": "client", "loader": "purpur"}),
    );
    assert_eq!(rejected.error.expect("purpur is server only").code, -32602);

    service
        .call(
            4,
            "settings.update",
            json!({"type": "client", "loader": "fabric"}),
        )
        .result
        .expect("fabric client");
    service
        .call(
            5,
            "settings.update",
            json!({"loader": "none", "create_defaults": true}),
        )
        .result
        .expect("reset loader");

    let client = service
        .call(
            6,
            "instance.create",
            json!({"id": "cli", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create client");
    assert_eq!(client["instance"]["kind"], "client");

    let explicit = service
        .call(
            7,
            "instance.create",
            json!({"id": "srv2", "minecraft": "1.21.1", "type": "server"}),
        )
        .result
        .expect("explicit type");
    assert_eq!(explicit["instance"]["kind"], "server");
    assert!(
        service
            .directory
            .join("instances/srv2/server.properties")
            .exists()
    );
    service.finish();
}

#[test]
fn moves_instance_id() {
    let service = Service::start("move");
    service
        .call(
            1,
            "instance.create",
            json!({"name": "Move", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    service
        .call(2, "instance.default", json!({"id": "move"}))
        .result
        .expect("default");
    let moved = service
        .call(3, "instance.move", json!({"id": "move", "to": "moved"}))
        .result
        .expect("move");
    assert_eq!(moved["id"], "moved");
    let info = service
        .call(4, "instance.info", json!({}))
        .result
        .expect("default info");
    assert_eq!(info["id"], "moved");
    service.finish();
}

#[test]
fn enforces_memory_minimum_by_kind() {
    let service = Service::start("memory");
    let server = service
        .call(
            1,
            "instance.create",
            json!({"name": "Srv", "minecraft": "1.21.1", "type": "server", "memory_mb": 256}),
        )
        .result
        .expect("server");
    assert_eq!(server["instance"]["config"]["memory_mb"], 256);

    let client = service.call(
        2,
        "instance.create",
        json!({"name": "Cli", "minecraft": "1.21.1", "memory_mb": 256}),
    );
    assert!(client.error.is_some());
    service.finish();
}

#[test]
fn creates_instance_without_links() {
    let service = Service::start("nolinks");
    service
        .call(
            1,
            "instance.create",
            json!({"name": "NoLinks", "minecraft": "1.20.4", "linking": "none"}),
        )
        .result
        .expect("create");
    let value = service
        .call(2, "instance.links", json!({"id": "nolinks"}))
        .result
        .expect("links");
    assert!(
        value["links"]
            .as_array()
            .expect("array")
            .iter()
            .all(|link| link["status"] == "missing")
    );
    service.finish();
}

#[test]
fn reports_instance_links() {
    let service = Service::start("links");
    service
        .call(
            1,
            "instance.create",
            json!({"name": "Links", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");
    let value = service
        .call(2, "instance.links", json!({"id": "links"}))
        .result
        .expect("links");
    let links = value["links"].as_array().expect("array");
    assert!(
        links
            .iter()
            .any(|link| { link["name"] == "saves" && link["status"] == "linked" })
    );
    assert!(
        links
            .iter()
            .any(|link| { link["name"] == "shaderpacks" && link["status"] == "linked" })
    );
    assert!(links.iter().any(|link| link["name"] == "usercache"));
    service.finish();
}

#[test]
fn creates_server_instance_with_server_links() {
    let service = Service::start("create-server");
    let created = service
        .call(
            1,
            "instance.create",
            json!({"name": "Server", "minecraft": "1.21.1", "type": "server"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["kind"], "server");

    let root = service.directory.join("instances/server");
    assert!(root.join("cache").exists());
    assert!(root.join("eula.txt").exists());
    assert!(!root.join("saves").exists());
    let cfg = fs::read_to_string(root.join(".arkmcl/instance.cfg")).expect("cfg");
    assert!(cfg.contains("type=server"));

    service.finish();
}

#[test]
fn moves_instances_between_groups() {
    let service = Service::start("group-move");
    for (index, (id, name, group)) in [
        ("one", "One", "alpha"),
        ("two", "Two", "alpha"),
        ("three", "Three", "beta"),
    ]
    .into_iter()
    .enumerate()
    {
        let response = service.call(
            index as u64 + 1,
            "instance.create",
            json!({"id": id, "name": name, "group": group}),
        );
        assert!(
            response.error.is_none(),
            "create error: {:?}",
            response.error
        );
    }

    let moved = service
        .call(
            10,
            "instance.group_move",
            json!({"from": "alpha", "to": "gamma"}),
        )
        .result
        .expect("group move");
    assert_eq!(moved["from"], "alpha");
    assert_eq!(moved["to"], "gamma");
    let mut ids: Vec<&str> = moved["moved"]
        .as_array()
        .expect("moved")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["one", "two"]);

    let groups = service
        .call(11, "instance.groups", json!({}))
        .result
        .expect("groups");
    let groups: Vec<&str> = groups
        .as_array()
        .expect("array")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(groups.contains(&"gamma"));
    assert!(groups.contains(&"beta"));
    assert!(!groups.contains(&"alpha"));

    let list = service
        .call(12, "instance.list", json!({}))
        .result
        .expect("list");
    for instance in list.as_array().expect("array") {
        if instance["id"] == "three" {
            assert_eq!(instance["group"], "beta");
        } else {
            assert_eq!(instance["group"], "gamma");
        }
    }

    service.finish();
}

#[test]
fn manages_accounts() {
    let service = Service::start("accounts");
    let offline = service
        .call(
            1,
            "account.add",
            json!({"kind": "offline", "name": "Player"}),
        )
        .result
        .expect("offline");
    assert_eq!(offline["kind"], "offline");
    assert!(
        offline["created_at"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert!(
        offline["id"]
            .as_str()
            .is_some_and(|uuid| uuid.contains('-'))
    );

    let external = service
        .call(
            2,
            "account.add",
            json!({"kind": "external", "name": "Ext", "server": "https://example.com/auth"}),
        )
        .result
        .expect("external");
    assert_eq!(external["kind"], "external");
    assert_eq!(external["server"], "https://example.com/auth");

    let microsoft = service
        .call(
            3,
            "account.add",
            json!({"kind": "microsoft", "name": "Ms", "id": "00000000-0000-0000-0000-000000000001", "access_token": "token", "refresh_token": "refresh"}),
        )
        .result
        .expect("microsoft");
    assert_eq!(microsoft["kind"], "microsoft");
    assert!(service.directory.join("tokens.json").exists());

    let list = service
        .call(4, "account.list", json!({}))
        .result
        .expect("list");
    assert_eq!(list.as_array().expect("array").len(), 3);

    let id = offline["id"].as_str().expect("uuid").to_owned();
    service
        .call(5, "account.remove", json!({"id": id}))
        .result
        .expect("remove");
    let list = service
        .call(6, "account.list", json!({}))
        .result
        .expect("list");
    assert_eq!(list.as_array().expect("array").len(), 2);

    let unknown = service.call(7, "account.add", json!({"kind": "unknown", "name": "Bad"}));
    assert_eq!(unknown.error.expect("unknown kind").code, -32602);

    let missing = service.call(
        8,
        "account.add",
        json!({"kind": "external", "name": "NoServer"}),
    );
    assert_eq!(missing.error.expect("missing server").code, -32602);
    service.finish();
}

#[test]
fn queries_account_info() {
    let service = Service::start("account-info");
    let added = service
        .call(
            1,
            "account.add",
            json!({"kind": "offline", "name": "Player"}),
        )
        .result
        .expect("add");
    let uuid = added["id"].as_str().expect("uuid").to_owned();

    let info = service
        .call(2, "account.info", json!({"id": uuid}))
        .result
        .expect("info");
    assert_eq!(info["name"], "Player");
    assert_eq!(info["kind"], "offline");

    let missing = service.call(
        3,
        "account.info",
        json!({"id": "00000000-0000-0000-0000-0000000000ff"}),
    );
    assert_eq!(missing.error.expect("missing").code, -32602);

    let invalid = service.call(4, "account.info", json!({}));
    assert_eq!(invalid.error.expect("id required").code, -32602);
    service.finish();
}

#[test]
fn queries_and_clears_default_account() {
    let service = Service::start("account-default-query");
    let added = service
        .call(
            1,
            "account.add",
            json!({"kind": "offline", "name": "Player"}),
        )
        .result
        .expect("add");
    let uuid = added["id"].as_str().expect("uuid").to_owned();

    let response = service.call(2, "account.default", json!({}));
    assert!(response.error.is_none(), "error: {:?}", response.error);
    let none = response.result.unwrap_or(Value::Null);
    assert!(none.is_null());

    service
        .call(3, "account.default", json!({"value": uuid}))
        .result
        .expect("set");
    let found = service
        .call(4, "account.default", json!({}))
        .result
        .expect("query");
    assert_eq!(found["id"], uuid);

    service
        .call(5, "account.remove", json!({"id": uuid}))
        .result
        .expect("remove");
    let cleared = service
        .call(6, "account.default", json!({}))
        .result
        .unwrap_or(Value::Null);
    assert!(cleared.is_null());
    service.finish();
}

#[test]
fn resolves_instance_account_by_name() {
    let service = Service::start("account-name");
    let offline = service
        .call(
            1,
            "account.add",
            json!({"kind": "offline", "name": "Player"}),
        )
        .result
        .expect("offline");
    let id = offline["id"].as_str().expect("uuid").to_owned();

    let created = service
        .call(
            2,
            "instance.create",
            json!({"id": "named", "name": "Named", "account": "Player"}),
        )
        .result
        .expect("create");
    assert_eq!(created["instance"]["account"], id);

    let response = service.call(
        3,
        "instance.update",
        json!({"id": "named", "account": "Missing"}),
    );
    let error = response.error.expect("unknown account must fail");
    assert_eq!(error.code, -32602);

    let cleared = service
        .call(
            4,
            "instance.update",
            json!({"id": "named", "account": null}),
        )
        .result
        .expect("clear");
    assert!(cleared["account"].is_null());
    service.finish();
}

#[test]
fn updates_version_and_loader_after_create() {
    let service = Service::start("update-profile");
    write_fabric_profile(&service.directory);
    service
        .call(
            1,
            "instance.create",
            json!({"id": "test", "name": "Test", "minecraft": "1.20.4"}),
        )
        .result
        .expect("create");

    let updated = service
        .call(
            2,
            "instance.update",
            json!({"id": "test", "loader": "fabric", "loader_version": "0.15.11"}),
        )
        .result
        .expect("install loader");
    assert!(
        updated["profile"]["components"]
            .as_array()
            .expect("components")
            .iter()
            .any(|component| component["uid"] == "net.fabricmc.fabric-loader"
                && component["version"] == "0.15.11")
    );

    let removed = service
        .call(
            3,
            "instance.update",
            json!({"id": "test", "loader": "none"}),
        )
        .result
        .expect("remove loader");
    assert!(
        !removed["profile"]["components"]
            .as_array()
            .expect("components")
            .iter()
            .any(|component| component["uid"] == "net.fabricmc.fabric-loader")
    );

    let error = service.call(
        4,
        "instance.update",
        json!({"id": "test", "loader_version": "0.15.11"}),
    );
    assert_eq!(error.error.expect("missing loader").code, -32602);

    let version = service
        .call(
            5,
            "instance.update",
            json!({"id": "test", "minecraft": "1.21"}),
        )
        .result
        .expect("change version");
    assert_eq!(version["profile"]["components"][0]["version"], "1.21");
    assert_eq!(version["state"], "notinstalled");

    service.finish();
}
