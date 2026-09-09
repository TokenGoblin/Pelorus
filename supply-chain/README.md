# supply-chain/

`cargo-vet` store. The three files here are owned by the tool — `cargo vet fmt`
rewrites them and strips comments — so the reasoning lives in this file
instead.

| File | What |
|---|---|
| `config.toml` | Which audit sets we import, and any policy exemptions |
| `audits.toml` | Audits performed by this project. Ours, not imported. |
| `imports.lock` | The imported audits, pinned. Committed deliberately. |

## Why we import rather than audit

Mozilla's and Google's audit sets are imported because auditing a browser
dependency tree unaided is a project of its own, and a gate that costs a month
per dependency is a gate that gets waived by month two (build-spec §5). What we
gain is coverage of the crates two browser vendors already read. What we accept
is that their judgement, not ours, is what stands behind most of the tree.

`imports.lock` is committed and CI runs `cargo vet --locked`, so a run checks
against a fixed set rather than against whatever the upstream files happen to
say today. Refreshing it is `cargo vet regenerate imports`, and it is a
deliberate commit — an upstream audit set that changed under us is exactly the
kind of movement worth seeing in a diff.

## What a pass means today

Nothing yet. Phase 0 has zero third-party dependencies, and `cargo vet` says so
in those words: "Vetting Succeeded (because you have no third-party
dependencies)." The store is wired now so that Phase 3 — the first real
dependency — lands against a policy that already exists rather than one written
under pressure to get a build green.
