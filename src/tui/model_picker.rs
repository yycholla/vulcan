//! Unified model picker tree builder (YYC-101).
//!
//! Turns a flat catalog (`Vec<ModelInfo>`) into a hierarchy that mini.files-
//! style columns can render: provider → lab → series → version. Pure
//! helper module — render and key dispatch live in `tui::mod`.

use crate::provider::catalog::ModelInfo;
use crate::tui::input::{TuiKeyCode, TuiKeyEvent};
use crate::tui::miller_columns::{MillerEntry, MillerPreview, MillerSource};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Default)]
pub struct ModelPickerState {
    /// Display labels for column 0, parallel to `provider_keys`.
    pub provider_labels: Vec<String>,
    /// Cache keys per column-0 row. `None` = legacy `[provider]` block.
    pub provider_keys: Vec<Option<String>>,
    /// Catalog cache keyed by provider key (`"default"` for legacy).
    pub items_by_key: HashMap<String, Vec<ModelInfo>>,
    /// Tree cache keyed by provider key.
    pub trees_by_key: HashMap<String, ModelTree>,
    /// Selection index per drilled column.
    pub path: Vec<usize>,
    /// Which column currently has focus (0 = column 0, etc.).
    pub focus: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelPickerOutcome {
    Continue,
    Close,
    Commit {
        profile: Option<String>,
        model_id: String,
    },
}

impl ModelPickerState {
    pub fn source(&self) -> UnifiedPickerSource<'_> {
        UnifiedPickerSource {
            provider_labels: &self.provider_labels,
            provider_keys: &self.provider_keys,
            items_by_key: &self.items_by_key,
            trees_by_key: &self.trees_by_key,
        }
    }

    pub fn miller_state(&self) -> crate::tui::miller_columns::MillerState {
        crate::tui::miller_columns::MillerState {
            path: self.path.clone(),
            focus: self.focus,
        }
    }

    pub fn handle_key(&mut self, key: TuiKeyEvent) -> ModelPickerOutcome {
        let mut state = self.miller_state();
        let mut close = false;
        let mut commit = false;
        {
            let source = self.source();
            match key.code {
                TuiKeyCode::Up | TuiKeyCode::Char('k') => {
                    crate::tui::miller_columns::move_cursor(&mut state, &source, -1);
                }
                TuiKeyCode::Down | TuiKeyCode::Char('j') => {
                    crate::tui::miller_columns::move_cursor(&mut state, &source, 1);
                }
                TuiKeyCode::Left | TuiKeyCode::Char('h') | TuiKeyCode::Char('H') => {
                    if !crate::tui::miller_columns::ascend(&mut state) {
                        close = true;
                    }
                }
                TuiKeyCode::Right | TuiKeyCode::Char('l') => {
                    if !crate::tui::miller_columns::drill(&mut state, &source) {
                        commit = true;
                    }
                }
                TuiKeyCode::Char('L') => {
                    crate::tui::miller_columns::drill(&mut state, &source);
                    commit = true;
                }
                TuiKeyCode::Enter => {
                    commit = true;
                }
                TuiKeyCode::Esc | TuiKeyCode::Char('q') => close = true,
                _ => {}
            }
        }
        self.path = state.path;
        self.focus = state.focus;

        if commit && let Some((key, model_id)) = self.source().leaf_at(&self.path) {
            return ModelPickerOutcome::Commit {
                profile: if key == "default" { None } else { Some(key) },
                model_id,
            };
        }
        if close {
            ModelPickerOutcome::Close
        } else {
            ModelPickerOutcome::Continue
        }
    }
}

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
        grouped.entry(lab.to_string()).or_default().push((i, rest));
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
        by_series.entry(series).or_default().push((*idx, tail));
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

