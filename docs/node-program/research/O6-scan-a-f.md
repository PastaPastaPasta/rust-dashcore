# O6 chain scans (a)–(f): results

Scans (a)–(f) of R4 §2.3.5 over the best chains of the two shared Dash Core v24.0.0-rc.3
oracles' block files, run with `dash-conformance scan` (code: `node/o6` at `264f43e44`; code
identity in `O6-scan-a-f.json`). Predicates cite Dash Core **v24.0.0-rc.4**; `quirks.toml` and
`check-quirks.py` stay on rc.3 until the pin bump. Machine-readable record:
[`O6-scan-a-f.json`](O6-scan-a-f.json) (counts, the scan (b) and (e) rows, scan (c)'s mainnet
rows). Scan (h) is `O6-scan-h.md`.

| Network | Population | Status |
|---|---|---|
| testnet | best chain 0..1,580,928 (8,655,032 txs, 30 blk files) | complete |
| mainnet | best chain 0..2,094,670 (53,395,175 txs, 251 blk files) | **partial: oracle block data ends at 2,094,670; see manager** |

Stale branches are reported separately (R3): testnet has 2 stale blocks (32 txs), with no
hits; mainnet has none linked to the best chain. Every best-chain height was read from the
first record of its hash, and each block's body was checked against its header's merkle root
(no block failed).

## Results

| Scan | Testnet | Mainnet |
|---|---|---|
| (a) `nVersion > 3 && nType != 0`, and framing that `== 3` and `>= 3` disagree on | 0 hits | 0 hits |
| (b) pre-DIP3 type bytes with version < 3 | 0 hits | **3 hits**, all before DIP3 (below) |
| (c) coinbase above the reward, old-budget windows and superblocks | 0 window overpays; 11,980 superblock overpays within their limit; **0 C3** | 222 window overpays (C1); 89 superblock overpays within their limit (C2); **0 C3** |
| (d) CbTx / best-ChainLock fields | 0 hits | 0 hits |
| (e) MNHF signals | 4 signals, 0 flags | 0 signals |
| (f) commitments of types not enabled at their height | 0 hits | 0 hits |

### (b) Pre-DIP3 coinbases with type bytes — mainnet

All three are coinbases (tx 0) whose raw 32-bit version field is `0x20000000`, which parses as
nVersion 0 with nType `0x2000`; each has 1 input and 2 outputs and none uses the one-byte
coinbase scriptSig. DIP3 is at 1,028,160. Quirks 2 and 10 (owner CS3).

| Height | txid | blk file @ record offset |
|---|---|---|
| 842,284 | `53fa8159168128ed8c0d31c2c2b6466f32ca27a69ea6fb114c1655bdfc734ac6` | `blk00034.dat`@128058112 |
| 844,394 | `c811fe2d7b02261e1e0f48f731549d7cef891a27a4595253a44308e58d6ba6b2` | `blk00035.dat`@20882128 |
| 866,404 | `5a78111e6f647204eb25744e022b906342ae4f3563794d1c2383e96a562245fc` | `blk00037.dat`@42627956 |

The three blocks are committed as fixtures (`conformance/tests/fixtures/o6-scan/`, with
`PROVENANCE.md`) and exercised by `tests/scan_real.rs`, for CS3 to reuse as real-chain vectors
for quirks 2 and 10. Testnet has no hit: every testnet transaction with type bytes is at or
after DIP3.

### (c) Coinbase value against the bound

`R` is the oracle's `getblockstats` `subsidy + totalfee`; the superblock limit is
`getsuperblockbudget`. A candidate is a best-chain block in an old-budget window
(`budget_start <= h < superblock_start && h % cycle < window`, `payments.cpp:262-345`) or at a
superblock height. Every candidate's oracle subsidy was also computed with
`dash_consensus::subsidy::block_subsidy_inner` (mainnet 1,817 of 1,817, testnet 65,718 of
65,718 agree).

