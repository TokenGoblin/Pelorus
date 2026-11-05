//! Phase 14 (ADR 045), the broker's frame tree, process key, pool and routing.
//!
//! Headless and deterministic: no process is spawned. A "process" here is an
//! open channel; the pid-level checks wait for the plan's later units.

use px_broker::{
    Broker, ChannelId, CommitPlan, DenyReason, FrameState, MAX_CONTENT_PROCESSES, MAX_FRAME_DEPTH,
    PoolLookup, ProcessKey,
};
use px_ipc::frames::{FrameEvent, FrameRequest, FrameResponse, NavigateUrl, NavigationKind};
use px_ipc::{FrameHost, FrameId, Request, Response, encode_frame};
use px_websec::origin::{Origin, Url};

/// Toy public suffix list: the last two labels.
fn psl(host: &str) -> Option<String> {
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    Some(labels[labels.len() - 2..].join("."))
}

fn origin(url: &str) -> Origin {
    Origin::from_url(&Url::parse(url).expect("url"))
}

fn channel(broker: &mut Broker) -> ChannelId {
    broker.open_channel().expect("channel")
}

fn navigate(frame: FrameId, url: &str) -> FrameRequest {
    FrameRequest::Navigate {
        frame,
        url: NavigateUrl::new(url.to_string()).expect("short"),
        kind: NavigationKind::Push,
    }
}

/// Commit `url` in `frame`, spawning (opening a channel) when asked.
fn commit_and_place(broker: &mut Broker, frame: FrameId, url: &str) -> ChannelId {
    match broker
        .commit_document(frame, origin(url), &psl)
        .expect("commit")
    {
        CommitPlan::InPlace => broker.frames().owner(frame).expect("owner"),
        CommitPlan::Transferred(t) => t.to,
        CommitPlan::NeedsProcess(key) => {
            let process = channel(broker);
            broker.attach_process(frame, key, process).expect("attach");
            process
        }
        other => panic!("unexpected plan {other:?}"),
    }
}

#[test]
fn oopif_key_ip_literal_top_level_yields_no_key() {
    let ip = origin("http://192.168.1.1/");
    assert!(ProcessKey::new(&ip, &ip, &psl).is_none());
    let localhost = origin("http://localhost/");
    assert!(ProcessKey::new(&localhost, &localhost, &psl).is_none());
    // An opaque top level has no site either.
    let opaque = Origin::new_opaque();
    assert!(ProcessKey::new(&opaque, &opaque, &psl).is_none());

    let mut broker = Broker::new();
    let ch = channel(&mut broker);
    let top = broker.create_frame(ch).expect("frame");
    assert_eq!(
        broker.commit_document(top, ip, &psl),
        Err(DenyReason::NoProcessKey)
    );
}

#[test]
fn oopif_key_distinguishes_origin_and_top_level_site() {
    let a = origin("https://www.example.com/");
    let a2 = origin("https://a.example.com/");
    let b = origin("https://other.org/");
    let embed = origin("https://cdn.third.net/");
    let k_www = ProcessKey::new(&a, &a, &psl).expect("key");
    let k_a = ProcessKey::new(&a, &a2, &psl).expect("key");
    assert_ne!(k_www, k_a, "same site, different origin: different key");
    let under_a = ProcessKey::new(&a, &embed, &psl).expect("key");
    let under_b = ProcessKey::new(&b, &embed, &psl).expect("key");
    assert_ne!(
        under_a, under_b,
        "one third party under two top-level sites: two keys"
    );
    assert_eq!(under_a, ProcessKey::new(&a2, &embed, &psl).expect("key"));
}

