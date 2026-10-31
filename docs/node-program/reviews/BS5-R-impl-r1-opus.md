# BS5-R fork impl, review r1 (Opus)

- Branch `node/bs5r`, head 430f5c77, worktree `bsdk-wt/bs5r` (read-only).
- Base: node/base-sdk 1eda0100. F1 56c4cd90 merges 89828032.
- Reviewed: F2–F8 as `git diff 56c4cd90 430f5c77`, and F1 for the params tree and the merge resolution.
- Core references: `dash` v24.0.0-rc.4 and v23.1.8.
- Plan: BS5-R-PLAN rev2. Rulings: DEC-900/903/907/911/943.

## Verdict: ACCEPT-WITH-NITS

Counts: 0 M / 2 m / 2 n.

`ChainSetup` applies Core's chain-params options in Core's order with Core's messages at both tags. I found no value, default or range that differs from Core on any network or release, outside the registered deviations:
- quirk 46;
- BL-31 negative heights;
- the asserts that become errors (V20/MN_RR order, devnet nonce).

The quirk 46 refusals are exactly the planned set, and the test makes them so. Devnet genesis is byte-exact. The F1 merge is mechanical.

The two minor findings are:
- m-1, a negation-parsing gap carried over unchanged from BS5 (1eda0100). It is not a BS5-R regression.
- m-2, a hardening gap in the corpus test.

Neither blocks the fork merge. m-1 can be a backlog row.

## Findings

### m-1: `-nopowtargetspacing` is accepted, but Core refuses it at argument parsing (pre-existing)

- **Commit:** F4 7347bfdd (carried over from 1eda0100 unchanged).
- **Where:** `pkgs/params/src/builder.rs:182-185` (`negate`), `:233-242` (the argument-parsing phase handles only `-minsporkkeys`), `:343-349` (devnet spacing).
- **Core:** `-powtargetspacing` is registered `ALLOW_ANY | DISALLOW_NEGATION` (rc.4 chainparams.cpp:1440; v23.1.8 :1392).
  - `InterpretOption` (rc.4 util/system.cpp:161-165; v23.1.8 :241-245) refuses any negated form during `ParseParameters`, before chain selection, on every network. The text is `Negating of -powtargetspacing is meaningless and therefore forbidden`.
  - It checks this before the double-negative rule, so `-nopowtargetspacing=0` is refused too.
  - No other chain-params or spork option has this flag (checked: the 17 `SetupChainParamsOptions` args, `-fastprune`, `-sporkaddr` and `-minsporkkeys`).
- **Failing inputs** (both releases):
  - `ChainParamsBuilder::new(Network::Devnet, r).negate(ChainOption::PowTargetSpacing)`: ours gives `Invalid value of powTargetSpacing (0)`; Core gives the negation error. The message differs.
  - The same on `Network::Main`, `Testnet3` or `Regtest`: ours **starts** (the option is ignored off devnet); Core refuses to start. Core refuses, we accept.
  - `-devnet=x -nopowtargetspacing=0`: the caller contract (builder.rs:147) maps a double negative to `arg("1")`, so ours starts with spacing 1; Core refuses.
- **Fix:** in `build()`'s argument-parsing phase (next to the v24 `-minsporkkeys` check), refuse any `negate(PowTargetSpacing)` with Core's text, on every network.
  - Document in the builder's doc that a caller must not turn a double-negated `-powtargetspacing` into a value. The builder can't see one.
  - Add `error/neg_powtargetspacing` (devnet and main) to `run.py`, so the corpus checks it against Core.

### m-2: the corpus tests don't pin the case set, so a dropped case passes

- **Commit:** F6 7f59383f.
- **Where:** `pkgs/params/tests/core_corpus.rs:430` (`cases.len() > 100`) and `pkgs/primitives/tests/chainparams_blocks.rs:108` (`checked > 80`).
- **What already works:**
  - A wrong mark fails: an unmarked quirk 46 refusal, a `deviation` on a non-`LlmqOutOfRange` refusal, or a `deviation` without a refusal (:446-454).
  - A changed error text fails (:455-458).
  - Any wrong table value fails: the full base+delta dump is compared, and `got.len() == want.len()`, at :477-500. Every `ChainParams`, `ConsensusParams`, `Bip9Deployment` and `AddrParams` field reaches the dump (checked mechanically). `bech32_hrp` goes through `platform_hrp`, and the deployments go through `deployments()`.
