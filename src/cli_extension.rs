//! YYC-234 (YYC-167 PR-1): `vulcan extension` lifecycle CLI.
//!
//! list / show / enable / disable / uninstall over the YYC-166
//! store + install state. `new` / `validate` / `install` land
//! in PR-2.

use anyhow::{Context, Result, anyhow};
use std::io::Write;
use std::path::Path;

use crate::cli::ExtensionSubcommand;
use crate::config::vulcan_home;
use crate::daemon::protocol::{Request, Response, read_frame_bytes, write_request};
use crate::extensions::ExtensionRegistry;
use crate::extensions::install_state::{
    ExtensionTrustStore, InstallState, InstallStateStore, SqliteInstallStateStore,
};

pub async fn run(cmd: ExtensionSubcommand) -> Result<()> {
    let home = vulcan_home();
    let install = SqliteInstallStateStore::try_new()?;
    match cmd {
        ExtensionSubcommand::List => list(&home, &install),
        ExtensionSubcommand::Show { id } => show(&home, &install, &id),
        ExtensionSubcommand::Enable { id } => set_enabled(&home, &install, &id, true).await,
        ExtensionSubcommand::Disable { id } => set_enabled(&home, &install, &id, false).await,
        ExtensionSubcommand::Kill { id } => kill(&home, &install, &id).await,
        ExtensionSubcommand::Trust { id } => trust_workspace(&install, &id),
        ExtensionSubcommand::Untrust { id } => untrust_workspace(&install, &id),
        ExtensionSubcommand::Uninstall { id, yes } => uninstall(&home, &install, &id, yes),
        ExtensionSubcommand::New { name, kind } => scaffold_new(&name, &kind),
        ExtensionSubcommand::Validate { path } => validate_at(&path),
        ExtensionSubcommand::Install { path } => install_from_path(&home, &install, &path),
    }
}

fn list(home: &Path, install: &SqliteInstallStateStore) -> Result<()> {
    let registry = ExtensionRegistry::new();
    // GH issue #549: surface cargo-crate extensions registered via
    // `inventory::submit!` alongside manifest-discovered ones.
    crate::extensions::api::wire_inventory_into_registry(&registry);
    let cwd = std::env::current_dir()?;
    let (ok, broken) = registry.load_from_store_and_workspace(home, &cwd, install, install);
    let entries = registry.list();
    if entries.is_empty() {
        println!("(no extensions installed)");
        println!("Drop manifests under {}/extensions/<id>/", home.display());
        return Ok(());
    }
    println!(
        "{:<24} {:<10} {:<10} {:<8} description",
        "id", "status", "version", "source"
    );
    for meta in entries {
        let source = match meta.source {
            crate::extensions::ExtensionSource::Builtin => "builtin",
            crate::extensions::ExtensionSource::LocalManifest => "local",
            crate::extensions::ExtensionSource::UntrustedSource => "workspace",
            crate::extensions::ExtensionSource::SkillDraft => "skill",
        };
        let desc_preview: String = meta
            .description
            .replace('\n', " ")
            .chars()
            .take(60)
            .collect();
        println!(
            "{:<24} {:<10} {:<10} {:<8} {}",
            meta.id,
            meta.status.as_str(),
            meta.version,
            source,
            desc_preview
        );
    }
    println!();
    println!("loaded ok: {ok}, broken: {broken}");
    Ok(())
}

