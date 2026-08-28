//! Teardown must leave nothing running.
//!
//! `#[ignore]`d: spawns a real server and a real compositor.
//!
//! ```text
//! cargo test -p termland-server --test session_teardown -- --ignored --nocapture
//! ```
//!
//! The unit tests in `registry.rs` prove the reaping primitive in isolation
//! against synthetic process groups. These prove the primitive is actually
//! reached by the two teardown paths a user hits, with a real compositor and a
//! real backgrounded app in the group — which is the shape that stranded
//! `plasmashell` on a workstation for hours, because the record was deleted
//! while the processes lived on.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use termland_protocol::{
    AudioCodec, Hello, Message, SessionCreate, SessionMode, TermlandCodec, VideoCodec,
    PROTOCOL_VERSION,
};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Last-resort cleanup: if an assertion fails mid-test, the session must not
/// outlive the run — that is the very bug under test.
struct SessionGuard {
    bin: &'static str,
    id: Option<String>,
    pgid: Option<i32>,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = Command::new(self.bin).args(["--close-session", &id]).output();
        }
        if let Some(pgid) = self.pgid.take() {
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }
}

fn first_on_path(names: &[&str]) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for name in names {
        for dir in std::env::split_paths(&path) {
            if std::fs::metadata(dir.join(name)).map(|m| m.is_file()).unwrap_or(false) {
                return Some((*name).to_string());
            }
        }
    }
    None
}

/// Live (non-zombie) members of a process group, read straight from `/proc`.
///
/// Deliberately a second implementation rather than a call into the server's
/// own `registry::group_members`: a test that reuses the code under test
/// cannot show that the code is right about the system.
fn group_members(pgid: i32) -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(pid) = name.parse::<i32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(idx) = stat.rfind(')') else { continue };
        let mut fields = stat[idx + 1..].split_whitespace();
        let (Some(state), Some(_ppid), Some(pgrp)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if pgrp.parse::<i32>() == Ok(pgid) && state != "Z" {
            out.push(pid);
        }
    }
    out.sort_unstable();
    out
}

fn describe(pids: &[i32]) -> String {
    pids.iter()
        .map(|p| {
            let cmd = std::fs::read_to_string(format!("/proc/{p}/cmdline"))
                .map(|c| c.replace('\0', " ").trim().to_string())
                .unwrap_or_default();
            format!("{p} ({cmd})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn sessions(bin: &str) -> Vec<(String, i32)> {
    let Ok(out) = Command::new(bin).arg("--list-sessions").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            // SESSION ID  MODE  SIZE  AGE  PID  AUDIO
            if f.len() < 6 {
                return None;
            }
            Some((f[0].to_string(), f[4].parse().ok()?))
        })
        .collect()
}

/// Start a server and a desktop session whose shell backgrounds a long-lived
/// process, standing in for `plasmashell`. Returns the guards plus the
/// session id and compositor pid (which is the process-group id, since the
/// compositor is `setsid`'d).
async fn start_session(port: u16) -> (ChildGuard, SessionGuard, String, i32) {
    let bin = env!("CARGO_BIN_EXE_termland-server");
    let terminal = first_on_path(&["foot", "konsole", "alacritty", "xterm"])
        .expect("no terminal emulator available");

    // Server output goes to a file so a failure can print why, rather than
    // being swallowed by a pipe nothing reads.
    let log_path = std::env::temp_dir().join(format!("termland-teardown-{port}.log"));
    let log = std::fs::File::create(&log_path).expect("create server log");
    let script = std::env::temp_dir()
        .join(format!("termland-teardown-shell-{port}.sh"))
        .to_string_lossy()
        .into_owned();
    std::fs::write(
        &script,
        format!("#!/bin/sh\nsh -c 'trap \"\" TERM; sleep 600' &\nexec {terminal}\n"),
    )
    .expect("write session shell script");
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod session shell script");

    let server = ChildGuard(
        Command::new(bin)
            .args(["--bind", "127.0.0.1", "--port", &port.to_string()])
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn termland-server"),
    );
    let mut guard = SessionGuard { bin, id: None, pgid: None };
    tokio::time::sleep(Duration::from_millis(800)).await;

    let stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    let mut framed = Framed::new(stream, TermlandCodec);
    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "session-teardown".into(),
        }))
        .await
        .expect("send Hello");
    match framed.next().await {
        Some(Ok(Message::HelloAck(_))) => {}
        other => panic!("expected HelloAck, got {other:?}"),
    }

    // A wrapper script, not an inline command: the server rejects shell
    // metacharacters in a client-supplied desktop_shell, which is correct --
    // a client must not get to inject shell. The auto-detected Plasma command
    // is server-side and is covered by termland-compositor's unit tests.
    //
    // The script backgrounds a process that IGNORES SIGTERM, as a stand-in
    // for plasmashell. Both halves matter:
    //
    //   backgrounded  nothing in the foreground owns it, so it outlives the
    //                 compositor exactly as the stranded instances did;
    //   ignores TERM  the stranded plasmashell processes were blocked in
    //                 drm_syncobj_array_wait_timeout and did not die on
    //                 SIGTERM. A plain `sleep` does die on SIGTERM, so a test
    //                 using one passes against the old, broken teardown too --
    //                 verified by reverting the fix and watching it stay green.
    //
    // Spawning a real plasmashell here would put a second Plasma on the
    // developer's desktop, which is the thing this test exists to stop.
    framed
        .send(Message::SessionCreate(SessionCreate {
            mode: SessionMode::Desktop,
            width: 640,
            height: 360,
            audio: false,
            quality: 30,
            desktop_shell: Some(script.clone()),
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: VideoCodec::all_preferred(),
            supported_audio_codecs: AudioCodec::all_preferred(),
        }))
        .await
        .expect("send SessionCreate");

    let deadline = Instant::now() + Duration::from_secs(45);
    let mut ready = None;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), framed.next()).await {
            Ok(Some(Ok(Message::SessionReady(r)))) => {
                ready = Some(r);
                break;
            }
            Ok(Some(Ok(_))) => continue,
            _ => break,
        }
    }
    let ready = ready.unwrap_or_else(|| {
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        panic!(
            "session never became ready. Server log:\n{}",
            log.lines().rev().take(25).collect::<Vec<_>>().into_iter().rev()
                .collect::<Vec<_>>().join("\n")
        )
    });
    let pgid = sessions(bin)
        .into_iter()
        .find(|(id, _)| *id == ready.session_id)
        .map(|(_, pid)| pid)
        .expect("session missing from the registry");

    guard.id = Some(ready.session_id.clone());
    guard.pgid = Some(pgid);

    // The backgrounded stand-in needs a moment to appear in the group.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let members = group_members(pgid);
    assert!(
        members.len() >= 2,
        "expected a compositor and its backgrounded app in group {pgid}, saw {}",
        describe(&members)
    );
    eprintln!("[test] session {} group {pgid}: {}", ready.session_id, describe(&members));

    (server, guard, ready.session_id, pgid)
}

