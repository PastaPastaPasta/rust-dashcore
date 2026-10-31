#!/usr/bin/env bash
# Pushes one checked CI snapshot to pasta's personal fork, and only that (DEC-680; CI-FORK.md).
#
# Usage: contrib/node/push-fork-snapshot.sh <short sha>
#        contrib/node/push-fork-snapshot.sh --delete <short sha>
#
# <short sha> is the 12-hex name check-fork-push.sh snapshot gave the passing snapshot: it left
# refs/fork-push/ci/<short> and a private receipt, fork-push/receipts/<short>.json, under
# DASH_EMBARGO_REGISTRY (default ~/private/embargo). This re-verifies the ref against the receipt
# (the same commit, parentless, the same tree), that the denylist is unchanged since the check and
# the receipt is at most MAX_AGE_HOURS (default 12) old, then pushes that commit by SHA as
# refs/heads/ci/<short>, never forced, so an existing branch is not overwritten. --delete removes
# the remote branch and the local ref after the CI run; the receipt stays as the record.
#
# The remote is https://github.com/PastaPastaPasta/rust-dashcore. --allow-overrides, first, lets
# --remote <url> replace it (the selftest pushes into a local bare repository).
set -euo pipefail

registry="${DASH_EMBARGO_REGISTRY:-$HOME/private/embargo}"
remote=https://github.com/PastaPastaPasta/rust-dashcore
die() { echo "push-fork-snapshot: $*" >&2; exit 2; }

if [ "${1:-}" = --allow-overrides ]; then
    shift
    if [ "${1:-}" = --remote ]; then remote="${2:?}"; shift 2; fi
fi
delete=0
if [ "${1:-}" = --delete ]; then delete=1; shift; fi
[ $# -eq 1 ] && [[ "$1" =~ ^[0-9a-f]{12}$ ]] || die "usage: $0 [--delete] <12-hex short sha>"
short="$1"
ref="refs/fork-push/ci/$short"

if [ "$delete" -eq 1 ]; then
    git push --quiet "$remote" --delete "refs/heads/ci/$short"
    git update-ref -d "$ref"
    echo "push-fork-snapshot: deleted ci/$short on $remote and $ref"
    exit 0
fi

receipt="$registry/fork-push/receipts/$short.json"
[ -f "$receipt" ] || die "no receipt $receipt (run check-fork-push.sh snapshot first)"
commit="$(git rev-parse -q --verify "$ref^{commit}")" || die "no $ref (the snapshot did not pass, or was pushed and deleted)"
python3 -I - "$receipt" "$commit" "$registry/fork-push/denylist.txt" "${MAX_AGE_HOURS:-12}" <<'PY' ||
import hashlib, json, subprocess, sys, time
receipt, commit, denylist, max_age = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
r = json.load(open(receipt))
git = lambda *a: subprocess.run(["git", *a], capture_output=True, text=True, check=True).stdout.strip()
problems = []
if r["commit"] != commit:
    problems.append("the ref is not the checked commit")
if git("rev-parse", f"{commit}^{{tree}}") != r["tree"]:
    problems.append("the commit's tree is not the checked tree")
if git("rev-list", "--parents", "-n", "1", commit).split()[1:]:
    problems.append("the commit has a parent")
if hashlib.sha256(open(denylist, "rb").read()).hexdigest() != r["denylist_sha256"]:
    problems.append("the denylist changed since the check (run it again)")
if time.time() - r["time"] > max_age * 3600:
    problems.append(f"the check is older than {max_age:g} hours (run it again)")
if problems:
    print("push-fork-snapshot: the snapshot does not match its receipt: " + "; ".join(problems), file=sys.stderr)
    sys.exit(1)
PY
    exit 2
existing="$(git ls-remote "$remote" "refs/heads/ci/$short")" || die "could not read $remote"
[ -z "$existing" ] || die "ci/$short already exists on $remote (delete it first; this never overwrites)"
git push --quiet "$remote" "$commit:refs/heads/ci/$short"
echo "push-fork-snapshot: pushed $commit as ci/$short to $remote"
