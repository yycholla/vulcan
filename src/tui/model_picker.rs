//! Unified model picker tree builder (YYC-101).
//!
//! Turns a flat catalog (`Vec<ModelInfo>`) into a hierarchy that mini.files-
//! style columns can render: provider → lab → series → version. Pure
//! helper module — render and key dispatch live in `tui::mod`.

use crate::provider::catalog::ModelInfo;
use std::collections::BTreeMap;

/// One node in the model tree. Leaves point at a `ModelInfo` index in
/// the source catalog so the caller can resolve full metadata when the
/// user lands on a leaf.
#[derive(Debug, Clone, Default)]
pub struct ModelTree {
    pub provider_label: String,
    /// Top-level entries (one per "lab" — the segment before the first
    /// `/` in OpenRouter-style ids, or "default" for flat slugs).
    pub labs: Vec<TreeNode>,
}

#[derive(Debug, Clone)]
pub struct TreeNode {
    pub label: String,
    pub children: Vec<TreeNode>,
    /// Some(index) when this node is a leaf pointing at a model in the
    /// source catalog. Internal nodes carry None.
    pub model_index: Option<usize>,
}

impl ModelTree {
    /// Walk the tree to reach the column at depth `depth`, taking the
    /// `selection_path[i]` index at each level. Returns the column's
    /// nodes, or empty if depth exceeds the path.
    pub fn column_at(&self, depth: usize, path: &[usize]) -> &[TreeNode] {
        if depth == 0 {
            return &self.labs;
        }
        let mut current: &[TreeNode] = &self.labs;
        for &idx in path.iter().take(depth) {
            let Some(node) = current.get(idx) else {
                return &[];
            };
            current = &node.children;
        }
        current
    }

    /// Number of columns the tree exposes given a fully-drilled path.
    pub fn max_depth(&self) -> usize {
        fn dive(nodes: &[TreeNode], current: usize, best: &mut usize) {
            if current > *best {
                *best = current;
            }
            for n in nodes {
                if !n.children.is_empty() {
                    dive(&n.children, current + 1, best);
                }
            }
        }
        let mut best = 0;
        dive(&self.labs, 1, &mut best);
        best
    }
}

/// Build a column-friendly tree from a flat catalog.
///
/// `provider_label` is the display name for the active provider profile
/// — used as the root row in column 0 and as the synthetic "lab" for
/// flat (non-slash) model ids.
pub fn build_model_tree(provider_label: &str, models: &[ModelInfo]) -> ModelTree {
    let mut grouped: BTreeMap<String, Vec<(usize, &str)>> = BTreeMap::new();
    for (i, m) in models.iter().enumerate() {
        let (lab, rest) = split_lab(&m.id, provider_label);
        grouped
            .entry(lab.to_string())
            .or_default()
            .push((i, rest));
    }
    let mut labs: Vec<TreeNode> = grouped
        .into_iter()
        .map(|(lab, entries)| build_lab_node(&lab, &entries))
        .collect();
    // Sort labs alphabetically with a couple of usual suspects pinned
    // toward the front.
    labs.sort_by(|a, b| a.label.cmp(&b.label));
    ModelTree {
        provider_label: provider_label.to_string(),
        labs,
    }
}

fn split_lab<'a>(id: &'a str, fallback_lab: &'a str) -> (&'a str, &'a str) {
    if let Some((lab, rest)) = id.split_once('/') {
        (lab, rest)
    } else {
        (fallback_lab, id)
    }
}

/// One pass through a lab's models: split each remaining id by
/// `-` / `:` / `.` into series / version components and group.
fn build_lab_node(lab: &str, entries: &[(usize, &str)]) -> TreeNode {
    // Build a tree of (series-tokens) → leaves.
    // For most ids the first token is a coherent series (`gpt`, `claude`,
    // `kimi`, `qwen2.5`); subsequent tokens describe the variant.
    let mut node = TreeNode {
        label: lab.to_string(),
        children: Vec::new(),
        model_index: None,
    };
    let mut by_series: BTreeMap<String, Vec<(usize, Vec<String>)>> = BTreeMap::new();
    for (idx, rest) in entries {
        let toks = tokenize(rest);
        let series = toks.first().cloned().unwrap_or_else(|| rest.to_string());
        let tail: Vec<String> = toks.into_iter().skip(1).collect();
        by_series
            .entry(series)
            .or_default()
            .push((*idx, tail));
    }
    for (series, leaves) in by_series {
        node.children.push(build_series_node(&series, &leaves));
    }
    node
}