fn show(home: &Path, install: &SqliteInstallStateStore, id: &str) -> Result<()> {
    let registry = ExtensionRegistry::new();
    crate::extensions::api::wire_inventory_into_registry(&registry);
    let cwd = std::env::current_dir()?;
    registry.load_from_store_and_workspace(home, &cwd, install, install);
    let meta = registry
        .get(id)
        .ok_or_else(|| anyhow!("extension `{id}` not installed"))?;
    println!("Extension {}", meta.id);
    println!("  name:        {}", meta.name);
    println!("  version:     {}", meta.version);
    println!("  status:      {}", meta.status.as_str());
    let source = match meta.source {
        crate::extensions::ExtensionSource::Builtin => "builtin",
        crate::extensions::ExtensionSource::LocalManifest => "local-manifest",
        crate::extensions::ExtensionSource::UntrustedSource => "workspace-untrusted",
        crate::extensions::ExtensionSource::SkillDraft => "skill-draft",
    };
    println!("  source:      {source}");
    println!("  priority:    {}", meta.priority);
    if !meta.description.is_empty() {
        println!("  description: {}", meta.description);
    }
    if let Some(perms) = &meta.permissions_summary {
        println!("  permissions: {perms}");
    }
    if let Some(reason) = &meta.broken_reason {
        println!("  broken_reason: {reason}");
    }
    if !meta.capabilities.is_empty() {
        let caps: Vec<&'static str> = meta.capabilities.iter().map(|c| c.as_str()).collect();
        println!("  capabilities: [{}]", caps.join(", "));
    }
    if let Some(state) = install.get(id)? {
        println!("\n  install_state.enabled:        {}", state.enabled);
        println!(
            "  install_state.installed_at:   {}",
            state.installed_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        if let Some(err) = &state.last_load_error {
            println!("  install_state.last_load_error: {err}");
        }
    } else {
        println!("\n  install_state: (no row — extension never explicitly installed)");
    }
    Ok(())
}

async fn set_enabled(
    home: &Path,
    install: &SqliteInstallStateStore,
    id: &str,
    enabled: bool,
) -> Result<()> {
    // Confirm the manifest exists before flipping state — flipping
    // state on an unknown id would silently rot.
    let registry = ExtensionRegistry::new();
    let cwd = std::env::current_dir()?;
    registry.load_from_store_and_workspace(home, &cwd, install, install);
    let meta = registry
        .get(id)
        .ok_or_else(|| anyhow!("extension `{id}` not installed"))?;
    if meta.source == crate::extensions::ExtensionSource::UntrustedSource
        && meta.broken_reason.as_deref() == Some("workspace extension requires trust")
        && enabled
    {
        return Err(anyhow!(
            "extension `{id}` was discovered from this workspace and must be trusted first"
        ));
    }
    // Upsert a state row if missing so subsequent flips have
    // something to mutate.
    if install.get(id)?.is_none() {
        let manifest_path = manifest_path_for(home, &cwd, id)
            .ok_or_else(|| anyhow!("extension `{id}` manifest not found"))?;
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let manifest = crate::extensions::ExtensionManifest::from_toml_str(&raw)?;
        install.upsert(&InstallState {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            enabled,
            installed_at: chrono::Utc::now(),
            last_load_error: None,
        })?;
        println!(
            "{} (newly tracked) {}",
            if enabled { "enabled" } else { "disabled" },
            id
        );
        notify_daemon_lifecycle(id, enabled).await?;
        return Ok(());
    }
    let flipped = install.set_enabled(id, enabled)?;
    if !flipped {
        return Err(anyhow!("extension `{id}` not installed"));
    }
    println!("{} {id}", if enabled { "enabled" } else { "disabled" });
    notify_daemon_lifecycle(id, enabled).await?;
    Ok(())
}

async fn kill(home: &Path, install: &SqliteInstallStateStore, id: &str) -> Result<()> {
    set_enabled(home, install, id, false).await?;
    if let Some(result) = call_daemon("extension.kill", serde_json::json!({ "id": id })).await? {
        println!("live daemon: {}", serde_json::to_string(&result)?);
    }
    println!("warning: kill may break in-flight tool calls for `{id}`");
    println!("force-stopped {id}");
    Ok(())
}

async fn notify_daemon_lifecycle(id: &str, enabled: bool) -> Result<()> {
    let method = if enabled {
        "extension.enable"
    } else {
        "extension.disable"
    };
    if let Some(result) = call_daemon(method, serde_json::json!({ "id": id })).await? {
        println!("live daemon: {}", serde_json::to_string(&result)?);
    }
    Ok(())
}

fn trust_workspace(install: &SqliteInstallStateStore, id: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let manifest_path = cwd.join(format!(".vulcan/extensions/{id}/extension.toml"));
    let raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest = crate::extensions::ExtensionManifest::from_toml_str(&raw)?;
    if manifest.id != id {
        return Err(anyhow!(
            "manifest id `{}` does not match requested extension `{id}`",
            manifest.id
        ));
    }
    let checksum = crate::extensions::store::manifest_checksum(&raw);
    let workspace_hash = workspace_hash(&cwd);
    install.trust(&workspace_hash, id, &checksum)?;
    if install.get(id)?.is_none() {
        install.upsert(&InstallState {
            id: manifest.id,
            version: manifest.version,
            enabled: false,
            installed_at: chrono::Utc::now(),
            last_load_error: None,
        })?;
    }
    println!("trusted {id} for workspace {}", cwd.display());
    println!("Run `vulcan extension enable {id}` to activate.");
    Ok(())
}

fn untrust_workspace(install: &SqliteInstallStateStore, id: &str) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let removed = install.untrust(&workspace_hash(&cwd), id)?;
    if removed {
        println!("untrusted {id} for workspace {}", cwd.display());
    } else {
        println!("no trust marker for {id} in workspace {}", cwd.display());
    }
    Ok(())
}

