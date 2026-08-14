//! Window (toplevel) enumeration via zwlr_foreign_toplevel_management_v1.
//!
//! Lets the server tell a client what windows are open inside a session, so the
//! client can offer a task list / window switcher — see issue #1.
//!
//! # Why this is done here rather than left to the in-session panel
//!
//! Issue #1 recorded that KDE's taskbar cannot enumerate windows in a Termland
//! session, and guessed this was a labwc limitation. It is not: labwc (0.9.6,
//! checked) implements both `zwlr_foreign_toplevel_management_v1` and
//! `ext_foreign_toplevel_list_v1`. KDE's `libtaskmanager` binds neither — it
//! speaks only `org_kde_plasma_window_management`, a KDE protocol implemented
//! by KWin. So a Plasma taskbar cannot work on any wlroots-based compositor,
//! and no labwc release will change that.
//!
//! Since labwc *does* publish the standard protocol, the window list is
//! available to us as an ordinary Wayland client. Surfacing it over Termland's
//! own protocol means the feature works regardless of which panel — if any —
//! runs inside the session, and it works for `App` (cage kiosk) sessions that
//! have no panel at all.
//!
//! # Scope
//!
//! Enumeration and state only: title, app id, and whether a window is
//! minimised, maximised, fullscreen or focused. The protocol also supports
//! activating and closing windows; that is deliberately not wired up here,
//! because acting on a window is a bigger surface (it needs a seat, and it
//! needs thought about what a remote client should be allowed to do) and is
//! better reviewed on its own.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1, zwlr_foreign_toplevel_manager_v1,
};

#[derive(Debug, thiserror::Error)]
pub enum ToplevelError {
    #[error("wayland connect: {0}")]
    Connect(String),
    #[error("compositor does not implement zwlr_foreign_toplevel_management_v1")]
    NoManager,
}

/// One window inside the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toplevel {
    /// Stable for the lifetime of the window, assigned by us. The protocol
    /// identifies windows by object, which does not survive being sent over
    /// the wire, so callers get an integer they can refer back to.
    pub id: u32,
    pub title: String,
    /// Application identifier, e.g. `konsole`. Clients use this to pick an
    /// icon; it is frequently the desktop-entry name but is not guaranteed to
    /// be, so treat a miss as "no icon" rather than an error.
    pub app_id: String,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub activated: bool,
}

/// Wayland client that tracks the compositor's toplevel list.
///
/// Like the other helpers in this crate this is a *separate* Wayland client
/// connection to the session's compositor, not a second use of the capture
/// connection: the protocols are independent and a failure here must not be
/// able to disturb the video path.
pub struct ToplevelWatcher {
    _conn: Connection,
    event_queue: wayland_client::EventQueue<State>,
    state: State,
}

#[derive(Default)]
struct State {
    manager: Option<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1>,
    /// Windows by protocol object id. Keyed this way because every event
    /// (title, app_id, state, closed) arrives against the handle object.
    windows: HashMap<u32, Toplevel>,
    /// Monotonic id source for `Toplevel::id`. Never reused, so a stale id
    /// from a client refers to nothing rather than to a different window.
    next_id: u32,
    /// Set when the compositor finishes a burst of updates, so callers can
    /// tell "no windows yet" from "no windows".
    settled: bool,
}

impl State {
    fn entry(&mut self, key: u32) -> &mut Toplevel {
        let next = &mut self.next_id;
        self.windows.entry(key).or_insert_with(|| {
            *next += 1;
            Toplevel { id: *next, ..Default::default() }
        })
    }
}

impl ToplevelWatcher {
    /// Connect to the session compositor and start tracking its windows.
    ///
    /// `runtime_dir` must be the compositor's *actual* runtime dir — see
    /// `ScreenCapturer::connect` for why this cannot be assumed to be this
    /// process's own under session isolation.
    ///
    /// Returns `NoManager` on a compositor without the protocol. Callers
    /// should treat that as "window enumeration unavailable" and carry on:
    /// this is an optional capability, exactly like `OutputResizer`.
    pub fn connect(display_name: &str, runtime_dir: &Path) -> Result<Self, ToplevelError> {
        let socket_path = runtime_dir.join(display_name);
        let stream = std::os::unix::net::UnixStream::connect(&socket_path)
            .map_err(|e| ToplevelError::Connect(format!("{}: {e}", socket_path.display())))?;
        let conn =
            Connection::from_socket(stream).map_err(|e| ToplevelError::Connect(e.to_string()))?;

        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();
        let mut state = State::default();

        let _registry = conn.display().get_registry(&qh, ());

        // First roundtrip discovers the manager global.
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| ToplevelError::Connect(format!("roundtrip: {e}")))?;

        if state.manager.is_none() {
            return Err(ToplevelError::NoManager);
        }

        // The compositor announces existing windows immediately after bind,
        // each followed by its title/app_id/state. One more roundtrip picks up
        // that initial burst so a caller that polls straight away sees the
        // windows that were already open.
        event_queue
            .roundtrip(&mut state)
            .map_err(|e| ToplevelError::Connect(format!("roundtrip toplevels: {e}")))?;

        tracing::info!(
            "ToplevelWatcher ready on {display_name} ({} window(s))",
            state.windows.len()
        );

