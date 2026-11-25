#!/usr/bin/env bash
# BL-917: `cargo fmt --check` for every cargo workspace in the repository other than the root one,
# which the root `cargo fmt --check` never reaches. Each runs in its own directory, so it checks that
# workspace's members and not the root crates they depend on by path. A workspace root
# that is neither listed nor excluded fails the check, so a new one has to be added here.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

workspaces=(
    node-fuzz
    dashd-store-bakeoff
    contrib/mobile/combined-sdk-probe
    contrib/node/cs2-diff
    contrib/node/dde-probe/bench
    contrib/node/dde-probe/bs7
    contrib/node/dde-probe/bs7-bench
    contrib/node/dde-probe/bs8
    contrib/node/dde-probe/bs9
    contrib/node/dde-probe/oracle
    contrib/node/ev4-serve/evodump
    contrib/node/fork-push-fp
    contrib/node/lk1b-captures/isdump
    contrib/node/perfgate/criterion-shim/v0.5
    contrib/node/perfgate/criterion-shim/v0.8
    contrib/node/spv-feature-off
    contrib/node/t6-probe/new
    contrib/node/t6-probe/old
)
# check-test-features.py's metadata fixtures: empty lib.rs files, no code to format.
excluded_prefix=contrib/node/check-test-features/fixtures/

roots=$(git grep -l -e '^\[workspace\]' -e '^\[workspace\.' -- '*Cargo.toml')
rc=0
for manifest in $roots; do
    dir=$(dirname "$manifest")
    [ "$dir" = . ] && continue
    case "$dir/" in "$excluded_prefix"*) continue ;; esac
    listed=
    for w in "${workspaces[@]}"; do [ "$w" = "$dir" ] && listed=1; done
    [ -n "$listed" ] || { echo "check-fmt-workspaces: $dir is a workspace root not listed in $0"; rc=1; }
done

for w in "${workspaces[@]}"; do
    [ -e "$w/Cargo.toml" ] || { echo "check-fmt-workspaces: $w/Cargo.toml is missing"; rc=1; continue; }
    if (cd "$w" && cargo fmt --check); then echo "check-fmt-workspaces: $w ok"
    else echo "check-fmt-workspaces: $w needs cargo fmt"; rc=1; fi
done
exit $rc
