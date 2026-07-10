//! labwc backend - lightweight wlroots compositor with multiple-window support.
//! Used for `SessionMode::Desktop` to host a real desktop session.

use std::path::Path;
use crate::backend::{detached_compositor_command, read_socket_from_log, socket_wrapper_cmd, wait_socket_ready, DetachedBackend};
use crate::session::CompositorError;

/// Launch labwc **detached** (setsid, stdio → `log_path`) so it survives the
/// spawning connection. Returns the PID + Wayland socket; the process is not
/// owned/killed here — terminate it explicitly via the session registry.
pub fn launch_detached(
    width: u32,
    height: u32,
    shell_cmd: &str,
    log_path: &Path,
) -> Result<DetachedBackend, CompositorError> {
    tracing::info!("Launching detached labwc: {shell_cmd} ({width}x{height})");
    let config_dir = write_minimal_config(width, height)?;

    let wrapper = socket_wrapper_cmd(shell_cmd);
    let sh_arg = format!("sh -c '{}'", wrapper.replace('\'', r"'\''"));

    let mut cmd = detached_compositor_command("labwc", width, height, log_path)?;
    cmd.arg("-C").arg(&config_dir)
        .arg("-S").arg(&sh_arg)
        .env("XCURSOR_THEME", "Adwaita")
        .env("XCURSOR_SIZE", "24");

    let mut process = cmd
        .spawn()
        .map_err(|e| CompositorError::StartFailed(format!("spawn labwc: {e}")))?;
    let pid = process.id();
    tracing::info!("labwc started detached (pid {pid}), waiting for socket...");

    let wayland_display = read_socket_from_log(log_path, &mut process)?;
    tracing::info!("labwc created socket: {wayland_display}");
    wait_socket_ready(&wayland_display);

    // Drop `process` without waiting/killing: std Child does not kill on drop,
    // so the detached compositor keeps running.
    Ok(DetachedBackend { pid, wayland_display })
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
