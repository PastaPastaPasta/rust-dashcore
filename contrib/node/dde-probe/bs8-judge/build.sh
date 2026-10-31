#!/usr/bin/env bash
# Build a Core-linked judge: Dash Core's own readers of the overlay messages
# (bs8-judge by default, or another judge source such as ../bs9-judge),
# linked against a clean build of the v24.0 oracle pin
# (dash-oracle-sys/pins.txt).
#
# A fresh run deletes the pin's build tree, `git archive`s the pinned commit
# into it, builds depends and dashd (whose libraries the judge links), and
# records the link line make used for dashd. The judge then links with the same
# libraries in place of dashd's own objects. MANIFEST records the commit, the
# judge source, every linked archive and the binary.
#
# usage: build.sh <out-dir> [<judge.cpp>]
#   The binary is named after the source's directory: bs8_judge, bs9_judge.
#   DASH_SRC     Dash Core repository (default ~/workspace/dash)
#   BS8_BUILD    build root (default /work/scratch/bs8/core-build)
#   BS8_SOURCES  depends download directory (default /work/cache/oracle/depends-sources)
#   BS8_REUSE    1: relink against an earlier build tree of the same commit
#   JOBS         parallel jobs (default 12)
set -euo pipefail

out=${1:?usage: build.sh <out-dir> [<judge.cpp>]}
pin=v24.0
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../../.." && pwd)
dash_src=${DASH_SRC:-$HOME/workspace/dash}
root=${BS8_BUILD:-/work/scratch/bs8/core-build}
sources=${BS8_SOURCES:-/work/cache/oracle/depends-sources}
jobs=${JOBS:-12}
reuse=${BS8_REUSE:-0}
judge=$(cd "$(dirname "${2:-$here/judge.cpp}")" && pwd)/$(basename "${2:-$here/judge.cpp}")
name=$(basename "$(dirname "$judge")" | tr - _)

log() { echo "[$name] $*" >&2; }
commit=$(awk -v p="$pin" '!/^#/ && $1 == p { print $2 }' "$repo/dash-oracle-sys/pins.txt")
[ -n "$commit" ] || { log "pin $pin is not in pins.txt"; exit 1; }
build=$root/$commit
mkdir -p "$out" "$root"
out=$(cd "$out" && pwd)

# The same environment rules as build-oracle.sh: no ccache, fixed locale.
while IFS= read -r var; do unset "$var"; done < <(env | sed -n 's/^\(CCACHE_[A-Z_]*\)=.*/\1/p')
export CCACHE_DISABLE=1 LC_ALL=C TZ=UTC
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS MAKEFLAGS RUSTC_WRAPPER

if [ "$reuse" = 1 ] && [ -f "$build/.bs8-link" ]; then
  log "reusing $build"
  mode=reused
else
  log "fresh clean build of $commit in $build"
  rm -rf "$build"
  mkdir -p "$build"
  git -C "$dash_src" archive --format=tar "$commit" | tar -x -C "$build"
  host=$("$build/depends/config.guess")
  (
    cd "$build"
    flock "$sources/.download.lock" make -C depends -j"$jobs" HOST="$host" NO_QT=1 NO_WALLET=1 NO_ZMQ=1 \
      NO_UPNP=1 NO_NATPMP=1 NO_USDT=1 SOURCES_PATH="$sources"
    ./autogen.sh
    CONFIG_SITE="$build/depends/$host/share/config.site" CC=gcc CXX=g++ \
      ./configure --prefix=/ --disable-ccache --disable-wallet --without-gui --disable-zmq \
      --without-miniupnpc --without-natpmp --disable-bench --disable-tests --disable-fuzz-binary --disable-man
    make -C src -j"$jobs" dashd
    rm -f src/dashd
    make -C src V=1 dashd
  ) > "$root/build-$pin.log" 2>&1 || { log "build failed, see $root/build-$pin.log"; exit 1; }
  # dashd's link command, as make ran it (libtool --mode=link ... -o dashd ...)
  grep -E -- '--mode=link .* -o dashd ' "$root/build-$pin.log" | tail -1 > "$build/.bs8-link"
  [ -s "$build/.bs8-link" ] || { log "no dashd link line in the log"; exit 1; }
  mode=fresh
fi

src=$build/src
dep=$build/depends/$("$build/depends/config.guess")
flags=$(sed -n 's/^.*-std=\(c++[0-9a-z]*\).*$/\1/p' "$build/.bs8-link" | head -1)
g++ -m64 -std="${flags:-c++20}" -O1 -DHAVE_CONFIG_H -DGSL_NO_IOSTREAMS -DBOOST_MULTI_INDEX_DISABLE_SERIALIZATION \
  -DBS8_COMMIT="\"$commit\"" \
  -I"$src" -I"$src/config" -I"$src/univalue/include" -I"$src/secp256k1/include" -I"$src/leveldb/include" \
  -isystem "$src/dashbls/include" -isystem "$src/dashbls/depends/relic/include" \
  -isystem "$src/dashbls/depends/minialloc/include" -isystem "$src/immer" -isystem "$dep/include" \
  -c "$judge" -o "$src/$name.o"
# Replace dashd's own objects with the judge's and its output with the judge.
link=$(sed -e "s/ -o dashd / -o $name /" -e 's/ [^ ]*bitcoind\.o//g' "$build/.bs8-link")
(cd "$src" && eval "${link/$name /$name $name.o }") > "$root/link-$name.log" 2>&1 ||
  { log "link failed, see $root/link-$name.log"; exit 1; }
bin=$out/$name
cp "$src/$name" "$bin"

{
  echo "pin $pin"
  echo "commit $commit"
  echo "build $mode"
  echo "source $(sha256sum "$judge" | cut -d' ' -f1) judge.cpp"
  echo "compiler $(g++ --version | head -1)"
  for lib in "$src"/libbitcoin_node.a "$src"/libbitcoin_common.a "$src"/libbitcoin_util.a; do
    echo "archive $(sha256sum "$lib" | cut -d' ' -f1) ${lib#"$build"/}"
  done
  echo "binary $(sha256sum "$bin" | cut -d' ' -f1)"
} > "$out/MANIFEST"
log "$bin ($(awk '$1 == "binary" { print $2 }' "$out/MANIFEST"))"
