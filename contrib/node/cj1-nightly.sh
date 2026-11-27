#!/usr/bin/env bash
# CJ1's nightly: builds dashd-node's lib test binary at the checkout's HEAD and runs the
# CJ1-W7 acceptance legs (node::tests::coinjoin::accept) one after the other at nice 10,
# recording CPU and wall time.
#
# usage: contrib/node/cj1-nightly.sh OUT_DIR [COMPONENT...]
#        contrib/node/cj1-nightly.sh --selftest
#   The caller must run `eval $(python3 contrib/setup-dashd.py)` first (DASHD_PATH must be set;
#   the script fails early if it is unset).
#   With no components, runs python_leg then wallet_leg, never at once. OUT_DIR gets head.txt,
#   build.log, setup.log (when setting up the harness), a log and a time file per component,
#   <component>-evidence/ on failure (capped at ~200 MB), and summary.txt: one row per
#   component, then DONE.
#   A component passes only with exit 0, `test result: ok. 1 passed`, a `summary ` line in its
#   log, no `skipped:` line, and a recorded time file; skipped tests fail closed (SKIPPED).
#   --selftest runs this script over stub binaries and checks its exits.
#
# The exit status is non-zero if DASHD_PATH is unset, an unknown component is named, the build
# fails, harness setup fails, any component fails (including SKIPPED, NO-TEST, NO-SUMMARY,
# TIMEOUT), or a required artifact (head.txt, a log, a time file, summary.txt) cannot be
# written. Every component runs regardless (unless the build or setup fails).
#
# env:
#   CJ1_NIGHTLY_SECONDS  wall-clock limit (seconds) overriding both components; defaults are
#                        python_leg 1200 s and wallet_leg 1500 s (the wallet leg bounds itself
#                        at CJ1_WALLET_LEG_SECONDS, default 900). Overruns are killed and fail.
#   CJ1_NIGHTLY_BIN      skip the build and run this dashd-node lib test binary.
#   CJ1_PYTHON           path to the harness venv Python interpreter; used with CJ1_SCRATCH when
#                        $CJ1_SCRATCH/setup.json exists. Otherwise the script creates a venv and
#                        runs capture.py setup under OUT_DIR/cj1 (mode 700).
#   CJ1_SCRATCH          path to the harness scratch directory holding setup.json (see CJ1_PYTHON).
set -u

COMPONENTS=(python_leg wallet_leg)

# The libtest arguments each component runs with.
test_args() {
  case $1 in
    python_leg)
      printf '%s\n' \
        --exact \
        node::tests::coinjoin::accept::dashd_clients_see_the_rdashd_server_answer_as_dashd \
        --nocapture
      ;;
    wallet_leg)
      printf '%s\n' \
        --ignored \
        --exact \
        node::tests::coinjoin::accept::dashd_wallets_mix_a_round_through_the_rdashd_server \
        --nocapture
      ;;
  esac
}

# A component's default wall-clock limit in seconds.
default_limit() {
  case $1 in
    python_leg) echo 1200 ;;
    wallet_leg) echo 1500 ;;
  esac
}

