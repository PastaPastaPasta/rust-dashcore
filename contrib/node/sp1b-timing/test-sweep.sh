#!/usr/bin/env bash
# nightly.sh's old-root sweep (SP1-notes D22, DEC-584) in fake layouts: it runs only with
# SP1B_SWEEP_TMP=1 and a TMPDIR strictly below the working directory, removes only mkdtemp-named
# roots (sp1b-XXXXXXXX) but the newest, and no name, a newline in it or not, reaches a path
# outside TMPDIR. nightly.sh stops after the sweep: SP1B_MAX_LOAD=0 skips the night (exit 2).
#
# usage: contrib/node/sp1b-timing/test-sweep.sh  (exits 0 when every case holds)
set -u
NIGHTLY=$(cd "$(dirname "$0")" && pwd)/nightly.sh
W=$(mktemp -d "${TMPDIR:-/tmp}/sp1b-sweep-test.XXXXXX")
trap 'rm -rf -- "$W"' EXIT
fail=0

# A fresh layout: $1/tmp with two old roots, the newest root, look-alikes, and a victim outside.
layout() {
  rm -rf -- "$1" && mkdir -p "$1/tmp" "$1/victim"
  mkdir "$1/tmp/sp1b-old11111" "$1/tmp/sp1b-old22222" "$1/tmp/sp1b-new33333" \
    "$1/tmp/sp1b-review" "$1/tmp/other" "$1/tmp/sp1b-x"$'\n'"victim" "$1/tmp/sp1b-abcdefgh"$'\n'
  touch -d '3 days ago' "$1/tmp/sp1b-old11111"
  touch -d '2 days ago' "$1/tmp/sp1b-old22222" "$1/tmp/sp1b-abcdefgh"$'\n'
  touch -d '1 day ago' "$1/tmp/sp1b-x"$'\n'"victim" "$1/tmp/sp1b-review"
}

# $1 case name, $2 cwd, $3 TMPDIR, $4 SP1B_SWEEP_TMP, $5 the expected listing of $W/case/tmp.
check() {
  local out
  (cd "$2" && TMPDIR=$3 SP1B_SWEEP_TMP=$4 SP1B_MAX_LOAD=0 SP1B_LOAD_WAIT_S=0 \
    "$NIGHTLY" "$W/out-$1" > "$W/log-$1" 2>&1)
  out=$(cd "$W/case/tmp" && find . -mindepth 1 -maxdepth 1 -printf '%P|' | tr '\n' '?' |
    tr '|' '\n' | sort | tr '\n' ' ')
  [ -d "$W/case/victim" ] || out="$out[victim gone]"
  if [ "$out" = "$5" ]; then
    echo "ok   $1"
  else
    echo "FAIL $1: left [$out], want [$5]"; sed 's/^/     /' "$W/log-$1"; fail=1
  fi
}

ALL="other sp1b-abcdefgh? sp1b-new33333 sp1b-old11111 sp1b-old22222 sp1b-review sp1b-x?victim "
SWEPT="other sp1b-abcdefgh? sp1b-new33333 sp1b-review sp1b-x?victim "

layout "$W/case"; check opt-in-unset "$W/case" "$W/case/tmp" "" "$ALL"
layout "$W/case"; check cwd-is-tmpdir "$W/case/tmp" "$W/case/tmp" 1 "$ALL"
layout "$W/case"; check tmpdir-not-below-cwd "$W/case/victim" "$W/case/tmp" 1 "$ALL"
layout "$W/case"; check relative-tmpdir-swept "$W/case" tmp 1 "$SWEPT"
layout "$W/case"; check unit-layout-swept "$W/case" "$W/case/tmp" 1 "$SWEPT"
grep -h 'removing\|not sweeping' "$W"/log-* | sed "s|$W|\$W|g; s/^/     /"
exit $fail
