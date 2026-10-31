# BS5 round 2 — narrow closure review

Read-only review of base-sdk **8c23be5** on **481f8c3**, specifically round-2 changes **fdda6b2 / 8c23be5**, and rust-dashcore **e11a7cfd**, limited to generator work **5fad804d / 307d8c11**. The dash-consensus adapt was not reviewed. Read REVIEW-COMMON and my round-1 review. Fresh detached worktrees: `/work/scratch/bs5-r2-gpt/base-sdk` and `rdc`. No commits, pushes, GitHub comments, or edits to the implementation branches. Temporary experiments in detached scratch files were restored; both worktrees finish clean.

| Round-1 item | Disposition |
| --- | --- |
| 1. Overwritten negative vbparams heights poison the builder | **CLOSED** |
| 2. Corpus provenance is unenforced | **PARTIAL** |
| 3. Spork overrides are missing | **CLOSED** |
| 4. BRR table lacks a regression guard | **CLOSED** |
| 5. Corpus adapter misparses negation | **CLOSED** |

## Closure evidence

**1 — CLOSED.** `pkgs/params/src/builder.rs` now retains signed temporary minimum heights per deployment and converts only final values. Re-ran both original inputs at both pins:

```text
-vbparams=v24:0:9:-1 -vbparams=v24:0:9:0
-vbparams=v24:0:9:-1 -vbparams=v24:0:9
```

Both build successfully and match Core's final minimum height zero. The permanent `overwritten_negative_heights_are_accepted` tests pass at both releases, including a different deployment whose final height remains -5 and the DIP3 overwrite. Corpus cases cover final-negative refusals and Core parsing-error precedence. Checked Core with `git show`: v23.1.8 `src/chainparams.cpp:932–936`, v24.0.0-rc.3 `:972–976`, and their parsing loops. Evidence in reviewer scratch: `params-test.log`, `adapter-probe.log`, `probe-v23_1.json`, `probe-v24_0.json`.

**2 — PARTIAL.** The original drift is caught: `selftest.sh` changed the isolated cached tree's `src/llmq/params.h` `max_store_depth()` by +1, and `build.sh` refused it. All six selftest checks passed, including refusal of my original round-1 drifted binary. Pin/commit/tree/source/binary checks and per-output provenance validation are real; final corpus publication follows successful comparisons. However, cache reuse can certify an archive built from changed source after that source has been restored. See the major finding below. Linked-artifact provenance remains unclosed.

**3 — CLOSED.** Both options and an owned `SporkSigners` result are present. Checked directly against both release tags with `git show`:

- v23.1.8 `src/init.cpp:1699–1714`, `src/spork.cpp:314–335`: decode every supplied address as P2PKH, deduplicate key IDs, then require `max/2 < min <= max`. Builder matches, including duplicate addresses, negation, wrapped integer thresholds, and address errors before threshold errors.
- v24.0.0-rc.3 `src/init.cpp:1746–1752`, `src/spork.cpp:219–230`: last address wins; warn when the resolved list has multiple entries. Builder matches these values and warning text. Removed `-minsporkkeys` is refused in the argument phase, ahead of chain errors; chain errors precede spork errors.
- Default signers/thresholds checked in `src/chainparams.cpp`: v23 lines 291–292, 484–485, 666–667, 866–867; v24 lines 302, 505, 697, 904. Dump startup block and setter logic match Core, with locks/logging omitted and errors/warnings captured as JSON.

Replayed all four round-1 spork witnesses at both pins, plus additional whitespace, empty-address, trailing-junk, and existing corpus cases. Corpus compares resolved key bytes, threshold, and successful warning text to Core. No mismatch. Evidence: `adapter-probe.log`, `probe-v23_1.json`, `probe-v24_0.json`, regenerated dumps.

**4 — CLOSED.** `pkgs/params/tests/brr_payments.rs` checks all 19 exported entries against `oracle_mn_payment` at both pins, with boundary samples and final-period saturation. Independently executed `mn_payment_cases` against hash-checked oracle archives; regenerated BRR fixture is byte-identical. Core table/formula checked at v23.1.8 `src/validation.cpp:1606–1631` and v24.0.0-rc.3 `src/masternode/payments.cpp:103–128`. Original mutation **513 → 514** now fails at **both pins**, height **1379128**, comparing 514000000 against Core's 513000000. Restored the table. Evidence: `mutation-brr.log`, `gen/mn.jsonl`.

