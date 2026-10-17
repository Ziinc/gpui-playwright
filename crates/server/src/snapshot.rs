//! Builds the semantic tree Playwright's selectors run against from the
//! elements gpui-base observed in the window's last completed frame.

use gpui::{ElementId, Role, Window};
use gpui_kit::base::test_support::{self, ElementSnapshot};
use gpui_playwright_selectors::{Checked, Node, Rect, Tree};
use std::collections::HashMap;

/// A tree plus the GPUI identity of every node, so element handles can find
/// the same element again in later frames.
pub(crate) struct Snapshot {
    pub tree: Tree,
    pub paths: Vec<Vec<ElementId>>,
    pub title: String,
}

impl Snapshot {
    pub fn capture(window: &Window) -> Self {
        let snapshots = test_support::snapshots(window);
        Self::build(snapshots, title(window))
    }

    pub fn index_of(&self, path: &[ElementId]) -> Option<usize> {
        self.paths.iter().position(|p| p.as_slice() == path)
    }

    fn build(snapshots: Vec<ElementSnapshot>, title: String) -> Self {
        // Parent = the nearest observed element whose path is a prefix.
        let by_path: HashMap<Vec<ElementId>, usize> = snapshots
            .iter()
            .enumerate()
            .map(|(i, s)| (s.path().to_vec(), i))
            .collect();
        let parent_of: Vec<Option<usize>> = snapshots
            .iter()
            .map(|s| {
                let path = s.path();
                (1..path.len())
                    .rev()
                    .find_map(|len| by_path.get(&path[..len]).copied())
            })
            .collect();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); snapshots.len()];
        let mut roots = Vec::new();
        for (i, parent) in parent_of.iter().enumerate() {
            match parent {
                Some(p) => children[*p].push(i),
                None => roots.push(i),
            }
        }
        // Document order: siblings top to bottom, then left to right.
        let order_key = |i: &usize| {
            let b = snapshots[*i].bounds();
            (
                (f32::from(b.origin.y) * 100.) as i64,
                (f32::from(b.origin.x) * 100.) as i64,
            )
        };
        roots.sort_by_key(order_key);
        for list in &mut children {
            list.sort_by_key(order_key);
        }

        let mut tree = Tree::new();
        let mut paths = Vec::with_capacity(snapshots.len());
        let mut stack: Vec<(usize, Option<usize>)> =
            roots.iter().rev().map(|&r| (r, None)).collect();
        while let Some((i, parent)) = stack.pop() {
            let index = tree.push(parent, node(&snapshots[i]));
            paths.push(snapshots[i].path().to_vec());
            stack.extend(children[i].iter().rev().map(|&c| (c, Some(index))));
        }
        Self { tree, paths, title }
    }
}

fn title(window: &Window) -> String {
    let title = window.window_title();
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if title.is_empty() {
        return crate::x11::title(window).unwrap_or_default();
    }
    title
}

fn node(snapshot: &ElementSnapshot) -> Node {
    let bounds = snapshot.bounds();
    let role = snapshot.role().and_then(aria_role);
    let checked = match (snapshot.indeterminate(), snapshot.checked()) {
        (Some(true), _) => Some(Checked::Mixed),
        (_, Some(true)) => Some(Checked::True),
        (_, Some(false)) => Some(Checked::False),
        _ => None,
    };
    Node {
        role: role.map(str::to_owned),
        label: snapshot.label().map(str::to_owned),
        // Rendered text is not observed yet. A label is what controls and
        // labelled elements display, except for text fields, whose label is
        // a placeholder or caption rather than their content.
        text: if role.is_some_and(is_text_field) {
            String::new()
        } else {
            snapshot.label().unwrap_or_default().to_owned()
        },
        value: snapshot.value().map(str::to_owned),
        description: None,
        test_id: snapshot.path().last().map(|id| id.to_string()),
        checked,
        disabled: snapshot.disabled().unwrap_or(false),
        expanded: snapshot.expanded(),
        selected: snapshot.selected(),
        pressed: None,
        level: None,
        readonly: false,
        focused: snapshot.focused().unwrap_or(false),
        visible: snapshot.visible(),
        bounds: Rect {
            x: f32::from(bounds.origin.x) as f64,
            y: f32::from(bounds.origin.y) as f64,
            width: f32::from(bounds.size.width) as f64,
            height: f32::from(bounds.size.height) as f64,
        },
        attributes: Default::default(),
    }
}

fn is_text_field(role: &str) -> bool {
    matches!(role, "textbox" | "searchbox" | "combobox" | "spinbutton")
}

/// Maps AccessKit roles onto the ARIA role names `getByRole` uses.
fn aria_role(role: Role) -> Option<&'static str> {
    Some(match role {
        Role::Button | Role::DefaultButton => "button",
        Role::CheckBox => "checkbox",
        Role::RadioButton => "radio",
        Role::Switch => "switch",
        Role::TextInput
        | Role::MultilineTextInput
        | Role::EmailInput
        | Role::PasswordInput
        | Role::PhoneNumberInput
        | Role::UrlInput => "textbox",
        Role::SearchInput => "searchbox",
        Role::ComboBox | Role::EditableComboBox => "combobox",
        Role::Slider => "slider",
        Role::SpinButton | Role::NumberInput => "spinbutton",
        Role::Link => "link",
        Role::List => "list",
        Role::ListItem => "listitem",
        Role::ListBox => "listbox",
        Role::ListBoxOption => "option",
        Role::Menu => "menu",
        Role::MenuBar => "menubar",
        Role::MenuItem => "menuitem",
        Role::MenuItemCheckBox => "menuitemcheckbox",
        Role::MenuItemRadio => "menuitemradio",
        Role::Tab => "tab",
        Role::TabList => "tablist",
        Role::TabPanel => "tabpanel",
        Role::Dialog => "dialog",
        Role::AlertDialog => "alertdialog",
        Role::Alert => "alert",
        Role::Status => "status",
        Role::ProgressIndicator => "progressbar",
        Role::Meter => "meter",
        Role::Heading => "heading",
        Role::Image => "img",
        Role::Table => "table",
        Role::Row => "row",
        Role::Cell => "cell",
        Role::ColumnHeader => "columnheader",
        Role::RowHeader => "rowheader",
        Role::Grid => "grid",
        Role::GridCell => "gridcell",
        Role::Tree => "tree",
        Role::TreeItem => "treeitem",
        Role::TreeGrid => "treegrid",
        Role::Toolbar => "toolbar",
        Role::Tooltip => "tooltip",
        Role::Group => "group",
        Role::RadioGroup => "radiogroup",
        Role::Paragraph => "paragraph",
        Role::Navigation => "navigation",
        Role::Main => "main",
        Role::Region => "region",
        Role::Article => "article",
        Role::Form => "form",
        Role::ScrollBar => "scrollbar",
        Role::Splitter => "separator",
        Role::DisclosureTriangle => "button",
        Role::GenericContainer => "generic",
        _ => return None,
    })
}
