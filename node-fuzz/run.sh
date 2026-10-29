#!/usr/bin/env bash
# EMBARGO-1
# The only way to run a node-fuzz target (node program CS0 §9.3, O3; PLAN §7 handling).
#
#   node-fuzz/run.sh <target> --pin v24.0 --cpu-hours H [--jobs J] [--public-seeds DIR]
#                    [--run-tag TAG] [--corpus-sample N --corpus-sample-bytes B]
#   node-fuzz/run.sh <target> --pin P --merge            (merge the working corpus in place)
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
# --corpus-sample N --corpus-sample-bytes B (the smoke's, BL-333) fuzz from a sample of the working
# corpus and the public seeds instead of all of them: at most N files and B bytes, chosen by the
# sha256 of their names, reflinked into the run's sample/ (removed at the end), which takes
# libFuzzer's new inputs. The working corpus is only read. The private seeds and the fixed
# regression vectors are loaded in full, as always.
#
# The target must be listed in node-fuzz/targets.toml, with the pin among its pins; the
# registry's public_seeds are used when --public-seeds is not given. Everything libFuzzer
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
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"

fail() { echo "run.sh: $*" >&2; exit 2; }
# The longest a certify window may run on after its stop (seconds; more than libFuzzer's 1200 s
# -timeout for one input). The driver kills a process that outlives it, and the verdict (as
# fuzzctl does) fails a window whose exit came later, killed or not (DEC-186 amendment 3 (a)).
stop_grace=1500

selftest() {
    local out status runs
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
    echo "node-fuzz selftest: ok"
}

[ "${1:-}" = "--selftest" ] && { selftest; exit 0; }

target="${1:-}"; shift || true
[ -n "$target" ] || fail "usage: run.sh <target> --pin v24.0 --cpu-hours H [--jobs J] [--public-seeds DIR] | --selftest"
pin="" cpu_hours="" jobs=1 public="" mode=run minimize="" order="" job_inputs=() runs="" job="" run_tag="" window="" wall_cap="" merge_status=""
replay_before="" replay_after="" sample_count="" sample_bytes=""
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
        *) fail "unknown argument $1" ;;
    esac
done
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

# The registry: the target and pin must be listed. Prints the target's public seed directories.
registry_seeds="$(python3 -I - "$here/targets.toml" "$target" "$pin" <<'EOF'
import sys, tomllib
path, name, pin = sys.argv[1:]
found = [t for t in tomllib.load(open(path, "rb")).get("target", []) if t.get("name") == name]
if not found:
    sys.exit(3)
if found[0].get("kind") != "selftest" and pin not in found[0].get("pins", []):
    sys.exit(4)
print("\n".join(found[0].get("public_seeds", [])))
EOF
)" || case $? in
    3) fail "$target is not in node-fuzz/targets.toml" ;;
    4) fail "--pin $pin is not one of the target's pins in node-fuzz/targets.toml" ;;
    *) fail "node-fuzz/targets.toml is unreadable" ;;
esac

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
for d in "$tdir" "$tdir/corpus" "$tdir/public-seeds" "$tdir/runs"; do
    [ -d "$d" ] || mkdir -m 700 "$d" 2>/dev/null || fail "the private sink is not writable"
done
# The leg's lock, held to the end: fuzzing runs and merges share it, a certify run holds it alone
# (nothing else may write the working corpus between its replays), and jobs, which never write
# the corpus, take none. A run that cannot take it fails at once; a farm slice the certify run
# blocks is a failed slice.
if [ -z "$job" ]; then
    exec 8>>"$tdir/run.lock" || fail "the private sink is not writable"
    flock -n "$([ "$mode" = certify ] && echo -x || echo -s)" 8 ||
        fail "another run holds the leg (a certify run excludes fuzzing and merges of its target)"