fn manifest_path_for(home: &Path, workspace: &Path, id: &str) -> Option<std::path::PathBuf> {
    let home_path = home.join(format!("extensions/{id}/extension.toml"));
    if home_path.is_file() {
        return Some(home_path);
    }
    let workspace_path = workspace.join(format!(".vulcan/extensions/{id}/extension.toml"));
    workspace_path.is_file().then_some(workspace_path)
}

fn workspace_hash(workspace: &Path) -> String {
    crate::extensions::store::manifest_checksum(&workspace.display().to_string())
}

async fn call_daemon(method: &str, params: serde_json::Value) -> Result<Option<serde_json::Value>> {
    let sock_path = vulcan_home().join("vulcan.sock");
    let mut stream = match tokio::net::UnixStream::connect(&sock_path).await {
        Ok(stream) => stream,
        Err(_) => return Ok(None),
    };
    let req = Request {
        version: 1,
        id: format!("cli-{method}"),
        session: "main".into(),
        method: method.into(),
        params,
    };
    write_request(&mut stream, &req)
        .await
        .context("writing daemon lifecycle request")?;
    let body = read_frame_bytes(&mut stream)
        .await
        .context("reading daemon lifecycle response")?;
    let resp: Response = serde_json::from_slice(&body).context("decoding daemon response")?;
    if let Some(err) = resp.error {
        anyhow::bail!("{}: {}", err.code, err.message);
    }
    Ok(resp.result)
}

fn uninstall(
    home: &Path,
    install: &SqliteInstallStateStore,
    id: &str,
    skip_prompt: bool,
) -> Result<()> {
    let dir = home.join(format!("extensions/{id}"));
    let dir_exists = dir.is_dir();
    let state_exists = install.get(id)?.is_some();
    if !dir_exists && !state_exists {
        return Err(anyhow!("extension `{id}` not installed"));
    }
    println!("About to uninstall extension `{id}`:");
    if dir_exists {
        println!("  - remove directory: {}", dir.display());
    }
    if state_exists {
        println!("  - remove install state row");
    }
    if !skip_prompt && !confirm("Type the extension id to confirm: ", id)? {
        println!("Aborted.");
        return Ok(());
    }
    if dir_exists {
        std::fs::remove_dir_all(&dir)?;
    }
    if state_exists {
        install.remove(id)?;
    }
    println!("uninstalled {id}");
    Ok(())
}