fn build_series_node(series: &str, leaves: &[(usize, Vec<String>)]) -> TreeNode {
    let mut node = TreeNode {
        label: series.to_string(),
        children: Vec::new(),
        model_index: None,
    };
    if leaves.len() == 1 {
        let (idx, _) = leaves[0];
        node.children.push(TreeNode {
            label: "(only)".to_string(),
            children: Vec::new(),
            model_index: Some(idx),
        });
        return node;
    }
    // Group leaves by the first remaining token (= "version") so users
    // can drill one more layer down. Anything past that is collapsed
    // into the leaf label.
    let mut by_version: BTreeMap<String, Vec<(usize, Vec<String>)>> = BTreeMap::new();
    for (idx, tail) in leaves {
        let (head, rest) = match tail.split_first() {
            Some((h, r)) => (h.clone(), r.to_vec()),
            None => ("(base)".to_string(), Vec::new()),
        };
        by_version.entry(head).or_default().push((*idx, rest));
    }
    for (version, group) in by_version {
        if group.len() == 1 {
            let (idx, rest) = &group[0];
            let label = if rest.is_empty() {
                version.clone()
            } else {
                format!("{} {}", version, rest.join(" "))
            };
            node.children.push(TreeNode {
                label,
                children: Vec::new(),
                model_index: Some(*idx),
            });
            continue;
        }
        // Multiple variants share the same version prefix → expose them
        // as siblings under the version node.
        let mut version_node = TreeNode {
            label: version.clone(),
            children: Vec::new(),
            model_index: None,
        };
        for (idx, rest) in &group {
            let label = if rest.is_empty() {
                "(base)".to_string()
            } else {
                rest.join(" ")
            };
            version_node.children.push(TreeNode {
                label,
                children: Vec::new(),
                model_index: Some(*idx),
            });
        }
        node.children.push(version_node);
    }
    node
}

fn tokenize(rest: &str) -> Vec<String> {
    rest.split(|c: char| c == '-' || c == ':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::catalog::{ModelFeatures, ModelInfo};

    fn mi(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            display_name: id.into(),
            context_length: 128_000,
            pricing: None,
            features: ModelFeatures::default(),
            top_provider: None,
        }
    }

    #[test]
    fn slash_ids_split_lab_then_series() {
        let models = vec![
            mi("moonshot/kimi-k2.6"),
            mi("moonshot/kimi-k2.5"),
            mi("openai/gpt-5"),
        ];
        let tree = build_model_tree("openrouter", &models);
        let lab_labels: Vec<&str> = tree.labs.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(lab_labels, vec!["moonshot", "openai"]);

        let moonshot = tree.labs.iter().find(|n| n.label == "moonshot").unwrap();
        let series_labels: Vec<&str> = moonshot
            .children
            .iter()
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(series_labels, vec!["kimi"]);

        let kimi = &moonshot.children[0];
        let versions: Vec<&str> = kimi.children.iter().map(|n| n.label.as_str()).collect();
        // Two leaves k2.5 and k2.6.
        assert_eq!(versions.len(), 2);
        assert!(versions.contains(&"k2.5"));
        assert!(versions.contains(&"k2.6"));
    }

    #[test]
    fn flat_ids_use_provider_label_as_lab() {
        let models = vec![mi("gpt-5"), mi("gpt-5-mini"), mi("o3")];
        let tree = build_model_tree("openai", &models);
        let labs: Vec<&str> = tree.labs.iter().map(|n| n.label.as_str()).collect();
        assert_eq!(labs, vec!["openai"]);

        let lab = &tree.labs[0];
        let series: Vec<&str> = lab.children.iter().map(|n| n.label.as_str()).collect();
        // gpt + o3
        assert!(series.contains(&"gpt"));
        assert!(series.contains(&"o3"));
    }

    #[test]
    fn single_model_in_lab_collapses_to_only_leaf() {
        let models = vec![mi("anthropic/claude-opus-4-7")];
        let tree = build_model_tree("openrouter", &models);
        let lab = &tree.labs[0];
        assert_eq!(lab.label, "anthropic");
        let claude = &lab.children[0];
        assert_eq!(claude.label, "claude");
        assert_eq!(claude.children.len(), 1);
        assert_eq!(claude.children[0].label, "(only)");
        assert_eq!(claude.children[0].model_index, Some(0));
    }

    #[test]
    fn column_at_walks_path() {
        let models = vec![mi("moonshot/kimi-k2.6"), mi("openai/gpt-5")];
        let tree = build_model_tree("openrouter", &models);
        // Column 0: labs.
        assert_eq!(tree.column_at(0, &[]).len(), 2);
        // Column 1: series under moonshot (idx 0).
        let series = tree.column_at(1, &[0]);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].label, "kimi");
        // Column 2: versions under moonshot/kimi.
        let versions = tree.column_at(2, &[0, 0]);
        assert!(!versions.is_empty());
    }
}