# Copy failure evidence ($CJ1_SCRATCH/*-target-run.txt and $DASHD_TEST_RETAIN_DIR if set and
# non-empty) into OUT_DIR/<component>-evidence/, capped at ~200 MB.
collect_evidence() {
  local comp=$1 ev=$OUT/$1-evidence log=$OUT/$1.log
  local cap=${CJ1_NIGHTLY_EVIDENCE_CAP:-209715200}
  local copied=0 truncated=0 created=0 f rel sz retain
  rm -rf "$ev"

  copy_one() {
    local src=$1 dest=$2
    [ -f "$src" ] || return 0
    sz=$(stat -c %s "$src" 2> /dev/null || echo 0)
    if [ $((copied + sz)) -gt "$cap" ]; then
      truncated=1
      return 0
    fi
    if [ "$created" -eq 0 ]; then
      mkdir -p "$ev" || return 1
      created=1
    fi
    mkdir -p "$(dirname "$dest")" && cp -p "$src" "$dest" || return 1
    copied=$((copied + sz))
  }

  if [ -n "${CJ1_SCRATCH:-}" ] && [ -d "$CJ1_SCRATCH" ]; then
    shopt -s nullglob
    for f in "$CJ1_SCRATCH"/*-target-run.txt; do
      copy_one "$f" "$ev/$(basename "$f")"
    done
    shopt -u nullglob
  fi

  if [ -n "${DASHD_TEST_RETAIN_DIR:-}" ] && [ -d "$DASHD_TEST_RETAIN_DIR" ]; then
    retain=${DASHD_TEST_RETAIN_DIR%/}
    while IFS= read -r -d '' f; do
      rel=${f#"$retain"/}
      copy_one "$f" "$ev/retained/$rel"
    done < <(find "$retain" -type f -print0 2> /dev/null | LC_ALL=C sort -z)
  fi

  if [ "$truncated" -ne 0 ]; then
    if [ "$created" -eq 0 ]; then
      mkdir -p "$ev" 2> /dev/null && created=1
    fi
    if [ "$created" -ne 0 ]; then
      printf 'evidence truncated at %s-byte cap (%s bytes copied)\n' "$cap" "$copied" \
        > "$ev/TRUNCATED.txt"
    fi
    echo "cj1-nightly: $comp evidence truncated at ${cap}-byte cap ($copied bytes copied)" >> "$log"
  fi
}

selftest() {
  local dir self=$0 fails=0
  dir=$(mktemp -d "${TMPDIR:-/tmp}/cj1-nightly-selftest.XXXXXX") || return 2
  mkdir -p "$dir/bin" "$dir/scratch" "$dir/retain/node0"
  echo '{}' > "$dir/scratch/setup.json"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$dir/bin/python"
  chmod +x "$dir/bin/python"

  # A stub test binary that identifies the component from its --exact test name.
  cat > "$dir/bin/stub" <<'EOF'
#!/usr/bin/env bash
comp=unknown
for arg in "$@"; do
  case $arg in
    *dashd_clients_see_the_rdashd_server_answer_as_dashd) comp=python_leg ;;
    *dashd_wallets_mix_a_round_through_the_rdashd_server) comp=wallet_leg ;;
  esac
done
if [ "${STUB_SLEEP:-}" = "$comp" ]; then
  exec sleep 30
fi
if [ "${STUB_ORPHAN:-}" = "$comp" ]; then
  sleep 300 &
  echo $! > "$STUB_ORPHAN_PID"
  echo "test result: FAILED. 0 passed; 1 failed"
  exit 101
fi
if [ "${STUB_FAIL:-}" = "$comp" ]; then
  if [ -n "${CJ1_SCRATCH:-}" ]; then
    printf 'sample-target-run-evidence\n' > "$CJ1_SCRATCH/${comp}-target-run.txt"
  fi
  echo "test result: FAILED. 0 passed; 1 failed"
  exit 101
fi
if [ "${STUB_SKIP:-}" = "$comp" ]; then
  echo "test $comp ... skipped: CJ1_PYTHON is not set"
  echo "test result: ok. 1 passed; 0 failed"
  exit 0
fi
if [ "${STUB_NOTEST:-}" = "$comp" ]; then
  echo "test result: ok. 0 passed; 0 failed"
  exit 0
fi
if [ "${STUB_NOSUMMARY:-}" = "$comp" ]; then
  echo "test result: ok. 1 passed; 0 failed"
  exit 0
fi
echo "summary {\"$comp\":\"ok\"} in 1.0s"
echo "test result: ok. 1 passed; 0 failed"
EOF
  chmod +x "$dir/bin/stub"

  # A stub timeout that records each component's wall-clock limit, then runs the command.
  cat > "$dir/bin/timeout" <<EOF
#!/usr/bin/env bash
[ "\${1:-}" = --foreground ] && shift
[ "\${1:-}" = -k ] && shift 2
echo "\$1" >> "$dir/limits"
shift
exec "\$@"
EOF
  chmod +x "$dir/bin/timeout"

  # A stub uv that always fails, to test setup failure without touching the network.
  cat > "$dir/bin/uv" <<'EOF'
#!/usr/bin/env bash
echo "stub uv failed: $*" >&2
exit 1
EOF
  chmod +x "$dir/bin/uv"

  local comps=() last_out="" rc=0 summary=""
  run() {
    local out=${reuse:-$dir/out.$RANDOM}
    last_out=$out
    : > "$dir/limits"
    env CJ1_NIGHTLY_BIN="$dir/bin/stub" \
      CJ1_PYTHON="$dir/bin/python" \
      CJ1_SCRATCH="$dir/scratch" \
      DASHD_PATH=/nonexistent/dashd \
      NIGHTLY_TIMEOUT="$dir/bin/timeout" \
      "$@" \
      "$self" "$out" "${comps[@]}" > "$dir/last.log" 2>&1
    rc=$?
    summary=$(cat "$out/summary.txt" 2> /dev/null)
  }

  fail() {
    echo "cj1-nightly: selftest FAILED: $1" >&2
    fails=$((fails + 1))
  }

  # 1. All pass + default limits check.
  run
  if ! { [ "$rc" -eq 0 ] \
      && grep -q "python_leg exit=0 status=ok" <<< "$summary" \
      && grep -q "wallet_leg exit=0 status=ok" <<< "$summary" \
      && grep -q "^DONE failed=0 unwritten=0$" <<< "$summary"; }; then
    fail "all components pass"
  fi
  if [ "$(tr "\n" " " < "$dir/limits")" != "1200 1500 " ]; then
    fail "default limits are 1200 s for python_leg and 1500 s for wallet_leg"
  fi

  # CJ1_NIGHTLY_SECONDS overrides both limits.
  run CJ1_NIGHTLY_SECONDS=45
  if ! { [ "$rc" -eq 0 ] && [ "$(tr "\n" " " < "$dir/limits")" = "45 45 " ]; }; then
    fail "CJ1_NIGHTLY_SECONDS overrides both limits"
  fi

  # 2. One component fails (other still runs and passes; evidence copied & capped).
  printf 'retained-debug-log\n' > "$dir/retain/node0/debug.log"
  run STUB_FAIL=python_leg DASHD_TEST_RETAIN_DIR="$dir/retain"
  if ! { [ "$rc" -ne 0 ] \
      && grep -q "python_leg exit=101 status=FAILED" <<< "$summary" \
      && grep -q "wallet_leg exit=0 status=ok" <<< "$summary" \
      && grep -q "^DONE failed=1 unwritten=0$" <<< "$summary"; }; then
    fail "one failing component fails the run while the other still runs"
  fi
  if ! { [ -f "$last_out/python_leg-evidence/python_leg-target-run.txt" ] \
      && [ -f "$last_out/python_leg-evidence/retained/node0/debug.log" ]; }; then
    fail "failure copies target-run.txt and DASHD_TEST_RETAIN_DIR into evidence"
  fi
  rm -f "$dir/scratch/python_leg-target-run.txt"

  # Evidence truncation cap.
  run STUB_FAIL=python_leg DASHD_TEST_RETAIN_DIR="$dir/retain" CJ1_NIGHTLY_EVIDENCE_CAP=5
  if ! { [ "$rc" -ne 0 ] && [ -f "$last_out/python_leg-evidence/TRUNCATED.txt" ]; }; then
    fail "evidence over cap writes TRUNCATED.txt"
  fi
  rm -f "$dir/scratch/python_leg-target-run.txt"

  # 3. One prints skipped: but reports ok (must FAIL as SKIPPED).
  run STUB_SKIP=wallet_leg
  if ! { [ "$rc" -ne 0 ] \
      && grep -q "python_leg exit=0 status=ok" <<< "$summary" \
      && grep -q "wallet_leg exit=0 status=SKIPPED" <<< "$summary"; }; then
    fail "a skipped test fails closed as SKIPPED"
  fi

  # 4. One runs no test (0 passed -> NO-TEST).
  run STUB_NOTEST=python_leg
  if ! { [ "$rc" -ne 0 ] \
      && grep -q "python_leg exit=0 status=NO-TEST" <<< "$summary" \
      && grep -q "wallet_leg exit=0 status=ok" <<< "$summary"; }; then
    fail "a component running 0 tests fails as NO-TEST"
  fi

  # Missing summary line fails closed as NO-SUMMARY.
  run STUB_NOSUMMARY=wallet_leg
  if ! { [ "$rc" -ne 0 ] && grep -q "wallet_leg exit=0 status=NO-SUMMARY" <<< "$summary"; }; then
    fail "a component without a summary line fails as NO-SUMMARY"
  fi

  # 5. One overruns its limit (real timeout kills it -> FAIL / TIMEOUT).
  run NIGHTLY_TIMEOUT=timeout CJ1_NIGHTLY_SECONDS=1 STUB_SLEEP=wallet_leg
  if ! { [ "$rc" -ne 0 ] \
      && grep -q "python_leg exit=0 status=ok" <<< "$summary" \
      && grep -q "wallet_leg exit=124 status=TIMEOUT" <<< "$summary"; }; then
    fail "a component overrunning its limit is killed and fails"
  fi

  # A failing component's leftover processes are reaped, not only on a timeout.
  run STUB_ORPHAN=python_leg STUB_ORPHAN_PID="$dir/orphan.pid"
  orphan=$(cat "$dir/orphan.pid" 2> /dev/null)
  if [ -z "$orphan" ]; then
    fail "the orphan stub recorded its child"
  elif kill -0 "$orphan" 2> /dev/null; then
    kill -KILL "$orphan" 2> /dev/null
    fail "a failed component's process group is reaped"
  fi
  if ! { [ "$rc" -ne 0 ] && grep -q "python_leg exit=101 status=FAILED" <<< "$summary"; }; then
    fail "the orphan stub's component fails"
  fi

  # 6. Unknown component name fails early, replacing a reused OUT_DIR's old summary.
  local stale=$dir/out.stale
  mkdir -p "$stale"
  echo "DONE failed=0 unwritten=0" > "$stale/summary.txt"
  comps=(bogus_leg)
  reuse=$stale run
  if ! { [ "$rc" -ne 0 ] && grep -q "unknown component: bogus_leg" "$dir/last.log" \
      && grep -q "^- - - bogus_leg status=UNKNOWN-COMPONENT$" <<< "$summary" \
      && ! grep -q "failed=0" <<< "$summary"; }; then
    fail "an unknown component name fails and replaces the old summary"
  fi
  comps=()

  # Unset DASHD_PATH fails early with clear message, replacing a reused OUT_DIR's old summary.
  echo "DONE failed=0 unwritten=0" > "$stale/summary.txt"
  env -u DASHD_PATH CJ1_NIGHTLY_BIN="$dir/bin/stub" CJ1_PYTHON="$dir/bin/python" \
    CJ1_SCRATCH="$dir/scratch" "$self" "$stale" > "$dir/last.log" 2>&1
  rc=$?
  summary=$(cat "$stale/summary.txt" 2> /dev/null)
  if ! { [ "$rc" -ne 0 ] && grep -q "DASHD_PATH" "$dir/last.log" \
      && grep -q "^- - - - status=NO-DASHD$" <<< "$summary" \
      && ! grep -q "failed=0" <<< "$summary"; }; then
    fail "unset DASHD_PATH fails early and replaces the old summary"
  fi

  # 7. Setup failure fails both components (row SETUP-FAILED).
  local out_setup=$dir/out.setupfail
  env -u CJ1_PYTHON -u CJ1_SCRATCH PATH="$dir/bin:$PATH" \
    CJ1_NIGHTLY_BIN="$dir/bin/stub" DASHD_PATH=/nonexistent/dashd \
    "$self" "$out_setup" > "$dir/last.log" 2>&1
  rc=$?
  summary=$(cat "$out_setup/summary.txt" 2> /dev/null)
  if ! { [ "$rc" -ne 0 ] \
      && [ -s "$out_setup/setup.log" ] \
      && grep -q "python_leg status=SETUP-FAILED" <<< "$summary" \
      && grep -q "wallet_leg status=SETUP-FAILED" <<< "$summary" \
      && grep -q "^DONE failed=2 unwritten=0$" <<< "$summary"; }; then
    fail "a setup failure fails both components with SETUP-FAILED"
  fi

  if [ "$fails" -eq 0 ]; then
    echo "cj1-nightly: selftest passed"
    rm -rf "$dir"
  else
    echo "cj1-nightly: selftest outputs kept in $dir" >&2
  fi
  [ "$fails" -eq 0 ]
}

if [ "${1:-}" = --selftest ]; then
  selftest
  exit
fi
if [ $# -lt 1 ]; then
  sed -n '2,31p' "$0" >&2
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
    echo "cj1-nightly: cannot write $SUMMARY: $*" >&2
    unwritten=$((unwritten + 1))
  fi
}
if ! : > "$SUMMARY"; then
  echo "cj1-nightly: cannot create $SUMMARY" >&2
  unwritten=$((unwritten + 1))
fi

early=0
for c in "$@"; do
  case $c in
    python_leg | wallet_leg) ;;
    *)
      echo "cj1-nightly: unknown component: $c (expected: ${COMPONENTS[*]})" >&2
      report "- - - $c status=UNKNOWN-COMPONENT"
      early=$((early + 1))
      ;;
  esac
done
if [ "$early" -eq 0 ] && [ -z "${DASHD_PATH:-}" ]; then
  echo "cj1-nightly: DASHD_PATH is unset; run 'eval \$(python3 contrib/setup-dashd.py)' first" >&2
  report "- - - - status=NO-DASHD"
  early=1
fi
if [ "$early" -ne 0 ]; then
  report "DONE failed=$early unwritten=$unwritten"
  exit 2
fi

# Ends whatever a component left in its process group: TERM, up to 10 s, then KILL.
reap_group() {
  kill -TERM -- "-$1" 2> /dev/null || return 0
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    kill -0 -- "-$1" 2> /dev/null || return 0
    sleep 1
  done
  kill -KILL -- "-$1" 2> /dev/null
}

# The timeout command (the selftest's stub records each limit).
TIMEOUT=${NIGHTLY_TIMEOUT:-timeout}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)

# A new session per component (util-linux setsid, or Python's where it is missing): its process
# group holds the test binary, capture.py and every dashd it starts.
if command -v setsid > /dev/null; then
  SESSION=(setsid)
else
  SESSION=(python3 -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])')
fi

BIN=""
if [ -n "${CJ1_NIGHTLY_BIN:-}" ]; then
  # Prebuilt binaries have no head: a reused OUT_DIR's head.txt must not pin them.
  rm -f "$OUT/head.txt"
  BIN=$(realpath -ms "$CJ1_NIGHTLY_BIN")
else
  if ! git -C "$ROOT" rev-parse HEAD > "$OUT/head.txt"; then
    echo "cj1-nightly: cannot write $OUT/head.txt" >&2
    unwritten=$((unwritten + 1))
  fi
  if ! (cd "$ROOT" && cargo test --locked -p dashd-node --features dashd-node/test-support \
      --lib --no-run --message-format=json 2> "$OUT/build.log" > "$OUT/build.json"); then
    report "BUILD FAILED (build.log)"
    exit 1
  fi
  BIN=$(jq -r 'select(.reason == "compiler-artifact" and .profile.test and .executable
                      and (.target.kind | index("lib"))) | .executable' "$OUT/build.json" | tail -1)
fi

setup_failed=0
if [ -n "${CJ1_PYTHON:-}" ] && [ -n "${CJ1_SCRATCH:-}" ] && [ -f "$CJ1_SCRATCH/setup.json" ]; then
  CJ1_PYTHON=$(realpath -ms "$CJ1_PYTHON")
  CJ1_SCRATCH=$(realpath -m "$CJ1_SCRATCH")
  export CJ1_PYTHON CJ1_SCRATCH
else
  scratch=$OUT/cj1
  venv=$scratch/venv
  python=$venv/bin/python
  setup_log=$OUT/setup.log
  if ! : > "$setup_log"; then
    echo "cj1-nightly: cannot create $setup_log" >&2
    unwritten=$((unwritten + 1))
    setup_failed=1
  elif ! (mkdir -p "$scratch" && chmod 700 "$scratch") >> "$setup_log" 2>&1; then
    setup_failed=1
  else
    rm -rf "$venv" "$scratch/setup.json"
    if ! {
      uv venv "$venv" &&
      uv pip install --python "$python" -r "$ROOT/contrib/node/cj1-transcripts/requirements.txt" &&
      CJ1_SCRATCH="$scratch" "$python" "$ROOT/contrib/node/cj1-transcripts/capture.py" setup
    } >> "$setup_log" 2>&1; then
      setup_failed=1
    elif [ ! -f "$scratch/setup.json" ] || [ ! -x "$python" ]; then
      echo "cj1-nightly: setup completed without $scratch/setup.json or $python" >> "$setup_log"
      setup_failed=1
    else
      CJ1_PYTHON=$python
      CJ1_SCRATCH=$scratch
      export CJ1_PYTHON CJ1_SCRATCH
    fi
  fi
fi

failed=0
if [ "$setup_failed" -ne 0 ]; then
  for c in "$@"; do
    report "- - - $c status=SETUP-FAILED"
    failed=$((failed + 1))
  done
  report "DONE failed=$failed unwritten=$unwritten"
  exit 1
fi

for c in "$@"; do
  log=$OUT/$c.log
  if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
    report "- - - $c status=NO-BINARY"
    failed=$((failed + 1))
    continue
  fi
  # A reused OUT_DIR's old times and evidence must not stand for this run's.
  rm -f "$OUT/$c.time"
  rm -rf "$OUT/$c-evidence"
  limit=${CJ1_NIGHTLY_SECONDS:-$(default_limit "$c")}
  mapfile -t args < <(test_args "$c")
  # In the background so its pid, which leads the new session, is known; --foreground keeps
  # timeout, the test binary, capture.py and its dashd children in that process group.
  (cd "$ROOT/dashd-node" && exec "${SESSION[@]}" /usr/bin/time -f "%U %S %e" -o "$OUT/$c.time" \
    "$TIMEOUT" --foreground -k 60 "$limit" nice -n 10 \
    "$BIN" "${args[@]}" > "$log" 2>&1) &
  pid=$!
  wait "$pid"
  rc=$?
  reap_group "$pid"
  status=ok
  times=$(tail -1 "$OUT/$c.time" 2> /dev/null)
  if [ "$rc" -eq 124 ] || [ "$rc" -eq 137 ]; then
    status=TIMEOUT
  elif [ "$rc" -ne 0 ]; then
    status=FAILED
  elif grep -Eq '(^|[[:space:]])skipped:' "$log" 2> /dev/null; then
    status=SKIPPED
  elif ! grep -Eq '^test result: ok\. 1 passed' "$log" 2> /dev/null; then
    status=NO-TEST
  elif ! grep -Eq '^summary ' "$log" 2> /dev/null; then
    status=NO-SUMMARY
  elif [ -z "$times" ]; then
    status=NO-TIME
  fi
  if [ "$status" != ok ]; then
    failed=$((failed + 1))
    collect_evidence "$c"
  fi
  report "${times:-- - -} $c exit=$rc status=$status"
done
report "DONE failed=$failed unwritten=$unwritten"
[ "$failed" -eq 0 ] && [ "$unwritten" -eq 0 ]
