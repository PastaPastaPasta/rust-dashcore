# O9b S6a review, round 2 (Opus, narrow)

**Verdict: CHANGES-REQUIRED.** 0 major, 1 minor, 1 nit.

**Scope:** `node/o9b-s6a`, `74c47cc96..56e2cf36b` (one commit, signed `G`). The Core reference is v24.0.0-rc.4, read with `git -C ~/workspace/dash show v24.0.0-rc.4:<path>`.

**Summary:**
- Every fix the brief lists is in, and the ones I could check against Core hold. m-1, n-1, n-2, n-3 and m-3 are closed.
- The order claims are now backed by the libstdc++ judge itself: 300 processes for each case (below).
- m-3's construction reaches exactly 8,000,000 records under Core's charging rule. Core's two verdicts pin the constant from both sides.
- No fix changes accept/refuse behaviour. `.rev()` on the two encoding loops can't change any refusal text, and the buffer reuse is behaviour-neutral.
- **m-2 is only partly closed.** The eight checks r1 named now have tests and mutants. But D24 now claims a complete list of "Validate checks that decoded input meets first", and that list misses seven more such terms. None of them has a test, so a mutant dropping any of them survives (m-1 below). The fix is tests and notes only, the same pattern again.

## Findings

### m-1: D24's list of Validate-only checks is incomplete; seven decoder-twin terms have no test and no mutant

- **Files:**
  - `dash-dml/src/snapshot/validate.rs:241`, `:246`, `:50-51`, `:302`, `:306-307`
  - `docs/node-program/designs/O9b-S6a-notes.md`, D24 ("Ten Validate checks repeat checks that decoded input meets first: …")
