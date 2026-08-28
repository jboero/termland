//! Filesystem-backed registry of persistent sessions.
//!
//! Each running session is a *detached* compositor process (see
//! `termland-compositor`'s detached launch) recorded as a small JSON file under
//! `$XDG_RUNTIME_DIR/termland/sessions/<id>.json`. Because the compositors are
//! the persistent state, there is no daemon: any connection process — including
//! a fresh SSH-subsystem process on reconnect — discovers and validates sessions
//! by reading this directory and checking that the recorded PID is alive and its
//! Wayland socket still exists.

use std::path::PathBuf;

/// Persisted metadata for one session. Serialized as a trivial line-based
/// `key=value` file (no serde dependency, so the offline vendored build stays
/// lean and the record is human-readable for debugging).
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub session_id: String,
    /// PID of the detached compositor (session-group leader after `setsid`).
    pub compositor_pid: u32,
    pub wayland_display: String,
    /// Human-readable mode, e.g. "desktop" or "app: firefox".
    pub mode: String,
    pub width: u32,
    pub height: u32,
    pub created_at_unix: u64,
    pub audio: bool,
    /// The PAM-authenticated username that created this session (server-side
    /// truth, from `--auth`), or `None` when the server isn't running with
    /// `--auth` — in which case there is no identity concept at all and
    /// ownership is not enforced (matches how `run_as: None` preserves
    /// unisolated behavior at the OS level too). Used to restrict
    /// `SessionList`/`SessionAttach`/`SessionClose` to the owning user once
    /// auth is on, so one authenticated user cannot see, attach to, or close
    /// another authenticated user's session just because they can guess or
    /// enumerate its id.
    pub owner: Option<String>,
    /// The compositor's actual `XDG_RUNTIME_DIR` at creation time (see
    /// `termland_compositor::backend::DetachedBackend`'s doc comment). Under
    /// session isolation this is the target user's `/run/user/<uid>`, which
    /// differs from the server process's own — every later connection to
    /// this compositor's Wayland socket (screen capture, output resize,
    /// cursor capture on resume, and the wl-copy/wl-paste-based clipboard/
    /// file-transfer subprocesses) needs this value. Stored rather than
    /// recomputed on attach/resume, so a fresh connection process doesn't
    /// need to re-derive it (which would require re-resolving `owner` via
    /// `getpwnam_r` again) and can't drift from what was actually used when
    /// the compositor was created.
    pub runtime_dir: String,
}

impl SessionRecord {
    fn to_kv(&self) -> String {
        format!(
            "session_id={}\ncompositor_pid={}\nwayland_display={}\nmode={}\nwidth={}\nheight={}\ncreated_at_unix={}\naudio={}\nowner={}\nruntime_dir={}\n",
            self.session_id,
            self.compositor_pid,
            self.wayland_display,
            self.mode,
            self.width,
            self.height,
            self.created_at_unix,
            self.audio,
            self.owner.as_deref().unwrap_or(""),
            self.runtime_dir,
        )
    }

