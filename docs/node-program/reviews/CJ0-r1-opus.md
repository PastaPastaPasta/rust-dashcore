# CJ0 dash-coinjoin: Opus r1 code review (second reviewer)

Range `6993f2f2f..adc4cac2d` (node/cj0-coinjoin), worktree ~/workspace/rdc-wt/cj0-coinjoin. Core reference: v24.0.0-rc.3 `0364ed626e`. I read the files from that commit with `git show`, not from the checkout. Read-only; nothing was committed or pushed. I did not look into D15, as the brief says.

## Verdict: ACCEPT WITH NITS

I compared every rule and state transition in `server.rs`, `client.rs`, `queue.rs`, `inouts.rs`, `collateral.rs`, `dstx.rs`, `relay.rs`, `rounds.rs`, `denom.rs` and `pool.rs` line by line against Core and found no behavioural divergence beyond D7–D12 and D16. I found nothing that needs changing before merge. All findings below are Low or Nit. Most are hand-off notes for CJ1, WL6 and O2a-CJ, or doc fixes.

What I ran:
- `cargo test -p dash-coinjoin` at `adc4cac2d`: 88 passed, 0 failed (unit 0, core_common 9, core_ports 46, relay_rules 12, sessions 21).
- `contrib/node/cj0-vectors/regen.sh` in a scratch copy of the repo, against `~/workspace/dash` and the pinned oracle dashd: all three data files came out **byte-identical** to the committed ones.
- `e610a33e64` (the vector pin) against `0364ed626e` (the rc.3 tag): no difference in `src/coinjoin/`, `wallet/coinjoin.cpp`, `masternode/meta.cpp`, `primitives/transaction.h` or `script/script.cpp`. In `net_processing.cpp` the only difference is the oracle hooks. Every `// Core:` line number I checked matches rc.3.

## Findings

**1. Low: CJ1 hand-off. Core sends a status reply before it decodes `dsa`, `dsi` and `dss`; the crate's handlers take decoded messages.**
- Core:
  - `server.cpp:71-79` checks `IsSessionReady()` and sends `ERR_QUEUE_FULL` *before* `vRecv >> dsa`.
  - `:204-211` sends `ERR_SESSION` before `vRecv >> entry`.
  - `:236-252` checks state (`ERR_SESSION`) and participant (`ERR_INVALID_INPUT`) before it reads `vecTxIn`.
- Here `process_dsa`, `process_dsi` and `process_dss` (`server.rs:402,552,664`) take a `DsAccept`, `DsEntry` or `SignedInputs`. If CJ1 decodes first, a peer whose message does not decode gets no reply, where Core would have answered.
- Impact: replies to malformed messages only. No state changes and no penalty difference (Core logs the decode exception and moves on).
- Fix: add to F4 (or a D-row) that CJ1 runs these pre-checks in Core's order before decoding. `is_session_ready`, `state` and `entries()[..].peer` are already public, so a small `ServerSession::pre_decode_reject(kind, peer, now)` helper would keep the order in the crate.

**2. Low (layer P, D17): the client rules assume a full-node coins view and mempool, which an SPV/mobile wallet doesn't have.**
- `ClientSession::process_final_tx` calls `is_valid_in_outs(.., InOutsKind::FinalTx)` (`client.rs:239-247`; Core `client.cpp:500`). That needs `CoinView::coin` for **every participant's** inputs, plus mempool and InstantSend-lock state (`inouts.rs:182`, `usable_coin`).
- The client's own collateral check is `is_collateral_valid` (Core `client.cpp:1137`). It needs the same view and a mempool `test_accept`.
- A wallet with no UTXO set can't answer for other participants' coins. It would refuse every final transaction (`ErrMissingTx`) or have to fake the view.
- Nothing in the crate is harmful to ship on mobile: it has no I/O, threads or libm, only B and P deps, and the server code is inert there. But D17's rationale ("WL6, the mobile wallet … need the client rules") hides this.
- Fix: add a sentence to D17/D14 saying the client checks presume a full-node `CoinView` and `MempoolAccept`, and that WL6/mobile must supply one or record a deviation (for example, how dashj handles foreign inputs). No code change in CJ0.

