#!/usr/bin/env bash
# Pushes one checked CI snapshot to pasta's personal fork, and only that (DEC-680; CI-FORK.md).
#
# Usage: contrib/node/push-fork-snapshot.sh <short sha>
#        contrib/node/push-fork-snapshot.sh --delete <short sha>
#        contrib/node/push-fork-snapshot.sh [--delete] --series <short sha>
#
# <short sha> is the 12-hex name check-fork-push.sh snapshot gave the passing snapshot: it left
# refs/fork-push/ci/<short> and a private receipt, fork-push/receipts/<short>.json, under
# DASH_EMBARGO_REGISTRY (default ~/private/embargo). This re-verifies the ref against the receipt
# (the same commit, parentless, the same tree), that the denylist is unchanged since the check and
# the receipt is at most MAX_AGE_HOURS (default 12) old, then pushes that commit by SHA as
# refs/heads/ci/<short>, never forced, so an existing branch is not overwritten. --delete removes
# the remote branch and the local ref after the CI run; the receipt stays as the record.
#
# --series <short> (DEC-1705) pushes the chain check-fork-push.sh series left at
# refs/fork-push/series/<short> (<short>: the source head's 12-hex prefix) as refs/heads/series/<short>,
# never under ci/ and never forced, after re-verifying it against fork-push/receipts/series-<short>.json:
# the ref is the receipt's last link, every link's commit has the recorded tree and the previous link
# as its only parent (the first none), the generated message and the one recorded identity, the chain
# has no other commit, and the denylist and age rules are those above. It also refuses a receipt
# without held_count_scope, the scope of the owner's approved held-reference count (DEC-1705 (3)):
# check-fork-push.sh series writes it only with --held-count-approved steps+messages, after the owner
# counted the held number by hand over every pushed step's own change,
# `git log -p --first-parent --diff-merges=first-parent <base>..<head>`, and over every message,
# `git log --format=%B <base>..<head>`
# (a net `git diff <base>...<head>` misses a term a middle commit adds and a later one removes). The
# held number itself is never written to the repository, the receipt or a log. --delete --series
# removes the remote branch, if it is still there, and the local ref after the series.yml run.
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

if [ "${1:-}" = --series ]; then
    shift
    [ $# -eq 1 ] && [[ "$1" =~ ^[0-9a-f]{12}$ ]] || die "usage: $0 [--delete] --series <12-hex short sha>"
    short="$1"
    ref="refs/fork-push/series/$short"
    if [ "$delete" -eq 1 ]; then
        existing="$(git ls-remote "$remote" "refs/heads/series/$short")" || die "could not read $remote"
        if [ -n "$existing" ]; then
            git push --quiet "$remote" --delete "refs/heads/series/$short"
        else
            echo "push-fork-snapshot: series/$short is already absent on $remote"
        fi
        git update-ref -d "$ref"
        echo "push-fork-snapshot: deleted series/$short on $remote and $ref"
        exit 0
    fi
    receipt="$registry/fork-push/receipts/series-$short.json"
    [ -f "$receipt" ] || die "no receipt $receipt (run check-fork-push.sh series first)"
    commit="$(git rev-parse -q --verify "$ref^{commit}")" || die "no $ref (the series did not pass, or was pushed and deleted)"
    python3 -I - "$receipt" "$commit" "$short" "$registry/fork-push/denylist.txt" "${MAX_AGE_HOURS:-12}" <<'PY' ||
import hashlib, json, re, subprocess, sys, time
receipt, commit, short, denylist, max_age = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], float(sys.argv[5])
git = lambda *a: subprocess.run(["git", *a], capture_output=True, text=True, check=True).stdout
problems = []
try:
    r = json.load(open(receipt))
    links = r["links"]
    n = len(links)
    if r.get("kind") != "series" or r["ref"] != f"refs/heads/series/{short}" or not n or r["head"][:12] != short \
            or not re.fullmatch(r"[0-9a-f]{40}", r["base"]) or not re.fullmatch(r"[0-9a-f]{40}", r["head"]) \
            or links[-1]["source"] != r["head"]:
        problems.append("the receipt is not this series' receipt")
    if r.get("held_count_scope") != f"git log -p --first-parent --diff-merges=first-parent {r['base']}..{r['head']}; git log --format=%B {r['base']}..{r['head']}":
        problems.append("the receipt does not record the approved held-reference count over every step's change "
                        "and every message (held_count_scope; check-fork-push.sh series --held-count-approved)")
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", r["label"]):
        problems.append("the receipt's label is not a plain name")
    if r["tip"] != commit or links[-1]["commit"] != commit:
        problems.append("the ref is not the checked chain's last commit")
    if git("rev-list", "--count", commit).strip() != str(n):
        problems.append(f"the chain does not hold exactly the {n} checked commits")
    prev = None
    for k, link in enumerate(links, 1):
        c = link["commit"]
        raw = git("cat-file", "commit", c)
        headers, _, message = raw.partition("\n\n")
        fields = [l.split(" ", 1) for l in headers.splitlines()]
        who = {f: v.rsplit(" ", 2)[0] for f, v in fields if f in ("author", "committer")}
        expect = f"series {r['label']} {k}/{n} {link['source'][:12]}"
        if link["k"] != k or not re.fullmatch(r"[0-9a-f]{40}", link["source"]):
            problems.append(f"link {k}: not in chain order")
        if git("rev-parse", f"{c}^{{tree}}").strip() != link["tree"]:
            problems.append(f"link {k}: the commit's tree is not the checked tree")
        if git("rev-list", "--parents", "-n", "1", c).split()[1:] != ([prev] if prev else []) or link["parent"] != prev:
            problems.append(f"link {k}: the parent is not the previous link" if prev else "link 1: the first commit has a parent")
        if message != expect + "\n" or link["message"] != expect:
            problems.append(f"link {k}: the message is not the generated one")
        if [f for f, _ in fields] != ["tree"] + (["parent"] if prev else []) + ["author", "committer"] \
                or who.get("author") != r["identity"] or who.get("committer") != r["identity"]:
            problems.append(f"link {k}: the headers or identity are not the checked ones")
        checks = link.get("checks")
        # DEC-1746: --only may narrow the set, never below 1 (denylist) and 7 (secrets).
        if not (isinstance(checks, list) and all(type(c) is int for c in checks) and checks == sorted(set(checks))
                and set(checks) <= set(range(1, 10)) and {1, 7} <= set(checks)):
            problems.append(f"link {k}: checks 1 and 7 did not both run on its tree")
        elif checks != links[0].get("checks"):
            problems.append(f"link {k}: ran other checks than link 1")
        prev = c
    if hashlib.sha256(open(denylist, "rb").read()).hexdigest() != r["denylist_sha256"]:
        problems.append("the denylist changed since the check (run it again)")
    if time.time() - r["time"] > max_age * 3600:
        problems.append(f"the check is older than {max_age:g} hours (run it again)")
except (KeyError, TypeError, ValueError, IndexError, subprocess.CalledProcessError) as e:
    problems.append(f"the receipt or a link cannot be read ({type(e).__name__})")
if problems:
    print("push-fork-snapshot: the series does not match its receipt: " + "; ".join(problems), file=sys.stderr)
    sys.exit(1)
PY
        exit 2
    existing="$(git ls-remote "$remote" "refs/heads/series/$short")" || die "could not read $remote"
    [ -z "$existing" ] || die "series/$short already exists on $remote (delete it first; this never overwrites)"
    git push --quiet "$remote" "$commit:refs/heads/series/$short"
    echo "push-fork-snapshot: pushed $commit as series/$short to $remote"
    exit 0
fi
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
