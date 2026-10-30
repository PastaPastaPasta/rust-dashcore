**Verdict: APPROVE-WITH-NITS.** BS5 can merge into `node/base-sdk` after the minor items below. Its re-pin into rust-dashcore must carry the dash-consensus adapt in the same change (F1).

# BS5 r1 — second reviewer (Opus): API, conventions, adapt plan

| | |
|---|---|
| Under review | base-sdk `node/bsdk-bs5-params` 0286d1a + ae304a1 (on 481f8c3); rust-dashcore `node/bs5-adapt` 5fad804d |
| Scope | API fit (CS2, CH1, LQ1/LQ2, EV2, AR1), code quality and base-sdk conventions, the rust-dashcore adapt and re-pin plan. Field-level Core equivalence is GPT's part. |
| Scratch | `/work/scratch/bs5-opus/` (test logs, regenerated corpus in `dump/`) |

## 0. What I re-ran

- **`cargo test -p dash-params`** at ae304a1 (detached scratch worktree): lib 40, builder 35, core_corpus 2 (one per pin), genesis_valid 8. All green.
- **Other checks on dash-params:**
  - `cargo clippy -p dash-params --all-targets --all-features -D warnings`, `cargo fmt --check`, `RUSTDOCFLAGS=-D warnings cargo doc` and a `--no-default-features` (no_std) build: all clean.
  - `cargo check --workspace --all-targets --all-features`: clean. The only warning is the known dash-pow SIMD one.
- **Corpus regenerated from Core at both pins.**
  - I rebuilt `chainparams_dump` with `contrib/node/bs5-chainparams/build.sh` against `oracle-shim-v23.1` (350ce242) and `-v24.0` (23769258). Both heads equal `dash-oracle-sys/pins.txt`.
  - I re-ran `run.py --cases-only`, the `chainparams_cases` oracle example and `run.py`. All 139 oracle cases per pin equal the pinned `oracle_chainparams_json`.
  - The regenerated `chainparams_v23_1.json5` and `chainparams_v24_0.json5` are **byte-identical** to the committed corpus.
  - Spread per pin: main 14 / test 12 / devnet 24 (23 at v24) / regtest 29 (27) successful cases, plus 66 / 69 error cases. That meets PLAN's "4 networks × 12 override combinations" with room to spare.
- **Mutations** (planted in my scratch worktree, then reverted):

| # | Mutation | Caught by |
|---|---|---|
| M2 | `all()` ignores `-no<option>` for repeatable options (`live = 0`) | **corpus** at both pins (`extra/neg_vbparams_after`), and `negation_follows_cores_settings` |
| M3 | `GetIntArg` → `int` saturates instead of wrapping | **corpus** at both pins (`devnet/last_value_and_atoi`, `nHighSubsidyBlocks`) |
| M4 | devnet genesis `nTime = prev + 2` | **corpus** at both pins (`devnet/none`, `chainTxData.nTime`, and the hash) |
| M1 | BL-31 refusal removed (negative heights clamp to 0) | `negative_heights_are_refused` (5 cases). The corpus can't catch it by design: the refusal isn't Core behaviour. |
| P1 | gap probe: mainnet `pins.llmq_25_67_activation_height` = 847001 | **nothing** (F5) |

- **Base-sdk merge trial.**
  - `git merge-tree` of BS5 with BS6a (9094d81) and with BS7 (0a61c4f) is conflict-free. The three branches touch disjoint crates (params/dev, primitives/types/codeql, p2p_core), plus Cargo.lock, which merges cleanly.
  - On the three-way tree (in my scratch index, no commit), `cargo check --workspace --all-targets --all-features` is clean. `cargo test -p dash-params -p dash-primitives -p dash-p2p-core` is green.

## 1. Findings

### F1 — major (plan, not code): the BS5 re-pin breaks `node/integration` unless the adapt ships in the same change
`node/integration` (20594ab1) already contains CS2 phases A/B in dash-consensus, and they consume the 33fcc59-era API:
- `dash_params::Network` and `Network::X.chain() -> &'static ChainParams`;
- `ChainParams: Copy` (`*Network::Main.chain()`);
- the `network_id: &'static str` field;
- the struct-literal `scrambled()` guard in `tests/common/mod.rs`;
- the `Bip9Deployments` type.

BS5 removes or changes all of these, so the re-pin and the adapt must land as one change, or the integration gate goes red. Merging BS5 into `node/base-sdk` is safe on its own, because nothing in rust-dashcore moves until `bump-base-sdk.sh` runs. Sizing and ownership are in §3, order in §4.

