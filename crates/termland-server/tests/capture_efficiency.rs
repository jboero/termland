//! Capture/encode efficiency benchmark: how much server and compositor CPU,
//! and how many video frames and bytes, a session costs under a few
//! representative desktop workloads.
//!
//! `#[ignore]`d: spawns a real server, which starts a real labwc compositor
//! and real apps (konsole, mpv). Run it manually and compare two builds or two
//! server configurations:
//!
//! ```sh
//! TERMLAND_BENCH_LABEL=before \
//!   cargo test --release -p termland-server --test capture_efficiency -- --ignored --nocapture
//! ```
//!
//! Environment:
//! - `TERMLAND_BENCH_LABEL`: name for this run in the output (default "run").
//! - `TERMLAND_BENCH_SECS`: measured seconds per workload (default 20).
//! - `TERMLAND_BENCH_WORKLOADS`: comma-separated subset of the names below.
//! - `TERMLAND_BENCH_SERVER_ARGS`: extra server arguments, space-separated,
//!   e.g. `--capture-mode full`.
//! - `TERMLAND_BENCH_QUALITY`: the session's quality, 1-100 (default 75).
//! - `TERMLAND_BENCH_BITRATE`: an explicit bitrate in kbps, instead of quality.
//! - `TERMLAND_BENCH_MAY_IDLE=1`: create the session with `video_may_idle`, as
//!   a current client does. Otherwise it is created as an older client would.
//! - `TERMLAND_BENCH_OUT`: append one CSV line per workload to this file.
//! - `TERMLAND_BENCH_SNAPSHOTS`: save a screenshot of each workload (via
//!   grim) to this directory after warmup, to check it is doing what it says.
//! - `CUDA_VISIBLE_DEVICES=` (empty) hides an NVIDIA GPU, so AV1 falls back
//!   to SVT-AV1: the CPU-bound case this benchmark mostly exists for.
//!
//! The workload apps never produce sound (`mpv --no-audio`) and the session
//! is created with `audio: false`.

use std::io::Write as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use termland_codec::VideoDecoder;
use termland_protocol::{
    AudioCodec, FrameType, Hello, Message, SessionCreate, SessionMode, TermlandCodec, VideoCodec,
    PROTOCOL_VERSION,
};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;

/// (name, app, script). The app is started as the session's shell; when a
/// script is given, it is written to a file and run by the app, because the
/// server rejects shell metacharacters in `desktop_shell`.
const WORKLOADS: &[(&str, &str, Option<&str>)] = &[
    // A terminal doing nothing: the common case for a remote desktop.
    ("idle", "konsole", None),
    // A character every 200 ms: someone typing.
    ("typing", "konsole -e", Some("while :; do printf x; sleep 0.2; done")),
    // Once a second: a clock or a status bar.
    ("clock", "konsole -e", Some("while :; do date; sleep 1; done")),
    // 20 new lines a second, each different, so every update scrolls the terminal.
    ("scroll", "konsole -e", Some("i=0; while :; do i=$((i+1)); echo \"line $i $(date +%N)\"; sleep 0.05; done")),
    // Full-screen motion every frame: the worst case, where damage can't help.
    (
        "video",
        "mpv --no-audio --no-osc --fs --loop=inf av://lavfi:testsrc2=size=1920x1080:rate=30",
        None,
    ),
];

/// The `desktop_shell` command for a workload, writing its script if it has one.
fn shell_command(name: &str, app: &str, script: Option<&str>) -> String {
    let Some(body) = script else { return app.to_string() };
    let dir = std::env::temp_dir().join(format!("termland-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sh"));
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    format!("{app} {}", path.display())
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Closes the persistent session on drop, so a failed run leaves no compositor behind.
struct SessionGuard {
    bin: &'static str,
    id: Option<String>,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else { return };
        match Command::new(self.bin).args(["--close-session", &id]).output() {
            Ok(o) if o.status.success() => {}
            _ => eprintln!("[bench] WARNING: could not close session {id} — close it by hand"),
        }
    }
}

/// utime + stime of a process, all threads, in seconds.
fn cpu_secs(pid: u32) -> Option<f64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the parenthesised comm, which may itself contain spaces.
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // utime and stime are fields 14 and 15 of the full line: 11 and 12 here.
    let ticks: u64 = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    Some(ticks as f64 / hz)
}

