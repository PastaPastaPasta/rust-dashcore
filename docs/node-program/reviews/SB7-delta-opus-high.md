# SB7 delta review, Opus high: m1–m4 of SB7-impl-opus-high

Reviewed `node/sb7-sml-core` at `646c91641`, rebased onto `node/integration` `c68b1c3e4`. The
minors are the last four commits:
- `a2da13a4b` m1;
- `c6d0ad2e5` m2;
- `aa4c24d54` m3;
- `646c91641` m4.

The work ran in a detached worktree, `/work/scratch/sb7-delta-opus/wt`, with its own target and
`TMPDIR`. The implementer's worktree and branch were not touched, and nothing was pushed.
Core reference: `~/workspace/dash`, the same tree the earlier review used.

## Findings

No Blocker or Major findings. All four minors are resolved, and every mutant the earlier review
named is now killed by a regular test. One process finding (minor) and two nits remain; none of
them asks for a code change on the branch.

### Minor

**p1: the m4 vector was fetched with RPCs that LQ1 §6.3.6 keeps off the live oracles.**
`/work/scratch/sb7/tools/fetch_qroot.py` ran two kinds of call on the live mainnet oracle
(127.0.0.1:27998):
- `protx diff 1 <B-1>` (`BuildSimplifiedMNListDiff`), with post-V20 targets 2,089,172 and
  2,089,193;
- `quorum info` for every active quorum. It reaches `ScanQuorums`/`GetQuorum`, which can rewrite
  `llmq_S` rows for rotated types under `cs_main`.

The B-M5 ruling (`designs/LQ1-quorum-members.md` §6.3.6) puts both on a replica built the O7 way.
Only `getblockhash`, `getblockheader`, `getblock` (≤ 3), `getbestblockhash`, `getblockchaininfo`
and `protx listdiff` may touch the live oracles. As in LQ1's own r0 probe, any row written was
byte-identical, and the calls can't be undone.

The vector itself is right. I re-derived it from pure reads alone (see "m4" below), so it doesn't
need refetching.

Fix (no branch change needed):
- record the deviation in STATUS, like the r0 probe;
- make `fetch_qroot.py` (or its successor) rebuild the active set from `getblock` scans the way
  my script does, so a future refresh stays on pure reads.

