//! YYC-212 PR-1: `vulcan config list/get/path/show` — read-only
//! discovery + inspection on top of the [`config_registry`].

use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, value};

use crate::cli::ConfigSubcommand;
use crate::config::{atomic_write, vulcan_home};
use crate::config_registry::{ConfigField, ConfigFile, FieldKind, all, lookup};

pub async fn run(cmd: ConfigSubcommand) -> Result<()> {
    match cmd {
        ConfigSubcommand::List => list(),
        ConfigSubcommand::Get { key, reveal } => get(&key, reveal),
        ConfigSubcommand::Path => paths(),
        ConfigSubcommand::Show { reveal } => show(reveal),
        ConfigSubcommand::Set { key, value } => set(&key, &value),
        ConfigSubcommand::Unset { key } => unset(&key),
    }
}

fn set(key: &str, raw: &str) -> Result<()> {
    let field = lookup(key).ok_or_else(|| {
        anyhow!("unknown config field `{key}`. `vulcan config list` for the catalog.")
    })?;
    let parsed = parse_value(field, raw)?;
    let path = file_path_for(field.file);
    let mut doc = read_document(&path)?;
    apply_set(&mut doc, key, parsed)?;
    write_document(&path, &doc)?;
    println!("set {key} in {}", path.display());
    Ok(())
}

fn unset(key: &str) -> Result<()> {
    let field = lookup(key).ok_or_else(|| {
        anyhow!("unknown config field `{key}`. `vulcan config list` for the catalog.")
    })?;
    let path = file_path_for(field.file);
    if !path.exists() {
        println!("(no override; default already in effect)");
        return Ok(());
    }
    let mut doc = read_document(&path)?;
    let removed = apply_unset(&mut doc, key);
    if removed {
        write_document(&path, &doc)?;
        println!("removed {key} from {}", path.display());
    } else {
        println!("(no override; default already in effect)");
    }
    Ok(())
}

/// Parse `raw` against the declared `FieldKind`. Returns the
/// canonical `toml_edit::Value`. Validation errors carry the
/// allowed values so the user can self-correct.
fn parse_value(field: &ConfigField, raw: &str) -> Result<toml_edit::Value> {
    match &field.kind {
        FieldKind::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Ok(true.into()),
            "false" | "off" | "no" | "0" => Ok(false.into()),
            _ => bail!("value `{raw}` is not a boolean. Accepted: true|false|on|off|yes|no|1|0"),
        },
        FieldKind::Int { min, max } => {
            let n: i64 = raw
                .parse()
                .map_err(|_| anyhow!("`{raw}` is not an integer"))?;
            if let Some(lo) = min {
                if n < *lo {
                    bail!("{n} below minimum {lo} for {}", field.path);
                }
            }
            if let Some(hi) = max {
                if n > *hi {
                    bail!("{n} above maximum {hi} for {}", field.path);
                }
            }
            Ok(n.into())
        }
        FieldKind::Float { min, max } => {
            let n: f64 = raw
                .parse()
                .map_err(|_| anyhow!("`{raw}` is not a number"))?;
            if let Some(lo) = min {
                if n < *lo {
                    bail!("{n} below minimum {lo} for {}", field.path);
                }
            }
            if let Some(hi) = max {
                if n > *hi {
                    bail!("{n} above maximum {hi} for {}", field.path);
                }
            }
            Ok(n.into())
        }
        FieldKind::Enum { variants } => {
            if variants.iter().any(|v| *v == raw) {
                Ok(raw.into())
            } else {
                bail!(
                    "value `{raw}` not in allowed set for {}. Allowed: [{}]",
                    field.path,
                    variants.join(", ")
                )
            }
        }
        FieldKind::String { .. } | FieldKind::Path => Ok(raw.into()),
    }
}

fn read_document(path: &Path) -> Result<DocumentMut> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let doc: DocumentMut = raw.parse().map_err(|e| {
        anyhow!(
            "failed to parse {} (refusing to overwrite a malformed file): {e}",
            path.display()
        )
    })?;
    Ok(doc)
}

fn write_document(path: &Path, doc: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(path, &doc.to_string())
}

fn apply_set(doc: &mut DocumentMut, dotted: &str, val: toml_edit::Value) -> Result<()> {
    let parts: Vec<&str> = dotted.split('.').collect();
    let leaf = parts.last().copied().expect("at least one segment");
    let mut current = doc.as_table_mut() as &mut dyn TableLike;
    for seg in parts.iter().take(parts.len() - 1) {
        current = ensure_table(current, seg)?;
    }
    current.insert_value(leaf, val);
    Ok(())
}

fn apply_unset(doc: &mut DocumentMut, dotted: &str) -> bool {
    let parts: Vec<&str> = dotted.split('.').collect();
    let leaf = match parts.last().copied() {
        Some(s) => s,
        None => return false,
    };
    // Walk to the parent table without creating intermediates;
    // bail early if a segment is absent or non-table.
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    for seg in parts.iter().take(parts.len() - 1) {
        match current.lookup_table_mut(seg) {
            Some(next) => current = next,
            None => return false,
        }
    }
    current.remove_key(leaf)
}

