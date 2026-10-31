# SB4 design confirmation, round 5 — Sol

Reviewed **e3d2d9ad71e5ddf80844ae6df4db6e305acf4f60** against **36266b40**, specifically `docs/node-program/designs/SB4-dash-chain.md` and SB0 §17 E3. References below are to those pinned blobs. Read REVIEW-COMMON, Sol r4, binding DEC-119, 123, 127, 128, 132, 136 and 140, and the relevant CH2 contract. PLAN §7 handling was followed; no private inputs were read. The integration worktree advanced, but these two design files still matched the reviewed commit when checked.

Scope is **only R4-M1/E3 and R4-M2, including D42**. Settled selection, mark and notification decisions are not reopened.

**Result: REWORK. Sol gate satisfied: no.** Locator selection closes the original fully checkpointed wallet witness, and the continuous guard closes the B109 handoff race. Two gaps remain.

## Findings

### R5-M1 — E3 permits durable replacement effects with an old, still-active checkpoint

**Severity:** major. **Refs:** `docs/node-program/designs/SB0-shared-base.md:1945` (“with or after” effects), `:1946` (active first entry means no rewind); `docs/node-program/designs/SB4-dash-chain.md:1220` (closure map instead says effects and record share one transaction).

Atomic **height and locator** does not imply atomic **effects and checkpoint**. The closure map states the intended stronger requirement, but E3's operative ordering permits an earlier, separate effects commit. Its proof addresses power loss after step 3, leaving the cut between steps 2 and 3 uncovered.

Concrete cut, in r4's same lost-disabled-suffix scenario:

1. Durable SPV state and wallet checkpoint are A100; the record holds height 100 and `locator(A100)`. The SPV floor is A70 and enforcement is observed enabled.
2. The SPV store commits a Buffered suffix observing the spork off and publishes B110, forking at A50.
3. The wallet commits its reorg truncation and B-chain effects, including B60's payment. It has not committed `update_wallet_synced_checkpoint(110, locator(B110))`. E3's “after” alternative permits this.
4. Power loss restores SPV state A100, leaving durable B effects and the old A100 wallet checkpoint.
5. E3 finds A100 on the restored active chain and does no truncation. The stored height also equals the restored tip, so forward resume has no blocks to process. B60's wrong-branch effects remain.

No torn height/locator pair is needed. Core's separate best-block write does not establish safety for this SPV fast path: rc.3 also revalidates every loaded confirmed/conflicted transaction against active-chain membership in `CWallet::LoadToWallet` (`wallet/wallet.cpp:1151–1167`). E3 specifies no equivalent repair here.

**Suggested fix:** require durable effects, including reorg truncation, and their height/locator checkpoint to commit in the **same wallet-DB transaction**. Remove “or after” and reconcile the ordering pointers with the closure map. An alternative needs an explicit recovery protocol for effects not covered by a checkpoint whose first entry remains active. SB14 must test crash cuts before/after commit, including the reorg case. Use a *committed Buffered but non-Durable* header suffix in the fixture; §15.2's “never committed” cannot be literal because publication follows commit.

### R5-M2 — D42 omits pressure checks across an unbounded disconnect leg

**Severity:** major. **Refs:** `docs/node-program/designs/SB4-dash-chain.md:1127` (IF_NEEDED evaluated once after enforcement; claimed permission from CH2 `:818`); `docs/node-program/designs/CH2-coins-flush.md:810`, `:820`, `:857`, `:1488`, `:1499`, `:1736` (native checks after every disconnect and CRITICAL wait/release).

CH2 `:818` allows different trigger blocks because **native Σ differs from Core's usage**, while retaining call sites, modes and arithmetic. It does not authorize suppressing checks across arbitrary disconnect depth. D42 changes sampling even for the **same native counter**.

Concrete pressure scenario:

- F is already in flight when enforcement takes the tip mutex. W can be waiting to publish its durable barrier under the shared feed/hub lock, so F need not release during the hold.
- A long losing branch spent distinct coins that existed before its fork. Each disconnect restores additional coins into active. Take 2,000 blocks restoring 1,000 distinct coins each. Per-block limits do not bound their sum; THRESHOLD cannot freeze a second F while the first is in flight.
- With B = 8 MiB and a 1 MiB F, counting **only** 72-byte restored slots, native usage reaches CRITICAL after disconnect 102. CH2's declared policy waits for release there and then freezes. D42 continues and reaches at least **145,048,576 bytes** before its first check, over 17 times B. Indexes, long payloads, Evo deltas and staged events add more. Increasing disconnect depth increases omitted-check growth.

This does not claim that CH2 makes B a universal RSS invariant; CH2 expressly gates peaks as evidence. It demonstrates loss of the disconnect-boundary pressure control and single-block growth envelope. A deferred walk can exhaust memory where a boundary-checked walk has opportunities to flush/release. D42 correctly forbids the deadlocking wait under this mutex, but removing pressure opportunities does not resolve that side of the composition.

**Durability/crash outcome:** deferral removes intermediate freeze/B1 opportunities. A crash during enforcement can leave an earlier durable cut and require more replay. No new torn-store defect is established: the previous durable state and any earlier in-flight packet remain recoverable under CH2's unchanged packet/barrier protocol. Forced copy/shutdown flushes can still satisfy their promises. Those facts do not make the memory change unobservable or authorize skipping native/Evo pressure checks at disconnect boundaries. Core calls `FlushStateToDisk(IF_NEEDED)` inside each `DisconnectTip` under `cs_main`, before moving its tip (`validation.cpp:3303–3316`); `EnforceBlock` holds `cs_main` throughout (`:4053`, `:4096–4105`).