fi
mkdir -m 700 "$run" 2>/dev/null || fail "the private sink is not writable"
# The run's mode, first: triage must never take a job killed before its verdict for fuzzing.
printf '%s\n' "$mode" >"$run/mode" || fail "the private sink is not writable"
for d in divergences crashes logs tmp; do mkdir -m 700 "$run/$d" || fail "the private sink is not writable"; done
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
features=()
[ "$target" = selftest_mismatch ] && features=(--features selftest)
# The oracle's coverage-instrumented flavour, so libFuzzer also sees Core's side of each
# comparison (O1's `fuzz` archives); DASH_ORACLE_FLAVOR=plain overrides it.
flavor="${DASH_ORACLE_FLAVOR:-fuzz}"
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
# a server it starts (sccache) would hold them.
mkdir -p "$here/target"
exec 9>>"$here/target/.node-fuzz-build.lock"
flock -x 9
before="$(tree_state)"
(cd "$here" && DASH_ORACLE_FLAVOR="$flavor" RUSTFLAGS="--cfg fuzzing -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 -Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table -Cllvm-args=-sanitizer-coverage-trace-compares" \
    cargo build --release --locked --target "$host" --target-dir "$here/target" --bin "$target" "${features[@]}" 8>&- 9>&-) \
    >"$run/logs/build.log" 2>&1 ||
    fail "the build failed; see the run's logs/build.log"
after="$(tree_state)"
bin="$here/target/$host/release/$target"
binary_sha=-
if [ "$mode" = certify ]; then
    trap 'rm -f "$run/bin"' EXIT
    cp --reflink=auto "$bin" "$run/bin" || fail "copying the binary into the run failed (is the private sink full?)"
    bin="$run/bin"
    binary_sha="$(sha256sum "$bin" | cut -c1-64)"
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
dest, count, cap, sources = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4:]
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
case "$mode" in
    run)
        if [ -n "$runs" ]; then
            args+=("-runs=$runs")
        else
            time_limited=1
        fi
        [ "$jobs" -gt 1 ] && args+=("-jobs=$jobs" "-workers=$jobs")
        # The farm stops a slice by writing $run/stop (BL-173): every worker reads it at the top of
        # its loop, prints its final stats and exits 0. A SIGINT can leave a worker hung in
        # libFuzzer's interrupt path until its ALARM writes an empty timeout artifact. (A budgeted
        # segment's -stop_file is its own, which the driver also fills from $run/stop; see below.)
        args+=("-print_final_stats=1")
        inputs=("${corpus[@]}")
        ;;
    merge|certify) args+=("-merge=1"); inputs=("${corpus[@]}") ;;
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
    if [ "$time_limited" = 1 ]; then
        # The driver builds the target's clean environment itself (nothing the caller exported
        # reaches it) and writes the segment's fuzz.out and cpu files.
        python3 -I "$here/window_driver.py" --farm "$run/stop" "$seg" "$cpu_seconds" "$((segment_backstop + 3600))" \
            "$run/tmp" "$target" "$pin" "$run_id" "$bin" "${segment_args[@]}" "${inputs[@]}" || status=$?
    else
        # A clean environment: nothing the caller exported (RUST_LIBFUZZER_DEBUG_PATH, ASAN_OPTIONS, …)
        # reaches the target.
        (cd "$seg" && env -i PATH="$PATH" HOME="$HOME" TMPDIR="$run/tmp" DASH_EMBARGO_DIR="$DASH_EMBARGO_DIR" \
            NODE_FUZZ_PIN="$pin" NODE_FUZZ_RUN="$run_id" RUST_BACKTRACE=1 \
            /usr/bin/time -f '%U %S' -o "$seg/cpu" "$bin" "${segment_args[@]}" "${inputs[@]}" >"$seg/fuzz.out" 2>&1) || status=$?
    fi
    # A slow unit (libFuzzer's -report_slow_units artifact) is no finding (BL-83).
    [ "$time_limited" = 1 ] && [ "$status" = 0 ] &&
        [ -z "$(find "$run/divergences" "$run/crashes" -type f ! -name 'slow-unit-*' -print -quit)" ] || break
done

