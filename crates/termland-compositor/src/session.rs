use std::path::Path;
use thiserror::Error;

use crate::backend::{self, detect_desktop_shell};
use crate::cursor_capture::{CursorCapture, CursorCapturer};
use crate::output_resize::OutputResizer;
use crate::screencopy::ScreenCapturer;

#[derive(Debug, Error)]
pub enum CompositorError {
    #[error("failed to start compositor: {0}")]
    StartFailed(String),
    #[error("capture error: {0}")]
    CaptureError(String),
    #[error("wayland error: {0}")]
    WaylandError(String),
    #[error("compositor exited unexpectedly")]
    CompositorExited,
}

#[derive(Debug, Clone)]
pub struct CompositorConfig {
    pub width: u32,
    pub height: u32,
    pub mode: SessionMode,
    /// For Desktop mode: the startup command to run inside labwc.
    /// If None, we auto-detect a terminal emulator.
    pub desktop_shell: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SessionMode {
    /// Full desktop session (labwc + multi-window shell).
    Desktop,
    /// Single fullscreen app (cage kiosk).
    App { command: String, args: Vec<String> },
}

impl From<termland_protocol::SessionMode> for SessionMode {
    fn from(mode: termland_protocol::SessionMode) -> Self {
        match mode {
            termland_protocol::SessionMode::Desktop => Self::Desktop,
            termland_protocol::SessionMode::App { command, args } => {
                Self::App { command, args }
            }
        }
    }
}

/// A per-connection *attachment* to a compositor session. Holds the screen
/// capturer and output resizer, but NOT the compositor process — the compositor
/// is spawned detached (see [`Compositor::create`]) and outlives any single
/// attachment. Dropping this disconnects capture/input but leaves the session
/// running; terminate the compositor explicitly via its PID.
pub struct Compositor {
    config: CompositorConfig,
    wayland_display: String,
    /// This compositor's actual `XDG_RUNTIME_DIR` - see
    /// `backend::DetachedBackend`'s doc comment for why this can differ from
    /// the server process's own under session isolation, and must be used
    /// for every later Wayland connection to this compositor.
    runtime_dir: std::path::PathBuf,
    capturer: Option<ScreenCapturer>,
    /// Separate Wayland client that drives zwlr_output_manager_v1 so we can
    /// change the headless output's size at runtime. Optional because older
    /// compositors may not advertise the protocol.
    resizer: Option<OutputResizer>,
    /// Separate Wayland client driving ext-image-copy-capture-v1's pointer
    /// cursor session, used for client-side-cursor mode (see
    /// `capture_cursor`). Optional because it's a staging protocol older
    /// compositors don't advertise; when absent, callers should fall back to
    /// not sending cursor shape updates at all.
    cursor_capturer: Option<CursorCapturer>,
    /// Name of the compositor backend for logging.
    backend_name: &'static str,
}

impl Compositor {
    /// Spawn a NEW detached compositor for `config` and attach to it. Returns
    /// the attachment plus the detached compositor's PID (record it in the
    /// session registry). The compositor survives this process.
    ///
    /// `run_as`: the PAM-authenticated username to isolate this session into
    /// (session isolation - see `backend::detached_compositor_command`), or
    /// `None` when the server was started without `--auth`, in which case
    /// behavior is unchanged from before this feature: the compositor runs
    /// as the server's own user. This must come from server-side auth state,
    /// never from client-supplied session-create parameters.
    pub fn create(config: CompositorConfig, log_path: &Path, run_as: Option<&str>) -> Result<(Self, u32), CompositorError> {
        let (pid, wayland_display, backend_name, runtime_dir) = match &config.mode {
            SessionMode::Desktop => {
                let shell = config.desktop_shell
                    .clone()
                    .unwrap_or_else(detect_desktop_shell);
                tracing::info!("Desktop shell: {shell}");
                let d = backend::labwc::launch_detached(config.width, config.height, &shell, log_path, run_as)?;
                (d.pid, d.wayland_display, "labwc", d.runtime_dir)
            }
            SessionMode::App { command, args } => {
                let d = backend::cage::launch_detached(config.width, config.height, command, args, log_path, run_as)?;
                (d.pid, d.wayland_display, "cage", d.runtime_dir)
            }
        };
        let comp = Self::connect(config, wayland_display, backend_name, runtime_dir)?;
        Ok((comp, pid))
    }

    /// Attach to an ALREADY-RUNNING compositor by its Wayland display name
    /// (session resume). Does not spawn anything; just connects a fresh
    /// capturer/resizer and re-asserts the output size.
    ///
    /// `runtime_dir` must be the SAME runtime dir the compositor was
    /// actually created with (see `DetachedBackend`'s doc comment) - callers
    /// get this from the session registry record (`registry::SessionRecord
    /// ::runtime_dir` in termland-server), not by recomputing it, since a
    /// fresh connection process has no other way to know whether the
    /// original session was isolated into a target user or not.
    pub fn attach(config: CompositorConfig, wayland_display: String, runtime_dir: std::path::PathBuf) -> Result<Self, CompositorError> {
        Self::connect(config, wayland_display, "compositor", runtime_dir)
    }

