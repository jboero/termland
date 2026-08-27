mod auth;
mod quic;
mod webtransport;
mod registry;
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

    /// Enable TLS for TCP connections. Auto-generates a self-signed cert in
    /// /etc/pki/termland/ (root) or ~/.config/termland/ (unprivileged) if no
    /// --tls-cert/--tls-key is provided.
    #[arg(long)]
    tls: bool,

    /// Path to TLS certificate PEM file (implies --tls)
    #[arg(long)]
    tls_cert: Option<String>,

    /// Path to TLS private key PEM file (implies --tls)
    #[arg(long)]
    tls_key: Option<String>,

    /// Generate the self-signed TLS keypair if it is missing, then exit.
    /// Run at install time by the RPM, because the service itself runs under
    /// ProtectSystem=strict and cannot write /etc/pki/termland. Also the
    /// command to run by hand if the keypair is ever missing. Idempotent — an
    /// existing keypair is kept.
    #[arg(long)]
    generate_cert: bool,

    /// Require PAM authentication before session creation.
    /// Uses the "termland" PAM service, falling back to "login".
    #[arg(long)]
    auth: bool,

    /// Also listen for QUIC (UDP) connections, alongside the TCP listener
    /// (Q1: QUIC as a drop-in byte transport, see docs/quic-transport.md).
    /// Purely additive — the TCP listener keeps running unchanged. QUIC
    /// always requires TLS 1.3, so this reuses --tls-cert/--tls-key (or
    /// auto-generates a self-signed cert, same as --tls) regardless of
    /// whether --tls itself was also passed for the TCP side.
    #[arg(long)]
    quic: bool,

    /// UDP port for the QUIC listener. Defaults to the same number as
    /// --port — that's fine, they're different protocols/sockets.
    #[arg(long)]
    quic_port: Option<u16>,

    /// Also listen for browser WebTransport (HTTP/3) connections.
    ///
    /// Separate from --quic on purpose: a browser cannot speak the raw QUIC
    /// listener's protocol, and the native client cannot speak HTTP/3
    /// extended CONNECT. See crates/termland-server/src/webtransport.rs.
    #[arg(long)]
    webtransport: bool,

    /// UDP port for the WebTransport listener. Defaults to --port + 1, since
    /// HTTP/3 and raw QUIC cannot share a socket (different ALPN).
    #[arg(long)]
    webtransport_port: Option<u16>,

    /// Browser origin permitted to open WebTransport sessions, e.g.
    /// https://desktop.example.com. Repeat for several.
    ///
    /// With none given, every browser request is refused. That is deliberate:
    /// without it, any page a user visits could reach a Termland server on
    /// their network and — on a server running without --auth — create and
    /// drive a desktop session.
    #[arg(long = "webtransport-origin", value_name = "ORIGIN")]
    webtransport_origins: Vec<String>,

    /// Generate shell completion script and exit.
    /// Usage: termland-server --completions bash > /etc/bash_completion.d/termland-server
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,

    /// List active sessions on this host (session id, mode, size, age, pid,
    /// audio) and exit. Reads the session registry directly — no server
    /// process needs to be listening for this to work.
    #[arg(long)]
    list_sessions: bool,

    /// Close (terminate) one or more sessions by id and exit. Repeatable
    /// (--close-session a --close-session b) or comma-separated
    /// (--close-session a,b). Exits non-zero if any given id doesn't
    /// correspond to a live session.
    #[arg(long, value_name = "ID", value_delimiter = ',')]
    close_session: Vec<String>,
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

    // --list-sessions / --close-session are one-shot, host-local admin
    // operations, just like --completions above: they short-circuit before
    // the listener (or its tracing setup) ever starts. They read/write the
    // filesystem-backed registry (crate::registry) directly rather than
    // talking to a running server over the wire — there is no daemon to
    // talk to, and none of this needs the tokio runtime beyond what
    // `#[tokio::main]` already gives us for free.
    //
    // No PAM/--auth gate is applied here: these operations are only reachable
    // by someone who already has filesystem access to $XDG_RUNTIME_DIR on
    // this host, and such a user can already `kill` the compositor PID (or
    // read/rm the session files) directly with equivalent effect. Gating
    // this CLI path wouldn't add any real protection, only friction for the
    // legitimate local-admin case it exists for.
    if args.list_sessions {
        return list_sessions();
    }
    if !args.close_session.is_empty() {
        return close_sessions(&args.close_session);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    if args.generate_cert {
        let cert = args.tls_cert.as_deref().map(std::path::Path::new);
        let key = args.tls_key.as_deref().map(std::path::Path::new);
        let (cert, key) = tls::generate_if_missing(cert, key)?;
        tracing::info!("TLS certificate: {}", cert.display());
        tracing::info!("TLS private key:  {}", key.display());
        return Ok(());
    }

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
            "Starting TCP listener on {}:{} (TLS: {}, Auth: {}, QUIC: {})",
            args.bind, args.port,
            if use_tls { "enabled" } else { "disabled" },
            if args.auth { "PAM" } else { "none" },
            if args.quic { "enabled" } else { "disabled" },
        );

        // Each listener is an independent future; whichever are enabled run
        // together and any one failing brings the server down, which is the
        // existing behaviour for TCP+QUIC.
        let tcp_fut = transport::run_tcp_listener(&args.bind, args.port, acceptor, args.auth);

        match (args.quic, args.webtransport) {
            (false, false) => tcp_fut.await?,
            (true, false) => {
                tokio::try_join!(tcp_fut, quic_listener(&args))?;
            }
            (false, true) => {
                tokio::try_join!(tcp_fut, webtransport_listener(&args))?;
            }
            (true, true) => {
                tokio::try_join!(tcp_fut, quic_listener(&args), webtransport_listener(&args))?;
            }
        }
    }

    Ok(())
}

