mod connection;
mod display;
mod overlay;
mod tray;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use tracing_subscriber::EnvFilter;

/// Video codec selectable via `--codec`. A `ValueEnum` (rather than a bare
/// string parser) so shell completions offer the codec names as suggestions.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum CodecArg {
    Av1,
    Vp9,
    Vp8,
    #[value(name = "h265", alias = "hevc")]
    H265,
    #[value(name = "h264", alias = "avc")]
    H264,
}

impl CodecArg {
    pub fn to_wire(self) -> termland_protocol::VideoCodec {
        use termland_protocol::VideoCodec;
        match self {
            CodecArg::Av1 => VideoCodec::Av1,
            CodecArg::Vp9 => VideoCodec::Vp9,
            CodecArg::Vp8 => VideoCodec::Vp8,
            CodecArg::H265 => VideoCodec::H265,
            CodecArg::H264 => VideoCodec::H264,
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "termland-client", about = "Termland remote desktop client", version)]
pub struct Args {
    /// Server address (host:port for TCP, user@host for SSH)
    #[arg(required_unless_present = "completions")]
    pub server: Option<String>,

    /// Use SSH subsystem instead of direct TCP.
    /// Runs: ssh -s <server> termland
    #[arg(long)]
    pub ssh: bool,

    /// Extra options to pass to the ssh command (repeatable).
    /// Example: --ssh-opt="-oPort=9022" --ssh-opt="-oCompression=yes"
    #[arg(long)]
    pub ssh_opt: Vec<String>,

    /// Requested width
    #[arg(long, default_value = "1280")]
    pub width: u32,

    /// Requested height
    #[arg(long, default_value = "720")]
    pub height: u32,

    /// Session mode: "desktop" or "app:<command>"
    /// Examples: --mode desktop, --mode app:firefox, --mode "app:konsole --profile Dark"
    #[arg(long, default_value = "desktop")]
    pub mode: String,

    /// Video quality (1-100). Lower values = lower bitrate for slow connections.
    /// Default 75. Typical: 90=high, 75=balanced, 50=low, 25=very low.
    #[arg(short, long, default_value = "75")]
    pub quality: u8,

    /// Force a specific video codec instead of negotiating the best available.
    /// When set, the server MUST encode with this codec (falling back to a
    /// software encoder for it if no hardware is available); the session fails
    /// if it cannot. Aliases: hevc = h265, avc = h264.
    #[arg(long, value_enum)]
    pub codec: Option<CodecArg>,

    /// Resume an existing session by id (see --list-sessions) instead of
    /// creating a new one. The remote apps keep running across disconnects.
    #[arg(long, value_name = "SESSION_ID")]
    pub attach: Option<String>,

    /// List resumable sessions on the server and exit (no window).
    #[arg(long)]
    pub list_sessions: bool,

    /// Close (terminate) a session by id and exit (no window).
    #[arg(long, value_name = "SESSION_ID")]
    pub close: Option<String>,

    /// Run as a system-tray session manager for the given host (one global
    /// icon; lists/ resumes/ closes sessions and starts new ones).
    #[arg(long)]
    pub tray: bool,

    /// For --mode desktop: startup command to run inside labwc.
    /// Examples: "konsole", "startplasma-wayland", "dbus-run-session sway".
    /// If omitted, the server auto-detects an available terminal.
    #[arg(long)]
    pub desktop_shell: Option<String>,

    /// Override encoder preset. SVT-AV1: 0..13 (higher = faster, lower latency).
    /// QSV: veryfast|faster|fast|medium|slow|slower|veryslow.
    /// NVENC: p1..p7 (p1 = fastest).
    #[arg(long)]
    pub preset: Option<String>,

    /// Override CRF / quantizer (SVT-AV1 only). 0..63, higher = smaller / lower quality.
    #[arg(long)]
    pub crf: Option<u8>,

    /// Extra svtav1-params to append to our low-delay defaults (SVT-AV1 only).
    /// Format: "key=value:key=value". Example: "fast-decode=1:tile-columns=2:scm=2"
    #[arg(long)]
    pub svt_params: Option<String>,

    /// Enable audio forwarding (Opus over PulseAudio).
    #[arg(long)]
    pub audio: bool,

    /// Connect with TLS encryption.
    #[arg(long)]
    pub tls: bool,

    /// Accept self-signed or invalid TLS certificates (use with --tls).
    #[arg(long)]
    pub accept_invalid_certs: bool,

    /// Username for authentication (defaults to current user).
    #[arg(short, long)]
    pub user: Option<String>,

    /// Password for authentication.
    /// WARNING: visible in /proc/pid/cmdline. Prefer TERMLAND_PASSWORD env var.
    #[arg(long)]
    pub password: Option<String>,

    /// Generate shell completion script and exit.
    /// Usage: termland-client --completions fish > ~/.config/fish/completions/termland-client.fish
    #[arg(long, value_name = "SHELL")]
    pub completions: Option<Shell>,
}

impl Args {
    pub fn session_mode(&self) -> termland_protocol::SessionMode {
        if self.mode == "desktop" {
            termland_protocol::SessionMode::Desktop
        } else if let Some(cmd) = self.mode.strip_prefix("app:") {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            termland_protocol::SessionMode::App {
                command: parts[0].to_string(),
                args: parts[1..].iter().map(|s| s.to_string()).collect(),
            }
        } else {
            termland_protocol::SessionMode::Desktop
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    if let Some(shell) = args.completions {
        clap_complete::generate(
            shell,
            &mut Args::command(),
            "termland-client",
            &mut std::io::stdout(),
        );
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // One-shot session-management ops (and the tray) run without a session
    // window.
    if args.list_sessions || args.close.is_some() || args.tray {
        use anyhow::Context;
        let server = args.server.clone().context("a server address is required")?;
        let params = connection::ConnectParams {
            mode: args.session_mode(),
            width: args.width,
            height: args.height,
            quality: args.quality,
            audio: false,
            ssh_opts: args.ssh_opt.clone(),
            tls: args.tls || args.accept_invalid_certs,
            accept_invalid_certs: args.accept_invalid_certs,
            username: args.user.clone(),
            password: args.password.clone(),
            desktop_shell: None,
            encoder_preset: None,
            encoder_crf: None,
            encoder_extra_params: None,
            codec: None,
            attach: None,
        };

        if args.tray {
            // Blocks until quit; runs its own refresh loop + runtime.
            return tray::run(server, args.ssh, params);
        }

        let op = match args.close.clone() {
            Some(id) => connection::ControlOp::Close(id),
            None => connection::ControlOp::List,
        };
        let rt = tokio::runtime::Runtime::new()?;
        return rt.block_on(connection::run_control(&server, args.ssh, params, op));
    }

    display::run(args)
}
