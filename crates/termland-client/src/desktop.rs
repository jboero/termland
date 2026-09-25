//! Freedesktop integration: the application id and icon that the packaged
//! `.desktop` file, the Wayland `app_id`, and the tray icon all have to agree
//! on, plus the per-user "start in tray at login" autostart entry.

// Autostart only has a caller where there is a tray to start into (Linux).
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::io;
use std::path::{Path, PathBuf};

/// Reverse-DNS application id. It is the basename of the packaged `.desktop`
/// file and icon (packaging/io.github.jboero.termland.*), and the Wayland
/// `app_id` of the manager window: compositors match a window to its launcher
/// entry — and so find its icon and name — by `app_id`, so these must stay
/// identical.
pub const APP_ID: &str = "io.github.jboero.termland";

/// Tray icon name: the packaged icon when it is installed, otherwise a stock
/// themed icon. A tray item naming an icon the theme cannot resolve shows up
/// as a blank square, which is what a `cargo run` build would get.
#[cfg(target_os = "linux")]
pub fn tray_icon_name() -> String {
    let installed = icon_search_dirs()
        .iter()
        .any(|d| d.join(format!("icons/hicolor/scalable/apps/{APP_ID}.svg")).is_file());
    if installed { APP_ID.into() } else { "video-display".into() }
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`, with the spec's defaults.
#[cfg(target_os = "linux")]
fn icon_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) => dirs.push(PathBuf::from(d)),
        None => {
            if let Some(h) = std::env::var_os("HOME") {
                dirs.push(PathBuf::from(h).join(".local/share"));
            }
        }
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(data_dirs.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs
}

fn autostart_path_in(config_dir: &Path) -> PathBuf {
    config_dir.join("autostart").join(format!("{APP_ID}.desktop"))
}

/// Whether the per-user autostart entry exists.
pub fn autostart_enabled() -> bool {
    crate::profile::dirs::config_dir().is_some_and(|d| autostart_path_in(&d).is_file())
}

/// Create or remove `~/.config/autostart/<APP_ID>.desktop`, which starts the
/// manager straight into the tray at login.
///
/// Deliberately per-user: the package does not install into
/// `/etc/xdg/autostart`, which would put a tray icon in front of every user on
/// the machine whether or not they use Termland.
pub fn set_autostart(enabled: bool) -> io::Result<()> {
    let dir = crate::profile::dirs::config_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
    let exe = std::env::current_exe()?;
    set_autostart_in(&dir, &exe, enabled)
}

fn set_autostart_in(config_dir: &Path, exe: &Path, enabled: bool) -> io::Result<()> {
    let path = autostart_path_in(config_dir);
    if enabled {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, autostart_entry(exe))
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

fn autostart_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Termland\n\
         Comment=Termland session manager in the system tray\n\
         Exec={} --manager --minimized\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_quote(&exe.to_string_lossy())
    )
}

/// Quote one argument for a desktop entry `Exec=` line, per the Desktop Entry
/// spec: arguments containing reserved characters go in double quotes, and
/// inside those `"`, `` ` ``, `$` and `\` are backslash-escaped. The value is
/// also a string that unescapes `\\` *before* the quoting rule applies, so
/// every one of those escaping backslashes is itself doubled: `$` is written
/// `\\$` and a literal backslash `\\\\` (the spec's own examples). A literal
/// `%` is `%%`, since `%x` is a field code.
fn exec_quote(arg: &str) -> String {
    const RESERVED: &[char] = &[
        ' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(',
        ')', '`',
    ];
    let arg = arg.replace('%', "%%");
    if !arg.contains(RESERVED) {
        return arg;
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '\\' => out.push_str("\\\\\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_quote_leaves_plain_paths_alone() {
        assert_eq!(exec_quote("/usr/bin/termland-client"), "/usr/bin/termland-client");
    }

    #[test]
    fn exec_quote_quotes_and_escapes_reserved_characters() {
        assert_eq!(exec_quote("/opt/my apps/termland"), "\"/opt/my apps/termland\"");
        assert_eq!(exec_quote("/opt/$x/t"), "\"/opt/\\\\$x/t\"");
        assert_eq!(exec_quote("/opt/a\\b"), "\"/opt/a\\\\\\\\b\"");
        assert_eq!(exec_quote("/opt/100%/t"), "/opt/100%%/t");
    }

    #[test]
    fn autostart_entry_starts_the_manager_minimized() {
        let entry = autostart_entry(Path::new("/usr/bin/termland-client"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=/usr/bin/termland-client --manager --minimized\n"));
        assert!(entry.contains(&format!("\nIcon={APP_ID}\n")));
    }

    #[test]
    fn set_autostart_creates_and_removes_the_entry() {
        let dir = std::env::temp_dir().join(format!("termland-autostart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let exe = Path::new("/usr/bin/termland-client");

        set_autostart_in(&dir, exe, true).unwrap();
        assert!(autostart_path_in(&dir).is_file());
        set_autostart_in(&dir, exe, false).unwrap();
        assert!(!autostart_path_in(&dir).exists());
        // Disabling when already disabled is not an error.
        set_autostart_in(&dir, exe, false).unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
