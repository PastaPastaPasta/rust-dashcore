# SB8 follow-ups (BL-209 (3) + BL-218): narrow Opus-h review r1

Head **f647bbff0** (`node/sb8-lq-followups`, one signed commit on a47bf2f61). Core source read at the
oracle pin 7188798effbf (`node/train-17-oracle-v24.0`).

## Findings

No Blocker, Major or Minor.

### Nits

- **N1. SB8 D9's amendment is a parenthetical on the old rule.**
  - Where: `docs/node-program/designs/SB8-notes.md:25`.
  - The row still reads "`InvalidSignature` if any pass had a signature to reject, else `NoQuorum` (amended by LQ3a D32: …)". Someone skimming it reads the superseded rule first.
  - Fix: replace the clause with "the last pass's answer (LQ3a D32)".
  - The same row's rationale (unchanged text) says "Core retries only the locks pass 1 marked bad". That's pass **0**, as `net_instantsend.cpp` v24:340-353 shows.
- **N2. D31(b) and D24's M2 argument could be stated more exactly.** Q3 has the details. Core does more than hold "the same quorums" in both scans: `ScanQuorums` snaps both start blocks to the same `pindexStore` (`llmq/quorumsman.cpp` v24:184-198). So only the start block's own predicates can differ: `IsQuorumRotationEnabled` (`llmq/options.cpp` v24:50-62) and `IsQuorumTypeEnabled` (:177). Each differs only when DIP0024 activates in one of two cycles: the lock's cycle (pass 0, start C+I vs C+I−1) or the cycle before it (pass 1, start C vs C−1). "The cycle where DIP0024 activates" covers only the first. One sentence fixes the D-row; no test is needed.
- **N3. `rotated_compared >= 100` is loose.**
  - Where: `node-l1/tests/l1_llmq.rs:3115`.
  - D29 records 280. If either chain's rotated commitments silently dropped out (for example, `connect_rotation` skipped for `llmq-regtest`), the floor would still pass on the other's alone.
  - Fix: assert the exact count, or a per-stem floor. The sibling tests (`second_pass_accepts == 2`, accept counts) already pin exact counts.
- **N4 (outside the diff, for LK1).** The score Core applies on a last-pass `InvalidSignature` has three properties:
  - it is `OLD_ACTIVE_SET_FAILURE_MISBEHAVIOR_SCORE` = 20, not a ban;
  - it is applied per **source**, at most once per batch (`ApplyVerificationResults`, `net_instantsend.cpp` v24:186-197, `ban && source_bad && penalized.emplace`);
  - so in a mixed batch, a peer whose no-quorum lock arrives beside an invalid one is still scored.

  SB8 D8 already records the per-source effects as P2P policy. The several docs that say "ban" for this path should say "score 20" when LK1 picks it up.

## Q1: is `combine` exactly Core's v24 behaviour? Yes.

Core's flow at the pin:

- `ProcessPendingISLocks` (`instantsend/net_instantsend.cpp` v24:330-356) runs pass 0 with `ban=false` (:340). It re-runs only `bad_is_locks` = `badMessages` (:342-353) with `signOffset=dkgInterval, ban=true`.
- In `BuildVerificationBatch`:
  - no quorum puts the lock in `badMessages` only (:154-158);
  - an unreadable signature puts the peer in `badSources` only (:122-126), so that lock is never retried. Ours handles this before the passes as `MalformedSignature`, `locks/mod.rs:612`.
- In `CBLSBatchVerifier::Verify` (`bls/bls_batchverifier.h` v24:84-118), an invalid signature lands in both `badSources` and `badMessages`.
- `ApplyVerificationResults` (:186-201) scores only when `ban && source_bad`.

So for one lock:

