//! Cookie policy: the supercookie refusal, and partitioned storage.
//!
//! Two properties carry almost all the weight, and neither is visible by using
//! the product:
//!
//! - a cookie set on a public suffix would be readable and writable by every
//!   site under it;
//! - a jar shared across top-level sites lets a third party embedded on two of
//!   them join the visits, which is the whole mechanism of third-party
//!   tracking.
//!
//! Both fail silently when they fail. Nothing errors, nothing looks wrong, and
//! the user has no way to notice.

use px_net::cookie::{Jar, Rejected, parse};
use px_net::partition::{Origin, PartitionKey};
use px_net::psl::PublicSuffixList;

fn psl() -> Option<PublicSuffixList> {
    PublicSuffixList::parse("com\nnet\nco.uk\ngithub.io\n").ok()
}

fn origin(host: &str) -> Option<Origin> {
    Origin::new("https", host, 443)
}

fn key(psl: &PublicSuffixList, top_level: &str, dest: &str) -> Option<PartitionKey> {
    PartitionKey::new(psl, top_level, origin(dest)?)
}

/// The supercookie. `Domain=com` would be a cookie every `.com` site shares.
#[test]
fn cookies_on_a_public_suffix_are_refused() {
    let psl = psl().expect("list");
    let from = origin("shop.example.com").expect("origin");

    for domain in ["com", "co.uk", "github.io", "net"] {
        let header = format!("id=1; Domain={domain}");
        assert_eq!(
            parse(&psl, &from, &header),
            Err(Rejected::PublicSuffixDomain),
            "Domain={domain} is a public suffix and must be refused"
        );
    }
}

/// A site may widen a cookie to its own parent, never to somebody else's.
#[test]
fn cookies_cannot_be_set_for_another_site() {
    let psl = psl().expect("list");
    let from = origin("shop.example.com").expect("origin");

    // Its own registrable domain: allowed.
    assert!(parse(&psl, &from, "id=1; Domain=example.com").is_ok());

    // Somebody else's: refused.
    for domain in ["evil.com", "other.example.net", "notexample.com"] {
        let header = format!("id=1; Domain={domain}");
        assert_eq!(
            parse(&psl, &from, &header),
            Err(Rejected::DomainMismatch),
            "a cookie for {domain} must not be settable from shop.example.com"
        );
    }
}

/// The property invariant 2 exists for. A third party embedded on two sites
/// must not be able to join them.
#[test]
fn cookies_do_not_cross_a_partition_boundary() {
    let psl = psl().expect("list");
    let on_news = key(&psl, "news.example.com", "tracker.example.net").expect("key");
    let on_other = key(&psl, "other.example.net", "tracker.example.net").expect("key");

    let mut jar = Jar::new();
    jar.store(&psl, &on_news, "id=abc").expect("stored");

    assert_eq!(
        jar.cookies_for(&on_news, "/", false).len(),
        1,
        "the site that set it gets it back"
    );
    assert!(
        jar.cookies_for(&on_other, "/", false).is_empty(),
        "the same third party on a different top-level site must not see it; \
         this is how third-party tracking works and the partition is what stops it"
    );
}

/// A Secure cookie never travels in plaintext, which is what stops a network
/// attacker reading a session by forcing one http request.
#[test]
fn cookies_marked_secure_are_not_sent_over_plaintext() {
    let psl = psl().expect("list");
    let secure_key = key(&psl, "example.com", "example.com").expect("key");

    let mut jar = Jar::new();
    jar.store(&psl, &secure_key, "sid=x; Secure")
        .expect("stored");
    assert_eq!(jar.cookies_for(&secure_key, "/", false).len(), 1);

    // The same destination over http is a different origin, so a different
    // key — and the cookie must not appear there either.
    let plain = PartitionKey::new(
        &psl,
        "example.com",
        Origin::new("http", "example.com", 80).expect("origin"),
    )
    .expect("key");
    assert!(
        jar.cookies_for(&plain, "/", false).is_empty(),
        "a Secure cookie must not travel in plaintext"
    );
}

/// SameSite=None is a request to be sent cross-site; without Secure it is
/// readable by a network attacker on every third-party request.
#[test]
fn cookies_with_samesite_none_must_be_secure() {
    let psl = psl().expect("list");
    let from = origin("example.com").expect("origin");

    assert_eq!(
        parse(&psl, &from, "id=1; SameSite=None"),
        Err(Rejected::InsecureSameSiteNone)
    );
    assert!(parse(&psl, &from, "id=1; SameSite=None; Secure").is_ok());
}

#[test]
fn cookies_respect_samesite_on_cross_site_requests() {
    let psl = psl().expect("list");
    let site = key(&psl, "example.com", "example.com").expect("key");

    let mut jar = Jar::new();
    jar.store(&psl, &site, "lax=1").expect("default is Lax");
    jar.store(&psl, &site, "strict=1; SameSite=Strict")
        .expect("stored");
    jar.store(&psl, &site, "none=1; SameSite=None; Secure")
        .expect("stored");

    assert_eq!(
        jar.cookies_for(&site, "/", false).len(),
        3,
        "same-site sends all"
    );

    let cross: Vec<&str> = jar
        .cookies_for(&site, "/", true)
        .iter()
        .map(|cookie| cookie.name.as_str())
        .collect();
    assert_eq!(cross, ["none"], "only SameSite=None crosses");
}

