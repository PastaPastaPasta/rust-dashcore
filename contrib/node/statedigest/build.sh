#!/usr/bin/env bash
# Build dashd (and test_dash, dash-cli, dash-wallet) from a Dash Core checkout for
# StateDigest recordings (O8 spec §11, §13): the stock v24.0.0-rc.3 baseline and the
# patched `node/state-digest-v24.0` branch, with the same flags.
#
# The depends tree comes from the oracle's shared cache, under the same locks and in
# the same order as contrib/node/build-oracle.sh (download lock, then the per-tree
# depends lock; BASE_CACHE keyed by the depends/ tree hash). Never run depends
# outside them: concurrent extraction once produced libraries missing symbols.
# Unlike the oracle archive, this build is not reproducible: it uses ccache and the
# wallet (functional tests need it). Provenance records the commit, flags and hashes.
#
# Usage: contrib/node/statedigest/build.sh --src <core checkout> --build <dir> --out <dir>
#   --src DIR     Dash Core checkout (a worktree; its depends/ must be committed)
#   --build DIR   build tree; the checkout is rsynced into it (mtimes kept, so
#                 rebuilds are incremental)
#   --out DIR     where the binaries and build-info.txt go
#   --cache DIR   oracle cache root (default /work/cache/oracle)
#   --jobs N      make jobs (default ${CARGO_BUILD_JOBS:-12})
set -euo pipefail

src= build= out= cache=/work/cache/oracle jobs="${CARGO_BUILD_JOBS:-12}"
while [ $# -gt 0 ]; do
    case "$1" in
        --src) src="$2"; shift 2 ;;
        --build) build="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        --cache) cache="$2"; shift 2 ;;
        --jobs) jobs="$2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
[ -n "${src}" ] && [ -n "${build}" ] && [ -n "${out}" ] || { echo "need --src, --build and --out" >&2; exit 2; }
src="$(cd "${src}" && pwd)"

if [ -n "$(git -C "${src}" status --porcelain -- depends)" ]; then
    echo "${src}: depends/ has uncommitted changes; the depends cache is keyed by its tree hash" >&2
    exit 1
fi
commit="$(git -C "${src}" rev-parse HEAD)"
branch="$(git -C "${src}" rev-parse --abbrev-ref HEAD)"
dirty="$(git -C "${src}" status --porcelain --untracked-files=no | wc -l)"
depends_id="$(git -C "${src}" rev-parse HEAD:depends)"
sources="${cache}/depends-sources"
depends_cache="${cache}/depends-cache/${depends_id}"
mkdir -p "${build}" "${out}" "${sources}" "${depends_cache}" "${cache}/locks"
build="$(realpath "${build}")"
# A configured tree copied from elsewhere keeps that tree's absolute paths (test/config.ini,
# obj/build.h, the depends prefix), so its tests and version would come from the original.
if [ -f "${build}/config.status" ] && ! grep -qx "abs_top_builddir = ${build}" "${build}/Makefile"; then
    echo "${build} was configured in another directory; use a fresh build tree" >&2
    exit 1
fi

# Only tracked files, so that nothing a checkout built in place (config.status, Makefiles,
# objects) or left untracked reaches the tree; the provenance's commit and modified_files
# then describe everything copied. No --delete: autogen, configure and make write into the
# tree. A patch series only adds and edits files, and -a keeps the checkout's mtimes, so
# make rebuilds exactly what changed.
git -C "${src}" ls-files -z | rsync -a --from0 --files-from=- "${src}/" "${build}/"
# The tree has no .git, so hand share/genbuild.sh the checkout's description: the
# binaries then report it (FormatFullVersion, the H records' "ver") instead of "-unk".
describe="$(git -C "${src}" describe --abbrev=12 --dirty)"
sed -i "s|^GIT_DESCRIPTION=\"\"\$|GIT_DESCRIPTION=\"${describe}\"|" "${build}/share/genbuild.sh"
grep -q "^GIT_DESCRIPTION=\"${describe}\"\$" "${build}/share/genbuild.sh" || { echo "could not set the build description" >&2; exit 1; }

cd "${build}"
host="$(depends/config.guess)"
flags="-O2 -g1"
flock "${sources}/.download.lock" flock "${cache}/locks/depends-${depends_id}.lock" \
    make -C depends -j"${jobs}" HOST="${host}" NO_QT=1 NO_ZMQ=1 NO_UPNP=1 NO_NATPMP=1 NO_USDT=1 \
    SOURCES_PATH="${sources}" BASE_CACHE="${depends_cache}"
if [ ! -f config.status ]; then
    ./autogen.sh
    CONFIG_SITE="${build}/depends/${host}/share/config.site" CFLAGS="${flags}" CXXFLAGS="${flags}" \
        ./configure --prefix=/ --enable-ccache --without-gui --disable-zmq --without-miniupnpc \
        --without-natpmp --disable-bench --disable-fuzz-binary --disable-man
fi
make -j"${jobs}"

bins=(src/dashd src/dash-cli src/dash-tx src/dash-wallet src/test/test_dash)
cp -f "${bins[@]}" "${out}/"
{
    echo "commit ${commit}"
    echo "branch ${branch}"
    echo "describe ${describe}"
    echo "modified_files ${dirty}"
    echo "depends_id ${depends_id}"
    echo "host ${host}"
    echo "flags ${flags}"
    echo "configure --enable-ccache --without-gui --disable-zmq --without-miniupnpc --without-natpmp --disable-bench --disable-fuzz-binary --disable-man"
    echo "depends NO_QT=1 NO_ZMQ=1 NO_UPNP=1 NO_NATPMP=1 NO_USDT=1"
    echo "compiler $(${CXX:-g++} --version | head -1)"
    for b in "${bins[@]}"; do echo "sha256 $(sha256sum "${out}/$(basename "${b}")" | cut -d' ' -f1) $(basename "${b}")"; done
} > "${out}/build-info.txt"
cat "${out}/build-info.txt"
