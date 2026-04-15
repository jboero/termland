mod transport;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "termland-server", about = "Termland remote desktop server")]
struct Args {
    /// Run as SSH subsystem (read/write protocol on stdin/stdout)
    #[arg(long)]
    subsystem: bool,

    /// Listen on TCP port (default when not in subsystem mode)
    #[arg(short, long, default_value = "7867")]
    port: u16,

    /// Bind address for TCP mode
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    if args.subsystem {
        tracing::info!("Starting in SSH subsystem mode");
        transport::run_subsystem().await?;
    } else {
        tracing::info!("Starting TCP listener on {}:{}", args.bind, args.port);
        transport::run_tcp_listener(&args.bind, args.port).await?;
    }

    Ok(())
}
