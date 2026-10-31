#!/usr/bin/env bash
# Build chainparams_dump for one oracle pin from a controlled clean Core build.
#
# The commit comes from dash-oracle-sys/pins.txt, never from a working tree.
# By default every run is a fresh clean build: the script deletes the pin's
# build tree, `git archive`s the commit into it, verifies every tracked file
# against the commit's blobs, builds depends (with no cache from an earlier
# run) and the libraries dash-tx links, and verifies the tracked files again.
# Then it writes the tree's anchor, .bs5-anchor: the sha256 of every file and
# the target of every symlink in the tree. It compiles chainparams_dump.cpp
# against that tree with the pin and commit compiled in. MANIFEST-<pin> records
# the commit, its tree, the sha256 of the dump source, of the anchor, of every
# linked archive and of the binary, and whether the build was fresh. run.py
# accepts a dump only if the binary and its embedded provenance agree with that
# MANIFEST and with pins.txt, and only from a fresh build unless it is told
# otherwise.
#
# BS5_REUSE=1 reuses an earlier build tree instead, for development only. The
# tree must still equal the commit, and every one of its files must equal the
# anchor its own clean build wrote; anything else is refused (an archive
# rebuilt from changed source that was then restored, BS5 review r2). Without
# an anchor it does a fresh build.
#
# usage: build.sh <pin> <out-dir>        (pin: a row of pins.txt)
#   DASH_SRC     Dash Core repository (default ~/workspace/dash)
#   BS5_BUILD    build root (default /work/scratch/bs5/core-build)
#   BS5_SOURCES  depends download directory (default /work/cache/oracle/depends-sources)
#   BS5_REUSE    1: reuse the anchored build tree (default: a fresh build)
#   JOBS         parallel jobs (default 12)
set -euo pipefail

pin=${1:?usage: build.sh <pin> <out-dir>}
out=${2:?usage: build.sh <pin> <out-dir>}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
pins_file=$repo/dash-oracle-sys/pins.txt
dash_src=${DASH_SRC:-$HOME/workspace/dash}
root=${BS5_BUILD:-/work/scratch/bs5/core-build}
sources=${BS5_SOURCES:-/work/cache/oracle/depends-sources}
jobs=${JOBS:-12}
reuse=${BS5_REUSE:-0}

log() { echo "[bs5-build] $*" >&2; }
commit=$(awk -v p="$pin" '!/^#/ && $1 == p { print $2 }' "$pins_file")
[ -n "$commit" ] || { log "pin $pin is not in $pins_file"; exit 1; }
git -C "$dash_src" cat-file -e "$commit^{commit}" || { log "$commit not in $dash_src"; exit 1; }
tree_id=$(git -C "$dash_src" rev-parse "$commit^{tree}")
build=$root/$commit
mkdir -p "$out"
out=$(cd "$out" && pwd)

# Every tracked file of the commit must be present with the commit's blob id
# (the pins have only regular files: modes 100644 and 100755). A file with the
# export-subst attribute (src/clientversion.cpp) must instead equal what
# `git archive` makes of it.
verify_tree() {
  local paths want got f
  paths=$(mktemp /dev/shm/bs5-tree.XXXXXX)
  git -C "$dash_src" ls-tree -r "$commit" | awk -F'\t' '{ split($1, m, " "); print $2 "\t" m[3] }' |
    LC_ALL=C sort > "$paths.want"
  cut -f1 "$paths.want" > "$paths"
  git -C "$dash_src" check-attr --source="$commit" --stdin export-subst < "$paths" |
    awk -F': ' '$3 == "set" { print $1 }' > "$paths.subst"
  while IFS= read -r f; do
    sha=$(git -C "$dash_src" archive --format=tar "$commit" -- "$f" | tar -xO "$f" | git hash-object --stdin)
    awk -F'\t' -v f="$f" -v s="$sha" 'BEGIN { OFS = "\t" } $1 == f { $2 = s } { print }' \
      "$paths.want" > "$paths.tmp" && mv "$paths.tmp" "$paths.want"
  done < "$paths.subst"
  want=$(cat "$paths.want")
  got=$(cd "$build" && git hash-object --no-filters --stdin-paths < "$paths" | paste "$paths" -)
  rm -f "$paths" "$paths.want" "$paths.subst"
  if [ "$want" != "$got" ]; then
    log "$pin: $build differs from $commit:"
    diff <(echo "$want") <(echo "$got") | head -20 >&2
    return 1
  fi
}

# Every file of the build tree with its sha256, and every symlink with its
# target, sorted: the anchor's content.
tree_digest() {
  (
    cd "$build"
    find . -path ./.bs5-anchor -prune -o -type f -print0 | xargs -0 -r -P "$jobs" -n 512 sha256sum |
      LC_ALL=C sort -k2
    find . -type l -printf 'link %p -> %l\n' | LC_ALL=C sort
  )
}

