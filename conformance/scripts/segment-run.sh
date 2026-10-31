#!/usr/bin/env bash
# Segment windows (O9b S4; R4 §2.3.4, A1 §4.6): each window `segment select` printed as
# `RUN <id> <chain> <base> <to>` on stdin runs `digest l2` as a pruned start from its bundle
# `<bundles>/<id>` (`digest l2` checks the loaded coins against golden(base)), base+1..=to with
# assumevalid on, a reverse leg back to the base, and `digest compare` of the forward log against
# the golden over the window on every key but EXCLUDE. The input must be a whole `segment select`
# output, ending in its `BUDGET` line; other lines are ignored. Every bundle is opened first
# (`segment open`: exactly the bundle's files, matching their SHA256SUMS, the manifest matching
# the sidecar, and the sidecar's chain and base the RUN line's); then the windows run one at a
# time, in order.
#
# Usage: <selection> | conformance/scripts/segment-run.sh <dash-conformance> <out-dir> \
#            <blocks-dir> <headers-file> <golden> <bundles-dir>
#   $DASH_EMBARGO_DIR must be set (mainnet and testnet compares need it even with --no-dump).
#   The inputs are one chain's (every RUN line must name it) and the caller's, installed
#   read-only with their own checksums.
# Writes <out-dir>/<id>.jsonl (with its reverse log and manifest), <id>.log and <id>.compare.log,
# all new files; each log ends with "EXIT <status>", and a line `WINDOW <id> <status> <seconds>`
# goes to stdout per window. Exit status: 0 every window passed (a selection with no RUN line
# passes); 2 a bundle failed to open, before any window ran; 1 anything else.
set -uo pipefail
set -o noclobber

# As the suffix check's (CH3a D167): `muhash` is finalized at the window's end only, where
# `digest l2` checks it; the evo keys V writes only from CH7, and the log must hold them at 0
# pairs until they leave this list.
readonly EXCLUDE=muhash,mnl,qc,cp,ehf
readonly EVO_KEYS="mnl qc cp ehf"
readonly REQUIRE=undo,stats,bi,txouts,amount,unspendable,bls,bur,dep

if [ $# -ne 6 ] || [ -z "${DASH_EMBARGO_DIR:-}" ]; then
    echo "usage: DASH_EMBARGO_DIR=<dir> $0 <dash-conformance> <out-dir> <blocks-dir> <headers-file> <golden> <bundles-dir>" >&2
    exit 1
fi
bin=$1 out=$2 blocks=$3 headers=$4 golden=$5 bundles=$6
[ -d "$out" ] || { echo "segment run: $out is not a directory"; exit 1; }

# The selection, whole.
runs=() last= one_chain=
while read -r verb id chain base to rest; do
    [ -n "$verb" ] && last=$verb
    [ "$verb" = RUN ] || continue
    if [ -n "$rest" ] || ! [[ "$base" =~ ^[0-9]+$ && "$to" =~ ^[0-9]+$ ]]; then
        echo "segment run: a malformed RUN line for $id"
        exit 1
    fi
    : "${one_chain:=$chain}"
    if [ "$chain" != "$one_chain" ]; then
        echo "segment run: window $id is on $chain; one run takes one chain's inputs"
        exit 1
    fi
    runs+=("$id $chain $base $to")
done
[ "$last" = BUDGET ] || { echo "segment run: the input does not end in segment select's BUDGET line"; exit 1; }

# Every bundle and log name before any window runs.
for run in "${runs[@]}"; do
    read -r id chain base to <<< "$run"
    for f in "$out/$id.log" "$out/$id.compare.log"; do
        if [ -e "$f" ] || [ -L "$f" ]; then
            echo "segment run: $f exists: the run writes only new files"
            exit 1
        fi
    done
    opened=$("$bin" segment open "$bundles/$id" < /dev/null 2>&1)
    if [ "$opened" != "BUNDLE $chain $base" ]; then
        echo "segment run: bundle $bundles/$id is not $chain at $base: $opened"
        exit 2
    fi
done

# The log holds no evo key the compare excluded: exit 0, else a message.
evo_held() {
    local key n
    for key in $EVO_KEYS; do
        n=$(awk -v k="C.s.$key" '/^excluded, not compared/ { x = 1; next } x && $1 == k { sub(",", "", $2); print $2 }' "$1")
        [ -n "$n" ] || { echo "the compare lists no $key"; return 1; }
        [ "$n" = 0 ] || { echo "the log holds $key at $n pairs: V writes it (CH7), drop it from EXCLUDE"; return 1; }
    done
}

failed=0
for run in "${runs[@]}"; do
    read -r id chain base to <<< "$run"
    log=$out/$id.log cmp=$out/$id.compare.log
    start=$SECONDS
    (
        echo "start $(date -u +%FT%TZ) load $(cut -d' ' -f1-3 /proc/loadavg); $bin"
        /usr/bin/time -v "$bin" digest l2 --chain "$chain" --blocks-dir "$blocks" \
            --out "$out/$id.jsonl" --to "$to" --assumevalid on --headers "$headers" \
            --par 16 --golden "$golden" --reverse-to "$base" --start-utxo "$bundles/$id/utxo.dat" < /dev/null
        status=$?
        echo "end $(date -u +%FT%TZ) load $(cut -d' ' -f1-3 /proc/loadavg)"
        echo "EXIT $status"
        exit "$status"
    ) > "$log" 2>&1
    status=$?
    if [ "$status" = 0 ]; then
        (
            "$bin" digest compare --mode linear --no-dump --require "$REQUIRE" \
                --exclude "$EXCLUDE" --from "$((base + 1))" --to "$to" "$out/$id.jsonl" "$golden" < /dev/null
            status=$?
            echo "EXIT $status"
            exit "$status"
        ) > "$cmp" 2>&1
        # 4: clean except the excluded keys (CH3a D167).
        if [ $? != 4 ]; then
            status=compare
        elif ! why=$(evo_held "$cmp"); then
            echo "segment run: $id: $why"
            status=evo
        fi
    fi
    echo "WINDOW $id $status $((SECONDS - start))"
    [ "$status" = 0 ] || failed=1
done
exit "$failed"
