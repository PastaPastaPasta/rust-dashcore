# node-fuzz

libFuzzer targets of the node program (O3): the L1 differential comparisons (A1 §4.4; CS0 §9.3),
each feeding its input to one `node-l1` comparison against Dash Core's in-process oracle on one
pin, and the model targets (DEC-59 Q1), which check node code against a reference model. `targets.toml` lists every target.

**Run targets only through `run.sh`.** A target started any other way refuses to run before it
executes an input. The runner keeps everything libFuzzer writes (working corpus, crash files,
logs, temporary files) in the private tree `$DASH_EMBARGO_DIR/fuzz/<target>/`, prints one summary
line, and exits 0 only when the run passed: no divergence, crash or sink failure, and the CPU
budget reached. A divergence is recorded in the run's `divergences/` and the run continues.
libFuzzer's slow-unit artifacts (`crashes/slow-unit-*`) are counted as `slow_units` but neither
fail the run nor stop it (BL-83: at load 150–250 every input is slow); timeouts (`timeout-*`)
and crashes still fail it. The verdict counts executions and sink failures in one log per
fuzzing process: each segment's `fuzz-<i>.log` under `--jobs`, else its `fuzz.out` (the `-jobs`
parent's `fuzz.out` repeats each worker's log, which used to count everything twice: BL-86).

```sh
node-fuzz/run.sh diff_tx_decode --pin v24.0 --cpu-hours 24 --jobs 8 --public-seeds <dir>
node-fuzz/run.sh diff_tx_decode --pin v24.0 --merge
node-fuzz/run.sh --selftest
```

