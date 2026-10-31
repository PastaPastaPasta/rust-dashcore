#!/usr/bin/env bash
# EMBARGO-1
# The integration gate's step for the private tests (CS0 §9.4, DEC-24 D6; PLAN §7 handling).
#
# Usage: DASH_EMBARGO_DIR=<private tree> contrib/node/check-private-tests.sh
#        contrib/node/check-private-tests.sh --expect-skipped [<tree>]
#        contrib/node/check-private-tests.sh --selftest [expect-skipped]
#
# --expect-skipped is the public CI's mode (DEC-650 3a item 8): with DASH_EMBARGO_DIR unset (it
# refuses to run when it is set), it runs each test contrib/node/private-tests.txt lists
# (quirk_01_embargo_1, private_seed_replay; a listed file that is absent fails) and passes only
# when each prints its one SKIPPED line and libtest reports exactly that test passing: none ran
# against a private tree, failed or panicked. The tool-driven steps (the BS7 classifier, the
# EMBARGO-5 suite) need the private tree, so they do not run, and no copy of the suite may be
# present. `--selftest expect-skipped` checks that mode alone, with a stand-in cargo.
#
# Requires DASH_EMBARGO_DIR (the caller sets it; this script never names the tree), an absolute
# path, with the tree, fuzz/ and quirk1/ mode 0700. It makes a directory for this run under
# .receipts/ (named by the run id, unique per run), runs quirk_01_embargo_1 and private_seed_replay
# with that id (DASH_EMBARGO_RECEIPT_ID, for those two processes only; the tests write their
# receipts into that directory), and then checks each receipt rather than Cargo's exit status
# alone. Gates running at once on one tree therefore never see each other's receipts. A run removes
# its own directory, and nothing else, when it exits (an EXIT trap, also on SIGTERM/INT/HUP). A run
# killed outright (SIGKILL, power loss) leaves its few-KB directory behind; that is harmless, since
# every run reads only its own. Clean such leftovers up by hand when no gate is running:
#   find "$DASH_EMBARGO_DIR/.receipts" -mindepth 1 -maxdepth 1 -type d -name 'check-private-tests-*' -mmin +1440 -exec rm -rf {} +
# - both carry this run's id;
# - quirk_01_embargo_1: a positive case count equal to quirk1/MANIFEST's `cases`;
# - private_seed_replay: required equal to the seeds fuzz/MANIFEST lists under `required` targets
#   (R), which is at least its `min`; executed == applicable, between R and all listed seeds.
# Then it runs BS7's tx/block fuzz (contrib/node/dde-probe/bs7, v24 oracle) for
# BS7_CLASSIFIER_SECONDS (default 30) per target with the tree's private classifier,
# bs7-classifier/classify, on bs7-classifier/seeds/{tx,block}.hex (DEC-97). Its receipt passes when, for
# each target, the fuzz exits 0 with no mismatch and at least one case counted apart, the classifier
# passes, and the classifier's receipt, in this run's directory, carries this run's id and exactly the fuzz's count.
# Last, when this revision has a dash-dml crate, it runs the EMBARGO-5 private dash-dml suite from
# the tree's e5/ (required, mode 0700, once dash-dml exists): the suite file is checked against
# e5/MANIFEST's sha256, copied into dash-dml/tests/ under a name made from the run id (removed by
# the same EXIT trap that removes the run directory), and run with cargo. It passes when cargo
# exits 0 and libtest reports exactly the MANIFEST's `tests` count passed, none failed or ignored.
# Only counts are printed, never test names or panic text.
# Prints one line per test (`<test>: receipt PASS …` or `<test>: receipt FAIL <why>`) after the tests'
# own output, and exits 0 only when all pass. --selftest uses synthetic trees in
# $TMPDIR only (no private data) and builds node-l1 and bs7-tool with the oracle.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

fail() { echo "check-private-tests: $*" >&2; exit 2; }

