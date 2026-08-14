//! Live check that window enumeration works against a real compositor.
//!
//! `#[ignore]`d by default: this spawns an actual headless labwc and a real
//! GUI application, which a CI runner cannot do. Run it on a desktop with:
//!
//! ```text
//! cargo test -p termland-compositor --test toplevel_enumeration -- --ignored --nocapture
//! ```
//!
//! The unit tests in `toplevels.rs` cover the bookkeeping (id stability, state
//! replacement, entry reuse). They deliberately do not prove that the protocol
//! exchange works, which is the part that would break if the compositor or the
//! wayland crates changed — hence this.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use termland_compositor::ToplevelWatcher;

/// Kills the compositor however the test exits, including on a panicking
/// assertion. Without this a failed run leaves a headless labwc — and whatever
/// it launched — running forever, which is exactly the leak that bit
/// `quic_q2_planes`.
struct LabwcGuard(Child);
impl Drop for LabwcGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Read labwc's stderr until the startup command echoes its Wayland socket.
///
/// Uses the same `TERMLAND_SOCKET:` marker the production launcher relies on
/// (`backend::startup_command_with_socket_echo`) rather than trying to parse
/// labwc's own logging, which is not a stable interface.
fn wait_for_socket(child: &mut Child, timeout: Duration) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let stderr = child.stderr.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("[labwc] {line}");
            if let Some(name) = line.strip_prefix("TERMLAND_SOCKET:") {
                let _ = tx.send(name.trim().to_string());
                return;
            }
        }
    });
    rx.recv_timeout(timeout).ok()
}

#[test]
#[ignore = "needs a real headless compositor and a GUI app; run manually on a desktop"]
fn enumerates_a_real_window_from_labwc() {
    let runtime_dir: PathBuf = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }))
        .into();

    // A terminal is the least heavyweight thing guaranteed to map a toplevel.
    let app = ["foot", "konsole", "alacritty", "xterm"]
        .into_iter()
        .find(|a| Command::new("which").arg(a).output().is_ok_and(|o| o.status.success()))
        .expect("no terminal emulator available to open a window with");
    eprintln!("[test] using {app} as the window to find");

    let child = Command::new("labwc")
        .arg("-S")
        .arg(format!(
            "sh -c 'echo \"TERMLAND_SOCKET:$WAYLAND_DISPLAY\" >&2; exec {app}'"
        ))
        .env("WLR_BACKENDS", "headless")
        .env("WLR_HEADLESS_OUTPUTS", "1")
        .env("WLR_HEADLESS_OUTPUT_MODE", "1280x720")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn labwc");

    // Guard *before* the first fallible step: everything below can panic, and
    // an unguarded compositor would outlive the test run.
    let mut guard = LabwcGuard(child);

    let display = wait_for_socket(&mut guard.0, Duration::from_secs(15))
        .expect("labwc never announced a wayland socket");
    eprintln!("[test] labwc up on {display}");

    let mut watcher = ToplevelWatcher::connect(&display, &runtime_dir)
        .expect("labwc must advertise zwlr_foreign_toplevel_management_v1");

    // The terminal needs a moment to actually map its window.
    let windows = watcher
        .poll_until_any(Duration::from_secs(15))
        .expect("polling for toplevels failed");

    eprintln!("[test] enumerated {} window(s): {windows:#?}", windows.len());

    assert!(
        !windows.is_empty(),
        "no windows enumerated — labwc advertised the protocol but reported nothing",
    );
    assert!(
        windows.iter().any(|w| !w.app_id.is_empty() || !w.title.is_empty()),
        "a window was found but carried neither app_id nor title, so a task list \
         would have nothing to show: {windows:#?}",
    );

    // Ids must be stable across polls — a task list that re-keys its entries
    // every refresh cannot track selection.
    let again = watcher.poll().expect("second poll failed");
    let before: Vec<u32> = windows.iter().map(|w| w.id).collect();
    let after: Vec<u32> = again.iter().map(|w| w.id).collect();
    assert_eq!(before, after, "window ids changed between polls");

    let _ = guard.0.kill();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(3) {
        if matches!(guard.0.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
