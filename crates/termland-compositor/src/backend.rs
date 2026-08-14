//! Shared backend support for launching headless wlroots compositors
//! (cage for single-app, labwc for full desktop).

use std::ffi::CString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use crate::session::CompositorError;

/// A detached compositor: its own process-session (setsid) with stdio going to
/// a logfile, so it survives the connection process that spawned it. Tracked by
/// PID (liveness via `kill(pid, 0)`); terminated explicitly, never on drop.
pub struct DetachedBackend {
    pub pid: u32,
    pub wayland_display: String,
    /// The `XDG_RUNTIME_DIR` this compositor was actually started with -
    /// **not necessarily this (server) process's own**. Under session
    /// isolation (`run_as: Some`) the compositor runs with the target user's
    /// `/run/user/<uid>`, which differs from whatever the server process
    /// itself inherited. Every later Wayland connection to this compositor
    /// - screen capture, output resize, cursor capture, and the
    /// wl-copy/wl-paste-based clipboard/file-transfer subprocesses in
    /// `termland-server` - MUST use this value, not read `XDG_RUNTIME_DIR`
    /// from their own environment, or they will simply fail to find the
    /// socket for an isolated session.
    pub runtime_dir: PathBuf,
}

/// A system user resolved via `getpwnam_r`, ready to drop the compositor's
/// privileges into. All fields are plain data (no further allocation needed)
/// so they can be moved as-is into a `pre_exec` closure, which runs in the
/// forked child and must not allocate (see `detached_compositor_command`).
///
/// # Session-lifecycle scope (deliberately not done here)
///
/// TODO(followup, security): this drops the *process* uid/gid/supplementary
/// groups, which is enough for filesystem/IPC isolation between sessions, but
/// it is not a full PAM/logind login: no `pam_open_session`/`pam_close_session`
/// /`pam_setcred`, no systemd-logind session registration, no cgroup
/// placement. A real login would want a PAM handle held open for the whole
/// compositor lifetime and closed when the session ends, but the compositor
/// here is a *detached* process (see `DetachedBackend`'s doc comment) that
/// deliberately outlives the connection handler that spawns it — there is no
/// natural point in the current control flow to hold that handle across a
/// process boundary. Do not attempt to bolt this on without rethinking the
/// detached-process architecture first.
#[derive(Clone, Debug)]
pub struct TargetUser {
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    pub home: String,
    /// Pre-built C string of the username. Kept around specifically for
    /// `initgroups()` inside `pre_exec`, which cannot itself call
    /// `CString::new` (allocation is not async-signal-safe).
    username_c: CString,
}

