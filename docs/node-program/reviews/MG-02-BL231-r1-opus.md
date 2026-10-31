# BL-231 governance signals: review round 1 (Opus-h)

Branch `dw/bl-231-gov-signals` at `9a13320e`, base `c6607d71`, 8 commits. Worktree
`/work/scratch/dw-bl231-r1-sol/wt` (detached, left clean). Core reference: `~/workspace/dash` at
`v24.0.0-rc.2-109-g15db4787500`, read only.

## Verdict

**CHANGES REQUIRED.** One Major: the D85 doubt map has no bound, and a single peer can grow it
for the whole session (shown with a scratch test, below). Everything else checks out against
Core: the operator-vote verification, the thresholds, the deletion mark and delay, the vote
order, and the unchanged funding behaviour. Two Minors and two Nits. The Major fix is small and
local to `doubt()`.

## Findings

### Major M1: D85 doubts grow without bound, and one peer can drive them

- **Where:** `rust/crates/dw-governance/src/store.rs:521` (`doubts`), `:2412` (`doubt`, no
  limit), with three feeders:
  - `:2281-2290`, the refused-for-sender's-bound branch of `add_operator_vote`. It calls
    `lose_held` even for a vote that was not held.
  - `:2364-2373`, `park`'s eviction of a held NO.
  - `:2286`, a held NO that fails.

  The only clean-up while the list has an unresolved masternode is per outpoint (`:1594-1597`
  `known()`, and `place_signal` `:2457`).