        Ok(Self { _conn: conn, event_queue, state })
    }

    /// Drain pending compositor events and return the current window list.
    ///
    /// Non-blocking: this is meant to be called on the same cadence as
    /// anything else polling the session, and never waits for a window to
    /// appear.
    pub fn poll(&mut self) -> Result<Vec<Toplevel>, ToplevelError> {
        self.event_queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| ToplevelError::Connect(format!("dispatch: {e}")))?;
        // A flush is needed because dispatch_pending only reads what has
        // already been buffered; without it our bind/ack traffic can sit
        // unsent and the compositor never tells us about new windows.
        let _ = self.event_queue.flush();
        Ok(self.windows())
    }

    /// Wait up to `timeout` for the window list to be non-empty. Only useful
    /// right after a session starts, when the shell has not mapped its first
    /// window yet.
    pub fn poll_until_any(&mut self, timeout: Duration) -> Result<Vec<Toplevel>, ToplevelError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.event_queue
                .roundtrip(&mut self.state)
                .map_err(|e| ToplevelError::Connect(format!("roundtrip: {e}")))?;
            if !self.state.windows.is_empty() || Instant::now() >= deadline {
                return Ok(self.windows());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Current windows, ordered by id so the list is stable between polls
    /// rather than reflecting hash iteration order.
    fn windows(&self) -> Vec<Toplevel> {
        let mut out: Vec<Toplevel> = self.state.windows.values().cloned().collect();
        out.sort_by_key(|w| w.id);
        out
    }

    /// Whether the compositor has finished at least one update burst.
    pub fn settled(&self) -> bool {
        self.state.settled
    }
}

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
            if interface == "zwlr_foreign_toplevel_manager_v1" {
                // v3 carries the events this module reads. Binding lower is
                // fine — the compositor simply sends fewer of them.
                let bind_version = version.min(3);
                state.manager = Some(registry.bind::<
                    zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
                    _,
                    _,
                >(name, bind_version, qh, ()));
            }
        }
    }
}

impl Dispatch<zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_manager_v1::Event;
        match event {
            Event::Toplevel { toplevel } => {
                // Create the entry now so a window with no title yet still
                // appears in the list rather than being invisible until it
                // names itself.
                let key = toplevel.id().protocol_id();
                state.entry(key);
            }
            Event::Finished => {
                state.manager = None;
            }
            _ => {}
        }
    }

    // The `toplevel` event creates a handle object, so wayland-client needs to
    // be told how to build its user data. Same pattern as OutputResizer's
    // head/mode children.
    wayland_client::event_created_child!(State, zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        let key = handle.id().protocol_id();

        match event {
            Event::Title { title } => state.entry(key).title = title,
            Event::AppId { app_id } => state.entry(key).app_id = app_id,
            Event::State { state: flags } => {
                // The compositor sends the complete state each time, as an
                // array of u32 enum values, so every flag is recomputed from
                // scratch rather than toggled.
                let w = state.entry(key);
                w.minimized = false;
                w.maximized = false;
                w.fullscreen = false;
                w.activated = false;
                for chunk in flags.chunks_exact(4) {
                    let v = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    match v {
                        0 => w.maximized = true,
                        1 => w.minimized = true,
                        2 => w.activated = true,
                        3 => w.fullscreen = true,
                        _ => {}
                    }
                }
            }
            Event::Closed => {
                state.windows.remove(&key);
            }
            Event::Done => {
                state.settled = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ids must never be reused: a client that remembers a window id and asks
    /// about it after the window closed has to get "gone", not a different
    /// window that happens to have taken the slot.
    #[test]
    fn ids_are_not_reused_after_a_window_closes() {
        let mut state = State::default();

        let first = state.entry(100).id;
        state.windows.remove(&100);
        let second = state.entry(200).id;

        assert_ne!(first, second, "a new window reused a closed window's id");
        assert!(second > first);
    }

    /// Every event arrives against the handle object, so repeated lookups for
    /// the same window must land on the same entry rather than creating one
    /// per event.
    #[test]
    fn repeated_events_for_one_window_share_an_entry() {
        let mut state = State::default();

        state.entry(7).title = "konsole".into();
        state.entry(7).app_id = "org.kde.konsole".into();

        assert_eq!(state.windows.len(), 1);
        let w = &state.windows[&7];
        assert_eq!(w.title, "konsole");
        assert_eq!(w.app_id, "org.kde.konsole");
    }

    /// State is resent in full, so flags must be recomputed rather than
    /// OR-ed in — otherwise a window that is un-maximised keeps reporting
    /// maximised forever.
    #[test]
    fn state_flags_are_replaced_not_accumulated() {
        let mut state = State::default();
        {
            let w = state.entry(1);
            w.maximized = true;
            w.activated = true;
        }

        // Simulate the recompute the State event performs.
        {
            let w = state.entry(1);
            w.minimized = false;
            w.maximized = false;
            w.fullscreen = false;
            w.activated = false;
            for v in [1u32] {
                match v {
                    0 => w.maximized = true,
                    1 => w.minimized = true,
                    2 => w.activated = true,
                    3 => w.fullscreen = true,
                    _ => {}
                }
            }
        }

        let w = &state.windows[&1];
        assert!(w.minimized);
        assert!(!w.maximized, "stale maximized survived a state update");
        assert!(!w.activated, "stale activated survived a state update");
    }

    #[test]
    fn windows_are_listed_in_stable_id_order() {
        let mut state = State::default();
        state.entry(300);
        state.entry(100);
        state.entry(200);

        let watcher_windows: Vec<u32> = {
            let mut out: Vec<Toplevel> = state.windows.values().cloned().collect();
            out.sort_by_key(|w| w.id);
            out.into_iter().map(|w| w.id).collect()
        };
        assert_eq!(watcher_windows, vec![1, 2, 3]);
    }
}
