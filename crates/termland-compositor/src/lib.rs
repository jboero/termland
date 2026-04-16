//! Termland session manager - launches a headless Wayland compositor
//! (cage for single-app kiosk, labwc for full desktop) and captures frames
//! via wlr-screencopy protocol.

mod backend;
mod output_resize;
mod screencopy;
mod session;
pub mod input;

pub use session::{Compositor, CompositorConfig, CompositorError, SessionMode};
pub use input::InputInjector;
pub use backend::validate_shell_command;
