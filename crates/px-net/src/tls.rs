//! TLS, and the trust anchors it verifies against (ADR 012, ADR 016).
//!
//! # Fail closed
//!
//! There is no way to disable verification from this module. No
//! `danger_accept_invalid_certs`, no flag, no environment variable. A TLS
//! failure is [`FetchError::Tls`] and the request does not proceed —
//! invariant 8, in the place it matters most.
//!
//! The single exception is a test-only trust anchor for the local server the
//! Phase 3 gate fetches from, and it is behind `#[cfg(feature = "testing")]`
//! so §14.4's release-artifact scan can prove it is absent from a shipping
//! binary. A flag being off is not evidence; absence of the symbol is.
//!
//! # Where the anchors come from
//!
//! ADR 012: the platform store is the source of trust and the bundled store is
//! the floor. Reading the Windows store is unsafe FFI, so it belongs in
//! `px-sandbox` — and until that wrapper exists this module uses the bundled
//! store and **says so**, rather than quietly shipping the floor as though it
//! were the decision.

use std::io::{Read, Write};
use std::sync::Arc;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::fetch::FetchError;

/// Which store the anchors actually came from.
///
/// Returned rather than logged, because ADR 012's verification criterion is
/// that a fallback to the bundled floor must never be silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorSource {
    /// The operating system's store — ADR 012's intended source.
    Platform,
    /// The bundled Mozilla store, because the platform store was unavailable.
    BundledFloor,
}

/// Build the trust anchors, reporting where they came from.
///
/// **Currently always [`AnchorSource::BundledFloor`].** ADR 012 decides that
/// the platform store is the source of trust; reading it needs
/// `CertOpenSystemStoreW` behind a `px-sandbox` wrapper that does not exist
/// yet. Rather than pretend, this reports the floor it is actually using. The
/// consequence is real and worth stating: an enterprise root installed by an
/// administrator is **not** trusted yet, so a machine behind a TLS-terminating
/// gateway cannot reach its internal sites until the wrapper lands.
pub fn root_store() -> (RootCertStore, AnchorSource) {
    let mut store = RootCertStore::empty();
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    (store, AnchorSource::BundledFloor)
}

/// The client configuration: TLS 1.2 and 1.3, ALPN offering HTTP/1.1 only.
pub fn client_config() -> Result<Arc<ClientConfig>, FetchError> {
    let (roots, _source) = root_store();
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    // ADR 015: HTTP/2 is not implemented, so it is not offered. Advertising
    // `h2` and then speaking HTTP/1.1 is how a connection desynchronises on
    // the first byte.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// Open a TLS session to `host` over an established stream.
pub fn connect<S: Read + Write>(
    host: &str,
    stream: S,
) -> Result<StreamOwned<ClientConnection, S>, FetchError> {
    let config = client_config()?;
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|_| FetchError::Tls(format!("{host} is not a valid server name")))?;
    let connection = ClientConnection::new(config, server_name)
        .map_err(|error| FetchError::Tls(error.to_string()))?;
    Ok(StreamOwned::new(connection, stream))
}

/// A client configuration that additionally trusts one test certificate.
///
/// §14.4: test-only capabilities live behind this feature and are scanned for
/// in release artifacts. `ci/gate-network.sh` runs that scan, for the same
/// reason `ci/gate-sandbox.sh` scans for the sandbox override — a trust anchor
/// that reached a shipping binary would be a universal man-in-the-middle.
#[cfg(feature = "testing")]
pub fn client_config_trusting(extra_root_der: &[u8]) -> Result<Arc<ClientConfig>, FetchError> {
    use rustls::pki_types::CertificateDer;

    let (mut roots, _source) = root_store();
    roots
        .add(CertificateDer::from(extra_root_der.to_vec()))
        .map_err(|error| FetchError::Tls(format!("test anchor rejected: {error}")))?;

    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