- **The rule this breaks:** one refusal test and one killed mutant per check (D24's own reason column). The brief also says "D24 lists every Validate-only check".
- **What's missing.** Each term below sits in `Validate` and has a decoder twin that refuses decoded input first. So no crafted case reaches it, and no constructed test sets it.
  - The first grep found `invalid params-derived evo per-type quorum counts` only in `rotation_snapshot_on_plain_type`, which takes the plain-type branch.
  - `validate_bounds_constructed_commitment_sizes` sets only equal, non-zero sizes.
  - The second grep found no test calling `validate` on a changed version or base list.

| # | Port | Core | Decoder twin | Concrete input: `populated()` changed by | Core (`validate(false)`) | With the term dropped or loosened by one |
|---|---|---|---|---|---|---|
| a | `validate.rs:50` (`signers != valid_members`) | `snapshot.cpp:49` | `snapshot.h:427-430` | `quorums[0].active_commitments[0]`: signers 8 bits, valid members 16 bits | `invalid evo quorum commitment sizes` | validates and hashes (nothing later looks at the sizes) |
| b | `validate.rs:51` (`num_bits == 0`) | `snapshot.cpp:50` | `snapshot.h:423-425` | the same commitment, both bitsets 0 bits | same | validates and hashes |
| c | `validate.rs:306` (active) | `snapshot.cpp:286` | `snapshot.h:630` | LLMQ_TEST's `active_commitments` with `[0]` pushed once more (`signingActiveQuorumCount + 1` entries) | `invalid params-derived evo per-type quorum counts` | `duplicate evo quorum base hash` |
| d | `validate.rs:307` (safety) | `snapshot.cpp:287` | `snapshot.h:635` | LLMQ_TEST's `safety_commitments` padded with clones to `total - active + 1` | same | `duplicate evo quorum base hash` |
| e | `validate.rs:302` (`ROTATION_CYCLES`, rotated branch) | `snapshot.cpp:288` | `snapshot.h:640` | the rotated type's `rotation_snapshots` padded with clones of `[0]` to 5 | same | `invalid evo quorum rotation snapshot` (repeated cycle) |
| f | `validate.rs:241` (version) | `snapshot.cpp:224` | `snapshot.h:669` | `version = 2` | `unsupported evo snapshot version` | validates and hashes |
| g | `validate.rs:246` (`check_canonical_list` of the base list) | `snapshot.cpp:228-229` | the list reader, `snapshot.h:256-297` | a base-list masternode that no diff touches, given `script_payout = vec![0x6a; MAX_MN_COMPACT_ITEMS_USIZE - 3]` (the `history_update_over_budget` recipe) | `canonical MN nested CompactSize budget exceeded` | `historical MN-list diff hash mismatch` |

- **Notes on the table:**
  - (e): M-SNAPCOUNT adds 1 to the whole comparison, and the plain case kills it. A mutant on the rotated branch's constant alone (`ROTATION_CYCLES + 1`) survives.
  - (g): for a nested-collection violation (payouts over 32), the replay's post-apply `check_canonical_invariants` gives the same text, so a dropped call can't be seen that way. The encoding case can, because the post-apply encoding loop covers only the updated masternodes.
  - (a), (b) and (f): Core's `GetEvoSnapshotHash` throws, and the mutant hashes. That is the S6b path D24 itself cites ("S6b hashes the snapshots the node builds").
- **Not run:** every mutant here is by inspection. `/work` had 195G free, below the brief's 230G floor, so I built nothing.
- **Fix:**
  1. Add rows (a)–(e) to `validate_bounds_constructed_collections`, or to a sibling table, at the limit and at the limit plus one where that applies. For (a) and (b), the refusing input is a mismatch or an empty bitset.
  2. Add (f) and (g) as one-line constructed refusals.
  3. Add a mutant for each: say M-V-SIZEEQ, M-V-EMPTY, M-V-ACTIVE, M-V-SAFETY, M-V-ROTCYCLES, M-V-VERSION and M-V-BASELIST. (g)'s mutant drops the call at `validate.rs:246` only. `check_canonical_list` is shared with the storage encoder (`codec.rs:322`).
  4. Fix D24's count and list (17, not 10).

### n-1: two new lines go past the 100-column width

- **Where:**
  - `dash-dml/src/snapshot/tests.rs:1643`, the `record_budget_cases` doc comment, 115 columns
  - `PROVENANCE.md:47`, 108 columns
- **Why it matters:** `rustfmt.toml` sets `max_width = comment_width = 100`. rustfmt doesn't wrap comments here, so CI won't catch it.
- **Fix:** rewrap both.

## The r1 findings, one by one

### m-1 (order, CLASS_ONLY, D22): CLOSED

- **The two cases are exact:**
  - `CLASS_ONLY` is `["history_updates_three_missing"]` (`tests.rs:1451`).
  - `history_updates_two_missing` and `history_updates_duplicate_owner` go through `assert_eq!(ours, core)`.
- **M-ORDER is pinned:** its pattern (`mutants.py`, `"        for (id, state) in diff.updated.iter().rev() {"`) matches only the apply walk (`validate.rs:137`). The two encoding loops spell their bindings `(_, state)` and `(id, _)`. `mutants.py` asserts one match.
  - Ascending order gives `id=3` against Core's `id=4`, and the other masternode for the duplicate owner, so both exact cases kill it.
- **The judge, re-run:** `/work/scratch/o9b-s6a/tool3/test_dash_o9b` (sha256 `9c3cc630…`, matching PROVENANCE), one process per run, so one salt per run.

  | Case | Runs | Verdicts |
  |---|---|---|
  | `history_updates_two_missing` | 300 | `id=4` in all 300 |
  | `history_updates_duplicate_owner` | 300 | `…0001a5a5…` in all 300 |
  | `history_updates_three_missing` | 300 | `id=90` in 279, `id=4` in 21 |

- **The three-update case really varies with the salt.** Inserting 3, then 4, then 90 into 13 buckets walks as `[4, 90, 3]` exactly when `bucket(90) == bucket(3) != bucket(4)`. That probability is (1/13)(12/13) ≈ 7.1%, and the judge shows 7.0%.
  - The recorded verdict (`id=90`) is the 93% outcome, and it is also the port's descending walk.
  - CLASS_ONLY is minimal: no single case with three or more updates can be exact, and every other multi-update case has exactly two (`tests.rs:1169`, `:1375-1376`).
- **How I built the three-update input:** from r1's `two_missing` hex, I changed `02 03 00 04 00` to `03 03 00 04 00 5a 00`. Its byte-reversed sha256 is `32c038b4…f279`, the same as `verdicts.txt`'s, so it is the implementer's input byte for byte.
- **D22** says what r1 asked: the salt, the fill order, copy and move keep order, ≤2 fixed, 3–13 depend on the salt, rehash from 14, libstdc++ is the oracle, and the libc++ caveat is marked unverified.
  - One wording nit, not raised as a finding: "ids share a bucket mod 13" means hashes mod 13.

### n-1 (encoding loops): CLOSED

- `validate.rs:205` and `:216` walk `.iter().rev()`, as Core's `snapshot.cpp:199` and `:204` walk the same map as `deterministicmns.cpp:420`.
- No mutant pins them, and none can.
  - `:205` can't fail on decoded input.
  - At `:216`, the refusal text names no masternode.
- This is fine.

### n-2 (citations): CLOSED

Each citation checked against rc.4:

| Citation | What's there |
|---|---|
| `deterministicmns.h:635` | `updatedMNs` |
| `saltedhasher.cpp:8` | the `GetRand` salt |
| `saltedhasher.cpp:10` | the static `StaticSaltedHasher::s` |
| `snapshot.h:361-368` | the update read, `emplace` at `:368` |
| `snapshot.h:553-557` | `CanonicallySortedCopy` (`:552` is its `template` line) |
| `snapshot.cpp:167` | the copy |
| `deterministicmns.cpp:420` | the walk |
| `snapshot.cpp:199`, `:204` | the two encoding loops |

### m-2 (Validate-only bounds): PARTLY CLOSED (see m-1 above)

The eight checks r1 named are each tested at the limit and at the limit plus one (`tests.rs:1476-1567`), and each has an M-V-* mutant. The mutant patterns are each unique in `validate.rs`, and none touches the decoder's twin.

### m-3 (the 8,000,000 constant): CLOSED

- **Core's rule:** `records_processed += current.GetCounts().total() + addedMNs.size()`, charged before the apply (`snapshot.cpp:182-185`). The port's `current.len()` is `GetAllMNsCount()` (`list.rs:330`).
- **The replay, newest first:**
  - Heights 499 down to 420 are diffs 1–80. Each sees 100,000 masternodes before its apply, so 80 × 100,000 = 8,000,000.
  - Diff 80 removes ids 1..99,999 after it is charged.
  - Diff 81 (height 419) sees one masternode, so 8,000,001.
  - The 81-diff snapshot's first 80 diffs are the 80-diff snapshot's.
- **Core's own verdicts pin the constant exactly:**
  - At 8,000,000 Core goes on past the budget, through all 80 replays (every `canonical_list_hash` matched, or it would say `diff hash mismatch`), and refuses on `missing or extra historical MN-list diff target`. So the constant is ≥ 8,000,000.
  - At 8,000,001 it refuses on the budget. So the constant is < 8,000,001.
  - The constant plus one fails `records_over_constant`; the constant minus one fails `records_at_constant`.
- **"Refused later on the targets" is the same refusal on both sides.**
  - With no quorums, Core's required set is empty and the 80 lists are extra (`snapshot.cpp:323-325`).
  - The port has the same order: an empty per-type loop, then the targets check (`validate.rs:350-354`).
  - The texts match: Core's is Validate's own throw, so it has one `iostream error` suffix, and the test strips it.
- **The constant has one use:** `MAX_RECONSTRUCTION_RECORDS` is used only at `validate.rs:289`, which is where M-RECORDS-CONST patches it.

### n-3 (buffer reuse): CLOSED

- `check_mn_encoding(dmn, &mut bytes)` clears, then writes into the caller's buffer.
- `check_canonical_list` (`codec.rs:334`) and the replay (`validate.rs:176`) each make one buffer.
- No behaviour change.

## Determinism and default-suite cost

- **The new default test, `validate_bounds_constructed_collections`,** is deterministic: it builds from `populated()`, with no randomness, time or environment. Its largest inputs:
  - 100,001 `RangesSet::add` calls into a `BTreeSet`, which is O(log n)
  - a 1,000,001-entry skip list, about 4 MB
  - 786k removed ids, where the replay fails at the first missing id
  - Nothing is encoded or hashed. That should be well under a second in debug. I didn't time it, since I built nothing.
- **`record_budget_cases()`** runs only from the ignored `gen_crafted` and `record_budget_constant_matches_core`.
  - The default `crafted_verdicts_match_core` counts their two verdict lines and doesn't build them.
  - Their generation is deterministic, because the ignored test checks each input's hash against `verdicts.txt`.

## What I checked and ran

- **Read:**
  - the full diff `74c47cc96..56e2cf36b`
  - `validate.rs` (apply, replay, Validate)
  - the `mutants.py` patterns
  - the new tests
  - D19, D22 and D24
- **Core rc.4:**
  - `evo/snapshot.cpp:30-53`, `:92-102`, `:157-337`
  - `evo/snapshot.h:250-300`, `:340-375`, `:405-450`, `:550-557`, `:612-645`, `:660-722`
  - `evo/deterministicmns.cpp:395-437`, `deterministicmns.h:635`
  - `saltedhasher.{h,cpp}`
- **Ran:** Core's judge, 900 processes in all (table above), from `/work/scratch/o9b-s6a-r2-opus/judge/`, at nice 19 with TMPDIR there. I left it in place and wrote nothing under `/work/scratch/o9b-s6a`.
- **Not run:**
  - The Rust build, tests and mutants. `df -BG /work` showed 195G free, below the 230G floor.
  - So the implementer's "218 pass, 51 killed" and the roughly 4-minute release test are unverified here, and m-1's mutants are by inspection.