/// Wait for the group to empty, so a slow exit is not read as a leak.
fn wait_empty(pgid: i32, timeout: Duration) -> Vec<i32> {
    let deadline = Instant::now() + timeout;
    loop {
        let members = group_members(pgid);
        if members.is_empty() || Instant::now() >= deadline {
            return members;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tokio::test]
#[ignore = "spawns a real compositor; run manually on a desktop"]
async fn closing_a_session_leaves_nothing_in_its_process_group() {
    let (_server, mut guard, id, pgid) = start_session(27881).await;
    let bin = env!("CARGO_BIN_EXE_termland-server");

    let out = Command::new(bin)
        .args(["--close-session", &id])
        .output()
        .expect("run --close-session");
    assert!(out.status.success(), "--close-session failed");
    guard.id = None;

    let survivors = wait_empty(pgid, Duration::from_secs(10));
    assert!(
        survivors.is_empty(),
        "teardown left processes running in group {pgid}: {}",
        describe(&survivors)
    );
    guard.pgid = None;

    assert!(
        !sessions(bin).iter().any(|(sid, _)| *sid == id),
        "session {id} is still in the registry after close"
    );
    eprintln!("[test] PASS: group {pgid} is empty and the record is gone");
}

/// The case that actually bit: the compositor dies first, its children are
/// reparented to init, and teardown has to reach them anyway. Orphans keep
/// their process-group id, so the group is still the handle — the old code
/// simply never used it, deleting the record and leaving them running.
#[tokio::test]
#[ignore = "spawns a real compositor; run manually on a desktop"]
async fn orphans_of_a_killed_compositor_are_still_reaped() {
    let (_server, mut guard, id, pgid) = start_session(27883).await;
    let bin = env!("CARGO_BIN_EXE_termland-server");

    // Simulate a compositor crash: kill only the leader, hard.
    unsafe {
        libc::kill(pgid, libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_secs(2));

    let orphans = group_members(pgid);
    assert!(
        !orphans.is_empty(),
        "test proves nothing: nothing outlived the compositor"
    );
    eprintln!("[test] orphans after killing the compositor: {}", describe(&orphans));

    let out = Command::new(bin)
        .args(["--close-session", &id])
        .output()
        .expect("run --close-session");
    assert!(out.status.success(), "--close-session failed");
    guard.id = None;

    let survivors = wait_empty(pgid, Duration::from_secs(10));
    assert!(
        survivors.is_empty(),
        "orphans of the dead compositor survived teardown in group {pgid}: {}",
        describe(&survivors)
    );
    guard.pgid = None;
    eprintln!("[test] PASS: orphaned group {pgid} was reaped");
}

/// The real thing: no `desktop_shell` from the client, so the server picks its
/// own — which on a KDE host is `dbus-run-session sh -c 'plasmashell
/// --no-respawn & ...'`. That command is what stranded Plasma instances on a
/// developer workstation, and it is not reachable from a client-supplied shell
/// because the server rejects shell metacharacters there.
///
/// Skipped where plasmashell is not installed rather than silently passing on
/// a terminal-only fallback, so a green run means what it looks like.
#[tokio::test]
#[ignore = "spawns a real desktop shell; run manually on a KDE desktop"]
async fn an_auto_detected_plasma_shell_is_fully_reaped() {
    if first_on_path(&["plasmashell"]).is_none() {
        eprintln!("[test] SKIP: plasmashell is not installed on this host");
        return;
    }
    let bin = env!("CARGO_BIN_EXE_termland-server");
    let port = 27885;
    let plasma_before = plasmashell_pids();

    let log_path = std::env::temp_dir().join(format!("termland-teardown-{port}.log"));
    let log = std::fs::File::create(&log_path).expect("create server log");
    let _server = ChildGuard(
        Command::new(bin)
            .args(["--bind", "127.0.0.1", "--port", &port.to_string()])
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("spawn termland-server"),
    );
    let mut guard = SessionGuard { bin, id: None, pgid: None };
    tokio::time::sleep(Duration::from_millis(800)).await;

    let stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
    let mut framed = Framed::new(stream, TermlandCodec);
    framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "session-teardown-plasma".into(),
        }))
        .await
        .expect("send Hello");
    match framed.next().await {
        Some(Ok(Message::HelloAck(_))) => {}
        other => panic!("expected HelloAck, got {other:?}"),
    }
    framed
        .send(Message::SessionCreate(SessionCreate {
            mode: SessionMode::Desktop,
            width: 640,
            height: 360,
            audio: false,
            quality: 30,
            // The point of this test: let the server choose.
            desktop_shell: None,
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: VideoCodec::all_preferred(),
            supported_audio_codecs: AudioCodec::all_preferred(),
        }))
        .await
        .expect("send SessionCreate");

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut ready = None;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), framed.next()).await {
            Ok(Some(Ok(Message::SessionReady(r)))) => {
                ready = Some(r);
                break;
            }
            Ok(Some(Ok(_))) => continue,
            _ => break,
        }
    }
    let ready = ready.unwrap_or_else(|| {
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        panic!("session never became ready. Server log:\n{log}")
    });
    let pgid = sessions(bin)
        .into_iter()
        .find(|(id, _)| *id == ready.session_id)
        .map(|(_, pid)| pid)
        .expect("session missing from the registry");
    guard.id = Some(ready.session_id.clone());
    guard.pgid = Some(pgid);

    // Plasma takes a while to come up; wait for it rather than racing it.
    let plasma_deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < plasma_deadline {
        if plasmashell_pids().len() > plasma_before.len() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let during = plasmashell_pids();
    let spawned: Vec<i32> = during.iter().copied().filter(|p| !plasma_before.contains(p)).collect();
    eprintln!(
        "[test] session {} group {pgid}: {}",
        ready.session_id,
        describe(&group_members(pgid))
    );
    eprintln!("[test] plasmashell spawned by the session: {}", describe(&spawned));

    // Let Plasma finish coming up before tearing it down. Killing it two
    // seconds into startup, while it is still registering DBus signal hooks
    // from timers, is a race a real session does not run: TERMLAND_SETTLE_SECS
    // exists so that difference can be measured rather than assumed.
    let settle: u64 = std::env::var("TERMLAND_SETTLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    std::thread::sleep(Duration::from_secs(settle));

    Command::new(bin)
        .args(["--close-session", &ready.session_id])
        .output()
        .expect("run --close-session");
    guard.id = None;

    let survivors = wait_empty(pgid, Duration::from_secs(15));
    assert!(
        survivors.is_empty(),
        "teardown left processes in group {pgid}: {}",
        describe(&survivors)
    );
    guard.pgid = None;

    // The check that matters to the host: no plasmashell beyond the ones that
    // were already running before this test started. A survivor here is what
    // collides with the real session's global shortcuts and takes kded6 down.
    let after = plasmashell_pids();
    let leaked: Vec<i32> = after.into_iter().filter(|p| !plasma_before.contains(p)).collect();
    assert!(
        leaked.is_empty(),
        "session stranded plasmashell on the host: {}",
        describe(&leaked)
    );
    eprintln!("[test] PASS: no plasmashell stranded, group {pgid} empty");
}

/// PIDs of every running `plasmashell`, host-wide.
fn plasmashell_pids() -> Vec<i32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(pid) = name.parse::<i32>() else { continue };
        if let Ok(cmd) = std::fs::read_to_string(format!("/proc/{pid}/cmdline")) {
            if cmd.split('\0').next().is_some_and(|a| a.ends_with("plasmashell")) {
                out.push(pid);
            }
        }
    }
    out.sort_unstable();
    out
}
