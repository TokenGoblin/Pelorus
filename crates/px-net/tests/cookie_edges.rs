//! Cookie edge cases beyond the two headline properties in `cookies.rs`:
//! lifetime attributes, host and path matching, size and count bounds, the
//! name prefixes, and the order cookies are sent in.
//!
//! Expected values are derived from RFC 6265bis (section numbers in comments),
//! not read back from the implementation.

use std::time::{Duration, Instant};

use px_net::cookie::{Jar, Rejected, SameSite, parse};
use px_net::partition::{Origin, PartitionKey};
use px_net::psl::PublicSuffixList;

fn psl() -> Option<PublicSuffixList> {
    PublicSuffixList::parse("com\nnet\nco.uk\n").ok()
}

fn origin(host: &str) -> Option<Origin> {
    Origin::new("https", host, 443)
}

fn key(psl: &PublicSuffixList, host: &str) -> Option<PartitionKey> {
    PartitionKey::new(psl, host, origin(host)?)
}

fn plain_origin(host: &str) -> Option<Origin> {
    Origin::new("http", host, 80)
}

// ---- lifetime ----------------------------------------------------------

/// 6265bis 5.7: a positive Max-Age sets an expiry that many seconds ahead.
#[test]
fn net_cookie_max_age_sets_an_expiry_that_many_seconds_ahead() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let before = Instant::now();
    let cookie = parse(&psl, &from, "a=1; Max-Age=3600").expect("cookie");
    let after = Instant::now();
    let at = cookie.expires_at.expect("persistent cookie");
    assert!(at >= before + Duration::from_secs(3600));
    assert!(at <= after + Duration::from_secs(3600));
}

#[test]
fn net_cookie_without_max_age_is_a_session_cookie() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    assert_eq!(parse(&psl, &from, "a=1").expect("c").expires_at, None);
}

/// Expires is documented as unparsed. It must never turn a cookie into a
/// persistent one, and it must not override Max-Age (6265bis 5.6.2: Max-Age
/// takes precedence).
#[test]
fn net_cookie_expires_never_makes_a_cookie_persistent_and_max_age_wins() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let only_expires =
        parse(&psl, &from, "a=1; Expires=Wed, 21 Oct 2099 07:28:00 GMT").expect("cookie");
    assert_eq!(only_expires.expires_at, None);

    let both = parse(
        &psl,
        &from,
        "a=1; Expires=Wed, 21 Oct 2099 07:28:00 GMT; Max-Age=0",
    )
    .expect("cookie");
    assert!(both.expires_at.is_some(), "Max-Age=0 still deletes");
}

#[test]
fn net_cookie_the_last_max_age_attribute_wins() {
    // 6265bis 5.7 step 3: when an attribute is repeated, the last one is used.
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let cookie = parse(&psl, &from, "a=1; Max-Age=3600; Max-Age=0").expect("cookie");
    let at = cookie.expires_at.expect("expiry");
    assert!(
        at <= Instant::now(),
        "the later Max-Age=0 means already expired"
    );
}

#[test]
fn net_cookie_negative_and_zero_max_age_delete_on_arrival() {
    let psl = psl().expect("psl");
    let k = key(&psl, "example.com").expect("key");
    for header in [
        "a=1; Max-Age=0",
        "a=1; Max-Age=-1",
        "a=1; Max-Age=-99999999999",
    ] {
        let mut jar = Jar::new();
        jar.store(&psl, &k, header).expect("accepted");
        assert!(
            jar.cookies_for(&k, "/", false).is_empty(),
            "{header} must not be sent"
        );
        jar.expire();
        assert!(jar.is_empty(), "{header} must be purged by expire()");
    }
}

#[test]
fn net_cookie_unparseable_max_age_is_refused() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    for bad in ["abc", "", "1.5", "3600s", "0x10"] {
        let header = format!("a=1; Max-Age={bad}");
        assert_eq!(
            parse(&psl, &from, &header),
            Err(Rejected::BadAttribute("Max-Age")),
            "{header}"
        );
    }
}

