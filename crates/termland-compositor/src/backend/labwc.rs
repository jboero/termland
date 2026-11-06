//! labwc backend - lightweight wlroots compositor with multiple-window support.
//! Used for `SessionMode::Desktop` to host a real desktop session.

use std::path::Path;
use crate::backend::{detached_compositor_command, read_socket_from_log, socket_wrapper_cmd, wait_socket_ready, DetachedBackend};
use crate::session::CompositorError;

/// Launch labwc **detached** (setsid, stdio → `log_path`) so it survives the
/// spawning connection. Returns the PID + Wayland socket; the process is not
/// owned/killed here — terminate it explicitly via the session registry.
///
/// `run_as`: forwarded to `detached_compositor_command` - when `Some`, labwc
/// (and everything it launches inside the desktop) run as that system user
/// instead of the server's own user. See that function's doc comment for the
/// privilege-drop mechanics.
pub fn launch_detached(
    width: u32,
    height: u32,
    shell_cmd: &str,
    log_path: &Path,
    run_as: Option<&str>,
    audio_sink: Option<&str>,
) -> Result<DetachedBackend, CompositorError> {
    tracing::info!("Launching detached labwc: {shell_cmd} ({width}x{height})");
    let config_dir = write_minimal_config(width, height, hosts_kwin(shell_cmd))?;

    let wrapper = socket_wrapper_cmd(shell_cmd);
    let sh_arg = format!("sh -c '{}'", wrapper.replace('\'', r"'\''"));

    let (mut cmd, runtime_dir) = detached_compositor_command("labwc", width, height, log_path, run_as, audio_sink)?;
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
    wait_socket_ready(&wayland_display, &runtime_dir);

    // Drop `process` without waiting/killing: std Child does not kill on drop,
    // so the detached compositor keeps running.
    Ok(DetachedBackend { pid, wayland_display, runtime_dir })
}

/// Random hex suffix so each call gets its own, unpredictable config
/// directory (see `write_minimal_config`'s doc comment for why this matters
/// beyond just avoiding same-path collisions between concurrent sessions).
fn random_suffix() -> String {
    let mut buf = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Write a minimal labwc config to a temp directory and return its path.
/// The config sets the headless output resolution and enables basic keybinds.
///
/// The directory name is `std::process::id()` (the *server's* pid, constant
/// for its whole lifetime) plus a random suffix, and creation uses
/// `create_dir` rather than `create_dir_all`. Both matter, not just for
/// tidiness: this is written by root (when running with `--auth`) then read
/// by labwc after it drops to the session's target user, and a fixed,
/// server-pid-only path would be the same predictable location
/// (`/tmp/termland-labwc-<pid>`) for every session for as long as the server
/// runs - trivially discoverable (`pgrep termland-server`) by any local user,
/// who could pre-plant a symlink there. `create_dir_all` treats "the path
/// already exists and resolves to a directory" as success (it calls `mkdir`,
/// sees `EEXIST`, and checks `is_dir()` - which follows symlinks), so a
/// planted symlink to another directory would be silently followed and then
/// have its permissions loosened via the `chmod 0755` below, or a planted
/// `rc.xml` symlink would have an arbitrary target file overwritten by the
/// `write`+`chmod 0644` further down - both running as root. The random
/// suffix makes the path unguessable; plain `create_dir` (unlike
/// `create_dir_all`) fails outright with `AlreadyExists` if anything is
/// already at that exact path, symlink or not, closing the race even if the
/// suffix were somehow predicted.
fn write_minimal_config(width: u32, height: u32, nested_kwin: bool) -> Result<String, CompositorError> {
    let dir = std::env::temp_dir()
        .join(format!("termland-labwc-{}-{}", std::process::id(), random_suffix()));
    std::fs::create_dir(&dir)
        .map_err(|e| CompositorError::StartFailed(format!("mkdir labwc config: {e}")))?;
    // This is written by the server (root, when running with --auth) but
    // read by labwc after it has dropped privileges to the session's target
    // user (see detached_compositor_command). Set world-readable/traversable
    // permissions explicitly rather than relying on the process umask, so
    // labwc can read it regardless of which user it's running as.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| CompositorError::StartFailed(format!("chmod labwc config dir: {e}")))?;
    }

    let rc_xml = rc_xml(width, height, nested_kwin);

    let rc_path = dir.join("rc.xml");
    std::fs::write(&rc_path, rc_xml)
        .map_err(|e| CompositorError::StartFailed(format!("write rc.xml: {e}")))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&rc_path, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| CompositorError::StartFailed(format!("chmod rc.xml: {e}")))?;
    }

    Ok(dir.to_string_lossy().to_string())
}

