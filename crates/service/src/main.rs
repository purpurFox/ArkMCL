//! `arkmcl-service` 入口。

use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    arkmcl_service::main_entry()
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("arkmcl-service is not supported on this platform");
    ExitCode::from(1)
}
