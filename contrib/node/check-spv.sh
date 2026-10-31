#!/usr/bin/env bash
# The SPV client's thread rules (SB0 §7, DEC-112 O5) for every crate
# layers.toml puts in V: clippy with contrib/node/spv-clippy.toml (rayon's
# global pool) plus pure-clippy.toml's blst and dash-pkc entries (DEC-79's
# pooled calls, under its audit stamp) rejects those calls in its library,
# with all features and with no default features, on the host and for
# aarch64-linux-android. Only the V crate is linted (`--no-deps`): its path
# dependencies keep their own layers' rules. The lints are denied, not forbidden: V's own
# clippy.toml bans std::sync's locks, which its store module allows (SB13
# D11). check-mobile.sh runs it; blst's `no-threads` in dash-spv-ffi and in
# no N package is check-layers.py's rule O5.
#
# Usage: contrib/node/check-spv.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
. contrib/node/clippy-lib.sh
list="$(layer_crates V)"

if [ -z "$list" ]; then
    echo "spv crates: layers.toml has no V workspace crate, nothing to check"
    exit 0
fi

read_stamp
lint_env

lint_spv() {  # lint_spv <crate> <conf dir>: on the host and each LINT_TARGET, with all features and with none
    local features target
    for target in "" "${LINT_TARGETS[@]}"; do
        for features in --all-features --no-default-features; do
            CLIPPY_CONF_DIR="$2" cargo clippy --locked --no-deps -p "$1" --lib "$features" ${target:+--target "$target"} -- \
                -D clippy::disallowed_methods -D clippy::disallowed_types || return 1
        done
    done
}

failed=()
while read -r crate manifest; do
    start_crate "$crate" "$manifest" || continue
    if ! conf="$(write_conf node-spv-clippy "$crate" "$manifest" contrib/node/spv-clippy.toml \
            contrib/node/pure-clippy.toml:blst::,dash_pkc::)" || ! blst_audited "$crate" ||
        ! lint_spv "$crate" "$conf"; then
        failed+=("$crate")
    fi
done <<< "$list"

finish "spv crates" "$list"
