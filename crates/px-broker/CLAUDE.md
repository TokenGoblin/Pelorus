# px-broker

Parent process, capability broker, policy, frame tree. Implemented in phase 1.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Authority comes from the channel, never from the message. Identify a
  caller by the connection a request arrived on. If a request carries its
  own identity, origin, partition key or TabId, that is a bug in the
  sender and a rejection here (invariant 9).
- This process owns every OS handle. Nothing else opens one.
- The broker unwinds, and wraps IPC dispatch in catch_unwind (§4.3). It is
  the one process that may not die.
- A dead content process is routine, not an error path to be surprised by —
  and a STALLED one is the harder case. It is cheaper for an attacker than
  crashing, and a liveness check cannot detect it: a child can be dead by
  try_wait while a grandchild still holds its stdout and the read blocks
  forever. Every wait on a content process has a deadline. Never add a
  blocking read or write to a peer on the broker's own thread.
- Hosting a frame and being allowed to ask about one are separate sets.
  grant_visibility is how a FrameId resolves remotely (§3); it conveys sight,
  never ownership.
- What a peer is told about a refusal is not what the log records. Response
  carries a bare Denied; DenyReason lives in Decision.audit. Distinguishing
  "gone" from "not yours" on the wire let one site enumerate another's frames.
- Ships as the `px-browser` binary. The name is neutral so that no compiled
  artifact carries the brand; packaging/ renames it (§2.1).
