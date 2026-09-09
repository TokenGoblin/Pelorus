# 005 — Handle passing waits for Phase 2; Phase 1 contains no unsafe

- **Status:** accepted
- **Date:** 2026-09-08
- **Phase:** 1
- **Invariants touched:** 1 (upheld), and the `forbid(unsafe_code)` hard rule

## Context

build-spec §3 lists "handle passing" among `px-ipc`'s responsibilities, and
Phase 1's description repeats it. The working agreement forbids `unsafe` in
every crate but `px-sandbox`.

These cannot both hold. Sending a file descriptor across a Unix domain socket
requires `sendmsg` with an `SCM_RIGHTS` control message; `std` has no API for
ancillary data. Windows requires `DuplicateHandle` against the peer's process
handle. Both are unsafe FFI, and neither has a safe wrapper in the standard
library.

So Phase 1 as written forces one of: unsafe in `px-ipc`, a widened remit for
`px-sandbox`, a new crate, or a dependency doing the one thing this project most
wants to own.

## Decision

**None of them, this phase.** Phase 1's gate does not mention handle passing —
it asks for fuzz-clean deserializers, rejection of identity claims, clean crash
recovery, and enforced size limits. All four are achievable with a transport
that needs no handle passing at all.

Phase 1 establishes the channel by **inheriting an endpoint at spawn**: the
broker spawns the content process with piped stdin and stdout via
`std::process::Command`, and those pipes are the channel. This is entirely safe
code, works identically on Windows and Linux, and gives the broker exactly the
property invariant 9 depends on — it knows which process a message came from
because it holds that process's pipe, not because the message says so.

In-band handle passing is **designed** in this phase — the `Channel` API leaves
room for it — and **implemented in Phase 2**, where `px-sandbox` exists, is
already making OS calls, and is already subject to adversarial review.

## Alternatives rejected

**`px-sandbox` owns all unsafe OS operations now.** The strongest long-term
answer, and probably where Phase 2 lands: it keeps the audited-unsafe surface at
exactly one crate, which is the property the rule exists to create rather than
an accident of naming. Rejected only for *this* phase, because implementing it
now means writing `SCM_RIGHTS` and `DuplicateHandle` code before anything needs
a handle passed, and unused unsafe is unreviewable unsafe.

**A new `px-os` crate.** Cleanest naming — the unsafe core called what it is —
but it adds a crate §3's architecture does not have, for a boundary we do not
yet need. Revisit in Phase 2 if `px-sandbox`'s remit turns out to be genuinely
two jobs.

**A dependency.** Rejected on principle: this is the exact operation whose
correctness the whole process model rests on.

## Consequences

**Phase 1 contains no `unsafe` anywhere**, and gate check 4 stays a source-text
assertion over 21 crates rather than 20 plus an exception.

**Phase 2 inherits a decision it must actually make.** This ADR defers, it does
not answer. Phase 2's ADR chooses between the widened `px-sandbox` and a new
`px-os`, with the benefit of knowing what the sandbox code actually looks like.

**Pipes are not the final transport.** stdin/stdout is a stream pair, adequate
for request/response with a length-prefixed framing layer, but it gives no
credential passing, no datagram boundaries, and no route to sending a handle.
Phase 2 or Phase 3 will replace it with a Unix socket and a named pipe. The
`px-ipc` API is written against `Read + Write` so that replacement changes the
transport and not its callers — and, usefully, so tests can drive the codec with
an in-memory hostile stream rather than a real process.

**One thing this buys that is easy to miss:** because the transport is generic
over `Read + Write`, the hostile-peer suite and the fuzz targets attack the
codec and the dispatch logic directly, at full speed, with no process spawn per
case. A transport that could only be exercised through a real child process
would make 24-hour fuzzing far less useful.

## Verification

Falsified if something in Phases 1–2 genuinely needs a handle passed before the
sandbox exists. Nothing in Phase 1's gate does. If Phase 2 finds one, this ADR
was wrong by one phase and the answer is the widened `px-sandbox`.
