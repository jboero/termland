//! Disposable browser↔Rust WebTransport echo.
//!
//! This is the interoperability spike that sits *beside* Termland, not inside
//! it: a tiny HTTP/3 WebTransport server that echoes whatever the browser
//! writes on one bidirectional stream. It exists to prove certificate-hash
//! setup, Origin visibility, and that `wtransport` actually talks to Chrome
//! before any Termland framing is involved.
//!
//!     cargo run -p termland-server --example webtransport_echo
//!
//! then open `web/spike/echo.html` (served over http://localhost) and paste
//! the SHA-256 the server prints.

use std::time::Duration;

use anyhow::{Context, Result};
use wtransport::tls::Sha256DigestFmt;
use wtransport::{Endpoint, Identity, ServerConfig};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .init();

    let identity = Identity::self_signed_builder()
        .subject_alt_names(["localhost", "127.0.0.1", "::1"])
        .from_now_utc()
        .validity_days(13)
        .build()
        .context("generating a development certificate")?;

    if let Some(leaf) = identity.certificate_chain().as_slice().first() {
        println!(
            "serverCertificateHashes:\n  {}",
            leaf.hash().fmt(Sha256DigestFmt::DottedHex)
        );
    }

    let config = ServerConfig::builder()
        .with_bind_address("127.0.0.1:4433".parse()?)
        .with_identity(identity)
        .keep_alive_interval(Some(Duration::from_secs(10)))
        .build();

    let endpoint = Endpoint::server(config).context("binding WebTransport echo")?;
    tracing::info!("echo listening on https://127.0.0.1:4433/echo");

    loop {
        let incoming = endpoint.accept().await;
        tokio::spawn(async move {
            if let Err(e) = echo_one(incoming).await {
                tracing::warn!("echo session ended: {e:#}");
            }
        });
    }
}

async fn echo_one(incoming: wtransport::endpoint::IncomingSession) -> Result<()> {
    let request = incoming.await.context("handshake")?;
    let origin = request
        .headers()
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("origin"))
        .map(|(_, v)| v.as_str());
    tracing::info!(
        "session from {} origin {:?} path {}",
        request.remote_address(),
        origin,
        request.path()
    );

    if request.path() != "/echo" && request.path() != "/echo/" {
        request.not_found().await;
        return Ok(());
    }

    let connection = request.accept().await?;
    let (mut send, mut recv) = connection.accept_bi().await?;
    let mut buf = vec![0u8; 4096];
    loop {
        match recv.read(&mut buf).await? {
            Some(n) => send.write_all(&buf[..n]).await?,
            None => break,
        }
    }
    send.finish().await?;
    Ok(())
}
