#!/bin/bash
# Usage: rdc-ci-verdict.sh <label> <full 40-hex source SHA> [--run <run-id>] [--override DEC-<n>]
# DEC-705 gate: green GitHub tier 1 (node-gate.yml on PastaPastaPasta/rust-dashcore) on the exact source
# commit is the required check for READY and train candidates. Finds the newest node-gate run of the
# snapshot of <source SHA> that was not cancelled (CI-S0's export pushes it as ci/<12-hex> with the
# message "ci snapshot of node/integration <full sha>", which is the run title's tail; a run dispatched
# on the fork's master with `snapshot` and `source` carries the same title), or checks the given run,
# which must be one, and prints one line:
#   RDC-CI GREEN <url> <label> ci/<short> <sha> (<n> jobs)
# Only push runs on ci/<short> and dispatches on master count: a dispatch from another branch runs that
# branch's copy of the workflow, and is a lookup error.
# Exit 0 only when that run completed with success and every job of its final attempt succeeded, with
# Deps, Static, Summary and conformance among them (a deps_only run never passes), in at most two
# attempts (one `gh run rerun --failed`; the line then names the jobs attempt 1 failed), and only when
# every other run of the same source has completed and none ended red (cancelled ones are ignored,
# but named): a red source is not re-dispatched or re-pushed into green without a DEC, which
# `--override DEC-<n>` names in the line.
# Exit 1 when it failed, was cancelled, is still pending, or no run exists (then it says how to push one).
# Exit 2 on a usage or lookup error.
set -uo pipefail
repo=PastaPastaPasta/rust-dashcore
workflow=node-gate.yml

usage="usage: $0 <label> <full 40-hex source SHA> [--run <run-id>] [--override DEC-<n>]
  Pass the full SHA of the commit you gate, resolved in your own worktree, e.g. \"\$(git rev-parse HEAD)\";
  refs, HEAD and short SHAs are refused. --override (manager only) names the DEC that allows a green
  after another red run of the same source."
[ $# -ge 2 ] || { echo "$usage" >&2; exit 2; }
label=$1; sha=$2; run="" override=""
shift 2
while [ $# -gt 0 ]; do
  case $1 in
    --run) [ $# -ge 2 ] && [ -z "$run" ] || { echo "$usage" >&2; exit 2; }; run=$2; shift 2 ;;
    --override) [ $# -ge 2 ] && [ -z "$override" ] || { echo "$usage" >&2; exit 2; }; override=$2; shift 2 ;;
    *) echo "$usage" >&2; exit 2 ;;
  esac
done
# The label goes into the one verdict line, so it cannot carry a line of its own.
[[ "$label" =~ ^[A-Za-z0-9._/-]+$ ]] || { echo "RDC-CI ERROR: label '$label' is not [A-Za-z0-9._/-]+" >&2; exit 2; }
# A ref or short SHA would have to be resolved in some repository, and this script cannot know the caller's.
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || { echo "RDC-CI ERROR: '$sha' is not a full 40-hex SHA" >&2; echo "$usage" >&2; exit 2; }
[ -z "$run" ] || [[ "$run" =~ ^[0-9]+$ ]] || { echo "RDC-CI ERROR: run id '$run' is not a number" >&2; exit 2; }
[ -z "$override" ] || [[ "$override" =~ ^DEC-[0-9]+[a-z]?$ ]] || { echo "RDC-CI ERROR: override '$override' is not DEC-<n>" >&2; exit 2; }
short=${sha:0:12}
title="node-gate ci/$short ci snapshot of node/integration $sha"

# Every run of this source, newest first: created, id, status, conclusion of its final attempt, url,
# final attempt.
runs=$(gh api --paginate "repos/$repo/actions/workflows/$workflow/runs?per_page=100" \
  --jq ".workflow_runs[] | select(.display_title == \"$title\") | [.created_at, .id, .status, (.conclusion // \"none\"), .html_url, .run_attempt] | @tsv" \
  | sort -r) || { echo "RDC-CI ERROR: run lookup failed" >&2; exit 2; }
if [ -z "$run" ]; then
  run=$(awk -F'\t' '$4 != "cancelled" { print $2; exit }' <<<"$runs")
  if [ -z "$run" ]; then
    echo "RDC-CI MISSING: no node-gate run for $label $sha${runs:+ (only cancelled ones; pass --run to check one)}"
    echo "  push: contrib/node/check-fork-push.sh snapshot $sha && contrib/node/push-fork-snapshot.sh $short"
    exit 1
  fi
fi

info=$(gh api "repos/$repo/actions/runs/$run" \
  --jq '[.display_title, .status, (.conclusion // "none"), .html_url, .run_attempt, .event, .head_branch, .path] | @tsv') \
  || { echo "RDC-CI ERROR: cannot read run $run" >&2; exit 2; }
IFS=$'\t' read -r got status conclusion url attempt event branch path <<<"$info"
[ "$got" = "$title" ] || { echo "RDC-CI ERROR: run $run is \"$got\", not the node-gate run of $sha" >&2; exit 2; }
case "$path $event:$branch" in
  ".github/workflows/$workflow push:ci/$short" | ".github/workflows/$workflow workflow_dispatch:master") ;;
  *) echo "RDC-CI ERROR: run $run is $event on $branch ($path), not a push of ci/$short or a dispatch on master" >&2; exit 2 ;;
