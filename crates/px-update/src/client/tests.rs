use super::*;
use crate::manifest::{Manifest, Rotation};
use crate::request::ALLOWED_HEADERS;
use crate::trust::tests::{NOW, pk, sigfile_text, trust1};
use std::collections::HashMap;

const ART: &[u8] = b"pretend this is the installer payload";

fn file_for(path: &str, body: &[u8]) -> FileRef {
    FileRef {
        size: body.len() as u64,
        sha256: sha256(body),
        sha512: sha512(body),
        path: path.to_string(),
    }
}

fn base_release() -> Release {
    Release {
        channel: "stable".into(),
        key_epoch: 1,
        sequence: 8,
        issued: NOW - 1000,
        expires: NOW + 6 * DAY_SECS,
        version: Version::new(1, 1, 0),
        artifacts: vec![
            Artifact {
                platform: "linux-x86_64".into(),
                file: file_for("releases/1.1.0/app-linux.tar", ART),
            },
            Artifact {
                platform: "windows-x86_64".into(),
                file: file_for("releases/1.1.0/app-win.zip", b"win"),
            },
        ],
        data: vec![
            DataEntry {
                kind: DataKind::Psl,
                version: 20_260_920,
                file: file_for("data/psl-20260920.dat", b"psl data"),
            },
            DataEntry {
                kind: DataKind::Hsts,
                version: 20_260_920,
                file: file_for("data/hsts-20260920.txt", b"hsts data"),
            },
        ],
    }
}

fn mirrors() -> Vec<Mirror> {
    ["m1.example.net", "m2.example.net"]
        .iter()
        .map(|h| Mirror {
            host: (*h).into(),
            port: 443,
            base: "/v1".into(),
        })
        .collect()
}

fn cfg() -> Config {
    Config::new("stable", "linux-x86_64", mirrors())
}

/// Installed 1.0.0, sequence 7 seen, data from 14 days ago, last check 8 days ago.
fn state() -> State {
    let mut s = State::new(Version::new(1, 0, 0), [20_260_909; 3]);
    s.max_sequence = 7;
    s.last_check = NOW - 8 * DAY_SECS;
    s.last_attempt = NOW - 8 * DAY_SECS;
    s
}

type Reply = Result<Vec<u8>, TransportError>;

#[derive(Default)]
struct Mock {
    files: HashMap<(String, String), Reply>,
    /// (host, resource, cap, request bytes)
    log: Vec<(String, String, u64, Vec<u8>)>,
}

impl Mock {
    fn put(&mut self, host: &str, resource: &str, r: Reply) {
        self.files.insert((host.into(), resource.into()), r);
    }

    fn publish(&mut self, host: &str, manifest: &[u8], signers: &[u8]) {
        self.put(host, "manifest.txt", Ok(manifest.to_vec()));
        self.put(
            host,
            "manifest.sig",
            Ok(sigfile_text(manifest, signers).into_bytes()),
        );
    }

    fn publish_release(&mut self, host: &str, r: &Release, signers: &[u8]) {
        let bytes = Manifest::Release(r.clone()).encode().into_bytes();
        self.publish(host, &bytes, signers);
    }

    fn resources(&self) -> Vec<String> {
        self.log.iter().map(|l| l.1.clone()).collect()
    }
}

impl Transport for Mock {
    fn fetch(&mut self, mirror: &Mirror, request: &Request, max_body: u64) -> Reply {
        // Every request that reaches a transport has passed the audit.
        assert_eq!(audit(&request.bytes), Ok(()));
        let text = String::from_utf8(request.bytes.clone()).unwrap();
        let line = text.lines().next().unwrap();
        let resource = line
            .split(' ')
            .nth(1)
            .unwrap()
            .strip_prefix("/v1/")
            .unwrap()
            .to_string();
        for h in text.lines().skip(1).filter(|l| !l.is_empty()) {
            assert!(ALLOWED_HEADERS.contains(&h.split(": ").next().unwrap()));
        }
        assert!(!text.contains('?'));
        self.log.push((
            mirror.host.clone(),
            resource.clone(),
            max_body,
            request.bytes.clone(),
        ));
        match self.files.get(&(mirror.host.clone(), resource)) {
            None => Err(TransportError::Status(404)),
            Some(Ok(b)) if b.len() as u64 > max_body => Err(TransportError::TooLarge),
            Some(r) => r.clone(),
        }
    }
}

