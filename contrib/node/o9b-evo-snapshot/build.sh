#!/usr/bin/env bash
# Build test_dash_o9b: Dash Core v24.0.0-rc.4's own test_dash with one more test file,
# evo_snapshot_vectors.cpp, which dumps Core's EvoSnapshot vectors and judges crafted inputs
# (O9b S6a; contrib/node/bs5-chainparams's provenance rules).
#
# Stage 1, the tree: a fresh `git archive` of the tag's commit, every tracked file verified
# against the commit's blobs, depends built with no cache from an earlier run, configured in
# place (never a copied tree: autotools writes absolute paths), and `make -C src test/test_dash`
# with V=1. The tracked files are verified again, and the tree's anchor (.o9b-anchor: the sha256
# of every file and the target of every symlink) is written. O9B_REUSE=1 reuses an anchored tree
# instead, for development: it must still equal the commit and its anchor, or it is refused.
#
# Stage 2, the binary: evo_snapshot_vectors.cpp compiled with the exact command that compiled
# test/test_dash-evo_snapshot_tests.o, and linked with the exact command that linked
# test/test_dash, plus that one object, into <out>/test_dash_o9b. The tree is not modified.
# <out>/MANIFEST records the commit, its tree, the build mode, the anchor's sha256, the vectors
# source's sha256, the compiler, and the binary's sha256.
#
# usage: build.sh <out-dir>
#   DASH_SRC      Dash Core repository (default ~/workspace/dash)
#   O9B_BUILD     build root (default /work/scratch/o9b-s6a/core-build)
#   O9B_SOURCES   depends download directory (default /work/scratch/o9b-s6a/depends-sources)
#   O9B_REUSE     1: reuse the anchored tree (default: a fresh build)
#   JOBS          parallel jobs (default 8)
set -euo pipefail

out=${1:?usage: build.sh <out-dir>}
here=$(cd "$(dirname "$0")" && pwd)
tag=v24.0.0-rc.4
dash_src=${DASH_SRC:-$HOME/workspace/dash}
root=${O9B_BUILD:-/work/scratch/o9b-s6a/core-build}
sources=${O9B_SOURCES:-/work/scratch/o9b-s6a/depends-sources}
jobs=${JOBS:-8}
reuse=${O9B_REUSE:-0}

