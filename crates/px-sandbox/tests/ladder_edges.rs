//! The sandbox ladder's admission rule, from outside the crate.
//!
//! Nothing here spawns a process or needs a privilege: it builds
//! `Capabilities` values by hand and checks `admit`'s decision, so the rule
//! "refuse below the floor, never degrade silently" is pinned independently of
//! what this machine happens to offer.

use px_sandbox::{Capabilities, FLOOR, Refusal, Restriction, Rung, admit, detect};

const ALL_RUNGS: [Rung; 6] = [
    Rung::NoNewPrivs,
    Rung::Seccomp,
    Rung::Landlock,
    Rung::UserNamespaces,
    Rung::RestrictedToken,
    Rung::JobObject,
];

const ALL_RESTRICTIONS: [Restriction; 5] = [
    Restriction::UnprivilegedUsernsClone,
    Restriction::ApparmorRestrictUserns,
    Restriction::MaxUserNamespacesZero,
    Restriction::NotSupported,
    Restriction::Inconclusive,
];

fn refusal_for(available: &[Rung]) -> Option<Refusal> {
    admit(&Capabilities::with_available(available)).err()
}

#[test]
fn sandbox_floor_is_never_empty_and_has_no_duplicates() {
    assert!(
        !FLOOR.is_empty(),
        "an empty floor admits an unsandboxed child"
    );
    for (index, rung) in FLOOR.iter().enumerate() {
        assert!(
            !FLOOR.iter().skip(index + 1).any(|other| other == rung),
            "{rung} listed twice"
        );
    }
}

#[test]
fn sandbox_floor_names_the_documented_rungs_for_this_platform() {
    // ADR 007 / build-spec: Linux needs no_new_privs + seccomp, Windows needs a
    // restricted token + a job object. Landlock is above the floor on Linux.
    if cfg!(target_os = "linux") {
        assert_eq!(FLOOR, &[Rung::NoNewPrivs, Rung::Seccomp]);
        assert!(!FLOOR.contains(&Rung::Landlock));
        assert!(!FLOOR.contains(&Rung::UserNamespaces));
    } else if cfg!(target_os = "windows") {
        assert_eq!(FLOOR, &[Rung::RestrictedToken, Rung::JobObject]);
    }
}

#[test]
fn sandbox_rung_names_are_distinct_and_display_matches() {
    let names: Vec<&str> = ALL_RUNGS.iter().map(|r| r.name()).collect();
    for (index, name) in names.iter().enumerate() {
        assert!(!name.is_empty());
        assert!(
            !names.iter().skip(index + 1).any(|other| other == name),
            "duplicate rung name {name}"
        );
    }
    for rung in ALL_RUNGS {
        assert_eq!(rung.to_string(), rung.name());
    }
}

#[test]
fn sandbox_no_capabilities_means_refusal_naming_the_entire_floor_in_order() {
    let refusal = refusal_for(&[]).expect("nothing offered must be refused");
    assert_eq!(refusal.missing(), FLOOR);
}

#[test]
fn sandbox_each_floor_rung_missing_alone_is_named_alone() {
    for missing in FLOOR {
        let offered: Vec<Rung> = ALL_RUNGS
            .iter()
            .copied()
            .filter(|rung| rung != missing)
            .collect();
        let refusal = refusal_for(&offered)
            .unwrap_or_else(|| unreachable!("{missing} was withheld yet the launch was admitted"));
        assert_eq!(refusal.missing(), &[*missing]);
    }
}

/// Extra rungs from another platform, or above the floor, must never stand in
/// for a floor rung: that is how a probe that "wrongly reports success"
/// launches an unsandboxed process.
#[test]
fn sandbox_rungs_outside_the_floor_never_substitute_for_it() {
    let extras: Vec<Rung> = ALL_RUNGS
        .iter()
        .copied()
        .filter(|rung| !FLOOR.contains(rung))
        .collect();
    assert!(!extras.is_empty());
    let caps = Capabilities::with_available(&extras);
    assert!(!caps.clears_floor());
    assert!(admit(&caps).is_err());
}

#[test]
fn sandbox_admission_returns_exactly_the_offered_rungs_in_offer_order() {
    let mut offered: Vec<Rung> = vec![Rung::Landlock];
    offered.extend(FLOOR.iter().rev().copied());
    offered.push(Rung::UserNamespaces);
    let applied = admit(&Capabilities::with_available(&offered)).expect("floor cleared");
    assert_eq!(applied, offered);
}

