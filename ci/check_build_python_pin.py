"""Check that a Python environment matches build-python/requirements.txt exactly.

ADR 023: stylo's `build.rs` generates its property definitions through Python 3
and Mako, so both are build inputs and invariant 7 now reads "same source + same
toolchain + same pinned build-time generators". This is what makes the pin real
rather than described: the environment must contain exactly the pinned versions
and nothing else.

Usage:  <interpreter-to-check> ci/check_build_python_pin.py <requirements.txt>

Exit 0 if the environment matches, 1 if it does not, 2 if the requirements file
is not made of exact pins. Diagnostics go to stderr; nothing goes to stdout,
because the caller is substituting an interpreter path out of it.

This is a separate file rather than a heredoc inside ci/setup-build-python.sh on
purpose. The comparison it replaces was written in shell and was wrong four times
in a row -- `pip freeze` emits CRLF on Windows, and every attempt to spell a
carriage return through `tr` produced a filter that deleted nothing while the
failure message printed `wanted` and `got` identically. Comparing parsed
name/version pairs cannot be defeated by a line ending.
"""

from __future__ import annotations

import subprocess
import sys


def parse(lines: list[str], source: str) -> dict[str, str]:
    """Parse `name==version` lines into a dict, rejecting anything inexact."""
    out: dict[str, str] = {}
    for raw in lines:
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if "==" not in line:
            # A range or a bare name would reintroduce exactly what ADR 023
            # closed: a release binary whose contents depend on what happened to
            # be installed on the machine that built it.
            print(f"FAIL {source}: {line!r} is not an exact pin", file=sys.stderr)
            sys.exit(2)
        name, version = line.split("==", 1)
        # PEP 503 normalisation, minus the parts that cannot appear here: pip
        # reports `MarkupSafe`, a requirements file may say `markupsafe`, and
        # they are the same package.
        out[name.strip().lower().replace("_", "-")] = version.strip()
    return out


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    req_path = sys.argv[1]

    with open(req_path, encoding="utf-8") as fh:
        want = parse(fh.readlines(), req_path)

    if not want:
        print(f"FAIL {req_path} pins nothing", file=sys.stderr)
        return 2

    frozen = subprocess.run(
        [sys.executable, "-m", "pip", "freeze", "--disable-pip-version-check"],
        capture_output=True,
        text=True,
        check=False,
    )
    if frozen.returncode != 0:
        print(f"FAIL pip freeze failed: {frozen.stderr.strip()}", file=sys.stderr)
        return 1

    have = parse(frozen.stdout.splitlines(), "pip freeze")

    if want == have:
        summary = ", ".join(f"{k}=={v}" for k, v in sorted(want.items()))
        print(f"  {summary}", file=sys.stderr)
        return 0

    wrong = {k: v for k, v in want.items() if have.get(k) != v}
    if wrong:
        detail = ", ".join(
            f"{k}=={v} (have {have.get(k, 'nothing')})" for k, v in sorted(wrong.items())
        )
        print(f"  pin not satisfied: {detail}", file=sys.stderr)

    extra = {k: v for k, v in have.items() if k not in want}
    if extra:
        # An undeclared package in the environment is a build input nobody wrote
        # down, which is the same failure as an unpinned one.
        detail = ", ".join(f"{k}=={v}" for k, v in sorted(extra.items()))
        print(f"  present but not pinned: {detail}", file=sys.stderr)

    return 1


if __name__ == "__main__":
    sys.exit(main())
