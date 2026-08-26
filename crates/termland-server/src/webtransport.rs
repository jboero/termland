//! WebTransport (HTTP/3) listener, for browser clients.
//!
//! # Why this is not just `--quic`
//!
//! A browser cannot speak to the raw QUIC listener in `quic.rs`. That endpoint
//! negotiates ALPN `termland/1` and starts exchanging Termland's own framing
//! immediately. Browser WebTransport is a *session* layered on HTTP/3: ALPN
//! `h3`, an extended-CONNECT request carrying `:authority`/`:path`/`Origin`,
//! and only then a bidirectional stream. There is no browser API that opens a
//! bare QUIC connection, so this is a second listener rather than a change to
//! the first — `--quic` and its Android client keep working untouched.
//!
//! # What is reused
//!
//! Everything above the transport. Once the session is established and the
//! client opens its control stream, the two halves are joined into one
//! `AsyncRead + AsyncWrite` and handed to the same `handle_session` that TCP,
//! TLS, the SSH subsystem and raw QUIC all use. Hello, PAM auth, the session
//! list/create/attach/close lifecycle, input, clipboard and codec negotiation
//! are the existing implementations, not parallel ones.
//!
//! # Scope of this first pass
//!
//! `handle_session` is called with `None` for the QUIC connection, so video
//! and audio travel as CBOR messages on the control stream — the pre-Q2
//! arrangement that TCP still uses. Splitting them onto a WebTransport uni
//! stream and datagrams needs the `Option<quinn::Connection>` coupling in
//! `run_session` generalised first, which is a larger change and deliberately
//! not attempted here.

use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use wtransport::endpoint::IncomingSession;
use wtransport::tls::{Certificate, Sha256DigestFmt};
use wtransport::{Endpoint, Identity, ServerConfig};

/// How long a generated development certificate is valid.
///
/// Browsers accept `serverCertificateHashes` only for certificates valid no
/// longer than two weeks, so this is a protocol limit rather than a
/// preference. It is also why the server's ordinary long-lived self-signed
/// certificate cannot be reused for the browser path.
const DEV_CERT_VALIDITY_DAYS: u32 = 13;

/// Decide whether a session request's `Origin` may proceed.
///
/// Fails closed for browsers. An empty allowlist rejects every request that
/// carries an `Origin` header, which is every browser request — without this,
/// any web page the user happens to visit could open a WebTransport session
/// to a Termland server on their LAN and, when the server runs without PAM
/// auth, create and drive a desktop session on it.
///
/// A request with *no* `Origin` is not a browser: native clients and tests do
/// not send one, and a hostile page cannot suppress it. Those are allowed, so
/// this check adds nothing for non-browser callers.
fn origin_allowed(origin: Option<&str>, allowed: &HashSet<String>) -> bool {
    match origin {
        None => true,
        Some(o) => allowed.contains(o),
    }
}

/// Read the `Origin` header without depending on its casing.
///
/// `SessionRequest::origin()` looks up the exact key `"origin"` in a map that
/// does no case folding, so a request spelling it `Origin` reads back as
/// absent — and absent means "not a browser", which this module allows. A
/// browser cannot exploit that (HTTP/3 header names are lowercase, and the
/// browser, not the page, writes them), but a check whose correctness rests on
/// the peer's good manners is not much of a check.
fn header_origin(headers: &HashMap<String, String>) -> Option<&str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("origin"))
        .map(|(_, v)| v.as_str())
}

