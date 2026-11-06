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
    has_program_in(name, std::env::var_os("PATH").as_deref())
}

/// `has_program` with the search path passed in.
///
/// Split out purely so it can be tested: the alternative is a test that
/// mutates `PATH` for the whole process, which races every other test in the
/// binary because they run on parallel threads. That is not theoretical — the
/// first version of this did exactly that and made a sibling test fail.
fn has_program_in(name: &str, path: Option<&std::ffi::OsStr>) -> bool {
    // A name with a separator is a path, not something to search for — matching
    // how a shell treats `./foo` versus `foo`.
    if name.contains('/') {
        return is_executable_file(Path::new(name));
    }

    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path).any(|dir| is_executable_file(&dir.join(name)))
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
///   1. KDE Plasma in a nested KWin (`plasma_desktop_command`)
///   2. KDE Plasma directly in labwc, when KWin is not installed
///   3. Bare terminal fallback
///
/// The returned command is passed to `sh -c` inside labwc via its -S flag.
///
/// `logout_service` is started alongside plasmashell, on the same private
/// bus, so Plasma's Log Out has something to talk to; without it, Log Out
/// does nothing. It is left out if it is not safe to embed in the script.
pub fn detect_desktop_shell(logout_service: Option<&str>) -> String {
    let terminal = detect_terminal();
    if has_program("plasmashell") && has_program("dbus-run-session") {
        let shell = if !has_program("kwin_wayland") {
            PlasmaShell::Labwc
        } else if !has_render_node() {
            tracing::warn!(
                "no usable GPU render node in /dev/dri: KWin cannot run nested without one, so \
                 Plasma runs directly in labwc, where Plasma 6.8 and later shows no desktop or \
                 panel, only the terminal"
            );
            PlasmaShell::Labwc
        } else {
            PlasmaShell::NestedKwin
        };
        return plasma_desktop_command(shell, logout_service, &terminal);
    }
    terminal
}

/// The host has a GPU render node this process can open, which a nested KWin
/// needs: its Wayland backend composites with EGL only. Without one it does
/// not fail, either; it keeps running and draws nothing, so this has to be
/// decided before it is started. Measured: in a CPU-rendered labwc, KWin had
/// its socket up 5 ms after starting and was still running, with nothing
/// drawn, 12 s later.
fn has_render_node() -> bool {
    has_render_node_in(Path::new("/dev/dri"))
}

fn has_render_node_in(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with("renderD")
            && std::fs::OpenOptions::new().read(true).write(true).open(e.path()).is_ok()
    })
}

/// Where plasmashell runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlasmaShell {
    /// In KWin, which runs full-screen as a window in labwc.
    ///
    /// Plasma 6.8 only works under KWin. plasmashell learns which screens it
    /// has from KWin's `kde_output_order_v1` protocol; up to 6.7 it fell back
    /// to Qt's screen list on compositors without it, and 6.8 removed that
    /// fallback. Under labwc plasmashell now finds no screens and draws
    /// nothing: no wallpaper, no panel, just whatever else was started.
    ///
    /// labwc stays the outer compositor because capture and input injection
    /// use wlroots protocols KWin does not offer. KWin is a client of labwc
    /// that composites with the GPU (its nested mode has no software path),
    /// and everything in the desktop is a client of KWin.
    NestedKwin,
    /// Directly in labwc. Works up to Plasma 6.7; used when KWin is not
    /// installed.
    Labwc,
}

