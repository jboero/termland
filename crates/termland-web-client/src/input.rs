//! DOM events to Termland input messages.
//!
//! The scancode table is `termland_protocol::input::browser_code_to_evdev`,
//! shared with the desktop client rather than restated here — two hand-kept
//! copies of the same mapping is how they drift.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use termland_protocol::input::{browser_code_to_evdev, buttons};
use termland_protocol::{
    ButtonState, KeyEvent, KeyState, Message, MouseButton, MouseMove, MouseScroll, TextInput,
};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Event, HtmlCanvasElement, KeyboardEvent, MouseEvent, WheelEvent};

use crate::transport::Outbox;

/// Wheel deltas arrive in browser units; the compositor wants something close
/// to a notch. Matches the desktop client's scroll step.
const SCROLL_STEP: f64 = 15.0;

/// What is currently held down, so a lost key-up cannot strand a modifier.
#[derive(Default)]
struct Held {
    keys: HashSet<u32>,
    buttons: HashSet<u32>,
}

/// Map a DOM `MouseEvent.button` to its evdev code.
pub fn mouse_button_code(button: i16) -> Option<u32> {
    Some(match button {
        0 => buttons::LEFT,
        1 => buttons::MIDDLE,
        2 => buttons::RIGHT,
        3 => buttons::BACK,
        4 => buttons::FORWARD,
        _ => return None,
    })
}

/// Scale a canvas-relative position to remote framebuffer pixels.
///
/// The canvas is displayed at whatever CSS size the layout gives it, which is
/// rarely the framebuffer size; sending unscaled coordinates puts the pointer
/// in the wrong place on any zoom other than 1:1.
pub fn to_remote(
    x: f64,
    y: f64,
    css_w: f64,
    css_h: f64,
    remote_w: u32,
    remote_h: u32,
) -> (f64, f64) {
    if css_w <= 0.0 || css_h <= 0.0 || remote_w == 0 || remote_h == 0 {
        return (x, y);
    }
    let rx = (x * remote_w as f64 / css_w).clamp(0.0, (remote_w - 1) as f64);
    let ry = (y * remote_h as f64 / css_h).clamp(0.0, (remote_h - 1) as f64);
    (rx, ry)
}

/// Keyboard and pointer capture over one canvas.
///
/// Every registered closure is kept in `_closures`: dropping a `Closure` frees
/// the JS shim the listener points at, and the listener then fires into freed
/// memory.
pub struct InputCapture {
    held: Rc<RefCell<Held>>,
    outbox: Outbox,
    _closures: Vec<Closure<dyn FnMut(Event)>>,
    _key_closures: Vec<Closure<dyn FnMut(KeyboardEvent)>>,
    _mouse_closures: Vec<Closure<dyn FnMut(MouseEvent)>>,
    _wheel_closures: Vec<Closure<dyn FnMut(WheelEvent)>>,
}

