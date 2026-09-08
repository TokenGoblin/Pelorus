# Architecture decision records

An ADR precedes implementation, never documents it afterwards.

An ADR is required for:

- Any new crate dependency — recording what it does, why not write it,
  maintainer count, audit status, and unsafe-line count.
- Any change to a product invariant (`docs/build-spec.md` §1).
- Any change to a decision settled in Appendix A. Say explicitly that you
  think it is wrong and stop; do not quietly work around it.
- Any increase in the dependency-tree unsafe-line count over the committed
  baseline.
- Each decision listed as open in §13, by its due phase.

Numbering is sequential and permanent: `NNN-slug.md`. A superseded ADR is
marked superseded and kept; it is never deleted or edited into agreement with
what replaced it.

The template is `docs/adr/template.md` — a Phase 0 deliverable, along with
`000-name.md` and `001-threat-model.md`.
