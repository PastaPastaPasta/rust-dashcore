# CS2 phases A+B, review r2 (Opus, focused)

| | |
|---|---|
| Branch | `node/cs2-pow-subsidy` at `8c2193a4`, worktree `~/workspace/rdc-wt/cs2-pow-subsidy` (untouched; all runs in clones) |
| Fix range | `1d4dbbec..8c2193a4` on the first-parent line after r1's `d88308d8`, including `18bdfb3c`, which merges `node/integration` at `ca66c271` |
| Round 1 | `reviews/CS2ab-r1-opus.md` (M1–M3, m1–m4, n1–n5), `reviews/CS2ab-r1-gpt.md` (N1) |
| Design | `designs/CS2-pow-subsidy.md` rev1 |
| Core | `v24.0.0-rc.3` and `v23.1.8` at `~/workspace/dash`. Oracle shims `23769258c239` / `350ce242a59c`, matching `dash-oracle-sys/pins.txt` |
| CS0 | `node/cs0-node-primitives` at `61056e73` (`~/workspace/rdc-wt/cs0-node-primitives`) |
| Inputs | my own reading and runs, plus the `code-review` skill at high effort as one input (triaged in §6) |
| Scratch | `/work/scratch/cs2ab-r2-opus/`: `rdc/` (branch clone, harness build), `merge/` (CS0 trial merge), `headers/` (copies of the dumps), `*.log` |

## 0. Summary

**The refactor changed no consensus decision.** I diffed `pow/`, `subsidy.rs` and `payments.rs` from `d88308d8` to `8c2193a4` line by line. I also re-ran every piece of Core evidence through the in-tree harness, and the tallies match the author's and my own r1 runs exactly:

- the corpus `--check`;
- both full history sweeps;
- two fresh-seed differentials.

The r1 majors are resolved:

- **M1:** `provisional.rs` is now item-for-item identical to CS0's `ctx.rs`, doc comments included. Its merge checklist is accurate: the trial merge has zero compile errors, and every test, clippy and `check-pure.sh` pass.
- **M2:** `from_chain_params(&ChainParams, DevnetOverrides)` exists. All 17 Core chain selections now go through it, and a scramble guard catches uncopied fields. I confirmed by mutation that the guard fires.
- **M3:** the harness is in the tree. It is reproducible from its seed and pinned archives, and the mainnet sweep passes.

The r1 minors are resolved as well:

- **m1:** all 20 reallocation periods are compared with Core.
- **m2:** 68589 is a single anchored constant, and quirk 15 is now checked against Core's own `GetPaymentsLimit`. The pin's upper bound remains a tracked TODO for CS3.
- **m4** is fixed. **m3** is improved but still has holes (m1 below).

I found no blocker and no major. The remaining minors fall into three groups:

- the floating-point confinement scan still lets a float out of `fp` in four ways, which I demonstrated (m1);
- the parameter views treat an unknown `network_id` as "other" instead of refusing it (m2);
- harness fail-closed and reproducibility gaps (m3), and a domain note on negative `-budgetparams` (m4).

## 1. What I ran

