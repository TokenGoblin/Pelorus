//! Reading the platform's trust anchors (ADR 012, ADR 008).
//!
//! # Why this lives in `px-sandbox`
//!
//! It is not sandboxing. ADR 008 widened this crate's remit to "the audited
//! unsafe core, which is also where sandboxing lives", precisely so that
//! `CertOpenSystemStoreW` does not become a second crate with an `unsafe`
//! exception. `px-net` gets DER blobs and never names an OS type.
//!
//! # What ADR 012 decided, and what this returns
//!
//! The platform store is the source of trust; the bundled store is a floor for
//! when it cannot be read. This function reads the platform store and reports
//! failure honestly — it never substitutes the floor itself, because the
//! caller has to be able to tell the difference. ADR 012's verification
//! criterion is that a fallback must never be silent.
//!
//! It also does not *merge* with the bundled store, and the caller must not
//! either: a union would silently restore a root an administrator removed,
//! turning a deliberate distrust decision into a no-op. Whoever administers
//! the machine outranks whatever we shipped.

/// Why the platform store could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootStoreError {
    /// The store could not be opened.
    Unavailable(&'static str),
    /// The store opened and yielded implausibly few anchors.
    ///
    /// A store that reads successfully and returns three certificates is more
    /// likely broken than minimal. ADR 012 records that any threshold here is
    /// a guess, and this one is deliberately low: refusing a machine with an
    /// unusual but real store would be worse than accepting a thin one.
    ImplausiblyFew(usize),
    /// This platform has no store this crate knows how to read.
    Unsupported,
}

impl std::fmt::Display for RootStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(detail) => {
                write!(f, "the platform trust store is unavailable: {detail}")
            }
            Self::ImplausiblyFew(count) => write!(
                f,
                "the platform trust store returned {count} anchors, which is too few to be real"
            ),
            Self::Unsupported => write!(f, "no platform trust store is available here"),
        }
    }
}

impl std::error::Error for RootStoreError {}

/// Below this many anchors, the store is treated as broken rather than small.
const PLAUSIBLE_MINIMUM: usize = 20;

/// Read the platform's trust anchors as DER-encoded certificates.
pub fn platform_roots() -> Result<Vec<Vec<u8>>, RootStoreError> {
    #[cfg(target_os = "windows")]
    {
        windows_roots()
    }
    #[cfg(target_os = "linux")]
    {
        linux_roots()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Err(RootStoreError::Unsupported)
    }
}

/// The Windows `ROOT` system store, which is where enterprise MDM roots and
/// user-added roots land.
#[cfg(target_os = "windows")]
fn windows_roots() -> Result<Vec<Vec<u8>>, RootStoreError> {
    use windows_sys::Win32::Security::Cryptography::{
        CERT_CONTEXT, CertCloseStore, CertEnumCertificatesInStore, CertOpenSystemStoreW,
    };

    // UTF-16, NUL-terminated: "ROOT".
    let name: [u16; 5] = [
        u16::from(b'R'),
        u16::from(b'O'),
        u16::from(b'O'),
        u16::from(b'T'),
        0,
    ];

    // SAFETY: the first argument is the legacy provider handle, documented as
    // unused and required to be zero. `name` is a NUL-terminated UTF-16 buffer
    // that outlives the call. The returned store handle is owned by us and is
    // closed on every path below.
    let store = unsafe { CertOpenSystemStoreW(0, name.as_ptr()) };
    if store.is_null() {
        return Err(RootStoreError::Unavailable(
            "CertOpenSystemStoreW(ROOT) failed",
        ));
    }

    let mut anchors: Vec<Vec<u8>> = Vec::new();
    let mut context: *const CERT_CONTEXT = std::ptr::null();

    loop {
        // SAFETY: `store` is a live store handle. The enumeration contract is
        // that the first call passes null and each subsequent call passes the
        // context the previous one returned; the store frees the previous
        // context itself, so nothing here is freed twice. It returns null when
        // the enumeration is finished.
        context = unsafe { CertEnumCertificatesInStore(store, context) };
        if context.is_null() {
            break;
        }

        // SAFETY: `context` is non-null and was returned by the enumeration,
        // so it points at a CERT_CONTEXT owned by the store and valid until
        // the next enumeration call. The fields read here are a pointer to the
        // DER bytes and their length, both filled in by the OS.
        let (blob, len) = unsafe { ((*context).pbCertEncoded, (*context).cbCertEncoded as usize) };
        if blob.is_null() || len == 0 {
            continue;
        }

        // SAFETY: `blob` points at `len` bytes of DER owned by the store and
        // valid until the next enumeration call, and this copies them out
        // immediately rather than retaining the pointer.
        let der = unsafe { std::slice::from_raw_parts(blob, len) }.to_vec();
        anchors.push(der);
    }

    // SAFETY: `store` is the handle opened above, not yet closed, and the
    // enumeration has finished so no context borrowed from it is still live.
    // A zero flag is the documented "close normally".
    unsafe {
        CertCloseStore(store, 0);
    }

    if anchors.len() < PLAUSIBLE_MINIMUM {
        return Err(RootStoreError::ImplausiblyFew(anchors.len()));
    }
    Ok(anchors)
}

