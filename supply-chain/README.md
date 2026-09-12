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

**"Vetting Succeeded (92 fully audited, 3 partially audited, 76 exempted)"** —
and the third number is the one to read.

Phase 0 had zero third-party dependencies and this section used to say so. Phase
5 added `stylo` (ADR 023): **+108 crates** in `Cargo.lock`. The five audit sets
imported above cover all but 76 of them, and refreshing those sets closed 30 on
its own, because Mozilla audits much of its own CSS engine's graph.

### An exemption is not an audit

Before Phase 5 this store held **one** exemption, `redox_syscall`, with a
paragraph in `config.toml` arguing why it was an exemption rather than a trust
entry. It now holds **77**, and 76 arrived in a single commit.

That difference is not cosmetic. An exemption records a crate trusted *without
anybody here having read it*. One, carefully argued, is a considered risk.
Seventy-six is a statement about how much of this tree is vouched for by nobody,
and the number belongs somewhere a person will see it rather than spread across
230 lines of generated TOML. `cargo vet fmt` strips comments from `config.toml`,
which is why it is here.

### What would actually reduce it

Two things, both in `docs/backlog.md`:

- **Import another trusted audit set.** `cargo vet` suggests `zcash`, which
  would cut the remaining diff substantially. Not done in Phase 5 on purpose:
  adding a trust root is a supply-chain decision of the same kind as adding a
  dependency, and this project requires an ADR for those. It should be a
  deliberate choice rather than a side effect of a phase that wanted a number to
  go down.
- **Audit the crates that carry the risk.** The 76 are not equal. `zerovec`
  (253 unsafe lines), `crossbeam-epoch` (195), `thin-vec` (88) and
  `atomic_refcell` exist to do things the borrow checker cannot express, and are
  worth reading in a way that a derive macro is not.

The failure mode to avoid is adding to the exemption list to make a gate pass.
Each entry is a crate nobody read, and the count is the point.