/// The session's registry record (`key=value` lines), which says where its
/// compositor is.
fn session_record(id: &str) -> std::collections::HashMap<String, String> {
    let dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    let path = format!("{dir}/termland/sessions/{id}.session");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Run grim against the session's compositor, writing to `out` ("-" for stdout).
fn grim(record: &std::collections::HashMap<String, String>, format: &str, out: &str) -> Vec<u8> {
    let output = Command::new("grim")
        .args(["-t", format, out])
        .env("WAYLAND_DISPLAY", &record["wayland_display"])
        .env("XDG_RUNTIME_DIR", &record["runtime_dir"])
        .output()
        .expect("run grim");
    assert!(output.status.success(), "grim failed: {}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

/// A server with one session, driven over TCP the way a client drives it.
struct Bench {
    // Field order is drop order: disconnect, close the session, stop the server.
    framed: Framed<TcpStream, TermlandCodec>,
    session: SessionGuard,
    server: ChildGuard,
    codec: Option<VideoCodec>,
    name: String,
    port: u16,
}

impl Bench {
    async fn start(name: &str, shell: &str, port: u16, video_may_idle: bool) -> Self {
        let bin = env!("CARGO_BIN_EXE_termland-server");
        let mut args = vec!["--bind".to_string(), "127.0.0.1".into(), "--port".into(), port.to_string()];
        if let Ok(extra) = std::env::var("TERMLAND_BENCH_SERVER_ARGS") {
            args.extend(extra.split_whitespace().map(String::from));
        }
        let child = Command::new(bin)
            .args(&args)
            .env("RUST_LOG", "warn")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("failed to spawn termland-server");
        let server = ChildGuard(child);
        let session = SessionGuard { bin, id: None };

        tokio::time::sleep(Duration::from_millis(800)).await;
        let stream = TcpStream::connect(("127.0.0.1", port)).await.expect("connect");
        let mut framed = Framed::new(stream, TermlandCodec);
        framed
            .send(Message::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                client_name: "capture-efficiency-bench".into(),
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
                width: WIDTH,
                height: HEIGHT,
                audio: false,
                quality: std::env::var("TERMLAND_BENCH_QUALITY")
                    .ok()
                    .and_then(|q| q.parse().ok())
                    .unwrap_or(75),
                desktop_shell: Some(shell.into()),
                encoder_preset: None,
                encoder_crf: None,
                encoder_extra_params: None,
                // AV1 only, so the encoder is NVENC AV1 or (GPU hidden) SVT-AV1.
                supported_codecs: vec![VideoCodec::Av1],
                supported_audio_codecs: AudioCodec::all_preferred(),
                video_may_idle,
                bitrate_kbps: std::env::var("TERMLAND_BENCH_BITRATE").ok().and_then(|b| b.parse().ok()),
            }))
            .await
            .expect("send SessionCreate");
        Self { framed, session, server, codec: None, name: name.into(), port }
    }

    /// The next message before `deadline`, or None once it passes. Records
    /// SessionReady; panics if the session or connection ends.
    async fn next(&mut self, deadline: Instant) -> Option<Message> {
        let left = deadline.saturating_duration_since(Instant::now());
        let name = &self.name;
        match tokio::time::timeout(left, self.framed.next()).await {
            Err(_) => None,
            Ok(Some(Ok(Message::SessionReady(sr)))) => {
                self.session.id = Some(sr.session_id.clone());
                self.codec = sr.codec;
                Some(Message::SessionReady(sr))
            }
            Ok(Some(Ok(Message::SessionEnd(se)))) => panic!("{name}: session ended: {}", se.reason),
            Ok(Some(Ok(msg))) => Some(msg),
            Ok(Some(Err(e))) => panic!("{name}: protocol error: {e}"),
            Ok(None) => panic!("{name}: connection closed"),
        }
    }

    fn record(&self) -> std::collections::HashMap<String, String> {
        session_record(self.session.id.as_deref().expect("no SessionReady yet"))
    }
}

fn may_idle_from_env() -> bool {
    std::env::var("TERMLAND_BENCH_MAY_IDLE").is_ok_and(|v| v == "1")
}

/// Wait for a video frame, up to `within`.
async fn next_video(b: &mut Bench, within: Duration) -> Option<termland_protocol::VideoFrame> {
    let deadline = Instant::now() + within;
    while let Some(msg) = b.next(deadline).await {
        if let Message::VideoFrame(vf) = msg {
            return Some(vf);
        }
    }
    None
}

#[derive(Default)]
struct Totals {
    frames: u64,
    keyframes: u64,
    bytes: u64,
}

async fn run_workload(name: &str, shell: &str, port: u16, secs: u64) -> String {
    let mut b = Bench::start(name, shell, port, may_idle_from_env()).await;
    let server_pid = b.server.0.id();

    // Let the app start and settle before measuring.
    let warmup_end = Instant::now() + Duration::from_secs(6);
    while b.next(warmup_end).await.is_some() {}
    let record = b.record();
    let comp_pid = record["compositor_pid"].parse().expect("compositor_pid");
    let label = std::env::var("TERMLAND_BENCH_LABEL").unwrap_or_else(|_| "run".into());
    if let Ok(dir) = std::env::var("TERMLAND_BENCH_SNAPSHOTS") {
        grim(&record, "png", &format!("{dir}/{label}-{name}.png"));
    }

    let server_cpu0 = cpu_secs(server_pid).unwrap_or(0.0);
    let comp_cpu0 = cpu_secs(comp_pid).unwrap_or(0.0);
    let start = Instant::now();
    let end = start + Duration::from_secs(secs);
    let mut t = Totals::default();
    while let Some(msg) = b.next(end).await {
        if let Message::VideoFrame(vf) = msg {
            t.frames += 1;
            t.bytes += vf.data.len() as u64;
            if matches!(vf.frame_type, FrameType::Keyframe) {
                t.keyframes += 1;
            }
        }
    }
    let wall = start.elapsed().as_secs_f64();
    let server_cpu = cpu_secs(server_pid).unwrap_or(0.0) - server_cpu0;
    let comp_cpu = cpu_secs(comp_pid).unwrap_or(0.0) - comp_cpu0;
    let codec = b.codec;
    drop(b);

    format!(
        "{label},{name},{codec:?},{:.1},{:.1},{:.1},{:.1},{},{:.1}",
        100.0 * server_cpu / wall,
        100.0 * comp_cpu / wall,
        t.frames as f64 / wall,
        t.bytes as f64 / wall / 1024.0,
        t.keyframes,
        wall,
    )
}

/// PSNR in dB between a decoded frame (0RGB u32 pixels) and a reference
/// screenshot (binary PPM, RGB bytes).
fn psnr(decoded: &[u32], ppm: &[u8]) -> f64 {
    // "P6\n<w> <h>\n255\n" then the pixels.
    let mut fields = 0;
    let mut i = 0;
    while fields < 4 {
        while ppm[i].is_ascii_whitespace() {
            i += 1;
        }
        while !ppm[i].is_ascii_whitespace() {
            i += 1;
        }
        fields += 1;
    }
    let rgb = &ppm[i + 1..];
    assert_eq!(rgb.len(), decoded.len() * 3, "reference and decoded frame differ in size");
    let mut sq: f64 = 0.0;
    for (px, r) in decoded.iter().zip(rgb.chunks_exact(3)) {
        for (shift, &want) in [16, 8, 0].iter().zip(r) {
            let d = ((px >> shift) & 0xff) as f64 - want as f64;
            sq += d * d;
        }
    }
    let mse = sq / (decoded.len() * 3) as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() }
}