check_receipts() {  # check_receipts <private tree> <run id>
    python3 -I - "$1" "$2" <<'EOF'
import os, sys

tree, run = sys.argv[1], sys.argv[2]

def lines(path):
    """The file's non-empty, non-comment lines, or None when it cannot be read."""
    try:
        text = open(path).read()
    except OSError:
        return None
    return [l.strip() for l in text.splitlines() if l.strip() and not l.startswith("#")]

def fields(rows):
    """`key value` rows as a dict; None when a row is malformed or a key repeats."""
    out = {}
    for row in rows:
        key, _, value = row.partition(" ")
        if not value or key in out:
            return None
        out[key] = value
    return out

def count(d, key):
    value = d.get(key, "")
    return int(value) if value.isdigit() else None

def manifest_number(path, key, index):
    rows = lines(path) or []
    if len(rows) <= index:
        return None
    k, _, value = rows[index].partition(" ")
    return int(value) if k == key and value.isdigit() else None

def seed_counts(path):
    """fuzz/MANIFEST's `min`, its seeds under `required` targets and all its seeds; None when
    it does not have that shape."""
    rows = lines(path)
    if not rows:
        return None
    key, _, value = rows[0].partition(" ")
    if key != "min" or not value.isdigit():
        return None
    state, required, listed = None, 0, 0
    for row in rows[1:]:
        if row.startswith("target "):
            parts = row.split()
            if len(parts) != 3 or parts[2] not in ("pending", "required"):
                return None
            state = parts[2]
        elif state is None:
            return None
        else:
            listed += 1
            required += state == "required"
    return int(value), required, listed

def receipt(test):
    rows = lines(os.path.join(tree, ".receipts", run, test))
    if rows is None:
        return None, "no receipt"
    d = fields(rows)
    if d is None:
        return None, "malformed receipt"
    if d.pop("run", None) != run:
        return None, "stale receipt (another run's id)"
    return d, None

results = []

q, why = receipt("quirk_01_embargo_1")
if q is not None:
    cases, declared = count(q, "cases"), manifest_number(os.path.join(tree, "quirk1", "MANIFEST"), "cases", 0)
    if not cases or cases != declared:
        q, why = None, "case count differs from quirk1/MANIFEST"
results.append(("quirk_01_embargo_1", why, q and f"{q['cases']} cases"))

r, why = receipt("private_seed_replay")
if r is not None:
    executed, applicable, required = (count(r, k) for k in ("executed", "applicable", "required"))
    manifest = seed_counts(os.path.join(tree, "fuzz", "MANIFEST"))
    if None in (executed, applicable, required, manifest):
        r, why = None, "receipt or fuzz/MANIFEST lacks a count"
    elif not executed or executed != applicable:
        r, why = None, "executed differs from applicable"
    elif required != manifest[1]:
        r, why = None, "required differs from fuzz/MANIFEST's required seeds"
    elif not manifest[1] >= manifest[0] >= 1:
        r, why = None, "fuzz/MANIFEST's required seeds are below its min"
    elif not manifest[1] <= applicable <= manifest[2]:
        r, why = None, "applicable is outside fuzz/MANIFEST's required and listed seeds"
results.append(("private_seed_replay", why, r and f"{r['executed']} seeds ({r['required']} required)"))

for test, why, summary in results:
    print(f"{test}: receipt PASS {summary}" if why is None else f"{test}: receipt FAIL {why}")
sys.exit(0 if all(why is None for _, why, _ in results) else 1)
EOF
}

check_classifier() {  # check_classifier <private tree> <run id> <fuzz output dir>
    python3 -I - "$1" "$2" "$3" <<'PY'
import json, os, sys

tree, run, out = sys.argv[1:]
counts, why = {}, None
for target in ("fuzz-tx", "fuzz-block"):
    try:
        status = int(open(os.path.join(out, target + ".status")).read())
        rows = [json.loads(l) for l in open(os.path.join(out, target + ".jsonl")) if l.strip()]
    except (OSError, ValueError):
        why = f"{target}: no fuzz output"
        break
    done = [r for r in rows if r.get("done") is True]
    verdict = [r.get("private_classifier") for r in rows if "private_classifier" in r]
    if not done or verdict != ["pass"] or status != 0:
        why = f"{target}: the fuzz or the classifier failed"
        break
    if done[-1].get("mismatches") != 0 or any("why" in r for r in rows):
        why = f"{target}: the fuzz found a mismatch"
        break
    apart = done[-1].get("allowlisted_plan7")
    if not isinstance(apart, int) or apart < 1:
        why = f"{target}: no case counted apart"
        break
    try:
        text = open(os.path.join(tree, ".receipts", run, "bs7_private_classifier." + target)).read()
    except OSError:
        why = f"{target}: no classifier receipt"
        break
    pairs = [l.split(" ", 1) for l in text.splitlines() if l.strip()]
    receipt = dict(r for r in pairs if len(r) == 2)
    if len(pairs) != 2 or len(receipt) != 2 or receipt.get("run") != run:
        why = f"{target}: stale or malformed classifier receipt"
        break
    if receipt.get("cases") != str(apart):
        why = f"{target}: the classifier's case count differs from the fuzz's"
        break
    counts[target] = apart
if why is None:
    print(f"bs7_private_classifier: receipt PASS {counts['fuzz-tx']} tx and {counts['fuzz-block']} block cases")
else:
    print(f"bs7_private_classifier: receipt FAIL {why}")
sys.exit(0 if why is None else 1)
PY
}

