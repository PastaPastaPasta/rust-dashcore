#!/bin/bash
#
# PF1: build dashd from a Dash Core tag with symbols and frame pointers, for
# perf call graphs. Same recipe as node-bench's scripts/build-dashd-bench.sh
# (depends inside debian:bookworm), but depends is rebuilt with
# -fno-omit-frame-pointer and the release flags are -O2 -g.
#
#   build-dashd-symbols.sh [tag] [jobs] [out-dir]
#
# Default tag v24.0.0-rc.3, jobs 8, out-dir /work/scratch/pf1.
# Output: <out>/bin/dashd-<tag>-sym and a .provenance file.
# The container is limited with --cpus=<jobs> (docker containers do not live
# under a `systemd-run --user` scope, so CPUQuota cannot cap them).
set -euo pipefail

TAG="${1:-v24.0.0-rc.3}"
JOBS="${2:-8}"
OUT="${3:-/work/scratch/pf1}"
DASH_REPO="${DASH_REPO:-${HOME}/workspace/dash}"
SRC_SEED="${PF1_DEPENDS_SOURCES:-}"   # optional: a depends/sources dir to reuse
IMAGE="debian:bookworm"
SRC="${OUT}/src/dash-${TAG}"
FP="-O2 -g -fno-omit-frame-pointer -mno-omit-leaf-frame-pointer"

COMMIT="$(git -C "${DASH_REPO}" rev-parse "${TAG}^{commit}")"
if [ ! -f "${SRC}/configure.ac" ]; then
  mkdir -p "${SRC}"
  git -C "${DASH_REPO}" archive "${COMMIT}" | tar -x -C "${SRC}"
  echo "${COMMIT}" >"${SRC}/COMMIT"
  if [ -n "${SRC_SEED}" ] && [ -d "${SRC_SEED}" ]; then
    mkdir -p "${SRC}/depends/sources"
    cp -a --reflink=auto "${SRC_SEED}/." "${SRC}/depends/sources/"
  fi
fi

docker run --rm --cpus="${JOBS}" -v "${SRC}:/src" -w /src -e JOBS="${JOBS}" -e FP="${FP}" \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" "${IMAGE}" bash -ec '
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      build-essential libtool autotools-dev automake pkg-config bsdmainutils python3 bison \
      curl ca-certificates cmake patch xz-utils >/dev/null
    make -C depends -j"${JOBS}" NO_QT=1 NO_UPNP=1 NO_NATPMP=1 NO_ZMQ=1 \
      linux_release_CFLAGS="${FP}" linux_release_CXXFLAGS="${FP}" >/dev/null
    HOST=$(depends/config.guess)
    ./autogen.sh >/dev/null
    CONFIG_SITE="$PWD/depends/${HOST}/share/config.site" ./configure --disable-gui --without-gui \
      --disable-fuzz-binary --disable-man --disable-bench --disable-tests --enable-reduce-exports \
      --enable-debug=no CFLAGS="${FP}" CXXFLAGS="${FP}" >/dev/null
    make -j"${JOBS}" -C src dashd dash-cli
    { gcc --version | head -n 1; dpkg-query -W -f "\${Package}=\${Version}\n" build-essential gcc g++ libc6-dev \
        cmake python3; } >/src/TOOLCHAIN
    chown -R "${HOST_UID}:${HOST_GID}" /src
  ' 2>&1 | tee "${SRC}/build.log"

mkdir -p "${OUT}/bin"
install -m 755 "${SRC}/src/dashd" "${OUT}/bin/dashd-${TAG}-sym"
{ echo "tag ${TAG} commit ${COMMIT}"
  echo "image ${IMAGE} $(docker image inspect --format '{{index .RepoDigests 0}}' "${IMAGE}")"
  echo "flags ${FP} (depends, CFLAGS, CXXFLAGS); configure --enable-debug=no --enable-reduce-exports --disable-bench --disable-tests"
  cat "${SRC}/TOOLCHAIN"
  echo "sha256 $(sha256sum "${OUT}/bin/dashd-${TAG}-sym" | cut -d' ' -f1)"; } >"${OUT}/bin/dashd-${TAG}-sym.provenance"
echo "built ${OUT}/bin/dashd-${TAG}-sym"
