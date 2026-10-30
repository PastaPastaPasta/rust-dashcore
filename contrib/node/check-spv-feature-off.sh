#!/usr/bin/env bash
# dash-spv and dash-spv-ffi with their optional features on and off (SB3).
#
# The workspace's own tests never see the production feature-off build: the
# `test-utils` self dev-dependency turns `log-file` (and tokio's `process`) on
# in every test of dash-spv and dash-spv-ffi. So contrib/node/spv-feature-off/
# is a consumer crate outside the workspace, with its own Cargo.lock and
# target/, that takes both libraries by path with no default features and runs
# its tests for each of the four variants: none, log-file, env-filter, both.
# It asserts that
#   - `log-file` creates the log file, and without it init_logging fails with
#     LoggingError::FileLoggingUnavailable (the C function with RuntimeError,
#     code 8, and a last error naming the feature) and creates nothing;
#   - RUST_LOG=debug is honoured only with `env-filter`;
#   - an FFI client is constructed and destroyed in every variant (the tokio
#     features the libraries keep are enough for the runtime).
#
# Usage: contrib/node/check-spv-feature-off.sh
# The consumer's target/ is contrib/node/spv-feature-off/target, or
# $SPV_FEATURE_OFF_TARGET_DIR when set (git ignores target/ either way).
set -euo pipefail

cd "$(dirname "$0")/spv-feature-off"
[ -z "${SPV_FEATURE_OFF_TARGET_DIR:-}" ] || export CARGO_TARGET_DIR="$SPV_FEATURE_OFF_TARGET_DIR"

failed=()
for features in "" log-file env-filter log-file,env-filter; do
    echo "== spv-feature-off: features [${features:-none}]"
    if ! cargo test --quiet --no-default-features ${features:+--features "$features"}; then
        failed+=("${features:-none}")
    fi
done
if [ "${#failed[@]}" != 0 ]; then
    echo "spv-feature-off failed for: ${failed[*]}" >&2
    exit 1
fi
echo "spv-feature-off: 4 variants ok"
