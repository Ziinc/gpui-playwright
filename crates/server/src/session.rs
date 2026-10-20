//! One Playwright connection: the object graph the client sees and the
//! methods it calls on those objects.
//!
//! The graph mirrors what a Chromium server would create (`Playwright`,
//! `BrowserType`, `Browser`, `BrowserContext`, `Page`, `Frame`,
//! `ElementHandle`, ...), so stock clients accept it. A `Page` is bound to one
//! GPUI window and its main `Frame` answers every selector-based call.

use gpui::{
    AnyWindowHandle, App, AsyncApp, ElementId, Modifiers, MouseButton, Pixels, Point, px, size,
};
use gpui_playwright_selectors::{Rect, Selector, as_locator, parse, preview, query_in};
use image::RgbaImage;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::ColorScheme;
use crate::aria;
use crate::expect;
use crate::input;
use crate::screenshot;
use crate::snapshot::Snapshot;
use crate::wire::{Call, PwError, event, result_response, serialized};

const POLL: Duration = Duration::from_millis(16);

type ResetHook = Rc<dyn Fn(&mut App)>;
type OpenPageHook = Rc<dyn Fn(&mut App) -> Option<AnyWindowHandle>>;
type AdoptPageHook = Rc<dyn Fn(&[AnyWindowHandle], &mut App) -> Option<AnyWindowHandle>>;
type ColorSchemeHook = Rc<dyn Fn(ColorScheme, &mut App)>;
type NavigateHook = Rc<dyn Fn(&str, AnyWindowHandle, &mut App) -> Result<(), String>>;

/// Application callbacks the server runs on Playwright's behalf.
#[derive(Default)]
pub(crate) struct Hooks {
    pub new_context: Option<ResetHook>,
    pub new_page: Option<OpenPageHook>,
    pub adopt_page: Option<AdoptPageHook>,
    pub color_scheme: Option<ColorSchemeHook>,
    pub navigate: Option<NavigateHook>,
    #[cfg(feature = "evaluate")]
    pub functions: HashMap<String, crate::script::ExposedFunction>,
}

enum Object {
    Browser,
    Context(ContextState),
    Page(PageState),
    Frame {
        page: String,
    },
    Element {
        page: String,
        path: Vec<ElementId>,
    },
    RequestContext,
    /// A script result `evaluateHandle` or `waitForFunction` returned, kept
    /// as its `SerializedValue`. Only scripts create one.
    #[cfg_attr(not(feature = "evaluate"), allow(dead_code))]
    JsHandle {
        page: String,
        value: Value,
    },
    /// Objects that exist for the client's sake and answer calls with no-ops.
    Inert,
}

#[derive(Default)]
struct ContextState {
    pages: Vec<String>,
    viewport: Option<(f64, f64)>,
}

struct PageState {
    window: AnyWindowHandle,
    frame: String,
    context: String,
    /// Opened by the `new_page` hook, so closing the page closes the window.
    owned: bool,
    mouse: Point<Pixels>,
    button: Option<MouseButton>,
    modifiers: Modifiers,
    /// The last `goto` URL, which `page.url()` and `toHaveURL` report.
    url: String,
}

struct State {
    outgoing: mpsc::Sender<String>,
    objects: HashMap<String, Object>,
    next: u64,
    closed: bool,
}

/// A cheap handle to one connection's state.
#[derive(Clone)]
pub(crate) struct Session {
    state: Rc<RefCell<State>>,
    /// The client's Playwright minor version; 0 when it sent none.
    minor: u32,
    hooks: Rc<Hooks>,
    /// Windows that pages of any connection hold, so two clients never share one.
    claimed: Rc<RefCell<Vec<AnyWindowHandle>>>,
}

/// What a selector-based call acts on.
enum Target {
    Selector {
        selector: Selector,
        strict: bool,
        scope: Option<Vec<ElementId>>,
    },
    Handle(Vec<ElementId>),
}

impl Target {
    fn describe(&self) -> String {
        match self {
            Target::Selector { selector, .. } => as_locator(selector),
            Target::Handle(_) => "element handle".into(),
        }
    }

    fn resolve(&self, snapshot: &Snapshot) -> Vec<usize> {
        match self {
            Target::Selector {
                selector, scope, ..
            } => match scope {
                Some(path) => match snapshot.index_of(path) {
                    Some(scope) => query_in(&snapshot.tree, selector, Some(scope)),
                    None => Vec::new(),
                },
                None => query_in(&snapshot.tree, selector, None),
            },
            Target::Handle(path) => snapshot.index_of(path).into_iter().collect(),
        }
    }

    fn strict(&self) -> bool {
        matches!(self, Target::Selector { strict: true, .. })
    }

    /// The same target with strict mode on, as `expect` always resolves.
    fn into_strict(self) -> Self {
        match self {
            Target::Selector {
                selector, scope, ..
            } => Target::Selector {
                selector,
                strict: true,
                scope,
            },
            handle => handle,
        }
    }

    /// The single element a strict call acts on, or a strict-mode violation.
    fn single(&self, snapshot: &Snapshot) -> Result<Option<usize>, PwError> {
        let matches = self.resolve(snapshot);
        if self.strict() && matches.len() > 1 {
            let locator = self.describe();
            let mut message = format!(
                "Error: strict mode violation: {locator} resolved to {} elements:\n",
                matches.len()
            );
            for (i, &index) in matches.iter().enumerate().take(10) {
                let aka = if i == 0 {
                    format!("{locator}.first()")
                } else {
                    format!("{locator}.nth({i})")
                };
                message.push_str(&format!(
                    "    {}) {} aka {aka}\n",
                    i + 1,
                    preview(&snapshot.tree, index)
                ));
            }
            return Err(PwError::new(message));
        }
        Ok(matches.first().copied())
    }
}

/// One poll of a retrying operation.
enum Step<T> {
    Done(T),
    Wait(Vec<String>),
}

/// The checks an action waits for before it acts, as Playwright's actionability.
#[derive(Clone, Copy)]
struct Checks {
    enabled: bool,
    editable: bool,
    stable: bool,
}

impl Checks {
    const POINTER: Self = Self {
        enabled: true,
        editable: false,
        stable: true,
    };
    const HOVER: Self = Self {
        enabled: false,
        editable: false,
        stable: true,
    };
    const EDIT: Self = Self {
        enabled: true,
        editable: true,
        stable: false,
    };
}

