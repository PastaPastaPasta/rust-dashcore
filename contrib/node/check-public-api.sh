#!/usr/bin/env bash
# SB4 A-13's public-API audit: dash-chain's public API (default features, blanket and auto-trait
# impls omitted) against the listing checked in at dash-chain/tests/public-api.txt. Any change to
# the API shows up as a diff of that file in review, where an API that moves an `Established`
# anchor would be visible. The tool and the nightly are pinned: rustdoc JSON, and so the listing,
# differ between nightlies.
#
# Usage: contrib/node/check-public-api.sh           exit 1 on any difference
#        contrib/node/check-public-api.sh --bless   rewrite the listing after an intended change
# Needs: cargo install cargo-public-api --locked --version 0.52.0
#        rustup toolchain install nightly-2026-07-15 --profile minimal
# Exit 2 when either is missing.
set -euo pipefail

cd "$(dirname "$0")/../.."
version=0.52.0
toolchain=nightly-2026-07-15
listing=dash-chain/tests/public-api.txt
if [ "$(cargo public-api --version 2>/dev/null)" != "cargo-public-api $version" ]; then
    echo "check-public-api.sh: needs cargo-public-api $version" >&2
    exit 2
fi
if ! rustup run "$toolchain" rustc --version > /dev/null 2>&1; then
    echo "check-public-api.sh: needs the $toolchain toolchain" >&2
    exit 2
fi
out="$(mktemp)"
trap 'rm -f "$out"' EXIT
cargo +"$toolchain" public-api -p dash-chain -ss --color never > "$out"
if [ "${1:-}" = --bless ]; then
    cp "$out" "$listing"
    exit 0
fi
if ! diff -u "$listing" "$out"; then
    echo "dash-chain's public API differs from $listing: review the diff (A-13), then --bless" >&2
    exit 1
fi
