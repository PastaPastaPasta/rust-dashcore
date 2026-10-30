#!/usr/bin/env bash
# The mainnet suffix check (CH3 R11; CH3a D165-D167): `digest l2` starts V as a pruned node at
# 599,999 from the installed UTXO snapshot, which it checks against golden(599,999) first, runs
# 600,000..=625,000, checks the log's MuHash at 625,000 and 614,000 against the golden, and
# reverses to 614,000. `digest compare` then checks the forward log against the golden over
# 600,000..=625,000 on every key but EXCLUDE.
#
# Usage: conformance/scripts/suffix-check.sh <dash-conformance> <out-dir> [name] [cache]
#   name defaults to suffix, cache to /work/cache/statedigest; $DASH_EMBARGO_DIR must be set
#   (mainnet compares need it even with --no-dump).
# Writes <out-dir>/<name>.jsonl (with its reverse log and manifest), <name>.log and
# <name>.compare.log, all new files; each log ends with "EXIT <status>". Exit status: 0 passed;
# 2 an input does not match its SHA256SUMS; 1 anything else: a usage error, no <out-dir>, a log
# that exists, the run failed, the compare did not exit 4 (clean except EXCLUDE), or its listing
# does not show the excluded keys held as expected.
set -uo pipefail
# Each status comes from its command, never from a log: a log another run left is never read.
set -o noclobber

# Fixed here, not an argument (D167): `muhash`, which the log holds at 625,000 and 614,000 only,
# where the run checks it; and the four evo keys V writes only from CH7, which then fails this
# check until it drops them.
readonly EXCLUDE=muhash,mnl,qc,cp,ehf
readonly REQUIRE=undo,stats,bi,txouts,amount,unspendable,bls,bur,dep

if [ $# -lt 2 ] || [ $# -gt 4 ] || [ -z "${DASH_EMBARGO_DIR:-}" ]; then
    echo "usage: DASH_EMBARGO_DIR=<dir> $0 <dash-conformance> <out-dir> [name] [cache]" >&2
    exit 1
fi
bin=$1 out=$2 name=${3:-suffix} cache=${4:-/work/cache/statedigest}
if [ ! -d "$out" ]; then
    echo "suffix check: $out is not a directory"
    exit 1
fi
log=$out/$name.log cmp=$out/$name.compare.log
for f in "$log" "$cmp"; do
    if [ -e "$f" ] || [ -L "$f" ]; then
        echo "suffix check: $f exists: the check writes only new files"
        exit 1
    fi
done
# The installed inputs, each named by its content (D162, D163, D168, D169).
headers=$cache/mainnet-headers-c0491276f1cf610c44c997fd366810ecac42e5dde9e2b3d443c3f01defce5309
golden=$cache/mainnet-db74a6338b6cfaa9be7105a218bf938cdc3bc7c851159d927b6d34976f5c44fa
blocks=$cache/mainnet-blocks-1ae864be4e82b7ede7fbbb8cd13ec0c417ec1424dd2582fcc8d99aa2802bbcf7
utxo=$cache/mainnet-utxo-599999-30d71cf400db18d3a41e3871959362746d199575e8e00bb383ed1518915cb03e

# The golden's SHA256SUMS also names the uncompressed golden, which is not stored.
(cd "$headers" && sha256sum --strict --quiet -c SHA256SUMS) &&
    (cd "$golden" && grep -v ' (uncompressed, not stored)$' SHA256SUMS |
        sha256sum --strict --quiet -c -) &&
    (cd "$blocks/blocks" && sha256sum --strict --quiet -c ../SHA256SUMS) &&
    (cd "$utxo" && sha256sum --strict --quiet -c SHA256SUMS) || exit 2

(
    echo "start $(date -u +%FT%TZ) load $(cut -d' ' -f1-3 /proc/loadavg); $bin"
    /usr/bin/time -v "$bin" digest l2 --chain main --blocks-dir "$blocks/blocks" \
        --out "$out/$name.jsonl" --to 625000 --assumevalid on --headers "$headers/mainnet-headers.bin" \
        --par 16 --golden "$golden/golden.jsonl.zst" --reverse-to 614000 \
        --start-utxo "$utxo/utxo-599999.dat"
    status=$?
    echo "end $(date -u +%FT%TZ) load $(cut -d' ' -f1-3 /proc/loadavg)"
    echo "EXIT $status"
    exit "$status"
) > "$log" 2>&1
status=$?
[ "$status" = 0 ] || { echo "suffix check: digest l2 exited $status, see $log"; exit 1; }

(
    /usr/bin/time -f "wall %e s, user %U s, max RSS %M KiB" "$bin" digest compare --mode linear \
        --no-dump --require "$REQUIRE" --exclude "$EXCLUDE" --from 600000 --to 625000 \
        "$out/$name.jsonl" "$golden/golden.jsonl.zst"
    status=$?
    echo "EXIT $status"
    exit "$status"
) > "$cmp" 2>&1
status=$?
[ "$status" = 4 ] || { echo "suffix check: the compare exited $status, not 4, see $cmp"; exit 1; }

# Our side must hold the excluded key $1 at exactly $2 pairs, by the compare's listing; $3 is
# what any other count means.
held() {
    local n
    n=$(awk -v k="C.s.$1" '/^excluded, not compared/ { x = 1; next } x && $1 == k { sub(",", "", $2); print $2 }' "$cmp")
    [ -n "$n" ] || { echo "suffix check: the compare lists no $1, see $cmp"; exit 1; }
    [ "$n" = "$2" ] || { echo "suffix check: the log holds $1 at $n pairs, not $2: $3"; exit 1; }
}
held muhash 2 "the run finalizes it at 614,000 and 625,000 only"
for key in mnl qc cp ehf; do
    held "$key" 0 "V writes it (CH7): drop it from EXCLUDE"
done
echo "suffix check: passed ($log, $cmp)"