#[test]
fn net_cookie_a_huge_max_age_never_becomes_already_expired() {
    // i64::MAX seconds overflows Instant; it must not wrap into the past.
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let cookie = parse(&psl, &from, "a=1; Max-Age=9223372036854775807").expect("cookie");
    if let Some(at) = cookie.expires_at {
        assert!(at > Instant::now());
    }
}

/// A later Set-Cookie with Max-Age=0 for the same name/domain/path deletes the
/// stored cookie (6265bis 5.7 step 21-22).
#[test]
fn net_cookie_max_age_zero_deletes_a_stored_cookie() {
    let psl = psl().expect("psl");
    let k = key(&psl, "example.com").expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &k, "sid=1").expect("stored");
    assert_eq!(jar.cookies_for(&k, "/", false).len(), 1);
    jar.store(&psl, &k, "sid=gone; Max-Age=0").expect("stored");
    assert!(jar.cookies_for(&k, "/", false).is_empty());
    jar.expire();
    assert!(jar.is_empty());
}

// ---- name=value parsing -------------------------------------------------

#[test]
fn net_cookie_requires_a_name_and_an_equals_sign() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    for header in ["", "novalue", "=value", " =value", ";", "; Secure", "  "] {
        assert_eq!(
            parse(&psl, &from, header),
            Err(Rejected::Malformed),
            "{header:?}"
        );
    }
}

#[test]
fn net_cookie_value_may_contain_equals_and_is_trimmed() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let cookie = parse(&psl, &from, "  tok =  a=b=c  ; Path=/").expect("cookie");
    assert_eq!(cookie.name, "tok");
    assert_eq!(cookie.value, "a=b=c");
}

#[test]
fn net_cookie_empty_value_is_allowed() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let cookie = parse(&psl, &from, "a=").expect("cookie");
    assert_eq!(cookie.value, "");
}

#[test]
fn net_cookie_attribute_names_are_case_insensitive_and_unknown_ones_ignored() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let cookie = parse(
        &psl,
        &from,
        "a=1; SECURE; httponly; SameSite=STRICT; Priority=High; Partitioned; ;;",
    )
    .expect("cookie");
    assert!(cookie.secure);
    assert!(cookie.http_only);
    assert_eq!(cookie.same_site, SameSite::Strict);
}

#[test]
fn net_cookie_samesite_defaults_to_lax() {
    // 6265bis 5.5: absent SameSite is treated as Lax.
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    assert_eq!(
        parse(&psl, &from, "a=1").expect("c").same_site,
        SameSite::Lax
    );
}

#[test]
fn net_cookie_samesite_with_an_unknown_value_is_refused() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    for v in ["", "Relaxed", "lax strict"] {
        let header = format!("a=1; SameSite={v}");
        assert_eq!(
            parse(&psl, &from, &header),
            Err(Rejected::BadAttribute("SameSite")),
            "{header}"
        );
    }
}

// ---- size --------------------------------------------------------------

#[test]
fn net_cookie_header_size_limit_is_4096_bytes_inclusive() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let base = "a=";
    let ok = format!("{base}{}", "x".repeat(4096 - base.len()));
    assert_eq!(ok.len(), 4096);
    assert!(parse(&psl, &from, &ok).is_ok());

    let too_big = format!("{ok}x");
    assert_eq!(parse(&psl, &from, &too_big), Err(Rejected::TooLarge));
}

#[test]
fn net_cookie_size_is_checked_in_bytes_not_chars() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    // 1366 three-byte chars = 4098 bytes (> 4096) but only 1366 chars.
    let header = format!("a={}", "\u{20ac}".repeat(1366));
    assert!(header.len() > 4096 && header.chars().count() < 4096);
    assert_eq!(parse(&psl, &from, &header), Err(Rejected::TooLarge));
}

// ---- domain and path matching -------------------------------------------

#[test]
fn net_cookie_without_domain_is_host_only() {
    let psl = psl().expect("psl");
    let from = origin("www.Example.com").expect("origin");
    let cookie = parse(&psl, &from, "a=1").expect("cookie");
    assert!(cookie.host_only);
    assert_eq!(cookie.domain, "www.example.com");
    assert!(cookie.matches_host("www.example.com"));
    assert!(cookie.matches_host("WWW.EXAMPLE.COM"));
    assert!(!cookie.matches_host("sub.www.example.com"));
    assert!(!cookie.matches_host("example.com"));
}

