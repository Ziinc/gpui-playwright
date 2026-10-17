//! `evaluate` and friends, run by an embedded QuickJS against a read-only
//! snapshot of the page. Scripts see a small DOM-like facade: `document`,
//! elements with their text, value, state, attributes and bounds,
//! `document.querySelector` with Playwright selectors, `location.href`, and the
//! functions the app exposed with `Server::expose_function`.

use gpui::App;
use gpui_playwright_selectors::{Checked, Tree, parse, query_in};
use rquickjs::{CatchResultExt, Context, Ctx, Function, Runtime, Value as JsValue};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A function the app exposes to page scripts.
pub(crate) type ExposedFunction = Rc<dyn Fn(Vec<Value>, &mut App) -> Result<Value, String>>;

/// What a script can read about the page.
pub(crate) struct Page {
    pub tree: Rc<Tree>,
    pub title: String,
    pub url: String,
    pub viewport: (f64, f64),
}

/// A value an argument's `{h: n}` refers to.
pub(crate) enum Handle {
    Element(usize),
    Value(Value),
}

/// The function's `this` or first parameter, for the `evalOnSelector` calls.
pub(crate) enum Subject {
    None,
    Element(usize),
    Elements(Vec<usize>),
    /// A JSHandle's `SerializedValue`.
    Value(Value),
}

thread_local! {
    // The app, while a script runs; exposed functions reach it through this.
    static APP: Cell<*mut App> = const { Cell::new(std::ptr::null_mut()) };
}

/// Runs `expression` and returns its result as a Playwright `SerializedValue`.
/// A function expression is called with the subject, then the argument.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate(
    page: &Page,
    expression: &str,
    is_function: Option<bool>,
    subject: Subject,
    arg: &Value,
    handles: &[Handle],
    functions: &HashMap<String, ExposedFunction>,
    cx: &mut App,
) -> Result<Value, String> {
    let runtime = Runtime::new().map_err(|err| err.to_string())?;
    // Scripts run on the app's main thread, so a runaway one must not freeze it.
    let deadline = Instant::now() + SCRIPT_TIME_LIMIT;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    runtime.set_memory_limit(SCRIPT_MEMORY_LIMIT);
    runtime.set_max_stack_size(1024 * 1024);
    let context = Context::full(&runtime).map_err(|err| err.to_string())?;
    let previous = APP.with(|app| app.replace(cx as *mut App));
    let result = context.with(|ctx| {
        run(
            &ctx,
            page,
            expression,
            is_function,
            subject,
            arg,
            handles,
            functions,
        )
    });
    APP.with(|app| app.set(previous));
    result
}

