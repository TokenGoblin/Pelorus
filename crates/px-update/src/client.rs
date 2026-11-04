//! The update client as a state machine of pure functions.
//!
//! `check` takes everything it needs as arguments (the transport, the config,
//! the trust set, the persisted state, the clock) and returns an [`Outcome`]
//! holding a [`Decision`] plus the new state and trust to persist. It does no
//! I/O of its own: the only way bytes enter is [`Transport::fetch`], which
//! px-net implements later by sending the request bytes verbatim over a fresh
//! connection that shares nothing with browsing state.
//!
//! Rules, each with a test:
//!
//! * Fail closed. Every error becomes `Decision::Rejected(reason)`; nothing is
//!   installed or recorded on a rejection except mirror failure counts.
//! * Authenticate before anything else. The manifest and its signature are
//!   fetched, the signature is verified over the raw bytes by a threshold of
//!   trusted keys, and only then is the manifest parsed. No artifact or data
//!   URL is requested during `check` at all; those are fetched by [`download`]
//!   after the decision, each against the digests the signed manifest names.
//! * Never go backwards. `sequence` may not be lower than the highest seen,
//!   the release version may not be lower than the installed one, and a data
//!   version may not be lower than the installed one (rollback protection).
//! * Freshness. A manifest outside its `issued`..`expires` window is refused,
//!   so a recorded old (once valid) manifest cannot be replayed.
//! * Mirrors are tried in a deterministic order, fewest recorded failures
//!   first, ties by configured order. Failure counts are bounded at
//!   [`MAX_FAILURES`] so a long outage cannot bury a mirror forever; a
//!   success resets the count. A mirror that serves something that fails
//!   verification is charged exactly like one that is unreachable.
//! * Stale data forces a check. A data file older than `max_data_age_secs`
//!   makes the check due regardless of the user's cadence (the cadence is an
//!   input, default weekly, and there is no jitter: same inputs, same answer).
//! * Downloads: the transport is told the size cap before it reads (the
//!   declared size); the body must then match the declared size and BOTH
//!   digests. Only verified bytes are ever returned; a short, long or
//!   corrupted body is never handed to a caller.
//!
//! Known limit: bodies are in memory. A streaming sink with an incremental
//! hasher is the follow-up for the 1 GiB artifact cap (px-net/px-store).

use crate::manifest::{Artifact, DataEntry, DataKind, FileRef, MAX_MANIFEST_BYTES, Release};
use crate::request::{
    MANIFEST_RESOURCE, Mirror, Request, RequestError, SIGNATURE_RESOURCE, audit, build_get,
};
use crate::sha2::{sha256, sha512};
use crate::trust::{SignatureSet, Trust, TrustError};
use crate::version::{Version, date_to_unix};

pub const WEEK_SECS: u64 = 7 * 86_400;
pub const DAY_SECS: u64 = 86_400;
pub const MAX_MIRRORS: usize = 8;
pub const MAX_FAILURES: u8 = 8;
const MAX_ROTATIONS_PER_CHECK: usize = 4;
const MAX_SIG_BYTES: u64 = 1024;

// ---- transport -------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// Could not connect or complete TLS.
    Connect,
    /// Non-200 status.
    Status(u16),
    /// The body exceeded `max_body` (the transport stops reading at the cap).
    TooLarge,
    /// The connection ended early.
    Truncated,
    Other,
}

/// What px-net implements. One call is one fresh, isolated connection.
pub trait Transport {
    /// Send `request.bytes` verbatim to `mirror` and return the response body
    /// of a `200`. Must refuse (`TooLarge`) rather than read past `max_body`
    /// bytes of body. Must not add headers, send cookies, reuse connections or
    /// sessions, follow redirects to another host, or touch browsing state.
    fn fetch(
        &mut self,
        mirror: &Mirror,
        request: &Request,
        max_body: u64,
    ) -> Result<Vec<u8>, TransportError>;
}

// ---- configuration and state -----------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub channel: String,
    pub platform: String,
    /// Fallback order. Empty means updates are not configured: fail closed.
    pub mirrors: Vec<Mirror>,
    /// User-configured seconds between scheduled checks; 0 = no scheduled
    /// checks (manual only; stale data still forces one, see `due`).
    pub cadence_secs: u64,
    /// A data file older than this forces a check.
    pub max_data_age_secs: u64,
    /// Minimum seconds between two attempts, success or failure.
    pub min_retry_secs: u64,
}

