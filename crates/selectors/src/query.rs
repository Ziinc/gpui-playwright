use crate::parse::{Attr, AttrValue, Part, Selector, TextMatcher};
use crate::tree::{Checked, Tree};

/// Every element matching `selector` in the whole tree, in document order.
pub fn query_all(tree: &Tree, selector: &Selector) -> Vec<usize> {
    query_in(tree, selector, None)
}

/// Matches below `scope` (exclusive), or in the whole tree when `scope` is `None`.
pub fn query_in(tree: &Tree, selector: &Selector, scope: Option<usize>) -> Vec<usize> {
    // Each entry is an element the next part searches under (or filters).
    let mut current: Vec<Option<usize>> = vec![scope];
    let mut search_scopes: Vec<Option<usize>> = vec![scope];
    for part in &selector.parts {
        if is_filter(part) {
            let elements: Vec<usize> = current.iter().flatten().copied().collect();
            current = apply_filter(tree, part, elements, &search_scopes)
                .into_iter()
                .map(Some)
                .collect();
        } else {
            search_scopes = current.clone();
            let mut found = Vec::new();
            for scope in &current {
                let candidates: Vec<usize> = match scope {
                    Some(scope) => tree.descendants(*scope),
                    None => (0..tree.len()).collect(),
                };
                found.extend(search(tree, part, candidates));
            }
            found.sort_unstable();
            found.dedup();
            current = found.into_iter().map(Some).collect();
        }
    }
    current.into_iter().flatten().collect()
}

fn is_filter(part: &Part) -> bool {
    matches!(
        part,
        Part::HasText(_)
            | Part::HasNotText(_)
            | Part::Has(_)
            | Part::HasNot(_)
            | Part::And(_)
            | Part::Or(_)
            | Part::Nth(_)
            | Part::Visible(_)
            | Part::Describe(_)
    )
}

fn apply_filter(
    tree: &Tree,
    part: &Part,
    elements: Vec<usize>,
    search_scopes: &[Option<usize>],
) -> Vec<usize> {
    match part {
        Part::HasText(matcher) => elements
            .into_iter()
            .filter(|&e| matcher.matches(&tree.text_content(e)))
            .collect(),
        Part::HasNotText(matcher) => elements
            .into_iter()
            .filter(|&e| !matcher.matches(&tree.text_content(e)))
            .collect(),
        Part::Has(inner) => elements
            .into_iter()
            .filter(|&e| !query_in(tree, inner, Some(e)).is_empty())
            .collect(),
        Part::HasNot(inner) => elements
            .into_iter()
            .filter(|&e| query_in(tree, inner, Some(e)).is_empty())
            .collect(),
        Part::And(inner) => {
            let other = in_scopes(tree, inner, search_scopes);
            elements.into_iter().filter(|e| other.contains(e)).collect()
        }
        Part::Or(inner) => {
            let mut all = elements;
            all.extend(in_scopes(tree, inner, search_scopes));
            all.sort_unstable();
            all.dedup();
            all
        }
        Part::Nth(n) => {
            let index = if *n < 0 {
                elements.len() as i64 + n
            } else {
                *n
            };
            usize::try_from(index)
                .ok()
                .and_then(|index| elements.get(index).copied())
                .into_iter()
                .collect()
        }
        Part::Visible(visible) => elements
            .into_iter()
            .filter(|&e| tree.node(e).visible == *visible)
            .collect(),
        Part::Describe(_) => elements,
        _ => unreachable!("not a filter"),
    }
}

