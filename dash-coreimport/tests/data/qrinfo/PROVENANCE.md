# qrinfo/

Dash Core v24.0.0's `qrinfo` replies and `getqrinfo` refusals on the regtest chain its functional
test `feature_llmq_rotation` leaves, captured 2026-11-19 with `contrib/node/lq3c-qrinfo` (steps in
its README). Nothing here reads a live network.

## Binary and test tree

- `dashcore-24.0.0-x86_64-linux-gnu.tar.gz` from the dashpay/dash v24.0.0 GitHub release, sha256
  `1ffce395e035fb2ee617c92e6eeaab139ca3d10db6f83d4e816136b726512c5e`, checked against the release's
  `SHA256SUMS.asc`. Its `bin/dashd` (`Dash Core version v24.0.0`) has sha256
  `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`, the same file
  `contrib/setup-dashd.py` installs for `DASHVERSION=24.0.0`.
- `test/` and `share/rpcauth` exported from the v24.0.0 tag with `git archive`;
  `feature_llmq_rotation.py --nocleanup` passed. The first run timed out in a DKG phase under
  machine load; the rerun used `--timeout-factor=20`.

## The chain

node0's datadir, copied, run with node0's `dash.conf` (its `-sporkkey` and consensus arguments)
and its other consensus arguments (`contrib/node/lq3c-qrinfo/node.sh`), without the test's mocktime,
so the blocks mined here carry wall-clock times.

- Active chain: tip T = 452 `2b3a3149…a406a9`. The last `llmq_test_dip0024` cycle base is
  c = 432 (`75c5ec5e…6899d`); both of its commitments (bases 432 and 433) were mined at m = 444
  (`1365c24f…3b2f7a`). DIP0003 at 2, V20 and MN_RR at 300, DIP0024 buried at 1 (`deployments`).
- Branches, made with SPORK_19 off from before the first `invalidateblock` to after the last
  `reconsiderblock` (`feature_llmq_rotation.py` does the same around its invalidations), each by
  `invalidateblock(F+1)`, `generatetodescriptor raw(51)`, `reconsiderblock(F+1)`:
  - A: F = 428 (c−4), blocks 429–444, tip `486dae60…7cd6b9`, `valid-fork`;
  - B: F = 420 (c−12), blocks 421–434, tip `356a1e71…3999e4`, `valid-fork`;
  - L: F = 450 (T−2), blocks 451–482, then `invalidateblock` of its first block
    `0fca433f…cb1932`, so it is `invalid` with its data on disk. Core's startup reconsiders the best
    invalid chain only when T is its ancestor (`validation.cpp:4167`), which L's is not.
  - The test's own `bad-qc-dup` block `74fdd235…6bb27a` at 444 (parent 443), `valid-headers`, with
    its data on disk.

