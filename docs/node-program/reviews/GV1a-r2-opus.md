**Verdict: ACCEPT WITH NITS**

# GV1a review (r2, Opus): narrow re-check of the r1 fix round

**Range:** `node/gv1a` `6c9a71ca7..c810b257f` (12 commits, `9f9cf6db9` … `c810b257f`), worktree `~/workspace/rdc-wt/gv1a`, which is clean at `c810b257f`.
**Core reference:** `v24.0.0-rc.3` (`da567d7dba`); rc.4 checked at `0e7382df80`.
**Method:** a reflink copy of the worktree in `/work/scratch/gv1a-r2-review/src`, built with `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=8` and `TMPDIR=/work/tmp`. Mutants were applied only in that copy. The worktree itself was not touched.

**Finding counts:** Blocker 0, Major 0, Minor 3, nit 2. Numbered by severity: 1–3 Minor, 4–5 nit; the sections below follow the brief's order.

All 12 r1 findings are resolved. The mutant claim reproduces exactly. The new `gov_superblock` comparison is faithful to Core and kills 11 of my 12 extra mutants of `superblock_valid`. The one survivor is finding 1 below.

possible security note: see manager

## 1. r1 findings, one line each

| r1 | verdict |
|---|---|
| Sol 1: unbounded vote signature read | **Resolved.** `vote.rs:45-61` reads the CompactSize at offset 84 with Core's range check and rejects counts above 96 before `GovVote::decode` runs. `signature_offset` checks the offset, and `signature_count_bound` covers 97 with a full payload, `MAX_SIZE + 1`, `u64::MAX` and a non-canonical 96. See §4. |
| Sol 2: selector 3 aliased mainnet | **Resolved.** `params_of` is four-way (`gov.rs:325-358`) with a named devnet `gv1a`. `proposal_selectors_reach_four_chains` asserts the network, the chain name, `-devnet=gv1a`, the devnet genesis and both Core verdicts per selector. See §5. |
| Sol 3: `is_valid` not compared with Core | **Resolved, with one gap** (finding 1). See §3. |
| Sol 4: public ring fields | **Resolved.** The fields are private. `new`, the accessors and `from_parts` were added. See §6. |
| Opus 1: sentinel thresholds | **Resolved.** `sentinel_thresholds_where_each_term_wins` pins mainnet quorum 10 on weight 12 (9 votes fail, 10 pass), `n/10 = 4` on a 42-weight list, `(2n)/3 = 9` at weight 14, and the empty list keeping `dirty`. This kills M10, M11 and M12. |
| Opus 2: trigger lifetimes | **Resolved.** `trigger_lifetimes_on_mainnet` covers `AddTrigger` at +24, +25, +576 (live) and +577 (expired), and `Clean` keeping a Valid trigger to +576 and an Executed one to the cycle. This kills M31, and M32 too. |
| Opus 3: far-future expiry only on testnet | **Resolved, L0 only.** `trigger::tests::expiry` loops over testnet, devnet and regtest. This kills M2 (it fails on Devnet). The suggested `diff_gov` seed was not added (accepted, see the note below). |
| Opus 4: height gates | **Resolved, L0 only.** `off_cycle_heights_are_never_triggered` covers `best`, `is_triggered`, the untouched flags and `payments` at 181. This kills M21 and M22. |
| Opus 5: early return | **Resolved, L0 only.** `triggered_stops_at_the_first_funded_trigger`, with a control showing that the later trigger alone does get `cached_delete`. This kills M1. |
| Opus 6: confirmations 1–5 and unmined | **Resolved, L0 and L1.** `tests/collateral.rs` checks the unmined branch and 0/1/5/6 confirmations, with texts equal to `object.cpp:810-873`. L1 `short_confirmations_postpone` uses `fixture_confirmed(k)` and compares records, ops and Core's own postponed/held outcomes. This kills M7 in both suites, and M6 too. |
| Opus 7: boundary votes and out-of-`int` height | **Resolved, L0 only.** `vote::tests::time_bound_is_inclusive` covers +3600 and +3601. `trigger::tests::event_height_is_an_int` covers `INT_MAX`, +1 and `INT_MIN − 1`. This kills M5 and M16. |
| Opus nit 8: `GOVERNANCE_...` placeholder | **Resolved** (`vote.rs:164`). |

Note on "L0 only": for Opus 3, 4, 5 and 7 I suggested Core-linked seeds as well. D41 instead pins them with L0 tests that write out Core's behaviour, and states that no fuzz input or seed changed, which keeps `diff_gov_vote`'s certified corpus as it was. Every former survivor now dies, and I checked each L0 expectation against rc.3. I accept the substitution. Replaying the vote boundary against Core remains the farm corpus's job.