impl InputCapture {
    pub fn attach(
        canvas: &HtmlCanvasElement,
        outbox: Outbox,
        remote: Rc<RefCell<(u32, u32)>>,
    ) -> Result<Self, JsValue> {
        let held: Rc<RefCell<Held>> = Rc::new(RefCell::new(Held::default()));
        let mut key_closures = Vec::new();
        let mut mouse_closures = Vec::new();
        let mut wheel_closures = Vec::new();
        let mut closures = Vec::new();

        // The canvas must be focusable for it to receive key events at all.
        canvas.set_tab_index(0);

        // --- keyboard ---
        for (event, pressed) in [("keydown", true), ("keyup", false)] {
            let out = outbox.clone();
            let held = held.clone();
            let cb = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
                let Some(scancode) = browser_code_to_evdev(&e.code()) else {
                    return;
                };
                e.prevent_default();
                if pressed {
                    held.borrow_mut().keys.insert(scancode);
                } else {
                    held.borrow_mut().keys.remove(&scancode);
                }
                out.send(&Message::KeyEvent(KeyEvent {
                    scancode,
                    keysym: 0,
                    state: if pressed { KeyState::Pressed } else { KeyState::Released },
                    modifiers: 0,
                }));
            });
            canvas.add_event_listener_with_callback(event, cb.as_ref().unchecked_ref())?;
            key_closures.push(cb);
        }

        // Composed text (IME, soft keyboards) that no scancode describes.
        {
            let out = outbox.clone();
            let cb = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
                let key = e.key();
                // Single printable characters only: named keys ("Enter",
                // "ArrowUp") already went out as scancodes above.
                if key.chars().count() == 1 && !e.ctrl_key() && !e.alt_key() && !e.meta_key() {
                    if let Some(c) = key.chars().next() {
                        if !c.is_control() {
                            out.send(&Message::TextInput(TextInput { text: key }));
                        }
                    }
                }
            });
            canvas.add_event_listener_with_callback("keypress", cb.as_ref().unchecked_ref())?;
            key_closures.push(cb);
        }

        // --- pointer ---
        {
            let out = outbox.clone();
            let remote = remote.clone();
            let canvas_for_move = canvas.clone();
            let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
                let rect = canvas_for_move.get_bounding_client_rect();
                let (rw, rh) = *remote.borrow();
                // Pointer lock reports deltas, not positions.
                let locked = web_sys::window()
                    .and_then(|w| w.document())
                    .and_then(|d| d.pointer_lock_element())
                    .is_some();
                let msg = if locked {
                    MouseMove {
                        x: e.movement_x() as f64,
                        y: e.movement_y() as f64,
                        absolute: false,
                    }
                } else {
                    let (x, y) = to_remote(
                        e.client_x() as f64 - rect.left(),
                        e.client_y() as f64 - rect.top(),
                        rect.width(),
                        rect.height(),
                        rw,
                        rh,
                    );
                    MouseMove { x, y, absolute: true }
                };
                out.send(&Message::MouseMove(msg));
            });
            canvas.add_event_listener_with_callback("mousemove", cb.as_ref().unchecked_ref())?;
            mouse_closures.push(cb);
        }

        for (event, pressed) in [("mousedown", true), ("mouseup", false)] {
            let out = outbox.clone();
            let held = held.clone();
            let cb = Closure::<dyn FnMut(MouseEvent)>::new(move |e: MouseEvent| {
                let Some(button) = mouse_button_code(e.button()) else {
                    return;
                };
                e.prevent_default();
                if pressed {
                    held.borrow_mut().buttons.insert(button);
                } else {
                    held.borrow_mut().buttons.remove(&button);
                }
                out.send(&Message::MouseButton(MouseButton {
                    button,
                    state: if pressed { ButtonState::Pressed } else { ButtonState::Released },
                }));
            });
            canvas.add_event_listener_with_callback(event, cb.as_ref().unchecked_ref())?;
            mouse_closures.push(cb);
        }

        {
            let out = outbox.clone();
            let cb = Closure::<dyn FnMut(WheelEvent)>::new(move |e: WheelEvent| {
                e.prevent_default();
                out.send(&Message::MouseScroll(MouseScroll {
                    dx: e.delta_x().signum() * SCROLL_STEP,
                    dy: e.delta_y().signum() * SCROLL_STEP,
                }));
            });
            canvas.add_event_listener_with_callback("wheel", cb.as_ref().unchecked_ref())?;
            wheel_closures.push(cb);
        }

        // The browser's own context menu would swallow the right-button up.
        {
            let cb = Closure::<dyn FnMut(Event)>::new(move |e: Event| e.prevent_default());
            canvas.add_event_listener_with_callback("contextmenu", cb.as_ref().unchecked_ref())?;
            closures.push(cb);
        }

        // Losing focus (alt-tab, tab hidden) means the matching key-up and
        // mouse-up never arrive, which leaves labwc with a stuck modifier or a
        // stuck BTN_LEFT. Release everything on the way out.
        {
            let out = outbox.clone();
            let held = held.clone();
            let cb = Closure::<dyn FnMut(Event)>::new(move |_e: Event| {
                release_all(&out, &held);
            });
            canvas.add_event_listener_with_callback("blur", cb.as_ref().unchecked_ref())?;
            if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
                doc.add_event_listener_with_callback(
                    "visibilitychange",
                    cb.as_ref().unchecked_ref(),
                )?;
            }
            closures.push(cb);
        }

        Ok(Self {
            held,
            outbox,
            _closures: closures,
            _key_closures: key_closures,
            _mouse_closures: mouse_closures,
            _wheel_closures: wheel_closures,
        })
    }

    /// Release anything still held — used on disconnect as well as blur.
    pub fn release_all(&self) {
        release_all(&self.outbox, &self.held);
    }
}

fn release_all(out: &Outbox, held: &Rc<RefCell<Held>>) {
    let mut held = held.borrow_mut();
    for scancode in held.keys.drain() {
        out.send(&Message::KeyEvent(KeyEvent {
            scancode,
            keysym: 0,
            state: KeyState::Released,
            modifiers: 0,
        }));
    }
    for button in held.buttons.drain() {
        out.send(&Message::MouseButton(MouseButton {
            button,
            state: ButtonState::Released,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dom_buttons_map_to_evdev_codes() {
        assert_eq!(mouse_button_code(0), Some(0x110));
        assert_eq!(mouse_button_code(1), Some(0x112), "DOM 1 is middle, not right");
        assert_eq!(mouse_button_code(2), Some(0x111), "DOM 2 is right, not middle");
        assert_eq!(mouse_button_code(3), Some(0x113));
        assert_eq!(mouse_button_code(4), Some(0x114));
        assert_eq!(mouse_button_code(9), None);
    }

    #[test]
    fn canvas_coordinates_scale_to_the_framebuffer() {
        // Canvas shown at half size: a click at its centre is the middle of
        // the remote screen, not a quarter of the way in.
        let (x, y) = to_remote(320.0, 180.0, 640.0, 360.0, 1280, 720);
        assert!((x - 640.0).abs() < 0.001, "x was {x}");
        assert!((y - 360.0).abs() < 0.001, "y was {y}");
    }

    #[test]
    fn coordinates_never_escape_the_framebuffer() {
        let (x, y) = to_remote(9999.0, 9999.0, 640.0, 360.0, 1280, 720);
        assert_eq!((x, y), (1279.0, 719.0));
        let (x, y) = to_remote(-50.0, -50.0, 640.0, 360.0, 1280, 720);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn a_degenerate_canvas_does_not_divide_by_zero() {
        let (x, y) = to_remote(10.0, 20.0, 0.0, 0.0, 1280, 720);
        assert_eq!((x, y), (10.0, 20.0));
        let (x, y) = to_remote(10.0, 20.0, 640.0, 360.0, 0, 0);
        assert_eq!((x, y), (10.0, 20.0));
    }
}
