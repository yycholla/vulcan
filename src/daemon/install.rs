//! Emit a systemd user unit so users who want supervised auto-restart
//! can opt in (`systemctl --user enable --now vulcan.service`).
//!
//! Per YYC-266 design: the daemon's default lifecycle is
//! `fork+exec` from the CLI client, with a systemd unit shipped as an
//! optional escape hatch for users who want crash auto-restart.
//! Slice 0 only covers the unit file emission; enabling the unit is the
//! user's responsibility.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// Build the unit file body for `path/to/vulcan` as the executable.
pub(crate) fn render_unit(exe: &Path) -> String {
    format!(
        r#"[Unit]
Description=Vulcan AI agent daemon
After=network.target

[Service]
Type=simple
ExecStart={exe} daemon start
Restart=on-failure
RestartSec=2s

[Install]
WantedBy=default.target
"#,
        exe = exe.display(),
    )
}

/// Resolve the systemd user-unit directory, honoring `XDG_CONFIG_HOME`
/// then falling back to `$HOME/.config`.
fn systemd_user_unit_dir() -> anyhow::Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .context("neither XDG_CONFIG_HOME nor HOME is set")?;
            PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("systemd").join("user"))
}

/// Write `vulcan.service` under `unit_dir`. Caller chooses the dir;
/// tests pass a tempdir, the CLI passes the real XDG path.
pub fn write_systemd_unit(unit_dir: &Path, exe: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(unit_dir)
        .with_context(|| format!("creating {}", unit_dir.display()))?;
    let unit_path = unit_dir.join("vulcan.service");
    let body = render_unit(exe);
    std::fs::write(&unit_path, body).with_context(|| format!("writing {}", unit_path.display()))?;
    Ok(unit_path)
}

/// CLI entry — resolves the XDG user unit dir, locates the running
/// `vulcan` exe, writes the unit, returns the path written.
pub fn install_systemd_default() -> anyhow::Result<PathBuf> {
    let dir = systemd_user_unit_dir()?;
    let exe = std::env::current_exe().context("locating own exe")?;
    write_systemd_unit(&dir, &exe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn render_unit_contains_required_sections() {
        let body = render_unit(Path::new("/opt/vulcan/bin/vulcan"));
        assert!(body.contains("[Unit]"));
        assert!(body.contains("[Service]"));
        assert!(body.contains("[Install]"));
        assert!(body.contains("ExecStart=/opt/vulcan/bin/vulcan daemon start"));
        assert!(body.contains("Restart=on-failure"));
        assert!(body.contains("WantedBy=default.target"));
    }

    #[test]
    fn write_systemd_unit_creates_file() {
        let dir = tempdir().unwrap();
        let exe = Path::new("/usr/local/bin/vulcan");
        let unit_path = write_systemd_unit(dir.path(), exe).unwrap();
        assert!(unit_path.exists());
        assert_eq!(unit_path.file_name().unwrap(), "vulcan.service");
        let body = std::fs::read_to_string(&unit_path).unwrap();
        assert!(body.contains("ExecStart=/usr/local/bin/vulcan daemon start"));
    }

    #[test]
    fn write_systemd_unit_creates_missing_parent_dirs() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        assert!(!nested.exists());
        let exe = Path::new("/x/vulcan");
        write_systemd_unit(&nested, exe).unwrap();
        assert!(nested.join("vulcan.service").exists());
    }

    #[test]
    fn write_systemd_unit_overwrites_existing() {
        let dir = tempdir().unwrap();
        write_systemd_unit(dir.path(), Path::new("/old/vulcan")).unwrap();
        write_systemd_unit(dir.path(), Path::new("/new/vulcan")).unwrap();
        let body = std::fs::read_to_string(dir.path().join("vulcan.service")).unwrap();
        assert!(body.contains("/new/vulcan"));
        assert!(!body.contains("/old/vulcan"));
    }
}
