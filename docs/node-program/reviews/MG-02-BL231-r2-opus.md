# BL-231 governance signals: review round 2 (Opus-h, narrow)

Range `9a13320e..10a7def5` on `dw/bl-231-gov-signals`, two commits: `1e6f35b7` (the r1 fixes) and
`10a7def5` (the simplifier). The worktree `/work/scratch/dw-bl231-r1-sol/wt` was detached at
`10a7def5` and left clean. The Core reference is `~/workspace/dash` at `v24.0.0-rc.2-109-g15db4787500`,
read only.

## Verdict

**ACCEPT WITH NITS.** M1, m1 and m2 are closed.
- Every feeder of the doubts map now goes through one bounded `doubt()`.
- The `doubted` fold can't leak, and it blocks the mark.
- The failed-check budget is per peer per run, and nothing gets past it unchecked into the store.
- Keys now compare as points.
- The refactor preserves behaviour.

Three nits:
- **n1:** the `doubt_count` decrements on the two clean-up paths are untested. Two of my mutants survived.
- **n2:** D98 and the readings doc describe the ambiguity as one-sided. Basic bytes are ambiguous too.
- **n3:** D98 can be removed cheaply with an upstream field. That belongs in the backlog, not this PR.

None of them blocks the merge.

## Findings

### n1: the `doubt_count` decrements on the clean-up paths are untested

- **Where:** `store.rs:1633-1639` (`drop_changed_operator_votes`, `gone`) and `store.rs:2492-2498`
  (`place_signal`, `before - d.len()`).
- **Evidence:** with either decrement removed, all 219 dw-governance lib tests still pass (mu5, mu7 below).
  `doubts_are_bounded_per_proposal_and_in_all` checks `held == doubt_count` only after inserts and
  `remove_object`. It never checks after a list change or a checked delete vote clears a doubt.
- **Scenario if it regresses:** doubts are cleared over a session by resolution, by masternodes
  leaving, or by checked delete votes. The count then drifts upward until `MAX_DOUBTS` trips with
  fewer than 10,000 real entries. From then on, every new doubt folds its proposal for the session.
  That is fail-safe (no wrong mark), but it breaks D85's "met only past … 10,000 in all". A
  decrement that subtracts too much would instead panic in debug builds and wrap in release.
- **Fix:** in the bounds test, or in the two existing doubt-ending tests, assert
  `doubt_count == doubts.values().map(len).sum()` after `set_resolved` drops an outpoint and after
  a checked delete vote clears one. Alternatively, add a `debug_assert!` of that sum in `doubt()`.

### n2: the ambiguity is two-sided, but D98 and the docs describe one side

