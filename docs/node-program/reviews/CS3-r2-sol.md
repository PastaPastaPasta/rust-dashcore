# CS3 confirmation review, round 2 — Sol

Reviewed `node/cs3-checks` at `7c33350217dd5f4b0a20cea8454bdd246ae97867`, limited to `65066e87`, `90bf0058`, and `7c333502` since r1's `ea501ed7`. Followed `REVIEW-COMMON.md` and DEC-92's sequential Sol → Opus-high order. Work was performed in a detached worktree under `/work/scratch/cs3-review-r2/`, with mode-0700 `TMPDIR=/dev/shm/cs3-review-r2-tmp` and `CARGO_BUILD_JOBS=8`. The implementer's worktree was read-only. Settled check implementations were not re-reviewed.

**CS3-S1 and CS3-S2 are closed. One new major finding prevents approval: the window clamp changes the generated recent history and can change a seed's verdict.** Rust and Core agree on both the original and replacement contexts; this is a test-harness regression, not an observed consensus divergence.

## Confirmed closures

### CS3-S1 — closed

`node-l1/tests/private_seed_replay.rs:105`–127 now distinguishes comparison from exclusion per seed. Only `BothAccept` or `BothReject` earns credit; every pin is still examined for divergence. A registered seed with only `OutOfDomain` outcomes fails the replay, whether required or pending. Another seed's successful comparison cannot give it credit. CS3 comparisons at v23 are excluded, so a CS3 seed necessarily earns its credit at v24.

The replay suite passed 7/0. Its six synthetic tests exercise registered/unregistered targets, v24-only comparison, required format/context exclusions, a successful seed beside an excluded seed, and an excluded pending seed. The seventh, real-private entry point explicitly reported SKIPPED because `DASH_EMBARGO_DIR` was unset. No private input was inspected.

### CS3-S2 — closed

`node-l1/tests/l1_checks.rs:80`–88 separates actual check rejections from matched decoding failures. At lines 112–121 it requires an acceptance, at least half of all cases reaching a check verdict, at most a fifth excluded, and the existing required rejection reasons.

The reproduced verdict/decode/exclusion counts were:

| Target | Cases | Check verdicts | Decode rejections | OutOfDomain | Verdict floor / exclusion cap |
| --- | ---: | ---: | ---: | ---: | ---: |
| Transaction | 3,000 | 2,001 | 999 | 0 | 1,500 / 600 |
| Block | 3,000 | 2,542 | 458 | 0 | 1,500 / 600 |
| Contextual transaction | 3,000 | 2,641 | 354 | 5 | 1,500 / 600 |
| Contextual block | 2,000 | 1,478 | 283 | 239 | 1,000 / 400 |
| Contextual header | 800 | 738 | 0 | 62 | 400 / 160 |

A refusal affecting more than 20% of any entire census cannot pass even if its required reasons and an acceptance survive. The verdict floor also prevents broad loss from being disguised as matched decoding failures. A scratch probe of the actual `Census::check` passed at exactly 50% checked / 20% excluded and confirmed failure at 49% checked, 21% excluded, 25% excluded with 75% still checked, and 76% matched decoding failures.

These are conservative coverage bounds, not an assertion of unchanged coverage. Depending on the target, 501, 600, 595, 161, or 98 additional verdict-to-exclusion cases could still pass the numeric bounds if the acceptance/reason requirements survived. The bounds close r1's vacuous-coverage hole and reject a broad refusal above a fifth of a target's cases; they do not promise detection of every smaller or category-specific loss. The boundary-row suite continues to prohibit exclusions entirely.

## Finding

### CS3-S3 — major: the performance clamp replaces the checked history instead of retaining its suffix

**Locations:** `dash-consensus/tests/support/checks.rs:538`, with `Position::window` at lines 248–271 and the unchanged-verdict claim at lines 103–104.

`HeaderInput::parse` passes a smaller length into `Position::window`. That function restarts the RNG from `self.seed` and generates forward from the window's first height. Consequently, a 4,040-header input clamped to 577 headers gets samples 0–576 relabelled as the most recent heights, instead of retaining samples 3,463–4,039. The previous tip, recent times, difficulty inputs, candidate timestamp/bits, and clock can change. Proving that Core reads no earlier than the retained height does not prove equivalence when the retained data are replaced.

**Reproduction using public synthetic data only:** build the same input bytes for the parser before and after `7c333502`:

```rust
let mut input = Position::encode_height(Network::Main, 68_590, 6);
input.resize(HEADER_INPUT_LEN, 0);
input[12] = 7; // select the 4,040-header window
input[13] = 3; // candidate version 4
input[14..18].copy_from_slice(&(-300i32).to_le_bytes());
```

At `90bf0058`, the original window's tip time is `1602284198`, its MTP is `1602283307`, and the candidate time is `1602283898`: **Core and Rust accept**. At `7c333502`, the replacement tip time is `1601515574`, its MTP is `1601515286`, and the candidate time is `1601515274`: **Core and Rust reject**, result class `BLOCK_INVALID_HEADER` (4), reason `time-too-old`, debug message `block's timestamp is too early 1601515274 1601515286`.

A temporary Rust test loaded the previous parser from `git show 90bf0058:dash-consensus/tests/support/checks.rs`, compared both contexts against the pinned live oracle, and asserted this acceptance-to-rejection change. It passed. The same test retained the actual final 577 headers of the original window and kept its original candidate and clock: Core and Rust both still accepted. Thus the history bound is sufficient for this case; regeneration causes the verdict change.

**Impact:** the optimization's stated invariant and this round's requested confirmation fail. Existing encoded header seeds can replay a different check scenario while preserving their byte digest and still obtaining a successful differential receipt. Green differential comparisons alone do not detect this, because both sides consume the replacement context. This finding makes no claim about the contents or verdict of the private seed.

**Suggested fix:** retain the single-hash optimization, but preserve the original check-consumed suffix and candidate semantics when shortening a window. If prefix hashing is omitted, advance the deterministic RNG/time state across that prefix rather than restarting at the retained first height. Alternatively, remove the clamp until an equivalent construction is available. Add an unclamped-versus-clamped regression using this public case and the relevant network/algorithm boundaries, then rerun the CS3 comparisons and the authorized private replay.

## Performance-fix assessment

The hash cache is safe: the sole `Window::new` caller passes the hash computed from each corresponding header; cached hashes and header order match the former recomputation.

The numeric cutoff itself is conservative for these fixed v24 network parameters. Core's `pow.cpp` uses `86,400 / 150 = 576` headers for a BTC retarget; its minimum-difficulty walk stops at the preceding interval boundary. DGW reads 24 headers, and `CBlockIndex::GetMedianTimePast` reads 11. The harness's buried-version and checkpoint checks add no deeper header-field lookback. Therefore 577 covers the non-KGW paths. Mainnet KGW windows remain unshortened, preserving their existing lookback and loop-exit predecessor. The defect is the changed data inside that sufficient window, not an insufficient cutoff.

## Verification and limits

All commands used the requested scratch/temp paths and eight build jobs, with `DASH_EMBARGO_DIR` unset:

- `cargo test -p dash-oracle-sys --features oracle --locked --no-fail-fast`: **62 passed, 0 failed**.
- `cargo test -p node-l1 --features oracle --locked --no-fail-fast`: **41 passed, 0 failed**, including all five CS3 proptest twins.
- Explicit CS3 rerun with `--test l1_checks --test l1_check_rows --test private_seed_replay -- --nocapture`: **14 passed, 0 failed**. Both boundary-row tests passed, the five censuses reproduced the counts above, and the replay synthetic tests passed with the real-private entry point skipped.
- Two temporary confirmation tests: **2 passed, 0 failed**; one proves the clamp's verdict change and true-suffix preservation, the other checks the census thresholds.

Oracle archives were pinned: v24 at `a9a483e51efdadeee37cf40ff8697da62b3cd012`, based on Dash Core `v24.0.0-rc.3`; oracle-diff also exercised the pinned v23 archive. The lookback assessment was checked against Core's `pow.cpp`, `chainparams.cpp`, `chain.h`, and `ContextualCheckBlockHeader` in `validation.cpp`.

The reported private-tests receipt race is treated as the known BL-49 environment flake, not a CS3 finding. I did not rerun the real-private wrapper or inspect the private-route input. This round did not repeat the full workspace or fuzz-smoke gates. Temporary tests, the detached worktree, logs, build output, and TMPDIR were removed; no surviving review-owned dashd was found. This review file is uncommitted. No commit, push, or GitHub comment was made.

**Sol gate satisfied: no.** Resolve CS3-S3 before the sequential Opus-high pass.

**Verdict: CHANGES-REQUIRED.**
