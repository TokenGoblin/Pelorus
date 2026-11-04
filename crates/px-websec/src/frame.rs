//! Framing controls: `X-Frame-Options`, the `iframe sandbox` attribute, and
//! the rule that CSP `frame-ancestors` overrides `X-Frame-Options`.
//!
//! Specifications: HTML "check a navigation response's adherence to
//! `X-Frame-Options`", HTML "parse a sandboxing directive", CSP3
//! `frame-ancestors` (which says it supersedes `X-Frame-Options`).
//!
//! The CSP side is owned by `csp`; this module takes its *result* as input
//! ([`FrameAncestors`]) so it has no dependency on how CSP was evaluated.
//!
//! # Deviations
//! - `SAMEORIGIN` is checked against *every* ancestor, not only the parent
//!   (the stricter reading; also what the HTML text does).
//! - `ALLOW-FROM` is obsolete and is an unrecognised value (ignored).
//! - Sandbox token parsing reuses `csp::sandbox::SandboxFlags` (same HTML
//!   algorithm for the CSP directive and the attribute).

use crate::csp::sandbox::SandboxFlags;
use crate::origin::{Origin, trace};

/// Enforcement points of this module.
pub const ENFORCEMENT_POINTS: &[&str] = &[
    "frame::xfo",
    "frame::combine",
    "frame::sandbox_flags",
    "frame::sandboxed_origin",
];

/// Bound on header text examined.
pub const MAX_HEADER_BYTES: usize = 4096;

/// Result of evaluating `X-Frame-Options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XfoVerdict {
    /// Framing allowed (header absent, unrecognised, or satisfied).
    Allowed,
    /// Framing refused.
    Blocked,
}

/// Evaluate `X-Frame-Options`.
///
/// `header_values` is every `X-Frame-Options` header line of the response
/// (each may itself be comma-separated). `response_origin` is the origin of
/// the response URL; `ancestors` are the origins of the framing documents
/// (empty for a top-level navigation, where the header does not apply).
pub fn check_xfo(
    header_values: &[&str],
    response_origin: &Origin,
    ancestors: &[Origin],
) -> XfoVerdict {
    trace::hit("frame::xfo");
    if ancestors.is_empty() {
        return XfoVerdict::Allowed;
    }
    let mut set: Vec<String> = Vec::new();
    for line in header_values {
        if line.len() > MAX_HEADER_BYTES {
            // Oversized input could be hiding a conflict: fail closed.
            return XfoVerdict::Blocked;
        }
        for part in line.split(',') {
            let v = part
                .trim_matches([' ', '\t', '\n', '\r'])
                .to_ascii_lowercase();
            if !set.contains(&v) {
                set.push(v);
            }
        }
    }
    let known = |v: &String| v == "deny" || v == "sameorigin";
    if set.len() > 1 {
        return if set.iter().any(known) {
            XfoVerdict::Blocked
        } else {
            XfoVerdict::Allowed
        };
    }
    match set.first().map(String::as_str) {
        Some("deny") => XfoVerdict::Blocked,
        Some("sameorigin") => {
            if ancestors.iter().all(|a| a.same_origin(response_origin)) {
                XfoVerdict::Allowed
            } else {
                XfoVerdict::Blocked
            }
        }
        _ => XfoVerdict::Allowed,
    }
}

/// An already-evaluated CSP `frame-ancestors` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAncestors {
    /// No enforced policy carries `frame-ancestors`.
    Absent,
    /// A `frame-ancestors` directive is enforced and the embedding passes.
    Allowed,
    /// A `frame-ancestors` directive is enforced and the embedding fails.
    Blocked,
}

/// Combine the two: when `frame-ancestors` is present it alone decides and
/// `X-Frame-Options` is ignored (even if XFO would block, even if
/// `frame-ancestors` allows); otherwise XFO decides.
pub fn embedding_allowed(frame_ancestors: FrameAncestors, xfo: XfoVerdict) -> bool {
    trace::hit("frame::combine");
    match frame_ancestors {
        FrameAncestors::Allowed => true,
        FrameAncestors::Blocked => false,
        FrameAncestors::Absent => xfo == XfoVerdict::Allowed,
    }
}

