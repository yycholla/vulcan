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
use crate::extensions::ExtensionRegistry;
use crate::extensions::install_state::{InstallState, InstallStateStore, SqliteInstallStateStore};

pub async fn run(cmd: ExtensionSubcommand) -> Result<()> {
    let home = vulcan_home();
    let install = SqliteInstallStateStore::try_new()?;
    match cmd {
        ExtensionSubcommand::List => list(&home, &install),
        ExtensionSubcommand::Show { id } => show(&home, &install, &id),
        ExtensionSubcommand::Enable { id } => set_enabled(&home, &install, &id, true),
        ExtensionSubcommand::Disable { id } => set_enabled(&home, &install, &id, false),
        ExtensionSubcommand::Uninstall { id, yes } => uninstall(&home, &install, &id, yes),
    }
}

fn list(home: &Path, install: &SqliteInstallStateStore) -> Result<()> {
    let registry = ExtensionRegistry::new();
    let (ok, broken) = registry.load_from_store(home, install);
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
    registry.load_from_store(home, install);
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

fn set_enabled(
    home: &Path,
    install: &SqliteInstallStateStore,
    id: &str,
    enabled: bool,
) -> Result<()> {
    // Confirm the manifest exists before flipping state — flipping
    // state on an unknown id would silently rot.
    let registry = ExtensionRegistry::new();
    registry.load_from_store(home, install);
    if registry.get(id).is_none() {
        return Err(anyhow!("extension `{id}` not installed"));
    }
    // Upsert a state row if missing so subsequent flips have
    // something to mutate.
    if install.get(id)?.is_none() {
        let manifest_path = home.join(format!("extensions/{id}/extension.toml"));
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
        return Ok(());
    }
    let flipped = install.set_enabled(id, enabled)?;
    if !flipped {
        return Err(anyhow!("extension `{id}` not installed"));
    }
    println!("{} {id}", if enabled { "enabled" } else { "disabled" });
    Ok(())
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

fn confirm(prompt: &str, expect: &str) -> Result<bool> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(prompt.as_bytes())?;
    stdout.flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim() == expect)
}