/// How close the picture a client is left with is to the real screen, once
/// motion stops. With damage capture the stream goes quiet when the screen
/// does, so the last frames sent must leave a clean image; with full capture
/// the encoder keeps refining the still image for as long as it runs.
///
/// Scrolls a terminal for 3 s, then stops; decodes every frame received, and
/// compares the last decoded frame with a lossless screenshot of the session.
#[tokio::test]
#[ignore = "needs labwc, konsole and grim; spawns a real session — run manually"]
async fn still_frame_quality() {
    let shell = shell_command(
        "burst",
        "konsole -e",
        Some("i=0; while [ $i -lt 60 ]; do i=$((i+1)); echo \"line $i $(date +%N)\"; sleep 0.05; done; exec sleep 100000"),
    );
    let mut b = Bench::start("burst", &shell, 27950, may_idle_from_env()).await;
    let mut decoder: Option<VideoDecoder> = None;
    let mut last: Option<Vec<u32>> = None;
    let mut frames = 0;
    let end = Instant::now() + Duration::from_secs(14);
    while let Some(msg) = b.next(end).await {
        if let Message::VideoFrame(vf) = msg {
            frames += 1;
            let codec = vf.codec.or(b.codec).expect("no codec announced");
            let dec = decoder.get_or_insert_with(|| VideoDecoder::for_codec(codec).expect("decoder"));
            if let Ok((_, _, pixels)) = dec.decode(&vf.data) {
                last = Some(pixels);
            }
        }
    }
    let reference = grim(&b.record(), "ppm", "-");
    let last = last.expect("no frame decoded");
    let label = std::env::var("TERMLAND_BENCH_LABEL").unwrap_or_else(|_| "run".into());
    eprintln!("{label},still-psnr-db,{:.2},frames,{frames}", psnr(&last, &reference));
}

