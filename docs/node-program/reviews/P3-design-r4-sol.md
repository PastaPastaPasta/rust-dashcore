# P3 block-download design, Sol round 4

Target: `node/p3-design` at `34cca01c3` (rev5), against Dash Core `v24.0.0-rc.3`. I read the common reviewer contract, my round-3 review, D37/D38 and the rev5 delta. This is a source-level design review: the stamped queue and `RelayList` are not implemented yet, so there is no P3 test to run. D12 is outside this review.

## Findings

### F1 — Major — merging across publication stamps can change section 7's wire result

Design: `P3-block-download.md:503-507,595-604,1224-1257`, `P3-notes.md:45`; Core: `net_processing.cpp:2292-2305,6407-6470,6522-6547`. D38's argument that a merged entry can be applied late as two late Core callbacks overlooks that **Core chooses recipients when each callback actually runs**. Rev5 fixes the recipients at each original callback, then delays the older append to the newer callback's stamp. Those two choices cannot always be represented by one Core scheduler order.

Concrete schedule: Q is a connected `sendheaders` peer that knows A's parent, but not A. V has published A and queued `UpdatedBlockTip(A)`. While V is in the next ABC hold connecting B on A, it pushes `BlockChecked(B)` with stamp `s+1` before publishing. The scheduler runs the older A callback now, while peer P is not fully connected, and queues U(A) *behind* that ineligible `BlockChecked`. P completes `VERACK`; the actor's post-message drain cannot pass `BlockChecked(B)` with snapshot `s`. The actor then takes section 7 snapshot S at `s`. V publishes B, and the scheduler runs U(B) before the head drain. U(B) merges with U(A) and gives both the newer stamp. Section 7 at S announces nothing to Q. At the next snapshot, P has learned A's parent but not A or B; it receives only U(B), so section 7 falls back to `inv(B)`. Q has U(A), U(B).

Core cannot produce both observations with that order of `VERACK` and section 7. If U(A) runs before Q's section 7, Q sends A then. If it runs after that section, P is already fully connected and Core's `ForEachNode` gives P both A and B. For P, `[A,B]` connects and sends `headers(A,B)`; `[B]` alone does not connect and falls back to `inv(B)`. An HB preference can also alter the compact versus inv path. This is a wire difference, not merely a delayed announcement. The ineligible `BlockChecked` ahead of U(A) is allowed by the stamps: the older queued callback reads `s` while V has pushed, but not published, `s+1`.

Fix: preserve the older callback's eligibility and recipient boundary until it is applied, or provide another bounded representation that proves the same per-peer schedule. Add an S4 case with an ineligible synchronous head, two adjacent `HeadersRelay` callbacks spanning a publication, and a peer completing `VERACK` between them; compare both peers' section 7 frames with Core. The current S4-x late-full-connection case has only one N and does not exercise this merge.

### F2 — Major — the claimed per-entry memory bound omits recipient churn

Design: `P3-block-download.md:493-502,638-653,1736-1746`, `P3-notes.md:45`; current actor: `dashd-p2p/src/processing/peerman.rs:273-279,349-362`. A coalesced `HeadersRelay` holds a `HashMap<PeerId, RelayList>`. The `P` in `8 × P` bounds peers fully connected **at one instant**, not the number of distinct peer IDs captured across all callbacks merged into one entry. With an ineligible head as above, the actor can keep processing handshakes and finalizations while the scheduler consumes an older backlog of `UpdatedBlockTip` callbacks. Each callback can add a newly connected peer ID to the same tail map; finalized peers' IDs remain there. With enough churn the map holds arbitrarily many keys even if concurrent connections never exceed `-maxconnections`. The stated `≈32 KB` per entry and the §2.5/§5.1/A-9 hard maximum do not follow.

Fix: retire a finalized peer's pending map entry (it can never receive the append), or bound the number of distinct recipient IDs across a merge by another lossless rule. Include peer connect/finalize churn while a prefix is ineligible in S1's peak-memory test. Account for the `HashMap` and the new `NewPoWValidBlock.peers` allocation in the byte budget as well.

## Dispositions

- **Round-3 F1:** D37 fixes the original S → N → H stale-snapshot crossing for `NewPoWValidBlock`. The V-local `view_seq + 1` stamp, head snapshot S, and C7(b) prevent the N entry from being applied with an older S or parking V. In the reviewed CH1 code, `abc.rs:703` publishes before `abc.rs:731`'s wait; `failure.rs:149` is before a hold whose preceding iteration calls `after_release`, and class M scopes publish before returning. Import accepts remain an explicit A-1 obligation, and the S4 entry gate correctly requires C6/C7 in CH1's signal contract. F1 above is a separate D38 interaction that defeats the broader order proof.
- **Round-3 F2:** For a fixed recipient set and a fixed callback eligibility boundary, `RelayList` is exact: lengths 0–8 retain every hash; at 9 or more Core sets `fRevertToInv` before the loop and consults only `.back()`. No deduplication is correct, including duplicates and reorg-mixed lists. Adjacent-entry coalescing bounds the **number** of relay entries by the intervening synchronous entries. F2 above is the missing bound on recipients *within* an entry.
- **Other delta:** Capturing `NewPoWValidBlock` recipients on V's thread matches Core's `ForEachNode(FullyConnectedOnly)` moment. The cutoff adds no wait edge to DEC-229/230. The S1 10,000-mark case and the S4 raw-vector proptest cover fixed recipients and list compression, but neither covers F1/F2's peer lifecycle schedule.

VERDICT: CHANGES-REQUIRED
