//! 系统密钥环（Secret Service，通过 `secret-tool`）。
//!
//! 不可用时由调用方回退到文件存储。

use std::io::Write;
use std::process::{Command, Stdio};

const SERVICE: &str = "com.purft.arkmcl";
const LABEL: &str = "ArkMCL";

/// 当前环境是否可用密钥环。
#[must_use]
pub fn enabled() -> bool {
    if disabled_by_env() {
        return false;
    }
    Command::new("secret-tool")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// 读取指定账户的密钥。
#[must_use]
pub fn lookup(account: &str) -> Option<String> {
    let output = Command::new("secret-tool")
        .args(["lookup", "service", SERVICE, "account", account])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let secret = String::from_utf8(output.stdout).ok()?;
    let trimmed = secret.trim_end_matches(['\r', '\n']).to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// 写入指定账户的密钥。
#[must_use]
pub fn store(account: &str, secret: &str) -> bool {
    let label = format!("{LABEL} {account}");
    let Ok(mut child) = Command::new("secret-tool")
        .args([
            "store", "--label", &label, "service", SERVICE, "account", account,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(secret.as_bytes()).is_err() {
            return false;
        }
    }
    child.wait().is_ok_and(|status| status.success())
}

/// 删除指定账户的密钥。
#[must_use]
pub fn clear(account: &str) -> bool {
    Command::new("secret-tool")
        .args(["clear", "service", SERVICE, "account", account])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn disabled_by_env() -> bool {
    std::env::var("ARKMCL_DISABLE_KEYRING").is_ok_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}
