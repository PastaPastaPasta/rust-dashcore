# dash-evo

The shared layer of Dash Core's evo consensus logic for the Rust Dash node and the SPV client
(SB0 layer P; EV addendum §1.1). The designs are `docs/node-program/designs/EV2-dml.md` (DML),
`LQ1-quorum-members.md` (LLMQ members) and `EV-addendum-SB0.md` (the split and the error
contract); EV0 landed the interfaces, and `API.md` maps every design requirement to the item that
serves it. The node's deterministic masternode list, its store and its records are the node-only
`dash-dml` (layer Pn), which builds on this crate; this crate never names it. This file lists what
other crates must know to use the crate correctly.

## Purity

An ordinary `std` crate with no I/O, clocks, threads or globals (A1 §1.5, §2.1), in the
`node-pure` group, so `contrib/node/check-pure.sh` enforces the bans. All context is passed in:
`&ChainParams`, the chain and deployment views, the `BlsCtx`, the time.

- **Layering.** This crate is P in `contrib/node/layers.toml`, so SB1's layer gates
  (`contrib/node/check-layers.py`) fail if its normal or build closure reaches `dash-dml`
  (Pn) or `imbl` (EV addendum §1.1), in the graph Cargo resolves for the whole workspace with
  every feature and target, widened by path crates' declared dependencies: a feature another
  crate enables in a shared dependency counts.
- **Determinism.** Consensus code uses `BTreeMap`/`BTreeSet` or a sorted `Vec`.
- **No panic on decoded data** (DEC-58). No invariant check may panic or abort on any path
  reachable from decoded, imported or peer-supplied data; violations there are values or typed
  errors. A broken ancestry invariant is `BrokenInvariant`, never a panic.
- **Pure crates may not declare `make_hash!` types**: the macro's expansion calls base-sdk's
  encoder, which the purity clippy bans at forbid level. `ProTxHash` is written out instead;
  its `Ord` is Core's `uint256::operator<`, a memcmp of the internal bytes, and `Display` is
  byte-reversed hex.

## Effects (A1 §2.10.1)

A function that can produce one of Core's non-transactional actions takes
`&mut dyn EffectSink` and never returns effects inside its `Result`. The chainstate applies an
attempt's log in emission order, regardless of the result, before acting on the result and
before the next attempt's read view. `Effect` is deliberately exhaustive: routers match every
variant, and CH7a's durability classification (ST3a's presumed or independent) is an exhaustive
`match` over `EffectKind` with no wildcard. `EffectSink` is write-only: an attempt's own rows are
read back through its snapshot overlay (`dash-dml`'s `ARCHITECTURE.md`), never from the sink.

## The error contract (EV addendum §2.2, §2.3)

Every fallible trait names its error as an associated type, so each product brings its own:

- `QuorumMemberSource` has `type Entry: MnEntryView` and `type Error`, and returns
  `MemberError<Self::Error>`:
  - `Missing(at)`: a per-block input the computation needs (`mn_list`, `coinbase_facts`) is not
    in the view. What it means is the product's: the node maps it to a store inconsistency (LQ1
    D4, `dash_dml::NodeMemberError`); SPV requests the input.
  - `View(e)`: the view failed (a read error, history below an anchored client's floor, a
    latch already tripped).
  - `UnknownLlmqType(t)`: Core's `IsQuorumTypeEnabled` throw.
- The trait has no `Send + Sync` supertrait: every runtime implementor carries a V-only
  capability (EV addendum §2.6, DEC-117).
- `MinedCommitmentReader::Error: From<BrokenInvariant>`, so the provided reads can report a
  mined commitment whose base is above the block that mined it.
- The list views `MnEntryView`, `MnListView` and `ScratchMnList` are **infallible** (DEC-109 (3)):
  their implementations are materialized values, and every fallible read happens in the view
  that produces a list. A crate that owns an entry type `T` implements `MnEntryView for T` and
  uses `Entry = Arc<T>` through this crate's forwarding impl (E0117).

## Not held is not absent (EV addendum §2.1)

