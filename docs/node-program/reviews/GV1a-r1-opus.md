# GV1a review (r1, Opus)

**Revision reviewed:** `node/gv1a` at `6c9a71ca7` (diff `ac41eae465..6c9a71ca7`)
**Core reference:** `v24.0.0-rc.3` (`git -C ~/workspace/dash archive v24.0.0-rc.3`); all Core citations below are to that tag.
**Scope:** `dash-governance/`, `node-l1/src/gov.rs`, `node-l1/src/lib.rs`, `node-l1/tests/l1_gov.rs`, `node-fuzz/public-seeds/diff_gov*/` (skimmed), `docs/node-program/designs/GV1a-notes.md`. The four findings in `GV1a-r1-sol.md` are not repeated here.

## Verdict

**ACCEPT WITH NITS**

**Finding counts:** Blocker 0, Major 0, Minor 7, nit 1.

I found no parity defect in the crate. Every rule I traced line by line matches Core: the hash serializations, trigger parsing, arithmetic, vote handling, best-superblock choice and rate buffer (see "Parity checked" below). All seven Minor findings are about **test strength**. I ran 22 hand mutants, each gated by an environment variable in a scratch export of `6c9a71ca7`. Eleven survived both `cargo test -p dash-governance` and `l1_gov`, and none of the eleven is equivalent to Core: each changes Core's answer on inputs that can occur. Several of them sit on the path to consensus: `IsSuperblockTriggered`, `GetBestSuperblock` and `IsValidSuperblock` decide block validity at superblock heights (`masternode/payments.cpp:368-380, 425-453`). The cause is mostly the fixture. Its regtest chain collapses several of Core's branches into one value: a cycle of 20 is below both 24 and 576, `max(1, 12/10) = 1`, and every collateral has 15 confirmations. The fuzz generators reach some of these cases, but the committed regression tests (L0 plus the `l1_gov` census of public seeds and 400 random inputs) do not.

## Findings

### 1. Minor — `UpdateSentinelVariables`' thresholds are only tested where `max(nGovernanceMinQuorum, n/10)` collapses to 1

**Location:** `dash-governance/src/tally.rs:319-324`; tests `dash-governance/tests/tally.rs` (`sentinel_thresholds`), L1 lists in `node-l1/src/gov.rs` (regtest fixture only).
**Core:** `governance/object.cpp:970-976`; the result feeds `IsSuperblockTriggered` (`governance/superblock.cpp:609-611`), which decides block validity (`masternode/payments.cpp:368, 425, 451`).

Every L0 and L1 tally runs on the fixture list: 12 valid weighted, with regtest's `nGovernanceMinQuorum = 1`. There `nAbsVoteReq = max(1, 12/10) = 1`, so neither term of the `max` is pinned. Three mutants survive both suites:
- M10: `vote_req = min_quorum` (the `n/10` term dropped);
- M11: `vote_req = weighted / 10` (the quorum floor dropped);
- M12: `dirty = false` set before the `weighted == 0` early return. Core returns first and leaves `fDirtyCache` set (`object.cpp:971`).

On mainnet (quorum 10, thousands of weighted masternodes) M10 would fund a trigger at 10 net yes votes instead of about 10 % of the network.

**Fix:** In `tests/tally.rs`, run `sentinel_thresholds` a second time with mainnet `GovParams`. On the 12-weight list the quorum floor of 10 then decides, which kills M11. Add a list of at least 20 weight with quorum 1 so that `n/10 > quorum` (kills M10). Duplicating fixture entries under new collaterals through `dash_dml`'s list builder would do. Add an empty list and assert that `dirty` stays `true` and no flag moves (kills M12). Assert the deletion threshold `(2n)/3` at a weight where `2*(n/3)` differs, for example 14 (9 against 8; at 12 and 13 both give 8).

### 2. Minor — trigger lifetimes are only tested where the Executed/Valid/Unknown horizons coincide

