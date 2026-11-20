#!/usr/bin/env bash
# Build ev2a_codec_probe: Dash Core's masternode-list codecs as a line oracle for dash-dml's codec
# vectors (designs/EV2-dml.md §2.9, §6.2). It compiles probe.cpp against a Core build tree of the
# v24.0 oracle pin (dash-oracle-sys/pins.txt) with unlocalized libraries, such as the one
# contrib/node/sb4-core-scenarios/build.sh makes, and links it the way that tree linked dashd. The
# build tree is only read: objects and the binary go to <out-dir>.
#
# usage: build.sh <core-build-tree> <out-dir>
set -euo pipefail

tree=$(cd "${1:?usage: build.sh <core-build-tree> <out-dir>}" && pwd)
out=${2:?usage: build.sh <core-build-tree> <out-dir>}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
log() { echo "[ev2a-codec-probe] $*" >&2; }

commit=$(awk '!/^#/ && $1 == "v24.0" { print $2 }' "$repo/dash-oracle-sys/pins.txt")
[ "$(basename "$tree")" = "$commit" ] || { log "$tree is not the v24.0 pin's tree ($commit)"; exit 1; }
[ -s "$tree/.sb4-link" ] || { log "$tree has no recorded dashd link line (.sb4-link)"; exit 1; }
src=$tree/src
host=$("$tree/depends/config.guess")
dep=$tree/depends/$host

while IFS= read -r var; do unset "$var"; done < <(env | sed -n 's/^\(CCACHE_[A-Z_]*\)=.*/\1/p')
export LC_ALL=C TZ=UTC
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS MAKEFLAGS

std=$(sed -n 's/^.*-std=\(c++[0-9a-z]*\).*$/\1/p' "$tree/.sb4-link" | head -1)
g++ -m64 -std="${std:-c++20}" -O1 -DHAVE_CONFIG_H -DGSL_NO_IOSTREAMS -DBOOST_MULTI_INDEX_DISABLE_SERIALIZATION \
  -ffile-prefix-map="$tree"=. -ffile-prefix-map="$here"=. \
  -I"$src" -I"$src/config" -I"$src/univalue/include" -I"$src/secp256k1/include" -I"$src/leveldb/include" \
  -isystem "$src/dashbls/include" -isystem "$src/dashbls/depends/relic/include" \
  -isystem "$src/dashbls/depends/minialloc/include" -isystem "$src/immer" -isystem "$dep/include" \
  -c "$here/probe.cpp" -o "$out/probe.o"
# dashd's link line with the probe's object in place of dashd's, writing to <out-dir>.
link=$(sed -e "s| -o dashd | -o $out/ev2a_codec_probe $out/probe.o |" -e 's/ [^ ]*bitcoind\.o//g' "$tree/.sb4-link")
(cd "$src" && eval "$link") > "$out/link.log" 2>&1 || { log "link failed, see $out/link.log"; exit 1; }
{
  echo "pin v24.0"
  echo "commit $commit"
  echo "source $(sha256sum "$here/probe.cpp" | cut -d' ' -f1) probe.cpp"
  echo "compiler $(g++ --version | head -1)"
  for lib in libbitcoin_node.a libbitcoin_common.a libbitcoin_util.a libbitcoin_consensus.a; do
    echo "archive $(sha256sum "$src/$lib" | cut -d' ' -f1) src/$lib"
  done
  echo "binary $(sha256sum "$out/ev2a_codec_probe" | cut -d' ' -f1)"
} > "$out/MANIFEST"
log "$out/ev2a_codec_probe"
