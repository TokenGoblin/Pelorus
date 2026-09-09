#![deny(unsafe_op_in_unsafe_fn)]

//! Process sandboxing, and the audited unsafe core (ADR 008).
//!
//! # What is here, and what is not, as of Phase 2
//!
//! **Here:** capability detection and the refusal path. Which rungs of the
//! ladder this machine offers, whether that clears the floor, and — when it
//! does not — a refusal that names the mechanism actually missing and the
//! remedy for the condition actually detected.
//!
//! **Not here yet:** applying a policy. That needs `prctl`/seccomp on Linux and
//! `CreateProcessAsUserW` on Windows, and the Phase 2 gate keeps failing until
//! it exists. Detection is deliberately separate because it is pure, testable
//! on both platforms, and contains no `unsafe` at all — every probe below is a
//! file read.
//!
//! # The ladder (ADR 007)
//!
//! §14.5 frames the Linux question as binary: sandbox available or not, SUID
//! helper or refuse to run. That framing is wrong. On Linux the sandbox is four
//! mechanisms with independent availability, and **only user namespaces is
//! commonly restricted**. Treating its absence as "no sandbox" discards three
//! that still work — and refuses in exactly the scenario §14.5 exists to
//! survive, which is what drives a user to `--no-sandbox`.
//!
//! So: apply every rung available, record which applied, refuse below a floor.
//!
//! # Probes fail closed
//!
//! A probe that wrongly reports *success* launches an unsandboxed process
//! believing it is sandboxed. A probe that wrongly reports failure refuses to
//! start. The second is recoverable and the first is not, so anything
//! inconclusive — a missing file, an unreadable file, an unparseable value —
//! counts as unavailable.

use std::fmt;

#[cfg(target_os = "linux")]
mod linux;
pub mod roots;
#[cfg(target_os = "windows")]
mod windows;

/// One mechanism the platform may offer.
///
/// Named per platform rather than abstracted into "filesystem isolation" and
/// friends: an operator reading a refusal needs the name their kernel or their
/// documentation uses, not ours.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rung {
    /// Linux: a process cannot gain privileges through `execve`.
    NoNewPrivs,
    /// Linux: syscall filtering.
    Seccomp,
    /// Linux: filesystem restriction. Kernel 5.13+.
    Landlock,
    /// Linux: mount, PID and network isolation. The one commonly restricted.
    UserNamespaces,
    /// Windows: a token with privileges and SIDs removed.
    RestrictedToken,
    /// Windows: the object that makes killing a process tree possible, and
    /// that carries the memory cap at Phase 17.
    JobObject,
}

impl Rung {
    /// The name to print. Deliberately the platform's spelling.
    pub fn name(self) -> &'static str {
        match self {
            Self::NoNewPrivs => "no_new_privs",
            Self::Seccomp => "seccomp-bpf",
            Self::Landlock => "Landlock",
            Self::UserNamespaces => "user namespaces",
            Self::RestrictedToken => "restricted token",
            Self::JobObject => "job object",
        }
    }
}

impl fmt::Display for Rung {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The minimum that must be present for a content process to launch (ADR 007).
///
/// Below this, refuse. This is invariant 8 made specific: "if a security
/// control cannot be applied, the operation does not proceed" needs a
/// definition of *the* control, and this is it — in one place, so Phase 17 can
/// raise it in one edit.
pub const FLOOR: &[Rung] = if cfg!(target_os = "linux") {
    &[Rung::NoNewPrivs, Rung::Seccomp]
} else if cfg!(target_os = "windows") {
    &[Rung::RestrictedToken, Rung::JobObject]
} else {
    // An unknown platform has no floor we can verify, so nothing clears it.
    // Fail closed by construction rather than by remembering to.
    &[Rung::NoNewPrivs, Rung::Seccomp, Rung::Landlock]
};

/// What this machine offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    available: Vec<Rung>,
    /// Why a rung is missing, when the platform told us something specific.
    /// Keyed by rung, and used to produce a remedy for the condition actually
    /// detected rather than a guess.
    notes: Vec<(Rung, Restriction)>,
}

/// Why a mechanism is unavailable, specifically enough to act on.
///
/// §14.5 says to name the sysctl. There is no single sysctl: the restriction is
/// spelled three different ways depending on the distribution, and naming the
/// wrong one sends a user to edit a setting that does not exist on their
/// machine. So the remedy comes from what was actually observed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Restriction {
    /// Debian and derivatives, historically.
    UnprivilegedUsernsClone,
    /// Recent Ubuntu.
    ApparmorRestrictUserns,
    /// RHEL-family and hardened kernels.
    MaxUserNamespacesZero,
    /// The kernel does not offer it at all.
    NotSupported,
    /// The probe could not reach a conclusion. Counts as unavailable.
    Inconclusive,
}

