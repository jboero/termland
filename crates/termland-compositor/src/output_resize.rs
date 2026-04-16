//! Dynamic output resize via zwlr_output_manager_v1.
//!
//! Both cage and labwc expose this protocol (v4). We connect as a client to
//! the compositor's wayland socket, wait for the head, then build a
//! configuration that sets a new custom mode on it and applies.
//!
//! Why not just recreate the whole compositor? Killing cage/labwc would drop
//! all running apps (konsole, plasmashell, etc.) - unacceptable. The output
//! management protocol lets us change the framebuffer dimensions live, and
//! clients inside the compositor see a normal wl_output size change.

use std::path::Path;
use std::time::{Duration, Instant};

use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_client::protocol::wl_registry;
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1,
    zwlr_output_configuration_v1,
    zwlr_output_head_v1,
    zwlr_output_manager_v1,
    zwlr_output_mode_v1,
};

#[derive(Debug, thiserror::Error)]
pub enum ResizeError {
    #[error("wayland connect: {0}")]
    Connect(String),
    #[error("no output manager global")]
    NoManager,
    #[error("no head found")]
    NoHead,
    #[error("apply failed: {0}")]
    ApplyFailed(String),
    #[error("timed out waiting for manager/head discovery")]
    Timeout,
}

/// Driver for zwlr_output_manager_v1. Holds a Wayland connection and caches
/// the first head we see (headless wlroots compositors only have one output
/// anyway).
pub struct OutputResizer {
    _conn: Connection,
    event_queue: wayland_client::EventQueue<State>,
    state: State,
}

#[derive(Default)]
struct State {
    manager: Option<zwlr_output_manager_v1::ZwlrOutputManagerV1>,
    head: Option<zwlr_output_head_v1::ZwlrOutputHeadV1>,
    /// Current serial, needed to build a configuration.
    serial: Option<u32>,
    /// Did the most recent apply() succeed/fail/cancel? None while pending.
    last_apply_result: Option<ApplyResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyResult {
    Succeeded,
    Failed,
    Cancelled,
}

impl OutputResizer {
    /// Connect to the compositor's Wayland socket and wait for the output
    /// manager globals to arrive.
    pub fn connect(display_name: &str) -> Result<Self, ResizeError> {
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| format!("/run/user/{}", nix::unistd::getuid()));
        let socket_path = Path::new(&runtime_dir).join(display_name);

        let stream = std::os::unix::net::UnixStream::connect(&socket_path)
            .map_err(|e| ResizeError::Connect(format!("{}: {e}", socket_path.display())))?;
        let conn = Connection::from_socket(stream)
            .map_err(|e| ResizeError::Connect(e.to_string()))?;

        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();
        let mut state = State::default();

        let _registry = conn.display().get_registry(&qh, ());

        // First roundtrip: discover the zwlr_output_manager_v1 global.
        event_queue.roundtrip(&mut state)
            .map_err(|e| ResizeError::Connect(format!("roundtrip: {e}")))?;

        if state.manager.is_none() {
            return Err(ResizeError::NoManager);
        }

        // Second pass: let the manager emit its Head + Done events, so we
        // know the serial and the head object we'll reconfigure.
        let deadline = Instant::now() + Duration::from_secs(2);
        while (state.head.is_none() || state.serial.is_none()) && Instant::now() < deadline {
            event_queue.roundtrip(&mut state)
                .map_err(|e| ResizeError::Connect(format!("roundtrip head: {e}")))?;
        }

        if state.head.is_none() {
            return Err(ResizeError::NoHead);
        }

        tracing::info!("OutputResizer ready on {display_name} (serial={:?})", state.serial);

        Ok(Self { _conn: conn, event_queue, state })
    }

    /// Resize the output to (width × height) at 60Hz. Blocks until the
    /// compositor acknowledges with Succeeded / Failed / Cancelled.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), ResizeError> {
        let qh = self.event_queue.handle();

        let manager = self.state.manager.clone().ok_or(ResizeError::NoManager)?;
        let head = self.state.head.clone().ok_or(ResizeError::NoHead)?;
        let serial = self.state.serial.ok_or(ResizeError::NoManager)?;

        // Build a new configuration based on the current serial.
        let config = manager.create_configuration(serial, &qh, ());
        let head_config = config.enable_head(&head, &qh, ());

        // Apply a custom mode. refresh is in mHz.
        head_config.set_custom_mode(width as i32, height as i32, 60_000);

        // Reset the apply result before calling apply()
        self.state.last_apply_result = None;
        config.apply();

        // Pump events until we get a result.
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.state.last_apply_result.is_none() && Instant::now() < deadline {
            self.event_queue.roundtrip(&mut self.state)
                .map_err(|e| ResizeError::ApplyFailed(format!("roundtrip: {e}")))?;
        }

        match self.state.last_apply_result {
            Some(ApplyResult::Succeeded) => {
                tracing::info!("Output resized to {width}x{height}");
                Ok(())
            }
            Some(ApplyResult::Failed) => Err(ResizeError::ApplyFailed("compositor reported Failed".into())),
            Some(ApplyResult::Cancelled) => Err(ResizeError::ApplyFailed("compositor reported Cancelled".into())),
            None => Err(ResizeError::Timeout),
        }
    }
}

// --- Dispatch impls ---

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            if interface == "zwlr_output_manager_v1" {
                let mgr = registry.bind::<zwlr_output_manager_v1::ZwlrOutputManagerV1, _, _>(
                    name, version.min(4), qh, (),
                );
                state.manager = Some(mgr);
            }
        }
    }
}

impl Dispatch<zwlr_output_manager_v1::ZwlrOutputManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _mgr: &zwlr_output_manager_v1::ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                // First head wins - headless wlroots only has one anyway.
                if state.head.is_none() {
                    state.head = Some(head);
                }
            }
            zwlr_output_manager_v1::Event::Done { serial } => {
                state.serial = Some(serial);
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, zwlr_output_manager_v1::ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (zwlr_output_head_v1::ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<zwlr_output_head_v1::ZwlrOutputHeadV1, ()> for State {
    fn event(
        _state: &mut Self,
        _head: &zwlr_output_head_v1::ZwlrOutputHeadV1,
        _event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // We don't care about head events here - we just need the head object.
    }

    wayland_client::event_created_child!(State, zwlr_output_head_v1::ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (zwlr_output_mode_v1::ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<zwlr_output_mode_v1::ZwlrOutputModeV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwlr_output_mode_v1::ZwlrOutputModeV1,
        _: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}

impl Dispatch<zwlr_output_configuration_v1::ZwlrOutputConfigurationV1, ()> for State {
    fn event(
        state: &mut Self,
        _cfg: &zwlr_output_configuration_v1::ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_output_configuration_v1::Event;
        state.last_apply_result = match event {
            Event::Succeeded => Some(ApplyResult::Succeeded),
            Event::Failed => Some(ApplyResult::Failed),
            Event::Cancelled => Some(ApplyResult::Cancelled),
            _ => state.last_apply_result,
        };
    }
}

impl Dispatch<zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
        _: zwlr_output_configuration_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {}
}
