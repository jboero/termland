//! Termland session manager - launches a headless Wayland compositor
//! (cage for single-app kiosk, labwc for full desktop) and captures frames
//! via wlr-screencopy protocol.

mod backend;
mod cursor_capture;
mod output_resize;
mod screencopy;
mod session;
mod toplevels;
pub mod input;

pub use session::{Compositor, CompositorConfig, CompositorError, SessionMode};
pub use input::InputInjector;
pub use backend::validate_shell_command;
pub use cursor_capture::{CursorCapture, CursorCaptureError, CursorCapturer};
pub use toplevels::{Toplevel, ToplevelError, ToplevelWatcher};