/// `MillerSource` adapter for the unified picker (YYC-102 follow-up).
/// Column 0 = configured providers; columns 1+ = the highlighted
/// provider's lab/series/version tree. Catalogs are passed in keyed by
/// stable cache keys (`"default"` for the legacy `[provider]` block,
/// the profile name for `[providers.<name>]` blocks).
pub struct UnifiedPickerSource<'a> {
    /// Display labels for column 0, parallel to `provider_keys`.
    pub provider_labels: &'a [String],
    /// Cache keys per column-0 row. `None` ↔ legacy `[provider]`.
    pub provider_keys: &'a [Option<String>],
    /// Per-provider catalog (already fetched).
    pub items_by_key: &'a HashMap<String, Vec<ModelInfo>>,
    /// Per-provider model tree built from the catalog.
    pub trees_by_key: &'a HashMap<String, ModelTree>,
}

impl<'a> UnifiedPickerSource<'a> {
    pub fn key_for(&self, provider_idx: usize) -> &str {
        match self.provider_keys.get(provider_idx) {
            Some(Some(name)) => name.as_str(),
            _ => "default",
        }
    }

    fn tree_for(&self, provider_idx: usize) -> Option<&'a ModelTree> {
        self.trees_by_key.get(self.key_for(provider_idx))
    }

    fn items_for(&self, provider_idx: usize) -> Option<&'a [ModelInfo]> {
        self.items_by_key
            .get(self.key_for(provider_idx))
            .map(|v| v.as_slice())
    }

    fn nodes_at(&self, path: &[usize]) -> Option<&'a [TreeNode]> {
        let provider_idx = *path.first()?;
        let tree = self.tree_for(provider_idx)?;
        let inner = &path[1..];
        Some(tree.column_at(inner.len(), inner))
    }

    pub fn leaf_at(&self, path: &[usize]) -> Option<(String, String)> {
        let provider_idx = *path.first()?;
        let tree = self.tree_for(provider_idx)?;
        let items = self.items_for(provider_idx)?;
        let inner = &path[1..];
        let mut current: &[TreeNode] = &tree.labs;
        let mut leaf: Option<usize> = None;
        for &idx in inner {
            let node = current.get(idx)?;
            if node.children.is_empty() {
                leaf = node.model_index;
                break;
            }
            leaf = node.model_index;
            current = &node.children;
        }
        let id = items.get(leaf?)?.id.clone();
        let provider_key = match self.provider_keys.get(provider_idx)? {
            Some(name) => name.clone(),
            None => "default".to_string(),
        };
        Some((provider_key, id))
    }
}

impl<'a> MillerSource for UnifiedPickerSource<'a> {
    fn header(&self, path: &[usize]) -> String {
        if path.is_empty() {
            return "~ providers".to_string();
        }
        let provider_idx = path[0];
        let label = self
            .provider_labels
            .get(provider_idx)
            .cloned()
            .unwrap_or_else(|| "?".into());
        if path.len() == 1 {
            return label;
        }
        let Some(tree) = self.tree_for(provider_idx) else {
            return label;
        };
        let inner = &path[1..];
        let mut current: &[TreeNode] = &tree.labs;
        let mut last = label;
        for &idx in inner {
            let Some(node) = current.get(idx) else {
                return last;
            };
            last = node.label.clone();
            if node.children.is_empty() {
                return last;
            }
            current = &node.children;
        }
        last
    }

    fn entries(&self, path: &[usize]) -> Vec<MillerEntry> {
        if path.is_empty() {
            return self
                .provider_labels
                .iter()
                .enumerate()
                .map(|(i, label)| MillerEntry {
                    label: label.clone(),
                    icon: if provider_has_free_model(self.tree_for(i), self.items_for(i)) {
                        "✦"
                    } else {
                        "▸"
                    }
                    .into(),
                    has_children: self
                        .tree_for(i)
                        .map(|t| !t.labs.is_empty())
                        .unwrap_or(false),
                })
                .collect();
        }
        let Some(nodes) = self.nodes_at(path) else {
            return Vec::new();
        };
        nodes
            .iter()
            .map(|node| MillerEntry {
                label: node.label.clone(),
                icon: if node_is_free_model(node, self.items_for(*path.first().unwrap_or(&0))) {
                    "◆"
                } else if node_has_free_model(node, self.items_for(*path.first().unwrap_or(&0))) {
                    "✦"
                } else if node.children.is_empty() {
                    "·"
                } else {
                    "◈"
                }
                .to_string(),
                has_children: !node.children.is_empty(),
            })
            .collect()
    }