**Suggested fix:** preserve DEC-140's continuous tip/handoff exclusion while specifying a bounded pressure path. Make the necessary CH2 flush/release, hub and subscriber progress independent of the held TipCell lock, or provide a bounded spill mechanism, and prove intermediate-disconnect progress and recovery. If CH2 must change, record its amendment explicitly; `:818` is not existing permission. Add a deep coin-restoring enforcement test with F in flight, a blocked hub operation, and native/Evo pressure crossing before the final disconnect. Check peak usage and completion. Latching pressure without a way to relieve it is insufficient.

## Confirmations within scope

- **Core locator mechanism: confirmed.** `CWallet::chainStateFlushed` → `WalletBatch::WriteBestBlock` stores the locator (`wallet/wallet.cpp:624–633`, `wallet/walletdb.cpp:187–196`), fed by `ChainStateFlushed(m_chain.GetLocator())` (`validation.cpp:3111–3114`). Scan progress also saves a locator (`wallet/wallet.cpp:2054–2061`). `AttachChain` uses `findLocatorFork` (`:3526–3537`) → `FindForkInGlobalIndex` (`node/interfaces.cpp:1418–1425`, `validation.cpp:182–197`): first active-chain entry, otherwise genesis. A known stale entry does not count. The established SPV anchor is the appropriate substituted terminus. Core scans starting at the common block (`wallet/wallet.cpp:3604`); E3 keeps that block's effects and scans its successor, safe with corrected effects/checkpoint atomicity.
- **37, not 50: confirmed.** Executing rc.3's exact `LocatorEntries` body (`chain.cpp:32–49`) gives `B110…B100, B99, B97, B93, B85, B69, A37, A0`. The single step after B100 is intentional: doubling happens after selecting its ancestor. The first restored-active entry is A37, even with all B entries known as stale. Truncating above 37 drops B60 and all B effects above the fork. The original witness with a *committed B110 checkpoint* is closed.
- **Size and seam: confirmed.** E3 keeps the locator in the host-persisted per-wallet key-wallet sync record and adds `wallet_synced_locator` and `update_wallet_synced_checkpoint` with defaults. The bound `11 + ceil(log2 h) + 1` is conservative for positive h; h = 0 has one entry. Actual Core-shaped lengths are 33 at height 2,551,766 and 43 at `u32::MAX`. Anchor clipping cannot enlarge the list. The record's height and locator pair are atomic; R5-M1 concerns effects.
- **Guard and A-39: confirmed.** SB4 `:746–757` holds one owned guard through staged disconnects, final snapshot store, target capture and handoff, including error return. `subscribe_at`/`finish()`, `deregister` and `publish_status` take the same mutex. The delayed B109 request waits until tip 80 and returns `Moved` without `finish()`. Pre-enforcement subscribers get all staged sequences; post-enforcement subscribers start after them. A-39 specifies these cases and reentrant `publish_status` detection. D42's ban on reentrant TipCell APIs and waits on threads that may take the lock is necessary and correctly stated. A debug owner check diagnoses direct reentry; it does not prove cross-thread wait freedom. The flush deferral remains R5-M2.

## Validation and limits

- Exact inputs and artifacts are under `/work/scratch/sb4-design-r5-sol/`. Core tag: **da567d7dba945143eee4d2ce1621c21b32089989** (`v24.0.0-rc.3`); all Core citations use its source blobs.
- Built/ran `core-locator.cpp` with `c++ -std=c++17 -O2 -Wall -Wextra -Werror`. `LocatorEntries` and `FindForkInGlobalIndex` bodies are extracted verbatim; only ancestry/hash/index/lock scaffolding is substituted. The 37 witness, stale-known, unknown/empty and active-first-entry controls, and **10,000** additional size/endpoint cases passed. This is an isolated exact-function probe, not full Core wallet execution.
- Ran `spec_checks.py`: anchor/size controls; real SQLite transactions reopened at the effects-before-checkpoint cut reproducing surviving B60; atomic before/after-commit controls recovering without B effects; a threaded A-39 guard model; and the pressure-counter witness. These are specification checks, not execution of an SB4/SB5/CH2 implementation. Pressure counts slots only, not measured full-node RSS.
- Ran `coins_tests` in `dash-wt/pr7841/src/test/test_dash`: **9 cases, 3,036,368 assertions passed**. Its `chain.cpp`, `coins.cpp` and `test/coins_tests.cpp` match rc.3 byte-for-byte. The existing newer executable is supplemental smoke evidence, not a D42 integration verdict.
- Tests used a mode-700 TMPDIR under `/dev/shm`, removed afterwards. No reviewed source, live chain instance, private input, GitHub message, commit or push was involved. This review file is left uncommitted.

## Verdict

**REWORK. Sol gate satisfied: no.** Require effects/checkpoint atomicity (R5-M1) and a bounded, deadlock-free CH2 pressure path under the continuous guard (R5-M2). Locator recovery at 37 and the original handoff exclusion are confirmed.