fn run(mock: &mut Mock, st: &State) -> Outcome {
    check(mock, &cfg(), &trust1(&[1], 1), st, NOW, Trigger::Scheduled)
}

fn one_mirror_with(r: &Release) -> Mock {
    let mut m = Mock::default();
    m.publish_release("m1.example.net", r, &[1]);
    m
}

// ---- the end-to-end gate -----------------------------------------------------

#[test]
fn end_to_end_signed_manifest_to_verified_artifact() {
    let rel = base_release();
    let mut mock = one_mirror_with(&rel);
    mock.put(
        "m1.example.net",
        "releases/1.1.0/app-linux.tar",
        Ok(ART.to_vec()),
    );
    let out = run(&mut mock, &state());
    let Decision::UpdateAvailable {
        release,
        artifact,
        data,
    } = out.decision
    else {
        panic!("{:?}", out.decision)
    };
    assert_eq!(release.version, Version::new(1, 1, 0));
    assert_eq!(artifact.platform, "linux-x86_64");
    assert_eq!(data.len(), 2);
    // Only the manifest and its signature were requested by the check.
    assert_eq!(mock.resources(), ["manifest.txt", "manifest.sig"]);
    assert_eq!(out.state.max_sequence, 8);
    assert_eq!(out.state.last_check, NOW);
    // Installed version only moves when the caller records an install.
    assert_eq!(out.state.installed, Version::new(1, 0, 0));

    let mut st = out.state;
    let body = download(&mut mock, &cfg(), &mut st, &artifact.file).unwrap();
    assert_eq!(body, ART);
    st.record_release_installed(release.version);
    assert_eq!(st.installed, Version::new(1, 1, 0));
    // The artifact request was capped at its declared size.
    let last = mock.log.last().unwrap();
    assert_eq!(last.2, ART.len() as u64);
}

#[test]
fn tampered_manifest_is_rejected_and_nothing_it_names_is_fetched() {
    let rel = base_release();
    let mut mock = Mock::default();
    let mut bytes = Manifest::Release(rel).encode().into_bytes();
    let sig = sigfile_text(&bytes, &[1]);
    // Point the artifact at a different digest after signing.
    let i = bytes.windows(4).position(|w| w == b"1234").unwrap_or(40);
    bytes[i] ^= 1;
    mock.put("m1.example.net", "manifest.txt", Ok(bytes));
    mock.put("m1.example.net", "manifest.sig", Ok(sig.into_bytes()));
    let before = state();
    let out = run(&mut mock, &before);
    assert!(matches!(
        out.decision,
        Decision::Rejected(Reason::Trust(TrustError::ThresholdNotMet { .. }))
    ));
    assert!(
        mock.resources()
            .iter()
            .all(|r| r == "manifest.txt" || r == "manifest.sig" || r.starts_with("rotation-")),
        "{:?}",
        mock.resources()
    );
    // Nothing recorded except the attempt and the mirror failure.
    assert_eq!(out.state.max_sequence, before.max_sequence);
    assert_eq!(out.state.last_check, before.last_check);
    assert_eq!(out.state.installed, before.installed);
}