#[test]
fn sandbox_offering_a_rung_twice_does_not_duplicate_it() {
    let caps = Capabilities::with_available(&[Rung::Seccomp, Rung::Seccomp, Rung::NoNewPrivs]);
    assert_eq!(caps.available(), &[Rung::Seccomp, Rung::NoNewPrivs]);
    assert!(caps.has(Rung::Seccomp));
    assert!(!caps.has(Rung::Landlock));
}

#[test]
fn sandbox_clears_floor_agrees_with_admit_for_every_subset() {
    for mask in 0u32..(1 << ALL_RUNGS.len()) {
        let offered: Vec<Rung> = ALL_RUNGS
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, rung)| *rung)
            .collect();
        let caps = Capabilities::with_available(&offered);
        let expected = FLOOR.iter().all(|rung| offered.contains(rung));
        assert_eq!(caps.clears_floor(), expected, "{offered:?}");
        assert_eq!(admit(&caps).is_ok(), expected, "{offered:?}");
        assert_eq!(caps.missing_from_floor().is_empty(), expected);
    }
}

#[test]
fn sandbox_hand_built_capabilities_carry_no_restriction_notes() {
    let caps = Capabilities::with_available(&[]);
    for rung in ALL_RUNGS {
        assert_eq!(caps.restriction(rung), None);
    }
}

#[test]
fn sandbox_refusal_message_names_every_missing_rung_and_explains_why() {
    let refusal = refusal_for(&[]).expect("refused");
    let text = refusal.to_string();
    assert!(text.starts_with("refusing to launch a content process: "));
    for rung in FLOOR {
        assert!(
            text.contains(&format!("{rung} is unavailable")),
            "message must name {rung}: {text}"
        );
    }
    assert!(text.contains("untrusted code"));
    // No restriction was recorded, so no remedy is invented: the message must
    // not name a sysctl it did not check (sandbox CLAUDE.md).
    assert!(!text.contains("sysctl"), "{text}");
}

#[test]
fn sandbox_refusal_for_a_single_rung_has_no_list_separator() {
    let Some(first) = FLOOR.first().copied() else {
        unreachable!("floor is non-empty")
    };
    let offered: Vec<Rung> = FLOOR.iter().copied().filter(|r| *r != first).collect();
    let text = refusal_for(&offered).expect("refused").to_string();
    assert!(text.contains(&format!("{first} is unavailable")));
    assert!(!text.contains("unavailable, "), "{text}");
}

#[test]
fn sandbox_remedies_are_distinct_actionable_and_name_the_right_knob() {
    let remedies: Vec<&str> = ALL_RESTRICTIONS.iter().map(|r| r.remedy()).collect();
    for (index, remedy) in remedies.iter().enumerate() {
        assert!(!remedy.is_empty());
        assert!(
            !remedies.iter().skip(index + 1).any(|other| other == remedy),
            "two restrictions share the remedy {remedy:?}"
        );
    }
    // The restriction "is spelled three different ways across distributions";
    // each remedy must name its own sysctl and not a neighbour's.
    assert!(
        Restriction::UnprivilegedUsernsClone
            .remedy()
            .contains("kernel.unprivileged_userns_clone")
    );
    assert!(
        Restriction::ApparmorRestrictUserns
            .remedy()
            .contains("kernel.apparmor_restrict_unprivileged_userns")
    );
    assert!(
        Restriction::MaxUserNamespacesZero
            .remedy()
            .contains("user.max_user_namespaces")
    );
    assert!(!Restriction::NotSupported.remedy().contains("sysctl"));
    assert!(Restriction::Inconclusive.remedy().contains("unavailable"));
}

/// `detect` only probes; running it needs no privilege. Whatever it reports
/// must be self-consistent with the admission rule.
#[test]
fn sandbox_detect_is_deterministic_and_consistent_with_admit() {
    let first = detect();
    let second = detect();
    assert_eq!(first, second, "probes must not flap between calls");
    assert_eq!(admit(&first).is_ok(), first.clears_floor());
    for (index, rung) in first.available().iter().enumerate() {
        assert!(
            !first.available().iter().skip(index + 1).any(|o| o == rung),
            "detect reported {rung} twice"
        );
    }
}
