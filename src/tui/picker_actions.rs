//! Model-picker keypress actions extracted from `tui/mod.rs` (YYC-108).
//! State mutations only; the rendering side lives in
//! `tui/render_overlays.rs`.

use super::state::AppState;

pub(super) fn picker_move(app: &mut AppState, delta: i32) {
    let depth = app.model_picker_focus;
    let path_prefix: Vec<usize> = app.model_picker_path.iter().copied().take(depth).collect();
    let len = app.model_picker_tree.column_at(depth, &path_prefix).len();
    if len == 0 {
        return;
    }
    while app.model_picker_path.len() <= depth {
        app.model_picker_path.push(0);
    }
    let cur = app.model_picker_path[depth] as i32 + delta;
    let max = (len - 1) as i32;
    app.model_picker_path[depth] = cur.clamp(0, max) as usize;
    // Reset deeper selections — the active branch changed.
    app.model_picker_path.truncate(depth + 1);
}

pub(super) fn picker_drill_or_commit(app: &mut AppState) -> Option<String> {
    let depth = app.model_picker_focus;
    let path_prefix: Vec<usize> = app.model_picker_path.iter().copied().take(depth).collect();
    let nodes = app
        .model_picker_tree
        .column_at(depth, &path_prefix)
        .to_vec();
    let sel = app.model_picker_path.get(depth).copied().unwrap_or(0);
    let node = nodes.get(sel)?;
    if node.children.is_empty() {
        // Leaf — commit.
        return node
            .model_index
            .and_then(|i| app.model_picker_items.get(i))
            .map(|m| m.id.clone());
    }
    // Drill: focus next column, default selection 0.
    while app.model_picker_path.len() <= depth + 1 {
        app.model_picker_path.push(0);
    }
    app.model_picker_path[depth + 1] = 0;
    app.model_picker_focus = depth + 1;
    None
}

pub(super) fn picker_commit_current(app: &AppState) -> Option<String> {
    super::render_overlays::picker_current_leaf(&app.model_picker_tree, &app.model_picker_path)
        .and_then(|i| app.model_picker_items.get(i))
        .map(|m| m.id.clone())
}

pub(super) fn initial_path_for_active_model(
    tree: &crate::tui::model_picker::ModelTree,
    active_id: &str,
    items: &[crate::provider::catalog::ModelInfo],
) -> Vec<usize> {
    let target = items.iter().position(|m| m.id == active_id);
    fn find_path(
        nodes: &[crate::tui::model_picker::TreeNode],
        target: Option<usize>,
        path: &mut Vec<usize>,
    ) -> bool {
        for (i, node) in nodes.iter().enumerate() {
            path.push(i);
            if node.model_index.is_some() && node.model_index == target {
                return true;
            }
            if find_path(&node.children, target, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    if !find_path(&tree.labs, target, &mut path) {
        // No exact match — start from column 0 with no drilled selection.
        path.clear();
        path.push(0);
    }
    path
}