- **The gap:** nothing fails when a case disappears.
  - Failing input: delete `refused/llmq_threshold_70000`, or all ten m-4/quirk 46 cases, from `chainparams_v24_0.json5`. Both tests still pass (178 > 100).
  - The Core-checked evidence for quirk 46 and the r2 m-4 order can therefore vanish in a later regeneration, for example with the rc.5 re-pin (DEC-945). The unit tests in `tests/builder.rs` would still pass, but they aren't Core-checked.
- **Fix:** assert the exact count per pin (188 today), or a required-name list that includes the ten BS5-R cases. Use the same count in `chainparams_blocks.rs`.

### n-1: two corpus case names contradict their kind

- **Commit:** F6.
- **Where:** `chainparams_v24_0.json5`, `refused/llmq_size_70000` (`-llmqtestparams=70000:2`).
- At v24 this case is Core's `error` (size > 400), not a refusal. At v23 the same name is a `refused`/46 case.
- Likewise `error/llmqtestplat_size_401` is `refused`/46 at v23.
- The test keys off the fields, so this is harmless, but the names mislead readers of the quirk 46 evidence.
- **Fix:** rename in `run.py` (for example `limit/llmq_size_70000`) when the rdc-gen patch lands.

### n-2: some quirk 46 orderings are unit-tested but have no Core-checked corpus case

- **Commit:** F6.
- **What's covered:** the corpus holds the plan's three cross-option `error/` cases and the main/test ignored-network starts.
- **What's only unit-tested** (`tests/builder.rs:436-499`, not checked against Core's dump):
  - the spork phase before `LlmqOutOfRange`: `-regtest -llmqtestparams=-1:2 -sporkaddr=invalid` gives `Invalid spork address specified with -sporkaddr`;
  - v24 `-minsporkkeys=1` with an out-of-range override;
  - `-llmqdevnetparams=-2:9` on main, test and regtest, and `-llmqtestparams=-1:2` on devnet (ignored-network starts).
- Our behaviour matches Core in each case (I traced init.cpp rc.4 :1746-1752 and v23.1.8 :1699-1714).
- **Fix:** worth adding to `run.py` with the m-1 case, so these are Core-checked too.

## What I checked

### 1. `ChainSetup` parity (builder.rs vs rc.4/v23.1.8 chainparams.cpp, init.cpp, spork.cpp)

**Devnet order.** Core's `CDevNetParams` runs subsidy/diff, then the genesis with `FindDevNetGenesisBlock(GetDevNetName())`, then chainlocks → IS-DIP0024 → platform → mnhf → `-llmqdevnetparams` → `-powtargetspacing` (rc.4 :635-684).
- Ours names the devnet first, then runs the same sequence (builder.rs:309-351).
- Naming first is neutral: the name can only fail where Core asserts.

**Regtest order.** Core runs `-fastprune`, then `-testactivationheight` → `-vbparams` → `-dip3params` → `-budgetparams` → the three size overrides → `-llmqtestinstantsenddip0024`, then the V20/MN_RR asserts (rc.4 :879-967).
- Ours matches (:359-430). The BL-31 negative-height refusals and the out-of-range record come after every Core throw.

**Messages.** Every `ParamsError` Display before `NegativeHeight` matches Core's `runtime_error` and `InitError` text, at both tags. This includes the vbparams 9-field text, `must NOT use rotation`, and `must not exceed ... (400)`.

**Within one option.** Core's order is split → size → threshold → `> MAX_LLMQ_SIZE` (v24 only; `caps_llmq_size`), and ours is the same. Ours then applies `u16::try_from`, which only records the value (:554-569).

**Across options.** `out_of_range` is raised after `sync_chain` and after the spork signers (:250-255), so every Core error and assert comes first.

**Role names.** These are looked up over the configured `llmq_params()` by `Display` (:577-600), as Core loops over `consensus.llmqs` by `name`. The rotation rules match: chainlocks forbids rotation, devnet IS-DIP0024 requires it, and the others don't care.

**Sporks.**
- v24: `GetArg("-sporkaddr", default)` with the `GetArgs().size() > 1` warning.
- v23: `IsArgSet` → `GetArgs` (empty after a negation), a set of key IDs, then `SetMinSporkKeys` with `min <= max/2 || min > max`.
- Both match (:258-292). Duplicate addresses collapse in both (Core uses `std::set`, ours `BTreeSet`).

