#![no_main]

//! Untrusted update manifest, signature file, request and Ed25519 inputs
//! (Phase 20, §4.3, §10).
//!
//! Input layout: one control byte, then the rest. The same bytes are offered
//! to every parser in px-update. Whatever they are, nothing may panic; a
//! manifest that parses must be a fixed point of encode-then-parse (the parser
//! accepts only the canonical encoding); a signature file that parses must
//! have strictly ascending key ids; and the signature verifier must return
//! `Ok` or `Err` for any split of the bytes into key, signature and message.

use libfuzzer_sys::fuzz_target;
use px_update::ed25519::{PUBLIC_KEY_LEN, SIGNATURE_LEN, check_public_key, verify};
use px_update::manifest::Manifest;
use px_update::request::audit;
use px_update::trust::SignatureSet;
use px_update::version::{Version, date_to_unix};

fuzz_target!(|data: &[u8]| {
    let Some((&control, bytes)) = data.split_first() else {
        return;
    };

    if let Ok(m) = Manifest::parse(bytes) {
        let again = m.encode();
        assert_eq!(again.as_bytes(), bytes, "accepted a non-canonical manifest");
        assert_eq!(Manifest::parse(again.as_bytes()), Ok(m));
    }

    let _ = SignatureSet::parse(bytes);
    let _ = audit(bytes);
    if let Ok(text) = std::str::from_utf8(bytes) {
        if let Some(v) = Version::parse(text) {
            assert_eq!(Version::parse(&v.to_string()), Some(v));
        }
        if let Ok(n) = text.trim().parse::<u32>() {
            let _ = date_to_unix(n);
        }
    }

    // Ed25519: key | signature | message, with the split moved by the control
    // byte so short, exact and over-long inputs are all reached. Only the
    // exact-length case runs the (slow) group arithmetic.
    let key_len = if control & 1 == 0 {
        PUBLIC_KEY_LEN
    } else {
        usize::from(control >> 1) % (PUBLIC_KEY_LEN + 2)
    };
    let (key, rest) = bytes.split_at(key_len.min(bytes.len()));
    let sig_len = if control & 2 == 0 {
        SIGNATURE_LEN
    } else {
        usize::from(control >> 2) % (SIGNATURE_LEN + 2)
    };
    let (sig, message) = rest.split_at(sig_len.min(rest.len()));
    let _ = verify(key, message, sig);
    let _ = check_public_key(key);
});
