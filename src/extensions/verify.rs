//! YYC-233 (YYC-166 PR-5): manifest verification — version
//! compatibility + optional checksum check.
//!
//! `load_from_store` calls `verify_compatible` for every
//! discovered manifest before activating it. Incompatible
//! manifests are marked `Broken` with a clear reason, mirroring
//! the parse-error path so dashboards have one shape to render.

use sha2::{Digest, Sha256};
use std::path::Path;
use thiserror::Error;

use super::manifest::ExtensionManifest;

#[derive(Debug, Error)]
pub enum VerificationError {
    #[error("extension `{id}` requires Vulcan ≥ {required}; running {running}")]
    VersionTooNew {
        id: String,
        required: String,
        running: String,
    },
    #[error(
        "extension `{id}` payload digest mismatch: manifest claims {expected}, computed {actual}"
    )]
    ChecksumMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    #[error("io error reading payload at {path}: {source}")]
    PayloadIo {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Verify a manifest is compatible with the running Vulcan
/// version. Non-numeric `min_vulcan_version` strings (or absent
/// fields) pass — the verification engine is intentionally
/// lenient on shape and strict only on the well-formed
/// "manifest demands a version newer than the runtime" case.
pub fn verify_compatible(
    manifest: &ExtensionManifest,
    running_version: &str,
) -> Result<(), VerificationError> {
    let required = match manifest.min_vulcan_version.as_deref() {
        Some(v) => v,
        None => return Ok(()),
    };
    let req = match parse_version_triple(required) {
        Some(v) => v,
        None => return Ok(()),
    };
    let cur = match parse_version_triple(running_version) {
        Some(v) => v,
        None => return Ok(()),
    };
    if cur < req {
        return Err(VerificationError::VersionTooNew {
            id: manifest.id.clone(),
            required: required.to_string(),
            running: running_version.to_string(),
        });
    }
    Ok(())
}

/// When the manifest declares `checksum = "sha256:<hex>"` and the
/// payload file exists at `payload_path`, compute SHA-256 over
/// it and compare. Returns `Ok(true)` when verified, `Ok(false)`
/// when no checksum or no payload (nothing to check), or `Err`
/// on mismatch.
pub fn verify_checksum_optional(
    manifest: &ExtensionManifest,
    payload_path: &Path,
) -> Result<bool, VerificationError> {
    let expected = match manifest.checksum.as_deref() {
        Some(s) => s.trim(),
        None => return Ok(false),
    };
    if !payload_path.exists() {
        return Ok(false);
    }
    let bytes = std::fs::read(payload_path).map_err(|e| VerificationError::PayloadIo {
        path: payload_path.display().to_string(),
        source: e,
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut actual_hex = String::with_capacity(7 + digest.len() * 2);
    actual_hex.push_str("sha256:");
    for b in digest.iter() {
        actual_hex.push_str(&format!("{:02x}", b));
    }
    if expected.eq_ignore_ascii_case(&actual_hex) {
        Ok(true)
    } else {
        Err(VerificationError::ChecksumMismatch {
            id: manifest.id.clone(),
            expected: expected.to_string(),
            actual: actual_hex,
        })
    }
}

fn parse_version_triple(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim().trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts
        .next()
        .map(|p| p.split(|c: char| !c.is_ascii_digit()).next().unwrap_or(""))
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::manifest::EntryKind;

    fn manifest_with(min: Option<&str>, checksum: Option<&str>) -> ExtensionManifest {
        ExtensionManifest {
            id: "ext".into(),
            name: "Ext".into(),
            version: "0.1.0".into(),
            entry: EntryKind::Builtin,
            capabilities: Vec::new(),
            permissions: None,
            checksum: checksum.map(String::from),
            min_vulcan_version: min.map(String::from),
            description: None,
        }
    }

    #[test]
    fn missing_min_version_always_passes() {
        let m = manifest_with(None, None);
        verify_compatible(&m, "0.1.0").unwrap();
    }

    #[test]
    fn matching_version_passes() {
        let m = manifest_with(Some("0.5.0"), None);
        verify_compatible(&m, "0.5.0").unwrap();
        verify_compatible(&m, "0.6.1").unwrap();
        verify_compatible(&m, "1.0.0").unwrap();
    }

    #[test]
    fn newer_required_version_fails() {
        let m = manifest_with(Some("0.6.0"), None);
        let err = verify_compatible(&m, "0.5.0").unwrap_err();
        match err {
            VerificationError::VersionTooNew { id, .. } => assert_eq!(id, "ext"),
            other => panic!("expected VersionTooNew, got {other:?}"),
        }
    }

    #[test]
    fn malformed_versions_pass_leniently() {
        let m = manifest_with(Some("not-semver"), None);
        verify_compatible(&m, "0.1.0").unwrap();
        let m = manifest_with(Some("0.1"), None);
        verify_compatible(&m, "weird").unwrap();
    }

    #[test]
    fn v_prefix_is_tolerated() {
        let m = manifest_with(Some("v1.0.0"), None);
        verify_compatible(&m, "v1.2.3").unwrap();
    }

    #[test]
    fn checksum_no_op_when_field_absent() {
        let m = manifest_with(None, None);
        let dir = tempfile::tempdir().unwrap();
        assert!(!verify_checksum_optional(&m, &dir.path().join("missing")).unwrap());
    }

    #[test]
    fn checksum_no_op_when_payload_absent() {
        let m = manifest_with(None, Some("sha256:00"));
        let dir = tempfile::tempdir().unwrap();
        assert!(!verify_checksum_optional(&m, &dir.path().join("missing.bin")).unwrap());
    }

    #[test]
    fn checksum_passes_when_payload_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, b"hello world").unwrap();
        // sha256("hello world")
        let expected = "sha256:b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let m = manifest_with(None, Some(expected));
        assert!(verify_checksum_optional(&m, &path).unwrap());
    }

    #[test]
    fn checksum_fails_on_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("payload.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let m = manifest_with(None, Some("sha256:deadbeef"));
        let err = verify_checksum_optional(&m, &path).unwrap_err();
        assert!(matches!(err, VerificationError::ChecksumMismatch { .. }));
    }
}
