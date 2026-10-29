#!/usr/bin/env bash
# The mobile gates (SB0 §2.3):
#   L6  the default-feature normal closure of dash-spv-ffi and key-wallet-ffi,
#       for aarch64-linux-android and aarch64-apple-ios, holds nothing that
#       contrib/node/mobile-deny.toml bans and no Pn or N crate, apart from
#       the entries of layers-baseline.toml (check-layers.py mobile);
#   L7  the stripped libdash_spv_ffi.so for aarch64-linux-android grows by at
#       most min(2 %, 256 KiB) over the reference contrib/node/mobile-size.txt,
#       the one committed at the merge base with node/integration (or
#       layers.toml's [size] while it has none; DEC-170), in the same build
#       environment (profile, NDK, API). The library may leave no sqlite3_
#       symbol undefined: Android links it with undefined symbols allowed, and
#       the bundled SQLite is built without what the store never uses
#       (android-env.sh), so a dependency that turns on a rusqlite feature
#       needing one would fail only on the device (BL-303).
#       It is built with `profile.mobile` once the workspace has one (SB2),
#       `release` until then, using the NDK through contrib/mobile/android-env.sh.
#   O5  (DEC-112, SB0 §7) blst's `no-threads` in no N package's own graph
#       (check-layers.py mobile), and the V crates' clippy bans on rayon's
#       global pool and blst's pooled calls (check-spv.sh, which needs the
#       NDK too, so --no-size skips it).
#
# Usage: contrib/node/check-mobile.sh [--no-size | --record [--ruling DEC-…]]
#   --no-size  L6 and O5's graph rule only (no NDK needed).
#   --record   build and record the size in mobile-size.txt. Growth over the
#              reference's allowance, in total, or a profile, NDK or API
#              change needs --ruling; re-recording does not move the reference.
set -euo pipefail

cd "$(dirname "$0")/../.."
mode="${1:-}"
case "$mode" in
    "" | --no-size | --record) ;;
    *) sed -n '2,/^set -euo/p' "$0" | sed '$d' >&2; exit 2 ;;
esac

rc=0
if [ "$mode" != --record ]; then
    python3 contrib/node/check-layers.py mobile || rc=1
fi
[ "$mode" = --no-size ] && exit "$rc"
if [ "$mode" != --record ]; then
    contrib/node/check-spv.sh || rc=1
fi

profile=release
grep -q '^\[profile\.mobile\]' Cargo.toml && profile=mobile
target=aarch64-linux-android
. contrib/mobile/android-env.sh
cargo build --locked -q -p dash-spv-ffi --lib --profile "$profile" --target "$target"
lib="target/$target/$profile/libdash_spv_ffi.so"
"$LLVM_STRIP" -o "$lib.stripped" "$lib"
undefined="$("$(dirname "$LLVM_STRIP")/llvm-nm" -D --undefined-only "$lib.stripped" | grep -w 'sqlite3_[a-z0-9_]*' || true)"
if [ -n "$undefined" ]; then
    printf 'error: %s leaves SQLite symbols undefined (the bundled SQLite omits them; contrib/mobile/android-env.sh):\n%s\n' \
        "$lib" "$undefined" >&2
    rc=1
fi
bytes="$(stat -c %s "$lib.stripped")"
python3 contrib/node/check-layers.py size "$bytes" --profile "$profile" --ndk "$ANDROID_NDK_VERSION" \
    --api "$ANDROID_API" "$@" || rc=1
exit "$rc"
