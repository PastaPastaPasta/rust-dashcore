#!/usr/bin/env bash
# EMBARGO-1
# The only way to run a node-fuzz target (node program CS0 §9.3, O3; PLAN §7 handling).
#
#   node-fuzz/run.sh <target> --pin v24.0 --cpu-hours H [--jobs J [--shard]] [--public-seeds DIR]
#                    [--run-tag TAG] [--corpus-sample N --corpus-sample-bytes B]
#   node-fuzz/run.sh <target> --pin P --merge            (merge the working corpus in place)
#   node-fuzz/run.sh <target> --pin P --minimize-corpus [--run-tag TAG]  (BL-841; the farm's)
#   node-fuzz/run.sh <target> --pin P --job NAME         (a minimisation or replay)
#   node-fuzz/run.sh <target> --pin P --certify --seconds S [--wall-cap W] [--run-tag TAG]
#   node-fuzz/run.sh --selftest
#
# A certify run (DEC-186, `fuzzctl certify`) merges the target's corpora into its working corpus
# (segment-1, libFuzzer -merge=1: an input joins only if it adds a feature, so the union is kept),
# measures that corpus with a replay pass (segment-2: -runs=0, whose INITED line is the corpus's
# cov and ft), fuzzes it with one libFuzzer process (segment-3) until the process has used S seconds
# of its own CPU time after its INITED (the whole corpus loaded), stops it at libFuzzer's loop
# boundary through its stop file (its DONE line, final stats and exit 0; DEC-186 amendment 3), and
# measures the corpus again (segment-4). Coverage is the replays' (amendment 2); the window's DONE
# line is a second, in-process check against its own INITED, read by fuzzctl. At the
# farm's priority on a loaded box the window takes longer in wall time; W seconds of wall time from
# the window's start (default 43200, 12 h) cap it. A single process, because the harness's
# start-up check refuses libFuzzer's -fork flags and temporary corpus directories. Segment-3's
# window.json records what stopped it, its CPU time at INITED and at the stop, and the wall time.
#
# A job is a file `fuzz/<target>/jobs/job-<32 hex>` (0600) of `mode <m>` and `input <path>`
# lines, which this script consumes. Its modes: `minimize` (a crash of an earlier run),
# `minimize-divergence` (a recorded divergence, keeping the field its sidecar `<path>.txt`
# names) and `replay` (a file or directory of findings or regression vectors, run once). A
# replay job with an `order fixed` line takes several `input` lines and runs them in the job's
# order: each is staged into a directory of its own and run with -shuffle=0 -prefer_small=0. A
# finding's path (its name may be its hash) and a divergence's field never appear on a command
# line or in an environment, which `ps` and /proc show (PLAN §7.5): inputs are copied into the
# run as `input/input-<n>` (fixed order: `input/<k>/input-<n>` for the job's k-th input) by
# shell redirection, and the field (and the command, when the sidecar names one) goes to the
# target in the run's `abort-field` file.
# --run-tag (16 hex digits) ends the run id, so a caller can find its run.
# --minimize-corpus (BL-841) merges the working corpus alone (libFuzzer -merge=1 into the run's
# empty cmin/: an input is kept only if it adds a feature) and, when the merge passed, swaps it in by
# two renames: corpus -> corpus.pre-cmin (kept 0700; only the farm removes it, through rm-if-idle,
# after the leg's next passing slice), then cmin/ -> corpus. The seeds, public seeds and regression
# vectors stay as they are. It runs alone: it holds the leg's lock exclusively to the end and starts
# only while it can take the triage lock (released before the merge), and refuses a target with an
# open triage cluster or a job waiting, a floor-gated (chain-v2) target (cmin-floor-gated), the
# opt-in builds, and any other mode, budget or sample. A swap cut between its renames leaves
# corpus.pre-cmin without corpus: every run refuses that tree (cmin-interrupted; again under the
# leg's lock) and the next --minimize-corpus rolls it back before anything else; while
# corpus.pre-cmin is kept beside corpus it refuses. Its summary line has counts only.
# --corpus-sample N --corpus-sample-bytes B (the smoke's, BL-333) fuzz from a sample of the working
# corpus and the public seeds instead of all of them: at most N files and B bytes, chosen by the
# sha256 of their names, reflinked into the run's sample/ (removed at the end), which takes
# libFuzzer's new inputs. The working corpus is only read. The private seeds and the fixed
# regression vectors are loaded in full, as always.
#
# --shard (BL-841b, the farm's --replay-mode shard) runs --jobs J single libFuzzer processes instead
# of one -jobs parent, each loading 1/J of the inputs: every input of the working corpus, the private
# seeds, the public seeds and the fixed regression vectors goes to exactly one shard (shards.py,
# rotated by the run id; reflinked into the run's shards/<k>, removed at the end; shards.json keeps
# the counts). Worker k fuzzes `new shards/<k>` with -features_dir=features/segment-<s>/<k>: every
# worker writes its new inputs to the run's new/ and rereads it (-reload), and at the run's end (and
# from its EXIT trap) the new inputs are folded into the working corpus by links, a name already
# there counted as a duplicate and left. A staged input is loaded exactly once in segment 1; from
# segment 2 on every worker also loads all of new/, the earlier segments' discoveries. Nothing a
# worker writes is the working corpus. It is for a budgeted fuzzing run of the default build with
# --jobs 2 or more: --certify, --merge, --minimize-corpus, --job, --runs, --corpus-sample and the
# opt-in builds refuse it.
#
# The target must be listed in node-fuzz/targets.toml, with the pin among its pins; the
# registry's public_seeds are used when --public-seeds is not given (a chain target refuses
# --public-seeds: its seeds are its encoding's compile, the registry's). A chain target (one with
# an `encoding`, O2b-4 §3a.3) runs only on a private tree marked with its encoding (its ENCODING
# file, tree_encoding.py), and gets the encoding in NODE_L1_CHAIN_ENCODING, refusing any other. A
# chain-v2 target's certify run replays its corpus into a phase census and fuzzes only when that
# census meets the target's floor (node-l1/census/<target>.floor.json, census_floor.py). Everything libFuzzer
# writes stays in the private tree $DASH_EMBARGO_DIR/fuzz/<target>/: the working corpus
# (corpus/), the private seeds (seeds/, used when present; never created here, since only the
# private seed manifest may add one), copied public seeds (public-seeds/), triage's regression
# vectors (regress/; every fuzzing run replays regress/fixed/) and one directory per run
# (runs/<id>/{divergences,crashes,logs,tmp,input,sample}, plus the empty file `built` once the target is
# built: fuzzctl starts a job's timeout from it).
# libFuzzer's stdout and stderr go to the run's logs/. The script prints one summary line and
# exits 0 only when the run passed: no divergence, crash or sink failure, and the CPU budget
# reached; a minimisation passes when libFuzzer exits 0 having written the run's
# crashes/minimized; a replay passes when no input diverges or crashes. It never sets
# DASH_EMBARGO_DIR (only --selftest points it at an empty directory of its own under
# /work/scratch).
#
# Two opt-in builds (BL-812; README "Two opt-in builds"), unset by default: NODE_FUZZ_TRACE_CMP=0
# drops the Rust crates' trace-compares (the fuzz oracle archive keeps Core's);
# NODE_FUZZ_COVERAGE=under-test instruments only the target's registry
# `under_test` crates and node-fuzz's bins, with the plain oracle. Either builds in
# node-fuzz/target/cov-<id>, is recorded in the verdict, is refused by --certify and --merge, and
# fuzzes a run-local copy of the working corpus, never the corpus itself.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"

fail() { echo "run.sh: $*" >&2; exit 2; }

# The opt-in builds' pieces (BL-812), read from $coverage, $trace_cmp, $target and $here.
# read_under_test sets $under_test, the rustc crate names of the target's registry `under_test`.
read_under_test() {
    under_test="$(python3 -I "$here/fuzzctl.py" under-test "$target" 2>/dev/null)" || case $? in
        4) fail "$target has no under_test in node-fuzz/targets.toml: NODE_FUZZ_COVERAGE=under-test is refused" ;;
        *) fail "$target's under_test in node-fuzz/targets.toml is not usable (see fuzzctl check)" ;;
    esac
}
# build_setup sets $sancov, $build_env and $target_dir: RUSTFLAGS for every target crate, or under
# test only sancov-rustc's for the listed crates and node-fuzz's bins, RUSTFLAGS keeping `--cfg
# fuzzing`. Cargo rebuilds when RUSTFLAGS change but not when the wrapper's crates or flags do, so
# every build but the default gets a directory of its own, named by all three.
build_setup() {
    sancov="-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 -Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table"
    [ "$trace_cmp" = 0 ] || sancov+=" -Cllvm-args=-sanitizer-coverage-trace-compares"
    target_dir="$here/target"
    if [ "$coverage" = under-test ]; then
        build_env=(RUSTFLAGS="--cfg fuzzing" RUSTC_WRAPPER="$here/sancov-rustc" NODE_FUZZ_SANCOV_FLAGS="$sancov"
                   NODE_FUZZ_SANCOV_CRATES="$(paste -sd, <<<"$under_test")")
    else
        build_env=(RUSTFLAGS="--cfg fuzzing $sancov")
    fi
    if [ "$coverage:$trace_cmp" != all:1 ]; then
        target_dir="$here/target/cov-$({ printf '%s\n' "$coverage" "$sancov" "$under_test"; cat "$here/sancov-rustc"; } |
            sha256sum | cut -c1-12)"
    fi
}
# same_build fails the run when the registry's under_test or sancov-rustc no longer give the build
# it planned ($1: "$under_test:$target_dir" as planned). run.sh calls it under the build lock.
same_build() {
    [ "$coverage" != under-test ] || read_under_test
    build_setup
    [ "$under_test:$target_dir" = "$1" ] ||
        fail "the registry's under_test or node-fuzz/sancov-rustc changed while this run waited for the build lock"
}
# The longest a certify window may run on after its stop (seconds; more than libFuzzer's 1200 s
# -timeout for one input). The driver kills a process that outlives it, and the verdict (as
# fuzzctl does) fails a window whose exit came later, killed or not (DEC-186 amendment 3 (a)).
stop_grace=1500

