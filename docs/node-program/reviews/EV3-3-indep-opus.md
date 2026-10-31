# EV3-3 (MNHF manager): independent consensus review (Opus)

| | |
|---|---|
| Reviewed | `node/ev3-mnhf` 475ee240a, only the EV3-3 delta: `git diff 8e9ae9934 475ee240a` (9ad34dfad, a4d4a37f1, 475ee240a). 895 of the 909 files are public seeds; I sampled them and checked the count. |
| Against | Dash Core `v24.0.0-rc.3`: `evo/mnhftx.{h,cpp}` (`mh`, `mhh`), `evo/specialtxman.cpp` (`stm`: `CheckSpecialTxInner`, `ProcessSpecialTxsInBlock`, `UndoSpecialTxsInBlock`), `evo/specialtx.h` (`GetTxPayload`), `chainparams.cpp:109-130` (`IsValidMNActivation`), `unordered_lru_cache.h`, `versionbits.cpp:237-252` (`SignalHeight`), `validation.cpp:351-380` (`ContextualCheckTransaction`) and `:6797-6803` (`GetSpecialTxRules`), `rpc/blockchain.cpp:1713-1739` (`getdeploymentinfo`) |
| Context | EV3-notes (branch copy) §1.4, §2, §4.4, D15–D21, A-2; `node-fuzz/public-seeds/PROVENANCE.md` `diff_mnhf_step` |
| Method | Read line by line against Core. Detached worktree at 475ee240a in /work/scratch/ev3-3-review, with a target reflink-seeded from the implementer's, deleted afterwards. Tests: `dash-dml --lib mnhf` 9/9, `mnhf_step_vectors` 2/2, `dash-consensus` all green, `node-l1 --features oracle --test l1_mnhf` 3/3 against the in-process v24.0 oracle (seeds: 782 accept, 108 reject, 10 out of domain, as the notes say). cargo-mutants 27.1.0 on `dash-dml/src/mnhf.rs`, plus hand mutants run against the dash-dml suite and `l1_mnhf`. |
| Verdict | **ACCEPT WITH NITS** (no B or M items) |

The implementer's validator items are all in place, and I did not re-review them: the read-back,
the fallible stage read (D21), the bit ≥ 29 check before the duplicate check, the seeds, ops and
`Decode` handling, the lock scope around the store read, the V20 accessor, and the LRU (no stamp on
a miss, `select_nth_unstable_by`).

## 1. Branch-for-branch parity

Everything below matches rc.3.

- **`IsValidMNActivation`** (`mnhf.rs:41-56` vs `chainparams.cpp:109-130`):
  - It walks `vDeployments` in order, and a deployment whose window does not hold the time is
    skipped (`timePast > nTimeout || timePast < nStartTime`, both bounds inclusive).
  - The first deployment on the bit whose window holds the time returns its `useEHF`. A bit with
    no such deployment is valid.
  - The `assert` is kept as a panic.
  - Start and timeout are raw `i64`, so `NEVER_ACTIVE`/`ALWAYS_ACTIVE` compare as Core's -2/-1
    do. Mainnet's bit 13 (V24 `NEVER_ACTIVE`, `useEHF`) is in window and valid in both.
