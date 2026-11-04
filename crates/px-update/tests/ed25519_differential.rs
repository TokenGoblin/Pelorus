//! Differential test: px-update's verifier must agree with `ring` on every
//! case in the fixture (honest signatures and randomly corrupted ones).
//!
//! The fixture lines are `public_key message signature expected`, all hex
//! (message `-` for empty; expected `1` means ring accepted). The committed
//! fixture is a 100-case sample (50 accepted, 50 rejected: the shortest of
//! 4000 cases generated once in a throwaway project outside the repo with
//! ring 0.17.14, never added to Cargo.lock). Point `ED25519_DIFF_FILE` at a
//! full set to rerun the whole comparison.

use px_update::ed25519::verify;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0))
        .collect()
}

#[test]
fn agrees_with_ring_on_every_fixture_case() {
    let text = match std::env::var("ED25519_DIFF_FILE") {
        Ok(p) => std::fs::read_to_string(p).unwrap(),
        Err(_) => include_str!("fixtures/ed25519_ring_sample.txt").to_string(),
    };
    let (mut agree, mut accepted, mut rejected) = (0usize, 0usize, 0usize);
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let f: Vec<&str> = line.split(' ').collect();
        assert_eq!(f.len(), 4, "malformed fixture line");
        let msg = if f[1] == "-" { vec![] } else { unhex(f[1]) };
        let ours = verify(&unhex(f[0]), &msg, &unhex(f[2])).is_ok();
        let ring = f[3] == "1";
        assert_eq!(ours, ring, "disagreement on {line}");
        agree += 1;
        if ring {
            accepted += 1;
        } else {
            rejected += 1;
        }
    }
    println!("agreement: {agree} cases ({accepted} accepted, {rejected} rejected)");
    assert!(agree >= 100 && accepted > 0 && rejected > 0);
}