#[test]
fn unsigned_wrongly_signed_and_garbage_are_rejected() {
    let rel = base_release();
    let bytes = Manifest::Release(rel).encode().into_bytes();
    for (sigs, label) in [
        (b"".to_vec(), "empty sig"),
        (b"garbage\n".to_vec(), "garbage sig"),
        (sigfile_text(&bytes, &[9]).into_bytes(), "untrusted signer"),
    ] {
        let mut mock = Mock::default();
        mock.put("m1.example.net", "manifest.txt", Ok(bytes.clone()));
        mock.put("m1.example.net", "manifest.sig", Ok(sigs));
        let out = run(&mut mock, &state());
        assert!(matches!(out.decision, Decision::Rejected(_)), "{label}");
    }
    let mut mock = Mock::default();
    mock.publish("m1.example.net", b"not a manifest\n", &[1]);
    let out = run(&mut mock, &state());
    assert!(matches!(
        out.decision,
        Decision::Rejected(Reason::Trust(TrustError::Parse(_)))
    ));
}

// ---- monotonic, fresh ----------------------------------------------------------

#[test]
fn same_version_is_up_to_date_and_data_refresh_is_separate() {
    let mut rel = base_release();
    rel.version = Version::new(1, 0, 0);
    rel.data.clear();
    let out = run(&mut one_mirror_with(&rel), &state());
    assert_eq!(out.decision, Decision::UpToDate);
    assert_eq!(out.state.max_sequence, 8);

    let mut rel = base_release();
    rel.version = Version::new(1, 0, 0);
    let out = run(&mut one_mirror_with(&rel), &state());
    let Decision::DataRefresh { data } = out.decision else {
        panic!()
    };
    assert_eq!(data.len(), 2);
}

#[test]
fn never_downgrades_release_sequence_or_data() {
    let mut rel = base_release();
    rel.version = Version::new(0, 9, 9);
    let out = run(&mut one_mirror_with(&rel), &state());
    assert!(matches!(
        out.decision,
        Decision::Rejected(Reason::VersionDowngrade { .. })
    ));

    let mut rel = base_release();
    rel.sequence = 6;
    let out = run(&mut one_mirror_with(&rel), &state());
    assert!(matches!(
        out.decision,
        Decision::Rejected(Reason::SequenceRollback {
            offered: 6,
            seen: 7
        })
    ));

    let mut rel = base_release();
    rel.data[0].version = 20_260_801;
    let out = run(&mut one_mirror_with(&rel), &state());
    assert_eq!(
        out.decision,
        Decision::Rejected(Reason::DataDowngrade(DataKind::Psl))
    );

    // An equal sequence (the same manifest fetched again) is fine.
    let mut st = state();
    st.max_sequence = 8;
    let out = run(&mut one_mirror_with(&base_release()), &st);
    assert!(matches!(out.decision, Decision::UpdateAvailable { .. }));
}

#[test]
fn replayed_old_manifest_is_stale() {
    let mut rel = base_release();
    rel.issued = NOW - 20 * DAY_SECS;
    rel.expires = NOW - DAY_SECS;
    let out = run(&mut one_mirror_with(&rel), &state());
    assert_eq!(
        out.decision,
        Decision::Rejected(Reason::Trust(TrustError::Expired))
    );
    let mut rel = base_release();
    rel.issued = NOW + 10 * DAY_SECS;
    rel.expires = NOW + 20 * DAY_SECS;
    let out = run(&mut one_mirror_with(&rel), &state());
    assert_eq!(
        out.decision,
        Decision::Rejected(Reason::Trust(TrustError::NotYetValid))
    );
}

#[test]
fn wrong_channel_and_missing_platform() {
    let mut rel = base_release();
    rel.channel = "beta".into();
    let out = run(&mut one_mirror_with(&rel), &state());
    assert_eq!(out.decision, Decision::Rejected(Reason::WrongChannel));

    // A newer release with no artifact for this platform is not an update
    // for us; the data refresh in the same manifest still goes through.
    let mut rel = base_release();
    rel.artifacts.retain(|a| a.platform != "linux-x86_64");
    let out = run(&mut one_mirror_with(&rel), &state());
    assert!(matches!(out.decision, Decision::DataRefresh { .. }));
}

// ---- schedule --------------------------------------------------------------------