/// The Plasma desktop command for `shell`. Pure, so both forms can be tested
/// on any machine.
///
/// Details here are load-bearing, learned from stranding `plasmashell`
/// processes on a developer workstation for hours:
///
///   --no-respawn   Bare plasmashell restarts itself when it dies, so a
///                  stranded instance resurrects indefinitely and no amount
///                  of killing settles it. A real Plasma session passes this
///                  flag for the same reason.
///
///   no `exec`      `exec` would replace this shell, destroying the only
///                  process that can clean up after plasmashell. Keeping the
///                  shell costs one process and buys a trap that stops the
///                  desktop when the session ends, instead of orphaning it.
///
///   wait after kill  Without it the shell exits as soon as the signal is
///                  sent, `dbus-run-session` tears down the private session
///                  bus, and plasmashell, still shutting down, aborts inside
///                  libdbus (`_dbus_warn_check_failed` <- `QDBusConnection`),
///                  a coredump and a KCrash handler on every teardown.
///                  Waiting keeps the bus alive until plasmashell is done
///                  with it. With KWin, plasmashell is stopped before KWin,
///                  so it is not left without its compositor either.
///
/// The trap does not run on SIGKILL, so this does not replace reaping the
/// process group in `registry::close`; it just means the common case never
/// gets that far.
///
/// The logout service is started first so it owns its names before anything
/// could ask for them, and stopped by the same trap.
pub fn plasma_desktop_command(shell: PlasmaShell, logout_service: Option<&str>, terminal: &str) -> String {
    let (start_logout, logout_pid) = match logout_service {
        Some(cmd) if validate_shell_command(cmd).is_ok() => (format!("{cmd} & logout=$!; "), " $logout"),
        Some(cmd) => {
            tracing::warn!("not starting logout service, unsafe command: {cmd:?}");
            (String::new(), "")
        }
        None => (String::new(), ""),
    };
    match shell {
        // KWin gets its own socket name, unique to this session, and
        // plasmashell is pointed at it once it exists. The session lasts as
        // long as KWin; Log Out ends it through the logout service.
        //
        // KWin's default Wayland socket would be the first free wayland-N,
        // which another session could take between KWin choosing it and
        // plasmashell connecting.
        //
        // kded6, which D-Bus starts rather than this script, is killed
        // (`stop_kded`) once plasmashell has stopped and again after KWin.
        // It crashes in its destructor (the device-notifications module
        // using a dead Wayland object) however it is asked to quit, even with
        // KWin still up (`kquitapp6 kded6` did that too), and an instance
        // D-Bus starts during teardown aborts for want of a display. SIGKILL
        // runs no destructor and dumps no core. Only this session's: D-Bus
        // double-forks what it starts, so kded6 is not a child of anything
        // here, but it keeps the session's process group, which is labwc's
        // pid (labwc is setsid'd). A plain `pkill kded6` would take the
        // host's own Plasma session's kded6 with it.
        //
        // The script records its pid (`desktop_pid_file`) so the session is
        // ended through it (`end_desktop`), not by stopping labwc: KWin is a
        // labwc client and aborts the moment labwc goes, and plasmashell,
        // a KWin client, with it. Signalled, the trap stops them in order;
        // then `dbus-run-session` takes the bus down, and labwc exits because
        // its session command (-S) has.
        PlasmaShell::NestedKwin => format!(
            "dbus-run-session sh -c '{start_logout}\
             desktop=$XDG_RUNTIME_DIR/termland-desktop-$LABWC_PID; echo $$ > $desktop; \
             stop_kded() {{ [ -n \"$LABWC_PID\" ] && pkill -KILL -x -g $LABWC_PID kded6; }}; \
             kwin=termland-kwin-$$; \
             kwin_wayland --socket $kwin --fullscreen true --xwayland --no-lockscreen & kwin_pid=$!; \
             i=0; while [ ! -S \"$XDG_RUNTIME_DIR/$kwin\" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done; \
             WAYLAND_DISPLAY=$kwin plasmashell --no-respawn & shell=$!; \
             trap \"trap - EXIT INT TERM; kill $shell{logout_pid} 2>/dev/null; wait $shell 2>/dev/null; stop_kded; kill $kwin_pid 2>/dev/null; wait $kwin_pid 2>/dev/null; stop_kded; rm -f $desktop\" EXIT INT TERM; \
             wait $kwin_pid'"
        ),
        // A terminal in the foreground so something is visible and labwc has
        // a session process to tie its lifetime to. When the terminal exits,
        // labwc exits.
        PlasmaShell::Labwc => format!(
            "dbus-run-session sh -c '{start_logout}plasmashell --no-respawn & shell=$!; \
             trap \"kill $shell{logout_pid} 2>/dev/null; wait $shell 2>/dev/null\" EXIT INT TERM; \
             {terminal}'"
        ),
    }
}

/// Where the nested-KWin desktop script records its pid: in the session's
/// runtime dir, named for its labwc, which exports `LABWC_PID` to it.
pub fn desktop_pid_file(runtime_dir: &Path, labwc_pid: u32) -> PathBuf {
    runtime_dir.join(format!("termland-desktop-{labwc_pid}"))
}

