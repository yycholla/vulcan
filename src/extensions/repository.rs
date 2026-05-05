//! Extension repository index parsing, discovery filters, and update planning.
//!
//! This module is deliberately offline-only. Repository fetch, install, and
//! signature verification are separate trust-sensitive steps; this slice owns
//! the deterministic data model that those callers can use with cached index
//! files and fixture tests.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("malformed repository JSON: {0}")]
    MalformedJson(#[from] serde_json::Error),
    #[error("malformed repository TOML: {0}")]
    MalformedToml(#[from] toml::de::Error),
    #[error("repository index schema_version must be 1")]
    UnsupportedSchema,
    #[error("repository extension `{id}` is missing checksum")]
    MissingChecksum { id: String },
    #[error(
        "repository extension `{id}` checksum mismatch: expected {expected}, computed {actual}"
    )]
    ChecksumMismatch {
        id: String,
        expected: String,
        actual: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryIndex {
    pub schema_version: u32,
    pub repository: RepositoryMetadata,
    #[serde(default)]
    pub extensions: Vec<RepositoryExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryConfig {
    pub id: String,
    pub cache_path: PathBuf,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub trusted_keys: Vec<String>,
}

#[derive(Debug)]
pub struct RepositoryCacheSnapshot {
    pub indexes: Vec<RepositoryIndex>,
    pub failures: Vec<RepositoryCacheFailure>,
}

impl RepositoryCacheSnapshot {
    pub fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryCacheFailure {
    pub repository_id: String,
    pub path: PathBuf,
    pub reason: String,
}

pub fn load_cached_indexes(configs: &[RepositoryConfig]) -> RepositoryCacheSnapshot {
    let mut indexes = Vec::new();
    let mut failures = Vec::new();
    for config in configs.iter().filter(|config| config.enabled) {
        match load_cached_index(&config.cache_path) {
            Ok(index) => indexes.push(index),
            Err(reason) => failures.push(RepositoryCacheFailure {
                repository_id: config.id.clone(),
                path: config.cache_path.clone(),
                reason,
            }),
        }
    }
    indexes.sort_by(|a, b| a.repository.id.cmp(&b.repository.id));
    failures.sort_by(|a, b| a.repository_id.cmp(&b.repository_id));
    RepositoryCacheSnapshot { indexes, failures }
}

fn load_cached_index(path: &Path) -> Result<RepositoryIndex, String> {
    let raw = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => RepositoryIndex::from_toml_str(&raw).map_err(|err| err.to_string()),
        _ => RepositoryIndex::from_json_str(&raw).map_err(|err| err.to_string()),
    }
}

impl RepositoryIndex {
    pub fn from_json_str(raw: &str) -> Result<Self, RepositoryError> {
        let index: Self = serde_json::from_str(raw)?;
        index.validate()?;
        Ok(index)
    }

    pub fn from_toml_str(raw: &str) -> Result<Self, RepositoryError> {
        let index: Self = toml::from_str(raw)?;
        index.validate()?;
        Ok(index)
    }

    pub fn search<'a>(&'a self, query: &RepositorySearch<'_>) -> Vec<RepositorySearchResult<'a>> {
        let mut out: Vec<_> = self
            .extensions
            .iter()
            .filter(|record| query.include_yanked || !record.yanked)
            .filter(|record| query.matches_text(record))
            .filter(|record| query.matches_capability(record))
            .filter(|record| query.matches_category(record))
            .filter_map(|record| {
                let compatibility =
                    RepositoryCompatibility::evaluate(record, query.platform, query.vulcan_version);
                if query.include_incompatible || compatibility.is_compatible() {
                    Some(RepositorySearchResult {
                        record,
                        compatibility,
                    })
                } else {
                    None
                }
            })
            .collect();
        out.sort_by(|a, b| {
            a.record
                .id
                .cmp(&b.record.id)
                .then_with(|| compare_versions_desc(&a.record.version, &b.record.version))
        });
        out
    }

    pub fn plan_updates<'a>(
        &'a self,
        installed: &[InstalledExtension],
        request: &UpdatePlanRequest<'_>,
    ) -> Vec<UpdatePlanEntry<'a>> {
        let mut out: Vec<_> = installed
            .iter()
            .map(|installed| {
                let candidates: Vec<_> = self
                    .extensions
                    .iter()
                    .filter(|record| record.id == installed.id)
                    .filter(|record| request.include_yanked || !record.yanked)
                    .collect();
                let Some(best) = newest_record(candidates) else {
                    return UpdatePlanEntry {
                        installed: installed.clone(),
                        candidate: None,
                        status: UpdateStatus::Current,
                        reason: Some("no repository record".to_string()),
                    };
                };
                let compatibility = RepositoryCompatibility::evaluate(
                    best,
                    request.platform,
                    request.vulcan_version,
                );
                if !compatibility.is_compatible() {
                    return UpdatePlanEntry {
                        installed: installed.clone(),
                        candidate: Some(best),
                        status: UpdateStatus::Blocked,
                        reason: Some(compatibility.reason()),
                    };
                }
                if compare_versions(&best.version, &installed.version) <= std::cmp::Ordering::Equal
                {
                    return UpdatePlanEntry {
                        installed: installed.clone(),
                        candidate: Some(best),
                        status: UpdateStatus::Current,
                        reason: None,
                    };
                }
                let risk = best.risk_reason();
                UpdatePlanEntry {
                    installed: installed.clone(),
                    candidate: Some(best),
                    status: if risk.is_some() {
                        UpdateStatus::Risky
                    } else {
                        UpdateStatus::Available
                    },
                    reason: risk,
                }
            })
            .collect();
        out.sort_by(|a, b| a.installed.id.cmp(&b.installed.id));
        out
    }

    fn validate(&self) -> Result<(), RepositoryError> {
        if self.schema_version != 1 {
            return Err(RepositoryError::UnsupportedSchema);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryMetadata {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub trusted_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryExtension {
    pub id: String,
    pub name: String,
    pub version: String,
    pub download_url: String,
    pub checksum: String,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub min_vulcan_version: Option<String>,
    #[serde(default)]
    pub max_vulcan_version: Option<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub publisher: PublisherMetadata,
    #[serde(default)]
    pub trust: RepositoryTrustMetadata,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub yanked: bool,
}

impl RepositoryExtension {
    pub fn verify_download_bytes(&self, bytes: &[u8]) -> Result<(), RepositoryError> {
        let expected = self.checksum.trim();
        if expected.is_empty() {
            return Err(RepositoryError::MissingChecksum {
                id: self.id.clone(),
            });
        }
        let actual = sha256_digest(bytes);
        if expected.eq_ignore_ascii_case(&actual) {
            Ok(())
        } else {
            Err(RepositoryError::ChecksumMismatch {
                id: self.id.clone(),
                expected: expected.to_string(),
                actual,
            })
        }
    }

    fn risk_reason(&self) -> Option<String> {
        if self.checksum.trim().is_empty() {
            return Some("missing checksum".to_string());
        }
        if !self.trust.verified_publisher {
            return Some("publisher is not verified".to_string());
        }
        if self.trust.scan_status.as_deref() != Some("passed") {
            return Some("scan status is not passed".to_string());
        }
        None
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherMetadata {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryTrustMetadata {
    #[serde(default)]
    pub verified_publisher: bool,
    #[serde(default)]
    pub scan_status: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RepositorySearch<'a> {
    pub query: Option<&'a str>,
    pub platform: &'a str,
    pub vulcan_version: &'a str,
    pub capability: Option<&'a str>,
    pub category: Option<&'a str>,
    pub include_incompatible: bool,
    pub include_yanked: bool,
}

impl<'a> RepositorySearch<'a> {
    pub fn current() -> Self {
        Self {
            platform: current_platform(),
            vulcan_version: env!("CARGO_PKG_VERSION"),
            ..Self::default()
        }
    }

    fn matches_text(&self, record: &RepositoryExtension) -> bool {
        let Some(query) = self.query.map(str::trim).filter(|q| !q.is_empty()) else {
            return true;
        };
        let query = query.to_ascii_lowercase();
        record.id.to_ascii_lowercase().contains(&query)
            || record.name.to_ascii_lowercase().contains(&query)
            || record
                .description
                .as_deref()
                .is_some_and(|desc| desc.to_ascii_lowercase().contains(&query))
    }

    fn matches_capability(&self, record: &RepositoryExtension) -> bool {
        self.capability.is_none_or(|capability| {
            record
                .capabilities
                .iter()
                .any(|candidate| candidate == capability)
        })
    }

    fn matches_category(&self, record: &RepositoryExtension) -> bool {
        self.category.is_none_or(|category| {
            record
                .categories
                .iter()
                .any(|candidate| candidate == category)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySearchResult<'a> {
    pub record: &'a RepositoryExtension,
    pub compatibility: RepositoryCompatibility,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepositoryCompatibility {
    Compatible,
    Incompatible { reasons: Vec<String> },
}

impl RepositoryCompatibility {
    fn evaluate(record: &RepositoryExtension, platform: &str, vulcan_version: &str) -> Self {
        let mut reasons = Vec::new();
        if !record.platforms.is_empty()
            && !record
                .platforms
                .iter()
                .any(|p| p == "any" || p == platform || p == std::env::consts::OS)
        {
            reasons.push(format!("platform `{platform}` is not supported"));
        }
        if let Some(min) = record.min_vulcan_version.as_deref()
            && compare_versions(vulcan_version, min) == std::cmp::Ordering::Less
        {
            reasons.push(format!("requires Vulcan >= {min}"));
        }
        if let Some(max) = record.max_vulcan_version.as_deref()
            && compare_versions(vulcan_version, max) == std::cmp::Ordering::Greater
        {
            reasons.push(format!("requires Vulcan <= {max}"));
        }
        if reasons.is_empty() {
            Self::Compatible
        } else {
            Self::Incompatible { reasons }
        }
    }

    pub fn is_compatible(&self) -> bool {
        matches!(self, Self::Compatible)
    }

    fn reason(&self) -> String {
        match self {
            Self::Compatible => String::new(),
            Self::Incompatible { reasons } => reasons.join("; "),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledExtension {
    pub id: String,
    pub version: String,
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdatePlanRequest<'a> {
    pub platform: &'a str,
    pub vulcan_version: &'a str,
    pub include_yanked: bool,
}

impl<'a> UpdatePlanRequest<'a> {
    pub fn current() -> Self {
        Self {
            platform: current_platform(),
            vulcan_version: env!("CARGO_PKG_VERSION"),
            include_yanked: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePlanEntry<'a> {
    pub installed: InstalledExtension,
    pub candidate: Option<&'a RepositoryExtension>,
    pub status: UpdateStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    Current,
    Available,
    Blocked,
    Risky,
}

pub fn current_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        (os, _) => os,
    }
}

fn newest_record(records: Vec<&RepositoryExtension>) -> Option<&RepositoryExtension> {
    records
        .into_iter()
        .max_by(|a, b| compare_versions(&a.version, &b.version))
}

fn compare_versions_desc(a: &str, b: &str) -> std::cmp::Ordering {
    compare_versions(b, a)
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    parse_version_key(a).cmp(&parse_version_key(b))
}

fn parse_version_key(raw: &str) -> (u64, u64, u64, String) {
    let trimmed = raw.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parse_version_component(parts.next());
    let minor = parse_version_component(parts.next());
    let patch = parse_version_component(parts.next());
    (major, minor, patch, trimmed.to_string())
}

fn parse_version_component(part: Option<&str>) -> u64 {
    part.and_then(|p| {
        p.split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|digits| digits.parse().ok())
    })
    .unwrap_or(0)
}

fn sha256_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest.iter() {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"
{
  "schema_version": 1,
  "repository": {
    "id": "first-party",
    "name": "First-party extensions",
    "url": "https://extensions.example.test/index.json",
    "trusted_keys": ["key-1"]
  },
  "extensions": [
    {
      "id": "ci-summary",
      "name": "CI Summary",
      "version": "0.2.0",
      "description": "Summarizes CI checks",
      "download_url": "https://extensions.example.test/ci-summary-0.2.0.vpk",
      "checksum": "sha256:efac6021de173f3684adc4e7a9dd56f41aed62a76b97254a2846ad97d981ef34",
      "min_vulcan_version": "0.1.0",
      "platforms": ["linux-x86_64", "macos-aarch64"],
      "capabilities": ["tool_provider"],
      "categories": ["ci"],
      "publisher": { "id": "vulcan", "name": "Vulcan" },
      "trust": { "verified_publisher": true, "scan_status": "passed" }
    },
    {
      "id": "linux-only",
      "name": "Linux Only",
      "version": "1.0.0",
      "download_url": "https://extensions.example.test/linux-only.vpk",
      "checksum": "sha256:00",
      "platforms": ["linux-x86_64"],
      "capabilities": ["hook_handler"],
      "categories": ["ops"],
      "publisher": { "id": "vulcan", "name": "Vulcan" },
      "trust": { "verified_publisher": true, "scan_status": "passed" }
    },
    {
      "id": "ci-summary",
      "name": "CI Summary",
      "version": "0.3.0",
      "download_url": "https://extensions.example.test/ci-summary-0.3.0.vpk",
      "checksum": "",
      "platforms": ["any"],
      "capabilities": ["tool_provider"],
      "categories": ["ci"]
    },
    {
      "id": "risky-rag",
      "name": "Risky RAG",
      "version": "0.1.0",
      "download_url": "https://extensions.example.test/risky-rag.vpk",
      "checksum": "sha256:00",
      "min_vulcan_version": "99.0.0",
      "platforms": ["any"],
      "capabilities": ["memory_backend"],
      "categories": ["rag"]
    }
  ]
}
"#;

    #[test]
    fn parses_repository_index_from_json() {
        let index = RepositoryIndex::from_json_str(INDEX).unwrap();
        assert_eq!(index.schema_version, 1);
        assert_eq!(index.repository.id, "first-party");
        assert_eq!(index.extensions.len(), 4);
    }

    #[test]
    fn parses_repository_index_from_toml() {
        let index = RepositoryIndex::from_toml_str(
            r#"
schema_version = 1

[repository]
id = "local"
name = "Local fixture"

[[extensions]]
id = "logger"
name = "Logger"
version = "0.1.0"
download_url = "file:///tmp/logger.vpk"
checksum = "sha256:00"
platforms = ["any"]
"#,
        )
        .unwrap();

        assert_eq!(index.extensions[0].id, "logger");
    }

    #[test]
    fn search_filters_by_query_capability_category_and_compatibility() {
        let index = RepositoryIndex::from_json_str(INDEX).unwrap();
        let results = index.search(&RepositorySearch {
            query: Some("ci"),
            platform: "linux-x86_64",
            vulcan_version: "0.1.0",
            capability: Some("tool_provider"),
            category: Some("ci"),
            include_incompatible: false,
            include_yanked: false,
        });

        let versions: Vec<_> = results.iter().map(|r| r.record.version.as_str()).collect();
        assert_eq!(versions, vec!["0.3.0", "0.2.0"]);
        assert!(results.iter().all(|r| r.compatibility.is_compatible()));
    }

    #[test]
    fn incompatible_records_are_hidden_unless_requested() {
        let index = RepositoryIndex::from_json_str(INDEX).unwrap();
        let hidden = index.search(&RepositorySearch {
            platform: "macos-aarch64",
            vulcan_version: "0.1.0",
            ..RepositorySearch::current()
        });
        assert!(!hidden.iter().any(|r| r.record.id == "risky-rag"));

        let visible = index.search(&RepositorySearch {
            platform: "macos-aarch64",
            vulcan_version: "0.1.0",
            include_incompatible: true,
            ..RepositorySearch::current()
        });
        let rag = visible
            .iter()
            .find(|r| r.record.id == "risky-rag")
            .expect("incompatible result is present");
        assert!(matches!(
            rag.compatibility,
            RepositoryCompatibility::Incompatible { .. }
        ));
    }

    #[test]
    fn update_plan_reports_current_available_blocked_and_risky() {
        let index = RepositoryIndex::from_json_str(INDEX).unwrap();
        let installed = vec![
            InstalledExtension {
                id: "ci-summary".to_string(),
                version: "0.1.0".to_string(),
                checksum: None,
            },
            InstalledExtension {
                id: "linux-only".to_string(),
                version: "1.0.0".to_string(),
                checksum: None,
            },
            InstalledExtension {
                id: "risky-rag".to_string(),
                version: "0.0.1".to_string(),
                checksum: None,
            },
            InstalledExtension {
                id: "not-in-repo".to_string(),
                version: "1.0.0".to_string(),
                checksum: None,
            },
        ];

        let plan = index.plan_updates(
            &installed,
            &UpdatePlanRequest {
                platform: "linux-x86_64",
                vulcan_version: "0.1.0",
                include_yanked: false,
            },
        );

        let status = |id: &str| {
            plan.iter()
                .find(|entry| entry.installed.id == id)
                .map(|entry| entry.status)
                .unwrap()
        };
        assert_eq!(status("ci-summary"), UpdateStatus::Risky);
        assert_eq!(status("linux-only"), UpdateStatus::Current);
        assert_eq!(status("risky-rag"), UpdateStatus::Blocked);
        assert_eq!(status("not-in-repo"), UpdateStatus::Current);
    }

    #[test]
    fn update_plan_reports_available_for_verified_newer_candidate() {
        let index = RepositoryIndex::from_json_str(INDEX).unwrap();
        let installed = vec![InstalledExtension {
            id: "linux-only".to_string(),
            version: "0.9.0".to_string(),
            checksum: None,
        }];

        let plan = index.plan_updates(
            &installed,
            &UpdatePlanRequest {
                platform: "linux-x86_64",
                vulcan_version: "0.1.0",
                include_yanked: false,
            },
        );

        assert_eq!(plan[0].status, UpdateStatus::Available);
    }

    #[test]
    fn verifies_download_bytes_before_install() {
        let index = RepositoryIndex::from_json_str(INDEX).unwrap();
        let record = index
            .extensions
            .iter()
            .find(|record| record.id == "ci-summary" && record.version == "0.2.0")
            .unwrap();

        record.verify_download_bytes(b"downloaded payload").unwrap();
        let err = record
            .verify_download_bytes(b"tampered payload")
            .unwrap_err();
        assert!(matches!(err, RepositoryError::ChecksumMismatch { .. }));
    }

    #[test]
    fn cache_snapshot_keeps_valid_indexes_when_one_repository_fails() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        let bad = dir.path().join("bad.json");
        std::fs::write(&good, INDEX).unwrap();
        std::fs::write(&bad, "not json").unwrap();

        let snapshot = load_cached_indexes(&[
            RepositoryConfig {
                id: "good".to_string(),
                cache_path: good,
                enabled: true,
                trusted_keys: Vec::new(),
            },
            RepositoryConfig {
                id: "bad".to_string(),
                cache_path: bad,
                enabled: true,
                trusted_keys: Vec::new(),
            },
            RepositoryConfig {
                id: "disabled".to_string(),
                cache_path: dir.path().join("missing.json"),
                enabled: false,
                trusted_keys: Vec::new(),
            },
        ]);

        assert_eq!(snapshot.indexes.len(), 1);
        assert_eq!(snapshot.indexes[0].repository.id, "first-party");
        assert_eq!(snapshot.failures.len(), 1);
        assert_eq!(snapshot.failures[0].repository_id, "bad");
    }

    #[test]
    fn rejects_unsupported_schema() {
        let err = RepositoryIndex::from_json_str(
            r#"{ "schema_version": 2, "repository": { "id": "x", "name": "X" } }"#,
        )
        .unwrap_err();
        assert!(matches!(err, RepositoryError::UnsupportedSchema));
    }
}