### F2 — minor: `ChainParams`/`ConsensusParams` should be `#[non_exhaustive]`
Shape: `pkgs/params/src/types.rs` `ConsensusParams` and `ChainParams` (pub fields, owned, built by the builder).

**The owned shape is right.**
- A devnet's name and overrides make params a runtime value. Core's own `CChainParams` is a heap object built once at startup.
- The extra cost is small and paid once:
  - a few `Vec`s (39 checkpoints, up to 8 LLMQs, 2–3 deployments) and one or two one-transaction genesis `Block`s;
  - the devnet nonce search hashes about two X11 headers on average, since regtest bits 0x207fffff give a target near 2²⁵⁵.
- Consumers should build once and pass `&ChainParams` or `Arc<ChainParams>`. CS0's `ConsensusCtx<'a>` and LQ1's `LlmqCtx<'a>` already borrow `&'a ChainParams`, so they fit unchanged.

**What's missing is a guard against hand-built params.**
- With all fields public and no `#[non_exhaustive]`, downstream code can build a `ChainParams` by struct literal and bypass the builder. Such params can break the invariants the API relies on:
  - `deployments[i]` belongs to `release.deployments()[i]`, which `ConsensusParams::deployment(pos)` indexes by position;
  - `devnet_name`/`devnet_genesis` are `Some` exactly on devnets;
  - each role type is in `llmqs`.
- CS2's fixtures do exactly this today (`scrambled()`).
- **Fix:** add `#[non_exhaustive]` to `ChainParams`, `ConsensusParams`, `HardCodedPins` and `SporkParams`. There's precedent in `p2p_core/src/ctx.rs`. Consumers can still read and mutate fields after `build()`, which is all CS2/CH3a/LQ2 tests need, but literals and exhaustive destructuring are gone.
- Cheapest now, while the only external user is `bspcheck`.

### F3 — minor: one `Release` type; CS0 re-exports `dash_params::Release`
**Rulings:**
- **`NetworkKind` survives.** BS5 deletes `dash_params::Network {Main, Testnet3, Regtest}`, and it had no devnet anyway. `NetworkKind` matches `FFINetwork`'s discriminants (0–3, checked against `dash-network/src/ffi.rs`), so BS16 is a pure mapping.
- **`dash_params::Release` survives.** CS0's `dash_consensus::release::Release` (at fe69be59) has the same two variants with the same meaning (`V23_1` = v23.1.8, `V24_0` = v24.0.0-rc.3). Two types would need a mapping, and a context could pair `ChainParams` built at one release with `UngatedRules` of the other.
- I agree with the implementer: CS0 does `pub use dash_params::Release`. The CodeQL "no foreign `pub use`" rule is base-sdk-only.

**What moves where in CS0:**
- `Release::default_for` is DEC-05 node policy, so it doesn't belong in base-sdk. It becomes a free function `dash_consensus::release::default_release(NetworkKind) -> Release`, with Devnet → `V24_0`.
- `name()`/`parse()`/`tags()` (the `-releaseprofile` spellings, including the future `24.0.0` alias) are node or AR1 configuration. They become free functions in the same module, since inherent methods can't be added to a foreign type.

**Make the mismatch unconstructible:**
- CH1 builds `rules = UngatedRules::of(params.release)`, rather than taking a separate release.
- Optionally, `ConsensusCtx` gets a constructor that `debug_assert!`s `params.release == rules.release`.

### F4 — minor: the builder's input contract is command-line-only; document it for AR1
`ChainParamsBuilder` (builder.rs:128-145, `last`/`all` at :219-231) models occurrences the way Core stores them from the command line:
- the last value wins;
- a repeatable option keeps the values after its last negation;
- a negation reads `"0"`.

Core's `CChainParams` reads the merged `ArgsManager`, and the merge rules differ by source:
- **config-file values:** the first occurrence wins (`reverse_precedence` in `GetSetting`);
- **network sections:** `[test]`, `[regtest]` and `[devnet]` apply to their chains;
- **`settings.json` and forced settings** sit at their own precedence levels;
- **`IsArgSet`** is true for a negated setting.

