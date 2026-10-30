# Environment for cross-building workspace crates for aarch64-linux-android
# with the NDK alone (no cargo-ndk). Source it:
#
#   . contrib/mobile/android-env.sh
#
# Needs ANDROID_HOME (or ANDROID_NDK_HOME) and `rustup target add
# aarch64-linux-android`. ANDROID_NDK_HOME wins; otherwise the newest NDK
# under $ANDROID_HOME/ndk. ANDROID_API (default 24) picks the clang wrapper.
# Exports LLVM_STRIP, LIBSQLITE3_FLAGS, ANDROID_NDK_VERSION (the NDK's Pkg.Revision) and
# ANDROID_API for the stripped size check (contrib/node/check-mobile.sh), which
# records them: a size measured with another NDK or API is a baseline migration.

android_ndk="${ANDROID_NDK_HOME:-}"
if [ -z "$android_ndk" ] && [ -n "${ANDROID_HOME:-}" ] && [ -d "$ANDROID_HOME/ndk" ]; then
    android_ndk="$(find "$ANDROID_HOME/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
fi
if [ -z "$android_ndk" ] || [ ! -d "$android_ndk/toolchains/llvm/prebuilt" ]; then
    echo "android-env.sh: no NDK found; set ANDROID_NDK_HOME or ANDROID_HOME" >&2
    return 1 2> /dev/null || exit 1
fi
android_tc="$(find "$android_ndk/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
android_api="${ANDROID_API:-24}"
export ANDROID_API="$android_api"
export ANDROID_NDK_VERSION="$(sed -n 's/^Pkg\.Revision *= *//p' "$android_ndk/source.properties" 2> /dev/null)"
if [ -z "$ANDROID_NDK_VERSION" ]; then
    echo "android-env.sh: $android_ndk/source.properties names no Pkg.Revision" >&2
    return 1 2> /dev/null || exit 1
fi

export CC_aarch64_linux_android="$android_tc/bin/aarch64-linux-android${android_api}-clang"
export CXX_aarch64_linux_android="$android_tc/bin/aarch64-linux-android${android_api}-clang++"
export AR_aarch64_linux_android="$android_tc/bin/llvm-ar"
export RANLIB_aarch64_linux_android="$android_tc/bin/llvm-ranlib"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_aarch64_linux_android"
# rs-x11-hash's build script assumes cargo-ndk and panics without this.
export CARGO_NDK_SYSROOT_PATH="$android_tc/sysroot"
# For bindgen-based build scripts.
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android="--sysroot=$android_tc/sysroot --target=aarch64-linux-android${android_api}"
export LLVM_STRIP="$android_tc/bin/llvm-strip"
# BL-303 (SB0 §3.1 v5): the bundled SQLite (dash-kv-sqlite) leaves out what the store never
# uses: full-text search, R-trees, JSON, STAT4, SOUNDEX, the column-metadata API, extension
# loading (SQL `load_extension()` goes too), the deprecated API and shared cache. `dbstat` stays:
# `approximate_size` reads it. libdash_spv_ffi.so is 670,456 B smaller (L7, which guards it: the
# full SQLite is +654 KiB, past the allowance; it also fails on undefined sqlite3_ symbols). Only
# builds that source this file get them (clippy-lib.sh's lint_env unsets them): iOS links the
# system SQLite, and other builds keep libsqlite3-sys's flags, as does a host that
# builds its own libsqlite3-sys (Platform's unified SDK; one `links = "sqlite3"` build per
# graph). Not in .cargo/config.toml: perfgate refuses heads that change it. A LIBSQLITE3_FLAGS
# already set wins.
: "${LIBSQLITE3_FLAGS=-USQLITE_ENABLE_FTS3 -USQLITE_ENABLE_FTS3_PARENTHESIS -USQLITE_ENABLE_FTS5 \
-USQLITE_ENABLE_RTREE -DSQLITE_OMIT_JSON -USQLITE_ENABLE_STAT4 -USQLITE_SOUNDEX \
-USQLITE_ENABLE_COLUMN_METADATA -USQLITE_ENABLE_LOAD_EXTENSION -DSQLITE_OMIT_LOAD_EXTENSION \
-DSQLITE_OMIT_DEPRECATED -DSQLITE_OMIT_SHARED_CACHE}"
export LIBSQLITE3_FLAGS
unset android_ndk android_tc android_api