#[tokio::test]
#[ignore = "needs labwc, konsole and mpv; spawns real sessions — run manually"]
async fn capture_efficiency() {
    let secs: u64 = std::env::var("TERMLAND_BENCH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(20);
    let only: Option<Vec<String>> = std::env::var("TERMLAND_BENCH_WORKLOADS")
        .ok()
        .map(|s| s.split(',').map(|w| w.trim().to_string()).collect());

    let header = "label,workload,codec,server_cpu_pct,compositor_cpu_pct,fps,kib_per_s,keyframes,secs";
    eprintln!("{header}");
    let mut out = std::env::var("TERMLAND_BENCH_OUT").ok().map(|p| {
        let new = !std::path::Path::new(&p).exists();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).expect("open TERMLAND_BENCH_OUT");
        if new {
            writeln!(f, "{header}").unwrap();
        }
        f
    });

    for (i, &(name, app, script)) in WORKLOADS.iter().enumerate() {
        if only.as_ref().is_some_and(|o| !o.iter().any(|w| w == name)) {
            continue;
        }
        let shell = shell_command(name, app, script);
        let line = run_workload(name, &shell, 27900 + i as u16, secs).await;
        eprintln!("{line}");
        if let Some(f) = out.as_mut() {
            writeln!(f, "{line}").unwrap();
        }
    }
}

/// Damage capture must not leave a client without a picture when it needs
/// one while nothing on screen is changing: after a resize, and when a client
/// attaches to an idle session (which must also start on a keyframe).
#[tokio::test]
#[ignore = "needs labwc and konsole; spawns a real session — run manually"]
async fn idle_session_still_answers_resize_and_attach() {
    let mut b = Bench::start("idle-resize-attach", "konsole", 27960, true).await;
    let warmup_end = Instant::now() + Duration::from_secs(6);
    while b.next(warmup_end).await.is_some() {}
    let session_id = b.session.id.clone().expect("no SessionReady");

    assert!(
        next_video(&mut b, Duration::from_secs(2)).await.is_none(),
        "an idle screen sent video to a client that set video_may_idle",
    );

    b.framed
        .send(Message::SessionResize(termland_protocol::SessionResize { width: 1280, height: 720 }))
        .await
        .expect("send SessionResize");
    // The compositor applies the new mode asynchronously, so a frame or two at
    // the old size may come first.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut resized = false;
    while let Some(vf) = next_video(&mut b, deadline.saturating_duration_since(Instant::now())).await {
        if (vf.width, vf.height) == (1280, 720) {
            resized = true;
            break;
        }
    }
    assert!(resized, "no frame at the new size within 2 s of a resize");

    // Settle, then detach and attach again from a fresh connection.
    let settle_end = Instant::now() + Duration::from_secs(2);
    while b.next(settle_end).await.is_some() {}
    let stream = TcpStream::connect(("127.0.0.1", b.port)).await.expect("reconnect");
    let old = std::mem::replace(&mut b.framed, Framed::new(stream, TermlandCodec));
    drop(old);
    b.framed
        .send(Message::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "capture-efficiency-bench".into(),
        }))
        .await
        .expect("send Hello");
    b.framed
        .send(Message::SessionAttach(termland_protocol::SessionAttach {
            session_id,
            audio: false,
            quality: 75,
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            supported_codecs: vec![VideoCodec::Av1],
            supported_audio_codecs: AudioCodec::all_preferred(),
            video_may_idle: true,
            bitrate_kbps: None,
        }))
        .await
        .expect("send SessionAttach");
    let vf = next_video(&mut b, Duration::from_secs(3)).await.expect("no frame after attaching");
    assert!(matches!(vf.frame_type, FrameType::Keyframe), "attach did not start on a keyframe");
}
