#![forbid(unsafe_code)]

//! The only crate that carries the product name.
//!
//! Every user-visible string that names the product is a constant here, and
//! `tests/brand/gate-brand-leak.sh` fails the build if the literal appears
//! anywhere it is not allowed (build-spec §2.2). The gate reads
//! [`PRODUCT_NAME`] out of this file at runtime rather than hardcoding it, so
//! a rename does not touch the gate.
//!
//! What is deliberately NOT here: the update URL and the release signing key.
//! They are brand-independent and permanent (§14.1). Routing them through this
//! crate would mean a rename changed the update endpoint, and every already
//! installed copy would keep polling the old one — silently ending security
//! updates for exactly the users who can no longer be reached. They live in
//! `px-update`, and `ci/gate-structure.sh` asserts they have not migrated here.

/// The product name as shown to a user.
///
/// Provisional. See `docs/adr/000-name.md` for the surviving alternates and
/// the reservation work due before Phase 3.
pub const PRODUCT_NAME: &str = "Pelorus";

/// Directory name for the profile and configuration directory.
///
/// Resolving this to an absolute path is `px-store`'s job (Phase 13), not this
/// crate's — that needs platform knowledge, and this crate holds strings.
pub const CONFIG_DIR: &str = "pelorus";

/// Configuration directory names used by previous versions of this product.
///
/// `px-store` migrates from these on first run (§2.3). A rename appends the
/// outgoing `CONFIG_DIR` here; entries are never removed, because a removed
/// entry silently orphans the profile of anyone who skipped a release.
///
/// Empty until the first rename. The migration path is written in Phase 0 and
/// exercised against a populated profile by Phase 13's gate, so that the code
/// which runs at a rename is not code first executed at a rename.
pub const LEGACY_CONFIG_DIRS: &[&str] = &[];

/// Scheme for internal pages.
///
/// At a rename, both the new and the legacy scheme are registered for one
/// release cycle (§2.3).
pub const INTERNAL_SCHEME: &str = "pelorus";

/// Internal schemes retired by a rename, still registered for one cycle.
pub const LEGACY_INTERNAL_SCHEMES: &[&str] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_non_empty() {
        assert!(!PRODUCT_NAME.is_empty());
        assert!(!CONFIG_DIR.is_empty());
        assert!(!INTERNAL_SCHEME.is_empty());
    }

    /// A scheme with an uppercase letter or a space is a scheme that will be
    /// mishandled somewhere. Cheap to assert now, expensive to discover later.
    #[test]
    fn schemes_are_lowercase_and_bare() {
        for scheme in core::iter::once(&INTERNAL_SCHEME).chain(LEGACY_INTERNAL_SCHEMES.iter()) {
            assert!(
                scheme
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "scheme {scheme:?} is not bare lowercase ASCII"
            );
        }
    }

    /// §2.3: a rename appends to the legacy list rather than replacing it. If
    /// the current directory ever appears in the legacy list, migration would
    /// read from and write to the same place.
    #[test]
    fn config_dir_is_not_also_a_legacy_dir() {
        assert!(!LEGACY_CONFIG_DIRS.contains(&CONFIG_DIR));
    }
}
