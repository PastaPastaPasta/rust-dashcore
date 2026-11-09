# BS5 chain-params corpus generator

> DEC-82 (v24 only): the node no longer uses v23.1, and `pins.txt` has no v23.1 row. The generator runs the pins `pins.txt` lists, so it now writes
> `chainparams_v24_0.json5` and `brr_oracle_payments.json5` only. `dash-chainparams` carries no `chainparams_v23_1.json5` (POL-3c, v24.0 only), and
> the base-sdk fork's copy stays as it was generated until POL-3d. Regenerating a v23.1 corpus needs the old v23.1 pin row back.
>
> Since v24.0.0-rc.5 the dump also writes `ext.minBlocksToKeep` from `CChainParams::MinBlocksToKeep()`, which earlier
> pins (v23.1, and v24.0 up to rc.4) lack. Since v24.0.0 it also copies `IPv4OptionalHeight` into `params.consensus`
> from `Consensus::Params::IPv4OptionalHeight`, which rc.5 lacks, so it builds only against a v24.0.0 or later pin. A
> v23.1 regeneration needs both lines left out as well.
>
> `ext.buriedDeployments` lists every buried deployment the pin's `Consensus::ValidDeployment` accepts, so v24.0.0's
> `ipv4_optional` (after `withdrawals`) is dumped without an edit to the loop.

This generator produces two files, which live in `dash-chainparams/corpus/`: `chainparams_v24_0.json5` and `brr_oracle_payments.json5` (`--out` takes any directory). `dash-chainparams`' `core_corpus` and `brr_oracle_payments` tests check `ChainParamsBuilder` and the BRR table against them, and `dash-consensus`'s `chainparams_blocks` test checks the block bytes. The hand-kept `chainparams_cases.json5` beside them lists the cases each corpus must hold. The base-sdk fork keeps its own copies, with `chainparams_v23_1.json5`, until POL-3d removes them. The base-sdk `brr_payments.json5` is upstream's own BRR corpus (`pkgs/params/contrib/brr_payments.py`), which this generator does not write. Every value comes from Dash Core, not from hand transcription.

- **`build.sh <pin> <out>`** reads the pin's commit from `dash-oracle-sys/pins.txt`. It never takes a working tree. By default every run is a fresh, controlled clean build of that commit:
  - Delete the pin's build tree, `git archive` into it, then check every tracked file against the commit's blobs. `export-subst` files must equal what `git archive` makes of them.
  - Build depends (its cache starts empty) and the libraries `dash-tx` links.
  - Verify the tree again, then write the tree's anchor, `.bs5-anchor`: the sha256 of every file and the target of every symlink in it.
  - Compile `chainparams_dump.cpp` with the pin and commit compiled in.
  - Write `MANIFEST-<pin>`, recording the commit, its tree, `build fresh`, the sha256 of the anchor, of the dump source, of each linked archive and of the binary.

  `BS5_REUSE=1` reuses the anchored tree instead, for development. The tree must still equal the commit and every file must equal its anchor, or `build.sh` refuses it; the MANIFEST then says `build reused`.
- **`chainparams_dump.cpp`** links Core's own code and prints, for one chain and argument list:
  - `params`: the O1 oracle's `ChainParamsJson()`, copied verbatim;
  - `ext`: the `CChainParams` members the oracle omits;
  - `spork`: `AppInitMain`'s `-sporkaddr`/`-minsporkkeys` block, verbatim, with `CSporkManager`'s two setters copied verbatim and Core's linked `DecodeDestination`;
  - `provenance`: the compiled-in pin and commit.
- **`run.py`**:
  - **Cases:** holds the case lists (4 chains × 12 combinations, devnet names, every throw site, error order, negation, limits, spork options, BL-31 refusals).
  - **Provenance:** refuses a dump unless its binary and MANIFEST match `pins.txt`, the commit's tree and the dump source, the MANIFEST is of a fresh build (`--allow-reused-build` lifts that, for development), and every output carries the same pin and commit. This check covers both JSON sections.
  - **Oracle comparison:** checks every case the oracle can take against the pinned archives' `oracle_chainparams_json` (`dash-oracle-sys` example `chainparams_cases`). The oracle prints `minBlocksToKeep` in its params (ABI 35) and the dump keeps it in `ext`: it must equal `ext.minBlocksToKeep`, and the rest of the params must be equal exactly (`test-run.py`).
  - **BRR fixture:** takes it from the archives' `oracle_mn_payment` (example `mn_payment_cases`).
  - **Output:** writes the corpus only after every check at both pins passed.
- **`selftest.sh <dumps>`** is the provenance regression test. It works on a copy of the anchored v24.0 tree and checks that:
  - `build.sh` refuses to reuse a tree with a drifted consensus header (the r1 reviewer's `max_store_depth + 1` experiment);
  - `build.sh` refuses to reuse a tree whose `libbitcoin_common.a` holds a `chainparams.o` rebuilt with regtest `nCreditPoolPeriodBlocks = 101`, its source then restored (the r2 reviewer's experiment);
  - `build.sh` reuses the intact tree;
  - `run.py` refuses an altered or foreign binary, a MANIFEST of another commit, a MANIFEST of another dump source, and a reused build's dump without `--allow-reused-build`.

  It also checks that the real tree still equals its anchor.

## Options outside the oracle

Three options are dump-tool-only (`tool_only`), because the oracle's `ParseParameters` doesn't register them:
- `-fastprune` is registered by dashd's `init.cpp`.
- `-sporkaddr` and `-minsporkkeys` are also `init.cpp` options. `-minsporkkeys` exists at v23 only, so v24's argument parser rejects it with Core's own text.

## Regenerate

```sh
S=/work/scratch/bs5; mkdir -p $S/gen/out $S/gen/corpus
B=contrib/node/bs5-chainparams
$B/build.sh v24.0 $S/dumps
python3 -I $B/run.py --cases-only $S/gen/cases.json --mn-cases-only $S/gen/mn.json
cargo run -q -p dash-oracle-sys --features oracle --example chainparams_cases -- $S/gen/cases.json > $S/gen/oracle.jsonl
cargo run -q -p dash-oracle-sys --features oracle --example mn_payment_cases -- $S/gen/mn.json > $S/gen/mn.jsonl
python3 -I $B/run.py --dumps $S/dumps --oracle-jsonl $S/gen/oracle.jsonl --mn-jsonl $S/gen/mn.jsonl \
  --out $S/gen/out --corpus $S/gen/corpus
$B/selftest.sh $S/dumps
```

On 2026-10-09, from fresh builds of both pins, there were 178 cases per pin (147 checked against the oracle, all equal), and the selftest passed. The corpus was byte-identical to the committed one except for the dump binaries' sha256.
