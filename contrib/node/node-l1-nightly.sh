#!/usr/bin/env bash
# node-l1's nightly (BL-548, DEC-905): builds node-l1's oracle test binaries at the checkout's HEAD
# and runs each nightly component (a test) alone at nice 10, recording its CPU time.
#
# usage: contrib/node/node-l1-nightly.sh OUT_DIR [COMPONENT...]
#        contrib/node/node-l1-nightly.sh --selftest
#   With no components, runs every one. OUT_DIR gets head.txt, build.log, a log and a time file
#   per component, and summary.txt: the seed, then one row per component, then DONE.
#   pairs_sweep_every_argument_d19 runs D19's ordered pairs with every argument pair below 16
#   (CI's pairs_first_reject_d19 runs four) from a seeded start, until its wall-clock limit.
#   L3's components (CH3a a10-5, plan §3.4) run against a real dashd v24 and keep their reports
#   in OUT_DIR: l3_full (the full set, l3_full.json), l3_warn (the warn leg, l3_warn.json), l3_av
#   (the six AV fixtures, l3_av.json), l3_devnet (the devnet walk, l3_devnet.json, which
#   passes on its own rule) and l3_live (OD-L3-18's live witnesses, both tests of l3_live: the
#   MNHF bodies, l3_live.json, and the DIP3 parent). When l3_full, l3_warn and l3_av all run,
#   the l3_census union step follows: `l3-census.py check --require-full` over their three
#   reports, at head.txt's head when this run built it (`--expect-head`; census.txt); the
#   devnet report is never its input (M-12). Each L3 test fails closed without DASHVERSION,
#   its dashd (L3_DASHD) and L3_DASH_TREE.
#   node-l1's other ignored tests need inputs or hands a nightly has not got; they are not here.
#   --selftest runs this script over stub binaries and a stub census and checks its exits.
#
# The exit status is non-zero if the build fails, any component fails, including one that runs
# no test (a stale or misspelled name under --exact), the census union fails or cannot run, or
# a required artifact (head.txt, a log, a time file, summary.txt) cannot be written. Every
# component runs regardless.
#
# env:
#   L1_SWEEP_SEED        the sweep's seed; default: the UTC date (YYYYMMDD), so its start rotates
#                        nightly. summary.txt records it; set it to replay a night's sweep.
#   L1_SWEEP_SECONDS     the sweep's wall-clock limit; default 3000. A component that overruns
#                        it by 600 s is killed and fails.
#   L3_NIGHTLY_SECONDS   every L3 component's wall-clock limit, then killed; default per
#                        component: l3_full 7200 (OD-L3-72 m2; the full set took 2143 s
#                        wall at rc.5 / ABI 33 on agentbox), the others 3600.
#   L1_NIGHTLY_BIN_DIR   skip the build and run the binaries in this directory, named by test
#                        target (l1_specialtx, l3_dashd, l3_warn, l3_av, l3_devnet_walk,
#                        l3_live).
#   L3_CENSUS            the census command (default: python3 -I contrib/node/l3-census.py).
set -u

COMPONENTS=(pairs_sweep_every_argument_d19 l3_full l3_warn l3_av l3_devnet l3_live)
# The census union's inputs, in its argument order (plan §6.3).
UNION=(l3_full l3_warn l3_av)

# The test target each component is in.
target() {
  case $1 in
    l3_full) echo l3_dashd ;;
    l3_warn) echo l3_warn ;;
    l3_av) echo l3_av ;;
    l3_devnet) echo l3_devnet_walk ;;
    l3_live) echo l3_live ;;
    *) echo l1_specialtx ;;
  esac
}

# The component's test in its target (run with --exact); none for l3_live, which runs its
# target's every test.
test_name() {
  case $1 in
    l3_warn) echo the_unknown_bit_warns_as_dashd_does ;;
    l3_av) echo the_av_fixtures_skip_only_what_assumevalid_skips ;;
    l3_devnet) echo the_devnet_walk_skips_the_period_below_its_minimum ;;
    l3_live) ;;
    *) echo "$1" ;;
  esac
}

# How many tests the component must pass: l3_live's two, else one.
tests_expected() {
  case $1 in
    l3_live) echo 2 ;;
    *) echo 1 ;;
  esac
}

# An L3 component's default wall-clock limit (OD-L3-72 m2).
l3_limit() {
  case $1 in
    l3_full) echo 7200 ;;
    *) echo 3600 ;;
  esac
}

