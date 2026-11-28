#!/usr/bin/env bash
# BL-1008: with the `oracle` feature on and an archive missing, dash-oracle-sys's
# build script links nothing. It must still pick the archive up once it appears,
# without `cargo clean`, and must not rerun on every build meanwhile.
#
# Builds `dash-oracle-sys --features oracle --lib` against a fresh, empty cache root
# with a private target directory, then:
#   P1  a second identical build does not rerun the build script;
#   P2  after a MANIFEST and a stand-in archive (made with `ar`, no Core code) appear
#       for every pin, the next identical build reruns it and links each pin.
# Every build runs with the same environment, so no env change triggers a rerun.
#
# Usage: contrib/node/check-oracle-rerun.sh
set -euo pipefail

src="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
pins_file="$src/dash-oracle-sys/pins.txt"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/check-oracle-rerun.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

cache="$tmp/cache"
mkdir "$cache"
export CARGO_TARGET_DIR="$tmp/target" DASH_ORACLE_CACHE="$cache" \
    DASH_ORACLE_ALLOW_UNPINNED=1 DASH_ORACLE_FLAVOR=plain DASH_SRC="$tmp/no-dash-src"
unset CARGO_BUILD_TARGET_DIR CARGO_BUILD_BUILD_DIR CARGO_BUILD_TARGET DASH_ORACLE_AUTOBUILD

build() {
    (cd "$src" && cargo build --locked -p dash-oracle-sys --features oracle --lib) > "$tmp/build.log" 2>&1 ||
        { cat "$tmp/build.log" >&2; echo "check-oracle-rerun: cargo build failed" >&2; exit 1; }
}
output() {
    local f
    f="$(ls -t "$CARGO_TARGET_DIR"/debug/build/dash-oracle-sys-*/output 2> /dev/null | head -n1 || true)"
    [ -n "$f" ] || { echo "check-oracle-rerun: no build-script output under $CARGO_TARGET_DIR" >&2; exit 1; }
    echo "$f"
}

build
out="$(output)"
grep -q '^cargo:rustc-link-lib=static=' "$out" &&
    { echo "check-oracle-rerun: the empty cache linked an archive" >&2; exit 1; }
before="$(stat -c %.9Y "$out")"
sleep 1
build
[ "$(stat -c %.9Y "$out")" = "$before" ] ||
    { echo "check-oracle-rerun: P1 failed: the build script reran with the archive still missing" >&2; exit 1; }
echo "P1 ok: no rerun while the archive is missing"

mapfile -t pins < <(grep -Ev '^[[:space:]]*(#|$)' "$pins_file" | awk '{ print $1, $2 }')
[ "${#pins[@]}" -gt 0 ] || { echo "check-oracle-rerun: $pins_file lists no pins" >&2; exit 1; }
sleep 1
echo "stand-in member, not Core code" > "$tmp/member.txt"
for p in "${pins[@]}"; do
    read -r name commit <<< "$p"
    tag="${name//./_}"
    dir="$cache/$commit/plain"
    mkdir -p "$dir"
    printf 'pin %s\ncommit %s\nflavor plain\nsymbol_prefix oracle_%s_\n' "$name" "$commit" "$tag" > "$dir/MANIFEST"
    ar rc "$dir/libdash_oracle_$tag.a" "$tmp/member.txt"
done
build
out="$(output)"
[ "$(stat -c %.9Y "$out")" != "$before" ] ||
    { echo "check-oracle-rerun: P2 failed: the build script did not rerun after the archives appeared" >&2; exit 1; }
for p in "${pins[@]}"; do
    read -r name _ <<< "$p"
    grep -qx "cargo:rustc-link-lib=static=dash_oracle_${name//./_}" "$out" ||
        { echo "check-oracle-rerun: P2 failed: $name is not linked after its archive appeared" >&2; exit 1; }
done
echo "P2 ok: the next build reran the script and linked ${#pins[@]} pin(s)"