# The same environment rules as build-oracle.sh: no ccache, fixed locale.
while IFS= read -r var; do unset "$var"; done < <(env | sed -n 's/^\(CCACHE_[A-Z_]*\)=.*/\1/p')
export CCACHE_DISABLE=1 LC_ALL=C TZ=UTC
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS MAKEFLAGS

if [ "$reuse" = 1 ] && [ -f "$build/.bs5-anchor" ]; then
  log "$pin: reusing $build (BS5_REUSE=1); checking it against its anchor"
  verify_tree
  if ! tree_digest | cmp -s - "$build/.bs5-anchor"; then
    log "$pin: $build differs from the anchor its clean build wrote:"
    diff "$build/.bs5-anchor" <(tree_digest) | head -20 >&2
    exit 1
  fi
  mode=reused
else
  log "$pin: fresh clean build of $commit in $build"
  rm -rf "$build"
  mkdir -p "$build"
  git -C "$dash_src" archive --format=tar "$commit" | tar -x -C "$build"
  verify_tree
  host=$("$build/depends/config.guess")
  (
    cd "$build"
    # depends' own cache (depends/built) starts empty in the fresh tree
    flock "$sources/.download.lock" make -C depends -j"$jobs" HOST="$host" NO_QT=1 NO_WALLET=1 NO_ZMQ=1 NO_UPNP=1 NO_NATPMP=1 NO_USDT=1 \
      SOURCES_PATH="$sources"
    ./autogen.sh
    CONFIG_SITE="$build/depends/$host/share/config.site" CC=gcc CXX=g++ \
      ./configure --prefix=/ --disable-ccache --disable-wallet --without-gui --disable-zmq \
      --without-miniupnpc --without-natpmp --disable-bench --disable-tests --disable-fuzz-binary --disable-man
    make -C src -j"$jobs" dash-tx
  ) > "$root/build-$pin.log" 2>&1 || { log "$pin: build failed, see $root/build-$pin.log"; exit 1; }
  verify_tree
  tree_digest > "$build/.bs5-anchor"
  chmod a-w "$build/.bs5-anchor"
  mode=fresh
fi

src=$build/src
dep=$build/depends/$("$build/depends/config.guess")
libs=(
  "$src/libbitcoin_common.a" "$src/libbitcoin_util.a" "$src/.libs/libunivalue.a" "$src/libbitcoin_consensus.a"
  "$src/crypto/.libs/libbitcoin_crypto_base.a" "$src/crypto/.libs/libbitcoin_crypto_sph.a"
  "$src/crypto/.libs/libbitcoin_crypto_x86_aesni.a" "$src/crypto/.libs/libbitcoin_crypto_sse41.a"
  "$src/crypto/.libs/libbitcoin_crypto_avx2.a" "$src/crypto/.libs/libbitcoin_crypto_x86_shani.a"
  "$src/crypto/.libs/libbitcoin_crypto_ssse3.a"
  "$src/dashbls/.libs/libdashbls.a" "$src/dashbls/.libs/librelic.a" "$src/dashbls/.libs/libmimalloc-secure.a"
  "$src/secp256k1/.libs/libsecp256k1.a" "$src/secp256k1/.libs/libsecp256k1_precomputed.a"
  "$dep/lib/libbacktrace.a" "$dep/lib/libgmp.a"
)
bin=$out/chainparams_dump_$pin
g++ -m64 -std=c++20 -O2 -DHAVE_CONFIG_H -DGSL_NO_IOSTREAMS -DBOOST_MULTI_INDEX_DISABLE_SERIALIZATION \
  -DBS5_PIN="\"$pin\"" -DBS5_COMMIT="\"$commit\"" \
  -I"$src" -I"$src/config" -I"$src/univalue/include" -I"$src/secp256k1/include" \
  -isystem "$src/dashbls/include" -isystem "$src/dashbls/depends/relic/include" \
  -isystem "$src/dashbls/depends/minialloc/include" -isystem "$src/immer" -isystem "$dep/include" \
  -c "$here/chainparams_dump.cpp" -o "$bin.o"
g++ -m64 -o "$bin" "$bin.o" "${libs[@]}" -lpthread
rm -f "$bin.o"

{
  echo "pin $pin"
  echo "commit $commit"
  echo "tree $tree_id"
  echo "build $mode"
  echo "anchor $(sha256sum "$build/.bs5-anchor" | cut -d' ' -f1)"
  echo "source $(sha256sum "$here/chainparams_dump.cpp" | cut -d' ' -f1) chainparams_dump.cpp"
  echo "compiler $(g++ --version | head -1)"
  for lib in "${libs[@]}"; do echo "archive $(sha256sum "$lib" | cut -d' ' -f1) ${lib#"$build"/}"; done
  echo "binary $(sha256sum "$bin" | cut -d' ' -f1)"
} > "$out/MANIFEST-$pin"
log "$pin: $bin ($(awk '$1 == "binary" { print $2 }' "$out/MANIFEST-$pin"))"
