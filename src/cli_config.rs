//! YYC-212 PR-1: `vulcan config list/get/path/show` — read-only
//! discovery + inspection on top of the [`config_registry`].
//! YYC-286: interactive picker + field-aware set when no value given.

use anyhow::{Context, Result, anyhow, bail};
use dialoguer::{Confirm, FuzzySelect, Input, Password};
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, value};

use crate::cli::ConfigSubcommand;
use crate::config::{atomic_write, vulcan_home};
use crate::config_registry::{ConfigField, ConfigFile, FieldKind, all, lookup};

pub async fn run(cmd: Option<ConfigSubcommand>) -> Result<()> {
    match cmd {
        None => interactive_picker(),
        Some(ConfigSubcommand::List) => list(),
        Some(ConfigSubcommand::Get { key, reveal }) => get(&key, reveal),
        Some(ConfigSubcommand::Path) => paths(),
        Some(ConfigSubcommand::Show { reveal }) => show(reveal),
        Some(ConfigSubcommand::Set {
            key,
            value: Some(raw),
        }) => set(&key, &raw),
        Some(ConfigSubcommand::Set { key, value: None }) => interactive_set(&key),
        Some(ConfigSubcommand::Unset { key }) => unset(&key),
        Some(ConfigSubcommand::Edit { section }) => edit(section.as_deref()),
    }
}

/// YYC-217: resolve a section name to the file that owns it under
/// the split layout (`keybinds.toml`, `providers.toml`,
/// `config.toml`). Pure so tests don't touch the filesystem.
fn resolve_section_path(home: &Path, section: Option<&str>) -> PathBuf {
    match section {
        Some("keybinds") => home.join("keybinds.toml"),
        Some("providers") | Some("provider") => home.join("providers.toml"),
        Some(_) | None => home.join("config.toml"),
    }
}

/// YYC-217: open the right config file in the user's `$EDITOR`. If
/// no editor is set, print the path so the user can open it
/// themselves. Routes section name → file via the same split
/// layout the registry uses (`config.toml` / `keybinds.toml` /
/// `providers.toml`).
fn edit(section: Option<&str>) -> Result<()> {
    let path = resolve_section_path(&vulcan_home(), section);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, "")?;
    }
    match std::env::var("EDITOR") {
        Ok(editor) if !editor.trim().is_empty() => {
            let status = std::process::Command::new(editor.trim())
                .arg(&path)
                .status()?;
            if !status.success() {
                bail!(
                    "editor exited non-zero ({status}). Check {} manually.",
                    path.display()
                );
            }
            println!("Saved {}", path.display());
            Ok(())
        }
        _ => {
            println!("$EDITOR is not set; open this file manually:");
            println!("  {}", path.display());
            Ok(())
        }
    }
}

/// Group fields by their top-level category (first path segment).
fn group_fields(fields: &[ConfigField]) -> Vec<(&str, Vec<&ConfigField>)> {
    let mut map: std::collections::BTreeMap<&str, Vec<&ConfigField>> =
        std::collections::BTreeMap::new();
    for f in fields {
        let cat = f.path.split('.').next().unwrap_or(f.path);
        map.entry(cat).or_default().push(f);
    }
    map.into_iter().collect()
}

