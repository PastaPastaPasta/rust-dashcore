# EV1-5a review, round 1

Verdict: **ACCEPT-WITH-NITS**

The landed `check_cbtx` and `cbtx_payload` logic matches Dash Core v24.0.0-rc.4 for all ordinary consensus inputs I traced. I found no production consensus divergence. There is one test-level mismatch in the claimed Core reject reason and one stale design-vector wording issue.

## Findings

### m-1 — edited-version fixture cases do not exercise the Core caller’s reject string

**Location:** `dash-dml/src/specialtx/cbtx/tests.rs:274-305`, especially `:296-304`.

The test parses a real v2 (height 900000) or v3 (905101/1569000) wire payload, clones the decoded `CoinbaseCommitment`, changes only `other.version`, and calls `check_cbtx` directly. That correctly tests `check_cbtx` as a pure function, but it does not prove that Core rejects the corresponding edited transaction with `bad-cbtx-version`. If the raw 900000 v2 payload’s version byte is changed to 3, Core’s `CCbTx::Serialize` (`v24.0.0-rc.4:src/evo/cbtx.h:54-67`) attempts to read the v3 ChainLock/balance fields and `GetTxPayload` fails first, yielding `bad-cbtx-payload` (`specialtx.h:17-35`). Conversely, changing the raw v3 payload’s version byte to 2 leaves the v3 fields trailing; `ds.empty()` is false and Core again yields `bad-cbtx-payload` before `CheckCbTx`.

**Concrete failing input:** the 900000 fixture with its CbTx version wire byte changed from 2 to 3 (or either v3 fixture changed from 3 to 2).

**Fix:** keep a direct `check_cbtx` test for the version boundary, but label it as such; add a caller-level test that constructs a complete canonical payload for the edited version (or assert the actual raw-edit result is `bad-cbtx-payload`). The edited-height case can be tested through the raw payload because its serialized shape is unchanged and Core reaches `bad-cbtx-height`.

### n-1 — D18’s pre-DIP0003 vector wording is still wrong for the pinned regtest

**Location:** `docs/node-program/designs/EV1-specialtx.md:877` (the pre-DIP3 D18 vector referenced by `docs/node-program/designs/EV1-notes.md:11`).

At rc.4 regtest, `DIP0008Height = 1` (`chainparams.cpp:822`). For a block at height 2 or later, `pindexPrev` is already at/above height 1, so `DeploymentActiveAt(*pindexPrev, DIP0008)` is true and a v1 CbTx is rejected as `bad-cbtx-version` (`cbtx.cpp:35-38`). The implementation and new tests correctly encode this: v1 passes only at block 1 (or if `-testactivationheight=dip0008@N` moves DIP0008 above the block), while pre-DIP3 blocks from height 2 through 431 accept only v2. Core also asserts `V20Height >= DIP0003Height` after `-testactivationheight` and `-dip3params` (`chainparams.cpp:879-880, 963-964`), so v3/v4 cannot pass below DIP3 on the pinned regtest.

**Concrete failing input:** pinned-default regtest block 2 with a height-2 v1 CbTx; Core returns `bad-cbtx-version`, contrary to “valid v1 or v2 … accepted.”

**Fix:** amend D18’s vector to use v2 for the pre-DIP3 regtest case, or use v1 specifically at block 1 / move DIP0008 above DIP3. The EV1-5a note already identifies this correction; the source design row should be brought into line.

## Parity checks

- `CheckCbTx` was compared line-for-line with `v24.0.0-rc.4:src/evo/cbtx.cpp:24-55`. The order and reason are exact: version 0 or >=5 -> `bad-cbtx-version`; with non-null `pindexPrev`, height mismatch -> `bad-cbtx-height`; DIP0008 at `pindexPrev` requires v2; V20 active **after** `pindexPrev` requires v3 and otherwise refuses >=3; V24∧V20 requires v4 and otherwise refuses >=4. Every return is `TX_CONSENSUS`, with no Core DoS score and no debug text. A null `pindexPrev` returns after the version-range check, exactly as Core.
- Activation heights and boundary behavior agree with rc.4: mainnet DIP3 1,028,160, DIP8 1,088,640, V20 1,987,776; testnet DIP3 7,000, DIP8 78,800, V20 905,100. Regtest has DIP8 1 and V20 = DIP3 = 432, with the V20>=DIP3 assertion. V24 is a versionbits deployment rather than a fixed buried height; the caller supplies its `GetSpecialTxRules(pindexPrev).v24` result. The Rust `active_at(prev, DIP0008)` and `active_after(prev, V20)` calls use the same block positions.
- `cbtx_payload` follows both Core callers (`specialtxman.cpp:247-255` and `:876-892`): coinbase check first (`bad-cbtx-invalid`), then complete payload/type check (`bad-cbtx-payload`), then `CheckCbTx`. Trailing bytes, short bytes, empty bytes, and a different decoded payload type are rejected as payload; a non-coinbase takes the first reason. The block caller maps the result to `BLOCK_CONSENSUS`; the dispatcher keeps `TX_CONSENSUS`. The type-5 arm has no deterministic uncaught exception: `GetTxPayload` catches decode exceptions and `CheckCbTx` only performs scalar comparisons, so dropping `failed-check-special-tx` from this table and lowering the floor 10->9 is justified (resource/OOM exceptions remain the general outer catch, as in Core).
- The three testnet raw transactions exactly match the corresponding entries in `dash-evo/tests/data/cbtx_testnet.json` (`core: /Dash Core:24.0.0/`, network testnet) at heights 900000 (v2), 905101 (v3), and 1569000 (v3), including the complete coinbase hex. Their decoded heights/versions and accepted statuses are valid. The raw-height edit would reach `bad-cbtx-height`; the version caveat is m-1 above.
- `dash-dml` remains Pn and already depends on P/S-layer crates including `dash-evo`; `dash-evo` only gains constants. `node-l1` is T. No new forbidden edge was introduced. The full layer checker passed.

## Mutant/coverage review

The branch notes report 16 env-gated mutants, all killed by the named unit tests; I did not rerun that campaign. The cheap remaining gap is the caller-level raw version mutation described in m-1; it can survive while every pure `check_cbtx` mutant remains killed. The public `cbtx_payload` contract intentionally assumes the caller has already selected raw type 5, matching both Core call sites; no separate tx-type check is required inside it.

## Checks run

- Read-only diff: `git -C /work/home/agent/workspace/rdc-wt/ev1-5a diff a64e41abe 337285e7a` and `git diff --check`.
- `cargo test -p dash-dml --all-features specialtx::cbtx --no-fail-fast` in reflink copy `/work/scratch/ev1-5a-r1-sol` (16 passed).
- `cargo test -p node-l1 --features oracle --test l1_dml_block check_tables_are_consistent --no-fail-fast` (passed).
- `cargo test -p node-l1 --features oracle --test l1_dml_block crafted_blocks_match_the_checked_path --no-fail-fast` (passed).
- `cargo fmt --all -- --check` (passed).
- `python3 contrib/node/check-layers.py` (L1/L2/L3/L8 passed; only the pre-existing baselined dash-coreimport edge reported).
- Verified the three fixture lines byte-for-byte against the Core-produced JSON provenance and inspected rc.4 `cbtx.cpp`, `cbtx.h`, `specialtx.h`, `specialtxman.cpp`, `deploymentstatus.h`, and `chainparams.cpp`.