impl Restriction {
    /// What to tell the user, for the condition actually detected.
    pub fn remedy(self) -> &'static str {
        match self {
            Self::UnprivilegedUsernsClone => {
                "enable with: sysctl -w kernel.unprivileged_userns_clone=1"
            }
            Self::ApparmorRestrictUserns => {
                "enable with: sysctl -w kernel.apparmor_restrict_unprivileged_userns=0"
            }
            Self::MaxUserNamespacesZero => "enable with: sysctl -w user.max_user_namespaces=N",
            Self::NotSupported => "this kernel does not provide it; a newer kernel is required",
            Self::Inconclusive => {
                "the check could not reach a conclusion, so it is treated as unavailable"
            }
        }
    }
}

impl Capabilities {
    /// Build a capability set directly.
    ///
    /// For callers that know what a machine offers without probing it —
    /// tests, and eventually the spawn path reporting back which rungs it
    /// actually managed to apply, which is not the same question as which
    /// ones were detected.
    pub fn with_available(available: &[Rung]) -> Self {
        let mut caps = Self::default();
        for rung in available {
            caps.offer(*rung);
        }
        caps
    }

    /// Whether a rung is available.
    pub fn has(&self, rung: Rung) -> bool {
        self.available.contains(&rung)
    }

    /// Every rung available, in a stable order.
    pub fn available(&self) -> &[Rung] {
        &self.available
    }

    /// Why a rung is missing, if the probe learned something specific.
    pub fn restriction(&self, rung: Rung) -> Option<Restriction> {
        self.notes
            .iter()
            .find(|(r, _)| *r == rung)
            .map(|(_, why)| *why)
    }

    /// Rungs in [`FLOOR`] that this machine does not offer.
    pub fn missing_from_floor(&self) -> Vec<Rung> {
        FLOOR
            .iter()
            .copied()
            .filter(|rung| !self.has(*rung))
            .collect()
    }

    /// Whether a content process may be launched at all.
    pub fn clears_floor(&self) -> bool {
        self.missing_from_floor().is_empty()
    }

    pub(crate) fn offer(&mut self, rung: Rung) {
        if !self.available.contains(&rung) {
            self.available.push(rung);
        }
    }

    pub(crate) fn deny(&mut self, rung: Rung, why: Restriction) {
        self.notes.retain(|(r, _)| *r != rung);
        self.notes.push((rung, why));
    }
}

/// Why a content process was refused, in a form a person can act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    missing: Vec<Rung>,
    remedies: Vec<(Rung, Restriction)>,
}

impl Refusal {
    /// The mechanisms that were required and absent.
    pub fn missing(&self) -> &[Rung] {
        &self.missing
    }
}

impl fmt::Display for Refusal {
    /// The message a user meets. It names the mechanism and the remedy for the
    /// condition detected, because §14.5's whole point is that a refusal
    /// saying nothing is what sends someone to `--no-sandbox` blind.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refusing to launch a content process: ")?;
        for (index, rung) in self.missing.iter().enumerate() {
            if index > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{rung} is unavailable")?;
            if let Some((_, why)) = self.remedies.iter().find(|(r, _)| r == rung) {
                write!(f, " ({})", why.remedy())?;
            }
        }
        write!(
            f,
            ". A content process runs untrusted code; without this it would run \
             with the same authority as the browser."
        )
    }
}

/// Detect what this machine offers.
///
/// Contains no `unsafe`. Every Linux probe is a file read under `/proc` or
/// `/sys`, which is the interface the kernel documents for exactly this, and it
/// keeps detection — the part that decides whether to refuse — free of FFI.
pub fn detect() -> Capabilities {
    let mut caps = Capabilities::default();

    #[cfg(feature = "testing")]
    if std::env::var_os("PX_TEST_FORCE_SANDBOX_UNAVAILABLE").is_some() {
        // Test-only, and gated so it cannot exist in a release artifact.
        // §14.4: a flag being off is not evidence; absence of the symbol from
        // the shipped binary is, and ci/gate-sandbox.sh scans for it.
        for rung in FLOOR {
            caps.deny(*rung, Restriction::Inconclusive);
        }
        return caps;
    }

    #[cfg(target_os = "linux")]
    {
        detect_linux(&mut caps);
    }

    #[cfg(target_os = "windows")]
    {
        // Both Windows rungs are creation-time operations rather than
        // queryable features: a restricted token and a job object are made,
        // not detected. So the probe *makes* the cheap half of each rather
        // than assuming it would work — see `windows::detect`.
        windows::detect(&mut caps);
    }

    caps
}

