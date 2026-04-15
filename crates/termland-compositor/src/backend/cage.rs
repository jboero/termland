//! Cage backend - kiosk compositor that runs a single fullscreen application.
//! Used for `SessionMode::App`.

use crate::backend::{compositor_command, read_socket_name, socket_wrapper_cmd, wait_socket_ready, LaunchedBackend};
use crate::session::CompositorError;

pub fn launch(
    width: u32,
    height: u32,
    app_cmd: &str,
    app_args: &[String],
) -> Result<LaunchedBackend, CompositorError> {
    let inner = if app_args.is_empty() {
        app_cmd.to_string()
    } else {
        format!("{app_cmd} {}", app_args.join(" "))
    };

    tracing::info!("Launching cage: {inner} ({width}x{height})");

    let mut cmd = compositor_command("cage", width, height);
    cmd.arg("-d")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(socket_wrapper_cmd(&inner))
        // Ensure cursor images are available to the compositor.
        .env("XCURSOR_THEME", "Adwaita")
        .env("XCURSOR_SIZE", "24");

    let mut process = cmd
        .spawn()
        .map_err(|e| CompositorError::StartFailed(format!("spawn cage: {e}")))?;

    tracing::info!("Cage started (pid {}), waiting for socket...", process.id());

    let stderr = process
        .stderr
        .take()
        .ok_or_else(|| CompositorError::StartFailed("no stderr from cage".into()))?;

    let (wayland_display, drain) = read_socket_name(stderr, &mut process)?;
    tracing::info!("Cage created socket: {wayland_display}");

    wait_socket_ready(&wayland_display);

    Ok(LaunchedBackend {
        process,
        wayland_display,
        _stderr_drain: drain,
    })
}
