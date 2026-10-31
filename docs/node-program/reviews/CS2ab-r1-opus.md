# CS2 phases A+B, review r1 (Opus)

| | |
|---|---|
| Branch | `node/cs2-pow-subsidy` at `d88308d8` (6 commits on `node/integration` `a900c163`), worktree `~/workspace/rdc-wt/cs2-pow-subsidy` |
| Design | `designs/CS2-pow-subsidy.md` rev1 (§3, §4, §5, §6.1, §10 phases A and B) |
| Reconciliation target | `node/cs0-node-primitives` at `fdfb54ee` (in progress); CS0 design r2 §8.4 |
| Core | `v24.0.0-rc.3` (0364ed62) and `v23.1.8` (728f5055), read at `~/workspace/dash` |
| Inputs | my own reading and runs, plus the `code-review` skill at high effort (one input; its findings are triaged in §5) |
| Scratch | `/work/scratch/cs2ab-r1-opus/` (sweep and diff logs, the trial merge, the clippy probe) |

## 0. Summary

The ported logic is correct. I compared every function against Core at both pins and found no consensus defect:

- **Proof of work:** `GetNextWorkRequired`'s dispatch order, BTC with the min-difficulty walk over raw words, `CalculateNextWorkRequired`, DGW (the 23-interval span, per-step truncation, the wrap), the 2 h and 4-spacing rules with the ×10 wrap, and `CheckProofOfWork`.
- **Subsidy:** `GetBlockSubsidyHelper`'s integer path, the decline with its early exit, the devnet factor, the strict-`>` treasury, and `GetPaymentsLimit`.
- **Payments:** `GetMasternodePayment` (v24 era and v23 `fV20Active`), `GetMnRewardEraAfter`, `PlatformShare` wrapping before the range check, and the operator/owner arithmetic, `ProRataFloor` and `SplitAmountByShares`.
- **`arith_uint256` overloads:** `*= int64` resolves to `*= uint32`, and `/= int64` resolves to `/=(uint64_t)` (`arith_uint256.h:159-162,210-211`). `div_u64` equals Core's limb path and its `> u32::MAX` fallback.

I also ran the evidence independently, including the **complete mainnet and testnet history sweeps on both pins, which pass** (§3).

The three majors are about hygiene and evidence, not correctness:

- the provisional crate does not reconcile cleanly with CS0, and it says it does;
- the parameter constructors depart from the design's `from_chain_params`/`DevnetOverrides` API, and that leaves the Core-derived fields to be derived again in test code;
- the harness that produced the recorded corpus and the history sweep exists only in scratch, and its mainnet sweep fails against this branch.

## 1. What I ran