#[cfg(target_os = "linux")]
fn detect_linux(caps: &mut Capabilities) {
    // no_new_privs is a prctl with no query interface and no kernel config
    // that removes it; it has been unconditionally present since 3.5. Treated
    // as available, and the spawn path will still check the prctl's return
    // value rather than trusting this.
    caps.offer(Rung::NoNewPrivs);

    // seccomp: the kernel exposes the actions it supports here when
    // CONFIG_SECCOMP_FILTER is on. Absence means no filtering.
    match read_trimmed("/proc/sys/kernel/seccomp/actions_avail") {
        Some(actions) if actions.contains("errno") || actions.contains("kill") => {
            caps.offer(Rung::Seccomp);
        }
        Some(_) => caps.deny(Rung::Seccomp, Restriction::NotSupported),
        None => caps.deny(Rung::Seccomp, Restriction::Inconclusive),
    }

    // Landlock advertises itself in the active LSM list.
    match read_trimmed("/sys/kernel/security/lsm") {
        Some(lsms) if lsms.split(',').any(|lsm| lsm.trim() == "landlock") => {
            caps.offer(Rung::Landlock);
        }
        Some(_) => caps.deny(Rung::Landlock, Restriction::NotSupported),
        None => caps.deny(Rung::Landlock, Restriction::Inconclusive),
    }

    detect_user_namespaces(caps);
}

#[cfg(target_os = "linux")]
fn detect_user_namespaces(caps: &mut Capabilities) {
    // Three spellings, checked in the order that gives the most specific
    // answer. §14.5 says to name "the sysctl"; there isn't one, and naming the
    // wrong one sends a user to edit a setting their kernel does not have.
    if let Some(value) = read_trimmed("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        && value == "1"
    {
        caps.deny(Rung::UserNamespaces, Restriction::ApparmorRestrictUserns);
        return;
    }
    if let Some(value) = read_trimmed("/proc/sys/kernel/unprivileged_userns_clone")
        && value == "0"
    {
        caps.deny(Rung::UserNamespaces, Restriction::UnprivilegedUsernsClone);
        return;
    }
    if let Some(value) = read_trimmed("/proc/sys/user/max_user_namespaces") {
        if value == "0" {
            caps.deny(Rung::UserNamespaces, Restriction::MaxUserNamespacesZero);
            return;
        }
        caps.offer(Rung::UserNamespaces);
        return;
    }
    // No max_user_namespaces at all means the kernel lacks user namespace
    // support. Inconclusive would also be defensible; NotSupported is more
    // useful to a reader and both refuse.
    caps.deny(Rung::UserNamespaces, Restriction::NotSupported);
}

/// Read a sysctl-style file, or `None` if it cannot be read.
///
/// Unreadable is not "permitted". Every caller treats `None` as unavailable.
#[cfg(target_os = "linux")]
fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|contents| contents.trim().to_owned())
}

/// Decide whether a content process may launch.
///
/// `Ok` carries the rungs that will be applied; `Err` is the refusal, with the
/// message a user sees.
pub fn admit(caps: &Capabilities) -> Result<Vec<Rung>, Refusal> {
    let missing = caps.missing_from_floor();
    if missing.is_empty() {
        return Ok(caps.available().to_vec());
    }
    let remedies = missing
        .iter()
        .filter_map(|rung| caps.restriction(*rung).map(|why| (*rung, why)))
        .collect();
    Err(Refusal { missing, remedies })
}

/// A policy step that was required and did not take.
///
/// Separate from [`Refusal`], and the distinction matters. A `Refusal` is
/// "this machine does not offer what we require", which a user can act on. A
/// `PolicyError` is "this machine said it offered it, and applying it failed
/// anyway" — a machine lying, a race, or a bug here. Both stop the launch;
/// only the first has a remedy worth printing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyError {
    /// A rung could not be applied.
    Failed {
        /// Which mechanism.
        rung: Rung,
        /// What specifically went wrong, for the log.
        detail: &'static str,
    },
    /// The floor was not cleared, so nothing was attempted.
    Refused(Refusal),
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed { rung, detail } => write!(
                f,
                "refusing to launch a content process: {rung} could not be applied ({detail}).                  The machine reported it was available, so this is a fault here rather than a                  missing kernel feature."
            ),
            Self::Refused(refusal) => write!(f, "{refusal}"),
        }
    }
}

