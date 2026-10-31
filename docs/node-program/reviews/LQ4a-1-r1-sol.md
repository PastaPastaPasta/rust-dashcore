# LQ4a-1 code review, round 1 (Sol)

**Target:** `node/lq4a-1` at `b129bc537`; scope is the `dash-llmq/src/dkg/` implementation against Dash Core v24.0.0-rc.4 (`49caf372bb`).

**Verdict: ACCEPT-WITH-NITS**

The reviewed state machine, member indexing, contribution draw/encryption order, receive-side complaint decisions, 32-entry pending verification trigger, batch signature grouping, wire field layout and sign/session hashes agree with Core for the LQ4a-1 slice. The two findings below are low-severity quality issues in the injected RNG seam; neither changes the current test-vector verdicts.

## Findings

### n-1. `ReplayRng` does not validate the IES recipient-count tag

**Rust:** `dash-llmq/src/dkg/rng.rs:188-197` (`Draw::Ies` and `ReplayRng::ies_seed`). **Core:** `src/bls/bls_ies.cpp:59-71` allocates one IV/blob slot per recipient during `InitEncrypt`; the session calls it from `src/active/dkgsession.cpp:83-95`.

`DkgRng::ies_seed` accepts `recipients`, but `Draw::Ies` stores only the seed and `ReplayRng::ies_seed` accepts any count. A replay in which member indexing or the recipient count changes from (for example) 33 to 32 will still consume the recorded IES draw and continue, so the first divergence is hidden even though Core's container shape and subsequent encryption loop differ. This is observable in a replay harness before any BLS verdict is reached.

Include the recipient count in the recorded `Ies` draw (or compare it in `ReplayRng::ies_seed`) and return a `ReplayMismatch` when it differs.

### n-2. `ReplayRng` does not validate the simulated-error rate argument

**Rust:** `dash-llmq/src/dkg/rng.rs:199-205` (`ReplayRng::rand_bool`). **Core:** `src/llmq/dkgsession.cpp:56-63` passes the current rate into `GetRandBool`, whose threshold is evaluated in `src/random.cpp:653-661`.

The replay record stores `Draw::Bool(SimError, bool)`, and `ReplayRng::rand_bool` matches only the error kind; it ignores `percent`. Replaying a transcript recorded with a 100% `contribution-lie` rate against a run configured at 50% still consumes the draw and returns the recorded decision, so a changed simulation input is silently accepted instead of being reported as the first mismatch.

Record the percent with each boolean draw, or compare the requested percent against the recorded decision metadata before consuming it.

## What I ran

- `CARGO_BUILD_JOBS=12 cargo test -p dash-llmq --lib --all-features` — **73 passed** (including the 33-member mid-batch trigger, LLMQ-100 vectors, BS12 transcript, and 100-member harness).
- `CARGO_BUILD_JOBS=12 cargo test -p dash-llmq --all-features` — **73 unit tests passed; 1 wire-firewall integration test passed; doc tests passed**.
- `CARGO_BUILD_JOBS=12 cargo clippy -p dash-llmq --all-features --all-targets -- -D warnings` — passed.
- `cargo fmt --all -- --check` — passed.
- `python3 contrib/node/check-layers.py` — passed (with the repository's existing baseline L3 note).
- `contrib/node/check-pure.sh` — passed for the checked pure crates; no `dash-llmq` violation reported.
- `git diff --check ba2d46fcf b129bc537` — passed.

The target worktree was not modified. Tests and inspection used `/work/scratch/lq4a1-rv-sol1`.