| pass 0 | pass 1 | Core | ours (new) |
|---|---|---|---|
| Valid | – | accept | accept |
| Invalid / NoQuorum | Valid | accept | accept |
| Invalid | NoQuorum | drop, no score | `NoQuorum` |
| NoQuorum | Invalid | drop, score 20 | `InvalidSignature` |
| Invalid | Invalid | drop, score 20 | `InvalidSignature` |
| NoQuorum | NoQuorum | drop, no score | `NoQuorum` |

Every pass Core decides therefore gets the right answer.

`BlsUnsupported`, `QuorumUnavailable` and `OutsideHorizon` say nothing about Core's verdict for their pass, and that pass may be pass 0, which could accept. So each of them must beat any Core rejection, and `combine` makes them do so. Their order among themselves has no Core meaning.

`UnknownLlmqType`, `View` and the other outcomes still return at once.

A pass-0 accept returns before pass 1 runs (`mod.rs:630-636`), as Core's retry set excludes it. The oracle's `IsLockVerdict` (`src/test/oracle/oracle_llmq.cpp` v24:1076-1084) mirrors the same two-pass loop and exposes each pass's status, which is what the differential compares.

## Q2: do callers change behaviour? No production caller changes.

`verify_islock` has no production caller in the workspace; LK1 isn't written yet. Its only users are:

- `dash-evo`'s unit tests;
- `node-l1/tests/l1_llmq.rs`, which maps `InvalidSignature`/`NoQuorum` to `NotVerified { invalid }`.

The oracle differential compares that mapping with Core's last-pass status. `verify_qsigrec`'s mapping (`locks/recsig.rs:184-198`) doesn't go through `combine` and is unchanged.

When LK1 lands, scoring from the new status is Core-equal per lock. The batch/per-source effects (N4, SB8 D8) and the `HasRecoveredSig` shortcut (SB8 D20, LQ3a D8) remain LK1's job, as already recorded.

## Q3: is the D24/M2 equivalence argument sound? Yes, and stronger than written.

M2 differs only when tip T = C + I. Pass 0 then starts at T (original) or T−1 (mutant); pass 1 starts at C or C−1.

- **The scans are the same.** `ScanQuorums` (`llmq/quorumsman.cpp` v24:184-198) maps a start before the cycle's mining window to the previous cycle's `windowEnd` block, and a start after the window to this cycle's `windowEnd`. That gives:
  - T = C+I, a boundary below its mining start, maps to C + windowEnd;
  - T−1 = C+I−1, above cycle C's window end, also maps to C + windowEnd;
  - for pass 1, C and C−1 both map to C−I+windowEnd.

  This holds whenever 0 < windowStart and windowEnd < I−1. That is true for every rotated type at the pin: `llmq_test_dip0024` 12/20 of 24, `llmq_devnet_dip0024` 12/20 of 48, `llmq_60_75` 42/50 of 288 (`llmq/params.h` v24:231-234, :309-312, :388-391). Commitments outside the mining window are invalid, so no commitment can sit at a boundary.
- **The start block's predicates.** `IsQuorumRotationEnabled` checks DIP0024 "active after" block `cycleBase−1` of the start block's cycle. So T and T−1 differ only if DIP0024 activates within cycle C (pass 0), and C and C−1 only if it activates within cycle C−I (pass 1). `IsQuorumTypeEnabled(start)` is similarly activation-adjacent.
- **Is "except possibly in DIP0024's activation cycle" a real gap?** It's real for mutant equivalence. It isn't a correctness gap: the shipped code uses `<`, as Core does (`net_instantsend.cpp` v24:144-146). `dash-evo`'s `islock_signs_at_the_tip_when_the_tip_is_the_cycle_end` kills M2.

  On mainnet and testnet the case needs a tip exactly one interval past DIP0024's activation cycle, which is in the past. Only a fresh regtest or devnet can reach it, for one block. No test is needed. Tightening the D-row wording (N2) is enough.

## Q4: are the capture and fixture provenance sound? Yes.

