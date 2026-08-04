//! Cage backend - kiosk compositor that runs a single fullscreen application.
//! Used for `SessionMode::App`.

use std::path::Path;
use crate::backend::{detached_compositor_command, read_socket_from_log, socket_wrapper_cmd, wait_socket_ready, DetachedBackend};
use crate::session::CompositorError;

/// Launch cage **detached** (setsid, stdio → `log_path`) so it survives the
/// spawning connection. Returns the PID + Wayland socket.
///
/// `run_as`: forwarded to `detached_compositor_command` - when `Some`, cage
/// (and the app it launches) run as that system user instead of the server's
/// own user. See that function's doc comment for the privilege-drop mechanics.
pub fn launch_detached(
    width: u32,
    height: u32,
    app_cmd: &str,
    app_args: &[String],
    log_path: &Path,
    run_as: Option<&str>,
) -> Result<DetachedBackend, CompositorError> {
    let inner = if app_args.is_empty() {
        app_cmd.to_string()
    } else {
        format!("{app_cmd} {}", app_args.join(" "))
    };
    tracing::info!("Launching detached cage: {inner} ({width}x{height})");

    let (mut cmd, runtime_dir) = detached_compositor_command("cage", width, height, log_path, run_as)?;
    cmd.arg("-d")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(socket_wrapper_cmd(&inner))
        .env("XCURSOR_THEME", "Adwaita")
        .env("XCURSOR_SIZE", "24");

    let mut process = cmd
        .spawn()
        .map_err(|e| CompositorError::StartFailed(format!("spawn cage: {e}")))?;
    let pid = process.id();
    tracing::info!("cage started detached (pid {pid}), waiting for socket...");

    let wayland_display = read_socket_from_log(log_path, &mut process)?;
    tracing::info!("cage created socket: {wayland_display}");
    wait_socket_ready(&wayland_display, &runtime_dir);

    Ok(DetachedBackend { pid, wayland_display, runtime_dir })
}

