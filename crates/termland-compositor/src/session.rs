use std::process::Child;
use thiserror::Error;

use crate::backend::{self, detect_desktop_shell};
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

/// A compositor session. The backend (cage or labwc) is chosen based on mode.
pub struct Compositor {
    config: CompositorConfig,
    process: Child,
    wayland_display: String,
    capturer: Option<ScreenCapturer>,
    /// Name of the compositor backend for logging.
    backend_name: &'static str,
    /// Kept alive so the compositor's stderr pipe stays drained for the
    /// session's lifetime. Without this, children SIGPIPE and crash.
    _stderr_drain: std::thread::JoinHandle<()>,
}

impl Compositor {
    /// Launch the appropriate compositor for the session mode and connect
    /// the screen capturer to it.
    pub fn new(config: CompositorConfig) -> Result<Self, CompositorError> {
        let (launched, backend_name) = match &config.mode {
            SessionMode::Desktop => {
                let shell = config.desktop_shell
                    .clone()
                    .unwrap_or_else(detect_desktop_shell);
                tracing::info!("Desktop shell: {shell}");
                (backend::labwc::launch(config.width, config.height, &shell)?, "labwc")
            }
            SessionMode::App { command, args } => {
                (backend::cage::launch(config.width, config.height, command, args)?, "cage")
            }
        };

        let capturer = ScreenCapturer::connect(&launched.wayland_display)
            .map_err(|e| CompositorError::WaylandError(format!("connect to {backend_name}: {e}")))?;

        tracing::info!("Screen capturer connected to {backend_name}");

        Ok(Self {
            config,
            process: launched.process,
            wayland_display: launched.wayland_display,
            capturer: Some(capturer),
            backend_name,
            _stderr_drain: launched._stderr_drain,
        })
    }

    /// Check if the compositor process is still running.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.process.try_wait(), Ok(None))
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

    pub fn width(&self) -> u32 {
        self.config.width
    }

    pub fn height(&self) -> u32 {
        self.config.height
    }

    pub fn wayland_display(&self) -> &str {
        &self.wayland_display
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width;
        self.config.height = height;
        tracing::info!(
            "Resize requested to {}x{} (not yet implemented)",
            width,
            height
        );
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        tracing::info!("Stopping {} (pid {})", self.backend_name, self.process.id());
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