impl Config {
    /// Default weekly cadence, 30-day data staleness bound, hourly retry floor.
    pub fn new(channel: &str, platform: &str, mirrors: Vec<Mirror>) -> Config {
        Config {
            channel: channel.to_string(),
            platform: platform.to_string(),
            mirrors,
            cadence_secs: WEEK_SECS,
            max_data_age_secs: 30 * DAY_SECS,
            min_retry_secs: 3600,
        }
    }
}

/// Persisted client state. The caller stores it (px-store) after each check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub installed: Version,
    /// Highest manifest sequence ever accepted.
    pub max_sequence: u64,
    /// Time of the last check that reached a verified conclusion; 0 = never.
    pub last_check: u64,
    /// Time of the last attempt, successful or not; 0 = never.
    pub last_attempt: u64,
    /// Installed data versions as `YYYYMMDD`, indexed by `DataKind`; 0 = none.
    pub data_versions: [u32; 3],
    /// Consecutive failures per configured mirror (bounded).
    pub mirror_failures: Vec<u8>,
}

fn kind_index(k: DataKind) -> usize {
    match k {
        DataKind::Psl => 0,
        DataKind::Hsts => 1,
        DataKind::Roots => 2,
    }
}

impl State {
    pub fn new(installed: Version, data_versions: [u32; 3]) -> State {
        State {
            installed,
            max_sequence: 0,
            last_check: 0,
            last_attempt: 0,
            data_versions,
            mirror_failures: Vec::new(),
        }
    }

    pub fn data_version(&self, k: DataKind) -> u32 {
        self.data_versions.get(kind_index(k)).copied().unwrap_or(0)
    }

    /// Call after a data file has been verified AND installed.
    pub fn record_data_installed(&mut self, k: DataKind, version: u32) {
        if let Some(slot) = self.data_versions.get_mut(kind_index(k))
            && version > *slot
        {
            *slot = version;
        }
    }

    /// Call after a release has been verified AND installed.
    pub fn record_release_installed(&mut self, v: Version) {
        if v > self.installed {
            self.installed = v;
        }
    }

    fn failures(&self, i: usize) -> u8 {
        self.mirror_failures.get(i).copied().unwrap_or(0)
    }

    fn charge(&mut self, i: usize) {
        if let Some(f) = self.mirror_failures.get_mut(i) {
            *f = f.saturating_add(1).min(MAX_FAILURES);
        }
    }

    fn clear(&mut self, i: usize) {
        if let Some(f) = self.mirror_failures.get_mut(i) {
            *f = 0;
        }
    }
}

// ---- when to check -----------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    No,
    Cadence,
    /// A data file is older than `max_data_age_secs`: check regardless of cadence.
    StaleData(DataKind),
}

/// Age in seconds of a data version, or `None` when it is unset or not a date
/// (both count as stale).
fn data_age(version: u32, now: u64) -> Option<u64> {
    date_to_unix(version).map(|t| now.saturating_sub(t))
}

/// Pure schedule decision. The retry floor applies to everything (it protects
/// the mirrors from a stuck client); within it, stale data beats the cadence.
pub fn due(cfg: &Config, state: &State, now: u64) -> Due {
    // A clock that went backwards past the last attempt does not suppress a check.
    if now >= state.last_attempt && now - state.last_attempt < cfg.min_retry_secs {
        return Due::No;
    }
    for k in DataKind::ALL {
        let stale = match data_age(state.data_version(k), now) {
            Some(age) => age > cfg.max_data_age_secs,
            None => true,
        };
        if stale {
            return Due::StaleData(k);
        }
    }
    if cfg.cadence_secs > 0
        && (state.last_check == 0
            || now < state.last_check
            || now - state.last_check >= cfg.cadence_secs)
    {
        return Due::Cadence;
    }
    Due::No
}

// ---- decisions ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Honour `due`.
    Scheduled,
    /// The user asked; skip the cadence (the retry floor still applies).
    Manual,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadError {
    Transport(TransportError),
    Request(RequestError),
    SizeMismatch,
    DigestMismatch,
    NoMirrors,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    NoMirrors,
    Request(RequestError),
    Transport(TransportError),
    Trust(TrustError),
    WrongChannel,
    SequenceRollback {
        offered: u64,
        seen: u64,
    },
    VersionDowngrade {
        offered: Version,
        installed: Version,
    },
    DataDowngrade(DataKind),
    /// Too soon after the previous attempt (retry floor).
    TooSoon,
}

