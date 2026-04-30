//! YYC-266 Slice 3 Task 3.4 greptest.
//!
//! `src/gateway/agent_map.rs` was deleted when the gateway ported to
//! routing prompts through the daemon's `Client`. This test guards
//! against accidental reintroduction of the symbol or its module —
//! whether by a CHERRY-PICK from an older branch, a copy-paste from
//! tests/, or an LLM-generated suggestion.
//!
//! The walk is hand-rolled (no walkdir dep) so this test stays free of
//! transitive dependencies and runs even on minimal builds.

#![cfg(feature = "gateway")]

use std::path::{Path, PathBuf};

#[test]
fn no_agent_map_module_or_references_in_gateway() {
    let gateway_dir = Path::new("src/gateway");
    let mut hits = Vec::new();
    walk_rust_files(gateway_dir, &mut |path| {
        let content = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                hits.push(format!("{}: read failed: {e}", path.display()));
                return;
            }
        };
        // Match the symbol or its module path. Anchor on
        // `agent_map` (snake_case module) and `AgentMap`
        // (PascalCase struct). Either one resurfacing means the
        // port has regressed.
        for needle in ["agent_map", "AgentMap"] {
            if content.contains(needle) {
                hits.push(format!("{}: contains `{needle}`", path.display()));
                break;
            }
        }
    });
    assert!(
        hits.is_empty(),
        "found agent_map references that must be removed:\n{}",
        hits.join("\n")
    );
}

/// Recurse `dir`, calling `f` on each `.rs` file found.
fn walk_rust_files(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        if path.is_dir() {
            walk_rust_files(&path, f);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            f(&path);
        }
    }
}
