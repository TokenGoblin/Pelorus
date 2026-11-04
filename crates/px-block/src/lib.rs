#![forbid(unsafe_code)]

//! Content blocking and tracker mitigations (Phase 19).
//!
//! Pure policy: this crate performs **no I/O** (no network, no filesystem, no
//! clock). It is handed list text, URLs and DNS answers and returns decisions;
//! the integration (px-net, px-content, px-store) acts on them. Filter lists
//! are untrusted text: nothing here panics on any input, every size is capped,
//! and there is no regex engine.
//!
//! # Units
//!
//! - [`filter`]: parse Adblock Plus / uBlock Origin lists into [`filter::Rule`]s.
//! - [`glob`]: the pattern language and its linear-time matcher.
//! - [`matcher`]: the deterministic token index and
//!   [`matcher::check_request`] -> [`matcher::Verdict`].
//! - [`params`]: tracking query-parameter stripping.
//! - [`referrer`]: stricter referrer policy for third-party requests.
//! - [`cname`]: CNAME-cloaking decision from a resolver-supplied chain.
//! - [`isolation`]: which partition key a (blocked or allowed) request gets.
//! - [`stats`]: bounded blocking counters.
//!
//! # Dependencies
//!
//! Only `px-websec` (workspace, std-only) for `Url`, `Origin`, the
//! `SiteResolver` trait and the referrer computation. The public suffix list
//! is px-net's; the integration passes
//! `|h| psl.registrable_domain(h)` as the resolver, so this crate never pulls
//! the TLS closure.
//!
//! # Syntax implemented
//!
//! Network filters only:
//!
//! - `||domain^` host anchor (start of host or after any `.` in the host),
//!   `|` start / end anchors, `*` wildcard, `^` separator with the exact
//!   Adblock Plus character class (see [`glob`]);
//! - `@@` exceptions;
//! - options `third-party` / `~third-party` (aliases `3p`, `first-party`,
//!   `1p`), resource types `script image stylesheet(css) xmlhttprequest(xhr)
//!   subdocument(frame) ping media font other` and their `~` negations,
//!   `domain=a.com|~b.a.com` (most specific entry wins, exclusion wins ties),
//!   `important`, `match-case`;
//! - comments (`!`, `# `), `[Adblock Plus ...]` headers;
//! - cosmetic lines (`##`, `#@#`, `#?#`, `#$#`, `#%#`, ... and `$$` HTML
//!   filters) are *recognised and skipped*, counted in
//!   [`filter::ParseStats::cosmetic_skipped`] (scriptlets additionally in
//!   `scriptlet_skipped`);
//! - `/regex/` filters are recognised and **rejected** (counted); there is no
//!   regex engine. Note this means a plain path filter written `/ads/` is
//!   treated as a regex, exactly as Adblock Plus does.
//!
//! # NOT implemented (stated plainly)
//!
//! Any rule using one of these options is rejected whole and counted under
//! [`filter::RejectReason::UnsupportedOption`]; it is never applied in part.
//!
//! - Cosmetic filtering of any kind: element hiding, procedural cosmetics
//!   (`:has-text`, `:xpath`...), CSS injection, extended selectors, HTML
//!   filtering. Skipped.
//! - Scriptlets (`+js(...)`) and ABP snippets. Skipped.
//! - `$redirect`, `$redirect-rule`, `$rewrite`, `$empty`, `$mp4` (resource
//!   replacement); `$removeparam` (list-driven parameter stripping; see
//!   [`params`] for the built-in, configurable alternative); `$csp`,
//!   `$permissions`, `$replace`, `$header`; `$badfilter` (the rule it would
//!   cancel stays active); `$denyallow`, `$to`, `$from`, `$method`;
//!   `$document`, `$popup`, `$elemhide`, `$generichide`, `$genericblock`,
//!   `$all`, `$inline-script`, `$object`, `$websocket`, `$webrtc`.
//! - Therefore top-level navigations (`ResourceType::Document`) are never
//!   blocked, and exceptions cannot switch other features off.
//! - Entity `domain=google.*` and regex `domain=/x/` entries.
//! - `@@...$important` (rejected). A rule with no literal text and no
//!   constraining option (`*`, `||`, `^`) is rejected as match-all.
//! - Hosts-file lines (`0.0.0.0 host`) and `/etc/hosts` syntax; they contain
//!   whitespace and are rejected as invalid.
//! - Preprocessor directives (`!#if`, `!#include`): treated as comments, so
//!   a list relying on `!#include` silently lacks the included rules.
//! - Percent-decoding or dot-segment normalisation of the request URL before
//!   matching (matching is on the URL as given), and IDNA (hosts must already
//!   be ASCII/punycode, as everywhere in `px-websec`).
//! - Bounce-tracking mitigation (named in the crate CLAUDE.md) needs
//!   navigation history and storage and is not in this unit.
//!
//! # Caps
//!
//! | what | cap |
//! |---|---|
//! | list text | 16 MiB ([`filter::MAX_LIST_BYTES`]), cut at a line boundary |
//! | line | 4096 bytes |
//! | rules per list | 500 000; per matcher 1 000 000 |
//! | pattern | 512 bytes |
//! | `domain=` entries | 128 per rule |
//! | request URL | 8192 bytes (px-websec `MAX_URL_BYTES`) |
//! | CNAME chain | 16 names |
//! | tracking-param table | 1024 entries of <= 128 bytes |
//! | stats | 4096 rules, 1024 domains, overflow counted |
//!
//! # Complexity and determinism
//!
//! Matching a request costs one URL tokenisation, `O(tokens * log buckets)`
//! index probes, and for each candidate rule a glob match that is
//! `O(url_len * longest_pattern_segment)` worst case with no backtracking
//! (see [`glob`]). Indexing by each rule's rarest token keeps candidate sets
//! to a handful on realistic lists. Everything is `BTreeMap`/sorted `Vec`;
//! no hash iteration order is observable.

pub mod cname;
pub mod filter;
pub mod glob;
pub mod isolation;
pub mod matcher;
pub mod params;
pub mod referrer;
pub mod stats;

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod hostile_tests;
#[cfg(test)]
mod matcher_tests;