impl std::error::Error for PolicyError {}

/// A content process that was created under a policy.
///
/// The policy is not applied to this child; it is the policy the child was
/// *made with*. See the platform modules for why that distinction is the whole
/// point — a process confined after creation runs unconfined for the window
/// before the confinement lands, and that window is attacker-reachable.
///
/// The broker keeps the supervisor — worker threads, deadline, restart policy
/// — and is handed one of these (ADR 008).
pub struct SandboxedChild {
    /// The parent's end of the child's stdin. Taken once by the supervisor.
    stdin: Option<std::fs::File>,
    /// The parent's end of the child's stdout. Taken once by the supervisor.
    stdout: Option<std::fs::File>,
    applied: Vec<Rung>,
    #[cfg(target_os = "windows")]
    inner: windows::Child,
    #[cfg(not(target_os = "windows"))]
    inner: std::process::Child,
}

impl SandboxedChild {
    /// Take the write end of the child's stdin. `None` after the first call.
    pub fn take_stdin(&mut self) -> Option<std::fs::File> {
        self.stdin.take()
    }

    /// Take the read end of the child's stdout. `None` after the first call.
    pub fn take_stdout(&mut self) -> Option<std::fs::File> {
        self.stdout.take()
    }

    /// The rungs this process was created under.
    ///
    /// Every entry was applied by the operating system as part of process
    /// creation. Nothing here is a claim made afterwards, and nothing here was
    /// reported by the child — a child describing its own confinement would be
    /// authority taken from a message, which invariant 9 forbids.
    pub fn applied(&self) -> &[Rung] {
        &self.applied
    }

    /// The child's process identifier.
    pub fn id(&self) -> u32 {
        self.inner.id()
    }

    /// Whether the child has exited, without blocking.
    pub fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        #[cfg(target_os = "windows")]
        {
            self.inner.try_wait()
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(self
                .inner
                .try_wait()?
                .map(|status| status.code().unwrap_or(-1)))
        }
    }

    /// Wait for the child to exit.
    pub fn wait(&mut self) -> std::io::Result<i32> {
        #[cfg(target_os = "windows")]
        {
            self.inner.wait()
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(self.inner.wait()?.code().unwrap_or(-1))
        }
    }

    /// Kill the child.
    ///
    /// On Windows this terminates the process; the job object it belongs to is
    /// what will make killing the whole tree possible when Phase 17 sets
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. On Linux it is a signal to the
    /// one process, and the orphaned-grandchild case remains open until
    /// Phase 17's cgroup — see `docs/backlog.md`.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.inner.kill()
    }
}

