#!/usr/bin/env bash
# Copies H-M and H-T (README.md) into this directory, each checked against SHA256SUMS before it
# replaces a local copy.
#   fetch.sh [<dir with mainnet.bin and testnet.bin>]    (default /work/scratch/cs2/headers)
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
src=${1:-/work/scratch/cs2/headers}
trap 'rm -f "$here"/*.bin.part' EXIT
for net in mainnet testnet; do
    if [[ ! -f "$src/$net.bin" ]]; then
        echo "fetch.sh: $src/$net.bin is missing; rebuild it as README.md says" >&2
        exit 1
    fi
    want=$(awk -v f="$net.bin" '$2 == f { print $1 }' "$here/SHA256SUMS")
    cp --reflink=auto "$src/$net.bin" "$here/$net.bin.part"
    got=$(sha256sum "$here/$net.bin.part" | cut -d' ' -f1)
    if [[ "$got" != "$want" ]]; then
        echo "fetch.sh: $src/$net.bin has sha256 $got, want $want" >&2
        exit 1
    fi
    mv "$here/$net.bin.part" "$here/$net.bin"
    echo "$net.bin: OK"
done
