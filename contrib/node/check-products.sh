#!/usr/bin/env bash
# The product registry and the global-allocator proof (SB0 L5; DEC-181):
# every workspace cdylib, staticlib and bin has a layers.toml [[product]] row,
# and each row's product links a canary that sets its own global allocator,
# on the host and for aarch64-linux-android as its row says
# (contrib/node/check-products.py).
#
# Usage: contrib/node/check-products.sh [--registry | <package>...]
set -euo pipefail

cd "$(dirname "$0")/../.."
. contrib/node/clippy-lib.sh
lint_env
exec python3 contrib/node/check-products.py "$@"
