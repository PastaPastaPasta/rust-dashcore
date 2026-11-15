#!/usr/bin/env bash
# Builds and runs CS2's differential harness against Dash Core's oracle (README.md).
#
#   contrib/node/cs2-diff/run.sh [--update-lock] <cs2-diff command> [args..]
#   contrib/node/cs2-diff/run.sh --update-lock      (only rewrite Cargo.lock and build)
#
# Relative paths in the arguments are taken from the repository root. The harness is a
# workspace of its own (it links the oracle archives), so it carries its own base-sdk [patch]
# table. This checks first that the table patches every package the root's does, to the same
# fork revision, so no base-sdk package can resolve to anything else. The archives come from
# contrib/node/build-oracle.sh.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"

fail() { echo "run.sh: $*" >&2; exit 2; }

python3 -I - "$root/Cargo.toml" "$here/Cargo.toml" <<'PY' || fail "the base-sdk [patch] does not match the root's"
import sys, tomllib
key = "https://github.com/dashpay/base-sdk"
root, ours = (tomllib.load(open(p, "rb")).get("patch", {}).get(key) or {} for p in sys.argv[1:])
pinned = {(spec.get("git"), spec.get("rev")) for spec in root.values()}
sys.exit(0 if root and len(pinned) == 1 and all(ours.get(name) == spec for name, spec in root.items())
         and all((spec.get("git"), spec.get("rev")) in pinned for spec in ours.values()) else 1)
PY

# --locked: a run uses exactly the committed Cargo.lock. When the path dependencies' own graphs
# change (CS0 adds dash-pow and dash-sha256 to dash-consensus), `run.sh --update-lock` rewrites
# the lock, which is then committed with that change.
locked=--locked
if [ "${1:-}" = --update-lock ]; then
    locked=
    shift
fi
cargo build --release --quiet $locked --manifest-path "$here/Cargo.toml" ||
    fail "the build failed${locked:+; if Cargo.lock is out of date, rerun with --update-lock and commit it}"
cd "$root"
[ "$#" = 0 ] && exit 0
exec "$here/target/release/cs2-diff" "$@"