- Targets: `diff_tx_decode`, `diff_block_decode`, `diff_header_decode` (CS0, L1);
  `diff_codec_primitives`, `diff_coin_decode`, `diff_undo_decode` (CS5a, L1: the coins database
  and undo codecs; `diff_codec_primitives`' first byte picks one of `oracle_coin_codec`'s twelve
  operations); `kv_model` (ST1's key-value model) and `coreimport_external_scan`
  (dash-coreimport's block-file scanner, private-route), model targets; and IM1a's six readers
  of Core's net-state files (`dash_coreimport::netstate`, model targets): `coreimport_peers_dat`,
  `coreimport_anchors_dat`, `coreimport_banlist_json`, `coreimport_fee_estimates_dat`,
  `coreimport_settings_json` (public-route) and `coreimport_mempool_dat` (private-route: it decodes
  transactions). Their seeds, in `node-fuzz/seeds/<target>/`, are the files a real Core
  v24.0.0-rc.3 regtest node wrote (`dash-coreimport/tests/data/netstate`). IM1b adds seven
  model targets for the rest of the import surface: the `CFlatDB` files `coreimport_governance_dat`
  (private-route: it embeds a masternode list), `coreimport_mncache_dat`,
  `coreimport_netfulfilled_dat` and `coreimport_sporks_dat` (each check also runs the input with
  its checksum repaired, which a mutation otherwise never keeps), and `coreimport_dash_conf`,
  `coreimport_onion_key` and `coreimport_i2p_key` (public-route; the key readers also check that
  their `Debug` output holds none of the key). Their seeds are
  the files Core wrote and the BS2 oracles hold (`dash-coreimport/tests/data/flatfiles`, with its
  `PROVENANCE.md`; the keys are throwaway). A finding the farm has
  not made yet but a review did is a unit test of the model instead: `cargo test --lib
  coreimport_netstate` (run in `node-fuzz/`: it is a workspace of its own) replays `nan-decay/fee_estimates.dat` (a NaN decay that Core loads and writes
  back, R2-M1) and each decay word of each horizon through the model.
  `connman_sched` (P2a-net, model, public-route) scripts `dashd-net`'s dial `sim` and checks
  `ThreadOpenConnections`' slot, netgroup and address rules after every step.
  `disconnect_pool` (CH3b b1, model, private-route) runs `dashd_chainstate::reorg`'s pool against
  a plain-vector model of Core's list and map over synthetic transactions; Core's own figures are
  b1's vectors (`test-vectors/disconnect-pool/`).
  `diff_p2p_<cmd>` are L1 targets for BS8's 14 overlay messages (O2c, v24.0 only:
  `node_l1::overlay`, whose module docs give the input layout; seeds from `cargo run -p node-l1
  --features oracle --example overlay_seeds`). A target compares only on the pins its
  `node_l1::TARGETS` entry lists. A later task adds an L1 target as a comparison in `node-l1`
  registered in `node_l1::TARGETS`, or a model target as a check registered in
  `node_fuzz::models::MODELS`; then a target file here (`l1_target!` or `model_target!`), a
  `[[bin]]` and a `targets.toml` entry. `fuzzctl check` fails until all of them agree, and `run.sh`
  refuses a target or pin `targets.toml` does not list.
- The honggfuzz targets in the workspace `fuzz/` crate's `dash/` and `hashes/` (upstream's,
  run by upstream's `fuzz.yml`) are deliberately outside this registry and the runner. A node
  crate's honggfuzz target there (`dash_coreimport/external_scan.rs`) is registered through the
  `honggfuzz` key of its node-fuzz port, which is what the farm runs; `fuzzctl check` fails on
  one that no entry names. Each entry also names its `owner` crate.
- `targets.toml` also gives each target its route (below), its per-PR smoke budget, its weight in
  the nightly farm and its public seed directories.
- The budget is per target per pin; each leg is run and recorded separately. After a fix, a leg
  starts a fresh budget (the working corpus is kept).
- `DASH_EMBARGO_DIR` must be set; the script never sets it. The target runs in a clean
  environment (`env -i` with `PATH`, `HOME`, `TMPDIR`, `DASH_EMBARGO_DIR` and the runner's
  variables), from the run's `logs/` directory, with its output redirected; the target checks
  all of that, and every libFuzzer argument, before it runs an input.
- A run stops after 100 recorded divergences; it fails either way.
- `--runs N` is for the self-test target only; real legs have a CPU budget.
- Minimisations and replays are **jobs**: `run.sh <target> --pin P --job <name>` runs the job
  file `fuzz/<target>/jobs/<name>` (`job-` and 32 hex digits, 0600), which holds `mode <m>` and
  `input <path>` lines and which the runner consumes. A finding's path (its name may be its hash)
  and a divergence's field must not reach a command line or an environment, which `ps` and
  /proc show (PLAN §7.5): the runner reads the job with shell builtins, copies inputs into the
  run as `input/input-<n>` by redirection, and gives a minimisation's field to the target in the
  run's `abort-field` file. The modes:
  - `minimize`: a crash file of an earlier run of the target; passes only when libFuzzer exits 0
    having written the run's `crashes/minimized`. Divergences are ignored, so a mutation that
    diverges does not fail it.
  - `minimize-divergence`: a recorded divergence of an earlier run, with its `<path>.txt`
    sidecar, minimised into `crashes/minimized` keeping the field the sidecar names (and its
    `cmd`, when it has one).
  - `replay`: a file or directory of findings or regression vectors, run once; passes when none
    diverges or crashes, and fails when there is nothing to run. With an `order fixed` line, a
    replay takes several `input` lines and runs them in the job's order: the k-th is staged into
    a directory of its own, `input/<k>/`, and libFuzzer gets those directories in order with
    `-shuffle=0 -prefer_small=0` (by default it shuffles its seed inputs, then sorts them by
    size). Within a directory libFuzzer reads in readdir order, so an exact order of files takes
    one file per `input`. libFuzzer runs one empty input before them, in any order. A job's
    other modes take exactly one `input`, and an ordered input with nothing to run fails the
    job.

  An input outside the target's runs and regression vectors is refused before a run is created.
  What `ps` still shows of a job: `run.sh <target> --pin <pin> --job <random name>`, and the
  target binary with libFuzzer's minimisation or replay flags and run-directory paths, so an
  observer can tell that some job runs on that target and pin, but not which finding or field.
  (While an input is staged, `/proc/<pid>/fd` of the `cat` copying it names the file for a
  moment, as for any process that reads it.) Each run writes its `mode` first, so triage never
  takes a job killed before its verdict for a fuzzing run.
- `--selftest` checks the runner in an empty directory of its own under `/work/scratch`: a
  planted divergence, a divergence the sink cannot record, a run directory it cannot create, a
  merge, a minimisation, a missing crash file, bad job names, an input that does not crash, a
  missing `DASH_EMBARGO_DIR`, a direct start, a divergence minimisation (and one for a field the
  input does not diverge in), replays of a divergent and an agreeing input and of nothing, a
  fixed-order replay (its staging, libFuzzer's reading order, its flags when `strace` is
  installed, and malformed ordered jobs), and an unregistered target and pin. During the
  minimisations and replays it watches every process it starts: none may show the finding's name
  or the divergence's field in its command line or environment; with `strace` installed it also
  traces every `execve` of a minimisation and checks the full argument lists.
- `run.sh` never creates `seeds/`: a seed directory the private seed manifest does not list fails
  the gate's private-seed replay (CS0 §9.4), so it passes `seeds/` only when it exists.
- `run.sh` uses GNU `stat`, `time` and `sha256sum` (agentbox).
- This is a workspace of its own, so `cargo test --workspace` never links the oracle archives. Its
  base-sdk `[patch]` table repeats the root's; `run.sh` refuses to build if they differ.
- `kv_model` keeps its per-input RocksDB in `/dev/shm` (its inputs are public): on the private
  tree's disk, RocksDB's syncs slow it several-fold.
- Instrumentation is cargo-fuzz's coverage set without a sanitizer (the Rust under test forbids
  `unsafe`), on the stable toolchain. The oracle is O1's coverage-instrumented `fuzz` flavour by
  default, so libFuzzer also sees Core's side of each comparison (about twice the starting
  coverage of the `plain` flavour); `DASH_ORACLE_FLAVOR=plain` overrides it. The verdict records
  the flavour and that archive's sha256 from `pins.txt`.
