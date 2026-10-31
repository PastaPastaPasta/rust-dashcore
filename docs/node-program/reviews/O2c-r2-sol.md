# O2c r2 — Sol confirmation review

Reviewed 2026-10-10 under `briefs/REVIEW-COMMON.md`, after reading `O2c-r1-sol.md`.
Scope: rust-dashcore `58f92d45..8fc99dcbe4c5e5b9f67fc922dda6530fc5643169`,
Core v24.0 `e4113fa396..9c6dd45e0b10d245c7afe452426d8ff965416547`, and
Core v23.1 `ffa0ef1505..d01a37221be1d033a9436721c08d64d0b2d6cffb`.
The four Rust commits and four Core commits have good signatures (`%G? = G`).
The implementation worktrees were not edited; nothing was committed or pushed.
PLAN §7: codenames only; no private evidence or inputs were opened.

## Findings

No new blocker, major, minor or nit in this narrow confirmation review.

## F1 / BL-61 — closed in both entries

At the pinned Core shim's `src/test/oracle/oracle.cpp:2272` and `:2483`, the inner
catches accept only `std::ios_base::failure`. Allocation failures and other
infrastructure exceptions escape to `Guarded()` (`:216`), whose standard and
catch-all exception paths return `ORACLE_E_EXCEPTION`. `Fail()` is noexcept and
preserves the status even if saving its error message cannot allocate.

Payload write-back is outside the read catch (`:2277`). All P2P write-backs,
including qdata's request and both vectors, use `WriteBack()` (`:234`, `:2396`,
`:2405`, `:2415`, `:2424`). It translates a write-back `std::ios_base::failure`
into `std::runtime_error`, so even a stream exception there becomes an oracle
failure, rather than a refusal of already-read input. Other exceptions already
escape unchanged. The two pins' shim source/header blobs are identical; v23.1's
P2P entry remains `ORACLE_E_UNSUPPORTED`, while its payload entry gets the fix.

`node-l1/src/overlay.rs:321` and `:388` still treat only `ErrorKind::Decode` or
an explicit refusal as Core rejection. `ErrorKind::Exception` produces
`Divergence("oracle error")` before any agreement or StrictFlag result.

**Independent probe against the new pinned v24.0 plain archive:** a separate C++
`operator new` override throws once at each selected allocation and records its
size/fired bit. The original 34-byte getqrinfo (zero base hashes, zero request hash,
extraShare 2) remains `Ok(StrictFlag)`. All four allocation failures produce an
oracle error:

```text
baseline: StrictFlag
allocation 1 (34 bytes, input copy): oracle error
allocation 2 (1 byte, write-back): oracle error
allocation 3 (33 bytes, write-back): oracle error
allocation 4 (66 bytes, write-back): oracle error
```

A 66-byte variant with one base hash also remains StrictFlag. Its five allocation
indices (sizes 66, 32, 1, 33, 66) all produce oracle errors, including index 2 in the
vector **read**. Repeating both probes with `std::runtime_error` at every index also
produces oracle errors. Injecting `std::ios_base::failure` at each of the original
case's three write-back indices produces oracle errors too. No failing allocation
produced `BothReject`. The probe also asserts that actual truncated reads remain
`ErrorKind::Decode`.

**Installed regressions distinguish the old implementation:** I copied the
current Rust oracle rlib into scratch and replaced only its two bundled Core
object members with the authenticated round-1 plain archive members. Those
members were checked byte-for-byte; implementation files, target rlibs and cached
archives were untouched. The unchanged installed
`node-l1/tests/l1_overlay_alloc.rs:31` then fails at allocation 2:

```text
left:  Ok(BothReject)
right: Err(Divergence(oracle error))
```

The unchanged installed `dash-oracle-sys/tests/alloc_fault.rs` also fails against
these old objects for both its P2P and payload tests. Both suites pass with the new
pins. The payload regression covers both pins, complete and truncated payloads,
and the input-copy/read/write-back allocations. Genuine refusals and v23.1 P2P
unsupported behavior also pass the ordinary P2P/ABI suites.

Evidence: `/work/scratch/o2c-r2-sol/classification-probe.log`,
`old-overlay-regression.log`, `old-sys-regression.log`, `targeted-tests.log`;
probe sources, exact negative-control compiler arguments and method are retained
in that directory and `commands.md`.

## Re-pin identity and reproducibility evidence

