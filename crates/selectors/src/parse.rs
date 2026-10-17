use regex::{Regex, RegexBuilder};
use std::fmt;

use crate::tree::normalize_whitespace;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

fn error<T>(message: impl Into<String>) -> Result<T, ParseError> {
    Err(ParseError(message.into()))
}

/// A string matcher with Playwright's text semantics.
#[derive(Debug, Clone)]
pub enum TextMatcher {
    /// `"text"s`: whole, case-sensitive, whitespace-normalized.
    Exact(String),
    /// `"text"i` or unquoted: case-insensitive substring, whitespace-normalized.
    Contains(String),
    Regex(Regex),
}

impl PartialEq for TextMatcher {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Exact(a), Self::Exact(b)) | (Self::Contains(a), Self::Contains(b)) => a == b,
            (Self::Regex(a), Self::Regex(b)) => a.as_str() == b.as_str(),
            _ => false,
        }
    }
}

impl TextMatcher {
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Self::Exact(expected) => normalize_whitespace(text) == *expected,
            Self::Contains(expected) => normalize_whitespace(text)
                .to_lowercase()
                .contains(expected.as_str()),
            Self::Regex(regex) => regex.is_match(text),
        }
    }

    /// The human-readable form used in locator descriptions.
    pub fn describe(&self) -> String {
        match self {
            Self::Exact(text) | Self::Contains(text) => text.clone(),
            Self::Regex(regex) => format!("/{}/", regex.as_str()),
        }
    }

    pub fn is_exact(&self) -> bool {
        matches!(self, Self::Exact(_))
    }

    fn exact(text: &str) -> Self {
        Self::Exact(normalize_whitespace(text))
    }

    fn contains(text: &str) -> Self {
        Self::Contains(normalize_whitespace(text).to_lowercase())
    }
}

/// Builds a regex from JavaScript `source` and `flags`.
pub fn js_regex(source: &str, flags: &str) -> Result<Regex, ParseError> {
    RegexBuilder::new(source)
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .build()
        .or_else(|err| {
            error(format!(
                "invalid regular expression /{source}/{flags}: {err}"
            ))
        })
}

#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Text(TextMatcher),
    Bool(bool),
    Number(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    pub name: String,
    /// `None` for a bare `[disabled]`, which means `true`.
    pub value: Option<AttrValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Role {
        role: String,
        attrs: Vec<Attr>,
    },
    Label(TextMatcher),
    Text(TextMatcher),
    TestId(TextMatcher),
    Attr(Attr),
    /// `#id` from CSS: the element's GPUI id.
    Id(String),
    HasText(TextMatcher),
    HasNotText(TextMatcher),
    Has(Selector),
    HasNot(Selector),
    And(Selector),
    Or(Selector),
    Chain(Selector),
    Nth(i64),
    Visible(bool),
    Describe(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub parts: Vec<Part>,
}

/// Parses a Playwright selector string such as `internal:role=button[name="Save"i] >> nth=0`.
pub fn parse(selector: &str) -> Result<Selector, ParseError> {
    let mut parts = Vec::new();
    for raw in split_parts(selector)? {
        parts.push(parse_part(raw.trim())?);
    }
    if parts.is_empty() {
        return error("empty selector");
    }
    Ok(Selector { parts })
}

/// Splits on `>>` outside quotes, brackets and regex literals.
fn split_parts(selector: &str) -> Result<Vec<&str>, ParseError> {
    let bytes = selector.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote: Option<u8> = None;
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else if c == b'"' || c == b'\'' || c == b'`' {
            quote = Some(c);
        } else if c == b'[' || c == b'(' {
            depth += 1;
        } else if c == b']' || c == b')' {
            depth -= 1;
        } else if depth == 0 && c == b'>' && bytes.get(i + 1) == Some(&b'>') {
            parts.push(&selector[start..i]);
            i += 2;
            start = i;
            continue;
        }
        i += 1;
    }
    if quote.is_some() {
        return error(format!("unterminated string in selector {selector:?}"));
    }
    parts.push(&selector[start..]);
    Ok(parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect())
}

