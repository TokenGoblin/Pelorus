# 015 — HTTP/1.1, written here; HTTP/2 deferred to its own phase

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 3
- **Invariants touched:** none
- **Narrows:** build-spec §9 Phase 3, which lists "HTTP/1.1 and HTTP/2". This
  ADR delivers the first and defers the second. **That is a reduction in stated
  scope**, and it is said here rather than discovered later in a gate report.

## Context

§9 Phase 3 asks for HTTP/1.1 and HTTP/2 in `px-net`. §3's dependency policy
says to write networking *policy* ourselves and take `rustls` from the
ecosystem. It names no HTTP implementation and no async runtime, which means
HTTP/2 arrives either as a dependency nobody has approved or as several
thousand lines of new parser.

The two are not comparable in cost.

**HTTP/1.1** is a line-oriented protocol with a small number of genuinely
dangerous edge cases — chunked encoding, the `Content-Length`/`Transfer-Encoding`
conflict that produces request smuggling, header folding, and the framing rules
that decide where one message ends. It is perhaps a thousand lines, it is
exactly the kind of parser this project's fuzzing infrastructure already exists
to attack, and every one of those edge cases is a decision we want to make
deliberately rather than inherit.

**HTTP/2** is HPACK — a stateful compressor with its own dynamic table and its
own decompression-bomb class — plus stream multiplexing, flow control windows,
settings negotiation, priority, and connection-level error handling. Getting it
wrong is remotely reachable on every connection. Realistically it needs `h2`,
which needs `tokio`, which takes this workspace from thirteen external crates
to roughly fifty and introduces an async runtime that reshapes `px-net`'s
architecture and, through it, the broker's.

There is no version of "add HTTP/2 as a detail of Phase 3" that is honest.

## Decision

**Phase 3 ships a hand-written HTTP/1.1 client, and no HTTP/2.**

`px-net` negotiates `http/1.1` via ALPN and does not offer `h2`. A server that
speaks only HTTP/2 is unreachable, and says so plainly rather than failing
obscurely.

**HTTP/2 gets its own ADR and its own phase**, taken up when there is a
concrete trigger rather than on principle. Two triggers are named now so the
decision is not indefinitely deferred by inattention:

- the Phase 23 compat suite failing on sites that are unreachable over
  HTTP/1.1, which is the measurement that would make this a real user problem;
  or
- Phase 8 or later needing multiplexing for a reason other than compatibility.

The deferred ADR must decide the dependency question — `h2` plus a runtime, or
written here — with the numbers this one only estimates.

## Alternatives rejected

**Take `hyper` + `h2` + `tokio` now.** Correct, battle-tested, and it delivers
§9 Phase 3 as written. Rejected for this phase on scale: about forty new crates
and an async runtime, adopted before there is a single consumer that needs
multiplexing, in a workspace whose entire external surface is currently
thirteen crates and whose unsafe baseline is already dominated by two
first-party binding crates. It also front-loads the largest architectural
commitment in the project onto the phase that can least evaluate it — nothing
yet renders a page, so there is no workload to judge it against.

**Hand-roll HTTP/2 now.** Keeps the dependency count at zero and keeps every
parser ours and fuzzable, which is genuinely this project's preference.
Rejected on effort and risk together: it is a phase of work on the code path
where a bug is reachable by any server the user connects to, written before
HTTP/1.1 has been exercised against a single real site.

**Ship HTTP/1.0.** Not seriously considered; no persistent connections makes
the pooling this phase's gate requires meaningless.

## Consequences

**Some sites will be unreachable.** In practice very few: essentially every
server that speaks HTTP/2 also speaks HTTP/1.1, because HTTP/2 has always been
negotiated by ALPN with 1.1 as the fallback. The exceptions are gRPC endpoints
and a small number of hosts configured HTTP/2-only, neither of which the Phase
3 gate's 200 URLs need.

**Performance is worse, measurably**, on pages with many subresources: no
multiplexing means head-of-line blocking per connection and more connections
per origin. This is a real regression against every shipping browser and is
accepted for now. It should be measured at Phase 23 rather than assumed
tolerable.

**The connection pool is built for one protocol and will have to accommodate
two.** Keeping that in mind is cheap now and expensive later, so the pool is
keyed by `(partition key, origin, protocol)` from the start even though the
protocol component takes exactly one value today. A pool that assumes one
connection per origin is a pool that has to be rewritten for multiplexing.

**§9's Phase 3 line no longer matches what the phase delivers**, and the gate
report will say so rather than quietly grading against a smaller target. If
anyone later reads §9 and expects HTTP/2 in `px-net`, this ADR is the answer to
where it went.

## Verification

Wrong if the compat work finds sites that matter and cannot be reached over
HTTP/1.1 — that is the first named trigger, and it is a measurement rather than
an opinion.

Wrong if the deferral turns out to have been permanent by neglect rather than
by decision. The triggers above exist to make that visible; a Phase 23 gate
that is failing on protocol support and has no HTTP/2 ADR open is the signal
that this decision was allowed to rot.

It is *not* falsified by HTTP/2 being faster. That was never in question.