| | Mainnet | Testnet |
|---|---|---|
| candidates | 1,817 | 65,718 |
| window, `V <= R` | 1,506 (332,323..614,819) | 20 (4,100..4,159) |
| **C1** window, `V > R` (accepted under `NoCheck`) | **222**, heights 332,320..614,813 in 17 of the windows; largest excess 2,529 DASH (height 332,321); total excess 122,434.95 DASH | 0 |
| superblock, `V <= R` | 0 | 53,718 |
| **C2** superblock, `V > R`, within `R + limit` | **89**, heights 631,408..2,093,616; largest excess 8,526 DASH | 11,980; largest excess 1,001.12 DASH |
| **C3** `V > R` anywhere else, or beyond the limit | **0** | **0** |

The mainnet C1 rows begin at 332,320, the first block of the window at 20 × 16,616: coinbase
118,108,031,847 duffs against `R` 508,031,847 (excess 1,176 DASH). Block 332,319, the block
before it, is not a candidate. Both are committed as fixtures. Quirk 14 (owners EV3, CH7a);
the bound itself is quirks 15 and 11 (CS2).

### (d) CbTx and the best-ChainLock rule

Mainnet: 1,066,511 CbTx (versions 1: 60,480; 2: 899,136; 3: 106,895); every version-3
signature is valid, none is zero, other-scheme or malleable. Testnet: 1,573,929 CbTx (versions
1: 71,800; 2: 826,300; 3: 672,900; 4: 2,929), 675,829 signatures, all valid. Neither network has
a `bestCLHeightDiff >= height`, a null signature with a non-zero diff, a null-after-non-null or
older-ChainLock transition (`CheckCbTxBestChainlock`), a CbTx outside `vtx[0]` or before DIP3, a
CbTx version outside the height rules, a wrong CbTx height or a non-canonical encoding.

The version rule includes `CheckCbTx`'s v24 leg (`evo/cbtx.cpp:45-50`): version 4 exactly where
v24 and V20 are both active, version 3 invalid there. The v24 state is read from the oracle's
`getdeploymentinfo` at the anchor tip and recorded in `inputs.v24`:

- testnet: v24 is active from height 1,578,000. The 2,929 version-4 CbTx are exactly the blocks
  1,578,000..1,580,928; none is earlier and no version-3 CbTx is at or after 1,578,000.
- mainnet: v24 is not listed by `getdeploymentinfo`, which leaves out a deployment that is not
  enabled (`NEVER_ACTIVE`; `rpc/blockchain.cpp` `SoftForkDescPushBack`,
  `chainparams.cpp:212-214`), so it is never active: the rule requires version 3 and no version-4
  CbTx exists. Without an oracle the state is unknown and the run is incomplete.

### (e) MNHF signals

Testnet: bit 10 at 1,066,623, bit 11 at 1,140,644, bit 12 at 1,349,780, bit 13 at 1,565,132;
each payload version 1 with a valid basic-scheme signature, no duplicate bit in a block and none
out of bounds: a bit at or above `VERSIONBITS_NUM_BITS` (29, `versionbits.h:22`) is flagged
`e.bit_out_of_bounds` as in `MNHFTxPayload::IsTriviallyValid` (`evo/mnhftx.cpp:95-103`) and gets
no activation verdict. Each is judged by `IsValidMNActivation` (`chainparams.cpp:109-130`, called from
`evo/mnhftx.cpp:124`) with the median time past of the block's parent, read from the best
chain's headers and equal to the oracle's `mediantime` for each of the four (1,721,313,865;
1,731,645,185; 1,761,113,872; 1,790,987,492). All four are valid: only bit 28, the non-EHF
TESTDUMMY deployment, is refused on mainnet and testnet, whatever the time. Mainnet: none.
Quirk 31 (EV3, also CS4).

### (f) Quorum commitments by type and height

