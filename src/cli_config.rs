//! YYC-212 PR-1: `vulcan config list/get/path/show` — read-only
//! discovery + inspection on top of the [`config_registry`].

use anyhow::{Result, anyhow};
use std::path::PathBuf;

use crate::cli::ConfigSubcommand;
use crate::config::vulcan_home;
use crate::config_registry::{ConfigField, ConfigFile, FieldKind, all, lookup};

pub async fn run(cmd: ConfigSubcommand) -> Result<()> {
    match cmd {
        ConfigSubcommand::List => list(),
        ConfigSubcommand::Get { key, reveal } => get(&key, reveal),
        ConfigSubcommand::Path => paths(),
        ConfigSubcommand::Show { reveal } => show(reveal),
    }
}

fn list() -> Result<()> {
    println!(
        "{:<48} {:<14} {:<20} {:<14} help",
        "key", "kind", "default", "file"
    );
    for f in all() {
        let kind_str = format_kind(&f.kind);
        let file_str = format_file(f.file);
        println!(
            "{:<48} {:<14} {:<20} {:<14} {}",
            f.path, kind_str, f.default, file_str, f.help
        );
    }
    Ok(())
}

fn get(key: &str, reveal: bool) -> Result<()> {
    let field = lookup(key).ok_or_else(|| {
        anyhow!("unknown config field `{key}`. `vulcan config list` for the catalog.")
    })?;
    let raw = lookup_in_file(field, key)?;
    let display = match raw {
        None => "(unset; default applies)".to_string(),
        Some(v) => format_value(&v, field, reveal),
    };
    println!("{display}");
    Ok(())
}

fn paths() -> Result<()> {
    let dir = vulcan_home();
    println!("vulcan_home: {}", dir.display());
    for (name, sub) in [
        ("config", "config.toml"),
        ("keybinds", "keybinds.toml"),
        ("providers", "providers.toml"),
    ] {
        let p = dir.join(sub);
        let exists = if p.exists() { "" } else { "  (not present)" };
        println!("{name}: {}{exists}", p.display());
    }
    Ok(())
}

fn show(reveal: bool) -> Result<()> {
    for f in all() {
        let raw = lookup_in_file(f, f.path)?;
        let display = match raw {
            None => "(default)".to_string(),
            Some(v) => format_value(&v, f, reveal),
        };
        println!("{} = {}", f.path, display);
    }
    Ok(())
}

/// Walk the dotted `key` through the on-disk TOML file backing
/// the field. Returns the raw `toml::Value` if present, `None`
/// when the file or key is missing (caller substitutes the
/// declared default).
fn lookup_in_file(field: &ConfigField, key: &str) -> Result<Option<toml::Value>> {
    let path = file_path_for(field.file);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let doc: toml::Value = toml::from_str(&raw)?;
    let mut current = &doc;
    for segment in key.split('.') {
        match current.get(segment) {
            Some(next) => current = next,
            None => return Ok(None),
        }
    }
    Ok(Some(current.clone()))
}

fn file_path_for(file: ConfigFile) -> PathBuf {
    let dir = vulcan_home();
    match file {
        ConfigFile::Config => dir.join("config.toml"),
        ConfigFile::Keybinds => dir.join("keybinds.toml"),
        ConfigFile::Providers => dir.join("providers.toml"),
    }
}

fn format_value(v: &toml::Value, field: &ConfigField, reveal: bool) -> String {
    if let FieldKind::String { secret: true } = field.kind {
        if !reveal {
            return match v {
                toml::Value::String(s) if s.is_empty() => "(unset)".into(),
                _ => "***redacted*** (pass --reveal to show)".into(),
            };
        }
    }
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        other => other.to_string(),
    }
}

fn format_kind(k: &FieldKind) -> String {
    match k {
        FieldKind::Bool => "bool".into(),
        FieldKind::Int { min, max } => match (min, max) {
            (Some(lo), Some(hi)) => format!("int {lo}..{hi}"),
            (Some(lo), None) => format!("int ≥ {lo}"),
            (None, Some(hi)) => format!("int ≤ {hi}"),
            (None, None) => "int".into(),
        },
        FieldKind::Enum { variants } => format!("enum [{}]", variants.join(", ")),
        FieldKind::String { secret } => {
            if *secret {
                "string (secret)".into()
            } else {
                "string".into()
            }
        }
        FieldKind::Path => "path".into(),
    }
}

fn format_file(f: ConfigFile) -> &'static str {
    match f {
        ConfigFile::Config => "config",
        ConfigFile::Keybinds => "keybinds",
        ConfigFile::Providers => "providers",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enum_field() -> ConfigField {
        lookup("tools.native_enforcement").unwrap().clone()
    }

    fn secret_field() -> ConfigField {
        lookup("provider.api_key").unwrap().clone()
    }

    #[test]
    fn format_value_redacts_secret_by_default() {
        let f = secret_field();
        let v = toml::Value::String("super-secret-key".into());
        let out = format_value(&v, &f, false);
        assert!(out.contains("redacted"), "expected redaction, got {out:?}");
        assert!(!out.contains("super-secret-key"));
    }

    #[test]
    fn format_value_reveals_secret_with_flag() {
        let f = secret_field();
        let v = toml::Value::String("super-secret-key".into());
        let out = format_value(&v, &f, true);
        assert_eq!(out, "super-secret-key");
    }

    #[test]
    fn format_value_renders_bool_and_string() {
        let f = enum_field();
        let block = toml::Value::String("block".into());
        assert_eq!(format_value(&block, &f, false), "block");
        let bool_field = lookup("tools.yolo_mode").unwrap();
        assert_eq!(
            format_value(&toml::Value::Boolean(true), bool_field, false),
            "true"
        );
    }

    #[test]
    fn format_kind_renders_enum_with_variants() {
        let s = format_kind(&FieldKind::Enum {
            variants: &["a", "b"],
        });
        assert_eq!(s, "enum [a, b]");
    }
}