#[test]
fn net_cookie_domain_attribute_covers_subdomains_on_label_boundaries() {
    let psl = psl().expect("psl");
    let from = origin("www.example.com").expect("origin");
    let cookie = parse(&psl, &from, "a=1; Domain=.Example.COM.").expect("cookie");
    assert!(!cookie.host_only);
    assert_eq!(cookie.domain, "example.com", "dots stripped, lowercased");
    assert!(cookie.matches_host("example.com"));
    assert!(cookie.matches_host("a.b.example.com"));
    assert!(!cookie.matches_host("notexample.com"));
    assert!(!cookie.matches_host("example.com.evil.net"));
    assert!(!cookie.matches_host(".example.com"), "empty leading label");
    assert!(!cookie.matches_host("com"));
}

#[test]
fn net_cookie_empty_domain_attribute_is_refused() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    for d in ["", ".", "..."] {
        let header = format!("a=1; Domain={d}");
        assert_eq!(
            parse(&psl, &from, &header),
            Err(Rejected::BadAttribute("Domain")),
            "{header}"
        );
    }
}

/// 6265bis 5.7 step 6 requires the request host to domain-match the Domain
/// attribute, so a parent host cannot set a cookie for a child.
#[test]
fn net_cookie_a_parent_host_cannot_set_a_cookie_for_a_child() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    assert_eq!(
        parse(&psl, &from, "a=1; Domain=www.example.com"),
        Err(Rejected::DomainMismatch)
    );
}

#[test]
fn net_cookie_domain_comparison_ignores_case_of_the_origin() {
    let psl = psl().expect("psl");
    let from = origin("WWW.Example.COM").expect("origin");
    assert!(parse(&psl, &from, "a=1; Domain=example.com").is_ok());
}

#[test]
fn net_cookie_path_matching_follows_rfc_6265_section_5_1_4() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    let cookie = parse(&psl, &from, "a=1; Path=/foo").expect("cookie");
    assert!(cookie.matches_path("/foo"));
    assert!(cookie.matches_path("/foo/"));
    assert!(cookie.matches_path("/foo/bar"));
    assert!(!cookie.matches_path("/foobar"), "not on a segment boundary");
    assert!(!cookie.matches_path("/fo"));
    assert!(!cookie.matches_path("/"));
    assert!(!cookie.matches_path("/Foo"), "paths are case-sensitive");

    let slash = parse(&psl, &from, "a=1; Path=/foo/").expect("cookie");
    assert!(slash.matches_path("/foo/bar"));
    assert!(
        !slash.matches_path("/foo"),
        "no trailing slash on the request"
    );

    let root = parse(&psl, &from, "a=1; Path=/").expect("cookie");
    assert!(root.matches_path("/"));
    assert!(root.matches_path("/anything/at/all"));
}

#[test]
fn net_cookie_missing_path_defaults_to_root_and_relative_path_is_refused() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    assert_eq!(parse(&psl, &from, "a=1").expect("c").path, "/");
    // Documented fail-closed behaviour: RFC 6265 would fall back to the
    // default path; this crate rejects the cookie instead.
    assert_eq!(
        parse(&psl, &from, "a=1; Path=foo"),
        Err(Rejected::BadAttribute("Path"))
    );
    assert_eq!(
        parse(&psl, &from, "a=1; Path="),
        Err(Rejected::BadAttribute("Path"))
    );
}

#[test]
fn net_cookie_jar_filters_by_request_path() {
    let psl = psl().expect("psl");
    let k = key(&psl, "example.com").expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &k, "a=1; Path=/app").expect("stored");
    jar.store(&psl, &k, "b=2; Path=/").expect("stored");

    let names = |path: &str| -> Vec<String> {
        let mut v: Vec<String> = jar
            .cookies_for(&k, path, false)
            .iter()
            .map(|c| c.name.clone())
            .collect();
        v.sort();
        v
    };
    assert_eq!(names("/app/x"), vec!["a", "b"]);
    assert_eq!(names("/application"), vec!["b"]);
    assert_eq!(names("/"), vec!["b"]);
}

