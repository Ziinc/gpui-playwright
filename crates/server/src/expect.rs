//! Server-side evaluation of `expect(locator)` expressions (`Frame.expect`).

use gpui_playwright_selectors::{Checked, Tree, js_regex, normalize_whitespace};
use serde_json::Value;

use crate::wire::{PwError, serialized};

/// One entry of `expectedText`.
struct Expected {
    string: Option<String>,
    regex: Option<regex::Regex>,
    match_substring: bool,
    ignore_case: bool,
    normalize_whitespace: bool,
}

impl Expected {
    fn parse(value: &Value) -> Result<Self, PwError> {
        let ignore_case = value["ignoreCase"].as_bool().unwrap_or(false);
        let regex = match value["regexSource"].as_str() {
            Some(source) => {
                let mut flags = value["regexFlags"].as_str().unwrap_or_default().to_owned();
                if ignore_case {
                    flags.push('i');
                }
                Some(js_regex(source, &flags).map_err(|err| PwError::new(err.to_string()))?)
            }
            None => None,
        };
        Ok(Self {
            string: value["string"].as_str().map(str::to_owned),
            regex,
            match_substring: value["matchSubstring"].as_bool().unwrap_or(false),
            ignore_case,
            normalize_whitespace: value["normalizeWhiteSpace"].as_bool().unwrap_or(false),
        })
    }

    fn matches(&self, actual: &str) -> bool {
        let actual = if self.normalize_whitespace {
            normalize_whitespace(actual)
        } else {
            actual.to_owned()
        };
        if let Some(regex) = &self.regex {
            return regex.is_match(&actual);
        }
        let Some(expected) = &self.string else {
            return false;
        };
        let expected = if self.normalize_whitespace {
            normalize_whitespace(expected)
        } else {
            expected.clone()
        };
        let (actual, expected) = if self.ignore_case {
            (actual.to_lowercase(), expected.to_lowercase())
        } else {
            (actual, expected)
        };
        if self.match_substring {
            actual.contains(&expected)
        } else {
            actual == expected
        }
    }
}

pub(crate) struct Outcome {
    pub matches: bool,
    pub received: Option<Value>,
}

/// The text Playwright would read for an element. Rendered text is not
/// observed yet, so a control's label stands in for its visible content.
pub(crate) fn element_text(tree: &Tree, index: usize) -> String {
    let text = tree.text_content(index);
    if text.is_empty() {
        tree.node(index).label.clone().unwrap_or_default()
    } else {
        text
    }
}

pub(crate) fn is_editable(tree: &Tree, index: usize) -> bool {
    let node = tree.node(index);
    matches!(
        node.role.as_deref(),
        Some("textbox" | "searchbox" | "combobox" | "spinbutton")
    ) && !node.disabled
        && !node.readonly
}

pub(crate) fn checked_state(tree: &Tree, index: usize) -> Option<&'static str> {
    let node = tree.node(index);
    match node.checked {
        Some(Checked::True) => Some("checked"),
        Some(Checked::Mixed) => Some("mixed"),
        Some(Checked::False) => Some("unchecked"),
        None if matches!(
            node.role.as_deref(),
            Some("checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio")
        ) =>
        {
            Some("unchecked")
        }
        None => None,
    }
}