    fn from_kv(text: &str) -> Option<SessionRecord> {
        let mut session_id = None;
        let mut compositor_pid = None;
        let mut wayland_display = None;
        let mut mode = String::new();
        let mut width = None;
        let mut height = None;
        let mut created_at_unix = None;
        let mut audio = false;
        // Absent entirely on records written before this field existed -
        // treated the same as an explicitly-empty value (see the `owner`
        // field's doc comment: an ownerless record is visible/attachable by
        // anyone once auth is on, which is correct here since such a record
        // predates ownership tracking and can't actually belong to a
        // different authenticated user under the new code).
        let mut owner: Option<String> = None;
        // Absent on records written before this field existed too. Such a
        // record was necessarily created before session isolation carried a
        // per-session runtime dir at all, so the server's own default
        // (matching the pre-fix behavior every caller implicitly assumed) is
        // the correct fallback, not an arbitrary guess.
        let mut runtime_dir: Option<String> = None;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "session_id" => session_id = Some(v.to_string()),
                "compositor_pid" => compositor_pid = v.parse().ok(),
                "wayland_display" => wayland_display = Some(v.to_string()),
                "mode" => mode = v.to_string(),
                "width" => width = v.parse().ok(),
                "height" => height = v.parse().ok(),
                "created_at_unix" => created_at_unix = v.parse().ok(),
                "audio" => audio = v == "true",
                "owner" => owner = if v.is_empty() { None } else { Some(v.to_string()) },
                "runtime_dir" => runtime_dir = if v.is_empty() { None } else { Some(v.to_string()) },
                _ => {}
            }
        }
        Some(SessionRecord {
            session_id: session_id?,
            compositor_pid: compositor_pid?,
            wayland_display: wayland_display?,
            mode,
            width: width?,
            height: height?,
            created_at_unix: created_at_unix?,
            audio,
            owner,
            runtime_dir: runtime_dir.unwrap_or_else(|| self::runtime_dir().to_string_lossy().into_owned()),
        })
    }
}

fn runtime_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}

/// `$XDG_RUNTIME_DIR/termland/sessions`.
pub fn base_dir() -> PathBuf {
    runtime_dir().join("termland").join("sessions")
}

/// Ensure the registry + logs directories exist.
pub fn ensure_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(base_dir().join("logs"))
}

/// Logfile the detached compositor writes stdout/stderr to.
pub fn log_path(id: &str) -> PathBuf {
    base_dir().join("logs").join(format!("{id}.log"))
}

/// `$XDG_RUNTIME_DIR/termland/clipboard-files/<session_id>` - scratch
/// directory for files received via clipboard file-paste
/// (`Message::FileTransferSend`) for one session. Mirrors `base_dir()`'s
/// convention of using this server process's `XDG_RUNTIME_DIR` as
/// scratch/state storage (same as session records and compositor logs).
///
/// Note this is the *server process's* runtime dir, not necessarily the
/// session's isolated user's: the clipboard watch/receive threads run at the
/// server process's own privilege level (see `transport.rs`'s
/// `clipboard_watch_thread`/incoming `FileTransferSend` handling), not
/// dropped into the session's target user the way the compositor itself is
/// under `--auth` session isolation.
///
/// Cleanup: `clipboard_files_cleanup` below removes this directory when a
/// session ends (see `transport.rs::run_session`'s `SessionOutcome`
/// handling). Even if that's skipped (process killed, etc.), it lives under
/// `XDG_RUNTIME_DIR`, which is conventionally tmpfs and cleared on
/// reboot/logout - a fallback, not a strict guarantee.
pub fn clipboard_files_dir(session_id: &str) -> PathBuf {
    runtime_dir().join("termland").join("clipboard-files").join(session_id)
}

/// Remove a session's clipboard-files scratch directory (see
/// `clipboard_files_dir`). Best-effort: errors (already gone, etc.) are
/// ignored, matching `remove()`'s style for session records above.
pub fn clipboard_files_cleanup(session_id: &str) {
    let _ = std::fs::remove_dir_all(clipboard_files_dir(session_id));
}

fn record_path(id: &str) -> PathBuf {
    base_dir().join(format!("{id}.session"))
}

/// Generate a fresh, unguessable session id.
pub fn new_session_id() -> String {
    let mut buf = [0u8; 6];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    }
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    format!("s{hex}")
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Atomically persist a record.
pub fn write(rec: &SessionRecord) -> std::io::Result<()> {
    ensure_dir()?;
    let tmp = record_path(&rec.session_id).with_extension("tmp");
    std::fs::write(&tmp, rec.to_kv())?;
    std::fs::rename(&tmp, record_path(&rec.session_id))
}

pub fn read(id: &str) -> Option<SessionRecord> {
    let text = std::fs::read_to_string(record_path(id)).ok()?;
    SessionRecord::from_kv(&text)
}

