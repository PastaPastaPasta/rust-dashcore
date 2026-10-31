# Provenance of `dash-consensus/tests/data`

Everything here was produced by Dash Core: through the consensus oracle (`dash-oracle-sys`, O1's
crate, ABI 4), which links Core's own functions from the two pinned releases (DEC-05), and, for
`history/`, from the headers of the BS2 reference nodes.

| Pin | Core commit (shim branch head) | Release |
|---|---|---|
| `v24.0` | `23769258c239` (`node/oracle-shim-v24.0`) | v24.0.0-rc.3, the one pin (DEC-82) |

The data was recorded on two pins, v23.1 (v23.1.8, commit `350ce242a59c`, `node/oracle-shim-v23.1`) and
v24.0, and the recorder checked that they agreed. DEC-82 dropped v23.1: the v23.1-only files
(`chainparams/v23.1/`, `coins/regtest-undo-v23.1.json`) are deleted, and the agreement checks below
were made when both pins existed.

Nothing was typed by hand. The recorder is CS2's differential harness,
`contrib/node/cs2-diff` (its `README.md` has every command). It draws the inputs from a fixed
seed (`0xC5200001`), runs each on both pins, checks the port against Core's answers, and writes
Core's answers:

```bash
contrib/node/cs2-diff/run.sh record dash-consensus/tests/data          # rewrite
contrib/node/cs2-diff/run.sh record dash-consensus/tests/data --check  # compare, exit 1 on a difference
```

The corpus was first recorded on the ABI 1 pins (`6d5c9ac2ed1b`, `684cb425e2bb`). Re-run on the
pins above, every file came out byte-identical except the `core_version` strings of
`chainparams/` (which name the shim commit) and the changes of CS2's review round 1: the
reallocation periods appended to `quirks.json`, and `payments_limit.json`, which records Core's
`GetPaymentsLimit` itself and replaces `payments_limit_treasury.json` (whose limit `nBits` the
recorder had chosen).

## `chainparams/<pin>/<name>.json`