#[test]
fn cadence_default_is_weekly_and_deterministic() {
    let c = cfg();
    assert_eq!(c.cadence_secs, 7 * 86_400);
    let mut s = state();
    s.last_check = NOW - 7 * DAY_SECS + 1;
    s.last_attempt = s.last_check;
    assert_eq!(due(&c, &s, NOW), Due::No);
    s.last_check = NOW - 7 * DAY_SECS;
    s.last_attempt = s.last_check;
    assert_eq!(due(&c, &s, NOW), Due::Cadence);
    // Same inputs, same answer, every time.
    assert_eq!(due(&c, &s, NOW), due(&c, &s, NOW));
    // Never checked: due.
    let mut never = state();
    never.last_check = 0;
    never.last_attempt = 0;
    assert_eq!(due(&c, &never, NOW), Due::Cadence);
    // Cadence 0 = manual only.
    let mut manual = cfg();
    manual.cadence_secs = 0;
    assert_eq!(due(&manual, &never, NOW), Due::No);
}

#[test]
fn not_due_means_no_network() {
    let mut s = state();
    s.last_check = NOW - DAY_SECS;
    s.last_attempt = s.last_check;
    let mut mock = one_mirror_with(&base_release());
    let out = run(&mut mock, &s);
    assert_eq!(out.decision, Decision::NotDue);
    assert!(mock.log.is_empty());
    assert_eq!(out.state, s);
}

#[test]
fn stale_data_file_forces_a_refresh_regardless_of_cadence() {
    // Checked an hour ago... no: checked a day ago, weekly cadence says no.
    let mut s = state();
    s.last_check = NOW - DAY_SECS;
    s.last_attempt = s.last_check;
    // PSL fetched 100 days ago.
    s.data_versions = [20_260_615, 20_260_909, 20_260_909];
    assert_eq!(due(&cfg(), &s, NOW), Due::StaleData(DataKind::Psl));
    let mut mock = one_mirror_with(&base_release());
    let out = run(&mut mock, &s);
    assert!(!mock.log.is_empty(), "stale data must reach the network");
    assert!(matches!(
        out.decision,
        Decision::UpdateAvailable { .. } | Decision::DataRefresh { .. }
    ));
    // Even with scheduled checks disabled.
    let mut c = cfg();
    c.cadence_secs = 0;
    assert_eq!(due(&c, &s, NOW), Due::StaleData(DataKind::Psl));
    // An unset data version counts as stale.
    let mut unset = state();
    unset.data_versions = [20_260_909, 0, 20_260_909];
    assert_eq!(due(&cfg(), &unset, NOW), Due::StaleData(DataKind::Hsts));
    // Boundary: exactly max age is not stale, one second over is.
    let edge = date_to_unix(20_260_909).unwrap() + 30 * DAY_SECS;
    let mut e = state();
    e.last_check = edge - 3600;
    e.last_attempt = edge - 7200;
    assert_eq!(due(&cfg(), &e, edge), Due::No);
    assert_eq!(due(&cfg(), &e, edge + 1), Due::StaleData(DataKind::Psl));
}

#[test]
fn retry_floor_protects_mirrors_even_from_stale_data() {
    let mut s = state();
    s.data_versions = [0; 3];
    s.last_attempt = NOW - 60;
    assert_eq!(due(&cfg(), &s, NOW), Due::No);
    let mut mock = one_mirror_with(&base_release());
    let out = check(
        &mut mock,
        &cfg(),
        &trust1(&[1], 1),
        &s,
        NOW,
        Trigger::Manual,
    );
    assert_eq!(out.decision, Decision::Rejected(Reason::TooSoon));
    assert!(mock.log.is_empty());
    // A clock that went backwards does not suppress a check.
    s.last_attempt = NOW + 10_000;
    assert_ne!(due(&cfg(), &s, NOW), Due::No);
}

