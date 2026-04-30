//! YYC-188: named bundles of project + task context.
//!
//! ## Scope of this PR
//!
//! - `ContextSource` enum (file path, doc reference, run id,
//!   artifact id, free-form note).
//! - `ContextPack` declaration with name, description, sources,
//!   and selection notes.
//! - Built-in catalog covering at least three packs (`gateway`,
//!   `hooks`, `review`).
//! - `lookup` + `all` + `resolve` accessors.
//!
//! ## Deliberately deferred
//!
//! - `--context-pack` CLI flag wiring on agent build.
//! - Token-budget pruning during resolve.
//! - Per-user packs in config (`[context_packs.<name>]`).
//! - Run-record annotation.

use serde::{Deserialize, Serialize};

/// A single source line in a pack. Kept small + flat so the
/// resolver can render a citation list without recursion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextSource {
    /// Concrete file in the repo.
    File { path: String, why: String },
    /// External doc reference (Linear doc id, URL, wiki page).
    Doc { reference: String, why: String },
    /// Past run id (YYC-179).
    Run { id: String, why: String },
    /// Past artifact id (YYC-180).
    Artifact { id: String, why: String },
    /// Free-form note that should land in the prompt verbatim
    /// (project rule, reminder, etc.).
    Note { text: String },
}

impl ContextSource {
    pub fn short_label(&self) -> String {
        match self {
            ContextSource::File { path, .. } => format!("file:{path}"),
            ContextSource::Doc { reference, .. } => format!("doc:{reference}"),
            ContextSource::Run { id, .. } => format!("run:{id}"),
            ContextSource::Artifact { id, .. } => format!("artifact:{id}"),
            ContextSource::Note { .. } => "note".to_string(),
        }
    }
}

/// A named pack. `name` is the user-visible handle; everything
/// else is metadata that drives resolution + display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPack {
    pub name: String,
    pub description: String,
    pub sources: Vec<ContextSource>,
}

