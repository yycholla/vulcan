use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

pub fn vulcan_command() -> Command {
    Command::new(vulcan_binary_path())
}

fn vulcan_binary_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_vulcan") {
        return PathBuf::from(path);
    }

    build_vulcan_binary();
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let exe = if cfg!(windows) {
        "vulcan.exe"
    } else {
        "vulcan"
    };
    let path = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("target"))
        .join("debug")
        .join(exe);
    assert!(
        path.exists(),
        "built vulcan binary missing at {}",
        path.display()
    );
    path
}

fn build_vulcan_binary() {
    let status = StdCommand::new("cargo")
        .args(["build", "-p", "vulcan", "--bin", "vulcan"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("spawn cargo build -p vulcan --bin vulcan");
    assert!(
        status.success(),
        "cargo build -p vulcan --bin vulcan failed"
    );
}
