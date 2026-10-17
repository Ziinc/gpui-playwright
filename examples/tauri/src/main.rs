//! The to-do example in a Tauri window: Tauri owns the event loop and the
//! window, tauri-plugin-gpui renders GPUI into it, and gpui-playwright serves
//! Playwright from inside the same GPUI app.
//!
//! ```sh
//! cd examples/tauri && GPUI_PLAYWRIGHT=9333 cargo run
//! ```

use gpui_kit::*;
use tauri_plugin_gpui::{GpuiConfig, GpuiOptions, GpuiWindowExt};

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            tauri_plugin_gpui::init_with(
                app,
                GpuiConfig::new()
                    .assets(gpui_kit::assets::Assets)
                    .on_launch(|cx| {
                        gpui_kit::init(cx);
                        todo::init(cx);
                        if let Some(server) = gpui_playwright::Server::from_env()
                            && let Err(err) = todo::playwright(server).start(cx)
                        {
                            eprintln!("gpui-playwright failed to start: {err}");
                            cx.quit();
                        }
                    }),
            )?;

            let window = tauri::WindowBuilder::new(app, "main")
                .title("Todos")
                .inner_size(560.0, 480.0)
                .build()?;
            // gpui-kit's `Root` hosts overlays and is where painted text is
            // observed, so Playwright needs it as the window's root view.
            window.attach_gpui_view(GpuiOptions::default(), |window, cx| {
                let view = cx.new(|cx| todo::TodoView::new(window, cx));
                cx.new(|cx| base::Root::new(view, window, cx))
            })?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Tauri app");
}
