# HTML tree-construction conformance corpus

62 `.dat` files, 622 KB. Read by `crates/px-dom/tests/html5lib.rs`, which is
where the ≥99% figure in `ci/gate-dom.sh` comes from.

## Where these came from, and why the name says html5lib

build-spec §9 Phase 4 names the gate item **"html5lib-tests ≥99%"**. When that
was written the corpus lived in `html5lib/html5lib-tests`, under
`tree-construction/`. It does not any more. That repository now contains a
one-line README:

> The HTML parser tree construction tests are now solely maintained on
> web-platform-tests:
> https://github.com/web-platform-tests/wpt/tree/master/html/syntax/parsing

So these files come from **web-platform-tests**, at commit
`2b6223e2c8ca580ef20e55c548f1b94e6bb4948d`, path
`html/syntax/parsing/resources/*.dat`. Same corpus, same `.dat` format, same
tests — a new address, not a different target. The directory keeps the name
`html5lib` because that is what the spec, the gate and every other engine call
this corpus.

This is written down rather than quietly substituted because "the gate says
html5lib-tests and the files come from WPT" is exactly the kind of discrepancy
that looks like someone grading against a corpus of their own choosing.

WPT is also where Phase 12 goes, so the tooling is not wasted.

## Vendored, not fetched

Same reason `ci/gate-network.sh` replays recorded traffic: a gate that needs
the internet is a gate that goes red when a third party has an outage, and one
that has been red for a reason nobody controls is one people learn to ignore.
Vendoring also fixes the target — "≥99%" means the same thing in a year.

Updating is deliberate: re-run the fetch, note the new commit here, and expect
the pass rate to move. A corpus that silently tracked upstream would turn a
conformance gate into a weather report.

## Licence

3-Clause BSD, `LICENSE.md`, copied from the same commit. This is test data
rather than a code dependency, so it is not in `deny.toml`'s allow list, which
covers the crate graph.
