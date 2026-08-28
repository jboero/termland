//! WebTransport (HTTP/3) listener for browser clients.
//!
//! A second listener: browsers cannot speak `--quic` (ALPN `termland/1`).
//! After extended-CONNECT, the control bidi stream is joined into
//! `AsyncRead + AsyncWrite` and handed to the same `handle_session`. Video
//! is Q2 on a server-opened uni stream (`MediaConnection::WebTransport`).

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use wtransport::endpoint::IncomingSession;
use wtransport::tls::{Certificate, Sha256DigestFmt};
use wtransport::{Endpoint, Identity, ServerConfig};

use crate::media::MediaConnection;

const SESSION_PATH: &str = "/termland";

/// Browsers accept `serverCertificateHashes` only for certificates valid no
/// longer than two weeks, so the server's ordinary long-lived self-signed
/// certificate cannot be reused here.
const DEV_CERT_VALIDITY_DAYS: u32 = 13;

/// Fail closed for browsers: an empty allowlist rejects every request that
/// carries an `Origin`. A missing origin is not a browser (native clients and
/// tests omit it; a page cannot suppress it) and is allowed. Without that
/// default, any page on the LAN could open a session — and without `--auth`,
/// create and drive a desktop.
fn origin_allowed(origin: Option<&str>, allowed: &HashSet<String>) -> bool {
    match origin {
        None => true,
        Some(o) => allowed.contains(o),
    }
}

/// Read the `Origin` header without depending on its casing.
///
/// `SessionRequest::origin()` looks up the exact key `"origin"` with no case
/// folding, so `Origin` would look absent — and absent is allowed. HTTP/3
/// names are lowercase in practice, but the check must not depend on that.
fn header_origin(headers: &HashMap<String, String>) -> Option<&str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("origin"))
        .map(|(_, v)| v.as_str())
}

fn path_allowed(path: &str) -> bool {
    let without_query = path.split_once('?').map(|(p, _)| p).unwrap_or(path);
    without_query.trim_end_matches('/') == SESSION_PATH
}

async fn resolve_identity(cert_path: Option<&Path>, key_path: Option<&Path>) -> Result<Identity> {
    if let (Some(cert), Some(key)) = (cert_path, key_path) {
        let identity = Identity::load_pemfiles(cert, key)
            .await
            .with_context(|| format!("loading {} / {}", cert.display(), key.display()))?;
        tracing::info!("WebTransport using certificate {}", cert.display());
        return Ok(identity);
    }

    let identity = Identity::self_signed_builder()
        .subject_alt_names(["localhost", "127.0.0.1", "::1"])
        .from_now_utc()
        .validity_days(DEV_CERT_VALIDITY_DAYS)
        .build()
        .context("generating a development certificate")?;

    print_cert_hash(identity.certificate_chain().as_slice());
    Ok(identity)
}

fn print_cert_hash(chain: &[Certificate]) {
    let Some(leaf) = chain.first() else { return };
    let hex = leaf.hash().fmt(Sha256DigestFmt::DottedHex);
    tracing::info!(
        "WebTransport development certificate (valid {DEV_CERT_VALIDITY_DAYS} days). \
         Pass this to the browser as serverCertificateHashes:\n  {hex}"
    );
}

pub async fn run_webtransport_listener(
    bind: &str,
    port: u16,
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
    allowed_origins: Vec<String>,
    require_auth: bool,
) -> Result<()> {
    let addr: SocketAddr = (bind, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {bind}:{port}"))?
        .next()
        .with_context(|| format!("no address for {bind}:{port}"))?;

    let identity = resolve_identity(cert_path, key_path).await?;
    let config = ServerConfig::builder()
        .with_bind_address(addr)
        .with_identity(identity)
        .keep_alive_interval(Some(Duration::from_secs(10)))
        .build();

    let endpoint = Endpoint::server(config).context("binding the WebTransport endpoint")?;
    let allowed: Arc<HashSet<String>> = Arc::new(allowed_origins.into_iter().collect());

    if allowed.is_empty() {
        tracing::warn!(
            "WebTransport listening on {addr} with no allowed origins — browser \
             requests will be rejected. Pass --webtransport-origin https://your.app \
             to permit one."
        );
    } else {
        tracing::info!(
            "WebTransport listening on {addr} (allowed origins: {})",
            allowed.iter().cloned().collect::<Vec<_>>().join(", "),
        );
    }

    loop {
        let incoming = endpoint.accept().await;
        let allowed = allowed.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_incoming(incoming, allowed, require_auth).await {
                tracing::warn!("WebTransport session ended: {e:#}");
            }
        });
    }
}

