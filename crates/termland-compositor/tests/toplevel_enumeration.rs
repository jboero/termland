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
use std::sync::{Arc, Mutex};
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

/// First of `names` that is an executable on `PATH`.
///
/// Resolves PATH directly rather than shelling out to `which`, which a minimal
/// container image does not ship — the same trap that made the server fall
/// through to a terminal that was not installed.
fn first_on_path(names: &[&str]) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for name in names {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if std::fs::metadata(&candidate).map(|m| m.is_file()).unwrap_or(false) {
                return Some((*name).to_string());
            }
        }
    }
    None
}

/// Read labwc's stderr until the startup command echoes its Wayland socket.
///
/// Uses the same `TERMLAND_SOCKET:` marker the production launcher relies on
/// (`backend::startup_command_with_socket_echo`) rather than trying to parse
/// labwc's own logging, which is not a stable interface.
fn wait_for_socket(
    child: &mut Child,
    timeout: Duration,
    log: Arc<Mutex<Vec<String>>>,
) -> Option<String> {
    use std::io::{BufRead, BufReader};
    let stderr = child.stderr.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // Keep draining after the socket appears rather than returning. The
    // interesting output is what comes *later* — the startup command dying
    // takes labwc with it, and the test then fails on a broken pipe that says
    // nothing about why. Everything is kept so a failure can print it.
    std::thread::spawn(move || {
        let mut announced = false;
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            log.lock().unwrap().push(line.clone());
            if !announced {
                if let Some(name) = line.strip_prefix("TERMLAND_SOCKET:") {
                    announced = true;
                    let _ = tx.send(name.trim().to_string());
                }
            }
        }
    });
    rx.recv_timeout(timeout).ok()
}

/// Print everything the compositor said. Called on the failure paths, where
/// the Wayland-level error ("broken pipe") is a consequence rather than a
/// cause.
fn dump_log(log: &Arc<Mutex<Vec<String>>>) -> String {
    let lines = log.lock().unwrap();
    if lines.is_empty() {
        return "  (compositor produced no output)".to_string();
    }
    lines.iter().map(|l| format!("  [labwc] {l}")).collect::<Vec<_>>().join("\n")
}

#[test]
#[ignore = "needs a real headless compositor and a GUI app; run manually on a desktop"]
fn enumerates_a_real_window_from_labwc() {
    let runtime_dir: PathBuf = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }))
        .into();

    // A terminal is the least heavyweight thing guaranteed to map a toplevel.
    let app = first_on_path(&["foot", "konsole", "alacritty", "xterm"])
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

    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let display = match wait_for_socket(&mut guard.0, Duration::from_secs(15), log.clone()) {
        Some(d) => d,
        None => panic!(
            "labwc never announced a wayland socket. Compositor output:\n{}",
            dump_log(&log)
        ),
    };
    eprintln!("[test] labwc up on {display}");

    let mut watcher = ToplevelWatcher::connect(&display, &runtime_dir)
        .expect("labwc must advertise zwlr_foreign_toplevel_management_v1");

    // The terminal needs a moment to actually map its window.
    let windows = match watcher.poll_until_any(Duration::from_secs(15)) {
        Ok(w) => w,
        Err(e) => panic!(
            "polling for toplevels failed: {e}\n\
             This usually means the compositor exited, which happens when its \
             startup command dies. Compositor output:\n{}",
            dump_log(&log)
        ),
    };

    eprintln!("[test] enumerated {} window(s): {windows:#?}", windows.len());

    assert!(
        !windows.is_empty(),
        "no windows enumerated — labwc advertised the protocol but reported \
         nothing. Compositor output:\n{}",
        dump_log(&log),
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