- **Reproducible procedure.** `run.sh` gains `LQ3A_SCRIPT` and copies `capture_*.py`, so the rotation script's `from capture_lq3a import …` resolves. `capture_lq3a.py`'s only change is the `TYPES` class attribute, which leaves its behaviour unchanged. Each run mines fresh keys, as the README already says; the procedure is reproducible, the bytes aren't, the same as `lq3`.
- **The script checks its own schedule.** It asserts F % 24 = 0, C = F+24, B signed at C, the A and B cycles = F, D's cycle = C, tip C+23, and V19 inactive at the start and end.
- **No hand edits.**
  - `.captures.json` equals `json.dumps(indent=2, sort_keys=True)+"\n"`, the script's own writer.
  - `.json` equals `json.dumps(indent=1, sort_keys=True)+"\n"`, `gen-llmq-regtest.py:252`.
  - The `fixture_sha256` in `.json` (5f620dea…) equals the sha256 of the decompressed `.fixture.gz`.
  - The `source` string is the one in PROVENANCE's command.
  - `dashd_sha256` e204e316… is the same binary as `lq3-regtest-v24.0` (v24.0.0-rc.3).
- **PROVENANCE.md matches the data.**
  - 29 `qsigrec`: 26 type 0x64 = 100, 3 type 0x67 = 103.
  - 3 `isdlock`, 5 `clsig`.
  - Tip 311; cycles F=264, C=288.
  - Locks: A 284/264, B 288/264, D 308/288.
  - Twelve commitments, four of them type 103 (bases 264/265 and 288/289, mined at 276 and 300 = cycle+12).
  - Four snapshot rows (192, 216, 240, 264).
  - `v19_deployment` inactive at height 2000; the args embedded in the fixture include `v19@2000` and `v20@2000 mn_rr@2000`.

## Q5: Blocker or Major in the diff? None.

- **The test-side rotated selection** recomputes the sign height and calls `selected_quorum` per pass that ran. It also checks accepted locks, so the two pass-1 accepts are compared on selection too.
- **The D23 counter** counts only verdicts where ours equals Core's no-score rejection, and `Tally::check` asserts equality on the rest.
- **`connect_rotation`**:
  - It follows O2a-SP's load → precompute → digest → restart → `set_mined` order.
  - It uses the tip's scheme for every block, as the existing rows tests at `l1_llmq.rs:973`/`:1022` do. The rows-digest equality guards it.
- **`CaptureView`** now derives `rotation_enabled` from `is_quorum_rotation_enabled`; the scheme passed there doesn't matter.

## Validation

Run in a detached worktree at f647bbff0 in a scratch worktree, with a reflinked target and the installed oracle pin.

- `cargo test -p dash-evo --lib locks`: 46 passed.
- `cargo test -p node-l1 --features oracle --test l1_llmq -- match_cores`: 8 passed (both captures' `clsig`, `qsigrec` and `isdlock` differentials, the active window, and the mined path with rotated commitments), 28.4 s.
- **Mutant "any pass invalid"** (`combine`: `InvalidSignature` sticky): killed.
  - `passes_combine_safely` fails (tests.rs:883).
  - `an_islock_rejected_by_both_passes_takes_the_last_pass_status` fails (tests.rs:793).
  - `rotated_legacy_islocks_match_cores_verdicts` fails ("lq3r-regtest-v24.0 isdlock 0 another message's sig Legacy").
  - `captured_islocks_match_cores_verdicts` (non-rotated) still passes, as D23 predicts.
- Core source checked at 7188798effbf:
  - `instantsend/net_instantsend.cpp` :104-201 and :330-356;
  - `bls/bls_batchverifier.h` :71-120;
  - `llmq/quorumsman.cpp` :170-200 and :738-808;
  - `llmq/options.cpp` :50-62;
  - `llmq/params.h`;
  - `src/test/oracle/oracle_llmq.cpp` :1030-1085.

ACCEPT WITH NITS
