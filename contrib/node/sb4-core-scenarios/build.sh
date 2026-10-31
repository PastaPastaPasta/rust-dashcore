#!/usr/bin/env bash
# Build sb4_scenarios: the Core-linked oracle harness for SB4 (dash-chain core),
# designs/SB4-dash-chain.md §15. It links Dash Core's own *unlocalized* server
# libraries plus libtest_util.a (RegTestingSetup, mining helpers) at the v24.0
# oracle pin (dash-oracle-sys/pins.txt), so every verdict is Core's code.
#
# A fresh run deletes the pin's build tree, `git archive`s the pinned commit into
# it, builds depends, configures with tests enabled, builds dashd (for its link
# line) and libtest_util.a, and records the link line make used for dashd. The
# harness then links with those libraries in place of dashd's objects, with the
# `--wrap`s of wrap-symbols.txt (VerifyChainLock's four-argument overload and
# Chainstate::EnforceBlock) and the interpositions of interpose-symbols.txt
# (Chainstate::ConnectBlock and DisconnectBlock); see check-seam.sh. MANIFEST
# records the commit, configure flags, the harness source and every linked
# archive's and derived object's SHA-256.
#
# usage: build.sh <out-dir>
#   DASH_SRC      Dash Core repository (default ~/workspace/dash)
#   SB4_BUILD     build root (default /work/scratch/sb4-core-scenarios/core-build)
#   SB4_SOURCES   depends download directory (default /work/cache/oracle/depends-sources)
#   SB4_REUSE     1: relink against an earlier build tree of the same commit
#   JOBS          parallel jobs (default 8)
set -euo pipefail

out=${1:?usage: build.sh <out-dir>}
pin=v24.0
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
dash_src=${DASH_SRC:-$HOME/workspace/dash}
root=${SB4_BUILD:-/work/scratch/sb4-core-scenarios/core-build}
sources=${SB4_SOURCES:-/work/cache/oracle/depends-sources}
jobs=${JOBS:-8}
reuse=${SB4_REUSE:-0}
configure_flags="--prefix=/ --disable-ccache --disable-wallet --without-gui --disable-zmq --without-miniupnpc --without-natpmp --disable-bench --enable-tests --disable-fuzz-binary --disable-man --disable-gui-tests"
# The seams: one -Wl,--wrap per line of wrap-symbols.txt (check-seam.sh proves
# each); build.sh checks every mangled name demangles to the recorded signature.
wraps=$(grep -v '^#' "$here/wrap-symbols.txt")

log() { echo "[sb4-core-scenarios] $*" >&2; }
commit=$(awk -v p="$pin" '!/^#/ && $1 == p { print $2 }' "$repo/dash-oracle-sys/pins.txt")
[ -n "$commit" ] || { log "pin $pin is not in pins.txt"; exit 1; }
build=$root/$commit
mkdir -p "$out" "$root"
out=$(cd "$out" && pwd)

# The same environment rules as build-oracle.sh: no ccache, fixed locale.
while IFS= read -r var; do unset "$var"; done < <(env | sed -n 's/^\(CCACHE_[A-Z_]*\)=.*/\1/p')
export CCACHE_DISABLE=1 LC_ALL=C TZ=UTC
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS MAKEFLAGS RUSTC_WRAPPER

if [ "$reuse" = 1 ] && [ -f "$build/.sb4-link" ]; then
  log "reusing $build"
  grep -qF -- "./configure $configure_flags" "$build/config.log" || { log "$build was configured differently"; exit 1; }
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
    # shellcheck disable=SC2086
    CONFIG_SITE="$build/depends/$host/share/config.site" CC=gcc CXX=g++ ./configure $configure_flags
    make -C src -j"$jobs" dashd libtest_util.a
    rm -f src/dashd
    make -C src V=1 dashd
  ) > "$root/build-$pin.log" 2>&1 || { log "build failed, see $root/build-$pin.log"; exit 1; }
  # dashd's link command, as make ran it (libtool --mode=link ... -o dashd ...)
  { grep -E -- '--mode=link .* -o dashd ' "$root/build-$pin.log" || true; } | tail -1 > "$build/.sb4-link"
  [ -s "$build/.sb4-link" ] || { log "no dashd link line in the log"; exit 1; }
  mode=fresh
fi

src=$build/src
host=${host:-$("$build/depends/config.guess")}
dep=$build/depends/$host
wrap_flags=
while IFS=$'\t' read -r sym _obj _fn want; do
  [ "$(echo "$sym" | c++filt)" = "$want" ] || { log "$sym does not demangle to $want"; exit 1; }
  wrap_flags="$wrap_flags -Wl,--wrap=$sym"
