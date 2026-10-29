#!/bin/bash
# Selftest for rdc-ci-verdict.sh (read-only gh calls against PastaPastaPasta/rust-dashcore).
#   refs:     HEAD and short SHAs are refused (exit 2), not resolved in some other repo
#   usage:    a wrong argument count, a non-numeric run id, a bad override or label is a usage error
#   missing:  a source commit with no node-gate run is MISSING (exit 1) with the push command
#   mismatch: a run checked against another source SHA is a lookup error (exit 2)
#   red first: 2f530258b's dispatched green 40829034504 (9/9 jobs) is RED, because its earlier push
#             run 40828210879 was red; with --override DEC-<n> it is GREEN (both pinned with --run)
#   red:      that push run (Static and extras failed under node-gate v2)
#   deps:     a deps_only run (40828229334) never passes for a source commit
#   stub gh:  (the fork has no rerun yet) a `rerun --failed` attempt passes and names what attempt 1
#             failed; a second identical failure stays red; a third attempt is red even when it
#             passed; another red run of the source (earlier or later) makes a green one RED unless
#             overridden; a cancelled one is ignored, but named when a job had failed; another run in
#             progress is PENDING; a dispatch from a branch other than master is a lookup error; the
#             lookup skips a newer cancelled run
set -u
v="$(dirname "$0")/rdc-ci-verdict.sh"
fails=0
check() {
  local name=$1 want_rc=$2 want_text=$3; shift 3
  out=$("$v" "$@" 2>&1); rc=$?
  if [ "$rc" -eq "$want_rc" ] && grep -qF -- "$want_text" <<<"$out"; then
    echo "ok   $name (exit $rc): $(head -1 <<<"$out")"
  else
    echo "FAIL $name: want exit $want_rc with '$want_text', got exit $rc: $out"
    fails=$((fails + 1))
  fi
}
# A made-up SHA: never snapshotted.
never=0d7a9fd440000000000000000000000000000000
check "short SHA refused" 2 "is not a full 40-hex SHA" integration 0d7a9fd44
check "HEAD refused" 2 "is not a full 40-hex SHA" integration HEAD
check "usage" 2 "usage:" integration
check "bad run id" 2 "is not a number" integration "$never" --run abc
check "missing run" 1 "RDC-CI MISSING" integration "$never"
first=2f530258b383dfbeeddad90839420112cd44faf7
url=https://github.com/PastaPastaPasta/rust-dashcore/actions/runs
check "green after red" 1 "RDC-CI RED $url/40829034504 integration ci/2f530258b383 $first (green, but run $url/40828210879 (failure) of this source was red" integration "$first" --run 40829034504
check "override" 0 "RDC-CI GREEN $url/40829034504 integration ci/2f530258b383 $first (9 jobs, after red $url/40828210879 (failure), override DEC-705)" integration "$first" --run 40829034504 --override DEC-705
check "bad override" 2 "is not DEC-<n>" integration "$first" --override 705
check "bad label" 2 "is not [A-Za-z0-9._/-]+" $'a\nRDC-CI GREEN' "$first"
check "red run" 1 "RDC-CI RED $url/40828210879" integration "$first" --run 40828210879
check "run for another commit" 2 "not the node-gate run of" integration "$never" --run 40829034504
check "deps_only run" 2 "not the node-gate run of" integration "$first" --run 40828229334