**3. Nit: D12's safety rests on an unstated requirement of `MempoolAccept`.**
- `is_collateral_valid` (`collateral.rs:345-376`) wraps its sums. That is harmless only because `test_accept` rejects every out-of-range amount (Core's `CheckTransaction` inside ATMP, `coinjoin.cpp:443`).
- The trait doc (`collateral.rs:324-328`) doesn't say so. Given finding 2, a wallet-side implementation without a mempool may not do it.
- Fix: either state in the trait contract that `test_accept` applies `CheckTransaction`'s `MoneyRange` checks, or add those checks in `is_collateral_valid`. Either way the boolean stays Core-equivalent, because Core returns false for such a transaction too.

**4. Nit: D11's wording and F2's recorded-RNG replay.**
- Core picks the charged offender with `Shuffle(.., FastRandomContext())` (`server.cpp:530`), a separate RNG, not `GetRand`. So Core's `GetRand` stream has no draw for the pick, while the port takes one extra `rand_below` (`server.rs:873-876`).
- The distribution is the same (uniform over the offence multiset, duplicates included), so D11 is acceptable. But "the draw order follows Core's: … then the pick" reads as if the streams line up.
- F2's "session replay … with a recorded RNG" must record the shuffle's outcome separately, or the Rust and Core streams drift after the first charged fee.
- Fix: one line each in D11 and F2.

**5. Nit: the `ServerEnv::rand_below` doc overstates a guarantee.**
- `server.rs:40-42` says the session reduces answers modulo `n`. `create_new_session` doesn't (`server.rs:492`): `i32::try_from(x).unwrap_or(0) + 1` overflows (a debug panic) for `x == i32::MAX`, and gives an id outside `1..=999_999` for any `x ≥ 999_999`.
- This is reachable only if the environment breaks its own contract, not from a peer.
- Fix: take `% 999_999` there, or correct the doc.

**6. Nit (evidence): `server_finalization_rechecks_live_side_coverage` cannot reach the guard it cites.**
- The test (`sessions.rs:704-737`, citing Core `coinjoin_inouts_tests.cpp:432`) passes with `create_final_transaction`'s live re-check (`server.rs:786-792`) deleted.
- `check_pool` (`server.rs:752-773`) only calls it after testing the same live side counts and entry count, and `charge_fees` does not change entries. So the guard can't be reached in the single-threaded port (D7).
- The test does pin the right outcome: no final transaction, then a reset on the next timeout.
- Fix: say in the doc comment that it checks `check_pool`'s outcome and that the guard is defensive.

## The brief's questions

**1. Rules against Core.** I found no divergences.
- **Denominations, collateral amounts, rebalance decisions:** checked against Core's own `constexpr` output, which reproduces byte-for-byte. Amount strings come from dashd.
- **`IsCollateralValid`:** same order and same answers, including `ATMPIfSaneFee`'s 0.1 DASH cap. D2's trait split matches.
- **Queues:**
  - `IsTimeOutOfBounds` uses adjusted time on both sides.
  - Duplicate check: equality, or same masternode and readiness.
  - Server order: denom 10 → null 100 → ProRegTx 10 → duplicate → bounds → signature 10 → ready ignored → threshold → `AllowMixing` → add + relay.
  - Client order: no denom check, `HasQueue` then `HasQueueFromMasternode`, ready → `TrySubmitDenominate`, else threshold → `AllowMixing` → tried mark → add.
  - D16 is correct and needed: Core's one `m_queueman` (`server.h:35`) is cleared by `SetNull` (`:296`), pruned in `CheckTimeout` (`:626`), and checked for `ERR_RECENT` against the masternode's own outpoint, any readiness (`:92`, `coinjoin.cpp:193`).
- **Server flows:**
  - `dsa`: QUEUE_FULL → MN_LIST → RECENT, then create or join with every check and message code in Core's order (ERR_MODE, DENOM, collateral, VERSION flags and rebalance/legacy latching, participant cap, prevout replay → ALREADY_HAVE).
  - `dsi`: SESSION, ENTRIES_FULL, the session-collateral binding without charging, MAXIMUM / SIZE_MISMATCH / shape-mismatch with charging, the duplicate input without charging, then `IsValidInOuts` with `fConsumeCollateral`.
  - `dss`: state, participant, over-cap, then `AddScriptSig` (scriptSig seen → verify → final tx → entry).
  - `CheckPool`, timeout finalization, `CommitFinalTransaction` (reset before telling anyone on failure), `RelayStatus`/`RelayFinalTransaction`/`RelayCompletedTransaction` disconnect handling, `SetState` timestamps, and the scheduler order: all match.
  - Each status update carries the state at push time, as in Core.
- **Client:**
  - `ProcessPoolStateUpdate`: range checks, then the session id is set only in QUEUE.
  - `SignFinalTransaction`: BIP69 → `IsValidInOuts(fFinalTx, fNextBlock)` → own outputs → own inputs (full `CTxIn` equality) → post-V24 cover check.
  - `CompletedTransaction`, and `CheckTimeout` with the 10 s lag and the 10 s error reset.
  - The timestamps WL6 must set (D14) match `client.cpp:378-408, 1453, 1543, 1626, 1672`.
- **BIP69 (D10):** the comparators match `primitives/transaction.h:411-434` and `prevector::operator<` (length, then bytes).
  - On the server, no comparator-equal elements can occur: duplicate prevouts and duplicate scripts are rejected.
  - On the client, a repeated prevout is refused either way. Only the refusal reason can differ, so D10 holds.
- **Rounds:** `GetRealOutpointCoinJoinRounds` matches, including the unmemoised unknown transaction, the in-progress `-10` cycle, the post-V24 rebalance reset, and the `IsFullyMixed` coin flip.
- **DSTX:**
  - `IsValidStructure` (both rule sets, `fPossiblyValidPostV24`).
  - `ValidateDSTX`: already-have → structure, with PREMATURE gated on `fNextBlock` → 24-block `GetMN` scan → `IsValidForMixingTxes` → signature → `DisallowMixing` and the 0.1 DASH priority.
  - Expiry, the confirmed-height events, and `AddDSTX` keeping the first.
- **Relay:** `senddsq` → WantsDSQ, `RelayDSQ`, `_RelayTransaction`'s `MSG_DSTX` choice, `CanAnnounceDstxTo` at announcement time only (getdata has no version gate, as in Core), and `MSG_DSQ` getdata through `queue_from_hash`.

**2. D11 and D12.** Both are acceptable: fees and collateral sums are not consensus, and the outcomes are the same.
- D11's distribution is identical (see finding 4 for the RNG-stream note).
- D12 changes only which check fails first. It never changes the boolean, provided `test_accept` is faithful (finding 3).
- I found no undeclared behavioural deviation. The pre-decode reply order (finding 1) is an API boundary and belongs in the hand-off notes.

**3. Evidence.**
- **Strong:** `core_common.json` runs Core's own functions over a wide grid, including `INT_MIN`/`INT_MAX`/`INT64_*` and off-by-one amounts. `core_fully_mixed.json` copies Core's hashing lines and `regen.sh` refuses to run if they drift. `core_amount_strings.json` comes from dashd. Each file records its provenance, `regen.sh` reproduces all three byte-identically today, and `core_common.rs` consumes every key.
- **Ports:** Core's `coinjoin_queue_tests`, `coinjoin_dstxmanager_tests` and the relevant `coinjoin_inouts_tests` cases are ported with Core's expectations and cite file and line.
  - The uncited Core cases are the wire-cap tests (BS9b) and the wallet manager, options and pending-observation tests (WL6).
  - The promotion/demotion count, adjacency and edge cases are merged into `validate_*_entry_cases`.
- **Weak:** as Sol noted, the server and client flows in `sessions.rs` beyond Core's `server_*` cases are hand-written expectations, not Core-derived. That is acceptable under D5/F2 until O2a-CJ's session replay lands. See finding 6 for one port that can't reach the code it names.

**4. Layer P.**
- Nothing that shouldn't be on mobile: no I/O, threads, globals or environment reads, and the one `f32` (`CalculateAmountPriority`) is plain IEEE arithmetic.
- Dependencies are B (base-sdk) plus `dash-consensus` (P). `ServerSession` is masternode-only code that is inert in a wallet build; a `server` feature is optional and I don't recommend it now.
- The real mobile question is finding 2: the client checks need a full-node coins view.

**5. Panics and unbounded growth from peer messages.** None found.
- **Indexing:**
  - `tx.inputs[index]`: the index comes from the loop that built the transaction.
  - `offenders[pick % len]`: `len > 0`.
  - `inputs.len() - larger_inputs`: `larger_inputs <= len`.
- **Arithmetic:**
  - Sums in `is_valid_in_outs` add only denominated values: at most about 9e13 duffs even for a 3 MB message.
  - Collateral sums wrap (D12).
  - `is_time_out_of_bounds` subtracts only non-negative values.
  - Saturating arithmetic in timeouts, the counters, composition and structure.
  - `MixSideCounts` stays at or below the participant count.
- **Growth:**
  - Session vectors and maps are capped by `max_participants`.
  - Both queue managers hold at most one queue per valid masternode per readiness, pruned by time (the server adds only non-ready queues and clears on reset).
  - `MixingCounters` is keyed by valid masternodes; the DSTX scan covers 24 blocks.
  - `DstxManager` is limited by `IsValidForMixingTxes`, pruned by `check`, as in Core.
  - `dss` is capped by `max_pool_in_outs` before decoding (BS9b `OverCap`).
- **Cost:** the per-`dss` work is the same O(n²) shape as Core's.
- **Recursion:** `rounds` is bounded by `MAX_COINJOIN_ROUNDS + random_rounds`, which is local configuration, not peer input.