/// Evaluates `expression` for the matched elements (all matches, in order).
pub(crate) fn evaluate(
    expression: &str,
    params: &Value,
    tree: &Tree,
    elements: &[usize],
    title: &str,
    url: &str,
) -> Result<Outcome, PwError> {
    let expected_text: Vec<Expected> = params["expectedText"]
        .as_array()
        .map(|values| values.iter().map(Expected::parse).collect())
        .transpose()?
        .unwrap_or_default();
    let first = elements.first().copied();
    let text_outcome = |actual: Option<String>| -> Outcome {
        match actual {
            Some(actual) => Outcome {
                matches: expected_text.first().is_some_and(|e| e.matches(&actual)),
                received: Some(serialized::string(&actual)),
            },
            None => Outcome {
                matches: false,
                received: None,
            },
        }
    };
    let flag = |value: bool| Outcome {
        matches: value,
        received: Some(serialized::boolean(value)),
    };
    let state = |name: &str, holds: bool| Outcome {
        matches: holds,
        received: Some(serialized::string(if holds {
            name
        } else {
            match name {
                "visible" => "hidden",
                "hidden" => "visible",
                "enabled" => "disabled",
                "disabled" => "enabled",
                "editable" => "readOnly",
                "focused" => "inactive",
                "attached" => "detached",
                "detached" => "attached",
                "empty" => "notEmpty",
                other => other,
            }
        })),
    };

    Ok(match expression {
        "to.be.attached" => state("attached", first.is_some()),
        "to.be.detached" => state("detached", first.is_none()),
        "to.be.visible" => state("visible", first.is_some_and(|e| tree.node(e).visible)),
        "to.be.hidden" => state("hidden", first.is_none_or(|e| !tree.node(e).visible)),
        "to.be.enabled" => match first {
            Some(e) => state("enabled", !tree.node(e).disabled),
            None => Outcome {
                matches: false,
                received: None,
            },
        },
        "to.be.disabled" => match first {
            Some(e) => state("disabled", tree.node(e).disabled),
            None => Outcome {
                matches: false,
                received: None,
            },
        },
        "to.be.editable" => match first {
            Some(e) => state("editable", is_editable(tree, e)),
            None => Outcome {
                matches: false,
                received: None,
            },
        },
        "to.be.readonly" => match first {
            Some(e) => state("readOnly", !is_editable(tree, e)),
            None => Outcome {
                matches: false,
                received: None,
            },
        },
        "to.be.focused" => match first {
            Some(e) => state("focused", tree.node(e).focused),
            None => Outcome {
                matches: false,
                received: None,
            },
        },
        "to.be.empty" => match first {
            Some(e) => {
                let node = tree.node(e);
                let empty = match &node.value {
                    Some(value) => value.is_empty(),
                    None => element_text(tree, e).is_empty(),
                };
                state("empty", empty)
            }
            None => Outcome {
                matches: false,
                received: None,
            },
        },
        "to.be.in.viewport" => flag(first.is_some_and(|e| tree.node(e).in_viewport)),
        "to.be.checked" => match first.and_then(|e| checked_state(tree, e)) {
            Some(actual) => {
                let options = serialized::decode(&params["expectedValue"]["value"]);
                let wanted = if options["indeterminate"].as_bool() == Some(true) {
                    "mixed"
                } else if options["checked"].as_bool() == Some(false) {
                    "unchecked"
                } else {
                    "checked"
                };
                Outcome {
                    matches: actual == wanted,
                    received: Some(serialized::string(actual)),
                }
            }
            None if first.is_some() => {
                return Err(PwError::new("Error: Not a checkbox or radio button"));
            }
            None => Outcome {
                matches: false,
                received: None,
            },
        },
        "to.have.count" => {
            let count = elements.len() as f64;
            Outcome {
                matches: params["expectedNumber"].as_f64() == Some(count),
                received: Some(serialized::number(count)),
            }
        }
        "to.have.value" => {
            text_outcome(first.map(|e| tree.node(e).value.clone().unwrap_or_default()))
        }
        "to.have.text" => text_outcome(first.map(|e| element_text(tree, e))),
        "to.have.text.array" => {
            let texts: Vec<String> = elements.iter().map(|&e| element_text(tree, e)).collect();
            Outcome {
                matches: texts.len() == expected_text.len()
                    && texts.iter().zip(&expected_text).all(|(a, e)| e.matches(a)),
                received: Some(serialized::array(
                    texts.iter().map(|t| serialized::string(t)).collect(),
                )),
            }
        }
        "to.have.accessible.name" => text_outcome(first.map(|e| tree.accessible_name(e))),
        "to.have.accessible.description" => {
            text_outcome(first.map(|e| tree.node(e).description.clone().unwrap_or_default()))
        }
        "to.have.role" => {
            text_outcome(first.map(|e| tree.node(e).role.clone().unwrap_or_default()))
        }
        "to.have.id" => {
            text_outcome(first.map(|e| tree.node(e).test_id.clone().unwrap_or_default()))
        }
        "to.have.title" => text_outcome(Some(title.to_owned())),
        "to.have.url" => text_outcome(Some(url.to_owned())),
        "to.have.attribute.value" | "to.have.attribute" => {
            let name = params["expressionArg"].as_str().unwrap_or_default();
            let value = first.and_then(|e| {
                let node = tree.node(e);
                match name {
                    "aria-label" => node.label.clone(),
                    "role" => node.role.clone(),
                    "value" => node.value.clone(),
                    "id" | "data-testid" => node.test_id.clone(),
                    "disabled" => node.disabled.then(String::new),
                    "aria-checked" => checked_state(tree, e).map(|s| match s {
                        "checked" => "true".to_owned(),
                        "mixed" => "mixed".to_owned(),
                        _ => "false".to_owned(),
                    }),
                    other => node.attributes.get(other).cloned(),
                }
            });
            if expression == "to.have.attribute" {
                flag(value.is_some())
            } else {
                match value {
                    Some(value) => text_outcome(Some(value)),
                    None => Outcome {
                        matches: false,
                        received: Some(serialized::null()),
                    },
                }
            }
        }
        other => {
            return Err(PwError::unsupported(&format!(
                "expect expression \"{other}\""
            )));
        }
    })
}

/// Expressions that pass when nothing matches, so they must not wait for an element.
pub(crate) fn allows_missing(expression: &str) -> bool {
    matches!(
        expression,
        "to.be.detached"
            | "to.be.hidden"
            | "to.have.count"
            | "to.have.text.array"
            | "to.have.title"
            | "to.have.url"
    )
}