#[test]
fn oopif_pool_same_key_and_group_share_everything_else_does_not() {
    let mut broker = Broker::new();
    let p1 = channel(&mut broker);
    let top = broker.create_frame(p1).expect("top");
    assert_eq!(
        commit_and_place(&mut broker, top, "https://www.example.com/"),
        p1
    );

    // Same origin child: same key, same group, same process.
    let same = broker.create_child_frame(p1, top, 0).expect("child");
    assert_eq!(
        broker.commit_document(same, origin("https://www.example.com/x"), &psl),
        Ok(CommitPlan::InPlace)
    );

    // Same site, different origin: its own process.
    let sibling = broker.create_child_frame(p1, top, 1).expect("child");
    let p2 = commit_and_place(&mut broker, sibling, "https://a.example.com/");
    assert_ne!(p2, p1);

    // A second child of the same origin shares p2.
    let again = broker.create_child_frame(p1, top, 2).expect("child");
    assert_eq!(
        commit_and_place(&mut broker, again, "https://a.example.com/"),
        p2
    );

    // The same third party under a different top-level site: a new process.
    let q1 = channel(&mut broker);
    let top_b = broker.create_frame(q1).expect("top");
    commit_and_place(&mut broker, top_b, "https://other.org/");
    let embed_b = broker.create_child_frame(q1, top_b, 0).expect("child");
    let third = commit_and_place(&mut broker, embed_b, "https://a.example.com/");
    assert_ne!(third, p2, "same origin, different top-level site");
    assert_ne!(third, q1);

    // The same page in a second tab (a second group) gets its own processes.
    let r1 = channel(&mut broker);
    let tab2 = broker.create_frame(r1).expect("top");
    let r = commit_and_place(&mut broker, tab2, "https://www.example.com/");
    assert_eq!(r, r1, "a lone first commit claims its own process");
    assert_ne!(
        broker.frames().group(tab2),
        broker.frames().group(top),
        "tabs are different groups"
    );
    assert_ne!(r1, p1);
}

#[test]
fn oopif_pool_over_the_cap_fails_closed_and_shares_nothing() {
    let mut broker = Broker::new();
    let mut first_top = None;
    let mut first_channel = None;
    for i in 0..MAX_CONTENT_PROCESSES {
        let ch = channel(&mut broker);
        let top = broker.create_frame(ch).expect("top");
        let url = format!("https://site{i}.example/");
        commit_and_place(&mut broker, top, &url);
        first_top.get_or_insert(top);
        first_channel.get_or_insert(ch);
    }
    assert_eq!(broker.pool().len(), MAX_CONTENT_PROCESSES);

    // A 65th top-level site is refused rather than parked in a shared process.
    let extra = channel(&mut broker);
    let top = broker.create_frame(extra).expect("top");
    assert!(
        broker
            .commit_document(top, origin("https://overflow.example/"), &psl)
            .is_err()
    );

    // A cross-origin child of an existing page gets a placeholder plan.
    let (top0, ch0) = (first_top.expect("top"), first_channel.expect("ch"));
    let child = broker.create_child_frame(ch0, top0, 0).expect("child");
    assert_eq!(
        broker.commit_document(child, origin("https://embedded.example/"), &psl),
        Ok(CommitPlan::AtCap)
    );
    assert_eq!(broker.frames().owner(child), Some(ch0), "nothing moved");
    assert_eq!(broker.pool().len(), MAX_CONTENT_PROCESSES);
}

#[test]
fn oopif_swap_old_channel_is_denied_after_transfer() {
    let mut broker = Broker::new();
    let top_ch = channel(&mut broker);
    let top = broker.create_frame(top_ch).expect("top");
    commit_and_place(&mut broker, top, "https://www.example.com/");
    let child = broker.create_child_frame(top_ch, top, 0).expect("child");
    let child_ch = commit_and_place(&mut broker, child, "https://evil.test/");
    assert_ne!(child_ch, top_ch);

    // The embedder sees the child as remote; the new owner hosts it.
    let ask = |b: &mut Broker, ch, f| b.dispatch(ch, Request::FrameHost { frame: f }).response;
    assert_eq!(
        ask(&mut broker, top_ch, child),
        Response::FrameHost {
            host: FrameHost::Remote
        }
    );
    assert_eq!(
        ask(&mut broker, child_ch, child),
        Response::FrameHost {
            host: FrameHost::Local
        }
    );
    // The new process was never told about the parent.
    assert_eq!(ask(&mut broker, child_ch, top), Response::Denied);

    // A navigation that moves the child again strips the previous owner of
    // every trace of it, since it does not host the child's parent.
    let leaf = broker.create_child_frame(child_ch, child, 0).expect("leaf");
    let leaf_ch = commit_and_place(&mut broker, leaf, "https://leaf.net/");
    assert_eq!(
        ask(&mut broker, leaf_ch, leaf),
        ask(&mut broker, leaf_ch, leaf)
    );
    assert_eq!(
        ask(&mut broker, child_ch, leaf),
        Response::FrameHost {
            host: FrameHost::Remote
        },
        "its embedder still sees it"
    );
    assert_eq!(
        ask(&mut broker, top_ch, leaf),
        Response::Denied,
        "the grandparent's process was never granted the leaf"
    );
}

