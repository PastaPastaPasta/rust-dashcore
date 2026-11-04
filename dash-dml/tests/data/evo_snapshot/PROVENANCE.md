# EvoSnapshot vectors and verdicts (O9b S6a)

Made by Dash Core's own code: `contrib/node/o9b-evo-snapshot/build.sh` links
`evo_snapshot_vectors.cpp` into a copy of test_dash from a fresh, blob-verified build of
v24.0.0-rc.4. The file's fixture block is `src/test/evo_snapshot_tests.cpp` lines 1-226 at the
tag, verbatim.

The build's MANIFEST (2026-10-29, agentbox). The tree was built fresh and anchored; this binary
was linked from that same tree with `O9B_REUSE=1`, which verified it against the commit's blobs
and the anchor first (`build reused`):

```
tag v24.0.0-rc.4
commit 0e7382df80d0bc6811068d076b232a5a26d34a3c
tree b83245b9383ed93d9167add1c6a8b538830b44f5
build reused
anchor 9ee5ff5e0132ec7bbf04230c832ea6b5effda40cdb4c989f7b7f254a2420419c
source 174ca450c1dd42697d29ce7c659fb059b8cbb7cb540a885e7053a133ce0b1810 evo_snapshot_vectors.cpp
core-test f5ab9a732e988ecb20778ba395f069ddfcc94b0e src/test/evo_snapshot_tests.cpp
compiler g++ (Ubuntu 15.2.0-16ubuntu1) 15.2.0
binary 9c3cc63009f4b40ccf573ee3780ad684c79e51565594ea0b33e5b483267f8334
```

## The files

- **`vectors.txt`**: Core's dump of its fixtures, one line each: `<name> <kind> <hex> <verdict>`.
  - The fixtures are `SyntheticSnapshot()` (forward and reverse representation), a default
    snapshot, a list-only snapshot and one with MNHF signals.
  - Also dumped: the populated list, its historical diffs, its commitments, its rotation snapshots,
    and `rotation_bitset_larger_than_quorum_roundtrips`'s entry.
  - `populated` carries the hash Core's `populated_v3_golden_value` pins.
  - Made by:

    ```
    O9B_DUMP=vectors.txt test_dash_o9b --run_test=o9b_evo_snapshot_vectors/dump
    ```

- **`verdicts.txt`**: Core's verdict on each crafted input: `<name> <kind> <sha256 of the input>
  <verdict>`.
  - Kinds are `vectors.txt`'s, plus `reconstruct:<budget>`: the snapshot decoded, then its
    history replayed again by `ReconstructHistoricalMNLists` under that record budget (`ok`
    ends with the number of lists).
  - The inputs are defined once, in `src/snapshot/tests.rs`'s `crafted()`. They are regenerated at
    test time and checked against the recorded hash, so the larger ones (about 8 MB of hex in all)
    are not stored. The two `records_*` cases (`record_budget_cases()`, 100,000 masternodes each)
    are checked only by the ignored `record_budget_constant_matches_core`, in release; the
    generator wants release too. The test checks the kind as well, so a case can't be judged
    as one kind and tested as another (a `reconstruct` budget, say).
  - Made by:

    ```
    O9B_CRAFTED=crafted.txt cargo test --release -p dash-dml --lib snapshot::tests::gen_crafted -- --ignored
    O9B_JUDGE=crafted.txt O9B_VERDICTS=verdicts.txt test_dash_o9b --run_test=o9b_evo_snapshot_vectors/judge
    ```

## Verdict format

A verdict is one of:

- `ok <unread bytes> <sha256 of the re-encoding> [<GetEvoSnapshotHash> <CanonicalMNListHash>]`
- `fail <exception class> <what()>`

Hashes are `uint256::ToString`, which is the bytes reversed, in hex.

The tests compare after two normalizations:

- They strip libstdc++'s `: iostream error` suffix. `Validate` rethrows a replay error's `what()`,
  so those errors carry the suffix twice.
- They read Core's `DataStream::read(): end of data` as `end of data`.

## Regenerating

Rerun `build.sh` (or `O9B_REUSE=1` on an anchored tree), then the commands above. If the
generator changes, regenerate `verdicts.txt`: `crafted_verdicts_match_core` refuses an input whose
hash differs from the recorded one, and refuses a stale count.
