//! Shared backend support for launching headless wlroots compositors
//! (cage for single-app, labwc for full desktop).

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStderr, Command, Stdio};
use crate::session::CompositorError;

/// A detached compositor: its own process-session (setsid) with stdio going to
/// a logfile, so it survives the connection process that spawned it. Tracked by
/// PID (liveness via `kill(pid, 0)`); terminated explicitly, never on drop.
pub struct DetachedBackend {
    pub pid: u32,
    pub wayland_display: String,
}

/// Like `compositor_command`, but detaches the child into its own session and
/// sends stdout+stderr to `log_path`. `setsid` keeps the compositor alive when
/// the spawning process (e.g. an SSH-subsystem connection) exits, and the
/// logfile avoids the SIGPIPE that a closed stderr pipe would cause.
pub fn detached_compositor_command(
    program: &str,
    width: u32,
    height: u32,
    log_path: &Path,
) -> Result<Command, CompositorError> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| CompositorError::StartFailed(format!("open log {}: {e}", log_path.display())))?;
    let log_err = log
        .try_clone()
        .map_err(|e| CompositorError::StartFailed(format!("clone log fd: {e}")))?;

    let mut cmd = Command::new(program);
    cmd.env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env_remove("GDK_BACKEND")
        .env_remove("QT_WAYLAND_RECONNECT")
        .env_remove("QT_QPA_PLATFORM")
        .env("WLR_BACKENDS", "headless")
        .env("WLR_HEADLESS_OUTPUTS", "1")
        .env("WLR_HEADLESS_OUTPUT_MODE", format!("{width}x{height}"))
        .env("XDG_SESSION_TYPE", "wayland")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    // New session (detach from the controlling terminal + process group) so an
    // SSH-subsystem SIGHUP on disconnect doesn't take the compositor down.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(cmd)
}

