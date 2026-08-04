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

/// Terminate a session: signal the whole compositor process group (the
/// compositor was `setsid`'d, so it leads its own group — this reaps its apps
/// too), then drop the record + logfile.
pub fn close(id: &str) {
    if let Some(rec) = read(id) {
        let pid = rec.compositor_pid as libc::pid_t;
        unsafe {
            // Negative pid → the whole process group.
            libc::kill(-pid, libc::SIGTERM);
            libc::kill(pid, libc::SIGTERM);
        }
        tracing::info!("Closed session {id} (compositor pid {})", rec.compositor_pid);
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
