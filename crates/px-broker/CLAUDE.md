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
- A dead content process is routine, not an error path to be surprised by.
- Ships as the `px-browser` binary. The name is neutral so that no compiled
  artifact carries the brand; packaging/ renames it (§2.1).
