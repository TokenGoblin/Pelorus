//! ADR 012's verification criteria, as tests.
//!
//! The ADR names two ways the root-store decision would be wrong, and both are
//! checkable:
//!
//! - a machine with a working platform store verifying against the bundled
//!   floor **without saying so** — a silent downgrade of trust anchors;
//! - the two stores being merged, which would silently restore a root an
//!   administrator removed.
//!
//! The second is the one that cannot be caught by using the product. Nothing
//! observable changes when a distrusted root is quietly trusted again.

use px_net::tls::{AnchorSource, root_store};

/// On a machine whose platform store is readable, that is what gets used.
///
/// The fallback is correct behaviour on a machine without one, so this asserts
/// the *reporting* rather than the outcome: whichever store was used, the
/// source must say which. A silent fallback is the failure ADR 012 most wants
/// to avoid.
#[test]
fn anchors_report_which_store_they_came_from() {
    let (store, source) = root_store();
    assert!(
        !store.is_empty(),
        "an empty trust store would fail every connection with no way to tell \
         'this site is untrustworthy' from 'this machine has no anchors'"
    );

    match source {
        AnchorSource::Platform => {
            // The platform store was readable, so px-sandbox must agree.
            assert!(
                px_sandbox::roots::platform_roots().is_ok(),
                "tls reported Platform while px-sandbox cannot read the store"
            );
        }
        AnchorSource::BundledFloor => {
            // The floor is only correct when the platform store is genuinely
            // unavailable. If px-sandbox can read it, this is the silent
            // downgrade ADR 012 forbids.
            assert!(
                px_sandbox::roots::platform_roots().is_err(),
                "the bundled floor was used on a machine whose platform store \
                 is readable — ADR 012 calls this the failure it most wants to \
                 avoid, and it is silent"
            );
        }
    }
}

/// The stores are not merged.
///
/// Union semantics would mean a root the administrator deliberately *removed*
/// is restored by us. This asserts the sizes are not additive: a merged store
/// would hold at least as many anchors as the platform store plus the bundle.
#[test]
fn anchors_are_not_a_union_of_platform_and_bundle() {
    let (store, source) = root_store();
    let bundled = webpki_roots::TLS_SERVER_ROOTS.len();

    let Ok(platform) = px_sandbox::roots::platform_roots() else {
        // No platform store here; there is nothing to union with, and the
        // fallback case is covered by the test above.
        return;
    };

    if source == AnchorSource::Platform {
        assert!(
            store.len() < platform.len().saturating_add(bundled),
            "the store holds {} anchors against {} platform + {} bundled — \
             that is a union, and a union silently restores roots an \
             administrator removed",
            store.len(),
            platform.len(),
            bundled
        );
    }
}

/// Reading twice gives the same anchors. A trust store that varies between
/// reads cannot be reasoned about.
#[test]
fn anchors_are_stable_across_reads() {
    let (first, first_source) = root_store();
    let (second, second_source) = root_store();
    assert_eq!(first.len(), second.len());
    assert_eq!(first_source, second_source);
}
