#!/usr/bin/env bash
# L3's tier 1 (CH3a a10, plan §3.4): the l3_dashd test against a real dashd v24,
# failing closed. The run fails, with its reason, on
#   - an unset DASHVERSION, or one that is not 24.0.0[-rc.N], or one that differs
#     from the v24 DASHVERSION of node-gate.yml or build-and-test.yml;
#   - no dashd: L3_DASHD if set, else exactly
#     ~/.rust-dashcore-test/dashcore-$DASHVERSION/bin/dashd, which setup-dashd.py
#     fetches when it is absent (no glob, never setup-dashd.py's default version);
#   - no L3_DASH_TREE (the Dash Core tree whose tag v$DASHVERSION pins the source);
#   - the test failing, or writing no report;
#   - a report with no cases, a manifest row that did not run, a leg not done, or legs
#     other than exactly par1 and par4 (this script runs set tier1 only);
#   - a leg with an up post holding more than one candidate (OD-L3-19's tripwire:
#     it voids the candidate-order equivalence and goes to L3);
#   - no inventory fixture for v$DASHVERSION, or the census `check --partial` over the
#     report failing (plan §6.5; its per-row status is kept as census.txt).
#
# Usage: DASHVERSION=24.0.0-rc.4 L3_DASH_TREE=<dash> contrib/node/l3-tier1.sh [report dir]
# The report dir (default: a new one under $TMPDIR) keeps the test's log, report.json and
# census.txt.
set -euo pipefail

cd "$(dirname "$0")/../.."

die() {
    echo "l3-tier1: $*" >&2
    exit 1
}

[ -n "${DASHVERSION:-}" ] || die "DASHVERSION is unset (the L3 lane never runs on setup-dashd.py's default)"
[[ $DASHVERSION =~ ^24\.0\.0(-rc\.[0-9]+)?$ ]] || die "DASHVERSION=$DASHVERSION is not 24.0.0[-rc.N]"

# The one v24 DASHVERSION a workflow sets.
workflow_v24() {
    local found
    found="$(grep -oE 'DASHVERSION: *"?24\.[^"[:space:]]*' "$1" | sed -E 's/^DASHVERSION: *"?//' | sort -u)"
    [ -n "$found" ] || die "$1 sets no v24 DASHVERSION"
    [ "$(wc -l <<<"$found")" -eq 1 ] || die "$1 sets more than one v24 DASHVERSION: $(tr '\n' ' ' <<<"$found")"
    echo "$found"
}
for wf in .github/workflows/node-gate.yml .github/workflows/build-and-test.yml; do
    v="$(workflow_v24 "$wf")"
    [ "$v" = "$DASHVERSION" ] || die "DASHVERSION=$DASHVERSION but $wf runs $v"
done

if [ -n "${L3_DASHD:-}" ]; then
    dashd="$L3_DASHD"
else
    dashd="$HOME/.rust-dashcore-test/dashcore-$DASHVERSION/bin/dashd"
    if [ ! -e "$dashd" ]; then
        DASHVERSION="$DASHVERSION" python3 contrib/setup-dashd.py >/dev/null
    fi
fi
[ -x "$dashd" ] || die "no dashd at $dashd"
[ -n "${L3_DASH_TREE:-}" ] || die "L3_DASH_TREE is unset (the Dash Core tree with tag v$DASHVERSION)"
fixture="contrib/node/l3-census/inventory-v$DASHVERSION.json"
[ -f "$fixture" ] || die "no inventory fixture $fixture"

out="${1:-$(mktemp -d "${TMPDIR:-/work/tmp}/l3-tier1.XXXXXX")}"
mkdir -p "$out"
report="$out/report.json"
[ ! -e "$report" ] || die "$report already exists: give a fresh report dir"

echo "l3-tier1: dashd $dashd (DASHVERSION=$DASHVERSION), report in $out"
status=0
L3_DASHD="$dashd" L3_DASH_TREE="$L3_DASH_TREE" L3_SET=tier1 L3_REPORT="$report" DASHVERSION="$DASHVERSION" \
    cargo test -p node-l1 --features l3 --test l3_dashd -- --nocapture >"$out/test.log" 2>&1 || status=$?
[ "$status" -eq 0 ] || die "l3_dashd failed ($status); see $out/test.log"
[ -s "$report" ] || die "l3_dashd wrote no report at $report"

python3 -I - "$report" <<'EOF'
import json
import sys

path = sys.argv[1]
with open(path) as f:
    report = json.load(f)
problems = []
cases = report.get("cases")
if not isinstance(cases, int) or cases <= 0:
    problems.append(f"cases = {cases!r}")
manifest = report.get("manifest")
if not isinstance(manifest, list) or not manifest:
    problems.append("no manifest rows")
else:
    for row in manifest:
        if row.get("status") != "ran":
            problems.append(f"manifest row {row.get('trial_id')!r}: {row.get('status')!r}")
legs = report.get("legs")
if not isinstance(legs, list) or not legs:
    problems.append("no legs")
else:
    names = sorted(str(leg.get("name")) for leg in legs)
    if names != ["par1", "par4"]:
        problems.append(f"legs {names}, but set tier1 has exactly ['par1', 'par4']")
    for leg in legs:
        name = leg.get("name")
        if leg.get("status") != "done":
            problems.append(f"leg {name!r}: {leg.get('status')!r}")
        multi = leg.get("multi_candidate_up_posts")
        if not isinstance(multi, int):
            problems.append(f"leg {name!r}: no multi_candidate_up_posts count")
        elif multi > 0:
            problems.append(f"leg {name!r}: {multi} up posts with more than one candidate (OD-L3-19)")
if problems:
    print(f"l3-tier1: {path} fails closed:", *problems, sep="\n  ", file=sys.stderr)
    sys.exit(1)
counts = ", ".join(f"{leg['name']} {leg['multi_candidate_up_posts']}" for leg in legs)
print(f"l3-tier1: {cases} cases, {len(manifest)} trials ran, legs done; multi-candidate up posts: {counts}")
EOF

python3 -I contrib/node/l3-census.py check --tree "$L3_DASH_TREE" --fixture "$fixture" --partial "$report" \
    >"$out/census.txt" || die "the census --partial fails; see $out/census.txt and the lines above"
tail -n 1 "$out/census.txt"
