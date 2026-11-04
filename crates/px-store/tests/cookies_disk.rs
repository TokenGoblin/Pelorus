//! Cookie persistence through a real store and px-net's real jar.
#![cfg(test)]

mod common;

mod cookies_on_disk {
    use super::common::fx::{TempDir, pk, psl};
    use px_net::cookie::{Cookie, Jar};
    use px_net::partition::{Origin, PartitionKey};
    use px_store::cookies::{LoadReport, load, save};
    use px_store::{Namespace, Store};
    use std::time::Instant;

    fn key(top: &str, host: &str) -> PartitionKey {
        PartitionKey::new(&psl(), top, Origin::new("https", host, 443).unwrap()).unwrap()
    }

    fn visible<'a>(jar: &'a Jar, k: &PartitionKey) -> Vec<&'a Cookie> {
        jar.cookies_for(k, "/", false)
    }

    #[test]
    fn jar_round_trips_per_partition_through_disk() {
        let d = TempDir::new("cookies");
        let a = key("a.example", "www.cdn.example");
        let b = key("b.example", "www.cdn.example");
        let psl = psl();

        let mut jar = Jar::new();
        jar.store(&psl, &a, "sid=A-SESSION; Domain=cdn.example; Path=/; Secure; SameSite=None; Max-Age=3600").unwrap();
        jar.store(&psl, &a, "theme=dark; Path=/; Max-Age=3600").unwrap();
        jar.store(&psl, &a, "tmp=session-only; Path=/").unwrap();
        jar.store(&psl, &b, "sid=B-SESSION; Path=/; Max-Age=3600").unwrap();

        let now = Instant::now();
        {
            let mut s = Store::open(d.path()).unwrap();
            let a_cookies = visible(&jar, &a);
            assert_eq!(a_cookies.len(), 3);
            assert_eq!(save(&mut s, &a, &a_cookies, now, 1_000).unwrap(), 3);
            let b_cookies = visible(&jar, &b);
            assert_eq!(save(&mut s, &b, &b_cookies, now, 1_000).unwrap(), 1);
        }

        let s = Store::open(d.path()).unwrap();
        let mut fresh = Jar::new();
        let report = load(&s, &psl, &a, &mut fresh, 1_030, true);
        assert_eq!(report, LoadReport { loaded: 3, ..LoadReport::default() });
        let mut names: Vec<&str> = visible(&fresh, &a).iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["sid", "theme", "tmp"]);
        // Partition a's load surfaced nothing from partition b.
        assert!(visible(&fresh, &b).is_empty());
        assert!(visible(&fresh, &a).iter().all(|c| c.value != "B-SESSION"));

        // Without include_session the session cookie stays behind.
        let mut no_session = Jar::new();
        let r = load(&s, &psl, &a, &mut no_session, 1_030, false);
        assert_eq!((r.loaded, r.skipped_session), (2, 1));

        // After the lifetime has passed, nothing persistent loads.
        let mut late = Jar::new();
        let r = load(&s, &psl, &a, &mut late, 1_000 + 3_601, true);
        assert_eq!((r.loaded, r.expired), (1, 2));
    }

    #[test]
    fn save_replaces_the_set_and_removes_deleted_cookies() {
        let d = TempDir::new("cookies-replace");
        let a = key("a.example", "www.cdn.example");
        let psl = psl();
        let mut s = Store::open(d.path()).unwrap();
        let mut jar = Jar::new();
        jar.store(&psl, &a, "one=1; Path=/; Max-Age=100").unwrap();
        jar.store(&psl, &a, "two=2; Path=/; Max-Age=100").unwrap();
        let now = Instant::now();
        save(&mut s, &a, &visible(&jar, &a), now, 0).unwrap();
        assert_eq!(s.keys(&a, Namespace::Cookies).len(), 2);

        let mut jar2 = Jar::new();
        jar2.store(&psl, &a, "two=2b; Path=/; Max-Age=100").unwrap();
        save(&mut s, &a, &visible(&jar2, &a), now, 0).unwrap();
        assert_eq!(s.keys(&a, Namespace::Cookies).len(), 1);

        let mut out = Jar::new();
        load(&s, &psl, &a, &mut out, 0, true);
        let v = visible(&out, &a);
        assert_eq!((v.len(), v[0].name.as_str(), v[0].value.as_str()), (1, "two", "2b"));
    }

    #[test]
    fn corrupt_and_invalid_records_are_rejected_not_loaded() {
        let d = TempDir::new("cookies-bad");
        let a = key("a.example", "www.cdn.example");
        let psl = psl();
        let mut s = Store::open(d.path()).unwrap();
        // Garbage under the cookie namespace.
        s.put(&a, Namespace::Cookies, b"junk", b"not a cookie").unwrap();
        // Page storage under the same key bytes is a different namespace.
        s.put(&a, Namespace::Storage, b"junk", b"page data").unwrap();
        let mut jar = Jar::new();
        let r = load(&s, &psl, &a, &mut jar, 0, true);
        assert_eq!((r.loaded, r.rejected), (0, 1));
        assert!(jar.is_empty());
    }

    #[test]
    fn a_planted_supercookie_is_refused_on_load() {
        use px_store::cookies::StoredCookie;
        use px_net::cookie::SameSite;
        let d = TempDir::new("cookies-super");
        let a = pk("a.example", "www.cdn.example");
        let psl = psl();
        let mut s = Store::open(d.path()).unwrap();
        // Somebody wrote a cookie scoped to a public suffix straight into the file.
        let evil = StoredCookie {
            name: "track".into(),
            value: "1".into(),
            domain: "example".into(),
            host_only: false,
            path: "/".into(),
            secure: false,
            http_only: false,
            same_site: SameSite::Lax,
            expires_unix: Some(10_000),
        };
        s.put(&a, Namespace::Cookies, &evil.store_key().unwrap(), &evil.encode().unwrap()).unwrap();
        let mut jar = Jar::new();
        let r = load(&s, &psl, &a, &mut jar, 0, true);
        assert_eq!((r.loaded, r.rejected), (0, 1));
        assert!(jar.is_empty());
    }
}
