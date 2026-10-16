//! Playwright's selector language over a semantic element tree.
//!
//! Playwright clients turn `getByRole`, `getByLabel`, `getByText`,
//! `getByTestId`, `filter`, `nth` and friends into `internal:*` selector
//! strings and send them to the server. This crate parses those strings and
//! resolves them against a [`Tree`] of elements, without any GPUI dependency.

mod describe;
mod parse;
mod query;
mod tree;

pub use describe::{as_locator, preview};
pub use parse::{Attr, AttrValue, ParseError, Part, Selector, TextMatcher, js_regex, parse};
pub use query::{query_all, query_in};
pub use tree::{Checked, Node, Rect, Tree, normalize_whitespace};
