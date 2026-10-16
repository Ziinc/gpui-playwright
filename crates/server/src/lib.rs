//! A Playwright server inside a GPUI application.
//!
//! Stock Playwright clients (`@playwright/test`, `playwright-python`,
//! `playwright-java`, `playwright-dotnet`) connect to the running app with
//! `chromium.connect(wsEndpoint)` or `connectOptions`, and drive its windows
//! as pages: locators, actions, `expect` assertions and screenshots run
//! against the elements gpui-kit observes each frame.
//!
//! ```ignore
//! gpui_kit::application().run(|cx| {
//!     gpui_kit::init(cx);
//!     // ... open the main window ...
//!     if let Some(server) = gpui_playwright::Server::from_env() {
//!         server.start(cx).expect("start gpui-playwright");
//!     }
//! });
//! ```

// Protocol errors carry their call log and details; they are built once per
// failed call, so their size does not matter.
#![allow(clippy::result_large_err)]

mod aria;
mod expect;
mod input;
mod screenshot;
mod session;
mod snapshot;
mod transport;
mod wire;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod x11;

use gpui::{AnyWindowHandle, App};
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use session::{Hooks, Session};
use transport::Event;

pub use transport::SUPPORTED_MINORS;

/// Address variable: `127.0.0.1:9333`, or a bare port.
pub const ADDRESS_VAR: &str = "GPUI_PLAYWRIGHT";
/// Optional secret path segment clients must connect with.
pub const TOKEN_VAR: &str = "GPUI_PLAYWRIGHT_TOKEN";
/// Optional file the WebSocket endpoint is written to once listening.
pub const ENDPOINT_FILE_VAR: &str = "GPUI_PLAYWRIGHT_ENDPOINT_FILE";

/// The colour scheme Playwright asked for with `colorScheme` or `emulateMedia`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
    NoPreference,
}

impl ColorScheme {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "no-preference" => Some(Self::NoPreference),
            _ => None,
        }
    }
}

/// Builds and starts the Playwright server.
pub struct Server {
    address: SocketAddr,
    token: Option<String>,
    endpoint_file: Option<PathBuf>,
    hooks: Hooks,
}

impl Server {
    /// A server listening on `address`. Port 0 picks a free port.
    pub fn new(address: SocketAddr) -> Self {
        Self {
            address,
            token: None,
            endpoint_file: None,
            hooks: Hooks::default(),
        }
    }

    /// A server configured from `GPUI_PLAYWRIGHT`, `GPUI_PLAYWRIGHT_TOKEN` and
    /// `GPUI_PLAYWRIGHT_ENDPOINT_FILE`, or `None` when `GPUI_PLAYWRIGHT` is unset,
    /// so release builds stay closed unless the environment opts in.
    pub fn from_env() -> Option<Self> {
        let value = std::env::var(ADDRESS_VAR).ok()?;
        let value = value.trim();
        let address = match value.parse::<u16>() {
            Ok(port) => SocketAddr::from(([127, 0, 0, 1], port)),
            Err(_) => match value.to_socket_addrs().ok().and_then(|mut a| a.next()) {
                Some(address) => address,
                None => {
                    log::error!("gpui-playwright: invalid {ADDRESS_VAR}={value}");
                    return None;
                }
            },
        };
        let server = Self::new(address);
        let server = match std::env::var(TOKEN_VAR) {
            Ok(token) if !token.is_empty() => server.with_token(token),
            _ => server,
        };
        Some(match std::env::var_os(ENDPOINT_FILE_VAR) {
            Some(path) => server.with_endpoint_file(path),
            None => server,
        })
    }

    /// Requires clients to connect to `ws://host:port/<token>`.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Writes the endpoint to `path` once the server listens.
    pub fn with_endpoint_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.endpoint_file = Some(path.into());
        self
    }

    /// Runs on every `browser.newContext()`, so each test starts from a known state.
    pub fn on_new_context(mut self, reset: impl Fn(&mut App) + 'static) -> Self {
        self.hooks.new_context = Some(Rc::new(reset));
        self
    }

    /// Opens a window for `context.newPage()`. Without it, a page adopts the
    /// first application window no other page holds.
    pub fn on_new_page(
        mut self,
        open: impl Fn(&mut App) -> Option<AnyWindowHandle> + 'static,
    ) -> Self {
        self.hooks.new_page = Some(Rc::new(open));
        self
    }

    /// Applies `colorScheme` from context options and `page.emulateMedia()`.
    pub fn on_color_scheme(mut self, apply: impl Fn(ColorScheme, &mut App) + 'static) -> Self {
        self.hooks.color_scheme = Some(Rc::new(apply));
        self
    }

    /// Starts listening and returns the WebSocket endpoint clients connect to.
    /// The endpoint is also printed to stdout for test runners to read.
    pub fn start(self, cx: &mut App) -> io::Result<String> {
        let (events, incoming) = async_channel::unbounded();
        let local = transport::listen(self.address, self.token.clone(), events)?;
        let endpoint = match &self.token {
            Some(token) => format!("ws://{local}/{token}"),
            None => format!("ws://{local}/"),
        };
        if let Some(path) = &self.endpoint_file {
            std::fs::write(path, &endpoint)?;
        }
        println!("gpui-playwright listening on {endpoint}");

        let hooks = Rc::new(self.hooks);
        let sessions: Rc<std::cell::RefCell<HashMap<u64, Session>>> = Default::default();
        cx.spawn({
            let sessions = sessions.clone();
            async move |cx| {
                while let Ok(event) = incoming.recv().await {
                    match event {
                        Event::Opened {
                            connection,
                            outgoing,
                        } => {
                            sessions
                                .borrow_mut()
                                .insert(connection, Session::new(outgoing, hooks.clone()));
                        }
                        Event::Message { connection, text } => {
                            let session = sessions.borrow().get(&connection).cloned();
                            if let Some(session) = session {
                                cx.update(|cx| session.receive(&text, cx));
                            }
                        }
                        Event::Closed { connection } => {
                            let session = sessions.borrow_mut().remove(&connection);
                            if let Some(session) = session {
                                cx.update(|cx| session.shutdown(cx));
                            }
                        }
                    }
                }
            }
        })
        .detach();
        // Pages close when the application closes their windows.
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let all: Vec<Session> = sessions.borrow().values().cloned().collect();
                for session in all {
                    cx.update(|cx| session.watch(cx));
                }
            }
        })
        .detach();
        Ok(endpoint)
    }
}