impl Session {
    pub fn new(
        outgoing: mpsc::Sender<String>,
        minor: u32,
        hooks: Rc<Hooks>,
        claimed: Rc<RefCell<Vec<AnyWindowHandle>>>,
    ) -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                outgoing,
                objects: HashMap::new(),
                next: 0,
                closed: false,
            })),
            minor,
            hooks,
            claimed,
        }
    }

    fn send(&self, message: Value) {
        let _ = self.state.borrow().outgoing.send(message.to_string());
    }

    fn guid(&self, prefix: &str) -> String {
        let mut state = self.state.borrow_mut();
        state.next += 1;
        format!("{prefix}@{:032x}", state.next)
    }

    fn create(&self, parent: &str, kind: &str, guid: &str, initializer: Value, object: Object) {
        self.state
            .borrow_mut()
            .objects
            .insert(guid.to_owned(), object);
        self.send(event(
            parent,
            "__create__",
            json!({"type": kind, "initializer": initializer, "guid": guid}),
        ));
    }

    fn dispose(&self, guid: &str) {
        self.state.borrow_mut().objects.remove(guid);
        self.send(event(guid, "__dispose__", json!({})));
    }

    fn is_closed(&self) -> bool {
        self.state.borrow().closed
    }

    /// Handles one client message: runs the call on the foreground and sends its response.
    pub fn receive(&self, text: &str, cx: &mut App) {
        let call = match Call::parse(text) {
            Ok(call) => call,
            Err(err) => {
                log::warn!("gpui-playwright: dropping malformed message: {err}");
                return;
            }
        };
        let session = self.clone();
        cx.spawn(async move |cx| {
            let response = match session.dispatch(&call, cx).await {
                Ok(result) => result_response(&call.id, result),
                Err(err) => err.into_response(&call.id),
            };
            if !session.is_closed() {
                session.send(response);
            }
        })
        .detach();
    }

    /// The connection went away: close windows the session opened.
    pub fn shutdown(&self, cx: &mut App) {
        let pages: Vec<(AnyWindowHandle, bool)> = {
            let mut state = self.state.borrow_mut();
            state.closed = true;
            state
                .objects
                .values()
                .filter_map(|object| match object {
                    Object::Page(page) => Some((page.window, page.owned)),
                    _ => None,
                })
                .collect()
        };
        self.claimed
            .borrow_mut()
            .retain(|window| !pages.iter().any(|(page, _)| page == window));
        let owned = pages
            .into_iter()
            .filter_map(|(window, owned)| owned.then_some(window));
        for window in owned {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// Reports pages whose windows the application closed.
    pub fn watch(&self, cx: &mut App) {
        let open = cx.windows();
        let gone: Vec<String> = self
            .state
            .borrow()
            .objects
            .iter()
            .filter_map(|(guid, object)| match object {
                Object::Page(page) if !open.contains(&page.window) => Some(guid.clone()),
                _ => None,
            })
            .collect();
        for page in gone {
            self.page_closed(&page);
        }
    }

    /// Forgets a page and everything under it, and tells the client it closed.
    fn page_closed(&self, page: &str) {
        {
            let mut state = self.state.borrow_mut();
            let Some(Object::Page(closed)) = state.objects.get(page) else {
                return;
            };
            let (context, frame) = (closed.context.clone(), closed.frame.clone());
            let window = closed.window;
            self.claimed
                .borrow_mut()
                .retain(|claimed| *claimed != window);
            if let Some(Object::Context(context)) = state.objects.get_mut(&context) {
                context.pages.retain(|p| p != page);
            }
            state.objects.retain(|guid, object| match object {
                Object::Element { page: owner, .. } => owner != page,
                _ => *guid != frame,
            });
        }
        self.send(event(page, "close", json!({})));
        self.dispose(page);
    }

    async fn dispatch(&self, call: &Call, cx: &mut AsyncApp) -> Result<Value, PwError> {
        if call.method == "initialize" && call.guid.is_empty() {
            if self.state.borrow().objects.contains_key("Playwright") {
                return Err(PwError::new(
                    "Playwright is already initialized on this connection",
                ));
            }
            return Ok(self.initialize());
        }
        let object = {
            let state = self.state.borrow();
            match state.objects.get(&call.guid) {
                None => return Err(PwError::closed()),
                Some(Object::Browser) => "Browser",
                Some(Object::Context(_)) => "BrowserContext",
                Some(Object::Page(_)) => "Page",
                Some(Object::Frame { .. }) => "Frame",
                Some(Object::Element { .. }) => "ElementHandle",
                Some(Object::RequestContext) => "APIRequestContext",
                Some(Object::JsHandle { .. }) => "JSHandle",
                Some(Object::Inert) => "Inert",
            }
        };
        match (object, call.method.as_str()) {
            (_, "updateSubscription") => Ok(Value::Null),
            ("Browser", "newContext") => self.new_context(call, cx),
            ("Browser", "close" | "stopTracing" | "startTracing") => Ok(Value::Null),
            ("Browser", "newBrowserCDPSession") => Err(PwError::unsupported("CDP")),
            ("BrowserContext", method) => self.context_call(call, method, cx).await,
            ("Page", method) => self.page_call(call, method, cx).await,
            ("Frame" | "ElementHandle", method) => self.frame_call(call, method, cx).await,
            ("JSHandle", "jsonValue") => match self.state.borrow().objects.get(&call.guid) {
                Some(Object::JsHandle { value, .. }) => Ok(json!({"value": value})),
                _ => Err(PwError::closed()),
            },
            ("JSHandle", "dispose") => {
                self.dispose(&call.guid);
                Ok(Value::Null)
            }
            ("JSHandle", "getPropertyList") => Ok(json!({"properties": []})),
            ("JSHandle", method) => self.frame_call(call, method, cx).await,
            ("APIRequestContext", "dispose") => {
                self.dispose(&call.guid);
                Ok(Value::Null)
            }
            ("APIRequestContext", method) => {
                Err(PwError::unsupported(&format!("APIRequestContext.{method}")))
            }
            ("Inert", "tracingStopChunk") => Ok(if call.str("mode") == Some("entries") {
                json!({"entries": []})
            } else {
                Value::Null
            }),
            ("Inert", _) => Ok(Value::Null),
            (object, method) => Err(PwError::unsupported(&format!("{object}.{method}"))),
        }
    }

    fn initialize(&self) -> Value {
        let mut types = Vec::new();
        for name in ["chromium", "firefox", "webkit"] {
            let guid = self.guid("browser-type");
            self.create(
                "",
                "BrowserType",
                &guid,
                json!({"executablePath": "", "name": name}),
                Object::Inert,
            );
            types.push(guid);
        }
        let android = self.guid("android");
        self.create("", "Android", &android, json!({}), Object::Inert);
        let electron = self.guid("electron");
        self.create("", "Electron", &electron, json!({}), Object::Inert);
        let browser = self.guid("browser");
        self.create(
            &types[0],
            "Browser",
            &browser,
            json!({
                "version": format!("gpui-playwright {}", env!("CARGO_PKG_VERSION")),
                "name": "chromium",
                "browserName": "chromium",
            }),
            Object::Browser,
        );
        self.create(
            "",
            "Playwright",
            "Playwright",
            json!({
                "chromium": {"guid": types[0]},
                "firefox": {"guid": types[1]},
                "webkit": {"guid": types[2]},
                "android": {"guid": android},
                "electron": {"guid": electron},
                "preLaunchedBrowser": {"guid": browser},
            }),
            Object::Inert,
        );
        json!({"playwright": {"guid": "Playwright"}})
    }

    fn new_context(&self, call: &Call, cx: &mut AsyncApp) -> Result<Value, PwError> {
        if let Some(reset) = self.hooks.new_context.clone() {
            cx.update(|cx| reset(cx));
        }
        if let Some(scheme) = call.str("colorScheme").and_then(ColorScheme::parse) {
            self.apply_color_scheme(scheme, cx);
        }
        let browser = call.guid.clone();
        // Contexts gained a `Debugger` in 1.59.
        let debugger = (self.minor == 0 || self.minor >= 59).then(|| {
            let debugger = self.guid("debugger");
            self.create(&browser, "Debugger", &debugger, json!({}), Object::Inert);
            self.send(event(&debugger, "pausedStateChanged", json!({})));
            debugger
        });
        let request_tracing = self.guid("tracing");
        self.create(
            &browser,
            "Tracing",
            &request_tracing,
            json!({}),
            Object::Inert,
        );
        let request = self.guid("request-context");
        self.create(
            &browser,
            "APIRequestContext",
            &request,
            json!({"tracing": {"guid": request_tracing}}),
            Object::RequestContext,
        );
        let tracing = self.guid("tracing");
        self.create(&browser, "Tracing", &tracing, json!({}), Object::Inert);

        let mut options = call.params.clone();
        if !options.is_object() {
            options = json!({});
        }
        let viewport = options
            .get("viewport")
            .and_then(|v| Some((v.get("width")?.as_f64()?, v.get("height")?.as_f64()?)));
        options["acceptDownloads"] = json!("accept");
        let context = self.guid("browser-context");
        self.create(
            &browser,
            "BrowserContext",
            &context,
            json!({
                "debugger": debugger.map(|guid| json!({"guid": guid})),
                // Required before 1.59; later clients ignore it.
                "isChromium": false,
                "requestContext": {"guid": request},
                "tracing": {"guid": tracing},
                "options": options,
            }),
            Object::Context(ContextState {
                pages: Vec::new(),
                viewport,
            }),
        );
        self.send(event(
            &browser,
            "context",
            json!({"context": {"guid": context}}),
        ));
        Ok(json!({"context": {"guid": context}}))
    }

    fn apply_color_scheme(&self, scheme: ColorScheme, cx: &mut AsyncApp) {
        if let Some(apply) = self.hooks.color_scheme.clone() {
            cx.update(|cx| apply(scheme, cx));
        }
    }

    async fn context_call(
        &self,
        call: &Call,
        method: &str,
        cx: &mut AsyncApp,
    ) -> Result<Value, PwError> {
        match method {
            "newPage" => self.new_page(&call.guid, cx).await,
            "close" => {
                let pages = match self.state.borrow().objects.get(&call.guid) {
                    Some(Object::Context(context)) => context.pages.clone(),
                    _ => Vec::new(),
                };
                for page in pages {
                    self.close_page(&page, cx);
                }
                self.send(event(&call.guid, "close", json!({})));
                self.dispose(&call.guid);
                Ok(Value::Null)
            }
            "cookies" => Ok(json!({"cookies": []})),
            "storageState" => Ok(json!({"cookies": [], "origins": []})),
            "addInitScript"
            | "clearCookies"
            | "clearPermissions"
            | "grantPermissions"
            | "pause"
            | "setDefaultNavigationTimeoutNoReply"
            | "setDefaultTimeoutNoReply"
            | "setExtraHTTPHeaders"
            | "setGeolocation"
            | "setHTTPCredentials"
            | "setNetworkInterceptionPatterns"
            | "setOffline"
            | "setWebSocketInterceptionPatterns" => Ok(Value::Null),
            method => Err(PwError::unsupported(&format!("BrowserContext.{method}"))),
        }
    }

    async fn new_page(&self, context: &str, cx: &mut AsyncApp) -> Result<Value, PwError> {
        let (window, owned) = match self.hooks.new_page.clone() {
            Some(open) => {
                let window = cx.update(|cx| open(cx)).ok_or_else(|| {
                    PwError::new("The application's new page handler did not open a window")
                })?;
                (window, true)
            }
            None => {
                let bound = self.claimed.borrow().clone();
                let free: Vec<AnyWindowHandle> = cx
                    .update(|cx| cx.windows())
                    .into_iter()
                    .filter(|window| !bound.contains(window))
                    .collect();
                let picked = match self.hooks.adopt_page.clone() {
                    Some(pick) => cx.update(|cx| pick(&free, cx)),
                    None => free.first().copied(),
                };
                let window = picked
                    .ok_or_else(|| {
                        PwError::new(
                            "No window to open as a page: every application window already has a page. \
                             Register an `on_new_page` handler to open windows on demand.",
                        )
                    })?;
                (window, false)
            }
        };
        // Claim before the first await, so concurrent `newPage` calls pick different windows.
        self.claimed.borrow_mut().push(window);

        let viewport = match self.state.borrow().objects.get(context) {
            Some(Object::Context(context)) => context.viewport,
            _ => None,
        };
        if let Some((width, height)) = viewport {
            let _ = window.update(cx, |_, window, _| {
                window.resize(size(px(width as f32), px(height as f32)))
            });
        }
        settle(window, cx).await;
        let (viewport, title) = window
            .update(cx, |_, window, _| {
                (window.viewport_size(), window.window_title())
            })
            .map_err(|_| PwError::closed())?;

        let frame = self.guid("frame");
        let page = self.guid("page");
        self.create(
            context,
            "Frame",
            &frame,
            json!({"url": "about:blank", "name": title, "loadStates": ["commit", "domcontentloaded", "load"]}),
            Object::Frame { page: page.clone() },
        );
        self.create(
            context,
            "Page",
            &page,
            json!({
                "mainFrame": {"guid": frame},
                "viewportSize": {
                    "width": f32::from(viewport.width).round(),
                    "height": f32::from(viewport.height).round(),
                },
                "isClosed": false,
            }),
            Object::Page(PageState {
                window,
                frame: frame.clone(),
                context: context.to_owned(),
                owned,
                mouse: Point::default(),
                button: None,
                modifiers: Modifiers::default(),
                url: "about:blank".into(),
            }),
        );
        if let Some(Object::Context(state)) = self.state.borrow_mut().objects.get_mut(context) {
            state.pages.push(page.clone());
        }
        self.send(event(context, "page", json!({"page": {"guid": page}})));
        Ok(json!({"page": {"guid": page}}))
    }

    fn close_page(&self, page: &str, cx: &mut AsyncApp) {
        let window = match self.state.borrow().objects.get(page) {
            Some(Object::Page(state)) => state.owned.then_some(state.window),
            _ => return,
        };
        self.page_closed(page);
        if let Some(window) = window {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// The page an object belongs to, and its window.
    fn page_of(&self, guid: &str) -> Result<(String, AnyWindowHandle), PwError> {
        let state = self.state.borrow();
        let page = match state.objects.get(guid) {
            Some(Object::Page(_)) => guid.to_owned(),
            Some(Object::Frame { page })
            | Some(Object::Element { page, .. })
            | Some(Object::JsHandle { page, .. }) => page.clone(),
            _ => return Err(PwError::closed()),
        };
        match state.objects.get(&page) {
            Some(Object::Page(state)) => Ok((page, state.window)),
            _ => Err(PwError::closed()),
        }
    }

    fn with_page<R>(&self, page: &str, f: impl FnOnce(&mut PageState) -> R) -> Option<R> {
        match self.state.borrow_mut().objects.get_mut(page) {
            Some(Object::Page(state)) => Some(f(state)),
            _ => None,
        }
    }

    fn frame_of_page(&self, page: &str) -> Option<String> {
        self.with_page(page, |state| state.frame.clone())
    }

    fn element_handle(&self, page: &str, snapshot: &Snapshot, index: usize) -> Value {
        let guid = self.guid("handle");
        let frame = self.frame_of_page(page).unwrap_or_default();
        self.create(
            &frame,
            "ElementHandle",
            &guid,
            json!({"preview": preview(&snapshot.tree, index)}),
            Object::Element {
                page: page.to_owned(),
                path: snapshot.paths[index].clone(),
            },
        );
        json!({"guid": guid})
    }

    fn is_handle(&self, guid: &str) -> bool {
        matches!(
            self.state.borrow().objects.get(guid),
            Some(Object::Element { .. })
        )
    }

    /// The target of a call on a frame (by selector) or an element handle.
    fn target(&self, call: &Call) -> Result<Target, PwError> {
        let handle = match self.state.borrow().objects.get(&call.guid) {
            Some(Object::Element { path, .. }) => Some(path.clone()),
            _ => None,
        };
        match (handle, call.str("selector")) {
            (scope, Some(selector)) => Ok(Target::Selector {
                selector: parse_selector(selector)?,
                strict: call.bool("strict"),
                scope,
            }),
            (Some(path), None) => Ok(Target::Handle(path)),
            (None, None) => Err(PwError::new("selector is required")),
        }
    }

    /// Repeats `step` every frame until it finishes or the call times out.
    async fn retry<T>(
        &self,
        call: &Call,
        cx: &mut AsyncApp,
        mut step: impl FnMut(&mut AsyncApp) -> Result<Step<T>, PwError>,
    ) -> Result<T, PwError> {
        let deadline = call.deadline();
        loop {
            if self.is_closed() {
                return Err(PwError::closed());
            }
            match step(cx)? {
                Step::Done(value) => return Ok(value),
                Step::Wait(log) => {
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(PwError::timeout(call.timeout, log));
                    }
                    cx.background_executor().timer(POLL).await;
                }
            }
        }
    }

    /// Waits for the target to be actionable and returns its bounds.
    async fn actionable(
        &self,
        call: &Call,
        target: &Target,
        window: AnyWindowHandle,
        checks: Checks,
        cx: &mut AsyncApp,
    ) -> Result<Rect, PwError> {
        let force = call.bool("force");
        let locator = target.describe();
        let previous: Cell<Option<Rect>> = Cell::new(None);
        self.retry(call, cx, |cx| {
            let snapshot = capture(window, cx)?;
            let mut log = vec![format!("  - waiting for {locator}")];
            let Some(index) = target.single(&snapshot)? else {
                return Ok(Step::Wait(log));
            };
            let node = snapshot.tree.node(index);
            log.push(format!(
                "  - locator resolved to {}",
                preview(&snapshot.tree, index)
            ));
            if !force {
                let reason = if !node.visible {
                    Some("element is not visible")
                } else if checks.enabled && node.disabled {
                    Some("element is not enabled")
                } else if checks.editable && !expect::is_editable(&snapshot.tree, index) {
                    if node.disabled || node.readonly {
                        Some("element is not editable")
                    } else {
                        return Err(PwError::new(
                            "Error: Element is not an <input>, <textarea> or [contenteditable] element",
                        ));
                    }
                } else if checks.stable && previous.replace(Some(node.bounds)) != Some(node.bounds)
                {
                    Some("element is not stable")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    log.push(format!("    - {reason}"));
                    return Ok(Step::Wait(log));
                }
            }
            if !node.in_viewport {
                scroll_toward(window, &snapshot, index, cx);
                log.push("    - scrolling into view if needed".into());
                previous.set(None);
                return Ok(Step::Wait(log));
            }
            Ok(Step::Done(node.bounds))
        })
        .await
    }

    async fn frame_call(
        &self,
        call: &Call,
        method: &str,
        cx: &mut AsyncApp,
    ) -> Result<Value, PwError> {
        let (page, window) = self.page_of(&call.guid)?;
        match method {
            "click" | "dblclick" | "tap" => {
                let target = self.target(call)?;
                let bounds = self
                    .actionable(call, &target, window, Checks::POINTER, cx)
                    .await?;
                let position = point_in(&bounds, &call.params["position"]);
                let count = match method {
                    "dblclick" => 2,
                    _ => call.f64("clickCount").unwrap_or(1.) as usize,
                };
                let button = input::mouse_button(call.str("button"));
                let modifiers = self.click_modifiers(&page, call);
                self.click_at(&page, window, position, button, count, modifiers, cx);
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "hover" => {
                let target = self.target(call)?;
                let bounds = self
                    .actionable(call, &target, window, Checks::HOVER, cx)
                    .await?;
                let position = point_in(&bounds, &call.params["position"]);
                let modifiers = self.click_modifiers(&page, call);
                self.move_mouse(&page, window, position, modifiers, cx);
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "check" | "uncheck" | "setChecked" => {
                let wanted = match method {
                    "check" => true,
                    "uncheck" => false,
                    _ => call.bool("checked"),
                };
                self.set_checked(call, &page, window, wanted, cx).await
            }
            "fill" => {
                let target = self.target(call)?;
                let value = call.str("value").unwrap_or_default().to_owned();
                self.focus_target(call, &page, &target, window, Checks::EDIT, cx)
                    .await?;
                let modifiers = self.modifiers_of(&page);
                let _ = window.update(cx, |_, window, cx| {
                    input::press(window, select_all(), cx);
                    if value.is_empty() {
                        input::press(window, input::keystroke("Backspace").expect("key"), cx);
                    } else {
                        input::insert_text(window, &value, cx);
                    }
                    if modifiers != Modifiers::default() {
                        input::set_modifiers(window, modifiers, cx);
                    }
                });
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "focus" => {
                let target = self.target(call)?;
                self.focus_target(call, &page, &target, window, Checks::HOVER, cx)
                    .await?;
                Ok(Value::Null)
            }
            "press" | "type" => {
                let target = self.target(call)?;
                self.focus_target(call, &page, &target, window, Checks::POINTER, cx)
                    .await?;
                if method == "press" {
                    let key = input::keystroke(call.str("key").unwrap_or_default())?;
                    let _ = window.update(cx, |_, window, cx| input::press(window, key, cx));
                } else {
                    type_text(window, call.str("text").unwrap_or_default(), cx).await;
                }
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "scrollIntoViewIfNeeded" | "dispatchEvent" if call.str("type").is_none() => {
                Ok(Value::Null)
            }
            "waitForElementState" => {
                let target = self.target(call)?;
                let wanted = call.str("state").unwrap_or("visible").to_owned();
                self.retry(call, cx, |cx| {
                    let snapshot = capture(window, cx)?;
                    let Some(index) = target.single(&snapshot)? else {
                        return Err(PwError::new("Element is not attached to the DOM"));
                    };
                    let node = snapshot.tree.node(index);
                    let holds = match wanted.as_str() {
                        "visible" => node.visible,
                        "hidden" => !node.visible,
                        "enabled" => !node.disabled,
                        "disabled" => node.disabled,
                        "editable" => expect::is_editable(&snapshot.tree, index),
                        _ => true,
                    };
                    Ok(if holds {
                        Step::Done(())
                    } else {
                        Step::Wait(vec![format!("  - waiting for element to be {wanted}")])
                    })
                })
                .await?;
                Ok(Value::Null)
            }
            "queryCount" => {
                let target = self.target(call)?;
                let snapshot = capture(window, cx)?;
                Ok(json!({"value": target.resolve(&snapshot).len()}))
            }
            "querySelector" => {
                let target = self.target(call)?;
                let snapshot = capture(window, cx)?;
                Ok(match target.single(&snapshot)? {
                    Some(index) => {
                        json!({"element": self.element_handle(&page, &snapshot, index)})
                    }
                    None => Value::Null,
                })
            }
            "querySelectorAll" => {
                let target = self.target(call)?;
                let snapshot = capture(window, cx)?;
                let elements: Vec<Value> = target
                    .resolve(&snapshot)
                    .into_iter()
                    .map(|index| self.element_handle(&page, &snapshot, index))
                    .collect();
                Ok(json!({"elements": elements}))
            }
            "waitForSelector" => {
                let target = self.target(call)?;
                let state = call.str("state").unwrap_or("visible").to_owned();
                let locator = target.describe();
                let (snapshot, index) = self
                    .retry(call, cx, |cx| {
                        let snapshot = capture(window, cx)?;
                        let index = target.single(&snapshot)?;
                        let visible = index.is_some_and(|i| snapshot.tree.node(i).visible);
                        let done = match state.as_str() {
                            "attached" => index.is_some(),
                            "detached" => index.is_none(),
                            "hidden" => !visible,
                            _ => visible,
                        };
                        Ok(if done {
                            Step::Done((snapshot, index))
                        } else {
                            Step::Wait(vec![format!("  - waiting for {locator} to be {state}")])
                        })
                    })
                    .await?;
                Ok(match index {
                    Some(index) if matches!(state.as_str(), "attached" | "visible") => {
                        json!({"element": self.element_handle(&page, &snapshot, index)})
                    }
                    _ => Value::Null,
                })
            }
            "waitForTimeout" => {
                let ms = call
                    .f64("waitTimeout")
                    .or(call.f64("timeout"))
                    .unwrap_or(0.);
                cx.background_executor()
                    .timer(Duration::from_millis(ms as u64))
                    .await;
                Ok(Value::Null)
            }
            "textContent" | "innerText" | "innerHTML" | "inputValue" | "getAttribute"
            | "boundingBox" | "isVisible" | "isHidden" | "isEnabled" | "isDisabled"
            | "isEditable" | "isChecked" => self.read_element(call, method, window, cx).await,
            "title" => {
                let title = window
                    .update(cx, |_, window, _| Snapshot::capture(window).title)
                    .map_err(|_| PwError::closed())?;
                Ok(json!({"value": title}))
            }
            "expect" => {
                let result = self.expect(call, window, cx).await;
                self.expect_result(call.bool("isNot"), result)
            }
            // `page.ariaSnapshot()` sends no selector: snapshot the whole window.
            "ariaSnapshot" if call.str("selector").is_none() && !self.is_handle(&call.guid) => {
                let snapshot = capture(window, cx)?;
                Ok(json!({"snapshot": aria::snapshot(&snapshot.tree, None)}))
            }
            "ariaSnapshot" => {
                let target = self.target(call)?;
                let locator = target.describe();
                let (snapshot, index) = self
                    .retry(call, cx, |cx| {
                        let snapshot = capture(window, cx)?;
                        Ok(match target.single(&snapshot)? {
                            Some(index) => Step::Done((snapshot, index)),
                            None => Step::Wait(vec![format!("  - waiting for {locator}")]),
                        })
                    })
                    .await?;
                Ok(json!({"snapshot": aria::snapshot(&snapshot.tree, Some(index))}))
            }
            "screenshot" => {
                let target = self.target(call)?;
                let bounds = self
                    .actionable(call, &target, window, Checks::HOVER, cx)
                    .await?;
                let image = self.screenshot(window, &call.params, Some(bounds), cx)?;
                Ok(json!({"binary": screenshot::base64(&screenshot::png(&image))}))
            }
            "ownerFrame" => Ok(json!({"frame": {"guid": self.frame_of_page(&page)}})),
            "contentFrame" => Ok(Value::Null),
            "dispose" => {
                self.dispose(&call.guid);
                Ok(Value::Null)
            }
            "highlight" | "hideHighlight" | "selectText" | "blur" => Ok(Value::Null),
            "goto" if self.hooks.navigate.is_some() => {
                let url = call.str("url").unwrap_or_default().to_owned();
                let navigate = self.hooks.navigate.clone().expect("checked");
                cx.update(|cx| navigate(&url, window, cx))
                    .map_err(|message| PwError::new(format!("page.goto: {message}")))?;
                self.with_page(&page, |state| state.url = url.clone());
                settle(window, cx).await;
                // A same-document navigation: the client updates `page.url()`
                // and resolves `waitForURL` without waiting on a response.
                self.send(event(
                    &call.guid,
                    "navigated",
                    json!({"url": url, "name": ""}),
                ));
                Ok(Value::Null)
            }
            "goto" => Err(PwError::unsupported(
                "page.goto() without Server::on_navigate (GPUI pages have no URL)",
            )),
            "reload" | "goBack" | "goForward" | "setContent" | "addScriptTag" | "addStyleTag" => {
                Err(PwError::unsupported(&format!(
                    "page.{method}() (GPUI pages have no URL or HTML content)"
                )))
            }
            // `allInnerTexts()` and `allTextContents()` are fixed expressions
            // that read text, which needs no JavaScript.
            "evalOnSelectorAll" if text_list_expression(call.str("expression")) => {
                let target = self.target(call)?;
                let snapshot = capture(window, cx)?;
                let texts = target
                    .resolve(&snapshot)
                    .into_iter()
                    .map(|index| serialized::string(&expect::element_text(&snapshot.tree, index)))
                    .collect();
                Ok(json!({"value": serialized::array(texts)}))
            }
            #[cfg(feature = "evaluate")]
            "evaluateExpression"
            | "evaluateExpressionHandle"
            | "evalOnSelector"
            | "evalOnSelectorAll"
            | "waitForFunction" => self.script_call(call, method, &page, window, cx).await,
            #[cfg(not(feature = "evaluate"))]
            "evaluateExpression"
            | "evaluateExpressionHandle"
            | "evalOnSelector"
            | "evalOnSelectorAll"
            | "waitForFunction" => Err(PwError::unsupported(
                "JavaScript evaluation without gpui-playwright's `evaluate` feature",
            )),
            method => Err(PwError::unsupported(&format!("Frame.{method}"))),
        }
    }

    /// `evaluate` and its variants. Scripts run against a snapshot of the page
    /// taken when the call arrives; `waitForFunction` takes a new one per poll.
    #[cfg(feature = "evaluate")]
    async fn script_call(
        &self,
        call: &Call,
        method: &str,
        page: &str,
        window: AnyWindowHandle,
        cx: &mut AsyncApp,
    ) -> Result<Value, PwError> {
        use crate::script::{self, Handle, Subject};

        let expression = call.str("expression").unwrap_or_default().to_owned();
        let is_function = call.params.get("isFunction").and_then(Value::as_bool);
        let arg = call.params.get("arg").cloned().unwrap_or(Value::Null);
        let url = self
            .with_page(page, |state| state.url.clone())
            .unwrap_or_default();
        let target = match method {
            "evalOnSelector" | "evalOnSelectorAll" => Some(self.target(call)?),
            _ => None,
        };
        let this_handle = match self.state.borrow().objects.get(&call.guid) {
            Some(Object::Element { path, .. }) => Some(Ok(path.clone())),
            Some(Object::JsHandle { value, .. }) => Some(Err(value.clone())),
            _ => None,
        };
        let handle_guids: Vec<String> = arg["handles"]
            .as_array()
            .map(|handles| {
                handles
                    .iter()
                    .filter_map(|h| h["guid"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let run = |cx: &mut AsyncApp| -> Result<Value, PwError> {
            let snapshot = capture(window, cx)?;
            let index_of = |path: &[gpui::ElementId]| {
                snapshot
                    .index_of(path)
                    .ok_or_else(|| PwError::new("Element is not attached to the DOM"))
            };
            let subject = match (&target, &this_handle) {
                (Some(target), _) if method == "evalOnSelectorAll" => {
                    Subject::Elements(target.resolve(&snapshot))
                }
                (Some(target), _) => match target.single(&snapshot)? {
                    Some(index) => Subject::Element(index),
                    None => {
                        return Err(PwError::new(format!(
                            "Failed to find element matching selector \"{}\"",
                            call.str("selector").unwrap_or_default()
                        )));
                    }
                },
                (None, Some(Ok(path))) => Subject::Element(index_of(path)?),
                (None, Some(Err(value))) => Subject::Value(value.clone()),
                (None, None) => Subject::None,
            };
            let mut handles = Vec::new();
            for guid in &handle_guids {
                handles.push(match self.state.borrow().objects.get(guid) {
                    Some(Object::Element { path, .. }) => Handle::Element(index_of(path)?),
                    Some(Object::JsHandle { value, .. }) => Handle::Value(value.clone()),
                    _ => return Err(PwError::new("JSHandle is disposed")),
                });
            }
            let page = script::Page {
                tree: std::rc::Rc::new(snapshot.tree.clone()),
                title: snapshot.title.clone(),
                url: url.clone(),
                viewport: window
                    .update(cx, |_, window, _| {
                        let size = window.viewport_size();
                        (f32::from(size.width) as f64, f32::from(size.height) as f64)
                    })
                    .unwrap_or_default(),
            };
            let functions = self.hooks.functions.clone();
            cx.update(|cx| {
                script::evaluate(
                    &page,
                    &expression,
                    is_function,
                    subject,
                    &arg,
                    &handles,
                    &functions,
                    cx,
                )
            })
            .map_err(|message| PwError::new(format!("Error: {message}")))
        };
        match method {
            "waitForFunction" => {
                let value = self
                    .retry(call, cx, |cx| {
                        let value = run(cx)?;
                        Ok(if truthy(&value) {
                            Step::Done(value)
                        } else {
                            Step::Wait(vec![
                                "  - waiting for the function to return a truthy value".into(),
                            ])
                        })
                    })
                    .await?;
                Ok(json!({"handle": self.js_handle(page, value)}))
            }
            "evaluateExpressionHandle" => {
                let value = run(cx)?;
                Ok(json!({"handle": self.js_handle(page, value)}))
            }
            _ => Ok(json!({"value": run(cx)?})),
        }
    }

    #[cfg(feature = "evaluate")]
    fn js_handle(&self, page: &str, value: Value) -> Value {
        let guid = self.guid("js-handle");
        let frame = self.frame_of_page(page).unwrap_or_default();
        let preview = serde_json::to_string(&serialized::decode(&value)).unwrap_or_default();
        self.create(
            &frame,
            "JSHandle",
            &guid,
            json!({"preview": preview}),
            Object::JsHandle {
                page: page.to_owned(),
                value,
            },
        );
        json!({"guid": guid})
    }

    /// Clicks the target unless it already has focus, so key input reaches it.
    async fn focus_target(
        &self,
        call: &Call,
        page: &str,
        target: &Target,
        window: AnyWindowHandle,
        checks: Checks,
        cx: &mut AsyncApp,
    ) -> Result<(), PwError> {
        let bounds = self.actionable(call, target, window, checks, cx).await?;
        let snapshot = capture(window, cx)?;
        let focused = target
            .single(&snapshot)?
            .is_some_and(|index| snapshot.tree.node(index).focused);
        if !focused {
            let (x, y) = bounds.center();
            let modifiers = self.modifiers_of(page);
            self.click_at(
                page,
                window,
                input::pos(x, y),
                MouseButton::Left,
                1,
                modifiers,
                cx,
            );
            settle(window, cx).await;
        }
        Ok(())
    }

    async fn set_checked(
        &self,
        call: &Call,
        page: &str,
        window: AnyWindowHandle,
        wanted: bool,
        cx: &mut AsyncApp,
    ) -> Result<Value, PwError> {
        let target = self.target(call)?;
        let state = |cx: &mut AsyncApp| -> Result<Option<&'static str>, PwError> {
            let snapshot = capture(window, cx)?;
            Ok(target
                .single(&snapshot)?
                .and_then(|index| expect::checked_state(&snapshot.tree, index)))
        };
        let bounds = self
            .actionable(call, &target, window, Checks::POINTER, cx)
            .await?;
        let wanted_state = if wanted { "checked" } else { "unchecked" };
        match state(cx)? {
            None => return Err(PwError::new("Error: Not a checkbox or radio button")),
            Some(current) if current == wanted_state => return Ok(Value::Null),
            Some(_) => {}
        }
        let position = point_in(&bounds, &call.params["position"]);
        let modifiers = self.modifiers_of(page);
        self.click_at(page, window, position, MouseButton::Left, 1, modifiers, cx);
        settle(window, cx).await;
        if state(cx)? != Some(wanted_state) {
            return Err(PwError::new(
                "Error: Clicking the checkbox did not change its state",
            ));
        }
        Ok(Value::Null)
    }

    async fn read_element(
        &self,
        call: &Call,
        method: &str,
        window: AnyWindowHandle,
        cx: &mut AsyncApp,
    ) -> Result<Value, PwError> {
        let target = self.target(call)?;
        let state_query = method.starts_with("is");
        if state_query {
            // State queries answer immediately, as in Playwright.
            let snapshot = capture(window, cx)?;
            let index = target.single(&snapshot)?;
            let tree = &snapshot.tree;
            let value = match (method, index) {
                ("isVisible", index) => index.is_some_and(|i| tree.node(i).visible),
                ("isHidden", index) => index.is_none_or(|i| !tree.node(i).visible),
                (_, None) => {
                    return Err(PwError::timeout(
                        call.timeout,
                        vec![format!("  - waiting for {}", target.describe())],
                    ));
                }
                ("isEnabled", Some(i)) => !tree.node(i).disabled,
                ("isDisabled", Some(i)) => tree.node(i).disabled,
                ("isEditable", Some(i)) => expect::is_editable(tree, i),
                ("isChecked", Some(i)) => match expect::checked_state(tree, i) {
                    Some(state) => state == "checked",
                    None => return Err(PwError::new("Error: Not a checkbox or radio button")),
                },
                _ => false,
            };
            return Ok(json!({"value": value}));
        }
        let locator = target.describe();
        let (snapshot, index) = self
            .retry(call, cx, |cx| {
                let snapshot = capture(window, cx)?;
                Ok(match target.single(&snapshot)? {
                    Some(index) => Step::Done((snapshot, index)),
                    None => Step::Wait(vec![format!("  - waiting for {locator}")]),
                })
            })
            .await?;
        let tree = &snapshot.tree;
        let node = tree.node(index);
        Ok(match method {
            "textContent" | "innerText" | "innerHTML" => {
                json!({"value": expect::element_text(tree, index)})
            }
            "inputValue" => match &node.value {
                Some(value) => json!({"value": value}),
                None if expect::is_editable(tree, index) => json!({"value": ""}),
                None => {
                    return Err(PwError::new(
                        "Error: Node is not an <input>, <textarea> or <select> element",
                    ));
                }
            },
            "getAttribute" => {
                let name = call.str("name").unwrap_or_default();
                let value = match name {
                    "aria-label" => node.label.clone(),
                    "role" => node.role.clone(),
                    "value" => node.value.clone(),
                    "id" | "data-testid" => node.test_id.clone(),
                    "disabled" => node.disabled.then(String::new),
                    other => node.attributes.get(other).cloned(),
                };
                match value {
                    Some(value) => json!({"value": value}),
                    None => Value::Null,
                }
            }
            "boundingBox" => {
                let b = node.bounds;
                json!({"value": {"x": b.x, "y": b.y, "width": b.width, "height": b.height}})
            }
            _ => Value::Null,
        })
    }

    async fn expect(
        &self,
        call: &Call,
        window: AnyWindowHandle,
        cx: &mut AsyncApp,
    ) -> Result<Value, PwError> {
        // Page-level matchers such as `toHaveTitle` send no selector.
        let target = match call.str("selector") {
            None if !self.is_handle(&call.guid) => None,
            // Matchers are strict whether or not the call says so.
            _ => Some(self.target(call)?.into_strict()),
        };
        let expression = call.str("expression").unwrap_or_default().to_owned();
        let is_not = call.bool("isNot");
        let url = self
            .page_of(&call.guid)
            .ok()
            .and_then(|(page, _)| self.with_page(&page, |state| state.url.clone()))
            .unwrap_or_default();
        let locator = target
            .as_ref()
            .map(Target::describe)
            .unwrap_or_else(|| "page".into());
        let last: RefCell<(Option<Value>, Option<String>, Vec<String>)> =
            RefCell::new((None, None, Vec::new()));
        let result = self
            .retry(call, cx, |cx| {
                let snapshot = capture(window, cx)?;
                let elements = match &target {
                    Some(target) => target.resolve(&snapshot),
                    None => Vec::new(),
                };
                let mut log = vec![format!("  - waiting for {locator}")];
                if elements.is_empty() && !expect::allows_missing(&expression) {
                    *last.borrow_mut() = (None, None, log.clone());
                    return Ok(Step::Wait(log));
                }
                // Strict for single-element matchers.
                let single = !matches!(
                    expression.as_str(),
                    "to.have.count" | "to.have.text.array" | "to.have.title" | "to.have.url"
                );
                if single && let Some(target) = &target {
                    target.single(&snapshot)?;
                }
                let outcome = expect::evaluate(
                    &expression,
                    &call.params,
                    &snapshot.tree,
                    &elements,
                    &snapshot.title,
                    &url,
                )?;
                if outcome.matches != is_not {
                    return Ok(Step::Done(()));
                }
                if let Some(&first) = elements.first() {
                    log.push(format!(
                        "    locator resolved to {}",
                        preview(&snapshot.tree, first)
                    ));
                }
                if let Some(received) = &outcome.received {
                    log.push(format!(
                        "      - unexpected value {}",
                        serde_json::to_string(&serialized::decode(received)).unwrap_or_default()
                    ));
                }
                let aria = elements
                    .first()
                    .map(|&first| aria::snapshot(&snapshot.tree, Some(first)));
                *last.borrow_mut() = (outcome.received.clone(), aria, log.clone());
                Ok(Step::Wait(log))
            })
            .await;
        match result {
            Ok(()) => Ok(Value::Null),
            Err(err) if err.name != "TimeoutError" => Err(err),
            Err(_) => {
                let (received, aria, log) = last.into_inner();
                let mut details = json!({"timedOut": true});
                if received.is_some() || aria.is_some() {
                    let mut value = json!({});
                    if let Some(received) = received {
                        value["value"] = received;
                    }
                    if let Some(aria) = aria {
                        value["ariaSnapshot"] = json!(aria);
                    }
                    details["received"] = value;
                }
                let mut error = PwError::new("Expect failed")
                    .with_log(log)
                    .with_details(details);
                error.name = "ExpectError";
                Err(error)
            }
        }
    }

    /// Clients before 1.61 read an `expect` failure from the result, not an error.
    fn expect_result(
        &self,
        is_not: bool,
        result: Result<Value, PwError>,
    ) -> Result<Value, PwError> {
        if self.minor == 0 || self.minor >= 61 {
            // These clients read every `expect` error's details, so an error
            // without them crashes the matcher instead of failing it.
            return result.map_err(|err| match err.details {
                Some(_) => err,
                None => {
                    let message = err.message.trim_start_matches("Error: ").to_owned();
                    err.with_details(json!({"customErrorMessage": message}))
                }
            });
        }
        let err = match result {
            Ok(_) => return Ok(json!({"matches": !is_not})),
            Err(err) if err.name == "ExpectError" => err,
            // Like Playwright's own server, report strict mode violations as
            // the matcher's message and throw everything else.
            Err(err) if err.message.contains("strict mode violation") => {
                return Ok(json!({"matches": is_not, "errorMessage": err.message, "log": err.log}));
            }
            Err(err) => return Err(err),
        };
        let mut result = json!({"matches": is_not, "timedOut": true, "log": err.log});
        if let Some(received) = err.details.as_ref().map(|details| &details["received"])
            && received.is_object()
        {
            // 1.60 wraps the value with the aria snapshot; older clients take it bare.
            result["received"] = if self.minor >= 60 {
                received.clone()
            } else {
                received["value"].clone()
            };
            if result["received"].is_null() {
                result
                    .as_object_mut()
                    .map(|result| result.remove("received"));
            }
        }
        Ok(result)
    }

    /// Clients before 1.61 read a screenshot mismatch from the result, not an error.
    fn expect_screenshot_result(&self, result: Result<Value, PwError>) -> Result<Value, PwError> {
        if self.minor == 0 || self.minor >= 61 {
            return result;
        }
        match result {
            Err(err) if err.name == "TargetClosedError" => Err(err),
            Err(err) => {
                let mut details = err.details.unwrap_or_default();
                let message = details["customErrorMessage"]
                    .as_str()
                    .unwrap_or(&err.message)
                    .to_owned();
                let mut result = json!({"errorMessage": message, "log": err.log});
                for key in ["actual", "previous", "diff", "timedOut"] {
                    if let Some(value) = details.get_mut(key).map(Value::take) {
                        result[key] = value;
                    }
                }
                Ok(result)
            }
            ok => ok,
        }
    }

    fn modifiers_of(&self, page: &str) -> Modifiers {
        self.with_page(page, |state| state.modifiers)
            .unwrap_or_default()
    }

    /// Held keyboard modifiers plus any the call names.
    fn click_modifiers(&self, page: &str, call: &Call) -> Modifiers {
        let names: Vec<&str> = call.params["modifiers"]
            .as_array()
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let extra = input::modifiers(&names);
        let held = self.modifiers_of(page);
        Modifiers {
            control: held.control || extra.control,
            alt: held.alt || extra.alt,
            shift: held.shift || extra.shift,
            platform: held.platform || extra.platform,
            function: held.function || extra.function,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn click_at(
        &self,
        page: &str,
        window: AnyWindowHandle,
        position: Point<Pixels>,
        button: MouseButton,
        count: usize,
        modifiers: Modifiers,
        cx: &mut AsyncApp,
    ) {
        self.with_page(page, |state| state.mouse = position);
        let _ = window.update(cx, |_, window, cx| {
            input::click(window, position, button, count, modifiers, cx)
        });
    }

    fn move_mouse(
        &self,
        page: &str,
        window: AnyWindowHandle,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut AsyncApp,
    ) {
        let pressed = self
            .with_page(page, |state| {
                state.mouse = position;
                state.button
            })
            .flatten();
        let _ = window.update(cx, |_, window, cx| {
            input::mouse_move(window, position, pressed, modifiers, cx)
        });
    }

    fn screenshot(
        &self,
        window: AnyWindowHandle,
        params: &Value,
        element: Option<Rect>,
        cx: &mut AsyncApp,
    ) -> Result<RgbaImage, PwError> {
        if let Some(kind) = params["type"].as_str()
            && kind != "png"
        {
            return Err(PwError::unsupported(&format!("{kind} screenshots")));
        }
        let mask_rects = self.mask_rects(window, &params["mask"], cx)?;
        let (image, scale) = window
            .update(cx, |_, window, _| {
                screenshot::capture(window).map(|image| (image, window.scale_factor()))
            })
            .map_err(|_| PwError::closed())??;
        let mut image = image;
        let color = mask_color(params["maskColor"].as_str());
        for rect in mask_rects {
            let x0 = (rect.x * scale as f64).max(0.) as u32;
            let y0 = (rect.y * scale as f64).max(0.) as u32;
            let x1 = (((rect.x + rect.width) * scale as f64).ceil() as u32).min(image.width());
            let y1 = (((rect.y + rect.height) * scale as f64).ceil() as u32).min(image.height());
            for y in y0..y1 {
                for x in x0..x1 {
                    image.put_pixel(x, y, color);
                }
            }
        }
        let clip = element.or_else(|| {
            let clip = &params["clip"];
            Some(Rect {
                x: clip.get("x")?.as_f64()?,
                y: clip.get("y")?.as_f64()?,
                width: clip.get("width")?.as_f64()?,
                height: clip.get("height")?.as_f64()?,
            })
        });
        if let Some(clip) = clip {
            image = screenshot::crop(&image, scale, clip.x, clip.y, clip.width, clip.height);
        }
        // `scale: "css"` (the toHaveScreenshot default) wants one pixel per CSS pixel.
        if params["scale"].as_str() == Some("css") && scale != 1. {
            let width = (image.width() as f32 / scale).round().max(1.) as u32;
            let height = (image.height() as f32 / scale).round().max(1.) as u32;
            image = image::imageops::resize(
                &image,
                width,
                height,
                image::imageops::FilterType::Triangle,
            );
        }
        Ok(image)
    }

    fn mask_rects(
        &self,
        window: AnyWindowHandle,
        mask: &Value,
        cx: &mut AsyncApp,
    ) -> Result<Vec<Rect>, PwError> {
        let Some(mask) = mask.as_array().filter(|mask| !mask.is_empty()) else {
            return Ok(Vec::new());
        };
        let snapshot = capture(window, cx)?;
        let mut rects = Vec::new();
        for entry in mask {
            let selector = parse_selector(entry["selector"].as_str().unwrap_or_default())?;
            for index in query_in(&snapshot.tree, &selector, None) {
                rects.push(snapshot.tree.node(index).bounds);
            }
        }
        Ok(rects)
    }

    /// The element bounds `expectScreenshot` and friends clip to.
    fn locator_bounds(
        &self,
        window: AnyWindowHandle,
        locator: &Value,
        cx: &mut AsyncApp,
    ) -> Result<Option<Rect>, PwError> {
        let Some(selector) = locator["selector"].as_str() else {
            return Ok(None);
        };
        let target = Target::Selector {
            selector: parse_selector(selector)?,
            strict: true,
            scope: None,
        };
        let snapshot = capture(window, cx)?;
        Ok(target
            .single(&snapshot)?
            .map(|index| snapshot.tree.node(index).bounds))
    }

    async fn page_call(
        &self,
        call: &Call,
        method: &str,
        cx: &mut AsyncApp,
    ) -> Result<Value, PwError> {
        let (page, window) = self.page_of(&call.guid)?;
        match method {
            "keyboardDown" | "keyboardUp" => {
                let name = call.str("key").unwrap_or_default();
                let modifier = modifier_key(name);
                let key = match modifier {
                    Some(_) => None,
                    None => Some(input::keystroke(name)?),
                };
                let down = method == "keyboardDown";
                let modifiers = self
                    .with_page(&page, |state| {
                        if let Some(set) = modifier {
                            set(&mut state.modifiers, down);
                        }
                        state.modifiers
                    })
                    .unwrap_or_default();
                let _ = window.update(cx, |_, window, cx| match key {
                    None => input::set_modifiers(window, modifiers, cx),
                    Some(key) if down => input::key_down(window, with_held(key, modifiers), cx),
                    Some(key) => input::key_up(window, with_held(key, modifiers), cx),
                });
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "keyboardPress" => {
                let key = input::keystroke(call.str("key").unwrap_or_default())?;
                let modifiers = self.modifiers_of(&page);
                let _ = window.update(cx, |_, window, cx| {
                    input::press(window, with_held(key, modifiers), cx)
                });
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "keyboardType" | "keyboardInsertText" => {
                let text = call.str("text").unwrap_or_default();
                if method == "keyboardType" {
                    type_text(window, text, cx).await;
                } else {
                    let _ = window.update(cx, |_, window, cx| input::insert_text(window, text, cx));
                }
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "mouseMove" => {
                let to = input::pos(call.f64("x").unwrap_or(0.), call.f64("y").unwrap_or(0.));
                let from = self
                    .with_page(&page, |state| state.mouse)
                    .unwrap_or_default();
                let steps = call.f64("steps").unwrap_or(1.).max(1.) as usize;
                let modifiers = self.modifiers_of(&page);
                for step in 1..=steps {
                    let t = step as f32 / steps as f32;
                    let position = Point {
                        x: from.x + (to.x - from.x) * t,
                        y: from.y + (to.y - from.y) * t,
                    };
                    self.move_mouse(&page, window, position, modifiers, cx);
                }
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "mouseDown" | "mouseUp" => {
                let button = input::mouse_button(call.str("button"));
                let count = call.f64("clickCount").unwrap_or(1.) as usize;
                let down = method == "mouseDown";
                let (position, modifiers) = self
                    .with_page(&page, |state| {
                        state.button = down.then_some(button);
                        (state.mouse, state.modifiers)
                    })
                    .unwrap_or_default();
                let _ = window.update(cx, |_, window, cx| {
                    if down {
                        input::mouse_down(window, position, button, count, modifiers, cx)
                    } else {
                        input::mouse_up(window, position, button, count, modifiers, cx)
                    }
                });
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "mouseClick" => {
                let position = input::pos(call.f64("x").unwrap_or(0.), call.f64("y").unwrap_or(0.));
                let button = input::mouse_button(call.str("button"));
                let count = call.f64("clickCount").unwrap_or(1.) as usize;
                let modifiers = self.modifiers_of(&page);
                self.click_at(&page, window, position, button, count, modifiers, cx);
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "mouseWheel" => {
                let position = self
                    .with_page(&page, |state| state.mouse)
                    .unwrap_or_default();
                let (dx, dy) = (
                    call.f64("deltaX").unwrap_or(0.),
                    call.f64("deltaY").unwrap_or(0.),
                );
                let _ = window.update(cx, |_, window, cx| {
                    input::wheel(window, position, dx, dy, cx)
                });
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "screenshot" => {
                settle(window, cx).await;
                let image = self.screenshot(window, &call.params, None, cx)?;
                Ok(json!({"binary": screenshot::base64(&screenshot::png(&image))}))
            }
            // The client reads a failure only from error details; a bare
            // error would make `toHaveScreenshot` pass.
            "expectScreenshot" => {
                let result =
                    self.expect_screenshot(call, window, cx)
                        .await
                        .map_err(|err| match err.details {
                            Some(_) => err,
                            None => {
                                let message = err.message.clone();
                                err.with_details(json!({"customErrorMessage": message}))
                            }
                        });
                self.expect_screenshot_result(result)
            }
            "setViewportSize" => {
                let viewport = &call.params["viewportSize"];
                let width = viewport["width"].as_f64().unwrap_or(1280.);
                let height = viewport["height"].as_f64().unwrap_or(720.);
                let _ = window.update(cx, |_, window, _| {
                    window.resize(size(px(width as f32), px(height as f32)))
                });
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "bringToFront" => {
                let _ = window.update(cx, |_, window, _| window.activate_window());
                Ok(Value::Null)
            }
            "emulateMedia" => {
                if let Some(scheme) = call.str("colorScheme").and_then(ColorScheme::parse) {
                    self.apply_color_scheme(scheme, cx);
                    settle(window, cx).await;
                }
                Ok(Value::Null)
            }
            "close" => {
                self.close_page(&page, cx);
                Ok(Value::Null)
            }
            "ariaSnapshot" => {
                let snapshot = capture(window, cx)?;
                Ok(json!({"snapshot": aria::snapshot(&snapshot.tree, None)}))
            }
            "addInitScript"
            | "setExtraHTTPHeaders"
            | "setNetworkInterceptionPatterns"
            | "setWebSocketInterceptionPatterns"
            | "setDefaultNavigationTimeoutNoReply"
            | "setDefaultTimeoutNoReply"
            | "startJSCoverage"
            | "startCSSCoverage"
            | "clearConsoleMessages"
            | "clearPageErrors"
            | "hideHighlight"
            | "setDockTile"
            | "screencastChapter"
            | "screencastHideActions"
            | "screencastRemoveOverlay"
            | "screencastSetOverlayVisible"
            | "screencastShowActions" => Ok(Value::Null),
            "consoleMessages" => Ok(json!({"messages": []})),
            "pageErrors" => Ok(json!({"errors": []})),
            "requests" => Ok(json!({"requests": []})),
            "stopJSCoverage" | "stopCSSCoverage" => Ok(json!({"entries": []})),
            "touchscreenTap" => {
                let position = input::pos(call.f64("x").unwrap_or(0.), call.f64("y").unwrap_or(0.));
                let modifiers = self.modifiers_of(&page);
                self.click_at(&page, window, position, MouseButton::Left, 1, modifiers, cx);
                settle(window, cx).await;
                Ok(Value::Null)
            }
            "goBack" | "goForward" | "reload" => Err(PwError::unsupported(&format!(
                "page.{method}() (GPUI pages have no navigation history)"
            ))),
            method => Err(PwError::unsupported(&format!("Page.{method}"))),
        }
    }

    async fn expect_screenshot(
        &self,
        call: &Call,
        window: AnyWindowHandle,
        cx: &mut AsyncApp,
    ) -> Result<Value, PwError> {
        let params = &call.params;
        let expected = match params["expected"].as_str() {
            Some(expected) => Some(screenshot::decode_base64(expected)?),
            None => None,
        };
        let is_not = call.bool("isNot");
        let options = screenshot::Options {
            threshold: params["threshold"].as_f64().unwrap_or(0.2),
            max_diff_pixels: params["maxDiffPixels"].as_u64(),
            max_diff_pixel_ratio: params["maxDiffPixelRatio"].as_f64(),
        };
        let deadline = call.deadline();
        let mut previous: Option<Vec<u8>> = None;
        let mut log = vec![format!(
            "  - taking page screenshot{}",
            if params["locator"].is_object() {
                " of locator"
            } else {
                ""
            }
        )];
        loop {
            if self.is_closed() {
                return Err(PwError::closed());
            }
            settle(window, cx).await;
            let bounds = self.locator_bounds(window, &params["locator"], cx)?;
            let actual = screenshot::png(&self.screenshot(window, params, bounds, cx)?);
            let mismatch = match &expected {
                Some(expected) => {
                    let mismatch = screenshot::compare(&actual, expected, &options);
                    if mismatch.is_none() != is_not {
                        return Ok(Value::Null);
                    }
                    mismatch
                }
                // Without a baseline, wait for two identical frames in a row.
                None => {
                    if previous.as_deref() == Some(actual.as_slice()) {
                        return Ok(json!({"actual": screenshot::base64(&actual)}));
                    }
                    None
                }
            };
            let timed_out = deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if timed_out {
                let mut details = json!({
                    "actual": screenshot::base64(&actual),
                    "timedOut": true,
                });
                match mismatch {
                    Some(mismatch) => {
                        log.push(format!("  - {}", mismatch.message));
                        details["customErrorMessage"] = json!(mismatch.message);
                        if let Some(diff) = mismatch.diff {
                            details["diff"] = json!(screenshot::base64(&screenshot::png(&diff)));
                        }
                    }
                    None if expected.is_some() => {
                        details["customErrorMessage"] =
                            json!("Screenshot matched the baseline, expected it not to.");
                    }
                    None => {
                        details["customErrorMessage"] = json!(format!(
                            "Timeout {}ms exceeded while waiting for a stable screenshot.",
                            call.timeout
                        ));
                    }
                }
                if let Some(previous) = &previous {
                    details["previous"] = json!(screenshot::base64(previous));
                }
                details["log"] = json!(log);
                return Err(PwError::new("Expect failed")
                    .with_log(log)
                    .with_details(details));
            }
            previous = Some(actual);
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
        }
    }
}

/// JavaScript truthiness of a `SerializedValue`.
#[cfg(feature = "evaluate")]
fn truthy(value: &Value) -> bool {
    if let Some(v) = value.get("v").and_then(Value::as_str) {
        return matches!(v, "Infinity" | "-Infinity");
    }
    match (
        value.get("b"),
        value.get("n"),
        value.get("s"),
        value.get("bi"),
    ) {
        (Some(b), ..) => b.as_bool().unwrap_or(false),
        (_, Some(n), ..) => n.as_f64().is_some_and(|n| n != 0.),
        (_, _, Some(s), _) => s.as_str().is_some_and(|s| !s.is_empty()),
        (_, _, _, Some(bi)) => bi.as_str() != Some("0"),
        // Objects, arrays, dates and regular expressions.
        _ => true,
    }
}

/// Whether an `$$eval` expression only maps elements to their text.
fn text_list_expression(expression: Option<&str>) -> bool {
    let compact: String = expression
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | ';'))
        .collect();
    matches!(
        compact.as_str(),
        "ee=>ee.mape=>e.innerText"
            | "ee=>ee.mape=>e.textContent||\"\""
            | "ee=>ee.mape=>e.textContent||''"
    )
}

fn parse_selector(selector: &str) -> Result<Selector, PwError> {
    parse(selector).map_err(|err| {
        PwError::new(format!(
            "Error: gpui-playwright cannot use selector \"{selector}\": {err}"
        ))
    })
}

fn capture(window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<Snapshot, PwError> {
    window
        .update(cx, |_, window, cx| {
            draw_if_hidden(window, cx);
            Snapshot::capture(window)
        })
        .map_err(|_| PwError::closed())
}

/// Hidden windows, including every window of a headless app, get no frames
/// from the platform, so the server renders them itself to observe changes.
fn draw_if_hidden(window: &mut gpui::Window, cx: &mut App) -> bool {
    if window.is_visible() {
        return false;
    }
    window.draw(cx).clear(cx);
    window.simulate_next_frame(cx);
    true
}

/// Waits for two frames, so the effects of input are rendered and observable.
/// Types `text` a key per character, yielding between short runs so other
/// calls and the app keep running while a long text types.
async fn type_text(window: AnyWindowHandle, text: &str, cx: &mut AsyncApp) {
    let characters: Vec<char> = text.chars().collect();
    for run in characters.chunks(16) {
        let run: String = run.iter().collect();
        if window
            .update(cx, |_, window, cx| input::type_text(window, &run, cx))
            .is_err()
        {
            return;
        }
        cx.background_executor()
            .timer(Duration::from_millis(1))
            .await;
    }
}

async fn settle(window: AnyWindowHandle, cx: &mut AsyncApp) {
    for _ in 0..2 {
        let (sender, receiver) = async_channel::bounded::<()>(1);
        let scheduled = window.update(cx, |_, window, cx| {
            window.refresh();
            window.on_next_frame(move |_, _| {
                let _ = sender.try_send(());
            });
            draw_if_hidden(window, cx);
        });
        if scheduled.is_err() {
            return;
        }
        for _ in 0..12 {
            if receiver.try_recv().is_ok() {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(8))
                .await;
        }
    }
}

/// Scrolls the nearest on-screen ancestor of `index` toward it, as
/// `scrollIntoViewIfNeeded` does, with a wheel event over that ancestor.
fn scroll_toward(window: AnyWindowHandle, snapshot: &Snapshot, index: usize, cx: &mut AsyncApp) {
    let tree = &snapshot.tree;
    let mut ancestor = tree.parent(index);
    while let Some(parent) = ancestor {
        if tree.node(parent).in_viewport {
            break;
        }
        ancestor = tree.parent(parent);
    }
    let target = tree.node(index).bounds;
    let _ = window.update(cx, |_, window, cx| {
        let viewport = window.viewport_size();
        let screen = Rect {
            x: 0.,
            y: 0.,
            width: f32::from(viewport.width) as f64,
            height: f32::from(viewport.height) as f64,
        };
        let over = ancestor.map_or(screen, |a| intersect(&tree.node(a).bounds, &screen));
        let (ox, oy) = over.center();
        let (tx, ty) = target.center();
        input::wheel(window, input::pos(ox, oy), tx - ox, ty - oy, cx);
    });
}

fn intersect(a: &Rect, b: &Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    Rect {
        x,
        y,
        width: ((a.x + a.width).min(b.x + b.width) - x).max(0.),
        height: ((a.y + a.height).min(b.y + b.height) - y).max(0.),
    }
}

/// A point inside `bounds`: the `position` option, or the centre.
fn point_in(bounds: &Rect, position: &Value) -> Point<Pixels> {
    match (position["x"].as_f64(), position["y"].as_f64()) {
        (Some(x), Some(y)) => input::pos(bounds.x + x, bounds.y + y),
        _ => {
            let (x, y) = bounds.center();
            input::pos(x, y)
        }
    }
}

fn select_all() -> gpui::Keystroke {
    input::keystroke("ControlOrMeta+a").expect("select-all keystroke")
}

fn with_held(mut key: gpui::Keystroke, held: Modifiers) -> gpui::Keystroke {
    key.modifiers.control |= held.control;
    key.modifiers.alt |= held.alt;
    key.modifiers.shift |= held.shift;
    key.modifiers.platform |= held.platform;
    key
}

type ModifierSetter = fn(&mut Modifiers, bool);

fn modifier_key(key: &str) -> Option<ModifierSetter> {
    match key {
        "Shift" => Some(|m, down| m.shift = down),
        "Control" => Some(|m, down| m.control = down),
        "Alt" => Some(|m, down| m.alt = down),
        "Meta" => Some(|m, down| m.platform = down),
        "ControlOrMeta" => Some(|m, down| {
            if cfg!(target_os = "macos") {
                m.platform = down
            } else {
                m.control = down
            }
        }),
        _ => None,
    }
}

fn mask_color(color: Option<&str>) -> image::Rgba<u8> {
    let hex = color
        .and_then(|c| c.strip_prefix('#'))
        .filter(|hex| hex.len() == 6);
    match hex.and_then(|hex| u32::from_str_radix(hex, 16).ok()) {
        Some(rgb) => image::Rgba([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255]),
        None => image::Rgba([255, 0, 255, 255]),
    }
}
