# Core-judged codec vectors

Written by `contrib/node/ev2a-codec-probe/regen.py`: every byte and verdict is Dash Core v24's
own code (the probe links the v24.0 oracle pin; each file's header has its MANIFEST).

- `lists.txt`: lists Core built with `AddMN`/`UpdateMN`/`RemoveMN` (every state version, both
  types, a 64-bit hash collision bucket, an empty list) and the first 48 masternodes, in Core's
  iteration order, of the mainnet lists after blocks 1,030,852 and 1,868,231. Those lists'
  content is the mainnet v24 oracle's `protx listdiff` (a pure read), encoded as evodb stores it
  by `dash-oracle-sys/scripts/dml-listdiff-check.py`, with that script's internal ids and
  counter; Core decoded and cut them.
- `diffs.txt`: `BuildDiff` between those lists; `apply.txt`: `ApplyDiff` of each.
- `verdicts.txt`: Core's canonical readers on seeded mutations of the canonical bytes.
- `budget.txt`: Core's canonical list reader on the nested CompactSize budget cases (EV2a fix
  r1): one-entry lists from `budget_cases` in `src/codec/tests.rs` whose version-3 entry
  exhausts the budget early in one nested field, written by
  `contrib/node/ev2a-codec-probe/budget.py`. The inputs are ours; every verdict is Core's.
