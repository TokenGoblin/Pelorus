"""Assert the compat site list is complete and usable (build-spec §8).

Forty sites, each with at least one scripted assertion. The list is the real
progress meter for the whole project, so an entry with no assertion is worse
than a missing entry: it counts towards forty and tests nothing.
"""

import sys
import tomllib

REQUIRED = 40
PATH = "tests/compat/sites.toml"

try:
    with open(PATH, "rb") as fh:
        doc = tomllib.load(fh)
except FileNotFoundError:
    print(f"FAIL {PATH} does not exist", file=sys.stderr)
    sys.exit(1)
except tomllib.TOMLDecodeError as exc:
    print(f"FAIL {PATH} is not valid TOML: {exc}", file=sys.stderr)
    sys.exit(1)

sites = doc.get("site", [])
failures = 0

for i, site in enumerate(sites):
    where = site.get("id") or f"site[{i}]"
    for field in ("id", "url"):
        if not site.get(field):
            print(f"FAIL {where}: missing {field}", file=sys.stderr)
            failures += 1
    if not site.get("assert"):
        print(f"FAIL {where}: no assertions — an entry that tests nothing "
              f"still counts towards forty", file=sys.stderr)
        failures += 1

ids = [s.get("id") for s in sites if s.get("id")]
for dup in sorted({i for i in ids if ids.count(i) > 1}):
    print(f"FAIL duplicate site id: {dup}", file=sys.stderr)
    failures += 1

if len(sites) != REQUIRED:
    print(f"FAIL {PATH} lists {len(sites)} sites, expected {REQUIRED} "
          f"(build-spec §8)", file=sys.stderr)
    failures += 1

if failures:
    sys.exit(1)
print(f"ok   {len(sites)} compat sites, all with assertions")
