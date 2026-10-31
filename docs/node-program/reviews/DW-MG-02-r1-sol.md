# Desktop MG-02 governance facade — independent review, round 1

Reviewed `dw/mg-02-governance` at `c38de626` against `3198a0a7`, the MG-02 design and mg-engine contract, with Dash Core v24.0.0-rc.3 as the protocol reference. The reviewed worktree was not changed.

## Findings

### Major — Unbounded inbound message queues allow a connected peer to exhaust wallet memory

`rust/crates/dw-governance/src/peer.rs:195-211`, `rust/crates/dw-governance/src/sync.rs:1313`, `rust/crates/dw-p2p/src/session.rs:393-394,498`

`Session::subscribe` and the shared governance event channel are both unbounded. The session reader dispatches every subscribed frame without waiting for the governance loop; the forwarder likewise sends events without backpressure. A peer can stream large valid-framed `govobj` payloads (up to the 3 MiB frame cap) or 50,000-entry `inv` batches while the loop parses, hashes, validates, or recovers votes. The per-peer `announced` and misbehaviour limits execute only after dequeue, so they do not bound queued bytes. A 1,001-frame burst can occupy roughly 3 GiB of payload before the misbehaviour threshold closes a peer, and repeated `inv` batches can also accumulate. This is an unauthenticated memory exhaustion path in the new governance session.

**Fix:** use bounded inbound channels or a bounded per-peer byte budget with read-side backpressure; close a peer when it exceeds the budget. Include queued payloads in the bound, and test a producer faster than the governance consumer.

### Major — Two controlled peers can bypass every object-store cap and retain arbitrary fake proposals

`rust/crates/dw-governance/src/store.rs:459-469,475-509,512-529,553-574`

The 500-per-peer, 2,000-global, and 32 MiB checks count only **unconfirmed** objects. When a second address announces the same hash, `note_announcer` marks it confirmed and frees all three quotas; confirmed objects have no count or byte cap. The wallet cannot verify proposal collateral and accepts two peer announcements as proof. Two controlled peers can therefore submit batches of at most 500 syntactically valid proposals with arbitrary far-future `end_epoch`, confirm each batch, and repeat without paying Core's collateral fee. `prune` retains these confirmed proposals until after their chosen end epoch. The store and user-visible proposal list can grow without a finite bound.

**Fix:** bound total retained objects and bytes, including confirmed ones. Couple that bound to a safe eviction or resync policy; a per-peer unconfirmed quota alone does not constrain this path. Add a regression that confirms successive batches from two peers and checks the total cap.

### Major — A `notfound` reply can change an object retry into a vote request and silently lose the object

`rust/crates/dw-governance/src/sync.rs:973-989` (retry and phase completion at `:701-727,1231-1248`)

`on_not_found` matches only the hash and sender, then passes the peer-supplied `i.kind` to `retry` instead of the pending request's `r.kind`. Consider an object announced by both an attacker and an honest Core peer. If the attacker is asked first and replies `notfound` with the same hash but kind 18 (vote), the wallet asks the honest peer for a *vote* of that hash. Core's `ProcessGetData` returns `notfound` for that absent vote; the retry is finished without counting it as lost. Both peers' `ssc(10)` counts and object inv counts can be satisfied while the real object was never fetched, so the wallet reaches `Synced` with a missing proposal.

**Fix:** match `notfound` on both hash and pending kind, and use `r.kind` for any retry. Treat a mistyped `notfound` as invalid peer input. Extend `notfound_moves_to_the_next_announcer` with a wrong-kind reply from the first peer.

### Major — Unrelated vote relays can satisfy a proposal's `ssc(11)` count

`rust/crates/dw-governance/src/sync.rs:736-775,1010-1034,1054-1071,1175-1184,1394-1418`

While a peer has any vote sync outstanding, every vote `inv` from it increments the single `vote_announced` counter, with no check that the vote belongs to an asked proposal. Core can relay other proposals' votes on that connection while answering `govsync`; a vote already held by the wallet is counted but creates no pending fetch. For example, the peer answers `ssc(11, 1)` for proposal A and sends an `inv` for a known vote on B before A's `inv`. `votes_done` becomes true, `phase_requests_done` is true, and the wallet can publish `Synced` before receiving A's vote. If the peer disconnects then, `on_closed` sees no votes owed and does not reassign A's vote sync. This can leave the tally incomplete despite the `Synced` phase.

**Fix:** attribute known and fetched vote hashes to their parent before crediting a vote-sync count. Do not let an unrelated relay settle an asked proposal's count; test interleaved A/B relay and disconnect sequences.

### Major — `voting_masternodes` reports a shared key's latest vote as every masternode's vote

`rust/crates/dw-engine/src/governance/voting.rs:53-77`

The facade collapses current votes by voting key, chooses one latest vote for that key, then copies its outcome and time to every masternode with the key. Dash permits multiple masternodes to share a voting key, and MG-02's ranged tally explicitly handles that case. If A and B share a key and A voted Yes while B voted No later, both rows say No; if only A voted, B still says it voted. The contract promises each masternode's current vote, so this can mislead the wallet owner about which masternode still needs a vote.

**Fix:** report a per-masternode vote only when its collateral outpoint has been resolved and checked. Otherwise expose a key-level or ambiguous last-seen state in the contract and UI. Test two list entries sharing a key with distinct votes and with only one vote seen.

### Major — Budget-order uncertainty is missed when proposals move in opposite directions

`rust/crates/dw-engine/src/governance/reads.rs:314-335,342-367`

`build_table` computes the fundable set at each proposal's lower bound and then at each proposal's upper bound. It marks `close_call` only if either whole-set result differs from the estimate. That does not test mixed outcomes. For example, suppose the budget fits exactly one of two equal-cost proposals, A's estimated net yes is 24 with range 15–24, and B's is 23 with range 14–23; all bounds clear a threshold of 10. The estimate, all-low and all-high scenarios each fund A, so both rows get `close_call = false`. Yet A at 21 and B at 23 is within the observed-vote ranges (independent voting keys), and Core would fund B. Such ranges can arise with three votes on each proposal under a key held by three evonodes and one regular masternode; their actual total can vary from 9 to 12 while the current per-vote range reports 3 to 12. The contract and MG-00 D45 require a neutral close-call indication when the range can change budget order.

**Fix:** for each candidate, test its worst rank against competitors at their best (and vice versa), or otherwise compute whether any feasible allocation changes membership. Add a two-proposal test where all-low and all-high preserve the estimate but opposite endpoints swap the funded proposal.

## Validation and limits

- Read the changed sync, store, tally, validator, P2P, facade and contract paths; checked the relevant `govsync` and `getdata` behavior against Dash Core v24.0.0-rc.3 source.
- Tests ran in `/work/scratch/dw-mg02-r1-sol/source` with its own target directory. `cargo test -p dw-governance -p dw-p2p --quiet` passed (governance 72 unit tests plus its integration tests; P2P 13 unit tests). `dw-engine` passed 966 unit tests (12 ignored), and its integration targets passed, including all 9 `mg_governance_contract` tests. The first isolated run lacked repository-level vectors and contract docs; the tests that failed for missing files passed after those fixtures were copied into scratch.
- The findings above are code-path proofs; no attack harness was added to the reviewed branch. I did not independently repeat the earlier mutation checks or run the four-masternode regtest suite.

## Verdict

**CHANGES-REQUIRED**
