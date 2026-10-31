# Review: SW-V24 S1 + S13 (rc.4 → rc.5 pin-move riders), Opus

| | |
|---|---|
| Reviewer | Opus (independent; brief `reviews/S1-S13-REVIEW-BRIEF.md`) |
| Written | 2026-10-31 02:18Z |
| rdc range | `ae6d9b0a4..3ba198877` on `node/sw-v24-rc5`: 3b052874a (S1 pin, G), d0a2c00c5 (SB4 regen, G), 3ba198877 (S13 tables, G) |
| Core | `node/oracle-v24.0-rc5` 1cdab7d9033 on v24.0.0-rc.5 8163290aa0 (tag object 3f3373a1) |
| Tier 1 | trial 5393d497c (`refs/swv24rc5/tier1-trial`), run `/work/scratch/gate-fast/runs/1031-012236-gate-5393d497c-A5C`, log `gate-fast/logs/swv24rc5-s13-t1.log` |

**Verdict: ACCEPT-WITH-NITS.** The replay is faithful. The SB4 regen and the S13 tables are generator output and hold only the allowed changes. Tier 1's reds are exactly the 27 predicted. The findings below are stale pin text and comments, plus two evidence notes. None of them changes code or vectors. 1, 2 and 4 are a doc-only rdc commit, and 3 can wait for the next shim commit.

## What I checked, and how

### Q1: oracle replay (faithful)