Every block was mined by dashd (the test's nodes or `generatetodescriptor` on the copy), except the
`bad-qc-dup` block, which the source test builds itself; it is recorded as the test left it. Every
commitment in the fixture is one Core's run produced: the capture builds and edits none.

After the branches dashd was stopped cleanly; `evodump` (`contrib/node/ev4-serve/evodump`, LevelDB's
C API) read the stopped copy's `evodb/`, and dashd was started again for the capture. The block
index (`blocks`) was read from that run.

## feature_llmq_rotation.json.gz

- `c`, `m`, `tip`, `forks`, `llmq_type` (103), `dkg_interval` (24), `deployments`.
- `blocks`: every block of every `getchaintips` branch as
  `[hash, height, prev, have_data, branch tip status, coinbase hex, txids]`, the active chain first
  by height. `have_data` is whether `getblock` returned the block.
- `evodb`: `[key hex, value hex]` of every `dmn_*`, `q_mc`, `q_mcih`, `q_mcihi` and `llmq_S` row.
  Core keeps a disconnected block's `dmn_D4` and a cycle block's `llmq_S`, so the branches have
  their rows too. 17 of the 34 `llmq_S` keys are `SerializeHash(103, block)` of an indexed block
  (the cycle blocks 144…432, A's and B's 432, L's 456 and 480; `snapshot.cpp:301-318`); the other
  17 hold the same values but match no type and block of the index, so `GetSnapshotForBlock`
  never reads them.
- `replies`: `[case, bases, block, extraShare, protocol version, payload]`: dashd's `qrinfo` reply to
  a P2P `getqrinfo`. One connection per protocol version, its `version` message at that version,
  so the reply is serialized at it and built the legacy way below 70236. The bands are 70221,
  70225, 70227, 70228, 70229, 70230, 70235, 70236 and 70242.
  - S = T (`S=tip`), c (`S=cycle-base`), c+2 (`S=before-mining`), m (`S=mining`), each with every
    base set and `extraShare` 0 and 1. The base sets, for a request block whose cycle base is cs:
    `none`; `one` (cs−53); `three-unsorted` (cs−52, cs−101, cs−28); `duplicate-pair` (cs−52 twice);
    `work-block` (cs−32, the H−C cycle's work block); `above-work` (min(T, cs−5), above every work
    block).
  - 70227 replies can't be read back at 70227, by Core or by base-sdk: Core's writer puts a
    BasicBLS-or-later entry's type fields after `isValid` without the entry's version, and its
    reader takes the entry for LegacyBLS (`evo/simplifiedmns.h` v24:72-101). `qrinfo_serve.rs`
    checks each one as its 70228 sibling written in 70227's layout.
  - `stale-S-answered`: branch A's block at c+2 with the `none` and `one` bases, both `extraShare`
    values. The capture checked each reply equal, byte for byte, to `S=before-mining`'s.

## refusals-feature_llmq_rotation.jsonl

One line per request, each on its own connection at 70242 and at 70235, followed by a `ping`.
`answered` is whether a `qrinfo` came before the `pong`, `log` holds the lines the request added to
`debug.log` (dashd ran with `-debug=net`), and `expect` is the error text the capture required in
Core's `getquorumrotationinfo failed … error=<text>` line. Every refusal here is scored
`Misbehaving` `(0 -> 1)`.

| case | request | 70242 | 70235 |
|---|---|---|---|
| `unknown-base` | base not in the index, S = T | `block <base> not found` | same |
| `unknown-S` | S not in the index | `block not found` | same |
| `low-S-cycle-base` | S = 10 | `Cannot find work block` | same |
| `low-S-first-target` | S = 30 | `Cannot find work block` | same |
| `no-snapshot` | S = 100 (no `llmq_S` row at 72) | `Cannot find quorum snapshot` | same |
| `long-branch-S` | S = L's tip (482) | `Cannot find block` | `block <genesis> and <L's 472> are not in the same chain` |
| `stale-S-above-mining` | S = A's block at m (h(S) ≥ m: cycle c's rows are read) | empty error | empty error |
| `stale-S-work-above-fork` | S = B's block at c+2 | `block <active 400> and <B's 424> …` | `block <genesis> and <B's 424> …` |
| `stale-base` | base = A's block at c+2, S = T | `block <base> is not in the active chain` | same |
| `stale-target-picks-base-above-fork` | base = active 423, S = B's block at c+2 | `block <active 423> and <B's 424> …` | same |

## Cases not produced

- `CannotFindBlock` from a low S: Core never reaches it that way, on any chain. Cycle heights step
  down by 24 from a cycle base, and the cycle at height 0 fails first on its work block (at −8,
  `Cannot find work block`), so no cycle height is ever negative. Here an S below 48 gives that
  error and an S from 48 up meets a cycle without a `llmq_S` row first (`Cannot find quorum
  snapshot`); both are captured (`low-S-*`, `no-snapshot`). `Cannot find block` comes from branch L
  instead, whose first target cycle (456) is above T.
- Unavailable block data (pruning): not reachable on this chain; unit tests cover it.
- A fork below c−24, and a state where the two quorum indices were last mined in different cycles:
  dropped by the plan (round 2). The split-index mutant (4) is unit-tested. The target-cycles-from-S
  mutant (7) is still caught here, through branch L: at `long-branch-S` it reaches the H diff and
  fails "not in the same chain" where Core fails `Cannot find block`.

## Counts

`tests/qrinfo_serve.rs` checks these against the fixture.

| count | n |
|---|---|
| `blocks` | 516 |
| `blocks active` | 453 |
| `blocks invalid` | 32 |
| `blocks valid-fork` | 30 |
| `blocks valid-headers` | 1 |
| `evodb dmn_D4` | 513 |
| `evodb dmn_R1` | 1 |
| `evodb llmq_S` | 34 |
| `evodb llmq_S of an indexed block` | 17 |
| `evodb q_mc` | 11 |
| `evodb q_mcih` | 5 |
| `evodb q_mcihi` | 6 |
| `refusals` | 20 |
| `replies` | 468 |
| `replies 70221` | 52 |
| `replies 70225` | 52 |
| `replies 70227` | 52 |
| `replies 70228` | 52 |
| `replies 70229` | 52 |
| `replies 70230` | 52 |
| `replies 70235` | 52 |
| `replies 70236` | 52 |
| `replies 70242` | 52 |
| `replies S=before-mining` | 108 |
| `replies S=cycle-base` | 108 |
| `replies S=mining` | 108 |
| `replies S=tip` | 108 |
| `replies stale-S-answered` | 36 |