/// No subcommand given → pick a category, then a field within it.
fn interactive_picker() -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "vulcan config (interactive) requires a terminal. Use `vulcan config list` to browse fields, or `vulcan config set <key> <value>` for scripting."
        );
    }

    let theme = dialoguer_theme();
    let groups = group_fields(&all());

    // ---- Level 1: pick a category ---------------------------------
    let cat_items: Vec<String> = groups
        .iter()
        .map(|(cat, fields)| {
            format!(
                "{} {}",
                cat.bold(),
                format!("({} fields)", fields.len()).dimmed()
            )
        })
        .collect();

    println!();
    let cat_pick = FuzzySelect::with_theme(&theme)
        .with_prompt("Pick a config category (Esc to cancel)")
        .items(&cat_items)
        .default(0)
        .interact()
        .context("cancelled")?;

    let (cat_name, cat_fields) = &groups[cat_pick];

    // ---- Level 2: pick a field within the category ----------------
    let field_labels: Vec<String> = cat_fields
        .iter()
        .map(|f| {
            let short = strip_prefix(f.path, cat_name);
            let current = match lookup_in_file(f, f.path) {
                Ok(Some(v)) => format_value(&v, f, false),
                _ => "(default)".to_string(),
            };
            format!("{:<48} {}", short, current)
        })
        .collect();

    println!();
    let field_pick = FuzzySelect::with_theme(&theme)
        .with_prompt(format!(
            "[{}] Pick a field to set (Esc to cancel)",
            cat_name
        ))
        .items(&field_labels)
        .default(0)
        .interact()
        .context("cancelled")?;

    let field = cat_fields[field_pick];
    let current_val = match lookup_in_file(field, field.path) {
        Ok(Some(v)) => format_value(&v, field, false),
        _ => format!("(default: {})", field.default),
    };
    println!();
    println!("  {}  ({})", field.path.bold(), field.help);
    println!(
        "  current: {}  |  kind: {}  |  file: {}",
        current_val,
        fmt_kind_short(&field.kind),
        format_file(field.file),
    );
    println!();

    let value = prompt_field_value(field, &theme)?;
    confirm_and_write(field, value)?;
    Ok(())
}

/// Remove a leading `prefix.` from `dotted`, returning the remainder.
fn strip_prefix<'a>(dotted: &'a str, prefix: &str) -> &'a str {
    if let Some(rest) = dotted.strip_prefix(prefix) {
        rest.strip_prefix('.').unwrap_or(rest)
    } else {
        dotted
    }
}

/// `vulcan config set <key>` without a value → field-aware prompt.
fn interactive_set(key: &str) -> Result<()> {
    let field = lookup(key).ok_or_else(|| {
        anyhow!("unknown config field `{key}`. `vulcan config list` for the catalog.")
    })?;

    if !std::io::stdin().is_terminal() {
        bail!(
            "interactive set requires a terminal. Provide the value as an argument: `vulcan config set {key} <value>`."
        );
    }

    let theme = dialoguer_theme();
    println!();
    println!("  {} ({})", field.path, field.help);
    println!(
        "  default: {}  |  kind: {}",
        field.default,
        fmt_kind_short(&field.kind)
    );
    println!();

    let value = prompt_field_value(field, &theme)?;
    confirm_and_write(field, value)?;
    Ok(())
}

/// Prompt for a value adapted to the field kind.
fn prompt_field_value(
    field: &ConfigField,
    theme: &dyn dialoguer::theme::Theme,
) -> Result<toml_edit::Value> {
    match &field.kind {
        FieldKind::Bool => {
            let confirmed = Confirm::with_theme(theme)
                .with_prompt(format!("Set {} to true?", field.path))
                .default(false)
                .interact()?;
            Ok(toml_edit::Value::from(confirmed))
        }
        FieldKind::Enum { variants } => {
            let pick = FuzzySelect::with_theme(theme)
                .with_prompt(format!("Pick value for {}", field.path))
                .items(*variants)
                .default(0)
                .interact()?;
            Ok(toml_edit::Value::from(variants[pick]))
        }
        FieldKind::Int { min, max } => {
            let default_val = field.default.parse::<i64>().unwrap_or(0);
            let range_hint = match (min, max) {
                (Some(lo), Some(hi)) => format!("{lo}..{hi}"),
                (Some(lo), None) => format!("≥ {lo}"),
                (None, Some(hi)) => format!("≤ {hi}"),
                (None, None) => "any integer".into(),
            };
            let input: String = Input::with_theme(theme)
                .with_prompt(format!("Integer for {} ({})", field.path, range_hint))
                .default(default_val.to_string())
                .validate_with(|s: &String| -> Result<(), String> {
                    let n: i64 = s.parse().map_err(|_| format!("not an integer: {s}"))?;
                    if let Some(lo) = min {
                        if n < *lo {
                            return Err(format!("below minimum {lo}"));
                        }
                    }
                    if let Some(hi) = max {
                        if n > *hi {
                            return Err(format!("above maximum {hi}"));
                        }
                    }
                    Ok(())
                })
                .interact_text()?;
            let n: i64 = input.parse().unwrap();
            Ok(toml_edit::Value::from(n))
        }
        FieldKind::Float { min, max } => {
            let default_val = field.default.parse::<f64>().unwrap_or(0.0);
            let range_hint = match (min, max) {
                (Some(lo), Some(hi)) => format!("{lo}..{hi}"),
                (Some(lo), None) => format!("≥ {lo}"),
                (None, Some(hi)) => format!("≤ {hi}"),
                (None, None) => "any float".into(),
            };
            let input: String = Input::with_theme(theme)
                .with_prompt(format!("Float for {} ({})", field.path, range_hint))
                .default(default_val.to_string())
                .validate_with(|s: &String| -> Result<(), String> {
                    let n: f64 = s.parse().map_err(|_| format!("not a number: {s}"))?;
                    if let Some(lo) = min {
                        if n < *lo {
                            return Err(format!("below minimum {lo}"));
                        }
                    }
                    if let Some(hi) = max {
                        if n > *hi {
                            return Err(format!("above maximum {hi}"));
                        }
                    }
                    Ok(())
                })
                .interact_text()?;
            let n: f64 = input.parse().unwrap();
            Ok(toml_edit::Value::from(n))
        }
        FieldKind::String { secret: true } => {
            let key: String = Password::with_theme(theme)
                .with_prompt(format!("Secret value for {} (hidden)", field.path))
                .allow_empty_password(true)
                .interact()?;
            Ok(toml_edit::Value::from(key))
        }
        FieldKind::String { secret: false } | FieldKind::Path => {
            let s: String = Input::with_theme(theme)
                .with_prompt(format!("Value for {}", field.path))
                .default(field.default.to_string())
                .interact_text()?;
            Ok(toml_edit::Value::from(s))
        }
        FieldKind::Model => pick_provider_model(field, theme),
        FieldKind::EmbeddingModel => pick_embedding_model(field, theme),
    }
}