/// Parse an `<iframe sandbox>` attribute. `None` (attribute absent) means
/// not sandboxed; `Some(flags)` lists what the sandbox still allows (an empty
/// attribute allows nothing). Tokens are space-separated and ASCII
/// case-insensitive; unknown tokens are ignored.
pub fn parse_sandbox_attribute(value: Option<&str>) -> Option<SandboxFlags> {
    trace::hit("frame::sandbox_flags");
    let v = value?;
    let v = v.get(..v.len().min(MAX_HEADER_BYTES * 4)).unwrap_or("");
    let tokens: Vec<&str> = v
        .split([' ', '\t', '\n', '\r', '\u{c}'])
        .filter(|t| !t.is_empty())
        .collect();
    Some(SandboxFlags::from_tokens(&tokens))
}

/// The origin a sandboxed document gets: opaque unless `allow-same-origin`.
/// `None` flags (not sandboxed) keeps `origin`.
pub fn sandboxed_origin(flags: Option<SandboxFlags>, origin: &Origin) -> Origin {
    trace::hit("frame::sandboxed_origin");
    match flags {
        Some(f) if f.forces_opaque_origin() => Origin::new_opaque(),
        _ => origin.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csp::sandbox::SandboxAllow;
    use crate::origin::Url;

    fn o(s: &str) -> Origin {
        Origin::from_url(&Url::parse(s).unwrap())
    }

    fn x(vals: &[&str], resp: &str, anc: &[&str]) -> XfoVerdict {
        let anc: Vec<Origin> = anc.iter().map(|a| o(a)).collect();
        check_xfo(vals, &o(resp), &anc)
    }

    #[test]
    fn deny_and_sameorigin() {
        let r = "https://a.example/page";
        assert_eq!(x(&["DENY"], r, &["https://a.example/"]), XfoVerdict::Blocked);
        assert_eq!(x(&["deny"], r, &["https://b.example/"]), XfoVerdict::Blocked);
        assert_eq!(x(&["SAMEORIGIN"], r, &["https://a.example/"]), XfoVerdict::Allowed);
        assert_eq!(x(&["sameorigin"], r, &["https://b.example/"]), XfoVerdict::Blocked);
        assert_eq!(x(&[" SameOrigin "], r, &["https://a.example:443/"]), XfoVerdict::Allowed);
        // Scheme and port matter.
        assert_eq!(x(&["SAMEORIGIN"], r, &["http://a.example/"]), XfoVerdict::Blocked);
        assert_eq!(x(&["SAMEORIGIN"], r, &["https://a.example:8443/"]), XfoVerdict::Blocked);
    }

    #[test]
    fn sameorigin_checks_every_ancestor() {
        let r = "https://a.example/page";
        assert_eq!(
            x(&["SAMEORIGIN"], r, &["https://a.example/", "https://evil.example/"]),
            XfoVerdict::Blocked
        );
        assert_eq!(
            x(&["SAMEORIGIN"], r, &["https://a.example/", "https://a.example/x"]),
            XfoVerdict::Allowed
        );
    }

    #[test]
    fn top_level_navigation_is_unaffected() {
        assert_eq!(x(&["DENY"], "https://a.example/", &[]), XfoVerdict::Allowed);
    }

    #[test]
    fn absent_and_unrecognised_allow() {
        let a = &["https://b.example/"];
        let r = "https://a.example/";
        assert_eq!(x(&[], r, a), XfoVerdict::Allowed);
        assert_eq!(x(&["ALLOW-FROM https://b.example"], r, a), XfoVerdict::Allowed);
        assert_eq!(x(&["bogus"], r, a), XfoVerdict::Allowed);
        assert_eq!(x(&[""], r, a), XfoVerdict::Allowed);
    }

    #[test]
    fn multiple_values_and_conflicts() {
        let a = &["https://a.example/"];
        let r = "https://a.example/";
        // Duplicates collapse.
        assert_eq!(x(&["DENY", "deny"], r, a), XfoVerdict::Blocked);
        assert_eq!(x(&["SAMEORIGIN, sameorigin"], r, a), XfoVerdict::Allowed);
        // Conflict between known values blocks.
        assert_eq!(x(&["SAMEORIGIN", "DENY"], r, a), XfoVerdict::Blocked);
        assert_eq!(x(&["SAMEORIGIN, DENY"], r, a), XfoVerdict::Blocked);
        // A known value plus garbage still blocks.
        assert_eq!(x(&["bogus, deny"], r, a), XfoVerdict::Blocked);
        assert_eq!(x(&["sameorigin", "bogus"], r, a), XfoVerdict::Blocked);
        // Only garbage: allowed.
        assert_eq!(x(&["a, b"], r, a), XfoVerdict::Allowed);
    }

    #[test]
    fn oversized_header_fails_closed() {
        let big = "x".repeat(MAX_HEADER_BYTES + 1);
        assert_eq!(x(&[&big], "https://a.example/", &["https://b.example/"]), XfoVerdict::Blocked);
    }

    #[test]
    fn opaque_ancestor_is_never_same_origin() {
        let r = o("https://a.example/");
        let anc = [Origin::new_opaque()];
        assert_eq!(check_xfo(&["SAMEORIGIN"], &r, &anc), XfoVerdict::Blocked);
    }

    #[test]
    fn frame_ancestors_overrides_xfo() {
        use FrameAncestors as F;
        use XfoVerdict as V;
        assert!(embedding_allowed(F::Allowed, V::Blocked), "FA allow beats XFO deny");
        assert!(!embedding_allowed(F::Blocked, V::Allowed), "FA block beats XFO allow");
        assert!(!embedding_allowed(F::Blocked, V::Blocked));
        assert!(embedding_allowed(F::Allowed, V::Allowed));
        assert!(embedding_allowed(F::Absent, V::Allowed));
        assert!(!embedding_allowed(F::Absent, V::Blocked));
    }

    #[test]
    fn sandbox_attribute() {
        assert_eq!(parse_sandbox_attribute(None), None);
        let none = parse_sandbox_attribute(Some("")).unwrap();
        assert!(!none.allows_scripts() && none.forces_opaque_origin());
        let f = parse_sandbox_attribute(Some("  allow-scripts\tALLOW-FORMS bogus allow-same-origin\n")).unwrap();
        assert!(f.allows_scripts());
        assert!(f.allows(SandboxAllow::Forms));
        assert!(f.allows(SandboxAllow::SameOrigin));
        assert!(!f.allows(SandboxAllow::Popups));
        assert!(!f.forces_opaque_origin());
        // Commas do not separate tokens.
        let c = parse_sandbox_attribute(Some("allow-scripts,allow-forms")).unwrap();
        assert!(!c.allows_scripts() && !c.allows(SandboxAllow::Forms));
    }

    #[test]
    fn sandbox_origin_derivation() {
        let a = o("https://a.example/");
        assert_eq!(sandboxed_origin(None, &a), a);
        let s = sandboxed_origin(parse_sandbox_attribute(Some("allow-scripts")), &a);
        assert!(s.is_opaque() && !s.same_origin(&a));
        let keep = sandboxed_origin(parse_sandbox_attribute(Some("allow-scripts allow-same-origin")), &a);
        assert_eq!(keep, a);
    }

    #[test]
    fn every_enforcement_point_is_reached() {
        trace::reset();
        let a = o("https://a.example/");
        let _ = check_xfo(&["DENY"], &a, std::slice::from_ref(&a));
        let _ = embedding_allowed(FrameAncestors::Absent, XfoVerdict::Allowed);
        let f = parse_sandbox_attribute(Some(""));
        let _ = sandboxed_origin(f, &a);
        trace::assert_all_hit(ENFORCEMENT_POINTS);
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut s: u64 = 0x1357_9BDF_2468_ACE0;
        let alphabet = b"denysameorigin,- \t\"allow-scripts\xff\xc3";
        let a = o("https://a.example/");
        for _ in 0..10_000 {
            let mut v = String::new();
            for _ in 0..(s % 120) {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                v.push(alphabet[(s % alphabet.len() as u64) as usize] as char);
            }
            let _ = check_xfo(&[&v], &a, std::slice::from_ref(&a));
            let f = parse_sandbox_attribute(Some(&v));
            let _ = sandboxed_origin(f, &a);
        }
    }
}