| Check | Result |
|---|---|
| `cargo test --locked -p dash-consensus --all-features` (debug) and `--release` | 46 tests in 12 targets, all pass in both. The release run exercises `broken_chain`'s release path |
| `cargo clippy --locked -p dash-consensus --all-features --all-targets -- -D warnings`; `cargo fmt --all --check`; `contrib/node/check-pure.sh` | clean; clean; `pure crates: 2 crate(s) ok` |
| `contrib/node/cs2-diff/run.sh record dash-consensus/tests/data --check` | **29 files checked**, exit 0, `Cargo.lock` untouched. Tallies: 1669 next_work equal / 34 refused; 558 payments_limit equal / 19 FpPending / 1 both refuse; 1525 subsidy equal / 365 FpPending / 43 both refuse / 107 halving-loop refusals |
| `run.sh sweep main mainnet.bin` (sha256 `58e6e286…f8251`, the dump PROVENANCE names) | 2,551,767 headers. PoW true **2,551,766**. `next_work` = header bits **2,483,177**. At heights ≤ 68589, **49,649** equal Core's `GetNextWorkRequired` on both pins (15,182 with equal bits, 34,467 with differing bits, highest **68588**). KGW `FpPending` exactly **15200..=34139** (18,940). V20+ subsidy equal on both pins **563,991**. Exit 0, 16 s |
| `run.sh sweep test testnet.bin` (`a8852602…59b27`) | PoW **1,568,459**, `next_work` exact **1,568,459**, V20+ subsidy **663,360**, pre-V20 `FpPending` 905,099. Exit 0, 7 s |
| `run.sh diff 2000 424242` | 22,000 PoW, 22,000 MN payment, 21,445 next_work, 20,000 platform share and 25,657 subsidy all equal, plus 558 payments_limit. 3 KGW `FpPending`, 552 refused windows. Byte-for-byte the r1 tallies for this seed |
| `run.sh diff 2000 777001` (a seed nobody had used) | 22,000 / 22,000 / 21,398 / 20,000 / 25,717 equal, 558 payments_limit, 1 KGW, 601 refused windows. Exit 0 |
| Dump authenticity | 13 spot heights, from genesis to the tip and across both block tips, through `contrib/oracle-cli` (read-only). Every hash in the dump is at the same height in the reference node's header tree |
| Trial merge of CS0 (§4) | 5 conflicted files, as the checklist says; 0 compile errors; 85 `dash-consensus` tests and CS0's `node-l1`/`node-sink`/`dash-sha256` tests pass; `check-pure.sh` ok; `record --check` ok |
| Scramble-guard mutation probe | I removed the copy of `nPowTargetSpacing`, then of `nSuperblockStartBlock`, from `chain_from`. Both times the guard panics with "a field the views read is not copied from Core's" |
| `fp_confinement` probe (§5) | four escapes pass `floats_stay_in_fp` |
| Fail-closed probes on the sweep | a dump corrupted by +1 s in one testnet `nTime` panics at the next height (exit 101). A mainnet dump **truncated to 100,000 headers exits 0** with no V20 comparisons (m3) |

## 2. Behaviour did not change

I compared the logic before (`d88308d8`) and after (`8c2193a4`), then re-ran Core's evidence on the new code (§1).

| Area | Change | Equivalence |
|---|---|---|
| `pow/btc.rs` min-difficulty walk | It now carries `(id, header)` and re-reads the header only after a step | The same predicate order as Core's `pindex->pprev && nHeight % interval != 0 && nBits == limit` (`pow.cpp:152`). The initial `header = tip = chain.header(last)` |
| `pow/btc.rs` retarget ancestor | `ancestor(..)` is now `Option`, and `None` leads to `broken_chain` | Under the contract `ancestor(last, h)` is `Some` for `h ≤ height`, which holds for `first_height = height − (interval − 1) ≥ 0`. The debug and release tests now cover both walks, at heights 575 and 2,000,000 |
| `nbits_acceptable` | It now calls `PowConfig::tolerance_applies` | The same predicate: `height ≤ until` and bits differ gives `FpPending`, otherwise `Ok(bits == expected)` |
| `PowParams::from_chain_params` | `pre_dgw_tolerance_until` now comes from `network_id == "main"` and `minimum_difficulty_blocks` from the overrides | For `dash-params`' three networks this equals the old `Network::Main` match. Sweep and `chain_params.rs` assert `from_network == from_chain_params(Core JSON)` on both pins |
| `SubsidyParams` | `network` comes from `network_id`. The genesis reward is now an argument. `Superblocks` is shared. `block_subsidy` takes `Option<&HeaderCtx>` | `prev.bits.to_base()` is the raw word, and `prev.height` keeps the same `i32` check. The off-devnet high-subsidy refusal moved into `DevnetOverrides::for_chain` and is now stricter: any non-default override is refused |
| `PaymentParams` | `superblock_cycle` now comes from `Superblocks` | The same value and the same `> 0` check |
| `dgw.rs`, `check_proof_of_work`, `subsidy` arithmetic, `payments` arithmetic | unchanged apart from comments | — |

The corpus regenerates with byte-identical answers (`--check`). The only differences from r1 are `core_version` strings and the cases r1 asked for (PROVENANCE explains them). Sweeps and differentials give identical tallies (§1). **Every consensus decision is identical.**

## 3. `from_chain_params` and Core's options at both pins

`chainparams.cpp` is the same at both pins for every option CS2 reads. The v24 line numbers are below; v23.1.8 is 40–44 lines earlier.

