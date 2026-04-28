//! YYC-230 (YYC-166 PR-2): on-disk extension store discovery.
//!
//! Walks `~/.vulcan/extensions/<id>/extension.toml` and returns
//! a list of `DiscoveredExtension`. One bad manifest does not
//! break discovery — its `parse_error` field carries the
//! diagnostic so the caller can surface a per-id failure
//! without losing the rest of the install set.
//!
//! No mutation in this PR. Install / uninstall / enable /
//! disable land in subsequent YYC-166 children.

use std::path::{Path, PathBuf};

use super::manifest::{ExtensionManifest, ManifestError};

/// One entry surfaced by [`discover`]. Either the manifest
/// parsed cleanly (`manifest = Some`) or the file parsed wrong
/// (`parse_error = Some`). The directory id is the install dir
/// name regardless.
#[derive(Debug)]
pub struct DiscoveredExtension {
    /// Install directory name (the `<id>` segment under
    /// `extensions/`). Reflects the on-disk path even when the
    /// manifest is malformed.
    pub dir_id: String,
    /// Absolute path to the install directory.
    pub dir: PathBuf,
    /// Parsed manifest. `None` when the file is missing or
    /// invalid.
    pub manifest: Option<ExtensionManifest>,
    /// Parse failure reason. `None` on success or when the file
    /// is missing entirely.
    pub parse_error: Option<ManifestError>,
}

/// Walk `<home>/extensions/` and return one entry per
/// subdirectory. A subdirectory with no `extension.toml` is
/// silently skipped — only outright manifest parse failures
/// surface as `DiscoveredExtension { manifest: None,
/// parse_error: Some(_) }`.
///
/// `home` is the explicit Vulcan home — production code passes
/// `crate::config::vulcan_home()`; tests pass a temp dir.
pub fn discover(home: &Path) -> Vec<DiscoveredExtension> {
    let mut out = Vec::new();
    let extensions_root = home.join("extensions");
    if !extensions_root.is_dir() {
        return out;
    }
    let entries = match std::fs::read_dir(&extensions_root) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let dir_id = match dir.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        let manifest_path = dir.join("extension.toml");
        if !manifest_path.is_file() {
            // No manifest — silently skip. The user might be
            // mid-install or staging a non-extension folder.
            continue;
        }
        match std::fs::read_to_string(&manifest_path) {
            Ok(raw) => match ExtensionManifest::from_toml_str(&raw) {
                Ok(manifest) => out.push(DiscoveredExtension {
                    dir_id,
                    dir,
                    manifest: Some(manifest),
                    parse_error: None,
                }),
                Err(err) => out.push(DiscoveredExtension {
                    dir_id,
                    dir,
                    manifest: None,
                    parse_error: Some(err),
                }),
            },
            Err(_) => continue,
        }
    }
    // Deterministic order so callers / tests don't depend on
    // OS-level readdir ordering.
    out.sort_by(|a, b| a.dir_id.cmp(&b.dir_id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: PathBuf, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn empty_store_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let found = discover(dir.path());
        assert!(found.is_empty());
    }

    #[test]
    fn missing_extensions_dir_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let nonexistent = dir.path().join("not-real-home");
        let found = discover(&nonexistent);
        assert!(found.is_empty());
    }

    #[test]
    fn discovers_valid_manifest() {
        let dir = tempdir().unwrap();
        write(
            dir.path().join("extensions/lint-helper/extension.toml"),
            r#"
id = "lint-helper"
name = "Lint Helper"
version = "0.1.0"

[entry]
kind = "builtin"
"#,
        );
        let found = discover(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir_id, "lint-helper");
        let manifest = found[0].manifest.as_ref().unwrap();
        assert_eq!(manifest.id, "lint-helper");
        assert!(found[0].parse_error.is_none());
    }

    #[test]
    fn surfaces_parse_error_without_breaking_other_entries() {
        let dir = tempdir().unwrap();
        write(
            dir.path().join("extensions/good/extension.toml"),
            r#"
id = "good"
name = "Good"
version = "0.1.0"

[entry]
kind = "builtin"
"#,
        );
        write(
            dir.path().join("extensions/bad/extension.toml"),
            "completely[broken",
        );
        let found = discover(dir.path());
        assert_eq!(found.len(), 2);
        // Sorted alphabetically.
        assert_eq!(found[0].dir_id, "bad");
        assert!(found[0].manifest.is_none());
        assert!(found[0].parse_error.is_some());
        assert_eq!(found[1].dir_id, "good");
        assert!(found[1].manifest.is_some());
    }

    #[test]
    fn skips_directories_without_a_manifest() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("extensions/just-a-dir")).unwrap();
        write(
            dir.path().join("extensions/with-manifest/extension.toml"),
            r#"
id = "with-manifest"
name = "X"
version = "0.1.0"

[entry]
kind = "builtin"
"#,
        );
        let found = discover(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].dir_id, "with-manifest");
    }

    #[test]
    fn discovery_sort_order_is_deterministic() {
        let dir = tempdir().unwrap();
        for id in ["zulu", "alpha", "mike"] {
            write(
                dir.path().join(format!("extensions/{id}/extension.toml")),
                &format!(
                    r#"
id = "{id}"
name = "{id}"
version = "0.1.0"

[entry]
kind = "builtin"
"#
                ),
            );
        }
        let found = discover(dir.path());
        let ids: Vec<String> = found.into_iter().map(|d| d.dir_id).collect();
        assert_eq!(
            ids,
            vec!["alpha".to_string(), "mike".to_string(), "zulu".to_string()]
        );
    }
}