run_classifier() {  # run_classifier <private tree> <run id> <fuzz output dir>
    local tree="$1" run_id="$2" out="$3" target tool status
    rm -f "$tree/.receipts/$run_id/bs7_private_classifier.fuzz-tx" "$tree/.receipts/$run_id/bs7_private_classifier.fuzz-block"
    cargo build --quiet --locked --release --features oracle \
        --manifest-path "$root/contrib/node/dde-probe/bs7/Cargo.toml" --target-dir "$root/target/dde-probe-bs7" ||
        return 1
    tool="$root/target/dde-probe-bs7/release/bs7-tool"
    for target in fuzz-tx fuzz-block; do
        status=0
        DASH_EMBARGO_DIR="$tree" DASH_EMBARGO_RECEIPT_ID="$run_id" \
            BS7_PRIVATE_CLASSIFIER="$tree/bs7-classifier/classify" \
            "$tool" "$target" "$tree/bs7-classifier/seeds/${target#fuzz-}.hex" "${BS7_CLASSIFIER_SECONDS:-30}" 1 \
            >"$out/$target.jsonl" 2>/dev/null || status=$?
        echo "$status" >"$out/$target.status"
    done
}

e5_private_suite() {  # e5_private_suite <private tree> <run id>; prints one result line, returns 1 on FAIL
    local tree="$1" run_id="$2" name want sha out="" status=0 passed
    if [ ! -d "$root/dash-dml" ]; then
        echo "e5_private_suite: skipped (no dash-dml in this revision)"
        return 0
    fi
    e5_fail() { echo "e5_private_suite: receipt FAIL $*"; return 1; }
    [ -d "$tree/e5" ] && [ "$(stat -c %a "$tree/e5")" = 700 ] ||
        { e5_fail "e5/ is missing or not mode 0700"; return 1; }
    # The manifest: `tests <N>` then `<sha256>  t_e5.rs`.
    want="$(sed -n '1s/^tests \([0-9][0-9]*\)$/\1/p' "$tree/e5/MANIFEST" 2>/dev/null)"
    sha="$(sed -n '2s/^\([0-9a-f]\{64\}\)  t_e5\.rs$/\1/p' "$tree/e5/MANIFEST" 2>/dev/null)"
    { [ -n "$want" ] && [ "$want" -ge 1 ] && [ -n "$sha" ] &&
        [ "$(wc -l <"$tree/e5/MANIFEST")" -eq 2 ]; } || { e5_fail "e5/MANIFEST is missing or malformed"; return 1; }
    [ -f "$tree/e5/t_e5.rs" ] || { e5_fail "e5/t_e5.rs is missing"; return 1; }
    [ "$(sha256sum "$tree/e5/t_e5.rs" | cut -c1-64)" = "$sha" ] ||
        { e5_fail "e5/t_e5.rs differs from e5/MANIFEST's sha256"; return 1; }
    # Only the part of the run id after its last '-' (date and random suffix) goes into the name.
    name="zz_private_$(printf '%s' "${run_id##*check-private-tests-}" | tr -c 'A-Za-z0-9' '_')"
    e5_copy="$root/dash-dml/tests/$name.rs"
    cp "$tree/e5/t_e5.rs" "$e5_copy"
    chmod 600 "$e5_copy"
    out="$(mktemp "${TMPDIR:-/tmp}/check-private-tests-e5.XXXXXX")"
    (cd "$root" && cargo test --quiet --locked -p dash-dml --all-features --test "$name") >"$out" 2>&1 || status=$?
    rm -f -- "$e5_copy"
    e5_copy=
    # Counts only: the summary lines are the only output read, and none of it is printed.
    passed="$(python3 -I - "$out" <<'PY'
import re, sys
passed = failed = ignored = 0
seen = False
for line in open(sys.argv[1], errors="replace"):
    m = re.match(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;", line)
    if m:
        seen = True
        passed, failed, ignored = passed + int(m[1]), failed + int(m[2]), ignored + int(m[3])
print(f"{passed} {failed} {ignored}" if seen else "none")
PY
)"
    rm -f -- "$out"
    if [ "$status" -ne 0 ]; then
        e5_fail "cargo test exited non-zero (${passed/none/no libtest summary}: passed failed ignored)"
        return 1
    fi
    [ "$passed" = "$want 0 0" ] ||
        { e5_fail "the suite reported (passed failed ignored) ${passed/none/no libtest summary}, expected $want 0 0"; return 1; }
    echo "e5_private_suite: receipt PASS $want tests"
}

run_private_tests() {  # run_private_tests <private tree>; prints one line per test
    local tree="$1" run_id status=0
    [ -n "$tree" ] || fail "DASH_EMBARGO_DIR is unset; the private tests need the private tree"
    # The tests run in their crates' directories, so a relative path would name other trees.
    case "$tree" in /*) ;; *) fail "DASH_EMBARGO_DIR must be an absolute path" ;; esac
    for d in "$tree" "$tree/fuzz" "$tree/quirk1" "$tree/bs7-classifier"; do
        [ -d "$d" ] && [ "$(stat -c %a "$d")" = 700 ] ||
            fail "the private tree, its fuzz/, quirk1/ or bs7-classifier/ is missing or not mode 0700"
    done
    mkdir -p -m 700 "$tree/.receipts"
    # The run removes the directory it made, and no other, however it ends: bash runs the EXIT
    # trap on SIGTERM/INT/HUP too (once the foreground command has returned).
    run_dir=
    # The EMBARGO-5 suite's copy in dash-dml/tests/ goes in the same trap.
    e5_copy=
    trap 'if [ -n "$run_dir" ]; then rm -rf -- "$run_dir"; fi; if [ -n "$e5_copy" ]; then rm -f -- "$e5_copy"; fi' EXIT
    # mktemp -d makes the name unique and the directory 0700, so no other run shares either.
    run_dir="$(mktemp -d "$tree/.receipts/check-private-tests-$(date -u +%Y%m%dT%H%M%SZ).XXXXXX")"
    run_id="$(basename "$run_dir")"
    cd "$root"
    DASH_EMBARGO_DIR="$tree" DASH_EMBARGO_RECEIPT_ID="$run_id" \
        cargo test --quiet --locked -p dash-consensus --test quirk_01_embargo_1 || status=1
    DASH_EMBARGO_DIR="$tree" DASH_EMBARGO_RECEIPT_ID="$run_id" \
        cargo test --quiet --locked -p node-l1 --features oracle --test private_seed_replay || status=1
    check_receipts "$tree" "$run_id" || status=1
    local out
    out="$(mktemp -d "${TMPDIR:-/tmp}/check-private-tests-fuzz.XXXXXX")"
    run_classifier "$tree" "$run_id" "$out" || status=1
    check_classifier "$tree" "$run_id" "$out" || status=1
    rm -rf "$out"
    e5_private_suite "$tree" "$run_id" || status=1
    return "$status"
}

skipped_test() {  # skipped_test <test> <cargo test args...>: one line; returns 1 unless it skipped cleanly
    local test="$1" out status=0 summary
    shift
    out="$(cargo test --quiet --locked "$@" -- --exact "$test" 2>&1)" || status=$?
    summary="$(grep -E '^test result: ' <<<"$out" | tr '\n' ' ')"
    if [ "$status" -ne 0 ]; then
        echo "$test: FAIL (cargo test exited $status)"
    elif [ "$(grep -c "^$test: SKIPPED: DASH_EMBARGO_DIR is unset" <<<"$out")" -ne 1 ]; then
        echo "$test: FAIL (no SKIPPED line, so it did not skip)"
    elif grep -qE "^$test: (PASS|FAIL)|panicked" <<<"$out"; then
        echo "$test: FAIL (it ran or panicked)"
    elif [ "$(grep -c '^test result: ' <<<"$out")" -ne 1 ] ||
        ! grep -qE '^test result: ok\. 1 passed; 0 failed; 0 ignored;' <<<"$summary"; then
        echo "$test: FAIL (libtest did not report exactly this one test passing: ${summary:-no summary})"
    else
        echo "$test: skipped cleanly"
        return 0
    fi
    return 1
}

expect_skipped() {  # expect_skipped [root]: the CI mode; every private test skips cleanly without the private tree
    local dir="${1:-$root}" status=0 file test
    [ -z "${DASH_EMBARGO_DIR+set}" ] || fail "--expect-skipped needs DASH_EMBARGO_DIR unset (it is set)"
    [ -f "$dir/contrib/node/private-tests.txt" ] || fail "no contrib/node/private-tests.txt (the list of private tests)"
    cd "$dir"
    while read -r file test; do
        case "$file" in ''|'#'*) continue ;; esac
        if [ ! -f "$file" ]; then
            echo "$test: FAIL ($file is listed but absent)"
            status=1
            continue
        fi
        skipped_test "$test" -p "${file%%/*}" --test "$(basename "$file" .rs)" || status=1
    done <"$dir/contrib/node/private-tests.txt"
    if compgen -G "$dir/dash-dml/tests/zz_private_*" >/dev/null; then
        echo "e5_private_suite: FAIL (a copy of the private suite is in dash-dml/tests)"
        status=1
    else
        echo "e5_private_suite: not run (it lives in the private tree)"
    fi
    echo "bs7_private_classifier: not run (it lives in the private tree)"
    return "$status"
}

selftest_expect_skipped() {  # --expect-skipped against a stand-in cargo
    local bin out st fake
    bin="$(mktemp -d "${TMPDIR:-/tmp}/check-private-tests-skip.XXXXXX")"
    # A stand-in tree (the stand-in cargo ignores it), so nothing is planted in this checkout.
    fake="$bin/tree"
    mkdir -p "$fake/contrib/node" "$fake/dash-dml/tests" "$fake/dash-consensus/tests" "$fake/node-l1/tests"
    cp "$root/contrib/node/private-tests.txt" "$fake/contrib/node/"
    while read -r file _; do case "$file" in ''|'#'*) ;; *) : >"$fake/$file" ;; esac; done <"$fake/contrib/node/private-tests.txt"
    # The stand-in prints what libtest and node_sink would for the case named by STUB_CASE.
    cat >"$bin/cargo" <<'SH'
#!/usr/bin/env bash
all="$*"
test="${all##* --exact }"
summary() { echo "test result: $1. $2 passed; $3 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"; }
case "$STUB_CASE" in
    skip) echo "$test: SKIPPED: DASH_EMBARGO_DIR is unset; the integration gate runs this test" >&2; summary ok 1 0 ;;
    ran) echo "$test: PASS 3 cases" >&2; summary ok 1 0 ;;
    panic) echo "thread '$test' panicked at tests/x.rs:1:1:" >&2; summary FAILED 0 1; exit 101 ;;
    silent) summary ok 1 0 ;;
    none) echo "$test: SKIPPED: DASH_EMBARGO_DIR is unset; the integration gate runs this test" >&2; summary ok 0 0 ;;
esac
SH
    chmod +x "$bin/cargo"
    sk_fail() { rm -rf -- "$bin"; echo "check-private-tests selftest: $*" >&2; exit 1; }
    st_skip() {  # st_skip <case> <expected status>
        st=0
        out="$(env -u DASH_EMBARGO_DIR PATH="$bin:$PATH" STUB_CASE="$1" "$root/contrib/node/check-private-tests.sh" --expect-skipped "$fake" 2>&1)" || st=$?
        [ "$st" -eq "$2" ] || sk_fail "--expect-skipped with a '$1' test exited $st, not $2: $out"
    }
    st_skip skip 0
    [ "$(grep -c ': skipped cleanly$' <<<"$out")" -eq 2 ] || sk_fail "--expect-skipped did not report two clean skips: $out"
    st_skip ran 1
    st_skip panic 1
    st_skip silent 1
    st_skip none 1
    st=0
    out="$(DASH_EMBARGO_DIR=/x PATH="$bin:$PATH" STUB_CASE=skip "$root/contrib/node/check-private-tests.sh" --expect-skipped "$fake" 2>&1)" || st=$?
    [ "$st" -eq 2 ] && grep -q 'needs DASH_EMBARGO_DIR unset' <<<"$out" || sk_fail "--expect-skipped ran with DASH_EMBARGO_DIR set: $out"
    : >"$fake/dash-dml/tests/zz_private_selftest.rs"
    st_skip skip 1
    rm -f -- "$fake/dash-dml/tests/zz_private_selftest.rs"
    mv "$fake/node-l1/tests/private_seed_replay.rs" "$fake/node-l1/tests/moved.rs"
    st_skip skip 1
    grep -q 'private_seed_replay: FAIL (node-l1/tests/private_seed_replay.rs is listed but absent)' <<<"$out" ||
        sk_fail "--expect-skipped did not report a moved private test: $out"
    rm -rf -- "$bin"
    echo "check-private-tests selftest: expect-skipped ok"
}

selftest() {
    local tree out status
    selftest_expect_skipped
    tmp="$(mktemp -d "${TMPDIR:-/tmp}/check-private-tests.XXXXXX")"
    trap 'rm -rf "$tmp"' EXIT
    tree="$tmp/tree"
    st_fail() { echo "check-private-tests selftest: $*" >&2; exit 1; }

    # A synthetic tree: one case both sides reject and one header seed (as the drivers'
    # self-tests use), listed with their SHA-256 like a real tree.
    mkdir -m 700 "$tree" "$tree/quirk1" "$tree/quirk1/cases" "$tree/fuzz" "$tree/fuzz/diff_header_decode" \
        "$tree/fuzz/diff_header_decode/seeds"
    printf '%s' '{"id":"case-001","pin":"v24.0","kind":"tx","input_hex":"00","expect":{"decode_ok":false}}' \
        >"$tree/quirk1/cases/a"
    printf 'cases 1\nmin 1\n%s  cases/a\n' "$(sha256sum "$tree/quirk1/cases/a" | cut -c1-64)" \
        >"$tree/quirk1/MANIFEST"
    head -c 80 /dev/zero >"$tree/fuzz/diff_header_decode/seeds/s"
    printf 'min 1\ntarget diff_header_decode required\n%s  diff_header_decode/seeds/s\n' \
        "$(sha256sum "$tree/fuzz/diff_header_decode/seeds/s" | cut -c1-64)" >"$tree/fuzz/MANIFEST"
    # A classifier that classifies nothing: it counts the cases it is sent and writes its receipt.
    # Seeds: one minimal transaction, and a block of it.
    mkdir -m 700 "$tree/bs7-classifier" "$tree/bs7-classifier/seeds"
    local seed_tx
    seed_tx="0100000001$(printf '%072d' 0)00ffffffff01$(printf '%016d' 0)0000000000"
    echo "$seed_tx" >"$tree/bs7-classifier/seeds/tx.hex"
    printf '%0160d01%s\n' 0 "$seed_tx" >"$tree/bs7-classifier/seeds/block.hex"
    counting() {  # counting <cases written to the receipt: "lines" or a number> <exit status> <run id or "">
        cat >"$tree/bs7-classifier/classify" <<SH
#!/bin/sh
n=\$(wc -l)
[ "$1" = lines ] || n=$1
mkdir -p "\$DASH_EMBARGO_DIR/.receipts/\$DASH_EMBARGO_RECEIPT_ID"
printf 'run %s\\ncases %s\\n' "${3:-\$DASH_EMBARGO_RECEIPT_ID}" "\$n" >"\$DASH_EMBARGO_DIR/.receipts/\$DASH_EMBARGO_RECEIPT_ID/bs7_private_classifier.\$1"
exit $2
SH
        chmod 700 "$tree/bs7-classifier/classify"
    }
    counting lines 0 ""
    export BS7_CLASSIFIER_SECONDS=3
    # The EMBARGO-5 suite: two trivial tests that use no private data, listed like a real tree.
    mkdir -m 700 "$tree/e5"
    e5_manifest() {  # e5_manifest <tests>: list e5/t_e5.rs as it is now
        printf 'tests %s\n%s  t_e5.rs\n' "$1" "$(sha256sum "$tree/e5/t_e5.rs" | cut -c1-64)" >"$tree/e5/MANIFEST"
    }
    e5_suite() {  # e5_suite <second test's body>
        printf '#[test]\nfn one() {}\n#[test]\nfn two() { %s }\n' "$1" >"$tree/e5/t_e5.rs"
    }
    e5_suite ""
    e5_manifest 2
    chmod 600 "$tree/e5/t_e5.rs" "$tree/e5/MANIFEST"
    # No copy of the suite may be left in dash-dml/tests/, whatever happened.
    e5_no_copy() { ! compgen -G "$root/dash-dml/tests/zz_private_*" >/dev/null || st_fail "a copy of the e5 suite was left behind: $1"; }

    # 1. The whole step passes on the synthetic tree, through all four tests and their receipts.
    out="$(run_private_tests "$tree" 2>&1)" || st_fail "the synthetic tree failed: $out"
    grep -q '^quirk_01_embargo_1: receipt PASS 1 cases$' <<<"$out" &&
        grep -q '^private_seed_replay: receipt PASS 1 seeds (1 required)$' <<<"$out" &&
        grep -qE '^bs7_private_classifier: receipt PASS [1-9][0-9]* tx and [1-9][0-9]* block cases$' <<<"$out" &&
        grep -q '^e5_private_suite: receipt PASS 2 tests$' <<<"$out" ||
        st_fail "the synthetic tree did not report four passes"
    e5_no_copy "after a passing run"

    # 1a. The EMBARGO-5 step fails when the manifest's count differs, the file's sha256 differs, a
    # test fails or e5/ is missing, and the copy is gone afterwards in every case.
    e5_fails() {  # e5_fails <what> <expected reason>
        local st=0
        out="$(e5_private_suite "$tree" check-private-tests-e5.selftest 2>&1)" || st=$?
        [ "$st" -ne 0 ] && grep -qF "e5_private_suite: receipt FAIL $2" <<<"$out" || st_fail "$1 passed: $out"
        e5_no_copy "$1"
    }
    e5_manifest 3
    e5_fails "a manifest count above the suite's" "the suite reported (passed failed ignored) 2 0 0, expected 3 0 0"
    e5_manifest 1
    e5_fails "a manifest count below the suite's" "the suite reported (passed failed ignored) 2 0 0, expected 1 0 0"
    e5_manifest 2
    sed -i "2s/^[0-9a-f]\{64\}/$(printf '%064d' 0)/" "$tree/e5/MANIFEST"
    e5_fails "a changed sha256" "e5/t_e5.rs differs from e5/MANIFEST's sha256"
    e5_suite 'panic!("synthetic");'
    e5_manifest 2
    e5_fails "a failing test" "cargo test exited non-zero (1 1 0"
    e5_suite ""
    e5_manifest 2
    mv "$tree/e5" "$tmp/e5.away"
    e5_fails "a missing e5/" "e5/ is missing or not mode 0700"
    mv "$tmp/e5.away" "$tree/e5"
    chmod 755 "$tree/e5"
    e5_fails "a group-readable e5/" "e5/ is missing or not mode 0700"
    chmod 700 "$tree/e5"

    # 1b. The classifier step fails when the classifier fails, miscounts, writes another run's id or
    # is missing.
    classifier_fails() {  # classifier_fails <what> <expected reason>
        local fuzz status=0
        fuzz="$(mktemp -d "$tmp/fuzz.XXXXXX")"
        run_classifier "$tree" this-run "$fuzz" >/dev/null 2>&1 || true
        out="$(check_classifier "$tree" this-run "$fuzz" 2>&1)" || status=$?
        rm -rf "$tree/.receipts/this-run"
        [ "$status" -ne 0 ] && grep -qF "bs7_private_classifier: receipt FAIL fuzz-tx: $2" <<<"$out" ||
            st_fail "$1 passed: $out"
    }
    counting lines 1 ""
    classifier_fails "a failing classifier" "the fuzz or the classifier failed"
    counting 1 0 ""
    classifier_fails "a classifier that miscounts" "the classifier's case count differs"
    counting lines 0 earlier-run
    classifier_fails "another run's classifier receipt" "stale or malformed classifier receipt"
    rm "$tree/bs7-classifier/classify"
    classifier_fails "a missing classifier" "the fuzz or the classifier failed"
    counting lines 0 ""

    # 1c. Two runs at once on one tree (the gates' case) each see their own four receipts, and leave
    # no run directory behind.
    local pa pb sa=0 sb=0
    run_private_tests "$tree" >"$tmp/run-a.out" 2>&1 & pa=$!
    run_private_tests "$tree" >"$tmp/run-b.out" 2>&1 & pb=$!
    wait "$pa" || sa=$?
    wait "$pb" || sb=$?
    for run in a b; do
        [ "$(grep -c ': receipt PASS ' "$tmp/run-$run.out")" -eq 4 ] ||
            st_fail "concurrent run $run did not report four receipts: $(cat "$tmp/run-$run.out")"
    done
    [ "$sa" -eq 0 ] && [ "$sb" -eq 0 ] || st_fail "a concurrent run exited non-zero ($sa, $sb)"
    [ -z "$(ls -A "$tree/.receipts")" ] || st_fail "a run left its receipt directory behind"
    e5_no_copy "after two concurrent runs"

    # A stand-in `cargo` for the next two cases: it writes the receipts the tests would. With
    # STUB_HOLD set it stops after the first receipt (recording its pid) until released.
    mkdir "$tmp/bin"
    cat >"$tmp/bin/cargo" <<'EOF'
#!/usr/bin/env bash
dir="$DASH_EMBARGO_DIR/.receipts/$DASH_EMBARGO_RECEIPT_ID"
case "$*" in
    *quirk_01_embargo_1*)
        printf 'run %s\ncases 1\n' "$DASH_EMBARGO_RECEIPT_ID" >"$dir/quirk_01_embargo_1"
        if [ -n "${STUB_HOLD:-}" ]; then
            echo $$ >"$STUB_HOLD.pid"
            : >"$STUB_HOLD.reached"
            # Ends too if the selftest has failed and removed its directory.
            until [ -e "$STUB_HOLD.release" ] || [ ! -e "$STUB_HOLD.reached" ]; do sleep 0.1; done
        fi ;;
    *private_seed_replay*)
        printf 'run %s\nexecuted 1\napplicable 1\nrequired 1\n' "$DASH_EMBARGO_RECEIPT_ID" \
            >"$dir/private_seed_replay" ;;
    *dash-dml*)
        echo "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s" ;;
esac
EOF
    chmod +x "$tmp/bin/cargo"

    # 1c. A run never removes a directory it did not make: an existing one, even an old one, and
    # whatever it holds, is left as it was.
    local foreign="$tree/.receipts/check-private-tests-foreign" other="$tree/.receipts/other"
    mkdir -m 700 "$foreign" "$other"
    printf 'x\n' >"$foreign/quirk_01_embargo_1"
    touch -d '48 hours ago' "$foreign" "$other"
    local before_f before_o
    before_f="$(stat -c %Y "$foreign")"; before_o="$(stat -c %Y "$other")"
    out="$(PATH="$tmp/bin:$PATH" run_private_tests "$tree" 2>&1)" || st_fail "a run beside a foreign directory failed: $out"
    [ "$(grep -c ': receipt PASS ' <<<"$out")" -eq 4 ] || st_fail "that run did not report four receipts: $out"
    [ -f "$foreign/quirk_01_embargo_1" ] && [ "$(stat -c %Y "$foreign")" = "$before_f" ] &&
        [ -d "$other" ] && [ "$(stat -c %Y "$other")" = "$before_o" ] ||
        st_fail "a run touched a directory it did not make"
    rm -rf "$foreign" "$other"
    [ -z "$(ls -A "$tree/.receipts")" ] || st_fail "a run left its receipt directory behind"

    # 1d. A run stopped by SIGTERM while checking (its test still running) removes its own
    # directory, runs no further test and does not report success. The signal reaches the run
    # first; the stand-in then dies, which lets the run act on it.
    (PATH="$tmp/bin:$PATH" STUB_HOLD="$tmp/hold" run_private_tests "$tree" >"$tmp/run-a.out" 2>&1) & pa=$!
    for _ in $(seq 300); do [ -e "$tmp/hold.reached" ] && break; sleep 0.1; done
    [ -e "$tmp/hold.reached" ] || st_fail "the run to interrupt did not reach its pause"
    [ -n "$(ls -A "$tree/.receipts")" ] || st_fail "the run to interrupt has no directory"
    kill -TERM "$pa"
    kill -TERM "$(cat "$tmp/hold.pid")"
    sa=0; wait "$pa" || sa=$?
    [ "$sa" -eq 143 ] || st_fail "the interrupted run exited $sa, not 143"
    [ -z "$(ls -A "$tree/.receipts")" ] || st_fail "the interrupted run left its directory behind"
    ! grep -q 'receipt PASS' "$tmp/run-a.out" || st_fail "the interrupted run reported a pass"
    e5_no_copy "after an interrupted run"

    receipts="$tree/.receipts/good-run"
    mkdir -m 700 "$receipts"
    expect_fail() {  # expect_fail <what> <expected line>
        status=0
        out="$(check_receipts "$tree" good-run 2>&1)" || status=$?
        [ "$status" -ne 0 ] && grep -qF "$2" <<<"$out" || st_fail "$1 passed: $out"
    }
    good() {
        printf 'run good-run\ncases 1\n' >"$receipts/quirk_01_embargo_1"
        printf 'run good-run\nexecuted 1\napplicable 1\nrequired 1\n' >"$receipts/private_seed_replay"
    }
    good
    check_receipts "$tree" good-run >/dev/null || st_fail "good receipts failed"

    # 2. A receipt from another run.
    sed -i 's/^run good-run$/run earlier-run/' "$receipts/quirk_01_embargo_1"
    expect_fail "a stale quirk receipt" "quirk_01_embargo_1: receipt FAIL stale receipt"
    good; sed -i 's/^run good-run$/run earlier-run/' "$receipts/private_seed_replay"
    expect_fail "a stale replay receipt" "private_seed_replay: receipt FAIL stale receipt"

    # 3. A missing receipt.
    good; rm "$receipts/quirk_01_embargo_1"
    expect_fail "a missing quirk receipt" "quirk_01_embargo_1: receipt FAIL no receipt"
    good; rm "$receipts/private_seed_replay"
    expect_fail "a missing replay receipt" "private_seed_replay: receipt FAIL no receipt"

    # 4. Counts that do not match.
    good; printf 'run good-run\ncases 2\n' >"$receipts/quirk_01_embargo_1"
    expect_fail "a case count above the manifest's" "quirk_01_embargo_1: receipt FAIL case count differs"
    good; printf 'run good-run\ncases 0\n' >"$receipts/quirk_01_embargo_1"
    expect_fail "zero cases" "quirk_01_embargo_1: receipt FAIL case count differs"
    good; printf 'run good-run\nexecuted 1\napplicable 2\nrequired 1\n' >"$receipts/private_seed_replay"
    expect_fail "executed below applicable" "private_seed_replay: receipt FAIL executed differs from applicable"
    good; printf 'run good-run\nexecuted 1\napplicable 1\nrequired 2\n' >"$receipts/private_seed_replay"
    expect_fail "required above the manifest's" "private_seed_replay: receipt FAIL required differs"
    good; cp "$tree/fuzz/MANIFEST" "$tmp/MANIFEST.keep"
    printf 'target diff_tx_decode required\n%064d  diff_tx_decode/seeds/t\n' 0 >>"$tree/fuzz/MANIFEST"
    expect_fail "a replay that counted fewer required seeds than listed" "private_seed_replay: receipt FAIL required differs"
    cp "$tmp/MANIFEST.keep" "$tree/fuzz/MANIFEST"
    good; sed -i 's/^min 1$/min 2/' "$tree/fuzz/MANIFEST"
    expect_fail "required below the manifest's min" "private_seed_replay: receipt FAIL fuzz/MANIFEST's required seeds are below its min"
    sed -i 's/^min 2$/min 1/' "$tree/fuzz/MANIFEST"
    good; printf 'run good-run\nexecuted 2\napplicable 2\nrequired 1\n' >"$receipts/private_seed_replay"
    expect_fail "more applicable seeds than listed" "private_seed_replay: receipt FAIL applicable is outside"
    good; printf 'run good-run\nexecuted 1\nrequired 1\n' >"$receipts/private_seed_replay"
    expect_fail "a receipt without the applicable count" "private_seed_replay: receipt FAIL receipt or fuzz/MANIFEST lacks a count"

    # 5. No tree, or a tree that is not private: refused before anything runs.
    status=0
    out="$(run_private_tests "" 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'DASH_EMBARGO_DIR is unset' <<<"$out" || st_fail "an unset tree ran"
    status=0
    out="$(cd "$tmp" && run_private_tests tree 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'must be an absolute path' <<<"$out" || st_fail "a relative tree ran"
    chmod 755 "$tree/fuzz"
    status=0
    out="$(run_private_tests "$tree" 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'not mode 0700' <<<"$out" || st_fail "a tree that is not private ran"

    echo "check-private-tests selftest: ok"
}

case "${1:-}" in
    --selftest) if [ "${2:-}" = expect-skipped ]; then selftest_expect_skipped; else selftest; fi ;;
    --expect-skipped) expect_skipped "${2:-}" ;;
    "") run_private_tests "${DASH_EMBARGO_DIR:-}" ;;
    *) fail "usage: DASH_EMBARGO_DIR=<private tree> $0 | $0 --expect-skipped | $0 --selftest [expect-skipped]" ;;
esac
