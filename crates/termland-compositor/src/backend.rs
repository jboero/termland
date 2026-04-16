//! Shared backend support for launching headless wlroots compositors
//! (cage for single-app, labwc for full desktop).

use std::process::{Child, ChildStderr, Command, Stdio};
use crate::session::CompositorError;

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