log() { echo "[o9b-evo-build] $*" >&2; }
commit=$(git -C "$dash_src" rev-parse --verify "$tag^{commit}")
tree_id=$(git -C "$dash_src" rev-parse "$commit^{tree}")
build=$root/$commit
mkdir -p "$out" "$sources"
out=$(cd "$out" && pwd)
scratch=$(mktemp -d "${TMPDIR:-/work/tmp}/o9b-evo-build.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

# Every tracked file of the commit must be present with the commit's blob id; a file with the
# export-subst attribute (src/clientversion.cpp) must equal what `git archive` makes of it.
verify_tree() {
  local paths=$scratch/paths f sha want got
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
  if [ "$want" != "$got" ]; then
    log "$build differs from $commit:"
    diff <(echo "$want") <(echo "$got") | head -20 >&2
    return 1
  fi
}

tree_digest() {
  (
    cd "$build"
    find . -path ./.o9b-anchor -prune -o -type f -print0 | xargs -0 -r -P "$jobs" -n 512 sha256sum |
      LC_ALL=C sort -k2
    find . -type l -printf 'link %p -> %l\n' | LC_ALL=C sort
  )
}

while IFS= read -r var; do unset "$var"; done < <(env | sed -n 's/^\(CCACHE_[A-Z_]*\)=.*/\1/p')
export CCACHE_DISABLE=1 LC_ALL=C TZ=UTC
unset CFLAGS CXXFLAGS CPPFLAGS LDFLAGS MAKEFLAGS

if [ "$reuse" = 1 ] && [ -f "$build/.o9b-anchor" ]; then
  log "reusing $build (O9B_REUSE=1); checking it against its anchor"
  verify_tree
  if ! tree_digest | cmp -s - "$build/.o9b-anchor"; then
    log "$build differs from the anchor its clean build wrote:"
    diff "$build/.o9b-anchor" <(tree_digest) | head -20 >&2
    exit 1
  fi
  mode=reused
else
  log "fresh clean build of $tag ($commit) in $build"
  if [ -e "$build" ]; then
    log "$build exists: remove it first (rm-if-idle), the script deletes nothing"
    exit 1
  fi
  mkdir -p "$build"
  git -C "$dash_src" archive --format=tar "$commit" | tar -x -C "$build"
  verify_tree
  host=$("$build/depends/config.guess")
  (
    cd "$build"
    flock "$sources/.download.lock" make -C depends -j"$jobs" HOST="$host" NO_QT=1 NO_WALLET=1 NO_ZMQ=1 \
      NO_UPNP=1 NO_NATPMP=1 NO_USDT=1 SOURCES_PATH="$sources"
    ./autogen.sh
    CONFIG_SITE="$build/depends/$host/share/config.site" CC=gcc CXX=g++ \
      ./configure --prefix=/ --disable-ccache --disable-wallet --without-gui --disable-zmq \
      --without-miniupnpc --without-natpmp --disable-bench --disable-fuzz-binary --disable-man
    make -C src -j"$jobs" V=1 test/test_dash
  ) > "$root/build.log" 2>&1 || { log "build failed, see $root/build.log"; exit 1; }
  verify_tree
  tree_digest > "$build/.o9b-anchor"
  chmod a-w "$build/.o9b-anchor"
  mode=fresh
fi

# The commands make ran (V=1), from the build log: the compile of evo_snapshot_tests.o and the
# link of test_dash. Both run in src/.
compile=$(grep -E -- '-o test/test_dash-evo_snapshot_tests\.o( |$)' "$root/build.log" | tail -1)
link=$(grep -E -- '--mode=link .* -o test/test_dash( |$)' "$root/build.log" | tail -1)
[ -n "$compile" ] && [ -n "$link" ] || { log "no compile or link line for test_dash in $root/build.log"; exit 1; }
obj=$scratch/evo_snapshot_vectors.o
vectors_compile=${compile//test\/test_dash-evo_snapshot_tests.o/$obj}
vectors_compile=${vectors_compile//test\/evo_snapshot_tests.cpp/$here/evo_snapshot_vectors.cpp}
# Automake's dependency tracking writes beside the object: keep it in scratch.
vectors_compile=${vectors_compile//test\/.deps\/test_dash-evo_snapshot_tests.Tpo/$scratch/vectors.Tpo}
vectors_compile=${vectors_compile//test\/.deps\/test_dash-evo_snapshot_tests.Po/$scratch/vectors.Po}
bin=$out/test_dash_o9b
vectors_link=${link/ -o test\/test_dash / -o $scratch/test_dash_o9b $obj }
(
  cd "$build/src"
  eval "$vectors_compile"
  eval "$vectors_link"
) > "$scratch/link.log" 2>&1 || { log "the vectors build failed:"; tail -30 "$scratch/link.log" >&2; exit 1; }
# libtool may leave a wrapper script; take the real binary when it does.
real=$scratch/test_dash_o9b
[ -x "$scratch/.libs/test_dash_o9b" ] && real=$scratch/.libs/test_dash_o9b
install -m 0555 "$real" "$bin"
verify_tree

{
  echo "tag $tag"
  echo "commit $commit"
  echo "tree $tree_id"
  echo "build $mode"
  echo "anchor $(sha256sum "$build/.o9b-anchor" | cut -d' ' -f1)"
  echo "source $(sha256sum "$here/evo_snapshot_vectors.cpp" | cut -d' ' -f1) evo_snapshot_vectors.cpp"
  echo "core-test $(git -C "$dash_src" rev-parse "$commit:src/test/evo_snapshot_tests.cpp") src/test/evo_snapshot_tests.cpp"
  echo "compiler $(g++ --version | head -1)"
  echo "binary $(sha256sum "$bin" | cut -d' ' -f1)"
} > "$out/MANIFEST"
log "$bin ($(awk '$1 == "binary" { print $2 }' "$out/MANIFEST"), build $mode)"