- A fuzz recipe change that moves the archive's sha256 renames its cache directory (BL-371:
  `<commit>/fuzz-nocmp`, relic and dashbls without trace-cmp; see `dash-oracle-sys/README.md`),
  so trees on the old archive keep building. It also moves the coverage a corpus reads (the
  BL-371 archive reads +5 cov on BLS legs), so no plateau or certify series spans the change
  (DEC-429): a leg's first certify after its tree takes the new archive starts a new cov
  baseline. Earlier certificates stay valid, being keyed on the rust-dashcore commit; there is
  no mass re-certify.

## The farm (`fuzzctl.py`, O3)

`fuzzctl.py` (Python 3.11+, stdlib) drives `run.sh`; it never runs a target any other way. Like
`run.sh` it needs `DASH_EMBARGO_DIR` and never sets it.

```sh
python3 -I node-fuzz/fuzzctl.py check        # registry vs Cargo.toml, target files, TARGETS, MODELS
python3 -I node-fuzz/fuzzctl.py selftest     # the tool's own tests (synthetic trees only)
python3 -I node-fuzz/fuzzctl.py smoke        # per-PR smoke
python3 -I node-fuzz/fuzzctl.py farm launch  # a night's farm: 8 h, CPUQuota=800%
python3 -I node-fuzz/fuzzctl.py farm launch --night n --only diff_p2p_qrinfo  # just these targets
python3 -I node-fuzz/fuzzctl.py farm status
python3 -I node-fuzz/fuzzctl.py farm stop
python3 -I node-fuzz/fuzzctl.py certify diff_p2p_qrinfo@v24.0 --launch  # certify a plateau
python3 -I node-fuzz/fuzzctl.py triage [--recheck]
python3 -I node-fuzz/fuzzctl.py status-line
python3 -I node-fuzz/fuzzctl.py export-public <dir>
```

- **Selftest.** The last line is the verdict, and it names every failed case. A failure of a
  case in `LOAD_FLAKY` (timed by the wall clock, so it can fail under load) reruns that case alone,
  up to 3 tries, and a pass that needed one says `ok-after-rerun`, never plain `ok`; any other
  failure, or one that fails every try (a skip alone is no pass), fails the selftest (BL-134).