## 2. Mutant reproduction

I applied `/work/scratch/gv1a/opus-mut/gvmut.patch` to the scratch copy with `git apply`. It reproduces my r1 mutants faithfully. I built once and ran each `GVMUT=n` with `--no-fail-fast`.

| M | L0 failing test | panic site (a real assertion) |
|---|---|---|
| none | — (all pass; L1 7/7 pass) | — |
| 1 | `triggered_stops_at_the_first_funded_trigger` | `tests/superblocks.rs:237` `assert_eq!` on the later trigger's flags |
| 2 | `trigger::tests::expiry` | `trigger.rs:319`, message "Devnet" |
| 5 | `vote::tests::time_bound_is_inclusive` | `vote.rs:291` `passes_static_checks(now + 3600)` |
| 7 | `confirmations_window_and_unmined` (and L1 `short_confirmations_postpone`) | `tests/collateral.rs:56`, message "1" |
| 10 | `sentinel_thresholds_where_each_term_wins` | `tests/tally.rs:182` (3 of 42 not funding) |
| 11 | same | `tests/tally.rs:170` (9 of 12 on mainnet not funding) |
| 12 | same | `tests/tally.rs:208` (empty list keeps `dirty`) |
| 16 | `trigger::tests::event_height_is_an_int` | `trigger.rs:369`, `Ok(-2147483648)` |
| 21 | `off_cycle_heights_are_never_triggered` | `tests/superblocks.rs:185` `best(.., 181).is_none()` |
| 22 | same | `tests/superblocks.rs:186` `!is_triggered(.., 181)` |
| 31 | `trigger_lifetimes_on_mainnet` | `tests/superblocks.rs:267`, message "631433" (= event + 25) |

The remaining r1 mutants agree with the implementer's table. These are killed by L0: M3, M6, M9, M13, M15, M19, M27 and M32. These survive L0 and are killed by L1 `census`: M4, M8 and M14. **The claim reproduces: all 11 former survivors die to real assertions in the named tests.**

## 3. `node_l1::gov_superblock` against `CSuperblock::IsValid`

I traced `superblock.cpp:248-334` and `masternode/payments.cpp` `IsBlockValueValid` and `IsBlockPayeeValid` at rc.3.

**What holds up:**
- Each IsValid branch has a case that reaches it alone:
  - output count: "repeated past the outputs" (non-V24 would otherwise pass);
  - payments limit: "budget over the limit". The miner's one-duff cut keeps the value at or below `reward + limit`, so the superblock maximum passes, and below `reward + total`, so the value branch passes;
  - block value: "over the block value";
  - match: missing, reordered, and duplicate with one output;
  - the V24 scan start: "duplicate, one output" and "repeated per output" give `!is_v24`.
- Both modes are reached only on testnet. That is correct: mainnet's rc.3 `DEPLOYMENT_V24` is `NEVER_ACTIVE` (`chainparams.cpp:213`), and `is_v24` is taken from Core's `SuperblockCheck`.
- The test maps every kind to an expected verdict, so a case that drifts into the "exceeded superblock max value" branch (`None`) fails rather than passes. It also requires every kind to be built. The suffix fix from the internal round (`9cd2a595f`) is right: Core's value errors start with "coinbase pays too much at height …".
- Comparing `payee_valid` only on valid cases is the most the oracle allows: it reports `None` once the value check fails (`dash-oracle-sys/src/payments.rs:308`).
- rc.3→rc.4: `superblock.cpp` and `masternode/payments.cpp` are unchanged.

**My own mutants of EV3's `superblock_valid`** (`dash-evo/src/payments.rs:575`, scratch only, `GVR2`-gated), run against `superblock_is_valid_against_core`:

| E | mutation | result |
|---|---|---|
| 1 | height gate dropped | **survives** (also survives `cargo test -p dash-evo` and `-p dash-governance`) |
| 2 / 3 | output count `<=` / dropped | killed ("repeated per output" / "past the outputs", Defined) |
| 4 / 5 | limit `>=` / dropped | killed ("budget at" / "over the limit") |
| 6 / 7 | value `>=` / dropped | killed ("exact" / "over the block value") |
| 12 | value compared with `reward` without the total | killed ("exact") |
| 8 / 9 | V24 start inclusive / strict start before V24 | killed ("duplicate, one output", Active / Defined) |
| 10 | scan always restarts at 0 | killed ("reordered") |
| 11 | matches on the script only | killed ("missing", testnet) |

### Finding 1 (Minor): the "off-height" case pins the payments-limit branch, not IsValid's height branch

**Location:** `node-l1/src/gov_superblock.rs:228-241`; claim in notes D39 (`docs/node-program/designs/GV1a-notes.md:74`); code `dash-evo/src/payments.rs:584`, reached through `dash-governance/src/trigger.rs:127`.