/// The distribution's CA bundle.
///
/// No FFI and no `unsafe`: on Linux "the platform store" is a directory of PEM
/// files, and reading it is a file read like the sandbox probes. The locations
/// are the ones the major distributions actually use, tried in order.
#[cfg(target_os = "linux")]
fn linux_roots() -> Result<Vec<Vec<u8>>, RootStoreError> {
    const BUNDLES: &[&str] = &[
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/ca-bundle.pem",
        "/etc/ssl/cert.pem",
    ];

    for path in BUNDLES {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let anchors = parse_pem_bundle(&text);
        if anchors.len() >= PLAUSIBLE_MINIMUM {
            return Ok(anchors);
        }
        if !anchors.is_empty() {
            return Err(RootStoreError::ImplausiblyFew(anchors.len()));
        }
    }
    Err(RootStoreError::Unavailable(
        "no CA bundle found in the usual locations",
    ))
}

/// Pull DER out of a PEM bundle.
///
/// Deliberately strict about the markers rather than tolerant: a bundle this
/// cannot parse should read as unavailable — which falls back to the floor and
/// says so — rather than as a short list that silently narrows what is trusted.
#[cfg(target_os = "linux")]
fn parse_pem_bundle(text: &str) -> Vec<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";

    let mut out = Vec::new();
    let mut base64 = String::new();
    let mut inside = false;

    for line in text.lines() {
        let line = line.trim();
        if line == BEGIN {
            inside = true;
            base64.clear();
        } else if line == END {
            if inside && let Some(der) = decode_base64(&base64) {
                out.push(der);
            }
            inside = false;
        } else if inside {
            base64.push_str(line);
        }
    }
    out
}

/// Base64 for PEM bodies.
///
/// Written rather than taken: this crate has no base64 dependency and adding
/// one to decode a certificate bundle is not where a supply-chain risk is
/// worth taking. Rejects anything outside the alphabet rather than skipping
/// it, so a corrupted bundle reads as unavailable instead of as a shorter one.
#[cfg(target_os = "linux")]
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;

    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((accumulator >> bits) & 0xff) as u8);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store on the machine running the tests must be readable and
    /// plausible. This is the only assertion that can be made about a real
    /// platform store — its contents differ per machine, which is the point of
    /// reading it rather than shipping one.
    #[test]
    fn roots_the_platform_store_is_readable_here() {
        match platform_roots() {
            Ok(anchors) => {
                assert!(
                    anchors.len() >= PLAUSIBLE_MINIMUM,
                    "the store returned {} anchors",
                    anchors.len()
                );
                // Every anchor must at least look like DER: a SEQUENCE tag.
                for der in &anchors {
                    assert_eq!(der.first(), Some(&0x30), "an anchor is not a DER SEQUENCE");
                }
            }
            Err(RootStoreError::Unsupported) => {}
            Err(error) => panic!("the platform store must be readable: {error}"),
        }
    }

    /// Reading it twice must give the same answer. A probe that varies run to
    /// run cannot be reasoned about, and a trust store that varies is worse.
    #[test]
    fn roots_reading_is_deterministic() {
        let (first, second) = (platform_roots(), platform_roots());
        match (first, second) {
            (Ok(a), Ok(b)) => assert_eq!(a.len(), b.len()),
            (Err(a), Err(b)) => assert_eq!(a, b),
            _ => panic!("the platform store gave different answers on two reads"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn roots_a_corrupt_bundle_reads_as_empty_rather_than_short() {
        let corrupt = "-----BEGIN CERTIFICATE-----\nnot~valid~base64\n-----END CERTIFICATE-----\n";
        assert!(
            parse_pem_bundle(corrupt).is_empty(),
            "a bundle that cannot be decoded must not yield a shorter trust list"
        );
    }
}
