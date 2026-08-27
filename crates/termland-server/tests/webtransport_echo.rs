//! Standalone WebTransport echo — no Termland protocol, no `termland-server`
//! binary. Proves the `wtransport` stack can complete an HTTP/3 session and
//! round-trip bytes on a client-opened bidi stream, which is the risky
//! dependency the browser path needs before Hello/HelloAck.
//!
//! A browser is still required to prove *Chrome* accepts the certificate
//! hash; that is `web/spike/echo.html` (and `web/test-browser.sh` for the
//! full Hello path). This test is the part that can run here without a
//! browser.

use std::time::Duration;

use anyhow::{Context, Result};
use wtransport::tls::Sha256DigestFmt;
use wtransport::{ClientConfig, Endpoint, Identity, ServerConfig};

#[tokio::test]
async fn wtransport_echoes_hello_bytes() -> Result<()> {
    let identity = Identity::self_signed_builder()
        .subject_alt_names(["localhost", "127.0.0.1"])
        .from_now_utc()
        .validity_days(13)
        .build()
        .context("dev certificate")?;
    let hash = identity
        .certificate_chain()
        .as_slice()
        .first()
        .expect("leaf")
        .hash()
        .fmt(Sha256DigestFmt::DottedHex);
    assert_eq!(
        hash.split(':').count(),
        32,
        "browser-facing hash must be 32 colon-grouped hex bytes, got {hash}"
    );

    let server_config = ServerConfig::builder()
        .with_bind_address("127.0.0.1:0".parse().unwrap())
        .with_identity(identity)
        .keep_alive_interval(Some(Duration::from_secs(5)))
        .build();
    let server = Endpoint::server(server_config).context("bind echo server")?;
    let addr = server.local_addr()?;

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await;
        let request = incoming.await.context("server handshake")?;
        let connection = request.accept().await.context("server accept")?;
        let (mut send, mut recv) = connection.accept_bi().await.context("server bidi")?;
        let mut buf = vec![0u8; 4096];
        let n = recv
            .read(&mut buf)
            .await
            .context("server read")?
            .unwrap_or(0);
        send.write_all(&buf[..n]).await.context("server write")?;
        send.finish().await.ok();
        // Keep the connection alive until the client has read the echo.
        tokio::time::sleep(Duration::from_millis(500)).await;
        anyhow::Ok(())
    });

    // Let the accept task reach `endpoint.accept()` before we dial.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client_config = ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();
    let client = Endpoint::client(client_config).context("client endpoint")?;
    let url = format!("https://127.0.0.1:{}/echo", addr.port());
    let connection = client.connect(&url).await.context("client connect")?;
    let (mut send, mut recv) = connection.open_bi().await?.await?;
    send.write_all(b"Hello").await.context("client write")?;

    let mut buf = vec![0u8; 4096];
    let n = recv
        .read(&mut buf)
        .await
        .context("client read")?
        .unwrap_or(0);
    assert_eq!(&buf[..n], b"Hello", "echo must return the same bytes");
    send.finish().await.ok();

    server_task.await.context("join server task")??;
    Ok(())
}