| Core option | Core | Crate |
|---|---|---|
| `-powtargetspacing` (devnet only) | `ParseInt64`, `< 1` throws, then sets `nPowTargetSpacing` (`:1352-1368`) | the raw field is copied. `ParamsError::TargetSpacing` outside `[1, i64::MAX/72]`, the design's documented overflow exclusion |
| `-minimumdifficultyblocks`, `-highsubsidyblocks`, `-highsubsidyfactor` (devnet only) | `GetIntArg` truncated to `int` (`:1247-1255`); the defaults are 0, 0 and 1 (`consensus/params.h:192-194`, v23 `:180-182`) | `DevnetOverrides` (i32, defaults 0, 0 and 1). Refused off a devnet (`params.rs:53`). Tested on main for pow and subsidy |
| `-budgetparams` (regtest) | three `ParseInt32` values with **no range check** (`:1166-1186`) into `nMasternodePaymentsStartBlock`, `nBudgetPaymentsStartBlock` and `nSuperblockStartBlock` | the raw fields are copied, but `dash-params` types them `BlockHeight` (u32), so negative values cannot be expressed (m4) |
| `-testactivationheight` (regtest: brr, v20, mn_rr, …) | `ParseInt32`, `0 ≤ h < INT_MAX` (`:1052`) | `BRRHeight` and `V20Height` copied, and `height()` refuses values ≥ 2³¹ |

Evidence that the constructor reproduces Core's views:

- The 17 recorded selections all go through `from_chain_params`. They are the 12 devnet variants, including the negative, wrapping and unused high-subsidy factors and mindiff 4000 × spacing 86400, plus `regtest-brr`, `regtest-budget` and the base chains.
- The recorder asserts that the v23.1 and v24.0 views are equal for every selection (`record.rs` `corpus()`).
- Every answer in the corpus and differential is compared with Core.

`devnet` is Core's exact `NetworkIDString()` (`chainparamsbase.cpp:15`; the devnet name is held separately), so keying on `"devnet"` is right. But see m2.

**The "differs in every field" guard** (`tests/common/mod.rs:100-111`, `:115-260`) works:

- The exhaustive destructuring of `ConsensusParams` makes a new `dash-params` field a compile error.
- Every field is perturbed (`^1`, `+ONE`, `!`, a hash flip, another LLMQ type).
- Both of my mutation probes fired.

Limits, all nits (n2):

- Only `consensus` is scrambled. A view that read a top-level `ChainParams` field (`is_test_chain`, `credit_pool_period_blocks`, …) would not be caught.
- A field read only through a predicate that `^1` does not flip, for example a `> 0` check, would also pass.

## 4. The 68589 constant and the quirk-15 `nBits`

**68589.** `pow/params.rs:44` `MAINNET_TOLERANCE_UNTIL = 68_589` is now the single literal; the tests and harness import it.

- Core has `NetworkIDString() == CBaseChainParams::MAIN && nHeight <= 68589` with `nHeight = pindexPrev->nHeight + 1` at `validation.cpp:4476` (v24) and `:4110` (v23.1.8). I read both. The anchors are right, and `tolerance_applies(height)` takes the block's own height, as Core's `nHeight` is.
- Recorded Core evidence: 34,467 headers at ≤ 68589 carry bits that differ from Core's `GetNextWorkRequired`, and the highest is 68588. `history_sample.rs` asserts this, and my sweep reproduces it. That bounds the pin from below.
- The bound from above still has no Core verdict. That needs Core's `ContextualCheckBlockHeader`, and it is tracked as `TODO(CS3)` in `history_sample.rs` and PROVENANCE. This is acceptable for A+B.

**Quirk 15.** `subsidy.rs` computes `if pow_allow_min_difficulty_blocks { pow_limit.compact(false) } else { 1 }`. That is Core's `int nBits = fPowAllowMinDifficultyBlocks ? UintToArith256(powLimit).GetCompact() : 1` at `governance/superblock.cpp:156` (v23.1.8 `governance/classes.cpp:152`), which I read at both pins.

The evidence is now Core's own `GetPaymentsLimit`, through O1b, and it discriminates. In `payments_limit.json`, Core's limit equals `treasury(1) × cycle` in 7 mainnet cases and `treasury(powLimit) × cycle` in 8 testnet and 3 regtest-budget cases. In every one of those cases the two candidates differ. The test requires at least 4 discriminating cases of each kind. The circularity r1 m2 found is closed.

