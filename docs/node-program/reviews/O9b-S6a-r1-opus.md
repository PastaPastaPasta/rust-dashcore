# O9b S6a review, round 1 (Opus)

**Verdict: CHANGES-REQUIRED.** 0 major, 3 minor, 3 nits.

**Scope:** `node/o9b-s6a` at `74c47cc96` (six commits on `node/integration` `8465296b5`). The Core reference is v24.0.0-rc.4, read with `git -C ~/workspace/dash show v24.0.0-rc.4:<path>`.

**Why CHANGES-REQUIRED with no major finding:** I found no accept/refuse or hash difference from Core. The port's behaviour is right. The required changes are tests and notes only:

- **m-1:** D22's reason for CLASS_ONLY is wrong. For the two cases it covers, Core's order is fixed, so the headline change of `74c47cc96` (descending order) is pinned by no test.
- **m-2:** three of Validate's own checks have no test. D24 says every Validate check has one.

Both break the program's rule of one test and one killed mutant per check, and both are cheap to fix.

## Findings

### m-1: Core's order is fixed for the two CLASS_ONLY cases, so the `.rev()` that `74c47cc96` adds is pinned by no test

- **Files:**
  - `dash-dml/src/snapshot/tests.rs:1445` (`CLASS_ONLY`)
  - `dash-dml/src/snapshot/validate.rs:137` (`diff.updated.iter().rev()`)
  - `docs/node-program/designs/O9b-S6a-notes.md`, row D22
- **The Core container:** `CDeterministicMNListDiff::updatedMNs` is `std::unordered_map<uint64_t, CDeterministicMNStateDiff, StaticSaltedHasher>` (`evo/deterministicmns.h:635`).
  - The salt is two `GetRand<uint64_t>()` values set at static initialisation, so it changes with every process (`saltedhasher.cpp:8-10`, `saltedhasher.h:77-86`).
  - The map is filled by `emplace` in ascending id order. The decoder enforces that order, at `evo/snapshot.h:362-368`, not at `deterministicmns.cpp:418-435` as D22 cites.
  - Before the replay, `CanonicallySortedCopy` (`snapshot.h:552-557`, called at `snapshot.cpp:167`) copy-constructs the map. `std::sort` then moves it.
  - Core walks it in `ApplyDiffForSnapshot` (`deterministicmns.cpp:420`) and in the two `ValidateMNEncoding` loops (`snapshot.cpp:199`, `:204`).
- **What libstdc++ does with it:**
  - A default-constructed map takes 13 buckets on its first insert. It does not rehash again until the 14th element.
  - `_M_insert_bucket_begin` links each new node directly after the bucket's "before" node. If the bucket is empty, that is the front of the list.
  - For the second element, both cases give the front. If its bucket is empty, the node goes to the front. If it shares the first element's bucket, that bucket's "before" node is `_M_before_begin`, so it also goes to the front.
  - `_Hashtable`'s copy (`_M_assign`) keeps node order, and a move keeps it too.
  - **So a two-update map is always walked larger id first, whatever the salt.** Salt-dependent order starts at three updates, where bucket collisions mod 13 matter. Rehashing starts at 14 updates.