/// Load the configured certificate, or generate a short-lived development one
/// and print the hash a browser needs to trust it.
async fn resolve_identity(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<Identity> {
    if let (Some(cert), Some(key)) = (cert_path, key_path) {
        let identity = Identity::load_pemfiles(cert, key)
            .await
            .with_context(|| format!("loading {} / {}", cert.display(), key.display()))?;
        tracing::info!("WebTransport using certificate {}", cert.display());
        return Ok(identity);
    }

    // No certificate given: generate one scoped to this run. Deliberately not
    // the server's usual keypair from tls.rs — that one is long-lived and
    // therefore unusable with serverCertificateHashes, and reusing it here
    // would silently produce a certificate no browser will accept.
    let identity = Identity::self_signed_builder()
        .subject_alt_names(["localhost", "127.0.0.1", "::1"])
        .from_now_utc()
        .validity_days(DEV_CERT_VALIDITY_DAYS)
        .build()
        .context("generating a development certificate")?;

    print_cert_hash(identity.certificate_chain().as_slice());
    Ok(identity)
}

/// Print the SHA-256 digest a browser passes as `serverCertificateHashes`.
fn print_cert_hash(chain: &[Certificate]) {
    let Some(leaf) = chain.first() else { return };
    let hex = leaf.hash().fmt(Sha256DigestFmt::BytesArray);
    tracing::info!(
        "WebTransport development certificate (valid {DEV_CERT_VALIDITY_DAYS} days). \
         Pass this to the browser as serverCertificateHashes:\n  {hex}"
    );
}

/// Serve WebTransport sessions until cancelled.
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

/// Take one session from HTTP/3 request through to `handle_session`.
async fn handle_incoming(
    incoming: IncomingSession,
    allowed: Arc<HashSet<String>>,
    require_auth: bool,
) -> Result<()> {
    let request = incoming.await.context("WebTransport handshake failed")?;
    let remote = request.remote_address();
    let origin = header_origin(request.headers()).map(str::to_string);

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
    crate::transport::handle_session(io, require_auth, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    /// The property that matters: with nothing configured, no browser gets in.
    /// A default-open allowlist would let any page a user visits reach a
    /// Termland server on their network.
    #[test]
    fn empty_allowlist_rejects_every_browser_origin() {
        let empty = allowlist(&[]);
        assert!(!origin_allowed(Some("https://evil.example"), &empty));
        assert!(!origin_allowed(Some("https://localhost:8080"), &empty));
        assert!(!origin_allowed(Some("null"), &empty));
    }

    /// Native clients and tests send no Origin, and a hostile page cannot
    /// suppress one, so absence is not a way past the check.
    #[test]
    fn requests_without_an_origin_are_allowed() {
        assert!(origin_allowed(None, &allowlist(&[])));
        assert!(origin_allowed(None, &allowlist(&["https://app.example"])));
    }

    #[test]
    fn configured_origins_are_allowed_and_others_are_not() {
        let allowed = allowlist(&["https://app.example", "https://localhost:8443"]);
        assert!(origin_allowed(Some("https://app.example"), &allowed));
        assert!(origin_allowed(Some("https://localhost:8443"), &allowed));
        assert!(!origin_allowed(Some("https://app.example.evil"), &allowed));
        assert!(!origin_allowed(Some("http://app.example"), &allowed));
    }

    /// Origins are compared exactly. A prefix or suffix match would let
    /// `https://app.example.evil.com` through on an allowlist containing
    /// `https://app.example`.
    #[test]
    fn origin_matching_is_exact_not_substring() {
        let allowed = allowlist(&["https://app.example"]);
        for hostile in [
            "https://app.example.evil.com",
            "https://notapp.example",
            "https://app.example:8443",
            "https://app.example/",
        ] {
            assert!(!origin_allowed(Some(hostile), &allowed), "{hostile} was allowed");
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The header map does no case folding, so this must. Reading `Origin` as
    /// absent would classify a browser request as a native one and let it
    /// straight past the allowlist.
    #[test]
    fn origin_header_is_found_whatever_its_casing() {
        for spelling in ["origin", "Origin", "ORIGIN", "OrIgIn"] {
            let h = headers(&[(spelling, "https://app.example")]);
            assert_eq!(
                header_origin(&h),
                Some("https://app.example"),
                "{spelling} was not recognised as the Origin header",
            );
        }
    }

    #[test]
    fn absent_origin_header_reads_as_none() {
        assert_eq!(header_origin(&headers(&[("user-agent", "curl")])), None);
        assert_eq!(header_origin(&headers(&[])), None);
    }

    /// A sandboxed iframe or a `file://` page sends the literal string "null".
    /// It must not be treated as "no origin".
    #[test]
    fn literal_null_origin_is_treated_as_an_origin() {
        assert!(!origin_allowed(Some("null"), &allowlist(&[])));
        assert!(origin_allowed(Some("null"), &allowlist(&["null"])));
    }
}
