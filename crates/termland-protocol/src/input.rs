use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyState {
    Pressed,
    Released,
    Repeat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEvent {
    pub scancode: u32,
    pub keysym: u32,
    pub state: KeyState,
    pub modifiers: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseMove {
    pub x: f64,
    pub y: f64,
    pub absolute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseButton {
    pub button: u32,
    pub state: ButtonState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseScroll {
    pub dx: f64,
    pub dy: f64,
}

/// Linux evdev button codes, as `MouseButton.button` carries them.
pub mod buttons {
    pub const LEFT: u32 = 0x110;
    pub const RIGHT: u32 = 0x111;
    pub const MIDDLE: u32 = 0x112;
    pub const BACK: u32 = 0x113;
    pub const FORWARD: u32 = 0x114;
}

/// Map a DOM `KeyboardEvent.code` to its Linux evdev scancode.
///
/// `KeyboardEvent.code` names a physical key, independent of layout, which is
/// exactly what evdev scancodes are — so this is a fixed table, not a
/// layout-dependent translation. The compositor applies the keymap.
///
/// Lives here rather than in a browser client so the mapping has one home:
/// `termland-client`'s desktop version of this table (keyed by winit's
/// `KeyCode`) and any browser client must agree on the same numbers, and a
/// second hand-maintained copy in another language is how they drift.
///
/// `None` for keys with no evdev equivalent; the caller should drop those
/// rather than guess.
pub fn browser_code_to_evdev(code: &str) -> Option<u32> {
    Some(match code {
        "Escape" => 1,
        "Digit1" => 2, "Digit2" => 3, "Digit3" => 4, "Digit4" => 5, "Digit5" => 6,
        "Digit6" => 7, "Digit7" => 8, "Digit8" => 9, "Digit9" => 10, "Digit0" => 11,
        "Minus" => 12, "Equal" => 13, "Backspace" => 14, "Tab" => 15,
        "KeyQ" => 16, "KeyW" => 17, "KeyE" => 18, "KeyR" => 19, "KeyT" => 20,
        "KeyY" => 21, "KeyU" => 22, "KeyI" => 23, "KeyO" => 24, "KeyP" => 25,
        "BracketLeft" => 26, "BracketRight" => 27, "Enter" => 28, "ControlLeft" => 29,
        "KeyA" => 30, "KeyS" => 31, "KeyD" => 32, "KeyF" => 33, "KeyG" => 34,
        "KeyH" => 35, "KeyJ" => 36, "KeyK" => 37, "KeyL" => 38,
        "Semicolon" => 39, "Quote" => 40, "Backquote" => 41,
        "ShiftLeft" => 42, "Backslash" => 43,
        "KeyZ" => 44, "KeyX" => 45, "KeyC" => 46, "KeyV" => 47, "KeyB" => 48,
        "KeyN" => 49, "KeyM" => 50,
        "Comma" => 51, "Period" => 52, "Slash" => 53, "ShiftRight" => 54,
        "NumpadMultiply" => 55, "AltLeft" => 56, "Space" => 57, "CapsLock" => 58,
        "F1" => 59, "F2" => 60, "F3" => 61, "F4" => 62, "F5" => 63, "F6" => 64,
        "F7" => 65, "F8" => 66, "F9" => 67, "F10" => 68,
        "NumLock" => 69, "ScrollLock" => 70,
        "Numpad7" => 71, "Numpad8" => 72, "Numpad9" => 73, "NumpadSubtract" => 74,
        "Numpad4" => 75, "Numpad5" => 76, "Numpad6" => 77, "NumpadAdd" => 78,
        "Numpad1" => 79, "Numpad2" => 80, "Numpad3" => 81, "Numpad0" => 82,
        "NumpadDecimal" => 83,
        "F11" => 87, "F12" => 88,
        "NumpadEnter" => 96, "ControlRight" => 97, "NumpadDivide" => 98,
        "PrintScreen" => 99, "AltRight" => 100,
        "Home" => 102, "ArrowUp" => 103, "PageUp" => 104,
        "ArrowLeft" => 105, "ArrowRight" => 106,
        "End" => 107, "ArrowDown" => 108, "PageDown" => 109,
        "Insert" => 110, "Delete" => 111,
        "Pause" => 119,
        "MetaLeft" => 125, "MetaRight" => 126, "ContextMenu" => 127,
        _ => return None,
    })
}

#[cfg(test)]
mod browser_keymap_tests {
    use super::*;

    /// The letters the desktop client's table (termland-client's
    /// `display.rs`) pins to the same numbers. If either side is ever
    /// retyped, this is what catches the drift.
    #[test]
    fn home_row_and_modifiers_match_evdev() {
        assert_eq!(browser_code_to_evdev("Escape"), Some(1));
        assert_eq!(browser_code_to_evdev("KeyA"), Some(30));
        assert_eq!(browser_code_to_evdev("KeyS"), Some(31));
        assert_eq!(browser_code_to_evdev("KeyD"), Some(32));
        assert_eq!(browser_code_to_evdev("Enter"), Some(28));
        assert_eq!(browser_code_to_evdev("Space"), Some(57));
        assert_eq!(browser_code_to_evdev("ControlLeft"), Some(29));
        assert_eq!(browser_code_to_evdev("ShiftLeft"), Some(42));
        assert_eq!(browser_code_to_evdev("AltLeft"), Some(56));
        assert_eq!(browser_code_to_evdev("MetaLeft"), Some(125));
    }

    #[test]
    fn unknown_codes_are_dropped_not_guessed() {
        assert_eq!(browser_code_to_evdev("Fn"), None);
        assert_eq!(browser_code_to_evdev(""), None);
        assert_eq!(browser_code_to_evdev("keya"), None, "match is case-sensitive");
    }

    /// A scancode collision would silently send the wrong key.
    #[test]
    fn every_mapped_code_is_unique() {
        const CODES: &[&str] = &[
            "Escape", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6",
            "Digit7", "Digit8", "Digit9", "Digit0", "Minus", "Equal", "Backspace",
            "Tab", "KeyQ", "KeyW", "KeyE", "KeyR", "KeyT", "KeyY", "KeyU", "KeyI",
            "KeyO", "KeyP", "BracketLeft", "BracketRight", "Enter", "ControlLeft",
            "KeyA", "KeyS", "KeyD", "KeyF", "KeyG", "KeyH", "KeyJ", "KeyK", "KeyL",
            "Semicolon", "Quote", "Backquote", "ShiftLeft", "Backslash", "KeyZ",
            "KeyX", "KeyC", "KeyV", "KeyB", "KeyN", "KeyM", "Comma", "Period",
            "Slash", "ShiftRight", "NumpadMultiply", "AltLeft", "Space", "CapsLock",
            "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "NumLock",
            "ScrollLock", "Numpad7", "Numpad8", "Numpad9", "NumpadSubtract",
            "Numpad4", "Numpad5", "Numpad6", "NumpadAdd", "Numpad1", "Numpad2",
            "Numpad3", "Numpad0", "NumpadDecimal", "F11", "F12", "NumpadEnter",
            "ControlRight", "NumpadDivide", "PrintScreen", "AltRight", "Home",
            "ArrowUp", "PageUp", "ArrowLeft", "ArrowRight", "End", "ArrowDown",
            "PageDown", "Insert", "Delete", "Pause", "MetaLeft", "MetaRight",
            "ContextMenu",
        ];
        let mut seen = std::collections::HashMap::new();
        for code in CODES {
            let sc = browser_code_to_evdev(code)
                .unwrap_or_else(|| panic!("{code} is in the list but maps to None"));
            if let Some(prev) = seen.insert(sc, *code) {
                panic!("scancode {sc} is used by both {prev} and {code}");
            }
        }
    }
}
