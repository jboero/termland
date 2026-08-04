//! Saved connection profiles for the session manager (`--manager`).
//!
//! Persisted as JSON at `~/.config/termland/profiles.json`. This is the only
//! piece of local persistent state termland-client has; everything else
//! (session lifetime, resumability) already lives on the server. We hand-roll
//! the XDG config-dir lookup (see `dirs` below) rather than adding the `dirs`
//! crate, matching the existing precedent in `termland-server/src/tls.rs`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One saved host + connection settings, everything needed to reproduce the
/// exact CLI invocation that would otherwise have to be typed by hand every
/// launch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// Stable identifier, independent of `display_name` so a rename doesn't
    /// change "which saved profile is this".
    pub id: String,
    pub display_name: String,
    /// host:port (TCP/TLS) or user@host (SSH).
    pub server: String,
    pub ssh: bool,
    pub ssh_opts: Vec<String>,
    pub tls: bool,
    pub accept_invalid_certs: bool,
    pub username: Option<String>,
    /// Only store the password when the user explicitly opts in via the
    /// "remember password" checkbox (default OFF) in the edit form — it is
    /// kept in plaintext in profiles.json, same tradeoff the CLI's own
    /// `--password` flag already warns about (visible in /proc/pid/cmdline),
    /// just persisted instead of transient. No keyring/secret-service
    /// integration here; that's out of scope for this pass.
    pub remember_password: bool,
    pub password: Option<String>,
    pub width: u32,
    pub height: u32,
    pub quality: u8,
    /// Free-form "desktop" / "app:<cmd>" string, same as `Args::mode`.
    pub mode: String,
    pub desktop_shell: Option<String>,
    /// One of av1/vp9/vp8/h265/h264 (matches `--codec`'s accepted names), or
    /// None to let the client negotiate.
    pub codec: Option<String>,
}

impl Profile {
    /// A fresh, unsaved profile with the same defaults `Args` uses.
    pub fn new_default() -> Self {
        Profile {
            id: generate_id(),
            display_name: "New profile".into(),
            server: String::new(),
            ssh: false,
            ssh_opts: Vec::new(),
            tls: false,
            accept_invalid_certs: false,
            username: None,
            remember_password: false,
            password: None,
            width: 1280,
            height: 720,
            quality: 75,
            mode: "desktop".into(),
            desktop_shell: None,
            codec: None,
        }
    }
}

/// Generate a stable-enough identifier for a new profile: not a UUID (no
/// dependency for that in this workspace), just wall-clock nanos + a process
/// counter, which is unique enough for a locally-edited JSON file that only
/// this binary writes.
fn generate_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{count:x}")
}

/// On-disk store: just the profile list, wrapped so the JSON has room to grow
/// (e.g. a future schema version field) without breaking old files.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    profiles: Vec<Profile>,
}

fn profiles_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("termland").join("profiles.json"))
}

/// Load saved profiles. Missing file -> empty list. Corrupt file -> empty
/// list plus a warning log, never a panic; this is user-editable state.
pub fn load() -> Vec<Profile> {
    let Some(path) = profiles_path() else {
        tracing::warn!("could not determine config dir; profiles will not persist");
        return Vec::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<Store>(&contents) {
            Ok(store) => store.profiles,
            Err(e) => {
                tracing::warn!("failed to parse {}: {e} (starting with no profiles)", path.display());
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            tracing::warn!("failed to read {}: {e} (starting with no profiles)", path.display());
            Vec::new()
        }
    }
}

/// Save the full profile list, creating `~/.config/termland/` if needed.
pub fn save(profiles: &[Profile]) -> anyhow::Result<()> {
    use anyhow::Context;
    let path = profiles_path().context("could not determine config dir")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("creating config dir")?;
    }
    let store = Store { profiles: profiles.to_vec() };
    let json = serde_json::to_string_pretty(&store).context("serializing profiles")?;
    std::fs::write(&path, json).context("writing profiles.json")?;
    Ok(())
}

/// Same tiny XDG helper as `termland-server/src/tls.rs`'s local `dirs` module
/// — duplicated rather than pulling in the `dirs` crate for a ~10-line lookup.
mod dirs {
    use std::path::PathBuf;
    pub fn config_dir() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_json_round_trip() {
        let mut p = Profile::new_default();
        p.display_name = "My Server".into();
        p.server = "example.com:7100".into();
        p.tls = true;
        p.remember_password = true;
        p.password = Some("hunter2".into());
        p.codec = Some("av1".into());

        let store = Store { profiles: vec![p.clone()] };
        let json = serde_json::to_string_pretty(&store).unwrap();
        let parsed: Store = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.profiles.len(), 1);
        let got = &parsed.profiles[0];
        assert_eq!(got.id, p.id);
        assert_eq!(got.display_name, "My Server");
        assert_eq!(got.server, "example.com:7100");
        assert!(got.tls);
        assert!(got.remember_password);
        assert_eq!(got.password.as_deref(), Some("hunter2"));
        assert_eq!(got.codec.as_deref(), Some("av1"));
    }

    // Both env-var-mutating cases live in one test (rather than two `#[test]`
    // fns) because cargo runs tests in parallel by default and XDG_CONFIG_HOME
    // is process-global — splitting them risks one test's env change leaking
    // into the other.
    #[test]
    fn missing_or_corrupt_file_yields_empty_list_not_panic() {
        let dir = std::env::temp_dir().join(format!("termland-test-{}", generate_id()));
        // SAFETY: test-only; no other test in this crate touches this env var.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        // Missing file.
        assert!(load().is_empty());

        // Corrupt file.
        std::fs::create_dir_all(dir.join("termland")).unwrap();
        std::fs::write(dir.join("termland").join("profiles.json"), b"not json").unwrap();
        assert!(load().is_empty());

        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
