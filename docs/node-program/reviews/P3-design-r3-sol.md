# P3 block-download design, Sol round 3

Target: `node/p3-design` at `7f7d1c21b` (rev4), against Dash Core `v24.0.0-rc.3`. I read the common reviewer contract, rev3 findings, DEC-229/230, the rev4 delta and the cited Core and CH1-node paths. D12 is outside this review. The P3 behavior is a design, so the existing P2P tests cannot exercise its new queue or ordering rule.

## Findings

### F1 — Major — snapshot-before-drain admits a compact announcement from a later chain state

Design: `P3-block-download.md:1192-1226`, `:1699`; Core: `net_processing.cpp:2224-2267`, `:6401-6551`, `validation.cpp:4831-4832`. The stamped prefix constrains `HeadersRelay`, but it does not constrain `NewPoWValidBlock`. Its per-peer half is applied by the head drain even when its push occurred **after** snapshot S.

Concrete interleaving: tip A is active and a high-bandwidth peer knows the fork parent and competing branch B, but not A. An earlier `UpdatedBlockTip(A)` has left A in this peer's `headers_relay`; its stamp is covered by S. `send_messages` snapshots A. Before its head drain, V reorganizes to B and accepts C extending B, pushing `NewPoWValidBlock(C)`. The drain uses its newer chain view (the design's one snapshot per drain, `:1151-1153`) and sends `cmpctblock(C)` to the peer that knows B. Section 7 then uses the old S: A looks active, its stamped entry is eligible, and the one-header HB path sends `cmpctblock(A)` after C. It also replaces `best_header_sent = C` with A. With the same one-way reorg, Core cannot produce that order: a section 7 before the reorg sends A before the callback sends C; a section 7 after C's callback sees A off the active chain and takes its inv fallback. Core's `cs_main` excludes the mixed view. The proof at `:1205-1217` establishes only “S includes B ⇒ N was pushed before S”; it does not handle “N pushed after S but drained before section 7.”

Fix: make snapshot S and the queue cutoff one ordered observation, then apply before section 7 only the signals present at that cutoff; later signals stay queued. Preserve the rule that any N whose block is already visible in S is included. An equivalent common ordering lock is fine if it meets CH1's lock rules. Prove the reorg crossing and the RPC/P2P crossings against Core, and test them. S4-x currently requests an event “between the drain and the snapshot,” which is impossible under its new snapshot-then-drain order; its matrix needs a distinct S → N → H case and a pending old-branch announcement.

### F2 — Major — the extra `HeadersRelay` bound does not cover all callback origins

Design: `P3-block-download.md:613-622`, `:1563-1570`; Core: `validation.cpp:4055-4072`, `:4140-4142` and `:3716-3722`. The `+11 HeadersRelay entries` argument counts one `UpdatedBlockTip` per outer ABC iteration and the callbacks limited at v:3760. `EnforceBlock` can walk multiple competing siblings and call `MarkConflictingBlock` for each; that path emits `UpdatedBlockTip` at v:4142 without an intervening `LimitValidationInterfaceQueue`. CH1-node likewise calls `updated_block_tip` from `locks.rs:462` on each mark (`dash-chain/src/chain/enforce.rs:201-280` walks the siblings). With the net actor stalled, the signals thread can transfer more than 11 of those callbacks into `SyncSignal`, and its push is expressly nonwaiting. The stated hard queue and resident-memory maximum is therefore false for that reorg origin.

Fix: account for all tip-callback origins in the maximum and in §5.1/A-9, or design a bounded, order-preserving handling rule that respects DEC-229's no-wait-in-hold constraint. Add an S1 case with many competing marks while the actor is stalled. The §5.1 budget also currently omits even the proposed `+11` term.

## Dispositions

- **Previous F1 remains open.** FIFO N-before-U, C6's publication-before-dispatch order, peer stamping at callback time, and the `view_seq` prefix close the rev3 `UpdatedBlockTip`-before-N case. A peer becoming fully connected after the callback is correctly absent from U's recipient list, matching Core's default `ForEachNode(FullyConnectedOnly)`. The new S → N → H crossing above is a different failure. C6 is implemented by CH1's `publish_view` then signals splice (`abc.rs:703-708`), but is still an A-1 request rather than text in CH1-node's signal contract; land that contract before S4.
- **Previous F2 closes at design level.** Both legs now use PMU `instructions:u` over A − B, including `NewPoWValidBlock` on both sides. E6's direct `ProcessNewBlock` pass with PeerManager unregistered provides the Core baseline. Callgrind is attribution only; the real-V merkle-pass gate remains. The Core comparison is explicitly PENDING until E1/E6 exist, and S6 cannot be READY while it is PENDING.
- **DEC-230 remains covered.** The always-on PreStart assertion and reachability test make the discard's prerequisites explicit. D34 adds no queue wait; F2 concerns the claimed maximum, not a new wait edge.

Validation: source-level schedule review against the cited Core tag and CH1-node code. I attempted `cargo test -p dashd-p2p --lib processing::` on the reviewed worktree, then stopped it during dependency compilation after about four minutes; no tests ran. No new P3 implementation test exists yet.

VERDICT: ACCEPT-WITH-CHANGES