fn scaffold_new(name: &str, kind: &str) -> Result<()> {
    let id = name.to_ascii_lowercase().replace([' ', '_'], "-");
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || id.is_empty()
    {
        return Err(anyhow!(
            "name `{name}` cannot become a valid extension id (lowercase letters, digits, `-`)"
        ));
    }
    let dest = std::env::current_dir()?.join(&id);
    if dest.exists() {
        return Err(anyhow!(
            "destination {} already exists; refusing to overwrite",
            dest.display()
        ));
    }
    std::fs::create_dir_all(&dest)?;

    let manifest = match kind {
        "prompt" => format!(
            r#"id = "{id}"
name = "{name}"
version = "0.1.0"
description = "Prompt-only extension scaffolded by `vulcan extension new`."
capabilities = ["prompt_injection"]

[entry]
kind = "builtin"
"#
        ),
        "rust" => format!(
            r#"id = "{id}"
name = "{name}"
version = "0.1.0"
description = "Rust extension scaffolded by `vulcan extension new`."
capabilities = ["hook_handler", "tool_provider"]

[entry]
kind = "builtin"
"#
        ),
        other => {
            return Err(anyhow!("unknown --kind `{other}`. Allowed: prompt | rust"));
        }
    };
    std::fs::write(dest.join("extension.toml"), manifest)?;
    let readme = format!(
        "# {name}\n\nScaffold generated by `vulcan extension new` (kind: `{kind}`).\n\n\
         Edit `extension.toml` to declare capabilities + permissions.\n\nInstall:\n\n```\n\
         vulcan extension validate {id}\n\
         vulcan extension install {id}\n\
         vulcan extension enable {id}\n```\n"
    );
    std::fs::write(dest.join("README.md"), readme)?;
    if kind == "rust" {
        let cargo = format!(
            r#"[package]
name = "{id}"
version = "0.1.0"
edition = "2024"

[dependencies]
"#
        );
        std::fs::write(dest.join("Cargo.toml"), cargo)?;
        std::fs::create_dir_all(dest.join("src"))?;
        std::fs::write(
            dest.join("src/lib.rs"),
            "//! TODO: implement `vulcan::extensions::CodeExtension` here.\n",
        )?;
    }
    println!("scaffolded {} at {}", id, dest.display());
    Ok(())
}

fn validate_at(path: &Path) -> Result<()> {
    let manifest_path = if path.is_dir() {
        path.join("extension.toml")
    } else {
        path.to_path_buf()
    };
    if !manifest_path.is_file() {
        return Err(anyhow!(
            "no extension.toml found at {}",
            manifest_path.display()
        ));
    }
    let raw = std::fs::read_to_string(&manifest_path)?;
    let manifest = crate::extensions::ExtensionManifest::from_toml_str(&raw)?;
    let pkg_version = env!("CARGO_PKG_VERSION");
    crate::extensions::verify_compatible(&manifest, pkg_version)?;
    println!("ok: {} v{}", manifest.id, manifest.version);
    if let Some(min) = &manifest.min_vulcan_version {
        println!("    min_vulcan_version: {min} (running {pkg_version})");
    }
    if !manifest.capabilities.is_empty() {
        println!("    capabilities: [{}]", manifest.capabilities.join(", "));
    }
    Ok(())
}

fn install_from_path(home: &Path, install: &SqliteInstallStateStore, path: &Path) -> Result<()> {
    let manifest_path = if path.is_dir() {
        path.join("extension.toml")
    } else {
        return Err(anyhow!(
            "install target must be a directory containing extension.toml"
        ));
    };
    let raw = std::fs::read_to_string(&manifest_path)?;
    let manifest = crate::extensions::ExtensionManifest::from_toml_str(&raw)?;
    crate::extensions::verify_compatible(&manifest, env!("CARGO_PKG_VERSION"))?;

    let dest = home.join(format!("extensions/{}", manifest.id));
    if dest.exists() {
        return Err(anyhow!(
            "{} is already installed at {}; uninstall first",
            manifest.id,
            dest.display()
        ));
    }
    std::fs::create_dir_all(home.join("extensions"))?;
    copy_dir_recursive(path, &dest)?;
    install.upsert(&InstallState {
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        enabled: false,
        installed_at: chrono::Utc::now(),
        last_load_error: None,
    })?;
    println!(
        "installed {} v{} at {}",
        manifest.id,
        manifest.version,
        dest.display()
    );
    println!("Run `vulcan extension enable {}` to activate.", manifest.id);
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn confirm(prompt: &str, expect: &str) -> Result<bool> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(prompt.as_bytes())?;
    stdout.flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim() == expect)
}
