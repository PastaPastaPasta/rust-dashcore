# cs2-diff

CS2's differential harness: the proof-of-work, subsidy and payment code of `dash-consensus`
against Dash Core's in-process oracle (`dash-oracle-sys`), on the pinned release
(v24.0.0-rc.3, DEC-82). It produces and checks the recorded corpus that `cargo test -p
dash-consensus` replays, and runs the checks too large for `cargo test`: the live random
differential and the full mainnet and testnet header-history sweeps (CS2 design §6).

It is a workspace of its own, like `node-fuzz`, so `cargo test --workspace` never links the oracle
archives. It needs `dash-oracle-sys` (O1, ABI 4, with O1b's `GetPaymentsLimit` entry point) and
the pinned archives (`contrib/node/build-oracle.sh`). It reuses `dash-consensus/tests/common`, so
Core's chain parameters reach the crate the same way in both.

Always run it through `run.sh`, from anywhere; relative paths are from the repository root. The
script checks that the harness's base-sdk `[patch]` table patches every package the root's does,
to the same fork revision, then builds it with `--locked` (release, with debug assertions and
overflow checks) and runs it. A build therefore uses exactly the committed `Cargo.lock`. When the
path dependencies' graphs change (CS0's merge adds to `dash-consensus`), `run.sh --update-lock`
rewrites the lock, which is committed with that change.

The harness refuses to run on oracle archives other than `dash-oracle-sys/pins.txt`'s unless
given `--dev` (anywhere on the command line; `dash-oracle-sys` itself also wants
`DASH_ORACLE_ALLOW_UNPINNED=1`), and every command first prints both oracles' Core versions on
stderr, so a log says which Cores answered.

## Commands

| Command | What it does | Time on agentbox |
|---|---|---|
| `run.sh record dash-consensus/tests/data --check` | regenerates the recorded corpus in memory and compares it with the files; exits 1 on any difference, or on a stale file under `chainparams/` or `oracle/` that the recorder no longer produces | about a minute (KGW's boundary windows) |
| `run.sh record dash-consensus/tests/data` | rewrites the corpus (`dash-consensus/tests/data/PROVENANCE.md`); review the diff | about a minute |
| `run.sh diff <cases> <seed>` | live differential: `<cases>` random cases per chain selection and kind, at the pin (next work including KGW windows, proof of work, masternode payment, subsidy, payments limit, platform share; ten times `<cases>` words through `ConvertBitsToDouble` and pairs through the ≤ 68589 tolerance). Any disagreement panics with the input, and so does a run that compares fewer than 90% of its windows | `diff 2000 424242`: 25 s |
| `run.sh kgw` | KGW's boundary windows (design §2.4): at every mass 14..=4032, Core and the port one second either side of both of the port's thresholds, at the pin; each pair must change Core's answer (but at 4032). It first checks which glibc `pow` the process, and so Core's KGW in the oracle, calls. Run it again under `GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA` for glibc's SSE2 variant | about a minute |
| `run.sh exhaustive bits <threads> [<from> <to>]` | every `nBits` (or the slice `[from, to)`) through `fp::Difficulty::from_bits` against Core's `ConvertBitsToDouble` at the pin, bit for bit; each word's two difficulties also go through the three subsidy formulas, whose debug assertions check every base is in [0, 1111] (design §6.3) | 2^32 words on 8 threads: about 4 minutes |
| `run.sh exhaustive subsidy <chain> <prev_height> [<from> <to>]` | every `nBits` (or a slice) as the previous block's at `prev_height` before V20: the port's `GetBlockSubsidyInner` against Core's at the pin, one thread per pin (design §6.3) | 2^32 words: about 25 minutes per process; run slices in parallel |
| `run.sh sweep main <mainnet.bin>`, `run.sh sweep test <testnet.bin>` | the full history sweep (below) over a recorded dump; `--unpinned` sweeps another one | 20–80 s and 10–30 s (X11 over the dump, load-dependent) |
| `run.sh sample main <mainnet.bin> dash-consensus/tests/data/history/mainnet.json` (and `test`) | rewrites the history sample `tests/history_sample.rs` replays, with Core's evidence for the tolerance pin; `--unpinned` as for `sweep` | about a minute |
| `run.sh params v24.0 <chain> [args..]` | prints Core's chain parameters (the input of `dump_headers.py`) | |

Every command prints a tally of what it compared. Refusals are counted separately, and each call
accepts only the refusal it can meet: a next-work window shorter than Core's lookback
(`Context`), or a subsidy, payments-limit or platform-share input Core asserts on (`Argument`);
any other oracle error panics.

## The history sweep

A dump is every header from genesis on a reference node's best header chain, 80 bytes each.
The two dumps the sweeps and the history sample use are recorded by sha256 in
`dash-consensus/tests/data/PROVENANCE.md`; `--until` rebuilds one byte for byte once the node
has moved past its tip, and `--sha256` fails unless it did:

```bash
contrib/node/cs2-diff/run.sh params v24.0 main > /work/scratch/<you>/main-params.json
python3 -I contrib/node/cs2-diff/dump_headers.py mainnet /work/scratch/<you>/mainnet.bin \
    /work/scratch/<you>/main-params.json --until 2551766 \
    --sha256 58e6e286297e40e9b9e3afa1e5a734c27bd76be065631fb27d3a5653bf5f8251
```

(testnet: `testnet`, `test-params.json` from `params v24.0 test`, `--until 1568459 --sha256
a88526024d3c3a2a82c797bd6348a8789ce5ee388ecf33742f3987e414659b27`.)

`dump_headers.py` makes read-only RPC calls to the BS2 reference node under `/work/chains`
(cookie authentication). Its blocks may lag its headers during IBD; the script walks the
headers above the block tip back from Core's checkpoints, from the best header chain's tip
(status `active` or `headers-only`), and checks every link.

Before it compares anything, `sweep` (and `sample`) checks the dump:
- its sha256 is a recorded dump's, for that network (unless `--unpinned`);
- every header's `hashPrevBlock` is the X11 hash of the header before it, and every block hash
  Core's chain parameters hard-code at a height in the dump (genesis, the checkpoints, BIP34,
  DIP0003 enforcement, the first superblock) is there;
- it reaches past V20 and DGW, and on mainnet past the tolerance's end.

After the comparisons, `sweep` requires the tally of a recorded dump to equal the recorded one
(`RECORDED` in `src/sweep.rs`), so an exit 0 proves the coverage. `sweep` then checks every
header:
- `CheckProofOfWork` (all but the tip, whose hash needs a successor);
- `GetNextWorkRequired` from the previous block equals the header's `nBits` wherever Core
  requires equality (testnet everywhere, mainnet from 68590); on mainnet up to 68589 it equals
  Core's own `GetNextWorkRequired` at the pin (KGW's heights, 15200..=34139, included), and the
  tolerance is Core's predicate on Core's `ConvertBitsToDouble` of both words; `nbits_acceptable`
  accepts every header, as Core did;
- the subsidy of every block equals Core's `GetBlockSubsidyInner` and
  `GetSuperblockSubsidyInner` at the pin, from the formulas before V20.
