# dash-conformance

A differential conformance harness. It decodes every block and transaction of a Dash chain
through two libraries:

- rust-dashcore (`dashcore`, built with X11 block hashes);
- base-sdk (`dash_primitives`, the workspace's pinned fork revision; see
  `docs/node-program/BASE-SDK-FORK.md`).

It then checks both libraries against each other and against a reference dashd (the
"oracle"). A third leg runs the node's own `dash_consensus` wrappers against the same references
(CS0 §9.2). The goal is to prove byte-for-byte equivalence before any type moves from one
library to the other, and to find where either library diverges from Dash Core.

It is `publish = false`, a development tool and not part of the library surface.

## What is checked, per block

| Check | Reference |
|---|---|
| Header decode, X11 block hash | oracle hash (RPC) or the next block's raw `prev_hash` (blk files) |
| Header fields | `getblock 1`, or library vs library |
| Whole-block decode consumes all bytes; re-encode is byte-identical | input bytes |
| Merkle root | `getblock 1`, or the raw header field |
| Per-tx decode, re-encode, txid | input bytes; `getblock 1` txids, or Core's txid rule applied to the bytes (sha256d, with v24's stable asset-unlock txid) |
| Tx fields: version/type, inputs, outputs, lock time, payload bytes | library vs library |
| Special-tx payload fields, mapped field by field (ProRegTx, ProUpServ/Reg/Rev, CbTx, QcTx, MnHF, AssetLock, AssetUnlock, and v24's ProDisTx, ProUpShareTx and ProUpSharedRegTx); BLS fields through their wire images (base-sdk: Core's re-serialization of the decoded object) | library vs library |
| A payload a library cannot interpret (base-sdk `Unknown`, dashcore `RawPayloadType`): `payload.unsupported` against it | the block is valid, so dashd interprets the payload |
| Every field of `getblock 2` (block level, transactions, payloads), in both directions | oracle JSON (sampled blocks) |
| The `dash_consensus` leg (`src/consensus.rs`): `decode_block` consumes the block exactly and finds it canonical (else each transaction is decoded alone to name the one that is not); `core_serialize`, `core_bytes` and `size` give back the bytes; the block hash; every txid and the merkle root, unmutated | oracle hash, `getblock 1` txids and root, or the chain link, Core's txid rule and the header's root; the input bytes |
| The base-sdk decode judges (`src/bsdk_judges.rs`, feature `bsdk-judges`, links the oracle shim; BS4b): node-l1's `judge_header`, `judge_block` and `judge_tx` over each block, header and transaction with the chain's bytes as Core's side, and `judge_payload` over each special-transaction payload of types 1 to 12 under both BLS scheme flags against the oracle's `GetTxPayload`. Records are `bsdk.*` against base-sdk and name a field only; counters `bsdk_blocks`, `bsdk_txs`, `bsdk_payloads` | the input bytes (Core's serialization); the oracle's payload deserializer |

Every leaf of dashd's `getblock 2` JSON falls into exactly one class, by its exact path
(array indices stripped; see `src/verbose.rs`):

- **compared**: each library's decoded value is rendered in dashd's format and compared
  (`verbose.field`); a value dashd prints that a library which decoded the payload did not
  produce is `verbose.missing`. This includes addresses (owner, voting, payout), endpoints
  (`addresses.*`, and `service`/platform ports when dashd prints them), payouts, shares and the
  v24 payload objects;
- **rendering of a compared field** (script `asm`/`desc`/`address`/`type`, decimal amounts,
  payout and share addresses, `instanceHash`): recomputed from dashd's own source field with
  an independent renderer (`src/render.rs`, `src/script.rs`, following Core v24's
  `ScriptToAsmStr`, `Solver`, `InferDescriptor`, base58check/bech32m, `ToStringAddrPort`); a
  difference is `verbose.render`, a harness finding;
- **chain or index data** (fee, spent index, InstantSend/ChainLock state, confirmations, …): an
  explicit list;
- anything else, including an unknown payload object: `harness-gap`, which fails the gate.

An independent splitter (`src/split.rs`) mirrors Core's `UnserializeTransaction`. It finds
transaction boundaries without either library, so a decode failure in one library is pinned
to an exact transaction and the rest of the block is still checked.

Every mismatch is attributed to a side:

- `dashcore` or `base-sdk`: that library disagrees with the bytes or the oracle.
- `both`: both libraries are wrong in the same way.
- `undecided`: the libraries disagree with each other and nothing independent decides.
- `harness`: data or I/O problems, and informational records.
- `dash-consensus`: the node's `dash_consensus` wrappers disagree with the bytes or the
  reference (the `consensus.*` checks).

## Usage

```bash
cargo build --release -p dash-conformance
B=target/release/dash-conformance

# Stream from an oracle over JSON-RPC (defaults: the shared oracles in docs/node-program/ORACLE.md)
$B rpc --net testnet --out /work/conformance/testnet-rpc
$B rpc --net mainnet --out /work/conformance/mainnet-rpc --from 1000000 --to 1100000 --resume

# Read dashd's blk*.dat directly (honours blocks/xor.dat). The best chain is anchored to the
# oracle's tip by default (--no-anchor to skip; regtest anchors only with --rpc-url)
$B blk --net mainnet --blocks-dir /work/chains/mainnet/data/blocks --include-stale \
       --out /work/conformance/mainnet-blk

# Any dashd: --rpc-url http://127.0.0.1:PORT plus --cookie FILE or --rpc-user/--rpc-password
$B rpc --net regtest --rpc-url http://127.0.0.1:19898 --cookie ~/.dashcore/regtest/.cookie --out /tmp/rt

# Rebuild summary.json from an output directory
$B summarize --out /work/conformance/mainnet-rpc

# O6: scan the blk files for the shapes of R4 §2.3.5 (a)-(f); see "Chain scans" below
$B scan --net testnet --blocks-dir /work/chains/testnet/data/testnet3/blocks --out /work/scratch/o6/out/testnet
```

Useful flags:

- `--from` / `--to`: the heights to check. Work is done in units (RPC: chunks of `--chunk`
  heights aligned to multiples of the chunk size; blk mode: whole `blk*.dat` files), and every
  unit holding a requested height is checked whole. An inverted range, or a `--to` above the
  oracle's tip, is an error.
- `--resume`: skip units an earlier run already checked under the same identity (below) and
  whose blocks have not changed since.
- `--threads N`: rayon workers (default: the number of cores, at most 8).
- `--chunk` / `--batch` (RPC): heights per unit, and blocks per JSON-RPC batch.
- `--verbose-every N` and `--verbose-types 1,2,…`: which blocks get the `getblock 2`
  cross-check. This is sampling: fields of unsampled blocks are only compared between the
  libraries and with `getblock 1` (hashes, txids, merkle root, header). Gate fixtures should
  use `--verbose-every 1`; `scripts/regtest-v24.sh` does.
- `--max-lines-per-group`: cap NDJSON lines per mismatch group. The rest are still counted.
- `--no-anchor` (blk): don't use the oracle. The best chain is then simply the highest one,
  which dashd has not necessarily accepted, so a validation finding (`header.pow`,
  `block.check`) on it is not by itself a library bug, and every record not linked to genesis
  is a failing `blk.orphan` (see below).
- `--fail-on-mismatch`: for CI. See the exit status below.

Exit status, for a gate:

- 0: every unit of the requested range is covered under the current identity and, with
  `--fail-on-mismatch`, none has a failing mismatch;
- 1 (with `--fail-on-mismatch`): some unit of the requested range has a failing mismatch,
  whether this run checked it or an earlier one did (a resumed run does not forget them);
- 2: some unit of the range is not covered (a unit failed, or the chain changed during the
  run; re-run with `--resume`), the RPC results in scope are not one chain ending in the run's
  pinned block (see below), mismatch records counted in `progress.ndjson` are missing from
  the log, blk mode found something that means blocks may have been skipped (`blk.scan`,
  `blk.scan_tail`, `blk.orphan`, `blk.range`), or the harness hit an error.

All checks count towards the gate, including harness-side ones (`tx.txid_ref`, …): each means
something was not verified. The exception is the informational `blk.unlinked_*` checks below,
which the gate line counts separately (`N informational`).

### Records not linked to genesis (blk mode)

dashd writes blocks in arrival order. During initial block download it fetches up to 1024
blocks ahead of its tip from several peers, so a block is often written before its parent, and
a fork block can be stored while its parent's data never is. dashd keeps such blocks off its
active chain (`m_blocks_unlinked`), and its own `-reindex` skips a record whose parent never
shows up. With the oracle (not with `--no-anchor`), blk mode asks dashd's block index
(`getblockheader`, `getblockhash`) about every record the files do not link to genesis. Both
libraries must agree on its hash, and dashd must know it with the same parent; then:

- `blk.unlinked_ahead` (informational): it is above the files' best tip and, by dashd's index,
  descends from it, whether dashd has connected it since or still waits for a parent's data.
  It was downloaded ahead of its parent. Blocks above the tip are never checked; a later run
  links (or reclassifies) it.
- `blk.unlinked_stale` (informational): otherwise, it is off dashd's active chain and its
  parent is another unlinked record or has no block data in dashd (`nTx` 0). With
  `--include-stale` it is checked as a stale block at dashd's height, against dashd's hash,
  when that height is at or below the tip.
- `blk.orphan` (failing, incomplete coverage): anything else. dashd does not know the hash or
  the libraries disagree on it, dashd's parent differs, dashd's active chain holds the block
  without it descending from the files' tip (at or below the tip, or after a reorg; re-run),
  or dashd has data for a parent that no record hashes to. Without the oracle every unlinked
  record is a `blk.orphan`.

## Output

Each `--out` directory holds one network, one source, and one RPC chunk size or blocks
directory. `meta.json` enforces this, and a lock file stops two processes from writing to the
same directory. The files are:

- `runs.ndjson`: one line per run, with its identity.
- `mismatches.ndjson`: one record per mismatch. Each record has the network, source, height,
  block hash, tx index and txid, check, side, field, expected and actual values, and detail.
  Records also carry the raw tx hex for transactions up to 16 KiB, as a repro, and are tagged
  with their unit and run.
- `progress.ndjson`: finished units (checkpoints), each with its identity and coverage.
- `invalidated.ndjson`: checkpoints a later run found no longer valid.
- `summary.json`: totals, plus mismatch groups (`check[:field]:side`) with counts, height
  ranges and examples, for the latest run's identity.

### Checkpoints

A checkpoint is reused (`--resume`) and counted only if all of these hold:

- **Same identity.** The identity combines the code, the oracle and the comparison policy.
  - Code: `build.rs` hashes the sources of every workspace crate the harness depends on (the
    harness, `dashcore` and its path dependencies) and the resolved name, version and source of
    every other dependency in `Cargo.lock` (so every base-sdk revision in the build), plus target
    and profile. At run time a digest of the executable is added, which also covers what a build
    script cannot see (Cargo feature unification, compiler version and flags).
  - Oracle (RPC mode): the oracle's version and subversion from `getnetworkinfo`.
  - Policy: the `getblock 2` sampling in RPC mode, and `--include-stale` and anchoring in blk
    mode.

  Changing a library, the harness, the oracle or the policy re-checks everything.
- **Same blocks.** An RPC unit records the hash of its last block and the parent of its first,
  and its blocks must link to each other. Each run first re-reads the hash at every recorded
  unit end and invalidates the units whose block changed (a reorg) or is above the tip. A blk
  unit records a signature over every selected block's location, both libraries' hashes, `prev_hash`, height, best-chain
  membership and reference hash, plus the file's size, mtime and inode. Each run recomputes the
  signatures and invalidates the units whose file changed. A block is also re-framed when it is
  read, so a file truncated or replaced since indexing fails its unit instead of being misread.
- **Same format.** Checkpoints carry a format number (now 2). Older ones (format 1 had no
  parent hash) are never reused: they are `unverified` until re-checked.

**One chain (RPC mode).** dashd can reorganise between any two of a run's calls, including
the separate `getblockhash` reads of the unit ends above, so those reads alone do not show
that reused and new results describe the same chain. Each RPC run therefore pins one block
when it starts: the oracle's block at the scope's last height (its tip by default; the height
comes from `getblockheader` of `getbestblockhash`, so the two agree). The last unit of the
scope must end in it. When the gate is decided, every result it counts for a unit of the
scope must cover exactly that unit's heights and end hash, and, in height order, the parent
of each unit's first block must be the previous unit's end hash. Last, `getblockhash` at the
pinned height must still return the pinned block. Block hashes commit to their parents, so
the counted results are then all on the chain ending in the pinned block, and that block is
on the oracle's chain when the gate is decided. Anything else exits 2 (`not one chain: …`);
the next `--resume` run, on a settled chain, invalidates the results the reorg replaced and
re-checks them. The tip growing during a run does not move the pinned block, so it does not
fail the gate.

The summary never drops a result for blocks that were not re-checked. Within a unit group (an
RPC chunk, a blk file), a later unit replaces an earlier one only when its coverage includes
the earlier one's; otherwise both are counted. Results of another identity, or invalidated
ones, that no current unit of the same policy re-checked are listed under `unverified` instead
of being counted or forgotten (a weaker policy never clears a stronger one's findings). A
unit's mismatch records are synced to disk before the unit is recorded as done, and a gate
fails closed if the counts in `progress.ndjson` exceed the records in the log. An output
directory written by an older harness keeps working: all its records are `unverified` until
the next run re-checks them.

## Chain scans

`scan` reads `blocks/` read-only (it opens files for reading only, and `tests/scan_run.rs`
checks this under `strace`) and classifies the best chain's blocks for the shapes of R4 §2.3.5
(a)-(f): typed transactions above version 3, pre-DIP3 type bytes, coinbases above the reward in
the old budget windows, CbTx best-chainlock fields, MNHF signals, and quorum commitments of
disabled or not-yet-enabled types. Stale branches are classified apart (`stale_hit_counts`).
The oracle gives the chain anchor and, for scan (c), `getblockstats` rewards and
`getsuperblockbudget`; `--no-oracle` skips both and lists scan (c)'s candidates unclassified.

It writes `scan-<letter>.ndjson` (one hit per line; hits found in a block carry the blk file and
record offset it came from, which the ChainLock-sequence hits of scan (d) and
`c.subsidy_mismatch` do not) and `summary.json` (counts, inputs, examples, `incomplete`). The
output is byte-identical across runs and thread counts on unchanged input. Notes on reading it:

- Scan (d)'s ChainLock sequence rules need the previous block's coinbase, so they are not
  evaluated at the `--from` block.
- Scan (d) judges CbTx's v24 leg (version 4 exactly when v24 and V20 are both active,
  `evo/cbtx.cpp:45-50`) against the oracle's `getdeploymentinfo` at the anchor tip: a listed,
  active `v24` gives the height it applies from, a listed inactive one means no scanned block
  is v24-active, and an unlisted one is `NEVER_ACTIVE` (`summary.json` `inputs.v24`). Without
  an oracle the state is unknown, versions 3 and 4 are let through at V20, and the run is
  incomplete.
- Scan (e) applies `IsValidMNActivation` (`chainparams.cpp:109-130`) with the median time past
  of the signal block's parent, read from the best chain's headers (`e.invalid_activation`; a
  verdict that would depend on a time it lacks, as for a stale block, is
  `e.activation_undecided`).
- Scan (f) evaluates `IsQuorumTypeEnabled` at the mined block's `pprev` (the rule for which
  types a block must carry). Core verifies a non-null commitment against its quorum base
  block's `pprev`, so a non-null `f1`/`f2` row is not by itself a commitment Core rejects.