impl ContextPack {
    pub fn matches(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

/// Render the pack as a compact System-prompt-ready block. The
/// `references` view: source labels + per-source `why`. The
/// agent loads referenced files separately when the prompt
/// builder sees the pack — this PR only renders the citation
/// list, not the file contents.
pub fn render_pack_summary(pack: &ContextPack) -> String {
    let mut out = String::new();
    out.push_str(&format!("## Context pack: {}\n", pack.name));
    out.push_str(&format!("_{}_\n\n", pack.description));
    if pack.sources.is_empty() {
        out.push_str("_No sources declared._\n");
        return out;
    }
    for src in &pack.sources {
        match src {
            ContextSource::File { path, why } => {
                out.push_str(&format!("- file `{path}` — {why}\n"));
            }
            ContextSource::Doc { reference, why } => {
                out.push_str(&format!("- doc `{reference}` — {why}\n"));
            }
            ContextSource::Run { id, why } => {
                out.push_str(&format!("- run `{id}` — {why}\n"));
            }
            ContextSource::Artifact { id, why } => {
                out.push_str(&format!("- artifact `{id}` — {why}\n"));
            }
            ContextSource::Note { text } => {
                out.push_str(&format!("- note: {text}\n"));
            }
        }
    }
    out
}

/// Built-in pack catalog. Each pack curates the load-bearing
/// docs/files for working in that part of the codebase. Light on
/// purpose — better that the user adds files than the catalog
/// over-injects.
pub fn builtin_packs() -> Vec<ContextPack> {
    vec![
        ContextPack {
            name: "gateway".into(),
            description: "Gateway daemon: lanes, queues, platforms, scheduler.".into(),
            sources: vec![
                ContextSource::File {
                    path: "src/gateway/mod.rs".into(),
                    why: "entry point + module map".into(),
                },
                ContextSource::File {
                    path: "src/gateway/lane.rs".into(),
                    why: "per-chat serial dispatch".into(),
                },
                ContextSource::File {
                    path: "src/gateway/queue.rs".into(),
                    why: "durable inbound/outbound queues".into(),
                },
                ContextSource::File {
                    path: "src/gateway/lane_router.rs".into(),
                    why: "lane → daemon-session routing (replaces agent_map; daemon owns Agent)"
                        .into(),
                },
                ContextSource::Note {
                    text: "Gateway lanes default to the `gateway-safe` capability profile (YYC-181)."
                        .into(),
                },
            ],
        },
        ContextPack {
            name: "hooks".into(),
            description: "Hook system: events, outcomes, built-ins, ordering.".into(),
            sources: vec![
                ContextSource::File {
                    path: "src/hooks/mod.rs".into(),
                    why: "event types + registry".into(),
                },
                ContextSource::File {
                    path: "src/hooks/audit.rs".into(),
                    why: "reference handler".into(),
                },
                ContextSource::File {
                    path: "src/hooks/skills.rs".into(),
                    why: "BeforePrompt injection sample".into(),
                },
                ContextSource::Note {
                    text: "First non-Continue outcome wins for blocking events; injections accumulate."
                        .into(),
                },
            ],
        },
        ContextPack {
            name: "review".into(),
            description: "Review-mode posture, capability profiles, contract tests.".into(),
            sources: vec![
                ContextSource::File {
                    path: "src/review/mod.rs".into(),
                    why: "report shape + critic prompt".into(),
                },
                ContextSource::File {
                    path: "src/tools/profile.rs".into(),
                    why: "reviewer / readonly capability profiles".into(),
                },
                ContextSource::File {
                    path: "tests/contracts.rs".into(),
                    why: "behavioral invariants the agent must keep".into(),
                },
                ContextSource::Note {
                    text: "Review mode does not mutate files; ride the `reviewer` profile."
                        .into(),
                },
            ],
        },
        ContextPack {
            name: "tui".into(),
            description: "TUI state, render loop, keybinds, status overlays.".into(),
            sources: vec![
                ContextSource::File {
                    path: "src/tui/mod.rs".into(),
                    why: "entry point + main loop".into(),
                },
                ContextSource::Note {
                    text: "TUI logs to a file (YYC-200); never println! in TUI mode.".into(),
                },
            ],
        },
        ContextPack {
            name: "provider".into(),
            description: "Provider config, streaming/buffered paths, error classes.".into(),
            sources: vec![
                ContextSource::File {
                    path: "src/provider/mod.rs".into(),
                    why: "LLMProvider trait + types".into(),
                },
                ContextSource::File {
                    path: "src/provider/openai.rs".into(),
                    why: "OpenAI-compatible buffered + streaming impl".into(),
                },
                ContextSource::Note {
                    text: "Both buffered (chat) and streaming (chat_stream) paths must honour every hook event."
                        .into(),
                },
            ],
        },
    ]
}

/// Resolve a pack by name. Returns `None` for unknown names.
pub fn lookup(name: &str) -> Option<ContextPack> {
    builtin_packs().into_iter().find(|p| p.matches(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_at_least_three_packs() {
        let packs = builtin_packs();
        assert!(
            packs.len() >= 3,
            "acceptance criterion requires ≥3 built-ins, got {}",
            packs.len()
        );
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup("gateway").is_some());
        assert!(lookup("Gateway").is_some());
        assert!(lookup("HOOKS").is_some());
        assert!(lookup("does-not-exist").is_none());
    }

    #[test]
    fn pack_names_are_unique() {
        let packs = builtin_packs();
        let mut names: Vec<&str> = packs.iter().map(|p| p.name.as_str()).collect();
        names.sort();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total);
    }

    #[test]
    fn render_summary_emits_pack_header_and_source_lines() {
        let pack = lookup("gateway").unwrap();
        let md = render_pack_summary(&pack);
        assert!(md.starts_with("## Context pack: gateway\n"));
        assert!(md.contains("file `src/gateway/lane.rs`"));
        assert!(md.contains("note: Gateway lanes default"));
    }

    #[test]
    fn empty_pack_renders_no_sources_marker() {
        let pack = ContextPack {
            name: "empty".into(),
            description: "fixture".into(),
            sources: Vec::new(),
        };
        let md = render_pack_summary(&pack);
        assert!(md.contains("_No sources declared._"));
    }

    #[test]
    fn source_short_label_returns_kind_prefixed_handle() {
        let f = ContextSource::File {
            path: "src/x.rs".into(),
            why: "y".into(),
        };
        assert_eq!(f.short_label(), "file:src/x.rs");
        let r = ContextSource::Run {
            id: "abc".into(),
            why: "y".into(),
        };
        assert_eq!(r.short_label(), "run:abc");
    }
}
