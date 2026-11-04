//! The `sandbox` directive (CSP3 §6.3.2, HTML "parse a sandboxing
//! directive").
//!
//! The directive's tokens are ASCII case-insensitive; unknown tokens are
//! ignored. The result is the set of capabilities the sandbox *allows*: with
//! no tokens at all, nothing is allowed. Applying these flags to a browsing
//! context is the embedder's job; this module only computes them.

/// One `allow-*` capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxAllow {
    /// `allow-downloads`
    Downloads,
    /// `allow-forms`
    Forms,
    /// `allow-modals`
    Modals,
    /// `allow-orientation-lock`
    OrientationLock,
    /// `allow-pointer-lock`
    PointerLock,
    /// `allow-popups`
    Popups,
    /// `allow-popups-to-escape-sandbox`
    PopupsToEscapeSandbox,
    /// `allow-presentation`
    Presentation,
    /// `allow-same-origin`
    SameOrigin,
    /// `allow-scripts`
    Scripts,
    /// `allow-storage-access-by-user-activation`
    StorageAccessByUserActivation,
    /// `allow-top-navigation`
    TopNavigation,
    /// `allow-top-navigation-by-user-activation`
    TopNavigationByUserActivation,
    /// `allow-top-navigation-to-custom-protocols`
    TopNavigationToCustomProtocols,
}

const TABLE: [(&str, SandboxAllow); 14] = [
    ("allow-downloads", SandboxAllow::Downloads),
    ("allow-forms", SandboxAllow::Forms),
    ("allow-modals", SandboxAllow::Modals),
    ("allow-orientation-lock", SandboxAllow::OrientationLock),
    ("allow-pointer-lock", SandboxAllow::PointerLock),
    ("allow-popups", SandboxAllow::Popups),
    (
        "allow-popups-to-escape-sandbox",
        SandboxAllow::PopupsToEscapeSandbox,
    ),
    ("allow-presentation", SandboxAllow::Presentation),
    ("allow-same-origin", SandboxAllow::SameOrigin),
    ("allow-scripts", SandboxAllow::Scripts),
    (
        "allow-storage-access-by-user-activation",
        SandboxAllow::StorageAccessByUserActivation,
    ),
    ("allow-top-navigation", SandboxAllow::TopNavigation),
    (
        "allow-top-navigation-by-user-activation",
        SandboxAllow::TopNavigationByUserActivation,
    ),
    (
        "allow-top-navigation-to-custom-protocols",
        SandboxAllow::TopNavigationToCustomProtocols,
    ),
];

impl SandboxAllow {
    fn bit(self) -> u16 {
        // Position in TABLE order; stable because it is derived from the
        // match below, not from a runtime lookup that could fail.
        match self {
            SandboxAllow::Downloads => 1 << 0,
            SandboxAllow::Forms => 1 << 1,
            SandboxAllow::Modals => 1 << 2,
            SandboxAllow::OrientationLock => 1 << 3,
            SandboxAllow::PointerLock => 1 << 4,
            SandboxAllow::Popups => 1 << 5,
            SandboxAllow::PopupsToEscapeSandbox => 1 << 6,
            SandboxAllow::Presentation => 1 << 7,
            SandboxAllow::SameOrigin => 1 << 8,
            SandboxAllow::Scripts => 1 << 9,
            SandboxAllow::StorageAccessByUserActivation => 1 << 10,
            SandboxAllow::TopNavigation => 1 << 11,
            SandboxAllow::TopNavigationByUserActivation => 1 << 12,
            SandboxAllow::TopNavigationToCustomProtocols => 1 << 13,
        }
    }
}

/// The set of capabilities a `sandbox` directive leaves enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SandboxFlags(u16);

impl SandboxFlags {
    /// Compute the allowed set from directive value tokens.
    pub fn from_tokens<S: AsRef<str>>(tokens: &[S]) -> SandboxFlags {
        let mut bits = 0u16;
        for t in tokens {
            let t = t.as_ref();
            if let Some((_, a)) = TABLE.iter().find(|(n, _)| t.eq_ignore_ascii_case(n)) {
                bits |= a.bit();
            }
        }
        SandboxFlags(bits)
    }

    /// Whether the capability is allowed.
    pub fn allows(self, a: SandboxAllow) -> bool {
        self.0 & a.bit() != 0
    }

    /// Whether the sandbox gives the document an opaque origin (i.e.
    /// `allow-same-origin` is absent).
    pub fn forces_opaque_origin(self) -> bool {
        !self.allows(SandboxAllow::SameOrigin)
    }

    /// Whether script may run in the sandboxed document.
    pub fn allows_scripts(self) -> bool {
        self.allows(SandboxAllow::Scripts)
    }

    /// Combine with an `<iframe sandbox>` attribute's flags: a capability
    /// survives only if both allow it (the more restrictive wins).
    pub fn intersect(self, other: SandboxFlags) -> SandboxFlags {
        SandboxFlags(self.0 & other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_directive_allows_nothing() {
        let f = SandboxFlags::from_tokens::<&str>(&[]);
        for (_, a) in TABLE {
            assert!(!f.allows(a), "{a:?}");
        }
        assert!(f.forces_opaque_origin());
        assert!(!f.allows_scripts());
    }

    #[test]
    fn each_token_sets_exactly_its_flag() {
        for (name, a) in TABLE {
            let f = SandboxFlags::from_tokens(&[name.to_ascii_uppercase()]);
            for (_, b) in TABLE {
                assert_eq!(f.allows(b), a == b, "{name} vs {b:?}");
            }
        }
    }

    #[test]
    fn unknown_tokens_ignored() {
        let f =
            SandboxFlags::from_tokens(&["allow-scripts", "allow-everything", "ALLOW-SAME-ORIGIN"]);
        assert!(f.allows_scripts());
        assert!(!f.forces_opaque_origin());
        assert!(!f.allows(SandboxAllow::Forms));
    }

    #[test]
    fn intersect_is_restrictive() {
        let a = SandboxFlags::from_tokens(&["allow-scripts", "allow-forms"]);
        let b = SandboxFlags::from_tokens(&["allow-scripts", "allow-popups"]);
        let c = a.intersect(b);
        assert!(c.allows_scripts());
        assert!(!c.allows(SandboxAllow::Forms));
        assert!(!c.allows(SandboxAllow::Popups));
    }
}