/// Spawn a content process under a sandbox policy.
///
/// Fails closed, in this order and for this reason:
///
/// 1. Detect what the machine offers.
/// 2. [`admit`] it against [`FLOOR`]. Below the floor, **nothing is spawned**
///    — the refusal is returned before a process exists, which is what
///    invariant 8 requires and what gate item 2 checks end to end.
/// 3. Create the process with the policy, not beside it.
///
/// A failure at step 3 is a [`PolicyError::Failed`] rather than a refusal: the
/// machine said it offered the mechanism and applying it did not work, which
/// is a fault here rather than a missing kernel feature. Both stop the launch.
pub fn spawn(executable: &std::path::Path) -> Result<SandboxedChild, PolicyError> {
    let caps = detect();
    admit(&caps).map_err(PolicyError::Refused)?;

    #[cfg(target_os = "windows")]
    {
        let spawned = windows::spawn(executable)?;
        Ok(SandboxedChild {
            stdin: Some(windows::into_file(spawned.stdin)),
            stdout: Some(windows::into_file(spawned.stdout)),
            applied: spawned.applied,
            inner: spawned.child,
        })
    }
    #[cfg(target_os = "linux")]
    {
        let spawned = linux::spawn(executable)?;
        Ok(SandboxedChild {
            stdin: Some(spawned.stdin),
            stdout: Some(spawned.stdout),
            applied: spawned.applied,
            inner: spawned.child,
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = executable;
        // Unreachable in practice: FLOOR on an unknown platform lists rungs
        // nothing offers, so `admit` above has already refused. Kept so that
        // adding a platform to FLOOR without adding a policy fails to build
        // rather than silently spawning unconfined.
        Err(PolicyError::Failed {
            rung: FLOOR[0],
            detail: "this platform has no sandbox implementation",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether this machine can actually exercise a policy — and a loud
    /// failure when it cannot but was promised it could.
    ///
    /// The tests below return early on a machine below the floor, because
    /// refusing there is correct behaviour and asserting a launch would make
    /// them tests about the machine. But a silent skip is indistinguishable
    /// from a pass, which is this project's most expensive recurring bug. So
    /// CI sets `PX_REQUIRE_SANDBOX`, and under it a machine that cannot clear
    /// the floor fails the suite rather than quietly skipping the only tests
    /// that exercise the policy at all.
    fn can_exercise_a_policy() -> bool {
        if detect().clears_floor() {
            return true;
        }
        assert!(
            std::env::var_os("PX_REQUIRE_SANDBOX").is_none(),
            "PX_REQUIRE_SANDBOX is set, but this machine does not clear the sandbox floor              ({:?} missing). The policy tests would pass without testing anything.",
            detect().missing_from_floor()
        );
        false
    }

    /// The content process stand-in these tests spawn.
    ///
    /// A real executable, not a mock: the question every test here asks is
    /// whether the *operating system* accepted the policy, and only the OS can
    /// answer it. `px-content` is not used because px-sandbox must not depend
    /// on it; any process that stays alive long enough to be inspected does.
    fn a_spawnable_executable() -> std::path::PathBuf {
        if cfg!(windows) {
            std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe")
        } else {
            std::path::PathBuf::from("/bin/cat")
        }
    }

    /// The whole of gate item 1 in one assertion: a process launches, and it
    /// launches *under* a policy rather than beside one.
    #[test]
    fn sandbox_policy_a_content_process_launches_under_a_policy() {
        if !can_exercise_a_policy() {
            return;
        }

        let mut child = spawn(&a_spawnable_executable()).expect("a sandboxed content process");
        let applied = child.applied().to_vec();
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            !applied.is_empty(),
            "a process launched under a policy must report the rungs it got"
        );
        for rung in FLOOR {
            assert!(
                applied.contains(rung),
                "{rung} is in FLOOR but was not applied; the launch should not have happened"
            );
        }
    }

    /// Nothing may be reported applied that detection says is unavailable.
    /// This is what keeps `applied()` usable as evidence rather than as an
    /// intention.
    #[test]
    fn sandbox_policy_never_reports_a_rung_the_machine_does_not_offer() {
        if !can_exercise_a_policy() {
            return;
        }
        let caps = detect();
        let mut child = spawn(&a_spawnable_executable()).expect("a sandboxed content process");
        let applied = child.applied().to_vec();
        let _ = child.kill();
        let _ = child.wait();

        for rung in &applied {
            assert!(
                caps.has(*rung),
                "{rung} was reported applied but detection says it is unavailable"
            );
        }
    }

    /// The child must be usable, not merely created. A policy that produces a
    /// confined process the broker cannot talk to has broken the product to
    /// secure it.
    #[test]
    fn sandbox_policy_leaves_the_child_talkable_to() {
        if !can_exercise_a_policy() {
            return;
        }
        let mut child = spawn(&a_spawnable_executable()).expect("a sandboxed content process");
        assert!(
            child.take_stdin().is_some(),
            "the broker needs the write end of the child's stdin"
        );
        assert!(
            child.take_stdout().is_some(),
            "the broker needs the read end of the child's stdout"
        );
        assert!(
            child.take_stdin().is_none(),
            "a pipe end is handed over once; a second caller must get None"
        );
        assert!(child.id() > 0, "a spawned child has a process id");
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Gate item 2's precondition, at the library level: below the floor,
    /// `spawn` refuses *before* a process exists, and says which mechanism is
    /// missing.
    #[test]
    fn sandbox_policy_refusal_below_the_floor_names_the_mechanism() {
        let caps = Capabilities::default();
        let refusal = admit(&caps).expect_err("an empty machine clears no floor");
        let error = PolicyError::Refused(refusal);
        let message = error.to_string();
        for rung in FLOOR {
            assert!(
                message.contains(rung.name()),
                "the refusal must name {rung}; got: {message}"
            );
        }
    }

    /// A `PolicyError::Failed` must not read like a missing kernel feature.
    /// The two have different remedies and conflating them sends a user to
    /// change a setting that was never the problem.
    #[test]
    fn sandbox_policy_a_failure_to_apply_is_not_reported_as_a_missing_feature() {
        let error = PolicyError::Failed {
            rung: FLOOR[0],
            detail: "a synthetic failure",
        };
        let message = error.to_string();
        assert!(message.contains("could not be applied"));
        assert!(
            message.contains("fault here"),
            "an application failure must say it is a fault on this side; got: {message}"
        );
    }

    fn with(available: &[Rung]) -> Capabilities {
        Capabilities::with_available(available)
    }

    #[test]
    fn sandbox_ladder_reports_what_this_machine_offers() {
        let caps = detect();
        // Detection must not panic and must be deterministic across calls —
        // a probe that varies run to run cannot be reasoned about.
        assert_eq!(caps, detect());
    }

    #[test]
    fn sandbox_ladder_missing_rungs_are_named_not_counted() {
        let caps = with(&[Rung::NoNewPrivs]);
        let missing = caps.missing_from_floor();
        if cfg!(target_os = "linux") {
            assert_eq!(missing, vec![Rung::Seccomp]);
        } else {
            assert!(
                !missing.is_empty(),
                "a partial set must not clear the floor"
            );
        }
    }

    #[test]
    fn sandbox_refuses_below_the_floor() {
        let caps = Capabilities::default();
        let refusal = admit(&caps).expect_err("an empty machine must be refused");
        assert_eq!(refusal.missing(), FLOOR);
    }

    #[test]
    fn sandbox_refuses_and_says_which_mechanism_is_missing() {
        let mut caps = Capabilities::default();
        for rung in FLOOR {
            caps.deny(*rung, Restriction::NotSupported);
        }
        let refusal = admit(&caps).expect_err("refused");
        let message = refusal.to_string();

        for rung in FLOOR {
            assert!(
                message.contains(rung.name()),
                "the refusal must name {rung}; got: {message}"
            );
        }
        assert!(
            message.contains("newer kernel") || message.contains("sysctl"),
            "the refusal must carry a remedy; got: {message}"
        );
    }

    /// §14.5 says to name the sysctl. There are three, and the message must
    /// come from the condition detected rather than a guess — naming the wrong
    /// one sends a user to edit a setting that does not exist.
    #[test]
    fn sandbox_refuses_with_the_remedy_for_the_condition_detected() {
        let cases = [
            (
                Restriction::UnprivilegedUsernsClone,
                "kernel.unprivileged_userns_clone",
            ),
            (
                Restriction::ApparmorRestrictUserns,
                "kernel.apparmor_restrict_unprivileged_userns",
            ),
            (
                Restriction::MaxUserNamespacesZero,
                "user.max_user_namespaces",
            ),
        ];
        for (restriction, expected) in cases {
            assert!(
                restriction.remedy().contains(expected),
                "{restriction:?} must name {expected}"
            );
            for (other, unexpected) in cases {
                if other != restriction {
                    assert!(
                        !restriction.remedy().contains(unexpected),
                        "{restriction:?} must not also name {unexpected}"
                    );
                }
            }
        }
    }

    #[test]
    fn sandbox_refuses_when_a_probe_is_inconclusive() {
        // Fail closed: an unreadable or unparseable probe is not permission.
        let mut caps = Capabilities::default();
        for rung in FLOOR {
            caps.deny(*rung, Restriction::Inconclusive);
        }
        assert!(admit(&caps).is_err());
        assert!(!caps.clears_floor());
    }

    #[test]
    fn a_full_floor_is_admitted() {
        let caps = with(FLOOR);
        let applied = admit(&caps).expect("a machine at the floor must be admitted");
        assert_eq!(applied, FLOOR.to_vec());
    }

    /// The ladder's point: losing a rung above the floor is not a refusal.
    /// Refusing on the loss of user namespaces is what §14.5 recommends and
    /// what ADR 007 rejects, because it refuses in the common case and drives
    /// the user to --no-sandbox.
    #[test]
    fn sandbox_ladder_a_rung_above_the_floor_is_not_required() {
        let mut caps = with(FLOOR);
        caps.deny(Rung::UserNamespaces, Restriction::ApparmorRestrictUserns);
        assert!(
            caps.clears_floor(),
            "losing user namespaces must not refuse the launch"
        );
        assert!(!caps.has(Rung::UserNamespaces));
    }
}
