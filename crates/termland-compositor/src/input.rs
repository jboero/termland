//! Virtual input injection into cage via wlr-virtual-pointer and zwp-virtual-keyboard.

use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};

/// The keymap normal scancode injection (`InputInjector::key`) is resolved
/// against. Text injection temporarily replaces it and must always put it back.
const STATIC_KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes { include "evdev+aliases(qwerty)" };
    xkb_types { include "complete" };
    xkb_compat { include "complete" };
    xkb_symbols { include "pc+us+inet(evdev)" };
    xkb_geometry { include "pc(pc105)" };
};"#;

/// First xkb keycode used by a synthesized text keymap. Keycode 8 maps to evdev
/// code 0 (`KEY_RESERVED`), which is not a legal key to report, so start at 9.
const FIRST_SYNTH_KEYCODE: u32 = 9;

/// Distinct keysyms one synthesized keymap can hold. The traditional xkb
/// keycode range ends at 255 and we start at 9, so 247 is the hard ceiling;
/// stay under it and chunk longer text into several keymaps.
const MAX_KEYS_PER_KEYMAP: usize = 240;

/// Spacing between synthesized text keys. The Wayland roundtrip alone can push
/// a whole word through inside a single millisecond, and consumers that key off
/// the event timestamp (repeat detection, same-timestamp coalescing) drop keys
/// when that happens.
const TEXT_KEY_DELAY: std::time::Duration = std::time::Duration::from_millis(2);

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("wayland connect: {0}")]
    Connect(String),
    #[error("missing global: {0}")]
    MissingGlobal(&'static str),
    #[error("failed to inject: {0}")]
    InjectFailed(String),
}

struct InputState {
    seat: Option<wl_seat::WlSeat>,
    pointer_mgr: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
    keyboard_mgr: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
}

impl InputState {
    fn new() -> Self {
        Self {
            seat: None,
            pointer_mgr: None,
            keyboard_mgr: None,
        }
    }
}

// --- Dispatch impls ---

