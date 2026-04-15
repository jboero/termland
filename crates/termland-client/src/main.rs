mod connection;
mod display;
mod overlay;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "termland-client", about = "Termland remote desktop client")]
pub struct Args {
    /// Server address (host:port for TCP, user@host for SSH)
    #[arg()]
    pub server: String,

    /// Use SSH subsystem instead of direct TCP
    #[arg(long)]
    pub ssh: bool,

    /// Requested width
    #[arg(long, default_value = "1280")]
    pub width: u32,

    /// Requested height
    #[arg(long, default_value = "720")]
    pub height: u32,

    /// Session mode: "desktop" or "app:command"
    #[arg(long, default_value = "desktop")]
    pub mode: String,

    /// Video quality (1-100). Lower values = lower bitrate for slow connections.
    /// Default 75. Typical: 90=high, 75=balanced, 50=low, 25=very low.
    #[arg(short, long, default_value = "75")]
    pub quality: u8,

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
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    display::run(args)
}