/// Ask the session's desktop to end itself, the clean way to end a session
/// that has one: SIGTERM to the desktop script, which stops plasmashell and
/// then KWin, after which labwc exits too. See `PlasmaShell::NestedKwin`.
///
/// Returns false, having signalled nothing, when there is no such script (no
/// pid file: another desktop, or one from before this existed) or the pid in
/// the file is not in the session's process group, which a stale file whose
/// pid was reused would not be. The caller then stops labwc itself.
pub fn end_desktop(runtime_dir: &Path, labwc_pid: u32) -> bool {
    let Ok(text) = std::fs::read_to_string(desktop_pid_file(runtime_dir, labwc_pid)) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<libc::pid_t>() else {
        return false;
    };
    // labwc is setsid'd, so the session's group id is its pid.
    if pid <= 0 || unsafe { libc::getpgid(pid) } != labwc_pid as libc::pid_t {
        return false;
    }
    unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
}

pub mod cage;
pub mod labwc;

#[cfg(test)]
mod desktop_shell_tests {
    use super::*;

    const LOGOUT: &str = "/usr/bin/termland-server --logout-service";

    /// Both forms of the Plasma command, built directly so the tests do not
    /// depend on what is installed here.
    fn plasma_commands() -> [(PlasmaShell, String); 2] {
        [PlasmaShell::NestedKwin, PlasmaShell::Labwc]
            .map(|shell| (shell, plasma_desktop_command(shell, Some(LOGOUT), "konsole")))
    }

    fn inner(cmd: &str) -> &str {
        cmd.split_once("dbus-run-session sh -c '")
            .and_then(|(_, rest)| rest.strip_suffix('\''))
            .expect("expected a `dbus-run-session sh -c '...'` wrapper")
    }

    /// Log Out talks to the logout service over the session's bus, so it has
    /// to be inside dbus-run-session, running before the desktop, and
    /// stopped with it.
    #[test]
    fn the_logout_service_runs_on_the_session_bus_and_is_stopped() {
        for (shell, cmd) in plasma_commands() {
            assert!(
                inner(&cmd).starts_with(&format!("{LOGOUT} & logout=$!; ")),
                "{shell:?}: logout service must start first, inside the bus: {cmd}"
            );
            assert!(cmd.contains("kill $shell $logout"), "{shell:?}: trap must stop the logout service: {cmd}");
        }
    }

    #[test]
    fn an_unsafe_logout_service_command_is_left_out() {
        for shell in [PlasmaShell::NestedKwin, PlasmaShell::Labwc] {
            let cmd = plasma_desktop_command(shell, Some("/tmp/x; rm -rf ~"), "konsole");
            assert!(!cmd.contains("rm -rf"), "{shell:?}: unsafe command embedded: {cmd}");
            assert!(!cmd.contains("$logout"), "{shell:?}: {cmd}");
        }
    }

    /// Bare plasmashell restarts itself when it dies, so a stranded instance
    /// resurrects forever and killing it never settles anything.
    #[test]
    fn plasmashell_is_launched_with_no_respawn() {
        for (shell, cmd) in plasma_commands() {
            assert!(
                cmd.contains("plasmashell --no-respawn"),
                "{shell:?}: plasmashell must not be allowed to respawn: {cmd}"
            );
        }
    }

    /// `exec` replaced the shell with the terminal, destroying the only
    /// process that could clean up the backgrounded plasmashell. The shell has
    /// to survive for the trap to exist at all.
    #[test]
    fn the_wrapper_shell_survives_to_run_its_trap() {
        for (shell, cmd) in plasma_commands() {
            assert!(!cmd.contains("exec "), "{shell:?}: exec destroys the shell that owns plasmashell: {cmd}");
            assert!(cmd.contains("trap "), "{shell:?}: no trap to clean up plasmashell: {cmd}");
            assert!(
                cmd.contains("EXIT") && cmd.contains("TERM"),
                "{shell:?}: trap must cover normal exit and termination: {cmd}"
            );
        }
    }

    /// The command is handed to `sh -c`; if it is not valid shell the session
    /// dies at startup with a message about syntax rather than about Termland.
    #[test]
    fn the_command_is_valid_shell() {
        for (shell, cmd) in plasma_commands() {
            // `sh -n` parses without executing.
            let status = std::process::Command::new("sh")
                .arg("-n")
                .arg("-c")
                .arg(inner(&cmd))
                .status()
                .expect("run sh -n");
            assert!(status.success(), "{shell:?}: not valid shell: {cmd}");
        }
    }

