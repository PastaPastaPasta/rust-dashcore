# EvoSnapshot vectors and verdicts (O9b S6a)

Made by Dash Core's own code: `contrib/node/o9b-evo-snapshot/build.sh` links
`evo_snapshot_vectors.cpp` into a copy of test_dash from a fresh, blob-verified build of
v24.0.0. The file's fixture block is `src/test/evo_snapshot_tests.cpp` lines 1-229 at the
tag, verbatim.

The build's MANIFEST (2026-11-05, agentbox). The tree was built fresh and anchored; this binary
was linked from that same tree with `O9B_REUSE=1`, which verified it against the commit's blobs
and the anchor first (`build reused`):

```
tag v24.0.0
commit 938fba502824b9d850b7c710601456dc3d71603e
tree 36748356538aa645ddd166e4bba8f854ac8a5b12
build reused
anchor e49edef2498bcd4e4b9ad884690983e2ca511b121f2d533e35b6163e325a19ab
source 578e678e436ef98ef77d9159208ddc6209470d2fb1cc6a86dd356265444cb05a evo_snapshot_vectors.cpp
core-test b375050448677e9d54bc4da5a494c94549887ea7 src/test/evo_snapshot_tests.cpp
compiler g++ (Ubuntu 15.2.0-16ubuntu1) 15.2.0
binary 826748955c94cfcd8d5ef6522cec684e0abb2140d88c185bafbfef266b7357fc
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