#[test]
fn manual_trigger_skips_cadence() {
    let mut s = state();
    s.last_check = NOW - DAY_SECS;
    s.last_attempt = s.last_check;
    let mut mock = one_mirror_with(&base_release());
    let out = check(
        &mut mock,
        &cfg(),
        &trust1(&[1], 1),
        &s,
        NOW,
        Trigger::Manual,
    );
    assert!(matches!(out.decision, Decision::UpdateAvailable { .. }));
}

// ---- mirrors -------------------------------------------------------------------------

#[test]
fn fallback_order_and_bounded_failure_accounting() {
    let rel = base_release();
    let mut mock = Mock::default();
    mock.publish_release("m2.example.net", &rel, &[1]);
    // m1 has nothing: 404s.
    let out = run(&mut mock, &state());
    assert!(matches!(out.decision, Decision::UpdateAvailable { .. }));
    assert_eq!(out.state.mirror_failures, [1, 0]);
    // m1 was tried first, then m2.
    assert_eq!(mock.log.first().unwrap().0, "m1.example.net");
    assert!(mock.log.iter().any(|l| l.0 == "m2.example.net"));

    // Next time the healthier mirror goes first.
    let mut mock2 = Mock::default();
    mock2.publish_release("m2.example.net", &rel, &[1]);
    mock2.publish_release("m1.example.net", &rel, &[1]);
    let mut s2 = out.state.clone();
    s2.last_attempt = 0;
    let out2 = check(
        &mut mock2,
        &cfg(),
        &trust1(&[1], 1),
        &s2,
        NOW,
        Trigger::Manual,
    );
    assert_eq!(mock2.log.first().unwrap().0, "m2.example.net");
    assert!(matches!(out2.decision, Decision::UpdateAvailable { .. }));

    // Failures saturate at MAX_FAILURES; a success resets.
    let mut s = state();
    let mut dead = Mock::default();
    for i in 0..30u64 {
        s.last_attempt = 0;
        let o = check(
            &mut dead,
            &cfg(),
            &trust1(&[1], 1),
            &s,
            NOW + i,
            Trigger::Manual,
        );
        assert!(matches!(
            o.decision,
            Decision::Rejected(Reason::Transport(_))
        ));
        s = o.state;
    }
    assert_eq!(s.mirror_failures, [MAX_FAILURES, MAX_FAILURES]);
    s.last_attempt = 0;
    let mut mock3 = Mock::default();
    mock3.publish_release("m1.example.net", &rel, &[1]);
    let o = check(
        &mut mock3,
        &cfg(),
        &trust1(&[1], 1),
        &s,
        NOW + 100,
        Trigger::Manual,
    );
    assert!(matches!(o.decision, Decision::UpdateAvailable { .. }));
    assert_eq!(o.state.mirror_failures, [0, MAX_FAILURES]);
    // Vector length follows the configured mirrors, never grows without bound.
    assert_eq!(o.state.mirror_failures.len(), 2);
}

#[test]
fn a_mirror_serving_a_bad_manifest_is_charged_and_skipped() {
    let rel = base_release();
    let bytes = Manifest::Release(rel.clone()).encode().into_bytes();
    let mut mock = Mock::default();
    // m1: validly signed by an untrusted key. m2: good.
    mock.publish("m1.example.net", &bytes, &[9]);
    mock.publish_release("m2.example.net", &rel, &[1]);
    let out = run(&mut mock, &state());
    assert!(matches!(out.decision, Decision::UpdateAvailable { .. }));
    assert_eq!(out.state.mirror_failures, [1, 0]);
}