#[test]
fn oopif_forged_frame_ids_all_get_the_same_denied_bytes() {
    let mut broker = Broker::new();
    let victim = channel(&mut broker);
    let attacker = channel(&mut broker);
    let top = broker.create_frame(victim).expect("top");
    commit_and_place(&mut broker, top, "https://www.example.com/");
    for ordinal in 0..4 {
        broker
            .create_child_frame(victim, top, ordinal)
            .expect("child");
    }

    let denied = encode_frame(&FrameResponse::Denied).expect("encode");
    for index in 0..64u32 {
        for generation in 0..3u32 {
            let forged = FrameId::new(index, generation);
            let (reply, _) = broker.dispatch_frame(
                attacker,
                FrameRequest::CreateChildFrame {
                    parent: forged,
                    element_ordinal: 0,
                },
            );
            assert_eq!(encode_frame(&reply).expect("encode"), denied);
            let (reply, _) = broker.dispatch_frame(attacker, navigate(forged, "https://a.test/"));
            assert_eq!(encode_frame(&reply).expect("encode"), denied);
        }
    }
    // And nothing was created or moved by the sweep.
    assert_eq!(broker.frames().owner(top), Some(victim));
}

#[test]
fn oopif_depth_nine_is_refused_and_depth_five_chains() {
    let mut broker = Broker::new();
    let mut ch = channel(&mut broker);
    let mut frame = broker.create_frame(ch).expect("top");
    commit_and_place(&mut broker, frame, "https://site0.example/");
    let mut channels = vec![ch];
    for level in 1..=5 {
        let child = broker.create_child_frame(ch, frame, 0).expect("child");
        let next = commit_and_place(&mut broker, child, &format!("https://site{level}.example/"));
        channels.push(next);
        ch = next;
        frame = child;
    }
    assert_eq!(broker.frames().depth(frame), Some(5));
    let mut distinct = channels.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 6, "six origins, six processes");

    // Keep nesting inside one process (same origin) until the limit.
    let mut depth = 5u8;
    while depth < MAX_FRAME_DEPTH {
        let child = broker.create_child_frame(ch, frame, 0).expect("child");
        frame = child;
        depth += 1;
        assert_eq!(broker.frames().depth(frame), Some(depth));
    }
    assert_eq!(
        broker.create_child_frame(ch, frame, 0),
        Err(DenyReason::TooDeep)
    );
    let (reply, audit) = broker.dispatch_frame(
        ch,
        FrameRequest::CreateChildFrame {
            parent: frame,
            element_ordinal: 1,
        },
    );
    assert_eq!(reply, FrameResponse::Denied);
    assert_eq!(audit, Some(DenyReason::TooDeep));
}

#[test]
fn oopif_child_creation_requires_hosting_the_parent_and_a_free_ordinal() {
    let mut broker = Broker::new();
    let a = channel(&mut broker);
    let b = channel(&mut broker);
    let top = broker.create_frame(a).expect("top");
    assert_eq!(
        broker.create_child_frame(b, top, 0),
        Err(DenyReason::NotYourFrame)
    );
    assert!(broker.create_child_frame(a, top, 0).is_ok());
    assert_eq!(
        broker.create_child_frame(a, top, 0),
        Err(DenyReason::FrameLimit)
    );
}

