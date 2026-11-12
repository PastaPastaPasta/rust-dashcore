#!/usr/bin/env bash
# KGW's floating point in dash-consensus (CS2 §2.4, §6.2), in a release build:
#   - with glibc's default `pow` ifunc, the variant this CPU selects: quirk 40's proof, whose
#     host-libm test measures that variant against its fixture, and all 16,076 KGW boundary
#     windows against Core's recorded answers (a debug `cargo test` replays about 1,200);
#   - with GLIBC_TUNABLES masking FMA, AVX2 and FMA4, which leaves glibc's SSE2 variant: quirk 40
#     again. The windows do not depend on the host's `pow` (the port reads its table, and Core's
#     answers are recorded); `contrib/node/cs2-diff/run.sh kgw` reruns Core's KGW under SSE2.
# Each run asserts the variant quirk 40's host-libm test measured: on x86_64 glibc selects FMA when
# the CPU has FMA and AVX2, else FMA4 when it has that (no fixture: only the proof's 10⁻⁹ interval
# covers it), else SSE2. contrib/node/check-no-libm.sh, a gate step of its own, checks that the
# compiled crate calls no libm at all.
#
# Usage: contrib/node/check-kgw-fp.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
if [ "$(uname -s)-$(uname -m)" != Linux-x86_64 ] || ! getconf GNU_LIBC_VERSION > /dev/null 2>&1; then
    echo "check-kgw-fp.sh: needs x86_64 Linux with glibc (its pow ifuncs are what it checks)" >&2
    exit 2
fi
flags=" $(grep -m1 '^flags' /proc/cpuinfo | cut -d: -f2) "
has() { [[ "$flags" == *" $1 "* ]]; }
if has fma && has avx2; then
    default=glibc-fma
elif has fma4; then
    default="the FMA4 variant (no fixture)"
else
    default=glibc-sse2
fi

quirk=(--test quirk_40_kgw_fp_platform_invariance)
cargo test --locked --release -p dash-consensus "${quirk[@]}" --test kgw_boundaries --no-run

# run <expected variant> <cargo test args..>
run() {
    local expected="$1" out measured
    shift
    out="$(cargo test --locked --release -p dash-consensus "$@" -- --nocapture 2>&1)" ||
        { printf '%s\n' "$out" >&2; echo "check-kgw-fp.sh: failed (${GLIBC_TUNABLES:-default ifunc})" >&2; exit 1; }
    measured="$(grep -m1 -o 'host glibc pow: .*' <<< "$out" || true)"
    if [ "$measured" != "host glibc pow: $expected" ]; then
        echo "check-kgw-fp.sh: expected $expected under ${GLIBC_TUNABLES:-the default ifunc}, measured '${measured:-nothing}'" >&2
        exit 1
    fi
    echo "${GLIBC_TUNABLES:-default ifunc}: $measured"
}
# the default run must not inherit a caller's tunables
unset GLIBC_TUNABLES
run "$default" "${quirk[@]}" --test kgw_boundaries
GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA,-FMA4 run glibc-sse2 "${quirk[@]}"
if [ "$default" = glibc-sse2 ]; then
    echo "note: this CPU cannot select glibc's FMA pow; only quirk 40's proof covers that table here"
fi
