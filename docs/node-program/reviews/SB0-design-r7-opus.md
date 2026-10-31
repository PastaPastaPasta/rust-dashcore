# SB0 narrow confirmation, round 7 — Opus (DEC-112 approval path)

Reviewed `docs/node-program/designs/SB0-shared-base.md` at **rev6, f03c86a0** (identical at integration HEAD dbe0fd36). Below, `SB0:N` means that file's line N. I compared it with the text my r6 review assessed (`reviews/SB0-design-r6-opus.md`, rev4 at 53091a32, which is 7d40936c plus the DEC-109 erratum) through rev5 (1f75dbb6) and rev6. Also read: `briefs/REVIEW-COMMON.md`, DEC-112, DEC-113, DEC-115, DEC-116 and `reviews/SB0-design-r7-sol.md`.

Core source checked at v24.0.0-rc.3: `chainlock/chainlock.cpp` (`UpdateBestChainlock`, `AcceptedBlockHeader`), `chainlock/handler.cpp` (`EnforceBestChainLock`), and `validation.cpp` (`EnforceBlock`, `MarkConflictingBlock`).

This is a design-only confirmation. No code changed, so I ran no tests. DEC-104/105/107/109/112/113/115/116 are binding, and nothing here reopens them.

> PLAN §7: this review says nothing about current SPV validation coverage beyond "PLAN §7 handling (CANDIDATE-SPV)".

---

## 1. rev5 against O1–O6 and the minors (DEC-112)

Every item is present and matches DEC-112's ruling. None is missing or wrong.

| Item | rev5/rev6 text | Verdict |
|---|---|---|
| **O1** | `best_header` follows Core's update sites (`SB0:394`, `:430-444`): strict-greater for `BLOCK_VALID_MASK`, `CONFLICT` headers don't move it, walk-back and `InvalidChainFound` hooks at V's sites, `EnforceBlock`'s marking, and the load recompute including conflicting entries. The node's readers are named (`:444`). `fork_target` is a separate SPV-only value (`:395`, `:446-455`) and `None` in the node. SB4 carries both of my node vectors (b1005 kept; `CONFLICT` plus restart). CH1-node owns the hooks (`:1503`) | Implemented |
| **O2** | SB12a `dash-spv::bridge` (`:1578`), with `Header::from_parts_hashed` debug-asserted and a DEC-51a note (`:135`, `:1063`), dashcore adapters for bip158 and sml, CV3 starting from the bridge (`:1595`), and the CPM rerun (`:1671-1682`) | Implemented |
| **O3** | Lists are retained while `recent`, `Unverified`, a pending fetch or the tip list needs them. Quorum rows are kept back to tip − R, with R = max(H_keep, 2·W_P) (`:944-955`). The lookup predicate, W_P's derivation and table (`:964-1000`), Q8 to Platform's owner (`:1776`), and gates in SB11/SB15/SB17 | Implemented as DEC-112 bounded it (on-demand fetch instead of my "keep rows indefinitely") |
| **O4** | No wallet locator (`:1339`). The resume point is min(wallet, durable tip), there is an additive synced-hash method, a check at open, and the ordering rule (`:1361-1386`). SB13 has independent-database crash points and SB14 has the wallet cases | Implemented |
| **O5** | `pool.install` everywhere, so no global pool. blst `no-threads` sits only in dash-spv-ffi's manifest, with `check-mobile.sh` keeping it out of every N graph (this respects DEC-79). `spv-clippy.toml`, explicit `worker_threads`/`max_blocking_threads`, and dashcore's LLMQ features dropped after SB15 (`:1180-1206`, L5 `:170`). The thread test runs in SB13 (old MN path), SB14, SB15, SB16 and SB17 | Implemented ("every path", per DEC-112) |
| **O6** | SB18 uses key-wallet at M5. SB18b (`dashd-wallet`, M6, deps SB18 and WL2a) is added (`:1585-1586`, `:1613`, Q4 `:1772`) | Implemented |
| m1–m10, n1–n5 | All present at the places §16.1 lists. I spot-checked m1 `:689`, m2 `:694`, m3 `:610`/`:627`, m4 `:1416`, m5 `:378-380`/`:1314`, m6 `:1316`, m7 `:749`, m9 `:401`, m10/n1 `§4.1.1`, n2 `:512`, n3 `:1287` and n5 `:1770` | Implemented |

S1, S2 and S3's single connection were not taken. They were simplicity suggestions, not must-fix items, so that choice is acceptable.

---

## 2. Do rev6's additions contradict O1–O6?

### 2.1 O1's separation: one must-fix (scoping)

`fork_target`'s eligibility walk (a) and floor descent (b) are SPV-only. The node never computes them (`SB0:455`), and `best_header` is untouched (`:1806`). That keeps O1's separation.

**R7-O1 (must-fix, one sentence). `set_active_tip`'s floor refusal is written on the shared tip writer without being scoped to SPV.**

- **Where:** `SB0:452`: "`set_active_tip` refuses a target that does not descend from `cl_floor`, with a debug assertion and a `ChainError`."
- **Why it reaches the node:**
  - `set_active_tip` is shared `dash-chain` code.
  - It is "the only active-tip mutator, and in the node it runs only on V" (P2, `SB0:1160`; `:407`).
  - `cl_floor` is Core's `bestChainLockBlockIndex` (`:399`) in both products.
  - The refusal sentence is in the SPV fork-target paragraph, but nothing restricts it to SPV, so an SB4 implementer would reasonably put it in the shared function.
