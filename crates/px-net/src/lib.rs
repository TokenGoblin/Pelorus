#![forbid(unsafe_code)]

//! Rustls, DoH, HTTP/1.1, partitioned pools.
//!
//! # Phase 3, and what it is not
//!
//! **HTTP/2 is not here, and is not coming this phase.** ADR 015 defers it and
//! narrows §9 Phase 3's stated scope: HTTP/2 is HPACK, multiplexing, flow
//! control and settings, and realistically arrives as `h2` plus an async
//! runtime — about forty crates adopted before anything in this project
//! renders a page to judge them against. `px-net` negotiates `http/1.1` by
//! ALPN and does not offer `h2`.
//!
//! # Where the hostile bytes are
//!
//! [`http1`] parses what a server sends, and a server is whoever the page came
//! from. That module carries the reasoning; the short version is that it
//! refuses ambiguous framing rather than resolving it, because a client that
//! resolves an ambiguity differently from an upstream cache is one half of a
//! response-splitting bug.

pub mod fetch;
pub mod hsts;
pub mod http1;
pub mod partition;
pub mod pool;
pub mod psl;
pub mod tls;