- **Where:**
  - `vote.rs:394-398` (`operator_key_readings` doc: "two when the legacy bytes are also a basic key");
  - MG-02 D98 (`docs/design/MG-02-governance.md:516`: "one pairing for basic bytes, at most two
    for legacy bytes that also read as basic");
  - the owner's summary ("Legacy bytes with the sign bit set …").
- **Fact:** byte 0 in `0x80..=0x9F` is valid in both encodings:
  - in basic, it is compressed with the sign clear;
  - in legacy, it is the sign set, with the top x bits in range.

  So a v2+ entry whose basic key has its sign clear (about half of all keys) also has two
  readings. It costs two pairings on a failed check, exactly like ambiguous legacy bytes. The
  test vector shows it: `legacy` (`0x95…`) is itself a valid basic key, namely the negation of
  `basic`, and `operator_key_readings(&legacy)` returns two readings. A v2 masternode whose
  operator key is that point carries exactly those bytes.
- **Consequence:** none beyond what D98 already accepts. The extra reading is the negated point,
  so the only conflation is still "rotate to exactly your own negation". m1's "one or two
  pairings" already covers the cost.
- **Fix:** reword these three places to "bytes valid in both encodings (top byte `0x80`–`0x9F`)".
  This is wording only.

### n3: D98 can be removed, and Core has no ambiguity to match

- **Core:** Core never compares ambiguous bytes.
  - `CDeterministicMNState` and `CSimplifiedMNListEntry` read `pubKeyOperator` through
    `CBLSLazyPublicKeyVersionWrapper(…, nVersion == ProTxVersion::LegacyBLS)` (`evo/dmnstate.h:107`,
    `:301`; `evo/simplifiedmns.h:83`).
  - `deterministicmns.cpp:1109` does `SetLegacy(nVersion == LegacyBLS)` on state diffs.
  - So each `CBLSLazyWrapper` carries `bufLegacyScheme`. `operator==` (`bls/bls.h:514-534`)
    compares bytes only when both schemes are equal. Otherwise it compares the decoded points.

  Core therefore compares exact points, and the negated-point conflation is wallet-only. It is
  caused by `MasternodeListSummary` dropping the version.
- **Is D98 acceptable?** Yes. The conflation needs an operator to choose its secret's
  negation. Even then, the votes kept are that operator's own (same vote hash). They count
  where Core would drop them only until the operator re-votes. No forgery is possible:
  - a signature that verifies under −P is the negation of the operator's signature over the
    same hash;
  - precheck treats it as a duplicate before any check (`sync.rs:1298-1317`).
- **The cheap removal:**
  - `MasternodeListSummary::from_entry` (platform fork `82fb33a`,
    `packages/rs-platform-wallet/src/masternode/list.rs:58`) already has the entry. rust-dashcore's
    `MasternodeListEntry.version` is decoded (`dash/src/sml/masternode_list_entry/mod.rs:136`, `:219`).
  - Add `legacy_operator_key: bool` (`version == 1`) to the summary, carry it in `ResolvedMasternode`,
    and give `operator_key_readings` exactly one reading. That removes both D98 and n2's second pairing.
  - It needs a platform-fork commit and a pin bump, so it belongs in a backlog row, not in this PR.

## Requested checks

### 1. M1 is closed

- **Feeders.** `doubt()` (`store.rs:2438-2452`) is the only insertion point. Its only caller is
  `lose_held` (`:2335-2340`), which has exactly three callers, all capped:
  - the failed held vote (`:2308-2310`);
  - the refused-for-bound branch (`:2317-2326`), which runs for unheld votes too, as D85 now says;
  - `park`'s eviction of a held vote (`:2397-2401`).

  mu10 (the refused-for-bound branch makes no doubt) is killed by
  `a_delete_no_refused_for_its_senders_bound_is_a_doubt` and the bounds test.
- **Caps.**
  - A new outpoint is inserted only below both `MAX_DOUBTS_PER_PROPOSAL` (2,000) and `MAX_DOUBTS`
    (10,000). Otherwise the proposal's map is dropped (`forget_doubts` subtracts its length) and
    the proposal goes into `doubted`.
  - An existing outpoint only moves its time forward.
  - A proposal already in `doubted` returns early.
  - The `entry().or_default()` map is never left empty: the fold branch removes it.
  - Walks over the map (`drop_changed_operator_votes`) are now bounded by 10,000.
- **No leak.**
  - `doubted` gets an insert only for a hash in `objects` (`:2439`).
  - Every way an object leaves goes through `remove_object` (callers `:1113`, `:1158`,
    `:1233-1240`; no other `objects.remove/clear/retain`), and `remove_object` clears both
    `doubts` and `doubted` unconditionally (`:1316-1317`). So `doubted` ≤ the object cap.
  - mu2 (keep `doubted` on remove) is killed.
- **Mark.** `mark_deleted` (`:1282-1284`) requires no `doubts` entry, no `doubted`, and no held
  parked delete NO. mu1 (ignore `doubted`) is killed. A folded proposal stays blocked until it
  leaves, even after every listed masternode resolves. Before the fold, invented-outpoint
  doubts healed at `all_resolved`, so the fold turns them into a session-long block. That is the
  stated price in D85 and fail-safe, so I accept it.
  - Honest traffic is nowhere near the fold: it would need more than 2,000 lost delete NOs on one
    proposal.
- **Exactness:** see n1. It is correct by reading, but the two clean-up decrements are untested.

### 2. m1 is closed

- **Nothing invalid is stored or counted.**
  - Past the budget, `on_vote` returns through `self.refused(item, from, taken)` before
    `OperatorVote::checked` (`sync.rs:1344-1353`). The copy is neither parked nor stored. The next
    announcer's copy is fetched and checked normally (test: peer 1's copy is stored).
  - With no other announcer, the vote is just not taken, with no doubt. That matches a failure at
    arrival (D85), and an attacker who is the sole announcer could withhold it anyway.
  - "Unchecked" means not checked *and* not kept.
- **Per peer per run.**
  - `failed_checks` lives in `PeerState`, built once per connected peer per run (`sync.rs:661`,
    no reconnects within a run).
  - It counts only `Some((_, false))` (`:1356-1361`). The gate applies only when a key exists,
    which is the only case that runs a pairing.
  - mu8 (no gate) and mu9 (no count) are both killed.
- **No cheaper bypass.**
  - ECDSA, malformed and duplicate copies are refused in `precheck` (`:1298-1317`) before any
    pairing. A negated-signature copy of a stored vote has the same hash, so it is a duplicate.
  - Unresolved outpoints are parked without a check. Parked checks keep their own bound: 64 a
    tick, once per key change, 2,000 per sender.
  - A successful check needs a real operator signature on a distinct vote.
  - So a peer costs at most 1,000 × 2 pairings per run on arrival.
  - `recovered` is now counted only after the gate, as reported.