- **Why that breaks the node.** In Core, the active tip routinely does not descend from `bestChainLockBlockIndex`:
  1. **Header known, block missing.** `AcceptedBlockHeader` sets the floor from a header alone (`v24 chainlock/chainlock.cpp:150-172`). Core's comment there says enforcement "might end up invalidating other chains but not activating the CLSIG locked chain. This happens when only the header is known but the block is still missing". After `ActivateBestChain`, the tip stays at the fork point or below the floor until the block arrives (`chainlock/handler.cpp:296-309`).
  2. **The enforcement reorg itself.** The floor is updated before `EnforceBlock`, and `MarkConflictingBlock` then calls `DisconnectTip` repeatedly (`v24 validation.cpp:4089-4108`). Each intermediate tip is on the old branch or at the fork point, so none descends from the floor. If V reflects those disconnects through `set_active_tip`, the first one is refused.
  3. More generally, the node connects blocks below a known, header-only floor. Every such tip is an ancestor of the floor, not a descendant.
- **Consequence.** A node built to the text fails or debug-asserts on an ordinary ChainLock-driven reorg or a late block. That is a Core-exactness break, and possibly a wedge on V, in the shared code that O1 was meant to keep free of SPV fork-choice rules.
- **Fix.** Scope the sentence: "In SPV (where `fork_target` is `Some`), `set_active_tip` refuses …; the node's V never refuses on this ground, since Core's tip may lag or sit below the floor." Give SB4 one node control next to the existing SPV property test: a ChainLock on a header-only block that conflicts with the active tip. The node disconnects to the fork point and keeps that tip until the block arrives, with no `ChainError`, and its `getbestblockhash` matches the oracle.

**Other checks in this area, all consistent:**
- The SPV side is sound. The floor is an accepted, eligible header, so a target always exists. Sol r7's forward-progress argument holds.
- R6-M1's (a)/(b) leave the m2 eviction rule (`:694`) and SB5's cold-record walk unchanged.

### 2.2 O4's wallet-checkpoint authority: consistent, plus one non-blocking edge

R6-M2 keeps the wallet authoritative and the SPV store locator-free. It changes only what happens when the wallet's hash is unknown. The min resume point and the ordering rule stand, and after `truncate_above(H)` the wallet's own height becomes the resume point. Under SB0's commit-before-publish and retained-prefix contracts, the argument that H is on both chains (`:1373-1377`) holds, and the stated ChainLock-safety assumption is acceptable. This does not contradict O4.

**R7-m1 (minor, not must-fix; for the SB14 brief).** The "caps only guard the arithmetic" claim (`:1376`) assumes the store was continuous with the wallet.
- **Scenario.** A wallet attached to a store it did not follow. Examples: a wallet restored from a backup and added to a running client, or a second wallet whose checkpoint came from another device.
- **What goes wrong.** That wallet can hold an unknown hash at a height h_w at or below the durable floor f (an orphan tip it saw elsewhere). The rule gives H = min(f, h_w, tip) = h_w, and `truncate_above(h_w)` is a no-op, so the orphan block's effects are kept.
- **Fix.** An unknown hash at or below f contradicts the continuity premise, so take the anchor branch (or the wallet's birth height) instead of capping at h_w.
- **Impact.** Rare, and it costs only a wrong confirmation height or a stale tx. It does not affect the R6-M2 power-loss case DEC-115 ruled on, and it needs no design change beyond one sentence.

### 2.3 O3's retention R = max(H_keep, 2·W_P): consistent

The two-target fetch (R6-M3/DEC-116) applies only when no row decides the lookup. Rows, the LRU and fetches answer one predicate (`:973`).

For lookups in the Platform window, h ≥ tip − W_P:
- A pruned row was replaced before tip − R ≤ tip − 2·W_P ≤ h − W_P.
- So the predicate's answer is not-found anyway, and a served type's fetch returns the same.
- R therefore still covers the window, and the fetch only extends reach to older h.

For non-served types, not-found for older h is DEC-116's allowed difference. The lifetime table's arithmetic matches Sol r7's independent check. My original O3 gate ("rotated out ≥ 1 day, after restart, no network") is correctly recast in SB15 as found with network and not-found without (`:1582`), per DEC-112's on-demand fetch.

---

## 3. Blockers

None. R7-O1 is a one-sentence scoping fix plus a control vector. It does not make implementation impossible as long as it is fixed before SB4's brief is written. It must not reach SB4 unscoped.

---

## Verdict

**APPROVE-WITH-CHANGES.** One must-fix:

1. **R7-O1:** scope `set_active_tip`'s `cl_floor` refusal (`SB0:452`) to SPV, state that the node's V never refuses on that ground, and add SB4's node control (a header-only ChainLock where the tip stays at the fork point with no error, matching the oracle).

Optional, for the SB14 brief: R7-m1 (an unknown wallet hash at or below the floor takes the anchor branch).

O1–O6 and all the minors are confirmed as DEC-112 ruled. rev6's additions don't contradict O1's separation (once R7-O1 is scoped), O4's authority or O3's R. A manager-applied edit for R7-O1 is enough; no further review round is needed.