- Each block read is checked against its header's merkle root; the first copy of a duplicated
  block is the one read.

Exit status is 0 when the scan is complete and 2 when it is not: a block that could not be
framed or does not match its header's merkle root, a blk file that changed under the scan (the
newest file may grow), a region of a blk file that holds no block record (named in
`incomplete` with its file, offset and size), `--to` above the chain tip, records not linked to
the chain scanned when there is no oracle anchor, or scan (c) without an oracle. `--from`
above the tip, and an `--out` inside the blocks directory, are errors. An unparsed region does not mean a best-chain height is missing;
`inputs` and `incomplete` say what was seen.

## Tests

- `tests/scan_run.rs` runs `scan` on a synthetic `blocks/` directory (best chain, a stale
  sibling, an orphan): classification, determinism, and that nothing in the directory is
  written. `tests/scan_crosscheck.rs` compares the scan's payload parsers with `dashcore`'s
  decoders on real testnet blocks (skipped where the oracle's directory is absent). The
  classifiers have positive and negative fixtures and in-process mutants in `src/scan/tests.rs`.
- `tests/fixtures.rs` uses real testnet blocks to check that clean blocks produce no
  mismatches. It also injects faults (oracle hash/txid/header changes, edited `getblock 2`
  JSON, an unknown special type, a payload length prefix that disagrees with the payload) and
  checks that each one is caught and attributed to the right side.
- `tests/fixtures.rs` also covers the `getblock 2` classification: wrong endpoints and key
  addresses, deprecated service fields, an unknown payload object (`harness-gap`), share
  fields, script renderings versus script bytes, block-level fields, the asset-unlock
  `instanceHash` and fee, and that every committed fixture's JSON is fully classified.
- `tests/blk_mode.rs` runs blk mode end to end on a synthetic chain with stale, duplicate and
  orphan blocks. It covers the gate on resume, narrower requests, a policy change and a
  same-size block replacement. It also classifies unlinked records against a mock of dashd's
  block index: blocks downloaded ahead of their parent and stale branches whose base dashd
  never stored (informational, the stale ones checked) at each stage of dashd's download,
  each failing class, and a resumed run whose classification changed.
- `tests/block_index.rs` reads dashd's block index through the RPC client from replies the
  testnet oracle gave (`tests/rpc-replies/`, HTTP status and error codes included).
- `tests/rpc_mode.rs` runs the binary against a mock oracle serving a synthetic chain. It
  checks the exit status of fresh and resumed gates, a policy change, a reorg (invalidation and
  re-check of exactly the affected chunks), a chain that changes during a unit, and range
  validation. It also reorganises the mock between the reads of a run: between the boundary
  reads of a resumed run (review r2's mixed-chain false pass), a boundary read answered from
  another chain (caught by the links between units alone), and after a run's last block,
  shallow and deep, against tip growth at the same point, which passes.
- `tests/txid_rule.rs` checks the txid reference against dashd's txids on every fixture
  (including a v2 asset unlock), and that RPC and blk mode judge txids alike.
- `tests/v24_findings.rs` takes blocks from regtest chains produced by Dash Core
  v24.0.0-rc.3's functional tests and snapshots today's library bugs on v24 payloads (now all
  in base-sdk). A fix in either library has to update this snapshot.
- `verbose::tests` renders every transaction of dashcore's Core-produced v24 vectors
  (`dash/tests/data/core_v24_*.json`) and compares the result with Core's JSON for it, both
  ways, including types 10–12 and the payloads Core cannot read.

`scripts/regtest-v24.sh` regenerates those chains by running Dash Core's v24 functional tests
(at a given tag, against the matching binaries) and checks every block of each chain over RPC.

`scripts/oracle-pass.sh` runs one incremental pass (RPC and blk mode, both networks) over the
shared oracles at idle priority. A systemd user timer runs it every 2 hours (see ORACLE.md).

Results and the bug list are in `docs/node-program/research/BS2-conformance-results.md`.