- **`extractEHFSignal` / `extractSignals`** (`:60-90` vs `mh:176-208`):
  - Special version and `TRANSACTION_MNHF_SIGNAL`, then a payload that decodes completely
    (`GetTxPayload`'s `ds.empty()`, `specialtx.h:18-28`). The payload version is not checked, as
    in Core.
  - `vtx[1..]`, and `bad-mnhf-duplicates-in-block` on the first repeat, in block order.
  - L1's signature variants show the BLS bytes decode the same on both sides
    (`CBLSSignatureVersionWrapper(…, false)`, `mhh:30`): a repeated bit with `0x02…` signature
    bytes is rejected on both sides.
- **`ProcessBlock`** (`process_signals` `:103-139` and `process_block` `:322-344` vs
  `mh:210-262`), in order:
  1. Extraction.
  2. `GetSignalsStage(pprev)`. It is read after extraction, so a duplicate in the block never
     reads the store. The unit test checks this.
  3. Empty `new_signals`: the stage, cached unless `fJustCheck`.
  4. Per bit, in block order, `bad-mnhf-duplicate` before `bad-mnhf-non-mn-fork`, at the block's
     own MTP (`HeaderCtx::mtp` is `GetMedianTimePast()` of the block).
  5. `fJustCheck`: the stage, with no cache insert and no row.
  6. The new bits inserted at the block's height, then `AddToCache`.

  The `bad-mnhf-nbit-out-of-bounds` check first (`:111-115`) is the port's stand-in for
  `IsTriviallyValid` (`mh:99-101`), which Core reaches through `CheckSpecialTxInner` on every
  transaction before `ProcessBlock` (`stm:853-874` before `:973`). With EV1's `check_mnhf_tx` run
  first (A-2), it cannot change a verdict. See N1 for its test coverage.
- **`GetSignalsStage`** (`ehf.rs:125-162` vs `mh:61-92`):
  - `DeploymentActiveAfter(prev, V20)`, then a null `prev` gives empty, then `GetForBlock(prev)`.
    Core calls `GetForBlock(nullptr)` before its null check, but that call has no effect.
  - The expiry loop covers every deployment on the bit with no `break` (`signal_time <
    nStartTime`), at the MTP of the signal's ancestor; a missing ancestor panics like Core's
    `assert`.
  - Core reads `Params()` here and `m_consensus_params` elsewhere. These are one object for a
    node, and the port uses `self.params` for both.
- **`GetFromCache` / `GetForBlock`** (`:288-314` vs `mh:285-362`):
  - Cache hit; else the empty map, inserted, where V20 is not active at the block; else the
    `mnhf_s2` row, inserted; else a miss. The lock is dropped around the store read.
  - The port holds one lock across the hit test and the V20 insert, where Core takes two
    back-to-back locks with no read between them. That is strictly more atomic, with the same
    result.
  - On a miss, Core walks back and re-runs `ProcessBlock` from disk (`mh:289-319`). The port
    raises a store fault instead (D17). The legacy `mnhf_s` read before MN_RR (`mh:353-359`) is
    left to the importer (D17). That is fine for blocks the node connected; N4 covers the blocks it
    did not.
- **`AddToCache`** (`:349-352` vs `mh:364-383`): the row goes out where V20 is active at the block;
  the store's `write_derived` refuses a different row (`store/mem.rs:73-87`), as Core's
  `WriteDerived` does, as a fault. The cache insert happens in all cases. For the order, see N3.
- **`UndoBlock`** (`:365-383` vs `mh:264-283`):
  - An extraction failure gives `Ok(false)`, and no signals gives `Ok(true)` without a read.
  - Otherwise `GetForBlock(pindex)`, with the stamp and insert of a normal read, and Core's
    `assert` becomes `StoreError::Inconsistent` (D15).
  - Core's `UndoBlock` erases neither the row nor the cache entry. `MemEvoStore::disconnect`
    erases nothing (`mem.rs:174-185`), and the port's cache is untouched. The state after a
    disconnect is therefore exactly Core's.
- **The LRU** (`:185-222` vs `unordered_lru_cache.h:39-117`):
  - A hit stamps and a miss does not; every insert stamps, including an overwrite.
  - Above 2 × 1000 entries, it keeps the 1000 entries with the highest stamps. Core's
    `nth_element(begin, begin+maxSize, …, >)` keeps `vec[0..maxSize)`; the port takes the
    `CACHE_SIZE-1`-th highest stamp and keeps everything at or above it. Stamps are unique, so the
    two sets are equal.
  - The stamp sequence of one `process_block` is Core's: one stamp for `prev`'s read and one for
    `AddToCache`. The cache entries, including the seeded tip entry (D16, O2a-MNHF D13), are
    ported faithfully.

## 2. Error classes (D21)

- `MnhfError::Invalid` arises only from Core's three reasons plus the `IsTriviallyValid` stand-in.
  Each one is `BlockValidationResult::Consensus` with an empty debug message; L1 checks both.
- Store reads (`mnhf_signals`) propagate as `MnhfError::Store` through `ehf_signals_stage_from`.
  A missing parent row is `Store` for `fJustCheck` both true and false (unit
  `a_missing_row_is_a_store_fault`).
- A store fault never becomes a reject reason: `reject_reason()` is `None` for `Store`, and L1
  treats any store fault as a divergence. A consensus reject never becomes a fault: no path maps
  `Invalid` to `Store`.
- Only the versionbits path panics, by CS4's contract (D20, `:389-393`).
- Core has a third class that is not ported: `catch (const std::exception&)`, which gives
  `failed-proc-mnhf-inblock` (`mh:257-260`) and `failed-procspectxsinblock` (`stm:993-995`). See
  N2.

## 3. The differential

### 3.1 Domain

L1's domain covers the following:

- V20 in five placements: from 2, and from tip−1 through tip+2. At tip+2, V20 is not active at the
  block but is active after it, so `stage_after` reads the cache entry that `process_block` itself
  made.
- Windows at m, m±1, m−600, m−601, m−3000 and m+600, which hit both MTP edges of
  `IsValidMNActivation` and the expiry edge for row signals 0–8 blocks back.
- `useEHF` both ways, rows with held bits, the seeded pre-V20 tip entry, and `fJustCheck`.
- Each payload form that carries no signal.

The 10 out-of-domain seeds are the 10 placements × flags of `[27, v20, flags, 0, 29, 0]`, a full
signal at bit 29. Core's `ProcessBlock` alone asserts on it (`IsValidMNActivation`), so the oracle
cannot answer, and excluding it hides no real difference. The filter is wider than it needs to be,
though (N1).

"Several deployments on one bit" (§4.4) is not in the domain (N5).

### 3.2 Reject branches

- Each reject is reached by a named crafted case in `crafted_steps_agree`:
  - `bad-mnhf-duplicates-in-block`: `[…0,13,0,0,13,2]`;
  - `bad-mnhf-duplicate`: a held bit and the seeded tip entry;
  - `bad-mnhf-non-mn-fork`: bit 28.
- The order of the duplicate and non-EHF checks is pinned by the held non-EHF bit case.
- `nbit-out-of-bounds` is unit-only, because it is out of the oracle's domain.

### 3.3 Mutants

**cargo-mutants** on `dash-dml/src/mnhf.rs`, with `--lib --test mnhf_step_vectors`, in place on the
scratch worktree (restored afterwards). Of 66 mutants, **57 were caught, 3 missed, 6 unviable and
0 timed out.** None of the misses matters:

- `ehf_signal` `||`→`&&` (`:61`) is equivalent: the payload match that follows still requires an
  `MnhfSignal` payload, and a non-special version has no payload.
- `MnhfError`'s `Display` → `Ok` (`:167`) is cosmetic.
- `seed_cache` → `()` (`:358`) is the harness-only hook. `l1_mnhf`'s seeded cases catch it
  (by inspection, `[27,3,0,4,13,0,0,13,0]` would turn from a reject into an accept; not run), but dash-dml's own suites do
  not.

**Hand mutants**, each run against the dash-dml suite and `l1_mnhf`:

| Mutant | dash-dml | `l1_mnhf` |
|---|---|---|
| H1: the pre-check reads the raw payload's bit byte for any MNHF-typed transaction, not `ehf_signal` | **survived** | **survived** (N1) |
| H2: `fJustCheck` also caches the block's entry | caught (`tests.rs:260`, `cached() == 1`) | survived (the oracle does not expose the cache after a check) |
| H3: `AddToCache` without its cache insert | caught | caught (`store fault` on `[27,0,0]`) |

Mutants caught by reading alone:

- MTP of `prev` in place of the block's: caught by the m−600/m−601 windows.
- `<=` for the expiry comparison: caught by L0's "re-signal at the window start".
- The stage read moved before extraction: caught by the unit test.
- An empty row: the notes already show both read-backs catch it.

A plausible `process_block` mutant that changes a verdict does not survive the combined suites.

## 4. Undo and reorg

- Core's `UndoBlock` is a pure check. Rows are keyed by hash and kept, and the cache is neither
  erased nor restamped beyond the one read.
- The port matches on every count. `l1_mnhf` checks `undo_ok` and `stage_undone` (trivially equal
  to `stage_before` on both sides), and the mined-chain walk connects the invalidated branch on
  its fork point with the rows kept.
- A reconnect recomputes the same row, and `write_derived` faults on any difference, as Core's
  `WriteDerived` aborts.
- One exception: possible security note, see manager.

## 5. Findings

No Blocker or Major findings.

### Minor

None.

### Nits

- **N1. The out-of-domain filter is wider than Core's assert, which leaves the port's own
  pre-check untested where it must stay silent.**
  - `node-l1/src/mnhf.rs:654-656` drops any input whose transactions name a bit ≥ 29. That
    includes `Trailing`, `OtherType` and `TxVersion{1,2,-1}`, which `extractEHFSignal` never reads
    (`mh:176-188`), so Core does not assert on them.
  - As a result, `process_signals`' pre-check (`dash-dml/src/mnhf.rs:111-115`), which the port
    added, is never compared on blocks where Core accepts. H1, the pre-check on raw payload bytes,
    survives everything.
  - No verdict changes in the node, because EV1 rejects those transactions first
    (`bad-mnhf-payload`, `bad-txns-type`).
  - Fix: filter only kinds `ehf_signal` reads (`Signal`, `PayloadVersion`, `TxVersion{4}`), and add
    a unit case: `process_signals` on a block with a trailing-byte payload at bit 30 is `Ok`.
- **N2. D21 does not record that Core's `std::exception` arms are not ported.**
  - `mh:257-260` (`failed-proc-mnhf-inblock`) and `stm:993-995` (`failed-procspectxsinblock`)
    look like missing reject reasons. On a consistent store, though, they are reached only through
    `GetForBlock`'s rebuild (a missing row, D17) or a database read error, and the port maps both
    to `StoreError`, which is right.
  - Add a sentence to D21 saying so, and cite the lines.
  - Possible security note: see manager.
- **N3. The cache is written before the row.**
  - Core calls `WriteDerived` before the cache insert, and a mismatch aborts before the cache is
    touched (`mh:368-382`).
  - The port inserts into the cache at once (`:350`), and the store refuses the row only when the
    delta is applied. After that fault, the cache holds the computed entry over the corrupt stored
    row.
  - This is harmless only if the node stops on an MNHF store fault, as Core's `AbortNode` does.
    State that in A-2(b).
- **N4. D17's invariant covers only blocks the node connected, and an RPC can reach the others.**
  - `getdeploymentinfo <blockhash>` takes any indexed block (`rpc/blockchain.cpp:1726-1733`) and
    calls `GetSignalsStage` on it. Its `DeploymentInfo` reaches `GetForBlock` again through
    versionbits' `SignalHeight` (`versionbits.cpp:238-252`).
  - For a side-branch block that was never connected, Core rebuilds from disk, or returns an RPC
    error when the data is missing. The node has no row for it: the fallible path gives
    `StoreError`, and the versionbits path panics (`:389-393`), which crashes the node from an
    authenticated RPC.
  - This is not a consensus path. Every consensus read of EHF state is on a connected parent:
    `GetSpecialTxRules(pindex->pprev)` in `ConnectBlock`, the mempool's tip, and `UpdateTip`'s
    version warning. Header and contextual checks read no EHF deployment.
  - Fix: narrow D17's sentence to "blocks the node connected", and add an ask to the
    `getdeploymentinfo` owner to error out before touching versionbits for a block without a
    row, or to rebuild.
- **N5. §4.4 promises L1 covers "several deployments on one bit", and it does not.**
  - `case_from_bytes` cannot move a bit (`DEPLOYMENTS`, `node-l1/src/mnhf.rs:47`), and no L0
    crafted vector shares a bit.
  - So `GetSignalsStage`'s no-`break` loop (`mh:77-86`) over two deployments on one bit has no
    test with a Core answer. `activation_by_window_and_use_ehf` covers the
    `IsValidMNActivation` side, port-only.
  - The case cannot happen at rc.3: no network shares a bit, and `-vbparams` cannot change one.
    Fix the text, not the code.
- **N6. A-2(b) does not say how to map `undo_block`'s `Ok(false)`.** Core's
  `UndoSpecialTxsInBlock` returns false, and the disconnect fails (`stm:1015-1016`). It cannot
  happen for a block the node connected, but the wiring must treat it as a failed disconnect and
  not drop it.
- **N7 (forward).** `EhfSignals::insert` and `FromIterator` panic on a bit ≥ 29 (`ehf.rs:41-47`).
  The persisted store's row decoder, and the evodb importer (D17), must reject such a row as a
  `StoreError` before building an `EhfSignals`. Core never writes one, but a corrupt row must not
  panic the node.
