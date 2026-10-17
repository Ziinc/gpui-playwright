//! A gpui-kit to-do app that stock Playwright drives through gpui-playwright.
//!
//! ```sh
//! GPUI_PLAYWRIGHT=9333 cargo run -p todo
//! ```
//!
//! Then connect any Playwright client to the printed `ws://` endpoint, or run
//! the suite in `e2e/`, which starts the app itself.

use gpui_kit::*;

fn open_main_window(cx: &mut App) -> Option<AnyWindowHandle> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(560.), px(480.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Todos".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| todo::TodoView::new(window, cx))
    }) {
        Ok((window, _)) => Some(window),
        Err(err) => {
            eprintln!("failed to open the window: {err}");
            None
        }
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            todo::init(cx);
            open_main_window(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            #[cfg(feature = "playwright")]
            if let Some(server) = gpui_playwright::Server::from_env() {
                let server = todo::playwright(server);
                if let Err(err) = server.start(cx) {
                    eprintln!("gpui-playwright failed to start: {err}");
                    cx.quit();
                }
            }
        });
}