/// Remove the record + logfile (does not touch the process).
pub fn remove(id: &str) {
    let _ = std::fs::remove_file(record_path(id));
    let _ = std::fs::remove_file(log_path(id));
}

/// Is `pid` a live process? `kill(pid, 0)` succeeds for a live, signalable
/// process; EPERM also means it exists (owned by someone else).
pub fn pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn socket_exists(wayland_display: &str) -> bool {
    runtime_dir().join(wayland_display).exists()
}

/// A record is live if its compositor PID is alive and its Wayland socket
/// still exists.
pub fn is_live(rec: &SessionRecord) -> bool {
    pid_alive(rec.compositor_pid) && socket_exists(&rec.wayland_display)
}

/// All live sessions, oldest first. Prunes records whose process/socket are gone.
pub fn list_alive() -> Vec<SessionRecord> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(base_dir()) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("session") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let Some(rec) = SessionRecord::from_kv(&text) else { continue };
            if is_live(&rec) {
                out.push(rec);
            } else {
                remove(&rec.session_id);
            }
        }
    }
    out.sort_by_key(|r| r.created_at_unix);
    out
}

/// How long the process group gets to honour `SIGTERM` before `SIGKILL`.
///
/// Generous on purpose: a desktop shell saving state on the way out is doing
/// something useful, and this only delays a teardown the user already asked
/// for. It is not generous enough to matter if nothing exits — the escalation
/// still happens.
const TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Poll interval while waiting for the group to go away.
const REAP_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// What happened when a session's process group was terminated.
#[derive(Debug, PartialEq, Eq)]
pub enum Reaped {
    /// Nothing was running under that group to begin with.
    AlreadyGone,
    /// Everything exited on `SIGTERM`.
    Terminated,
    /// `SIGTERM` was ignored; `SIGKILL` finished the job.
    Killed,
    /// Processes survived even `SIGKILL`. PIDs are the survivors.
    Survivors(Vec<i32>),
}

/// Live (non-zombie) members of process group `pgid`, read from `/proc`.
///
/// `kill(-pid, 0)` is not usable as the liveness check here: it succeeds for a
/// group whose only remaining member is an unreaped zombie, which would make
/// teardown escalate to `SIGKILL` and then report survivors forever. Zombies
/// hold no resources and own no Wayland connection, so they are not what this
/// is looking for.
pub fn group_members(pgid: i32) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<i32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if let Some((state, pgrp)) = parse_stat_state_and_pgrp(&stat) {
            if pgrp == pgid && state != 'Z' {
                found.push(pid);
            }
        }
    }
    found.sort_unstable();
    found
}

/// Pull the state and process-group fields out of a `/proc/<pid>/stat` line.
///
/// The comm field (2nd) is wrapped in parentheses and may itself contain
/// spaces and parentheses, so the fields after it are found by splitting at the
/// *last* `)` rather than by counting spaces from the start.
fn parse_stat_state_and_pgrp(stat: &str) -> Option<(char, i32)> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    let mut fields = after_comm.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse().ok()?;
    Some((state, pgrp))
}