The test's provenance comment (`dash-evo/tests/cbtx.rs`) describes the method accurately. It
doesn't claim the method was pure. The script's docstring ("from a read-only mainnet reference
node") overstates it.

Outside this delta: the SB7 memory notes and `/work/scratch/sb7/tools/spot_check.py` show that
earlier SB7 rounds also ran `protx diff` (`fetch_sml.py`, per block) and `protx info` on the
live oracles. That is pre-existing; the manager should decide whether it needs the same STATUS
note.

### Nits

- **n6: stale gate allow-list entry.** `/work/scratch/gate-self-devdep.py` still allow-lists
  `("dash-evo", "test-utils"): "SB7 m1"`. Run on this tree, the script no longer reports dash-evo,
  so the entry is dead. Following the script's own rule ("delete the entry when it lands"),
  remove it when SB7 merges; otherwise a reintroduced self dev-dep would pass silently.
- **n7: the canary's guard is `not(debug_assertions)`, not `not(clippy)`.** The brief allows an
  equivalent guard, and this one is equivalent for every command the gate and CI run:
  - clippy, test and check use the dev or test profile, where the canary is inert;
  - only a release-profile bench or build of the bench with the feature trips it.

  The one gap is `cargo clippy --release --all-features --all-targets`, which nobody runs.
  `not(debug_assertions)` also catches a `--release` build of the bench with the feature, which
  `not(clippy)` would not. Keep it.

## m1: bench without `test-utils`

- `dash-evo/Cargo.toml` has no self dev-dependency. `tests/consumers.rs` has
  `required-features = ["test-utils"]`, and the lock drops the `dash-evo → dash-evo` edge.
- `cargo clippy --locked -p dash-evo --all-features --all-targets -- -D warnings` passes.
  The workspace-wide clippy is the gate's, which passed on this head.
- `cargo bench -p dash-evo --no-run --message-format=json`: the `dash_evo` lib artifacts linked
  into `sml_root` have `features = []`, `opt_level 3` and `debug_assertions false`.
- `cargo bench -p dash-evo --features test-utils --no-run` fails with the canary's
  `compile_error!` at `benches/sml_root.rs:15`, so the canary is live.
- On the gate's side:
  - `cargo metadata` lists `consumers` with `required-features ["test-utils"]`, so
    `gate-feature-tests.py` runs `cargo test -p dash-evo --features test-utils`. Run here:
    `consumers` 4 passed.
  - Without the feature, `cargo test -p dash-evo` skips `consumers`, as intended.
  - `gate-self-devdep.py` passes and lists no dash-evo line (n6).

## m2: memcmp order of proTxHashes

`the_list_root_sorts_protx_hashes_by_memcmp` (`tests/sml_core_vectors.rs`) uses three
proTxHashes whose internal bytes are `a = 01…ff`, `b = 02…00` and `c = 00…80`:
- memcmp order (first byte first) is c, a, b;
- numeric order (`arith_uint256`, last byte most significant) is b, c, a.

The expected root is built by hand: `H(H(c,a), H(b,b))`, where `H` is sha256d of the 64-byte
concatenation, with Core's odd-leaf duplication. `assert_ne!` checks that it differs from the
numeric-order root.

It shares no code with the tree under test:
- the hand-built root uses `dash_consensus::hash::sha256d` directly, not `compute_merkle_root`
  or `MerkleCache`;
- the leaf hashes come from `SmlEntry::hash`, which Core's `simplifiedmns_merkleroots` vector in
  the same file pins.

The test checks three paths: `merkle_root_mn_list`, `SmlList::from_entries` (BTreeMap order) and
`SmlList::upsert` in reverse insertion order (binary search).

Results:
- M04 (numeric sort in `merkle_root_mn_list`) is killed by this test only.
- M04b (numeric `Ord` on `ProTxHash`, which moves `SmlList`) is killed by this test and by the
  existing `types::tests::pro_tx_hash_orders_by_internal_bytes`.

## m3: the version-2 quorum root

`cbtx_root_checks` now runs testnet 900,000's CbTx and checks two things:
- its version is 2;
- the quorum root is computed: the matching root gives `Ok`, and another root gives
  `bad-cbtx-quorummerkleroot`.

I checked the fixture against the testnet oracle with pure `getblockhash` and `getblock 2`. The
block hash, `coinbase_hex` (byte-equal), CbTx version 2 and `merkleRootQuorums` all match.
M14 (`<=` instead of `<`) is killed by `cbtx_root_checks`.

## m4: the mainnet `merkleRootQuorums` vector

**Provenance, re-derived from pure reads only:**
`/work/scratch/sb7-delta-opus/tools/qroot_recompute.py` uses only `getblockhash` and
`getblock 2`, at concurrency 1 under `nice 19` / `ionice -c 3`, with every height ≤ 2,090,000.
It shares no code with `fetch_qroot.py` or with dash-evo.

For each block B it does the following:
1. It parses every coinbase-following transaction's raw hex itself (special version ≥ 3, type 6,
   0 in / 0 out, payload length to the end). It parses the `CFinalCommitment` too: `quorumIndex`
   only for versions 2 and 4, then the two DYNBITSETs and the fixed-size keys and signatures,
   which must consume the payload exactly. Its `signersCount` is cross-checked against Core's
   JSON. The hash is sha256d of the commitment bytes.
2. It rebuilds `GetMinedAndActiveCommitmentsUntilBlock(B-1)` by scanning down from B-1 to
   2,086,000, following Core's rules:
   - non-rotated types keep the newest `signingActiveQuorumCount` by mined height;
   - the rotated 60_75 keeps the newest commitment per `quorumIndex`, and all 32 must be present.

   Mainnet 50_60 is no longer mined: `IsQuorumTypeEnabled` turns it off after
   DIP0024QuorumsHeight 1,738,698. Its 24 still count (A1 quirk 18), so a second window scans
   down from 1,738,800 and finds them down to 1,738,140.
3. It checks that B-1 mines no non-null commitment, then compares the root of that set with B-1's
   CbTx `merkleRootQuorums`.
4. It applies Core's `CalcCbTxMerkleRootQuorums` loop for B and compares the result with B's CbTx
   root.

| Block | B-1 mines | Active set = fixture | Root B-1 (mine = CbTx = fixture) | B mines | Root B (mine = CbTx = fixture) | `qc_txs` = fixture |
|---|---|---|---|---|---|---|
| 2,089,173 | 0 | plain and indexed equal | `fa64b6ff…999b` | 400_60 #0, 400_85 #0 | `f6987e13…537a` | yes (2) |
| 2,089,194 | 0 | plain and indexed equal | `41a4aed1…62db` | 60_75 indices 0–31 | `32bbb585…2c18` | yes (32) |

Both block hashes match the fixture, so the vector is confirmed end to end:
- the hash convention is `SerializeHash` of the commitment, which is sha256d of the payload after
  its 6 bytes of version and height;
- the plain lists are newest first, so the popped entry is the oldest;
- the stale-type inclusion holds.

The vector also covers something the brief didn't list: a type that is no longer mined (50_60,
24 hashes from 2023) still counts toward the root.