async fn handle_incoming(
    incoming: IncomingSession,
    allowed: Arc<HashSet<String>>,
    require_auth: bool,
) -> Result<()> {
    let request = incoming.await.context("WebTransport handshake failed")?;
    let remote = request.remote_address();
    let origin = header_origin(request.headers()).map(str::to_string);

    if !path_allowed(request.path()) {
        tracing::warn!(
            "Rejecting WebTransport session from {remote}: path {:?} is not {SESSION_PATH}",
            request.path(),
        );
        request.not_found().await;
        return Ok(());
    }

    if !origin_allowed(origin.as_deref(), &allowed) {
        tracing::warn!(
            "Rejecting WebTransport session from {remote}: origin {:?} is not allowed",
            origin.as_deref().unwrap_or("<none>"),
        );
        // 403 rather than dropping the connection, so the browser surfaces a
        // real error to the page instead of a generic network failure.
        request.forbidden().await;
        return Ok(());
    }

    tracing::info!(
        "WebTransport session from {remote} (origin {}, path {})",
        origin.as_deref().unwrap_or("<none>"),
        request.path(),
    );

    let connection = request.accept().await.context("accepting the session")?;

    // Same contract as raw QUIC: the client opens exactly one bidirectional
    // stream and it carries the control plane for the whole session.
    let (send, recv) = connection
        .accept_bi()
        .await
        .context("client did not open a control stream")?;

    let io = tokio::io::join(recv, send);
    crate::transport::handle_session(io, require_auth, MediaConnection::WebTransport(connection))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn empty_allowlist_rejects_every_browser_origin() {
        let empty = allowlist(&[]);
        assert!(!origin_allowed(Some("https://evil.example"), &empty));
        assert!(!origin_allowed(Some("https://localhost:8080"), &empty));
        assert!(!origin_allowed(Some("null"), &empty));
    }

    #[test]
    fn requests_without_an_origin_are_allowed() {
        assert!(origin_allowed(None, &allowlist(&[])));
        assert!(origin_allowed(None, &allowlist(&["https://app.example"])));
    }

    #[test]
    fn origin_matching_is_exact() {
        let allowed = allowlist(&["https://app.example", "https://localhost:8443"]);
        assert!(origin_allowed(Some("https://app.example"), &allowed));
        assert!(origin_allowed(Some("https://localhost:8443"), &allowed));
        for hostile in [
            "https://app.example.evil.com",
            "https://notapp.example",
            "https://app.example:8443",
            "https://app.example/",
            "http://app.example",
        ] {
            assert!(
                !origin_allowed(Some(hostile), &allowed),
                "{hostile} was allowed"
            );
        }
    }

    #[test]
    fn origin_header_is_found_whatever_its_casing() {
        for spelling in ["origin", "Origin", "ORIGIN", "OrIgIn"] {
            let h = headers(&[(spelling, "https://app.example")]);
            assert_eq!(header_origin(&h), Some("https://app.example"), "{spelling}");
        }
        assert_eq!(header_origin(&headers(&[("user-agent", "curl")])), None);
        assert_eq!(header_origin(&headers(&[])), None);
    }

    #[test]
    fn literal_null_origin_is_an_origin() {
        assert!(!origin_allowed(Some("null"), &allowlist(&[])));
        assert!(origin_allowed(Some("null"), &allowlist(&["null"])));
    }

    #[test]
    fn control_path_accepts_slash_and_query() {
        assert!(path_allowed("/termland"));
        assert!(path_allowed("/termland/"));
        assert!(path_allowed("/termland?x=1"));
        for hostile in [
            "/",
            "/echo",
            "/termland.evil",
            "/Termland",
            "/termland/extra",
        ] {
            assert!(!path_allowed(hostile), "{hostile} was allowed");
        }
    }
}