/// Terminate a process group and *confirm* it is gone.
///
/// The compositor is `setsid`'d, so it leads its own group and its apps inherit
/// that group id — including after the compositor itself dies, since orphans
/// keep their pgid when they are reparented. Signalling the group is therefore
/// the way to reach a desktop shell whose compositor has already exited.
///
/// The confirmation is the point. The previous implementation sent `SIGTERM`,
/// logged success and deleted the session record without checking anything, and
/// on this project's own workstation the group did *not* die: orphaned
/// `plasmashell` processes were left blocked in `drm_syncobj_array_wait_timeout`
/// waiting on GPU fences from a compositor that no longer existed. They ignored
/// `SIGTERM` entirely and needed `SIGKILL`. Because the record was already
/// gone, `--list-sessions` showed nothing and nothing would ever reap them.
pub fn terminate_group(pgid: i32, grace: std::time::Duration) -> Reaped {
    if group_members(pgid).is_empty() {
        return Reaped::AlreadyGone;
    }

    // Stage 1: the leader alone. For a desktop session the leader is the
    // compositor, and its startup command shuts the session down in order —
    // the Plasma wrapper kills plasmashell and waits for it before
    // `dbus-run-session` tears the private bus down.
    //
    // Signalling the whole group up front defeats that: `dbus-daemon` is in
    // the group too, so it takes SIGTERM at the same moment as plasmashell,
    // the bus vanishes mid-shutdown, and plasmashell aborts inside libdbus
    // (`_dbus_warn_check_failed` -> `abort`) leaving a coredump on every
    // teardown. Giving the leader a head start turns that into a clean exit.
    unsafe {
        libc::kill(pgid, libc::SIGTERM);
    }
    if wait_for_group_exit(pgid, grace) {
        return Reaped::Terminated;
    }

    // Stage 2: anything the leader did not take with it. This is where an
    // orphaned group with no leader left is reached, since orphans keep the
    // process-group id when they are reparented.
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
        libc::kill(pgid, libc::SIGTERM);
    }
    if wait_for_group_exit(pgid, grace) {
        return Reaped::Terminated;
    }

    tracing::warn!(
        "process group {pgid} ignored SIGTERM after {:?}; escalating to SIGKILL",
        grace * 2
    );
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
        libc::kill(pgid, libc::SIGKILL);
    }

    // SIGKILL cannot be caught, so this is short — it only covers the kernel
    // tearing the processes down, not any handler.
    if wait_for_group_exit(pgid, std::time::Duration::from_secs(2)) {
        return Reaped::Killed;
    }

    // Uninterruptible sleep (D state) survives SIGKILL until the syscall
    // returns; this is exactly where the stranded compositors were found.
    Reaped::Survivors(group_members(pgid))
}

/// Poll until the group has no live members, or the deadline passes.
fn wait_for_group_exit(pgid: i32, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if group_members(pgid).is_empty() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(REAP_POLL);
    }
}

/// Terminate a session and drop its record + logfile.
///
/// The record is removed whatever happens, because leaving it behind would
/// advertise a session that can no longer be attached to. Survivors are logged
/// at error level with their PIDs so there is something to act on, rather than
/// disappearing silently as they used to.
pub fn close(id: &str) {
    if let Some(rec) = read(id) {
        let pid = rec.compositor_pid as libc::pid_t;
        match terminate_group(pid, TERM_GRACE) {
            Reaped::AlreadyGone => {
                tracing::info!("Closed session {id} (compositor pid {pid} was already gone)");
            }
            Reaped::Terminated => {
                tracing::info!("Closed session {id} (compositor pid {pid})");
            }
            Reaped::Killed => {
                tracing::info!("Closed session {id} (compositor pid {pid}, needed SIGKILL)");
            }
            Reaped::Survivors(pids) => {
                tracing::error!(
                    "Session {id}: process group {pid} survived SIGKILL; still running: {pids:?}. \
                     These are usually blocked in an uninterruptible GPU wait and will need \
                     manual attention."
                );
            }
        }
    }
    remove(id);
}

/// Path of the resume marker used only for logging/diagnostics.
pub fn describe_mode(mode: &termland_protocol::SessionMode) -> String {
    match mode {
        termland_protocol::SessionMode::Desktop => "desktop".to_string(),
        termland_protocol::SessionMode::App { command, .. } => format!("app: {command}"),
    }
}