# The environment variable an L3 component writes its report to.
report_var() {
  case $1 in
    l3_full) echo L3_REPORT ;;
    l3_warn) echo L3_WARN_REPORT ;;
    l3_av) echo L3_AV_REPORT ;;
    l3_devnet) echo L3_WALK_REPORT ;;
    l3_live) echo L3_MNHF_REPORT ;;
  esac
}

selftest() {
  local dir self=$0 fails=0
  dir=$(mktemp -d "${TMPDIR:-/work/tmp}/node-l1-nightly-selftest.XXXXXX") || return 2
  mkdir -p "$dir/bin"
  # A stub test binary: `--exact NAME` passes one test (l3_live, run without, two), writing its
  # report if it has one, unless STUB_FAIL or STUB_NOTEST names its target, or STUB_ONE does
  # (one test passes); it records its limit (`timeout`'s) as <target>.limit.
  cat > "$dir/bin/stub" <<'EOF'
#!/usr/bin/env bash
t=$(basename "$0")
[ "${STUB_FAIL:-}" = "$t" ] && { echo "test result: FAILED. 0 passed; 1 failed"; exit 101; }
[ "${STUB_NOTEST:-}" = "$t" ] && { echo "test result: ok. 0 passed; 0 failed"; exit 0; }
for v in L3_REPORT L3_WARN_REPORT L3_AV_REPORT L3_WALK_REPORT L3_MNHF_REPORT; do
  case "$t:$v" in
    l3_dashd:L3_REPORT|l3_warn:L3_WARN_REPORT|l3_av:L3_AV_REPORT|l3_devnet_walk:L3_WALK_REPORT|l3_live:L3_MNHF_REPORT)
      echo '{}' > "${!v}" ;;
  esac
done
n=1
[ "$t" = l3_live ] && [ "$1" != --exact ] && n=2
[ "${STUB_ONE:-}" = "$t" ] && n=1
echo "test result: ok. $n passed; 0 failed"
EOF
  chmod +x "$dir/bin/stub"
  for t in l1_specialtx l3_dashd l3_warn l3_av l3_devnet_walk l3_live; do
    ln -s stub "$dir/bin/$t"
  done
  # A stub timeout: records each component's limit, then runs the command.
  printf '#!/usr/bin/env bash\n[ "$1" = -k ] && shift 2\necho "$1" >> "%s/limits"\nshift\nexec "$@"\n' "$dir" \
    > "$dir/bin/timeout"
  chmod +x "$dir/bin/timeout"
  # A stub census: records its arguments, exits STUB_CENSUS_RC.
  printf '#!/usr/bin/env bash\necho "$@" > "%s/census.args"\nexit "${STUB_CENSUS_RC:-0}"\n' "$dir" \
    > "$dir/census"
  chmod +x "$dir/census"
  run() {
    local out=$dir/out.$RANDOM
    : > "$dir/limits"
    env L1_NIGHTLY_BIN_DIR="$dir/bin" L3_CENSUS="$dir/census" DASHVERSION=24.0.0-rc.4 \
      L3_DASH_TREE=/nonexistent L1_SWEEP_SECONDS=5 L3_NIGHTLY_SECONDS=30 NIGHTLY_TIMEOUT="$dir/bin/timeout" "$@" \
      "$self" "$out" > "$dir/last.log" 2>&1
    rc=$?
    # shellcheck disable=SC2034 # read by expect's checks
    summary=$(cat "$out/summary.txt" 2> /dev/null)
  }
  expect() {
    if ! eval "$2"; then
      echo "node-l1-nightly: selftest FAILED: $1" >&2
      fails=$((fails + 1))
    fi
  }
  run
  expect "every component passes" '[ $rc -eq 0 ] && grep -q "l3_census exit=0 status=ok" <<< "$summary"'
  expect "the union takes full, warn and av, in order" \
    'grep -q -- "--require-full $dir/out\.[0-9]*/l3_full.json $dir/out\.[0-9]*/l3_warn.json $dir/out\.[0-9]*/l3_av.json$" "$dir/census.args"'
  run STUB_FAIL=l3_av
  expect "a failed component fails the run" '[ $rc -ne 0 ] && grep -q "l3_av exit=101 status=FAILED" <<< "$summary"'
  expect "the union does not run without its inputs" 'grep -q "l3_census status=NOT-RUN" <<< "$summary"'
  run STUB_NOTEST=l3_warn
  expect "a component that runs no test fails" '[ $rc -ne 0 ] && grep -q "l3_warn exit=0 status=NO-TEST" <<< "$summary"'
  run STUB_ONE=l3_live
  expect "l3_live passing one test of its two fails" \
    '[ $rc -ne 0 ] && grep -q "l3_live exit=0 status=NO-TEST" <<< "$summary"'
  run
  expect "l3_live runs and keeps its report" '[ $rc -eq 0 ] && grep -q "l3_live exit=0 status=ok" <<< "$summary"'
  expect "L3_NIGHTLY_SECONDS overrides every L3 limit" \
    '[ "$(tr "\n" " " < "$dir/limits")" = "605 30 30 30 30 30 " ]'
  run L3_NIGHTLY_SECONDS=
  expect "l3_full's limit is 7200 s, the other L3 components' 3600 s" \
    '[ "$(tr "\n" " " < "$dir/limits")" = "605 7200 3600 3600 3600 3600 " ]'
  run STUB_CENSUS_RC=1
  expect "a failed union fails the run" '[ $rc -ne 0 ] && grep -q "l3_census exit=1 status=FAILED" <<< "$summary"'
  run
  expect "DONE counts nothing failed" 'grep -q "^DONE failed=0 unwritten=0$" <<< "$summary"'
  rm "$dir/bin/l3_av"
  run
  expect "a missing binary fails" '[ $rc -ne 0 ] && grep -q "l3_av status=NO-BINARY" <<< "$summary"'
  ln -s stub "$dir/bin/l3_av"
  local out=$dir/out.devnet
  env L1_NIGHTLY_BIN_DIR="$dir/bin" L3_CENSUS="$dir/census" "$self" "$out" l3_devnet > /dev/null 2>&1
  rc=$?
  expect "the devnet walk alone passes on its own, no union" \
    '[ $rc -eq 0 ] && ! grep -q l3_census "$out/summary.txt"'
  if [ $fails -eq 0 ]; then
    echo "node-l1-nightly: selftest passed"
    # Only a passing selftest cleans up; a failing one keeps its outputs.
    rm -r "$dir"
  else
    echo "node-l1-nightly: selftest outputs kept in $dir" >&2
  fi
  [ $fails -eq 0 ]
}