/// The raw-QUIC listener future for the current arguments.
fn quic_listener(args: &Args) -> impl std::future::Future<Output = Result<()>> + '_ {
    let port = args.quic_port.unwrap_or(args.port);
    quic::run_quic_listener(
        &args.bind,
        port,
        args.tls_cert.as_deref().map(std::path::Path::new),
        args.tls_key.as_deref().map(std::path::Path::new),
        args.auth,
    )
}

/// The browser WebTransport listener future for the current arguments.
///
/// Defaults to `--port + 1` rather than sharing the QUIC port: both are UDP
/// but they negotiate different ALPN (`h3` vs `termland/1`), so one socket
/// cannot serve both.
fn webtransport_listener(args: &Args) -> impl std::future::Future<Output = Result<()>> + '_ {
    let port = args.webtransport_port.unwrap_or(args.port.saturating_add(1));
    webtransport::run_webtransport_listener(
        &args.bind,
        port,
        args.tls_cert.as_deref().map(std::path::Path::new),
        args.tls_key.as_deref().map(std::path::Path::new),
        args.webtransport_origins.clone(),
        args.auth,
    )
}

/// `--list-sessions`: print all live sessions from the registry and exit.
/// Mirrors the SESSION ID / MODE / SIZE / AGE columns of
/// `termland-client`'s `--list-sessions` (which talks to a running server
/// over the network), plus PID and audio, which are only available here
/// because this runs on the same host as the registry.
fn list_sessions() -> Result<()> {
    let sessions = registry::list_alive();
    if sessions.is_empty() {
        println!("No active sessions.");
        return Ok(());
    }

    let now = registry::now_unix();
    println!(
        "{:<16} {:<16} {:<11} {:>8} {:>10} {:>5}",
        "SESSION ID", "MODE", "SIZE", "AGE", "PID", "AUDIO"
    );
    for s in &sessions {
        println!(
            "{:<16} {:<16} {:<11} {:>8} {:>10} {:>5}",
            s.session_id,
            s.mode,
            format!("{}x{}", s.width, s.height),
            format_age(now.saturating_sub(s.created_at_unix)),
            s.compositor_pid,
            if s.audio { "on" } else { "off" },
        );
    }
    Ok(())
}

/// `--close-session`: terminate the named session(s) and exit. Errors
/// (non-zero exit) if any id doesn't correspond to a live session, rather
/// than silently succeeding on a typo.
fn close_sessions(ids: &[String]) -> Result<()> {
    let mut any_missing = false;
    for id in ids {
        if registry::read(id).is_some() {
            registry::close(id);
            println!("Closed session {id}");
        } else {
            eprintln!("Error: no such session '{id}'");
            any_missing = true;
        }
    }
    if any_missing {
        std::process::exit(1);
    }
    Ok(())
}

/// Same formatting as `termland-client`'s connection.rs::format_age — kept
/// as a tiny local duplicate rather than shared across crates, since it's a
/// two-line display helper, not shared logic.
fn format_age(secs: u64) -> String {
    if secs < 60 { format!("{secs}s") }
    else if secs < 3600 { format!("{}m", secs / 60) }
    else if secs < 86400 { format!("{}h", secs / 3600) }
    else { format!("{}d", secs / 86400) }
}