#[test]
fn all_mirrors_failing_is_a_rejection_that_records_no_progress() {
    let mut mock = Mock::default();
    mock.put(
        "m1.example.net",
        "manifest.txt",
        Err(TransportError::Connect),
    );
    mock.put(
        "m2.example.net",
        "manifest.txt",
        Err(TransportError::Truncated),
    );
    let before = state();
    let out = run(&mut mock, &before);
    assert!(matches!(
        out.decision,
        Decision::Rejected(Reason::Transport(_))
    ));
    assert_eq!(out.state.last_check, before.last_check);
    assert_eq!(out.state.max_sequence, before.max_sequence);
    assert_eq!(out.state.last_attempt, NOW);
    // The security-relevant reason wins over a plain network failure.
    let mut mock = Mock::default();
    mock.put(
        "m1.example.net",
        "manifest.txt",
        Err(TransportError::Connect),
    );
    let rel = base_release();
    let bytes = Manifest::Release(rel).encode().into_bytes();
    mock.publish("m2.example.net", &bytes, &[9]);
    let out = run(&mut mock, &before);
    assert!(matches!(out.decision, Decision::Rejected(Reason::Trust(_))));
}

#[test]
fn no_mirrors_or_too_many_fails_closed() {
    let mut c = cfg();
    c.mirrors.clear();
    let out = check(
        &mut Mock::default(),
        &c,
        &trust1(&[1], 1),
        &state(),
        NOW,
        Trigger::Manual,
    );
    assert_eq!(out.decision, Decision::Rejected(Reason::NoMirrors));
    let mut c = cfg();
    c.mirrors = (0..=MAX_MIRRORS).map(|_| mirrors()[0].clone()).collect();
    let out = check(
        &mut Mock::default(),
        &c,
        &trust1(&[1], 1),
        &state(),
        NOW,
        Trigger::Manual,
    );
    assert_eq!(out.decision, Decision::Rejected(Reason::NoMirrors));
}

#[test]
fn invalid_mirror_config_never_reaches_the_transport() {
    let mut c = cfg();
    c.mirrors = vec![Mirror {
        host: "Bad Host".into(),
        port: 443,
        base: "/v1".into(),
    }];
    let mut mock = Mock::default();
    let out = check(
        &mut mock,
        &c,
        &trust1(&[1], 1),
        &state(),
        NOW,
        Trigger::Manual,
    );
    assert!(matches!(
        out.decision,
        Decision::Rejected(Reason::Request(_))
    ));
    assert!(mock.log.is_empty());
}

// ---- rotation through the client ------------------------------------------------------

fn rotation_to(new_seed: u8) -> (Vec<u8>, String) {
    let bytes = Manifest::Rotation(Rotation {
        key_epoch: 2,
        issued: NOW - 1000,
        expires: NOW + 6 * DAY_SECS,
        threshold: 1,
        keys: vec![pk(new_seed)],
    })
    .encode()
    .into_bytes();
    let sig = sigfile_text(&bytes, &[1]);
    (bytes, sig)
}

#[test]
fn client_follows_a_rotation_then_accepts_the_new_key() {
    let mut rel = base_release();
    rel.key_epoch = 2;
    let mut mock = Mock::default();
    mock.publish_release("m1.example.net", &rel, &[2]);
    let (rb, rs) = rotation_to(2);
    mock.put("m1.example.net", "rotation-2.txt", Ok(rb));
    mock.put("m1.example.net", "rotation-2.sig", Ok(rs.into_bytes()));
    let out = run(&mut mock, &state());
    assert!(
        matches!(out.decision, Decision::UpdateAvailable { .. }),
        "{:?}",
        out.decision
    );
    assert_eq!(out.trust.epoch(), 2);
    assert_eq!(out.trust.revoked(), &[pk(1)]);
    // Persisting the new trust: the old key is now dead.
    let mut rel_old = base_release();
    rel_old.key_epoch = 2;
    let mut m2 = Mock::default();
    m2.publish_release("m1.example.net", &rel_old, &[1]);
    let o = check(
        &mut m2,
        &cfg(),
        &out.trust,
        &state(),
        NOW,
        Trigger::Scheduled,
    );
    assert!(matches!(o.decision, Decision::Rejected(Reason::Trust(_))));
}

