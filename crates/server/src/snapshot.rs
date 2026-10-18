//! Builds the semantic tree Playwright's selectors run against from the
//! elements gpui-base observed in the window's last completed frame.

use gpui::{ElementId, Role, Window};
#[cfg(feature = "painted-text")]
use gpui_kit::base::test_support::TextSnapshot;
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
        let items = test_support::snapshots(window)
            .into_iter()
            .map(Item::Element);
        #[cfg(feature = "painted-text")]
        let items = items.chain(
            test_support::texts(window)
                .into_iter()
                .enumerate()
                .map(|(i, text)| Item::Text(i, text)),
        );
        let items = items.collect();
        Self::build(items, title(window))
    }

    pub fn index_of(&self, path: &[ElementId]) -> Option<usize> {
        self.paths.iter().position(|p| p.as_slice() == path)
    }

    fn build(items: Vec<Item>, title: String) -> Self {
        let item_paths: Vec<Vec<ElementId>> = items.iter().map(Item::path).collect();
        // Parent = the nearest observed element whose path is a prefix.
        let by_path: HashMap<&[ElementId], usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, Item::Element(_)))
            .map(|(i, _)| (item_paths[i].as_slice(), i))
            .collect();
        let parent_of: Vec<Option<usize>> = item_paths
            .iter()
            .map(|path| {
                (1..path.len())
                    .rev()
                    .find_map(|len| by_path.get(&path[..len]).copied())
            })
            .collect();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
        let mut roots = Vec::new();
        for (i, parent) in parent_of.iter().enumerate() {
            match parent {
                Some(p) => children[*p].push(i),
                None => roots.push(i),
            }
        }
        // An element that contains painted text shows that text rather than
        // its label.
        let shows_text: Vec<bool> = children
            .iter()
            .map(|list| list.iter().any(|&c| items[c].is_text()))
            .collect();
        // Document order: siblings top to bottom, then left to right.
        let order_key = |i: &usize| {
            let b = items[*i].bounds();
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
        let mut paths = Vec::with_capacity(items.len());
        let mut stack: Vec<(usize, Option<usize>)> =
            roots.iter().rev().map(|&r| (r, None)).collect();
        while let Some((i, parent)) = stack.pop() {
            let node = match &items[i] {
                Item::Element(snapshot) => element_node(snapshot, shows_text[i]),
                #[cfg(feature = "painted-text")]
                Item::Text(_, text) => text_node(text),
            };
            let index = tree.push(parent, node);
            paths.push(item_paths[i].clone());
            stack.extend(children[i].iter().rev().map(|&c| (c, Some(index))));
        }
        Self { tree, paths, title }
    }
}

/// What gpui-kit observed in a frame: identified elements, and the text that
/// text components painted inside them.
enum Item {
    Element(ElementSnapshot),
    #[cfg(feature = "painted-text")]
    Text(usize, TextSnapshot),
}

impl Item {
    fn is_text(&self) -> bool {
        match self {
            Item::Element(_) => false,
            #[cfg(feature = "painted-text")]
            Item::Text(..) => true,
        }
    }

    fn path(&self) -> Vec<ElementId> {
        match self {
            Item::Element(snapshot) => snapshot.path().to_vec(),
            #[cfg(feature = "painted-text")]
            // Text has no id of its own; its paint order tells texts apart.
            Item::Text(i, text) => {
                let mut path = text.path().to_vec();
                path.push(ElementId::NamedInteger(
                    "gpui-playwright-text".into(),
                    *i as u64,
                ));
                path
            }
        }
    }

    fn bounds(&self) -> gpui::Bounds<gpui::Pixels> {
        match self {
            Item::Element(snapshot) => snapshot.bounds(),
            #[cfg(feature = "painted-text")]
            Item::Text(_, text) => text.bounds(),
        }
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

fn element_node(snapshot: &ElementSnapshot, shows_text: bool) -> Node {
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
        // Without painted text inside, a label is what controls and labelled
        // elements display, except for text fields, whose label is a
        // placeholder or caption rather than their content.
        text: if shows_text || role.is_some_and(is_text_field) {
            String::new()
        } else {
            snapshot.label().unwrap_or_default().to_owned()
        },
        value: snapshot.value().map(str::to_owned),
        description: description(snapshot).map(str::to_owned),
        test_id: snapshot.path().last().map(|id| id.to_string()),
        checked,
        disabled: snapshot.disabled().unwrap_or(false),
        expanded: snapshot.expanded(),
        selected: snapshot.selected(),
        pressed: None,
        level: None,
        readonly: false,
        focused: snapshot.focused().unwrap_or(false),
        visible: snapshot.visible() || has_box(bounds),
        in_viewport: snapshot.visible(),
        bounds: Rect {
            x: f32::from(bounds.origin.x) as f64,
            y: f32::from(bounds.origin.y) as f64,
            width: f32::from(bounds.size.width) as f64,
            height: f32::from(bounds.size.height) as f64,
        },
        // `getByPlaceholder` and `getByTitle` match these. A tooltip is the
        // description gpui-kit gives a control, as `title` is on the web.
        attributes: [
            ("placeholder", placeholder(snapshot)),
            ("title", description(snapshot)),
        ]
        .into_iter()
        .filter_map(|(name, value)| Some((name.to_owned(), value?.to_owned())))
        .collect(),
    }
}

#[cfg(feature = "painted-text")]
fn description(snapshot: &ElementSnapshot) -> Option<&str> {
    snapshot.description()
}

#[cfg(not(feature = "painted-text"))]
fn description(_: &ElementSnapshot) -> Option<&str> {
    None
}

#[cfg(feature = "painted-text")]
fn placeholder(snapshot: &ElementSnapshot) -> Option<&str> {
    snapshot.placeholder()
}

#[cfg(not(feature = "painted-text"))]
fn placeholder(_: &ElementSnapshot) -> Option<&str> {
    None
}

#[cfg(feature = "painted-text")]
fn text_node(text: &TextSnapshot) -> Node {
    let bounds = text.bounds();
    let alt = text.is_alternative().then(|| text.text().to_owned());
    Node {
        // Alternative text stands in for an image, as `<img alt>` does.
        role: alt.as_ref().map(|_| "img".to_owned()),
        label: alt.clone(),
        text: if alt.is_some() {
            String::new()
        } else {
            text.text().to_owned()
        },
        visible: text.visible() || has_box(bounds),
        in_viewport: text.visible(),
        bounds: Rect {
            x: f32::from(bounds.origin.x) as f64,
            y: f32::from(bounds.origin.y) as f64,
            width: f32::from(bounds.size.width) as f64,
            height: f32::from(bounds.size.height) as f64,
        },
        attributes: alt.into_iter().map(|alt| ("alt".to_owned(), alt)).collect(),
        ..Node::default()
    }
}

/// gpui-kit reports an element as visible only when it shows on screen, so
/// one scrolled out of a container or the window reads as hidden. Playwright
/// counts any non-empty box as visible. gpui-kit does not say whether an
/// unseen element was clipped or styled invisible, and clipping is by far the
/// common case in GPUI, so a non-empty box counts as visible.
fn has_box(bounds: gpui::Bounds<gpui::Pixels>) -> bool {
    bounds.size.width > gpui::px(0.) && bounds.size.height > gpui::px(0.)
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
