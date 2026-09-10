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
#
# Two forms are accepted. <id>+<user>@users.noreply.github.com is a person's
# GitHub-provided address, which is the point. noreply@github.com is what
# GitHub itself sets as committer for a commit made through the web UI, and
# carries no identity at all.
bad_ident=0
while IFS='|' read -r hash ae ce; do
    case "$ae" in *@users.noreply.github.com | noreply@github.com) ;; *)
        fail "commit $hash has author email outside the noreply domain"
        bad_ident=1 ;;
    esac
    case "$ce" in *@users.noreply.github.com | noreply@github.com) ;; *)
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

# Every fuzz target's committed corpus is checked through the git index, not
# through the filesystem.
#
# An empty directory is invisible to git: it cannot be committed, and
# `git status` does not report it, so `fuzz/corpus/dom_stale_handle` existed on
# the machine that ran the gate and did not exist anywhere else. The px-dom
# harness test read it with read_dir, found it, and passed. In CI the directory
# was absent and the same test panicked. The `dom` job was red for five commits
# while three commit messages said it was green.
#
# That is the fourth time this project has shipped a check that passes locally
# and fails in CI, so the check is written against the index -- the only view of
# the tree that is the same on every machine.
#
# Targets with no seeds are named here rather than allowed by silence. The list
# is a debt, not a policy: an empty corpus means the campaign starts that target
# from nothing.
corpus_empty="http_response http_chunked"
corpus_bad=0
while read -r target; do
    [ -n "$target" ] || continue
    seeds="$(git ls-files -- "fuzz/corpus/$target" | wc -l)"
    case " $corpus_empty " in
        *" $target "*)
            if [ "$seeds" -eq 0 ]; then
                ok "$target has no committed corpus, which is a known gap"
            else
                fail "$target now has $seeds committed seed(s);"
                fail "  remove it from corpus_empty in $0"
                corpus_bad=1
            fi
            ;;
        *)
            if [ "$seeds" -gt 0 ]; then
                ok "$target has $seeds committed corpus seed(s) in the index"
            else
                fail "fuzz target $target has no corpus file in the git index."
                fail "  A directory that exists only on your machine is not a corpus:"
                fail "  git cannot store an empty directory, so CI will not see it."
                corpus_bad=1
            fi
            ;;
    esac
done < <(grep -E '^name = ' fuzz/Cargo.toml | sed -e 's/^name = "//' -e 's/"$//' | grep -v '^px-fuzz$')
[ "$corpus_bad" -eq 0 ] && ok "every fuzz target's corpus is accounted for"

verdict "structure"
