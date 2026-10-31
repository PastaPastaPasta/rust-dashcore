#!/usr/bin/env bash
# The pure-crate bans (SB0 L4; A1 §1.5, §2.1, X-1). Every crate layers.toml
# puts in P or Pn, or marks `pure = true` (a sans-io S crate held to the
# stricter bans), is an ordinary `std` library with no I/O, threads or clocks
# (globals are left to review):
#   - cargo-deny with contrib/node/pure-deny.toml, rooted at that crate only,
#     with all features and without dev-dependencies, rejects async runtimes,
#     sockets, storage engines, clocks and thread pools anywhere in its graph,
#     except blst's pool, reached through blst and dash-pkc alone;
#   - clippy with contrib/node/pure-clippy.toml rejects the std entry points to
#     the file system, sockets, processes, threads and the clock, and the blst
#     and dash-pkc entry points that run on blst's pool, in its library and in
#     any workspace crate it depends on, with all features and with no default
#     features, on the host and for aarch64-linux-android (the Apple targets
#     need the Mac: BL-74). The lints are forbidden, so `#[allow]` cannot
#     lift them. The blst bans hold for the blst release and base SDK
#     revision they were audited against, which pure-clippy.toml's stamp
#     names and this checks;
#   - link-probe.py, as for the shared crates: reviewed [[native]] records
#     for its closure's native packages, sample links, and the advisory
#     source scan (DEC-188); the allocator proof is the canary link of every
#     registered product (check-products.sh, DEC-181), and consumers outside
#     that registry are out of scope.
#
# Usage: contrib/node/check-pure.sh [--stamp]
#   --stamp  check only that pure-clippy.toml's stamp names the base SDK pin
#            (no build).
set -euo pipefail

cd "$(dirname "$0")/../.."
. contrib/node/clippy-lib.sh
read_stamp
if [ "${1:-}" = --stamp ]; then
    [ -z "$stale" ] && echo "pure-clippy.toml's audit stamp names the base SDK pin"
    exit "${stale:-0}"
fi
list="$(layer_crates P Pn pure)"

if [ -z "$list" ]; then
    echo "pure crates: layers.toml has no P, Pn or pure workspace crate, nothing to check"
    exit 0
fi

lint_env

failed=()
while read -r crate manifest; do
    start_crate "$crate" "$manifest" || continue
    if ! conf="$(write_conf node-pure-clippy "$crate" "$manifest" contrib/node/pure-clippy.toml)" ||
        ! cargo deny --manifest-path "$manifest" --config contrib/node/pure-deny.toml \
            --all-features --exclude-dev --locked check bans || ! blst_audited "$crate" ||
        ! lint "$crate" "$conf" ||
        ! python3 contrib/node/link-probe.py "$crate" "$manifest" "${LINT_TARGETS[@]}"; then
        failed+=("$crate")
    fi
done <<< "$list"

finish "pure crates" "$list"