fn parse_part(part: &str) -> Result<Part, ParseError> {
    let (engine, body) = match part.find('=') {
        Some(eq)
            if part[..eq].bytes().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b':' | b'+' | b'*')
            }) && eq > 0 =>
        {
            (&part[..eq], part[eq + 1..].trim())
        }
        _ if part.starts_with('"') || part.starts_with('\'') => ("text", part),
        _ if part.starts_with("//") || part.starts_with("..") => ("xpath", part),
        _ => ("css", part),
    };
    match engine {
        "internal:role" | "role" => parse_role(body),
        "internal:label" => Ok(Part::Label(parse_text(body)?)),
        "internal:text" | "text" => Ok(Part::Text(parse_text(body)?)),
        "internal:has-text" => Ok(Part::HasText(parse_text(body)?)),
        "internal:has-not-text" => Ok(Part::HasNotText(parse_text(body)?)),
        "internal:testid" => {
            let attr = parse_single_attr(body)?;
            match attr.value {
                Some(AttrValue::Text(matcher)) => Ok(Part::TestId(matcher)),
                _ => error(format!("invalid test id selector {body:?}")),
            }
        }
        "data-testid" | "data-test-id" | "data-test" | "id" => {
            Ok(Part::TestId(TextMatcher::Exact(unquote(body).to_owned())))
        }
        "internal:attr" => Ok(Part::Attr(parse_single_attr(body)?)),
        "internal:has" => Ok(Part::Has(nested(body)?)),
        "internal:has-not" => Ok(Part::HasNot(nested(body)?)),
        "internal:and" => Ok(Part::And(nested(body)?)),
        "internal:or" => Ok(Part::Or(nested(body)?)),
        "internal:chain" => Ok(Part::Chain(nested(body)?)),
        "internal:describe" => Ok(Part::Describe(json_string(body)?)),
        "nth" => body
            .parse()
            .map(Part::Nth)
            .or_else(|_| error(format!("invalid nth {body:?}"))),
        "visible" => match body {
            "true" => Ok(Part::Visible(true)),
            "false" => Ok(Part::Visible(false)),
            _ => error(format!("invalid visible {body:?}")),
        },
        "css" => parse_css(body),
        other => error(format!(
            "selector engine \"{other}\" is not supported by gpui-playwright; use getByRole, getByLabel, getByText or getByTestId"
        )),
    }
}

fn unquote(body: &str) -> &str {
    let body = body.trim();
    if body.len() >= 2
        && ((body.starts_with('"') && body.ends_with('"'))
            || (body.starts_with('\'') && body.ends_with('\'')))
    {
        &body[1..body.len() - 1]
    } else {
        body
    }
}

fn json_string(body: &str) -> Result<String, ParseError> {
    serde_json::from_str::<String>(body)
        .or_else(|_| error(format!("expected a quoted selector, got {body:?}")))
}

fn nested(body: &str) -> Result<Selector, ParseError> {
    parse(&json_string(body)?)
}

/// Reads a quoted string at the start of `input`, honouring JSON escapes.
/// Returns the string and the rest of the input.
fn read_quoted(input: &str) -> Result<(String, &str), ParseError> {
    let quote = input.as_bytes()[0];
    let mut i = 1;
    let bytes = input.as_bytes();
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            c if c == quote => break,
            _ => i += 1,
        }
    }
    if i >= bytes.len() {
        return error(format!("unterminated string {input:?}"));
    }
    let inner = &input[1..i];
    let value = if quote == b'"' {
        serde_json::from_str::<String>(&input[..=i]).unwrap_or_else(|_| inner.to_owned())
    } else {
        inner.replace("\\'", "'").replace("\\\\", "\\")
    };
    Ok((value, &input[i + 1..]))
}