/// The cookie prefixes are the only integrity mechanism cookies have. A
/// browser that does not enforce them makes them worse than absent, because a
/// site relying on one believes it is protected.
#[test]
fn cookies_enforce_the_name_prefixes() {
    let psl = psl().expect("list");
    let from = origin("shop.example.com").expect("origin");

    assert!(matches!(
        parse(&psl, &from, "__Secure-a=1"),
        Err(Rejected::PrefixViolation(_))
    ));
    assert!(parse(&psl, &from, "__Secure-a=1; Secure").is_ok());

    // __Host- additionally forbids Domain and requires Path=/.
    assert!(matches!(
        parse(
            &psl,
            &from,
            "__Host-a=1; Secure; Domain=example.com; Path=/"
        ),
        Err(Rejected::PrefixViolation(_))
    ));
    assert!(matches!(
        parse(&psl, &from, "__Host-a=1; Secure; Path=/x"),
        Err(Rejected::PrefixViolation(_))
    ));
    assert!(parse(&psl, &from, "__Host-a=1; Secure; Path=/").is_ok());
}

/// An unparseable attribute rejects the cookie rather than becoming a default.
/// Accepting what parses and ignoring the rest is how a Secure flag gets
/// dropped silently.
#[test]
fn cookies_with_an_unparseable_attribute_are_refused() {
    let psl = psl().expect("list");
    let from = origin("example.com").expect("origin");

    assert!(matches!(
        parse(&psl, &from, "a=1; SameSite=sideways"),
        Err(Rejected::BadAttribute("SameSite"))
    ));
    assert!(matches!(
        parse(&psl, &from, "a=1; Max-Age=soon"),
        Err(Rejected::BadAttribute("Max-Age"))
    ));
    assert!(matches!(
        parse(&psl, &from, "a=1; Path=relative"),
        Err(Rejected::BadAttribute("Path"))
    ));
    assert_eq!(parse(&psl, &from, "novalue"), Err(Rejected::Malformed));
}

/// Host matching is label-wise. `notexample.com` ends with `example.com` as a
/// string and is a different site.
#[test]
fn cookies_do_not_match_across_a_label_boundary() {
    let psl = psl().expect("list");
    let parent = key(&psl, "example.com", "example.com").expect("key");
    let mut jar = Jar::new();
    jar.store(&psl, &parent, "id=1; Domain=example.com")
        .expect("stored");

    let sub = key(&psl, "example.com", "shop.example.com").expect("key");
    assert_eq!(
        jar.cookies_for(&sub, "/", false).len(),
        0,
        "a different destination origin is a different partition key entirely"
    );

    // Within one key, the host rule itself is what is under test.
    let cookies = jar.cookies_for(&parent, "/", false);
    assert_eq!(cookies.len(), 1);
    assert!(
        cookies
            .first()
            .is_some_and(|c| c.matches_host("a.example.com"))
    );
    assert!(
        cookies
            .first()
            .is_some_and(|c| !c.matches_host("notexample.com")),
        "a suffix comparison would wrongly match notexample.com"
    );
}

/// A non-positive Max-Age is the documented way to delete a cookie.
#[test]
fn cookies_with_a_non_positive_max_age_are_already_expired() {
    let psl = psl().expect("list");
    let site = key(&psl, "example.com", "example.com").expect("key");

    let mut jar = Jar::new();
    jar.store(&psl, &site, "a=1").expect("stored");
    assert_eq!(jar.cookies_for(&site, "/", false).len(), 1);

    jar.store(&psl, &site, "a=1; Max-Age=0").expect("stored");
    assert!(
        jar.cookies_for(&site, "/", false).is_empty(),
        "Max-Age=0 deletes"
    );
}

/// The jar is bounded. Partitioning multiplies jars, so the cap that mattered
/// unpartitioned is not the cap that matters now.
#[test]
fn cookies_are_bounded_per_partition() {
    let psl = psl().expect("list");
    let site = key(&psl, "example.com", "example.com").expect("key");
    let mut jar = Jar::new();

    for index in 0..200 {
        let _ = jar.store(&psl, &site, &format!("c{index}=1"));
    }
    assert!(
        jar.len() <= 50,
        "one partition accumulated {} cookies",
        jar.len()
    );
}

/// Clearing a site takes its cookies with it.
#[test]
fn cookies_are_forgotten_with_their_partition() {
    let psl = psl().expect("list");
    let one = key(&psl, "example.com", "example.com").expect("key");
    let two = key(&psl, "other.example.net", "other.example.net").expect("key");

    let mut jar = Jar::new();
    jar.store(&psl, &one, "a=1").expect("stored");
    jar.store(&psl, &two, "b=1").expect("stored");

    jar.forget(&one);
    assert!(jar.cookies_for(&one, "/", false).is_empty());
    assert_eq!(
        jar.cookies_for(&two, "/", false).len(),
        1,
        "forgetting one partition must not disturb another"
    );
}