Mainnet: 225,760 commitments (LLMQ_50_60 85,339; LLMQ_60_75 44,617; LLMQ_100_67 75,108;
LLMQ_400_60 10,262; LLMQ_400_85 10,434): 93,015 non-null and 132,745 null, every one of an
enabled type at its height. Testnet: 643,580, also 218,365 non-null and 425,215 null, all
enabled. So no commitment of a disabled type after its disable height (f1) and none before its
enable height (f2). Quirks 18, 19, 36 (LQ2) and 34 (BS5). The rule is evaluated at the mined
block's `pprev` (the required-count rule); Core verifies a non-null commitment against its
quorum base block's `pprev`, so a non-null f1/f2 row would not by itself be a commitment Core
rejects.

## Mainnet: the partial record

The mainnet oracle's block data ends at 2,094,670; see manager. The population above is the
best chain from 0 to that height; nothing after it was scanned. The scan reports, as plain
facts about the block files:

- one 14,405-byte region at `blk00250.dat`@20110026 holds no block record; every best-chain
  height to 2,094,670 has a record. The tool therefore exits 2 for mainnet, as documented in
  `conformance/README.md` (an unparsed region of a blk file makes it exit 2).
- of the 2,094,865 records indexed, 2,094,671 are on the best chain to the anchor; the others
  are 56 records above the anchor, 137 not linked to the chain and 1 duplicate.

The mainnet re-run (S9) waits on the manager.

## Method and checks

- **Read-only.** `scan` opens the blk files with `File::open` and reads them with
  `read_exact_at`; each run was a `systemd-run --user` unit with `ReadOnlyPaths=/work/chains`,
  `Nice=19`, idle I/O and `CPUQuota=800%`. `tests/scan_run.rs` checks the directory is
  byte-for-byte unchanged and, under `strace`, that no write-class call touches it. The output
  target is refused if it resolves, symlinks followed, inside the blocks directory or the datadir
  around it, the datadir being taken from the blocks path as given as well as from where it
  resolves (`blocks/` may itself be a symlink).
- **Anchored.** The best chain is pinned to the oracle's tip hash. The oracle also supplied scan
  (c)'s rewards (`getblockstats` by block hash, `getsuperblockbudget`) and the `v24` deployment state
  (`getdeploymentinfo` at the anchor tip); no other call.
- **Independent classifiers.** The CbTx, MNHF and commitment payloads are parsed by hand
  (`src/scan/payload.rs`) and cross-checked against `dashcore`'s decoders on 866,535 real testnet
  transactions (`tests/scan_crosscheck.rs`). BLS fields decode through `dash-pkc`.
- **Fixtures and mutants.** Every scan has positive and negative fixtures; 19 single-predicate
  mutants are each killed by at least one (`mutants_are_killed`).
- **Determinism.** Output is byte-identical across runs and thread counts.
- **Cost.** Mainnet: 33.6 GB of blk files, ~4 min wall at idle priority; testnet 3.9 GB,
  ~4 min (the indexing pass reads headers only).

## What this means for the quirks

| Quirk | Owner | This scan |
|---|---|---|
| 2, 10 | CS3 | (b): the three mainnet coinbases above are real-chain vectors for both |
| 14 | EV3, CH7a | (c): 222 mainnet window overpays (C1), 0 C3 |
| 11, 15 | CS2 | (c): oracle and `dash_consensus` subsidies agree on all 67,535 candidates |
| 31 | EV3, CS4 | (e): 4 testnet signals (bits 10–13), none on mainnet, no flags |
| 16, 22, 29 | CH7a, BS11b, LQ1a | (d): no hit, so no real-chain case for them |
| 18, 19, 36, 34 | LQ2, BS5 | (f): no hit |
| (new 46, 47) | CS3, CH7a | not added: scans (a) and (d) have no hits |

## Reproducing

```sh
B=target/release/dash-conformance
$B scan --net testnet --blocks-dir /work/chains/testnet/data/testnet3/blocks --out OUT/testnet --to 1580928
$B scan --net mainnet --blocks-dir /work/chains/mainnet/data/blocks --out OUT/mainnet --to 2094670
```

The testnet chain is live; its figures are for heights 0..1,580,928 (`--to 1580928`), the tip at
the first run, so that reruns stay comparable. The latest rerun's anchor was a later tip.