# A certify run (DEC-186 amendments 2 and 3): replay passes before and after the window measure
# the working corpus; between them, the window. Each replay runs the binary once over the corpus
# with -runs=0 (libFuzzer loads it, prints INITED with its cov and ft, and stops) and its exit
# status goes to the verdict. The window is one libFuzzer process on the corpus that window_driver.py
# ends through its stop file once the process has used $window seconds of its own CPU time after
# INITED, with $wall_cap seconds of wall time and $stop_grace seconds from the stop to the exit as
# its limits (the driver's header has the rules). Its -max_total_time (libFuzzer's own clock, which
# follows the civil clock) is only a backstop.
replay_corpus() {  # <segment>: prints the replay's exit status
    local seg="$run/logs/$1" rc=0
    mkdir -m 700 "$seg"
    (cd "$seg" && env -i PATH="$PATH" HOME="$HOME" TMPDIR="$run/tmp" DASH_EMBARGO_DIR="$DASH_EMBARGO_DIR" \
        NODE_FUZZ_PIN="$pin" NODE_FUZZ_RUN="$run_id" RUST_BACKTRACE=1 \
        /usr/bin/time -f '%U %S' -o "$seg/cpu" "$bin" "-artifact_prefix=$run/crashes/" -runs=0 \
        -print_final_stats=1 "$tdir/corpus" >"$seg/fuzz.out" 2>&1) || rc=$?
    echo "$rc"
}
no_findings() { [ -z "$(find "$run/divergences" "$run/crashes" -type f ! -name 'slow-unit-*' -print -quit)" ]; }
if [ "$mode" = certify ]; then
    merge_status="$status"
    if [ "$merge_status" = 0 ] && no_findings; then
        replay_before="$(replay_corpus segment-2)"
        status="$replay_before"
    fi
    if [ "${replay_before:-}" = 0 ] && no_findings; then
        seg="$run/logs/segment-3"
        mkdir -m 700 "$seg"
        status=0
        python3 -I "$here/window_driver.py" "$seg" "$window" "$wall_cap" "$run/tmp" "$target" "$pin" "$run_id" "$bin" \
            "-artifact_prefix=$run/crashes/" "-max_total_time=$((wall_cap + 3600))" "-print_final_stats=1" \
            "-stop_file=$seg/stop" "$tdir/corpus" || status=$?
        if [ "$status" = 0 ] && no_findings &&
            python3 -I -c 'import json, sys; sys.exit(json.load(open(sys.argv[1]))["stopped_by"] != "cpu-window")' \
                "$seg/window.json" 2>/dev/null; then
            replay_after="$(replay_corpus segment-4)"
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
python3 -I - "$run" "$target" "$pin" "${cpu_hours:-0}" "$mode" "$status" "$divergences" "$crashes" "$slow_units" "$refused" "$root" "${runs:-}" "$flavor" "${window:-}" "${wall_cap:-}" "${merge_status:-}" "${replay_before:-}" "${replay_after:-}" "$stop_grace" <<'EOF'
import glob, json, os, re, subprocess, sys
run, target, pin, budget, mode, status, divergences, crashes, slow_units, refused, root, runs, flavor, window, wall_cap, merge_status, replay_before, replay_after, stop_grace = sys.argv[1:]
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
budget_met = mode != "run" or bool(runs) or cpu >= float(budget)
minimized = os.path.isfile(os.path.join(run, "crashes", "minimized"))
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
    ok = clean and crashes == 0 and merge_status == "0" and replay_before == "0" and ended
else:
    ok = clean and crashes == 0 and budget_met and status == 0
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
        (any(map(before_inited, glob.glob(os.path.join(run, "logs", "segment-*")))),
         "budget spent before INITED (corpus load)"),
        (not budget_met, "budget not met"), (status, f"libFuzzer exited {status}"),
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
}
if mode == "certify":
    exit_status = lambda x: int(x) if x.lstrip("-").isdigit() else None
    verdict["certify"] = {"engine": "single", "jobs": 1, "seconds": int(window), "wall_cap": int(wall_cap),
                          "merge_status": exit_status(merge_status), "replay_before_status": exit_status(replay_before),
                          "replay_after_status": exit_status(replay_after)}
fd = os.open(os.path.join(run, "verdict.json"), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
with os.fdopen(fd, "w") as f:
    json.dump(verdict, f, indent=2)
print("node-fuzz " + " ".join(f"{k}={verdict[k]}" for k in (
    "target", "pin", "mode", "cpu_hours", "budget_cpu_hours", "executions", "divergences",
    "crashes", "sink_failures", "slow_units", "rust_dashcore", "rust_dashcore_dirty", "base_sdk", "oracle_commit",
    "oracle_flavor", "status")) + (f" ({reason})" if reason else ""))
sys.exit(0 if ok else 1)
EOF