    fn preview(&self, path: &[usize]) -> Option<MillerPreview> {
        if path.len() < 2 {
            return None;
        }
        let provider_idx = *path.first()?;
        let items = self.items_for(provider_idx)?;
        let tree = self.tree_for(provider_idx)?;
        let inner = &path[1..];
        let mut current: &[TreeNode] = &tree.labs;
        let mut leaf: Option<usize> = None;
        for &idx in inner {
            let node = current.get(idx)?;
            if node.children.is_empty() {
                leaf = node.model_index;
                break;
            }
            leaf = node.model_index;
            current = &node.children;
        }
        let model = items.get(leaf?)?;
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            model.id.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        if model.context_length > 0 {
            lines.push(Line::from(format!(
                "context  : {}",
                crate::tui::state::format_thousands(model.context_length as u32)
            )));
        }
        let mut feats = Vec::new();
        if model.features.tools {
            feats.push("tools");
        }
        if model.features.reasoning {
            feats.push("reasoning");
        }
        if model.features.vision {
            feats.push("vision");
        }
        if model.features.json_mode {
            feats.push("json");
        }
        if !feats.is_empty() {
            lines.push(Line::from(format!("features : {}", feats.join(", "))));
        }
        if let Some(p) = &model.pricing {
            lines.push(Line::from(format!(
                "pricing  : ${:.4}/1k in · ${:.4}/1k out",
                p.input_per_token * 1000.0,
                p.output_per_token * 1000.0,
            )));
        }
        if let Some(top) = &model.top_provider {
            lines.push(Line::from(format!("upstream : {top}")));
        }
        Some(MillerPreview {
            title: model.id.clone(),
            lines,
        })
    }
}

/// `MillerSource` adapter that drives the universal miller-columns
/// widget from a `ModelTree` + the source catalog (YYC-102).
pub struct ModelPickerSource<'a> {
    pub tree: &'a ModelTree,
    pub items: &'a [ModelInfo],
    pub root_label: String,
}

impl<'a> ModelPickerSource<'a> {
    pub fn new(tree: &'a ModelTree, items: &'a [ModelInfo], root_label: String) -> Self {
        Self {
            tree,
            items,
            root_label,
        }
    }

    fn nodes_at(&self, path: &[usize]) -> &'a [TreeNode] {
        self.tree.column_at(path.len(), path)
    }

    pub fn leaf_at(&self, path: &[usize]) -> Option<usize> {
        let mut current: &[TreeNode] = &self.tree.labs;
        let mut leaf: Option<usize> = None;
        for &idx in path {
            let node = current.get(idx)?;
            if node.children.is_empty() {
                return node.model_index;
            }
            leaf = node.model_index;
            current = &node.children;
        }
        leaf
    }
}

impl<'a> MillerSource for ModelPickerSource<'a> {
    fn header(&self, path: &[usize]) -> String {
        if path.is_empty() {
            return self.root_label.clone();
        }
        let mut current: &[TreeNode] = &self.tree.labs;
        let mut last = self.root_label.clone();
        for &idx in path {
            let Some(node) = current.get(idx) else {
                return last;
            };
            last = node.label.clone();
            if node.children.is_empty() {
                return last;
            }
            current = &node.children;
        }
        last
    }

    fn entries(&self, path: &[usize]) -> Vec<MillerEntry> {
        self.nodes_at(path)
            .iter()
            .map(|node| MillerEntry {
                label: node.label.clone(),
                icon: if node_is_free_model(node, Some(self.items)) {
                    "◆"
                } else if node_has_free_model(node, Some(self.items)) {
                    "✦"
                } else if node.children.is_empty() {
                    "·"
                } else {
                    "◈"
                }
                .to_string(),
                has_children: !node.children.is_empty(),
            })
            .collect()
    }

