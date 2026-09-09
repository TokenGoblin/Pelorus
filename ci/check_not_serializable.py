"""Assert a type is not serialisable.

Used for `ChannelId`, which is the broker's answer to "who sent this". If it
could be serialised it could travel inside a message, and then it would be a
claim rather than an observation (invariant 9).

## This is the weaker of two checks, on purpose

`ci/gate-ipc.sh` also asserts that `px-broker` has no `serde` dependency at
all. That is the check that actually holds the property: without a dependency,
`#[derive(Serialize)]` cannot name the trait under any spelling or alias, so
the guarantee is structural rather than a matter of inspection. This file is
defence in depth, and it is the only thing that catches a hand-written `impl`.

## Why it is deliberately crude

Two smarter versions failed. First:

    grep -B3 'struct ChannelId' file | grep -q 'Serialize'

reports success when the first grep matches nothing at all, so renaming or
moving the type turned it into a check that passed having inspected nothing.
Requiring `derive(` and `Serialize` on the same line then missed a multi-line
derive — which is what rustfmt emits once the list is long. A backwards walk
balancing brackets missed this, because brackets inside a string literal count
as delimiters and drove the walk to stop early:

    #[derive(Serialize)]
    #[doc = concat!(
        "((("
    )]
    pub struct ChannelId(u64);

So this version does no parsing. It looks for a derive mentioning the trait
anywhere in the window above the declaration, and over-approximates: an
unrelated serde-derived type within `WINDOW` lines would fail the gate. That is
fail-closed, it cannot arise in a crate with no serde dependency, and it has no
clever failure mode for formatting to defeat.

Usage: check_not_serializable.py <TypeName> <file> [file...]
Exit 0 if the type is found and is not serialisable, 1 otherwise.
"""

import re
import sys

TRAITS = ("Serialize", "Deserialize")

# Generous. A derive block, however formatted, plus doc comments, is well under
# this — and being too generous only makes the check stricter.
WINDOW = 30


def main():
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2

    type_name = sys.argv[1]
    paths = sys.argv[2:]

    declaration = re.compile(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+" + re.escape(type_name) + r"\b"
    )
    trait_alt = "|".join(TRAITS)
    manual_impl = re.compile(
        r"impl\b[^;{]*\b(" + trait_alt + r")\b[^;{]*\bfor\s+" + re.escape(type_name) + r"\b"
    )
    derived = re.compile(r"\b(" + trait_alt + r")\b")

    found = False
    bad = False

    for path in paths:
        try:
            with open(path, encoding="utf-8") as fh:
                text = fh.read()
        except OSError as exc:
            print(f"FAIL cannot read {path}: {exc}", file=sys.stderr)
            return 1

        lines = text.splitlines()

        for i, line in enumerate(lines):
            if not declaration.match(line):
                continue
            found = True

            start = max(0, i - WINDOW)
            window = lines[start:i]
            # Only look when the window mentions a derive at all, so a bare
            # `use serde::Serialize` import elsewhere in the file does not fail
            # every type declared after it.
            if not any("derive" in w for w in window):
                continue
            for offset, w in enumerate(window):
                # Comments are skipped, and not as a convenience: the type's
                # own doc comment explains that it must never be `Serialize`,
                # so a scan that reads comments fails on the very sentence
                # documenting the property it is checking.
                if w.strip().startswith("//"):
                    continue
                hit = derived.search(w)
                if hit:
                    print(
                        f"FAIL {path}:{start + offset + 1} a derive above "
                        f"{type_name} (declared at line {i + 1}) mentions "
                        f"{hit.group(1)}:\n        {w.strip()}",
                        file=sys.stderr,
                    )
                    bad = True

        for match in manual_impl.finditer(text):
            found = True
            bad = True
            line_no = text.count("\n", 0, match.start()) + 1
            print(
                f"FAIL {path}:{line_no} {type_name} has a hand-written "
                f"{match.group(1)} impl",
                file=sys.stderr,
            )

    if not found:
        print(
            f"FAIL {type_name} was not declared in any of: {' '.join(paths)}\n"
            f"      this check must not pass by failing to find its subject",
            file=sys.stderr,
        )
        return 1

    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
