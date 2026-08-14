use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::{Context, Result};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

/// System-wide PKI location, used when running as root (i.e. as the systemd
/// service). Follows the distro convention of `/etc/pki/<app>/` rather than
/// `dirs::config_dir()`, which for `User=root` resolves to `/root/.config` —
/// a path `ProtectHome=` renders read-only for any hardened unit, and not
/// somewhere an administrator would think to look for a server's keypair.
const SYSTEM_PKI_DIR: &str = "/etc/pki/termland";

fn running_as_root() -> bool {
    // SAFETY: geteuid() is always successful and takes no arguments.
    unsafe { libc::geteuid() == 0 }
}

fn config_dir() -> PathBuf {
    if running_as_root() {
        PathBuf::from(SYSTEM_PKI_DIR)
    } else {
        user_config_dir()
    }
}

fn user_config_dir() -> PathBuf {
    if let Some(config) = dirs::config_dir() {
        config.join("termland")
    } else {
        PathBuf::from("/etc/termland")
    }
}

/// Resolve the default cert + key pair together, so the two never come from
/// different directories.
///
/// Root gets `/etc/pki/termland/`; an unprivileged server (SSH-subsystem
/// mode, or someone running it by hand) keeps `~/.config/termland/`.
///
/// Installations predating the `/etc/pki` move generated their keypair under
/// `~/.config/termland/`, which for the service meant `/root/.config/termland/`.
/// If such a pair is still there and the system one is not, keep using it:
/// silently generating a fresh cert would change the fingerprint out from
/// under every client that has already pinned the old one.
fn default_cert_key() -> (PathBuf, PathBuf) {
    let dir = config_dir();
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));

    if running_as_root() && !(cert.exists() && key.exists()) {
        let legacy = user_config_dir();
        let (legacy_cert, legacy_key) = (legacy.join("cert.pem"), legacy.join("key.pem"));
        if legacy_cert.exists() && legacy_key.exists() {
            tracing::warn!(
                "Using legacy TLS keypair in {} — the default is now {}. \
                 Move cert.pem/key.pem there (or set --tls-cert/--tls-key) to \
                 silence this; regenerating instead would change the \
                 certificate fingerprint seen by existing clients.",
                legacy.display(),
                dir.display(),
            );
            return (legacy_cert, legacy_key);
        }
    }

    (cert, key)
}

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
/// `/etc/pki/termland/` (root) or `~/.config/termland/` (unprivileged) and
/// auto-generates a self-signed cert if missing — see `default_cert_key`.
pub fn load_or_generate_cert(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let (default_cert, default_key) = default_cert_key();
    let cert_path = cert_path.map(PathBuf::from).unwrap_or(default_cert);
    let key_path = key_path.map(PathBuf::from).unwrap_or(default_key);

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

/// Create the default keypair if it is missing, without building a rustls
/// config around it. Invoked as `termland-server --generate-cert`, which the
/// RPM's `%post` runs at install time.
///
/// It exists because the long-running service runs under
/// `ProtectSystem=strict` and therefore cannot write `/etc` — the same reason
/// `mod_ssl` generates its certificate from a packaging hook rather than from
/// httpd itself.
///
/// Idempotent: an existing pair is left alone, so `%post` can re-run on
/// upgrade and an administrator can run it by hand to recover a lost keypair.
pub fn generate_if_missing(
    cert_path: Option<&Path>,
    key_path: Option<&Path>,
) -> Result<(PathBuf, PathBuf)> {
    let (default_cert, default_key) = default_cert_key();
    let cert_path = cert_path.map(PathBuf::from).unwrap_or(default_cert);
    let key_path = key_path.map(PathBuf::from).unwrap_or(default_key);

    if cert_path.exists() && key_path.exists() {
        tracing::info!("TLS keypair already present: {}", cert_path.display());
        return Ok((cert_path, key_path));
    }

    generate_self_signed(&cert_path, &key_path)
        .with_context(|| format!("generating self-signed keypair at {}", cert_path.display()))?;

    Ok((cert_path, key_path))
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
