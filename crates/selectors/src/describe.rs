use crate::parse::{AttrValue, Part, Selector, TextMatcher};
use crate::tree::{Checked, Tree};

/// Renders a selector the way Playwright's JavaScript locators read, for logs
/// and error messages: `getByRole('button', { name: 'Save' }).first()`.
pub fn as_locator(selector: &Selector) -> String {
    let mut out = String::new();
    for part in &selector.parts {
        let piece = match part {
            Part::Role { role, attrs } => {
                let mut options = Vec::new();
                for attr in attrs {
                    match (&attr.name[..], &attr.value) {
                        ("include-hidden", _) => options.push("includeHidden: true".to_owned()),
                        ("name", Some(AttrValue::Text(m))) => {
                            options.push(format!("name: {}", text_arg(m)));
                            if m.is_exact() {
                                options.push("exact: true".to_owned());
                            }
                        }
                        (name, Some(AttrValue::Bool(b))) => options.push(format!("{name}: {b}")),
                        (name, Some(AttrValue::Number(n))) => options.push(format!("{name}: {n}")),
                        (name, Some(AttrValue::Text(m))) => {
                            options.push(format!("{name}: {}", text_arg(m)))
                        }
                        (name, None) => options.push(format!("{name}: true")),
                    }
                }
                if options.is_empty() {
                    format!("getByRole('{role}')")
                } else {
                    format!("getByRole('{role}', {{ {} }})", options.join(", "))
                }
            }
            Part::Label(m) => format!("getByLabel({})", text_with_exact(m)),
            Part::Text(m) => format!("getByText({})", text_with_exact(m)),
            Part::TestId(m) => format!("getByTestId({})", text_arg(m)),
            Part::Attr(attr) => match &attr.value {
                Some(AttrValue::Text(m)) => match attr.name.as_str() {
                    "placeholder" => format!("getByPlaceholder({})", text_with_exact(m)),
                    "title" => format!("getByTitle({})", text_with_exact(m)),
                    "alt" => format!("getByAltText({})", text_with_exact(m)),
                    name => format!("locator('[{name}={}]')", m.describe()),
                },
                _ => format!("locator('[{}]')", attr.name),
            },
            Part::Id(id) => format!("locator('#{id}')"),
            Part::HasText(m) => format!("filter({{ hasText: {} }})", text_arg(m)),
            Part::HasNotText(m) => format!("filter({{ hasNotText: {} }})", text_arg(m)),
            Part::Has(inner) => format!("filter({{ has: {} }})", as_locator(inner)),
            Part::HasNot(inner) => format!("filter({{ hasNot: {} }})", as_locator(inner)),
            Part::And(inner) => format!("and({})", as_locator(inner)),
            Part::Or(inner) => format!("or({})", as_locator(inner)),
            Part::Chain(inner) => format!("locator({})", as_locator(inner)),
            Part::Nth(0) => "first()".to_owned(),
            Part::Nth(-1) => "last()".to_owned(),
            Part::Nth(n) => format!("nth({n})"),
            Part::Visible(v) => format!("filter({{ visible: {v} }})"),
            Part::Describe(description) => format!("describe('{description}')"),
        };
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&piece);
    }
    out
}

fn text_arg(matcher: &TextMatcher) -> String {
    match matcher {
        TextMatcher::Regex(regex) => format!("/{}/", regex.as_str()),
        TextMatcher::Exact(text) | TextMatcher::Contains(text) => {
            format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
        }
    }
}

fn text_with_exact(matcher: &TextMatcher) -> String {
    if matcher.is_exact() {
        format!("{}, {{ exact: true }}", text_arg(matcher))
    } else {
        text_arg(matcher)
    }
}

/// A short HTML-like preview of an element, like Playwright's `<button>Save</button>`.
pub fn preview(tree: &Tree, index: usize) -> String {
    let node = tree.node(index);
    let tag = node.role.as_deref().unwrap_or("element");
    let mut attrs = String::new();
    if let Some(id) = &node.test_id {
        attrs.push_str(&format!(" id=\"{id}\""));
    }
    if let Some(label) = &node.label {
        attrs.push_str(&format!(" aria-label=\"{label}\""));
    }
    match node.checked {
        Some(Checked::True) => attrs.push_str(" checked"),
        Some(Checked::Mixed) => attrs.push_str(" aria-checked=\"mixed\""),
        _ => {}
    }
    if node.disabled {
        attrs.push_str(" disabled");
    }
    let text = tree.text_content(index);
    let text: String = if text.chars().count() > 50 {
        text.chars().take(50).collect::<String>() + "…"
    } else {
        text
    };
    format!("<{tag}{attrs}>{text}</{tag}>")
}