D39 says the height branch "is compared through the off-height trigger" and that the mutation check "kills every viable mutant". Off a superblock height, `GetPaymentsLimit` is 0 (`superblock.cpp:149-151`). Every fixture trigger has a positive total, so `check(!ours, …)` at `:232` holds through the limit branch even with the height gate removed (E1).

E1 is not equivalent. Core accepts zero-amount payments: `ParsePaymentAmount` takes `"0"` (`MoneyRange(0)`), and `CGovernancePayment` sets `fValid` for any amount (`superblock.cpp:415-424`). With a zero total and a coinbase holding the 0-value outputs, the limit and value branches pass, and only the height gate rejects. `Superblock::is_valid` is a public API, and `AtTip` and GV1b callers reach it. Nothing in L0, EV3 or L1 pins the gate.

**Fix:** add a unit test (in `trigger.rs` tests, or EV3 next to `superblock_valid`) that parses a trigger with `"payment_amounts": "0"`. On a coinbase of a miner output at or below the reward plus a 0-value output to the payee, assert `is_valid(.., h, ..)` is `true` at a superblock height `h` and `false` at `h + 1`. Then reword D39: the off-height L1 case pins "never triggered" on Core's side and GV1a's rejection through the limit, and the height gate is pinned by the new unit test.

## 4. `vote.rs` signature-count bound: the domain is unchanged (D37 is right)

`GovVote` is `outpoint(36) ‖ parent(32) ‖ outcome(4) ‖ signal(4) ‖ time(8) ‖ Vec<u8>` (base-sdk `569f4adfe` `pkgs/primitives/src/gov.rs:267-290`), so the CompactSize sits at a fixed offset of 84. `Vec<T>::decode` reads the same `CompactSize::decode_size`, with the same `MAX_SIZE` and canonicality checks (`pkgs/types/src/codec.rs:305`). Case by case:
- Shorter than 85 bytes: rejected before and after.
- A count that fails `decode_size`: rejected by both.
- A count above 96: before, either the decode failed or the length was not 65 or 96.
- A count of 96 or less: the old path, unchanged.

So the accepted set is identical, and `diff_gov_vote`'s domain is unchanged. D37's correction also holds: `into_len(data.len())` caps the count at the bytes left and reserves in `MAX_VECTOR_ALLOCATE` batches, so the old decode never allocated an oversized vector. Core rc.3 `vote.h:199` `LIMITED_VECTOR(obj.vchSig, BLS_SIG_SIZE)` is unchanged in rc.4.

## 5. `diff_gov` selector change (`a8bd0301f`)

**Devnet parameters.** I dumped `params_of(3)` from the harness and compared it with rc.3 `CDevNetParams`:

| parameter | value |
|---|---|
| budget start / cycle / window | 4100 / 50 / 10 |
| superblock start / cycle / maturity window | 4200 / 24 / 8 |
| quorum / filter elements | 1 / 500 |
| V20, DIP0003, DIP0003 enforcement, BRR heights | 2 |
| spacing | 150 |
| halving interval | 210240 |
| high-subsidy blocks / factor | 0 / 1 |
| `payments_limit_bits` (`powLimit` compact, `fPowAllowMinDifficultyBlocks`) | `0x207fffff` |
| address prefixes | 140 / 19 |
| Platform HRP | `tdash` |
| devnet name | `devnet-gv1a` (Core's `GetDevNetName`), which matches the devnet genesis coinbase |

`is_mainnet` and `is_testnet` are both false, so the off-mainnet expiry applies and votes are signed over the message, as in Core. Everything matches. rc.4 does not change `chainparams.cpp`.

**Seeds.** At `6c9a71ca7` the 390 `diff_gov` seeds were 162 objects (case 1) and 76 proposals each on selectors 0, 1 and 2. **No old seed used selector 3**, so no seed changed meaning. The range adds 76 files and modifies or deletes none. Each new seed is byte-identical to an old proposal seed except for selector 3. "466" in D38 is the total `diff_gov` seed count, not the number of regenerated seeds.

### Finding 3 (Minor): `diff_gov`'s certification waiver predates the devnet domain

**Location:** notes D38 (`GV1a-notes.md:73`) against D31 (`:66`, DEC-389 (1): `diff_gov` waived as certified on two certify runs at `568878726`).

D38 notes that selector 3 now tests devnet, "which changes `diff_gov`'s domain". The devnet path was never fuzzed, and any selector-3 input in the farm corpus silently moved from mainnet to devnet. No row records whether DEC-389's waiver covers the new domain.

**Fix:** add a manager decision to D38: either extend the waiver explicitly (the devnet shares testnet's rules except for the signing branch, and has the 76 Core vectors), or schedule a `diff_gov` night and certify after the change.

## 6. `RateCheckBuffer::from_parts`

Core's `SERIALIZE_METHODS` (`governance.h:150-153`) rejects only stream errors: truncation, and a `vecTimestamps` count above `MAX_SIZE` or non-canonical. These are `dash-coreimport`'s reader's job (`netstate/governance.rs:266-279`), before `from_parts` ever runs. Core rejects no field values: any vector length and any `int` positions load, and are then indexed unchecked. So `from_parts` (`rate.rs:47`) rejects a strict superset of what Core's deserializer rejects. Its rule (five timestamps, both positions in `0..5`) is exactly `dash-coreimport`'s `RateCheckBuffer::is_consistent`, so the two layers agree. `empty = true` with `start != end` is accepted, which is right: Core's `AddTimestamp`, `GetCount` and `GetMin`/`GetMax` are well defined there, and the Rust ring follows them.

### Finding 4 (nit): `from_parts`'s `None` covers states Core loads

**Location:** `dash-governance/src/rate.rs:43-47`.

Some rejected states are well defined in Core: a vector longer than five with both positions below 5 uses only the first five slots. If GV1b drops a masternode's record on `None`, `MasternodeRateCheck` then lets that masternode through, because no record means pass (`rate.rs`, `masternode_rate_check`). That is more permissive than Core, which keeps the history.

**Fix:** say in the doc that `None` also covers corrupt but well-defined states. Add a D40 sentence that GV1b's choice on `None` (drop the record, or refuse the file) is a recorded GV1b decision.

## 7. rc.3 → rc.4 differences in the governance code GV1a touches

`src/governance` changes in `common.h`, `governance.cpp` and `governance.h`. `superblock.cpp`, `object.cpp`, `vote.{h,cpp}` and `votedb` are unchanged.

### Finding 2 (Minor): rc.4 bounds the governance object's signature to 96 bytes at deserialization

**Location:** `node-l1/src/gov.rs:854` (`decode_whole::<GovObject>`), the model's object decode for script `OBJECT` ops (`gov.rs:1334`), and the generator's "signature (u8 length and bytes)" edit (`gov.rs:30`). The crate has no bounded object decoder (compare `vote::decode_network`).

rc.4 `governance/common.h` adds `MAX_SIGNATURE_SIZE{96}` and `READWRITE(LIMITED_VECTOR(obj.vchSig, MAX_SIGNATURE_SIZE))` to `Governance::Object`'s network serialization. At the rc.4 pin, Core rejects an object with a signature of 97 to 255 bytes at decode, while GV1a decodes it and goes on to the record. `diff_gov` case 1 and `diff_gov_vote` `OBJECT` ops will diverge ("decode verdict") as soon as the oracle moves to rc.4 (the R5 rc.4 train).

**Fix:** add `object::decode_network(bytes, release)` that bounds the signature count before decoding, with the bound only from rc.4, mirroring `vote::sig_count_within_limit`. Route both harness paths through it, and add a 97-byte-signature regression. If that waits for the rc.4 rebase, record it now as a D-row rebase item so it isn't found by the farm.

The other rc.4 governance changes (in `governance.{h,cpp}`, including the `governance.dat` format bump to version 17) belong to GV1b and `dash-coreimport`, not this crate. GV1b's design should cite rc.4 for them.

## 8. Other

### Finding 5 (nit, process): the GVMUT run applied the patch inside the live worktree

**Location:** `/work/scratch/gv1a/opus-mut/run.sh:3` (`cd /work/home/agent/workspace/rdc-wt/gv1a`).

D41 says the patch was reversed afterwards, and the worktree is clean at `c810b257f` with no diff against my copy. But a mutated tree on the branch's own checkout risks an accidental commit, and it races any concurrent gate or review that reads the worktree.

**Fix:** run future rounds in a reflink copy, as this review did.

## Validation

All of this ran in the scratch copy, which was deleted afterwards.
- `cargo test -p dash-governance`: passes, both unpatched and with the patch and no `GVMUT`.
- `cargo test -p node-l1 --features oracle --test l1_gov`: 7/7 pass, 12 s.
- `cargo clippy -p dash-governance -p node-l1 --features node-l1/oracle --all-targets -- -D warnings`: clean on the unpatched code. Before running it, I checked with `diff -rq` that the copy matched the worktree after reverting all mutations.
- Mutants: the 22 GVMUT mutants (L0 for all, L1 for none, 4, 7, 8 and 14) and the 12 `GVR2` mutants of `superblock_valid` against `superblock_is_valid_against_core`.

I did not run tier 1, fuzz or CodeQL. Nothing was edited, committed or pushed in the worktree.
