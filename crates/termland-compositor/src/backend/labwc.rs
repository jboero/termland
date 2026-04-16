//! labwc backend - lightweight wlroots compositor with multiple-window support.
//! Used for `SessionMode::Desktop` to host a real desktop session.

use std::process::Stdio;
use crate::backend::{compositor_command, read_socket_name, socket_wrapper_cmd, wait_socket_ready, LaunchedBackend};
use crate::session::CompositorError;

/// Launch labwc headlessly with the given startup command inside it.
///
/// `shell_cmd` is run as labwc's session process. When it exits, labwc exits.
/// Examples: "konsole", "startplasma-wayland"
pub fn launch(
    width: u32,
    height: u32,
    shell_cmd: &str,
) -> Result<LaunchedBackend, CompositorError> {
    tracing::info!("Launching labwc: {shell_cmd} ({width}x{height})");

    // Use a minimal config dir so we don't pick up user's ~/.config/labwc.
    // This keeps sessions reproducible and avoids surprising behavior.
    let config_dir = write_minimal_config(width, height)?;

    // labwc's -S (session) flag parses its argument with g_shell_parse_argv and
    // execs the result directly (via g_spawn_async), WITHOUT invoking /bin/sh -c.
    // That means shell syntax (;, $VAR, pipes) won't be expanded unless we wrap
    // the whole command in an explicit `sh -c '...'`.
    let wrapper = socket_wrapper_cmd(shell_cmd);
    let sh_arg = format!("sh -c '{}'", wrapper.replace('\'', r"'\''"));

    let mut cmd = compositor_command("labwc", width, height);
    cmd.arg("-C").arg(&config_dir)
        .arg("-S").arg(&sh_arg)
        // Ensure labwc can find a cursor theme to render. Without these vars,
        // the remote cursor is invisible because no bitmap is loaded.
        .env("XCURSOR_THEME", "Adwaita")
        .env("XCURSOR_SIZE", "24")
        .stdout(Stdio::null());

    let mut process = cmd
        .spawn()
        .map_err(|e| CompositorError::StartFailed(format!("spawn labwc: {e}")))?;

    tracing::info!("labwc started (pid {}), waiting for socket...", process.id());

    let stderr = process
        .stderr
        .take()
        .ok_or_else(|| CompositorError::StartFailed("no stderr from labwc".into()))?;

    let (wayland_display, drain) = read_socket_name(stderr, &mut process)?;
    tracing::info!("labwc created socket: {wayland_display}");

    wait_socket_ready(&wayland_display);

    Ok(LaunchedBackend {
        process,
        wayland_display,
        _stderr_drain: drain,
    })
}

/// Write a minimal labwc config to a temp directory and return its path.
/// The config sets the headless output resolution and enables basic keybinds.
fn write_minimal_config(width: u32, height: u32) -> Result<String, CompositorError> {
    let dir = std::env::temp_dir().join(format!("termland-labwc-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|e| CompositorError::StartFailed(format!("mkdir labwc config: {e}")))?;

    // rc.xml: basic window manager keybinds + set output mode
    let rc_xml = format!(r#"<?xml version="1.0"?>
<labwc_config>
  <core>
    <decoration>client</decoration>
  </core>
  <output>
    <name>HEADLESS-1</name>
    <mode><width>{width}</width><height>{height}</height><refresh>60</refresh></mode>
  </output>
  <keyboard>
    <keybind key="A-F4"><action name="Close"/></keybind>
    <keybind key="A-Tab"><action name="NextWindow"/></keybind>
    <keybind key="W-d"><action name="ShowDesktop"/></keybind>
    <keybind key="W-Return"><action name="Execute" command="konsole"/></keybind>
  </keyboard>
</labwc_config>
"#);

    let rc_path = dir.join("rc.xml");
    std::fs::write(&rc_path, rc_xml)
        .map_err(|e| CompositorError::StartFailed(format!("write rc.xml: {e}")))?;

    Ok(dir.to_string_lossy().to_string())
}