selftest() {
    local out status runs
    unset NODE_FUZZ_COVERAGE NODE_FUZZ_TRACE_CMP  # the cases set them where they test them (BL-812)
    selftest_dir="$(mktemp -d /work/scratch/node-fuzz-selftest.XXXXXX)"
    trap 'chmod -R u+w "$selftest_dir" 2>/dev/null; rm -rf "$selftest_dir"' EXIT
    local dir="$selftest_dir"
    chmod 700 "$dir"
    mkdir -m 700 "$dir/fuzz" "$dir/fuzz/selftest_mismatch" "$dir/fuzz/selftest_mismatch/seeds"
    printf 'node-fuzz selftest' >"$dir/fuzz/selftest_mismatch/seeds/a"
    chmod 600 "$dir/fuzz/selftest_mismatch/seeds/a"
    mkfifo "$dir/tick"
    # A job file of the given lines for `--job`; prints its name.
    job_of() {
        local name
        name="job-$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
        mkdir -p -m 700 "$dir/fuzz/selftest_mismatch/jobs"
        printf '%s\n' "$@" >"$dir/fuzz/selftest_mismatch/jobs/$name"
        echo "$name"
    }
    job() { job_of "mode $1" "input $2"; }
    # Watches every process descended from this shell until watch_stop: none may show a
    # private input's name or a divergence's field in its command line or environment.
    # It returns once the watcher has scanned the process table once, so the job it watches is
    # never started before the watcher is looking (a slow python start under load missed it, BL-174).
    watch_start() {
        printf '%s\n' "$@" >"$dir/markers"
        rm -f "$dir/watch.ready"
        python3 -I - "$dir/markers" "$$" "$dir/watch" <<'EOF' &
import os, sys, time
markers = [m.encode() for m in open(sys.argv[1]).read().split("\n") if m]
root, out, me = int(sys.argv[2]), sys.argv[3], os.getpid()
seen, hits = set(), 0
while not os.path.exists(out + ".stop"):
    parent = {}
    for pid in os.listdir("/proc"):
        try:
            parent[int(pid)] = int(open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            pass
    for pid in parent:
        q, n = pid, 0
        while q != root and q in parent and n < 64:
            q, n = parent[q], n + 1
        if q != root or pid == me:
            continue
        for part in ("cmdline", "environ"):
            try:
                blob = open(f"/proc/{pid}/{part}", "rb").read()
            except OSError:
                continue
            seen.add(pid)
            hits += sum(m in blob for m in markers)
    if not os.path.exists(out + ".ready"):
        open(out + ".ready", "w").close()
    time.sleep(0.005)
open(out, "w").write(f"observed {len(seen)} hits {hits}\n")
EOF
        watcher=$!
        # Waits with a builtin on a pipe nothing writes to, so the wait starts no process the
        # watcher could count as observed.
        local tries=0
        until [ -e "$dir/watch.ready" ]; do
            kill -0 "$watcher" 2>/dev/null || fail "selftest: the process watcher died before it was ready"
            tries=$((tries + 1))
            [ "$tries" -le 1200 ] || fail "selftest: the process watcher was not ready within a minute"
            read -r -t 0.05 <>"$dir/tick" || true
        done
    }
    watch_stop() {
        local _ observed hits
        : >"$dir/watch.stop"
        wait "$watcher"
        rm -f "$dir/watch.stop"
        read -r _ observed _ hits <"$dir/watch"
        [ "$observed" -gt 0 ] && [ "$hits" = 0 ] ||
            fail "selftest: a private input's name or field reached a command line or environment"
    }

    # 16. BL-812: a coverage setting run.sh does not know, an under-test build of a target without
    # `under_test` or with the fuzz oracle, and a certify run or a merge off the default build are refused
    # before anything is created or built. (First: it builds nothing.)
    local host
    host="$(rustc -vV | sed -n 's/^host: //p')"
    before="$(find "$dir/fuzz" | sort)"
    refused() { # MESSAGE ENV... -- ARGS...
        local want=$1 envs=() status=0 out
        shift
        while [ "$1" != -- ]; do envs+=("$1"); shift; done
        shift
        out="$(env DASH_EMBARGO_DIR="$dir" "${envs[@]}" "$0" "$@" --pin v24.0 2>&1)" || status=$?
        [ "$status" -ne 0 ] && grep -qF -- "$want" <<<"$out" ||
            fail "selftest: BL-812: ${envs[*]} $* was not refused for: $want (rc $status)"
    }
    refused "NODE_FUZZ_COVERAGE must be all or under-test" NODE_FUZZ_COVERAGE=some -- selftest_mismatch --runs 1
    refused "NODE_FUZZ_TRACE_CMP must be 0 or 1" NODE_FUZZ_TRACE_CMP=2 -- selftest_mismatch --runs 1
    refused "DASH_ORACLE_FLAVOR=fuzz is refused" NODE_FUZZ_COVERAGE=under-test DASH_ORACLE_FLAVOR=fuzz -- selftest_mismatch --runs 1
    refused "diff_tx_decode has no under_test" NODE_FUZZ_COVERAGE=under-test -- diff_tx_decode --cpu-hours 1
    refused "--certify takes the default build only" NODE_FUZZ_TRACE_CMP=0 -- selftest_mismatch --certify --seconds 60
    refused "--merge takes the default build only" NODE_FUZZ_TRACE_CMP=0 -- selftest_mismatch --merge
    refused "--merge takes the default build only" NODE_FUZZ_COVERAGE=under-test -- selftest_mismatch --merge
    [ "$(find "$dir/fuzz" | sort)" = "$before" ] || fail "selftest: BL-812: a refused coverage setting created something"
    # sancov-rustc against a rustc that prints its arguments: the flags go to a listed crate of this
    # repository built for the target and to node-fuzz's bins, nothing else; forms it cannot read fail.
    local fake="$dir/fake-rustc" want got
    printf '#!/bin/sh\necho "$*"\n' >"$fake"
    chmod +x "$fake"
    wrapped() { # EXPECTED CARGO_MANIFEST_DIR CARGO_PKG_NAME RUSTC-ARGS...
        want=$1
        got="$(CARGO_MANIFEST_DIR="$2" CARGO_PKG_NAME="$3" NODE_FUZZ_SANCOV_CRATES=dash_chain,node_l1 \
            NODE_FUZZ_SANCOV_FLAGS="-Cfoo -Cbar" "$here/sancov-rustc" "$fake" "${@:4}" 2>/dev/null)" || got="rc $?"
        [ "$got" = "$want" ] || fail "selftest: BL-812: sancov-rustc ${*:2} gave [$got], want [$want]"
    }
    wrapped "--crate-name dash_chain --target x -Cfoo -Cbar" "$here/../dash-chain" dash-chain --crate-name dash_chain --target x
    wrapped "--crate-name dash_chain --target=x -Cfoo -Cbar" "$here/../dash-chain" dash-chain --crate-name dash_chain --target=x
    wrapped "--crate-name dash_chain" "$here/../dash-chain" dash-chain --crate-name dash_chain
    wrapped "--crate-name dash_chain --target x" "$dir" dash-chain --crate-name dash_chain --target x
    wrapped "--crate-name dash_kv --target x" "$here/../dash-kv" dash-kv --crate-name dash_kv --target x
    wrapped "--crate-name diff_chain_s0 --crate-type bin --target x -Cfoo -Cbar" "$here" node-fuzz \
        --crate-name diff_chain_s0 --crate-type bin --target x
    wrapped "--crate-name node_fuzz --crate-type lib --target x" "$here" node-fuzz --crate-name node_fuzz --crate-type lib --target x
    wrapped "rc 2" "$here" node-fuzz @args
    wrapped "rc 2" "$here/../dash-chain" dash-chain --crate-name=dash_chain --target x
    wrapped "rc 2" "$here/../dash-chain" dash-chain --crate-name dash_chain --crate-type=lib --target x
    # A crate is listed by its whole name: neither a longer name nor a part of a listed one is.
    wrapped "--crate-name dash_chainparams --target x" "$here/../dash-chainparams" dash-chainparams \
        --crate-name dash_chainparams --target x
    wrapped "--crate-name dash_chai --target x" "$here/../dash-chain" dash-chain --crate-name dash_chai --target x
    wrapped "-vV" "$here" node-fuzz -vV
    # The build's flags and directory as a run computes them (build_setup): the default build puts
    # sancov and trace-compares on every crate through RUSTFLAGS, in node-fuzz/target;
    # NODE_FUZZ_TRACE_CMP=0 drops trace-compares alone, in a directory of its own; under-test keeps
    # `--cfg fuzzing` in RUSTFLAGS and hands the flags and its crates to sancov-rustc.
    local nl=$'\n' cmp=-Cllvm-args=-sanitizer-coverage-trace-compares nocmp_flags default_flags cov_all cov_ut
    nocmp_flags="-Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4"
    nocmp_flags+=" -Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table"
    default_flags="--cfg fuzzing $nocmp_flags $cmp"
    setup_of() { # COVERAGE TRACE_CMP UNDER_TEST: the directory, then the build's environment, a line each
        (coverage=$1 trace_cmp=$2 under_test=$3; build_setup; printf '%s\n' "$target_dir" "${build_env[@]}")
    }
    got="$(setup_of all 1 '')"
    [ "$got" = "$here/target${nl}RUSTFLAGS=$default_flags" ] || fail "selftest: BL-812: the default build is [$got]"
    got="$(setup_of all 0 '')"
    cov_all="${got%%"$nl"*}"
    [[ "$cov_all" =~ ^"$here"/target/cov-[0-9a-f]{12}$ ]] && [ "${got#*"$nl"}" = "RUSTFLAGS=--cfg fuzzing $nocmp_flags" ] ||
        fail "selftest: BL-812: a NODE_FUZZ_TRACE_CMP=0 build is [$got]"
    got="$(setup_of under-test 1 "node_sink${nl}dash_chain")"
    cov_ut="${got%%"$nl"*}"
    [[ "$cov_ut" =~ ^"$here"/target/cov-[0-9a-f]{12}$ ]] && [ "$cov_ut" != "$cov_all" ] &&
        [ "${got#*"$nl"}" = "RUSTFLAGS=--cfg fuzzing${nl}RUSTC_WRAPPER=$here/sancov-rustc${nl}NODE_FUZZ_SANCOV_FLAGS=$nocmp_flags $cmp${nl}NODE_FUZZ_SANCOV_CRATES=node_sink,dash_chain" ] ||
        fail "selftest: BL-812: an under-test build is [$got]"
    got="$(setup_of under-test 0 node_sink)"
    [ "${got%%"$nl"*}" != "$cov_ut" ] && [[ "$got" == *"${nl}NODE_FUZZ_SANCOV_FLAGS=$nocmp_flags${nl}"* ]] ||
        fail "selftest: BL-812: an under-test build without trace-compares is [$got]"
    # read_under_test: fuzzctl's list, and its exit 4 (no list) or any other failure (a list its
    # check refuses) stop the run.
    local fake_here="$dir/fake-fuzzctl" rc
    mkdir -m 700 "$fake_here"
    for rc in 0 4 5; do
        printf 'import sys\nprint("node_sink")\nsys.exit(%s)\n' "$rc" >"$fake_here/fuzzctl.py"
        got="$( (here="$fake_here" target=t; read_under_test; echo "under_test=$under_test") 2>&1)" || got+=" rc $?"
        case "$rc:$got" in
            "0:under_test=node_sink") ;;
            "4:run.sh: t has no under_test in node-fuzz/targets.toml: NODE_FUZZ_COVERAGE=under-test is refused rc 2") ;;
            "5:run.sh: t's under_test in node-fuzz/targets.toml is not usable (see fuzzctl check) rc 2") ;;
            *) fail "selftest: BL-812: read_under_test after fuzzctl's exit $rc gave [$got]" ;;
        esac
    done
    # same_build (run under the build lock; fuzzctl_test pins where): the planned build stands
    # while nothing moved, and a changed under_test list or sancov-rustc fails the run. Here on a
    # copy of the wrapper and a fuzzctl that prints a list file.
    local change
    printf 'import os, sys\nprint(open(os.path.join(os.path.dirname(sys.argv[0]), "list")).read(), end="")\n' \
        >"$fake_here/fuzzctl.py"
    for change in none list wrapper; do
        printf 'node_sink\n' >"$fake_here/list"
        cp "$here/sancov-rustc" "$fake_here/sancov-rustc"
        got="$( (here="$fake_here" target=t coverage=under-test trace_cmp=0
                read_under_test
                build_setup
                case $change in
                    list) printf 'dash_chain\nnode_sink\n' >"$fake_here/list" ;;
                    wrapper) printf '# changed\n' >>"$fake_here/sancov-rustc" ;;
                esac
                same_build "$under_test:$target_dir"
                echo same) 2>&1)" || got+=" rc $?"
        case "$change:$got" in
            none:same) ;;
            list:*"changed while this run waited for the build lock rc 2") ;;
            wrapper:*"changed while this run waited for the build lock rc 2") ;;
            *) fail "selftest: BL-812: same_build after a change of the $change gave [$got]" ;;
        esac
    done

    # 1. A divergence fails the run, is recorded once, and its input is not printed.
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --runs 50 --run-tag 00000000000000aa 2>&1)" || status=$?
    runs=("$dir"/fuzz/selftest_mismatch/runs/*)
    [ "$status" -ne 0 ] || fail "selftest: a run with a divergence passed"
    [[ "${runs[0]}" == *-v24.0-*-00000000000000aa ]] || fail "selftest: the run id does not end in the run tag"
    grep -q 'status=fail' <<<"$out" || fail "selftest: no failing summary line"
    [ "$(find "${runs[0]}/divergences" -type f ! -name '*.txt' | wc -l)" = 1 ] ||
        fail "selftest: expected exactly one recorded divergence"
    ! grep -q 'node-fuzz selftest' <<<"$out" || fail "selftest: the input appeared in the output"

    # 2. A divergence the sink cannot record stops the target, and the run fails.
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" NODE_FUZZ_SELFTEST_READONLY=1 "$0" selftest_mismatch --pin v24.0 --runs 50 2>&1)" || status=$?
    [ "$status" -ne 0 ] || fail "selftest: a run with an unwritable sink passed"
    grep -q 'sink_failures=1' <<<"$out" || fail "selftest: the sink failure was not counted"

    # 3. A run directory the runner cannot create stops the run with a fixed diagnostic.
    chmod 500 "$dir/fuzz/selftest_mismatch/runs"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --runs 50 2>&1)" || status=$?
    chmod 700 "$dir/fuzz/selftest_mismatch/runs"
    [ "$status" -ne 0 ] || fail "selftest: a run with an unwritable sink passed"
    grep -q 'the private sink is not writable' <<<"$out" || fail "selftest: no sink diagnostic"

    # 4. Merging the working corpus runs libFuzzer's merge children, which the target accepts.
    # libFuzzer may have saved the planted input to the working corpus in case 1 (it keeps every
    # input that adds coverage); the merge runs over agreeing inputs only.
    rm -f "$dir/fuzz/selftest_mismatch/seeds/a"
    for f in "$dir/fuzz/selftest_mismatch/corpus"/*; do
        ! cmp -s "$f" <(printf 'node-fuzz selftest') || rm -f "$f"
    done
    printf 'another input' >"$dir/fuzz/selftest_mismatch/seeds/b"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --merge 2>&1)" || status=$?
    [ "$status" -eq 0 ] && grep -q 'mode=merge .*status=pass' <<<"$out" || fail "selftest: the merge failed"

    # 5. Minimising a crash of an earlier run passes only when libFuzzer wrote the minimised
    # input; the job is consumed, and no process shows the crash file's name.
    local crashes="$dir/fuzz/selftest_mismatch/runs/earlier/crashes" name
    mkdir -p -m 700 "$dir/fuzz/selftest_mismatch/runs/earlier" "$crashes"
    printf 'node-fuzz crash' >"$crashes/crash-a"
    name="$(job minimize "$crashes/crash-a")"
    watch_start /crashes/crash- /divergences/ planted
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$name" 2>&1)" || status=$?
    watch_stop
    [ "$status" -eq 0 ] && grep -q 'mode=minimize .*status=pass' <<<"$out" &&
        [ "$(find "$dir/fuzz/selftest_mismatch/runs" -path '*/crashes/minimized' -type f | wc -l)" = 1 ] ||
        fail "selftest: the minimisation failed"
    [ ! -e "$dir/fuzz/selftest_mismatch/jobs/$name" ] || fail "selftest: the job was not consumed"

    # 6. A job whose input does not exist or lies outside this target's runs and regression
    # vectors, a job name that is not one, and a missing job are refused before anything is
    # created or built.
    local before
    before="$(find "$dir/fuzz/selftest_mismatch/runs" -mindepth 1 -maxdepth 1 | wc -l)"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job minimize "$crashes/missing")" 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'the input is not an existing regular file' <<<"$out" ||
        fail "selftest: a missing crash file was not refused"
    for elsewhere in "$crashes/../../earlier/crashes/crash-a" "$dir/fuzz/crash-a"; do
        status=0
        out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job minimize "$elsewhere")" 2>&1)" || status=$?
        [ "$status" -ne 0 ] && grep -q 'not a finding or regression vector of this target' <<<"$out" ||
            fail "selftest: a crash path outside the target's runs was not refused"
    done
    for bad in job-nothex "job-$(printf '%032d' 0)"; do
        status=0
        out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$bad" 2>&1)" || status=$?
        [ "$status" -ne 0 ] && grep -q -- '--job: ' <<<"$out" || fail "selftest: a bad job name was not refused"
    done
    [ "$(find "$dir/fuzz/selftest_mismatch/runs" -mindepth 1 -maxdepth 1 | wc -l)" = "$before" ] ||
        fail "selftest: a refused minimisation created a run"

    # 7. An input that does not crash fails the minimisation (libFuzzer exits non-zero).
    printf 'another input' >"$crashes/crash-b"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job minimize "$crashes/crash-b")" 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'mode=minimize .*status=fail' <<<"$out" ||
        fail "selftest: a minimisation that found no crash passed"

    # 8. Without DASH_EMBARGO_DIR nothing runs.
    status=0
    out="$(env -u DASH_EMBARGO_DIR "$0" selftest_mismatch --pin v24.0 --runs 50 2>&1)" || status=$?
    [ "$status" -ne 0 ] || fail "selftest: a run without DASH_EMBARGO_DIR started"

    # 9. The target refuses to start outside the runner, before any input runs.
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$here/target/$(rustc -vV | sed -n 's/^host: //p')/release/selftest_mismatch" -runs=1 2>&1)" || status=$?
    [ "$status" -eq 2 ] && grep -q 'refusing to run' <<<"$out" ||
        fail "selftest: the target ran outside the runner"
    # 10. A recorded divergence minimises (the field's divergence aborts, nothing is recorded);
    # a minimisation for another field finds nothing. No process shows the divergence's name
    # (its hash) or its field.
    local divergence sha
    divergence="$(find "${runs[0]}/divergences" -type f ! -name '*.txt' -print -quit)"
    sha="${divergence##*/}"
    local other="$dir/fuzz/selftest_mismatch/runs/earlier/divergences"
    mkdir -m 700 "$other"
    cp "$divergence" "$other/"
    printf 'target selftest_mismatch\npin v24.0\nfield zq_other_field_7f3a\n' >"$other/$sha.txt"
    local j1 j2
    j1="$(job minimize-divergence "$divergence")"
    j2="$(job minimize-divergence "$other/$sha")"
    watch_start "$sha" /divergences/ planted zq_other_field_7f3a
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$j1" 2>&1)" || status=$?
    local status2=0
    DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$j2" >/dev/null 2>&1 || status2=$?
    watch_stop
    [ "$status" -eq 0 ] && grep -q 'mode=minimize-divergence .*status=pass' <<<"$out" ||
        fail "selftest: the divergence minimisation failed"
    # The same, deterministically: every program the job executes, with its arguments.
    if command -v strace >/dev/null; then
        printf '%s\n' "$sha" /divergences/ planted >"$dir/markers"
        strace -f -qq -s 4096 -e trace=execve -o "$dir/trace" \
            env DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job minimize-divergence "$divergence")" >/dev/null 2>&1 ||
            fail "selftest: the traced divergence minimisation failed"
        ! grep -q -F -f "$dir/markers" "$dir/trace" ||
            fail "selftest: a private input's name or field reached a command line"
    else
        echo "node-fuzz selftest: strace is missing; the execve check is skipped" >&2
    fi
    [ "$status2" -ne 0 ] || fail "selftest: a minimisation for a field the input does not diverge in passed"
    # A sidecar that names a command (a model target's divergence does) puts it in the abort file,
    # and the minimisation keeps only a divergence on that command: this target diverges on none,
    # so it finds nothing even in the input's own field. A command the sidecar spells badly is
    # refused before anything is created (BL-267).
    local on_cmd="$dir/fuzz/selftest_mismatch/runs/earlier2/divergences" own_field
    own_field="$(sed -n 's/^field //p' "$divergence.txt")"
    mkdir -p -m 700 "$on_cmd"
    cp "$divergence" "$on_cmd/"
    printf 'target selftest_mismatch\npin v24.0\nfield %s\ncmd qgetdata\n' "$own_field" >"$on_cmd/$sha.txt"
    status=0
    DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job minimize-divergence "$on_cmd/$sha")" >/dev/null 2>&1 || status=$?
    [ "$status" -ne 0 ] || fail "selftest: a minimisation for a command the input does not diverge on passed"
    [ "$(printf '%s\n' "$dir"/fuzz/selftest_mismatch/runs/*/abort-field | xargs -d '\n' grep -lx 'cmd qgetdata' | wc -l)" = 1 ] ||
        fail "selftest: the sidecar's command did not reach the abort file"
    printf 'target selftest_mismatch\npin v24.0\nfield %s\ncmd Bad Cmd\n' "$own_field" >"$on_cmd/$sha.txt"
    before="$(find "$dir/fuzz/selftest_mismatch/runs" -mindepth 1 -maxdepth 1 | wc -l)"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job minimize-divergence "$on_cmd/$sha")" 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'a bad command in its sidecar' <<<"$out" ||
        fail "selftest: a bad command in a sidecar was not refused"
    [ "$(find "$dir/fuzz/selftest_mismatch/runs" -mindepth 1 -maxdepth 1 | wc -l)" = "$before" ] ||
        fail "selftest: a refused minimisation created a run"

    # 11. A replay fails on a divergent input and passes on an agreeing one; a replay with
    # nothing to run fails.
    j1="$(job replay "$divergence")"
    watch_start "$sha" /divergences/ planted
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$j1" 2>&1)" || status=$?
    watch_stop
    [ "$status" -ne 0 ] && grep -q 'mode=replay .*divergences=1 .*status=fail' <<<"$out" ||
        fail "selftest: a replay of a divergence passed"
    ! grep -q 'node-fuzz selftest' <<<"$out" || fail "selftest: the input appeared in the output"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job replay "$crashes/crash-b")" 2>&1)" || status=$?
    [ "$status" -eq 0 ] && grep -q 'mode=replay .*status=pass' <<<"$out" ||
        fail "selftest: a replay of an agreeing input failed"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job replay "$other/$sha.txt")" 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'nothing to run' <<<"$out" || fail "selftest: an empty replay passed"
    # A fixed-order replay runs its inputs (here a file, a directory of two files, the file
    # again) each from a directory of its own, in the job's order, with libFuzzer's shuffle and
    # size sort off (with `strace` installed, checked in its arguments), and no process shows an
    # input's name. More than one input without `order fixed`, `order` other than `fixed` or
    # outside a replay, a fixed-order job without inputs, and one of its inputs that has nothing
    # to run, are refused.
    local ordered="$dir/fuzz/selftest_mismatch/regress/ordered" tracer=() k dirs=() lines parts
    local want=("input-1" "input-2 input-3" "input-4") from=("$crashes/crash-b" "$ordered/1" "$ordered/2" "$crashes/crash-b")
    command -v strace >/dev/null && tracer=(strace -f -qq -s 4096 -e trace=execve -o "$dir/trace")
    rm -f "$dir/trace"
    mkdir -m 700 "$dir/fuzz/selftest_mismatch/regress" "$ordered"
    printf 'a longer agreeing input' >"$ordered/1"
    printf 'short' >"$ordered/2"
    watch_start crash-b /regress/ordered
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "${tracer[@]}" "$0" selftest_mismatch --pin v24.0 \
        --run-tag 00000000000000f1 --job "$(job_of "mode replay" "order fixed" "input $crashes/crash-b" "input $ordered" \
        "input $crashes/crash-b")" 2>&1)" || status=$?
    watch_stop
    [ "$status" -eq 0 ] && grep -q 'mode=replay .*status=pass' <<<"$out" || fail "selftest: a fixed-order replay failed"
    runs=("$dir"/fuzz/selftest_mismatch/runs/*-00000000000000f1)
    for k in 1 2 3; do
        [ "$(cd "${runs[0]}/input/$k" && echo *)" = "${want[k - 1]}" ] || fail "selftest: a fixed-order replay staged input $k wrongly"
        dirs+=("${runs[0]}/input/$k")
    done
    for k in 1 2 3 4; do
        cmp -s "${from[k - 1]}" "${runs[0]}"/input/*/input-$k || fail "selftest: a fixed-order replay staged a file wrongly"
    done
    [ "$(grep -o '[0-9]* files found in .*' "${runs[0]}"/logs/segment-1/fuzz.out)" = \
        "$(printf '%s files found in %s\n' 1 "${dirs[0]}" 2 "${dirs[1]}" 1 "${dirs[2]}")" ] ||
        fail "selftest: a fixed-order replay did not read its inputs in order"
    [ "${#tracer[@]}" -eq 0 ] || grep -q -F -- "-shuffle=0\", \"-prefer_small=0\", \"${dirs[0]}\", \"${dirs[1]}\", \"${dirs[2]}\"]" "$dir/trace" ||
        fail "selftest: a fixed-order replay left libFuzzer's shuffle or size sort on"
    [ "${#tracer[@]}" -eq 0 ] || ! grep -q -F -e crash-b -e /regress/ordered "$dir/trace" ||
        fail "selftest: a fixed-order replay's input name reached a command line"
    for lines in "mode replay|input $crashes/crash-b|input $crashes/crash-b" \
        "mode minimize|order fixed|input $crashes/crash-a" "mode replay|order random|input $crashes/crash-b" \
        "mode replay|order|input $crashes/crash-b|input $crashes/crash-b" "mode replay|order fixed"; do
        status=0
        IFS='|' read -r -a parts <<<"$lines"
        out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job_of "${parts[@]}")" 2>&1)" || status=$?
        [ "$status" -ne 0 ] && grep -q 'a malformed job' <<<"$out" || fail "selftest: a malformed ordered job ran ($lines)"
    done
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job_of "mode replay" "order fixed" \
        "input $crashes/crash-b" "input $other/$sha.txt")" 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'an ordered input has nothing to run' <<<"$out" ||
        fail "selftest: a fixed-order replay dropped an input that has nothing to run"

    # 12. A target or pin the registry does not list, or a bad run tag, is refused before
    # anything is created.
    before="$(find "$dir/fuzz" -mindepth 1 -maxdepth 1 | wc -l)"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --runs 1 --run-tag x 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'run-tag must be' <<<"$out" || fail "selftest: a bad run tag was not refused"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" not_a_target --pin v24.0 --cpu-hours 1 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'not in node-fuzz/targets.toml' <<<"$out" ||
        fail "selftest: an unregistered target was not refused"
    local unlisted
    unlisted="$(python3 -I - "$here/targets.toml" <<'EOF'
