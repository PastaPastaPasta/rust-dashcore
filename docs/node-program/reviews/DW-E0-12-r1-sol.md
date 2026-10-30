# DW E0-12 — review r1 (Sol)

Reviewed on 2026-10-09 as the sole reviewer under DEC-92. Scope: `89cd547`, `a580102`, and `aace655`; branch head `aace6555533f1c7835d551088fc04cfce5c2667d`.

## Verdict

**APPROVED WITH SUGGESTIONS**. The requested gate passes on the clean trial merge with `7bf0bf3`. No blocking defect found in the E0-12 changes.

## Findings

1. **P3 — Keep the instruction-count result separate from CI elapsed-time predictions.** `docs/research/shielded-cost.md:51`, `docs/research/shielded-cost.md:69`, `docs/research/shielded-cost.md:78`. The measured +5.9% / +6.6% instruction deltas are correct, but “instruction counts do not depend on load” / “load-independent” is stronger than the evidence, and “the feature adds about 6%” to a 25–60 minute cold CI build extrapolates instructions directly to elapsed time. There is one perf sample per configuration, no measured variance, and C compiler caching remains enabled; the hosted CI runner also has a different machine and cache state. Instruction counts are substantially less sensitive to scheduler waiting than elapsed time, but they are a workload proxy rather than a timing prediction. Suggested change: describe the percentages as the observed user-space instruction deltas on this host, explicitly say their spread is unmeasured, and leave hosted-runner elapsed-time overhead unresolved until measured there. This does not invalidate the recorded samples.

2. **P3 — Give the raw measurement evidence a durable reference.** `docs/research/shielded-cost.md:27`, `docs/research/shielded-cost.md:129`. The document says the raw log is kept with the scripts but supplies no location, artifact link, or measurement-revision identifier. I found and checked `/work/scratch/e0-12/{measure.sh,measure-perf.sh,results.tsv,perf.tsv,perf-*.txt,uptime.log}`; those host-local scratch files support the document today but are not available to another checkout and may disappear. Suggested change: retain the scripts and raw results in the repo or a durable artifact, link them, and identify the measured source revision. The documented commands are sufficient to start another run, but not to audit these exact samples without finding that scratch directory.

## Merge and default-build isolation

- Created the requested detached worktree at `/work/scratch/e0-12-sol/repo` from `aace655`, then ran `git merge --no-commit --no-ff 7bf0bf3`. The merge was clean. The trial-merge tree is `e29ff738df8b15236cfd79cf9a62552d397582e4`; no merge commit or branch update was created.
- Relative to `7bf0bf323aa920de5e73de91bcb581303b68e2e1`, the merged tree changes exactly the six E0-12 paths. Every one of those files still matches `aace655` byte-for-byte. `git diff --check 7bf0bf3` passed.
- Independently compared `cargo tree --workspace -e features --locked` on current main and the merged worktree after normalizing only checkout paths: identical. The default `cargo metadata --format-version 1 --locked` resolved nodes, dependency edges, and activated features are also identical after path normalization. The implementer's original saved default-feature trees also compare identically after path normalization.
- `rust/crates/dw-engine/Cargo.toml:14` adds only an opt-in feature forwarding to both upstream shielded features; there is no `default` feature enabling it. The upstream wallet defaults remain `bls,eddsa`, and the existing workspace disables storage defaults. No engine library source or default-on code path is added by this patch.
- `rust/crates/dw-engine/Cargo.toml:18` correctly gates the example with `required-features = ["shielded"]`. Explicitly building the example without the feature fails with Cargo's expected “requires the features: shielded” error (exit 101), rather than leaking a shielded import into default builds. The enabled feature reaches `platform-wallet/shielded`, `platform-wallet-storage/shielded`, `dash-sdk/shielded`, and `dpp/shielded-client` as intended.

## Nightly workflow

`.github/workflows/nightly.yml:132` parses as YAML and defines a normal blocking job on `ubuntu-24.04`, matching the existing regtest runner. Its checkout and Swatinem cache actions use the existing pinned revisions, with the same `rust -> target` cache workspace and the same disk/protoc/toolchain setup. Build and Clippy commands use `--locked`; the warm-up has its own timeout. The existing scheduled/default-branch and manual triggers remain intact, PR CI is untouched, permissions stay `contents: read`, and the new job references no secrets.

**It should block nightly status.** A deterministic compile/lint/proving-key initialization failure is actionable and would defeat E0-12's requirement to keep the opt-in feature green. This is a nightly workflow result, not an added PR merge gate. Warm-up calls only the local `OnceLock<ProvingKey>::get_or_init(ProvingKey::build)` at the pinned upstream revision; it requires neither wallet credentials nor a Platform connection. The example is a prover-initialization smoke check, not proof-generation or shielded-flow coverage, which the research document acknowledges.