/// Reads a `/source/flags` regex literal at the start of `input`.
fn read_regex(input: &str) -> Result<(Regex, &str), ParseError> {
    let bytes = input.as_bytes();
    let mut i = 1;
    let mut in_class = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => break,
            _ => {}
        }
        i += 1;
    }
    if i >= bytes.len() {
        return error(format!("unterminated regular expression {input:?}"));
    }
    let source = &input[1..i];
    let rest = &input[i + 1..];
    let flags_end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    Ok((js_regex(source, &rest[..flags_end])?, &rest[flags_end..]))
}

/// Parses a text body: `"x"s`, `"x"i`, `"x"`, `/re/i` or unquoted text.
fn parse_text(body: &str) -> Result<TextMatcher, ParseError> {
    let (matcher, rest) = read_text_value(body)?;
    if !rest.trim().is_empty() {
        return error(format!("unexpected {rest:?} after text {body:?}"));
    }
    Ok(matcher)
}

fn read_text_value(input: &str) -> Result<(TextMatcher, &str), ParseError> {
    let input = input.trim_start();
    if input.starts_with('"') || input.starts_with('\'') {
        let (text, rest) = read_quoted(input)?;
        if let Some(rest) = rest.strip_prefix('s') {
            Ok((TextMatcher::exact(&text), rest))
        } else if let Some(rest) = rest.strip_prefix('i') {
            Ok((TextMatcher::contains(&text), rest))
        } else {
            // A quoted value without a suffix is exact in Playwright's text engine.
            Ok((TextMatcher::exact(&text), rest))
        }
    } else if input.starts_with('/') {
        let (regex, rest) = read_regex(input)?;
        Ok((TextMatcher::Regex(regex), rest))
    } else {
        Ok((TextMatcher::contains(input), ""))
    }
}

fn parse_role(body: &str) -> Result<Part, ParseError> {
    let name_end = body.find('[').unwrap_or(body.len());
    let role = body[..name_end].trim().to_lowercase();
    if role.is_empty() {
        return error("role selector needs a role name");
    }
    let mut rest = &body[name_end..];
    let mut attrs = Vec::new();
    while !rest.trim().is_empty() {
        let (attr, next) = read_attr(rest.trim_start())?;
        attrs.push(attr);
        rest = next;
    }
    Ok(Part::Role { role, attrs })
}

fn parse_single_attr(body: &str) -> Result<Attr, ParseError> {
    let (attr, rest) = read_attr(body.trim())?;
    if !rest.trim().is_empty() {
        return error(format!("unexpected {rest:?} in {body:?}"));
    }
    Ok(attr)
}

/// Reads one `[name]` or `[name=value]` attribute.
fn read_attr(input: &str) -> Result<(Attr, &str), ParseError> {
    let Some(inner) = input.strip_prefix('[') else {
        return error(format!("expected '[' in {input:?}"));
    };
    let name_end = inner
        .find(['=', ']'])
        .ok_or_else(|| ParseError(format!("unterminated attribute {input:?}")))?;
    let name = inner[..name_end].trim().to_owned();
    let after = &inner[name_end..];
    if let Some(rest) = after.strip_prefix(']') {
        return Ok((Attr { name, value: None }, rest));
    }
    let value_input = after[1..].trim_start();
    let (value, rest) = if value_input.starts_with('"')
        || value_input.starts_with('\'')
        || value_input.starts_with('/')
    {
        let (matcher, rest) = read_text_value(value_input)?;
        (AttrValue::Text(matcher), rest)
    } else {
        let end = value_input
            .find(']')
            .ok_or_else(|| ParseError(format!("unterminated attribute {input:?}")))?;
        let raw = value_input[..end].trim();
        let value = match raw {
            "true" => AttrValue::Bool(true),
            "false" => AttrValue::Bool(false),
            _ => match raw.parse::<f64>() {
                Ok(number) => AttrValue::Number(number),
                Err(_) => AttrValue::Text(TextMatcher::exact(raw)),
            },
        };
        (value, &value_input[end..])
    };
    let rest = rest
        .trim_start()
        .strip_prefix(']')
        .ok_or_else(|| ParseError(format!("expected ']' in {input:?}")))?;
    Ok((
        Attr {
            name,
            value: Some(value),
        },
        rest,
    ))
}