**The test itself** (`quorums_root_of_mainnet_blocks`):
- It decodes the raw qc transactions itself (`block_qc`: payload, null check, `extra_payload()[6..]`).
- It self-checks each previous block's root from the active set through
  `merkle_root_quorums_from_hashes`, then checks B's root through `merkle_root_quorums` with
  mainnet V24 chain parameters.
- For each mined non-rotated type, it asserts the list is full and that reversing it (so the pop
  removes the newest) gives a different root.
- `prev_qc_commitments == 0` is read from the fixture. My recompute verifies it independently.

## Rebase onto `c68b1c3e4`

- **Range-diff:** `git range-diff cb7dec621..node/sb7-sml-core-backup-956921df2
  c68b1c3e4..646c91641` shows all 24 commits as `=`, patch-identical.
- **Same files:** the branch touches the same 26 files on both bases.
- **Overlap with integration:** the only files that `cb7dec621..c68b1c3e4` (CH2b b1, docs, BL
  notes) and the branch both touch are `Cargo.lock` and `contrib/node/perfgate/workloads.toml`.
  The branch's changes to both are identical before and after the rebase (compared as `+`/`-`
  line sets):
  - the lock gains `criterion`, `flate2` and `serde_json` for dash-evo, and `dash-evo` for
    dashcore;
  - `workloads.toml` gains the five `sml-root-*` rows in their own block.
- **No code path in common:** CH2b b1 changes `dashd-chainstate`, `dashd-store` and the
  perfgate/selftest scripts. None of those depend on dash-evo or dashcore's `sml`.
  `node-fuzz/Cargo.lock` changed on integration, but node-fuzz doesn't depend on dashcore, so
  the new dashcore → dash-evo edge needs no nested-lock update.

## Mutation table

Each mutant was applied to `dash-evo/src` in the review worktree and run against
`cargo test -p dash-evo --no-fail-fast` (lib, `cbtx`, `cbtx_root_history`, `sml_core_vectors`).
The tree was restored after each run; `git status` was clean at the end.

| # | Mutant | Killed by |
|---|---|---|
| M04 | `merkle_root_mn_list` sorts numerically (reversed bytes) | `the_list_root_sorts_protx_hashes_by_memcmp` (m2) |
| M04b | `ProTxHash` `Ord` numeric (moves `SmlList`'s order) | the m2 test, `types::tests::pro_tx_hash_orders_by_internal_bytes` |
| M08 | non-rotated pop only when `len > count` | `quorums_root_assembly`, `quorums_root_of_mainnet_blocks` (m4) |
| M09 | pop the newest (`remove(0)`) instead of the oldest | `quorums_root_assembly`, `quorums_root_of_mainnet_blocks` (m4) |
| M10a | excess check `>=` | `quorums_root_assembly`, `quorums_root_of_mainnet_blocks` (m4) |
| M10b | excess check `> max + 1` | `quorums_root_assembly` only (a valid block never carries excess, so no real vector can kill it) |
| M14 | quorum root gated at `<= 2` | `cbtx_root_checks` (m3) |
| M21 | a rotated commitment keeps the old hash at its index (`or_insert`) | `quorums_root_assembly`, `quorums_root_of_mainnet_blocks` (m4) |

That is 8 of 8 killed. Every survivor from the earlier review is now killed by a regular test,
and the real vector kills every assembly mutant that a valid block can show.

## Validation

`CARGO_BUILD_JOBS=12`, `nice -n 10`, `--locked`, target and `TMPDIR` under
`/work/scratch/sb7-delta-opus`.

| Run | Result |
|---|---|
| `cargo clippy -p dash-evo --all-features --all-targets -- -D warnings` | pass |
| `cargo bench -p dash-evo --no-run` | pass; lib `features = []` |
| `cargo bench -p dash-evo --features test-utils --no-run` | fails on the canary (expected) |
| `cargo test -p dash-evo` | lib 15, `cbtx` 7, `cbtx_root_history` 31 + 1 ignored, `sml_core_vectors` 2; `consumers` not built |
| `cargo test -p dash-evo --features test-utils` | as above, plus `consumers` 4 |
| `gate-self-devdep.py` on this tree | pass, no dash-evo line |
| testnet 900,000 fixture vs oracle (`getblock 2`) | equal |
| `qroot_recompute.py`, mainnet 2,089,173 and 2,089,194 | ALL OK (pure reads only) |

Oracle access was `getblockhash` and `getblock` only, at concurrency 1, with mainnet heights
≤ 2,090,000. Nothing under `/work/chains` or `/work/bench` was written. The worktree and target
were removed afterwards. The recompute script and its logs stay in
`/work/scratch/sb7-delta-opus/{tools,logs}`. No commit or push was made, and this report is
uncommitted.

**Verdict:** APPROVE-WITH-NITS

m1–m4 are resolved, with no code changes requested. p1 is a process record (STATUS note, and pure
reads for future refreshes). n6 is a gate-script cleanup at merge.