    /// Plasma 6.8 draws nothing outside KWin, so plasmashell has to connect
    /// to the nested KWin, not to labwc, and KWin has to fill labwc's output.
    #[test]
    fn plasmashell_runs_in_the_nested_kwin() {
        let cmd = plasma_desktop_command(PlasmaShell::NestedKwin, None, "konsole");
        assert!(cmd.contains("kwin_wayland --socket $kwin --fullscreen true"), "{cmd}");
        assert!(cmd.contains("WAYLAND_DISPLAY=$kwin plasmashell"), "{cmd}");
        // Unique to this session: the inner shell's pid.
        assert!(cmd.contains("kwin=termland-kwin-$$;"), "{cmd}");
    }

    /// kded6 is killed only in this session's process group: a bare
    /// `pkill kded6` would kill the host's own Plasma session's kded6.
    #[test]
    fn only_the_sessions_kded_is_killed() {
        let cmd = plasma_desktop_command(PlasmaShell::NestedKwin, None, "konsole");
        assert!(cmd.contains(r#"[ -n "$LABWC_PID" ] && pkill -KILL -x -g $LABWC_PID kded6"#), "{cmd}");
        assert_eq!(cmd.matches("pkill").count(), 1, "{cmd}");
        let plasma = cmd.find("wait $shell").unwrap();
        let first = plasma + cmd[plasma..].find("stop_kded").expect("after plasmashell");
        let kwin = cmd.find("kill $kwin_pid").unwrap();
        assert!(first < kwin, "kded must go before KWin: {cmd}");
        assert!(cmd[kwin..].contains("stop_kded"), "and again after KWin: {cmd}");
    }

    /// plasmashell is stopped, and waited for, before KWin, so it shuts down
    /// with its compositor and bus still there.
    #[test]
    fn plasmashell_is_stopped_before_kwin() {
        let cmd = plasma_desktop_command(PlasmaShell::NestedKwin, None, "konsole");
        let shell = cmd.find("wait $shell").expect("waits for plasmashell");
        let kwin = cmd.find("kill $kwin_pid").expect("stops kwin");
        assert!(shell < kwin, "plasmashell must be gone before KWin is stopped: {cmd}");
    }

    /// Runs the nested-KWin script with stand-ins for kwin_wayland and
    /// plasmashell, then ends the session as labwc does (SIGTERM to the
    /// shell), and checks what each saw and the order they stopped in.
    #[test]
    fn the_nested_kwin_script_starts_and_stops_the_desktop_in_order() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("termland-kwin-script-{}", std::process::id()));
        let bin = dir.join("bin");
        let runtime = dir.join("run");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let log = dir.join("log");
        let stub = |name: &str, body: &str| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let log_s = log.display();
        // kwin_wayland: make its socket (a real one: the script waits on -S).
        stub("kwin_wayland", &format!(
            "echo \"kwin $*\" >> {log_s}\n\
             python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1])' \"$XDG_RUNTIME_DIR/$2\"\n\
             trap 'echo kwin-stopped >> {log_s}; exit 0' TERM\n\
             while :; do sleep 0.05; done"
        ));
        stub("plasmashell", &format!(
            "echo \"plasmashell on $WAYLAND_DISPLAY\" >> {log_s}\n\
             trap 'sleep 0.3; echo plasmashell-stopped >> {log_s}; exit 0' TERM\n\
             while :; do sleep 0.05; done"
        ));
        // dbus-run-session: run its command, as the real one does.
        stub("dbus-run-session", "exec \"$@\"");
        // pkill only records what it was asked: this runs on a developer's
        // desktop, whose own processes it must never reach. (An earlier
        // version of the script, run by this test, quit the developer's own
        // kded6, and with it the system tray.)
        let pkill_log = dir.join("pkill");
        stub("pkill", &format!("echo \"$*\" >> {}", pkill_log.display()));

        let cmd = plasma_desktop_command(PlasmaShell::NestedKwin, None, "konsole");
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exec {cmd}"))
            .env("PATH", &path)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("LABWC_PID", "4242")
            // Nothing here may reach the desktop the test runs on.
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .spawn()
            .expect("run the desktop script");
        // Wait for plasmashell to start.
        let started = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::fs::read_to_string(&log).is_ok_and(|l| l.contains("plasmashell on"))
        });
        // The script (`exec`'d into, so the child itself) recorded its pid.
        let pid_file = desktop_pid_file(&runtime, 4242);
        let recorded = std::fs::read_to_string(&pid_file).unwrap_or_default();
        // End the session as labwc does.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        let child_pid = child.id();
        let _ = child.wait();
        let pid_file_left = pid_file.exists();
        let pkills = std::fs::read_to_string(&pkill_log).unwrap_or_default();
        let lines: Vec<String> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(started, "plasmashell never started: {lines:?}");
        assert_eq!(recorded.trim(), child_pid.to_string(), "the script must record its own pid");
        assert!(!pid_file_left, "the pid file must go with the desktop");
        assert_eq!(
            pkills.lines().collect::<Vec<_>>(),
            ["-KILL -x -g 4242 kded6"; 2],
            "kded6 is killed in the session's process group only, after plasmashell and after KWin"
        );
        let socket = lines[0].split_whitespace().nth(2).expect("kwin socket").to_string();
        assert!(socket.starts_with("termland-kwin-"), "{lines:?}");
        assert_eq!(
            lines[1..],
            [format!("plasmashell on {socket}"), "plasmashell-stopped".into(), "kwin-stopped".into()],
            "plasmashell must connect to KWin, and stop before it"
        );
    }

    /// A process standing in for the desktop script: in its own process
    /// group (as labwc's session is), recording SIGTERM.
    fn stand_in_session(dir: &Path) -> std::process::Child {
        use std::os::unix::process::CommandExt;
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("trap 'echo term > {}/got; exit 0' TERM; while :; do sleep 0.05; done", dir.display()))
            .process_group(0)
            .spawn()
            .expect("spawn stand-in")
    }

    #[test]
    fn end_desktop_signals_the_recorded_script() {
        let dir = std::env::temp_dir().join(format!("termland-end-desktop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = stand_in_session(&dir);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let group = session.id();
        std::fs::write(desktop_pid_file(&dir, group), format!("{group}\n")).unwrap();
        let signalled = end_desktop(&dir, group);
        let _ = session.wait();
        let got = std::fs::read_to_string(dir.join("got")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(signalled);
        assert_eq!(got.trim(), "term");
    }

    /// A stale pid file must not get an unrelated process signalled.
    #[test]
    fn end_desktop_leaves_a_pid_outside_the_session_alone() {
        let dir = std::env::temp_dir().join(format!("termland-end-desktop-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut other = stand_in_session(&dir);
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Recorded for a session whose group is not `other`'s.
        std::fs::write(desktop_pid_file(&dir, 999_999), other.id().to_string()).unwrap();
        let signalled = end_desktop(&dir, 999_999);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let got = dir.join("got").exists();
        let _ = other.kill();
        let _ = other.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!signalled && !got, "signalled a process outside the session");
        assert!(!end_desktop(&dir, 999_999), "no pid file: nothing to signal");
    }
}

#[cfg(test)]
mod render_node_tests {
    use super::*;

    #[test]
    fn a_render_node_is_needed_for_kwin() {
        let dir = std::env::temp_dir().join(format!("termland-dri-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!has_render_node_in(&dir), "empty: no GPU");
        // A display card alone is not a render node.
        std::fs::write(dir.join("card0"), b"").unwrap();
        assert!(!has_render_node_in(&dir), "card0 only");
        std::fs::write(dir.join("renderD128"), b"").unwrap();
        assert!(has_render_node_in(&dir), "renderD128");
        // One this process cannot open does not count.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("renderD128"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::OpenOptions::new().read(true).open(dir.join("renderD128")).is_err();
        if unreadable {
            assert!(!has_render_node_in(&dir), "renderD128 not openable");
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!has_render_node_in(&dir), "no /dev/dri at all");
    }
}

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
    ///
    /// Passes the path in rather than unsetting the process's own: mutating
    /// PATH here would race the other tests in this binary, which run on
    /// parallel threads.
    #[test]
    fn missing_path_variable_yields_no_match() {
        assert!(!has_program_in("sh", None));
    }

    /// An empty PATH is not the same as an absent one, and must also match
    /// nothing rather than searching the current directory.
    #[test]
    fn empty_path_matches_nothing() {
        assert!(!has_program_in("sh", Some(std::ffi::OsStr::new(""))));
    }

    /// An explicit path still resolves even with no PATH to search.
    #[test]
    fn explicit_paths_ignore_the_search_path() {
        let sh = ["/bin/sh", "/usr/bin/sh"]
            .into_iter()
            .find(|p| is_executable_file(Path::new(p)));
        if let Some(sh) = sh {
            assert!(has_program_in(sh, None));
        }
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