| Check | Result |
|---|---|
| `cargo test -p dash-consensus` | 39 tests, all pass (17 unit; chain_params 1, core_vectors 3, fp_confinement 2, noncanonical 3, oracle_corpus 6, properties 6, quirk 08 1, quirk 12 1) |
| `cargo clippy -p dash-consensus --all-features --all-targets -- -D warnings`; `cargo fmt --check` | clean |
| `contrib/node/check-pure.sh`, `contrib/node/check-base-sdk-pins.sh` (in a scratch clone) | `pure crates: 1 crate(s) ok`; `base SDK pin ok` |
| Does the libm ban actually fire? Probe crate, clippy 0.1.99, with the same `disallowed-methods` paths | yes: `f64::powf`, `f64::mul_add` and `f64::sqrt` are all reported, including through `let f = f64::powf` |
| **History sweep, testnet** (the implementer's `cs2-diff sweep`, rebuilt against `d88308d8`) | 1,568,459 `CheckProofOfWork` true; 1,568,459 `next_work` equal to the header's `nBits`; 663,360 V20+ subsidies equal to Core's `GetBlockSubsidyInner`/`GetSuperblockSubsidyInner` on **both pins**; 905,099 pre-V20 `FpPending`. EXIT 0 |
| **History sweep, mainnet** (after a one-line harness fix, see M3) | 2,551,766 PoW true; 2,483,177 `next_work` exact (h ≥ 68590); 49,649 at h ≤ 68589 equal to Core's `GetNextWorkRequired` on both pins (34,467 headers differ from it, as the tolerance allows); KGW `FpPending` at exactly 15200..=34139 (18,940); 563,991 V20+ subsidies equal on both pins. EXIT 0 |
| Fresh-seed live differential (`cs2-diff diff 2000 424242`, both pins, all 17 chain selections) | 22,000 PoW, 22,000 MN payment, 21,445 next_work, 20,000 platform share, 25,657 subsidy all equal; refusals and `FpPending` only where expected |
| Trial merge of `node/cs0-node-primitives` into this branch (scratch clone) | see M1 |

The header dumps are `/work/scratch/cs2/headers/{mainnet,testnet}.bin`, sha256 in `/work/scratch/cs2ab-r1-opus/headers.sha256`. The logs are `sweep-{main,test}.log` and `diff-seed424242.log` in my scratch.

## 2. Findings

### M1 (major): the provisional crate does not match CS0, but says it does

**Where:**
- `dash-consensus/src/provisional.rs:4` says "The names and signatures are CS0's, so when CS0 merges, `pow` switches its imports … and this module is deleted". That is not true.
- `provisional.rs:32` and `:34`, `src/pow/btc.rs:55-56`, `src/pow/mod.rs:156-171`, `tests/common/mod.rs:172,206,210` (which `benches/next_work.rs` shares).

**What differs.** CS0's approved design r2 (§8.4, lines 554-559) and its branch (`dash-consensus/src/ctx.rs`) have:

| | CS2 provisional | CS0 r2 / branch |
|---|---|---|
| `header` | `fn header(&self, id) -> &HeaderCtx` | `-> HeaderCtx`, by value ("works over a locked or epoch-managed arena"; the change came from a CS0 review) |
| `ancestor` | `-> BlockId` (total) | `-> Option<BlockId>` (Core's `GetAncestor` returns null) |
| `HeaderCtx` | 3 fields | 7 fields (`hash`, `version`, `mtp`, `chain_work` as well) |
| Contract | "`prev(id)` is `Some` exactly when `height > 0`; `ancestor` defined for every `h <= height`" | only "every `BlockId` must come from this arena; implementations may panic otherwise" |

CS2's §3.8 `broken_chain` reasoning depends on the `prev` contract, and CS0's trait does not state it.

**What a merge actually costs.** I merged `node/cs0-node-primitives` into `node/cs2-pow-subsidy` in a scratch clone. There were content conflicts in five files: `Cargo.toml`, `Cargo.lock`, `dash-consensus/Cargo.toml`, `dash-consensus/clippy.toml` and `dash-consensus/src/lib.rs`; `LICENSE` and `ci-groups.yml` merged cleanly. I resolved them naively: CS0's files, plus CS2's modules, `FpPending`, the `next_work` bench, and deleting `provisional.rs`. The result had:
- 2 library errors, both in `btc.rs`: `ancestor`'s `Option`, and passing a by-value `HeaderCtx` where `calculate_next_work_required` takes `&HeaderCtx`;
- then 3 errors in each of 9 test and bench targets: E0053 on `header`/`ancestor` in `VecChain` and `Orphans`, and E0063 on the 3-field `HeaderCtx` literals.

The work is small, but whoever merges second meets it unannounced, and the doc comment tells them there is nothing to do.

**Fix, now, on this branch:**
1. Make `provisional.rs` exactly CS0 r2: return `HeaderCtx` by value and `ancestor -> Option<BlockId>`. In `btc.rs`, `let Some(first) = chain.ancestor(..) else { return broken_chain(p) }`.
2. Add a test-only constructor for `HeaderCtx` (height, time, bits, everything else zero), so tests and the bench never write a struct literal.
3. Ask CS0 to add the `prev`-is-`Some`-iff-height-`> 0` sentence to `ChainAncestry`'s doc. That is a one-line change to an interface skeleton CS0 says its owners review. Otherwise reword §3.8 and `broken_chain` around CS0's contract.
4. Record the reconciliation of `Cargo.toml`, `clippy.toml` and `lib.rs` as a checklist in the commit or the task report:
   - keep CS0's `features = ["std"]` deps;
   - add the `[[bench]] next_work`;
   - keep CS2's libm bans, and drop CS2's I/O, clock and thread duplicates, which CS0 §2 leaves to `pure-clippy.toml` (see m4);
   - add `FpPending`.

### M2 (major): the parameter constructors differ from the design's API, and Core-derived fields are derived twice

**Where:** `src/pow/params.rs:115-130` (`PowParams::from_network`), `src/subsidy.rs:148-176`, `src/payments.rs:105-114`, `tests/common/mod.rs:58-148`.

**The design.** §3.1 says each view is "built from `&dash_params::ChainParams` plus a `DevnetOverrides` (zero, zero and one by default)". It also says "When BS5 lands, `from_chain_params` reads the devnet fields directly, and the function signatures do not change." CS0's `ConsensusCtx` carries `params: &'a ChainParams`, not a `Network`.

**The branch.** Each view has two layers:
- a raw `*Config` with public fields;
- `from_network(Network)`, which covers only the three `dash-params` networks without options.

Devnets, and regtest with `-budgetparams` or `-testactivationheight`, have no constructor in the crate. A caller must hand-fill the `*Config` and must know to derive:
- `pre_dgw_tolerance_until` (68589 iff network id is `main`);
- `payments_limit_bits` (powLimit compact iff `fPowAllowMinDifficultyBlocks`, else 1, quirk 15);
- `SubsidyNetwork`;
- `genesis_reward`;
- the same `superblock_cycle` in both `SubsidyConfig` and `PaymentConfig`. Nothing ties those two together.

The deviation is not listed in design §8 or in the commit messages.

**Concrete consequences:**
- The 17 Core chain selections in the recorded corpus all go through the test helpers, which re-derive these fields themselves (`tests/common/mod.rs:70`, `:117-122`). The crate's own derivation is checked only for main, test and regtest (`tests/chain_params.rs`).
- For the two Core-derived rules, the test and the crate run the same typed-in logic, so the check is circular (m2).
- When CH1 or CS3 have a `&ChainParams` in hand, they will have to map it back to a `Network` by `network_id` string, or write a third copy of the derivation.

**Fix (small):**
- Add `PowParams::from_chain_params(&ChainParams, &DevnetOverrides)`, and the same for `SubsidyParams` and `PaymentParams`.
  - `SubsidyParams` needs the genesis reward: take it as an argument, or add a `Network` hint, because `ChainParams` has no genesis at 33fcc59.
  - Implement `from_network(n)` as `from_chain_params(n.chain(), &DevnetOverrides::default())`.
  - Derive `payments_limit_bits`, the tolerance pin and the network class inside the constructor from `network_id` and the consensus fields.
- In `tests/common`, build a `ChainParams` from Core's JSON by overriding the regtest or main `ChainParams` consensus fields, and the overrides for devnets. Then call the crate constructor, so the 17 selections test the crate's derivation.
- Alternatively, amend design §3.1/§8 and get the manager's sign-off on the `*Config` + `from_network` shape. The duplication and circularity would remain.

### M3 (major): the recorder, live differential and history sweep are out of tree; the mainnet sweep fails against this branch

**Where:** `tests/data/PROVENANCE.md` ("the recorder was CS2's differential harness (`cs2-diff record`, kept in the task's scratch directory until the L1 targets land with O1's crate)"). The harness is at `/work/scratch/cs2/diff/` (`main.rs`, `cases.rs`, `sweep.rs`).

**Problems:**
1. **The corpus cannot be regenerated or audited from the repository.** It holds 1,650 `next_work` windows and the PoW, subsidy, payment, platform-share, payments-limit, quirk and non-canonical files. Its generator lives in scratch, which is not durable, and design §6.1 asks for recorded JSON "regenerated by a script". Phase A's `diff_next_work` (design §10) does not exist in tree either. CS0 set the precedent: `node-l1/tests/recorded_fixtures.rs` re-checks CS0's recorded expectations against the live oracle, and CS0 brings `node-l1` and `dash-oracle-sys` into the workspace.
2. **The mainnet history sweep fails against `d88308d8`.** At `sweep.rs:69` it asserts `nbits_acceptable(h, bits, e) == Err(FpPending)` for every h ≤ 68589. The branch's `nbits_acceptable` returns `Ok(true)` when the bits are equal, which is correct (`pow/mod.rs:102-107`). Reproduction: `cs2-diff sweep main mainnet.bin` panics with `left: Ok(true) right: Err(FpPending)`. So the mainnet sweep was not run to completion against the final code, and nothing in the commits records a sweep result. Once that one assert is changed to expect `Ok(true)` when `e == bits`, the sweep passes in full on both pins (§1).

**Fix (before A+B merge):**
- Land the recorder, the live diff and the header sweep in the tree, in `node-l1` (or `contrib/node/`) once CS0 has merged, with the line fixed.
- Point `PROVENANCE.md` at that path and the exact command.
- Record the sweep tallies of §1 (or the implementer's own rerun) in the task report.

### m1 (minor): four reallocation-table entries have no Core expectation run on every PR

`payments.rs:145-146` `REALLOCATION_PERIODS` matches `masternode/payments.cpp:103-123` at both pins (I checked by eye). On every PR, the Core-produced expectations reach:
- in `masternode_payment.json`, Classic era, after the reallocation start: periods 0, 1, 5–10, 12–14 and 16–18;
- in O4's BRR records: periods 0–2.

Periods **3, 4, 11 and 15 (540, 546, 582, 594)** are never compared with Core, so a typo there passes the per-PR suite. The quirk-12 test checks only the start, one step and the cap.

**Fix:** record one mainnet Classic case per period, 19 cases, in `quirks.json` or `masternode_payment.json`.

### m2 (minor): the Core-derived pins are checked against themselves

- **68589.** It is typed in at `pow/params.rs:128` and again at `tests/common/mod.rs:70`. Every per-PR test derives it the same way. The mainnet sweep bounds it from below: a smaller pin would require exact `nBits` at heights where history differs. But a larger pin would go unnoticed, because heights 68590+ all match exactly.
- **`payments_limit_bits` (quirk 15).** The derivation in `subsidy.rs:170-174` is copied in `tests/common/mod.rs:117-122`. `payments_limit_treasury.json` compares only V20+ heights, where the fixed base makes `nBits` irrelevant, and the recorder picked the limit `nBits` itself (PROVENANCE: "the oracle has no `GetPaymentsLimit` entry point until O1b").

So phase B has no Core evidence for quirk 15's `nBits` choice, and none for the 68589 pin from above. That is acceptable for A+B, provided no one counts these tests as evidence.

**Fix:**
- State the gap in `PROVENANCE.md` or the test docs.
- Close it in phase C with `oracle_superblock_payments_limit` (O1b) and with CS3/O2a's `ContextualCheckBlockHeader` verdict at 68589/68590.
- Under M2, keep the pin as a single constant in the crate rather than two literals.

### m3 (minor): `fp_confinement` misses floats whose type is inferred

`tests/fp_confinement.rs` flags only lines that spell `f32` or `f64`. In phase C, a line such as `if fp::difficulty(b) <= 75.0 { … }` in `subsidy.rs` has no such token and no float arithmetic, so neither this test nor `clippy::float_arithmetic` catches it. Design §2.2 rule 5 requires that `f64` never escape `fp`.

**Fix, in phase C:** also flag float literals (`\b\d+\.\d+\b`, `\d+e\d+`) outside `fp/`, and add a check that no `pub` item under `fp/` names `f64` in its signature. The scan also skips *every* directory named `fp` at any depth; restrict it to `src/fp` as the design specifies (nit).

### m4 (minor): `clippy.toml` duplicates part of `pure-clippy.toml`, and `check-pure.sh` ignores the crate file

`dash-consensus/clippy.toml:7-14,22-26` repeats a subset of `contrib/node/pure-clippy.toml`'s I/O, clock and thread bans. It is missing `fs::read`, `OpenOptions`, `thread::scope` and others. `check-pure.sh` sets `CLIPPY_CONF_DIR` to root + `pure-clippy.toml`, so the purity run never sees this file's HashMap and libm bans. Those are enforced only by the ordinary `cargo clippy -D warnings`. CS0 §2 keeps I/O, clock and thread bans in the shared file only, and plans to move HashMap/HashSet there (N6).

**Fix:**
- At reconciliation, drop the duplicates and keep the libm bans plus CS0's sole-encoder bans.
- Make sure the ordinary clippy job covers `dash-consensus`, or append the crate's file in `check-pure.sh`.

### Nits

- n1 `src/lib.rs:40` and `src/pow/mod.rs:22`: "mainnet heights 15200..34139" reads as exclusive in Rust notation. KGW runs for next heights 15200..=34139, as the sweep confirms. Write `15200..=34139`.
- n2 `dash-consensus/clippy.toml:15-17`: "Core's consensus floating point uses only IEEE + - * / and comparisons". Core calls `pow` twice. The port avoids libm by writing `x·x` and using the KGW table. Reword.
- n3 `src/pow/btc.rs:70`: "clamped, the timespan fits" in `u32` holds only for Core's fixed `nPowTargetTimespan` of 86400. `PowParams` accepts any timespan up to `i64::MAX/7`. The code is right either way, since it mirrors `operator*=(uint32_t)`. Say "for Core's 86400", or have `PowParams::new` require `4·timespan ≤ u32::MAX`.
- n4 `src/subsidy.rs:285-289`: `prev: Option<(u32, u32)>` is an unnamed `(bits, height)` pair of the same type, so a swap compiles. This follows design §4.1, but `(CompactTarget, u32)` or a `&HeaderCtx` would remove the hazard.
- n5 `tests/common/mod.rs:76-107`: a hand-written genesis parser. After CS0 merges, use `dash_consensus::decode_block`.

## 3. Test evidence, by kind

- **Core vectors (L0, O4):**
  - `pow_tests` (6 `next_work` records, main and devnet);
  - `subsidy_tests` (3 exact, 9 `FpPending`);
  - all 71 `block_reward_reallocation_tests` checkpoints, including MN payment, platform share and the template's owner output;
  - each `next_work` record asserts that Core's two copies of the expected bits agree.

  `split_amount_by_shares` is ported by hand with Core's own assertions until D8. Good.
- **Recorded oracle corpus:** I checked a sample of edges directly.
  - `platform_share.json` contains `share == MAX_MONEY` at `5_600_000_000_000_000` and refusals one above and at −3.
  - `subsidy.json` carries Core's own witness (`block_bits_1`, `block_bits_207fffff`) that every answered subsidy ignores `nBits`. That is a nice touch.

  Reproducibility is weak (M3), and coverage of the reallocation table has gaps (m1).
- **History sweep:** this is the strongest evidence for phase A+B. It covers every DGW and BTC header on mainnet and testnet, the ≤ 68589 range against Core's own `GetNextWorkRequired`, and every V20+ subsidy against both pins. It passes in full (§1). It is not yet in tree and was broken against this branch (M3).
- **Properties:** "no panic" holds over arbitrary chains and parameter sets, and `FpPending` appears only on KGW heights. The split sums hold, the empty list is handled, and a zero share total is the only error.

## 4. Are the constants sourced from Core?

Every literal carries a `// Core:` anchor, and I re-read each against rc.3 and v23.1.8. They fall into four groups by evidence:

- **Backed by Core outputs on every PR or in the sweeps:**
  - `COIN` and `MAX_MONEY` (`consensus/amount.h:15,26`);
  - the fixed base 5, `/14`, the 10% and 20% divisors and the strict `>`;
  - DGW's 24 and ×3/÷3, BTC's ×4/÷4, 7200 s, ×10 and 2·spacing;
  - the increment ladder (`/5`, `/20`, `/40`, k = 1, 2 and 3–7, 9);
  - `375/1000`.
- **Typed in, but only partly checked against Core:** `REALLOCATION_PERIODS` (m1), and the 68589 pin and quirk-15 limit bits (m2).
- **Derived from `dash-params`, which equals Core's chainparams JSON on both pins** (`tests/chain_params.rs`): powLimit, spacing and timespan, KGW/DGW heights, increase block and period, BRR, superblock start and cycle, V20 height, halving interval, budget start. The genesis reward is checked against a parse of Core's own `genesisBlock`.
- **Not yet exercised against Core:** the operator/owner `/10000` with operator bps ≠ 0, and multi-owner payout lists. The design assigns these to the phase D coinbase history leg; edge unit tests follow the binaries' semantics (design §6.2).

## 5. The `code-review` skill's findings, triaged

| Skill finding | Disposition |
|---|---|
| `fp_confinement` misses inferred floats | accepted as m3 |
| 68589 hard-coded twice (repo CLAUDE.md "no hardcoded network parameters") | accepted, folded into m2/M2. Design §3.1 allows the anchored literal until BS5 |
| `block_subsidy` tuple swap hazard | nit n4 |
| `15200..34139` notation | nit n1 |
| **DGW `as u32` truncates from spacing ≥ 59,652,324, not 2²⁹** | **rejected.** `nActualTimespan = last.time − first.time` comes from two `uint32` times, so it is < 2³². The upper clamp (72·spacing) can replace it only with a *smaller* value, and is inactive once 72·spacing ≥ 2³². Only the lower clamp, 8·spacing, can produce a value ≥ 2³², which needs spacing ≥ 2²⁹. The code comment and design §2.5/§3.6 are right |
| BTC "clamped, the timespan fits" | nit n3; holds for Core's only timespan, 86400 |
| `clippy.toml` libm rationale | nit n2 |
| `SubsidyConfig` derivable and duplicated fields | accepted, part of M2 |
| crate `clippy.toml` vs `CLIPPY_CONF_DIR` | accepted as m4 (verified by reading `check-pure.sh`) |
| hand-written genesis parser in tests | nit n5 |

The skill found no correctness defect either. Its Core anchor and overload checks agree with mine.

## Verdict

**CHANGES-REQUIRED.** The port itself is correct and well evidenced: the history sweeps pass in full on both pins. The required changes are:
- align `provisional.rs` with CS0 r2's `ChainAncestry`/`HeaderCtx`, and drop the false "signatures are CS0's" claim (M1);
- add the design's `from_chain_params` + `DevnetOverrides` constructors, or get the deviation approved (M2);
- land the recorder, live differential and history sweep in tree, with the mainnet sweep fixed and its results recorded (M3).

The minors (m1–m4) are cheap and should go in the same round.