- **Smoke.** `run.sh --selftest`, then every leg (target × pin) with a smoke budget, each until it
  has spent at least its `smoke` CPU seconds (`targets.toml`: a floor of 360 CPU-s today; a
  leg's segments overshoot it, and builds, the self-test and any triage come on top: about 13
  minutes wall at load 90; the summary reports CPU and wall seconds), 4 legs at a time with one
  libFuzzer process each. Every fuzzing run also replays the private seeds and the fixed
  regression vectors. A smoke leg loads those in full but only a sample of the working corpus
  and the public seeds (BL-333; `run.sh --corpus-sample`): libFuzzer reads its stop file only
  once its corpus is loaded, and farmed corpora of thousands of inputs at a few executions a
  second outlived the budget until the kill. The sample is at most `smoke_corpus_max` files and
  `smoke_corpus_max_bytes` bytes (`targets.toml`; 64 files and 4 MiB by default), taken in the
  order of their names' sha256 and reflinked into the run's `sample/`, which takes libFuzzer's
  new inputs and is removed at the end: the smoke never writes, merges or prunes the working
  corpus. The public seeds are sampled with it, since diff_dml_block's 1,385 alone take five
  times its budget to load; node-l1's tests (`l1_dml`, `l1_overlay`, …; `public-seeds/
  PROVENANCE.md`) run each of `public-seeds/` against the oracle. The farm and `certify` load all
  of it, as before. A failed run's verdict and line
  give its `reason`, `budget spent before INITED (corpus load)` among them, and the smoke's
  aggregate line counts the failed legs by reason. It prints `run.sh`'s line for each
  public-route leg and one aggregate line (private-route legs' lines go to
  `fuzz/.smoke/<time>.log`), triages any finding, and exits
  non-zero on any divergence, crash, sink failure, unmet budget, triage error or self-test
  failure. `--only` must name targets with smoke legs. Triage jobs have deadlines (15 minutes
  for a minimisation, 5 for a replay); one that runs out keeps the finding unminimised. The
  farm's triage also stops when a bench window is about to start, node-bench runs or the unit
  stops: between jobs, and by interrupting a running job (checked every second).
- **Nightly farm.** `farm launch` starts the worker as a transient `systemd-run --user` unit
  (`node-fuzz-farm-<night>`, `CPUQuota=<cores × 100>%`, and the lowest priority, DEC-184 (4):
  `Nice=19`, `IOSchedulingClass=idle`, `CPUWeight=10`; `KillMode=mixed`). The night's budget,
  `--hours × --cores` CPU-hours (8 × 8 by default; `--cores 16` doubles it), is a cap shared
  out over the launch's legs by their `nightly` weights. The worker runs slices of `--jobs`
  libFuzzer workers (`--slice-minutes`, default 30), `cores / jobs` at a time, each slice going
  to the leg furthest below its share. `--only TARGET…` (repeatable; passed through `launch`)
  farms only those targets' legs, which split the whole cap among themselves. Nothing is ever
  redistributed: a leg that stops early (a suspected plateau, a finding, a broken build) leaves
  its share unspent. No slice is given more than what is left of the cap after all the CPU time
  of the night's slices (credited or not) and the slices running, so no sequence of launches for
  a night spends more than the cap (the worker then ends `cap`); a leg that restarts its share
  after a finding gets what the cap still allows. Progress is `fuzz/.farm/<night>/slices.jsonl`: a `start` row (fsynced) before
  each slice's `run.sh` starts, with a random slice id that `run.sh --run-tag` puts at the end of
  the run id, and a `done` row with its outcome before the slice counts as finished. A launch
  first accounts, once, for every slice started but never recorded done (a killed worker): from
  its run's verdict; else from what the run left (its finished segments' CPU time, its recorded
  divergences and crashes, and its logs read by run.sh's rules: sink failures, crashes libFuzzer
  reported but never saved, refusals), as a finding, a failure or, with none of those, an
  interruption; else as lost. A slice with a finding (a sink failure or crash included) earns no
  credit, recovered or not. The plan (`plan.json`) and the ledger fail closed: a plan that is
  unreadable, empty, for another night, made for a different registry, or with bad shares, and
  any malformed ledger row, make the worker (and `farm status`) refuse with a non-zero exit
  instead of scheduling or reporting from it; only a torn final row (a kill during an append) is
  dropped, and the summary reports it as `ledger_repaired`. `farm launch --night <id>` for
  a night with budget left continues it (the night's budget is fixed by its first launch; each
  launch runs for `--hours`), and `farm stop` (SIGTERM; the worker stops its slices through
  their stop files and SIGINTs triage's run.sh processes, so libFuzzer prints its stats and
  `run.sh` writes the verdict) keeps everything recorded. A slice's `run.sh` starts no segment
  once `<run>/stop` exists, and ends each segment on its own CPU time (BL-228;
  `docs/node-program/designs/BL-228-notes.md`): `window_driver.py --farm` measures the segment's process tree (the
  `-jobs` workers included) from /proc and, at the CPU still missing from `--cpu-hours` plus one
  second, writes the segment's `-stop_file` (it also copies a non-empty `<run>/stop` there);
  `-max_total_time` is a one-day backstop, so a starved leg loads its corpus once, not once per
  wall-clock segment. Every interrupt (a
  suspected plateau, a bench window, the deadline, a stop) writes it, and falls back to SIGINT
  after 90 s and SIGKILL 30 s later, well inside libFuzzer's 1200 s `-timeout`: a worker that
  hangs in libFuzzer's interrupt path would otherwise end in an ALARM and an empty `timeout-*`
  artifact (BL-173; `designs/BL-173-notes.md`). libFuzzer reads the file only once fuzzing, so a
  slice that is still building or loading its corpus gets the SIGINT at once. No log reading ever
  drops, discounts or closes a timeout (BL-173 D18): an empty `timeout-*` that every worker log
  shows written after its own `run interrupted` is a finding like any other, counted in the
  slice's verdict, with a `suspected_interrupt` label that triage keeps on its cluster (`interrupts`
  counts the labelled hits, `interrupt_evidence` the run, segment, worker and interrupt line of the
  first 20). Such a finding costs a leg its credit for the slice, at most one night. A human who
  has looked closes the cluster: `fuzzctl close-phantom CLUSTER --reason TEXT --by NAME` closes one
  open cluster every hit of which carries the label (not one that is unknown, not open, or has an
  unlabelled hit), records who, when and why in `fuzz/<target>/triage-audit.jsonl`, and moves its
  vector to `regress/fixed/`. Only that command closes one; no farm, nightly or triage path runs
  it. Without `--night`, `farm stop` stops every farm unit and timer.
  `--on-calendar '*-*-* 01:07'` (the nightly schedule) installs a timer instead; its runs take
  the date as the night.
  Only one farm runs at a time (`launch` refuses while a farm unit is active; the worker locks
  its night before it makes the plan or repairs the ledger, and a second worker for the night
  refuses). The journal (`journalctl --user -u node-fuzz-farm-<night>`) carries `run.sh`'s
  line for public-route slices only, and for triage only the aggregate STATUS line.
- **Plateau suspicion (DEC-184 (1), DEC-186 (1)).** A stop heuristic that saves CPU; it never
  certifies anything (`certify` below does). A leg (target@pin) is *suspected* of a plateau when,
  over its last `--plateau-minutes` (default 60, at least 15; passed through `launch`) of its own
  fuzzing time, its libFuzzer processes gained no `cov` and at most 0.1 % of the `ft` the leg
  had at the window's start, and at least one full window has run (a leg that reports no
  coverage is never suspected). A target whose pure replays drift (DEC-260; diff_chain) sets
  its own rule in `targets.toml`: `plateau_cov_slack` (cov a window may gain and still be flat,
  default 0, at most 64) and `plateau_ft = false` (ft left out). The slack is the leg's, summed
  over its processes like the rest of the history, so a leg run with `--jobs` J allows each
  process about 1/J of it. The same rule applies to `farm status` and to the window of
  `certify` below; `certify`'s comparisons between processes have a rule of their own.
  On every poll (`--poll`, 30 s) and at each slice's end the
  worker reads the running slices' worker logs (`fuzz-<i>.log`, or `fuzz.out` with one process).
  Each process is measured against itself from its own `INITED` line (its corpus loaded), and
  the leg's growth is the sum of its processes' growths, so a process that lags another or
  restarts on a smaller corpus still shows its growth. An interval between two polls is fuzzing
  time only when both ends saw the same segment with every process past `INITED`: an interval
  that spans a corpus load is not. Durations are monotonic-clock seconds. The worker appends the
  leg's cumulative history (`fuzz_s`, `cov_gain`, `ft_gain`, `ft`; a leg's history starts at 0
  seconds) to `fuzz/.farm/<night>/coverage.jsonl` (fsynced; a torn final row is dropped, any
  other bad row, or a history without its start or going backwards, fails closed); it carries
  over slices however legs rotate and over worker restarts. A finding voids the leg's history as
  it voids its credit. A suspected leg is stopped for the night: a `suspected` row is appended,
  its running slice is interrupted at the next poll and recorded as a plain `interrupted` slice,
  and a later launch for the night finds it suspected from the history. `farm status` reports
  `legs_suspected` and each positively public leg as `<leg>=plateau-suspected@ft<ft>(<W>m)`, for
  as long as the leg's history still shows the suspicion over its window (a later launch with a
  longer window may run the leg again); `status-line` adds one `SW-FUZZ farm` line for the latest night, naming only legs whose target
  the current registry lists as public and counting every other leg (private-route, removed or
  renamed). A target that plateaus at a low ceiling is reworked (seeds, dictionaries, mutators,
  new targets: BL-87), not run longer.
- **Certification (DEC-186 (2), amendments 2 and 3).** `certify <target>@<pin>` is the only path that
  certifies a plateau. It runs `run.sh --certify --seconds S` (default 3600) in four segments.
  - Segment-1 merges the target's corpora into its working corpus (`-merge=1`, which keeps every
    input that adds a feature, so the union of the features is kept).
  - Segment-2 and segment-4 are replay passes: the same binary once over the working corpus with
    `-runs=0`, before and after the window. libFuzzer loads the corpus, prints `INITED` with its
    cov and ft, and stops; those are the corpus's coverage, the baseline and the result (Sol r2
    B1).
  - Segment-3 is the window: one libFuzzer process on that corpus until it has used S seconds of
    its own CPU time (user + system of the process, not of children it reaped; read from the
    kernel about every 10 s) after its `INITED`. Then the driver writes the window's stop file
    (`-stop_file`, DEC-186 amendment 3 (a)). libFuzzer reads it at the top of its fuzzing loop,
    between two mutation rounds, so a discovery in flight is printed, saved and counted before it
    stops (Sol r3 B1); it then prints its `DONE` line (cov and ft, measured in the process),
    `Done N runs`, its final stats, and exits 0. No signal interrupts it. SIGUSR1's graceful exit
    (libFuzzer's `handle_usr1`, on by default, and honoured: neither node-sink nor node-fuzz
    installs a signal handler) stops at the same boundary but through `_Exit(0)` after `exiting as
    requested` and the final stats, with no `DONE` line, so it is not used. The window is CPU
    time, so the farm's low priority dilates it in wall time but never shortens it. A wall-clock
    cap (`--wall-cap-hours`, default 12, on a monotonic clock from the window's start) stops a
    window that cannot get its CPU time with SIGINT (libFuzzer exits 72), and SIGKILL follows any
    stop the process outlives by 1500 s (more than libFuzzer's 1200 s `-timeout` for one input),
    a stuck corpus load or a window that never reaches its loop's top included; a SIGINT or a
    kill never certifies. The 1500 s is a constant: the selftest may shorten it, and the 10 s
    between CPU checks, through `NODE_FUZZ_CERTIFY_GRACE` and `NODE_FUZZ_CERTIFY_POLL`, but any
    value that is not a finite number of seconds above 0 and at most the bound stops run.sh
    before it builds (Sol r4 B1). `-max_total_time` (libFuzzer's own clock, which follows the
    civil clock) is only a backstop. Its `window.json` records the run's target, pin and id,
    what stopped it (`cpu-window` or `wall-cap`), the CPU time at `INITED` and at the stop, the
    wall time, the exit, and the time from the stop to the exit (`stop_to_exit`, monotonic,
    unrounded). An exit more than 1500 s after the stop fails the run whatever its status
    (run.sh's verdict and `certify`'s status both check it), so an overdue exit 0 never
    certifies, even one the kill missed.

  One process, because the harness's start-up check refuses libFuzzer's `-fork` flags (all but
  `-stop_file`, which the window needs) and the temporary corpus directories of its children;
  `certify` reads the harness's allowed flags and
  refuses to run if they ever admit fork mode, until a fork engine is built and verified. The
  window's executions are its final stats' `number_of_executed_units` less its `INITED` line's
  count. run.sh records the source the binary was built from in the run's `source`: the commit,
  whether the tree was dirty or changed during the build, the run's id, and the sha256 of the
  binary. Every change the build could see counts, whatever the user's git configuration:
  tracked changes, files marked assume-unchanged or skip-worktree, and untracked files unless a
  committed `.gitignore` ignores them (Sol r2 M2); a directory of untracked files that are all
  ignored counts as dirty, which only ever fails safe. Builds are serialised by a lock on
  `node-fuzz/target/.node-fuzz-build.lock` and always go to `node-fuzz/target` (whatever
  `CARGO_TARGET_DIR` or cargo's configuration says), and a certify run copies the binary into
  the run under that lock and executes the copy in every segment (removing it at the end), so
  a later build cannot change what it measures. The copy needs a sink that allows executables
  (not `noexec`); a run killed with SIGKILL together with its `fuzzctl` leaves it behind.

  Each leg has a lock, `fuzz/<target>/run.lock`: fuzzing runs and merges share it, a certify run
  holds it alone for the whole run (nothing else writes the working corpus between its
  replays), and triage's jobs, which never write the corpus, take none. A run that cannot take
  it fails at once, so a farm slice of a leg being certified is a failed slice, and a certify
  run of a leg the farm is fuzzing fails before it builds.

  The status is derived from the run's evidence whenever it is read, never stored as a boolean;
  the first that applies:
  - `no-evidence`: no readable certify verdict (missing or torn).
  - `finding`: divergences, crashes or sink failures, the most of the verdict's counts and the
    run's files and logs.
  - `invalid`: evidence that contradicts itself or the run's place in the tree (Sol r2 M3). The
    verdict, `window.json` and `source` must name the run's own target, pin and id, at the
    verdict's commit. `window.json` must have exactly its fields, and agree with the verdict's
    window, wall cap and exit. A CPU-window stop must show its CPU time, a wall-cap stop the cap
    reached, and every stop (and only a stop) its `stop_to_exit`. `source` must have its four
    lines and agree with the verdict's dirty flag. A `certificate.json` that is unreadable, not
    `{"evidence", "cutoff", "eligibility"}` and an optional well-formed `"plateau"`, or whose
    evidence differs from the run's or whose cutoff is not a run stamp at or after the run's
    start is also invalid.
  - `failed`: not one process, a failed merge, a missing or failed replay before the window, a
    refusal, a verdict that is not a pass, or an exit other than the window's stop: a CPU window
    must end with exit 0, unkilled (its stop file), and a wall-cap stop with SIGINT's 72 or a kill.
    Either way libFuzzer must exit within 1500 s of the stop; a kill after the default grace is
    always later, so a killed window, even at the wall cap, fails.
  - `starved`: the wall cap came first. Never certified; rerun it when the box is quieter.
  - `failed`: a window log that breaks its grammar (DEC-186 amendment 3 (c)). After `INITED`
    (exactly one, whole) every line must end with a newline and be one of libFuzzer 0.4.13's
    forms: a status line (`#N\t` and a six-wide label: `NEW`, `REDUCE`, `pulse`, `RELOAD`, then
    `cov:`, `ft:`, `corp:`, optional `focus:` and `lim:`, `exec/s:`, `rss:`), whose `NEW` and
    `REDUCE` lines end with `L: a/b MS: n` and min(n, 10) mutator names and optional quoted
    dictionary words; a `NEW_FUNC` line; a slow unit's three lines; or the harness's strictness
    count. Executions, cov and ft never go backwards. Then the `DONE` line, an optional
    recommended dictionary, `Done N runs`, and the five final stats, each once and in order, and
    nothing after them; the `DONE` line, `Done` and `number_of_executed_units` count the same
    executions, and the `DONE` line's cov and ft are the highest printed since `INITED` (each
    discovery prints a line, so a higher `DONE` means a lost one). Torn, repeated, unknown or misplaced content fails. So does a log that lost
    lines (libFuzzer ignores its write errors, as on a full sink): its `NEW` and `REDUCE` lines
    after `INITED` must equal the units its final stats' `new_units_added` counts, as it prints
    each before it counts it and the window ends between two units (Sol r3 B2). Also `failed`:
    no replay after the window, or one lower than the one before it by more than the target's
    replay rule (under `growing`) allows: by default any cov, or ft by more than 0.1 %. Loads of
    one corpus vary by a few features (five replays of one corpus: ft 3927-3930; two smokes'
    windows loaded to ft 3736 after a replay of 3738, then to cov 1122 after 1123), so the
    window's own `INITED` is not tested against the replays; the leg's lock and the binary's
    copy make its corpus and binary the replays'.
  - `growing`: a line of the window's log after its `INITED` that shows a discovery; the `DONE`
    line's cov different from the window's `INITED`'s, or its ft above it by more than 0.1 % (the
    in-process check, amendment 3 (b)); or a replayed cov or ft gain over the target's replay
    rule (below; by default any cov, or ft over 0.1 %).
    A line shows a discovery when it is a `NEW` or `NEW_FUNC` line, or a status line whose cov or
    ft is above every earlier line's (`INITED`'s included) whatever its label: libFuzzer labels a
    discovery `REDUCE` when its parent input was ever reduced, and `RELOAD` when it re-reads the
    corpus. The first real smoke's window had 583 `NEW` and 355 `REDUCE` lines that raised cov
    or ft. Under a target's `plateau_cov_slack`, the `DONE` line's cov may differ from
    `INITED`'s by that much (the drift within the window's process); with `plateau_ft = false`,
    the ft comparisons and the discovery lines are left out, so DONE against INITED bounds the
    cov found in the window. The replay after the window still fails if it loses ft (beyond the
    replay rule below), which is reproducibility, not growth.
    The two replays run in processes of their own, and one corpus loads to different coverage
    in different processes. A target whose replays differ that way sets `replay_cov_slack`
    (default 0, at most 64) and `replay_ft_slack` (default 0, at most 1000) in `targets.toml`
    (DEC-271), each from a measurement: ceil(1.5 × the worst pairwise spread of one fixed corpus
    replayed in at least 6 processes) on the pinned archive, capped at 64 for cov, the ft key
    only when that spread is over 0.1 % of ft. Each process is a `replay` job (above) with one
    `input` line naming the corpus directory under the target's `regress/` and no `order fixed`
    line, so libFuzzer loads it in its own order, as certify's replays do; run it N times
    (`run.sh <target> --pin <pin> --job <name>`, one job file each) and read each run's `INITED`
    cov and ft in `logs/segment-1/fuzz.out`. Keep the runs and note the corpus and its size
    beside the keys, and measure again when the corpus has grown and on every oracle pin
    change, keeping the worst figure measured (DEC-277). The replay after the window may then
    be lower than the one before it by `replay_cov_slack` cov and by the larger of 0.1 % and
    `replay_ft_slack` of ft and still reproduce, and higher by as much without growing.
    The plateau slack never reaches these comparisons, nor these slacks the window's;
    `plateau_ft = false` still leaves the replayed ft gain out of the growth test (it is a
    growth test), so for such a target `replay_ft_slack` bounds only the fall. `certify` records
    both rules in the run's certificate.json, and the status reader judges the run by the
    stricter of each and the registry's. A certificate without one (from before DEC-260 or
    DEC-271) is judged by its default, which is no looser than what judged it then, so a
    registry change never certifies an earlier run.
  - `insufficient-progress`: fewer executions in the window than the target's floor
    (`min_certify_execs` in `targets.toml`, default 10⁶, for a target too slow to have explored
    much in its window).
  - `uncertified`: a window under 3600 s; no `source` record, a tree dirty or changed while the
    binary was built, or no known commit; or the plain oracle, without Core's coverage.
  - `certified`.

  `certify` writes the evidence (`baseline`, `final`, `new_lines`, `window_done`, `log_problems`,
  `cpu_seconds`, `executions`, `window.json` and `source` among it), its completion cutoff (the
  run stamp once run.sh exited), what `eligibility` found at that cutoff and the target's plateau
  and replay rules as the run's `certificate.json`. It exits 0 only when
  certified. One function, `eligibility`, decides whether a leg may be certified at a cutoff,
  failing closed (DEC-186 amendment 3 (d)): it refuses an unreadable triage state (a torn file
  or a malformed cluster), an open cluster seen on the pin (or on no pin), any cluster whose
  regression vector or provenance is missing or unreadable, and any run of the target, of any
  pin, whose triage failed or that is not triaged yet (pending, or never triageable), unless it
  started after the cutoff (a later farm slice, which cannot have touched the window). `certify`
  triages the leg's runs (without minimising) and asks it before the window (refusing to start)
  and after it at its cutoff (status `finding`, exit 1); `farm status` and `status-line` ask it
  at the cutoff the certificate records and add the reasons the certificate records, so they
  report what the command did and never upgrade a refusal once its pending runs triage clean
  (Sol r3 B3, M1). A certify run without a certificate (killed before it completed) reads at
  most `uncertified`.
  An open finding on a leg outranks its certificate whenever it was found. `--launch` runs it in a transient unit at the farm's priority with
  `CPUQuota=100%`: `node-fuzz-certify-<target>-<pin>` for a public leg; any other leg gets a
  random unit name and travels in the unit's environment, not its command line (unit
  descriptions are fixed, so no target reaches the journal that way). `farm status` and
  `status-line` report each leg's latest certify run (`<leg>=<status>@<rev>`), naming public legs
  only. At Nice 19 on a box at load 100–190 a window gets about a quarter of a core, so a
  3600 CPU-s window takes about four hours of wall time, plus the merge and two replays.
