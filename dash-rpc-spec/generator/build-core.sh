#!/usr/bin/env bash
# Builds dashd from a fresh `git archive` of a Dash Core tag, with the wallet and ZMQ as in
# release builds, for `generate.py build-dump`. A native build through depends (about 30 min at
# 12 jobs with the depends sources cached). Needs only POSIX tools, Bash 3.2+, git and Core's own
# build requirements; set MAKE=gmake where GNU make is not `make` (BSDs).
#
# Usage: build-core.sh <dash-core-git-dir> <tag> <out-dir> [depends-sources-dir]
set -euxo pipefail

git_dir="$(cd "$1" && pwd)"
tag="$2"
mkdir -p "$3"
out="$(cd "$3" && pwd)"
sources="${4:-$out/depends-sources}"
jobs="${CARGO_BUILD_JOBS:-12}"
make="${MAKE:-make}"
[ ! -e "$out/core" ] || { echo "$out/core exists; use a fresh directory" >&2; exit 1; }

mkdir -p "$out/core" "$sources"
git -C "$git_dir" archive "$tag" | tar -x -C "$out/core"
git -C "$git_dir" rev-parse "$tag^{commit}" > "$out/core.commit"
cd "$out/core"
"$make" -C depends -j"$jobs" NO_QT=1 NO_UPNP=1 NO_NATPMP=1 SOURCES_PATH="$sources"
# depends installs into depends/<host triplet>; ask it which (honours HOST= like the build).
host="$("$make" -s -C depends print-host | sed -n 's/^host=//p')"
[ -f "depends/$host/share/config.site" ] || { echo "no depends/$host/share/config.site" >&2; exit 1; }
./autogen.sh
CONFIG_SITE="$PWD/depends/$host/share/config.site" ./configure \
    --without-gui --disable-tests --disable-bench --disable-fuzz-binary --disable-man \
    --enable-wallet --without-miniupnpc --without-natpmp
"$make" -C src -j"$jobs" dashd
echo "built $out/core for $host (commit $(cat "$out/core.commit"))"
