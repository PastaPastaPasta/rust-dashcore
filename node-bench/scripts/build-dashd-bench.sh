#!/bin/bash
#
# Build Dash Core's microbenchmark binary (bench_dash) for a tag, using
# depends inside a pinned Debian container so the toolchain is reproducible.
# Release tarballs do not ship bench_dash, so this is the only way to get it.
#
#   scripts/build-dashd-bench.sh [tag] [jobs]       (default: v23.1.8, 16)
#
# Source: `git archive` of <tag> from $DASH_REPO (default ~/workspace/dash),
# so the shared checkout is never touched. Output:
#   $NODE_BENCH_ROOT/bin/bench_dash-<tag>  and  .../src/dash-<tag>/build.log
#   $NODE_BENCH_ROOT/bin/bench_dash-<tag>.provenance  (commit, image digest, compiler, apt packages)
# The Debian tag and apt packages are not pinned (the tag moves); the provenance
# file records what a build actually used.
set -euo pipefail

TAG="${1:-v23.1.8}"
JOBS="${2:-16}"
ROOT="${NODE_BENCH_ROOT:-/work/bench}"
DASH_REPO="${DASH_REPO:-${HOME}/workspace/dash}"
IMAGE="debian:bookworm"
SRC="${ROOT}/src/dash-${TAG}"
OUT="${ROOT}/bin/bench_dash-${TAG}"

git -C "${DASH_REPO}" rev-parse -q --verify "${TAG}^{commit}" >/dev/null \
  || git -C "${DASH_REPO}" fetch --tags origin
COMMIT="$(git -C "${DASH_REPO}" rev-parse "${TAG}^{commit}")"

if [ -f "${SRC}/COMMIT" ] && [ "$(cat "${SRC}/COMMIT")" != "${COMMIT}" ]; then
  echo "${SRC} holds $(cat "${SRC}/COMMIT"), but ${TAG} is now ${COMMIT}: remove ${SRC} to rebuild" >&2
  exit 1
fi
if [ ! -f "${SRC}/configure.ac" ]; then
  mkdir -p "${SRC}"
  git -C "${DASH_REPO}" archive "${COMMIT}" | tar -x -C "${SRC}"
  echo "${COMMIT}" >"${SRC}/COMMIT"
fi

# Inside the container: toolchain, depends (no Qt), configure with benchmarks.
docker run --rm -v "${SRC}:/src" -w /src -e JOBS="${JOBS}" -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  "${IMAGE}" bash -ec '
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
      build-essential libtool autotools-dev automake pkg-config bsdmainutils python3 bison \
      curl ca-certificates cmake patch xz-utils >/dev/null
    make -C depends -j"${JOBS}" NO_QT=1 NO_UPNP=1 NO_NATPMP=1 NO_ZMQ=1 >/dev/null
    HOST=$(depends/config.guess)
    ./autogen.sh >/dev/null
    CONFIG_SITE="$PWD/depends/${HOST}/share/config.site" ./configure --enable-bench --disable-gui \
      --disable-fuzz-binary --without-gui --disable-man >/dev/null
    make -j"${JOBS}" -C src bench/bench_dash
    { gcc --version | head -n 1; dpkg-query -W -f "\${Package}=\${Version}\n" build-essential gcc g++ libc6-dev \
        cmake python3; } >/src/TOOLCHAIN
    chown -R "${HOST_UID}:${HOST_GID}" /src
  ' 2>&1 | tee "${SRC}/build.log"

install -m 755 "${SRC}/src/bench/bench_dash" "${OUT}"
{ echo "tag ${TAG} commit ${COMMIT}"
  echo "image ${IMAGE} $(docker image inspect --format '{{index .RepoDigests 0}}' "${IMAGE}")"
  cat "${SRC}/TOOLCHAIN"
  echo "sha256 $(sha256sum "${OUT}" | cut -d' ' -f1)"; } >"${OUT}.provenance"
echo "built ${OUT} (${TAG} @ ${COMMIT}); provenance in ${OUT}.provenance"