/// Curated list of embedding models that `cortex-memory-core`'s
/// `create_embedding_service` dispatch actually recognises. Each
/// entry maps to a `fastembed::EmbeddingModel` variant inside cortex.
/// Models NOT in this list silently fall back to bge-small (a bug in
/// cortex-memory-core 0.3.1).
///
/// **Important**: switching to a model with a different dimension
/// on an existing `cortex.redb` corrupts the HNSW index. The user
/// must delete `~/.vulcan/cortex.redb` and re-seed.
const KNOWN_EMBEDDING_MODELS: &[(&str, &str)] = &[
    (
        "BAAI/bge-small-en-v1.5",
        "384-dim  ~130 MB  (default, fast)",
    ),
    ("BAAI/bge-base-en-v1.5", "768-dim  ~430 MB  (balanced)"),
    (
        "BAAI/bge-large-en-v1.5",
        "1024-dim ~1.3 GB  (highest quality)",
    ),
];

/// Suffix appended to the picker prompt when the user already has
/// a cortex.redb, warning them about re-creation.
const MODEL_SWITCH_WARNING: &str = "⚠ Switching to a different-dimension model requires deleting \
     ~/.vulcan/cortex.redb and re-seeding (vulcan --seed-cortex).";

/// Interactive fuzzy-select picker for embedding models.
fn pick_embedding_model(
    field: &ConfigField,
    theme: &dyn dialoguer::theme::Theme,
) -> Result<toml_edit::Value> {
    let labels: Vec<String> = KNOWN_EMBEDDING_MODELS
        .iter()
        .map(|(id, hint)| format!("{:<25} {}", id, hint.dimmed()))
        .collect();

    println!();
    if std::path::Path::new(&format!("{}/cortex.redb", vulcan_home().display())).exists() {
        println!("{}\n", MODEL_SWITCH_WARNING.yellow());
    }
    let pick = FuzzySelect::with_theme(theme)
        .with_prompt(format!("Pick an embedding model for {}", field.path))
        .items(&labels)
        .default(0)
        .interact()
        .context("picker cancelled")?;

    Ok(toml_edit::Value::from(KNOWN_EMBEDDING_MODELS[pick].0))
}