esac
grep -q "^[^"$'\t'"]*"$'\t'"$run"$'\t' <<<"$runs" || { echo "RDC-CI ERROR: run $run is not in the $workflow runs of $sha" >&2; exit 2; }

if [ "$status" != completed ]; then
  echo "RDC-CI PENDING $url $label ci/$short $sha (status $status)"
  exit 1
fi
# A green next to a run still in progress could turn red after the merge.
busy=$(awk -F'\t' -v r="$run" '$2 != r && $3 != "completed" { print $5; exit }' <<<"$runs")
if [ -n "$busy" ]; then
  echo "RDC-CI PENDING $url $label ci/$short $sha (another run of this source is not done: $busy)"
  exit 1
fi

list_jobs() {
  gh api --paginate "repos/$repo/actions/runs/$1/attempts/$2/jobs?per_page=100" \
    --jq '.jobs[] | [.name, (.conclusion // "none")] | @tsv'
}
# The final attempt lists every job, the ones a `gh run rerun --failed` carried over included.
jobs=$(list_jobs "$run" "$attempt") || { echo "RDC-CI ERROR: cannot list jobs" >&2; exit 2; }
reran=""
if [ "$attempt" -gt 1 ]; then
  first=$(list_jobs "$run" 1) || { echo "RDC-CI ERROR: cannot list jobs" >&2; exit 2; }
  reran=$(awk -F'\t' '$2 != "success" { printf "%s=%s; ", $1, $2 }' <<<"$first")
fi
n=$(grep -c . <<<"$jobs")
bad=$(awk -F'\t' '$2 != "success" { printf "%s=%s; ", $1, $2 }' <<<"$jobs")
missing=
for j in Deps Static Summary conformance; do
  grep -q "^$j"$'\t' <<<"$jobs" || missing+="$j "
done
# The newest other completed run of this source that ended neither green nor cancelled (a later one
# counts too, so `--run` cannot pick an older green past a newer red).
other_red=$(awk -F'\t' -v r="$run" '$2 != r && $4 != "success" && $4 != "cancelled" { print $5 " (" $4 ")"; exit }' <<<"$runs")
# Cancelled runs do not count, but one cancelled after a job failed is named in the line.
cancelled_failed=""
while IFS=$'\t' read -r _ id _ c u a; do
  [ "$id" != "$run" ] && [ "$c" = cancelled ] || continue
  cjobs=$(list_jobs "$id" "$a") || { echo "RDC-CI ERROR: cannot list jobs of run $id" >&2; exit 2; }
  awk -F'\t' '$2 == "failure" || $2 == "timed_out" { f = 1 } END { exit !f }' <<<"$cjobs" && cancelled_failed+=" $u"
done <<<"$runs"
note=${cancelled_failed:+, cancelled after a failed job:$cancelled_failed}${override:+, override $override}
# The gate allows one `gh run rerun --failed` per source commit (BL-20 flakes and runner failures only);
# the line names what attempt 1 failed so the gate reader can check that.
retry=
[ "$attempt" -gt 1 ] && retry=", attempt $attempt, rerun of: ${reran:-nothing failed}"
if [ "$attempt" -gt 2 ]; then
  echo "RDC-CI RED $url $label ci/$short $sha (attempt $attempt: more than one rerun${retry#*attempt $attempt})"
  exit 1
fi
if [ "$conclusion" = success ] && [ -z "$bad" ] && [ -z "$missing" ]; then
  if [ -n "$other_red" ] && [ -z "$override" ]; then
    echo "RDC-CI RED $url $label ci/$short $sha (green, but run $other_red of this source was red: a re-run of a red source needs --override DEC-<n>)"
    exit 1
  fi
  echo "RDC-CI GREEN $url $label ci/$short $sha ($n jobs$retry${other_red:+, after red $other_red}$note)"
  exit 0
fi
echo "RDC-CI RED $url $label ci/$short $sha (run $conclusion; ${bad}${missing:+missing: $missing}${retry}$note)"
exit 1
