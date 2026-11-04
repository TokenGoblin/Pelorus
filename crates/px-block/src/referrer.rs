//! Referrer trimming for third-party requests.
//!
//! The referrer *computation* is px-websec's (`referrer::compute`); this
//! module only decides how much stricter than the page's own policy a
//! cross-site request should be, and never loosens one.
//!
//! | mode | cross-site request |
//! |---|---|
//! | [`ThirdPartyReferrer::Default`] | policy unchanged |
//! | [`ThirdPartyReferrer::OriginOnly`] | `unsafe-url` and `no-referrer-when-downgrade` (which would send the full URL cross-site) become `strict-origin-when-cross-origin`; all others unchanged (they already send at most the origin) |
//! | [`ThirdPartyReferrer::None`] | `no-referrer` |
//!
//! Same-site requests are never touched.

use px_websec::origin::Url;
use px_websec::referrer::{self, ReferrerPolicy};

/// How hard to trim the referrer on third-party requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThirdPartyReferrer {
    /// Leave the policy alone.
    Default,
    /// Never send more than the origin cross-site.
    OriginOnly,
    /// Send no referrer cross-site.
    None,
}

/// The policy to use for a request: `policy` unless the request is
/// third-party and `mode` asks for stricter.
pub fn tightened_policy(
    policy: ReferrerPolicy,
    is_third_party: bool,
    mode: ThirdPartyReferrer,
) -> ReferrerPolicy {
    if !is_third_party {
        return policy;
    }
    match mode {
        ThirdPartyReferrer::Default => policy,
        ThirdPartyReferrer::None => ReferrerPolicy::NoReferrer,
        ThirdPartyReferrer::OriginOnly => match policy {
            ReferrerPolicy::UnsafeUrl | ReferrerPolicy::NoReferrerWhenDowngrade => {
                ReferrerPolicy::StrictOriginWhenCrossOrigin
            }
            other => other,
        },
    }
}

/// The `Referer` value to send: the effective policy (request's own, else
/// the document's, else the default), tightened, then px-websec's
/// computation.
pub fn referrer_for(
    source: Option<&Url>,
    request_policy: Option<ReferrerPolicy>,
    document_policy: Option<ReferrerPolicy>,
    request_url: &Url,
    is_third_party: bool,
    mode: ThirdPartyReferrer,
) -> Option<String> {
    let effective = referrer::effective_policy(request_policy, document_policy);
    let policy = tightened_policy(effective, is_third_party, mode);
    referrer::compute(source, policy, request_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ReferrerPolicy; 8] = [
        ReferrerPolicy::NoReferrer,
        ReferrerPolicy::NoReferrerWhenDowngrade,
        ReferrerPolicy::SameOrigin,
        ReferrerPolicy::Origin,
        ReferrerPolicy::StrictOrigin,
        ReferrerPolicy::OriginWhenCrossOrigin,
        ReferrerPolicy::StrictOriginWhenCrossOrigin,
        ReferrerPolicy::UnsafeUrl,
    ];

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn first_party_is_never_touched() {
        for p in ALL {
            for m in [
                ThirdPartyReferrer::Default,
                ThirdPartyReferrer::OriginOnly,
                ThirdPartyReferrer::None,
            ] {
                assert_eq!(tightened_policy(p, false, m), p);
            }
        }
    }

    #[test]
    fn origin_only_caps_full_url_policies() {
        let src = u("https://a.com/private/page?token=1");
        let req = u("https://tracker.net/p");
        let get = |p| {
            referrer_for(
                Some(&src),
                Some(p),
                None,
                &req,
                true,
                ThirdPartyReferrer::OriginOnly,
            )
        };
        assert_eq!(
            get(ReferrerPolicy::UnsafeUrl).as_deref(),
            Some("https://a.com/")
        );
        assert_eq!(
            get(ReferrerPolicy::NoReferrerWhenDowngrade).as_deref(),
            Some("https://a.com/")
        );
        assert_eq!(get(ReferrerPolicy::NoReferrer), None);
        assert_eq!(get(ReferrerPolicy::SameOrigin), None);
        assert_eq!(
            get(ReferrerPolicy::Origin).as_deref(),
            Some("https://a.com/")
        );
    }

    #[test]
    fn never_sends_more_than_untightened() {
        let src = u("https://a.com/private/page?token=1");
        let req = u("https://tracker.net/p");
        for p in ALL {
            let base = referrer::compute(Some(&src), p, &req);
            for m in [ThirdPartyReferrer::OriginOnly, ThirdPartyReferrer::None] {
                let t = referrer_for(Some(&src), Some(p), None, &req, true, m);
                match (&base, &t) {
                    (None, Some(_)) => panic!("{p:?}/{m:?} loosened"),
                    (Some(b), Some(t)) => assert!(t.len() <= b.len(), "{p:?}/{m:?}"),
                    _ => {}
                }
                if m == ThirdPartyReferrer::None {
                    assert_eq!(t, None);
                }
            }
        }
    }

    #[test]
    fn effective_policy_order_is_websec_s() {
        let src = u("https://a.com/x");
        let req = u("https://b.com/");
        // Request policy wins over document policy.
        let r = referrer_for(
            Some(&src),
            Some(ReferrerPolicy::NoReferrer),
            Some(ReferrerPolicy::UnsafeUrl),
            &req,
            true,
            ThirdPartyReferrer::Default,
        );
        assert_eq!(r, None);
        // Default policy when nothing is specified: origin cross-origin.
        let r = referrer_for(
            Some(&src),
            None,
            None,
            &req,
            true,
            ThirdPartyReferrer::Default,
        );
        assert_eq!(r.as_deref(), Some("https://a.com/"));
    }
}