- **Bench windows.** The farm yields to benchmarks (DEC-23(b)). It starts no slice that would
  overlap a reserved window, shortens slices to end before one, interrupts running slices when
  one is about to start, and waits while any process carries node-bench's `NODE_BENCH_MARK`
  (a node-bench run is in progress). Reserved windows are rows `<start> <end> [id]` (ISO 8601,
  UTC unless an offset is given) in `~/.config/node-program/bench-windows` (or `--windows`,
  `NODE_FUZZ_BENCH_WINDOWS`); a missing file reserves nothing and a malformed one blocks the
  farm.
- **Triage.** After each farm slice and smoke leg with a finding, and at the start of each farm
  launch, every finished fuzzing run not triaged yet is clustered by target, kind and class: the
  field for a divergence; for a crash, the panic location and the first frames of the backtrace
  (never message text, which may carry input values), or libFuzzer's reason. A new cluster's
  input is minimised through a `run.sh` job (`minimize` for crashes; `minimize-divergence` for
  divergences, which makes that field's divergence abort so libFuzzer's crash minimisation keeps
  the class). A minimised input is replayed and kept only if it still shows the
  cluster's kind and class; otherwise (and after two failed attempts) the finding itself is the
  vector. Vectors are `fuzz/<target>/regress/open/<cluster>-<generation>`: each file belongs to
  one cluster, so clusters that share an input never share, delete or promote each other's
  file. Provenance is in `regress/meta/` (outside the directories libFuzzer reads). A
  promotion saves the cluster's state before it moves the file, and every pass first puts each
  vector where its cluster's state says (finishing an interrupted promotion); a vector found in
  neither place is a triage error. An empty `timeout-*` artifact that every worker log showing its write wrote after
  its own `run interrupted` is a finding with a `suspected_interrupt` label (BL-173 D18; a write
  before an interrupt withholds the label), counted as `suspected_interrupts`, clustered and counted
  in a slice's verdict like any timeout. `--recheck` (each farm launch)
  replays open vectors on the pins they were seen on and moves those that pass to
  `regress/fixed/`, which every fuzzing run replays. A job's timeout (replay 300 s, minimise
  900 s) starts once `run.sh` has built the target (it creates the run's `built` file), and a
  build has an hour of its own. A replay that times out, is cancelled, ends without a verdict or
  fails without a divergence, crash or sink failure (a kill, a refusal) leaves its cluster open
  and counts as a triage error (`recheck_unfinished`, exit status 1); only a verdict makes a
  cluster diverging or fixed, and a cluster is fixed only when every pin it was seen on passed.
  A replay the farm itself cancelled (a bench run started, SIGTERM) is no error: the recheck
  stops and the cluster stays open.
  A minimisation without a verdict does not use up one of a cluster's attempts (BL-174). On a model target a divergence
  sidecar's optional `cmd` line goes into the cluster's class (`<field> cmd=<cmd>`), so one
  field on different commands is not one cluster. The message models (`p2p_decode_model`,
  `p2p_gate_model`) write that line, Core's name for one of its 74 commands or `unknown` for
  anything else a mutated input spells (BL-267). A `minimize-divergence` job carries the line
  into the run's `abort-field` file (a second line, `cmd <name>`), so minimising cannot shrink
  onto the same field on another command. `--recheck` re-keys an open field-only cluster of a
  model target (one opened before the sidecars named commands) to the class its vector's replay
  gives it, moving its vector and provenance, or merging it into the `cmd=` cluster that exists
  (which keeps its own open vector). A cluster whose replays name no command, or two, keeps its
  class. A finding of a fixed cluster's class in a
  run started after the fix reopens it, with the new input as its open vector. The harness's
  own stops (the divergence limit, a sink failure) are not crash findings. A run whose logs
  record a sink failure gets an open `sink` finding (no vector: there is no input to keep),
  which holds its leg until `triage --ack-sink-failures` closes it once the cause is fixed; a
  crash libFuzzer reported without saving its input is an open crash finding without a vector.
  A run killed before
  its verdict is triaged an hour later. A run whose triage fails stays pending and is retried;
  after three failed passes it is a persistent error. Triage errors (unreadable state, runs not
  triaged, missing vectors) make `triage`, `status-line`, the smoke and the farm worker exit
  non-zero, and the STATUS line then reads `UNKNOWN` instead of a count. Only a missing
  `triage.json` is empty state. An existing one must have exactly the expected shape, and each
  cluster must be filed under its own id (the hash of its target, kind and class), own its
  vector's name, carry its promotion record when fixed, and have its vector where its state
  says, with provenance naming the cluster. Anything else is an error.
  Clusters are `fuzz/<target>/triage.json`.
- **Routing (PLAN §7.5).** A target's `route` is `private` unless `targets.toml` says
  `public`; every tx/block-codec target is private. Private-route findings, vectors and
  clusters stay in the private tree, and every summary counts them in one aggregate.
  `status-line` prints the STATUS-ready lines: the open and fixed counts, one `TASK fuzz-<id>`
  line per open public cluster (target, kind, class, pins), and one `TASK fuzz-private` line
  for all open private findings, which a designated reader triages in the private tree.
  `export-public` copies public-route vectors with a `PROVENANCE.md` for the manager to commit.
- After a fix, a leg that found something starts a fresh budget (CS0 §9.3): a leg's credit
  toward its share is the CPU time of its clean slices (passed or interrupted) since its last
  finding, so earlier time, clean or not, no longer counts once it has found something; failed
  and lost slices earn none. The summary reports all the time spent (`cpu_hours`) and the
  credited time (`credited_cpu_hours`) separately. A launch holds every leg whose target has an
  open finding on that pin (after its recheck), whose last slice found something in a run not
  triaged yet, or whose target's triage state cannot be read; within a launch a leg retires
  after a finding, or after two failures without one (a broken build). The farm triages the
  runs it recovers after a kill at once, not an hour later.