## Measurement audit

Checked the scripts, raw perf output, successful build logs, TSV records, and warm-up outputs under `/work/scratch/e0-12`. The Rust cache bypass and separate fresh target directory per run are implemented; off/on elapsed-time runs interleave and reverse order. The four perf records report 100% counter running time and successful exit codes.

- Raw instructions: dev `7,776,686,785,640 -> 8,233,744,995,248` (**+5.877%**); release `6,339,703,210,471 -> 6,760,263,995,148` (**+6.634%**). The doc rounds these correctly.
- Compiled crate counts are 517 / 547 in the successful builds. The 40 lockfile additions versus 30 extra compiled crates is expected: the lock includes optional/target-specific transitive packages too.
- Stripped sizes are dev `54,292,392 -> 54,637,448` bytes (**+345,056 bytes**), release `42,363,184 -> 42,593,648` bytes (**+230,464 bytes**). The unstripped sizes, repetition variation, target-dir sizes, and CPU-time summaries also match the raw records at the documented precision.
- Cold-process warm-up medians are **3.232 s dev** (4 runs, 2.972–3.462 s) and **3.508 s release** (6 runs, 2.920–3.782 s). These match the document. Cached-call output rounds to 0 microseconds; the source confirms subsequent calls only read the process-wide cache.
- The doc honestly reports two elapsed-time repetitions, one perf repetition per cell, discarded incomplete work, unresolved wall-clock build overhead, the host/load context, the enabled C cache, and the distinction between warm-up and a first full proof. It correctly limits binary-size conclusions to today's reachable code and calls for remeasurement when X2 wires the flows. Apart from finding 1, the method and reported numbers are sound for this exploratory measurement.

These checks audit the implementer's measurements; I did not repeat the eight clean builds or four perf builds and do not claim the figures were remeasured on the new main revision.

## Lockfile and licenses

`rust/Cargo.lock:3987` and the other new package records contain **40 additions, no removals, and no version/source/checksum changes to existing packages**. Existing records change only feature-dependent edges or disambiguated package names. All additions are absent from the default resolved graph and belong to the expected commitment-tree/Orchard/Halo 2 closure. Less obvious packages are explained by Orchard -> reddsa -> frost-rerandomized -> frost-core -> postcard (including heapless/atomic-polyfill/embedded-io); the git additions are the expected pinned GroveDB commitment tree, Dash Orchard fork, and Dash note-encryption fork.

Inspected all 40 additions' Cargo license declarations: 27 `MIT OR Apache-2.0`, eight `MIT`, four legacy `MIT/Apache-2.0`, and one `Zlib OR MIT OR Apache-2.0` (`visibility`). All permit an MIT choice; no new copyleft or proprietary license was found. Also ran `cargo deny --all-features --locked list --format json --layout crate` with cargo-deny 0.20.2: exit 0, no unlicensed entries among the 26 additions in its pruned active graph. The other 14 additions were checked via metadata/manifests. The repo has no `deny.toml` or configured cargo-deny CI gate, so there is no project-policy `cargo deny check` pass to claim.

## Validation on the trial merge

All Cargo commands run from the detached worktree's `rust/`, with toolchain 1.98.1 and its own `rust/target`. The requested Clippy warning flag is passed after Cargo's `--` separator. Local gates use `CARGO_BUILD_JOBS=6`, `TMPDIR=/dev/shm/e0-12-sol` (mode 0700), and `RUSTC_WRAPPER=`; the initial attempt using shared sccache stalled, was stopped only for this worktree, and was restarted without the wrapper.

| Check | Result |
|---|---|
| `cargo fmt --check` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS (exit 0) |
| `cargo clippy --workspace --all-targets -p dw-engine --features shielded -- -D warnings` | PASS (exit 0) |
| `cargo test --workspace` | PASS (exit 0): 554 passed, 0 failed, 1 ignored (the opt-in OS-keyring round trip) across suite results |
| Feature-enabled `dwcli` and warm-up example builds; fresh-process warm-up | PASS (all exit 0): readiness false -> true; `warm_up_ms=3208`, `cached_warm_up_us=0` |

Review logs and graph/license evidence: `/work/scratch/e0-12-sol/logs/`.

## Limitations and cleanup

No hosted Actions run, cross-platform shielded build, funded proof, or X2 flow was exercised; those are beyond this opt-in measurement task. The detached worktree was removed with `git worktree remove --force` after validation and its removal was verified. The maintained checkout remains on `7bf0bf3` with its original status. No source fixes, branch updates, or commits were made; this report is not committed.