/// Poll the compositor's logfile for the `TERMLAND_SOCKET:<name>` marker echoed
/// by `socket_wrapper_cmd`, returning the `wayland-N` name. Used with the
/// detached launch (stderr goes to a file rather than a pipe we read directly).
pub fn read_socket_from_log(log_path: &Path, proc: &mut Child) -> Result<String, CompositorError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(log_path) {
            for line in content.lines() {
                if let Some(s) = line.strip_prefix("TERMLAND_SOCKET:") {
                    let name = s.trim();
                    if !name.is_empty() {
                        return Ok(name.to_string());
                    }
                }
            }
        }
        if let Ok(Some(status)) = proc.try_wait() {
            return Err(CompositorError::StartFailed(format!(
                "compositor exited early with {status} (see log {})",
                log_path.display()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(CompositorError::StartFailed(format!(
        "could not determine compositor socket from log {}",
        log_path.display()
    )))
}

/// A launched compositor process with its Wayland socket.
///
/// `_stderr_drain` keeps the compositor's stderr pipe readable for the
/// lifetime of the session. Without it, writes to stderr from labwc +
/// plasmashell + dbus etc. would SIGPIPE and kill the session.
pub struct LaunchedBackend {
    pub process: Child,
    pub wayland_display: String,
    pub _stderr_drain: std::thread::JoinHandle<()>,
}

/// Build a `Command` with the environment every headless wlroots compositor needs:
/// - WLR_BACKENDS=headless
/// - WLR_HEADLESS_OUTPUTS=1
/// - WLR_HEADLESS_OUTPUT_MODE=<width>x<height>
/// - XDG_SESSION_TYPE=wayland
/// - WAYLAND_DISPLAY / DISPLAY / GDK_BACKEND / QT_* removed so children don't
///   accidentally connect to the parent session.
pub fn compositor_command(program: &str, width: u32, height: u32) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env_remove("GDK_BACKEND")
        .env_remove("QT_WAYLAND_RECONNECT")
        .env_remove("QT_QPA_PLATFORM")
        .env("WLR_BACKENDS", "headless")
        .env("WLR_HEADLESS_OUTPUTS", "1")
        .env("WLR_HEADLESS_OUTPUT_MODE", format!("{width}x{height}"))
        .env("XDG_SESSION_TYPE", "wayland")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

/// Shell wrapper that echoes the compositor-assigned WAYLAND_DISPLAY to
/// stderr as "TERMLAND_SOCKET:<name>" before exec'ing the real child command.
/// Used to reliably detect which wayland-N socket the compositor created.
///
/// SECURITY: `child_command` is embedded in a shell string. Callers MUST
/// sanitize or validate this input before passing it here.
pub fn socket_wrapper_cmd(child_command: &str) -> String {
    format!("echo \"TERMLAND_SOCKET:$WAYLAND_DISPLAY\" >&2; exec {child_command}")
}

/// Validate a command string for shell safety. Rejects characters that could
/// enable command injection when embedded in a shell context.
/// Allows: alphanumeric, spaces, hyphens, underscores, dots, slashes, equals,
/// colons, commas, single quotes (for arguments), and @.
pub fn validate_shell_command(cmd: &str) -> Result<(), CompositorError> {
    if cmd.is_empty() {
        return Err(CompositorError::StartFailed("empty command".into()));
    }
    for ch in cmd.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => {}
            ' ' | '-' | '_' | '.' | '/' | '=' | ':' | ',' | '\'' | '@' | '+' => {}
            _ => {
                return Err(CompositorError::StartFailed(format!(
                    "rejected shell metacharacter '{ch}' in command: {cmd}"
                )));
            }
        }
    }
    // Reject if it starts with a dash (option injection)
    if cmd.starts_with('-') {
        return Err(CompositorError::StartFailed(format!(
            "command must not start with '-': {cmd}"
        )));
    }
    Ok(())
}

/// Read the socket name from the compositor's stderr stream, then spawn a
/// drainer thread that keeps the pipe open so children don't SIGPIPE.
///
/// Returns the `wayland-N` name and a JoinHandle for the drainer thread
/// (which owns the BufReader for the rest of the session's lifetime).
pub fn read_socket_name(
    stderr: ChildStderr,
    proc: &mut Child,
) -> Result<(String, std::thread::JoinHandle<()>), CompositorError> {
    use std::io::{BufRead, BufReader};

    let mut reader = BufReader::new(stderr);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut socket_name: Option<String> = None;

    // Read line-by-line until we see the marker or hit a terminal condition.
    let mut line_buf = String::new();
    while std::time::Instant::now() < deadline {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let line = line_buf.trim_end();
                if let Some(socket) = line.strip_prefix("TERMLAND_SOCKET:") {
                    socket_name = Some(socket.trim().to_string());
                    break;
                }
                if let Ok(Some(status)) = proc.try_wait() {
                    return Err(CompositorError::StartFailed(format!(
                        "compositor exited with {status}: {line}"
                    )));
                }
            }
            Err(e) => {
                return Err(CompositorError::StartFailed(format!("read stderr: {e}")));
            }
        }
    }

    let socket_name = socket_name.ok_or_else(|| {
        CompositorError::StartFailed("could not determine compositor's Wayland socket".into())
    })?;

    // Spawn a thread that keeps the pipe drained for the rest of the session.
    // Without this, the compositor and its children SIGPIPE and die as soon as
    // the stderr buffer fills up.
    let drain_handle = std::thread::Builder::new()
        .name("compositor-stderr-drain".into())
        .spawn(move || {
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) => break, // EOF - compositor exited
                    Ok(_) => {
                        // Log at debug level so we don't spam but don't silently
                        // discard potentially useful info either.
                        let line = buf.trim_end();
                        if !line.is_empty() {
                            tracing::debug!("compositor stderr: {line}");
                        }
                    }
                    Err(_) => break,
                }
            }
            tracing::debug!("compositor stderr drain thread exiting");
        })
        .expect("spawn stderr drain thread");

    Ok((socket_name, drain_handle))
}

/// Wait for the Wayland socket file to appear in XDG_RUNTIME_DIR, then
/// give the compositor a short grace period to finish initialization.
pub fn wait_socket_ready(wayland_display: &str) {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", nix::unistd::getuid()));
    let socket_path = std::path::Path::new(&runtime_dir).join(wayland_display);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if socket_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
}

/// Check if a program exists in PATH.
fn has_program(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Auto-detect a terminal emulator.
pub fn detect_terminal() -> String {
    for t in &["konsole", "foot", "alacritty", "xfce4-terminal", "xterm"] {
        if has_program(t) {
            return (*t).to_string();
        }
    }
    "xterm".to_string()
}

/// Auto-detect the best default desktop shell command for `--mode desktop`.
///
/// Priority order:
///   1. KDE Plasma: plasmashell + terminal (panels, plasmoids, wallpaper)
///   2. GNOME Shell (unlikely to work well in labwc, but we try)
///   3. Bare terminal fallback
///
/// The returned command is passed to `sh -c` inside labwc via its -S flag.
pub fn detect_desktop_shell() -> String {
    let terminal = detect_terminal();

    if has_program("plasmashell") && has_program("dbus-run-session") {
        // Start plasmashell (panels/plasmoids) in the background, plus a terminal
        // in the foreground so something is visible and labwc has a session process
        // to tie its lifetime to. When the terminal exits, labwc exits.
        return format!("dbus-run-session sh -c 'plasmashell & exec {terminal}'");
    }

    terminal
}

pub mod cage;
pub mod labwc;