### 3. m2 is closed; D98 is acceptable (n2 and n3)

- `drop_changed_operator_votes` (`store.rs:1596-1617`) uses `same_operator_key`, so a re-encoding
  is not a change.
- `add_operator_vote` matches checked keys with `same_operator_key` (`:2303`, `:2306`) and parks a
  failure under the list's current bytes.
- `failed_under` moves to the new bytes on a pure re-encoding (`:1624-1629`), so
  `take_parked_ready`'s byte compare doesn't retry it.
- mu3 (re-encoding counts as a change) and mu4 (`failed_under` not moved) are killed by
  `a_reencoded_operator_key_keeps_its_votes`.
- For Core's disambiguation, the remaining risk and the fix, see n2 and n3.

### 4. The refactor preserves behaviour

- **`Parked`.** It now holds an `OperatorVote` with `checked` cleared and `held` set from `park`'s
  own `held = failed_under.is_none()` (`:2406-2408`). That is the old `Parked.held`, so a held vote
  that failed under its masternode's key is still parked unheld. mu6 (keep the vote's own `held`)
  fails 6 tests.
- **The merge path.** `p.vote.held |= held` is the same as before.
- **`take_parked_ready`.** It returns the same `(vote, held, checked: None)` as before.
- **`outcome` removed.** `precheck` returns `vote.vote_outcome()` after the range check
  (`store.rs:2003-2013`), so `is_delete_no` on the vote equals the old stored outcome.
  `OperatorVote.outcome` had no users outside dw-governance besides dw-engine's `reads.rs` tests,
  which were updated.
- **`ask_refetches` removes `refetches.out[hash]` itself.** The other three callers already pass
  hashes that aren't out:
  - `refused` and `notfound` get them from `take_refetch`, which removed them;
  - `refetch_votes` skips `out` hashes (`sync.rs:1749`).

  So for those three it is a no-op, and close and timeout behave as before.
- **The other helpers.** `send_batched` is the same loop, `release` is the same per-peer
  decrement, and `signal_votes_of` is the same filter.

### 5. Nothing else changed

Only the six files in the stat changed. `apps/` and the FFI are untouched, and the docs change
covers only D85, D89 and the new D98. Nothing changes beyond the r1 findings and their tests.

## What I ran

All runs used `CARGO_TARGET_DIR=/work/scratch/dw-bl231-r1-sol/target`, `CARGO_BUILD_JOBS=8` and
`RUSTC_WRAPPER=`, in `systemd-run --user … -p MemoryMax=24G`.

- **`cargo test -p dw-governance`** (`r2-test-gov.log`): lib 219/0, golden 3/0,
  oracle_governance 4/0, doctests 0. This includes `doubts_are_bounded_per_proposal_and_in_all`,
  `failed_operator_checks_are_bounded_per_peer` and `a_reencoded_operator_key_keeps_its_votes`.
- **`cargo test -p dw-engine --test mg_governance_contract`** (`r2-test-contract.log`): 9/0.
- **`cargo test -p dw-console --test console_governance`** (`r2-test-console.log`): 16/0.
- **Mutants:** 10 hand mutants, `r2-mut/run.sh`, with diffs and logs in `r2-mut/`. Each was
  applied by `sed`, run with `cargo test -p dw-governance --lib`, and restored by `git checkout`.
  The tree is clean afterwards.

  | Mutant | Result | Killed by |
  |---|---|---|
  | mu1, mark ignores `doubted` | killed | bounds test |
  | mu2, `remove_object` keeps `doubted` | killed | bounds test |
  | mu3, re-encoding counts as a change | killed | re-encoded test |
  | mu4, `failed_under` not moved | killed | re-encoded test |
  | mu5, `drop_changed` doesn't subtract `gone` | **survived** | (n1) |
  | mu6, failed vote parked with its own `held` | killed | 6 tests |
  | mu7, `place_signal` doesn't subtract | **survived** | (n1) |
  | mu8, no failed-check gate | killed | budget test |
  | mu9, failures not counted | killed | budget test |
  | mu10, refused-for-bound makes no doubt | killed | 2 tests |

- **An aside, outside the range:** under mu2, `sync::tests::a_refetched_vote_is_taken_only_from_the_peer_asked`
  also failed once, at `sync_tests.rs:4299` (`honest.hash() == item.hash`). The test builds the same
  vote twice with `vote()`, which stamps `unix_now() - 100`. If a second boundary falls between the
  two calls, the hashes differ. It is a wall-clock flake from 6ece39d4, not this range, and it
  passed in the clean run. Pinning the time in `vote()` would fix it. That is worth a backlog line.
- **Not run:** Playwright, since the range doesn't touch `apps/`, and the regtest suite.