#[test]
fn oopif_navigate_is_authorised_for_the_frame_or_an_ancestor_only() {
    let mut broker = Broker::new();
    let top_ch = channel(&mut broker);
    let top = broker.create_frame(top_ch).expect("top");
    commit_and_place(&mut broker, top, "https://www.example.com/");
    let child = broker.create_child_frame(top_ch, top, 0).expect("child");
    let child_ch = commit_and_place(&mut broker, child, "https://evil.test/");

    let ok = |r: (FrameResponse, Option<DenyReason>)| r.0 == FrameResponse::NavigationAuthorised;
    assert!(ok(
        broker.dispatch_frame(top_ch, navigate(child, "https://x.test/"))
    ));
    assert!(ok(
        broker.dispatch_frame(child_ch, navigate(child, "https://x.test/"))
    ));

    // The child cannot navigate its parent or the top.
    let (reply, audit) = broker.dispatch_frame(child_ch, navigate(top, "https://x.test/"));
    assert_eq!(reply, FrameResponse::Denied);
    assert!(audit.is_some());
    // Unparsable targets are refused.
    let (_, audit) = broker.dispatch_frame(top_ch, navigate(child, "not a url"));
    assert_eq!(audit, Some(DenyReason::BadUrl));
}

#[test]
fn oopif_crash_marks_frame_crashed_and_destroys_its_subtree() {
    let mut broker = Broker::new();
    let top_ch = channel(&mut broker);
    let top = broker.create_frame(top_ch).expect("top");
    commit_and_place(&mut broker, top, "https://www.example.com/");
    let mid = broker.create_child_frame(top_ch, top, 0).expect("mid");
    let mid_ch = commit_and_place(&mut broker, mid, "https://mid.test/");
    let leaf = broker.create_child_frame(mid_ch, mid, 0).expect("leaf");
    let leaf_ch = commit_and_place(&mut broker, leaf, "https://leaf.test/");
    let sibling = broker.create_child_frame(top_ch, top, 1).expect("sibling");
    let sibling_ch = commit_and_place(&mut broker, sibling, "https://sib.test/");

    let events = broker.channel_died(mid_ch);
    assert_eq!(
        events,
        vec![(top_ch, FrameEvent::FrameCrashed { frame: mid })],
        "only the embedder is told, and only about the crashed frame"
    );
    assert_eq!(broker.frames().state(mid), Some(FrameState::Crashed));
    assert_eq!(broker.frames().state(leaf), None, "the subtree is gone");
    assert_eq!(broker.frames().state(top), Some(FrameState::Live));
    assert_eq!(broker.frames().state(sibling), Some(FrameState::Live));
    assert_eq!(broker.frames().owner(sibling), Some(sibling_ch));
    assert!(!broker.is_open(mid_ch));
    assert!(broker.is_open(leaf_ch), "other processes are untouched");

    // The id stays stable but nothing hosts it, and it cannot be created into
    // or navigated by guesswork.
    assert!(!broker.holds_frame(mid_ch, mid));
    assert_eq!(
        broker.create_child_frame(top_ch, mid, 0),
        Err(DenyReason::NoSuchFrame)
    );

    // A parent dying takes its descendants with it.
    let events = broker.channel_died(top_ch);
    assert!(events.is_empty(), "no surviving embedder to tell");
    assert_eq!(broker.frames().state(mid), None);
    assert_eq!(broker.frames().state(sibling), None);
    assert_eq!(broker.frames().state(top), Some(FrameState::Crashed));
}

#[test]
fn oopif_dead_key_can_be_served_by_a_new_process() {
    let mut broker = Broker::new();
    let p = channel(&mut broker);
    let top = broker.create_frame(p).expect("top");
    commit_and_place(&mut broker, top, "https://www.example.com/");
    let key = broker
        .frames()
        .process_key(top, &psl)
        .expect("key for a committed top");
    assert!(matches!(
        broker.pool().lookup(broker.frames().group(top).expect("g"), &key),
        PoolLookup::Share(c) if c == p
    ));
    broker.channel_died(p);
    assert!(matches!(
        broker
            .pool()
            .lookup(broker.frames().group(top).expect("g"), &key),
        PoolLookup::Spawn
    ));
}

#[test]
fn oopif_broker_does_not_link_rustls() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = std::process::Command::new(cargo)
        .args([
            "tree",
            "-p",
            "px-broker",
            "-e",
            "normal",
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree must run: a test that cannot check fails closed");
    assert!(output.status.success(), "{output:?}");
    let tree = String::from_utf8_lossy(&output.stdout);
    assert!(tree.contains("px-websec"), "the key's crate edge");
    for banned in ["rustls", "ring ", "px-net", "webpki"] {
        assert!(
            !tree.contains(banned),
            "{banned} reached px-broker:\n{tree}"
        );
    }
}
