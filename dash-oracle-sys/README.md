# dash-oracle-sys

In-process Dash Core consensus oracle for differential tests and differential
fuzzing (node-program A1 §4.4, R4 §2.2). It is test-only and never a runtime
dependency.

## What it links

The Dash Core branch `node/oracle-shim-v24.0` (v24.0.0-rc.4 since DEC-458; the one pin since DEC-82) in
`~/workspace/dash` carries `src/test/oracle/{oracle.h,oracle.cpp}`. That file is a C ABI over Core's own
functions; the shim adds no consensus logic. `pins.txt` records the commit of
each pin, the branch whose head it is, and the sha256 of the archive built from it.
The branch is `node/oracle-shim-<pin>`, except while a shim change is under review:
its commits then sit on the task's branch (O2a: `node/o2a-oracle-v2-v24.0`), `pins.txt` names
that, and the merge fast-forwards `node/oracle-shim-<pin>` to the same commits and points the
column back.

`contrib/node/build-oracle.sh` builds each pinned commit from a clean
`git archive` against Core's `depends`. It partial-links the shim with every
Core archive it reaches into one object, renames the entry points to
`oracle_<pin>_*` and hides every other symbol. That way several pins can live in one
Rust binary without clashing. Outputs are cached in
`/work/cache/oracle/<commit>/<subdir>/` with a `MANIFEST` recording the
recipe inputs (script hash, flags, `SOURCE_DATE_EPOCH`), the toolchain and the hash.
`<subdir>` is the flavor's name, except `fuzz-nocmp` for fuzz: when a fuzz recipe change
moves its sha256 the directory gets a new name, so trees still pinned to the old
archive keep linking it while the new one is installed next to it.

The archive's sha256 is its identity. `build.rs` copies each cached archive into
`OUT_DIR`, hashes the copy, and links only a copy whose sha256 is the one
`pins.txt` records for the flavor; a different or unrecorded archive fails the
build. `DASH_ORACLE_ALLOW_UNPINNED=1` links it anyway, for development; the
crate then reports `ARCHIVES_PINNED == false`, and `Oracle::new` panics unless the
same variable is also set when the binary runs. So such a build cannot stand in for
the pinned releases by accident.

## Entry points (ABI 29)

The pinned shim is the train-17 bundle (DEC-238): the train-15 bundle (ABI 18 and 22 surfaces) with BS11a-2, O2a-CJ1, EV2b, O2a-MNHF, O2a-GOV and O2a-LQ3, whose reserved numbers 19 to 25 were never installed, all numbered ABI 26, and O2b-3h's chain-session change. The rows name what later bundles changed: ABI 27 and 28 (BL-211, the T19 bundle) and ABI 29 (EV-RC4, rc.4's checked credit-pool sums).

