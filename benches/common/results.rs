//! Bench-result schema (v1) and JSON writer.
//!
//! Each bench/binary appends its `BenchGroup`s to a shared file at
//! `target/bench-results.json`. The writer reads-modify-writes the file so
//! independent benches can run in any order. `scripts/bench-diff.py`
//! consumes the resulting document.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Document {
    pub schema: u32,
    pub git_sha: String,
    pub timestamp: String,
    pub host: Host,
    pub groups: BTreeMap<String, Vec<Measurement>>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Host {
    pub cpu: String,
    pub cores: usize,
    pub os: String,
}

/// One measurement within a group. Optional fields let one schema serve
/// micro benches (ns/iter) and the soak binary (p50/p99 ms, RSS).
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Measurement {
    pub name: String,
    /// Wall time per iteration, ns.
    pub ns_per_iter: Option<f64>,
    /// Allocations per iteration.
    pub allocs_per_iter: Option<u64>,
    /// Soak: turn number this sample was taken at.
    pub turn: Option<usize>,
    /// Soak: 50th-percentile per-turn latency, ms.
    pub p50_ms: Option<f64>,
    /// Soak: 99th-percentile per-turn latency, ms.
    pub p99_ms: Option<f64>,
    /// Soak: resident set size, kB.
    pub rss_kb: Option<u64>,
    /// Soak: cumulative allocs since start (dhat).
    pub allocs_total: Option<u64>,
    /// Serialized message-history payload, bytes.
    pub payload_bytes: Option<u64>,
}

/// Read the existing document if any, merge in `additions`, and write back.
pub fn append(path: &Path, additions: BTreeMap<String, Vec<Measurement>>) -> std::io::Result<()> {
    let mut doc = if path.exists() {
        let data = fs::read(path)?;
        serde_json::from_slice::<Document>(&data).unwrap_or_else(|_| fresh_doc())
    } else {
        fresh_doc()
    };
    for (group, measurements) in additions {
        doc.groups.entry(group).or_default().extend(measurements);
    }
    let serialized = serde_json::to_vec_pretty(&doc).expect("serialize bench results");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::File::create(path)?;
    f.write_all(&serialized)?;
    Ok(())
}

fn fresh_doc() -> Document {
    Document {
        schema: SCHEMA_VERSION,
        git_sha: git_sha(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        host: host_info(),
        groups: BTreeMap::new(),
    }
}

fn git_sha() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn host_info() -> Host {
    Host {
        cpu: std::env::var("HOSTTYPE").unwrap_or_else(|_| std::env::consts::ARCH.into()),
        cores: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0),
        os: std::env::consts::OS.into(),
    }
}