/// 6265bis 5.8.3 step 2: cookies with longer paths are listed before cookies
/// with shorter paths; ties keep creation order.
#[test]
fn net_cookie_longer_paths_are_sent_first() {
    let psl = psl().expect("psl");
    let k = key(&psl, "example.com").expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &k, "root=1; Path=/").expect("stored");
    jar.store(&psl, &k, "deep=2; Path=/a/b").expect("stored");
    jar.store(&psl, &k, "mid=3; Path=/a").expect("stored");
    jar.store(&psl, &k, "mid2=4; Path=/a").expect("stored");

    let order: Vec<&str> = jar
        .cookies_for(&k, "/a/b/c", false)
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(order, vec!["deep", "mid", "mid2", "root"]);
}

// ---- Secure, SameSite ---------------------------------------------------

/// 6265bis 5.7 step 11: a Secure cookie must be refused when the response came
/// over a non-secure channel, otherwise a network attacker can overwrite a
/// session cookie by injecting an http reply.
#[test]
fn net_cookie_secure_cookie_from_an_insecure_origin_is_refused() {
    let psl = psl().expect("psl");
    let from = plain_origin("example.com").expect("origin");
    assert!(
        parse(&psl, &from, "sid=evil; Secure").is_err(),
        "Secure cookies may only be set over a secure channel"
    );
    // A non-Secure cookie over http is fine.
    assert!(parse(&psl, &from, "pref=1").is_ok());
}

#[test]
fn net_cookie_strict_and_lax_are_withheld_cross_site_but_none_is_sent() {
    let psl = psl().expect("psl");
    let k = key(&psl, "example.com").expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &k, "s=1; SameSite=Strict").expect("stored");
    jar.store(&psl, &k, "l=1; SameSite=Lax").expect("stored");
    jar.store(&psl, &k, "d=1").expect("stored (default lax)");
    jar.store(&psl, &k, "n=1; SameSite=None; Secure")
        .expect("stored");

    assert_eq!(jar.cookies_for(&k, "/", false).len(), 4);
    let cross: Vec<&str> = jar
        .cookies_for(&k, "/", true)
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(cross, vec!["n"]);
}

// ---- prefixes ------------------------------------------------------------

#[test]
fn net_cookie_prefixed_names_that_meet_their_requirements_are_accepted() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    assert!(parse(&psl, &from, "__Secure-a=1; Secure").is_ok());
    assert!(parse(&psl, &from, "__Secure-a=1; Secure; Domain=example.com").is_ok());
    assert!(parse(&psl, &from, "__Host-a=1; Secure; Path=/").is_ok());
}

#[test]
fn net_cookie_host_prefix_rejects_each_missing_requirement() {
    let psl = psl().expect("psl");
    let from = origin("www.example.com").expect("origin");
    for header in [
        "__Host-a=1; Path=/",
        "__Host-a=1; Secure",
        "__Host-a=1; Secure; Path=/app",
        "__Host-a=1; Secure; Path=/; Domain=example.com",
    ] {
        assert!(
            matches!(
                parse(&psl, &from, header),
                Err(Rejected::PrefixViolation(_))
            ),
            "{header}"
        );
    }
    assert!(matches!(
        parse(&psl, &from, "__Secure-a=1"),
        Err(Rejected::PrefixViolation(_))
    ));
}

#[test]
fn net_cookie_prefix_must_be_at_the_start_of_the_name() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    assert!(parse(&psl, &from, "x__Host-a=1").is_ok());
    assert!(parse(&psl, &from, "__Hostile=1").is_ok());
}

/// Genuine spec ambiguity: RFC 6265bis drafts before -11 compare the prefix
/// case-sensitively; later drafts (and browsers) compare case-insensitively.
/// The safe reading is case-insensitive, since a site cannot rely on the
/// weaker guarantee. Raised rather than guessed.
#[test]
#[ignore = "6265bis draft ambiguity: case-sensitivity of cookie name prefixes"]
fn net_cookie_prefix_comparison_is_case_insensitive() {
    let psl = psl().expect("psl");
    let from = origin("example.com").expect("origin");
    assert!(matches!(
        parse(&psl, &from, "__SECURE-a=1"),
        Err(Rejected::PrefixViolation(_))
    ));
    assert!(matches!(
        parse(&psl, &from, "__host-a=1; Secure"),
        Err(Rejected::PrefixViolation(_))
    ));
}

