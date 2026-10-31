ACCEPT-WITH-NITS

Independent review of SW-V24 S1 + S13, completed 2026-10-31T02:14:03Z.

## Findings

1. **nit — `dash-oracle-sys/README.md:9`: the current-pin overview still identifies rc.4.** The “What it links” paragraph says `node/oracle-shim-v24.0` is at `v24.0.0-rc.4`, while this change pins `node/oracle-v24.0-rc5` at rc.5 and updates the same README's ABI section to 32. Someone following the overview gets the wrong release/branch description. Update this paragraph to identify the rc.5 pin and branch, retaining the generic branch-promotion explanation if useful. This does not affect the build: `pins.txt` is correct.

No blocker, major, or minor findings. The remaining citation sweep is explicitly assigned to S14; historical rc.4 references are not blanket findings. In particular, `dash-oracle-sys/src/lib.rs:103` still labels the parity release rc.4 and should move with that sweep.

## Scope and identities

Read the task brief, DELEGATE-RULES.md, REVIEW-COMMON.md, PLAN.md, the specified source deltas and evidence. Reviewed frozen rdc branch `node/sw-v24-rc5`, head `3ba1988778f1839c1c4aefb968c6a2edd7e96b7f`, against `ae6d9b0a4`:

- `3b052874a7cfb36a1aed49e572b2cbf317762166` — S1 oracle pin; signature **G**.
- `d0a2c00c5e77fe66641cf1b8626c4211878bf17a` — SB4 regeneration; signature **G**.
- `3ba1988778f1839c1c4aefb968c6a2edd7e96b7f` — S13 tables/captures; signature **G**.

Core pin: `1cdab7d9033a8d89f3b6e8723d9cd72f171f1dcd`, based on rc.5 `8163290aa0307ed4bc2fc0ba15b78c20d61dfa88`.

## Oracle replay and ABI

The requested range-diff maps every one of the 147 old non-merge patches. Only two patches differ, both solely in adjacent include context (`quorumsman.cpp`, `blockstorage.cpp`); the sole additional patch bumps the ABI to 32. The recorded rebase resolutions do not leave an unexplained final-tree delta.

Reviewed the hunks in all nine both-touched files. Independently checked that the accumulated shim's added/deleted lines are identical before and after replay in each file: Makefile.am, chainparams.cpp, init.cpp, llmq/quorumsman.cpp, net_processing.cpp, node/blockstorage.cpp, util/fs_helpers.cpp, validation.cpp, validation.h. The rc.5 edits remain in their appropriate functions, and the replay/StateDigest hooks remain in place. All 23 added shim/StateDigest files are byte-identical to the old bundle except the expected oracle.h ABI constant. In particular, oracle.cpp and its chain-parameter dump are byte-identical: no schema addition, including no premature `minBlocksToKeep` key.

ABI 32 agrees in Core's oracle.h, Rust ffi.rs, smoke.rs and the README ABI section; pins.txt selects the matching Core commit. Independently hashed both actual plain archives and both actual fuzz archives under the supplied scratch reproduction roots: all four match the respective pins.txt hashes. Reviewed the successful two-root reproduction logs. No oracle cache was read.

## SB4

Independently compared the committed gzip/JSON vectors at `3b052874a` and `d0a2c00c5`, including equal file sets. All 54 vectors change only `provenance.core_commit`, `core_tag`, `binary_sha256`, `archives_sha256`, plus these two permitted measurements:

- DGW: 492,336,600 → 491,985,759 instructions, −0.071260%.
- KGW: 870,227,059 → 870,753,083 instructions, +0.060447%.

Both are within BENCH_TOLERANCE = 0.005. All other JSON values, including other provenance fields, are identical. Every vector names the new full pin and rc.5 tag. The regeneration and gate constants agree. The old evidence's “outside the ruling's list”/compare exit 1 predates DEC-966; the current committed delta satisfies the updated rule. The initial missing-archive seam check is superseded by check-seam.out's successful complete check. The recorded regeneration check passed (52 exact, two tolerance-checked); the trial's SB4 and smoke suites pass.

## S13

The RPC table changes only its release/version and binary provenance lines. Its command data is unchanged. The option table changes only the source/version header and the three CE5-02 help texts. The 14 changed captured help files contain only release banners and those same CE5-02 values; generator implementation changes are pin constants, release checksum constants and version documentation, with no generator behavior drift.

Re-ran both emitters into this review directory. Both generated tables reproduce byte-for-byte from the saved inputs. Additionally regenerated the option table using source files obtained directly with `git show` from rc.5, again byte-identical.

The changed option values trace directly to rc.5 `src/chainparams.cpp`:

- `-assumevalid`: mainnet line 231; testnet line 442.
- `-minimumchainwork`: mainnet line 230; testnet line 441.
- `-checkpoints`: mainnet's final checkpoint at line 348, height 2,563,000.

Core init.cpp lines 598, 622 and 806 read exactly those chain parameters when constructing these help texts. These are CE5-02 option-help changes; no unrelated help/option change was found.

The capture log records Linux, win64 and gen_options exit 0. The RPC build/capture log records 289 commands, 347 conversion entries and exit 0. Some large S13 scratch inputs were removed during this review, after the successful generator replay; fresh release capture/checksum verification was consequently not repeated. This is a verification limit, not evidence of generator drift.

## Tier 1

Trial `5393d497c448f8414027499c1c68f4c245823fd9` has exactly the expected parents: the reviewed head and CI-S0 `7f04e0447209fc9dafc6fdcdc063b0f0ed8412ef`.

Run directory: `/work/scratch/gate-fast/runs/1031-012236-gate-5393d497c-A5C`.
Log: `/work/scratch/gate-fast/logs/swv24rc5-s13-t1.log` (its header names the trial SHA and rc.5 ST2_DASHD).

All 18 other recorded steps exit 0; only oracle-diff exits 1. The normal test lane reports 8,431 passed, zero failed. S13's three dependent suites pass: dash-rpc-spec dashd_help (2/2), rdash-cli xcli (5/5), and rdashd `the_coins_setup_options_help_is_dashd_s`.

Parsed oracle-diff independently: exactly **27 failed tests in nine test binaries**, all in PLAN §4's predicted set:

- payments: 1; l1_coinjoin: 3; l1_credit_pool: 1; l1_payments: 4 — CE5-01, as listed in the updated plan.
- l1_boundaries: 1; l1_checks: 4; l1_twins: 7 — predicted CE5-01/02 and behaviour change in `ContextualCheckTransaction`, see manager. Both permitted variable-seed twin legs appear in this run.
- l1_mnsync: 4 — behaviour change in `CMasternodeMetaInfo::SetPlatformBan`, see manager.
- l1_versionbits: 2 — CE5-01.

The exact qualified names are in oracle-red-set.txt. No additional failing suite or test was found. This is acceptance of these riders under the planned red-set contract, not a claim that the complete train is already green.

## Review execution and artifacts

No builds or Cargo tests were rerun; the brief makes those optional. Validation performed here was read-only Git/source inspection, full vector comparison, independent archive hashing, saved-input generator replay, direct-rc.5 option regeneration, and gate-log inspection. Mutants: not applicable to these pin/generated-data riders. No off-brief edits, branch switches, commits, pushes, PRs, or GitHub posts. The reviewed worktree remained clean at the reviewed head.

Only this review directory was written: `/work/scratch/sw-v24-rc5/reviews/s1s13-sol/`. It contains this report, verification script/results, the qualified red set, range-diff and hunk-check summaries, regenerated tables and a small rc.5 source snapshot used by the option generator.