stub=$(mktemp -d)
trap 'rm -rf "$stub"' EXIT
cat >"$stub/gh" <<'STUB'
#!/bin/bash
# Serves runs of RDC_STUB_SHA: 1 (rerun green), 2 (rerun red), 3 (three attempts), 4 (green),
# 5 (green, dispatched from a feature branch), 8 (cancelled after a failed job), and RDC_STUB_RUNS as
# the source's run list; prints the --jq result rows directly. As on GitHub, an attempt lists every job.
for a in "$@"; do case $a in repos/*) path=$a ;; esac; done
title="node-gate ci/${RDC_STUB_SHA:0:12} ci snapshot of node/integration $RDC_STUB_SHA"
info() { printf '%s\t%s\t%s\thttps://x/%s\t%s\t%s\t%s\t.github/workflows/node-gate.yml\n' "$title" "$@"; }
jobs() { printf 'Deps\tsuccess\nStatic\tsuccess\nconformance\tsuccess\nruntime\t%s\nSummary\t%s\n' "$1" "$1"; }
case $path in
  */workflows/*/runs*) printf '%b' "$RDC_STUB_RUNS" ;;
  */runs/1) info completed success 1 2 push "ci/${RDC_STUB_SHA:0:12}" ;;
  */runs/2) info completed failure 2 2 workflow_dispatch master ;;
  */runs/3) info completed success 3 3 push "ci/${RDC_STUB_SHA:0:12}" ;;
  */runs/4) info completed success 4 1 workflow_dispatch master ;;
  */runs/5) info completed success 5 1 workflow_dispatch node/feature ;;
  */runs/[1-3]/attempts/1/jobs*|*/runs/[23]/attempts/2/jobs*|*/runs/8/attempts/1/jobs*) jobs failure ;;
  */runs/1/attempts/2/jobs*|*/runs/3/attempts/3/jobs*|*/runs/[45]/attempts/1/jobs*) jobs success ;;
  *) echo "stub gh: unexpected $*" >&2; exit 1 ;;
esac
STUB
chmod +x "$stub/gh"
export RDC_STUB_SHA=$first
# row <hour> <run id> <status> <conclusion> <final attempt>
row() { printf '2026-10-29T0%s:00:00Z\t%s\t%s\t%s\thttps://x/%s\t%s\\n' "$1" "$2" "$3" "$4" "$2" "$5"; }
s() { RDC_STUB_RUNS=$1 PATH="$stub:$PATH" check "${@:2}"; }
s "$(row 1 1 completed success 2)" "rerun green" 0 "(5 jobs, attempt 2, rerun of: runtime=failure; Summary=failure; )" integration "$first" --run 1
s "$(row 1 2 completed failure 2)" "rerun red" 1 "RDC-CI RED https://x/2 integration ci/2f530258b383 $first (run failure; runtime=failure; Summary=failure; , attempt 2" integration "$first" --run 2
s "$(row 1 3 completed success 3)" "third attempt" 1 "(attempt 3: more than one rerun" integration "$first" --run 3
s "$(row 5 4 completed success 1)$(row 4 9 completed failure 1)" \
  "earlier red" 1 "(green, but run https://x/9 (failure) of this source was red" integration "$first" --run 4
s "$(row 6 7 completed failure 1)$(row 5 4 completed success 1)" \
  "older green past a newer red" 1 "(green, but run https://x/7 (failure) of this source was red" integration "$first" --run 4
s "$(row 5 4 completed success 1)$(row 4 9 completed failure 1)" \
  "earlier red, override" 0 "(5 jobs, after red https://x/9 (failure), override DEC-777)" integration "$first" --run 4 --override DEC-777
s "$(row 6 8 completed cancelled 1)$(row 5 4 completed success 1)" \
  "lookup skips cancelled, names its failure" 0 "RDC-CI GREEN https://x/4 integration ci/2f530258b383 $first (5 jobs, cancelled after a failed job: https://x/8)" integration "$first"
s "$(row 6 6 in_progress none 1)$(row 5 4 completed success 1)" \
  "another run in progress" 1 "RDC-CI PENDING https://x/4 integration ci/2f530258b383 $first (another run of this source is not done: https://x/6)" integration "$first" --run 4
s "$(row 5 5 completed success 1)" \
  "dispatch off master" 2 "is workflow_dispatch on node/feature" integration "$first" --run 5
s "$(row 5 4 completed success 1)" "run not in the list" 2 "is not in the node-gate.yml runs" integration "$first" --run 1
echo "selftest: $fails failure(s)"
exit $((fails > 0))