/// Interactive fuzzy-select picker for LLM provider models.
///
/// Fetches the catalog from the active provider's `/models` endpoint
/// (using the same cache as `vulcan model list`). Falls back to a
/// plain text input when the catalog is unavailable (offline, bad
/// key, etc.) so `config set` never dead-ends.
fn pick_provider_model(
    field: &ConfigField,
    theme: &dyn dialoguer::theme::Theme,
) -> Result<toml_edit::Value> {
    // Try to fetch the provider catalog. We need a tokio runtime
    // since `fetch_catalog` is async but we're in a sync context.
    let models = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(fetch_models_from_active_provider()),
        Err(_) => fetch_models_from_active_provider_blocking(),
    };

    match models {
        Ok(ref list) if !list.is_empty() => {
            let labels: Vec<String> = list
                .iter()
                .map(|m| {
                    let ctx = if m.context_length > 0 {
                        format!(" {}", m.context_length.to_string().dimmed())
                    } else {
                        String::new()
                    };
                    format!("{}{}", m.id, ctx)
                })
                .collect();

            println!();
            let pick = FuzzySelect::with_theme(theme)
                .with_prompt(format!("Pick a model for {}", field.path))
                .items(&labels)
                .default(0)
                .interact()
                .context("picker cancelled")?;

            Ok(toml_edit::Value::from(list[pick].id.clone()))
        }
        _ => {
            // Catalog unavailable — fall back to free text.
            tracing::warn!("provider catalog unavailable; falling back to text input");
            let s: String = Input::with_theme(theme)
                .with_prompt(format!("Model id for {} (catalog unavailable)", field.path))
                .default(field.default.to_string())
                .interact_text()?;
            Ok(toml_edit::Value::from(s))
        }
    }
}

/// Fetch the model catalog from the active provider (async path —
/// called when a tokio runtime is available).
async fn fetch_models_from_active_provider() -> Result<Vec<crate::provider::catalog::ModelInfo>> {
    let dir = vulcan_home();
    let config = crate::config::Config::load_from_dir(&dir).unwrap_or_default();
    let provider = config.active_provider_config();
    let api_key = config.api_key().unwrap_or_default();
    crate::cli_model::fetch_catalog(provider, &api_key).await
}

/// Blocking fallback: spin up a temporary tokio runtime to fetch
/// the catalog. Used when `cli_config` is called outside an
/// existing async context (e.g. `vulcan config set` from main).
fn fetch_models_from_active_provider_blocking() -> Result<Vec<crate::provider::catalog::ModelInfo>>
{
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(fetch_models_from_active_provider())
}

/// Show a confirmation summary, then write.
fn confirm_and_write(field: &ConfigField, val: toml_edit::Value) -> Result<()> {
    let theme = dialoguer_theme();
    let display = val.to_string();
    let confirmed = Confirm::with_theme(&theme)
        .with_prompt(format!("Write {} = {} to config?", field.path, display))
        .default(true)
        .interact()?;
    if !confirmed {
        println!("Aborted — nothing written.");
        return Ok(());
    }
    let path = file_path_for(field.file);
    let mut doc = read_document(&path)?;
    apply_set(&mut doc, field.path, val)?;
    write_document(&path, &doc)?;
    println!("✓ set {} in {}", field.path, path.display());
    Ok(())
}

fn dialoguer_theme() -> dialoguer::theme::ColorfulTheme {
    dialoguer::theme::ColorfulTheme::default()
}