- `git range-diff 0e7382df80..35e3bcf11ad 8163290aa0..1cdab7d9033` (`range-diff.txt` here) gives 145 `=`, 2 `!` and 1 `>`. The two `!` (db389855140 `llmq/quorumsman.cpp`, acaedfa8db1 `node/blockstorage.cpp`) differ only in include context lines that rc.5 moved (`util/thread.h`, `util/fs_helpers.h`). The `>` is 1cdab7d9033 itself: `ORACLE_ABI_VERSION 31 → 32`, `oracle.h` only.
- range-diff skips merge commits, and the rebase stopped on 5 merges (`rebase.log`; `resolve-loop.sh` takes the original merge's file only when rc.4→rc.5 left that file alone). So I also checked the tree directly, file by file. For each of the 68 files the bundle touches, `git merge-file` (current = rc.5, base = rc.4, other = 35e3bcf11ad) merges cleanly and gives exactly `1cdab7d9033^:<file>` (`threeway.txt`: 68 SAME, rc 0). Both bundles touch the same 68 files.
- The 9 both-touched files are exactly PLAN item 1's list (`both-touched.txt`). I read every hunk on both sides. None of the bundle's hooks or trace lines overlaps an rc.5 hunk. One interaction is semantic: finding 3 (`quorumsman.cpp`). `Makefile.am` is fine: rc.5's new `BUILT_SOURCES =` is the only definition (the oracle include does not set it). In `validation.cpp`, the bundle's `ReplayBlocks` change is one `OriginScope` line, apart from rc.5's `SetTip`/BLS-scheme hunks. Those are S2's domain.
- Dump schema: `git diff 35e3bcf11ad 1cdab7d9033 -- src/test/oracle` is the one ABI line, so `oracle_chainparams_json` (oracle.cpp:1862) is byte-identical (M-2 (b) holds). The stopped first attempt f8478d35b26 (the dump key) is a sibling, not an ancestor.
- ABI 32 is consistent across `oracle.h:81`, `ffi.rs:11`, `smoke.rs:135`, README:37/39 and pins.txt (commit, branch, both sha256 = the two-root repro outputs `repro-{plain,fuzz}.out`). In tier 1, `abi_and_version` and `sb4_core_scenarios` (3/3, including `smoke_regenerates_byte_identical`) are green (oracle-diff.log:438-470).

### Q2: SB4 regen (only allowed fields)

My own decoder (`tools/sb4-indep.py`; it flattens every leaf, list lengths included, and does not reuse `sb4-compare.py`) over `3b052874a → d0a2c00c5` gives:
- the same 54 files;
- provenance changes only in `core_commit` (54, all `1cdab7d9033…`), `core_tag` (54, all `v24.0.0-rc.5`), `binary_sha256` (54, all `c78b3ed9…`, matching `build.out`) and `archives_sha256` (1,026 leaves, one value shared by all 54). `command`, `compiler` and `configure` are unchanged;
- outside provenance, only the two bench `instructions_u` values: dgw 492,336,600 → 491,985,759 (−0.071%), kgw 870,227,059 → 870,753,083 (+0.060%), both inside 0.005.

The gate's tag constant (`sb4_core_scenarios.rs:626`) and `regen.py` `CORE_TAG` move to rc.5. The commit has 57 files: 54 vectors, README, regen.py and the gate. The rc.4 move a6bf09a29 had the same shape. `sb4-compare.py` does what DEC-966 says. It does not check that `core_commit` equals the pin, but the gate test does.

### Q3: S13 (version lines + CE5-02 defaults only)

- `dash-args`: I ran `gen_options.py` from a fresh `git archive v24.0.0-rc.5 src test/functional` plus the committed captures. Its output is **byte-identical** to the committed `generated.rs`, so there are no hand edits and no generator drift. The S13 scratch inputs (`core-src`, `release`, `rpcspec-work`) were deleted at 02:14Z during this review, so this rerun is from my own inputs.
- The captures match the release. The cached rc.5 release `dashd` sha256 is `fa6f61de…8a3c` (= PROVENANCE). Its `-help`, `-help -help-debug` and `dash-cli -help` output, run in a separate datadir, is byte-identical to the committed `help/` files.
- Every non-version change traces to rc.5 `src/chainparams.cpp`: `-assumevalid` main `0000…0346a180…e98f` and test `0000…116cd3c1…318c` (`defaultAssumeValid`), `-minimumchainwork` main `…c15981a81af44a6abe1a` and test `…03ed573a939487fc` (`nMinimumChainWork`), and `-checkpoints … until block 2563000` (the last mainnet checkpoint). All of them are CE5-02 (INVENTORY:112), which DEC-971 (2) allows ("version lines plus the CE5-02 option-help defaults").
- `dash-rpc-spec`: only header, version and comment lines change. Corroboration: `src/rpc/client.cpp` is blob 260e3875 at both tags. rc.4→rc.5 adds no `RPCHelpMan`/`RPCArg`/`AddArg` line (the only rpc change is `pruneblockchain`'s clamp body). Tier 1's `dashd_help::help_matches_dashd` is green against the rc.5 dashd. I did not rebuild the rpcspec dump (30-min Core build).
- The file footprint equals the rc.4 pair fcdcda4a7 + ef18ea65e, minus `recognized.txt`, which rc.5 does not change.

### Q4: tier 1 on 5393d497c

- The trial tree `6cc41e371` equals `git merge-tree --write-tree 3ba198877 7f04e0447`. `gate_env: ST2_DASHD 24.0.0-rc.5`.
- Red step: only `oracle-diff`, with 617 passed and 27 failed. Those 27 (`tier1-reds.txt`) are **set-equal** to the S1 rerun's `oracle-diff.l1fails` (26) plus `payments::regular_block_masternode_payment`. That is PLAN §4 / DEC-958 / DEC-970's predicted set, including `l1_twins::diff_contextual_check_block_header`.
- Every other step is green. The `test` step had 8431 passed and 0 failed, and S13's suites are green there: `dashd_help` 2/2, `xcli_*`, and `dashd_v24::the_coins_setup_options_help_is_dashd_s` (test.log:10503).

### Q5: stale text

Findings 1–4.

## Findings

1. **minor**: `dash-oracle-sys/src/lib.rs:103` (`/// v24.0.0-rc.4, the parity reference.`) and `dash-oracle-sys/README.md:9` (`(v24.0.0-rc.4 since DEC-458; …)`) still name rc.4 as the pin. The rc.4 pin move a6bf09a29 updated both lines (`git log -S`), so S1 should move them too. Fix: `v24.0.0-rc.5, the parity reference.`, and README:9 → `v24.0.0-rc.5 since DEC-945`. Ideally also name the pinned branch (`node/oracle-v24.0-rc5`) in place of `node/oracle-shim-v24.0`.
2. **minor**: `dashd-masternode/tests/fixtures/oracle-v24/PROVENANCE.md:4, 12-15`. The provenance chain stops at 35e3bcf11a. Line 4 still says `masternode/meta.cpp` and `flat-database.h` are "unchanged from v24.0.0-rc.4". At rc.5 both changed, along with `meta.h` (meta: behaviour change in `CMasternodeMetaMan`, MN5-07, see manager). Each earlier oracle move appended its line (627f19fed BL-477, BL-497). `core_written_vector_is_current` passes at 1cdab7d9033 (tier-1 oracle-diff.log:1041), so the bytes still hold. Fix: append "So does the move to 1cdab7d9033 (`node/oracle-v24.0-rc5`, ABI 32, plain `dc03c627…`; v24.0.0-rc.5)" and scope line 4's "unchanged" claim to rc.4.
3. **minor (Core shim comment, not urgent)**: `src/test/oracle/oracle.h:1586` on 1cdab7d9033 says the LLMQ contexts run Core's quorum managers with "no worker threads". That describes rc.4's `CQuorumManager`. rc.5 has a behaviour change in `CQuorumManager::CQuorumManager` / `QueueQuorumForWarming` (MN5-05; see manager) that the comment does not cover. The shim still builds a `CQuorumManager` per `check_special_tx_block` call (oracle.cpp:4374) and per LLMQ context (oracle_llmq.cpp:464). Tier 1's LLMQ/BLS/QDATA suites are green, so I see no verdict effect. Fix: re-check the comment and the per-call construction against rc.5's constructor, and correct it in the next shim commit (S7's `minBlocksToKeep` commit or the next tag's S1). Don't make a new pin just for this.
4. **nit**: `dash-oracle-sys/README.md:39` says "ABI 32 (… the v24.0.0-rc.5 pin, with no entry point changed)". The bump rule (`oracle.h:80`) counts semantics too, and PLAN item 1 bumps because verdict domains move. The Core commit message is more precise ("No entry point changed its signature; the Core functions behind some of them follow rc.5"). Fix: "with no entry point signature changed; the Core functions behind them follow rc.5".
5. **nit (evidence)**: the trial merge 5393d497c is **unsigned** (`%G?` = N), while `commit.gpgsign=true` and `refs/swv24rc5/S11a-trial` is signed. Rule 13's `git merge --no-ff` would have signed it. Its tree is the clean merge-tree result, and it is a local-only ref that is never merged, so the verdict stands. Fix: confirm how it was made (for example `commit-tree`), and make future trials with the rule-13 recipe.
6. **nit (evidence)**: `sb4-evidence/build.out` records `check-seam` FAIL ("linked archive depends/…/libbacktrace.a is missing", exit 1). The separate `check-seam.out` a minute later passes (23 archives match MANIFEST), and `SB4-DIFF.md` says only "check-seam OK". Fix: add one line to SB4-DIFF.md on why the first check-seam failed and what changed before the rerun.

## Not checked

- I did not rerun the win64 captures (Wine), the rpcspec dump build or `oracle-diff`. I took no heavy lock.
- I did not read `/work/cache/oracle`, so I did not verify the installed archive sha256s myself (DEC-958 records the manager's check).

## Files here

`range-diff.txt`, `bundle-rc{4,5}.files`, `both-touched.txt`, `threeway.txt`, `tier1-reds.txt`, `tools/sb4-indep.py`, `regen/generated.rs` (independent dash-args regen), `dd/` (rc.5 release help outputs), `core-src/` (git archive of the rc.5 tag) and `mf/` (merge-file temporaries), both deletable.