## 5. Findings

### m1 (minor): `fp_confinement` still lets a float out of `src/fp`

**Where:** `dash-consensus/tests/fp_confinement.rs:76-84`, `:162-196`.

**What I did.** I put this into a clone's `src/fp/mod.rs` and a sibling `src/fp_probe_user.rs`, then ran `floats_stay_in_fp`. It **passed**:

```rust
pub trait AsFloat { fn value(&self) -> f64; }          // rule 2 never looks inside a trait
impl AsFloat for Difficulty { fn value(&self) -> f64 { self.0 } }
pub struct Pair {
    pub count: u32,          // a pub named field opens a "multi-line signature"…
}
pub enum Horizon {           // …which swallows this line, so variants are never tracked
    Crossed(f64),
    Open,
}
pub enum OneLine { Crossed(f64), Open }   // depth 1 − 1 = 0: variants never scanned
// outside fp:
crate::fp::difficulty(a).value() <= crate::fp::difficulty(b).value()   // no literal, no f64 token
crate::fp::difficulty(a).value() <= 1_000f64   // `1_000` prefix is neither empty, all-digit, nor ends in `_`
```

**Why it matters.** The trait escape is the realistic one. Phase C's `fp` is likely to implement a trait for `Difficulty`. Outside `fp`, comparing two `.value()` results needs no float token, and `clippy::float_arithmetic` ignores comparisons. That is exactly the design §2.2 rule-5 breach the scan exists to stop.

Mitigations:
- `rustfmt` expands one-line enums and structs, so `OneLine` is only theoretical.
- Phase C has not landed, so nothing leaks today.

**Fix (before phase C's `fp` merges, ideally in this branch):**
- Inside `src/fp`, report every float-naming line inside a `trait` body or an `impl <Trait> for` body. Only private inherent methods are exempt.
- Open a continuation only for item keywords (`fn`, `struct`, `enum`, `trait`, `type`, `const`, `static`, `impl`, `use`), never for field lines.
- Track enum variants from the item line even when the body closes on that line.
- In `has_float`, treat a prefix of digits and `_` as numeric, so `1_000f64` is caught.
- Add each probe above to `the_scan_finds_what_it_must`.

A sturdier alternative: parse `src/fp` with `syn` (as a dev-dependency) and reject any non-private item, trait item or trait impl whose types mention `f32`/`f64`.

**Residual limits to document:**
- the line scanner mis-tracks raw strings containing `"` and string literals that span lines, which can blank real code after the closing quote;
- macros are not expanded.

### m2 (minor): an unknown `network_id` silently becomes "other"

**Where:**
- `subsidy.rs:128-132`: `match chain.network_id { "main" => Main, "devnet" => Devnet, _ => Other }`;
- `pow/params.rs:75`: `network_id == "main"`;
- `params.rs:53`: `network_id != "devnet"`.

**Scenario.** BS5 adds devnets to `dash-params` (PLAN: "NetworkKind + Devnet"). Suppose a `ChainParams` reaches the crate with a `network_id` that is not exactly Core's `NetworkIDString()`. Examples are `"devnet-cs2"` (Core's datadir and `-devnet` name form) or `"mainnet"`. Then:
- the 68589 tolerance silently disappears on mainnet;
- a devnet's block-1 subsidy takes the non-devnet path. Core's `isDevnet` would force the fixed base once `nPrevHeight >= nHighSubsidyBlocks`, and with the default 0 that is already true at block 1;
- the devnet overrides are refused, which is safe, but only when they are non-default.

All three are fail-open except the last.

**Fix:** in `from_chain_params`, refuse any `network_id` outside Core's four IDs (`main`, `test`, `regtest`, `devnet`) with a new `ParamsError::UnknownNetwork`. Share one constant list with `tests/common::NETWORK_IDS`. When BS5 lands `NetworkKind`, match on that instead.

### m3 (minor): harness gaps in reproducibility and fail-closed behaviour

The harness is reproducible where it matters:
- seeded RNG;
- archives pinned by sha256 (`build.rs`), and `Oracle::new` asserts `ARCHIVES_PINNED`;
- `run.sh` verifies the base-sdk `[patch]` table;
- `record --check` passes, and the differential tallies are bit-identical across runs and reviewers.

