//! Translates Playwright input (key names, mouse buttons) into GPUI events and
//! dispatches them the way a platform would.

use gpui::{
    App, InputEvent as _, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta,
    ScrollWheelEvent, Window, point, px,
};

use crate::wire::PwError;

pub(crate) fn pos(x: f64, y: f64) -> Point<Pixels> {
    point(px(x as f32), px(y as f32))
}

pub(crate) fn mouse_button(name: Option<&str>) -> MouseButton {
    match name {
        Some("right") => MouseButton::Right,
        Some("middle") => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// Parses Playwright modifier names: `Alt`, `Control`, `ControlOrMeta`, `Meta`, `Shift`.
pub(crate) fn modifiers(names: &[&str]) -> Modifiers {
    let mut modifiers = Modifiers::default();
    for name in names {
        match *name {
            "Alt" => modifiers.alt = true,
            "Control" => modifiers.control = true,
            "Meta" => modifiers.platform = true,
            "Shift" => modifiers.shift = true,
            "ControlOrMeta" => {
                if cfg!(target_os = "macos") {
                    modifiers.platform = true
                } else {
                    modifiers.control = true
                }
            }
            _ => {}
        }
    }
    modifiers
}

/// Converts a Playwright key description such as `Enter`, `Shift+ArrowLeft`,
/// `ControlOrMeta+a` or `A` into a GPUI keystroke.
pub(crate) fn keystroke(description: &str) -> Result<Keystroke, PwError> {
    let tokens: Vec<&str> = if description == "+" {
        vec!["+"]
    } else {
        description.split('+').collect()
    };
    let (key, names) = tokens
        .split_last()
        .ok_or_else(|| PwError::new(format!("Unknown key: \"{description}\"")))?;
    let mut mods = modifiers(names);
    let key = match *key {
        "Enter" => "enter".to_owned(),
        "Tab" => "tab".to_owned(),
        "Backspace" => "backspace".to_owned(),
        "Delete" => "delete".to_owned(),
        "Escape" => "escape".to_owned(),
        "ArrowLeft" => "left".to_owned(),
        "ArrowRight" => "right".to_owned(),
        "ArrowUp" => "up".to_owned(),
        "ArrowDown" => "down".to_owned(),
        "Home" => "home".to_owned(),
        "End" => "end".to_owned(),
        "PageUp" => "pageup".to_owned(),
        "PageDown" => "pagedown".to_owned(),
        "Insert" => "insert".to_owned(),
        "Space" | " " => "space".to_owned(),
        key if key.len() > 1 && key.starts_with('F') && key[1..].parse::<u8>().is_ok() => {
            key.to_lowercase()
        }
        key if key.starts_with("Key") && key.len() == 4 => key[3..].to_lowercase(),
        key if key.starts_with("Digit") && key.len() == 6 => key[5..].to_owned(),
        key if key.chars().count() == 1 => {
            let c = key.chars().next().unwrap_or_default();
            if c.is_uppercase() {
                mods.shift = true;
            }
            c.to_lowercase().to_string()
        }
        other => return Err(PwError::new(format!("Unknown key: \"{other}\""))),
    };
    Ok(Keystroke {
        modifiers: mods,
        key,
        key_char: None,
    })
}

/// Key down and up for one keystroke, like a user pressing it.
pub(crate) fn press(window: &mut Window, key: Keystroke, cx: &mut App) {
    key_down(window, key.clone(), cx);
    key_up(window, key, cx);
}

pub(crate) fn key_down(window: &mut Window, key: Keystroke, cx: &mut App) {
    // Enter and Tab run their bindings without the simulated IME inserting
    // text, matching gpui-kit's test input.
    if matches!(key.key.as_str(), "enter" | "tab") {
        window.dispatch_event(
            KeyDownEvent {
                keystroke: key,
                is_held: false,
                prefer_character_input: false,
            }
            .to_platform_input(),
            cx,
        );
    } else {
        window.dispatch_keystroke(key, cx);
    }
}

pub(crate) fn key_up(window: &mut Window, key: Keystroke, cx: &mut App) {
    let key = key.with_simulated_ime();
    window.dispatch_event(KeyUpEvent { keystroke: key }.to_platform_input(), cx);
}

/// Types text character by character through the focused input handler.
pub(crate) fn type_text(window: &mut Window, text: &str, cx: &mut App) {
    for character in text.chars() {
        if character == '\n' || character == '\r' {
            press(window, keystroke("Enter").expect("enter"), cx);
            continue;
        }
        let text = character.to_string();
        let mut key = Keystroke::parse(&text).unwrap_or(Keystroke {
            modifiers: Modifiers::default(),
            key: text.clone(),
            key_char: None,
        });
        key.key_char = Some(text);
        window.dispatch_keystroke(key, cx);
    }
}

/// Inserts `text` in one input event, like a paste or an IME commit, without
/// a key event per character.
pub(crate) fn insert_text(window: &mut Window, text: &str, cx: &mut App) {
    let Some(first) = text.chars().next() else {
        return;
    };
    window.dispatch_keystroke(
        Keystroke {
            modifiers: Modifiers::default(),
            key: first.to_string(),
            key_char: Some(text.to_owned()),
        },
        cx,
    );
}

pub(crate) fn set_modifiers(window: &mut Window, modifiers: Modifiers, cx: &mut App) {
    window.dispatch_event(
        ModifiersChangedEvent {
            modifiers,
            capslock: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
}

pub(crate) fn mouse_move(
    window: &mut Window,
    position: Point<Pixels>,
    pressed: Option<MouseButton>,
    modifiers: Modifiers,
    cx: &mut App,
) {
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: pressed,
            modifiers,
        }
        .to_platform_input(),
        cx,
    );
}

pub(crate) fn mouse_down(
    window: &mut Window,
    position: Point<Pixels>,
    button: MouseButton,
    click_count: usize,
    modifiers: Modifiers,
    cx: &mut App,
) {
    window.dispatch_event(
        MouseDownEvent {
            button,
            position,
            modifiers,
            click_count,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
}

pub(crate) fn mouse_up(
    window: &mut Window,
    position: Point<Pixels>,
    button: MouseButton,
    click_count: usize,
    modifiers: Modifiers,
    cx: &mut App,
) {
    window.dispatch_event(
        MouseUpEvent {
            button,
            position,
            modifiers,
            click_count,
        }
        .to_platform_input(),
        cx,
    );
}

/// A full click: move, then down and up `count` times.
pub(crate) fn click(
    window: &mut Window,
    position: Point<Pixels>,
    button: MouseButton,
    count: usize,
    modifiers: Modifiers,
    cx: &mut App,
) {
    if modifiers != Modifiers::default() {
        set_modifiers(window, modifiers, cx);
    }
    mouse_move(window, position, None, modifiers, cx);
    for click_count in 1..=count.max(1) {
        mouse_down(window, position, button, click_count, modifiers, cx);
        mouse_up(window, position, button, click_count, modifiers, cx);
    }
    if modifiers != Modifiers::default() {
        set_modifiers(window, Modifiers::default(), cx);
    }
}

/// Playwright's wheel deltas are positive downwards; GPUI's are the
/// direction content moves.
pub(crate) fn wheel(window: &mut Window, position: Point<Pixels>, dx: f64, dy: f64, cx: &mut App) {
    window.dispatch_event(
        ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(pos(-dx, -dy)),
            ..Default::default()
        }
        .to_platform_input(),
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_playwright_keys() {
        let key = keystroke("Shift+ArrowLeft").unwrap();
        assert_eq!(key.key, "left");
        assert!(key.modifiers.shift);
        assert_eq!(keystroke("Enter").unwrap().key, "enter");
        let key = keystroke("A").unwrap();
        assert_eq!(key.key, "a");
        assert!(key.modifiers.shift);
        assert_eq!(keystroke("KeyB").unwrap().key, "b");
        assert!(keystroke("Hyper").is_err());
    }
}
