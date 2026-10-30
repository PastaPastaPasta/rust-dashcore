#!/usr/bin/env bash
# Tests contrib/differential.sh's verdicts with stub test binaries: it must pass only when every
# scheduled run left a log reporting a pass and all its streams, and refuse empty work.
#
# Usage: contrib/test-differential.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/x11-difftest.XXXXXX")
trap 'rm -rf "$work"' EXIT

# stub <mode>: a test binary for differential.sh's runs. Modes:
#   pass     reports its streams and a pass;
#   fail     reports a failure and exits non-zero;
#   silent   exits 0 without reporting anything;
#   nocases  reports a pass but no compared stream;
#   vanish   reports a pass, then deletes every log (xargs still sees success);
#   unwritable  reports a pass, then replaces the log directory by a file, so the later runs
#            cannot open their logs (even as root).
stub() {
    cat > "$work/stub" <<STUB
#!/usr/bin/env bash
mode=$1
if [ "\$mode" = fail ]; then echo 'test result: FAILED. 0 passed; 1 failed'; exit 101; fi
[ "\$mode" = silent ] && exit 0
if [ "\$mode" != nocases ]; then
    n=1
    [ "\$2" = core_live_stages ] && n=11
    for i in \$(seq 1 \$n); do echo "stage\$i: 10 cases (seed 1) match [\"harness\"]"; done
fi
echo 'test result: ok. 1 passed; 0 failed; 0 ignored'
[ "\$mode" = vanish ] && rm -f "$work/logs"/*.log
if [ "\$mode" = unwritable ]; then rm -rf "$work/logs"; touch "$work/logs"; fi
exit 0
STUB
    chmod +x "$work/stub"
}

# run <expected status> <mode> [<harness>...]
run() {
    local want=$1 mode=$2
    shift 2
    [ $# -gt 0 ] || set -- harness
    stub "$mode"
    rm -rf "$work/logs"
    local got=0
    DIFF_TEST_BIN="$work/stub" DIFF_LOG="$work/logs" JOBS=1 \
        "$here/differential.sh" 1 10 "$@" > "$work/out" 2>&1 || got=$?
    if [ "$got" != "$want" ]; then
        echo "differential.sh with a '$mode' stub and harnesses ($*) exited $got, expected $want:" >&2
        cat "$work/out" >&2
        exit 1
    fi
    echo "ok: '$mode' stub, harnesses ($*), exit $got: $(tail -1 "$work/out")"
}

run 0 pass
run 0 pass harness "env DASH_X11_CORE_SOFT=1 harness"
run 1 fail
run 1 silent
run 1 nocases
run 1 vanish
run 1 unwritable
# No work at all, or an empty harness, is a usage error.
run 2 pass ""
stub pass
got=0
DIFF_TEST_BIN="$work/stub" DIFF_LOG="$work/logs" "$here/differential.sh" 1 10 > "$work/out" 2>&1 || got=$?
[ "$got" = 2 ] || { echo "no harness: exit $got, expected 2" >&2; exit 1; }
got=0
DIFF_TEST_BIN="$work/stub" DIFF_LOG="$work/logs" "$here/differential.sh" 0 10 harness > "$work/out" 2>&1 || got=$?
[ "$got" = 2 ] || { echo "zero seeds: exit $got, expected 2" >&2; exit 1; }
echo "ok: no harness and zero seeds are usage errors"
