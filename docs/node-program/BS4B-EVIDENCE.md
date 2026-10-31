# BS4b: decode-domain evidence (base-sdk against Dash Core v24)

**Task:** BS4b (PLAN row; DEC-92 tier S-h). Differential evidence for A1 §1.4 rule DDE on the objects BS4a fixed: `Transaction`, `Block`, `BlockHeader` and the special-transaction payloads of types 1–12.
**Release profile:** v24 only (DEC-82). The four targets run on the v24.0 pin only (`pins` in their `node-l1` entries and in `targets.toml`; `fuzzctl check` compares the two, and the private seed replay runs each target on its own pins). Types 10–12 do not exist on v23.1, and v23.1's payload layouts differ.
**Companion:** `DDE-SWEEP.md` (BS4a's sweep and its fixes). This document is the evidence that the fixed decoders agree with Core over the whole of that domain, by differential execution.

## 1. What is compared

| Target (node-fuzz / `node_l1::TARGETS`) | base-sdk side | Core side (oracle) |
|---|---|---|
| `diff_bsdk_tx_decode` | `Transaction::decode` | `oracle_tx_decode` (O1) |
| `diff_bsdk_block_decode` | `Block::decode` | `oracle_block_decode` (O1) |
| `diff_bsdk_header_decode` | `BlockHeader::decode` | `oracle_header_decode` (O1) |
| `diff_payload_decode` | `SpecialPayload::decode` for one type, under each BLS scheme flag | `oracle_payload_decode` (O2a), once per flag |

Re-encoding is CS0's (`diff_tx_decode` and the other CS0 targets) and is not asserted here.

**Verdict and field values.** The comparison reads Core's field values from its re-serialization of the object it decoded:

- **Verdict:** both decode, or both reject.
- **Consumed:** the number of bytes read (tx, block, header); for a payload, whether the deserializer used every byte (`GetTxPayload` returns nothing unless it did).
- **Values:** for transactions, blocks and headers, every field the base-sdk object holds, chunk by chunk (version, inputs, outputs, lock time, payload bytes, header fields, transaction list), against the same chunk of Core's bytes. For a payload the value comparison goes through base-sdk's own codec: its decoded struct is written back with its codec and compared with Core's re-serialization, so a transposition of two same-width fields in base-sdk's struct (the owner and voting key IDs of a ProRegTx, a commitment's quorum hash and vvec hash) is not visible, because decode and encode use the same order. Named payload fields against Core are BS2's `getblock 2` leg (`conformance/src/verbose.rs`: every block with types 1–4 and 7–12, one block in 1,000 for types 5 and 6). A divergence is reported as the name of the field only (CS0 §9.5).
- **BLS fields (rule DDE #10):** two fields differ in representation by design, and the judge accounts for exactly them, at the field's own position in the codec's bytes. A lazy field (a ProTx operator key) keeps the raw bytes Core read; base-sdk's codec writes zeros once it has found the bytes invalid (BL-44, open: an unchanged invalid lazy key re-encodes as zeros in base-sdk and as its raw bytes in Core; the lazy patch is the judge's accommodation of that divergence, and closing BL-44 means removing it). Because the patch puts the raw bytes back, the comparison cannot see a lazy key's validity (§3.1.1). A field under the process-wide flag (the CbTx and asset-unlock signatures) is written by Core under the flag, as zeros when the bytes are no object in that scheme. The payload target runs both flags (basic and legacy), and each of the two must agree. The position comes from the codec, not from searching for bytes: the lazy key's offset is where two encodings of the payload differ (key unset and key a known valid point), the signatures' offset is counted from the end of the payload. Every other byte must equal Core's.

The judges live in `node-l1/src/bsdk_decode.rs` and take Core's side as plain data (`CoreDecode`, `CorePayload`), so the same code runs live (fuzz, census) and against recorded verdicts.

## 2. DDE rows covered

| Row | Subject | Reached by | Tag |
|---|---|---|---|
| #1 | `TxOut` value is an `i64` (range is `CheckTransaction`'s) | tx and block rows with output values from `i64::MIN` to `i64::MAX` | `DDE#1` |
| #3 | extra-payload length up to `MAX_SIZE` | tx rows on the length prefix at, below and above `MAX_SIZE`; non-canonical and 9-byte forms (`MAX_SIZE`, 2^32, `u64::MAX`); a live test (`extra_payload_at_max_size`, no recorded fixture) compares transactions whose extra payload is `MAX_SIZE` − 1, `MAX_SIZE` and `MAX_SIZE` + 1 bytes (accepted, accepted, refused by both) | `DDE#3` |
| #4 | `LlmqType` is an open byte (named 1–7, 100–108; Core takes any) | commitment payload with 19 type bytes, including 103 and 105 | `DDE#4` |
| #6 | payload consumption: trailing bytes, short payloads, empty | one trailing byte, two trailing bytes, one byte short, empty, for every type × version | `DDE#6` |
| #10 | BLS: zeros, a valid object in each encoding, bytes valid in neither scheme | every BLS field of every version in the floor table (§3.1.1), one row per class; the census and the oracle-free replay both enforce the floor | `DDE#10` |
| #2, #5, #7, #8, #9 | P2P-message rows | not reachable by these decoders; **BS7's** | N/A |

Generic rules (CompactSize canonicality, the `MAX_SIZE` range, `COMPACTSIZE` field type range) have `sweep` rows. `matrix` rows hold one payload for each version of each type, taken from a Core-produced seed where one exists and edited otherwise.

The payload matrix reaches every type 1–12 at versions 0–4 (types 1–4), 0–5 (types 5 and 6, the commitment's own version for 6) and 0–3 (types 7–12). The version just past the last one a type defines is Core's "version it does not know" path. The census extends each to the seeded versions and to the arbitrary values the mutations produce.

## 3. Evidence

All counts are from the committed code. Reproduce with §6.

### 3.1 Rows (named, deterministic)

416 rows (`node-l1/tests/support/bsdk_rows.rs`): 320 accepted by both, 96 rejected by both; 0 divergences.

| Target | DDE#1 | DDE#3 | DDE#4 | DDE#6 | DDE#10 | matrix | sweep | Total |
|---|---|---|---|---|---|---|---|---|
| `diff_bsdk_tx_decode` | 8 | 14 | | | | | 14 | 36 |
| `diff_bsdk_block_decode` | 8 | | | | | | 8 | 16 |
| `diff_bsdk_header_decode` | | | | | | | 5 | 5 |
| `diff_payload_decode` | | | 19 | 96 | 180 | 56 | 8 | 359 |

#### 3.1.1 The DDE #10 floor

Each `DDE#10` row carries its coverage as data (`Row.covers`: payload type, version, BLS field index, class), and `floor_problems` (shared by the oracle-free replay and the census) requires, for every cell of the table below, a row that both sides judged with the outcome the field's kind gives, under both BLS scheme flags. Classes: **zero** (no object); **valid basic** and **valid legacy** (a Core-derived valid point, written in each encoding, which covers the version's own scheme and the opposite one; where the two encodings were the same bytes one row would cover both classes, but none of the 45 fields' encodings coincide); **neither scheme** (bytes canonical in neither). Row names carry only the payload type, version, field and class.

| Type | Field (kind) | Versions | Cells |
|---|---|---|---|
| 1, 3 | operator key (lazy) | 1–3 | 24 |
| 12 | operator key (lazy) | 0–3 | 16 |
| 2, 4 | operator signature (eager) | 1–3 | 24 |
| 5 | best CL signature (global) | 3–5 | 12 |
| 6 | quorum key, quorum signature, members signature (eager) | 0–5 | 72 |
| 7 | MnHF signature (eager, fixed scheme) | 0–3 | 16 |
| 9 | asset-unlock signature (global) | 0–3 | 16 |

The versions are those at which Core reads the field: types 6, 7, 9 and 12 read theirs at every version (the commitment's own version, 0–5), the coinbase commitment from version 3, and types 1–4 at versions 1–3 (the versions on both ends read the version only).

**180 cells** (45 type/version/field triples × 4 classes), each compared under two flags: 360 comparisons. Outcomes, as Core gives them and the floor enforces: zeros and a valid object in either encoding are accepted under both flags for every kind; bytes canonical in neither scheme are accepted where the field is lazy (types 1, 3, 12: the bytes are kept as read) and refused under both flags where it is global or eager. For a lazy field, the comparison enforces acceptance, consumption and byte identity (Core keeps the bytes, the judge restores them where base-sdk's codec wrote zeros for an invalid key, every byte outside the key equals Core's), **not validity**: Core writes the raw bytes whatever they are, and the judge patches the codec's zeros back, so a wrong version-to-scheme choice in base-sdk would still agree. The row builder therefore records base-sdk's view of each lazy row as asserts at build time (`check_lazy_verdict`): the scheme the key captures from the version is Core's rule (legacy for version 1 of types 1 and 3, basic otherwise; this is what a wrong version-to-scheme choice would change, and it does not depend on the point), zeros and bytes canonical in neither scheme are invalid, and a valid point in the encoding of the scheme the version names (legacy for version 1 of types 1 and 3, basic otherwise) is valid; the other scheme's encoding is not asserted (it depends on the point). Core's verdict on an operator key under each version's scheme is EV1's `diff_check_special_tx` (both BLS variants, `bad-protx-operator-key`), and BS2's `getblock 2` leg compares base-sdk's resolved operator key with Core's on chain data under Core's version rule (`conformance/src/verbose.rs`); BS6a's per-field scheme-source unit tests are the non-differential check, and base-sdk's wrapper vectors cover the wrapper algorithm, not the payload's version-to-scheme choice.

**Witnesses.** The chains hold a non-null field for some versions only. Where a seed has one at the version, the row is cut from it. Where not (25 of the 45 triples; the other 20 are cut from seeds), the witness is built: a Core-derived valid point from another seed is carried to that version by re-encoding the version (`with_version`, which for a commitment also adds or drops the index), and is located by its unique occurrence in the payload. The one layout change is the version 3 coinbase commitment, whose seeds hold no signature: the point is written over the null signature before the credit pool balance (position checked against the unique occurrence). A witness that cannot be built fails the row builder, so the floor cannot be met by omission.

**Negative controls** (`bsdk_recorded.rs`, no oracle): keeping one `DDE#10` row per payload type makes `floor_problems` report cells of every type; dropping every row of a class, or of any one (type, version, field), is reported; a row recorded with the other outcome, or one that diverged, is reported for each field kind and class. The weaker check this replaces (a row name beginning with the type) accepted a single row per type.

### 3.2 Census (rows plus seeded mutations)

`cargo test -p node-l1 --features oracle --test l1_bsdk_decode census` compares every row and `BS4B_CENSUS_CASES` seeded mutations (default 40) of every seed: truncate, extend, overwrite a window, bit flip, insert, delete, boundary byte. It fails on any divergence and unless (every one of the 277 seeds is mutated `BS4B_CENSUS_CASES` times):

- every DDE class above was reached, with both outcomes where the class has both;
- every payload type 1–12 at every version of the matrix was compared under both BLS flags, and accepted under both wherever a seed names the version;
- every target saw both outcomes;
- the DDE #10 floor holds on the live comparisons (§3.1.1): every cell has a row compared under both flags with its field kind's outcome.

Transaction versions in the rows, the seeds and the mutations are within `0..=0x7fff`.

With 400 mutations per seed (release build): 0 divergences over

| Target | Both accepted | Both rejected |
|---|---|---|
| `diff_bsdk_tx_decode` | 5,290 | 4,342 |
| `diff_bsdk_block_decode` | 17,115 | 18,035 |
| `diff_bsdk_header_decode` | 20,695 | 14,598 |
| `diff_payload_decode` (inputs; each also run under both flags) | 13,400 | 17,836 |

The default run (40 per seed) is part of the gate (`oracle-diff` step).

### 3.3 Replay without the oracle

`node-l1/tests/data/bsdk_decode_recorded.json` holds Core's verdict on each of the 416 rows: accept/reject, bytes consumed, and Core's re-serialization (per BLS flag for payloads). Each row carries the SHA-256 of its input, so the test can rebuild the bytes by code and prove they are the bytes Core judged (CS3's `checks_recorded.json` pattern). A row's name, target, tag and input digest must all match.

- `recorded_rows_match_the_oracle` (oracle feature): the file equals what the oracle gives now. `NODE_L1_REGENERATE=1` rewrites it; review the diff.
- `bsdk_recorded` (no oracle): rebuilds each row, checks the digest, and runs the same judge against the recorded Core side. It must reach the recorded agreement and the expectation of the row.

### 3.4 node-fuzz

The four targets are registered (owner `node-l1`, kind `l1`, pin `v24.0`, route `private`). Smoke budgets: 60 s each for tx, block and payload; 30 s for header. Nightly weights 4, 4, 1, 4. `fuzzctl check` accepts the registry and `fuzzctl selftest` passes, including a test that a pin mismatch between `targets.toml` and `node_l1::TARGETS` is reported.

Seeds (`public_seeds`: `node-l1/tests/data/bsdk-seeds/<target>/`): 24 transactions, 88 blocks, 88 headers and 77 payloads, covering payload types 1–12 and every payload version Core's chains produce. Nothing is hand-written. `node-l1/scripts/gen-bsdk-seeds.py` cuts them from files that carry their own provenance, keeping for each (type, version) the three payloads with the most non-zero bytes, so that BLS fields that are set are preferred to null ones:

- Core functional-test chains: `dash/tests/data/core_v24_regtest.json`, `conformance/tests/data/*.json`
- dashd v24 `getblock` of mainnet and testnet blocks: `dash-oracle-sys/tests/data/*.json`
- Core's `trivially_valid.json` and `trivially_invalid.json` ProTx rows, and the v24 asset-unlock transactions

`gen-bsdk-seeds.py --check` fails when the committed seeds differ from what the generator writes.

After the merge of `node/integration`, a `node_l1::TARGETS` entry has one `pins: &'static [Pin]` field after `compare`, given as one of the consts `ALL` and `V24` or as a literal; the four targets use `V24`. `fuzzctl check` reads the field in any position, in either form, and compares it with the target's `pins` in `targets.toml`.

### 3.5 Full history through the conformance harness (BS2)

`dash-conformance rpc --features bsdk-judges` (`conformance/src/bsdk_judges.rs`) runs the same judges over every block of the mainnet and testnet v24 oracles' best chains (read-only RPC): `judge_header` and `judge_block` over each header and block, `judge_tx` over each transaction (Core's side is the bytes the chain holds, which Core serialized), and `judge_payload` over each special-transaction payload of types 1–12 under both BLS scheme flags against the oracle's `GetTxPayload`. Records would be `bsdk.*` against base-sdk and name a field only. The payload leg requires every chain payload to be accepted by both sides under both flags and read in full (a best-chain payload that both sides reject, or that is not consumed, is a `bsdk.payload` record against the harness; the first version of the leg counted that as agreement). The table below is the run of the harness at `b368bf46`, before that requirement; the rerun with it is recorded in §3.5.1. The judges (`node-l1/src/bsdk_decode.rs`) and the oracle shim are identical at the head of this branch.

| Chain | Heights | Blocks (headers) | Transactions | Payloads judged (under 2 flags) | By type | `bsdk.*` records | All harness mismatches |
|---|---|---|---|---|---|---|---|
| mainnet | 0 – 2,094,670 (the oracle's tip) | 2,094,671 | 53,395,175 | 1,332,050 | 1: 16,105; 2: 18,354; 3: 4,536; 4: 784; 5: 1,066,511; 6: 225,760 | **0** | 0 |
| testnet | 0 – 1,569,315 (the oracle's tip) | 1,569,316 | 8,585,124 | 2,248,740 | 1: 3,127; 2: 8,371; 3: 928; 4: 181; 5: 1,562,316; 6: 638,001; 7: 4; 8: 34,010; 9: 1,802 | **0** | 0 |

Total: 3,663,987 blocks, 61,980,299 transactions, 3,580,790 payloads (7,161,580 judgements), 0 divergences. Payload types 10–12 (and mainnet's 7–9) occur in neither history; the rows and the census cover them. The RPC mode sees the best chain only (no stale blocks). Reproduce: `cargo build --release --config 'profile.release.package.dashd-masternode.debug-assertions=true' -p dash-conformance --features bsdk-judges`, then `dash-conformance rpc --net <mainnet|testnet> --out <dir> --resume --threads 8` (about 41 and 45 minutes at 860 and 585 blocks/s, under load). Logs and summaries: `/work/scratch/bs4b/full/{mainnet,testnet}/`.

#### 3.5.1 Rerun with the acceptance requirement

The same harness with the stricter payload leg, built from `71df0092` (the judges and shim are those of this branch's head), over the full best chains with `--threads 8` (read-only RPC; the later rule of concurrency 1 and mainnet at or below 2,090,000 postdates this run):

| Chain | Heights | Blocks (headers) | Transactions | Payloads judged (under 2 flags) | `bsdk.*` records | All harness mismatches |
|---|---|---|---|---|---|---|
| mainnet | 0 – 2,094,670 (the oracle's tip) | 2,094,671 | 53,395,175 | 1,332,050 | **0** | 0 |
| testnet | 0 – 1,569,409 (the oracle's tip at the time; it had grown by 94 blocks since the first run) | 1,569,410 | 8,586,780 | 2,248,908 | **0** | 0 |

Every chain payload (types 1–9 occur) was accepted by both sides under both flags and read in full. Short ranges at the same revision (mainnet 1,028,000–1,030,999; testnet 7,000–9,999) also pass with 0 mismatches. Logs: `/work/scratch/bs4b/m2b/`.

### 3.6 Campaign (O3 farm)

The four targets ran on v24.0 through the farm's worker (`fuzzctl farm work`, night `bs4b-r1`, in a detached checkout whose registry weights only these four targets equally; built at `8250ad6a`: this branch's `6d509894` plus the registry weights; the later commits to `71df0092` changed rows, tests, documents and the conformance leg, not the fuzz judges or the targets). It ran at nice 19 and a 4-core quota beside the other farm units, for its 24-hour deadline. A receipt per target (`credited_cpu_seconds`, `total_cpu_seconds`, `divergences`, `crashes`, `sink_failures`, the revisions of the slices) is written by the ledger reader to `$DASH_EMBARGO_DIR/fuzz/.farm/bs4b-r1/receipts/<target>.json`.

Result at the deadline (receipts regenerated at the merged head): 70.45 CPU-hours credited, **0 divergences, 0 crashes, 0 sink failures**.

| Target | CPU-hours credited |
|---|---|
| `diff_bsdk_tx_decode` | 18.20 |
| `diff_bsdk_block_decode` | 17.22 |
| `diff_bsdk_header_decode` | 17.76 |
| `diff_payload_decode` | 17.27 |

The 24 CPU-hour per-target budget was not reached (the box was loaded); DEC-184 replaced such budgets with "ran until plateau, 0 divergences", which §3.7 records.

### 3.7 Plateau certification (FZ1 `fuzzctl certify`)

`fuzzctl certify <target>@v24.0 --seconds 14400 --wall-cap-hours 24 --launch`, from a detached checkout at `64ba2479b` (the farm's priority, one core each). The legs ran together first; the two that came back `growing` were run once more (DEC-186: `growing` means rerun). Under DEC-252 no further reruns are made, and the legs are recorded as they stand.

| Leg | Verdict | Run | CPU-s | Executions | cov | ft | Discovery lines |
|---|---|---|---|---|---|---|---|
| `diff_bsdk_tx_decode` | **certified** | 1 | 14,400.35 | 80,817,236 | 640 → 640 | 2056 → 2056 | 0 |
| `diff_bsdk_header_decode` | **certified** | 1 | 14,401.72 | 95,607,810 | 359 → 359 | 379 → 379 | 0 |
| `diff_bsdk_block_decode` | **not certified** (`growing`) | 1 | 14,401.67 | 59,130,975 | 931 → 931 | 3939 → 3941 (+2) | 2 |
| | | 2 | 14,404.89 | 74,878,136 | 931 → 931 | 3941 → 3952 (+11) | 5 |
| `diff_payload_decode` | **not certified** (`failed`) | 1 (`growing`) | 14,401.78 | 20,624,533 | 3363 → 3363 | 8645 → 8650 (+5) | 5 |
| | | 2 (`failed`) | 14,403.59 | 33,955,244 | 3364 → 3363 (−1) | 8653 → 8659 (+6) | 6 |

cov and ft are the replay before the window and the replay after it. Every run: 0 divergences, 0 crashes, 0 sink failures, no refusals, clean eligibility (no open finding on the pin), and the certificate names `64ba2479b` with a clean tree. The executions are the window's own count (its final stats less the count at `INITED`) from one libFuzzer process, so the double count of BL-86, which arises under `-jobs`, does not apply to this table. It does apply to the farm campaign's totals in §3.6, which ran with `--jobs 2`; those totals are CPU-hours, and no execution count is quoted from them.

- `diff_bsdk_block_decode`: coverage is flat at 931 in both runs; the feature count still rises (+2, then +11). The second run started from the corpus the first left.
- `diff_payload_decode`: the first run was `growing` (ft +5). The second is `failed`, not `growing`: its replay after the window had one cov point fewer than the replay before it (3364 → 3363), which FZ1 treats as not reproducible, and its ft rose by 6. The README notes that loads of one corpus vary by a few features, so the cov point may be replay noise; BL-216 checks that.

Both certified legs and the four legs' zero-divergence results stand as evidence that base-sdk agrees with Core on everything these targets reached. For the two uncertified legs the evidence is the rows, the census, the full-history run (§3.5) and the campaign (§3.6), not a plateau.

## 4. Findings

No divergence between base-sdk and Core in the rows or the census. The node-fuzz smoke found two defects in the judge, both fixed:

- A field a codec re-encodes at another length than the input's (the net-info list of a ProTx) moves the BLS field that follows it. The first judge located the lazy key by searching for its bytes; it is now located by the codec's own offset (§1).
- A BLS signature of one repeated byte matches at several offsets when it is searched for, so the first judge could rewrite its neighbour (the first payload smoke run reported 54 such inputs, all asset unlocks, none a base-sdk divergence). The signatures are now placed by their distance from the end of the payload, and two rows (`signature of one repeated byte`, asset unlock and coinbase commitment) keep the case in the evidence. All 54 inputs now agree.

Two facts about the comparison:

- A version-0 payload is only its version field; one byte short of it still decodes.
- Whether base-sdk judges a lazy key valid is not compared by this target at all (§1, §3.1.1); EV1's `diff_check_special_tx` owns it.

Coverage: the source data holds a non-null BLS field for 20 of the 45 type/version/field triples of the DDE #10 floor (§3.1.1); the other 25 (the quorum commitment at versions 3 and 4 and the version 3 coinbase commitment, whose samples in the seed sources are null, and the versions that no chain data names: the quorum commitment at 0, 2 and 5, the coinbase commitment at 5, and types 7, 9 and 12 at the versions other than those the chains carry) are covered by witnesses built from a Core-derived valid point, and Core is the judge of each. The chains themselves do carry non-null v3 and v4 commitments and v3 coinbase commitments with a ChainLock signature (for example mainnet 2,094,611: v3; 2,094,666 and 2,094,667: v4; 2,094,665: coinbase commitment v3; the testnet chain carries v3 commitments as well), and the full-history run (§3.5) judged every one of them under both flags, so those versions are also covered by real data there.

## 5. Not covered, owners

- Re-encoding and field-level encoders: CS0 (`diff_tx_decode`, `diff_block_decode`, `diff_header_decode`).
- P2P message decoders (DDE #2, #5, #7, #8, #9): BS7.
- v23.1: these targets do not run on it (§ header). The private seed replay runs each target on its own pins.
- Range, semantic and policy checks: rules (CheckTransaction and the consensus checks), not decoders.

## 6. Reproducing

```bash
# the committed seeds are what the generator writes
python3 -I node-l1/scripts/gen-bsdk-seeds.py "$PWD" --check
# rows, recorded verdicts, census (oracle feature; builds the shim)
cargo test -p node-l1 --features oracle --test l1_bsdk_decode
BS4B_CENSUS_CASES=400 cargo test --release --config 'profile.release.package.dashd-masternode.debug-assertions=true' -p node-l1 --features oracle --test l1_bsdk_decode census -- --nocapture
# replay of the recorded verdicts, no oracle
cargo test -p node-l1 --test bsdk_recorded
# regenerate Core's verdicts after a row or shim change, then review the diff
NODE_L1_REGENERATE=1 cargo test -p node-l1 --features oracle --test l1_bsdk_decode recorded_rows_match_the_oracle
# node-fuzz registry and smoke (the private sink is set for the smoke only)
python3 -I node-fuzz/fuzzctl.py check
```

## 7. Decisions

| Row | Decision |
|---|---|
| D1 | A `node_l1::TARGETS` entry has one `pins: &'static [Pin]` field after `compare` (consts `ALL`, `V24`, or a literal); the four targets use `V24`. `fuzzctl check` reads it in any position, as a literal or a const, ignoring comments, and compares it with `targets.toml`. |
| D2 | Two rows moved to the private corpus (PLAN §7 handling). |
| D3 | Under DEC-252 this evidence row merges with its uncertified legs recorded as uncertified (§3.7): `diff_bsdk_tx_decode` and `diff_bsdk_header_decode` certified; `diff_bsdk_block_decode` (`growing`) and `diff_payload_decode` (`failed`) not. The rework is BL-216: structured seeds or mutators for `diff_bsdk_block_decode`, and a check of `diff_payload_decode`'s replay noise. BL-216 must land before these two legs go `required` in the private MANIFEST. |