Every disagreement panics: the pins disagree, an unexpected refusal, a mismatch, a coverage under 90% of windows.

The gaps:

1. **A sweep's exit 0 does not prove coverage.** `contrib/node/cs2-diff/src/sweep.rs:103-176` prints tallies but asserts none. My 100,000-header truncation of the mainnet dump exits 0 with zero V20 subsidy comparisons. The sweep also never checks that the dump is Core's chain: no genesis or checkpoint hash comparison, no X11 hash of headers. PoW uses the *next* header's `hashPrevBlock` as the hash.
   - **Fix:** assert that the dump reaches past V20 (and, on mainnet, past 68590). Assert that every checkpoint in Core's chainparams within the dump equals `history.hash(h)`.
   - Once CS0 is in, X11-hash each header (`dash-pow`) against its successor's `hashPrevBlock`.
   - Optionally take `--expect-sha256`, or print the input's sha256 so a log is self-identifying.
2. **The dump itself can't be regenerated to the recorded sha256.** `dump_headers.py` always dumps to the node's current best header, which has already moved to 2,551,913. It checks links only at chunk and segment joins (`:110`), although the README says "checks every link". It takes `getchaintips`' first entry at the header height without filtering on status (`:82`).
   - **Fix:** add `--until <height>`, so PROVENANCE's dumps (`0..=2551766`, `0..=1568459`) can be rebuilt byte-exact. Check every intra-chunk link. Pick the tip with status `headers-only` or `active`.
3. **`run.sh:29` builds without `--locked`.** A rebuild can silently rewrite the committed `contrib/node/cs2-diff/Cargo.lock`. The CS0 trial merge did exactly that, which is expected and is in the checklist. A reviewer cannot tell whether a given run used the committed graph.
   - **Fix:** build `--locked` by default, and add an explicit `run.sh --update-lock`. Alternatively, fail if `git diff --quiet Cargo.lock` is false after the build.
4. **`DASH_ORACLE_ALLOW_UNPINNED=1` passes through.** Then `record --check` (which ignores `core_version`) and the sweeps run against dev archives, with nothing in the output saying so.
   - **Fix:** cs2-diff should refuse `!ARCHIVES_PINNED` unless a `--dev` flag is given, and print both pins' `core_version()` at the start of every command.
5. **`record --check` does not detect stale files.** It compares only the files it generates. A file the recorder stopped producing would linger unnoticed, as `payments_limit_treasury.json` would have if it had not been deleted by hand. Nit-level.

### m4 (minor): negative `-budgetparams` cannot be expressed; record it as an exclusion

Core's regtest `-budgetparams` accepts any `int` (`chainparams.cpp:1174-1184`, no range check). `nBudgetPaymentsStartBlock = -1` makes Core's strict `nPrevHeight > nBudgetPaymentsStartBlock` true at `nPrevHeight = 0`, so block 1 pays a treasury part. With 0 it does not.

`dash-params` types the field `BlockHeight` (u32), so such a chain cannot be built. Clamping −1 to 0 in BS5's builder would silently change block 1's split. That is fp-pending today, and becomes a real answer in phase C. A negative superblock start behaves like 0 for every reachable height, so only the budget start matters.

**Fix:** list this in design §2.5 as an excluded regtest-only (DEBUG_ONLY) input. Tell BS5 its builder must refuse negative `-budgetparams`, not clamp them; BS5 could alternatively carry `i32`.

### Nits

- **n1** `pow/params.rs:29` `pre_dgw_tolerance_until: Option<i32>` can now only be `Some(68589)` or `None`, because `PowParams::new` is private. A `bool` (`mainnet_tolerance`) plus the constant would remove the unreachable `Some(n)` state. Keep it if BS5 will make the height a `dash-params` field.
- **n2** The scramble guard covers only `ConsensusParams` and value-changing reads (§3). Scramble the top-level `ChainParams` fields too, except `network_id`, which is copied.
- **n3** `Superblocks` is shared by derivation, but nothing stops a caller from building `SubsidyParams` and `PaymentParams` from two different `ChainParams`. CS0's `ConsensusCtx` carries one `&ChainParams`. Consider a single `ConsensusViews::from_chain_params` that builds all three.
- **n4** `sweep.rs:205` assumes the tolerance evidence is in DGW territory: `h − 25` is too short a window for a BTC retarget height, and underflows for `h < 25`. It is latent, since today's value is 68588. Size the window like `core_next_work` does.
- **n5** `main.rs:288-297`: `check_payments_limit` makes the two quirk-15 treasury calls even in `diff`, which drops them. They are cheap; split them out only if `diff` grows.
- **n6** The merge checklist: `rustfmt` places `pub use params::…` after `pub use ctx::…`, and says so if you insert it above. This is trivial, but worth saying "then `cargo fmt`" in `provisional.rs`.