#[cfg(test)]
mod reap_tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    /// Spawns a shell in its own session (so it leads a process group, exactly
    /// as the detached compositor does) and guarantees the group is gone when
    /// the test ends, however it ends.
    struct GroupGuard(Child);

    impl GroupGuard {
        fn spawn(script: &str) -> Self {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(script).stdout(Stdio::null()).stderr(Stdio::null());
            unsafe {
                cmd.pre_exec(|| {
                    // setsid: new session, and this process leads a new group.
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            GroupGuard(cmd.spawn().expect("spawn test group"))
        }

        fn pgid(&self) -> i32 {
            self.0.id() as i32
        }

        /// Give the shell time to start its children before signalling.
        fn settle(&self) {
            std::thread::sleep(Duration::from_millis(400));
        }
    }

    impl Drop for GroupGuard {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn stat_parsing_survives_a_comm_with_spaces_and_parens() {
        // The comm field is arbitrary and unescaped; splitting on whitespace
        // from the left puts every later field in the wrong place.
        let line = "1234 (weird (name) here) S 1 4321 4321 0 -1 4194304";
        assert_eq!(parse_stat_state_and_pgrp(line), Some(('S', 4321)));
    }

    #[test]
    fn stat_parsing_reads_state_and_pgrp_of_a_normal_line() {
        let line = "42 (plasmashell) D 1 99 99 0 -1 4194560 1234";
        assert_eq!(parse_stat_state_and_pgrp(line), Some(('D', 99)));
    }

    #[test]
    fn group_members_finds_this_test_process() {
        let pgid = unsafe { libc::getpgid(0) };
        let members = group_members(pgid);
        let me = std::process::id() as i32;
        assert!(members.contains(&me), "own pid {me} missing from group {pgid}: {members:?}");
    }

    #[test]
    fn an_empty_group_is_already_gone() {
        // A pgid nothing can be using: kernel pids do not reach this.
        assert_eq!(terminate_group(0x7fff_fffe, Duration::from_millis(200)), Reaped::AlreadyGone);
    }

    #[test]
    fn a_cooperative_group_exits_on_sigterm() {
        let g = GroupGuard::spawn("sleep 60 & sleep 60");
        g.settle();
        assert_eq!(terminate_group(g.pgid(), Duration::from_secs(3)), Reaped::Terminated);
        assert!(group_members(g.pgid()).is_empty());
    }

    /// The behaviour that was missing: the old code sent SIGTERM, logged
    /// success and deleted the record. A group that ignores SIGTERM — which is
    /// what the stranded plasmashell processes did — simply stayed alive.
    #[test]
    fn a_group_ignoring_sigterm_is_escalated_to_sigkill() {
        let g = GroupGuard::spawn("trap '' TERM; sleep 60 & trap '' TERM; wait");
        g.settle();
        let result = terminate_group(g.pgid(), Duration::from_millis(400));
        assert_eq!(result, Reaped::Killed, "SIGTERM-ignoring group was not escalated");
        assert!(group_members(g.pgid()).is_empty(), "survivors after SIGKILL");
    }

    /// The actual failure on the workstation: the session leader was already
    /// gone and its children had been reparented to init. Orphans keep their
    /// process-group id, so the group is still the handle that reaches them —
    /// and nothing was using it.
    #[test]
    fn orphans_of_a_dead_leader_are_still_reaped() {
        let mut g = GroupGuard::spawn("sleep 60 & sleep 60 & exit 0");
        g.settle();
        let pgid = g.pgid();
        // The leader has exited; reap it so it is not a zombie holding the id.
        let _ = g.0.wait();

        let before = group_members(pgid);
        assert!(
            !before.is_empty(),
            "test is not exercising anything: no orphans survived the leader"
        );

        assert_eq!(terminate_group(pgid, Duration::from_secs(3)), Reaped::Terminated);
        assert!(group_members(pgid).is_empty(), "orphans survived teardown");
    }

    /// A zombie leader must not read as a live group: it would make teardown
    /// escalate to SIGKILL and then report survivors that are not really there.
    #[test]
    fn a_zombie_does_not_count_as_a_live_group_member() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("exit 0").stdout(Stdio::null()).stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn");
        let pid = child.id() as i32;
        // Deliberately not waited on, so it stays a zombie.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            group_members(pid).is_empty(),
            "zombie counted as a live group member"
        );
        let mut child = child;
        let _ = child.wait();
    }
}
