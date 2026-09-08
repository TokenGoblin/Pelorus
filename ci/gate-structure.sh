#!/usr/bin/env bash
#
# Repository structure invariants that no other gate covers.
#
#   - every crate carries a CLAUDE.md (build-spec §5, Phase 0 deliverable)
#   - px-brand does not hold the update URL or signing key (§14.1)
#   - scripts CI executes are executable in the index
#   - no personal identity in git metadata
#
# OS-independent. CI runs it on Linux only.

. "$(dirname "$0")/lib.sh"

shopt -s nullglob
crates=(crates/px-*/)
if [ ${#crates[@]} -eq 0 ]; then
    fail "no crates found under crates/"
else
    for dir in "${crates[@]}"; do
        if [ -f "$dir/CLAUDE.md" ]; then
            ok "$dir CLAUDE.md"
        else
            fail "$dir has no CLAUDE.md"
        fi
    done
fi

# §14.1: renaming must never move the update endpoint. The way that breaks is
# somebody helpfully consolidating "all the constants" into px-brand.
BRAND_SRC="crates/px-brand/src/lib.rs"
if [ -f "$BRAND_SRC" ]; then
    if grep -qiE 'UPDATE_URL|SIGNING_KEY|PUBLIC_KEY|https?://' "$BRAND_SRC"; then
        fail "$BRAND_SRC looks like it holds an update URL or key; §14.1 puts"
        fail "  those in px-update, permanently, out of scope for a rename"
    else
        ok "px-brand holds no update endpoint or key (§14.1)"
    fi
fi

# core.filemode is false on a Windows checkout, so `chmod +x` never reaches
# git and a script committed there arrives on a Linux runner as 0644. The
# workflow invokes these directly; the whole gate would fail with "Permission
# denied" before running a single check. Rule: a shebang means executable.
while IFS= read -r -d '' path; do
    case "$path" in *.sh) ;; *) continue ;; esac
    head -n 1 "$path" | grep -q '^#!' || continue
    mode="$(git ls-files -s -- "$path" | cut -d' ' -f1)"
    if [ "$mode" = "100755" ]; then
        ok "$path is executable in the index"
    else
        fail "$path has a shebang but is mode $mode in the index;"
        fail "  fix with: git update-index --chmod=+x $path"
    fi
done < <(git ls-files -z 'ci/*' 'tests/*')

# This repository is public. Git metadata is the easiest place for a real name
# or a personal address to end up, because git takes them from whatever the
# machine happens to be configured with and nothing ever looks again.
#
# The rule is expressed as a shape, not as a name: commit identities must use
# a GitHub noreply address. A check that grepped for the maintainer's real name
# would have to contain the maintainer's real name, which is the leak it is
# trying to prevent.
bad_ident=0
while IFS='|' read -r hash ae ce; do
    case "$ae" in *@users.noreply.github.com) ;; *)
        fail "commit $hash has author email outside the noreply domain"
        bad_ident=1 ;;
    esac
    case "$ce" in *@users.noreply.github.com) ;; *)
        fail "commit $hash has committer email outside the noreply domain"
        bad_ident=1 ;;
    esac
done < <(git log --format='%h|%ae|%ce')
[ "$bad_ident" -eq 0 ] && ok "every commit identity uses a noreply address"

# Session links are account-scoped, useless to anyone reading the public repo,
# and tie the history to a particular person's activity.
if git log --format='%B' | grep -q 'claude\.ai/code/session'; then
    fail "a commit message contains a Claude session URL"
else
    ok "no session URLs in commit messages"
fi

verdict "structure"