**Spork decoding.** Ours keeps the NUL and Core-space pre-checks and the case-insensitive HRP prefix. `SetSporkAddress` (rc.4 spork.cpp:219-230) decodes with `DecodeDestination`, and ours then needs a 21-byte payload with a matching version. The F5 dash-script differential test passes on all 4 networks × 2 releases.

**Option set.** Core's 17 `SetupChainParamsOptions` args are identical at v23.1.8 and rc.4. `ChainOption` holds those 17 plus `-devnet`, `-fastprune`, `-sporkaddr` and `-minsporkkeys`. Core has no `-superblockparams` at either tag; budget/superblock is `-budgetparams` only.

**Release deltas.** I diffed chainparams.cpp between v23.1.8 and rc.4. The only value deltas are:
- v24 bit 12→13;
- testnet v24 start NEVER_ACTIVE → 1790208000;
- `evo_shares` (new at v24);
- `bech32_platform_hrp`;
- `fRequireStandard`;
- `vSporkAddresses`/`nMinSporkKeys` → `strSporkAddress`;
- the assumeutxo `EvoSnapshotHash`;
- the `MAX_LLMQ_SIZE` cap.

`release.rs` and `setup/tables.rs` cover exactly these. `llmq/params.h` differs only by `MAX_LLMQ_SIZE` and default member initialisers. The `vDeployments` blocks of all four networks match `Release::v24` at both tags (the scratch v24dep files and my own grep).

**rc.3 → rc.4 (Q10).** These files are identical between v24.0.0-rc.3 (0364ed626ec) and rc.4: chainparams.cpp, chainparamsbase.cpp, llmq/params.h, init.cpp, spork.cpp, util/system.cpp, consensus/params.h and deploymentinfo.cpp. So the header note holds.