    /// Connect the screen capturer + output resizer to a running compositor.
    fn connect(
        config: CompositorConfig,
        wayland_display: String,
        backend_name: &'static str,
        runtime_dir: std::path::PathBuf,
    ) -> Result<Self, CompositorError> {
        let capturer = ScreenCapturer::connect(&wayland_display, &runtime_dir)
            .map_err(|e| CompositorError::WaylandError(format!("connect to {backend_name}: {e}")))?;

        tracing::info!("Screen capturer connected to {backend_name} ({wayland_display})");

        // Optional: connect a second Wayland client for output management.
        // If the protocol isn't available (older compositors), sessions still
        // work - they just can't be resized after startup.
        let mut resizer = match OutputResizer::connect(&wayland_display, &runtime_dir) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("OutputResizer unavailable ({e}) - remote resize disabled");
                None
            }
        };

        // Force the output to the requested dimensions. labwc's rc.xml
        // `<output name="HEADLESS-1">` matching is unreliable in practice, so
        // driving it via zwlr_output_manager_v1 is the only reliable path. On
        // attach this also re-asserts the size the resuming client wants.
        if let Some(r) = resizer.as_mut() {
            if let Err(e) = r.resize(config.width, config.height) {
                tracing::warn!(
                    "Initial resize to {}x{} failed ({e}); session will use \
                     the compositor default",
                    config.width, config.height
                );
            }
        }

        // Optional: connect the pointer cursor capture session. Same
        // "degrade, don't fail the whole session" treatment as the resizer -
        // ext-image-copy-capture-v1 is a staging protocol many compositors
        // (and older wlroots versions) don't implement yet.
        let cursor_capturer = match CursorCapturer::connect(&wayland_display, &runtime_dir) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(
                    "Cursor shape capture unavailable ({e}) - client-side cursor \
                     mode will show a generic placeholder instead of the real shape"
                );
                None
            }
        };

        Ok(Self {
            config,
            wayland_display,
            runtime_dir,
            capturer: Some(capturer),
            resizer,
            cursor_capturer,
            backend_name,
        })
    }

    /// Capture the current frame. Returns (width, height, rgba_data).
    /// `overlay_cursor`: include the compositor's cursor in the captured frame.
    pub fn capture_frame(&mut self, overlay_cursor: bool) -> Result<(u32, u32, Vec<u8>), CompositorError> {
        let capturer = self
            .capturer
            .as_mut()
            .ok_or_else(|| CompositorError::CaptureError("capturer not initialized".into()))?;

        capturer
            .capture_frame(overlay_cursor)
            .map_err(|e| CompositorError::CaptureError(e.to_string()))
    }

    /// Capture the current pointer cursor bitmap (image + hotspot +
    /// visibility), independent of the video frame. Used for client-side
    /// cursor rendering: see `CursorModeMsg` in termland-protocol for why
    /// that mode exists and what it needs this for.
    ///
    /// Returns an error if cursor capture isn't available on this compositor
    /// (see the `cursor_capturer` field doc) - callers should treat that as
    /// "no shape update this round", not a fatal condition.
    pub fn capture_cursor(&mut self) -> Result<CursorCapture, CompositorError> {
        let capturer = self
            .cursor_capturer
            .as_mut()
            .ok_or_else(|| CompositorError::CaptureError("cursor capturer not available".into()))?;

        capturer
            .capture_cursor()
            .map_err(|e| CompositorError::CaptureError(e.to_string()))
    }

    pub fn width(&self) -> u32 {
        self.config.width
    }

    pub fn height(&self) -> u32 {
        self.config.height
    }

    pub fn wayland_display(&self) -> &str {
        &self.wayland_display
    }

    /// This compositor's actual `XDG_RUNTIME_DIR` (see the field's own doc
    /// comment). Callers that spawn their own subprocesses or connections
    /// against this compositor's Wayland socket - `termland-server`'s
    /// clipboard/cursor watch threads and the session registry record -
    /// must use this, not their own process's `XDG_RUNTIME_DIR`.
    pub fn runtime_dir(&self) -> &std::path::Path {
        &self.runtime_dir
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    /// Resize the headless output to (width, height). Requires that the
    /// compositor exposed zwlr_output_manager_v1 at startup.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), CompositorError> {
        let resizer = self.resizer.as_mut().ok_or_else(|| {
            CompositorError::WaylandError("output manager not available".into())
        })?;
        resizer.resize(width, height)
            .map_err(|e| CompositorError::WaylandError(format!("resize: {e}")))?;
        self.config.width = width;
        self.config.height = height;
        Ok(())
    }
}

// No Drop kill: the compositor process is detached and owned by the session
// registry (terminated explicitly via its PID), not by this attachment.