Both Core branch heads equal `pins.txt`. The pin's own
`contrib/node/build-oracle.sh --jobs 8 --flavor plain` and `--flavor fuzz` each
returned two verified cache hits. This tooling hashes the actual archive bytes
and checks manifest pin/commit/flavor/symbol identity. All four installed archives
match the recorded digests:

```text
v23.1 plain 481180930f10b29e1ab95ae0c3c9070eb945c0f2f2af5e7ddb2a85e1b72f74c1
v23.1 fuzz  dd7731843194794da5a4802249391c649085419788326305ba0c320e257aa2d7
v24.0 plain 84cedba0588fae0db0900db191af837421254f2f9ee4ceef3b0a4053a052c8d5
v24.0 fuzz  87de464e87a241f47691578cc6c9444d61afd5d71140ef6b28143709fe57611f
```

Their manifest digest fields and recipe hashes also match the reviewed script
(`428e540f4eba350696f34d9cc8c393aa77d7060fb4a5a42747d3d0a6ec60b801`).
`/work/scratch/o2c/logs/r1-repin-plain.log` and `r1-repin-fuzz.log` record matching
new-pin hashes from both clean roots and installation in `/work/cache/oracle`.
I independently verified current archive identity; the two-build reproducibility
claim is supported by those existing logs. No Core rebuild was attempted on the
loaded host. Evidence: `build-oracle-cached-{plain,fuzz}.log` and
`pin-manifests.json` under the reviewer scratch directory.

## F2 — closed

Core `src/test/oracle/oracle.h:620`, Rust `dash-oracle-sys/src/p2p.rs:41`, and
`node-l1/src/overlay.rs:226` now explicitly limit qdata success to the modeled
codec reads. The docs cover exact counts (including contribution undercounts),
the handler's earlier stop after a refused verification vector, conditional
contribution reads controlled by node/quorum/member state, and subsequent handler
validation. Counts remain separate and compared. The ABI explicitly says full
handler parity would require a separate stateful entry. This satisfies F2.

## Validation and program dependencies

All reviewer commands ran at nice 10. Rust/test commands used
`CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, the O2c worktree's own target directory,
and mode-700 `TMPDIR=/work/scratch/o2c-r2-sol/tmp`. No full workspace gate or
private replay was run.

- Required P2P and overlay suites: **8/8 PASS**, including the 206-case judge,
  O4 fixtures, 4,200 mutations and the corpus's 11 declared strict flags.
- New `alloc_fault` and `l1_overlay_alloc`: **3/3 PASS**. Shared-helper refactor
  validation via `contextual`: **15/15 PASS**. ABI/pin `smoke`: **18/18 PASS**.
  Archive authentication regressions: **8/8 PASS**. Total Rust tests: **52/52**.
- `fuzzctl check`: **PASS**, 23 targets / 28 smoke legs / 28 farm legs.
  `fuzzctl selftest`, which executes `fuzzctl_test`: **50/50 PASS**.
- `cargo fmt --all -- --check` and review-range `git diff --check`: **PASS**.
- `check-base-sdk-pins.sh`: expected **FAIL**, exclusively because
  `d53e0927907919a0a29c679c73e79288ac68f393` is not yet on `node/base-sdk` or an
  accepted tag. BL-55 remains a merge-order dependency: merge BS8, re-pin the
  root and rerun the program gate before claiming it is green.
- Supplemental `selftest-checks.sh` pin-check portion independently reproduced
  exactly **7 expected BL-55 failures**. Its subsequent cargo-deny R2 **clean tree
  passed** with the disk tempdir, confirming the reported quota-related rerun
  outcome. I stopped the supplemental script before completing its unrelated
  later mutation suites to limit host load; this is a partial checks-selftest,
  not a claim that the complete program gate passed. See `selftest-checks.log`
  and `selftest-stop.json`.
- Read-only `systemctl --user show` confirms
  `node-fuzz-farm-o2c-overlay-r2.service` is active/running from the O2c campaign
  checkout. Its HEAD pin file contains these new commits and hashes. The service
  and checkout were not changed. This confirms service activity and checkout pins,
  not completion of its remaining campaign budget or private results.

DEC-150 supersedes the round-1 BL-62 recommendation: O2c correctly leaves the
static pin-set shape/check to BS4b and will adopt its canonical `Target.pins`
shape after BS4b merges. No separate BL-62 check is requested in O2c.

F1/BL-61 and F2 are closed; no remaining O2c confirmation-review issue blocks the
Sol gate. The existing BS8 re-pin and program merge-order requirements remain.

**Verdict:** APPROVE

Sol gate satisfied: yes
