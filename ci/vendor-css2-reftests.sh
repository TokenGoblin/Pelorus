#!/usr/bin/env bash
#
# Vendor the WPT CSS2 reftest subset into tests/wpt/css2/.
#
# ADR 029. The subset is committed rather than fetched at test time, for the
# reason every vendored corpus in this project is: a test corpus that changes
# between runs is not a threshold, and CI that reaches the network to decide
# whether a gate passes is CI that goes red when GitHub does.
#
# Run deliberately, not from CI. Re-running it is how the subset is refreshed, and
# refreshing it is a commit whose diff shows exactly which tests moved — which is
# the point. `tests/wpt/css2/PROVENANCE` records the commit the files came from.
#
# Usage:
#   ci/vendor-css2-reftests.sh            # fetch at WPT master
#   ci/vendor-css2-reftests.sh <sha>      # fetch at a specific commit

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

DEST="tests/wpt/css2"
CRASH_DEST="${CRASH_DEST:-tests/wpt/css2-crashtests}"
REPO="web-platform-tests/wpt"
REF="${1:-master}"

# The directories that map onto Phase 6's scope: "box tree, BFCs, inline, floats,
# positioned" (§9). Deliberately not the whole of CSS2 --
#
#   generated-content  needs ::before/::after box generation
#   pagination         is print layout, which this project does not do
#   lists              needs list markers
#   tables             is its own layout algorithm, and not in this phase
#
# Each of those is a real subsystem rather than a detail, and including tests for
# subsystems this phase does not build would mean a threshold set by how many of
# them were excluded afterwards.
DIRS="normal-flow floats floats-clear box-display visuren positioning linebox"

command -v gh >/dev/null 2>&1 || {
    echo "FAIL gh is required to enumerate and fetch the subset" >&2
    exit 1
}

resolved="$(gh api "repos/$REPO/commits/$REF" --jq '.sha')"
echo "  WPT $REF resolves to $resolved" >&2

# One tree read for the whole of css/CSS2, rather than one API call per file.
css_sha="$(gh api "repos/$REPO/git/trees/$resolved" --jq '.tree[] | select(.path=="css") | .sha')"
css2_sha="$(gh api "repos/$REPO/git/trees/$css_sha" --jq '.tree[] | select(.path=="CSS2") | .sha')"

listing="$(mktemp)"
gh api "repos/$REPO/git/trees/$css2_sha?recursive=1" \
    --jq '.tree[] | select(.type=="blob") | [.path, .sha] | @tsv' > "$listing"

mkdir -p "$DEST"
mkdir -p "$CRASH_DEST"

# Only `.html`, and only pairs. A test whose reference is a `.xht` is not usable
# here (ADR 029: px-dom has no XML path), and a reference with no test is dead
# weight in the corpus.
wanted="$(mktemp)"
for dir in $DIRS; do
    awk -v d="$dir/" -F'\t' '$1 ~ "^"d && $1 ~ /\.html$/ { print }' "$listing" >> "$wanted"
done

fetched=0
pairs=0
crashtests=0
while IFS=$'\t' read -r path sha; do
    case "$path" in
        *-ref.html) continue ;;   # references are fetched with their test
    esac

    # Crashtests, which have no reference and are not graded against one: a
    # crashtest passes if the engine does not crash. They were invisible to this
    # script for the length of Phase 6 because it pairs on a `-ref.html` name and
    # they have none, and there are 28 of them in these seven directories.
    #
    # Worth more than their count suggests. They are the inputs that made Chrome
    # and Firefox crash, which is a better-chosen adversarial corpus than anything
    # hand-written, and they need no oracle, no threshold and no pairing.
    case "$path" in
        */crashtests/*|*-crash.html)
            mkdir -p "$CRASH_DEST/$(dirname "$path")"
            gh api "repos/$REPO/git/blobs/$sha" --jq '.content' \
                | base64 --decode > "$CRASH_DEST/$path"
            crashtests=$((crashtests + 1))
            continue
            ;;
    esac

    ref="${path%.html}-ref.html"
    ref_sha="$(awk -v r="$ref" -F'\t' '$1 == r { print $2 }' "$wanted")"
    [ -n "$ref_sha" ] || continue   # no reference: not a reftest we can run

    for entry in "$path:$sha" "$ref:$ref_sha"; do
        file="${entry%%:*}"
        blob="${entry##*:}"
        mkdir -p "$DEST/$(dirname "$file")"
        # The blob API rather than raw.githubusercontent, so the content is
        # addressed by the sha the tree listing gave: the file cannot change
        # between enumerating and fetching it.
        gh api "repos/$REPO/git/blobs/$blob" --jq '.content' \
            | base64 --decode > "$DEST/$file"
        fetched=$((fetched + 1))
    done
    pairs=$((pairs + 1))
done < "$wanted"

cat > "$CRASH_DEST/PROVENANCE" <<CRASHPROV
WPT CSS2 crashtests, vendored by ci/vendor-css2-reftests.sh.

repository  https://github.com/$REPO
commit      $resolved
directories $DIRS
selection   files under a crashtests/ directory, or named *-crash.html
files       $crashtests

A crashtest carries no reference and is not compared against one. It passes if
the engine does not crash on it -- and for this project that means it does not
panic, does not overflow its stack and does not fail to terminate, because
px-layout contains no unsafe code and has no other way to crash.

These were skipped for the whole of Phase 6 because this script pairs a test
with a \`-ref.html\` of the same name and a crashtest has none. They are the
inputs that made Chrome and Firefox crash, so as an adversarial corpus for
exactly the features these directories cover they are better chosen than
anything written by hand.
CRASHPROV

cat > "$DEST/PROVENANCE" <<PROV
WPT CSS2 reftest subset, vendored by ci/vendor-css2-reftests.sh.

repository  https://github.com/$REPO
commit      $resolved
directories $DIRS
selection   .html files only, and only those with a matching -ref.html
pairs       $pairs
files       $fetched

Why only .html: css/CSS2 holds 10,501 .xht files against 816 .html. px-dom has
no XML path, and whether html5ever's HTML parser produces the tree an XML parser
would was an empirical question about a parser this project did not write.

Answered at the end of Phase 6 by sampling thirty .xht files: it does not, and not
for the reason you would guess. There are no self-closing non-void elements in the
sample at all. What there is, in half of them, is a bare CDATA section immediately
inside <style> -- and an HTML parser hands that to the CSS parser as stylesheet
text, which discards the entire stylesheet rather than one rule. A test and a
reference with no styles are both the user-agent skeleton, and two skeletons agree,
so these files would raise the conformance number without measuring anything.
ADR 029 has the measurement.

This paragraph lives in ci/vendor-css2-reftests.sh, because PROVENANCE is generated
and the first version of the note was written into the file and lost the next time
the script ran.

Why these directories: they are §9 Phase 6's scope -- box tree, BFCs, inline,
floats, positioned. generated-content, pagination, lists and tables each need a
subsystem this phase does not build.

Refresh by re-running the script. The diff is the record of what moved.
PROV

rm -f "$listing" "$wanted"
echo "  vendored $pairs pairs ($fetched files) into $DEST" >&2
echo "  vendored $crashtests crashtests into $CRASH_DEST" >&2