/// The small CSS subset GPUI elements can answer: `#id`, `[role=x]`,
/// `[data-testid=x]` and other attributes.
fn parse_css(body: &str) -> Result<Part, ParseError> {
    let body = body.trim();
    if let Some(id) = body.strip_prefix('#')
        && !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
    {
        return Ok(Part::Id(id.to_owned()));
    }
    if body.starts_with('[') {
        let attr = parse_single_attr(body)?;
        return Ok(match (attr.name.as_str(), &attr.value) {
            ("role", Some(AttrValue::Text(TextMatcher::Exact(role)))) => Part::Role {
                role: role.to_lowercase(),
                attrs: Vec::new(),
            },
            ("data-testid" | "data-test-id" | "data-test" | "id", Some(AttrValue::Text(m))) => {
                Part::TestId(m.clone())
            }
            _ => Part::Attr(attr),
        });
    }
    error(format!(
        "CSS selector {body:?} is not supported by gpui-playwright; only #id and [attribute=value] are. Use getByRole, getByLabel, getByText or getByTestId"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generated_locators() {
        let selector = parse(r#"internal:role=button[name="Add"i]"#).unwrap();
        assert_eq!(
            selector.parts,
            vec![Part::Role {
                role: "button".into(),
                attrs: vec![Attr {
                    name: "name".into(),
                    value: Some(AttrValue::Text(TextMatcher::Contains("add".into()))),
                }],
            }]
        );

        let selector = parse(r#"internal:role=button >> nth=0"#).unwrap();
        assert_eq!(selector.parts.len(), 2);
        assert_eq!(selector.parts[1], Part::Nth(0));

        let selector = parse(r#"internal:label="Name"i"#).unwrap();
        assert_eq!(
            selector.parts,
            vec![Part::Label(TextMatcher::Contains("name".into()))]
        );

        let selector = parse(r#"internal:testid=[data-testid="save"s]"#).unwrap();
        assert_eq!(
            selector.parts,
            vec![Part::TestId(TextMatcher::Exact("save".into()))]
        );
    }

    #[test]
    fn parses_role_states_and_regex() {
        let selector =
            parse(r#"internal:role=checkbox[checked=true][include-hidden][name=/do+ne/i]"#)
                .unwrap();
        let Part::Role { role, attrs } = &selector.parts[0] else {
            panic!("role");
        };
        assert_eq!(role, "checkbox");
        assert_eq!(attrs[0].value, Some(AttrValue::Bool(true)));
        assert_eq!(attrs[1].value, None);
        let Some(AttrValue::Text(TextMatcher::Regex(regex))) = &attrs[2].value else {
            panic!("regex");
        };
        assert!(regex.is_match("DOOONE"));
    }

    #[test]
    fn parses_nested_and_ignores_separators_in_strings() {
        let selector = parse(
            r#"internal:role=listitem >> internal:has="internal:text=\"a >> b\"i" >> internal:has-text=/x/"#,
        )
        .unwrap();
        assert_eq!(selector.parts.len(), 3);
        let Part::Has(inner) = &selector.parts[1] else {
            panic!("has");
        };
        assert_eq!(
            inner.parts,
            vec![Part::Text(TextMatcher::Contains("a >> b".into()))]
        );
    }

    #[test]
    fn rejects_unsupported_css() {
        assert!(parse("div > span").is_err());
        assert_eq!(parse("#save").unwrap().parts, vec![Part::Id("save".into())]);
    }
}