**Location:** `dash-governance/src/superblocks.rs:90-93` (`add_trigger`), `:120` (`clean`); `dash-governance/src/trigger.rs:145`.
**Core:** `governance/superblock.cpp:345-357` (horizons `nSuperblockCycle`, `min(576, cycle)`, `min(24, cycle)`), `:461-464` (`SetStatus(Valid)` before `IsExpired`), `:500-506`.

Regtest's cycle of 20 is below 24, so every horizon is 20. Testnet and devnet (24) also collapse Valid and Unknown into one value. Only mainnet tells the three apart. `trigger.rs`'s unit test `expiry` checks `is_expired` on mainnet directly, but no test checks which status `AddTrigger` and `Clean` apply. M31 survives both suites: `add_trigger` judging expiry with `Status::Unknown` (horizon 24) instead of `Valid` (576). On mainnet that mutant would return `false` from `AddTrigger` for a trigger 25–576 blocks old, and Core's `AddGovernanceObject` would then `PrepareDeletion` it (`governance.cpp:348-352`).

**Fix:** Add a `SuperblockSet` test on mainnet params, using a synthetic trigger object with a mainnet address (`parse` needs no signature). Call `add_trigger` at `event + 25` and `event + 576` (live) and at `event + 577` (expired). After `execute_best`, assert that `clean` keeps the trigger up to `event + cycle` and drops it after.

### 3. Minor — the off-mainnet "too far into the future" expiry is only tested on testnet

**Location:** `dash-governance/src/trigger.rs:152`.
**Core:** `governance/superblock.cpp:366-371` (`NetworkIDString() != CBaseChainParams::MAIN`, so regtest and devnet as well as testnet).

M2 survives both suites. It replaces `!p.is_mainnet()` with `p.is_testnet()`, which drops the rule on regtest and devnet. The unit test `expiry` checks only testnet and mainnet. The L1 parsed-trigger record compares both expiries on the regtest context, but the generated event heights never exceed the tip (179) plus `2 * 20`.

**Fix:** In `trigger.rs`'s `expiry` test, loop the "far future" assertions over regtest and devnet as well as testnet, with `cycle * 2` taken from each network's params. Add a `diff_gov` public seed whose trigger has `event_block_height = 179 + 41` (and one at `+40`).

### 4. Minor — the height gates of `GetBestSuperblock` and `IsSuperblockTriggered` are untested

**Location:** `dash-governance/src/superblocks.rs:154` and `:186`.
**Core:** `governance/superblock.cpp:560-562`, `:581-583`. `AddTrigger` does not check the event height (`:436-465`), so a trigger for a height that is not a superblock height is held.

M21 drops the gate in `best` and M22 drops it in `is_triggered`, and both survive both suites. The script heights include 179 and 181, but no generated or seeded trigger has an off-cycle event height that is funded. Core's payment checks call these only once `check_superblock` says the height may be a superblock (`masternode/payments.cpp:362-368, 422-425`). So whether a missing gate reaches block validity depends on whether every EV3 and GV1b caller repeats that check. `AtTip`'s `GovernanceView` methods (`superblocks.rs:252-264`) rely on the crate's own gate.