## 6. The `code-review` skill's findings, triaged

| Skill finding | Disposition |
|---|---|
| One-line `pub enum` variants never scanned | accepted, in m1. `rustfmt` makes it theoretical |
| A `pub` field line opens a signature and swallows the next enum | accepted and **reproduced**, m1 |
| Trait items and trait impls are not checked | accepted and **reproduced**, m1 (the main escape) |
| `1_000f64` missed | accepted and **reproduced**, m1 |
| `dump_headers.py` tip chosen without status | accepted, m3.2 |
| `sample` range of 25 headers too short for a BTC height | accepted as nit n4 (latent) |
| Intra-chunk links not checked | accepted, m3.2 |
| 68589 hard-coded, against the repo `CLAUDE.md` rule | **rejected.** Core hard-codes it in `ContextualCheckBlockHeader`, keyed on `NetworkIDString()`, not in chainparams. Design §3.1 and PLAN's BS5 row ("the 68589 pin in `dash-params`") assign the move to BS5. It is now one anchored constant (r1 m2's request) |
| `Option<i32>` is derivable | nit n1 |
| `diff` wastes treasury oracle calls | nit n5 |

The skill found no consensus defect. Its review of the refactor itself agrees with §2: `ParamsError`, `DevnetOverrides`, `Superblocks`, the BTC walk, `ancestor`'s `Option`, the guard, and quirks 12 and 15.

## 7. Round-1 dispositions

| r1 item | Status |
|---|---|
| M1, provisional mismatch with CS0 | **resolved.** The items are identical to `ctx.rs@61056e73`. The checklist is accurate and the trial merge is green (§1) |
| M2, `from_chain_params`/`DevnetOverrides` | **resolved** (§3). New minors: m2, m4 |
| M3, harness out of tree; mainnet sweep broken | **resolved.** The harness is in tree and the sweep is fixed and reproduced. New minor: m3 |
| m1, four reallocation periods unchecked | **resolved.** `quirks.json` has every period's first height, and `quirk_12` asserts all 20 |
| m2, Core-derived pins checked against themselves | **resolved** for quirk 15 (Core's `GetPaymentsLimit`). For 68589, resolved from below, with the upper bound a tracked TODO for CS3 |
| m3, inferred floats | **partly resolved.** Literals and `pub` signatures are now caught; see the new m1 |
| m4, clippy duplicates | **resolved.** The crate file keeps determinism, sole-encoder and libm bans. I/O stays in `pure-clippy.toml`; CS0's `check-pure.sh` merges the crate file, as the trial merge confirms |
| n1–n5 | resolved (`15200..=34139`, the libm rationale, the 86400 comment, `Option<&HeaderCtx>`, `genesis_reward(&Block)`) |
| GPT N1, overflow comments | resolved. `payments.rs:7-19` now lists which products can overflow. I checked the thresholds: 2.46×10¹⁶ / 3.75×10⁸ ≈ 6.6×10⁷, and `i64::MAX / 10⁴` ≈ 9.22×10¹⁴ |

## Verdict

**APPROVE-WITH-NITS.**

- The refactor preserves every consensus decision: diffed by hand, and confirmed by the corpus, both full history sweeps on both pins, and two fresh-seed differentials.
- `from_chain_params` reproduces Core's devnet and regtest-option parameters at both pins.
- The 68589 and quirk-15 values match Core's source, and quirk 15 now has Core's own discriminating answers.
- The CS0 merge is clean.

Before phase C's `fp` module lands, close m1. I would do m2 now: it is a one-line fail-closed change in consensus code. m3 and m4 are cheap and can ride with phase C.