    fn preview(&self, path: &[usize]) -> Option<MillerPreview> {
        let leaf = self.leaf_at(path)?;
        let model = self.items.get(leaf)?;
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(Span::styled(
            model.id.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        if model.context_length > 0 {
            lines.push(Line::from(format!(
                "context  : {}",
                crate::tui::state::format_thousands(model.context_length as u32)
            )));
        }
        let mut feats = Vec::new();
        if model.features.tools {
            feats.push("tools");
        }
        if model.features.reasoning {
            feats.push("reasoning");
        }
        if model.features.vision {
            feats.push("vision");
        }
        if model.features.json_mode {
            feats.push("json");
        }
        if !feats.is_empty() {
            lines.push(Line::from(format!("features : {}", feats.join(", "))));
        }
        if let Some(p) = &model.pricing {
            lines.push(Line::from(format!(
                "pricing  : ${:.4}/1k in · ${:.4}/1k out",
                p.input_per_token * 1000.0,
                p.output_per_token * 1000.0,
            )));
        }
        if let Some(top) = &model.top_provider {
            lines.push(Line::from(format!("upstream : {top}")));
        }
        Some(MillerPreview {
            title: model.id.clone(),
            lines,
        })
    }
}

fn tokenize(rest: &str) -> Vec<String> {
    rest.split(['-', ':'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn provider_has_free_model(tree: Option<&ModelTree>, items: Option<&[ModelInfo]>) -> bool {
    let Some(tree) = tree else {
        return false;
    };
    tree.labs
        .iter()
        .any(|node| node_has_free_model(node, items))
}

fn node_has_free_model(node: &TreeNode, items: Option<&[ModelInfo]>) -> bool {
    node_is_free_model(node, items)
        || node
            .children
            .iter()
            .any(|child| node_has_free_model(child, items))
}

fn node_is_free_model(node: &TreeNode, items: Option<&[ModelInfo]>) -> bool {
    let Some(items) = items else {
        return false;
    };
    let Some(index) = node.model_index else {
        return false;
    };
    items.get(index).is_some_and(model_is_free)
}

fn model_is_free(model: &ModelInfo) -> bool {
    let zero_price = model
        .pricing
        .as_ref()
        .is_some_and(|pricing| pricing.input_per_token == 0.0 && pricing.output_per_token == 0.0);
    zero_price || model.id.ends_with(":free") || model.display_name.ends_with(":free")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::catalog::{ModelFeatures, ModelInfo, Pricing};

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

    fn free_mi(id: &str) -> ModelInfo {
        ModelInfo {
            pricing: Some(Pricing {
                input_per_token: 0.0,
                output_per_token: 0.0,
            }),
            ..mi(id)
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
        let series_labels: Vec<&str> = moonshot.children.iter().map(|n| n.label.as_str()).collect();
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

    #[test]
    fn free_models_mark_lab_family_and_leaf() {
        let models = vec![free_mi("inclusionai/ling-2.6-1t:free")];
        let tree = build_model_tree("openrouter", &models);
        let source = ModelPickerSource::new(&tree, &models, "openrouter".into());

        let labs = source.entries(&[]);
        assert_eq!(labs[0].icon, "✦");

        let families = source.entries(&[0]);
        assert_eq!(families[0].icon, "✦");

        let versions = source.entries(&[0, 0]);
        assert_eq!(versions[0].icon, "◆");
    }

    #[test]
    fn picker_state_commits_selected_leaf() {
        let models = vec![mi("anthropic/claude-opus-4-7")];
        let mut items_by_key = HashMap::new();
        items_by_key.insert("paid".into(), models.clone());
        let mut trees_by_key = HashMap::new();
        trees_by_key.insert("paid".into(), build_model_tree("openrouter", &models));
        let mut state = ModelPickerState {
            provider_labels: vec!["paid".into()],
            provider_keys: vec![Some("paid".into())],
            items_by_key,
            trees_by_key,
            path: vec![0, 0, 0, 0],
            focus: 3,
        };

        assert_eq!(
            state.handle_key(TuiKeyEvent::new(
                TuiKeyCode::Enter,
                crate::tui::input::TuiKeyModifiers::NONE,
            )),
            ModelPickerOutcome::Commit {
                profile: Some("paid".into()),
                model_id: "anthropic/claude-opus-4-7".into(),
            }
        );
    }
}