Core has two kinds of "no answer": a **normal absence**, which is a value that changes the result
(no snapshot row breaks a rotation, an unreadable coinbase selects the hash-form modifier, a
null ancestor or `pprev` ends a walk), and a **local fault**, which is fail-stop on the node. SPV
adds a third, input it does not hold, which Core never sees.

A view returns a normal-absence value only when it *knows* Core would see that absence. Input it
merely lacks is `Ok(None)` from `mn_list`/`coinbase_facts` (so `MemberError::Missing`) or
`Err(View(e))`; never an absent snapshot row, never an unavailable coinbase, and never
`Ok(None)` from `LlmqChain::ancestor`/`prev`. The node holds everything, so nothing changes for
it. `tests/consumers/spv.rs` shows SPV's mapping (missing or not held: request; below the floor:
unavailable; storage: stop) beside the node's (`Missing` and `View` are fatal).

## The chain contract (EV addendum §2.2)

`LlmqChain` is the chain LQ1 reads (bases, cycle bases, cycle predecessors, work blocks):

- `ancestor(id, h)` is `Ok(None)` only when `h > height(id)` (Core's null `GetAncestor`);
- `prev(id)` is `Ok(None)` only at genesis;
- a block the view should hold but does not (below an anchored client's floor, not yet fetched)
  is `Err`, never `Ok(None)`;
- heights below zero are never passed: callers compute them in `i64` and take Core's
  null-pointer branch without a view call.

The node's complete ancestry is `CompleteChain`, which wraps a `ChainAncestry` and never errs.

## BLS (A1 §1.4.1, DEC-71)

No global flag. Each call site takes a `BlsCtx`: `bls_ctx_for_block(h)` inside block processing,
`bls_ctx_after(tip)` elsewhere and at a DKG base. No record carries it: CH7a recomputes the tip
context after each commit, and nothing else moves it (v24's `ScopedBLSLegacyScheme`; DEC-82, so
no leak effect). Decoding never reads the
global context; global-sourced fields decode scheme-free and are resolved at use, where the
caller's `BlsCtx` is passed.

## Core files mirrored

| Rust | Core (v24.0.0-rc.3, DEC-82) |
|---|---|
| `types` | `evo/dmn_types.h` (`MnType`), `uint256` (`ProTxHash` order) |
| `effect` | the A1 §2.10.1 inventory |
| `bls` | `validation.cpp` (`ScopedBLSLegacyScheme`), `evo/specialtxman.cpp` (the V19 switch), `node/chainstate.cpp` |
| `llmq::view` | `evo/deterministicmns.h` (`CDeterministicMNCPtr`, `GetCounts`, `AddMN`), as LQ1 reads them |
| `llmq::chain` | `chain.h` (`CBlockIndex::GetAncestor`, `pprev`) |
| `llmq::members` | `llmq/utils.cpp` (`GetAllQuorumMembers`, the trait only) |
| `llmq::mined` | `llmq/blockprocessor.cpp` (`GetMinedCommitment`, `GetMinedCommitmentsUntilBlock`, `GetLastMinedCommitmentsByQuorumIndexUntilBlock`) |
| `llmq::snapshot` | `llmq/snapshot.h` (`CQuorumSnapshot`) |
| `sml::entry` | `evo/simplifiedmns.{h,cpp}` (`CSimplifiedMNListEntry`), `evo/netinfo.h` (`NetInfoSerWrapper`) |
| `sml::root` | `evo/simplifiedmns.cpp` (`CalcMerkleRoot`, `CalcCbTxMerkleRootMNList`), `evo/cbtx.cpp` (`CalcCbTxMerkleRootQuorums`), `consensus/merkle.cpp` |
| `sml::score` | `llmq/utils.cpp` (`calculateQuorumScore`, `CalculateScoresForQuorum`), `evo/dmnstate.h` (`UpdateConfirmedHash`) |
| `cbtx` | `evo/specialtxman.cpp` (the root checks), `merkleblock.cpp` (`ExtractMatches`), `evo/cbtx.cpp` (`GetNonNullCoinbaseChainlock`) |

Deviations are listed in `API.md` §5.
