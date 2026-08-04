//! Pure helpers for clipboard file-paste transfer: `text/uri-list` parsing,
//! percent-encoding/decoding, and destination-filename sanitization.
//!
//! Shared by the server (`crates/termland-server/src/transport.rs`) and the
//! client (`crates/termland-client/src/{connection,display}.rs`), which both
//! need to:
//!   1. turn a `wl-paste --type text/uri-list` listing into local paths to
//!      read (when *sending* a clipboard file-paste), and
//!   2. turn a received [`crate::FileEntry::name`] into a safe filename to
//!      write, and the files written back into a `text/uri-list` for
//!      `wl-copy --type text/uri-list` (when *receiving* one).
//!
//! Kept side-effect-free (no filesystem or process I/O) so all of it can be
//! unit tested directly, in particular the filename sanitization - the one
//! function here with real security weight, since it's the only thing
//! standing between a maliciously-named `FileEntry` arriving over the wire
//! and a path-traversal write on the receiving side.

/// Parse a `text/uri-list` MIME payload (RFC 2483: one URI per line, blank
/// lines and `#`-prefixed comment lines ignored) into local filesystem
/// paths. Only `file://` entries are kept - a `text/uri-list` can in
/// principle carry other URI schemes (e.g. some file managers also emit
/// `x-special/nautilus-clipboard` framing lines), but only local files can be
/// read and forwarded by this feature, so anything else is silently skipped.
pub fn parse_uri_list(text: &str) -> Vec<std::path::PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        .map(percent_decode)
        .map(std::path::PathBuf::from)
        .collect()
}