import sys, tomllib
for t in tomllib.load(open(sys.argv[1], "rb"))["target"]:
    missing = [p for p in ("v24.0",) if t["kind"] != "selftest" and p not in t.get("pins", [])]
    if missing:
        print(t["name"], missing[0])
        break
EOF
)"
    if [ -n "$unlisted" ]; then
        status=0
        out="$(DASH_EMBARGO_DIR="$dir" "$0" ${unlisted% *} --pin ${unlisted#* } --cpu-hours 1 2>&1)" || status=$?
        [ "$status" -ne 0 ] && grep -q 'not one of the target' <<<"$out" ||
            fail "selftest: a pin the registry does not list was not refused"
    fi
    [ "$(find "$dir/fuzz" -mindepth 1 -maxdepth 1 | wc -l)" = "$before" ] ||
        fail "selftest: a refused target created a directory"

    # 13. A certify run executes its own copy of the binary, whose sha256 its source record
    # keeps, and removes the copy (here the merge finds the planted divergence, so no window runs).
    printf 'node-fuzz selftest' >"$dir/fuzz/selftest_mismatch/seeds/a"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --certify --seconds 60 --run-tag 00000000000000c1 2>&1)" ||
        status=$?
    runs=("$dir"/fuzz/selftest_mismatch/runs/*-00000000000000c1)
    local built
    built="$(sha256sum "$here/target/$(rustc -vV | sed -n 's/^host: //p')/release/selftest_mismatch" | cut -c1-64)"
    [ "$status" -ne 0 ] && grep -q 'mode=certify .*status=fail' <<<"$out" &&
        [ "$(sed -n 3,4p "${runs[0]}/source")" = "${runs[0]##*/}"$'\n'"$built" ] && [ ! -e "${runs[0]}/bin" ] ||
        fail "selftest: a certify run's binary copy or its source record is wrong"

    # 14. The leg's lock: a certify run excludes fuzzing runs and merges of its target, and they
    # exclude it; jobs take no lock.
    exec 7>>"$dir/fuzz/selftest_mismatch/run.lock"
    flock -s 7
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --certify --seconds 60 2>&1 7>&-)" || status=$?
    [ "$status" -ne 0 ] && grep -q 'another run holds the leg' <<<"$out" ||
        fail "selftest: a certify run started while a fuzzing run held its leg"
    flock -u 7
    flock -x 7
    for mode in "--runs 1" --merge; do
        status=0
        out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 $mode 2>&1 7>&-)" || status=$?
        [ "$status" -ne 0 ] && grep -q 'another run holds the leg' <<<"$out" ||
            fail "selftest: a run ($mode) started while a certify run held its leg"
    done
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --job "$(job replay "$crashes/crash-b")" 2>&1 7>&-)" ||
        status=$?
    [ "$status" -eq 0 ] && grep -q 'mode=replay .*status=pass' <<<"$out" || fail "selftest: the leg's lock blocked a job"
    exec 7>&-

    # 15. A sampled run (the smoke's, BL-333) still loads every private seed, here the planted
    # divergence, with no sample at all; it loads the sample it is given, and it leaves the
    # working corpus exactly as it was and no sample behind.
    local tree="$dir/fuzz/selftest_mismatch" before_sample
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --runs 50 --corpus-sample 0 --corpus-sample-bytes 0 2>&1)" ||
        status=$?
    [ "$status" -ne 0 ] && grep -q 'divergences=[1-9].*status=fail (divergences)' <<<"$out" ||
        fail "selftest: a sampled run did not load the private seeds"
    rm -f "$tree/seeds/a"
    for f in "$tree/corpus"/*; do
        ! cmp -s "$f" <(printf 'node-fuzz selftest') || rm -f "$f"
    done
    for k in 1 2 3 4 5; do printf 'sampled input %s' "$k" >"$tree/corpus/sample-$k"; done
    corpus_state() { (cd "$tree/corpus" && find . -type f -printf '%p %s %T@ %i\n' | sort && find . -type f -exec sha256sum {} + | sort); }
    before_sample="$(corpus_state)"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --runs 50 --run-tag 00000000000000e1 \
        --corpus-sample 2 --corpus-sample-bytes 4096 2>&1)" || status=$?
    runs=("$tree"/runs/*-00000000000000e1)
    [ "$status" -eq 0 ] && grep -q 'status=pass' <<<"$out" ||
        fail "selftest: a sampled run failed"
    grep -q "^INFO: *2 files found in ${runs[0]}/sample\$" "${runs[0]}/logs/segment-1/fuzz.out" ||
        fail "selftest: a sampled run did not load its sample"
    [ "$(corpus_state)" = "$before_sample" ] || fail "selftest: a sampled run changed the working corpus"
    [ ! -e "${runs[0]}/sample" ] || fail "selftest: a sampled run left its sample behind"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --merge --corpus-sample 2 --corpus-sample-bytes 1 2>&1)" ||
        status=$?
    [ "$status" -ne 0 ] && grep -q 'is for fuzzing runs' <<<"$out" || fail "selftest: a sampled merge was not refused"

    # 14. G-ENC-5 (O2b-4 §3a.3): a chain target (the registry's own rows) refuses before anything
    # is created, copied or built: a tree marked with another encoding, an unmarked tree that holds
    # inputs (the tree a smoke left before the marker among them), an unmarked tree that cannot
    # take the marker, and --public-seeds from any source. The tree stays as it was (no marker, no
    # copy, no run directory, so no build), and no process executes a chain target's binary.
    enc_tree() {  # <target>: every path under its tree, with its size, mtime and contents
        (cd "$dir/fuzz/$1" && find . -printf '%p %y %s %T@\n' | sort && find . -type f -exec sha256sum {} + | sort)
    }
    enc_case() {  # <diagnostic> <target> [run.sh options...]
        local want="$1" t="$2" before
        shift 2
        before="$(enc_tree "$t")"
        watch_start /release/diff_chain
        status=0
        out="$(DASH_EMBARGO_DIR="$dir" "$0" "$t" --pin v24.0 --cpu-hours 1 "$@" 2>&1)" || status=$?
        watch_stop
        [ "$status" -ne 0 ] && grep -qF -- "$want" <<<"$out" ||
            fail "selftest: G-ENC-5: $t $* was not refused for: $want (rc $status: $(tail -n 1 <<<"$out"))"
        [ "$(enc_tree "$t")" = "$before" ] || fail "selftest: G-ENC-5: a refused run of $t changed its tree"
    }
    local v2="$dir/fuzz/diff_chain_s0" seeds="$dir/enc-seeds"
    enc_reset() { rm -rf "$v2" "$seeds"; mkdir -m 700 "$v2" "$seeds"; }
    # A v2 tree marked chain-v1.
    enc_reset
    printf 'chain-v1\n' >"$v2/ENCODING"
    enc_case "encoding-mismatch" diff_chain_s0
    # An unmarked v2 tree with one corpus file: refused, and no marker written.
    enc_reset
    mkdir -m 700 "$v2/corpus"
    printf 'x' >"$v2/corpus/0"
    enc_case "has no ENCODING marker and holds inputs" diff_chain_s0
    # A floor-gated target may run sharded (BL-841b §4): --shard reaches the same refusal.
    enc_case "has no ENCODING marker and holds inputs" diff_chain_s0 --jobs 2 --shard
    # An empty unmarked v2 tree that cannot take the marker (tree_encoding.py's exit 2).
    enc_reset
    chmod 500 "$v2"
    if [ -w "$v2" ]; then
        echo "node-fuzz selftest: G-ENC-5: a 0500 tree is writable here (euid 0); the unwritable-marker case is skipped" >&2
    else
        enc_case "the private tree's ENCODING marker cannot be read or written" diff_chain_s0
    fi
    chmod 700 "$v2"
    # (a1) A marked v2 tree and --public-seeds from an unlabelled directory of chain-shaped bytes.
    enc_reset
    printf 'chain-v2\n' >"$v2/ENCODING"
    mkdir -m 700 "$v2/public-seeds"
    printf '\x00\x13\x1e' >"$seeds/a"
    enc_case "--public-seeds is refused for chain target diff_chain_s0" diff_chain_s0 --public-seeds "$seeds"
    # (a2) The same with a directory labelled chain-v1.
    printf 'chain-v1\n' >"$seeds/ENCODING"
    enc_case "--public-seeds is refused for chain target diff_chain_s0" diff_chain_s0 --public-seeds "$seeds"
    # (a3) The legacy row refuses --public-seeds too.
    mkdir -m 700 "$dir/fuzz/diff_chain"
    printf 'chain-v1\n' >"$dir/fuzz/diff_chain/ENCODING"
    enc_case "--public-seeds is refused for chain target diff_chain" diff_chain --public-seeds "$seeds"
    rm -rf "$dir/fuzz/diff_chain"
    # (b) An unmarked tree whose only input is a public seed, as a smoke before the marker left it
    # (OD-L3-96 Q6): corpus/ empty, public-seeds/ the compile, runs/ present. So for diff_chain_av
    # (Q6's 11-07 addition).
    local t smoke
    for t in diff_chain_s0 diff_chain_av; do
        smoke="$dir/fuzz/$t"
        rm -rf "$smoke"
        mkdir -m 700 "$smoke" "$smoke/corpus" "$smoke/public-seeds" "$smoke/runs" "$smoke/runs/20261101T000000Z-v24.0-1"
        cp "$here/public-seeds/$t"/* "$smoke/public-seeds/"
        enc_case "has no ENCODING marker and holds inputs" "$t"
        [ ! -e "$smoke/ENCODING" ] || fail "selftest: G-ENC-5: a refused tree was marked"
        rm -rf "$smoke"
    done
    rm -rf "$v2" "$seeds"

    # 17. BL-812: an under-test build without trace-compares runs, links the plain oracle and says
    # so in its verdict, builds in a directory of its own (the default binary stays as it was),
    # and instruments exactly its registry's under_test (node-sink) among the libraries: counters
    # there, no comparison callbacks anywhere, nothing in any other crate. It loads a run-local copy
    # of every input, and the working corpus stays as it was. It runs no mutation (-runs=0: the
    # inputs only), so it cannot reach the planted divergence, which no input holds: libFuzzer's
    # seed is the clock's, and mutating the inputs earlier cases left in the corpus can reach it.
    local default_bin="$here/target/$host/release/selftest_mismatch" default_sum cov_bin deps rlib crate counted=() plain=0 n_inputs
    default_sum="$(sha256sum <"$default_bin")"
    before_sample="$(corpus_state)"
    n_inputs="$( (ls -A "$tree/corpus"; ls -A "$tree/public-seeds") | sort -u | wc -l)"
    for f in "$tree"/corpus/* "$tree"/public-seeds/* "$tree"/seeds/* "$tree"/regress/fixed/*; do
        [ ! -f "$f" ] || ! cmp -s "$f" <(printf 'node-fuzz selftest') ||
            fail "selftest: BL-812: an input of the under-test run holds the planted divergence ($f)"
    done
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" NODE_FUZZ_COVERAGE=under-test NODE_FUZZ_TRACE_CMP=0 "$0" selftest_mismatch --pin v24.0 \
        --runs 0 --run-tag 00000000000000b8 2>&1)" || status=$?
    runs=("$tree"/runs/*-00000000000000b8)
    [ "$status" -eq 0 ] && grep -q 'oracle_flavor=plain coverage=under-test rust_trace_cmp=0 status=pass' <<<"$out" ||
        fail "selftest: BL-812: an under-test build's run failed (rc $status: $(tail -n 1 <<<"$out"))"
    python3 -I -c 'import json, sys; v = json.load(open(sys.argv[1])); sys.exit(not (v["coverage"] == "under-test" and v["rust_trace_cmp"] is False))' \
        "${runs[0]}/verdict.json" || fail "selftest: BL-812: the verdict does not record the under-test build"
    grep -q "^INFO: *$n_inputs files found in ${runs[0]}/sample\$" "${runs[0]}/logs/segment-1/fuzz.out" ||
        fail "selftest: BL-812: an under-test run did not fuzz a copy of all $n_inputs inputs"
    [ "$(corpus_state)" = "$before_sample" ] || fail "selftest: BL-812: an under-test run changed the working corpus"
    [ ! -e "${runs[0]}/sample" ] || fail "selftest: BL-812: an under-test run left its copy behind"
    [ "$(sha256sum <"$default_bin")" = "$default_sum" ] || fail "selftest: BL-812: an under-test build replaced the default binary"
    cov_bin="$(sed -n 's/^node-fuzz: bin //p' "${runs[0]}/logs/build.log")"
    case "$cov_bin" in "$here"/target/cov-*/"$host"/release/selftest_mismatch) ;;
        *) fail "selftest: BL-812: the under-test binary ($cov_bin) is not under node-fuzz/target/cov-*" ;;
    esac
    # Counts, not `grep -q`: under pipefail a reader that exits at its first match makes a writer
    # whose SIGPIPE is not ignored fail the whole test (fuzzctl starts run.sh with SIGPIPE at its
    # default). An rlib's readelf output runs to hundreds of KB, far more than a pipe holds.
    [ "$(readelf -SW "$cov_bin" | grep -c __sancov_cntrs)" -gt 0 ] || fail "selftest: BL-812: the under-test binary has no counters"
    deps="${cov_bin%/selftest_mismatch}/deps"
    for rlib in "$deps"/lib*.rlib; do
        crate="${rlib##*/lib}"
        crate="${crate%-*.rlib}"
        if [ "$(readelf -SW "$rlib" 2>/dev/null | grep -c __sancov_cntrs)" -gt 0 ]; then counted+=("$crate"); else plain=$((plain + 1)); fi
        [ "$(nm -u "$rlib" 2>/dev/null | grep -cE '__sanitizer_cov_trace_(const_)?(cmp|switch)')" -eq 0 ] ||
            fail "selftest: BL-812: $crate calls trace-compare callbacks in a build without them"
    done
    [ "${counted[*]}" = node_sink ] && [ "$plain" -ge 20 ] ||
        fail "selftest: BL-812: the under-test build instrumented [${counted[*]}] (want node_sink) and left $plain crates plain"
    # The flags cargo recorded for each unit it built for the target: the default build's on every
    # one in node-fuzz/target, and `--cfg fuzzing` alone in the under-test directory (the wrapper
    # adds the rest). The default build's libraries call the trace-compare callbacks. Read under the
    # build lock, so no build writes them meanwhile.
    exec 6>>"$here/target/.node-fuzz-build.lock"
    flock -x 6
    python3 -I - "$here/target/$host/release/.fingerprint" "$default_flags" "${cov_bin%/selftest_mismatch}/.fingerprint" <<'EOF' ||