#[allow(clippy::too_many_arguments)]
fn run<'js>(
    ctx: &Ctx<'js>,
    page: &Page,
    expression: &str,
    is_function: Option<bool>,
    subject: Subject,
    arg: &Value,
    handles: &[Handle],
    functions: &HashMap<String, ExposedFunction>,
) -> Result<Value, String> {
    let fail = |ctx: &Ctx<'js>, err: rquickjs::Error| -> String {
        match ctx.catch().into_exception() {
            Some(exception) => format!(
                "{}{}",
                exception.message().unwrap_or_default(),
                exception
                    .stack()
                    .map(|stack| format!("\n{stack}"))
                    .unwrap_or_default()
            ),
            None => err.to_string(),
        }
    };
    let globals = ctx.globals();
    let tree = page.tree.clone();
    let query = Function::new(
        ctx.clone(),
        move |selector: String, scope: Option<usize>| query_json(&tree, &selector, scope),
    )
    .map_err(|err| err.to_string())?;
    globals
        .set("__gpwQuery", query)
        .map_err(|err| err.to_string())?;
    let mut exposed = Vec::new();
    for (name, function) in functions {
        let function = function.clone();
        let call = Function::new(
            ctx.clone(),
            move |args: String| -> rquickjs::Result<String> {
                let args: Vec<Value> = serde_json::from_str(&args).unwrap_or_default();
                let app = APP.with(Cell::get);
                // SAFETY: `evaluate` set the pointer from the `&mut App` it holds
                // for the whole synchronous script run, and uses `cx` for nothing
                // else meanwhile.
                let cx = unsafe { app.as_mut() }.ok_or(rquickjs::Error::Unknown)?;
                match function(args, cx) {
                    Ok(value) => Ok(value.to_string()),
                    Err(message) => Err(rquickjs::Error::new_from_js_message(
                        "value", "value", message,
                    )),
                }
            },
        )
        .map_err(|err| err.to_string())?;
        globals
            .set(format!("__gpwExposed_{name}"), call)
            .map_err(|err| err.to_string())?;
        exposed.push(name.clone());
    }

    let elements = |indices: &[usize]| -> Value {
        Value::Array(
            indices
                .iter()
                .map(|&i| element_json(&page.tree, i))
                .collect(),
        )
    };
    let setup = json!({
        "title": page.title,
        "url": page.url,
        "viewport": [page.viewport.0, page.viewport.1],
        "body": element_body(&page.tree),
        "exposed": exposed,
    });
    let subject = match subject {
        Subject::None => Value::Null,
        Subject::Element(i) => json!({"one": element_json(&page.tree, i)}),
        Subject::Elements(list) => json!({"all": elements(&list)}),
        Subject::Value(value) => json!({"value": value}),
    };
    let handles: Vec<Value> = handles
        .iter()
        .map(|handle| match handle {
            Handle::Element(i) => json!({"element": element_json(&page.tree, *i)}),
            Handle::Value(value) => json!({"value": value}),
        })
        .collect();
    ctx.eval::<(), _>(PRELUDE).map_err(|err| fail(ctx, err))?;
    let setup_fn: Function = globals.get("__gpwSetup").map_err(|err| err.to_string())?;
    setup_fn
        .call::<_, ()>((setup.to_string(),))
        .map_err(|err| fail(ctx, err))?;

    let is_function = match is_function {
        Some(flag) => flag,
        None => ctx
            .eval::<bool, _>(format!("typeof ({expression}) === 'function'"))
            .unwrap_or(false),
    };
    let source = if is_function {
        format!("({expression})")
    } else {
        format!("(() => ({expression}))")
    };
    let user: Function = ctx.eval(source).catch(ctx).map_err(|err| err.to_string())?;
    let invoke: Function = globals.get("__gpwInvoke").map_err(|err| err.to_string())?;
    let result: JsValue = invoke
        .call((
            user,
            is_function,
            subject.to_string(),
            arg.to_string(),
            Value::Array(handles).to_string(),
        ))
        .map_err(|err| fail(ctx, err))?;
    let result = match result.as_promise() {
        Some(promise) => promise
            .clone()
            .finish::<JsValue>()
            .map_err(|err| fail(ctx, err))?,
        None => result,
    };
    let serialize: Function = globals
        .get("__gpwSerialize")
        .map_err(|err| err.to_string())?;
    let text: String = serialize.call((result,)).map_err(|err| fail(ctx, err))?;
    serde_json::from_str(&text).map_err(|err| err.to_string())
}

fn query_json(tree: &Tree, selector: &str, scope: Option<usize>) -> rquickjs::Result<String> {
    let selector = parse(selector).map_err(|err| {
        rquickjs::Error::new_from_js_message("selector", "selector", err.to_string())
    })?;
    let found: Vec<Value> = query_in(tree, &selector, scope)
        .into_iter()
        .map(|i| element_json(tree, i))
        .collect();
    Ok(Value::Array(found).to_string())
}

fn element_json(tree: &Tree, index: usize) -> Value {
    let node = tree.node(index);
    json!({
        "index": index,
        "role": node.role,
        "label": node.label,
        "text": tree.text_content(index),
        "value": node.value,
        "checked": match node.checked {
            Some(Checked::True) => json!(true),
            Some(Checked::Mixed) => json!("mixed"),
            Some(Checked::False) => json!(false),
            None => Value::Null,
        },
        "disabled": node.disabled,
        "focused": node.focused,
        "testId": node.test_id,
        "attributes": node.attributes,
        "bounds": {
            "x": node.bounds.x,
            "y": node.bounds.y,
            "width": node.bounds.width,
            "height": node.bounds.height,
        },
    })
}