impl Reason {
    /// A reason that says the mirror served something wrong, as opposed to
    /// the network simply not working.
    pub fn is_security(&self) -> bool {
        !matches!(
            self,
            Reason::Transport(_) | Reason::NoMirrors | Reason::TooSoon | Reason::Request(_)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Scheduled check, nothing due; the network was not touched.
    NotDue,
    UpToDate,
    UpdateAvailable {
        release: Box<Release>,
        artifact: Artifact,
        /// Data files in the same manifest that are newer than installed.
        data: Vec<DataEntry>,
    },
    DataRefresh {
        data: Vec<DataEntry>,
    },
    Rejected(Reason),
}

/// The decision plus what to persist. `trust` may have advanced even when the
/// decision is a rejection (a verified rotation is kept).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub decision: Decision,
    pub state: State,
    pub trust: Trust,
}

fn mirror_order(state: &State, n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|i| (state.failures(*i), *i));
    order
}

fn fetch_checked(
    transport: &mut dyn Transport,
    mirror: &Mirror,
    resource: &str,
    max_body: u64,
) -> Result<Vec<u8>, Reason> {
    let request = build_get(mirror, resource).map_err(Reason::Request)?;
    // Defence in depth: nothing leaves this crate that the audit rejects.
    audit(&request.bytes).map_err(Reason::Request)?;
    let body = transport
        .fetch(mirror, &request, max_body)
        .map_err(Reason::Transport)?;
    // Do not trust the transport to have honoured the cap.
    if u64::try_from(body.len()).map_or(true, |n| n > max_body) {
        return Err(Reason::Transport(TransportError::TooLarge));
    }
    Ok(body)
}

fn fetch_signed(
    transport: &mut dyn Transport,
    mirror: &Mirror,
    manifest_resource: &str,
    sig_resource: &str,
) -> Result<(Vec<u8>, SignatureSet), Reason> {
    let manifest = fetch_checked(
        transport,
        mirror,
        manifest_resource,
        MAX_MANIFEST_BYTES as u64,
    )?;
    let sig_bytes = fetch_checked(transport, mirror, sig_resource, MAX_SIG_BYTES)?;
    let sigs = SignatureSet::parse(&sig_bytes).map_err(Reason::Trust)?;
    Ok((manifest, sigs))
}

/// One mirror, start to finish. `trust` is advanced in place by any verified
/// rotation applied on the way.
fn try_mirror(
    transport: &mut dyn Transport,
    mirror: &Mirror,
    cfg: &Config,
    trust: &mut Trust,
    state: &State,
    now: u64,
) -> Result<Release, Reason> {
    let (bytes, sigs) = fetch_signed(transport, mirror, MANIFEST_RESOURCE, SIGNATURE_RESOURCE)?;
    let mut rotations = 0usize;
    let mut first_err: Option<TrustError> = None;
    let release = loop {
        match trust.open_release(&bytes, &sigs, now) {
            Ok(r) => break r,
            Err(e @ (TrustError::ThresholdNotMet { .. } | TrustError::EpochMismatch { .. })) => {
                // Possibly signed under a newer epoch: catch up one rotation at a time.
                let first = first_err.get_or_insert(e);
                if rotations >= MAX_ROTATIONS_PER_CHECK {
                    return Err(Reason::Trust(first.clone()));
                }
                let next = trust.epoch().saturating_add(1);
                let fetched = fetch_signed(
                    transport,
                    mirror,
                    &format!("rotation-{next}.txt"),
                    &format!("rotation-{next}.sig"),
                );
                let Ok((rbytes, rsigs)) = fetched else {
                    return Err(Reason::Trust(first.clone()));
                };
                match trust.apply_rotation(&rbytes, &rsigs, now) {
                    Ok(t) => *trust = t,
                    Err(_) => return Err(Reason::Trust(first.clone())),
                }
                rotations += 1;
            }
            Err(e) => return Err(Reason::Trust(e)),
        }
    };

    if release.channel != cfg.channel {
        return Err(Reason::WrongChannel);
    }
    if release.sequence < state.max_sequence {
        return Err(Reason::SequenceRollback {
            offered: release.sequence,
            seen: state.max_sequence,
        });
    }
    if release.version < state.installed {
        return Err(Reason::VersionDowngrade {
            offered: release.version,
            installed: state.installed,
        });
    }
    for d in &release.data {
        if d.version < state.data_version(d.kind) {
            return Err(Reason::DataDowngrade(d.kind));
        }
    }
    Ok(release)
}

fn merge_reason(worst: Option<Reason>, new: Reason) -> Option<Reason> {
    match worst {
        Some(w) if w.is_security() || !new.is_security() => Some(w),
        _ => Some(new),
    }
}

