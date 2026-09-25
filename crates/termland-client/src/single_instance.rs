//! Single-instance guard for `--manager`.
//!
//! The manager lives in the system tray, so launching it again — the app-menu
//! entry clicked twice, the per-host `--tray`'s "Manage profiles…", autostart
//! racing a manual start — must not open a second window with a second tray
//! icon. A second launch instead asks the running instance to show its window,
//! and exits.
//!
//! Two files in a private per-user directory:
//!  - `manager.lock`, held with `flock` for the primary's whole lifetime. The
//!    kernel drops the lock when the process dies, however it dies, so a crash
//!    never leaves a stale "already running" state behind the way a PID file
//!    would.
//!  - `manager.sock`, a Unix socket the primary listens on. A secondary writes
//!    `show\n` to it. The socket is only ever (re)bound while holding the
//!    lock, so removing a leftover socket file before binding can't pull it
//!    out from under a live primary.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LOCK_FILE: &str = "manager.lock";
const SOCKET_FILE: &str = "manager.sock";
const SHOW: &str = "show";

pub enum Instance {
    /// No other manager is running; this process now holds the lock.
    Primary(Primary),
    /// Another manager holds the lock.
    Secondary,
}

pub struct Primary {
    // Never read: holding the open file *is* holding the flock.
    _lock: File,
    listener: UnixListener,
}

/// `$XDG_RUNTIME_DIR/termland`, or a per-uid directory under the temp dir when
/// there is no runtime dir (e.g. a bare X11 session without logind).
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("termland"),
        // SAFETY: getuid cannot fail.
        None => std::env::temp_dir().join(format!("termland-{}", unsafe { libc::getuid() })),
    }
}

/// Create `dir` as 0700 if needed, and refuse to use it unless it is ours and
/// no one else can write to it. The temp-dir fallback is world-writable
/// territory: another user could have pre-created the directory to swap in
/// their own socket.
///
/// Group/other *read* access is fine and does occur: the server's session
/// registry creates this same directory as 0750. Reading the directory
/// exposes nothing — the lock is 0600, and connecting to the socket needs
/// write permission on it, which the umask withholds from everyone else.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let meta = std::fs::metadata(dir)?;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    if meta.uid() != uid || meta.permissions().mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} must be owned by uid {uid} and writable only by it", dir.display()),
        ));
    }
    Ok(())
}

/// Try to become the one running manager.
pub fn acquire(dir: &Path) -> io::Result<Instance> {
    ensure_private_dir(dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join(LOCK_FILE))?;
    // SAFETY: plain syscall on an fd we own for the duration of the call.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        return if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(Instance::Secondary)
        } else {
            Err(err)
        };
    }

    let socket = dir.join(SOCKET_FILE);
    match std::fs::remove_file(&socket) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(&socket)?;
    Ok(Instance::Primary(Primary { _lock: lock, listener }))
}

impl Primary {
    /// Listen for show requests on a background thread for the rest of the
    /// process's life, calling `on_show` for each. The lock moves into the
    /// thread with the listener, so it is held until the process exits.
    pub fn serve(self, on_show: impl Fn() + Send + 'static) {
        std::thread::spawn(move || {
            let _lock = self._lock;
            for stream in self.listener.incoming() {
                let Ok(stream) = stream else { continue };
                // A client that connects and says nothing must not wedge the
                // only thread that answers show requests.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_ok() && line.trim() == SHOW {
                    on_show();
                }
            }
        });
    }
}

/// Ask the running manager to show its window.
///
/// Retries briefly: the primary takes the lock before it binds the socket, so
/// a launch that lands in that gap sees the lock held but nothing listening.
pub fn request_show(dir: &Path) -> io::Result<()> {
    let socket = dir.join(SOCKET_FILE);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match UnixStream::connect(&socket) {
            Ok(mut stream) => return stream.write_all(format!("{SHOW}\n").as_bytes()),
            Err(e)
                if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("termland-single-instance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn second_acquire_is_secondary_until_primary_goes_away() {
        let dir = scratch_dir("lock");
        let primary = acquire(&dir).unwrap();
        assert!(matches!(primary, Instance::Primary(_)));
        assert!(matches!(acquire(&dir).unwrap(), Instance::Secondary));

        // Dropping the primary releases the flock but leaves the socket file
        // behind, as a crash would. The next launch must take over anyway.
        drop(primary);
        assert!(dir.join(SOCKET_FILE).exists());
        assert!(matches!(acquire(&dir).unwrap(), Instance::Primary(_)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_show_reaches_the_primary() {
        let dir = scratch_dir("show");
        let Instance::Primary(primary) = acquire(&dir).unwrap() else {
            panic!("fresh directory should give a primary");
        };
        let (tx, rx) = mpsc::channel();
        primary.serve(move || {
            let _ = tx.send(());
        });

        request_show(&dir).unwrap();
        rx.recv_timeout(Duration::from_secs(5)).expect("primary never saw the show request");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_the_0750_directory_the_server_creates() {
        let dir = scratch_dir("0750");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(matches!(acquire(&dir).unwrap(), Instance::Primary(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_a_directory_other_users_can_write() {
        let dir = scratch_dir("perms");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = acquire(&dir).err().expect("a 0777 directory must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