/// `document.body`: every top-level element's text.
fn element_body(tree: &Tree) -> Value {
    let text: Vec<String> = tree
        .roots()
        .map(|root| tree.text_content(root))
        .filter(|text| !text.is_empty())
        .collect();
    json!({
        "index": null,
        "role": "document",
        "text": text.join(" "),
        "attributes": {},
        "disabled": false,
        "focused": false,
        "bounds": {"x": 0, "y": 0, "width": 0, "height": 0},
    })
}

const PRELUDE: &str = include_str!("script_prelude.js");

/// How long one script may run before it is interrupted.
const SCRIPT_TIME_LIMIT: Duration = Duration::from_secs(10);
const SCRIPT_MEMORY_LIMIT: usize = 256 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_playwright_selectors::Node;

    fn page() -> Page {
        let mut tree = Tree::new();
        let list = tree.push(
            None,
            Node {
                test_id: Some("list".into()),
                ..Node::default()
            },
        );
        tree.push(
            Some(list),
            Node {
                role: Some("checkbox".into()),
                label: Some("Milk".into()),
                text: "Milk".into(),
                checked: Some(Checked::True),
                visible: true,
                ..Node::default()
            },
        );
        Page {
            tree: Rc::new(tree),
            title: "Todos".into(),
            url: "about:blank".into(),
            viewport: (560., 480.),
        }
    }

    fn eval(expression: &str, subject: Subject, arg: Value) -> Result<Value, String> {
        let mut functions: HashMap<String, ExposedFunction> = HashMap::new();
        functions.insert(
            "double".into(),
            Rc::new(|args, _| Ok(json!(args[0].as_f64().unwrap_or_default() * 2.))),
        );
        let cx = gpui::TestAppContext::single();
        cx.update(|cx| {
            evaluate(
                &page(),
                expression,
                None,
                subject,
                &json!({"value": arg, "handles": []}),
                &[],
                &functions,
                cx,
            )
        })
    }

    #[test]
    fn evaluates_expressions_functions_and_promises() {
        assert_eq!(
            eval("1 + 1", Subject::None, Value::Null),
            Ok(json!({"n": 2}))
        );
        assert_eq!(
            eval("document.title", Subject::None, Value::Null),
            Ok(json!({"s": "Todos"}))
        );
        assert_eq!(
            eval(
                "(a) => a.x * 3",
                Subject::None,
                json!({"o": [{"k": "x", "v": {"n": 2}}], "id": 1})
            ),
            Ok(json!({"n": 6}))
        );
        assert_eq!(
            eval("async () => 7", Subject::None, Value::Null),
            Ok(json!({"n": 7}))
        );
        assert_eq!(
            eval("() => undefined", Subject::None, Value::Null),
            Ok(json!({"v": "undefined"}))
        );
        assert_eq!(
            eval("double(21)", Subject::None, Value::Null),
            Ok(json!({"n": 42}))
        );
    }

    #[test]
    fn reads_elements() {
        assert_eq!(
            eval(
                "(e) => [e.textContent, e.checked, e.getAttribute('aria-label')]",
                Subject::Element(1),
                Value::Null
            ),
            Ok(json!({"a": [{"s": "Milk"}, {"b": true}, {"s": "Milk"}], "id": 1}))
        );
        assert_eq!(
            eval(
                "document.querySelectorAll('internal:role=checkbox').length",
                Subject::None,
                Value::Null
            ),
            Ok(json!({"n": 1}))
        );
        assert!(
            eval(
                "() => { throw new Error('boom') }",
                Subject::None,
                Value::Null
            )
            .unwrap_err()
            .contains("boom")
        );
    }
}