import glob, json, os, sys
for d, want in ((sys.argv[1], sys.argv[2].split()), (sys.argv[3], ["--cfg", "fuzzing"])):
    units = {f: json.load(open(f)).get("rustflags") for f in glob.glob(os.path.join(d, "*", "*.json"))
             if os.path.basename(f).startswith(("lib-", "bin-"))}
    bad = sorted(os.path.basename(os.path.dirname(f)) for f, flags in units.items() if flags != want)
    if len(units) < 20 or bad:
        sys.exit(f"{d}: {len(units)} units, {len(bad)} without {want}: {bad[:5]}")
EOF
        fail "selftest: BL-812: a unit was built without its build's flags (one built by hand with other RUSTFLAGS? remove node-fuzz/target/$host/release and run again)"
    plain=0
    for rlib in "$here/target/$host/release/deps"/lib*.rlib; do
        if [ "$(nm -u "$rlib" 2>/dev/null | grep -cE '__sanitizer_cov_trace_(const_)?cmp')" -gt 0 ]; then plain=$((plain + 1)); fi
    done
    [ "$plain" -ge 20 ] || fail "selftest: BL-812: only $plain of the default build's libraries call trace-compare callbacks"
    exec 6>&-

    # 18. BL-841: --minimize-corpus merges the working corpus alone into its run's cmin/ and swaps it
    # in by two renames: the new corpus is a subset of the old with the same cov and ft, and the old
    # one is kept as corpus.pre-cmin (0700), which run.sh never deletes. A merge with a finding
    # leaves the corpus as it was. A kill before the renames leaves the old corpus, and the killed
    # run's cmin/ is never adopted; between them, every run refuses the tree (cmin-interrupted, nothing
    # created) and the next minimisation rolls it back, before it refuses for anything else; after
    # them, the new corpus stands and a minimisation is refused while the old one is kept. Each
    # refused combination creates nothing. During the merge another process can take the triage
    # lock, not the leg's or the run's liveness lock (its live file). A merge that exits non-zero
    # swaps nothing. A swap cut just after a run's first check: the run checks again under the
    # leg's lock and refuses, and a job creates no corpus.
    local cdir="$tree/corpus" pre="$tree/corpus.pre-cmin" k f want_features
    rm -rf "$tree/seeds" "$tree/regress" "$pre"
    rm -f "$tree"/jobs/job-*  # earlier cases' refused jobs: a minimisation refuses while one waits
    find "$cdir" -mindepth 1 -delete
    for k in 1 2 3 4 5 6 7 8; do printf 'cmin input %s' "$k" >"$cdir/in-$k"; done
    cmin_features() {  # <run tag>: the INITED line of a replay of the corpus (-runs=0: no mutation)
        local o rc=0 r
        o="$(DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --runs 0 --run-tag "$1" 2>&1)" || rc=$?
        [ "$rc" -eq 0 ] || fail "selftest: BL-841: a replay of the corpus failed (rc $rc: $(tail -n 1 <<<"$o"))"
        r=("$tree"/runs/*-"$1")
        grep -m 1 -oE 'INITED cov: [0-9]+ ft: [0-9]+' "${r[0]}/logs/segment-1/fuzz.out" ||
            fail "selftest: BL-841: a replay of the corpus printed no INITED"
    }
    cmin_sums() { (cd "$1" && find . -maxdepth 1 -type f -exec sha1sum {} + | cut -c1-40 | sort -u); }
    cmin_run() {  # <run tag> [ENV...]: a minimisation; sets $status and $out
        local tag="$1"
        shift
        status=0
        out="$(env "$@" DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --minimize-corpus --run-tag "$tag" 2>&1)" ||
            status=$?
    }
    cmin_refused() {  # <diagnostic> [ENV...] -- <run.sh arguments...>: refused, and nothing created
        local want="$1" envs=() before
        shift
        while [ "$1" != -- ]; do envs+=("$1"); shift; done
        shift
        before="$(find "$dir/fuzz" -printf '%p %s %T@\n' | sort)"
        status=0
        out="$(env "${envs[@]}" DASH_EMBARGO_DIR="$dir" "$0" "$@" 2>&1 7>&-)" || status=$?
        [ "$status" -ne 0 ] && grep -qF -- "$want" <<<"$out" ||
            fail "selftest: BL-841: $* was not refused for: $want (rc $status: $(tail -n 1 <<<"$out"))"
        [ "$(find "$dir/fuzz" -printf '%p %s %T@\n' | sort)" = "$before" ] ||
            fail "selftest: BL-841: a refused run ($*) created or changed something"
    }
    want_features="$(cmin_features 00000000000000c0)"
    # Refusals: another mode, budget or sample; an opt-in build; a floor-gated target; an unreadable
    # triage state; a job waiting; a held triage lock; a held leg lock. (The lock files exist first.)
    : >>"$dir/fuzz/.triage.lock"
    : >>"$tree/run.lock"
    local m="takes no other mode, budget or sample"
    cmin_refused "$m" -- selftest_mismatch --pin v24.0 --minimize-corpus --certify --seconds 60
    cmin_refused "$m" -- selftest_mismatch --pin v24.0 --merge --minimize-corpus
    cmin_refused "$m" -- selftest_mismatch --pin v24.0 --minimize-corpus --job "job-$(printf '%032d' 0)"
    cmin_refused "$m" -- selftest_mismatch --pin v24.0 --minimize-corpus --corpus-sample 2 --corpus-sample-bytes 1
    cmin_refused "$m" -- selftest_mismatch --pin v24.0 --minimize-corpus --cpu-hours 1
    cmin_refused "$m" -- selftest_mismatch --pin v24.0 --minimize-corpus --runs 1
    cmin_refused "$m" -- selftest_mismatch --pin v24.0 --minimize-corpus --jobs 2
    cmin_refused "--minimize-corpus takes the default build only" NODE_FUZZ_TRACE_CMP=0 -- selftest_mismatch --pin v24.0 --minimize-corpus
    cmin_refused "--minimize-corpus takes the default build only" NODE_FUZZ_COVERAGE=under-test -- selftest_mismatch --pin v24.0 --minimize-corpus
    cmin_refused "cmin-floor-gated" -- diff_chain_s0 --pin v24.0 --minimize-corpus
    printf 'not json' >"$tree/triage.json"
    cmin_refused "triage state is unreadable" -- selftest_mismatch --pin v24.0 --minimize-corpus
    rm -f "$tree/triage.json"
    f="$(job_of "mode replay" "input $cdir/in-1")"
    cmin_refused "has a job waiting" -- selftest_mismatch --pin v24.0 --minimize-corpus
    rm -f "$tree/jobs/$f"
    exec 7>>"$dir/fuzz/.triage.lock"
    flock -x 7
    cmin_refused "a triage holds the private tree" -- selftest_mismatch --pin v24.0 --minimize-corpus
    exec 7>&-
    exec 7>>"$tree/run.lock"
    flock -s 7
    cmin_refused "another run holds the leg" -- selftest_mismatch --pin v24.0 --minimize-corpus
    exec 7>&-
    # The minimisation: a strict subset, the same features, the old corpus kept 0700.
    local old_sums
    old_sums="$(cmin_sums "$cdir")"
    cmin_run 00000000000000c1 NODE_FUZZ_SELFTEST_CMIN_LOCKS="$dir/cmin-locks"
    [ "$status" -eq 0 ] && grep -q 'mode=minimize-corpus .*status=pass' <<<"$out" ||
        fail "selftest: BL-841: the minimisation failed (rc $status: $(tail -n 1 <<<"$out"))"
    [ "$(cat "$dir/cmin-locks")" = "$(printf 'triage=free\nleg=held\nrun=held')" ] ||
        fail "selftest: BL-841: after the merge, the locks were not: triage free, the leg and the run held ($(tr '\n' ' ' <"$dir/cmin-locks"))"
    grep -qE ' cmin_files_before=8 cmin_bytes_before=[0-9]+ cmin_files_after=[1-7] cmin_bytes_after=[0-9]+ status=pass$' <<<"$out" ||
        fail "selftest: BL-841: the minimisation kept no strict subset, or its line lacks the counts"
    ! grep -qF "$dir" <<<"$(grep '^node-fuzz ' <<<"$out")" || fail "selftest: BL-841: the summary line names a path"
    [ -z "$(comm -13 <(printf '%s\n' "$old_sums") <(cmin_sums "$cdir"))" ] ||
        fail "selftest: BL-841: the minimised corpus holds an input the old one did not"
    [ -d "$pre" ] && [ "$(stat -c %a "$pre")" = 700 ] && [ "$(cmin_sums "$pre")" = "$old_sums" ] ||
        fail "selftest: BL-841: the old corpus was not kept, whole and 0700, as corpus.pre-cmin"
    [ "$(cmin_features 00000000000000c2)" = "$want_features" ] ||
        fail "selftest: BL-841: the minimised corpus lost features"
    # The old corpus still kept: refused (and run.sh never deletes it).
    cmin_refused "old corpus is still kept" -- selftest_mismatch --pin v24.0 --minimize-corpus
    rm -rf "$pre"
    # A merge with a finding (the planted divergence) swaps nothing.
    printf 'node-fuzz selftest' >"$cdir/planted"
    local state
    state="$(cmin_sums "$cdir")"
    cmin_run 00000000000000c3
    [ "$status" -ne 0 ] && grep -q 'mode=minimize-corpus .*status=fail' <<<"$out" ||
        fail "selftest: BL-841: a minimisation with a finding passed"
    [ "$(cmin_sums "$cdir")" = "$state" ] && [ ! -e "$pre" ] ||
        fail "selftest: BL-841: a minimisation with a finding changed the corpus"
    rm -f "$cdir/planted"
    # A merge that exited non-zero, kept inputs and found nothing swaps nothing.
    state="$(cmin_sums "$cdir")"
    cmin_run 00000000000000cc NODE_FUZZ_SELFTEST_CMIN_STATUS=1
    [ "$status" -ne 0 ] && grep -q 'mode=minimize-corpus .*status=fail' <<<"$out" ||
        fail "selftest: BL-841: a minimisation whose merge failed passed (rc $status)"
    [ "$(cmin_sums "$cdir")" = "$state" ] && [ ! -e "$pre" ] ||
        fail "selftest: BL-841: a minimisation whose merge failed changed the corpus"
    for f in "$cdir"/*; do printf 'cmin again %s' "${f##*/}" >"$cdir/again-${f##*/}"; done
    # A kill before the renames: the old corpus stands, and the killed run's cmin/ is never adopted.
    state="$(cmin_sums "$cdir")"
    cmin_run 00000000000000c4 NODE_FUZZ_SELFTEST_CMIN_KILL=before
    [ "$status" -eq 137 ] && [ "$(cmin_sums "$cdir")" = "$state" ] && [ ! -e "$pre" ] ||
        fail "selftest: BL-841: a kill before the renames changed the corpus (rc $status)"
    while IFS= read -r -d '' f; do printf 'node-fuzz stale cmin' >"$f/stale"; done < <(find "$tree" -type d -name cmin -print0)
    cmin_run 00000000000000c5
    [ "$status" -eq 0 ] || fail "selftest: BL-841: the minimisation after a kill failed (rc $status: $(tail -n 1 <<<"$out"))"
    ! grep -q "$(printf 'node-fuzz stale cmin' | sha1sum | cut -c1-40)" <<<"$(cmin_sums "$cdir")" ||
        fail "selftest: BL-841: a killed run's cmin/ was adopted"
    rm -rf "$pre"
    # A kill between the renames: no corpus; every run refuses, creating nothing; a minimisation rolls back.
    for f in "$cdir"/*; do printf 'cmin third %s' "${f##*/}" >"$cdir/third-${f##*/}"; done
    state="$(cmin_sums "$cdir")"
    cmin_run 00000000000000c6 NODE_FUZZ_SELFTEST_CMIN_KILL=between
    [ "$status" -eq 137 ] && [ ! -e "$cdir" ] && [ "$(cmin_sums "$pre")" = "$state" ] ||
        fail "selftest: BL-841: a kill between the renames did not leave the old corpus alone (rc $status)"
    cmin_refused "cmin-interrupted" -- selftest_mismatch --pin v24.0 --runs 0
    cmin_refused "cmin-interrupted" -- selftest_mismatch --pin v24.0 --merge
    local kept=("$pre"/*)
    cmin_refused "cmin-interrupted" -- selftest_mismatch --pin v24.0 --job "$(job replay "${kept[0]}")"
    rm -f "$tree"/jobs/job-*
    # With an open triage cluster: rolled back, then refused.
    local cid
    cid="$(printf 'selftest_mismatch\0divergence\0selftest' | sha256sum | cut -c1-12)"
    printf '{"clusters": {"%s": {"kind": "divergence", "class": "selftest", "pins": ["v24.0"], "hits": 1, "first_seen": "r1", "state": "open", "minimized": false, "attempts": 0, "vector": null}}, "runs": []}\n' \
        "$cid" >"$tree/triage.json"
    cmin_run 00000000000000cb
    [ "$status" -ne 0 ] && grep -q 'rolled back a corpus minimisation' <<<"$out" && grep -qF 'open triage cluster' <<<"$out" &&
        [ "$(cmin_sums "$cdir")" = "$state" ] && [ ! -e "$pre" ] ||
        fail "selftest: BL-841: a minimisation did not roll back the cut swap before refusing (rc $status: $(tail -n 1 <<<"$out"))"
    rm -f "$tree/triage.json"
    cmin_run 00000000000000c7
    [ "$status" -eq 0 ] && [ -d "$cdir" ] && [ "$(cmin_sums "$pre")" = "$state" ] ||
        fail "selftest: BL-841: the minimisation after the rollback failed (rc $status: $(tail -n 1 <<<"$out"))"
    rm -rf "$pre"
    # A swap cut just after a run's first check.
    local runs_before
    runs_before="$(find "$tree/runs" -mindepth 1 -maxdepth 1 | wc -l)"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" NODE_FUZZ_SELFTEST_CMIN_CUT=1 "$0" selftest_mismatch --pin v24.0 --runs 0 2>&1)" || status=$?
    [ "$status" -ne 0 ] && grep -qF cmin-interrupted <<<"$out" && [ ! -e "$cdir" ] &&
        [ "$(find "$tree/runs" -mindepth 1 -maxdepth 1 | wc -l)" = "$runs_before" ] ||
        fail "selftest: BL-841: a run did not refuse a swap cut after its first check (rc $status: $(tail -n 1 <<<"$out"))"
    mv -T "$pre" "$cdir"
    status=0
    out="$(DASH_EMBARGO_DIR="$dir" NODE_FUZZ_SELFTEST_CMIN_CUT=1 "$0" selftest_mismatch --pin v24.0 \
        --job "$(job replay "$tree/runs/earlier/crashes/crash-b")" 2>&1)" || status=$?
    [ "$status" -eq 0 ] && grep -q 'mode=replay .*status=pass' <<<"$out" && [ ! -e "$cdir" ] ||
        fail "selftest: BL-841: a job ran badly or created a working corpus (rc $status: $(tail -n 1 <<<"$out"))"
    mv -T "$pre" "$cdir"
    # A kill after the renames: the new corpus stands, and a minimisation is refused while the old is kept.
    for f in "$cdir"/*; do printf 'cmin fourth %s' "${f##*/}" >"$cdir/fourth-${f##*/}"; done
    state="$(cmin_sums "$cdir")"
    want_features="$(cmin_features 00000000000000ca)"
    cmin_run 00000000000000c8 NODE_FUZZ_SELFTEST_CMIN_KILL=after
    [ "$status" -eq 137 ] && [ "$(cmin_sums "$pre")" = "$state" ] && [ -d "$cdir" ] &&
        [ -z "$(comm -13 <(printf '%s\n' "$state") <(cmin_sums "$cdir"))" ] ||
        fail "selftest: BL-841: a kill after the renames did not leave the new corpus (rc $status)"
    runs=("$tree"/runs/*-00000000000000c8)
    [ -f "${runs[0]}/cmin.counts" ] && [ ! -e "${runs[0]}/cmin" ] ||
        fail "selftest: C-841-F13: a minimisation killed after its renames left no counts"
    [ "$(cmin_features 00000000000000c9)" = "$want_features" ] || fail "selftest: BL-841: the corpus after a kill lost features"
    cmin_refused "old corpus is still kept" -- selftest_mismatch --pin v24.0 --minimize-corpus
    rm -rf "$pre"

    # 19. BL-841b: --shard. It is refused, creating nothing, for anything but a budgeted fuzzing run
    # of the default build with --jobs 2 or more, and in the cmin-interrupted state. A shard run of
    # two workers loads each input of the working corpus, the private seeds, the public seeds and
    # the regression vectors in exactly one worker (each log names its shard's count), folds its new
    # inputs into the working corpus and changes nothing else there, removes shards/ and keeps the
    # counts in shards.json. A run stopped through its stop file folds, and so does a run that fails
    # after its segments (the EXIT trap). A divergence planted in the corpus is the run's finding,
    # and it survives a SIGKILL, which folds nothing and leaves the working corpus as it was.
    local s="is for a budgeted fuzzing run with --jobs 2 to 999" srun k n want total folded
    cmin_refused "$s" -- selftest_mismatch --pin v24.0 --certify --seconds 60 --jobs 2 --shard
    cmin_refused "$s" -- selftest_mismatch --pin v24.0 --merge --jobs 2 --shard
    cmin_refused "$s" -- selftest_mismatch --pin v24.0 --minimize-corpus --shard
    cmin_refused "$s" -- selftest_mismatch --pin v24.0 --job "job-$(printf '%032d' 0)" --jobs 2 --shard
    cmin_refused "$s" -- selftest_mismatch --pin v24.0 --cpu-hours 1 --jobs 2 --shard --corpus-sample 2 --corpus-sample-bytes 1
    cmin_refused "$s" -- selftest_mismatch --pin v24.0 --cpu-hours 1 --jobs 1 --shard
    cmin_refused "$s" -- selftest_mismatch --pin v24.0 --cpu-hours 1 --shard
    cmin_refused "$s" -- selftest_mismatch --pin v24.0 --runs 5 --jobs 2 --shard
    cmin_refused "--shard takes the default build only" NODE_FUZZ_TRACE_CMP=0 -- selftest_mismatch --pin v24.0 --cpu-hours 1 --jobs 2 --shard
    mv -T "$cdir" "$pre"
    cmin_refused "cmin-interrupted" -- selftest_mismatch --pin v24.0 --cpu-hours 1 --jobs 2 --shard
    mv -T "$pre" "$cdir"
    mkdir -m 700 "$tree/seeds" "$tree/regress" "$tree/regress/fixed"
    printf 'shard seed' >"$tree/seeds/s1"
    printf 'shard vector' >"$tree/regress/fixed/v1"
    shard_json() {  # <run> <python expression over the record r>
        python3 -I -c 'import json, sys; r = json.load(open(sys.argv[1] + "/shards.json")); print(eval(sys.argv[2]))' "$@"
    }
    shard_run() {  # <run tag> [ENV...] -- [run.sh options...]: sets $status, $out and $srun
        local tag="$1" envs=()
        shift
        while [ "$1" != -- ]; do envs+=("$1"); shift; done
        shift
        status=0
        out="$(env "${envs[@]}" DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --jobs 2 --shard \
            --run-tag "$tag" "$@" 2>&1 7>&-)" || status=$?
        srun=("$tree"/runs/*-"$tag")
    }
    before_sample="$(corpus_state)"
    local before_names
    before_names="$(ls -A "$cdir" | sort)"
    shard_run 00000000000000d1 -- --cpu-hours 0.0005
    [ "$status" -eq 0 ] && grep -q 'mode=run .*status=pass' <<<"$out" ||
        fail "selftest: BL-841b: a shard run failed (rc $status: $(tail -n 1 <<<"$out"))"
    total="$( (printf '%s\n' "$before_names"; ls -A "$tree/seeds" "$tree/public-seeds" "$tree/regress/fixed" | grep -v ':$') |
             grep -v '^$' | sort -u | wc -l)"
    [ -f "${srun[0]}/logs/segment-1/fuzz-0.log" ] && [ -f "${srun[0]}/logs/segment-1/fuzz-1.log" ] &&
        [ ! -e "${srun[0]}/logs/segment-1/fuzz-2.log" ] || fail "selftest: BL-841b: a shard run of two did not run two workers"
    [ "$(shard_json "${srun[0]}" 'r["jobs"], r["inputs"], sum(x["files"] for x in r["shards"])')" = "(2, $total, $total)" ] ||
        fail "selftest: BL-841b: the shards do not hold each of the $total inputs once ($(shard_json "${srun[0]}" 'r["inputs"]'))"
    for k in 0 1; do
        n="$(shard_json "${srun[0]}" "r[\"shards\"][$k][\"files\"]")"
        grep -q "^INFO: *$n files found in ${srun[0]}/shards/$k\$" "${srun[0]}/logs/segment-1/fuzz-$k.log" ||
            fail "selftest: BL-841b: worker $k did not load its shard alone"
        [ -n "$(ls -A "${srun[0]}/features/segment-1/$k")" ] || fail "selftest: BL-841b: worker $k wrote no feature sets"
    done
    folded="$(shard_json "${srun[0]}" 'r["folded"]')"
    [ ! -e "${srun[0]}/shards" ] && [ -z "$(comm -23 <(printf '%s\n' "$before_sample") <(corpus_state))" ] &&
        [ "$(( $(ls -A "$cdir" | wc -l) - $(printf '%s\n' "$before_names" | wc -l) ))" = "$folded" ] ||
        fail "selftest: BL-841b: a shard run's fold changed the working corpus otherwise, or left its shards"
    # Stopped through the run's stop file once both workers fuzz: the run ends, and folds.
    status=0
    DASH_EMBARGO_DIR="$dir" "$0" selftest_mismatch --pin v24.0 --jobs 2 --shard --run-tag 00000000000000d3 --cpu-hours 0.05 \
        >"$dir/shard-stop.out" 2>&1 7>&- &
    local spid=$! waited=0
    srun=()
    while [ "$waited" -lt 600 ]; do
        srun=("$tree"/runs/*-00000000000000d3)
        [ -d "${srun[0]}" ] && grep -qs 'INITED' "${srun[0]}/logs/segment-1/fuzz-0.log" &&
            grep -qs 'INITED' "${srun[0]}/logs/segment-1/fuzz-1.log" && break
        sleep 1
        waited=$((waited + 1))
    done
    printf 'stop\n' >"${srun[0]}/stop"
    wait "$spid" || status=$?
    [ "$(shard_json "${srun[0]}" '"folded" in r')" = True ] && [ ! -e "${srun[0]}/shards" ] ||
        fail "selftest: BL-841b: a shard run stopped through its stop file did not fold (rc $status)"
    # A failure after the segments: the EXIT trap folds.
    shard_run 00000000000000d4 NODE_FUZZ_SELFTEST_SHARD_FAIL=1 -- --cpu-hours 0.0005
    [ "$status" -ne 0 ] && grep -q 'a shard run failed after its segments' <<<"$out" &&
        [ "$(shard_json "${srun[0]}" '"folded" in r')" = True ] && [ ! -e "${srun[0]}/shards" ] ||
        fail "selftest: BL-841b: a shard run that failed did not fold from its EXIT trap (rc $status)"
    # A divergence planted in the working corpus: one shard holds it, and it is the run's finding.
    printf 'node-fuzz selftest' >"$cdir/plant"
    shard_run 00000000000000d2 -- --cpu-hours 0.0005
    [ "$status" -ne 0 ] && grep -q 'divergences=[1-9].*status=fail (divergences)' <<<"$out" &&
        [ -n "$(ls -A "${srun[0]}/divergences")" ] ||
        fail "selftest: BL-841b: a divergence in a shard was not the run's finding (rc $status)"
    # SIGKILLed through its process group, as the farm kills a slice, once both workers fuzz and the
    # planted divergence is recorded (OD-L3-161 Q5): no fold runs, the finding stays in divergences/
    # (new/ is lost, and a worker that found the input again by mutation wrote it there too), the
    # working corpus is unchanged, and what the farm's reconcile reads of the run is that finding
    # and a shard record never folded.
    before_sample="$(corpus_state)"
    before_names="$(ls -A "$cdir" | sort)"
    status=0
    DASH_EMBARGO_DIR="$dir" setsid "$0" selftest_mismatch --pin v24.0 --jobs 2 --shard --run-tag 00000000000000d5 \
        --cpu-hours 0.05 >"$dir/shard-kill.out" 2>&1 7>&- &
    spid=$! waited=0
    srun=()
    while [ "$waited" -lt 600 ]; do
        srun=("$tree"/runs/*-00000000000000d5)
        [ -d "${srun[0]}" ] && [ -n "$(ls -A "${srun[0]}/divergences" 2>/dev/null)" ] &&
            grep -qs 'INITED' "${srun[0]}/logs/segment-1/fuzz-0.log" &&
            grep -qs 'INITED' "${srun[0]}/logs/segment-1/fuzz-1.log" && break
        sleep 1
        waited=$((waited + 1))
    done
    [ "$waited" -lt 600 ] && [ "$(ps -o pgid= -p "$spid" | tr -d ' ')" = "$spid" ] ||
        fail "selftest: BL-841b: a shard run to kill did not reach INITED with its finding, or has no group of its own" \
            "(waited ${waited}s, run dir $([ -d "${srun[0]}" ] && echo yes || echo no)," \
            "divergences $(ls -A "${srun[0]}/divergences" 2>/dev/null | wc -l)," \
            "INITED $(grep -cs INITED "${srun[0]}/logs/segment-1/fuzz-0.log")/$(grep -cs INITED "${srun[0]}/logs/segment-1/fuzz-1.log")," \
            "alive $(kill -0 "$spid" 2>/dev/null && echo yes || echo no), pgid $(ps -o pgid= -p "$spid" | tr -d ' ')/$spid:" \
            "$(tail -n 1 "$dir/shard-kill.out" | cut -c1-200))"
    kill -KILL -- "-$spid" || true
    wait "$spid" || status=$?
    waited=0
    while kill -0 -- "-$spid" 2>/dev/null && [ "$waited" -lt 60 ]; do
        sleep 1
        waited=$((waited + 1))
    done
    local got
    got="rc $status, alive $(kill -0 -- "-$spid" 2>/dev/null && echo yes || echo no),"
    got+=" verdict $([ -e "${srun[0]}/verdict.json" ] && echo yes || echo no),"
    got+=" folded $(shard_json "${srun[0]}" '"folded" in r' 2>&1 | tail -n 1 || :),"
    got+=" corpus $([ "$(corpus_state)" = "$before_sample" ] && [ "$(ls -A "$cdir" | sort)" = "$before_names" ] &&
        echo same || echo changed),"
    got+=" reconcile $(python3 -I -c 'import sys; sys.path.insert(0, sys.argv[1]); import fuzzctl
r = fuzzctl.run_findings(sys.argv[2]); print(r["divergences"] > 0, "folded" in fuzzctl.shard_record(sys.argv[2]))' \
        "$here" "${srun[0]}" 2>&1 | tail -n 1 | cut -c1-120 || :)"
    [ "$got" = "rc 137, alive no, verdict no, folded False, corpus same, reconcile True False" ] ||
        fail "selftest: BL-841b: a SIGKILLed shard run lost its finding, folded, or changed the working corpus ($got)"
    for f in "$cdir"/*; do
        ! cmp -s "$f" <(printf 'node-fuzz selftest') || rm -f "$f"
    done
    rm -rf "$tree/seeds" "$tree/regress"
    echo "node-fuzz selftest: ok"
}

[ "${1:-}" = "--selftest" ] && { selftest; exit 0; }

target="${1:-}"; shift || true
[ -n "$target" ] || fail "usage: run.sh <target> --pin v24.0 --cpu-hours H [--jobs J] [--public-seeds DIR] | --selftest"
pin="" cpu_hours="" jobs=1 public="" mode=run minimize="" order="" job_inputs=() runs="" job="" run_tag="" window="" wall_cap="" merge_status="" cmin=""
replay_before="" replay_after="" census_status="" sample_count="" sample_bytes="" shard=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --pin) pin="$2"; shift 2 ;;
        --cpu-hours) cpu_hours="$2"; shift 2 ;;
        --jobs) jobs="$2"; shift 2 ;;
        --public-seeds) public="$2"; shift 2 ;;
        --merge) mode=merge; shift ;;
        --certify) mode=certify; shift ;;
        --seconds) window="$2"; shift 2 ;;
        --wall-cap) wall_cap="$2"; shift 2 ;;
        --job) job="$2"; shift 2 ;;
        --run-tag) run_tag="$2"; shift 2 ;;
        --runs) runs="$2"; shift 2 ;;  # a fixed number of executions instead of a budget (selftest)
        --corpus-sample) sample_count="$2"; shift 2 ;;
        --corpus-sample-bytes) sample_bytes="$2"; shift 2 ;;
        --minimize-corpus) cmin=1; shift ;;
        --shard) shard=1; shift ;;
        *) fail "unknown argument $1" ;;
    esac
done
if [ -n "$cmin" ]; then
    [ "$mode" = run ] && [ -z "$job$runs$cpu_hours$sample_count$sample_bytes$window$wall_cap" ] && [ "$jobs" = 1 ] ||
        fail "--minimize-corpus takes no other mode, budget or sample (it merges the working corpus alone)"
    mode=minimize-corpus
fi
case "$pin" in v24.0) ;; *) fail "--pin must be v24.0" ;; esac
[[ "$target" =~ ^[a-z0-9_]+$ ]] || fail "bad target name"
[[ "$jobs" =~ ^[1-9][0-9]*$ ]] || fail "--jobs must be a positive integer"
if [ "$mode" = run ] && [ -z "$runs" ] && [ -z "$job" ]; then
    [[ "$cpu_hours" =~ ^[0-9]+(\.[0-9]+)?$ ]] || fail "--cpu-hours is required"
fi
[ -z "$runs" ] || [ "$target" = selftest_mismatch ] || fail "--runs is for the self-test only; real legs have a CPU budget"
[ -z "$job" ] || [[ "$job" =~ ^job-[0-9a-f]{32}$ ]] || fail "--job: not a job name"
[ -z "$run_tag" ] || [[ "$run_tag" =~ ^[0-9a-f]{16}$ ]] || fail "--run-tag must be 16 hex digits"
if [ -n "$sample_count$sample_bytes" ]; then
    [[ "$sample_count" =~ ^[0-9]+$ ]] && [[ "$sample_bytes" =~ ^[0-9]+$ ]] ||
        fail "--corpus-sample and --corpus-sample-bytes take whole numbers, and come together"
    [ "$mode" = run ] && [ -z "$job" ] || fail "--corpus-sample is for fuzzing runs (the smoke's)"
fi
# A shard run (BL-841b) is a farm slice: budgeted, of 2 to 999 workers, fuzzing, unsampled.
if [ -n "${shard:-}" ]; then
    [ "$mode" = run ] && [ -z "$job$runs$sample_count$sample_bytes" ] && [ -n "$cpu_hours" ] &&
        [ "$jobs" -ge 2 ] && [ "$jobs" -le 999 ] ||
        fail "--shard is for a budgeted fuzzing run with --jobs 2 to 999 (no --certify, --merge, --minimize-corpus, --job, --runs or --corpus-sample)"
fi
if [ "$mode" = certify ]; then
    [[ "$window" =~ ^[1-9][0-9]*$ ]] && [ "$window" -ge 60 ] || fail "--seconds must be a whole number of seconds, at least 60"
    wall_cap="${wall_cap:-43200}"
    [[ "$wall_cap" =~ ^[1-9][0-9]*$ ]] && [ "$wall_cap" -ge "$window" ] || fail "--wall-cap must be whole seconds, at least --seconds"
    [ -z "$job$runs$cpu_hours" ] && [ "$jobs" = 1 ] || fail "--certify runs one libFuzzer process for --seconds"
    # The window's driver waits at most $stop_grace seconds from its stop to libFuzzer's exit, and
    # checks the CPU time every 10 s. The selftest may shorten both through the environment, never
    # lengthen them (Sol r4 B1): anything but a finite number of seconds above 0 and at most the
    # bound stops the run here, before the build (and the driver checks again).
    python3 -I - "$stop_grace" <<'EOF' || fail "NODE_FUZZ_CERTIFY_GRACE must be above 0 and at most $stop_grace seconds, NODE_FUZZ_CERTIFY_POLL above 0 and at most 10"
import os, sys
for name, limit in (("NODE_FUZZ_CERTIFY_GRACE", float(sys.argv[1])), ("NODE_FUZZ_CERTIFY_POLL", 10.0)):
    try:
        value = float(os.environ.get(name, limit))
    except ValueError:
        sys.exit(1)
    if not 0 < value <= limit:  # NaN and infinity fail too
        sys.exit(1)
EOF
else
    [ -z "$window$wall_cap" ] || fail "--seconds and --wall-cap are for --certify"
fi

# The registry: the target and pin must be listed. Prints the target's cargo features (a comma
# list, the first line, empty for none: D40), its encoding (the second, empty for a target that
# is no chain target: D55) and then its public seed directories.
registry="$(python3 -I - "$here/targets.toml" "$target" "$pin" <<'EOF'
import re, sys, tomllib
path, name, pin = sys.argv[1:]
found = [t for t in tomllib.load(open(path, "rb")).get("target", []) if t.get("name") == name]
if not found:
    sys.exit(3)
if found[0].get("kind") != "selftest" and pin not in found[0].get("pins", []):
    sys.exit(4)
features = found[0].get("features", [])
if (not isinstance(features, list) or len(set(map(str, features))) != len(features)
        or not all(isinstance(f, str) and re.fullmatch(r"[a-z0-9_][a-z0-9_-]*", f) for f in features)):
    sys.exit(5)
encoding = found[0].get("encoding", "")
if "encoding" in found[0] and not (isinstance(encoding, str) and re.fullmatch(r"chain-v[1-9][0-9]*", encoding)):
    sys.exit(6)
print(",".join(features))
print(encoding)
print("\n".join(found[0].get("public_seeds", [])))
EOF
)" || case $? in
    3) fail "$target is not in node-fuzz/targets.toml" ;;
    4) fail "--pin $pin is not one of the target's pins in node-fuzz/targets.toml" ;;
    5) fail "$target's features in node-fuzz/targets.toml are not a list of distinct cargo feature names" ;;
    6) fail "$target's encoding in node-fuzz/targets.toml is not a chain encoding" ;;
    *) fail "node-fuzz/targets.toml is unreadable" ;;
esac
registry_features="$(head -n 1 <<<"$registry")"
registry_seeds="$(tail -n +3 <<<"$registry")"
declare +x encoding  # an inherited export would hand it to every child
encoding="$(sed -n 2p <<<"$registry")"
# A chain target's public seeds are its encoding's compile, the registry's directory (OD-L3-95
# (1)): another directory's inputs are of no known encoding. Refused before anything is created.
if [ -n "$encoding" ] && [ -n "$public" ]; then
    fail "--public-seeds is refused for chain target $target: its seeds are the registry's $(tr '\n' ' ' <<<"$registry_seeds")"
fi
# The build's coverage (BL-812; the header). Checked before anything is created.
coverage="${NODE_FUZZ_COVERAGE:-all}" trace_cmp="${NODE_FUZZ_TRACE_CMP:-1}" under_test=""
case "$coverage" in all|under-test) ;; *) fail "NODE_FUZZ_COVERAGE must be all or under-test" ;; esac
case "$trace_cmp" in 0|1) ;; *) fail "NODE_FUZZ_TRACE_CMP must be 0 or 1" ;; esac
if [ "$coverage" = under-test ]; then
    [ "${DASH_ORACLE_FLAVOR:-plain}" = plain ] ||
        fail "NODE_FUZZ_COVERAGE=under-test builds the oracle's plain flavor; DASH_ORACLE_FLAVOR=$DASH_ORACLE_FLAVOR is refused"
    read_under_test
fi
# A certify run measures the default build, and a merge rewrites the working corpus by the running
# build's features, which an opt-in build has fewer of.
case "$mode" in
    certify|merge|minimize-corpus) [ "$coverage:$trace_cmp" = all:1 ] ||
        fail "--$mode takes the default build only (NODE_FUZZ_COVERAGE and NODE_FUZZ_TRACE_CMP unset)" ;;
esac
# A shard run's new inputs are folded into the working corpus, which an opt-in build must not fill.
[ -z "${shard:-}" ] || [ "$coverage:$trace_cmp" = all:1 ] ||
    fail "--shard takes the default build only (NODE_FUZZ_COVERAGE and NODE_FUZZ_TRACE_CMP unset)"
# A floor-gated target's certify run is gated on its census floor (O2b-4 4e2): a minimisation could
# drop an input that reaches a census cell without adding a libFuzzer feature (BL-841, OD-L3-157 (3)).
if [ "$mode" = minimize-corpus ] && [ "$encoding" = chain-v2 ]; then
    fail "cmin-floor-gated: $target's certify run is gated on its census floor; its corpus is not minimised"
fi

[ -n "${DASH_EMBARGO_DIR:-}" ] || fail "DASH_EMBARGO_DIR is unset; L1 targets run only with the private sink (CS0 §9.3)"
fuzz="$DASH_EMBARGO_DIR/fuzz"
[ -d "$fuzz" ] && [ "$(stat -c %a "$fuzz")" = 700 ] || fail "the private fuzz directory is missing or not mode 0700"

# node-fuzz is a workspace of its own; its base-sdk [patch] must be the root's.
python3 -I - "$root/Cargo.toml" "$here/Cargo.toml" <<'EOF' || fail "node-fuzz/Cargo.toml's base-sdk [patch] differs from the root's"
import sys, tomllib
key = "https://github.com/dashpay/base-sdk"
patch = [tomllib.load(open(p, "rb")).get("patch", {}).get(key) for p in sys.argv[1:]]
sys.exit(0 if patch[0] and patch[0] == patch[1] else 1)
EOF

umask 077
tdir="$fuzz/$target"
# A minimisation cut between its two renames left the old corpus as corpus.pre-cmin and no corpus:
# any run would create and use an empty one, so every run refuses the tree until --minimize-corpus
# rolls it back (BL-841). A run that writes the corpus checks again under the leg's lock, which a
# minimisation holds across both renames.
cmin_interrupted() {
    if [ -d "$tdir/corpus.pre-cmin" ] && [ ! -e "$tdir/corpus" ] && [ "$mode" != minimize-corpus ]; then
        fail "cmin-interrupted: $target's corpus minimisation was cut between its renames; --minimize-corpus rolls it back"
    fi
}
cmin_interrupted
# The self-test's minimisation cut between its renames just after that check.
if [ "$target" = selftest_mismatch ] && [ -n "${NODE_FUZZ_SELFTEST_CMIN_CUT:-}" ] && [ -d "$tdir/corpus" ]; then
    mv -T "$tdir/corpus" "$tdir/corpus.pre-cmin"
fi
# A minimisation runs alone (OD-L3-157 (c)): it holds the leg's lock exclusively to the end, and
# starts only while it can take the triage lock (fuzzctl's) and the target has no open triage
# cluster and no job waiting. A cut swap is rolled back first, whatever refuses after it: that only
# restores the tree as it was. An earlier minimisation's old corpus still kept beside the corpus
# refuses. The triage lock is released before the merge: nothing triage writes is the corpus.
if [ "$mode" = minimize-corpus ]; then
    [ -d "$tdir/corpus" ] || [ -d "$tdir/corpus.pre-cmin" ] || fail "--minimize-corpus: $target has no working corpus"
    exec 5>>"$fuzz/.triage.lock" || fail "the private sink is not writable"
    flock -n -x 5 || fail "--minimize-corpus: a triage holds the private tree"
    exec 8>>"$tdir/run.lock" || fail "the private sink is not writable"
    flock -n -x 8 || fail "another run holds the leg (a minimisation runs alone)"
    if [ -d "$tdir/corpus.pre-cmin" ] && [ ! -e "$tdir/corpus" ]; then
        mv -T "$tdir/corpus.pre-cmin" "$tdir/corpus" || fail "rolling back the cut minimisation failed"
        echo "run.sh: rolled back a corpus minimisation cut between its renames" >&2
    fi
    python3 -I "$here/fuzzctl.py" cmin-ready "$target" || case $? in
        3) fail "--minimize-corpus: $target has an open triage cluster; triage it first" ;;
        4) fail "--minimize-corpus: $target has a job waiting" ;;
        *) fail "--minimize-corpus: $target's triage state is unreadable" ;;
    esac
    [ ! -e "$tdir/corpus.pre-cmin" ] ||
        fail "--minimize-corpus: the last minimisation's old corpus is still kept (corpus.pre-cmin; the farm removes it after the leg's next passing slice)"
    exec 5>&-
fi
# The job, read with shell builtins only and consumed. Checked before anything is created or
# built: a crash to minimise is a crash of a run of this target; a divergence to minimise or a
# path to replay is a finding of a run of this target or a regression vector (a file, or for a
# replay also a directory). Only a fixed-order replay (`order fixed`) takes more than one input.
if [ -n "$job" ]; then
    jobfile="$tdir/jobs/$job"
    [ -f "$jobfile" ] && [ ! -L "$jobfile" ] || fail "--job: no such job"
    while IFS=' ' read -r key value; do
        case "$key" in
            mode) mode="$value" ;;
            input) job_inputs+=("$value") ;;
            order) [ -n "$value" ] || fail "--job: a malformed job"; order="$value" ;;
            *) fail "--job: a malformed job" ;;
        esac
    done <"$jobfile"
    rm -f "$jobfile"
    case "$mode" in minimize|minimize-divergence|replay) ;; *) fail "--job: a malformed job" ;; esac
    case "$order:$mode:${#job_inputs[@]}" in
        :*:1|fixed:replay:[1-9]*) ;;
        *) fail "--job: a malformed job" ;;
    esac
    for minimize in "${job_inputs[@]}"; do
        case "/$minimize/" in */../*) fail "--job: the input is not a finding or regression vector of this target" ;; esac
        case "$mode:$minimize" in
            minimize:"$tdir"/runs/*/crashes/*) ;;
            minimize-divergence:"$tdir"/runs/*/divergences/*) ;;
            replay:"$tdir"/runs/*/divergences/*|replay:"$tdir"/runs/*/crashes/*|replay:"$tdir"/regress/*) ;;
            *) fail "--job: the input is not a finding or regression vector of this target" ;;
        esac
        if [ -L "$minimize" ] || ! { [ -f "$minimize" ] || { [ "$mode" = replay ] && [ -d "$minimize" ]; }; }; then
            fail "--job: the input is not an existing regular file"
        fi
    done
fi
# The field a divergence minimisation keeps: the `field` line of the divergence's sidecar, and
# with it the `cmd` line (a model target's divergence names the message command it is on), so a
# shrink onto the same field on another command is not kept (BL-267).
declare +x field cmd  # an inherited export would hand them to every child
field="" cmd=""
if [ "$mode" = minimize-divergence ]; then
    if [ -f "$minimize.txt" ]; then
        while IFS=' ' read -r key value; do
            if [ "$key" = field ]; then field="$value"; fi
            if [ "$key" = cmd ]; then cmd="$value"; fi
        done <"$minimize.txt"
    fi
    [[ "$field" =~ ^[a-z0-9_][a-z0-9_\ -]*$ ]] || fail "--job: the divergence has no field in its sidecar"
    [ -z "$cmd" ] || [[ "$cmd" =~ ^[a-z0-9_.-]{1,24}$ ]] || fail "--job: the divergence has a bad command in its sidecar"
fi
# A chain target's private tree holds inputs of its encoding only (§3a.3 (3)): a tree marked with
# another fails, and an unmarked tree is marked only when it holds no input (a chain-v1 tree, the
# legacy target's, is adopted once). Checked before anything is copied into it.
if [ -n "$encoding" ]; then
    python3 -I "$here/tree_encoding.py" adopt "$tdir" "$encoding" || case $? in
        3) fail "encoding-mismatch: $target's private tree is not marked $encoding" ;;
        4) fail "$target's private tree has no ENCODING marker and holds inputs: refused for $encoding" ;;
        *) fail "the private tree's ENCODING marker cannot be read or written" ;;
    esac
fi
public_dirs=()
if [ -n "$public" ]; then
    public_dirs=("$public")
elif [ -n "$registry_seeds" ]; then
    while IFS= read -r d; do public_dirs+=("$root/$d"); done <<<"$registry_seeds"
fi
for p in "${public_dirs[@]}"; do
    [ -d "$p" ] || fail "a public seed directory is missing"
done

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$pin-$$${run_tag:+-$run_tag}"
run="$tdir/runs/$run_id"
[ -d "$tdir" ] || mkdir -m 700 "$tdir" 2>/dev/null || fail "the private sink is not writable"
# The leg's lock, held to the end: fuzzing runs and merges share it, a certify run holds it alone
# (nothing else may write the working corpus between its replays), and jobs, which never read or
# write the corpus (nor create it), take none. A run that cannot take it fails at once; a farm
# slice the certify run blocks is a failed slice.
if [ -z "$job" ] && [ "$mode" != minimize-corpus ]; then  # a minimisation holds it already
    exec 8>>"$tdir/run.lock" || fail "the private sink is not writable"
    flock -n "$([ "$mode" = certify ] && echo -x || echo -s)" 8 ||
        fail "another run holds the leg (a certify run excludes fuzzing and merges of its target)"
    cmin_interrupted
fi
dirs=("$tdir/public-seeds" "$tdir/runs")
[ -n "$job" ] || dirs+=("$tdir/corpus")
for d in "${dirs[@]}"; do
    [ -d "$d" ] || mkdir -m 700 "$d" 2>/dev/null || fail "the private sink is not writable"
done
mkdir -m 700 "$run" 2>/dev/null || fail "the private sink is not writable"
# The run's mode, first: triage must never take a job killed before its verdict for fuzzing. The
# file is also the run's liveness lock (BL-841 r2 N1): run.sh holds it exclusively on fd 4 to the
# end, inherited by its fuzzing processes (not by the build), so triage takes a run without a
# verdict for a killed one only once it can lock the file. It is written and locked under another
# name and renamed into place, so `mode` is never seen unlocked while the run lives; `live`, a
# second name for the same file made before the rename, marks a run that holds the lock (r3 R3-1):
# a run from an older run.sh has a `mode` but no `live`, and triage gives it the hour as before.
{ exec 4>"$run/mode.new" && flock -n -x 4 && printf '%s\n' "$mode" >&4 && ln "$run/mode.new" "$run/live" &&
    mv -T "$run/mode.new" "$run/mode"; } ||
    fail "the private sink is not writable"
for d in divergences crashes logs tmp; do mkdir -m 700 "$run/$d" || fail "the private sink is not writable"; done
# A minimisation's output: this run's own, new and empty, so no earlier run's is ever adopted.
[ "$mode" != minimize-corpus ] || mkdir -m 700 "$run/cmin" || fail "the private sink is not writable"
# The self-test's sink-failure case: a divergence the target cannot record (a directory where
# the self-test input's record file would go).
if [ "$target" = selftest_mismatch ] && [ -n "${NODE_FUZZ_SELFTEST_READONLY:-}" ]; then
    mkdir -m 700 "$run/divergences/$(printf 'node-fuzz selftest' | sha256sum | cut -c1-64)"
fi

for p in "${public_dirs[@]}"; do
    # Opaque names: libFuzzer prints the paths it replays.
    find "$p" -type f -print0 | while IFS= read -r -d '' f; do
        cp "$f" "$tdir/public-seeds/$(sha256sum "$f" | cut -c1-16)"
    done || fail "copying the public seeds failed"
done
# A job's inputs, copied into the run as input/input-<n> by shell redirection (no process gets
# their paths; sidecars are left out). A fixed-order replay gives the job's k-th input a
# directory of its own, input/<k>/: libFuzzer reads its corpus directories in argument order, and
# a directory's files in readdir order (so an exact order takes one file per input). A
# minimisation's field goes to the target in the run's abort-field file
# (node_fuzz::ABORT_FIELD_FILE): "-", which names no field, makes a crash minimisation ignore
# divergences.
if [ -n "$job" ]; then
    mkdir -m 700 "$run/input" || fail "the private sink is not writable"
    staged=0 ordered=() into="$run/input"
    stage() { staged=$((staged + 1)); cat <"$1" >"$into/input-$staged" || fail "copying the inputs failed"; }
    for input in "${job_inputs[@]}"; do
        if [ "$order" = fixed ]; then
            into="$run/input/$((${#ordered[@]} + 1))"
            mkdir -m 700 "$into" || fail "the private sink is not writable"
            ordered+=("$into")
        fi
        if [ -d "$input" ]; then
            files=("$input"/*)
        else
            files=("$input")
        fi
        before=$staged
        for f in "${files[@]}"; do
            case "$f" in *.txt|*.json) continue ;; esac
            if [ -f "$f" ] && [ ! -L "$f" ]; then stage "$f"; fi
        done
        # An ordered input that stages nothing would drop out of the order unseen.
        [ "$order" != fixed ] || [ "$staged" -gt "$before" ] || fail "--job: an ordered input has nothing to run"
    done
    [ "$staged" -gt 0 ] || fail "--job: nothing to run"
    case "$mode" in
        minimize-divergence) printf '%s\n' "$field" ${cmd:+"cmd $cmd"} >"$run/abort-field" ;;
        minimize) printf '%s\n' - >"$run/abort-field" ;;
    esac
fi

# Coverage as cargo-fuzz instruments it, without a sanitizer (the Rust under test forbids unsafe).
# An explicit --target keeps RUSTFLAGS away from build scripts, as cargo-fuzz does.
host="$(rustc -vV | sed -n 's/^host: //p')"
# The registry's features for the bin (D40; `fuzzctl check` holds them to its required-features).
features=()
[ -z "$registry_features" ] || features=(--features "$registry_features")
# The oracle's coverage-instrumented flavour, so libFuzzer also sees Core's side of each
# comparison (O1's `fuzz` archives); DASH_ORACLE_FLAVOR=plain overrides it. An under-test build
# links the plain flavor.
flavor="${DASH_ORACLE_FLAVOR:-fuzz}"
[ "$coverage" != under-test ] || flavor=plain
case "$flavor" in plain|fuzz) ;; *) fail "DASH_ORACLE_FLAVOR must be plain or fuzz" ;; esac
# The source the binary is built from, for the verdict: the commit, and whether the tree was dirty
# or changed during the build. Every change the build could see counts, whatever the user's git
# configuration (Sol r2 M2): tracked changes, files marked assume-unchanged or skip-worktree, and
# untracked files (a new bin that cargo discovers) unless a committed .gitignore ignores them; an
# ignore rule from core.excludesFile, info/exclude or an untracked .gitignore does not count.
# Git runs without the caller's GIT_* variables and without an fsmonitor, and anything
# unexpected (a failed git, a name that is not UTF-8) reads as an unknown source.
tree_state() {
    python3 -I - "$root" <<'EOF'
import os, subprocess, sys
env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
def git(*args, stdin="", ok=(0,)):
    r = subprocess.run(["git", "-C", sys.argv[1], "-c", "core.excludesFile=/dev/null", "-c", "core.fsmonitor=false",
                        *args], input=stdin, capture_output=True, text=True, env=env)
    if r.returncode not in ok:
        raise OSError(r.stderr)
    return r.stdout
try:
    head = git("rev-parse", "--verify", "--short=12", "HEAD").strip()
    changes = git("status", "--porcelain", "-z", "--untracked-files=no", "--ignore-submodules=none").split("\0")
    changes += [f for f in git("ls-files", "-z", "-v").split("\0") if f and not f.startswith("H ")]
    tracked = set(git("ls-files", "-z").split("\0")) - {""}
    others = git("ls-files", "-z", "--others", "--directory", "--no-empty-directory")
    # check-ignore -z -v -n: source, line, pattern and path of each, an empty source when unmatched.
    out = git("check-ignore", "-z", "--stdin", "-v", "-n", stdin=others, ok=(0, 1)).split("\0") if others else []
    for source, _, pattern, path in zip(*[iter(out[:len(out) // 4 * 4])] * 4):
        name = path.rstrip("/").rsplit("/", 1)[-1]
        if source not in tracked or pattern.startswith("!") or name == ".gitignore":
            changes.append(f"?? {path}")
    print("\n".join([head] + sorted(c for c in changes if c)))
except Exception:
    print("-")
    print("unknown")
EOF
}
# Builds are serialised (fd 9 on target/.node-fuzz-build.lock), so no other run's build replaces
# the binary between this build and its use; --target-dir puts it where $bin says, whatever the
# caller's CARGO_TARGET_DIR or cargo configuration. A certify run executes a copy of it in the run
# (deleted at the end), whose sha256 its source record keeps: a build after its lock is released
# cannot change what the certify run measures. The locks' descriptors are closed for cargo, as
# a server it starts (sccache) would hold them (a minimisation's triage lock, fd 5, and the run's
# liveness lock, fd 4).
# The build's flags and directory (BL-812; build_setup).
build_setup
mkdir -p "$here/target"
exec 9>>"$here/target/.node-fuzz-build.lock"
flock -x 9
before="$(tree_state)"
# The registry's under_test and sancov-rustc are read again under the lock, after the snapshot the
# source record compares with: a change while this run waited would build another crate list, or
# put another wrapper's objects in this id's directory, under a clean record.
same_build "$under_test:$target_dir"
(cd "$here" && env DASH_ORACLE_FLAVOR="$flavor" "${build_env[@]}" \
    cargo build --release --locked --target "$host" --target-dir "$target_dir" --bin "$target" "${features[@]}" 4>&- 5>&- 8>&- 9>&-) \
    >"$run/logs/build.log" 2>&1 ||
    fail "the build failed; see the run's logs/build.log"
after="$(tree_state)"
bin="$target_dir/$host/release/$target"
echo "node-fuzz: bin $bin" >>"$run/logs/build.log"
binary_sha=-
if [ "$mode" = certify ]; then
    trap 'rm -f "$run/bin"' EXIT
    cp --reflink=auto "$bin" "$run/bin" || fail "copying the binary into the run failed (is the private sink full?)"
    bin="$run/bin"
    binary_sha="$(sha256sum "$bin" | cut -c1-64)"
fi
# The encoding the binary is built for and, for a chain-v2 target, the sha256 of the census floor
# its certify run is gated on (census_floor.py checks the copy it judges against it).
if [ -n "$encoding" ]; then
    printf '%s\n' "$encoding" >"$run/encoding" || fail "the private sink is not writable"
    if [ "$encoding" = chain-v2 ]; then
        floor_src="$root/node-l1/census/$target.floor.json"
        { if [ -f "$floor_src" ]; then sha256sum <"$floor_src" | cut -c1-64; else echo missing; fi; } \
            >"$run/floor.sha256" || fail "the private sink is not writable"
    fi
fi
exec 9>&-
# The build is done: fuzzctl starts a job's timeout from this file (BL-174).
: >"$run/built" || fail "the private sink is not writable"
{ printf '%s\n' "${after%%$'\n'*}"; [ "$before" = "$after" ] && [ "$(printf '%s\n' "$after" | wc -l)" = 1 ] && echo clean || echo dirty
  printf '%s\n' "$run_id" "$binary_sha"; } >"$run/source"

args=("-artifact_prefix=$run/crashes/")
inputs=()
time_limited=0
# The corpus directories of fuzzing and merging: the working corpus first (libFuzzer writes new
# inputs only there), the private seeds when present, the public seeds, and the fixed regression
# vectors when present. A sampled run (the smoke's, BL-333) puts its sample of the working corpus
# and the public seeds in their place, first, so libFuzzer's new inputs go to the sample and the
# working corpus is only read.
# A fuzzing run of an opt-in build (BL-812) is staged the same way, with every input: libFuzzer
# keeps the inputs that reach features its build counts (and deletes only those it added itself),
# and an opt-in build lacks features the default build counts, so its additions would fill the
# working corpus by another measure. Such a run adds nothing to the working corpus.
if [ "$mode" = run ] && [ -z "${sample_count:-}" ] && [ "${coverage:-all}:${trace_cmp:-1}" != all:1 ]; then
    sample_count=inf sample_bytes=inf
fi
corpus=("$tdir/corpus")
if [ -n "${sample_count:-}" ]; then
    # Reflinks (a copy where the filesystem has none): the sample's files are libFuzzer's to
    # replace or delete, and the working corpus's stay as they are. The order is the sha256 of the
    # file names (libFuzzer's corpus names are its inputs' sha1, the public seeds' their sha256's
    # first 16 digits), never mtime or chance; a file that would pass the byte cap is skipped.
    # A farm slice of the leg (they share its lock) may delete a corpus file it reduced meanwhile:
    # such a file is passed over. (The only other EXIT trap is certify's, which is never sampled.)
    trap 'rm -rf "$run/sample"' EXIT
    mkdir -m 700 "$run/sample" || fail "the private sink is not writable"
    python3 -I - "$run/sample" "$sample_count" "$sample_bytes" "$tdir/corpus" "$tdir/public-seeds" <<'EOF' || fail "sampling the working corpus failed"
import fcntl, hashlib, os, shutil, sys
dest, count, cap, sources = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), sys.argv[4:]
FICLONE = 0x40049409
pool = {}
for src in sources:
    with os.scandir(src) as it:
        for e in it:
            if e.is_file(follow_symlinks=False):
                pool.setdefault(e.name, e.path)
taken = used = 0
for name in sorted(pool, key=lambda n: hashlib.sha256(os.fsencode(n)).digest()):
    if taken == count:
        break
    try:
        a = open(pool[name], "rb")
    except FileNotFoundError:
        continue
    with a:
        size = os.fstat(a.fileno()).st_size
        if used + size > cap:
            continue
        with open(os.path.join(dest, name), "xb") as b:
            try:
                fcntl.ioctl(b.fileno(), FICLONE, a.fileno())
            except OSError:
                shutil.copyfileobj(a, b)
    taken, used = taken + 1, used + size
EOF
    corpus=("$run/sample")
fi
[ -d "$tdir/seeds" ] && corpus+=("$tdir/seeds")
[ -n "${sample_count:-}" ] || corpus+=("$tdir/public-seeds")
[ -d "$tdir/regress/fixed" ] && corpus+=("$tdir/regress/fixed")
# A shard run (BL-841b; the header): every one of those inputs in exactly one of shards/0..J-1, and
# each worker's corpus `new shards/<k>` (the driver puts k for `{shard}`). The fold of new/ runs at
# the end and from the EXIT trap (a stop, a failure or a signal), under the leg's lock (fd 8), which
# is held to the exit; a SIGKILL loses new/ (findings are in crashes/ and divergences/, never new/).
# On a SIGTERM bash runs the trap at once, while the workers are still exiting: an input one writes
# to new/ after the fold is neither folded nor counted. SIGINT waits for the workers.
if [ -n "${shard:-}" ]; then
    case "$run" in *'{'*|*'}'*) fail "the private tree's path has a brace, which --shard's workers cannot take" ;; esac
    trap 'python3 -I "$here/shards.py" fold "$run" "$tdir/corpus" >/dev/null 2>&1' EXIT
    mkdir -m 700 "$run/new" "$run/shards" "$run/features" || fail "the private sink is not writable"
    python3 -I "$here/shards.py" stage "$run" "$jobs" "$run_id" "${corpus[@]}" || fail "staging the shards failed"
    corpus=("$run/new" "$run/shards/{shard}")
fi
case "$mode" in
    run)
        if [ -n "$runs" ]; then
            args+=("-runs=$runs")
        else
            time_limited=1
        fi
        [ "$jobs" -gt 1 ] && [ -z "${shard:-}" ] && args+=("-jobs=$jobs" "-workers=$jobs")
        # The farm stops a slice by writing $run/stop (BL-173): every worker reads it at the top of
        # its loop, prints its final stats and exits 0. A SIGINT can leave a worker hung in
        # libFuzzer's interrupt path until its ALARM writes an empty timeout artifact. (A budgeted
        # segment's -stop_file is its own, which the driver also fills from $run/stop; see below.)
        args+=("-print_final_stats=1")
        inputs=("${corpus[@]}")
        ;;
    merge|certify) args+=("-merge=1"); inputs=("${corpus[@]}") ;;
    minimize-corpus) args+=("-merge=1"); inputs=("$run/cmin" "$tdir/corpus") ;;
    minimize|minimize-divergence)
        args+=("-minimize_crash=1" "-runs=10000" "-exact_artifact_path=$run/crashes/minimized")
        inputs=("$run/input/input-1")
        ;;
    replay)
        args+=("-runs=0" "-print_final_stats=1")
        if [ "$order" = fixed ]; then
            # libFuzzer shuffles its seed inputs, then sorts them by size, unless told not to.
            args+=("-shuffle=0" "-prefer_small=0")
            inputs=("${ordered[@]}")
        else
            inputs=("$run/input")
        fi
        ;;
esac

# One libFuzzer process per segment, each in a log directory of its own (with -jobs, libFuzzer
# writes fuzz-N.log into its working directory). A budgeted run repeats segments, each given the
# CPU time still missing, until the measured CPU time reaches the budget. A segment ends on its own
# CPU time, not on a wall clock (BL-228): on a loaded box a process gets less CPU than wall time,
# and a segment that stopped after a wall-clock estimate ended after about one corpus reload, which
# the next segment then paid again. window_driver.py --farm stops it through its stop file once its
# process tree (the workers of -jobs included) has used the allotment, and copies the farm's own
# stop file ($run/stop, BL-173) to it. -max_total_time is a backstop of a day, never the normal
# stop. The working corpus carries over between segments.
segment_backstop=86400
status=0
segment=0
while :; do
    # A stopped run (its workers exited 0 at the loop boundary, short of the budget) starts no more.
    [ -e "$run/stop" ] && break
    segment=$((segment + 1))
    seg="$run/logs/segment-$segment"
    segment_args=("${args[@]}")
    if [ "$time_limited" = 1 ]; then
        # The CPU seconds still missing, over all processes of the segment, and one more so that
        # the segment's measured end is past the budget (the driver counts 10 ms ticks): a
        # shortfall would start a segment that reloads the corpus for it.
        cpu_seconds="$(python3 -I - "$run/logs" "$cpu_hours" <<'EOF'
import glob, math, os, sys
logs, budget = sys.argv[1], float(sys.argv[2])
spent = sum(sum(float(x) for x in open(f).read().split()[-2:]) for f in glob.glob(os.path.join(logs, "*", "cpu"))) / 3600
missing = budget - spent
print(0 if missing <= 0 else math.ceil(missing * 3600) + 1)
EOF
)"
        [ "$cpu_seconds" -gt 0 ] || break
        segment_args+=("-stop_file=$seg/stop" "-max_total_time=$segment_backstop")
    elif [ "$mode" = run ]; then
        segment_args+=("-stop_file=$run/stop")
    fi
    mkdir -m 700 "$seg"
    if [ -n "${shard:-}" ]; then
        mkdir -m 700 "$run/features/segment-$segment" || fail "the private sink is not writable"
        for ((k = 0; k < jobs; k++)); do
            mkdir -m 700 "$run/features/segment-$segment/$k" || fail "the private sink is not writable"
        done
        segment_args+=("-features_dir=$run/features/segment-$segment/{shard}")
    fi
    if [ "$time_limited" = 1 ]; then
        # The driver builds the target's clean environment itself (nothing the caller exported
        # reaches it) and writes the segment's fuzz.out and cpu files.
        python3 -I "$here/window_driver.py" ${encoding:+--encoding "$encoding"} --farm "$run/stop" ${shard:+--shards "$jobs"} "$seg" \
            "$cpu_seconds" "$((segment_backstop + 3600))" \
            "$run/tmp" "$target" "$pin" "$run_id" "$bin" "${segment_args[@]}" "${inputs[@]}" || status=$?
    else
        # A clean environment: nothing the caller exported (RUST_LIBFUZZER_DEBUG_PATH, ASAN_OPTIONS, …)
        # reaches the target.
        (cd "$seg" && env -i PATH="$PATH" HOME="$HOME" TMPDIR="$run/tmp" DASH_EMBARGO_DIR="$DASH_EMBARGO_DIR" \
            NODE_FUZZ_PIN="$pin" NODE_FUZZ_RUN="$run_id" RUST_BACKTRACE=1 ${encoding:+NODE_L1_CHAIN_ENCODING="$encoding"} \
            /usr/bin/time -f '%U %S' -o "$seg/cpu" "$bin" "${segment_args[@]}" "${inputs[@]}" >"$seg/fuzz.out" 2>&1) || status=$?
    fi
    # A slow unit (libFuzzer's -report_slow_units artifact) is no finding (BL-83).
    [ "$time_limited" = 1 ] && [ "$status" = 0 ] &&
        [ -z "$(find "$run/divergences" "$run/crashes" -type f ! -name 'slow-unit-*' -print -quit)" ] || break
done
if [ -n "${shard:-}" ]; then
    # The self-test's failure after the segments: the EXIT trap folds.
    [ "$target" != selftest_mismatch ] || [ -z "${NODE_FUZZ_SELFTEST_SHARD_FAIL:-}" ] ||
        fail "selftest: a shard run failed after its segments"
    python3 -I "$here/shards.py" fold "$run" "$tdir/corpus" || fail "folding the shard run's new inputs failed"
fi

# A certify run (DEC-186 amendments 2 and 3): replay passes before and after the window measure
# the working corpus; between them, the window. Each replay runs the binary once over the corpus
# with -runs=0 (libFuzzer loads it, prints INITED with its cov and ft, and stops) and its exit
# status goes to the verdict. The window is one libFuzzer process on the corpus that window_driver.py
# ends through its stop file once the process has used $window seconds of its own CPU time after
# INITED, with $wall_cap seconds of wall time and $stop_grace seconds from the stop to the exit as
# its limits (the driver's header has the rules). Its -max_total_time (libFuzzer's own clock, which
# follows the civil clock) is only a backstop. A chain target's replays write their census, bound
# to the replay's phase, to census.json in their segment (O2b-4 §4.6); a chain-v2 target's window
# runs only when the census of the replay before it meets the target's floor (census_floor.py).
replay_corpus() {  # <segment> <phase>: prints the replay's exit status
    local seg="$run/logs/$1" rc=0 census=()
    mkdir -m 700 "$seg"
    [ -z "$encoding" ] || census=(NODE_L1_CHAIN_ENCODING="$encoding" NODE_L1_CHAIN_STATS="$seg/census.json"
                                  NODE_L1_CHAIN_PHASE="$2")
    (cd "$seg" && env -i PATH="$PATH" HOME="$HOME" TMPDIR="$run/tmp" DASH_EMBARGO_DIR="$DASH_EMBARGO_DIR" \
        NODE_FUZZ_PIN="$pin" NODE_FUZZ_RUN="$run_id" RUST_BACKTRACE=1 "${census[@]}" \
        /usr/bin/time -f '%U %S' -o "$seg/cpu" "$bin" "-artifact_prefix=$run/crashes/" -runs=0 \
        -print_final_stats=1 "$tdir/corpus" >"$seg/fuzz.out" 2>&1) || rc=$?
    echo "$rc"
}
no_findings() { [ -z "$(find "$run/divergences" "$run/crashes" -type f ! -name 'slow-unit-*' -print -quit)" ]; }
# A minimisation's swap (BL-841): only after a merge that exited 0 with no finding and kept an input.
# The self-test kills the run before, between or after the two renames.
if [ "$mode" = minimize-corpus ]; then
    cmin_kill() {
        [ "$target" = selftest_mismatch ] || return 0
        # Which locks another process could take here, after the merge: the triage lock, the leg's,
        # and the run's liveness lock.
        if [ "$1" = before ] && [ -n "${NODE_FUZZ_SELFTEST_CMIN_LOCKS:-}" ]; then
            { flock -n -x "$fuzz/.triage.lock" true && echo triage=free || echo triage=held
              flock -n -s "$tdir/run.lock" true && echo leg=free || echo leg=held
              flock -n -s "$run/live" true && echo run=free || echo run=held; } >"$NODE_FUZZ_SELFTEST_CMIN_LOCKS"
        fi
        if [ "${NODE_FUZZ_SELFTEST_CMIN_KILL:-}" = "$1" ]; then kill -9 $$; fi
    }
    cmin_count() { find "$1" -maxdepth 1 -type f -printf '%s\n' | awk '{ n++; b += $1 } END { print n + 0, b + 0 }'; }
    read -r files_before bytes_before < <(cmin_count "$tdir/corpus")
    read -r files_after bytes_after < <(cmin_count "$run/cmin")
    swapped=0
    # The self-test's failed merge that kept inputs and found nothing (libFuzzer exits non-zero on a
    # SIGINT while it writes, say).
    if [ "$target" = selftest_mismatch ] && [ -n "${NODE_FUZZ_SELFTEST_CMIN_STATUS:-}" ]; then
        status="$NODE_FUZZ_SELFTEST_CMIN_STATUS"
    fi
    if [ "$status" = 0 ] && no_findings && [ "$files_after" -gt 0 ]; then
        # Written before the renames as well (C-841-F13): a run killed after the second one has its
        # counts and no cmin/, from which the farm records the minimisation as passed.
        printf '%s\n' "$files_before $bytes_before $files_after $bytes_after 0" >"$run/cmin.counts" ||
            fail "the private sink is not writable"
        cmin_kill before
        mv -T "$tdir/corpus" "$tdir/corpus.pre-cmin" && chmod 700 "$tdir/corpus.pre-cmin" &&
            cmin_kill between && mv -T "$run/cmin" "$tdir/corpus" && swapped=1
        cmin_kill after
    fi
    printf '%s\n' "$files_before $bytes_before $files_after $bytes_after $swapped" >"$run/cmin.counts" ||
        fail "the private sink is not writable"
fi
if [ "$mode" = certify ]; then
    merge_status="$status"
    if [ "$merge_status" = 0 ] && no_findings; then
        replay_before="$(replay_corpus segment-2 pre-replay)"
        status="$replay_before"
    fi
    if [ "${replay_before:-}" = 0 ] && no_findings && [ "$encoding" = chain-v2 ]; then
        census_status=0
        python3 -I "$here/census_floor.py" gate --run "$run" --target "$target" --pin "$pin" --encoding "$encoding" \
            --floor-src "$root/node-l1/census/$target.floor.json" >"$run/logs/segment-2/floor.out" 2>&1 ||
            census_status=$?
    fi
    if [ "${replay_before:-}" = 0 ] && no_findings && [ "${census_status:-0}" = 0 ]; then
        seg="$run/logs/segment-3"
        mkdir -m 700 "$seg"
        status=0
        python3 -I "$here/window_driver.py" ${encoding:+--encoding "$encoding"} "$seg" "$window" "$wall_cap" "$run/tmp" \
            "$target" "$pin" "$run_id" "$bin" \
            "-artifact_prefix=$run/crashes/" "-max_total_time=$((wall_cap + 3600))" "-print_final_stats=1" \
            "-stop_file=$seg/stop" "$tdir/corpus" || status=$?
        if [ "$status" = 0 ] && no_findings &&
            python3 -I -c 'import json, sys; sys.exit(json.load(open(sys.argv[1]))["stopped_by"] != "cpu-window")' \
                "$seg/window.json" 2>/dev/null; then
            replay_after="$(replay_corpus segment-4 post-replay)"
        fi
    fi
    rm -f "$run/bin"
fi

# The verdict. (fuzzctl's selftest runs this part, to the end of the file, on fixture runs.)
divergences="$(find "$run/divergences" -type f ! -name '*.txt' | wc -l)"
# libFuzzer's slow-unit artifacts are counted apart and do not fail the run: at load 150-250
# every input is slow (BL-83). Timeouts (timeout-*) and crashes still fail it.
crashes="$(find "$run/crashes" -type f ! -name minimized ! -name 'slow-unit-*' | wc -l)"
slow_units="$(find "$run/crashes" -type f -name 'slow-unit-*' | wc -l)"
refused="$(cat "$run"/logs/segment-*/fuzz.out 2>/dev/null | grep -c 'refusing to run' || true)"
python3 -I - "$run" "$target" "$pin" "${cpu_hours:-0}" "$mode" "$status" "$divergences" "$crashes" "$slow_units" "$refused" "$root" "${runs:-}" "$flavor" "${window:-}" "${wall_cap:-}" "${merge_status:-}" "${replay_before:-}" "${replay_after:-}" "$stop_grace" "${census_status:-}" "${coverage:-all}" "${trace_cmp:-1}" <<'EOF'
import glob, json, os, re, subprocess, sys
run, target, pin, budget, mode, status, divergences, crashes, slow_units, refused, root, runs, flavor, window, wall_cap, merge_status, replay_before, replay_after, stop_grace, census_status, coverage, trace_cmp = sys.argv[1:]
cpu = sum(sum(float(x) for x in open(f).read().split()[-2:]) for f in glob.glob(os.path.join(run, "logs", "*", "cpu"))) / 3600
# One log per fuzzing process: each segment's fuzz-<i>.log under -jobs, else its fuzz.out. Under
# -jobs the parent's fuzz.out repeats each worker's log as the worker exits, so reading both
# counts every execution and sink failure twice (BL-86).
logs = []
for seg in glob.glob(os.path.join(run, "logs", "segment-*")):
    logs += glob.glob(os.path.join(seg, "fuzz-*.log")) or glob.glob(os.path.join(seg, "fuzz.out"))
execs = sink_failures = 0
for log in logs:
    text = open(log, errors="replace").read()
    execs += sum(int(m.group(1)) for m in re.finditer(r"stat::number_of_executed_units:\s*(\d+)", text))
    sink_failures += sum("sink write failed" in line for line in text.splitlines())
def rev(repo):
    return subprocess.run(["git", "-C", repo, "rev-parse", "--short=12", "HEAD"], capture_output=True, text=True).stdout.strip() or "-"
lock = open(os.path.join(root, "Cargo.lock")).read()
base_sdk = (re.search(r"base-sdk\?rev=([0-9a-f]{40})", lock) or [None, "-"])[1][:12]
pins = {l.split()[0]: l.split() for l in open(os.path.join(root, "dash-oracle-sys", "pins.txt")) if l.strip() and not l.startswith("#")}
pin_row = pins.get(pin, ["-", "-", "-", "-", "-"])
dirty = bool(subprocess.run(["git", "-C", root, "status", "--porcelain", "--untracked-files=no"], capture_output=True, text=True).stdout.strip())
source_rev = rev(root)
# The source as the build saw it (run.sh's `source`, of this run); a certify run without it has no
# known source.
try:
    source_rev, built, source_run, _ = open(os.path.join(run, "source")).read().split()
    if source_run != os.path.basename(run):
        raise ValueError("the source record of another run")
    dirty = built != "clean"
except (OSError, ValueError):
    if mode == "certify":
        source_rev, dirty = "-", None
divergences, crashes, slow_units, refused, status = map(int, (divergences, crashes, slow_units, refused, status))
# A chain-v2 target's certify window runs only after its census floor gate passed (census_floor.py:
# 0 met, 1 missed, else an error; not run: n/a).
census_floor = {"": "n/a", "0": "met", "1": "missed"}.get(census_status, "error")
budget_met = mode != "run" or bool(runs) or cpu >= float(budget)
minimized = os.path.isfile(os.path.join(run, "crashes", "minimized"))
cmin = {}
clean = divergences == 0 and sink_failures == 0 and refused == 0
if mode in ("minimize", "minimize-divergence"):
    # libFuzzer's minimisation replays the crash (its own crash files are expected) and exits 0
    # once it has written the smallest crashing input it found to -exact_artifact_path.
    ok = clean and status == 0 and minimized
elif mode == "certify":
    # Passes when every step ran as intended: the merge and the first replay exited 0, the driver
    # stopped the window (at its CPU window through the stop file, after which libFuzzer exited 0
    # unkilled and the second replay exited 0; or at the wall cap, SIGINT's exit 72 or a kill of a
    # process that outlived it), and libFuzzer exited within $stop_grace seconds of the stop.
    # Whether it certifies is fuzzctl's to derive (DEC-186).
    try:
        w = json.load(open(os.path.join(run, "logs", "segment-3", "window.json")))
        stopped, killed, overdue = w["stopped_by"], w["killed"] is True, not w["stop_to_exit"] <= float(stop_grace)
    except (OSError, ValueError, KeyError, TypeError):
        stopped, killed, overdue = None, False, True
    ended = not overdue and ((stopped == "cpu-window" and status == 0 and not killed and replay_after == "0") or
                             (stopped == "wall-cap" and (status == 72 or (killed and status == 137))))
    ok = (clean and crashes == 0 and merge_status == "0" and replay_before == "0" and ended
          and census_floor in ("met", "n/a"))
elif mode == "minimize-corpus":
    # BL-841: the merge exited 0 with no finding, and its corpus was swapped in (run.sh's counts).
    try:
        cmin = dict(zip(("files_before", "bytes_before", "files_after", "bytes_after", "swapped"),
                        map(int, open(os.path.join(run, "cmin.counts")).read().split())))
    except (OSError, ValueError):
        cmin = {}
    ok = clean and crashes == 0 and status == 0 and cmin.get("swapped") == 1
else:
    ok = clean and crashes == 0 and budget_met and status == 0
# A shard run (BL-841b): its counts, and it passes only once its new inputs were folded.
try:
    shard = json.load(open(os.path.join(run, "shards.json")))
except (OSError, ValueError):
    shard = None
if shard is not None:
    ok = ok and "folded" in shard
# A budgeted segment whose driver stopped it at its CPU allotment with a process that never printed
# INITED spent the budget loading its corpus: libFuzzer reads its stop file only once fuzzing, so it
# went on loading until the kill (BL-333).
def before_inited(seg):
    try:
        stopped = json.load(open(os.path.join(seg, "segment.json")))["stopped_by"] == "cpu-window"
    except (OSError, ValueError, KeyError, TypeError):
        return False
    procs = glob.glob(os.path.join(seg, "fuzz-*.log")) or glob.glob(os.path.join(seg, "fuzz.out"))
    return stopped and any(not re.search(r"^#\d+\s+INITED\b", open(p, errors="replace").read(), re.M) for p in procs)
reason = None
if not ok:
    reason = next(why for failed, why in (
        (divergences, "divergences"), (crashes, "crashes"), (sink_failures, "sink failures"),
        (refused, "the target refused to run"),
        (mode == "certify" and census_floor not in ("met", "n/a"), "census floor not met"),
        (any(map(before_inited, glob.glob(os.path.join(run, "logs", "segment-*")))),
         "budget spent before INITED (corpus load)"),
        (not budget_met, "budget not met"), (status, f"libFuzzer exited {status}"),
        (mode == "minimize-corpus" and cmin.get("files_after") == 0, "the merge kept no input"),
        (shard is not None and "folded" not in shard, "the shard run's new inputs were not folded"),
        (True, "the run did not end as intended")) if failed)
verdict = {
    "target": target, "pin": pin, "oracle_commit": pin_row[1][:12], "oracle_flavor": flavor,
    "oracle_archive_sha256": pin_row[4] if flavor == "fuzz" else pin_row[3],
    "mode": mode, "run": os.path.basename(run), "rust_dashcore": source_rev, "rust_dashcore_dirty": dirty, "base_sdk": base_sdk,
    "toolchain": subprocess.run(["rustc", "--version"], capture_output=True, text=True, cwd=root).stdout.strip(),
    "cpu_hours": round(cpu, 4), "budget_cpu_hours": float(budget), "executions": execs,
    "divergences": divergences, "crashes": crashes, "sink_failures": sink_failures,
    "slow_units": slow_units, "refused": refused, "libfuzzer_status": status, "minimized": minimized, "segments": len(glob.glob(os.path.join(run, "logs", "segment-*"))),
    "status": "pass" if ok else "fail", "reason": reason,
    "coverage": coverage, "rust_trace_cmp": trace_cmp == "1",
}
if mode == "certify":
    exit_status = lambda x: int(x) if x.lstrip("-").isdigit() else None
    verdict["certify"] = {"engine": "single", "jobs": 1, "seconds": int(window), "wall_cap": int(wall_cap),
                          "merge_status": exit_status(merge_status), "replay_before_status": exit_status(replay_before),
                          "replay_after_status": exit_status(replay_after)}
    verdict["census_floor"] = census_floor
if mode == "minimize-corpus":
    verdict["cmin"] = cmin
if shard is not None:
    verdict["shard"] = {k: shard.get(k) for k in ("jobs", "inputs", "folded", "duplicates", "fold_redundant")}
fd = os.open(os.path.join(run, "verdict.json"), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
with os.fdopen(fd, "w") as f:
    json.dump(verdict, f, indent=2)
# A build other than the default is named before the status (BL-812).
build = "" if (coverage, trace_cmp) == ("all", "1") else f" coverage={coverage} rust_trace_cmp={trace_cmp}"
print("node-fuzz " + " ".join(f"{k}={verdict[k]}" for k in (
    "target", "pin", "mode", "cpu_hours", "budget_cpu_hours", "executions", "divergences",
    "crashes", "sink_failures", "slow_units", "rust_dashcore", "rust_dashcore_dirty", "base_sdk", "oracle_commit",
    "oracle_flavor")) + build
      + ("".join(f" cmin_{k}={cmin.get(k, '-')}" for k in ("files_before", "bytes_before", "files_after", "bytes_after"))
         if mode == "minimize-corpus" else "")
      + f" status={verdict['status']}" + (f" ({reason})" if reason else ""))
sys.exit(0 if ok else 1)
EOF
