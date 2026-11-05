//! Test-only: reach the network through an HTTP CONNECT proxy and trust the
//! proxy's CA.
//!
//! Some build and test sandboxes allow outbound HTTPS only through an egress
//! proxy that re-terminates TLS with its own CA. This module lets the live-web
//! checks run there. It exists only under `#[cfg(feature = "testing")]` (§14.4):
//! the proxy is read from `HTTPS_PROXY`, the extra trust anchors from the PEM
//! file named by `PELORUS_TEST_CA_BUNDLE`, and neither is looked at in a
//! shipping build, where this whole module is absent and `cargo xtask network`
//! scans for it. Certificate verification is never switched off: the proxy CA
//! is simply one more anchor, added to the normal store.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

use crate::fetch::FetchError;
use crate::tls;

/// `host:port` of the proxy named by `HTTPS_PROXY`, if one is set.
fn proxy_address() -> Option<String> {
    let raw = std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .ok()?;
    let rest = raw
        .strip_prefix("http://")
        .unwrap_or(raw.as_str())
        .trim_end_matches('/');
    if rest.is_empty() || rest.contains('@') {
        return None;
    }
    Some(rest.to_string())
}

/// Whether a proxy is configured for this process.
pub fn active() -> bool {
    proxy_address().is_some()
}

/// Open a tunnel to `host:port` through the proxy, or `None` if none is set.
pub fn connect(host: &str, port: u16) -> Option<std::io::Result<TcpStream>> {
    let proxy = proxy_address()?;
    Some(tunnel(&proxy, host, port))
}

fn tunnel(proxy: &str, host: &str, port: u16) -> std::io::Result<TcpStream> {
    let bad = |m: &str| std::io::Error::other(m.to_string());
    if host.bytes().any(|b| b < 0x21 || b == 0x7f) {
        return Err(bad("host contains a control character"));
    }
    let mut stream = TcpStream::connect(proxy)?;
    let target = format!("{host}:{port}");
    write!(
        stream,
        "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
    )?;
    stream.flush()?;
    // Read the proxy's answer one byte at a time so nothing of the tunnelled
    // stream is consumed.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(bad("proxy answer too long"));
        }
        if stream.read(&mut byte)? == 0 {
            return Err(bad("proxy closed the connection"));
        }
        head.push(byte.first().copied().unwrap_or(0));
    }
    let line = head.split(|b| *b == b'\r').next().unwrap_or_default();
    let line = String::from_utf8_lossy(line);
    let status = line.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(bad(&format!("proxy refused the tunnel: {line}")));
    }
    Ok(stream)
}

/// The normal trust store plus every certificate in `PELORUS_TEST_CA_BUNDLE`.
pub fn config() -> Result<Arc<ClientConfig>, FetchError> {
    let (mut roots, _source): (RootCertStore, _) = tls::root_store();
    if let Ok(path) = std::env::var("PELORUS_TEST_CA_BUNDLE") {
        let pem = std::fs::read_to_string(&path)
            .map_err(|e| FetchError::Tls(format!("test CA bundle unreadable: {e}")))?;
        for der in pem_certificates(&pem) {
            // A certificate rustls will not take as an anchor is skipped.
            let _ = roots.add(CertificateDer::from(der));
        }
    }
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// DER bodies of the `CERTIFICATE` blocks in a PEM file.
fn pem_certificates(pem: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut body = String::new();
    let mut inside = false;
    for line in pem.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN CERTIFICATE") {
            inside = true;
            body.clear();
        } else if line.starts_with("-----END CERTIFICATE") {
            if inside && let Some(der) = base64(&body) {
                out.push(der);
            }
            inside = false;
        } else if inside {
            body.push_str(line);
        }
    }
    out
}

fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = ((acc << 6) | u32::from(v)) & 0x00ff_ffff;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64("a*"), None);
    }
}
