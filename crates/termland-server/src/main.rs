mod auth;
mod tls;
mod transport;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "termland-server", about = "Termland remote desktop server", version)]
struct Args {
    /// Run as SSH subsystem (read/write protocol on stdin/stdout).
    /// Register in sshd_config: Subsystem termland /usr/bin/termland-server --subsystem
    #[arg(long)]
    subsystem: bool,

    /// Listen on TCP port (default when not in subsystem mode)
    #[arg(short, long, default_value = "7867")]
    port: u16,

    /// Bind address for TCP mode
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    /// Enable TLS for TCP connections. Auto-generates a self-signed cert
    /// in ~/.config/termland/ if no --tls-cert/--tls-key is provided.
    #[arg(long)]
    tls: bool,

    /// Path to TLS certificate PEM file (implies --tls)
    #[arg(long)]
    tls_cert: Option<String>,

    /// Path to TLS private key PEM file (implies --tls)
    #[arg(long)]
    tls_key: Option<String>,

    /// Require PAM authentication before session creation.
    /// Uses the "termland" PAM service, falling back to "login".
    #[arg(long)]
    auth: bool,

    /// Generate shell completion script and exit.
    /// Usage: termland-server --completions bash > /etc/bash_completion.d/termland-server
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if let Some(shell) = args.completions {
        clap_complete::generate(
            shell,
            &mut Args::command(),
            "termland-server",
            &mut std::io::stdout(),
        );
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    if args.subsystem {
        tracing::info!("Starting in SSH subsystem mode");
        transport::run_subsystem().await?;
    } else {
        let use_tls = args.tls || args.tls_cert.is_some() || args.tls_key.is_some();

        if args.auth && !use_tls {
            tracing::warn!("WARNING: --auth without --tls sends credentials in PLAINTEXT!");
            tracing::warn!("Anyone on the network can intercept passwords.");
            tracing::warn!("Add --tls or use SSH subsystem mode for secure authentication.");
        }
        if !use_tls && args.bind != "127.0.0.1" && args.bind != "::1" {
            tracing::warn!("Listening without TLS on non-localhost ({}) — traffic is unencrypted", args.bind);
        }

        let acceptor = if use_tls {
            let cert = args.tls_cert.as_deref().map(std::path::Path::new);
            let key = args.tls_key.as_deref().map(std::path::Path::new);
            Some(tls::build_tls_acceptor(cert, key)?)
        } else {
            None
        };

        tracing::info!(
            "Starting TCP listener on {}:{} (TLS: {}, Auth: {})",
            args.bind, args.port,
            if use_tls { "enabled" } else { "disabled" },
            if args.auth { "PAM" } else { "none" },
        );

        transport::run_tcp_listener(&args.bind, args.port, acceptor, args.auth).await?;
    }

    Ok(())
}