**Fix:** Add an L0 test that holds a re-encoded fixture trigger with `event_block_height` 181, funds it, and asserts `best(.., 181)` is `None`, `is_triggered(.., 181)` is `false` (with the state's `dirty` untouched) and `payments(.., 181)` is `None`. Add a `diff_gov_vote` seed with the same object followed by `BLOCK 181`.

### 5. Minor — `IsSuperblockTriggered`'s early return is not pinned

**Location:** `dash-governance/src/superblocks.rs:189-201`.
**Core:** `governance/superblock.cpp:589-617`. Iteration stops at the first funded trigger in map order, so later triggers for the same height do not get `UpdateSentinelVariables`: their `fDirtyCache` and `fCachedFunding` stay as they were and no deletion mark is added.

M1 survives both suites. It updates every trigger for the height and then returns whether any was funded. The L0 `superblocks.rs` tests hold one trigger. The L1 scripts can hold several triggers for one height, but they compare `STATE` before the final `BLOCK`, and the random mid-script `BLOCK` ops did not separate the two behaviours.

**Fix:** In `tests/superblocks.rs`, hold two triggers for height 180 (re-timed copies, as `best_superblock_ties_go_to_the_first_hash` already does). Fund the first in map order and give the second `dirty = true` plus delete votes at the deletion threshold. After `is_triggered`, assert that the second state's `dirty` is still `true` and `cached_delete` is `false`. Add a `diff_gov_vote` seed with `STATE` ops after a mid-script `BLOCK 180`.

### 6. Minor — the collateral confirmation window 1–5 and the unmined branch are never exercised

**Location:** `dash-governance/src/object.rs:288-290`, `:317-331`; harness `node-l1/src/gov.rs:531` (`CollateralMode::Active => self.tip - c.height + 1`) and the postponement branch at `:1053`.
**Core:** `governance/object.cpp:815-819` ("is not mined yet"), `:853-873` (`nConfirmationsIn < 6`, `fMissingConfirmations = nConfirmationsIn >= 1`); postponement in `governance.cpp` `ProcessObject` / `CheckPostponedObjects`.

All four fixture collaterals were mined at height 165 with the tip at 179, so they have 15 confirmations when active and 0 when inactive. Nothing builds `FoundCollateral { mined: false }`. As a result `missing_confirmations` is only ever compared as `false`, the "pre-accepted -- waiting for required confirmations" text is never compared, and the harness's postponed path is dead. M7 survives both suites: `missing_confirmations = confirmations > 1` instead of `>= 1`. GV1b's postponement depends on exactly this flag. D16 lists "unmined/short of confirmations", but the harness cannot produce either.

**Fix:** Add a unit test in `object.rs` (or L0) of `collateral_valid` over a fixture collateral transaction with `mined: false` and with `confirmations` 0, 1, 5 and 6, asserting Core's full `strError` texts and the flag. In the harness, add a collateral mode that answers with a short-confirmation context, for example a tip at `c.height + k`, if the oracle's `GovContext` can express it. If it can't, record the limit in D16.

### 7. Minor — boundary cases the generators target are absent from the committed tests

**Location:** `dash-governance/src/vote.rs:153-157`; `dash-governance/src/trigger.rs:96`; `node-l1/tests/l1_gov.rs:58-80` (the census: public seeds plus 400 splitmix inputs).
**Core:** `governance/vote.cpp:213-218` (`Time() > GetAdjustedTime() + 1h`, so `+3600` is valid and `+3601` is not); `governance/superblock.cpp:95` with `univalue/include/univalue.h:136-147` (`getInt<int>`: a value above `INT_MAX` throws "JSON integer out of range").

M5 (`vote.time < now + 3600`) and M16 (`event_block_height` read with `getInt<int64_t>` and truncated) survive both suites. The vote generator aims at the bound (`node-l1/src/gov.rs:670`, `now + 3600 - 2 ..= +2`), so the certified farm corpus probably covers it. But that corpus is not in the repo, and no public seed or unit test lands on `+3600` or `+3601` with an otherwise valid vote. Nothing feeds a trigger an out-of-`int` height either.

**Fix:** Add unit tests asserting `passes_static_checks` at `now + 3600` (`true`) and `now + 3601` (`false`), and asserting that `Superblock::parse` on `"event_block_height":2147483648` returns `Err("JSON integer out of range")`. Add the boundary votes, signed with a fixture key, to `gov_vote_seed_inputs()` so the census replays them against Core.

### 8. nit — placeholder in `MAX_TIME_AHEAD`'s doc

**Location:** `dash-governance/src/vote.rs:147` reads `` `GOVERNANCE_...` +1 h ``.
**Core:** `governance/vote.cpp:213` uses a literal `1h`; there is no named constant.

**Fix:** Write "the `+ 1h` in `IsValid`" (or similar) and drop the `GOVERNANCE_...` placeholder.

## Parity checked (no finding)

- **Hashes, byte for byte.** `GovObject::hash` = `hashParent ‖ revision(i32) ‖ time(i64) ‖ CompactSize+HexStr(vchData) ‖ outpoint(hash,n) ‖ 0x00 ‖ 0xffffffff ‖ CompactSize+vchSig` (`governance/common.cpp:23-39`). `signature_hash` = the network serialization without `vchSig` (`common.h:70-84`, `object.cpp:551-554`). `data_hash` = SHA256d of the serialized hex string (`object.cpp:543-549`). Vote hash = `outpoint ‖ 0x00 ‖ 0xffffffff ‖ parent ‖ signal ‖ outcome ‖ time` (`vote.cpp:145-156`). Vote signature hash = `outpoint ‖ parent ‖ outcome ‖ signal ‖ time` (`vote.h:183-192`). `GetSignatureString` (`vote.h:166-172`) uses signed `int` text, so a wire value of `0xffffffff` prints `-1` on both sides. All of these were checked against base-sdk `pkgs/primitives/src/gov.rs` at the pin `569f4adf`.
- **Arithmetic.** `GetPaymentsLimit` = `IsValidBlockHeight` gate, `fV20Active = height >= V20Height`, `GetSuperblockSubsidyInner(nBits, height - 1)` × cycle (`superblock.cpp:145-163`, `validation.cpp` `GetBlockSubsidyHelper`). EV3's port matches it, including the strict `nPrevHeight > nBudgetPaymentsStartBlock` and the V20 divisor 5/10. The per-network cycle, start, quorum and spacing in `dash-params` match `chainparams.cpp:168-175, 377-384, 560-567, 801-808`. `GetPaymentsTotalAmount` cannot overflow under the data size, and its wrapping sum matches the release build. `ParsePaymentAmount` and `ParseFixedPoint` match `superblock.cpp:22-64` and `util/strencodings.cpp:336-442`.
- **Trigger parsing.** The UniValue reader matches `univalue_read.cpp:54-460` token by token (the `strncmp` keywords, the numbers' look-ahead past `end`, the `\u` bound, the partial root after `clear()`). Duplicate keys resolve to the first (`findKey`, `univalue.cpp:156-166`). Key order and JSON whitespace (4 characters) match. `getInt<int>` matches `from_chars` over the whole token. Base58 decoding skips leading and trailing `IsSpace` (`base58.cpp`), so `" X…"` is a valid trigger address, as D12 says. The Platform-HRP prefix check matches `key_io.cpp:49`. `SplitString` keeps empty pieces. `ParseHashStr` matches `core_read.cpp:161-168`. `GetHexStrData` matches `superblock.cpp:389-413`, including `"type":2` without a space.
- **Best superblock.** `std::map<uint256>` order (`superblock.h:179`), the strict `>` update and `nYesCount > 0` (`superblock.cpp:556-576`). `IsSuperblockTriggered` uses the first funded trigger in map order. `Clean`'s ErrorInvalid / expired / `PrepareDeletion` sequence matches `:473-527`.
- **Votes.** `ProcessVote`'s order (known vote, masternode, signal none/unsupported, obsolete, same-time outcome tie, rate check, then signature) and its messages and penalties (`object.cpp:375-481`). `last_update` changes only with rate checks on. The `RemoveOldVotes`, `RebuildIndex` and `RemoveInvalidVotes` re-hash (`votedb.cpp:32-133`, `object.cpp:499-526`). `IsValid`'s static gates and the testnet-only hash signing (`vote.cpp:158-250`). Core has no rule tying vote time to object creation time, and the port adds none.
- **Rate buffer.** The ring, the `0`/`INT64_MAX` seeds of max/min and `GetRate` (`governance.h:66-153`). `MasternodeRateCheck`'s too-old and too-new bounds, the bypass and `2 * 1.1 / cycle_seconds` (`governance.cpp:726-788`). The crate takes the adjusted clock and the wall clock separately (`now` vs `adjusted_now`), as Core's mix of `GetTime()` and `GetAdjustedTime()` requires.
- **Proposal validator.** It matches `object.cpp:31-301, 999-1033`, including `ParseHex`'s whitespace-between-pairs rule (`strencodings.cpp:81-97`) and the assigned, not appended, "data exceeds" text.

No security concern in Core's own behaviour came up.

## Mutants

These are environment-gated (`GVMUT=n`) in a `git archive 6c9a71ca7` export under `/work/scratch/gv1a-opus-r1/src`; the worktree was not touched. "L0" is `cargo test -p dash-governance`; "L1" is `cargo test -p node-l1 --features oracle --test l1_gov`.

| M | mutation | L0 | L1 |
|---|---|---|---|
| 0 | none (baseline) | pass | pass |
| 1 | `is_triggered` updates every trigger, no early return | **survives** | **survives** |
| 2 | far-future expiry on testnet only | **survives** | **survives** |
| 3 | signature check before the rate check | killed | survives |
| 4 | `last_update = now` even without rate checks | survives | killed |
| 5 | vote `+1 h` bound exclusive | **survives** | **survives** |
| 6 | collateral fee `>` instead of `>=` | survives | killed |
| 7 | `fMissingConfirmations` at `> 1` | **survives** | **survives** |
| 8 | `GetHexStrData` `"type": 2` | survives | killed |
| 9 | rate bypass ignores `fForce` | killed | survives |
| 10 | `nAbsVoteReq = quorum` | **survives** | **survives** |
| 11 | `nAbsVoteReq = n/10` | **survives** | **survives** |
| 12 | `dirty` cleared on an empty list | **survives** | **survives** |
| 13 | base58 leading spaces not skipped | killed | survives |
| 14 | unknown parent: funding vote's voting-key branch dropped | survives | killed |
| 15 | `Clean` marks invalid triggers expired | killed | survives |
| 16 | event height via `i64` truncated to `i32` | **survives** | **survives** |
| 19 | regtest ECDSA over the hash | killed | killed |
| 21 | `best` without the height gate | **survives** | **survives** |
| 22 | `is_triggered` without the height gate | **survives** | **survives** |
| 27 | name not lower-cased | killed | killed |
| 31 | `AddTrigger` judges expiry as Unknown | **survives** | **survives** |
| 32 | Valid horizon = cycle | killed | survives |

## Validation

All builds and tests used `CARGO_TARGET_DIR=/work/scratch/gv1a-opus-r1/target` with `-j 8`. No existing `gv1a*` target could seed it, so it was built from scratch.

- `cargo clippy -p dash-governance --all-targets -- -D warnings`: passed (export of `6c9a71ca7`).
- `cargo test -p dash-governance`: passed (29 unit, 1 proposal-vector, 2 regtest replay, 3 superblock, 7 tally).
- `cargo test -p node-l1 --features oracle --test l1_gov`: builds with the existing oracle setup and passes (4 tests, 26 s).

**Note for the manager:** during this review the GV1a worktree moved from `6c9a71ca7` to `bd26949e4`, which adds two Sol fixes (`9f9cf6db9` bounding the vote signature count, `bd26949e4` making the rate buffer's ring private). It also has uncommitted edits to `node-l1/src/gov.rs` (the four-network selector for Sol #2). So the results above come from a `git archive` export of `6c9a71ca7`, not from the live worktree. The later commits only touch `rate.rs` and `vote.rs` and do not affect these findings, but M5 and nit 8 should be re-read against the new `vote.rs`.

Nothing was edited, committed or pushed in the worktree.
