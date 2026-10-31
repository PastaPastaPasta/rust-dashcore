**Verdict: CHANGES-REQUIRED.**

# DP1-02 Phase A — Sol r2

Reviewed `dw/dp1-02-registration` at `af4e47ef9c17644c537452a7f556806b63be0fb0`, narrowly against the Sol r1 report on `02ae788`, DEC-171, and m4 Contract-Version 10. PLAN §7 handling applies. No target-branch edits, commits, pushes, GitHub comments, or testnet transactions.

## R1 closure

The original nine probes were adapted from `/work/scratch/dp1-02-r1-sol/review-probes.patch`; the broader mechanism checks below are separate from those repros.

- **R1-01 — partial mechanism closure.** The original nonzero-index and synthetic-index-0 repros are fixed: reconstructed index 0 is treated as unknown, and Finish uses a persisted waiting-row index or allocates a fresh row under the index guard. An unreadable restored row is still bypassed by Finish when its recovered lock has an unknown index: R2-02.
- **R1-02 — original finding closed.** `take` admits only one decision holder, and a taken run cannot enter a library call. Dropping that decision cannot resurrect the retired run. The separate unclaimed evidence-write hole is R2-03.
- **R1-03 — closed.** A known `KeysPrepared` row with its explicit tracked outpoint is normalized before funding; resume finishes it and Finish can clear `broadcast_unknown` on positive tracked evidence. No second funding is needed.
- **R1-04 — partial mechanism closure.** An unknown funding/profile/phase/outpoint encoding stops the driver and refuses discard. An explicit outpoint or a known-index matching lock remains committed. Unknown-index reconstruction does not get the same protection: R2-02.
- **R1-05 — original adoption repro closed; index ownership mechanism incomplete.** The driver adopts by the identity derived from its own outpoint, and the completed other holder no longer donates its identity. A stale tap snapshot can still attach the old index's lock after a new owner has reassigned the row: R2-03.
- **R1-06 — closed.** The profile encoding is interpreted before transitions/funding, and an unsupported profile stays unfunded and preserved.
- **R1-07 — partial mechanism closure.** The real spendable view now overlays manual/dust locks and host reservations; `coins_locked` refuses the original static-lock case. The check and selection are not atomic: a lock completed after the snapshot still reaches the signer (R2-04). Funded recovery itself is not stranded by the static refusal.
- **R1-08 — partial mechanism closure.** Process-wide run IDs and run-scoped cleanup fix the old drop/forget/reopen repro, and normal updates/deletes check persisted generations. A retired, queued claim acquisition can still bump the generation after the new claim acquired it: R2-01.

## New findings

### R2-01 — major: a queued retired claim can invalidate the reopened owner

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:544`, `registration_flow.rs:1084`, `registration_flow.rs:1188`; `rust/crates/dw-appdb/src/registration.rs:249`; scheduling/lifecycle: `rust/crates/dw-engine/src/coins.rs:218`, `multiwallet.rs:285`.

**Interleaving:** Claim A reserves the in-memory holder and submits `claim_registration` to `spawn_blocking`, but the closure pauses before SQL. Unload/forget removes A's holder. Reopen/resume B acquires a new run and persists owner 1. A's previously submitted closure now runs and unconditionally increments owner to 2. A's later cleanup correctly leaves B's in-memory holder alone, but B's persisted authority has been stolen. Cancelling an async waiter does not cancel an already running blocking closure.

The driver tests only in-memory ownership, reads rows with any owner, and ignores `RegistrationUpdate::Stale`. Its normal next transition therefore returns `Again` indefinitely while every write is rejected. B still occupies the flow, so resume and Finish cannot claim it. This is a new claim-acquisition race, distinct from the fixed stale-drop repro.

**Reproduction:** `review_sol_r2_retired_persist_does_not_invalidate_reopened_owner` gates the actual `Claim::persist` blocking closure immediately before SQL, reclaims the row, then releases the old closure. It records the new owner's generation and the later stored generation, then runs the reopened driver with a bounded wait.

**Suggested fix:** Fence the acquisition itself against retirement/replacement, including the blocking closure's lifetime; unload/reclaim must not overtake an old acquisition that can still commit. Also stop a driver on persisted-generation loss rather than turning it into an endless `Again` loop. A check before submitting the closure alone leaves the same gap.

### R2-02 — major: Finish consumes an ambiguous recovered lock despite an unknown row

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:750`, `registration_flow.rs:767`, `registration_flow.rs:724`, `registration_flow.rs:868`; `rust/crates/dw-engine/src/platform/registration.rs:872`, `registration.rs:880`, `registration.rs:911`.