/// Short kind label for picker items.
fn fmt_kind_short(k: &FieldKind) -> String {
    match k {
        FieldKind::Bool => "bool".into(),
        FieldKind::Int { min, max } => match (min, max) {
            (Some(lo), Some(hi)) => format!("int [{lo}..{hi}]"),
            (Some(lo), None) => format!("int [≥{lo}]"),
            (None, Some(hi)) => format!("int [≤{hi}]"),
            (None, None) => "int".into(),
        },
        FieldKind::Float { min, max } => match (min, max) {
            (Some(lo), Some(hi)) => format!("float [{lo}..{hi}]"),
            (Some(lo), None) => format!("float [≥{lo}]"),
            (None, Some(hi)) => format!("float [≤{hi}]"),
            (None, None) => "float".into(),
        },
        FieldKind::Enum { variants } => format!("enum[{}]", variants.join("|")),
        FieldKind::String { secret: true } => "🔒 secret".into(),
        FieldKind::String { secret: false } => "string".into(),
        FieldKind::Path => "path".into(),
        FieldKind::Model => "model".into(),
        FieldKind::EmbeddingModel => "embedding".into(),
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
    println!("{} set {} in {}", "✓".green(), key, path.display());
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
        FieldKind::String { .. }
        | FieldKind::Path
        | FieldKind::Model
        | FieldKind::EmbeddingModel => Ok(raw.into()),
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
    // Colored header row
    println!(
        "{} {} {} {} {} {}",
        "key".bold().white().on_blue(),
        "kind".bold().white().on_blue(),
        "current".bold().white().on_blue(),
        "default".bold().white().on_blue(),
        "file".bold().white().on_blue(),
        "help".bold().white().on_blue(),
    );
    for f in all() {
        let kind_str = colored_kind(&f.kind);
        let default_str = f.default.dimmed().to_string();
        let file_label = format_file(f.file);
        let file_str = file_label.green().to_string();

        // Look up the actual current value from config files
        let current_str = match lookup_in_file(f, f.path) {
            Ok(Some(v)) => format_value(&v, f, false).green().to_string(),
            _ => {
                // Not explicitly set — show "(default)" in yellow
                "(default)".yellow().to_string()
            }
        };

        println!(
            "{:<48} {:<14} {:<20} {:<20} {:<14} {}",
            f.path, kind_str, current_str, default_str, file_str, f.help
        );
    }
    Ok(())
}

/// Colored kind badge for `list`.
fn colored_kind(k: &FieldKind) -> String {
    match k {
        FieldKind::Bool => "bool".cyan().to_string(),
        FieldKind::Int { .. } => format_kind(k).blue().to_string(),
        FieldKind::Float { .. } => format_kind(k).blue().to_string(),
        FieldKind::Enum { .. } => format_kind(k).yellow().to_string(),
        FieldKind::String { secret: true } => "🔒 secret".red().to_string(),
        FieldKind::String { secret: false } => "string".green().to_string(),
        FieldKind::Path => "path".magenta().to_string(),
        FieldKind::Model => "model".cyan().to_string(),
        FieldKind::EmbeddingModel => "embedding".cyan().to_string(),
    }
}

fn get(key: &str, reveal: bool) -> Result<()> {
    let field = lookup(key).ok_or_else(|| {
        anyhow!("unknown config field `{key}`. `vulcan config list` for the catalog.")
    })?;
    let raw = lookup_in_file(field, key)?;
    match raw {
        None => {
            // Unset — show dim "(default: <val> applies)"
            println!("{} {}", "(default)".dimmed(), field.default.yellow());
        }
        Some(v) if is_secret_not_revealed(field, reveal) => {
            // Secret redacted
            println!("{}", "***redacted*** (pass --reveal to show)".red());
        }
        Some(v) => {
            let display = format_value(&v, field, reveal);
            // Explicitly set — show in green with a checkmark
            println!("✓ {}", display.green());
        }
    }
    Ok(())
}

/// True when the value should be redacted.
fn is_secret_not_revealed(field: &ConfigField, reveal: bool) -> bool {
    matches!(&field.kind, FieldKind::String { secret: true } if !reveal)
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
        match raw {
            None => {
                println!("{} {} {}", f.path, "=".dimmed(), f.default.yellow());
            }
            Some(v) if is_secret_not_revealed(f, reveal) => {
                println!("{} = {}", f.path, "***redacted***".red());
            }
            Some(v) => {
                let display = format_value(&v, f, reveal);
                println!("✓ {} = {}", f.path, display.green());
            }
        }
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
        FieldKind::Model => "model".into(),
        FieldKind::EmbeddingModel => "embedding".into(),
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
mod yyc217_tests {
    use super::*;

    #[test]
    fn resolve_section_path_routes_known_sections() {
        let home = std::path::Path::new("/tmp/x");
        assert_eq!(
            resolve_section_path(home, Some("keybinds")),
            home.join("keybinds.toml"),
        );
        assert_eq!(
            resolve_section_path(home, Some("providers")),
            home.join("providers.toml"),
        );
        assert_eq!(
            resolve_section_path(home, Some("provider")),
            home.join("providers.toml"),
        );
    }

    #[test]
    fn resolve_section_path_defaults_unknown_to_config_toml() {
        let home = std::path::Path::new("/tmp/x");
        assert_eq!(resolve_section_path(home, None), home.join("config.toml"),);
        assert_eq!(
            resolve_section_path(home, Some("scheduler")),
            home.join("config.toml"),
        );
        assert_eq!(
            resolve_section_path(home, Some("totally-unknown")),
            home.join("config.toml"),
        );
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