/// Resolve `username` to a system user via the thread-safe `getpwnam_r`.
///
/// Deliberately NOT `getpwnam`: that function returns a pointer into a single
/// shared static buffer and is documented as unsafe to call from more than
/// one thread at a time. This server handles many connections concurrently
/// (each on its own tokio task / blocking thread), so a data race here would
/// be a real, remotely-triggerable bug, not a theoretical one.
///
/// Fails closed on anything short of "a real, non-root system user": unknown
/// username, a `getpwnam_r` error, or a resolved uid of 0. In particular, a
/// PAM-authenticated "root" is refused rather than silently kept privileged —
/// callers must not interpret a resolution failure as "fall back to running
/// as the server's own user"; the whole point of this feature is to *not* do
/// that.
pub fn resolve_target_user(username: &str) -> Result<TargetUser, CompositorError> {
    let username_c = CString::new(username)
        .map_err(|_| CompositorError::StartFailed(format!("username contains NUL byte: {username:?}")))?;

    // SAFETY: `pwd` is a plain-old-data struct that getpwnam_r fully
    // populates on success; zero-initializing before the call is standard
    // practice for this API and only matters if we bail out before rc==0.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // getpwnam_r needs a caller-supplied buffer for the strings it points
    // `pwd`'s fields into. Start reasonably large and grow on ERANGE rather
    // than trying to size it perfectly up front.
    //
    // `libc::c_char`, NOT a hardcoded `i8`: on Linux, `c_char` is signed on
    // x86_64 but UNSIGNED on aarch64 - `getpwnam_r`'s buffer parameter type
    // follows the platform's `c_char`, so a hardcoded `i8` compiles fine on
    // x86_64 and fails to compile at all on aarch64 (`expected *mut u8,
    // found *mut i8`). Caught by a real aarch64 COPR build failure.
    let mut buf: Vec<libc::c_char> = vec![0; 4096];

    loop {
        let rc = unsafe {
            libc::getpwnam_r(
                username_c.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == 0 {
            break;
        }
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return Err(CompositorError::StartFailed(format!(
            "getpwnam_r({username}) failed: errno {rc}"
        )));
    }

    if result.is_null() {
        // rc == 0 but no entry found - "no such user", not an error.
        return Err(CompositorError::StartFailed(format!(
            "cannot isolate session: no such system user '{username}'"
        )));
    }

    if pwd.pw_uid == 0 {
        return Err(CompositorError::StartFailed(format!(
            "refusing to run an isolated session as uid 0 (user '{username}' \
             resolved to root) - this would defeat session isolation entirely"
        )));
    }

    if pwd.pw_dir.is_null() {
        return Err(CompositorError::StartFailed(format!(
            "user '{username}' has no home directory in passwd"
        )));
    }
    // SAFETY: pw_dir is a non-null NUL-terminated string owned by `buf`
    // (getpwnam_r points passwd's string fields into the buffer we gave it),
    // which is still alive here. We copy it into an owned String immediately
    // so nothing borrows from `buf` past this function returning.
    let home = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }
        .to_string_lossy()
        .into_owned();

    Ok(TargetUser {
        uid: pwd.pw_uid,
        gid: pwd.pw_gid,
        home,
        username_c,
    })
}

/// Ensure `/run/user/<uid>` exists and is owned/permissioned correctly for
/// `target`, returning its path. On a normal interactive login this
/// directory is created by `pam_systemd`/logind; since this server
/// deliberately does not invoke that machinery (see `TargetUser`'s doc
/// comment), we create it ourselves the first time a session isolates into a
/// given user. If it already exists (a real login for the same user is also
/// active, or a previous termland session created it) we leave it alone
/// rather than re-chowning a directory something else may be relying on.
///
/// Must be called while still privileged (root) - an unprivileged process
/// cannot `chown` a directory to another uid.
fn ensure_runtime_dir(target: &TargetUser) -> Result<PathBuf, CompositorError> {
    let dir = PathBuf::from(format!("/run/user/{}", target.uid));
    if dir.exists() {
        return Ok(dir);
    }

    std::fs::create_dir_all(&dir)
        .map_err(|e| CompositorError::StartFailed(format!("mkdir {}: {e}", dir.display())))?;

    let dir_c = CString::new(dir.as_os_str().as_encoded_bytes())
        .map_err(|e| CompositorError::StartFailed(format!("runtime dir path: {e}")))?;

    // SAFETY: dir_c is a valid NUL-terminated path we just created; chown/
    // chmod are plain syscalls with no aliasing/lifetime hazards here.
    let rc = unsafe { libc::chown(dir_c.as_ptr(), target.uid, target.gid) };
    if rc != 0 {
        return Err(CompositorError::StartFailed(format!(
            "chown {} to uid={} gid={}: {}",
            dir.display(), target.uid, target.gid, std::io::Error::last_os_error()
        )));
    }
    let rc = unsafe { libc::chmod(dir_c.as_ptr(), 0o700) };
    if rc != 0 {
        return Err(CompositorError::StartFailed(format!(
            "chmod {} to 0700: {}", dir.display(), std::io::Error::last_os_error()
        )));
    }

    Ok(dir)
}

/// Like `compositor_command`, but detaches the child into its own session and
/// sends stdout+stderr to `log_path`. `setsid` keeps the compositor alive when
/// the spawning process (e.g. an SSH-subsystem connection) exits, and the
/// logfile avoids the SIGPIPE that a closed stderr pipe would cause.
///
/// `run_as`: when `Some(username)`, the spawned process (and everything cage/
/// labwc launches inside the session) drops privileges to that system user
/// immediately after `fork()`, before `exec()` — see the `pre_exec` closure
/// below for the exact mechanics and why the ordering inside it is not
/// negotiable. When `None` (no `--auth`, so there is no PAM-authenticated
/// identity to isolate into), behavior is exactly as before this feature:
/// the compositor runs as the server's own user.
///
/// Returns the `Command` plus the `XDG_RUNTIME_DIR` it will use, so callers
/// (`wait_socket_ready`) poll for the Wayland socket in the *same* directory
/// the child was actually told to use — which is the target user's
/// `/run/user/<uid>` when `run_as` is set, not the server process's own
/// runtime dir.
pub fn detached_compositor_command(
    program: &str,
    width: u32,
    height: u32,
    log_path: &Path,
    run_as: Option<&str>,
    audio_sink: Option<&str>,
) -> Result<(Command, PathBuf), CompositorError> {
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
        .env("XDG_SESSION_TYPE", "wayland");

    // Route this session's audio to its own sink, for this process tree only.
    // Setting the *global* default sink instead would silence the local
    // desktop for as long as the session runs, and strand it on a dead sink
    // if the server exits without unloading the null sink.
    if let Some(sink) = audio_sink {
        cmd.env("PULSE_SINK", sink);
    }

    cmd
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

    let runtime_dir = match run_as {
        None => std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{}", nix::unistd::getuid()))),
        Some(username) => {
            // Resolve the uid/gid/home *here*, in the parent, while still
            // privileged - not inside pre_exec. A bad username or uid-0
            // resolution needs to fail this whole session-creation call with
            // a normal `Result`/log line; inside pre_exec our only recourse
            // is "the syscall failed", which is far less informative and, if
            // this resolution logic were duplicated there, would also be
            // running in an async-signal-safe context that can't allocate to
            // even build the error message.
            let target = resolve_target_user(username)?;
            let runtime_dir = ensure_runtime_dir(&target)?;

            cmd.env("HOME", &target.home)
                .env("USER", username)
                .env("LOGNAME", username)
                .env("XDG_RUNTIME_DIR", &runtime_dir);

            // Data moved into the pre_exec closure below. Everything here is
            // already-resolved plain data (an owned CString + two integers) -
            // no lookups or string-building happen inside the closure itself.
            let username_c = target.username_c.clone();
            let uid = target.uid;
            let gid = target.gid;

            // SAFETY / ASYNC-SIGNAL-SAFETY: this closure runs in the child
            // between fork() and exec(), a context where the process may
            // have only one thread (this one) but shares its address space
            // with a parent that could have other threads mid-allocation, so
            // only async-signal-safe operations are allowed: no heap
            // allocation, no locking, no formatting - raw syscalls on data
            // that was fully prepared above, in the parent, before fork.
            //
            // Ordering is load-bearing, not stylistic:
            //   1. initgroups() BEFORE setgid()/setuid() - sets this
            //      process's supplementary group list to the TARGET user's
            //      own groups (looked up from /etc/group by initgroups
            //      itself via `username_c`). Skipping this is the single
            //      most common privilege-drop bug there is: without it, the
            //      child keeps the SERVER's supplementary groups (root's,
            //      typically just `root`/`wheel`-equivalent, but potentially
            //      `docker` or other privileged groups on some systems) even
            //      after setuid/setgid succeed and the primary ids look
            //      correctly dropped. initgroups() requires CAP_SETGID,
            //      which we still have at this point (we haven't called
            //      setuid yet).
            //   2. setgid() BEFORE setuid() - once setuid() drops root's
            //      effective uid, the process no longer has the capability
            //      needed to change its gid at all (dropping uid drops the
            //      capability set derived from being uid 0 first). Reversing
            //      these two would either fail setgid() outright, or -worse-
            //      silently leave the process running with the SERVER's
            //      original gid if the failure isn't checked.
            //   3. setuid() LAST - the final, unrecoverable drop back to an
            //      unprivileged uid.
            // Every step's return value is checked and turned into an `Err`
            // on failure. Returning `Err` here aborts `Command::spawn()`
            // cleanly (posix_spawn/fork machinery reports it as a normal
            // spawn error; the child never reaches exec()) - that
            // Result-checking IS the verification. There is deliberately no
            // "verify the drop succeeded" step in the parent after spawn():
            // by the time the parent observes anything, the child has either
            // already exec'd (drop succeeded) or already exited (it didn't),
            // so there is nothing left in the child to inspect from outside.
            unsafe {
                cmd.pre_exec(move || {
                    if libc::initgroups(username_c.as_ptr(), gid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setgid(gid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setuid(uid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }

            runtime_dir
        }
    };

    Ok((cmd, runtime_dir))
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

/// Wait for the Wayland socket file to appear in `runtime_dir`, then give the
/// compositor a short grace period to finish initialization.
///
/// `runtime_dir` must be the *same* directory the compositor process was
/// actually told to use (its `XDG_RUNTIME_DIR`), which is `detached_compositor_command`'s
/// second return value - it is NOT necessarily this (server) process's own
/// `XDG_RUNTIME_DIR`/uid. When a session runs as a different target user
/// (privilege drop), the socket appears under that user's `/run/user/<uid>`,
/// not the server's - polling the server's own directory would never find it.
pub fn wait_socket_ready(wayland_display: &str, runtime_dir: &Path) {
    let socket_path = runtime_dir.join(wayland_display);

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
/// Is `name` an executable somewhere on `PATH`?
///
/// Resolves `PATH` directly rather than shelling out to `which`. That binary is
/// not guaranteed to exist — a minimal Fedora container does not ship it, and
/// distributions have been dropping it from default installs — and when it is
/// missing, `status()` fails and *every* lookup here quietly returns false.
///
/// The failure that exposed this is worth recording, because nothing about it
/// pointed at program detection: `detect_terminal` fell through every candidate
/// to its last-resort `xterm`, and the session died with
/// `sh: exec: xterm: not found`. The same silent-false would also skip the
/// plasmashell branch of `detect_desktop_shell`, downgrading a desktop session
/// to a bare terminal with no explanation.
fn has_program(name: &str) -> bool {
    // A name with a separator is a path, not something to search for — matching
    // how a shell treats `./foo` versus `foo`.
    if name.contains('/') {
        return is_executable_file(Path::new(name));
    }

    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable_file(&dir.join(name)))
}

/// Does `path` exist, is it a regular file, and is any execute bit set?
///
/// Checks the mode rather than calling `access(X_OK)` because this only needs
/// to be right about the common case, and a file we can see but not execute is
/// indistinguishable from absent for the purpose of picking a default.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
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

#[cfg(test)]
mod program_lookup_tests {
    use super::*;

    /// The regression this guards: `which` is absent from minimal images, and
    /// the old implementation shelled out to it, so every lookup returned
    /// false and terminal detection fell through to a fallback that was not
    /// installed either.
    #[test]
    fn finds_a_program_that_is_definitely_on_path() {
        assert!(has_program("sh"), "sh must be found via PATH");
    }

    #[test]
    fn does_not_find_a_program_that_does_not_exist() {
        assert!(!has_program("termland-definitely-not-a-real-binary-xyzzy"));
    }

    /// A name containing a separator is a path, as in a shell.
    #[test]
    fn absolute_paths_are_used_directly() {
        assert!(has_program("/bin/sh") || has_program("/usr/bin/sh"));
        assert!(!has_program("/nonexistent/path/to/nothing"));
    }

    /// A directory on PATH sharing a candidate's name must not count as a
    /// match — exec would fail on it.
    #[test]
    fn directories_are_not_executables() {
        assert!(!is_executable_file(Path::new("/usr")));
        assert!(!is_executable_file(Path::new("/tmp")));
    }

    /// A readable but non-executable file is not a program.
    #[test]
    fn non_executable_files_are_rejected() {
        assert!(!is_executable_file(Path::new("/etc/hostname")));
    }

    /// With no PATH at all, a bare name resolves to nothing rather than
    /// panicking or falling back to a guess.
    #[test]
    fn missing_path_variable_yields_no_match() {
        let saved = std::env::var_os("PATH");
        // SAFETY: single-threaded test; PATH is restored before returning.
        unsafe { std::env::remove_var("PATH") };
        let found = has_program("sh");
        if let Some(v) = saved {
            unsafe { std::env::set_var("PATH", v) };
        }
        assert!(!found, "no PATH must mean no match");
    }

    /// detect_terminal must return something that actually exists when any
    /// candidate is installed — the old bug made it return a name that was
    /// not present, which only failed later at exec time.
    #[test]
    fn detected_terminal_is_a_real_program_when_one_exists() {
        let candidates = ["konsole", "foot", "alacritty", "xfce4-terminal", "xterm"];
        if candidates.iter().any(|c| has_program(c)) {
            let picked = detect_terminal();
            assert!(
                has_program(&picked),
                "detect_terminal returned {picked:?}, which is not on PATH",
            );
        }
    }
}

#[cfg(test)]
mod target_user_tests {
    use super::resolve_target_user;

    /// The "fail closed on root" rule is the whole point of this feature -
    /// if a caller passed "root" as the PAM-authenticated username (e.g. a
    /// misconfigured PAM service allowing root login), we must refuse to
    /// isolate into it rather than silently keeping the compositor running
    /// as the server's own (privileged) user. This is testable without
    /// actually being root: `getpwnam_r("root")` succeeds as a plain lookup
    /// for anyone, it's the *use* of uid 0 that's privileged, not the lookup.
    #[test]
    fn refuses_uid_zero() {
        let err = resolve_target_user("root").unwrap_err().to_string();
        assert!(err.contains("uid 0"), "expected a uid-0 refusal, got: {err}");
    }

    /// An unknown username must be rejected explicitly, not treated as "fall
    /// back to something else". The username is deliberately implausible so
    /// this doesn't flake on a system that happens to have such an account.
    #[test]
    fn refuses_unknown_username() {
        let err = resolve_target_user("termland-test-nonexistent-user-8f3c1a")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no such system user"), "expected a no-such-user error, got: {err}");
    }

    /// A username containing a NUL byte can't even be turned into a
    /// C string for the syscall - must be rejected before any FFI call,
    /// not panic or truncate silently.
    #[test]
    fn refuses_embedded_nul() {
        assert!(resolve_target_user("bad\0user").is_err());
    }
}
