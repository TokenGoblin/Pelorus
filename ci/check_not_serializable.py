"""Assert a type is not serialisable.

Used for `ChannelId`, which is the broker's answer to "who sent this". If it
could be serialised it could travel inside a message, and then it would be a
claim rather than an observation (invariant 9).

Why this is a parser and not a grep. The check began as:

    grep -B3 'struct ChannelId' file | grep -q 'Serialize'

which reports success when the first grep matches nothing at all — so renaming
or moving the type turned it into a check that passed having inspected nothing.
The repair required `derive(` and `Serialize` on the same line, which a
multi-line derive defeats — and a multi-line derive is exactly what rustfmt
produces once the list is long, in a project that runs `cargo fmt --check`.

Both failures share a shape: a check that cannot fail looks identical to a
check that passes. So this reads the whole attribute block, however it is
formatted, and exits non-zero if it cannot find the type at all.

Usage: check_not_serializable.py <TypeName> <file> [file...]
Exit 0 if the type is found and is not serialisable, 1 otherwise.
"""

import re
import sys

TRAITS = ("Serialize", "Deserialize")


def attribute_block_before(lines, index):
    """Collect the contiguous attribute block immediately above `index`.

    Walks backwards over `#[...]` attributes and their continuation lines,
    which is what makes a derive spread over ten lines as visible as one on a
    single line.
    """
    block = []
    depth = 0
    i = index - 1
    while i >= 0:
        line = lines[i].strip()
        if not line:
            i -= 1
            continue
        if line.startswith("///") or line.startswith("//"):
            i -= 1
            continue
        depth += line.count(")") + line.count("]")
        depth -= line.count("(") + line.count("[")
        block.append(line)
        if line.startswith("#["):
            depth = 0
            i -= 1
            continue
        if depth <= 0 and not line.startswith("#"):
            # Not part of an attribute block.
            block.pop()
            break
        i -= 1
    return "\n".join(reversed(block))


def main():
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2

    type_name = sys.argv[1]
    paths = sys.argv[2:]
    declaration = re.compile(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+" + re.escape(type_name) + r"\b"
    )
    manual_impl = re.compile(
        r"impl\b[^;{]*\b(" + "|".join(TRAITS) + r")\b[^;{]*\bfor\s+" + re.escape(type_name) + r"\b"
    )

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
            block = attribute_block_before(lines, i)
            for trait in TRAITS:
                if re.search(r"\b" + trait + r"\b", block):
                    print(
                        f"FAIL {path}:{i + 1} {type_name} derives {trait}\n"
                        f"      attribute block was:\n"
                        + "\n".join("        " + b for b in block.splitlines()),
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