#[test]
fn forged_rotation_does_not_move_trust() {
    // An attacker signs a rotation with a key the client does not trust.
    let mut rel = base_release();
    rel.key_epoch = 2;
    let mut mock = Mock::default();
    mock.publish_release("m1.example.net", &rel, &[7]);
    let bytes = Manifest::Rotation(Rotation {
        key_epoch: 2,
        issued: NOW - 1000,
        expires: NOW + 6 * DAY_SECS,
        threshold: 1,
        keys: vec![pk(7)],
    })
    .encode()
    .into_bytes();
    let sig = sigfile_text(&bytes, &[7]);
    mock.put("m1.example.net", "rotation-2.txt", Ok(bytes));
    mock.put("m1.example.net", "rotation-2.sig", Ok(sig.into_bytes()));
    let out = run(&mut mock, &state());
    assert!(matches!(out.decision, Decision::Rejected(Reason::Trust(_))));
    assert_eq!(out.trust.epoch(), 1);
}

// ---- downloads ------------------------------------------------------------------------

#[test]
fn download_verification() {
    let f = file_for("data/x.bin", b"hello world");
    assert_eq!(verify_download(&f, b"hello world"), Ok(()));
    assert_eq!(
        verify_download(&f, b"hello worl"),
        Err(DownloadError::SizeMismatch)
    );
    assert_eq!(
        verify_download(&f, b"hello world!"),
        Err(DownloadError::SizeMismatch)
    );
    assert_eq!(
        verify_download(&f, b"hello worle"),
        Err(DownloadError::DigestMismatch)
    );
    // Both digests must match, not either.
    let mut g = f.clone();
    g.sha512[0] ^= 1;
    assert_eq!(
        verify_download(&g, b"hello world"),
        Err(DownloadError::DigestMismatch)
    );
    let mut h = f.clone();
    h.sha256[0] ^= 1;
    assert_eq!(
        verify_download(&h, b"hello world"),
        Err(DownloadError::DigestMismatch)
    );
}

#[test]
fn download_never_returns_unverified_bytes_and_falls_back() {
    let f = file_for("data/x.bin", b"hello world");
    let mut st = state();
    st.mirror_failures = vec![0, 0];
    // m1: corrupted. m2: truncated-then-good is not possible; m2 is good.
    let mut mock = Mock::default();
    mock.put("m1.example.net", "data/x.bin", Ok(b"hello worle".to_vec()));
    mock.put("m2.example.net", "data/x.bin", Ok(b"hello world".to_vec()));
    assert_eq!(
        download(&mut mock, &cfg(), &mut st, &f).unwrap(),
        b"hello world"
    );
    assert_eq!(st.mirror_failures, [1, 0]);
    // Every attempt was capped at the declared size.
    assert!(mock.log.iter().all(|l| l.2 == 11));

    // Truncated and oversized bodies from every mirror: error, no bytes.
    let mut mock = Mock::default();
    mock.put("m1.example.net", "data/x.bin", Ok(b"hello".to_vec()));
    mock.put(
        "m2.example.net",
        "data/x.bin",
        Ok(b"hello world and then some".to_vec()),
    );
    let mut st = state();
    let err = download(&mut mock, &cfg(), &mut st, &f).unwrap_err();
    assert!(matches!(
        err,
        DownloadError::Transport(TransportError::TooLarge) | DownloadError::SizeMismatch
    ));
    // A transport that ignores the cap is caught anyway.
    struct Greedy;
    impl Transport for Greedy {
        fn fetch(&mut self, _: &Mirror, _: &Request, _: u64) -> Reply {
            Ok(vec![0u8; 1 << 20])
        }
    }
    let mut st = state();
    assert_eq!(
        download(&mut Greedy, &cfg(), &mut st, &f),
        Err(DownloadError::Transport(TransportError::TooLarge))
    );
}

#[test]
fn download_with_no_mirrors_fails_closed() {
    let mut c = cfg();
    c.mirrors.clear();
    let f = file_for("a/b", b"x");
    assert_eq!(
        download(&mut Mock::default(), &c, &mut state(), &f),
        Err(DownloadError::NoMirrors)
    );
}