`oracle_chainparams_json` (Core's `CChainParams` and `Consensus::Params`), verbatim, for these
chain selections:

| Name | Core chain and arguments |
|---|---|
| `main`, `test`, `regtest` | `main`, `test`, `regtest` |
| `devnet` | `devnet -devnet=cs2` |
| `devnet-spacing{1,30,60,86400}` | `devnet -devnet=cs2 -powtargetspacing=<n>` |
| `devnet-mindiff4000` | `devnet -devnet=cs2 -minimumdifficultyblocks=4000` |
| `devnet-mindiff4000-spacing86400` | the same with `-powtargetspacing=86400` |
| `devnet-highsubsidy` | `devnet -devnet=cs2 -highsubsidyblocks=500 -highsubsidyfactor=10` |
| `devnet-highsubsidy-maxfactor` | `... -highsubsidyblocks=300000 -highsubsidyfactor=184467440` |
| `devnet-highsubsidy-negative` | `... -highsubsidyblocks=-5 -highsubsidyfactor=-3` |
| `devnet-highsubsidy-wrap` | `... -highsubsidyblocks=10 -highsubsidyfactor=2147483647` (`500 * COIN * factor` leaves int64: Core's compiled code wraps) |
| `devnet-highsubsidy-unused` | `... -highsubsidyblocks=0 -highsubsidyfactor=200000000` (a factor that never applies) |
| `regtest-brr` | `regtest -dip3params=30:50 -testactivationheight=brr@1000 -testactivationheight=v20@1200 -testactivationheight=mn_rr@2200` (Core's `block_reward_reallocation_tests` setup) |
| `regtest-budget` | `regtest -budgetparams=10:20:0 -testactivationheight=v20@700 -testactivationheight=mn_rr@700` |
| `devnet-mindiff4000-spacing{86401,178956971,536870911,536870912,536870913,2147483648,128102389400760775}` | `devnet -devnet=cs2 -minimumdifficultyblocks=4000 -powtargetspacing=<n>`: spacings above the timespan, which Core runs because the BTC rule is unreachable (CS2 §2.5): just above it; a DGW divisor above `u32::MAX`; 2^29 − 1, 2^29, 2^29 + 1 and 2^31, around and past where DGW's clamped timespan stops fitting `operator*=(uint32_t)` (it truncates to 0, 8 and 0); and ⌊i64::MAX/72⌋. Recorded at v24.0 like every chain beyond the base four; the windows run on both pins |

For every chain the recorder checked that both pins gave the same parameter views (`v23.1/` held
the four base chains and is deleted). The tests build each selection's `ChainParams` with `dash-params`'
`ChainParamsBuilder` and the arguments above (`tests/common/mod.rs` `selection`), and build the
views with the crate's own `from_chain_params`. They build the views again from Core's raw fields
copied into parameters scrambled in every other field, and require both to be equal, so the
builder's values are Core's and the views read only fields Core's dump holds. The hard-coded pins
(the 68589 tolerance) are `dash-params`' own; quirk 15's `nBits` and the network paths the views
derive themselves.

## `oracle/*.json`

Each case was run on **both pins** (except `split_amount_by_shares.json`: v23.1.8 lacks
`SplitAmountByShares`); they agreed on every case (the recorder fails otherwise), so
one answer is stored. `null` means the oracle refused the input because Core asserts on it or
overflows (`ErrorKind::Argument`). A header window shorter than what Core reads
(`ErrorKind::Context`) is drawn again. Fields named after the input (`chain`, `label`,
`superblock`) are the recorder's labels; the evidence is Core's answers.

| File | Core function | Inputs |
|---|---|---|
| `next_work.json` | `GetNextWorkRequired` over a header window (`first_height`, `[nTime, nBits]` per header, the candidate's `nTime`) | 150 cases per pow-relevant chain: random and adversarial `nBits`, non-monotonic times, gaps around 2, 4 spacings and 2 h, retarget boundaries, the KGW/DGW switch heights, `nMinimumDifficultyBlocks`; windows of at most 600 headers, so no KGW case (KGW's are `kgw_boundaries.json` and the history sample) |
| `check_proof_of_work.json` | `CheckProofOfWork` | 60 per chain: hashes at the target, ±1, random and zero; negative, overflowing, zero and above-powLimit `nBits` |
| `subsidy.json` | `GetBlockSubsidyInner` (`block`) and `GetSuperblockSubsidyInner` (`treasury`); `block_bits_1` and `block_bits_207fffff` are `GetBlockSubsidyInner` at the same height with those `nBits`, Core's own witness of whether the answer depends on the difficulty | 120 per chain: the formula-era, decline, treasury, high-subsidy and V20 boundaries, `prev_bits` of 1, powLimit and random |
| `masternode_payment.json` | `GetMasternodePayment` (v23.1.8 with `fV20Active = era != Classic`) | 80 per chain: the increase steps, BRR, the reallocation start and periods; block values over Core's domain `[INT64_MIN/600, INT64_MAX/600]` |
| `platform_share.json` | `PlatformShare` | the `MoneyRange` edge and the overflow bounds, random values |
| `payments_limit.json` | `CSuperblock::GetPaymentsLimit` (`limit`, O1b's entry point), and `GetSuperblockSubsidyInner(b, h - 1, h >= V20Height)` for `b` = 1 and the powLimit compact (`treasury_bits_1`, `treasury_bits_pow_limit`), quirk 15's two candidates | per chain: superblock heights around the first superblock and V20Height, the height after each, heights before the first |
| `quirks.json` | `GetNextWorkRequired`, `GetMasternodePayment`, `ConvertBitsToDouble`, `GetBlockSubsidyInner` and `GetSuperblockSubsidyInner` | labelled cases for the quirk tests: 8 and 12, including the first height of each of the 20 reallocation periods on mainnet (the 19 entries of `vecPeriods`, then the cap); 9, the tolerance's boundary pairs with Core's two doubles (`n1`, `n2`) and the verdict (`accepts`) of the recorder's literal restatement of Core's predicate, `!(abs(n1 - n2) > n1 * 0.5)`, on them (see `tolerance.json`); 11, mainnet subsidies across the early-difficulty, formula, decline and treasury boundaries |
| `noncanonical.json` | `CheckProofOfWork`, `GetNextWorkRequired` | the three encodings of 2^232 (`0x1e010000`, `0x1f000100`, `0x20000001`), their sign-bit and overflowing neighbours, at hash = target − 1, target and target + 1 on main, test and regtest; BTC and min-difficulty windows that return a raw non-canonical word, and a DGW window over them (CS2 §6.1) |
| `kgw_boundaries.json` | `GetNextWorkRequired` over a KGW boundary window, as `[m, a, nBits]` | for every mass m in 14..=4032, the window of `tests/common::kgw_boundary_window(m, a)` at `a` = `A_fast(m)`, `A_fast(m) + 1`, `A_slow(m) − 1` and `A_slow(m)`, the port's thresholds (16,076 cases), CS2 §2.4's differential. Recorded with the host glibc's FMA `pow` in Core's KGW, which the recorder checks; Core's `nBits` differ across every pair below m = 4032 |
| `convert_bits_to_double.json` | `ConvertBitsToDouble`, as `[nBits, hex(double)]` | every size byte with mantissas 0, 1, 0xffff, 0x7fffff, 0x800000, 0x800001 and 0xffffff, the encodings of 2^232, and 1000 random words. The sweep over every word is `cs2-diff exhaustive bits` (below) |
| `tolerance.json` | `ConvertBitsToDouble` of both words; `accepts` is not Core's output but the recorder's literal restatement of Core's predicate, `!(abs(n1 - n2) > n1 * 0.5)`, on Core's doubles, as `[header nBits, expected nBits, accepts]` | 1000 random pairs: unrelated words, neighbours, one flipped bit (CS2 §6.2 `diff_nbits_acceptable`). Every release build compiles the predicate as written (`CS2-fp-release-binaries.md` §3.3), and the history sweep has Core accepting every mainnet header to 68589; Core's own verdict on a rejected pair waits for O2a's header validation |
| `next_work_large_spacing.json` | `GetNextWorkRequired`, as `next_work.json` | 100 windows per large-spacing devnet (above): DGW windows with arbitrary gaps between the `uint32` times, a few around `nMinimumDifficultyBlocks`. DGW's divisor `24 · spacing` is above `u32::MAX` from 178,956,971 (Core's fallback to its general division), and from 2^29 the clamped timespan does not fit `operator*=(uint32_t)` (CS2 §6.2). Drawn from a seed of their own (`0xC5200001 ^ 0x10`), so the files above keep theirs |
| `split_amount_by_shares.json` | `SplitAmountByShares` (O1b; v24.0 only, v23.1.8 lacks it), as `[total, shares, amounts]` | 300 cases of 1–8 positive shares, mostly of a real collateral's size (products with the total above 2^64), totals up to 1000 DASH with awkward remainders; and a zero share total over two shares, where Core throws (`null`) (CS2 §6.2 `diff_payout_split` a). Same seed as the line above |
| `versionbits.json` (CS4) | `VersionBitsCache::State`, `StateSinceHeight`, `Statistics` and `ComputeBlockVersion`, and `CMNHFManager::GetSignalsStage`, run in each case's order against one cache (`oracle_versionbits_state`, ABI 12, v24.0 only; shim branch `node/cs4-oracle-versionbits-v24.0`) | `node-l1`'s `versionbits_record` over `tests/support/versionbits_rows.rs`: the dynamic threshold's table (attempts 0–16 for nine (window, start, min, falloff) shapes, L0), regtest's own v24/evo_shares schedule swept up and down, the signal height's query order, quirk 31's stage expiry (CS4's part), quirk 32's floor on devnet and testnet and its threshold overflow at attempt 46341, and the state edges (always/never active, timeout, min_activation_height, exactly the threshold, the top mask, MTP vs block time, mainnet, genesis). The recorder checks the node's answers against Core's before writing; `--check` re-records in memory |

## `history/{mainnet,testnet}.json`

`contrib/node/cs2-diff/run.sh sample <main|test> <headers.bin> <out>`, from header dumps of the
BS2 reference nodes' best header chains (`contrib/node/cs2-diff/dump_headers.py`, read-only RPC):

| Dump | Headers | sha256 |
|---|---|---|
| mainnet | 0..=2551766 | `58e6e286297e40e9b9e3afa1e5a734c27bd76be065631fb27d3a5653bf5f8251` |
| testnet | 0..=1568459 | `a88526024d3c3a2a82c797bd6348a8789ce5ee388ecf33742f3987e414659b27` |

Either dump can be rebuilt byte for byte from the reference node once it has moved past the
dump's tip: `dump_headers.py <net> <out> <params.json> --until <tip> --sha256 <sha256>` (the
commands are in `contrib/node/cs2-diff/README.md`). Both were rebuilt that way on 2026-10-08,
with the nodes' best headers at 2551928 and 1568638, and matched. `cs2-diff sweep` and `sample`
refuse any other dump unless given `--unpinned`, and `sweep` requires the recorded tally
(`RECORDED` in `contrib/node/cs2-diff/src/sweep.rs`).

Each range holds Core's headers (`[nTime, nBits, hash]`, the hash in internal byte order, taken
from the next header's `hashPrevBlock`, given only for the checked heights), from as far back as
Core reads for them (BTC's 576-block interval, KGW's 4032 blocks, DGW's 24), and for each checked
height Core's `GetNextWorkRequired` and its `GetBlockSubsidyInner`/`GetSuperblockSubsidyInner` on
both pins. The ranges cover genesis and the first retarget, BTC→KGW, KGW→DGW (mainnet), BTC→DGW
(testnet), the end of the early-mainnet difficulty (4500), the subsidy formulas' height bounds
(5465, 17000, 24000), the first decline (210241) and treasury share (328010) on mainnet, the end
of mainnet's tolerance, V20 and the dump's tip. The recorder checked that each range's window
gives Core's answer at every checked height.

`tolerance` (mainnet) is Core's evidence for the ≤ 68589 tolerance pin: of the headers at heights
1..=68589, 34,467 carry `nBits` that differ from Core's `GetNextWorkRequired`, and Core accepted
them; the highest is at 68588. That bounds the pin from below. The bound from above, Core's
`ContextualCheckBlockHeader` rejecting different bits at 68590, needs CS3's L1 target
(`diff_contextual_check_block_header`, O2a); until then the pin's upper side rests on the
`// Core:` anchor (`validation.cpp:4476`, v23.1.8 `:4110`).

## Exhaustive sweeps

Not recorded data: CS2 §6.3's merge gate, run against Core's compiled code on both pins (O1's
in-process oracle) by `contrib/node/cs2-diff`'s release build, which keeps debug assertions and
overflow checks on. A rerun is the same deterministic command; `contrib/node/cs2-diff/README.md`
has the commands and their run times.

| Command | Inputs | Result |
|---|---|---|
| `exhaustive bits 8` | every `nBits` word, 0..2^32 | `fp::Difficulty::from_bits` equals Core's `ConvertBitsToDouble` bit for bit on v24.0 and v23.1.8 for all 4,294,967,296 words. Each word's difficulty and early-mainnet difficulty also went through the three subsidy formulas, every base in [0, 1111] (`fp`'s debug assertion) |
| `exhaustive subsidy <chain> <prev_height>` | every word as the previous block's `nBits` before V20, at six (chain, previous height) points, each in 16 slices of 2^28 words: main 4500 (the early-mainnet difficulty, first formula), main 5000 (first formula), main 10000 (the CPU formula), main 20000 (the CPU formula for a difficulty ≤ 75, else the GPU one), main 30000 (the GPU formula), test 4500 (the first formula on the full difficulty, where mainnet would take the early one) | the port's `GetBlockSubsidyInner` equals Core's on both pins at all 6 × 2^32 = 25,769,803,776 inputs |

Both ran on 2026-10-08, each binary built from a commit of `node/cs2-fp`:

- `exhaustive bits`: `d58327fa`, the branch's final code on `node/integration` after CS0's
  merge, from a clean tree, in 16 slices of 2^28 words. Binary sha256
  `d9a2799cd0bbd499cbaecdfdf857dd498735b0823fb08e19e50fd7ebe2196ab6`.
- `exhaustive subsidy`: `b0299fc9`, before that rebase, with an uncommitted change to
  `cs2-diff`'s `sweep.rs` (the history sweep, which `exhaustive subsidy` does not run). Binary
  sha256 `9f680c5e5e7f96acd09a837eb28da6afdbd4e3ee8bb029fd1cf2596a5a900ec9`. The code it
  exercises is the same at
  `d58327fa`: `params.rs` and `fp/kgw_pow.rs` byte for byte; `Difficulty`'s `from_bits`,
  `from_bits_mainnet_early`, `at_most_75` and three `subsidy_base_*`, `fp::truncate`,
  `SubsidyParams::from_chain_params`, `block_subsidy_inner`, `decline`, `pre_v20_base` and
  `clamp` function for function; `block_subsidy_helper` but for its comments (v23.1.8 anchors).

A later code change to any of those functions reruns the subsidy sweep, and any rebuild of `fp`
against other code (a merge) reruns the bits sweep, which takes minutes.

Both reran on 2026-10-09 for the combined BS5+BS6a re-pin (base SDK `a1d4dd3`, DEC-72), which
rebuilds `fp` against new code and changes `SubsidyParams::from_chain_params` (DEC-75): binary
sha256 `b69cac4139ff65bf3a03b3510db650739e888e06cbe50c5a3f016e6eea8ecd10`, built from `81117ec8`
on `node/repin-bs5-bs6a`, whose `dash-consensus`, `cs2-diff` and `dash-oracle-sys` are byte for
byte those of the branch's final commit. `exhaustive bits 8`: equal for all 4,294,967,296 words.
`exhaustive subsidy`: the same six points in 16 slices each, 96 of 96 exit 0, equal at all
25,769,803,776 inputs.

## `coins/regtest-undo-v24.0.json` (CS5a)

Undo records Dash Core wrote on a regtest chain, recorded by
`contrib/node/coins-undo-vectors.py` on 2026-10-08 with the release binaries:

| File | Release | `dashd` sha256 |
|---|---|---|
| `regtest-undo-v24.0.json` | v24.0.0-rc.3 (`/work/chains/bin/dashcore-24.0.0-rc.3`, tarball sha256 `9327f000…69ff52` matching the release's `SHA256SUMS.asc`) | `e204e3163185…7ab5bb4` |

```bash
PYTHONPATH=<dir with a dash_hash stub> contrib/node/coins-undo-vectors.py --dashd <dashd> \
    --framework <git archive of test/functional at the release's tag> \
    --datadir <empty dir> --port <p> --rpcport <p+1> --out regtest-undo-<pin>.json
```

The script mines 101 blocks to `raw(51)`, funds twelve script kinds from two coinbases (block
102) and spends them all with a third coinbase and an output created in the same block (block
103), through `generateblock`, so only consensus applies. Signatures are RFC 6979. For blocks
101–103 each file holds Core's raw block (`getblock 0`), the undo record and its checksum copied
from `blocks/rev00000.dat` after `stop`, and the prevouts of `getblock <hash> 3`, which Core reads
from that undo record. Block hex and checksums differ between the files (timestamps, and the
parent hash each checksum binds); the undo records and prevouts are byte-identical, and a re-run
of the script reproduces them byte for byte.

The mainnet records the tests also read are `dash-coreimport/tests/data/mainnet-0-1300/` (its
own `PROVENANCE.md`).

## `locktime/regtest-v24.0.json` (CS5b)

Dash Core's verdicts on BIP68, BIP112 and BIP113 boundaries, recorded by
`contrib/node/locktime-vectors.py` on 2026-10-09 with the v24.0.0-rc.3 release `dashd` (the same
binary as `regtest-undo-v24.0.json` above, sha256 `e204e3163185…7ab5bb4`) and Core's
`test/functional` at `v24.0.0-rc.3`:

```bash
PYTHONPATH=<dir with a dash_hash stub> contrib/node/locktime-vectors.py --dashd <dashd> \
    --framework <git archive of test/functional at v24.0.0-rc.3> \
    --datadir <empty dir> --port <p> --rpcport <p+1> --out regtest-v24.0.json
```

On a mock clock (block `h` at 1,700,000,000 + 600·`h`, blocks 100 and 114 8 s and 313 s later
so that two time locks end exactly one second before the cutoff and at it) the script mines to
`raw(51)`, funds P2SH(`OP_TRUE`) coins at heights 106 and 120 and P2SH(`<n> OP_CHECKSEQUENCEVERIFY
OP_DROP OP_TRUE`) coins at 106, and mines to height 140. For 34 transactions spending them, judged
for block 141, the file holds Core's `testmempoolaccept` verdict and its `generateblock … false`
(`TestBlockValidity`) verdict, the coins' heights and scripts, and headers 0–140 for the median
times past. Neither call changes the chain. A re-run reproduces the file byte for byte (sha256
`4e8bec1e375fbdc0…bae4ee08`). The replay is `tests/locktime_vectors.rs`.

## `coins/regtest-utxo-hash-v24.0.json` (CS5b)

Dash Core's UTXO-set hashes, recorded by `contrib/node/utxo-hash-vectors.py` on 2026-10-09 with
the same v24.0.0-rc.3 `dashd` and `test/functional` as `locktime/regtest-v24.0.json`:

```bash
PYTHONPATH=<dir with a dash_hash stub> contrib/node/utxo-hash-vectors.py --dashd <dashd> \
    --framework <git archive of test/functional at v24.0.0-rc.3> \
    --datadir <empty dir> --port <p> --rpcport <p+1> --out regtest-utxo-hash-v24.0.json
```

At heights 0, 1, 101, 102, 103 and 110 of a mock-clock chain mined to `raw(51)`, the file holds
Core's `dumptxoutset` snapshot (hex) with `gettxoutsetinfo hash_serialized_2` and
`gettxoutsetinfo muhash` for the same tip. Block 102 adds a 200-output transaction (zero values,
a P2SH output, an `OP_RETURN` that never enters the set) and a two-output one; block 103 spends
some of each and an output created in the same block, and adds a coin of each special script
type Core's coins database compresses (P2PKH, P2SH, and P2PK with compressed and valid
uncompressed keys of both parities: codes 0 to 5) and a P2PK whose uncompressed key is off the
curve (stored as a script); block 104 spends the two-output transaction's last coin. A re-run
reproduces the file byte for byte (sha256 `6d17c687510cbde2…a1e721c7b0`). The replay is
`tests/utxo_hash_vectors.rs`.

## `coins/muhash-boundary-v24.0.json` (CS5b)

Dash Core's MuHash3072 arithmetic on boundary values, from `contrib/node/muhash-vectors/`: a
program linked with a built Core tree's `libbitcoin_crypto_base` (with its SHA-256 variants and
`memory_cleanse`), so the numbers come from Core's own `crypto/muhash.cpp`. Recorded on
2026-10-09 against the O2a shim tree `a9a483e51e` (v24.0.0-rc.3 plus the shim; the script refuses
a tree whose crypto, serialization or `support/cleanse` sources differ from the tag):

```bash
contrib/node/muhash-vectors/muhash-vectors.sh <built Core tree> muhash-boundary-v24.0.json
```

Twelve 3072-bit values (0, 1, 2, p − 2, p − 1, p, p + 1, 2^3072 − 1, 2^3071, 2^3008 − 1, and the
0x55… and 0xaa… patterns, with p = 2^3072 − 1103717): for all 144 ordered pairs, the numerator of
`{a, 1} *= {b, 1}`; for 16 quotients (every value as denominator under 0x55…, and p − 1, p,
p + 1 and 2^3072 − 1 over 2), `Finalize`'s hash and the numerator it leaves. A re-run
reproduces the file byte for byte (sha256 `de92576dab267b27…b878a63d29`). The replay is
`tests/muhash_vectors.rs`.