done <<< "$wraps"
# The interposed definitions (interpose-symbols.txt): copy each defining member
# into $out/derived (not the shared build tree: MANIFEST pins the copy), make the
# symbol weak there with __real_<sym> at its address, link the copy ahead of the
# archives and point the symbol at __wrap_<sym>. Members go in file order.
interposes=$(grep -v '^#' "$here/interpose-symbols.txt")
derived=$out/derived
rm -rf "$derived"
mkdir -p "$derived"
interpose_flags=
interpose_objs=
members=()
declare -A objcopy_args=()
while IFS=$'\t' read -r sym member _callers want; do
  [ "$(echo "$sym" | c++filt)" = "$want" ] || { log "$sym does not demangle to $want"; exit 1; }
  [ "$(ar t "$src/libbitcoin_node.a" | grep -cxF "$member")" = 1 ] || { log "$member is not unique in libbitcoin_node.a"; exit 1; }
  orig=$derived/orig-$member
  [ -e "$orig" ] || { ar p "$src/libbitcoin_node.a" "$member" > "$orig"; members+=("$member"); }
  # st_value and section of the one global FUNC definition; the section's name must be unique.
  read -r value ndx < <(readelf -sW "$orig" | awk -v s="$sym" '$8 == s && $4 == "FUNC" && $5 == "GLOBAL" { print $2, $7 }') || true
  [ -n "${value:-}" ] || { log "$member does not define $sym globally"; exit 1; }
  sec=$(readelf -SW "$orig" | sed -n "s/^ *\[ *$ndx\] \([^ ]*\) .*/\1/p")
  [ "$(readelf -SW "$orig" | grep -cE "^ *\[ *[0-9]+\] ${sec//./\\.} ")" = 1 ] || { log "section $sec of $sym is not unique"; exit 1; }
  objcopy_args[$member]+=" --weaken-symbol=$sym --add-symbol=__real_$sym=$sec:0x$value,global,function"
  interpose_flags="$interpose_flags -Wl,--defsym=$sym=__wrap_$sym"
  value=
done <<< "$interposes"
for member in "${members[@]}"; do
  # shellcheck disable=SC2086
  objcopy ${objcopy_args[$member]} "$derived/orig-$member" "$derived/sb4-$member"
  interpose_objs="$interpose_objs $derived/sb4-$member"
done
std=$(sed -n 's/^.*-std=\(c++[0-9a-z]*\).*$/\1/p' "$build/.sb4-link" | head -1)
g++ -m64 -std="${std:-c++20}" -O1 -DHAVE_CONFIG_H -DGSL_NO_IOSTREAMS -DBOOST_MULTI_INDEX_DISABLE_SERIALIZATION \
  -DSB4_CORE_COMMIT="\"$commit\"" \
  -ffile-prefix-map="$build"=. -ffile-prefix-map="$here"=. \
  -I"$src" -I"$src/config" -I"$src/univalue/include" -I"$src/secp256k1/include" -I"$src/leveldb/include" \
  -isystem "$src/dashbls/include" -isystem "$src/dashbls/depends/relic/include" \
  -isystem "$src/dashbls/depends/minialloc/include" -isystem "$src/immer" -isystem "$dep/include" \
  -c "$here/harness.cpp" -o "$src/sb4_scenarios.o"
# Replace dashd's own objects with the harness, add libtest_util.a ahead of the
# server libraries, and wrap the seams.
link=$(sed -e 's/ -o dashd / -o sb4_scenarios /' -e 's/ [^ ]*bitcoind\.o//g' "$build/.sb4-link")
link=${link/sb4_scenarios /sb4_scenarios sb4_scenarios.o$interpose_objs libtest_util.a$wrap_flags$interpose_flags }
(cd "$src" && eval "$link") > "$root/link-$pin.log" 2>&1 || { log "link failed, see $root/link-$pin.log"; exit 1; }
bin=$out/sb4_scenarios
cp "$src/sb4_scenarios" "$bin"

# Every static archive the final link read: libtool echoes the resolved command;
# -l names resolve through its -L directories (depends' boost, libevent, gmp).
link_archives() {
  local tok name d dirs=()
  # shellcheck disable=SC2013
  for tok in $(sed -n 's/^libtool: link: //p' "$root/link-$pin.log"); do
    case $tok in
      *.a) echo "$src/$tok" ;;
      -L*) dirs+=("${tok#-L}") ;;
      -l*)
        name=${tok#-l}
        for d in "${dirs[@]}"; do
          if [ -e "$d/lib$name.a" ]; then echo "$d/lib$name.a"; break; fi
        done ;;
    esac
  done | xargs -r realpath | sort -u
}
archives=$(link_archives)
[ -n "$archives" ] || { log "no archives found in $root/link-$pin.log"; exit 1; }

{
  echo "pin $pin"
  echo "commit $commit"
  echo "build $mode"
  echo "configure $configure_flags"
  while IFS=$'\t' read -r sym _; do echo "wrap $sym"; done <<< "$wraps"
  while IFS=$'\t' read -r sym _; do echo "interpose $sym"; done <<< "$interposes"
  for obj in $interpose_objs; do
    echo "object $(sha256sum "$obj" | cut -d' ' -f1) ${obj#"$out"/}"
  done
  echo "source $(sha256sum "$here/harness.cpp" | cut -d' ' -f1) harness.cpp"
  echo "compiler $(g++ --version | head -1)"
  while IFS= read -r lib; do
    echo "archive $(sha256sum "$lib" | cut -d' ' -f1) ${lib#"$build"/}"
  done <<< "$archives"
  echo "binary $(sha256sum "$bin" | cut -d' ' -f1)"
} > "$out/MANIFEST"
log "$bin ($(awk '$1 == "binary" { print $2 }' "$out/MANIFEST"))"
