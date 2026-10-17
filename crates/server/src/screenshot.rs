//! PNG capture and Playwright-compatible image comparison.

use base64::Engine as _;
use gpui::Window;
use image::{ImageFormat, Rgba, RgbaImage};
use std::io::Cursor;

use crate::wire::PwError;

pub(crate) fn capture(window: &Window) -> Result<RgbaImage, PwError> {
    let rendered = window.render_to_image();
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if rendered.is_err()
        && let Some(image) = crate::x11::capture(window)
    {
        return Ok(image);
    }
    rendered.map_err(|err| {
        let hint = if window.is_visible() {
            ""
        } else {
            " (the window is hidden or the app runs headless; screenshots need a visible window on a display)"
        };
        PwError::new(format!("Taking a screenshot failed: {err}{hint}"))
    })
}

pub(crate) fn crop(image: &RgbaImage, scale: f32, x: f64, y: f64, w: f64, h: f64) -> RgbaImage {
    let s = scale as f64;
    let x = ((x * s).max(0.) as u32).min(image.width());
    let y = ((y * s).max(0.) as u32).min(image.height());
    let w = ((w * s).ceil() as u32).min(image.width() - x);
    let h = ((h * s).ceil() as u32).min(image.height() - y);
    image::imageops::crop_imm(image, x, y, w, h).to_image()
}

pub(crate) fn png(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("encoding PNG into memory");
    bytes
}

pub(crate) fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(crate) fn decode_base64(text: &str) -> Result<Vec<u8>, PwError> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|err| PwError::new(format!("invalid base64 image: {err}")))
}

pub(crate) struct Options {
    /// Per-channel colour distance in 0..1 under which pixels count as equal.
    pub threshold: f64,
    pub max_diff_pixels: Option<u64>,
    pub max_diff_pixel_ratio: Option<f64>,
}

pub(crate) struct Mismatch {
    pub message: String,
    pub diff: Option<RgbaImage>,
}

/// Compares two PNGs. Returns `None` when they match within the options.
pub(crate) fn compare(actual: &[u8], expected: &[u8], options: &Options) -> Option<Mismatch> {
    let (Ok(actual), Ok(expected)) = (
        image::load_from_memory_with_format(actual, ImageFormat::Png),
        image::load_from_memory_with_format(expected, ImageFormat::Png),
    ) else {
        return Some(Mismatch {
            message: "Could not decode the screenshot as PNG.".into(),
            diff: None,
        });
    };
    let (actual, expected) = (actual.to_rgba8(), expected.to_rgba8());
    if actual.dimensions() != expected.dimensions() {
        return Some(Mismatch {
            message: format!(
                "Expected an image {}px by {}px, received {}px by {}px.",
                expected.width(),
                expected.height(),
                actual.width(),
                actual.height()
            ),
            diff: None,
        });
    }
    let limit = (options.threshold * 255.).round() as i32;
    let mut diff = RgbaImage::new(actual.width(), actual.height());
    let mut different = 0u64;
    for (x, y, a) in actual.enumerate_pixels() {
        let e = expected.get_pixel(x, y);
        let distance = (0..4)
            .map(|c| (a.0[c] as i32 - e.0[c] as i32).abs())
            .max()
            .unwrap_or(0);
        if distance > limit {
            different += 1;
            diff.put_pixel(x, y, Rgba([255, 0, 0, 255]));
        } else {
            let gray = ((a.0[0] as u32 + a.0[1] as u32 + a.0[2] as u32) / 3 / 4 + 191) as u8;
            diff.put_pixel(x, y, Rgba([gray, gray, gray, 255]));
        }
    }
    let total = (actual.width() as u64 * actual.height() as u64).max(1);
    let allowed = match (options.max_diff_pixels, options.max_diff_pixel_ratio) {
        (Some(pixels), _) => pixels,
        (None, Some(ratio)) => (ratio * total as f64) as u64,
        (None, None) => 0,
    };
    (different > allowed).then(|| Mismatch {
        message: format!(
            "{different} pixels (ratio {:.2} of all image pixels) are different.",
            different as f64 / total as f64
        ),
        diff: Some(diff),
    })
}