- **Evidence:**
  - **Probe** (`/work/scratch/o9b-s6a-r1-opus/order2.cpp`, built with g++ 15.2.0, the judge's compiler per PROVENANCE): default-constructed map, a random salt per trial, filled ascending, then copied and moved, 20,000 salts per size.

    | Updates | Bucket count | Distinct walk orders |
    |---|---|---|
    | 2 | 13 | 1 (always descending) |
    | 3 | 13 | 2 (descending in 93%) |
    | 4 | 13 | 5 |
    | 13 | 13 | 17,376 |
    | 14 | 29 | rehashed |

    The probe uses a mixing hash in place of SipHash. That doesn't matter for two updates, where the result is the same for any hash.
  - **Core's judge** (`/work/scratch/o9b-s6a/tool3/test_dash_o9b`; its sha256 `9c3cc630…` matches PROVENANCE): I ran it from my scratch directory in 40 separate processes, so 40 different salts. Every run gave the recorded verdict: `history_updates_two_missing` → `id=4`, and `history_updates_duplicate_owner` → the `…0001a5a5…` masternode.
- **Answer to the brief's question:** for these input sizes the order is in practice fixed, and D22's reason (salt, rehash) does not apply to either CLASS_ONLY case. Both cases have exactly two updates (`tests.rs:1166-1167`, `:1366-1369`). Descending order is right for them on libstdc++. It is also what Core does for any map of up to 13 updates with no bucket collision.
- **One caveat for D22, unverified:** libc++ isn't installed here, so I couldn't run this. From libc++'s source, `unordered_map`'s copy constructor re-inserts the source's elements in iteration order. That would reverse a two-update map to ascending. If so, a macOS (libc++) Core build reports `id=3` where the judge's libstdc++ build reports `id=4`. The order is then fixed for each standard library, and differs only between them. Linux and Windows (mingw) release builds use libstdc++.
- **Failing input (surviving mutant):** in the reflink copy, I changed `validate.rs:137` to `diff.updated.iter()`, which is ascending.
  - All 217 dash-dml lib tests still pass.
  - With `CLASS_ONLY` empty, the mutant fails `crafted_verdicts_match_core`: ours `… id=3`, Core `… id=4`.
  - With the real code and `CLASS_ONLY` empty, `crafted_verdicts_match_core` passes. D21's key-value normalisation already handles the owner text.
- **Fix:**
  1. Empty `CLASS_ONLY`, so the two cases compare exactly. This kills the ascending mutant; add it to mutants.py/D24 as M-ORDER.
  2. Rewrite D22:
     - The container and salt as above.
     - "≤2 updates: fixed (descending) on libstdc++. 3–13: descending unless ids collide mod 13. ≥14: rehash."
     - The libstdc++ copy keeps order.
     - The libc++ caveat.
  3. If you want to pin the case that really varies, add a three-update case as the only CLASS_ONLY entry, with its Core verdict.

### m-2: three of Validate's own checks have no test, and a mutant dropping all three survives

- **File:** `dash-dml/src/snapshot/validate.rs`. The three checks:
  - `:245-251`, `oversized evo snapshot collection`
  - `:254-261`, Validate's `historical MN-diff operation budget exceeded`
  - `:270-272`, `oversized CRangesSet range count`
- **Core:** `snapshot.cpp:230-235`, `:239-241`, `:248-250`.
- **Why the corpus can't reach them:** the decoder refuses first, at the same bounds and in two cases with the same text, so no decoded input gets here. They are reachable only through the public `validate`/`hash` on a constructed snapshot. That is the path S6b will use to hash the snapshots the node builds. There, Core's `GetEvoSnapshotHash` throws and ours must too.
- **The claim this breaks:** D24 says "the S6a r1 review added each Validate and replay check". It lists constructed tests for the commitment-size ceiling and the repeated cycle, but not for these three.
- **Failing input (surviving mutant):** in the reflink copy, I prefixed all three conditions with `false &&`. All 217 dash-dml lib tests pass.
  - A concrete input the mutant gets wrong: `populated()` with one historical diff whose `removed` holds `max_historical_mn_operations() + 1` ids. Core's `GetEvoSnapshotHash` throws `historical MN-diff operation budget exceeded`. The mutant goes on into the replay and refuses with a different text, or hashes the snapshot.
- **Fix:** add constructed tests in the style of `validate_bounds_constructed_commitment_sizes`, each with a named mutant in mutants.py/D24.
  - 15 quorum types, or 193 history entries, or 4,097 modifiers, or 30 signals → `oversized evo snapshot collection`. One per term is better, as with the chain terms.
  - The operation budget plus one, as above.
  - 100,001 ranges, if `RangesSet` can be constructed that large; otherwise say why not.
  - The rotation entry's own size terms (`:337-338`, `activeQuorumMembers > MAX_MNS`, skip list `> MAX_SKIPLIST_ENTRIES`) have the same gap. I found this by reading the code and did not run a mutant for them.

### m-3: the 8,000,000 record budget is covered only by its citation

- **Files:** `dash-dml/src/snapshot/mod.rs:46` (`MAX_RECONSTRUCTION_RECORDS`), used at `validate.rs:287`.
- **Core:** `snapshot.h:84`, used as the default argument at `:738` and passed by `snapshot.cpp:265`.
- **What I checked:** the literal matches Core today. M-RECORDS mutates the comparison, which `reconstruct:32/33` checks through the explicit `max_records` parameter. No test reaches the constant itself.
- **Failing input (by inspection; not run):** change `8_000_000` to `9_000_000`, or pass `usize::MAX` at `validate.rs:287`. Every test passes.
  - Take a 100,000-masternode base list with 81 zero-operation diffs, each with a correct `canonical_list_hash`. That is 8,100,000 records, within the 192-list horizon.
  - Core refuses it with `historical MN-list reconstruction record budget exceeded`.
  - The mutant goes on to `missing or extra historical MN-list diff target`.
- **Is it a real gap?** A small one. The parity risk is drift of a single literal, and the gate doesn't catch it. A real fixture is inherently about 8M record visits on each side. The brief's alternative, 160 replays of 50,000 masternodes, costs the same.
- **Fix:**
  - **Cheap:** a Core-judged pair at 100,000 masternodes × 80 and × 81 zero-operation diffs. 80 diffs is exactly 8,000,000 records, so it passes the budget and then fails on the targets. Run it `#[ignore]`d or in a release-only tier, and generate the input; don't store it.
  - **Minimum:** record in D24 that the constant has no mutant, and why.

### n-1: the encoding checks after apply walk the updates ascending, and the apply walks them descending

- **File:** `dash-dml/src/snapshot/validate.rs:203` and `:214`.
- **Core:** `snapshot.cpp:199` and `:204` walk the same `updatedMNs`, in the same order as `ApplyDiffForSnapshot`.
- **Impact:** none I could build.
  - `:203` can't fail on decoded input, because each update already passed the same bounded check at decode.
  - At `:214`, two updated masternodes that both overflow give the same budget text.
- **Fix:** use `.iter().rev()` at both sites, with a comment pointing to D22, so a single order rule covers all three loops.

### n-2: D22's citation for the fill order is wrong

- **File:** D22 in `O9b-S6a-notes.md`.
- **Problem:** it says the map is "filled in ascending id order (`deterministicmns.cpp:418-435`)". Those lines are the walk. The fill is `snapshot.h:362-368`.
- **Also record:** the copy at `snapshot.cpp:167` / `snapshot.h:553`. The order argument depends on that copy keeping order.
- **Fix:** fold both into m-1's rewrite of D22.

### n-3: the storage encoder now allocates a buffer per masternode

- **File:** `dash-dml/src/codec.rs:375-380` (`check_mn_encoding`).
- **Before the split:** `encode_canonical_list` reused one `bytes` buffer, with `bytes.clear()` for each masternode.
- **Now:** `check_mn_encoding` creates a new `Vec` for each masternode, which on a full list is up to 100,000 allocations per call. The behaviour is otherwise identical (see focus 4 below).
- **Why it matters:** PF0's allocation rows have blocked merges before. If any PF0 row encodes canonical lists through this path, it will show.
- **Fix:** pass a scratch buffer, `check_mn_encoding(dmn, &mut bytes)`, from both callers. Or check the PF0 alloc rows before merging.

## The four focus areas

### 1. Accept/refuse parity with Core

I went through each refusal in `snapshot/codec.rs` and `snapshot/validate.rs`, and the reused list and diff readers in `codec.rs:835-962`, against `snapshot.h:89-720`, `snapshot.cpp:30-337`, `deterministicmns.cpp:395-437` and `serialize.h:451-477`. Each one matches Core in its check, bound, text and order:

- **Version, base hash, list:** `snapshot.h:668-678`, `256-297`. The reader's order is the canonical-encoding check → internal id → payouts → network info → list order → prefix run → `AddMN` (`:265-292`). The port has the same order.
- **Count bounds:**
  - quorum types: 14 (`AVAILABLE_LLMQS` at base-sdk `f27bc4f5` has the same 14 entries, and the same `signingActiveQuorumCount`, `keepOldConnections` and `useRotation`, as `llmq/params.h:138-511`)
  - history: 192 lists (computed from that table)
  - operations: 786,432
  - modifiers: 4,096
  - ranges: 100,000
  - MNHF signals: 29
  - every one checks `>` before allocating, like `ReadBoundedCompactSize`
- **Per-type counts:** `SnapshotCommitmentCount`, with no underflow (non-rotated total ≥ active+1; rotated total = 2·active). The rotation flag is checked before the parameter lookup (`:618-627`).
- **Commitments:** in `ReadMinedQuorumCommitment` (`:409-449`):
  - the index is read only for versions 2 and 4
  - the signers bound and the empty check come before the bitset; the valid-members bound and the size equality come before its bitset
  - the BLS scheme is legacy only for versions 1 and 2 (base-sdk `commitment_bls_scheme`)
  - all four BLS fields and `mined_block_hash` are read before the canonical comparison, so a short stream gives `end of data` first in both
  - the per-field comparison is equivalent to Core's hash of `[pk, vvec, sig, sig]`
- **`ReadFixedBitSet`:** `nbytes > remaining` is checked before reading; the padding mask is `~(0xff >> rem)`; a size-0 bitset reads nothing.
- **Rotation entries:** the bitset is bounded by `MAX_MNS`, the skip list by 1,000,000; nothing is preallocated from a claimed count.
- **MNHF signals:** strictly ascending on the wire; Validate's `bit ≥ 29` and height checks.
- **Validate order** (`snapshot.cpp:224-336`):
  - version → base → list invariants → each masternode's encoding → collection sizes → ops → credit amounts → ranges → MNHF → top-level order → replay → per-type checks → targets → modifiers. The port keeps that order.
  - Deriving `required_work_hashes` from `required_modifiers` is exact: Core inserts the same pairs at both sites (`:296-297`, `:319-320`).
- **Replay** (`snapshot.cpp:157-220`):
  - the chain terms, the record charge, merged prefixes, encoding before apply, apply, invariants, encoding after apply, then the hash
  - `ApplyDiffForSnapshot`: the identity check runs before the position is set, then removals → take every update out → put each back → additions → counter
  - each matches
- **Checks the port adds:** only D20's hidden-field refusal. I agree with D20: an applied `CDeterministicMNStateDiff(a, b)` gives exactly `b`, and `b` is a state Core wrote.
- **Checks the port drops:** none.

### 2. CLASS_ONLY

See m-1.

### 3. Hash and encoding

- **Core's hash:** `GetEvoSnapshotHash` re-serialises the object after `Validate(false)` and takes a single SHA256 (`snapshot.cpp:339-347`). `EvoSnapshot::hash` does the same. Nothing hashes the original bytes where Core re-encodes, or the other way round.
- **Canonical checks:** these compare a re-encoding with the consumed bytes (the masternode, the state diff, the commitment's BLS fields), as Core's `CheckCanonicalEncoding` and `bls_input` do.
- **Sort keys:** `uint256` order is `memcmp` on storage bytes, and so is `[u8; 32]` order. History sorts by `Reverse((height, hash))`.
- **Ties:** equal keys are refused by Validate before `hash` encodes anything, so the different sort stability of Rust and Core can't show in a hash.
- **Pinned byte for byte by Core's vectors:**
  - the empty case: `minimal`, `mnlist_empty`, `mndiff_empty`
  - at the limit: `signals_all_bits`, `commitment_full_size`, `mnlist_script_at_budget`, `mnlist_payouts_at_limit`, `mnlist_prefix_run_at_limit`, `credit_locked_max_money`
  - `populated_v3_golden_value`

### 4. The codec.rs split (`fc81e511b`)

- **Behaviour is unchanged.** The parent's `encode_canonical_list` (`codec.rs:317-353` at `8465296b5`) already made two passes, in this order:
  1. all invariants
  2. prefix runs
  3. the counter
  4. a second walk in Core order with `write_dmn`/`read_canonical_dmn`

  `check_canonical_invariants` followed by the walk in `check_canonical_list` is that same sequence. So Sol r1's n-1, which says the parent interleaved the checks for each masternode, does not hold at `8465296b5`, and nothing needs restoring.
- **`read_canonical_state_diff`** is the old inline block, moved unchanged.
- **The only differences:**
  - Read errors now render as Display (Core's text) instead of Debug names. No code or test in the workspace matches the old names.
  - The per-masternode allocation in n-3.

### 5. Tests and mutants

- **Surviving mutants I found:** the ascending walk (m-1, run), the three Validate checks (m-2, run), and the record constant (m-3, by inspection).
- **Corpus coverage:** the "at the limit" cases that end in `end of data` really are one under each decode bound (the read goes on past the count). The "over" cases are one past it.

## What I checked and ran

- **Read:**
  - the full diff `8465296b5..74c47cc96`
  - Core rc.4: `evo/snapshot.{h,cpp}`, `evo/deterministicmns.{h,cpp}` (`ApplyDiffForSnapshot`, `updatedMNs`), `saltedhasher.{h,cpp}`, `serialize.h` (`ReadFixedBitSet`), `llmq/params.h` (the table)
  - base-sdk `f27bc4f5` `pkgs/params/src/llmq.rs`; `commitment_bls_scheme`
- **Ran**, in a reflink copy at `/work/scratch/o9b-s6a-r1-opus/wt` (CARGO_BUILD_JOBS=8, nice 19, TMPDIR inside the copy; left in place for the manager):
  - `cargo test -p dash-dml --lib snapshot::`: 7 passed, 1 ignored.
  - The full lib suite: 217 passed (unmutated; also with each mutant below).
  - `CLASS_ONLY` emptied: passes.
  - `.rev()` removed: 217 passed. With `CLASS_ONLY` emptied as well, it fails on `id=3` vs `id=4`.
  - The three Validate-only checks disabled together: 217 passed.
  - The sources were restored after each probe; `git status` in the copy is clean apart from `tmp/`.
- **libstdc++ order probe:** `/work/scratch/o9b-s6a-r1-opus/order2.cpp`, results in m-1.
- **Core judge:** `gen_crafted` in the copy wrote `crafted.txt`. `test_dash_o9b` judged the three update cases 40 times, from `/work/scratch/o9b-s6a-r1-opus/judge/`. Every run gave the recorded verdict. I wrote nothing under `/work/scratch/o9b-s6a`.
