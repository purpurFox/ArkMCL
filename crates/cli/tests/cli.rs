use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_arkmcl"))
        .args(args)
        .env_remove("ARKMCL_LANG")
        .env("ARKMCL_NO_AUTOSTART", "1")
        .output()
        .expect("run arkmcl")
}

#[test]
fn version_is_localized_stably() {
    let output = run(&["--lang", "en-US", "version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "ArkMCL 0.1.0"
    );
}

#[test]
fn help_supports_both_languages() {
    let english = run(&["--lang", "en-US", "help"]);
    assert!(english.status.success());
    assert!(String::from_utf8_lossy(&english.stdout).contains("Usage:"));

    let chinese = run(&["--lang", "zh-CN", "help"]);
    assert!(chinese.status.success());
    assert!(String::from_utf8_lossy(&chinese.stdout).contains("用法"));
}

#[test]
fn help_lists_new_layout() {
    let output = run(&["--lang", "en-US", "help"]);
    let text = String::from_utf8_lossy(&output.stdout);
    for token in [
        "account",
        "instance",
        "launch",
        "resource",
        "--list-sort-by",
        "--json",
    ] {
        assert!(text.contains(token), "help must mention {token}");
    }
}

#[test]
fn unknown_command_exits_with_usage_error() {
    let output = run(&["--lang", "en-US", "nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown command"));
}

#[test]
fn info_prints_json() {
    let output = run(&["--lang", "en-US", "info"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    assert_eq!(value["name"], "ArkMCL");
    assert_eq!(value["locale"], "en-US");
    assert!(
        value["socket"]
            .as_str()
            .is_some_and(|path| path.ends_with("arkmcl.sock"))
    );
}

#[test]
fn account_add_requires_name_with_server() {
    let output = run(&[
        "--lang",
        "en-US",
        "account",
        "--add",
        "--server",
        "https://example.com/auth",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("<name>"));
}

#[test]
fn create_requires_name_without_id() {
    let output = run(&[
        "--lang",
        "en-US",
        "instance",
        "--create",
        "--version",
        "1.20.4",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--name"));
}

#[test]
fn read_only_actions_reject_setters() {
    let cases: [(&[&str], &str); 5] = [
        (
            &["--lang", "en-US", "instance", "--list", "--name", "foo"],
            "--name",
        ),
        (
            &[
                "--lang", "en-US", "instance", "demo", "--info", "--memory", "1024",
            ],
            "--memory",
        ),
        (
            &[
                "--lang",
                "en-US",
                "launch",
                "one",
                "--stop",
                "--account",
                "x",
            ],
            "--account",
        ),
        (
            &[
                "--lang",
                "en-US",
                "account",
                "player",
                "--add",
                "--list-sort-by",
                "name",
            ],
            "--list-sort-by",
        ),
        (
            &[
                "--lang",
                "en-US",
                "launch",
                "one",
                "--prepare",
                "--list-sort",
                "desc",
            ],
            "--list-sort",
        ),
    ];
    for (args, needle) in cases {
        let output = run(args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(needle),
            "args: {args:?}"
        );
    }
}

#[test]
fn instance_type_and_create_defaults_stay_global() {
    for args in [
        &["--lang", "en-US", "instance", "demo", "--type", "server"][..],
        &[
            "--lang",
            "en-US",
            "instance",
            "demo",
            "--create-defaults",
            "false",
        ][..],
        &["--lang", "en-US", "instance", "demo", "--create-defaults"][..],
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(
            !String::from_utf8_lossy(&output.stderr).is_empty(),
            "args: {args:?}"
        );
    }
}

#[test]
fn resource_actions_reject_irrelevant_flags() {
    let cases: [(&[&str], &str); 4] = [
        (
            &["--lang", "en-US", "resource", "--list", "--api", "modrinth"],
            "--api",
        ),
        (
            &[
                "--lang",
                "en-US",
                "resource",
                "--autoremove",
                "--list-limit",
                "5",
            ],
            "--list-limit",
        ),
        (
            &[
                "--lang",
                "en-US",
                "resource",
                "--search",
                "sodium",
                "--type",
                "mods",
                "--download-to-instance",
                "x",
            ],
            "--download-to-instance",
        ),
        (
            &[
                "--lang",
                "en-US",
                "resource",
                "--lookup",
                "sodium",
                "--list-sort",
                "desc",
            ],
            "--list-sort",
        ),
    ];
    for (args, needle) in cases {
        let output = run(args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(needle),
            "args: {args:?}"
        );
    }
}

#[test]
fn launch_account_was_removed() {
    let output = run(&["--lang", "en-US", "launch", "--account", "x"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--account"));
}

#[test]
fn account_add_rejects_list() {
    let output = run(&["--lang", "en-US", "account", "player", "--add", "--list"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--list"));
}

#[test]
fn action_flags_must_not_conflict() {
    let cases: [(&[&str], &str); 4] = [
        (
            &["--lang", "en-US", "launch", "one", "--start", "--stop"],
            "--start",
        ),
        (
            &["--lang", "en-US", "account", "player", "--info", "--add"],
            "--info",
        ),
        (
            &["--lang", "en-US", "instance", "--list", "--remove", "foo"],
            "--list",
        ),
        (
            &[
                "--lang",
                "en-US",
                "resource",
                "--list-remote",
                "--autoremove",
            ],
            "--list-remote",
        ),
    ];
    for (args, needle) in cases {
        let output = run(args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(needle),
            "args: {args:?}"
        );
    }
}

#[test]
fn prepare_rejects_invalid_threads() {
    for value in ["0", "65", "many"] {
        let output = run(&[
            "--lang",
            "en-US",
            "launch",
            "--prepare",
            "--prepare-download-threads",
            value,
        ]);
        assert_eq!(output.status.code(), Some(2), "value: {value}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(value),
            "value: {value}"
        );
    }
}

#[test]
fn launch_start_rejects_batch() {
    let batch = run(&["--lang", "en-US", "launch", "--start", "one", "two"]);
    assert_eq!(batch.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&batch.stderr).contains("--start"));
}

#[test]
fn remove_requires_selector() {
    let output = run(&["--lang", "en-US", "account", "--remove"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("<id|name>"));
}

#[test]
fn group_move_requires_target() {
    let output = run(&["--lang", "en-US", "instance", "--group-move", "alpha"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("<to>"));
}

#[test]
fn resource_validates_arguments_before_connecting() {
    let limit = run(&["--lang", "en-US", "resource", "--list-limit", "0"]);
    assert_eq!(limit.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&limit.stderr).contains("limit"));

    let missing = run(&["--lang", "en-US", "resource", "--download-mod", "sodium"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--list-download-to-instance"));
}

#[test]
fn account_default_rejects_batch() {
    let output = run(&["--lang", "en-US", "account", "--default", "one", "two"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--default"));
}

#[test]
fn invalid_memory_is_rejected() {
    let output = run(&["--lang", "en-US", "instance", "demo", "--memory", "abc"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid memory"));
}

#[test]
fn resource_remote_validates_sort_key() {
    let output = run(&[
        "--lang",
        "en-US",
        "resource",
        "--list-remote",
        "--list-sort-by",
        "nope",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("sort key"));
}

#[test]
fn resource_remote_loader_requires_value() {
    let output = run(&[
        "--lang",
        "en-US",
        "resource",
        "--list-remote",
        "--filter-loader",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--filter-loader"));
}

#[test]
fn resource_search_requires_type() {
    let output = run(&["--lang", "en-US", "resource", "--search", "sodium"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--type"));
}

#[test]
fn resource_search_rejects_unknown_type() {
    let output = run(&[
        "--lang", "en-US", "resource", "--search", "sodium", "--type", "nope",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("nope"));
}

#[test]
fn completion_scripts_are_available() {
    for (shell, needle) in [
        ("bash", "complete -F _arkmcl_complete arkmcl"),
        ("zsh", "#compdef arkmcl"),
        ("fish", "complete -c arkmcl"),
    ] {
        let output = run(&["--lang", "en-US", "completion", shell]);
        assert!(output.status.success(), "shell: {shell}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(needle),
            "shell: {shell}"
        );
    }
}

#[test]
fn completion_scripts_track_current_flags() {
    for shell in ["bash", "zsh", "fish"] {
        let output = run(&["--lang", "en-US", "completion", shell]);
        assert!(output.status.success(), "shell: {shell}");
        let script = String::from_utf8_lossy(&output.stdout);
        for name in [
            "type",
            "group-move",
            "link",
            "lwjgl",
            "create-defaults",
            "move",
            "import",
            "download-plugin",
            "prepare-download-threads",
            "java",
        ] {
            let needle = if shell == "fish" {
                format!("-l {name}")
            } else {
                format!("--{name}")
            };
            assert!(script.contains(&needle), "shell {shell} missing {needle}");
        }
        assert!(!script.contains("--create-no-defaults"), "shell {shell}");
        assert!(!script.contains("followers"), "shell {shell}");
        assert!(!script.contains("quilt"), "shell {shell}");
    }
}

#[test]
fn completion_rejects_unknown_shell() {
    let output = run(&["--lang", "en-US", "completion", "pwsh"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn complete_prints_static_topics() {
    let output = run(&["--lang", "en-US", "complete", "resource-types"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("resourcepacks"));
}

#[test]
fn launch_terminal_rejects_batch() {
    let output = run(&["--lang", "en-US", "launch", "--terminal", "one", "two"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn launch_log_was_removed() {
    let output = run(&["--lang", "en-US", "launch", "--log"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--log"));
}
