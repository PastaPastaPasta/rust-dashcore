**Verdict: APPROVE-WITH-NITS.** The consensus arithmetic is unchanged. The parameter views build correctly from BS5's `ChainParams`, and every CS2 check passes in a fresh worktree of the head. Three cosmetic nits, none blocking.

# BS5 adapt of `dash-consensus`: CS2-owner review

| | |
|---|---|
| Branch | `node/bs5-adapt` at `ed390f11`, on `node/integration` `b4a2c50f` (CS2-C merged). Read only; not touched |
| Commits reviewed | `50ec303d` (views on BS5's `dash-params`), `82211796` (`Release` from `dash-params`), `d471175f` and `ed390f11` (`check-release-refs.sh` only), `3dc929be` (tolerance pin tied to the network; the view guard) |
| Scope | the CS2 parts: `dash-consensus/src/{params,pow,subsidy,payments,fp}`, its tests and `contrib/node/cs2-diff`. `release.rs`, `ctx.rs` and `check-release-refs.sh` are CS0/O17's; I only built and ran them |
| Worktree | `/work/scratch/cs2-bs5rev/rdc`, a fresh detached worktree of `ed390f11`. I copied in the implementer's four uncommitted pin files (root and cs2-diff `Cargo.toml`/`Cargo.lock`, base-sdk `8c23be55`, the head of `node/bsdk-bs5-params`). Nothing was committed. Logs are in `/work/scratch/cs2-bs5rev/*.log` |
| Load | 41–52; every run used `systemd-run --user -p CPUQuota=800%`, one at a time |

## 1. Consensus arithmetic is unchanged

I compared every non-test function of `b4a2c50f` and `ed390f11`:

- `fp/mod.rs`, `fp/kgw_pow.rs`, `pow/kgw.rs`, `pow/dgw.rs` and `pow/btc.rs` are byte-identical.
- In `pow/mod.rs`, `subsidy.rs` and `payments.rs`, only the view constructors changed: `from_chain_params`, with `from_network` removed. That leaves `get_next_work_required`, `check_proof_of_work`, `nbits_acceptable`, `div_u64`, `block_subsidy_helper`, `decline`, `pre_v20_base`, `clamp`, `block_subsidy_inner`, `superblock_payments_limit` and every payment function untouched.
- `src/params.rs` swaps `CoreNetwork`/`DevnetOverrides` for `check_devnet_arguments` and adds the `TolerancePin` error.

What the views compute is the same as before:

- **`PowParams`.** The fields come from `ChainParams::consensus` as before. `minimum_difficulty_blocks` is now the consensus field, where it used to be the override. The tolerance is `pins.pre_dgw_tolerance_until` through `height()`.
- **`SubsidyParams`.** `NetworkKind` maps to `SubsidyNetwork` exactly as `CoreNetwork` did (`Mainnet`→Main, `Devnet`→Devnet, `Testnet`/`Regtest`→Other). The high-subsidy fields come from `consensus`.
- **`PaymentParams`.** Unchanged apart from dropping the network-name check, which can no longer fail now that `network` is an enum.

The executable evidence agrees:

- `record --check` reproduces the whole corpus.
- `cs2-diff kgw` gives 16,076 windows equal to Core under glibc's FMA and SSE2 `pow`.
- Both history sweeps end with their recorded tallies: mainnet's 2,551,766 headers, including the 68,589-height tolerance era and 18,940 KGW heights, and testnet's 1,568,459.

## 2. Building the views from `ChainParams`

- **The tolerance pin and `TolerancePin` (`pow/params.rs`).**
  - `from_chain_params` refuses a pin missing on mainnet or present anywhere else. That is Core's `NetworkIDString() == MAIN` keying, now made checkable.
  - The `// Core: validation.cpp:4476 (v23.1.8: 4110)` anchor moved with it.
  - `every_dash_params_network_is_valid` checks that `ChainParamsBuilder` gives 68589 on mainnet and `None` on the other three. `the_tolerance_is_dash_params_pin` covers both relabelling directions. A pin above `i32::MAX` is refused as `Height`.
  - A hand-edited mainnet pin (`Some(100)`) is accepted, like any other `ChainParams` field the views take as data. Quirk 9 still asserts the literal 68589, and the history sample still requires the highest differing height, 68588, to be within the pin. I'm fine with that division of labour.
- **The devnet arguments.** `check_devnet_arguments` refuses non-default `minimum_difficulty_blocks`, `high_subsidy_blocks` or `high_subsidy_factor` off a devnet. `PowParams` and `SubsidyParams` call it; `PaymentParams` reads none of them and doesn't. That matches the old `for_network`.
- **The view guard in `tests/common`.**
  - Each view is built twice: from `ChainParamsBuilder` with the recorded selection's arguments, and from Core's raw dump fields copied into parameters scrambled in every other field. The two must be equal.
  - `assert_all_differ` makes the scrambling exhaustive over `ChainParams`' and `ConsensusParams`' `Debug` fields, so a field `dash-params` adds later can't slip past. This is stronger than the old copy-only check.
  - `chain_params.rs` runs it for every recorded file on both pins: 17 selections plus the 7 large-spacing devnets at v24.0, and the base four at v23.1.
- **The large-spacing devnets.**
  - `LARGE_SPACING` moved to `tests/common` beside `SELECTIONS`, with all seven entries and their per-spacing doc intact. `cs2-diff` re-exports both.
  - `oracle_corpus::next_work_required` resolves them through `selection()`, so their 700 windows are replayed on views built by the builder. The per-chain floor of 80 DGW windows still holds.
  - The `PROVENANCE.md` text is updated to match.

## 3. The quirk tests still test what they should

- Quirks 8, 11, 15 and 40, `kgw_boundaries`, `noncanonical_bits` and `core_vectors` are unchanged. They reach the views through `tests/common`, so the guard above covers their inputs.
- Quirk 9 now reads the pin from the mainnet view, asserts it is 68589, and checks Core's verdicts at the boundary as before.
- `properties.rs` now draws `NetworkKind::ALL` and gives each network its own pins and, on devnets, its own arguments. Same coverage as the four `NetworkIDString`s it replaces.

**Mutation probes.** Each plants one fault in the scratch worktree and runs the tests that should catch it; the file is restored afterwards. All six were caught:

| Planted fault | Caught by |
|---|---|
| no `TolerancePin` check | `pow::params::tests::the_tolerance_is_dash_params_pin` |
| tolerance dropped (`None` everywhere) | `mainnet_history_sample` |
| devnets take the non-devnet subsidy path | `oracle_corpus::subsidy_helper` (Core's devnet answers) |
| devnet arguments accepted off a devnet | `chain_params::devnet_arguments_off_devnets_are_refused` |
| `chain_from` stops copying `nHighSubsidyFactor` | `chain_params::dash_params_match_core` (the view guard) |
| `PowParams` ignores `minimum_difficulty_blocks` | `chain_params::dash_params_match_core` (the large-spacing devnets no longer validate) |

## 4. Runs at `ed390f11` (fresh worktree, local pin)

Every step exited 0:

| Step | Result |
|---|---|
| `cargo fmt --all --check` | ✓ |
| `cargo clippy --locked -p dash-consensus --all-features --all-targets -D warnings` | ✓ |
| cs2-diff `cargo clippy --locked --all-targets -D warnings` | ✓ |
| `cargo test --locked -p dash-consensus` | ✓ 25 binaries, 183 passed, 0 failed |
| `contrib/node/check-no-libm.sh` | ✓ no libm among the 110 symbols the release rlib imports |
| `contrib/node/check-kgw-fp.sh` | ✓ default ifunc `glibc-fma`; `-AVX2,-FMA,-FMA4` `glibc-sse2` |
| `cs2-diff/run.sh record dash-consensus/tests/data --check` | ✓ |
| `cs2-diff/run.sh kgw`, default and `GLIBC_TUNABLES=…-AVX2,-FMA` | ✓ 16,076 equal and 4,018 non-vacuous pairs, each |
| `cs2-diff/run.sh sweep main` / `sweep test` | ✓ the recorded tallies |

Outside CS2's scope, for the record: `contrib/node/pin-delta/check-release-refs.sh` fails here, but only on ST2's `dashd-store/src/flatfile/blocks.rs:393-394`, its provisional `Release` match. Integration fails the same way, so the cause is not this branch.

## Nits (non-blocking)

1. **`SubsidyParams::from_chain_params(chain, genesis_reward)`** still takes the genesis reward as an argument. `ChainParams` now carries `genesis`, and the real callers derive it as `genesis_reward(&chain.genesis)`: `tests/common::subsidy_params` (and cs2-diff through it) and `derived_per_network`. Only unit and property tests pass other values (`50 * COIN`, `0`, any `i64`), and they could vary `chain.genesis` instead. Deriving it inside `from_chain_params` would drop an argument a production caller could get wrong; the `GenesisReward` error would move into the constructor.
2. **`ParamsError::DevnetOverridesOffDevnet`** names a type that no longer exists. Something like `DevnetArgumentsOffDevnet` would read better; the message text is already right.
3. **`cs2-diff/src/record.rs`** builds the base chains' v23 views with a literal `"v23.1"` (around line 124), but the large-spacing loop uses `v23.pin().name()`. Use the latter in both places. Also, `fp.rs::exhaustive_subsidy`'s `.expect("main, test or regtest")` accepts any `VARIANTS` name, while `Chain::new(chain, &[])` drops that selection's arguments. Only `main` and `test` are swept, so this is cosmetic.

Design errata for DEC-72, the manager's to fold in: CS2 design §3.1 and §7 still describe `DevnetOverrides`, `from_network` and the `MAINNET_TOLERANCE_UNTIL` constant.

## Summary

The adapt does what DEC-72 asks:

- the arithmetic is untouched;
- the views build from BS5's `ChainParams`, with the network as an enum, the devnet arguments as consensus fields and the 68589 pin from `HardCodedPins`, tied to mainnet by `TolerancePin`;
- the new two-sided view guard is a real improvement over the old copy-only check, and fails when a field is missed.

Every CS2 check passes in a fresh worktree of the head with the local pin, and all six mutation probes were caught.
