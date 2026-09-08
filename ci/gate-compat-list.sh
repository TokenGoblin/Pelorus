#!/usr/bin/env bash
#
# The forty-site compat list (build-spec §8).
#
# Deliberately NOT one of the five Phase 0 gate checks in §9 — those are build,
# reproducibility, supply chain, forbid(unsafe_code) and the brand leak. This
# is a Phase 0 deliverable that only the person who will daily-drive the
# browser can produce, so it gets its own job. A red job here says "the list is
# outstanding", not "the phase gate failed", and the distinction is worth a
# separate line in CI.

. "$(dirname "$0")/lib.sh"

if [ -z "$PY_BIN" ]; then
    fail "no python >= 3.11 on PATH; cannot check the compat site list"
else
    "$PY_BIN" ci/check_compat_sites.py || fail "compat site list is not ready"
fi

verdict "compat-list"
