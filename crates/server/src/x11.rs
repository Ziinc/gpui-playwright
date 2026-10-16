//! Reads screenshots and titles back from the X server, for X11 windows
//! where GPUI cannot render a frame to an image or report its title.

use gpui::Window;
use image::RgbaImage;
use raw_window_handle::RawWindowHandle;
use std::cell::OnceCell;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, ImageFormat};
use x11rb::rust_connection::RustConnection;

thread_local! {
    static CONNECTION: OnceCell<Option<RustConnection>> = const { OnceCell::new() };
}

fn with_connection<R>(f: impl FnOnce(&RustConnection) -> Option<R>) -> Option<R> {
    CONNECTION.with(|cell| {
        cell.get_or_init(|| x11rb::connect(None).ok().map(|(connection, _)| connection))
            .as_ref()
            .and_then(f)
    })
}

/// The X11 window id behind a GPUI window, when it is an X11 window.
fn window_id(window: &Window) -> Option<u32> {
    match raw_window_handle::HasWindowHandle::window_handle(window)
        .ok()?
        .as_raw()
    {
        RawWindowHandle::Xlib(handle) => Some(handle.window as u32),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        _ => None,
    }
}

pub(crate) fn capture(window: &Window) -> Option<RgbaImage> {
    let id = window_id(window)?;
    with_connection(|connection| {
        let geometry = connection.get_geometry(id).ok()?.reply().ok()?;
        let reply = connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                id,
                0,
                0,
                geometry.width,
                geometry.height,
                !0,
            )
            .ok()?
            .reply()
            .ok()?;
        let (width, height) = (geometry.width as u32, geometry.height as u32);
        // 24- and 32-bit visuals arrive as little-endian BGRX.
        if reply.data.len() < (width * height * 4) as usize {
            return None;
        }
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for pixel in reply.data.as_chunks::<4>().0.iter().take((width * height) as usize) {
            rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
        }
        RgbaImage::from_raw(width, height, rgba)
    })
}

pub(crate) fn title(window: &Window) -> Option<String> {
    let id = window_id(window)?;
    with_connection(|connection| {
        let utf8 = connection
            .intern_atom(false, b"UTF8_STRING")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let net_wm_name = connection
            .intern_atom(false, b"_NET_WM_NAME")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let read = |property: u32, kind: u32| {
            let reply = connection
                .get_property(false, id, property, kind, 0, 1024)
                .ok()?
                .reply()
                .ok()?;
            (!reply.value.is_empty()).then(|| String::from_utf8_lossy(&reply.value).into_owned())
        };
        let title = read(net_wm_name, utf8)
            .or_else(|| read(AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()));
        let _ = connection.flush();
        title
    })
}