If AR1 naively replays `dash.conf` then the command line as builder occurrences, the config-file precedence comes out wrong. The current API is still enough for AR1 if AR1 does one of two things:
- passes **resolved** values: one `arg`/`negate` per single-valued option (Core's `GetArg` result) and the `GetArgs` list for the two repeatable options;
- or uses an additive entry point, e.g. `ChainParamsBuilder::from_settings(kind, release, &impl ChainSettings)` with `get(ChainOption) -> Option<Setting { Value(String), Negated }>` and `get_all(ChainOption) -> Vec<String>`.

**Fix now:** one doc paragraph on `ChainParamsBuilder` stating that occurrences are command-line semantics and other sources must be resolved first. The trait can wait for AR1 (it's additive, no re-pin pressure).

Two smaller AR1 notes:
- `-chain`/`-testnet`/`-regtest`/`-devnet` *selection*, including the "invalid combination" error, correctly stays out of dash-params.
- The `-name=v` / `-noname` / `-noname=0` parsing lives only in the corpus test (`tests/core_corpus.rs` `add_arg`). AR1 has its own `InterpretOption` port, so it shouldn't move into the crate.

### F5 — minor: quirk 34 — `HardCodedPins` is the right API, but one copy is untested and the boundary is undocumented
Shape: `types.rs:422-445` holds `HardCodedPins { pre_dgw_tolerance_until, llmq_25_67_activation_height }`; `bad-sb-start` and `bad-dip3-enf-block` stay as `ConsensusParams` checkpoints.

**The split is right:**
- the two hash pins are `CChainParams` members in Core;
- the other two are literals in `validation.cpp`;
- rule sites (CS2 `bad-diffbits`, LQ1/LQ2 `IsQuorumTypeEnabled`, EV2b `bad-dip3-enf-block`, CH3a) read `params.pins.*`, a uniform place;
- the pub fields let a regtest test set a pin to exercise a rule site, which CH3a and LQ2 will want.

**Two gaps:**
- **Untested mainnet copy.** Core's `TESTNET_LLMQ_25_67_ACTIVATION_HEIGHT` is a global constant, but BS5 stores a copy per chain. `hard_coded_pins` (tests/builder.rs:213) checks it only on test/devnet/regtest, and P1 (mainnet 847001) survived every test. That's harmless today, since mainnet doesn't configure LLMQ_25_67, but it's a degree of freedom Core lacks. **Fix:** loop the assertion over all four kinds.
- **Undocumented boundary.** Other `NetworkIDString()`-keyed Core literals stay at CS2/LQ1 rule sites, not in `pins`:
  - the mainnet `nPrevHeight <= 4500` subsidy branch (`validation.cpp:1814` v24);
  - the LLMQ_50_60 testnet/devnet branch (`:6680`);
  - the devnet subsidy path.

  That's fine, since A1's quirk 34 lists only the three pins, but the `HardCodedPins` doc should say which literals are deliberately absent. Otherwise the CS2 adapt may double-source 68589 (CS2 derives it today in `pow/params.rs:76`), or a later task may add 4500 inconsistently.

### F6 — minor (adapt plan): `check-release-refs.sh` fires on every builder call outside `release.rs`
CS0's O17 §2 rule-2 check (`contrib/node/pin-delta/check-release-refs.sh`) greps `\bRelease::` in every `*.rs` except `release.rs`. BS5's entry point is `ChainParamsBuilder::new(kind, Release::V24_0)`, so the CS2 adapt and every later test fixture (LQ1, EV2, CH1) trips it. That includes `Release::ALL`.

**Fix, in the re-pin PR:** add a helper in `dash_consensus::release`, e.g. `pub fn chain_params(kind, release)` and an `ALL` re-export for tests, or exempt `tests/` and builder arguments in the check. Choose before the adapt is written, so that 30 call sites don't get rewritten later.

### F7 — minor (design docs): consumers written against `&'static`
- EV2's `DmlParams.llmqs: &'static [LlmqParams]` (`designs/EV2-dml.md:496-499`) can't borrow from an owned `ChainParams`. It needs `&'a [LlmqParams]`, or `DmlParams<'a>`.
- LQ1 §2.7 says `ChainParams::llmqs()`. The API is `params.consensus.llmqs` plus `consensus.llmq(t)`. That's a wording fix only.
- CS2 §3.1 promised that "the function signatures do not change" when BS5 lands. They do change: `DevnetOverrides` drops out of `from_chain_params`, and `ParamsError::UnknownNetwork` becomes unrepresentable. Record that as a CS2 design deviation at adapt time.

### F8 — nits
- **Infallible constructors.** Mainnet and testnet `build()` can't fail, and regtest/devnet without arguments fail only on the unreachable devnet-nonce path. Yet every caller unwraps or `.ok()`s a `Result`. `bspcheck.rs` now does `filter_map(...build().ok())`, which would silently skip a network if `build` ever did fail. Consider `ChainParams::default_for(kind, release) -> ChainParams` (documented as panicking only where Core asserts), or at least `expect` in bspcheck.
- **`genesis.rs` `push_data`.** The `Err(_) => return` arm (length > u32) silently leaves the push without its data. It's unreachable, but a comment or a debug assertion would say so.
- **`pkgs/params/CHANGELOG.md`** still reads "Initial release" only. The crate is unreleased (0.0.0), so that's acceptable, but the other crates record breaking API changes (`types`, `pkc`). One line for the `Network` → `NetworkKind`/builder switch would help whoever upstreams this.
- **Block-hash fields as `Hash256`.** `hash_genesis_block`, checkpoints, `superblock_start.1` and `dip0003_enforcement.1` are `Hash256`, not `BlockHash`, so consumers convert at each rule site (EV2's `DmlParams` wants `Option<BlockHash>`). This predates BS5; it's only worth doing if BS5 is touched again.
- **`Bip9Deployment` doesn't carry its `DeploymentPos`.** The pairing is by position with `release.deployments()`. That's fine given F2's `non_exhaustive`; without it, a hand-built `Vec` can misalign silently.

## 2. Conventions

- **Commit lint.** `maint/lint/lint_commit.py -r 481f8c3..ae304a1` passes both:
  - `params%feat!: build Core's chain params per kind and release` (4 lines including the trailer);
  - `params%test: check built params against Core's own dumps`.
- **`feat!`** is correct: `Network`, `Network::chain()`, `ChainParams: Copy`, `network_id: &'static str` and `Bip9Deployments` are removed. That matches the existing `params%refac!` precedent (a258c7b). Both commits are signed.
- **CodeQL.**
  - The policy suite was really run (`/work/scratch/bs5/codeql3.log`: 6 queries, exit 0; not the exit-77 skip).
  - dash-params is `no-serde` in `maint/codeql/rust/policy.model.yml:20`, so the missing serde derives on `NetworkKind`/`Release`/`ChainParams` are correct.
  - **The "no `pub use` of foreign crates" rule is respected:**
    - `lib.rs` re-exports only crate modules;
    - `bitcoin_units::BlockHeight` is `pub(crate) use` in types.rs, so the glob `pub use types::*` doesn't leak it;
    - the old `LlmqType` re-export was already dropped in a258c7b.
  - As a result, consumers need direct `bitcoin-units` and `dash-primitives` dependencies to name `BlockHeight`/`LlmqType` in fixtures. dash-consensus already has both.
- **Semgrep, ruff, markdown:** clean (`lintall2.log`, `semgrep.log`).
- **rust-dashcore commit** `test(node): generate dash-params' chain-params corpus from Core`: conventional, signed, and the body is accurate (139 oracle cases + 6 tool-only `-fastprune` = 145).
- **`_typos.toml` entry** (`SER_NETWORK` under `[default.extend-identifiers]`) is **needed**. I ran typos 1.40.0, the pre-commit pin, on `chainparams_dump.cpp` with the old config: it reports `SER` → `SET` at :161 and :248. The pre-commit hook would then silently rewrite the identifier (memory: rdc typos rewrites). `extend-identifiers` is the right section, scoped to the whole identifier, as `SHUF_00BA` already is.

## 3. The CS2 adapt the implementer deferred

**What changes** (on the CS2 A/B code in `node/integration`, plus CS0's `release.rs`/`ctx.rs` and CS2-C's extra subsidy sites):

| File | Change |
|---|---|
| `dash-consensus/src/params.rs` | `CoreNetwork::of` → `match chain.network` (`NetworkKind`). `NETWORK_IDS`, `DevnetOverrides` and `ParamsError::{UnknownNetwork, DevnetOverridesOffDevnet}` go (or stay as a cheap re-check of `consensus.minimum_difficulty_blocks` etc. off devnet, now that fields are mutable). `test_support::devnet_chain` → the builder. |
| `src/pow/params.rs` | read `consensus.minimum_difficulty_blocks` and `pins.pre_dgw_tolerance_until` (drop the local `MAINNET_TOLERANCE_UNTIL` derivation, F5). `from_network(Network)` → `(NetworkKind, Release)` or removed. |
| `src/subsidy.rs`, `src/payments.rs` | the same: devnet high-subsidy fields from `consensus`, network from `NetworkKind`. |
| `src/release.rs`, `src/ctx.rs` (CS0) | F3: re-export `dash_params::Release`; `default_release(NetworkKind)`. |
| `tests/common/mod.rs` | **the bulk.** `chain_from` copies Core JSON into a mainnet template, and `scrambled` names every field of `ChainParams`/`ConsensusParams` (now about 35 + 50 fields, with `Vec`s, `Option<Block>` and `pins`). Better: build the views from `ChainParamsBuilder` with each recorded case's arguments, and keep a field-scramble guard by **mutating** a built value (F2-compatible). |
| `tests/chain_params.rs`, `tests/properties.rs` | delete the `network_id` string cases (unrepresentable now); proptest strategies over `NetworkKind`. |
| `contrib/node/cs2-diff/src/sweep.rs` + its `Cargo.toml`/`Cargo.lock` | the API move, and the nested `[patch]` bumped by hand (the bump script doesn't see it; memory: fork pin mechanics). The same for `node-fuzz/` if CS0 brings it. |

**Size:** S. About 400–700 changed lines across about 12 files, mostly tests; roughly 0.5–1 agent-day plus one integration-gate run. No consensus logic changes: the views' arithmetic is untouched, and only their inputs move.

**Who:** the **BS5 implementer** owns the re-pin PR, including this adapt. They know the new API and already have `bs5-adapt`. The **CS2 owner** is a required reviewer on `dash-consensus/**` and `contrib/node/cs2-diff/**`, and the CS0 owner reviews the `release.rs` change. Keeping it out of CS2-C keeps a consensus-phase merge from also carrying an API migration and a re-pin.

**When:** after CS0 and CS2-C have merged at the current pin (481f8c3), as the single "BS5 + BS6a" re-pin (§4). Then the adapt covers CS2-C's sites too, once.

## 4. Proposed merge and re-pin order

**base-sdk (`node/base-sdk`), no rust-dashcore effect until a re-pin:**
1. **BS5** → `node/base-sdk` after F2/F4/F5 (and any GPT findings).
2. **BS6a** → `node/base-sdk` when its review closes. Order relative to BS5 doesn't matter: the files are disjoint, and the three-way merge builds and tests green (§0).
3. **BS7** → `node/base-sdk` when done (still implementing). It's also disjoint and merge-trialled clean.

**rust-dashcore (`node/integration`):**

4. **CS0** merges at the current pin 481f8c3, as approved. Don't reopen it for BS5.
5. **CS2-C** (`node/cs2-fp`) rebases on CS0 and merges at 481f8c3, with its planned bits re-sweep.
6. **One re-pin: BS5 + BS6a** → `node/integration`. It contains:
   - `bump-base-sdk.sh` to the `node/base-sdk` head;
   - `bs5-adapt` (5fad804d) and `bs6a-adapt` (242e0b7f; trial merge with bs5-adapt clean);
   - the §3 adapt, with F3 and F6 applied;
   - the nested `cs2-diff`/`node-fuzz` pins bumped by hand;
   - the full integration gate (dashd suites included) and a cs2-diff re-sweep, because its parameter source changed.
7. **BS16** (rust-dashcore, S, never gates) as a small follow-up PR on top of the re-pin.
8. **BS7's re-pin** separately, when BS7 merges, or folded into the next one (BS8). It has no node-consensus consumers yet, and P2a can work against the base-sdk branch until then.

**Why one re-pin for BS5 + BS6a, not two:**
- Each re-pin costs a full integration gate, hand bumps of the nested workspaces, and a cs2-diff re-run.
- BS5 and BS6a are both node-consensus-gate items, both in review now, disjoint, and already shown to build together.
- BS5's re-pin can't be split from its adapt anyway (F1).

**Fallback:** if CS2-C isn't merge-ready when step 6 is, run step 6 first, and CS2-C absorbs the adapt in its rebase (it re-sweeps at merge regardless). Don't hold BS5 back for long: LQ1a, EV2, CH1 and the CS2 views all read it.

## 5. Summary

BS5 delivers what the PLAN row asks:
- an owned, builder-made `ChainParams` that replays Core's constructors;
- Core's error strings byte for byte;
- a Core-produced corpus that I regenerated byte-identically at both pins;
- a test suite that caught every Core-semantics mutation I planted.

The API fits CS0, LQ1, CH1 and CS2 with small changes: `NetworkKind` survives, `dash_params::Release` becomes the only `Release`, and `HardCodedPins` is the quirk-34 home. Before merging, the crate should:
- add `#[non_exhaustive]` to the param structs (F2);
- document the builder's command-line-only contract for AR1 (F4);
- test and bound the pins (F5).

On the rust-dashcore side, BS5 can't be re-pinned without the dash-consensus adapt (F1). That adapt is S-sized, owned by the BS5 implementer and reviewed by the CS2 owner, and it should ride in one combined BS5 + BS6a re-pin after CS0 and CS2-C merge, with F6's `check-release-refs` accommodation decided first.