// ---- jar bounds and identity ---------------------------------------------

#[test]
fn net_cookie_jar_holds_exactly_fifty_per_partition() {
    let psl = psl().expect("psl");
    let k = key(&psl, "example.com").expect("key");
    let mut jar = Jar::new();
    for i in 0..50 {
        jar.store(&psl, &k, &format!("c{i}=1")).expect("within cap");
    }
    assert_eq!(jar.len(), 50);
    assert_eq!(jar.store(&psl, &k, "c50=1"), Err(Rejected::JarFull));
    assert_eq!(jar.len(), 50);
}

#[test]
fn net_cookie_replacing_when_full_is_not_an_addition() {
    let psl = psl().expect("psl");
    let k = key(&psl, "example.com").expect("key");
    let mut jar = Jar::new();
    for i in 0..50 {
        jar.store(&psl, &k, &format!("c{i}=1")).expect("within cap");
    }
    jar.store(&psl, &k, "c7=updated")
        .expect("replacement allowed");
    assert_eq!(jar.len(), 50);
    let value = jar
        .cookies_for(&k, "/", false)
        .iter()
        .find(|c| c.name == "c7")
        .map(|c| c.value.clone());
    assert_eq!(value.as_deref(), Some("updated"));
}

#[test]
fn net_cookie_the_cap_is_per_partition_not_global() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com").expect("key");
    let b = key(&psl, "b.com").expect("key");
    let mut jar = Jar::new();
    for i in 0..50 {
        jar.store(&psl, &a, &format!("c{i}=1")).expect("a");
    }
    assert!(jar.store(&psl, &b, "c0=1").is_ok(), "b has its own budget");
    assert_eq!(jar.len(), 51);
}

/// Identity is (name, domain, path) (6265bis 5.7 step 22).
#[test]
fn net_cookie_identity_is_name_domain_and_path() {
    let psl = psl().expect("psl");
    let k = key(&psl, "www.example.com").expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &k, "a=1; Path=/x").expect("stored");
    jar.store(&psl, &k, "a=2; Path=/y").expect("different path");
    jar.store(&psl, &k, "a=3; Domain=example.com; Path=/x")
        .expect("different domain scope");
    assert_eq!(jar.len(), 3);
    jar.store(&psl, &k, "a=4; Path=/x")
        .expect("replaces the first");
    assert_eq!(jar.len(), 3);
}

#[test]
fn net_cookie_names_are_case_sensitive() {
    let psl = psl().expect("psl");
    let k = key(&psl, "example.com").expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &k, "Sid=1").expect("stored");
    jar.store(&psl, &k, "sid=2").expect("stored");
    assert_eq!(jar.len(), 2);
}

#[test]
fn net_cookie_forget_only_drops_one_partition() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com").expect("key");
    let b = key(&psl, "b.com").expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &a, "x=1").expect("a");
    jar.store(&psl, &b, "x=1").expect("b");
    jar.forget(&a);
    assert!(jar.cookies_for(&a, "/", false).is_empty());
    assert_eq!(jar.cookies_for(&b, "/", false).len(), 1);
    // Forgetting an unknown partition is a no-op.
    jar.forget(&a);
    assert_eq!(jar.len(), 1);
}

#[test]
fn net_cookie_a_domain_cookie_is_not_sent_to_a_sibling_partition_host() {
    // Same top-level, different destination host: different partition key, so
    // nothing is shared even though the Domain would textually match.
    let psl = psl().expect("psl");
    let www =
        PartitionKey::new(&psl, "example.com", origin("www.example.com").expect("o")).expect("key");
    let api =
        PartitionKey::new(&psl, "example.com", origin("api.example.com").expect("o")).expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &www, "a=1; Domain=example.com")
        .expect("stored");
    assert!(jar.cookies_for(&api, "/", false).is_empty());
    assert_eq!(jar.cookies_for(&www, "/", false).len(), 1);
}
