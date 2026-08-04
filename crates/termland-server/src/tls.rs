use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::{Context, Result};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

fn config_dir() -> PathBuf {
    dirs_or_default("termland")
}

fn dirs_or_default(name: &str) -> PathBuf {
    if let Some(config) = dirs::config_dir() {
        config.join(name)
    } else {
        PathBuf::from(format!("/etc/{name}"))
    }
}

fn default_cert_path() -> PathBuf { config_dir().join("cert.pem") }
fn default_key_path() -> PathBuf { config_dir().join("key.pem") }

/// The crypto backend both the TCP+TLS acceptor and the QUIC listener
/// (`crate::quic`) build their rustls configs against. Selected explicitly
/// (rather than relying on rustls's "exactly one provider feature enabled"
/// auto-detection) because a full-workspace build unifies this crate's
/// `aws-lc-rs`-default `rustls` dependency with `termland-mobile-core`'s
/// explicit `ring` pin, leaving *both* compiled in — at which point
/// `ServerConfig::builder()`'s implicit provider lookup is ambiguous and
/// panics at runtime. `builder_with_provider` sidesteps that entirely.
/// `aws-lc-rs` is picked to match what `termland-client` already uses
/// explicitly for its certificate verifier.
fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// Load (or generate, if missing) the server's TLS certificate + private key,
/// parsed and ready to hand to an rustls `ServerConfig`. Shared by the
/// TCP+TLS acceptor below and `crate::quic`'s QUIC listener so there is one
/// place that knows how to find/create the cert, not two.
///
/// If `cert_path`/`key_path` are provided, loads those. Otherwise looks in
/// `~/.config/termland/` and auto-generates a self-signed cert if missing.
pub fn load_or_generate_cert(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let cert_path = cert_path.map(PathBuf::from).unwrap_or_else(default_cert_path);
    let key_path = key_path.map(PathBuf::from).unwrap_or_else(default_key_path);

    if !cert_path.exists() || !key_path.exists() {
        tracing::info!("No TLS certificate found, generating self-signed...");
        generate_self_signed(&cert_path, &key_path)
            .context("failed to generate self-signed certificate")?;
    }

    let cert_pem = std::fs::read(&cert_path)
        .with_context(|| format!("reading {}", cert_path.display()))?;
    let key_pem = std::fs::read(&key_path)
        .with_context(|| format!("reading {}", key_path.display()))?;

    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parsing certificate PEM")?;

    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .context("parsing private key PEM")?
        .context("no private key found in PEM")?;

    tracing::info!("TLS configured with {}", cert_path.display());
    Ok((certs, key))
}

/// Load or generate a TLS server configuration for the TCP+TLS acceptor.
pub fn build_tls_acceptor(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<TlsAcceptor> {
    let (certs, key) = load_or_generate_cert(cert_path, key_path)?;

    let config = ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .context("unsupported TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building TLS server config")?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Build a plain rustls `ServerConfig` (not yet wrapped for QUIC) using the
/// same cert loading and crypto provider as `build_tls_acceptor`. Used by
/// `crate::quic` to construct quinn's `ServerConfig` around.
pub fn build_rustls_server_config(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<ServerConfig> {
    let (certs, key) = load_or_generate_cert(cert_path, key_path)?;

    ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()
        .context("unsupported TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building TLS server config")
}

fn generate_self_signed(cert_path: &Path, key_path: &Path) -> Result<()> {
    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut params = rcgen::CertificateParams::new(vec![
        "localhost".to_string(),
    ])?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String("Termland Server".into()),
    );
    params.subject_alt_names = vec![
        rcgen::SanType::DnsName("localhost".try_into()?),
        rcgen::SanType::IpAddress(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        rcgen::SanType::IpAddress(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
    ];

    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    std::fs::write(cert_path, cert.pem())?;
    std::fs::write(key_path, key_pair.serialize_pem())?;

    // Restrictive permissions on the key
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
    }

    tracing::info!("Generated self-signed certificate: {}", cert_path.display());
    tracing::info!("Private key: {}", key_path.display());
    Ok(())
}

/// Helper: get the config directory, falling back for non-home environments.
mod dirs {
    use std::path::PathBuf;
    pub fn config_dir() -> Option<PathBuf> {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
            })
    }
}