fn in_scopes(tree: &Tree, selector: &Selector, scopes: &[Option<usize>]) -> Vec<usize> {
    let mut out: Vec<usize> = scopes
        .iter()
        .flat_map(|scope| query_in(tree, selector, *scope))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

fn search(tree: &Tree, part: &Part, candidates: Vec<usize>) -> Vec<usize> {
    match part {
        Part::Role { role, attrs } => {
            let include_hidden = attrs.iter().any(|attr| attr.name == "include-hidden");
            candidates
                .into_iter()
                .filter(|&e| {
                    let node = tree.node(e);
                    node.role.as_deref() == Some(role.as_str())
                        && (include_hidden || node.visible)
                        && attrs.iter().all(|attr| role_attr_matches(tree, e, attr))
                })
                .collect()
        }
        Part::Label(matcher) => candidates
            .into_iter()
            .filter(|&e| {
                tree.node(e)
                    .label
                    .as_deref()
                    .is_some_and(|label| matcher.matches(label))
            })
            .collect(),
        Part::Text(matcher) => {
            let matching: Vec<usize> = candidates
                .into_iter()
                .filter(|&e| matcher.matches(&tree.text_content(e)))
                .collect();
            // Keep the innermost matches, as Playwright's text engine does.
            matching
                .iter()
                .copied()
                .filter(|&e| !matching.iter().any(|&other| tree.is_ancestor(e, other)))
                .collect()
        }
        Part::TestId(matcher) => candidates
            .into_iter()
            .filter(|&e| {
                tree.node(e)
                    .test_id
                    .as_deref()
                    .is_some_and(|id| matcher.matches(id))
            })
            .collect(),
        Part::Id(id) => candidates
            .into_iter()
            .filter(|&e| tree.node(e).test_id.as_deref() == Some(id.as_str()))
            .collect(),
        Part::Attr(attr) => candidates
            .into_iter()
            .filter(|&e| {
                let node = tree.node(e);
                let actual = match attr.name.as_str() {
                    "aria-label" => node.label.as_deref(),
                    "role" => node.role.as_deref(),
                    "value" => node.value.as_deref(),
                    name => node.attributes.get(name).map(String::as_str),
                };
                match (&attr.value, actual) {
                    (None, actual) => actual.is_some(),
                    (Some(AttrValue::Text(matcher)), Some(actual)) => matcher.matches(actual),
                    (Some(AttrValue::Bool(b)), Some(actual)) => actual == b.to_string(),
                    (Some(AttrValue::Number(n)), Some(actual)) => actual == n.to_string(),
                    _ => false,
                }
            })
            .collect(),
        Part::Chain(inner) => {
            let mut out = Vec::new();
            for e in candidates {
                if inner_root_match(tree, inner, e) {
                    out.push(e);
                }
            }
            out
        }
        _ => unreachable!("not a search part"),
    }
}

/// `internal:chain` re-applies a whole selector; an element matches when it is a result.
fn inner_root_match(tree: &Tree, inner: &Selector, e: usize) -> bool {
    query_all(tree, inner).contains(&e)
}

fn role_attr_matches(tree: &Tree, e: usize, attr: &Attr) -> bool {
    let node = tree.node(e);
    let wants = |actual: Option<bool>| match &attr.value {
        None => actual == Some(true),
        Some(AttrValue::Bool(b)) => actual.unwrap_or(false) == *b,
        _ => false,
    };
    match attr.name.as_str() {
        "name" => match &attr.value {
            Some(AttrValue::Text(matcher)) => name_matches(matcher, &tree.accessible_name(e)),
            _ => false,
        },
        "checked" => match (&attr.value, node.checked) {
            (Some(AttrValue::Text(TextMatcher::Exact(v))), Some(Checked::Mixed)) => v == "mixed",
            (Some(AttrValue::Text(_)), _) => false,
            (value, checked) => {
                let checkable = matches!(
                    node.role.as_deref(),
                    Some("checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio")
                );
                let actual = checked
                    .map(|c| c == Checked::True)
                    .or(checkable.then_some(false));
                match value {
                    None => actual == Some(true),
                    Some(AttrValue::Bool(b)) => actual == Some(*b),
                    _ => false,
                }
            }
        },
        "disabled" => wants(Some(node.disabled)),
        "expanded" => wants(node.expanded),
        "selected" => wants(node.selected),
        "pressed" => wants(node.pressed),
        "level" => match &attr.value {
            Some(AttrValue::Number(n)) => node.level.map(f64::from) == Some(*n),
            _ => false,
        },
        "include-hidden" => true,
        _ => false,
    }
}

/// Role names compare like Playwright: a non-exact name is a case-insensitive substring.
fn name_matches(matcher: &TextMatcher, name: &str) -> bool {
    matcher.matches(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;
    use crate::tree::Node;

    fn node(role: &str, label: Option<&str>, text: &str) -> Node {
        Node {
            role: (!role.is_empty()).then(|| role.to_owned()),
            label: label.map(str::to_owned),
            text: text.to_owned(),
            visible: true,
            ..Default::default()
        }
    }

    fn sample() -> Tree {
        let mut tree = Tree::new();
        let root = tree.push(None, node("group", None, ""));
        let list = tree.push(Some(root), node("list", None, ""));
        let a = tree.push(Some(list), node("listitem", None, ""));
        tree.push(Some(a), node("checkbox", Some("Buy milk"), ""));
        tree.push(Some(a), node("", None, "Buy milk"));
        let b = tree.push(Some(list), node("listitem", None, ""));
        let mut done = node("checkbox", Some("Walk dog"), "");
        done.checked = Some(Checked::True);
        tree.push(Some(b), done);
        tree.push(Some(b), node("", None, "Walk dog"));
        let mut add = node("button", None, "Add");
        add.test_id = Some("add".into());
        tree.push(Some(root), add);
        let mut hidden = node("button", Some("Hidden"), "");
        hidden.visible = false;
        tree.push(Some(root), hidden);
        tree
    }

    fn q(tree: &Tree, selector: &str) -> Vec<usize> {
        query_all(tree, &parse(selector).unwrap())
    }

    #[test]
    fn role_name_and_state() {
        let tree = sample();
        assert_eq!(q(&tree, r#"internal:role=button[name="add"i]"#), vec![8]);
        assert_eq!(
            q(&tree, r#"internal:role=button[name="add"s]"#),
            Vec::<usize>::new()
        );
        assert_eq!(q(&tree, r#"internal:role=checkbox[checked=true]"#), vec![6]);
        assert_eq!(
            q(&tree, r#"internal:role=checkbox[checked=false]"#),
            vec![3]
        );
        assert_eq!(q(&tree, r#"internal:role=button"#), vec![8]);
        assert_eq!(
            q(&tree, r#"internal:role=button[include-hidden]"#),
            vec![8, 9]
        );
    }

    #[test]
    fn text_prefers_innermost() {
        let tree = sample();
        assert_eq!(q(&tree, r#"internal:text="milk"i"#), vec![4]);
        assert_eq!(q(&tree, r#"internal:text="Buy milk"s"#), vec![4]);
    }

    #[test]
    fn filters_and_chains() {
        let tree = sample();
        assert_eq!(
            q(
                &tree,
                r#"internal:role=listitem >> internal:has-text="dog"i"#
            ),
            vec![5]
        );
        assert_eq!(
            q(
                &tree,
                r#"internal:role=listitem >> internal:has="internal:role=checkbox[checked=true]""#
            ),
            vec![5]
        );
        assert_eq!(q(&tree, r#"internal:role=listitem >> nth=-1"#), vec![5]);
        assert_eq!(
            q(&tree, r#"internal:role=listitem >> internal:role=checkbox"#),
            vec![3, 6]
        );
        assert_eq!(q(&tree, r#"internal:testid=[data-testid="add"s]"#), vec![8]);
        assert_eq!(q(&tree, "#add"), vec![8]);
        assert_eq!(
            q(
                &tree,
                r#"internal:role=button >> internal:or="internal:role=list""#
            ),
            vec![1, 8]
        );
    }
}
