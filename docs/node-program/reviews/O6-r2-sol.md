# O6 review (round 2, Sol)

Status: complete.

## Scope and checks

Re-checked the three round-1 majors on `node/o6` at `aecdcd026` (fixes `498e194c4` and `67583569f`) against Dash Core `v24.0.0-rc.4`. I inspected the guard/state/classifier changes, used a reflink copy under `/work/scratch/o6-rv-sol2/` for probes, compared the research records with `/work/scratch/o6/out`, and ran the focused tests, formatting, clippy, and quirk check.

## Findings

### Major: scan (e) still accepts MNHF bits that Core rejects

`conformance/src/scan/classify.rs:473-485` uses `bit >= 32` as the out-of-bounds test and calls the activation table for bits below 32. Core rc.4 defines `VERSIONBITS_NUM_BITS = 29` (`src/versionbits.h:22`) and `MNHFTxPayload::IsTriviallyValid` rejects every `signal.versionBit >= VERSIONBITS_NUM_BITS` (`src/evo/mnhftx.cpp:95-103`). Therefore bits 29, 30, and 31 are Core-invalid, independently of `IsValidMNActivation`, but this scan emits only `e.signal` and no `e.bit_out_of_bounds`. A synthetic bit-29 MNHF block in the reflink probe produced exactly that result. Change the bound and activation call to the Core 29-bit bound, and add a boundary fixture/mutant that distinguishes 28 from 29 (the current bit-31 boundary mutant only masks the wrong 32-bit base).

### Major: the output guard loses the original datadir when `blocks/` itself is a symlink

`conformance/src/scan/run.rs:132-145` canonicalizes `blocks_dir` first and derives the
protected parent/datadir from that resolved path. If `<datadir>/blocks` is a symlink to a real
blocks directory elsewhere, the guard protects the real directory and its parent, but not the
node's original `<datadir>`. In the reflink probe, `blocks_dir = <datadir>/blocks` (a symlink to
`<other>/blocks`) and `--out <datadir>/out` passed the guard and `write_outputs` created output
there. This is the exact whole-datadir boundary the fix claims to enforce. Derive the enclosing
datadir from the absolute, user-supplied blocks path (canonicalizing its parent chain) as well as
protecting the resolved blocks target, and add a regression for a symlinked blocks directory.

## Guard assessment

The canonicalization fix handles existing and new output paths, relative paths, `..` components after symlinks, and dangling symlinks by resolving the nearest existing ancestor. `Path::starts_with` is component-aware. For an ordinary directory it protects `<datadir>/blocks` and, for `<datadir>/testnet3/blocks` (and other named chain subdirectories), the enclosing datadir; mainnet's `<datadir>/blocks` protects its parent. The unresolved blocks-directory symlink case above defeats that enclosing-datadir protection. There is also a theoretical symlink-swap TOCTOU between the guard and `create_dir_all`; the normal invocation is additionally intended to run with the blocks tree read-only.

## Scan (d) assessment

The v24 leg now matches rc.4 `evo/cbtx.cpp:45-50`: `is_v24_active && isV20` requires version 4, and the exact block-height boundary is `h >= activation_height`, matching `DeploymentActiveAfter(pindexPrev, ...)` for the block being scanned. `getdeploymentinfo` at the anchored tip supplies the first active height; inactive-at-tip cannot have an earlier active ancestor on that chain, and an omitted deployment is correctly `NEVER_ACTIVE` because rc.4 `rpc/blockchain.cpp:1466` returns early when `DeploymentEnabled` is false (`chainparams.cpp:212-214`). With no oracle, versions 3/4 at or after V20 are left undecided and the run is marked incomplete. The new boundary fixtures and v24 mutant are meaningful and killed.

## Scan (e) assessment beyond the finding

For in-chain blocks the MTP source is correct: `mnhftx.cpp:124` passes `pindexPrev->GetMedianTimePast()`, and the scanner computes the median of the parent plus its ten ancestors from best-chain headers. The deployment start/timeout comparisons mirror `chainparams.cpp:109-130`, including inclusive endpoints. The 29-bit bound above remains unresolved.

## Tests, mutants, and rerun records

Passed on the review copy: `cargo fmt --all -- --check`; `cargo clippy -p dash-conformance --all-targets --all-features -- -D warnings`; `cargo test -p dash-conformance --test scan_run --all-features -- --nocapture`; `cargo test -p dash-conformance --lib scan::tests --all-features -- --nocapture`; and `python3 -I contrib/node/check-quirks.py --core /work/home/agent/workspace/dash`.

The claimed 18/18 mutant suite passes, including the two new mutants, but it does not cover the actual 29-vs-32 Core boundary. The raw testnet/mainnet summaries agree with the research JSON on ranges, tips, counters, hit counts, incomplete lists, and v24 state (the only structural differences are the summary's extra code identity and the JSON's scalar-vs-object `unparsed` representation). The four raw scan-(e) rows match the documented testnet signals and all are valid. The tracked mainnet text continues to say only `data ends at 2,094,670; see manager` for the partial range.

## Verdict

CHANGES-REQUIRED
