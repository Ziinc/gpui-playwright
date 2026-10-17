use std::collections::BTreeMap;

/// Tri-state checked value, as ARIA's `aria-checked`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checked {
    True,
    False,
    Mixed,
}

/// Window-local rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.width / 2., self.y + self.height / 2.)
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// Semantic facts about one element, as Playwright's selector engines see them.
#[derive(Clone, Debug, Default)]
pub struct Node {
    /// ARIA role name in lower case, such as `button` or `textbox`.
    pub role: Option<String>,
    /// Explicit accessible label.
    pub label: Option<String>,
    /// Text rendered by this element itself, excluding descendants.
    pub text: String,
    pub value: Option<String>,
    pub description: Option<String>,
    pub test_id: Option<String>,
    pub checked: Option<Checked>,
    pub disabled: bool,
    pub expanded: Option<bool>,
    pub selected: Option<bool>,
    pub pressed: Option<bool>,
    pub level: Option<u32>,
    pub readonly: bool,
    pub focused: bool,
    /// Rendered with a non-empty box, as Playwright's `visible`. An element
    /// scrolled or laid out outside the window is still visible.
    pub visible: bool,
    /// At least partly inside the window's viewport.
    pub in_viewport: bool,
    pub bounds: Rect,
    /// Extra attributes for `internal:attr`, such as `placeholder` or `title`.
    pub attributes: BTreeMap<String, String>,
}

/// A semantic tree in document order: every parent precedes its children.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    nodes: Vec<Node>,
    parents: Vec<Option<usize>>,
    children: Vec<Vec<usize>>,
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a node. `parent` must be an index returned earlier.
    pub fn push(&mut self, parent: Option<usize>, node: Node) -> usize {
        let index = self.nodes.len();
        if let Some(parent) = parent {
            assert!(parent < index, "a parent must precede its children");
            self.children[parent].push(index);
        }
        self.nodes.push(node);
        self.parents.push(parent);
        self.children.push(Vec::new());
        index
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, index: usize) -> &Node {
        &self.nodes[index]
    }

    pub fn parent(&self, index: usize) -> Option<usize> {
        self.parents[index]
    }

    pub fn children(&self, index: usize) -> &[usize] {
        &self.children[index]
    }

    pub fn roots(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter(|&index| self.parents[index].is_none())
    }

    /// Whether `ancestor` is a strict ancestor of `index`.
    pub fn is_ancestor(&self, ancestor: usize, mut index: usize) -> bool {
        while let Some(parent) = self.parents[index] {
            if parent == ancestor {
                return true;
            }
            index = parent;
        }
        false
    }

    /// Strict descendants of `index`, in document order.
    pub fn descendants(&self, index: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.children[index].iter().rev().copied().collect();
        while let Some(next) = stack.pop() {
            out.push(next);
            stack.extend(self.children[next].iter().rev());
        }
        out
    }

    /// Text of the element and its descendants, whitespace-normalized.
    pub fn text_content(&self, index: usize) -> String {
        let mut parts = vec![self.nodes[index].text.as_str()];
        for descendant in self.descendants(index) {
            parts.push(self.nodes[descendant].text.as_str());
        }
        normalize_whitespace(&parts.join(" "))
    }

    /// Accessible name: the explicit label, else the element's text content.
    pub fn accessible_name(&self, index: usize) -> String {
        match &self.nodes[index].label {
            Some(label) => normalize_whitespace(label),
            None => self.text_content(index),
        }
    }
}

pub fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
