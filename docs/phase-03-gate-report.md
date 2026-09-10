# Phase 3 gate report

*Network core. Branch `phase/03-network`.*

## The four gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| 200 URLs fetched correctly | `net_fetches_*` ×3, against a generated corpus and a local replay server | **Pass** |
| 24h fuzz on HTTP framing | `fuzz-campaign` run [34389705961](https://github.com/TokenGoblin/Pelorus/actions/runs/34389705961) | **Pass**, with a limitation recorded below |
| Zero plaintext DNS, zero connections outside the requested set | `net_connects_only_*` ×4, plus the symbol audit | **Pass**, with a stated caveat |
| PSL version asserted, stale-PSL test fails | `psl_version_*` ×6 | **Pass** |

168 tests. `ci/gate-network.sh` passes locally on Windows; the only red CI job
is `compat-list`, which is Phase 0's outstanding deliverable and is red on
`main` as well.

## The campaign

Eight shards, `-max_total_time=14400` each, **33.9 billion executions, no
crashes and no artifacts written**.

| Target | Shard | Runs | cov | ft | corpus |
|---|---|---:|---:|---:|---|
| `frame_request` | 1 | 12,321,131,188 | 170 | 209 | 87 / 1,784 b |
| `channel_stream` | 1 | 3,819,434,553 | 176 | 629 | 236 / 16 KiB |
| `channel_stream` | 2 | 2,439,332,349 | 178 | 716 | 279 / 596 KiB |
| `broker_sequence` | 1 | 3,210,910 | 512 | 3,335 | 1,023 / 241 KiB |
| `frame_response` | 1 | 7,504,978,187 | 152 | 190 | 79 / 966 b |
| `http_response` | 1 | 1,282,392,433 | 409 | 1,539 | 554 / 94 KiB |
| `http_response` | 2 | 1,309,271,834 | 405 | 1,542 | 596 / 116 KiB |
| `http_chunked` | 1 | 5,266,617,898 | 122 | 451 | 216 / 9,543 b |

**`-max_len` was verified applied rather than assumed**, the three ways Phase 1
learned to check: the flag appears on all eight `Running` lines, the
`-max_len is not provided` warning appears **zero** times, and libFuzzer's own
`lim:` field reaches 1,100,000 and holds there for 10,102 log lines. The third
is the load-bearing one — a flag on a command line proves it was passed, `lim:`
proves libFuzzer acted on it.

The new targets earned their shards. `http_response` reached 409 coverage
points and ~1,540 features against a parser that did not exist this morning,
which is more than any IPC target except `broker_sequence`.

### The limitation, which the numbers do not show

**`-max_len` is 1,100,000, and that number was chosen for a different
boundary.** It straddles `px-ipc`'s `MAX_MESSAGE_BYTES` (1,048,576)
deliberately, which is why it exists. The HTTP parser's own limits are
`MAX_BODY_BYTES` at 32 MiB and `MAX_CHUNK_BYTES` at 8 MiB, and **neither was
approached**. So the honest statement of this gate item is: 24 CPU-hours found
no crash in HTTP framing for inputs up to 1.1 MB, and the parser's own size
boundaries are unfuzzed.

That is the same shape of gap Phase 1 hit, caught earlier this time — before
recording a pass rather than after. It is not a reason to withhold the item:
the bounds themselves are unit-tested, and a 32 MiB `-max_len` would spend the
campaign's budget generating enormous inputs instead of exploring structure.
It is a reason to say what was and was not covered.

**Since fixed.** `ci/gate-fuzz-smoke.sh` now picks `-max_len` per target: the
HTTP targets get 8,500,000, just past `MAX_CHUNK_BYTES`, and everything else
keeps 1,100,000. Deliberately not 32 MiB — the chunk bound is the one a
declared length reaches directly, and a 32 MiB ceiling would spend the budget
generating enormous inputs instead of exploring structure. **The numbers in
the table above predate that change**, and the next campaign is what will
exercise the wider range. Saying so because a report that quietly implied the
recorded run used the new configuration would be describing a campaign nobody
ran.

## What the phase built

`px-net` went from a six-line stub to twelve modules: TLS and trust anchors,
an HTTP/1.1 client, the public suffix list, the HSTS preload list, partition
keys, a connection pool, cookies, redirect policy, and DNS message handling
with the resolver choice.

Five ADRs, four of them decisions the spec named and one it did not:

- **012** — the root store: platform as the source of trust, bundled as a
  floor, never merged.
- **013** — revocation by pushed CRL set; why there is no OCSP, recorded so the
  rule is not relaxed by someone reasonable.
- **014** — the system resolver by default, DoH by choice, no provider compiled
  in. Settles §13 open decision 5.
- **015** — HTTP/1.1 now, HTTP/2 deferred. **This narrows §9's stated scope**
  and says so in its own header.
- **016** — rustls with `ring` rather than `aws-lc-rs`, chosen on measured
  numbers. §3 names rustls; it does not name the crypto backend, which is where
  the risk actually lives.

## The design decision this phase kept making

Refuse rather than resolve. It shows up four times, in four modules, and it is
the same argument each time: where a specification offers a tiebreak, a message
that *needs* the tiebreak was probably not written by anyone honest, and a
client that breaks the tie differently from an upstream is the other half of a
bug.

- **HTTP framing.** `Content-Length` and `Transfer-Encoding` together is
  rejected, not resolved in favour of the latter as RFC 9112 §6.1 permits. Two
  `Content-Length` headers are rejected even when they agree.
- **Redirects.** A downgrade to plaintext, userinfo in the authority, a scheme
  this crate does not fetch — refused rather than normalised. Userinfo in
  particular: silently *stripping* `https://evil@good.example` resolves an
  ambiguity the user could not see.
- **HSTS.** A preloaded host over plaintext is refused rather than upgraded,
  because upgrading changes the origin and therefore the partition, after the
  caller built a key for the other one.
- **Cookies.** An unparseable attribute rejects the cookie rather than becoming
  a default. Accepting what parses and ignoring the rest is how a `Secure` flag
  gets dropped silently.

## What is weaker than it sounds

**TLS is configured but not exercised end to end.** ALPN offers `http/1.1`
only, there is no way to disable verification, and the trust anchors come from
the platform store — but the gate's 200 URLs go over plaintext loopback, so no
handshake happens in any test. That needs a test CA, and until it exists the
TLS path is reviewed code rather than tested code.

**The unsafe baseline no longer measures what it claims.** ADR 016 brought in
`ring`: 120,164 lines of assembly and 5,413 of C that `ci/unsafe-audit.sh`
cannot see, because it counts `unsafe` tokens in Rust. It reports `ring` as
230. The number went from 13,341 to 17,294 and the real increase is far larger.
In `docs/backlog.md`.

**Revocation coverage is partial and may stay that way.** ADR 013 chose pushed
CRL sets, which fail closed within their coverage — but curating a set is a
sourcing problem this project may not be able to solve alone. The honest
options are consuming somebody else's set or having no coverage.

**The DNS gate item measures this process, not the machine.** Under ADR 014's
default the *operating system* resolves names, so "zero plaintext DNS" means
this process opened no DNS socket. It does not mean no plaintext DNS left the
machine. Written in the gate's own header so a green line is not read as the
stronger claim.

**Redirect targets with Unicode hosts are refused, not converted.** `url` and
`idna` would resolve them properly and cost 29 crates, almost all ICU4X
machinery. Deferred to its first real consumer; the refusal fails closed and
will be wrong for real sites until then.

## What §9 asks for and this phase did not deliver

**`px-net` is a library, not a process.** §9 opens with "`px-net` as its own
process" and the gate does not check for it, so a passing gate does not mean a
complete phase.

It stopped for a specific reason rather than a general one. A broker→px-net
fetch request has to name a destination and a partition, and `ci/gate-ipc.sh`
forbids exactly those field names anywhere under `crates/px-ipc/src/`. That
rule is right for the direction it was written for — a content process must
never describe its own authority — and broker→px-net is a different
relationship, because the broker *is* the authority. The ways out are a gate
amendment scoping the ban by direction, or a design where px-net learns the
partition from which channel a request arrived on. Both are protocol-shape
decisions, and **ADR 009 is marked PROPOSED with an explicit note that it was
not taken on the standing authorisation.** Renaming fields to slip past the
regex would be gaming a check this project put there deliberately.

**HTTP/2**, by ADR 015, with two named triggers so the deferral cannot become
permanent by neglect.

**The DoH query is not wired to the network.** Encoding and parsing are done
and tested; sending one means an HTTPS request, which needs a partition key,
which needs a resolved name. Which partition a DoH lookup belongs to — its own,
the requesting site's, or a shared one that would itself be an oracle — is a
real decision and is better made than settled by whichever call site was
written first.

**`data/` has no root store**, and §9 lists one. ADR 012 supersedes that: the
platform store is the source of trust and `webpki-roots` is the floor, so there
is no file to version. Recorded in `data/README.md` rather than left looking
like an omission.

## What CI caught that local runs did not

Worth its own section, because it is the second time this project has been
saved by a check rather than by attention.

The gate went red across nine jobs after a push that passed everything locally.
Three causes, all mine: the product name in two User-Agent strings and a HAR
key, breaking a hard rule three times in one afternoon; two scripts chmod'd
locally without staging the mode; and a gate that required 200 files in
`tests/compat/archives`, which `.gitignore` deliberately excludes because ADR
002 keeps recorded archives out of a public repository.

That last one would have stayed red forever. Locally the files existed because
they had just been written, and git had been ignoring them the whole time
without saying anything. The fix was to notice that a generated fixture whose
every host is the loopback address is not the same thing as recorded traffic
from real sites, and to stop storing one where the other belongs.

## Verdict

**All four gate items pass**, and the campaign was verified to have tested what
it was meant to rather than merely reported clean.

The phase is **not closeable** on the gate alone. §9 asks for a process
boundary this phase did not build, and the reason is a decision that belongs to
whoever owns ADR 009 — which is marked PROPOSED precisely so it is not decided
from underneath by whichever implementation needs it first.

Said plainly: what this phase establishes is that a request can be made safely,
partitioned correctly, and refused when anything is ambiguous. What it does not
establish is that the network lives in its own process, which is the first line
of §9 Phase 3 and the part a gate cannot check.