/// The desktop command runs KWin nested in labwc (`PlasmaShell::NestedKwin`).
fn hosts_kwin(shell_cmd: &str) -> bool {
    shell_cmd.contains("kwin_wayland")
}

/// labwc's rc.xml: the output mode, and basic window manager keybinds.
///
/// With KWin nested (`nested_kwin`), KWin is the window manager and labwc
/// only hosts its one window, so labwc binds no keys (Alt+Tab, Alt+F4 and the
/// rest have to reach KWin) and draws no title bar on KWin's window, which
/// KWin itself makes full-screen.
fn rc_xml(width: u32, height: u32, nested_kwin: bool) -> String {
    let keyboard = if nested_kwin {
        // labwc loads its default keybinds when rc.xml has none, so one
        // binding that does nothing, on keys nobody presses, stands in.
        r#"  <keyboard>
    <keybind key="W-A-C-S-F24"><action name="None"/></keybind>
  </keyboard>
  <windowRules>
    <windowRule identifier="org.kde.kwin" serverDecoration="no"/>
  </windowRules>"#
    } else {
        r#"  <keyboard>
    <keybind key="A-F4"><action name="Close"/></keybind>
    <keybind key="A-Tab"><action name="NextWindow"/></keybind>
    <keybind key="W-d"><action name="ShowDesktop"/></keybind>
    <keybind key="W-Return"><action name="Execute" command="konsole"/></keybind>
  </keyboard>"#
    };
    format!(r#"<?xml version="1.0"?>
<labwc_config>
  <core>
    <decoration>client</decoration>
  </core>
  <output>
    <name>HEADLESS-1</name>
    <mode><width>{width}</width><height>{height}</height><refresh>60</refresh></mode>
  </output>
{keyboard}
</labwc_config>
"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the one do-nothing binding: anything else would take keys like
    /// Alt+Tab away from KWin.
    #[test]
    fn labwc_binds_no_keys_when_kwin_is_the_desktop() {
        let rc = rc_xml(1280, 720, true);
        assert_eq!(rc.matches("<keybind ").count(), 1, "{rc}");
        assert!(rc.contains(r#"<keybind key="W-A-C-S-F24"><action name="None"/></keybind>"#), "{rc}");
        assert!(!rc.contains("<default/>"), "<default/> loads labwc's own keybinds: {rc}");
        assert!(rc.contains(r#"identifier="org.kde.kwin" serverDecoration="no""#), "{rc}");
    }

    #[test]
    fn labwc_keeps_its_keybinds_without_kwin() {
        let rc = rc_xml(1280, 720, false);
        assert!(rc.contains(r#"key="A-Tab""#), "{rc}");
        assert!(!rc.contains("org.kde.kwin"), "{rc}");
    }

    #[test]
    fn kwin_is_recognised_in_the_desktop_command() {
        let kwin = crate::backend::plasma_desktop_command(crate::backend::PlasmaShell::NestedKwin, None, "konsole");
        let labwc = crate::backend::plasma_desktop_command(crate::backend::PlasmaShell::Labwc, None, "konsole");
        assert!(hosts_kwin(&kwin));
        assert!(!hosts_kwin(&labwc));
    }
}