**Reproduction:** Restore a newer-version CoreBalance `KeysPrepared` row at persisted index 7, without its outpoint, from a backup behind the source machine's funding. Reconstruct its unconsumed lock from chain: the pinned library reports index 0 / `RecoveredFromChain`. The row's funding encoding is unsupported in this build.

`lock_of` cannot match the unknown-index lock, so `unheld` does not reserve it for the unreadable row. `awaiting_a_lock` excludes that row. Finish creates another row at index 8 and consumes the original lock to create that row's identity, while leaving the index-7 row unsupported and reporting it as having no committed funds. This is the unknown-row variant of the lost registration-index class; the branch's regression covers only a known nonzero-index lock behind an unreadable row.

**Reproduction:** `review_sol_r2_unknown_row_keeps_ambiguous_reconstructed_lock`, using the same pinned reconstruction metadata as R1-01.

**Suggested fix:** When an unreadable, outpoint-less registration row can own an unattributed recovered lock, preserve that ambiguity: count possible funding conservatively and park the lock rather than allocating/consuming it through another row. Do not make unknown encodings eligible for normal import; require positive evidence to disambiguate ownership.

### R2-03 — major: an evidence write bypasses the new owner and index

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:967`, `registration_flow.rs:989`, `registration_flow.rs:1330`; `rust/crates/dw-appdb/src/registration.rs:296`.

**Interleaving:** A changeset tap reads a `KeysPrepared` row at index 7, without an outpoint, and selects it for lock L7. Before the tap's update, a current claim takes the row and performs the driver's supported `Taken`-index reassignment to index 8, still at `KeysPrepared`. The old tap now calls `update_registration(..., owner=None, ...)`. Its predicate checks phase/failure only; neither the persisted generation, the index it matched, nor the outpoint-null condition is in the SQL. It succeeds and advances the index-8 row with L7.

A concrete background-update state is an older unreadable row U at index 7 and a runnable restored row R also at 7, with a known-index tracked L7. `unheld` reserves L7 for U; R's `index_owner` therefore observes the older shared index and chooses `Taken`, permitting its reassignment to 8. The tap instead skips U because it cannot interpret it, selects R at 7, and is outside R's call guard. Pause its write; resume R under a new claim and let the reassignment complete; release the tap. Its obsolete association is accepted. Its appdb reads and write are separate lock acquisitions, and it does not participate in the index lock. A subsequently prepared index-8 funding can be left beside the wrong recorded lock.

**Reproduction:** `review_sol_r2_evidence_write_cannot_overwrite_reassigned_index` pauses the real tap immediately before its update, takes a new persisted claim, executes the same owner-checked index update used by `assign_index`, then releases the tap. The stale evidence update is accepted.

**Suggested fix:** Resolve evidence through the same ownership policy as the driver, including unreadable holders, and route it to the row owner. Any association write must be an atomic compare-and-set on the exact generation/index/outpoint snapshot it proved. On a mismatch, retain the evidence and reconcile it again; recording positive evidence must not transfer ownership based on an obsolete row read. Restrict the generic `owner=None` write API so it cannot silently advance arbitrary stale snapshots.

### R2-04 — major: `coins_locked` has a snapshot-to-signing race

**Locations:** `rust/crates/dw-engine/src/platform/registration_flow.rs:1408`, `registration_flow.rs:1428`, `registration_flow.rs:1456`, `registration_flow.rs:1976`; `rust/crates/dw-engine/src/coins.rs:360`.

**Interleaving:** The funding step reads a real coin snapshot with no protected coins. The user completes `lock_outpoints` for its source coin before the asset-lock builder selects/signs. Funding still uses the saved `protected=0` result and the builder receives no exclusions. The registration call guard excludes discard; it does not exclude coin-policy changes.

**Reproduction:** `review_sol_r2_coin_locked_after_snapshot_is_not_signed` credits the real fixture with one confirmed coin, obtains the real `PwLib::spendable` snapshot, completes the real manual lock, and lets the real registration driver invoke the pin's offline asset-lock builder. The resulting signed transaction includes the now-locked coin. The probe uses a library seam only to place this interleaving and replace broadcast with the offline builder; wallet, snapshot, lock API, signer, and selection are real. Nothing is broadcast.

**Suggested fix:** Make policy validation and input selection one fenced operation, sharing exclusion with manual/dust lock writes and spend reservations, or pass the authoritative excluded set into selection. A second snapshot with the same unfenced gap is insufficient. The interim refusal may remain, but it must hold through selection/signing.

## Validation

The reviewer's stream disconnected before final validation; the probe patch and logs are under `/work/scratch/dp1-02-r2-sol/`. The manager accepted the draft as the r2 verdict.

Sol gate: NOT SATISFIED.
