#!/usr/bin/env bash
# Re-pins the workspace to a revision of the local base SDK fork (A1 §1.6).
#
# Usage: contrib/node/bump-base-sdk.sh [--no-gates] [REF]
#
#   REF         commit or ref in the fork repository; default `node/base-sdk`.
#               It must be contained in `node/base-sdk`.
#   --no-gates  skip the gates; only re-pin, update Cargo.lock, check the pin
#               and tag it.
#
# Steps:
#   1. resolve REF in the fork named by the root manifest's [patch] URL;
#   2. base SDK gate: `just test lint` on a detached worktree of the revision,
#      never the fork's working tree (override with BASE_SDK_CHECKS="just test");
#   3. set the [patch] rev to it and the upstream declaration rev to its
#      merge base with `origin/develop` (the newest upstream commit it holds),
#      in the root manifest and in every other workspace with a base SDK
#      [patch] (node-fuzz, cs2-diff);
#   4. update Cargo.lock and those workspaces' lockfiles, and run
#      check-base-sdk-pins.sh;
#   5. rust-dashcore gates: cargo fmt --check, clippy -D warnings,
#      cargo deny check, check-layers.py, check-pure.sh and check-shared.sh
#      (which fail until pure-clippy.toml's audit stamp names the new
#      revision: re-audit the blst bans, update the stamp, re-run; DEC-79),
#      cargo test --workspace, and `cargo test -p dash-conformance` (BS2's
#      testnet and regtest fixture sets) when that crate is a member. A full
#      BS2 run over the testnet oracle is not part of this script; see
#      docs/node-program/BASE-SDK-FORK.md;
#   6. tag the revision `node-base-sdk-YYYY-MM-DD` in the fork, also with
#      --no-gates, so a committed pin stays fetchable after `node/base-sdk`
#      is rebased.
# The manifest and lockfile changes are left uncommitted for review.
set -euo pipefail

gates=1
ref=node/base-sdk
for arg in "$@"; do
    case "$arg" in
        --no-gates) gates=0 ;;
        -h|--help) sed -n '2,/^[^#]/s/^# \{0,1\}//p' "$0"; exit 0 ;;
        -*) echo "unknown option: $arg" >&2; exit 2 ;;
        *) ref="$arg" ;;
    esac
done

cd "$(dirname "$0")/../.."
pins=contrib/node/base_sdk_pins.py
read -r url old_rev old_upstream < <(python3 "$pins" show)
repo="${url#file://}"

rev="$(git -C "$repo" rev-parse --verify "$ref^{commit}")"
git -C "$repo" merge-base --is-ancestor "$rev" node/base-sdk \
    || { echo "error: $rev is not on node/base-sdk in $repo" >&2; exit 1; }
upstream="$(git -C "$repo" merge-base "$rev" origin/develop)" \
    || { echo "error: $rev shares no history with origin/develop in $repo" >&2; exit 1; }

if [ "$gates" = 1 ]; then
    wt="$(mktemp -d "${TMPDIR:-/tmp}/base-sdk-$rev.XXXXXX")"
    trap 'git -C "$repo" worktree remove --force "$wt" > /dev/null 2>&1; rm -rf "$wt"' EXIT
    git -C "$repo" worktree add --detach "$wt" "$rev" > /dev/null
    base_checks="${BASE_SDK_CHECKS:-just test lint}"
    echo "== base SDK: $base_checks"
    (cd "$wt" && $base_checks)
fi

echo "fork:     $old_rev -> $rev"
echo "upstream: $old_upstream -> $upstream"
python3 "$pins" set "$rev" "$upstream"
cargo metadata --format-version 1 > /dev/null
files="Cargo.toml and Cargo.lock"
nested="$(python3 "$pins" nested)"
for dir in $nested; do
    cargo metadata --manifest-path "$dir/Cargo.toml" --format-version 1 > /dev/null
    files="$files, $dir/Cargo.{toml,lock}"
done
contrib/node/check-base-sdk-pins.sh

if [ "$gates" = 1 ]; then
    echo "== rust-dashcore gates"
    cargo fmt --all --check
    cargo clippy --workspace --all-features --all-targets -- -D warnings
    cargo deny check
    python3 contrib/node/check-layers.py
    contrib/node/check-pure.sh
    contrib/node/check-shared.sh
    cargo test --workspace
    if cargo metadata --no-deps --format-version 1 | grep -q '"name":"dash-conformance"'; then
        echo "== BS2 conformance sets"
        cargo test -p dash-conformance
    fi
fi

# Keep the pinned revision reachable after `node/base-sdk` is rebased.
tag="$(git -C "$repo" tag --points-at "$rev" -l 'node-base-sdk-*' | head -n 1)"
if [ -z "$tag" ]; then
    base="node-base-sdk-$(date +%F)"
    tag="$base"; n=1
    while git -C "$repo" rev-parse -q --verify "refs/tags/$tag" > /dev/null; do
        n=$((n + 1)); tag="$base.$n"
    done
    git -C "$repo" tag -m "rust-dashcore base SDK pin" "$tag" "$rev"
fi
echo "pinned $rev, tagged $tag in $repo"
echo "next: commit $files as \"chore: re-pin base SDK fork to ${rev:0:7}\""
