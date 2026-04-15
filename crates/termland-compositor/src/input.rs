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
    pub fn connect(display_name: &str) -> Result<Self, InputError> {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| format!("/run/user/{}", nix::unistd::getuid()));
        let socket_path = std::path::Path::new(&runtime_dir).join(display_name);

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

    /// Send a minimal xkb keymap to the virtual keyboard.
    fn send_keymap(vk: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1) -> Result<(), InputError> {
        use nix::sys::memfd;
        use nix::unistd;
        use std::os::fd::AsFd;
        use std::io::Write;

        // Minimal xkb keymap that maps evdev scancodes
        let keymap = r#"xkb_keymap {
    xkb_keycodes { include "evdev+aliases(qwerty)" };
    xkb_types { include "complete" };
    xkb_compat { include "complete" };
    xkb_symbols { include "pc+us+inet(evdev)" };
    xkb_geometry { include "pc(pc105)" };
};"#;

        let keymap_bytes = keymap.as_bytes();
        let size = keymap_bytes.len() + 1; // null terminated

        let fd = memfd::memfd_create(c"termland-keymap", memfd::MemFdCreateFlag::MFD_CLOEXEC)
            .map_err(|e| InputError::InjectFailed(format!("memfd_create: {e}")))?;

        unistd::ftruncate(&fd, size as i64)
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