- **Scenario:**
  1. Take a current proposal `P` and a list with at least one unresolved masternode. That is the
     normal state at start, for any masternode the resolver gave up on (D80), and always when
     there is no collateral source (no Platform, no `--collateral-rpc`).
  2. One outbound peer fills its 2,000 parked slots with BLS-sized `delete` NO votes on `P`, each
     for an invented outpoint `(txid, 1000+i)`.
  3. It then sends more such votes, each with a fresh outpoint. Each one gets
     `Err(Unverifiable)`, and `lose_held` → `doubt(P, outpoint, time)` adds a new entry
     `doubts[P][outpoint]`.

  Nothing drops those entries: no checked vote of that outpoint ever arrives, the outpoint is in
  no `previous` list, and `all_resolved()` is false. In the sync, the peer's `unkept` budget
  (`sync.rs:1368`, `max_unkept` 25,000, `:281`) caps this at about 25,000 new entries per peer
  per run. Every new run resets that budget, so over a session the map has no limit.

  The eviction path (`:2371`, inside `:2364-2373`) adds up to 2,000 more per peer per run once the pool is full, with
  no `unkept` cost at all. The `GovernanceStore` API itself enforces nothing: the scratch test
  below sent 50,000 votes from one peer and got `doubts = 50000`, `parked = 2000`.

  The design does say a peer can block a mark this way (D85's fail-safe). It does not say the
  wallet holds one entry per invented outpoint for the session. Every other per-peer structure
  in this store has a bound (D22, D53, `MAX_ORPHAN_VOTES`, `MAX_UNSTORED_LOST`, the 100,000
  remembered hashes). `drop_changed_operator_votes` also walks the whole map on every
  `set_resolved` (`:1594`), so CPU grows with it too.
- **Fix:** bound the doubts per proposal. For example, keep at most `MAX_PARKED_PER_PEER` (or
  the list size) outpoints per proposal. Past that, replace the map with a whole-proposal doubt
  (a `HashSet<[u8; 32]>`, bounded by the object cap) that blocks the mark until the proposal
  leaves. That is the same fail-safe outcome with bounded memory.
  - Add a store test that pushes past the bound and asserts the size, plus a D85 sentence.
  - Optionally, in the refused-for-bound branch, create the doubt only for a vote that was held,
    or one whose outpoint is listed but unresolved. The sync already asks another announcer for
    a refused copy.

### Minor m1: arrival BLS checks are limited only by `max_unkept`, which is sized for ECDSA

- **Where:** `sync.rs:1335-1339` (`OperatorVote::checked` on each delivered operator vote),
  `:1368` (`unkept`), `:281` (`max_unkept: 25_000`); `vote.rs:200-213` (a failing check runs a
  second pairing on the legacy re-read whenever that re-read decodes).
- **Scenario:** a peer announces operator votes for outpoints the wallet resolved and serves
  copies with garbage 96-byte signatures. Each one costs one or two BLS pairings (about 1 ms
  each, as `sync.rs:368` says) inline in `Run::handle`, with no penalty.
  - It gets refused up to 25,000 times per peer per run, so roughly 25–50 s of the sync task's
    CPU per peer per run, and up to 3 peers.
  - D89's "64 checks a tick" covers only the parked path. D63's budget was sized for compact
    recovery, about 20–40× cheaper than a pairing.
- **Fix:** give failed operator checks their own small budget per peer per run (for example
  1,000), or count each failed BLS check as about 20 toward `unkept`. Say in D89 that arrival
  checks are bounded by it.

### Minor m2: a re-encoded operator key drops votes that Core keeps

- **Where:** `store.rs:1580` (`new.operator_key != old.operator_key`, comparing bytes); the
  bytes come from `collateral.rs:234`.
- **Scenario:**
  - A version-1 list entry carries its operator key in the legacy encoding. When that
    masternode's entry moves to version 2 (for example a v2 ProUpServTx, which keeps the key),
    the same point arrives in the basic encoding.
  - Core's `RemoveInvalidVotes` compares `pubKeyOperator` as `CBLSLazyWrapper::operator==`
    (`src/bls/bls.h:514-534`). It falls back to comparing points when the schemes differ, so it
    keeps the masternode's votes.
  - The wallet sees different bytes and drops every stored `valid`, `delete` and `endorsed`
    vote of that outpoint.
  - If one of those was a `delete` NO, the delete net yes rises. A mark can then follow in a
    run whose coverage came from D68 window records, without fetching that NO again. Core would
    not mark there.
  - Narrow, but it is a path the D84 deviation list doesn't cover, so focus 3 asks about it.
- **Fix:** compare points, not bytes. Canonicalise both keys to the modern encoding (try basic,
  else legacy → `reencode(Modern)`, as `verify_operator` does) before comparing, or store the
  canonical form in `ResolvedMasternode`. `take_parked_ready`'s `failed_under` check (`:2322`)
  then also stops retrying on a pure re-encoding. Add a test with the vector key in both
  encodings.

### Nit n1

`add_operator_vote`'s doc (`store.rs:2246-2254`) and D85 call a NO refused for its sender's
bound "held". At arrival it isn't (`held: false`), and `lose_held` runs for it anyway. Say
"refused for its sender's bound (perhaps its only announcer's copy)" to match the code and
`a_delete_no_refused_for_its_senders_bound_is_a_doubt`.

### Nit n2

`verify_operator` (`vote.rs:200`) tries the basic encoding, then the legacy one, on every call,
including for keys known to be basic. Once m2 stores a canonical key, the fallback can go, and
so can the second pairing of m1.

## Requested checks

1. **Operator-vote verification (D89): pass.**
   - Scheme: `BlsPublicKey::verify` under `BlsScheme::Modern` is `BlsScIetf`, the IETF basic
     scheme (`dashcore-crypto` `crypto/src/bls.rs:211-221`). That matches Core's
     `sig.SetBytes(vchSig, false)` and `VerifyInsecure(pubKey, sigHash, false)` (`vote.cpp:192-207`).
   - Hash: `signature_hash()` is sha256d of the serialization without the signature, in
     internal byte order (`vote.rs:155-159`). That is Core's `GetSignatureHash()`, and the same
     function is already covered for ECDSA by the testnet vectors.
   - Key encoding: the list's bytes are tried as basic, then as legacy re-encoded to basic.
     Legacy bytes read as basic give the negated point, which only the operator's own
     (negated) signature verifies, and the vote hash leaves the signature out. So no forgery is
     possible (`vote.rs:191-213`).
   - Selection follows Core's `onlyVotingKeyAllowed` (`object.cpp:458`) in `precheck`
     (`store.rs:1976-1993`):
     - BLS on a proposal's signals 2–4 is checked against the operator key;
     - BLS on a funding vote is `Malformed`;
     - ECDSA on signals 2–4 is `Unverifiable`, with no penalty;
     - a vote on an unknown parent is `Unverifiable`.
   - No lock while verifying:
     - In `on_vote` (`sync.rs:1335-1339`), the guard from `store()` is a temporary dropped at
       the end of the `let key` statement.
     - In `check_parked` (`:1696-1717`), `take_parked_ready` returns a `Vec` and the guard is
       dropped.
     - `GovernanceStore::add_vote` does verify under `&mut self`, but its only callers are
       tests. The production facade relays funding votes only (`voting.rs:121`, `GovernanceVote::funding`).
   - MG-01: the diff adds no secret type or signing call. The `FORBIDDEN` names of
     `no_operator_signing.rs` and the clippy disallowed paths don't occur in any added line;
     `clippy.toml`, dw-protx and dw-ffi are untouched.
   - The BLS vector was produced outside the repo. The `bls` feature only adds public-key and
     signature types.
   - Mutation: dropping the legacy fallback (`||` → `&&`) fails
     `verify_operator_checks_a_basic_bls_signature_under_either_key_encoding` (`vote.rs:538`).
2. **Parked pool and bounds: pass, with M1 and m1.**
   - 10,000 in all, oldest first; `parked_order` is compacted (`store.rs:2346-2393`).
   - 2,000 per sender, checked before eviction, so a sender at its bound can't evict
     (`:2359-2363`).
   - At most 64 checks a tick, oldest first, only those whose key changed (`take_parked_ready`
     `:2314`, `sync.rs:370`). The per-sender key is the outbound `SocketAddr`, which is stable
     across runs.
   - No penalty for any operator outcome, so honest peers are never penalised. `Parked` doesn't
     count `unkept`.
   - Still unbounded: the doubts (M1), and arrival-check CPU beyond what `max_unkept` limits
     (m1).
   - Mutation: dropping `lose_held` on eviction (`store.rs:2368-2372` → a bare `unpark`) fails
     `parked_votes_are_bounded_and_a_held_no_evicted_is_a_doubt` (`store.rs:4844`).
3. **Deletion mark (D84, D85): pass, except m2.**
   - The mark needs, all at once (`mark_deleted`, `store.rs:1228-1272`):
     - `votes_synced` (cleared by `begin_vote_sync` at run start, `sync.rs:2108`; filled by
       `set_synced(&voted)` only after the vote phase drained);
     - no doubt, and no held parked delete NO;
     - a proposal;
     - delete net yes, exact (resolved weights only), ≥ `delete_threshold`.
   - `valid_weight == 0` returns, as Core's `nWeightedMnCount == 0` does (`object.cpp:970`).
   - The first time is kept. `:1176-1180` removes the proposal at `t + 600 <= now`, matching
     Core's `nTimeSinceDeletion >= GOVERNANCE_DELETION_DELAY` (`governance.cpp:440`); the hash
     goes to the lapsed set.
   - Freezing: `parent()` treats a marked proposal as expired, so its votes are ignored (Core
     `:843`). `current_proposals` and `is_current_proposal` skip it (Core `:954`).
   - `weighted()` keeps it from eviction (`:1136-1144`). Core doesn't skip it in `GetApprovedProposals`.
   - A forged vote can't count: `place_signal` is reached only with `checked == (resolved key,
     true)` (`:2268`).
   - Other paths to a mark Core wouldn't make: D84's three listed cases (D91 `NONE`, a newer
     operator key, D68 window coverage), plus m2.
   - Mutation: removing `!waiting.contains(*h)` fails
     `a_delete_no_the_wallet_cannot_check_blocks_the_mark` (`store.rs:4725`).
4. **Sentinel thresholds (D83): pass.**
   - `threshold` = max(q, W/10) and `delete_threshold` = max(q, ⌊2W/3⌋) (u64 inside, no
     overflow), matching `nAbsVoteReq` and `nAbsDeleteReq` (`object.cpp:974-975`).
   - Console `Flags::of` (`dw-console/src/governance.rs:175-184`) matches `:990-996`:
     - funding and endorsed: yes − no ≥ req;
     - valid: no − yes < req;
     - delete: the mark.
   - `CountMatchingVotes` (`object.cpp:879-893`) counts any listed masternode's weight,
     PoSe-banned ones included, as `signal_tally` does with resolved weights.
   - Same-time order: `replaces` refuses `outcome <= current`. Core refuses `<`, but an equal
     outcome at the same time is the same hash, so it is a duplicate first (`object.cpp:430-441`).
   - `sentinel_thresholds_follow_core_at_their_bounds` covers each bound below and at.
5. **D86/D87/D88: pass.**
   - D86: `vote_rows` (`reads.rs:250`) sorts by (outpoint, signal, time). Core's
     `GetCurrentVotes` walks `std::map<COutPoint>`, then `mapInstances` by signal code
     (`governance.cpp:548-584`), and `VoteSignal`'s derived order is codes 1–4.
     - The console recomputes the hash with the row's signal; the hash covers it.
     - `gobject get` fills all four `*Result`s as `GetVotesJson` does (`rpc/governance.cpp:762-765`).
     - The list filters match `:638-641`.
   - D87:
     - `refetch_votes` rotates the first peer per hash (`sync.rs:1755-1757`).
     - Timeout, close and `notfound` moves go through `ask_refetches`, batched per peer at
       `getdata_batch` (`:1772-1800`).
     - `accepted()` ends a refetch only once the store holds the vote (`:961-966`), so a parked
       copy keeps it open (`a_parked_copy_does_not_end_a_refetch`).
     - The single `refused` path stays unbatched, which is fine.
   - D88:
     - `round_unless_stopped` loops on `stop.changed()` and returns `None` only for an error or
       `true` (`collateral.rs:472-490`). The old `continue` on `false` restarted the round; the
       new loop keeps the same future, which is no observable change, since stop only ever
       sends `true`.
     - `the_rpc_source_maps_dashds_replies` covers -5, other errors, a non-string result, the
       401 and the closed connection.
6. **Funding records (D64–D81): unchanged.** Funding still goes through `recover` +
   `add_checked`:
   - `check_slot` is the old inline logic moved, and its orphan-slot key adds the signal,
     always 1 for funding orphans, since other signals never orphan.
   - `lose_orphan` notes lost only for funding, and only funding orphans exist.
   - The BLS funding verdicts (`Malformed` on a proposal, `Unverifiable` on an unknown parent)
     and the trigger `Ignored` are as before.
   - The behavioural changes for funding are Core's: votes on a marked proposal are ignored,
     and a marked proposal is not vote-synced.
   - The signal votes live in their own map. The only shared state is `seen_votes` and
     `vote_count`, both kept balanced in `remove_signal_vote` and `remove_object`.
7. **Tests:**
   - The named tests do fail under the bugs they target. Three local mutations, each restored
     with `git checkout`, and each failed its target test:
     - eviction without a doubt;
     - the mark ignoring held parked NOs;
     - verification without the legacy fallback.
   - The sync-level tests run through the real `on_vote` with a real BLS vector:
     - `an_operator_vote_is_checked_against_the_operator_key`: the forged copy is refused with
       no penalty, and the honest one is stored from the other announcer;
     - `a_parked_operator_vote_is_checked_once_its_key_resolves`.
   - Gap: no test bounds the doubts (M1).

## Validation

All builds used `RUSTC_WRAPPER=` and `-j 8`; nothing under `/tmp`.

- **dw-governance:** `CARGO_TARGET_DIR=/work/scratch/dw-bl231-r1-sol/target cargo test -j 8 -p dw-governance`.
  216 passed, 0 failed. golden 3 passed, oracle_governance 4 passed, doctests 0.
- **dw-engine:** `cargo test -j 8 -p dw-engine governance`, same target. Exit 0. Lib unit tests 20 passed, 0 failed (1,096 filtered out). The 17 integration binaries matched 0 tests under the `governance` name filter, so `mg_governance_contract.rs` (the Contract-Version 4 surface) was not run. Nor was dw-console's `console_governance.rs`, which is outside both commands the brief allows.
- **Mutations and demo:** run with `CARGO_TARGET_DIR=/work/scratch/dw-bl231-r1-sol/target-mutation1`
  (the scratch dir's mutation target; using it kept the engine build's lock free), each
  `cargo test -p dw-governance --lib <test>`. Script and logs are in
  `/work/scratch/dw-bl231-r1-sol/opus-mut/`.
  - m1-evict-no-doubt: `parked_votes_are_bounded_and_a_held_no_evicted_is_a_doubt` FAILED (killed).
  - m2-waiting-ignored: `a_delete_no_the_wallet_cannot_check_blocks_the_mark` FAILED (killed).
  - m3-no-legacy-fallback: `verify_operator_checks_a_basic_bls_signature_under_either_key_encoding` FAILED (killed).
  - demo-doubts (temporary test appended, then removed). One sender sent 2,000 parked and
    50,000 refused `delete` NOs with fresh outpoints. Output: `doubts=50000 parked=2000
    refetch=50000`, passed. This shows M1.
  - `git status --short` was empty after each step; HEAD is still `9a13320e`.
- **Not run:** the regtest suite `dwd_governance.py` (check 6, D90). I read it: it asserts
  Core's refusal of `vote-alias`/`vote-many` votes on the other signals and the parity of flags,
  results and lists after `mockscheduler`. I also didn't run the dw-protx guard test; the diff
  scan above stands in for it. The earlier Sol run's `mutation-operator.log` gives no record of
  what it mutated, so I didn't count it.