if [ "${1:-}" = --selftest ]; then
  selftest
  exit
fi
if [ $# -lt 1 ]; then
  sed -n '2,39p' "$0" >&2
  exit 2
fi
OUT=$(realpath -m "$1")
shift
[ $# -eq 0 ] && set -- "${COMPONENTS[@]}"
mkdir -p "$OUT" || exit 2
SUMMARY=$OUT/summary.txt
# Every write of a required artifact goes through here: a report the runner could not write
# fails the run, though the components still run.
unwritten=0
report() {
  if ! printf '%s\n' "$*" >> "$SUMMARY"; then
    echo "node-l1-nightly: cannot write $SUMMARY: $*" >&2
    unwritten=$((unwritten + 1))
  fi
}
if ! : > "$SUMMARY"; then
  echo "node-l1-nightly: cannot create $SUMMARY" >&2
  unwritten=$((unwritten + 1))
fi

export L1_SWEEP_SEED=${L1_SWEEP_SEED:-$(date -u +%Y%m%d)}
export L1_SWEEP_SECONDS=${L1_SWEEP_SECONDS:-3000}
# The timeout command (the selftest's stub records each limit).
TIMEOUT=${NIGHTLY_TIMEOUT:-timeout}
report "seed L1_SWEEP_SEED=$L1_SWEEP_SEED limit L1_SWEEP_SECONDS=$L1_SWEEP_SECONDS"

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
declare -A BIN
if [ -n "${L1_NIGHTLY_BIN_DIR:-}" ]; then
  for c in "$@"; do
    t=$(target "$c")
    # -s: the path as named, symlinks kept (a binary may tell its target by its name).
    BIN[$t]=$(realpath -ms "$L1_NIGHTLY_BIN_DIR/$t")
  done
else
  if ! git -C "$ROOT" rev-parse HEAD > "$OUT/head.txt"; then
    echo "node-l1-nightly: cannot write $OUT/head.txt" >&2
    unwritten=$((unwritten + 1))
  fi
  # The executables this build produced (not the newest file in target/): the oracle targets,
  # then the L3 ones (feature l3), each only if a component needs it.
  : > "$OUT/build.json"
  : > "$OUT/build.log"
  declare -A WANT
  for c in "$@"; do WANT[$(target "$c")]=1; done
  for features in oracle l3; do
    args=()
    for t in "${!WANT[@]}"; do
      case $features:$t in
        oracle:l1_specialtx | l3:l3_*) args+=(--test "$t") ;;
      esac
    done
    [ ${#args[@]} -eq 0 ] && continue
    if ! (cd "$ROOT" && cargo test -p node-l1 --features "$features" "${args[@]}" --no-run \
        --message-format=json 2>> "$OUT/build.log" >> "$OUT/build.json"); then
      report "BUILD FAILED (build.log)"
      exit 1
    fi
  done
  while read -r name exe; do
    BIN[$name]=$exe
  done < <(jq -r 'select(.reason == "compiler-artifact" and .profile.test and .executable)
                  | "\(.target.name) \(.executable)"' "$OUT/build.json")
fi

failed=0
declare -A PASSED
for c in "$@"; do
  bin=${BIN[$(target "$c")]:-}
  log=$OUT/$c.log
  if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    report "- - - $c status=NO-BINARY"
    failed=$((failed + 1))
    continue
  fi
  limit=$((L1_SWEEP_SECONDS + 600))
  envs=()
  var=$(report_var "$c")
  if [ -n "$var" ]; then
    limit=${L3_NIGHTLY_SECONDS:-$(l3_limit "$c")}
    # A reused OUT_DIR's old report must not stand for this run's.
    rm -f "$OUT/$c.json"
    envs=("$var=$OUT/$c.json")
  fi
  # A reused OUT_DIR's old times must not stand for this run's.
  rm -f "$OUT/$c.time"
  filter=()
  name=$(test_name "$c")
  [ -n "$name" ] && filter=(--exact "$name")
  (cd "$ROOT/node-l1" && env "${envs[@]}" /usr/bin/time -f "%U %S %e" -o "$OUT/$c.time" \
    "$TIMEOUT" -k 60 "$limit" nice -n 10 \
    "$bin" "${filter[@]}" --include-ignored --nocapture > "$log" 2>&1)
  rc=$?
  status=ok
  times=$(tail -1 "$OUT/$c.time" 2> /dev/null)
  if [ $rc -ne 0 ]; then
    status=FAILED
  elif ! grep -Eq "^test result: ok\. $(tests_expected "$c") passed" "$log" 2> /dev/null; then
    status=NO-TEST
  elif [ -z "$times" ]; then
    status=NO-TIME
  elif [ -n "$var" ] && [ ! -s "$OUT/$c.json" ]; then
    status=NO-REPORT
  fi
  if [ $status = ok ]; then
    PASSED[$c]=1
  else
    failed=$((failed + 1))
  fi
  report "${times:-- - -} $c exit=$rc status=$status"
done

# The census union (plan §6.3), when every input component was asked for.
asked=0
for u in "${UNION[@]}"; do
  for c in "$@"; do [ "$c" = "$u" ] && asked=$((asked + 1)); done
done
if [ $asked -eq ${#UNION[@]} ]; then
  inputs=()
  for u in "${UNION[@]}"; do
    [ -n "${PASSED[$u]:-}" ] && inputs+=("$OUT/$u.json")
  done
  if [ ${#inputs[@]} -ne ${#UNION[@]} ]; then
    report "- - - l3_census status=NOT-RUN (an input component failed)"
    failed=$((failed + 1))
  else
    if [ -n "${L3_CENSUS:-}" ]; then
      census=("$L3_CENSUS")
    else
      census=(python3 -I "$ROOT/contrib/node/l3-census.py")
    fi
    fixture=$ROOT/contrib/node/l3-census/inventory-v${DASHVERSION:-unset}.json
    # A build from this tree pins the union to its head; prebuilt binaries have none.
    expect_head=()
    [ -s "$OUT/head.txt" ] && expect_head=(--expect-head "$(cat "$OUT/head.txt")")
    "${census[@]}" check --tree "${L3_DASH_TREE:-unset}" --fixture "$fixture" \
      --require-full "${inputs[@]}" "${expect_head[@]}" > "$OUT/census.txt" 2>&1
    rc=$?
    status=ok
    [ $rc -eq 0 ] || { status=FAILED; failed=$((failed + 1)); }
    report "- - - l3_census exit=$rc status=$status"
  fi
fi
report "DONE failed=$failed unwritten=$unwritten"
[ $failed -eq 0 ] && [ $unwritten -eq 0 ]