**5 — CLOSED.** `pkgs/params/tests/core_corpus.rs:355–402` implements the relevant `LocaleIndependentAtoi<int>` / `InterpretBool` normalization. Checked both tags' `src/util/system.cpp` and `src/util/strencodings.h`. Re-ran `-nofastprune=garbage`, `=0x1`, `=0abc`: both Core pins and the adapter enable fast pruning, height 100. Corresponding `-novbparams` cases produce Core's malformed-versionbits error rather than clearing the option. Additional prefix, sign, and overflow witnesses pass. Evidence: `adapter-probe.log`, `probe-v23_1.json`, `probe-v24_0.json`.

## Remaining finding in changed code

**Major — cached linked archives can be relabeled as a clean pinned build.** Rust-dashcore `contrib/node/bs5-chainparams/build.sh:72`, `:89–91`, `:121`; `run.py:499–505`.

The `.bs5-built` marker skips rebuilding. `verify_tree` checks tracked source blobs, but neither it nor the marker authenticates existing linked archives against the artifacts from the original clean build. `build.sh` hashes whatever archives are currently present into a new output MANIFEST. `run.py` validates the binary against that newly issued MANIFEST, without an independently established archive provenance anchor.

Concrete reproduction, entirely in the reviewer's private copied build tree:

1. Start with a genuine v24 pinned build and `.bs5-built` marker.
2. Temporarily change regtest `nCreditPoolPeriodBlocks = 100` to `101` in `src/chainparams.cpp`, rebuild only `libbitcoin_common_a-chainparams.o`, and replace that member of `libbitcoin_common.a`.
3. Restore `chainparams.cpp` byte-for-byte to pinned source, leaving the modified archive cached.
4. Reviewed `build.sh v24.0 ...`: **exit 0**. `run.py --verify-dump v24.0 ...`: **exit 0, provenance ok**.
5. Dump has unchanged `params`, but `ext.creditPoolPeriodBlocks` changes **100 → 101**, while embedding expected pin and commit `23769258c239e69282569e9f9e219ce9e2c4ee2e`.
6. Complete generator with freshly executed pinned oracle outputs: **exit 0**, prints `all oracle cases equal the pinned oracle_chainparams_json`, and publishes the altered corpus.

This reproduces the material round-1 failure through the new cache path: extension consensus data can differ from pinned Core while the generator positively certifies provenance. It needs neither a forged MANIFEST nor changes to the generator. Rebuilding under temporary source changes followed by restoring source is sufficient.

**Suggested fix:** retain artifact hashes from the initial controlled clean build as a cache-integrity manifest, verify them before reuse, and refuse or rebuild a changed cache. Cover linked archives and relevant generated build inputs; do not regenerate the trusted baseline from arbitrary cached artifacts. Alternatively always rebuild in a fresh archive-derived tree. Add this source-restored/archive-drift scenario to `selftest.sh`.

Evidence under `/work/scratch/bs5-r2-gpt/`: `cache_drift.py`, `cache-object-build.log`, `cache-drift-build.log`, `cache-drift-summary.log`, `cache-regenerate.log`, `cache-dumps/`, `cache-gen/corpus/chainparams_v24_0.json5`. The private original archive was restored after producing evidence. This is the remaining portion of round-1 finding 2, not an unrelated scope expansion. No other new finding in changed code.

## Validation and limits

- `cargo test -p dash-params --all-features`: **98 tests passed** (40 unit, 2 BRR, 38 builder, 10 corpus/adapter, 8 genesis).
- `cargo clippy -p dash-params --all-features --all-targets -- -D warnings`: passed.
- Temporarily added **19 independent replay cases per pin** covering original overwrite, spork, and adapter reproducers plus further parsing/address cases: passed; restored fixtures.
- Recompiled both dump tools in isolated copies of the implementer's controlled Core build caches, checking tracked blobs through the reviewed script. Executed both oracle examples from fresh detached rdc against pinned hash-checked archives. Full regeneration: **178 cases per pin, 147 oracle-cross-checked per pin**, all equal. Chain-parameter corpus bodies are identical to committed bodies; only recorded dump binary hashes differ. BRR fixture is byte-identical.
- Original header-drift selftest: all six checks passed. Cached-archive experiment: build, provenance verification, and full generator all incorrectly accepted changed extension data.
- Used `TMPDIR=/dev/shm/bs5-r2-gpt-tmp` (mode 700), with build job limits of 8. No daemons started this round; existing round-1 daemon evidence corroborated by pinned source and fresh dump comparisons. RAM temporary directory removed after completion. Evidence remains under `/work/scratch/bs5-r2-gpt/`.

**Final verdict: CHANGES-REQUIRED.** Four items closed; item 2 partial because cached linked artifacts can still silently change extension consensus data while receiving a pinned provenance label.