impl Dispatch<wl_registry::WlRegistry, ()> for InputState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_seat" => {
                    if state.seat.is_none() {
                        state.seat =
                            Some(registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(8), qh, ()));
                    }
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    state.pointer_mgr = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.keyboard_mgr = Some(registry.bind(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for InputState {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, ()> for InputState {
    fn event(_: &mut Self, _: &zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, _: zwlr_virtual_pointer_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ()> for InputState {
    fn event(_: &mut Self, _: &zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, _: zwp_virtual_keyboard_manager_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, ()> for InputState {
    fn event(_: &mut Self, _: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, _: zwlr_virtual_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ()> for InputState {
    fn event(_: &mut Self, _: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, _: zwp_virtual_keyboard_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

/// Handles injecting keyboard and mouse events into cage's Wayland session.
pub struct InputInjector {
    _conn: Connection,
    event_queue: EventQueue<InputState>,
    state: InputState,
    virtual_pointer: zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    virtual_keyboard: zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    /// Current modifier bitmask (XKB mods_depressed).
    mods_depressed: u32,
}

impl InputInjector {
    /// Connect to the given Wayland display and set up virtual input devices.
    ///
    /// `runtime_dir`: see `screencopy::ScreenCapturer::connect`'s doc
    /// comment - must be the compositor's actual runtime dir, not
    /// necessarily this process's own. Getting this wrong here specifically
    /// means keyboard/mouse input injection silently fails for an isolated
    /// session - the single most critical of all these Wayland connections,
    /// since a session with video but no input is useless.
    pub fn connect(display_name: &str, runtime_dir: &std::path::Path) -> Result<Self, InputError> {
        let socket_path = runtime_dir.join(display_name);

        let stream = std::os::unix::net::UnixStream::connect(&socket_path)
            .map_err(|e| InputError::Connect(format!("{}: {e}", socket_path.display())))?;

        let conn = Connection::from_socket(stream)
            .map_err(|e| InputError::Connect(e.to_string()))?;

        let display = conn.display();
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();
        let mut state = InputState::new();

        let _registry = display.get_registry(&qh, ());
        event_queue.roundtrip(&mut state)
            .map_err(|e| InputError::Connect(format!("roundtrip: {e}")))?;

        let seat = state.seat.as_ref()
            .ok_or(InputError::MissingGlobal("wl_seat"))?;
        let pointer_mgr = state.pointer_mgr.as_ref()
            .ok_or(InputError::MissingGlobal("zwlr_virtual_pointer_manager_v1"))?;
        let keyboard_mgr = state.keyboard_mgr.as_ref()
            .ok_or(InputError::MissingGlobal("zwp_virtual_keyboard_manager_v1"))?;

        // Create virtual devices
        let virtual_pointer = pointer_mgr.create_virtual_pointer(Some(seat), &qh, ());
        let virtual_keyboard = keyboard_mgr.create_virtual_keyboard(seat, &qh, ());

        // Send a minimal xkb keymap so the virtual keyboard works
        Self::send_keymap(&virtual_keyboard)?;

        event_queue.roundtrip(&mut state)
            .map_err(|e| InputError::Connect(format!("roundtrip after device creation: {e}")))?;

        tracing::info!("Input injector connected to {display_name}");

        Ok(Self {
            _conn: conn,
            event_queue,
            state,
            virtual_pointer,
            virtual_keyboard,
            mods_depressed: 0,
        })
    }

    /// Send the standard evdev-scancode keymap to the virtual keyboard.
    fn send_keymap(vk: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1) -> Result<(), InputError> {
        Self::upload_keymap(vk, STATIC_KEYMAP)
    }

    /// Upload an xkb keymap to the virtual keyboard over a memfd.
    fn upload_keymap(
        vk: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        keymap: &str,
    ) -> Result<(), InputError> {
        use nix::sys::memfd;
        use nix::unistd;
        use std::os::fd::AsFd;
        use std::io::Write;

        let keymap_bytes = keymap.as_bytes();
        let size = keymap_bytes.len() + 1; // null terminated

        let fd = memfd::memfd_create(c"termland-keymap", memfd::MemFdCreateFlag::MFD_CLOEXEC)
            .map_err(|e| InputError::InjectFailed(format!("memfd_create: {e}")))?;

        unistd::ftruncate(&fd, size as nix::libc::off_t)
            .map_err(|e| InputError::InjectFailed(format!("ftruncate: {e}")))?;

        // Write keymap to fd
        let mut file = std::fs::File::from(fd);
        file.write_all(keymap_bytes)
            .map_err(|e| InputError::InjectFailed(format!("write keymap: {e}")))?;
        file.write_all(&[0]) // null terminator
            .map_err(|e| InputError::InjectFailed(format!("write null: {e}")))?;

        // Re-extract the fd for the Wayland call
        let fd = std::os::fd::OwnedFd::from(file);

        // WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1 = 1
        vk.keymap(1, fd.as_fd(), size as u32);

        Ok(())
    }

    /// Inject a key event. scancode is evdev scancode.
    pub fn key(&mut self, scancode: u32, pressed: bool) {
        let time = self.timestamp_ms();
        let key_state = if pressed { 1 } else { 0 };

        // Update modifier bitmask if this is a modifier key.
        // XKB modifier bit positions:
        //   Shift=0x1, CapsLock=0x2, Ctrl=0x4, Alt/Mod1=0x8, Super/Mod4=0x40
        let mod_bit = match scancode {
            42 | 54 => Some(0x1),   // ShiftLeft, ShiftRight
            29 | 97 => Some(0x4),   // ControlLeft, ControlRight
            56 | 100 => Some(0x8),  // AltLeft, AltRight
            125 | 126 => Some(0x40), // SuperLeft, SuperRight
            _ => None,
        };

        self.virtual_keyboard.key(time, scancode, key_state);

        if let Some(bit) = mod_bit {
            if pressed {
                self.mods_depressed |= bit;
            } else {
                self.mods_depressed &= !bit;
            }
            self.virtual_keyboard.modifiers(self.mods_depressed, 0, 0, 0);
        }

        if let Err(e) = self.flush() {
            tracing::error!("Key inject flush failed: {e}");
        }
    }

    /// Inject already-composed Unicode text (a soft-keyboard / IME commit).
    ///
    /// Scancodes cannot express codepoints that are absent from the static
    /// keymap, so this synthesizes a throwaway keymap that binds spare keycodes
    /// to exactly the keysyms this string needs (the wtype approach), types
    /// them, and restores the static keymap. It works against every surface,
    /// including terminals and games that don't implement text-input, because
    /// as far as the client is concerned these are ordinary key presses.
    ///
    /// Errors are returned rather than swallowed: a failure mid-way can leave
    /// the throwaway keymap installed, which breaks all later scancode input,
    /// so the caller needs to know.
    pub fn text(&mut self, text: &str) -> Result<(), InputError> {
        // Chunk by *distinct* chars, not length: one keymap holds
        // MAX_KEYS_PER_KEYMAP keysyms but can type them any number of times, so
        // a long Latin paste (~100 distinct chars) still fits in one keymap.
        let mut chunks: Vec<Vec<char>> = Vec::new();
        let mut chunk: Vec<char> = Vec::new();
        let mut distinct: Vec<char> = Vec::new();

        for c in text.chars() {
            if Self::keysym_name(c).is_none() {
                tracing::debug!("Text inject: dropping untypable U+{:04X}", c as u32);
                continue;
            }
            if !distinct.contains(&c) {
                if distinct.len() >= MAX_KEYS_PER_KEYMAP {
                    chunks.push(std::mem::take(&mut chunk));
                    distinct.clear();
                }
                distinct.push(c);
            }
            chunk.push(c);
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        if chunks.is_empty() {
            return Ok(());
        }

        tracing::debug!(
            "Text inject: {} chars in {} keymap chunk(s)",
            text.chars().count(),
            chunks.len()
        );

        // Any modifier the client is holding from the KeyEvent path would turn
        // every synthesized key into a shortcut (Ctrl+<key>), so drop the mask
        // for the duration and re-assert it after the keymap is restored.
        let held_mods = self.mods_depressed;
        if held_mods != 0 {
            self.virtual_keyboard.modifiers(0, 0, 0, 0);
        }

        let mut result = Ok(());
        for chunk in &chunks {
            if let Err(e) = self.type_chunk(chunk) {
                result = Err(e);
                break;
            }
        }

        // Restore unconditionally, including on the error path: while a
        // synthesized keymap is installed, every subsequent KeyEvent scancode
        // resolves to the wrong keysym (or to nothing at all).
        let restore = Self::send_keymap(&self.virtual_keyboard);
        if held_mods != 0 {
            self.virtual_keyboard.modifiers(held_mods, 0, 0, 0);
        }
        let restore = restore.and_then(|()| self.flush());

        result.and(restore)
    }

    /// Upload a keymap covering this chunk's distinct chars, then press and
    /// release the corresponding keycode for every char in order.
    fn type_chunk(&mut self, chars: &[char]) -> Result<(), InputError> {
        let mut distinct: Vec<char> = Vec::new();
        // evdev codes to emit, in order. wl_keyboard (and so the virtual
        // keyboard) carries evdev codes, which are xkb keycodes minus 8.
        let mut sequence: Vec<u32> = Vec::with_capacity(chars.len());

        for &c in chars {
            let idx = match distinct.iter().position(|&d| d == c) {
                Some(i) => i,
                None => {
                    distinct.push(c);
                    distinct.len() - 1
                }
            };
            sequence.push(FIRST_SYNTH_KEYCODE + idx as u32 - 8);
        }

        if distinct.len() > MAX_KEYS_PER_KEYMAP {
            return Err(InputError::InjectFailed(format!(
                "text chunk needs {} keycodes, max {MAX_KEYS_PER_KEYMAP}",
                distinct.len()
            )));
        }

        let mut keycodes = String::new();
        let mut symbols = String::new();
        for (i, &c) in distinct.iter().enumerate() {
            let kc = FIRST_SYNTH_KEYCODE + i as u32;
            let name = Self::keysym_name(c).ok_or_else(|| {
                InputError::InjectFailed(format!("untypable char U+{:04X}", c as u32))
            })?;
            keycodes.push_str(&format!("        <K{kc}> = {kc};\n"));
            // A single symbol per key makes xkb infer the ONE_LEVEL type, so the
            // keysym is produced regardless of any modifier state.
            symbols.push_str(&format!("        key <K{kc}> {{ [ {name} ] }};\n"));
        }

        let keymap = format!(
            r#"xkb_keymap {{
    xkb_keycodes "termland_text" {{
        minimum = 8;
        maximum = {max};
{keycodes}    }};
    xkb_types {{ include "complete" }};
    xkb_compat {{ include "complete" }};
    xkb_symbols "termland_text" {{
{symbols}    }};
}};"#,
            max = FIRST_SYNTH_KEYCODE as usize + distinct.len(),
        );

        Self::upload_keymap(&self.virtual_keyboard, &keymap)?;
        // Make sure the compositor has compiled the new keymap (and would have
        // reported a protocol error) before we start pressing its keycodes.
        self.flush()?;

        for &evdev in &sequence {
            let time = self.timestamp_ms();
            self.virtual_keyboard.key(time, evdev, 1);
            self.virtual_keyboard.key(time + 1, evdev, 0);
            self.flush()?;
            std::thread::sleep(TEXT_KEY_DELAY);
        }

        Ok(())
    }

    /// xkb keysym name for a char, or `None` if it cannot be typed.
    ///
    /// The `U<hex>` form covers everything printable: xkbcommon resolves it to
    /// the Latin-1 keysym below U+0100 and to `0x01000000 + codepoint` above it
    /// (so emoji and CJK work). It deliberately rejects control codepoints
    /// (< U+0020, U+007F..U+009F) as NoSymbol, and a keymap containing NoSymbol
    /// fails to compile in the compositor, which would tear down our virtual
    /// keyboard - hence the legacy names for newline/tab and the drop for the
    /// rest.
    fn keysym_name(c: char) -> Option<String> {
        let cp = c as u32;
        match c {
            '\n' | '\r' => Some("Return".to_string()),
            '\t' => Some("Tab".to_string()),
            _ if cp < 0x20 || (0x7F..0xA0).contains(&cp) => None,
            _ => Some(format!("U{cp:04X}")),
        }
    }

    /// Inject absolute pointer motion. Coordinates are in client pixel space,
    /// scaled to the compositor's resolution via the extent parameters.
    pub fn pointer_motion_absolute(&mut self, x: f64, y: f64, client_width: u32, client_height: u32) {
        let time = self.timestamp_ms();
        // zwlr_virtual_pointer_v1::motion_absolute takes plain uint coordinates
        // where (x, y) is within the bounding box (0,0)-(x_extent, y_extent).
        // The compositor maps proportionally: pointer_x = x / x_extent * output_width
        self.virtual_pointer
            .motion_absolute(time, x as u32, y as u32, client_width, client_height);
        self.virtual_pointer.frame();
        let _ = self.flush();
    }

    /// Inject relative pointer motion, as used by pointer-lock / trackpad
    /// clients. `dx`/`dy` are compositor pixels, not a fraction of the
    /// output — the protocol field `MouseMove.absolute = false` is what
    /// selects this path.
    pub fn pointer_motion_relative(&mut self, dx: f64, dy: f64) {
        let time = self.timestamp_ms();
        self.virtual_pointer.motion(time, dx, dy);
        self.virtual_pointer.frame();
        let _ = self.flush();
    }

    /// Inject a mouse button event. button is Linux input event code (e.g., 0x110 = BTN_LEFT).
    pub fn pointer_button(&mut self, button: u32, pressed: bool) {
        let time = self.timestamp_ms();
        let state = if pressed {
            wayland_client::protocol::wl_pointer::ButtonState::Pressed
        } else {
            wayland_client::protocol::wl_pointer::ButtonState::Released
        };
        self.virtual_pointer.button(time, button, state);
        self.virtual_pointer.frame();
        let _ = self.flush();
    }

    /// Inject a scroll event.
    pub fn pointer_scroll(&mut self, dx: f64, dy: f64) {
        let time = self.timestamp_ms();
        if dy.abs() > 0.001 {
            self.virtual_pointer.axis(
                time,
                wayland_client::protocol::wl_pointer::Axis::VerticalScroll,
                dy,
            );
        }
        if dx.abs() > 0.001 {
            self.virtual_pointer.axis(
                time,
                wayland_client::protocol::wl_pointer::Axis::HorizontalScroll,
                dx,
            );
        }
        self.virtual_pointer.frame();
        let _ = self.flush();
    }

    fn flush(&mut self) -> Result<(), InputError> {
        self.event_queue
            .roundtrip(&mut self.state)
            .map_err(|e| InputError::InjectFailed(format!("flush: {e}")))?;
        Ok(())
    }

    fn timestamp_ms(&self) -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u32
    }
}