| Entry point | Core function |
|---|---|
| `tx_decode` | `CTransaction` deserialize/serialize, `GetHash()`, `GetInstanceHash()` |
| `block_decode`, `header_decode` | `CBlock`/`CBlockHeader` deserialize/serialize, `GetHash()`, `BlockMerkleRoot()` |
| `merkle_root` | `ComputeMerkleRoot()` |
| `coin_codec` | `CompactSize`, `VARINT`, `CompressAmount`, `ScriptCompression`, `Coin`, `CTxUndo`, `CBlockUndo` |
| `check_transaction`, `check_block` | `CheckTransaction()`, `CheckBlock()` |
| `legacy_sigops` | `GetLegacySigOpCount()` |
| `verify_script` | `VerifyScript()` with `TransactionSignatureChecker`, returning the `ScriptError` |
| `signature_hash` | `SignatureHash()` |
| `next_work_required` | `GetNextWorkRequired()` (BTC, KGW, DGW, min-difficulty) over a header window |
| `check_proof_of_work`, `compact_decode`, `compact_encode`, `block_proof` | `CheckProofOfWork()`, `arith_uint256::SetCompact/GetCompact`, `GetBlockProof()` |
| `block_subsidy_inner`, `superblock_subsidy_inner` | `GetBlockSubsidyInner()`, `GetSuperblockSubsidyInner()` |
| `mn_payment`, `platform_share` | `GetMasternodePayment()`, `PlatformShare()` |
| `chainparams_json` | the selected `CChainParams` and `Consensus::Params` |
| `eval_script` | `EvalScript()` with `BaseSignatureChecker` (error and final stack) |
| `pubkey_verify`, `check_low_s`, `parse_der_lax` | `CPubKey::Verify()`, `CPubKey::CheckLowS()`, pubkey.cpp's lax DER parser |
| `check_signature_encoding`, `check_minimal_push` | `CheckSignatureEncoding()`, `CheckMinimalPush()` |
| `scriptnum_decode`, `scriptnum_encode`, `scriptnum_is_minimally_encoded`, `scriptnum_minimally_encode` | `CScriptNum` and its encoding helpers |
| `find_and_delete`, `script_sigops`, `script_sigops_with_sig` | `FindAndDelete()`, `CScript::GetSigOpCount()` |
| `transaction_sigops` | `GetLegacySigOpCount()`, `GetP2SHSigOpCount()`, `GetTransactionSigOpCount()` over the spent outputs |
| `check_input_scripts` | `CheckInputScripts()` over the spent outputs (Core's own failure classification) |
| `convert_bits_to_double`, `superblock_payments_limit` | `ConvertBitsToDouble()`, `CSuperblock::GetPaymentsLimit()` |
| `split_amount_by_shares` | `SplitAmountByShares()` |
| `payload_decode` | `GetTxPayload<T>()` per special transaction type (consumed-all, re-encoding) |
| `bls_decode`, `bls_verify` | `CBLSWrapper`/`CBLSLazyWrapper` decoding under either scheme (VALID, INVALID, or a throw), `CBLSSignature::VerifyInsecure()` |
| `check_tx_inputs` | `Consensus::CheckTxInputs()` over given coins |
| `check_special_tx` | `CSpecialTxProcessor::CheckSpecialTx()` (context) |
| `check_special_tx_block` | `CSpecialTxProcessor::CheckSpecialTxInner()` as `ProcessSpecialTxsInBlock()` calls it: the chain overloads of `CheckMNHFTx()` and `CheckAssetUnlockTx()`, the credit pool's index set, Core's quorum block processor and quorum manager over a [`BlockContext`]'s mined commitments, built per call; each seeded commitment passes `ProcessBlock()`'s signature checks and `CFinalCommitment::Verify()` against the context's members (context; ABI 13, member and signature checks ABI 16) |
| `netinfo_validate`, `netinfo_add_entry` | `NetInfoInterface::Validate()` and `AddEntry()` on an `MnNetInfo` or `ExtNetInfo` as a ProTx payload carries it, returning the `NetInfoStatus` (and the entry Core added) under the selected network (ABI 13; `-allowprivatenet` is accepted since ABI 16, and since rc.4 changes no verdict) |
| `dml_apply`, `dml_listdiff_json` | `RebuildListFromBlock()` and `GetMNPayee()` (context; the new list in evodb and canonical form); `protx listdiff`'s rendering of two lists |
| `dml_block` | the masternode-list part of `ProcessSpecialTxsInBlock()`: the `CheckSpecialTxInner()` loop, `BuildNewListFromBlock()`, `CDeterministicMNManager::ProcessBlock()`, with the rejecting stage (context) |
| `mnlist_canonical` | `SerializeCanonicalMNList()`, `UnserializeCanonicalMNList()`, `CanonicalMNListHash()` (v24 `evo/snapshot.h`), and the evodb encoding |
| `mnlist_diff`, `mnlist_apply_diff`, `state_diff` | `CDeterministicMNList::BuildDiff()`, `ApplyDiff()`, `CDeterministicMNStateDiff` |
| `mnlist_core_order` | the order `ForEachMN()` visits a list built by a sequence of adds, updates and removals (immer CHAMP) |
| `unique_property_hash` | `GetUniquePropertyHash()` per property type |
| `mnlist_payee`, `mnlist_projected` | `GetMNPayee()`, `GetProjectedMNPayees()` of a list |
| `sml_entry_hash`, `sml_merkle_root` | `CSimplifiedMNListEntry` and `CalcHash()`, `CalcCbTxMerkleRootMNList()` (stateful through Core's process-wide root cache: the shim runs it once and serves an `ORACLE_E_BUFFER` retry from the kept output) |
| `special_tx_sign_hash` | the hashes Core verifies provider signatures over: `SerializeHash(payload)`, the ProRegTx sign string and its message hash, `MakeSharedRegConsentHash()`, the ProDisTx sign hash |
| `sequence_locks` | `CalculateSequenceLocks()`, `EvaluateSequenceLocks()` (context) |
| `contextual_check_tx`, `contextual_check_header`, `contextual_check_block` | the static `ContextualCheck*()` of validation.cpp, through test-only trampolines (context) |
| `verify_commitment` | `CFinalCommitment::Verify()` (context; the rejection detail is Core's LLMQ log line) |
| `versionbits_state` | `VersionBitsCache::State()`, `StateSinceHeight()`, `Statistics()`, `ComputeBlockVersion()` and `CMNHFManager::GetSignalsStage()`, run in order against one cache that starts empty, with Core's `CMNHFManager` over the EHF signals each header's block holds (context; ABI 12, CS4) |
| `p2p_decode` | the overlay messages' `Unserialize` under `SER_NETWORK` at a stream version and BLS scheme, with the handlers' own checks (qgetdata's length, qdata's bounded vectors), and the write-back: clsig, isdlock, qsigrec, qfcommit, mnauth, spork, platformban, qgetdata, qdata, getqrinfo, qrinfo, mnlistdiff, one SML entry, one quorum snapshot (ABI 7, O2c) |
| `coins_ops` | a script of `CCoinsViewCache` ops (`AddCoin`, `SpendCoin`, `AccessCoin`, `HaveCoin`, `HaveCoinInCache`, `Flush`, `Sync`, `Uncache`, best block, `BatchWrite`, `DynamicMemoryUsage`) over a per-call `CCoinsViewDB` ← tip ← child stack, each op's output or Core's `logic_error` or assertion, and the database after a tip flush (ABI 15, O2a-CO) |
| `bls_exec` | a register script of `CBLSWrapper` and `CBLSLazyWrapper` ops (decode, serialize, `Aggregate*`, `Sub*`, `Verify*`, the threshold shares and recovery, DH, IES, and from ABI 26 `operator==`, `+`, `-`, `*`, `IsValid()`, `GetHash()` with its cache and the impls' `operator==`), each record with relic's error code after Core's call and, under the log flag, the relic entries Core called with their codes (ABI 14, BS12; ABI 26, BS11a-2). `bls_exec_injecting` sets a relic code at the nth call of a symbol, for the call log's own selftest |
| `chain_session` ([`ChainSession`]: `step`, `save`, `restore`) | Core's own `ChainstateManager` and Dash managers on regtest (LevelDB in memory, block files under `TMPDIR`), driven op by op as dashd's RPC handlers, `Shutdown()` and startup drive them; each op's report: the reply, the StateDigest records, the validation signals and the state after it. One per process, holding the pin for its lifetime (ABI 22, O2b) |
| `cj_rule`, `cj_entry`, `cj_in_outs`, `cj_collateral`, `cj_message_hash`, `cj_check_signature`, `cj_queue_time_out_of_bounds`, `cj_dstx_structure` | src/coinjoin's session-free rules: the denomination rules of `coinjoin/common.h` and `ValidateFinalTxComposition()`, `ValidatePromotionEntry()`/`ValidateDemotionEntry()`, `CCoinJoinBaseSession::IsValidInOuts()` and `CoinJoin::IsCollateralValid()` over a [`CjView`] (a coins tip, a `CTxMemPool` and the InstantSend locks, `IsLocked()` and `ProcessTransaction()` answered through `--wrap` seams), the dsq/dstx `GetSignatureHash()`, `GetHash()` and `CheckSignature()`, `CCoinJoinQueue::IsTimeOutOfBounds()`, and `CCoinJoinBroadcastTx::IsValidStructure()` (context; ABI 26, O2a-CJ1) |
| `mnhf_step` | `CMNHFManager::ProcessBlock()` for one block as `ProcessSpecialTxsInBlock()` calls it (`fJustCheck` from an [`MnhfContext`], whose signal changes are the rows `GetForBlock()` reads), then for a connected block `GetSignalsStage()` of the block and `UndoBlock()` (context; ABI 26, O2a-MNHF; `seed_tip_cache` ABI 28, BL-211) |
| `block_value_payee` | `CMNPaymentsProcessor::IsBlockValueValid()` and `IsBlockPayeeValid()` as `ConnectBlock()` calls them, with `ConnectBlock()`'s superblock check level, era and strict multiplicity, and the outputs `GetBlockTxOuts()` and `GetSuperblockPayments()` give: a `SuperblockManager` holding a [`PaymentsContext`]'s triggers and their votes (or, format 2, the triggers a governance op script leaves held: `block_value_payee_scripted`), the sync and ChainLock gates and the fees (context; ABI 18, O2a-GV; format 2 ABI 26, O2a-GOV) |
| `credit_pool_step` | `CSpecialTxProcessor::CheckCreditPoolDiffForBlock()` and `CCreditPoolManager::GetCreditPool()` for one block: the pool after the tip from a [`StepContext`], whose blocks `node::ReadBlockFromDisk()` serves through a `--wrap` seam (context; ABI 18, O2a-GV; rc.4's checked sums on prev amounts outside the money range ABI 29, EV-RC4) |
| `gov_proposal_validate` | `governance::ValidateProposal()` (`CProposalValidator`) at an adjusted time (ABI 26, O2a-GOV) |
| `gov_object` | one `CGovernanceObject`: `GetHash()`, `GetDataHash()`, `GetSignatureHash()`, `GetMinCollateralFee()`, `IsValidLocally()` without and with the collateral (`TxIndex::FindTx()` serves a [`GovContext`]'s collaterals), and the trigger `CSuperblock`'s parse, height, payments limit and expiry (context; ABI 26, O2a-GOV) |
| `gov_vote` | one `CGovernanceVote`: `GetHash()`, `GetSignatureHash()`, and `IsValid()` with the voting and the operator key and `IsValidForUnknownParent()` against the tip's masternode list (context; ABI 26, O2a-GOV) |
| `gov_ops` | a script of governance ops over one `CGovernanceManager` at the tip: the time, `ProcessObject()` and `ProcessVote()` on network messages with each verdict, exception and penalty, an object's tallies and current votes, and `IsSuperblockTriggered()` / `GetSuperblockPayments()` (context; ABI 26, O2a-GOV) |

The entry points marked "context" run against a [`Context`]: the headers ending at
the tip, version-bits states, the BLS scheme flag, the adjusted time, the checkpoint
setting with the best known header, the tip's masternode list or the fact that the
context holds none, coins, and the members of quorums. `Context::new` takes every
field; none has a default. The shim builds Core's own objects from it; inside a contextual call `llmq::utils::GetAllQuorumMembers()` returns the
context's members (`build-oracle.sh` links the shim's `__wrap_` seams with
`ld --wrap`). `src/test/oracle/oracle.h` ("chain contexts") documents the format and
what Core reads that the context must cover. `scripts/dml-listdiff-check.py` checks
`dml_apply` and `dml_block` against a reference dashd's `protx listdiff` on sampled mainnet
blocks, with the payee against the coinbase and the SML root against the CbTx;
`scripts/gen-dml-crafted.py` records a release dashd's verdicts on crafted blocks for
`dml_block`.

Arguments are refused (`ErrorKind::Argument`) only where Core would assert or run
into undefined behaviour; `src/test/oracle/oracle.h` lists each case.
`ErrorKind::Context` marks the deliberate domain restrictions: the call lacks
context Core would read, so there is no Core verdict. That covers:

- a header window shorter than what Core's next-work algorithm reads (for KGW, its
  whole PastBlocksMax lookback, since where its event horizon stops depends on its
  floating-point arithmetic), or than a median time past Core needs;
- a masternode list the context does not hold (none given, or another block's list);
- quorum members the context lacks, and a quorum base block outside its headers;
- through `check_special_tx` (the mempool path), every MNHF signal and asset unlock once
  DIP0003 is active, which Core checks against a quorum manager; `check_special_tx_block`
  checks them against the block context's mined commitments, and refuses only the
  verdicts that turn on blocks below a window that does not start at genesis (an unknown
  quorum hash, a short platform-quorum scan) and an MNHF signal whose median time past
  the headers do not reach.
- the EHF signals of a placeholder below the headers at which V20 is active, which
  version bits read at a queried block and at the end of the period before it.

`src/test/oracle/oracle.h` gives each entry point's cases.

## Usage

```rust
use dash_oracle_sys::{Chain, Oracle, Pin};

let core = Oracle::new(Pin::V24_0);
let decoded = core.tx_decode(&tx_bytes)?;
let script = core.verify_script(&spk, &tx_bytes, 0, amount, flags)?;
let bits = core.next_work_required(&Chain::main(), &headers, first_height, &next_header)?;
```

Calls that read chain parameters take a `Chain` and serialize on a per-pin
lock, so a pin serves one chain at a time. All other calls can run concurrently.

## Building and testing

Everything is behind the `oracle` feature, and building the archives needs Linux
with GNU binutils (`ld -r`, `objcopy`):

```bash
contrib/node/build-oracle.sh            # once; about 10 minutes per pin on agentbox
cargo test -p dash-oracle-sys --features oracle
```

- `DASH_ORACLE_AUTOBUILD=1` makes `build.rs` run the script for missing archives.
  Without it a missing archive only skips linking (with a warning), so
  `cargo check`, `clippy` and `doc` with `--all-features` work on any machine;
  only linking a binary fails.
- `DASH_ORACLE_CACHE` sets the cache root (default `/work/cache/oracle`).
- `DASH_ORACLE_FLAVOR` is `plain` (the default), `fuzz` or `ubsan`. The fuzz flavor
  uses clang with `-fsanitize=fuzzer-no-link` and only links into libFuzzer
  binaries. relic and dashbls are compiled without `trace-cmp`
  (`contrib/node/oracle-relic/nocmp-clang`, BL-371): their edge counters stay,
  but the comparison callbacks in the field arithmetic are gone. They cost more
  than a third of the instructions of a BLS-heavy oracle call. The ubsan flavor (`-fsanitize=undefined`, gcc, recoverable as in Core's
  CI) is a development build only (`build-oracle.sh --dev --flavor ubsan`, never
  pinned): run the tests against it with
  `UBSAN_OPTIONS=halt_on_error=1:print_stacktrace=1:suppressions=$PWD/dash-oracle-sys/ubsan-suppressions.txt`,
  so the first report not suppressed aborts the process. The suppressions cover
  third-party code only: Core's X11 implementation, which stores through misaligned
  pointers by design (`SPH_UNALIGNED`) and shifts negative values in C on every
  header hash, and the dependency entries of Core's own
  `test/sanitizer_suppressions/ubsan`.
- `DASH_SRC` points at the Dash Core repository (default `~/workspace/dash`).
- `DASH_ORACLE_ALLOW_UNPINNED=1` links archives `pins.txt` does not record (see above).

The script reuses a cached archive only if the sha256 of its bytes matches
`pins.txt`, and refuses to publish a fresh build that does not match (it keeps it
under `<cache>/rejected/`). It disables ccache, fixes the locale and time zone,
and maps the build directory out of every path the compiler records, so the
archive does not depend on where or when it was built.

To move a pin, commit the shim on the new release branch, then run
`contrib/node/check-oracle-repro.sh --work <empty dir> --update-pins --install
/work/cache/oracle [--flavor fuzz] <pin>` for each flavor. It points `pins.txt` at
the branch head, builds the archive from scratch in two fresh cache roots with
different paths (each building its own depends), records its sha256 only if both
builds are byte-identical, and installs it in the shared cache. Without
`--update-pins` it checks the recorded archive the same way.
(`contrib/node/build-oracle.sh --update-pins <pin>` records a single build's hash.)

While changing the shim, `contrib/node/build-oracle.sh --dev <checkout> --cache
<dir> <pin>` builds the checkout's working tree in a build directory it keeps,
so rebuilds are incremental. Link it with `DASH_ORACLE_CACHE=<dir>/dev
DASH_ORACLE_ALLOW_UNPINNED=1` (at build and run time).

The smoke tests run every entry point: `tests/smoke.rs` against
mainnet and testnet samples captured from a reference dashd, including the undo
records it wrote to disk; `tests/contexts.rs` against regtest and devnet chains
mined by the official v24.0.0-rc.3 release, the regtest chain of
Core's block_reward_reallocation test and the v24 asset-unlock txids;
`tests/script.rs` against what Core's own script tests compute; `tests/contextual.rs`
runs the contextual entry points on Core-accepted mainnet samples, Core's
`trivially_invalid.json` rejections, and the verdicts of the
v24.0.0-rc.3 release binary on crafted regtest blocks and sequence locks; `tests/netinfo.rs`
runs the NetInfo entries on the rows of Core's `evo_netinfo_tests.cpp`, and
`tests/specialtx_block.rs` the block path on MNHF signals and asset unlocks signed for
quorums it seeds, one reject per branch of Core's checks. Expected values
come from dashd or Core's tests (and dashcore where it has the same function),
never from the oracle (`tests/data/PROVENANCE.md`). `tests/archive_check.rs`
covers the archive authentication without linking anything.