/// Tiny trait so we can reuse the dotted-path walker for both
/// the document root (`Table`) and inline subtables.
trait TableLike {
    fn insert_value(&mut self, key: &str, val: toml_edit::Value);
    fn remove_key(&mut self, key: &str) -> bool;
    fn lookup_table_mut(&mut self, key: &str) -> Option<&mut dyn TableLike>;
    fn ensure_subtable(&mut self, key: &str) -> Option<&mut dyn TableLike>;
}

impl TableLike for toml_edit::Table {
    fn insert_value(&mut self, key: &str, val: toml_edit::Value) {
        self.insert(key, value(val));
    }

    fn remove_key(&mut self, key: &str) -> bool {
        self.remove(key).is_some()
    }

    fn lookup_table_mut(&mut self, key: &str) -> Option<&mut dyn TableLike> {
        match self.get_mut(key)? {
            Item::Table(t) => Some(t as &mut dyn TableLike),
            _ => None,
        }
    }

    fn ensure_subtable(&mut self, key: &str) -> Option<&mut dyn TableLike> {
        // entry().or_insert with a Table item.
        let entry = self
            .entry(key)
            .or_insert_with(|| Item::Table(toml_edit::Table::new()));
        match entry {
            Item::Table(t) => Some(t as &mut dyn TableLike),
            _ => None,
        }
    }
}

fn ensure_table<'a>(parent: &'a mut dyn TableLike, key: &str) -> Result<&'a mut dyn TableLike> {
    parent
        .ensure_subtable(key)
        .ok_or_else(|| anyhow!("config segment `{key}` exists but is not a table"))
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
        FieldKind::Float { min, max } => match (min, max) {
            (Some(lo), Some(hi)) => format!("float {lo}..{hi}"),
            (Some(lo), None) => format!("float ≥ {lo}"),
            (None, Some(hi)) => format!("float ≤ {hi}"),
            (None, None) => "float".into(),
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

    // ── YYC-212 PR-2 writers ─────────────────────────────────

    #[test]
    fn parse_value_validates_enum_against_declared_set() {
        let f = enum_field();
        assert!(parse_value(&f, "block").is_ok());
        let err = parse_value(&f, "wat").unwrap_err().to_string();
        assert!(err.contains("not in allowed set"), "got {err:?}");
        assert!(err.contains("[off, warn, block]"));
    }

    #[test]
    fn parse_value_enforces_int_bounds() {
        let f = lookup("tools.dangerous_commands.quota_per_session")
            .unwrap()
            .clone();
        assert!(parse_value(&f, "5").is_ok());
        assert!(
            parse_value(&f, "-1")
                .unwrap_err()
                .to_string()
                .contains("below")
        );
        assert!(
            parse_value(&f, "9999")
                .unwrap_err()
                .to_string()
                .contains("above")
        );
    }

    #[test]
    fn parse_value_accepts_float_in_range() {
        let f = lookup("compaction.trigger_ratio").unwrap().clone();
        assert!(parse_value(&f, "0.9").is_ok());
        assert!(
            parse_value(&f, "1.5")
                .unwrap_err()
                .to_string()
                .contains("above")
        );
        assert!(
            parse_value(&f, "abc")
                .unwrap_err()
                .to_string()
                .contains("not a number")
        );
    }

    #[test]
    fn parse_value_normalizes_bool_synonyms() {
        let f = lookup("tools.yolo_mode").unwrap().clone();
        for yes in ["true", "on", "yes", "1", "TRUE"] {
            assert!(matches!(parse_value(&f, yes).unwrap(), v if v.as_bool() == Some(true)));
        }
        for no in ["false", "off", "no", "0", "FALSE"] {
            assert!(matches!(parse_value(&f, no).unwrap(), v if v.as_bool() == Some(false)));
        }
        assert!(parse_value(&f, "maybe").is_err());
    }

    #[test]
    fn apply_set_creates_nested_table_and_writes_value() {
        let mut doc = DocumentMut::new();
        apply_set(&mut doc, "compaction.trigger_ratio", 0.9.into()).unwrap();
        let serialized = doc.to_string();
        assert!(
            serialized.contains("[compaction]"),
            "expected nested table header, got:\n{serialized}"
        );
        assert!(
            serialized.contains("trigger_ratio = 0.9"),
            "missing value, got:\n{serialized}"
        );
    }

    #[test]
    fn apply_unset_removes_existing_leaf() {
        let mut doc: DocumentMut = "[compaction]\ntrigger_ratio = 0.9\n"
            .parse()
            .expect("seed parses");
        let removed = apply_unset(&mut doc, "compaction.trigger_ratio");
        assert!(removed);
        assert!(!doc.to_string().contains("trigger_ratio"));
    }

    #[test]
    fn apply_unset_returns_false_when_missing() {
        let mut doc: DocumentMut = "[compaction]\n".parse().unwrap();
        let removed = apply_unset(&mut doc, "compaction.trigger_ratio");
        assert!(!removed);
    }
}