/// Percent-decode a URI path component (`%20` -> ' ', `%C3%A9` -> "é", ...).
/// Operates on raw bytes (not `str` slicing) so a multi-byte UTF-8 sequence
/// split across an escape never causes a char-boundary panic. An incomplete
/// or malformed `%` escape is passed through literally rather than erroring -
/// a garbled clipboard entry should just fail to resolve as a real file path
/// later (`std::fs::read` errors, logged and skipped), not panic here.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-encode a filesystem path for embedding in a `file://` URI.
/// RFC 3986 unreserved characters (plus `/`, kept literal so the path
/// separators stay readable) pass through; everything else - spaces,
/// non-ASCII, punctuation - is escaped. Inverse of [`percent_decode`].
pub fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Build the `text/uri-list` body (`file://<percent-encoded path>` per line,
/// CRLF-terminated per RFC 2483) that `wl-copy --type text/uri-list` expects
/// on stdin, from a list of absolute local paths.
pub fn build_uri_list(paths: &[std::path::PathBuf]) -> String {
    let mut out = String::new();
    for p in paths {
        out.push_str("file://");
        out.push_str(&percent_encode_path(&p.to_string_lossy()));
        out.push_str("\r\n");
    }
    out
}

/// Sanitize a [`crate::FileEntry::name`] into a filename safe to create
/// inside a fixed scratch directory. This is a real path-traversal surface:
/// the name arrives over the wire from the other side (client or server) and
/// is used directly in a filesystem write, so a name like `../../etc/passwd`
/// or an absolute path (`/etc/passwd`) must never be allowed to escape the
/// scratch directory it's joined onto.
///
/// Rejects (returns `None` for) anything that isn't a plain, single-component
/// basename: empty names, `.`/`..`, and any name containing a `/`, `\`, or
/// NUL byte. Unlike `termland_compositor::validate_shell_command` (which
/// allow-lists a punctuation set for strings destined for a shell command
/// line), this is a filename destined for `Path::join` into a directory we
/// already control, so the safe move is "must be exactly a basename" rather
/// than trying to enumerate every dangerous character - reject-not-strip,
/// so a bad name is dropped (with a warning, by the caller) instead of
/// silently rewritten into something the sender didn't intend.
pub fn sanitize_filename(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return None;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_uri_list_basic() {
        let text = "file:///tmp/a.txt\nfile:///tmp/b.txt\n";
        let paths = parse_uri_list(text);
        assert_eq!(paths, vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]);
    }

    #[test]
    fn parse_uri_list_skips_blank_and_comment_lines() {
        let text = "# a comment\nfile:///tmp/a.txt\n\n   \nfile:///tmp/b.txt\n";
        let paths = parse_uri_list(text);
        assert_eq!(paths, vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]);
    }

    #[test]
    fn parse_uri_list_skips_non_file_schemes() {
        let text = "http://example.com/a.txt\nfile:///tmp/b.txt\n";
        let paths = parse_uri_list(text);
        assert_eq!(paths, vec![PathBuf::from("/tmp/b.txt")]);
    }

    #[test]
    fn parse_uri_list_percent_decodes_spaces_and_unicode() {
        // Real output captured from `wl-copy --type text/uri-list` +
        // `wl-paste --type text/uri-list` on a live Wayland session for a
        // file named "my file with spaces & üñïçødé.txt".
        let text = "file:///tmp/my%20file%20with%20spaces%20%26%20%C3%BC%C3%B1%C3%AF%C3%A7%C3%B8d%C3%A9.txt\n";
        let paths = parse_uri_list(text);
        assert_eq!(paths, vec![PathBuf::from("/tmp/my file with spaces & üñïçødé.txt")]);
    }

    #[test]
    fn percent_decode_passes_through_malformed_escape() {
        // Not a valid hex escape - kept literal rather than erroring.
        assert_eq!(percent_decode("100%done"), "100%done");
        // Truncated escape at end of string.
        assert_eq!(percent_decode("abc%2"), "abc%2");
    }

    #[test]
    fn percent_encode_decode_roundtrip() {
        let original = "/tmp/my file with spaces & üñïçødé.txt";
        let encoded = percent_encode_path(original);
        let decoded = percent_decode(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn build_uri_list_produces_parseable_output() {
        let paths = vec![PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/c.txt")];
        let list = build_uri_list(&paths);
        let parsed = parse_uri_list(&list);
        assert_eq!(parsed, paths);
    }

    #[test]
    fn sanitize_filename_accepts_plain_basename() {
        assert_eq!(sanitize_filename("report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(sanitize_filename("üñïçødé.txt").as_deref(), Some("üñïçødé.txt"));
        assert_eq!(sanitize_filename("  spaced.txt  ").as_deref(), Some("spaced.txt"));
    }

    #[test]
    fn sanitize_filename_rejects_path_traversal() {
        // The exact case called out as a must-reject in the task: a relative
        // traversal that would otherwise escape the scratch directory.
        assert_eq!(sanitize_filename("../../etc/passwd"), None);
        assert_eq!(sanitize_filename("..\\..\\windows\\system32"), None);
    }

    #[test]
    fn sanitize_filename_rejects_absolute_path() {
        assert_eq!(sanitize_filename("/etc/passwd"), None);
    }

    #[test]
    fn sanitize_filename_rejects_dot_and_dotdot_and_empty() {
        assert_eq!(sanitize_filename("."), None);
        assert_eq!(sanitize_filename(".."), None);
        assert_eq!(sanitize_filename(""), None);
        assert_eq!(sanitize_filename("   "), None);
    }

    #[test]
    fn sanitize_filename_rejects_embedded_separator() {
        assert_eq!(sanitize_filename("sub/dir/file.txt"), None);
        assert_eq!(sanitize_filename("weird\\name.txt"), None);
        assert_eq!(sanitize_filename("nul\0byte.txt"), None);
    }
}

#[cfg(test)]
mod live_wayland_interop_check {
    // TEMPORARY, manual-only verification against this machine's real
    // Wayland session via wl-copy/wl-paste. Not part of the normal suite
    // (no Wayland session in CI) - run explicitly with:
    //   cargo test -p termland-protocol live_interop -- --ignored --nocapture
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    #[ignore]
    fn live_interop_build_uri_list_roundtrips_through_wl_clipboard() {
        let dir = std::env::temp_dir().join("termland-live-interop-check");
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("my file with spaces & üñïçødé.txt");
        std::fs::write(&file_path, b"hello world").unwrap();

        let uri_list = build_uri_list(&[file_path.clone()]);
        eprintln!("built uri-list:\n{uri_list:?}");

        // wl-copy daemonizes (stays running in the background to serve the
        // clipboard selection - normal wl-clipboard behavior). Must not
        // inherit our stdout/stderr, or the detached process holds those
        // fds open forever and hangs anything downstream waiting for EOF
        // (e.g. a shell pipeline reading `cargo test`'s output).
        let mut child = Command::new("wl-copy")
            .args(["--type", "text/uri-list"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn wl-copy");
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(uri_list.as_bytes()).unwrap();
        }
        assert!(child.wait().unwrap().success());

        let list_types = Command::new("wl-paste").arg("--list-types").output().unwrap();
        let types_text = String::from_utf8_lossy(&list_types.stdout);
        eprintln!("wl-paste --list-types:\n{types_text}");
        assert!(types_text.lines().any(|l| l.trim() == "text/uri-list"));

        let read_back = Command::new("wl-paste")
            .args(["--type", "text/uri-list", "--no-newline"])
            .output()
            .unwrap();
        assert!(read_back.status.success());
        let read_text = String::from_utf8_lossy(&read_back.stdout);
        eprintln!("wl-paste --type text/uri-list --no-newline:\n{read_text:?}");

        // Byte-identical roundtrip: what we wrote to wl-copy's stdin must be
        // exactly what wl-paste --no-newline reads back (this is the
        // assumption clipboard_watch_thread's echo-suppression hash depends
        // on server-side).
        assert_eq!(read_text, uri_list);

        let parsed = parse_uri_list(&read_text);
        assert_eq!(parsed, vec![file_path.clone()]);

        // And the file itself is actually readable at the parsed path -
        // the whole point: content isn't on the clipboard, just the path.
        let data = std::fs::read(&parsed[0]).unwrap();
        assert_eq!(data, b"hello world");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