/// Run one update check. See the module documentation for the rules.
pub fn check(
    transport: &mut dyn Transport,
    cfg: &Config,
    trust: &Trust,
    state: &State,
    now: u64,
    trigger: Trigger,
) -> Outcome {
    let mut st = state.clone();
    let mut tr = trust.clone();
    let done = |decision, st: State, tr: Trust| Outcome {
        decision,
        state: st,
        trust: tr,
    };

    let floor_blocks = now >= state.last_attempt && now - state.last_attempt < cfg.min_retry_secs;
    if floor_blocks {
        let d = match trigger {
            Trigger::Scheduled => Decision::NotDue,
            Trigger::Manual => Decision::Rejected(Reason::TooSoon),
        };
        return done(d, st, tr);
    }
    if trigger == Trigger::Scheduled && due(cfg, state, now) == Due::No {
        return done(Decision::NotDue, st, tr);
    }
    if cfg.mirrors.is_empty() || cfg.mirrors.len() > MAX_MIRRORS {
        return done(Decision::Rejected(Reason::NoMirrors), st, tr);
    }
    st.mirror_failures.resize(cfg.mirrors.len(), 0);
    st.last_attempt = now;

    let mut worst: Option<Reason> = None;
    for i in mirror_order(&st, cfg.mirrors.len()) {
        let Some(mirror) = cfg.mirrors.get(i) else {
            continue;
        };
        match try_mirror(transport, mirror, cfg, &mut tr, &st, now) {
            Ok(release) => {
                st.clear(i);
                st.last_check = now;
                st.max_sequence = st.max_sequence.max(release.sequence);
                let data: Vec<DataEntry> = release
                    .data
                    .iter()
                    .filter(|d| d.version > st.data_version(d.kind))
                    .cloned()
                    .collect();
                let artifact = release
                    .artifacts
                    .iter()
                    .find(|a| a.platform == cfg.platform)
                    .cloned();
                let decision = match artifact {
                    Some(artifact) if release.version > st.installed => Decision::UpdateAvailable {
                        release: Box::new(release),
                        artifact,
                        data,
                    },
                    _ if !data.is_empty() => Decision::DataRefresh { data },
                    _ => Decision::UpToDate,
                };
                return done(decision, st, tr);
            }
            Err(reason) => {
                st.charge(i);
                worst = merge_reason(worst, reason);
            }
        }
    }
    done(
        Decision::Rejected(worst.unwrap_or(Reason::NoMirrors)),
        st,
        tr,
    )
}

// ---- downloads -----------------------------------------------------------------

/// Check a downloaded body against the signed manifest's entry: exact size and
/// both digests. Pure; the caller never uses a body that fails this.
pub fn verify_download(file: &FileRef, body: &[u8]) -> Result<(), DownloadError> {
    if u64::try_from(body.len()) != Ok(file.size) {
        return Err(DownloadError::SizeMismatch);
    }
    if sha256(body) != file.sha256 || sha512(body) != file.sha512 {
        return Err(DownloadError::DigestMismatch);
    }
    Ok(())
}

/// Download one file named by a verified manifest, trying mirrors in failure
/// order. Returns only bytes that passed [`verify_download`].
pub fn download(
    transport: &mut dyn Transport,
    cfg: &Config,
    state: &mut State,
    file: &FileRef,
) -> Result<Vec<u8>, DownloadError> {
    if cfg.mirrors.is_empty() || cfg.mirrors.len() > MAX_MIRRORS {
        return Err(DownloadError::NoMirrors);
    }
    state.mirror_failures.resize(cfg.mirrors.len(), 0);
    let mut last = DownloadError::NoMirrors;
    for i in mirror_order(state, cfg.mirrors.len()) {
        let Some(mirror) = cfg.mirrors.get(i) else {
            continue;
        };
        // The cap handed to the transport is the declared size: it never
        // reads more than the manifest said the file has.
        let result = match fetch_checked(transport, mirror, &file.path, file.size) {
            Ok(body) => verify_download(file, &body).map(|()| body),
            Err(Reason::Transport(e)) => Err(DownloadError::Transport(e)),
            Err(Reason::Request(e)) => Err(DownloadError::Request(e)),
            Err(_) => Err(DownloadError::NoMirrors),
        };
        match result {
            Ok(body) => {
                state.clear(i);
                return Ok(body);
            }
            Err(e) => {
                state.charge(i);
                last = e;
            }
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests;