**Quirk 46 refusal set.** It is exactly the plan's domain:
- v24: size < 0, threshold < 0, or threshold > 65 535 (a size of 401..65 535 is Core's error);
- v23: either value outside 0..=65 535;
- nothing on ignored networks.

`quirk_46_llmq_overrides_outside_u16` covers the boundaries 65 535/65 536, −1 and i32::MIN/MAX at both releases. `core_corpus.rs:439-454` makes any corpus refusal other than `LlmqOutOfRange` fail unless it is unmarked, and requires every `LlmqOutOfRange` refusal to be marked 46.

### 2. Devnet genesis (F3)

**Encoding.** `DevnetGenesisParams` follows Core's `CreateDevNetGenesisBlock`/`FindDevNetGenesisBlock` (rc.4 :43-107):
- `OP_1` plus the minimal push, an `OP_RETURN` output and a 50 DASH reward;
- version 4, prev = the genesis hash, `nTime + 1` and the genesis `nBits`;
- the first nonce from 0 whose X11 hash ≤ the target.

The primitives `From<&DevnetGenesisParams>` follows #83's genesis `From` style (`TxOut::new`, literals elsewhere, as #83 does).

**Byte-exact against Core.** `chainparams_blocks.rs` compares the serialized genesis and devnet genesis blocks with Core's bytes for every accepted case at both pins. That is the hash, merkle root, time, bits and nonce of:
- the unnamed devnet;
- `-devnet=` (empty name);
- `-nodevnet` (`devnet-0`, as Core does);
- `bs5`;
- a long name;
- 80-byte (PUSHDATA1) and 300-byte (PUSHDATA2) names.

The static `DEVNET_GENESIS_HASH` 65aeaff7… and `chain_tx_data.timestamp` 1417713338 equal both corpora's devnet base.

**m-4 tie test.** It covers names of 0, 1, 75, 76, 255 and 256 bytes and a non-ASCII name (`genesis_valid.rs`).

**Leaked checkpoint slice (DEC-911).**
- `name_devnet` (setup.rs:158-173) leaks only the named devnet's two-entry list, once per build. It also sets `chain_tx_data.timestamp`, as Core's `chainTxData` does.
- The doc line is on `ChainParamsBuilder` (builder.rs:139-141) and on `name_devnet`.
- `named_devnet_checkpoints` checks `chain.checkpoints == setup.checkpoints`.

### 3. Corpora and their test (F6)

**Cases.** 188 cases per pin (178 + 10). The ten per pin are:
- the three cross-option `error/` cases;
- the two ignored-network starts;
- the five quirk 46 refusals at v24 (four `refused`/46 plus `llmq_size_70000` as Core's error), and six refusals marked 46 at v23 (including `llmqtestplat_size_401`).

The plan's m-1 splits are present (`limit_llmq_odd` 0:5 accepted, `limit_devnet_negative` accepted).

**Header-only regeneration.** `f9e48ce4..430f5c77` and `65278644..7f59383f` change only the three corpus headers (DEC-943's expectation). The dump sha256 lines name the rebuilt binaries (accepted).

**The 31 `tool_only` cases per pin** (fastprune, spork) carry over from BS5 unchanged.

**What the test does and doesn't catch:** see m-2 and n-2. `chain.consensus == *setup.consensus` and the checkpoints are asserted after every accepted case (r2 m-6).

### 4. API and the p2p_core reuse (F2, F7)

**The ruling check.** `git diff 89828032 430f5c77` over `addr/chain/consensus/deployment/genesis/llmq/lib.rs` and `params/` has a single `-` line, the `Network` derive. `mainnet/test3/regtest.rs` gain only the row 10/29 fields, with Core's defaults (0/0/1 and a zero hash; consensus/params.h at both tags).

**`LlmqType::AVAILABLE`** is in `available_llmqs` order (params.h:138). `max_cycles`, `max_store_depth` and `is_single_member` match params.h:122-129.

**The pin test** (`tests/setup.rs`) covers all four networks, `LlmqNone` and `Unknown(0xfe)`, the V23_1 known-bit set {12, 28}, `as u8` 0..3 and `network_id`.

**p2p_core against 1eda0100:**
- `param` is now u16 → u64. It is unchanged for representable values and never multiplied in u16.
- `MAX_BATCHED_SIGS = MAX_LLMQ_SIZE as usize` (400).
- `overlay_ii`'s 1 MiB cap test now uses `u16::MAX`/0 and still exceeds the cap (65 535 × 133, 65 535 × 36).
- `core_overlay_ii` skips the 15 DKG seeds with quorum (−1, −5, −2), which quirk 46 makes unreachable. It asserts the count, so the skip isn't silent.
- `coinjoin`/`overlay_iii` build through the builder and read only `pool_max_participants`.
- `portable` still forwards `dash-params/portable` (F3), and `dash-pow` keeps `default-features = false` as primitives does.

**Call-site regressions.** No base-sdk code matches `dash_params::Network` exhaustively or uses `as u8`. `bspcheck` takes upstream's static form at F1 and returns to the builder at F7 (planned; same magic and genesis). `deployment_mut` is `pub(crate)`.

**rdc e7d144feb** only converts its own `Network` into `CoreNetwork::{Main,Testnet3,Regtest}`, so adding `Devnet` doesn't break it (plan R1).

### 5. Fork hygiene

**F1.**
- `git diff 89828032 56c4cd90 -- pkgs/params` is empty.
- The files that differ from both parents are Cargo/Bazel locks, the p2p_core and primitives imports (`LlmqType`/`AddrParams` from params), `param` → u16, the local 400 in sigshare, and `support.rs` dropping `LlmqType`.
- Nothing beyond 89828032 is carried in.

**Commits.**
- All eight are signed (`%G?` = G).
- No message line is over 73 columns.
- The subjects follow upstream's `pkg%type!:` form.

**CodeQL.** The required derives (Clone/Debug/Eq/Hash/PartialEq, with the float exemption on `ChainSetup`) and `Unencodable` are present on every new type. The full combs at 7347bfdd, 65278644 and f9e48ce4 (code-identical to F4, F6 and F8) ran `lint_codeql` with attrib; only the known libz-sys `lint_cargo` red failed.

**Semgrep** passes at 7f59383f (fast comb).

**Not seen:** I couldn't find the implementer's list of departures 2–12 in scratch or node-program. I judged the departures from the diff against the plan instead. The non-plan items I saw are the bspcheck round-trip, the `core_overlay_ii` skip, `SporkSigners` being `#[non_exhaustive]`, and `Network::ALL`/`from_network_id`/`DeploymentPos::{index,gbt_force}`. All are behaviour-neutral.

**Not run:** no build, no tests and no CodeQL (read-only review). The Core sources were extracted to /work/scratch/bs5r-impl-r1-opus (left in place).
