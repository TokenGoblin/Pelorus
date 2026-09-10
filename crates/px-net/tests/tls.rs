//! A real TLS handshake, against a local server with a test CA.
//!
//! # Why this file exists
//!
//! The Phase 3 gate fetches 200 URLs over plaintext loopback. That exercises
//! the HTTP parser thoroughly and the TLS path not at all — `px_net::tls` was
//! reviewed code rather than tested code, and the gate report said so. This is
//! the file that changes that.
//!
//! # The test-only trust anchor
//!
//! §14.4: a test-only capability must be behind a feature and must be scanned
//! for in release artifacts, because a flag being off is not evidence. An extra
//! trust anchor is the sharpest example of why — one that reached a shipping
//! binary would be a universal man-in-the-middle, and it would look like
//! nothing at all.
//!
//! So `client_config_trusting` is `#[cfg(feature = "testing")]`, this whole
//! file is behind the same feature, and the certificates live in
//! `tests/net/tls/` rather than anywhere a build could reach them by accident.
//!
//! The CA here signs exactly one name, `localhost`, and expires like any other
//! certificate. It is not a secret — the private key is committed beside it,
//! deliberately, so nobody is tempted to treat it as one.
//!
//! # What is asserted
//!
//! That the handshake *works* is the smaller half. The half that matters is
//! that it **fails** where it should: an untrusted chain and a name mismatch
//! must both be refused, and refused by default rather than by a flag someone
//! could turn off. A TLS client that cannot be shown rejecting a bad
//! certificate has not been shown to verify anything.

#![cfg(feature = "testing")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

fn tls_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("net")
        .join("tls")
}

fn read(name: &str) -> std::io::Result<Vec<u8>> {
    std::fs::read(tls_dir().join(name))
}

/// Connect with deadlines on both directions.
///
/// Without these, the two tests that assert a *refusal* can hang instead of
/// failing: a client that rejects the certificate stops talking, the server is
/// still waiting for a request, and neither side closes. A hung test is a worse
/// failure report than a failed one, and this is how a test suite comes to
/// have a step everyone knows to skip.
fn connect_with_deadline(port: u16) -> std::io::Result<TcpStream> {
    let stream = TcpStream::connect(("127.0.0.1", port))?;
    let deadline = std::time::Duration::from_secs(5);
    stream.set_read_timeout(Some(deadline))?;
    stream.set_write_timeout(Some(deadline))?;
    Ok(stream)
}

/// A TLS server on an ephemeral loopback port, serving one fixed response.
///
/// Returns the port. The listener thread lives as long as the process, which
/// is fine for a test binary and wrong for anything else.
fn start_tls_server() -> std::io::Result<u16> {
    let cert = CertificateDer::from(read("server.der")?);
    let key = PrivateKeyDer::try_from(read("server.key.der")?)
        .map_err(|error| std::io::Error::other(format!("bad test key: {error}")))?;

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(|error| std::io::Error::other(format!("bad test cert: {error}")))?;
    // The client offers http/1.1 only (ADR 015). A server that agreed to
    // anything else would make the ALPN assertion below meaningless.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let config = Arc::new(config);

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let config = Arc::clone(&config);
            std::thread::spawn(move || {
                let Ok(connection) = ServerConnection::new(config) else {
                    return;
                };
                let mut tls = StreamOwned::new(connection, stream);
                // Drain the request line and headers.
                let mut buffer = [0u8; 4096];
                let _ = tls.read(&mut buffer);
                let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello tls!!");
                let _ = tls.flush();
            });
        }
    });

    Ok(port)
}

/// The positive case: a full handshake and a fetched body over TLS.
#[test]
fn tls_handshake_completes_and_the_body_arrives() {
    let port = start_tls_server().expect("the test TLS server must start");
    let ca = read("ca.der").expect("the test CA must be present");

    let config = px_net::tls::client_config_trusting(&ca).expect("a config trusting the test CA");

    let stream = connect_with_deadline(port).expect("connect");
    let mut session = px_net::tls::connect_with("localhost", stream, config)
        .expect("the handshake must complete against a certificate the client trusts");

    session
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write");
    session.flush().expect("flush");

    let mut raw = Vec::new();
    let _ = session.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.contains("hello tls!!"),
        "the body must arrive over TLS; got: {text}"
    );
}

/// The half that matters. An untrusted chain must be refused **by default** —
/// with the ordinary client config, which trusts only the platform store.
#[test]
fn tls_refuses_a_certificate_it_does_not_trust() {
    let port = start_tls_server().expect("server");

    let stream = connect_with_deadline(port).expect("connect");
    let mut session = px_net::tls::connect("localhost", stream)
        .expect("the session object is built before any bytes move");

    // The handshake happens on first use. It must fail: the test CA is not in
    // the platform store, and there is no way to ask this client to skip
    // verification.
    let write = session.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    let mut sink = Vec::new();
    let read = session.read_to_end(&mut sink);

    assert!(
        write.is_err() || read.is_err(),
        "a certificate signed by an untrusted CA must be refused; the exchange \
         completed instead, which means verification did not happen"
    );
}

/// A certificate valid for a different name must be refused, even when the
/// chain itself is trusted. This is the check that stops a valid certificate
/// for `attacker.example` being accepted for `bank.example`.
#[test]
fn tls_refuses_a_name_that_does_not_match_the_certificate() {
    let port = start_tls_server().expect("server");
    let ca = read("ca.der").expect("ca");
    let config = px_net::tls::client_config_trusting(&ca).expect("config");

    let stream = connect_with_deadline(port).expect("connect");
    // The certificate names localhost and 127.0.0.1. Ask for something else.
    let mut session = px_net::tls::connect_with("other.test", stream, config)
        .expect("the session object is built before any bytes move");

    let write = session.write_all(b"GET / HTTP/1.1\r\nHost: other.test\r\n\r\n");
    let mut sink = Vec::new();
    let read = session.read_to_end(&mut sink);

    assert!(
        write.is_err() || read.is_err(),
        "a certificate valid for localhost must not be accepted for other.test"
    );
}

/// ALPN must negotiate http/1.1 and nothing else, because ADR 015 defers
/// HTTP/2 and a connection that agreed to h2 would desynchronise on the first
/// byte.
#[test]
fn tls_negotiates_http_1_1_only() {
    let port = start_tls_server().expect("server");
    let ca = read("ca.der").expect("ca");
    let config = px_net::tls::client_config_trusting(&ca).expect("config");
    assert_eq!(
        config.alpn_protocols,
        vec![b"http/1.1".to_vec()],
        "the client must offer http/1.1 and nothing else"
    );

    let stream = connect_with_deadline(port).expect("connect");
    let mut session = px_net::tls::connect_with("localhost", stream, config).expect("handshake");
    session
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write");
    session.flush().expect("flush");

    let negotiated = session.conn.alpn_protocol().map(<[u8]>::to_vec);
    assert_eq!(
        negotiated,
        Some(b"http/1.1".to_vec()),
        "the negotiated protocol must be http/1.1"
    );
}
