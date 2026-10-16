//! `locator.ariaSnapshot()` in Playwright's YAML-like format.

use gpui_playwright_selectors::{Checked, Tree};

/// Renders the subtree at `root`, or the whole tree, as an ARIA snapshot.
pub(crate) fn snapshot(tree: &Tree, root: Option<usize>) -> String {
    let mut lines = Vec::new();
    match root {
        Some(root) => render(tree, root, 0, &mut lines),
        None => {
            for root in tree.roots() {
                render(tree, root, 0, &mut lines);
            }
        }
    }
    lines.join("\n")
}

fn render(tree: &Tree, index: usize, depth: usize, lines: &mut Vec<String>) {
    let node = tree.node(index);
    if !node.visible {
        return;
    }
    // Generic containers are transparent, like `generic` nodes in Playwright snapshots.
    let role = match node.role.as_deref() {
        None | Some("generic") => {
            if !node.text.is_empty() && tree.children(index).is_empty() {
                lines.push(format!("{}- text: {}", indent(depth), quote(&node.text)));
            }
            for &child in tree.children(index) {
                render(tree, child, depth, lines);
            }
            return;
        }
        Some(role) => role,
    };
    let mut line = format!("{}- {role}", indent(depth));
    let name = tree.accessible_name(index);
    if !name.is_empty() {
        line.push_str(&format!(" {}", quote(&name)));
    }
    match node.checked {
        Some(Checked::True) => line.push_str(" [checked]"),
        Some(Checked::Mixed) => line.push_str(" [checked=mixed]"),
        _ => {}
    }
    if node.disabled {
        line.push_str(" [disabled]");
    }
    if node.expanded == Some(true) {
        line.push_str(" [expanded]");
    }
    if node.selected == Some(true) {
        line.push_str(" [selected]");
    }
    if let Some(level) = node.level {
        line.push_str(&format!(" [level={level}]"));
    }
    let value = node.value.as_deref().filter(|value| !value.is_empty());
    let children = tree.children(index);
    match (value, children.is_empty()) {
        (Some(value), _) => {
            line.push_str(&format!(": {}", quote(value)));
            lines.push(line);
        }
        (None, true) => lines.push(line),
        (None, false) => {
            line.push(':');
            lines.push(line);
            for &child in children {
                render(tree, child, depth + 1, lines);
            }
        }
    }
}

fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}

fn quote(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}
