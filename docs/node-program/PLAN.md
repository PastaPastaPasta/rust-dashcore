# PLAN: execution plan for the Rust Dash node program

| | |
|---|---|
| Status | **v2 (A1-rev1)**, written with the revision of A1 after design review A1r round 1. Every finding's disposition is in `designs/A1-rev1-dispositions.md`. The manager owns this file and updates it as tasks land; live state is in `STATUS.md` |
| Date | 2026-10-08 (week 0) |
| Binding inputs | `CHARTER.md` ("Done when" 1–6); `DECISIONS.md` (DEC-01…17); `PRD.md` (168 stories `US-*`, `FR-*`, `NFR-*`, release criteria `REL-*`); `designs/A1-architecture.md` rev1 (authoritative architecture; section refs "A1 §x") |
| Already done or running (not re-planned) | **BS1** merged (9494ed8e). **BS2** nearly done (reference oracles under `/work/chains` + `dash-conformance`; `node/bs2-conformance-harness`). **B1** running (`node-bench` + the dashd 23.1.8 baseline; `node/bench-harness`). **Wave 1, running:** BS3, O1, O4, ST1, V24a (V24a is the `dashcore` v24 task and replaces v1's BS17) |

> **EMBARGO-1.** An unfixed Dash Core security issue, referred to only by this codename. EMBARGO-1: see ~/private/embargo/EMBARGO-1.md (access per §7). Nothing in this plan creates an upstream PR or a public artifact that could reveal it. DEC-09 keeps every node branch local.

**Revision 2026-10-09 (pasta direction; DEC-81…DEC-91). This section overrides anything below that conflicts.**

1. **One stack for SPV and the node (DEC-81).** New gating design **SB0** (Fable ∥ Opus, reconciled; Sol review, then Opus high after Sol per DEC-92): the shared base for dash-spv (mobile/FFI) and rdashd (SPV mode + full validation), built with or from dash-spv. SB0 decides the layering, R2's replacement, the storage engine for the shared base (DEC-85 re-check of DEC-43 under mobile constraints), CV5's placement, coordination with the dash-spv maintainers' refactor (#902), and the rework of P2a, CH1, EV0/EV2, LQ1 and A1 §1–§2. **Held until SB0 is approved:** P2a/P2b implementation, CH1, CH2b, EV0's merge, EV2a/b, LQ1a/b, ND1, O13a. **Continuing** (below or beside the shared boundary, SB0 cannot change them): CS-series consensus (pure crates), CH2a a1–a2 (in-memory coins layer; a3's engine config waits for SB0's storage ruling), ST3a (engine-agnostic harness), O2a/O3/PF0 tooling, base-sdk work, desktop.
2. **v24 only (DEC-82).** Task **RP-V24** collapses `Release`/`ReleaseProfile`, `-releaseprofile`, O17's `profile` rows, BS6c's v23.1 layout, per-profile rows in P2a/LQ4a/ND1/CH, the v23.1 oracle pin in gates, fuzz registry and farm, and the dual-pin acceptance language (M2 exit, REL-2.x) to v24. Historical-chain validation stays (v24 validates history). DEC-34(a)'s v23 legacy-wallet default is gone (descriptor wallets).
3. **Honest identity (DEC-83).** `-honestua` and identity spoofing are dropped; O13a shrinks to protocol-parity transcripts (minus identity); O13b/O13c are dropped; NFR-FP rows are rewritten.
4. **Platforms (DEC-84).** Linux, macOS, Windows, BSD (in that order); portable code now, a cross-target `cargo check` gate step, real Mac/Windows QA when pasta provides access.
5. **base-sdk upstream (DEC-86, DEC-91).** Task **BSX**: leniency catalogue (a consensus-required → node-side layer where possible / b likely Core bug → pasta / c unnecessary → remove) over BS4a, BS5, BS6a, BS7, BS11a-t, plus an API-shape review against kwvg's patterns; then **BSX-PR**: a series of small upstream PRs to dashpay/base-sdk (leniency-free units first), opened and babysat by the manager, with the fork folded back after every review change. BS7 merges into the fork after BSX classifies it.
6. **Process (DEC-88).** Fable only for SB0-class calls; never for desktop. Dual review where risk needs it. Explicit effort settings on every delegation.
7. **M7 (DEC-89).** Plan around a real masternode arriving later; regtest/devnet MN work first. tDASH on request.

**SB0 integration (2026-10-09; DEC-119, SB0 rev6.1 092080a5).** SB0 is approved. Its rows are §4.12: SB1–SB19, SB12a, SB18b, and SB-ST as a contingency. The rows it changes carry "(SB0 rev6.1)":
- P2a is split into P2a-core and P2a-net on `dash-p2p` (SB6);
- CH1 becomes CH1-node, its shared half moving to SB4/SB5;
- CH2b takes `i:SB5`;
- CV5 is superseded by SB9 + SB16 (the product is SB18);
- EV4 is split: EV4-core is SB7, and EV4-serve stays;
- EV0, EV2a/b and LQ1a/b are re-scoped per the EV addendum as it stands: rev5 726ff9a1, with Sol r6 APPROVE and Opus high r7 APPROVE-WITH-CHANGES (aeedda94); approval pending.

The hold is lifted from P2a, P2b, CH1, CH2b, ND1 and O13a. **EV0, EV2a/b and LQ1a/b stay held until the EV addendum is approved.** Opus r7's five must-fix edits are not applied here. The rows follow the addendum when it is approved. Three of the edits touch rows:
- O2 moves `impl MnListView`/`ScratchMnList for MnList` into EV0's first commit;
- O1 adds a snapshot-cache interleaving test to CH7b;
- O5 either moves LQ1a's model-only conformance work into its own S row or makes REL11 31.5 the plan number.

The start list is §8.3 (Wave 2), and the model recheck is §3 ("SB0 recheck"; `tools/cpm_plan.py`).

**What changed from v1.**
- **Corrected dependency graph (F17, M-10).**
  - Merge dependencies are now separate from interface-only dependencies.
  - Test-harness cycles are broken by splits: O9 → O9a/b, CH7b's CL-conflict scenarios moved to LK1a, EV0.
  - Missing edges are added: clsig and spork intake and sporks for M4, RP2a for O15.
  - Milestone exits now match the tasks inside them.
- **Re-baselined dates (M-9).** The critical path was recomputed from the graph, including design and review latency (§3). Honest dates: Done-1 replay ≈ wk 30, M5 ≈ wk 44, M7 ≈ wk 52, M8 ≈ wk 58.
- **Decoupled masternode milestone (M-9a).** The MN overlay gates on regtest-correct evo/LLMQ, not on the full mainnet replay campaign.
- **Split tasks (F18).** BS4, BS6, BS9, BS11, CS5, CH6, P2, P6, QM1, RP1–RP3, IM1, O2, O9 and O12 are split into PR-sized pieces.
- **New tasks:**
  - O17 PIN-DELTA (M-2);
  - BS11a-t, the BLS types (B-1);
  - O2b `diff_chain` (M-7, F15);
  - ST3a/b power-loss testing (F03, M-4);
  - EV0 (types first);
  - P2b, the inbound minimal handshake;
  - P5a and SP0, overlay intake and sporks for a following node (M-10.1);
  - PF0, the deterministic-counter gate (M-11).
- **Changed risk classes (F18):** the class follows the riskiest changed behaviour, and SEC is added.
- **New §7 (M-1, EMBARGO-1).**
- **Wave 1b (§8).**

**Contents.**
- §0 Conventions
- §1 Milestones and exit criteria
- §2 Workstreams and staffing
- §3 Critical path
- §4 Task graph
- §5 Standing workstreams
- §6 Upstream sync
- §7 Embargo handling (EMBARGO-1)
- §8 Wave 1 (running), Wave 1b and Wave 2 (post-SB0)
- §9 Risks
- §10 Questions for pasta
- Appendix: story traceability

---

## 0. Conventions

**Dispatch tiers (DEC-92; the Model column of §4).** `impl S-h` = Sonnet 5.5 high (acceptance fully enumerated by the brief (DEC-130)); `impl O-m` = Opus 5.5 medium (semi-hard); `impl O-h` = Opus 5.5 high (hard). `D O-h` = design by Opus high (single designer); `DX` = very hard design, Opus xhigh and/or Fable high (rare; never desktop). `rev Sol` = GPT-6.1-Sol review; `rev Sol→O-h` = critical (consensus, security, storage durability): Sol first, Opus high only after Sol is satisfied or the implementer stops accepting Sol's feedback, never in parallel. Exploration/search sub-tasks: Haiku. A struggling Sonnet implementer is redone one tier up (O-m), not given more fix rounds. Every Sonnet brief requires a `code-review-validator` pass before reporting (DEC-130).


**Task IDs** are prefixed by stream:

| Prefix | Stream |
|---|---|
| BS | Base SDK |
| CV | dashcore convergence |
| CS | pure consensus |
| ST, CH | storage, chainstate |
| EV, LQ | evo, LLMQ consensus and protocol math |
| P | P2P |
| AR, ND, RP, MP, MN, IX, ZQ | args/CLI, node, RPC, mempool, mining, indexes, ZMQ/REST |
| O | verification and oracles |
| WL, IM | wallet, import/export |
| SP, GV, CJ, AM, QM, DK, SG, LK, GS, PL | masternode overlay |
| PF | performance (standing) |
| RL | release and operations |
| SB | shared SPV/node base (SB0; DEC-81, DEC-119) |

**Branches and worktrees.** Every task becomes:
- a brief, `briefs/<ID>-<slug>.md`;
- a branch, `node/<id>-<slug>`, cut from `node/integration`;
- a worktree, `~/workspace/rdc-wt/<id>-<slug>`.

Tasks in other repositories (PROCESS.md):
- **base-sdk:** branches `node/bs<N>-<slug>` in `~/workspace/base-sdk`, with worktrees under `~/workspace/bsdk-wt/`.
- **Dash Core:** branches in `~/workspace/dash`, with worktrees under `~/workspace/dash-wt/`.

**Size** (one implementer, tests included): **S** ≤ 1 agent-week · **M** 1–2 · **L** 2–4.
- A task that cannot be reviewed as one independent API or behaviour, with its tests, is split. Rev1 split every v1 row that bundled independent behaviours (F18).
- No task may be XL.

**Risk class.** This sets the review level per PROCESS.md and A1 §4.13. **The class follows the riskiest behaviour the change touches, not the crate prefix** (rev1, GPT F18). A change to hashing or decoding helpers, state mutation, clocks, init or shutdown, block submission or chain invalidation is CON/STATE wherever it lives.

| Class | Meaning | Review | Gate |
|---|---|---|---|
| **CON** | consensus-critical (★) | **Sequential (DEC-92):** GPT-6.1-Sol xhigh first; then Opus high (with the `code-review` skill) only after Sol is satisfied or the implementer stops accepting Sol's feedback | G-consensus |
| **STATE** | chainstate, storage, crash consistency, clock/init | same as CON (storage durability is critical) | G-state |
| **LLMQ** | quorums, DKG, signing, BLS, CL/IS | same as CON | G-LLMQ |
| **P2P** | wire, transport, relay (fingerprint dropped, DEC-83) | Sol; the sequential Opus pass only for untrusted-input/DoS-relevant tasks tagged `rev Sol→O-h` | G-P2P |
| **OTH** | everything else | GPT-6.1-Sol high | G-other |
| **+SEC** (modifier) | key handling, wallet encryption and signing, untrusted-file parsers, the oracle trust boundary | sequential: Sol xhigh first, then Opus high with the `security-review` skill (DEC-92) | G-SEC |

Two rules apply on top of the table:
- Any **optimisation** of consensus or state code gets two reviewers, whatever its parent task's class.
- Code under **PLAN §7 handling** is reviewed by Claude reviewers only (§7.4).

**Design step:**
- **—** none: the brief plus the A1 section suffices.
- **D** a separate design doc `designs/<ID>.md` by Opus xhigh, with an independent design review before implementation.
- **D2** independent Fable 5.1 and Opus 5.5 designs, reconciled into one (the user's rule for the hardest architecture calls). Used for CH2, EV2 and P2a.
- **Design steps start as soon as A1-rev1 is approved**, in parallel with their predecessors' implementation (§8). Design inputs are A1 sections, not predecessor code.

**Model:**
- **O-h** Opus 5.5 high (the default).
- **O-xh** Opus 5.5 xhigh (the hardest consensus and crypto work).
- **S-h** Sonnet 5.5 high (acceptance fully enumerated by the brief (DEC-130); a failing Sonnet task is redone on Opus). Every Sonnet brief requires a `code-review-validator` pass before reporting (DEC-130).

**Dependencies** (rev1 notation, GPT F17):
- A plain ID is a **merge** dependency: the task starts after that task merges.
- **`i:ID`** is an **interface** dependency: the task may start once the dependency's API skeleton is agreed (its brief and design, or a merged types-only PR), but it **merges** only after the dependency merges.
- **Acceptance evidence** that only a later task can produce (a replay campaign, a soak) gates the **milestone exit**, not the task's merge. The milestone lists it.

**Acceptance column:**
- **Tests** use PRD/A1 tags:
  - **L0** vectors;
  - **L1** BS2 equality / oracle differential. **DEC-184: every "≥ N CPU-h" fuzz budget in this plan reads "ran until plateau with 0 divergences".** At merge, the 5-min smoke plus new targets run to plateau; at release, every target runs to plateau (historically ≥ 24 / 72 CPU-h);
  - **L2** replay windows; **L3** mutations; **FT** functional tests; **MIX**; **L6** fingerprint; **DIFF-RPC**; **XCLI**; **SPV**;
  - **CRASH**, the ST3 model, device and VM levels (rev1).
- **BG** is the bench guard: the metrics that must not regress against the previous `node/integration` head (A1 §3.11).
  - Per PR, the guard is **PF0's deterministic counters**: instructions, allocations, syscalls and I/O on the named workloads. `crit:` means the crate's criterion/iai bench; "dash-spv" means dash-spv's benches plus `spv-headers-*`.
  - Wall-clock `scenario·metric` entries are milestone evidence, measured in reserved windows (§5 SW-OPS).
- Every task also passes **G0**: fmt, clippy `-D warnings`, `cargo test --workspace`, the dash-spv dashd suites, `cargo deny` (including the pure-crate bans; v1's `no_std` job is dropped), the BS2 regtest sets, and the private EMBARGO-1 tests (§7.5).

**Dates** are calendar weeks from 2026-10-08. They come from the §3 model: S/M/L midpoints, plus design and review latency, plus ~15% contingency from M4 on, assuming the staffing of §2 (about 11.5–14.5 concurrent implementers through wk 44) plus reviewers. With fewer implementers the off-critical streams slip first. They are planning estimates, not commitments.

---

## 1. Milestones and exit criteria

Each milestone closes when **every** exit criterion holds on `node/integration`. The evidence is recorded under `node-program/` and in STATUS.md. A milestone's **Content** lists the tasks expected to land in its window. A task's **MS** column (§4) is the latest milestone it may land in, and tasks often land earlier.

### M0: Foundations (wk 0–7, target 2026-11-26)

**Content:**
- BS1 (merged), BS2, B1, BS3;
- O1 (oracle shim v1), O3 (fuzz farm), O4 (vectors and regenerated fixtures), O5 (quirk scaffolding), O6 (chain scans);
- O7 (DROPPED, DEC-82: no v23.1.8 reference; the StateDigest v1 spec and the extractor stay with O8/O15);
- O17 (PIN-DELTA);
- ST1 (store and bake-off), ST2 (block files), ST3a (the model-level power-loss harness);
- PF0 (the counter gate);
- CS1 (complete), PF1 (started), CS2 started, AR1/AR2/AR3 started.

**Exit:**
1. BS1, BS2 and B1 are merged. The dashd 23.1.8 and 24.0.x baselines are recorded in `research/B1-dashd-baseline.md` (NFR-PERF-02).
2. BS3 is merged: one base-sdk source; `Cargo.lock` committed; the `cargo deny` R2 rule and the pure-crate bans are live.
3. `libdash_oracle` builds for the v24.0 pin (DEC-82: v24 only). The v1 entry points pass smoke differential tests on BS2 corpus samples. The O3 farm runs every target's per-PR smoke and the nightly schedule.
4. Core vectors are imported with provenance. The mnlistdiff/qrinfo fixtures are regenerated from the oracle (R4 F12). All quirk tests exist (`#[ignore]` with an owner): R4's 34, plus rev1's 35–38, with quirk 1 named `quirk_01_embargo_1`.
5. Chain scans (a)–(h) are recorded (NFR-CEQ-13). Scan (g) (EMBARGO-1) is handled per §7.
6. The StateDigest v1 spec, with its field classes, is written. Golden extraction from the **v24** oracles is running for both networks (A1 §4.6).
7. The ST1 bake-off decision is recorded in `designs/ST1-bakeoff.md`. ST3a's `FaultyEngine`/`FaultyFlatFiles` pass their self-tests.
8. **O17**: `designs/O17-pin-delta.md` classifies every delta (A1 §2.25), and the `ReleaseProfile` spec (`designs/O17-release-profile.md`) is approved.
9. **PF0**: the counter gate runs on every PR, with measured noise floors.
10. CS1: `script_tests.json`, `sighash.json` and the script parts of `tx_valid/tx_invalid.json` pass at 100% (L0). `diff_script_verify` and `diff_sighash` are clean for ≥ 24 CPU-h (L1). (`diff_tx_decode` is CS0's and BS4b's, at M1.)

### M1: Base SDK milestone (wk 0–13, target 2027-01-07)

The charter's first engineering milestone.

**Content:**
- BS4a/b, BS5, BS6a/b, BS7, BS8, BS11a-t, BS11a, BS11b, BS12, BS13, BS15, BS16;
- SB1, SB2 and SB3 (SB0 rev6.1; SB3 may run into M2);
- CS0, CS2, CS3, CS5a/b;
- O2a (shim v2 and the coverage matrix).

BS9a/b and BS10 continue to M5, BS14 moves to M6, and BS18 stays off the critical path.

**Exit ("Base SDK integrated", A1 §1.7 rev1):**
1. The base-sdk items BS3 (bsdk part; the rdc part merged at M0, exit 2), BS4a/b, BS5, BS6a/b, BS7, BS8, BS11a-t, BS11a, BS11b, BS12 and BS13 are merged into `node/base-sdk` and pinned. The rust-dashcore items BS15 and BS16 are merged into `node/integration`. (v1's exit wrongly required rust-dashcore tasks to merge into base-sdk: F17.)
2. BS2 is green over **full mainnet and testnet history** across dashcore, base-sdk and dashd (NFR-CEQ-03).
3. The BS4b, BS6b, BS7 and BS8 differential targets have ≥ 24 CPU-h clean; `diff_bls_decode`/`diff_bls_verify` have ≥ 72.
4. (SB0 rev6.1) SB1's gates pass on `node/integration`: `check-layers.py`, `check-shared.sh` and `check-mobile.sh`. SB0 §2.3's rules L1–L9 replace R2's `cargo deny` rule (SB0 §12.3), so node crates reach the legacy stack (`dashcore` and the wallet crates, SB0's layer W) only through L3's `allow_w`. `dashcore` compiles Platform's import list unchanged (NFR-CMP-01).
5. `dash-consensus` core rules (CS0–CS3, CS5a/b) are complete:
   - L0 is 100% for `tx_valid`/`tx_invalid`, `pow_tests`, `subsidy_tests` and `block_reward_reallocation_tests`;
   - L1 `diff_check_transaction`, `diff_check_block`, `diff_next_work` and `diff_subsidy_payments` are clean;
   - the historical sweep passes: every mainnet and testnet `nBits` and every `getblockstats` subsidy is equal, and the admissible regtest/devnet domain is equal (FR-CON-14/15/20; A1 §4.5).
6. The quirks owned by CS0–CS3 and CS5a/b are un-ignored and pass (FR-CON-34). Quirk 1 passes against the private corpus.
7. O2a's coverage matrix (`compat/COVERAGE.md`) has no unexplained "none" rows for M1–M4 functions.

### M2: Headers-first sync (wk 6–18.5, target 2027-02-14; wk 18 before SB0)

**Content:** P1, SB4, SB5, SB6, CH1-node, P2a-core, P2a-net, P2b, ND1, RP0, AR3 (complete), EV0, O8, O9a, O11, O13a, O16, BS18; SB3 if it did not land in M1 (SB0 rev6.1).

**Exit:**
1. `rdashd` syncs **mainnet and testnet headers** to the tip, from public peers and from the oracle over localhost. The header chain, `chainwork` and `getchaintips` equal the oracle's. A restart reloads the index without rework (FR-CHS-01/02).
2. Handshake transcripts are identical to the v24 reference's (DEC-82): v1 and v2 transports, **outbound (P2a-core and P2a-net over SB6; SB0 rev6.1) and inbound (P2b, minimal: handshake and headers serving)**, MN probe excluded. This covers PRD §4.2 behaviours 1–5, 9 and 10 (O13a; NFR-FP-01, partial). (v1 required inbound while inbound was planned for M5: F17.)
3. `rdash-cli` passes XCLI against dashd for `-getinfo`, `-netinfo`, `-addrinfo`, `help`, `getblockchaininfo` and `getpeerinfo`. Stock dash-cli works against rdashd for the same set (FR-CLI-04).
4. The first functional tests pass via `node/func-compat-v24.0` (A1 Appendix D, M2 row). The FT-MAP skeleton is generated.
5. BG: PF0 counters for `startup-regtest` and headers-only `startup-mainnet` are recorded against dashd. In the window: `startup_s` ≤ dashd and `idle_rss_mib` ≤ dashd.
6. **Shared base (SB0 rev6.1):** SB1's gates are green; SB4, SB5 and SB6 are merged; `dash-chain` and `dash-p2p` build for aarch64-android within SB0's L5/L6.

### M3: Full validation, non-evo (wk 10–23, target 2027-03-18)

**Content:** CS4, CS6, ST3b, CH2, CH3a, CH3b, CH4, CH5, P3, MN0, RP1a, O9b, O10 (non-evo operators), O2b (non-evo). Landing in the window without gating M3's exit: EV1, EV2a, EV2b. Shared base (SB0 rev6.1): SB7, SB8, SB9, SB10, SB12a, SB12.

**Exit:**
1. **Replay mainnet 0 → 1,028,159 and testnet 0 → DIP3 activation** with `-assumevalid=0`.
   - StateDigest is equal at every block for the UTXO MuHash, block stats, `undo_hash` and deployment answers (NFR-CEQ-06, partial).
   - At height 1,028,159, `hash_serialized_2`, the full `dumptxoutset` set-diff and `verifychain 4 0` are equal.
2. **CRASH** (FR-STO-03, US-NODE-04; A1 §3.5.8) passes on `regtest-40000` and a split-flush configuration:
   - the ST3a model level;
   - the ST3b device level: `dm-log-writes` prefix replay at every flush/FUA mark, double crash, `dm-flakey` fsync errors, disk full;
   - the VM hard-power-off harness.

   Every case restarts **without reindex** and with digests equal to the oracle's. The F01 case passes: an accepted-but-script-invalid block persisted before connection is rejected after restart. Kill tests pass as smoke. Regtest reorg suites pass.
3. L3: 100% parity on the non-evo mutation operators (coinbase value, locktime, limits, merkle, header, multi-failure precedence under `-par=1` and parallel).
4. `diff_chain` (O2b, non-evo operations) is clean for ≥ 24 CPU-h.
5. The tier-1 non-evo functional tests pass (A1 Appendix D, M3 row).
6. BG:
   - PF0 counters for `reindex-chainstate` over the pre-DIP3 windows are recorded against dashd;
   - in the window: `reindex-chainstate-mainnet` and `-noav` over the pre-DIP3 window are not slower than dashd's `-reindex` equivalent (REL-1.3, partial; A1 §4.6 m-2);
   - `cpu_ms_per_block` ≤ dashd.

### M4: Evo/LLMQ consensus, full parity (wk 14–35, target 2027-06-10; the replay exit ≈ wk 30, 2027-05-06)

This is **Charter Done-1**.

**Content:** EV3, EV4-serve, LQ1a/b, LQ2, LQ3 (EV0–EV2b land earlier), CH7a/b, CH6a, SP0, P5a, LK1a (CL node role), RP2a, O15 (shadow smoke), O2b (evo), and the O8 full-state digests. Shared base and SPV (SB0 rev6.1): SB11, SB13, SB14, SB15; SB16 and SB19 (M4–M5).

**Exit:**
1. **REL-1.1:** full mainnet and testnet replay from genesis.
   - Runs: `-assumevalid=0`, default assumevalid, and a P2P sync from the oracle.
   - StateDigest v1 is equal at every block, the `-statedigestlog` digests (CanonicalMNListHash, effects, …) are equal, and the tip checks hold.
   - `-consensusxcheck` is on.
2. The NFR-CEQ-08 node-local scenarios pass on regtest:
   - CL conflicts (from LK1a);
   - superblocks under the **unsynced / NoCheck** path;
   - the assumevalid boundary;
   - reorgs across V19 with **`clsig` traffic** decoding across the switch (the only overlay input an M4 node processes), across V24, and across DIP24 cycle bases;
   - the non-IS failure sequences of A1 §2.10.1: a GBT proposal at a cycle base; a block that fails after `ProcessCommitment`; a platform-ban reset in a block that fails later.

   Superblocks with governance synced, the IS cases and the IS-removal failure sequence move to **M5**, because their runtimes (GV1b, LK1b) are M5 tasks (F17, M-10.2). The V19 reorg with full masternode traffic (isdlock, recsig, sig shares, DKG) is an M7 gate (M7 exit 1).
3. `diff_chain` with evo operations (ProTx churn, QcTx, asset lock/unlock, MNHF, CL/IS arrival, failures, reorgs) is clean for ≥ 72 CPU-h.
4. **Live following.** rdashd syncs from public peers only and follows the tip, receiving `clsig` and sporks (P5a, SP0) and enforcing ChainLocks (LK1a). **Shadow mode runs for 7 days with zero divergence as a smoke.** The M4 node has no IS or governance runtime, so it is weaker than dashd at the tip; the REL-1.2 clock starts at M5 (M-10.2). The tripwire is live (US-NODE-22).
5. The Core datadir **safe** import (CH6a) reproduces the oracle tip state (US-IMP-02), with the source's files unchanged. Fast import is CH6b at M6, provisional.
6. REL-1.3:
   - PF0 counters for metrics 1–3 workloads are recorded against dashd;
   - in the window: `ibd-*-seed`, `ibd-*-seed-noav` and `reindex-*` are not slower than dashd.

### M5: Full-node services, non-masternode drop-in (wk 20–44, target 2027-08-12)

**Content:**
- P2P: P4, P5, P6a, P6b, P8 (part);
- overlays and governance: SP1, GV1a/b, CJ0, LK1b (IS node role);
- mempool, mining and indexes: MP1a/b, MP2, MN1, IX1, IX2, ZQ1, CH8;
- base-sdk overlay types: BS9a/b, BS10;
- RPC: RP1b, RP2b, RP3a, IM1a;
- verification: O13b, O14, O12a (mixed mode A);
- release: RL1 (packaging start);
- SPV product (SB0 rev6.1): SB17, SB18.

**Exit:**
1. The required tier-1 and tier-3 functional tests pass at 100% (non-wallet) on the v24.0 tree. The **mixed-mode A** passlist is green: every DTF test runs with rdashd as the non-MN nodes (NFR-CEQ-05, partial).
2. **L6, node half (REL-2.6, part):**
   - all 31 behaviours plus +A…+E identical for a non-MN node, per profile (NFR-FP-01);
   - timers KS p > 0.05 (NFR-FP-03);
   - classifier held-out AUC ≤ 0.55 from a non-MN viewpoint (NFR-FP-02, part);
   - NFR-FP-04 holds.
3. **SPV smoke:** dash-spv `dashd_sync` and `dashd_masternode` pass with rdashd as the only peer; the BIP37 ordering test passes (REL-5.7, part). The real serving check is EV4-serve's byte identity (A1 §4.11). (SB0 rev6.1) The same suites also pass *as* rdashd `-spv` (SB18, run through O16). SB18 uses key-wallet, so M5 does not wait for WL2a; `dashd-wallet` for `-spv` is SB18b at M6.
4. Mining and mutation parity:
   - GBT blocks from rdashd are accepted by dashd and vice versa on a 10^5-block alternating-miner regtest run with equal digests (US-MINE-01/02);
   - template contents equal dashd's after each same-tip dependency change (A1 §2.13);
   - L3 covers 100% of mutations, including CbTx, QcTx (with BLS encodings), ProTx, asset and MNHF operators and `testmempoolaccept`.
5. DIFF-RPC passes for every **non-wallet, non-masternode** MUST RPC in PRD §5 (REL-3.2, part). The MN and Platform signing RPCs are RP3b at M7 (F17). The Platform P-1 frozen fixtures pass (PRD §5.3).
6. **REL-1.2:** the clock starts when LK1b and GV1b are merged. M5 closes only after ≥ 7 days on mainnet and ≥ 30 days on testnet with zero tripwire alarms and the same tip as dashd at every ChainLock (NFR-CEQ-10). The NFR-CEQ-08 IS scenarios (including the ChainLocked block whose IS-lock removal survives a later payee failure, A1 §2.10.1) and the superblocks-synced scenarios pass.
7. BG: PF0 counters for metrics 5–13 are recorded against dashd; in the window, `mempool-accept`, `gbt-regtest`, `startup-*`, `rpc-*`, RSS and disk are not slower than dashd.

### M6: Wallet and migration (wk 24–46, target 2027-08-26)

**Content:** WL1–WL7, IM1b, IM2, BS14, CH6b (fast import), AR4 (partial), SB18b (SB0 rev6.1).

**Exit:**
1. The required **tier-4** set (58 wallet tests + Dash wallet tests) passes. DTF runs with **rdashd as node 0**.
2. Core wallet fixtures import with identical addresses and balances against dashd: BDB legacy non-HD, Dash HD (`hdchain`), encrypted, CoinJoin-used, watch-only, SQLite descriptor with and without a mnemonic, `dumpwallet`. This covers US-IMP-03/04/05/06/13. The importers have passed G-SEC review.
3. **Migration round trip:** dashd datadir → rdashd (US-NODE-18) → export → dashd (US-IMP-09), with equal balances and chain. Fast import (CH6b) is still marked provisional unless its A1 §2.10 evidence passes.
4. XCLI passes for OW-11, -12, -14, -20, -21. CoinJoin mixing against dashd MN coordinators works on regtest (US-CJ-01…06).
5. BG: `rescan-mainnet` `rescan_time_s` ≤ dashd (in the window); no regression on `rpc-*` counters.

### M7: Masternode on testnet (wk 26–52, target 2027-10-07)

This is **Charter Done-2 on testnet**.

The track is **decoupled from Done-1** (M-9a). Its implementation tasks gate on regtest-correct evo/LLMQ (CH7b merged with `diff_chain` and regtest L4), not on the REL-1.1 replay campaign.

**Content:** AM1, P7, QM1a/b, LQ4a/b, LQ5, DK1, SG1a/b, LK1c, GS1, CJ1, RP3b, PL1, O12b (mixed mode B + devnet), O13c, RL3 (security review), RL4.

**Exit:**
1. **REL-2.1:** every Dash-specific functional test passes in the three NFR-CEQ-09(a) mixes (alternating, all-Rust, mixed mode B), with the six Stage-A pass criteria (A1 §4.9), for the v24.0 tree. The NFR-CEQ-08 reorg across V19 **with full masternode traffic** (isdlock, recsig, sig shares, DKG decoding across the switch; A1 §1.4.1) passes.
2. **REL-2.2:** the devnet soak runs ≥ 2 weeks (topology, workload and chaos per NFR-CEQ-09b) with equal DKG success and zero PoSe on Rust MNs.
3. **REL-2.3:** testnet for ≥ 30 consecutive days with ≥ 4 regular Rust MNs and ≥ 1 Rust evonode serving Platform through dashmate (US-PLAT-09):
   - zero PoSe attributable to rdashd;
   - DKG valid-member rate not worse than dashd's;
   - every Rust MN completes ≥ 1 DKG per LLMQ type it is selected for;
   - its CL and IS shares are used in recovered signatures;
   - ≥ 50 CoinJoin sessions are served, with both dashd and Rust clients.
   This needs §10 Q2.
4. **REL-2.4:** hot swap dashd → rdashd → dashd on testnet with PoSe 0 throughout (US-MN-20).
5. **REL-2.6, MN half:** the classifier AUC ≤ 0.55 from a masternode peer's viewpoint. **REL-2.7:** B1 metrics 14–18 (`mn-*-regtest`) are not slower than dashd (in the window).
6. The internal security review (RL3; NFR-SEC-09, internal part) is complete.
7. Per **DEC-06**, at exit the manager raises the mainnet collateral and hosting request to pasta (§10 Q1), together with the mainnet-v24 timing (DEC-82: no v23.1.8 MN profile).

### M8: Release (wk 44–58, target 2027-11-18)

This covers **Charter Done-3 to Done-6**, and Done-2 on mainnet.

**Content:** RL1–RL7, AR4, P8 (complete), WL7, the remaining SHOULD RPCs, the dashj client test, tier-2 smoke (§10 Q3).

**Exit:**
1. **REL-2.5:** a mainnet pilot of ≥ 1 regular MN for ≥ 14 days, no PoSe, paid every cycle (if pasta approves Q1).
2. **REL-3:**
   - OW-01…OW-22 pass XCLI with rdash-cli and dash-cli against rdashd and dashd;
   - every MUST RPC has a DIFF-RPC test;
   - every SHOULD RPC is implemented or has pasta's deferral (DEC-10);
   - the dash-tx, dash-util and dash-wallet equivalents pass `bitcoin-util-test.json` and `tool_wallet`.
3. **REL-4:** `compat/STORIES.md` maps all 168 stories to green tests per surface. The 21 former "D: pending" stories follow DEC-01. Desktop parity per `docs/parity.md` comes from the desktop roadmap (D-track).
4. **REL-5:** all suites pass:
   - tiers 1–4 at 100% of the required set on the v24.0 tree, with an empty expected-failures file and < 1% flakes over 10 runs;
   - fuzz budgets met, `diff_chain` included;
   - L2, L3, NFR-CEQ-08, CRASH, mixed networks and scans pass;
   - DIFF-RPC with a reviewed registry;
   - SPV, Dash Evo Tool, desktop and dashj light clients;
   - dashmate end to end.
5. **REL-6:**
   - `bench/REPORT-<version>.md` covers all 18 metrics with confidence intervals, from wall-clock runs on the dedicated host (§10 Q4) or in reserved windows, with the PF0 counter history attached;
   - no metric is slower than dashd;
   - shortfalls against 2× are explained using A1 §3.9's declared limits;
   - per-PR results have no gaps.
6. NFR-OPS-04 docs, signed tier-1 artifacts (NFR-SEC-06), upgrade-in-place across two schema versions (US-NODE-20), and the tier-2 smoke (§10 Q3).

### REL → milestone map

| REL | Milestone | Main tasks |
|---|---|---|
| 1.1 replay parity | M4 | CH7a/b, EV*, LQ1–3, O7–O9b, O2b |
| 1.2 live follow 7/30 d | M5 (smoke from M4) | O15, P3, P5a, LK1a, LK1b, GV1b |
| 1.3 metrics 1–3 | M4 (reconfirmed M8) | CH4, PF0, PF3 |
| 2.1 MIX regtest | M7 | O12b + all MN tasks |
| 2.2 devnet soak | M7 | O12b, RL4 |
| 2.3 testnet 30 d | M7 | RL4 (needs Q2) |
| 2.4 hot swap | M7 | RL4, CH6a |
| 2.5 mainnet pilot | M8 | RL5 (needs Q1; mainnet on v24) |
| 2.6 indistinguishability | M5 (node), M7 (MN) | O13a–c, O17 |
| 2.7 metrics 14–18 | M7 | PF6 |
| 3.1–3.3 CLI and RPC coverage | M8 (progressive from M2) | AR3, AR4, RP*, O14, RL7 |
| 4.1–4.4 stories | M8 | RL7; desktop D-track |
| 5.1 gates | always | — |
| 5.2 L0/L1 | M4 | O1–O4, O2a/b |
| 5.3 FT tiers | M8 (tiers gate M3–M7) | O11 |
| 5.4 fuzz budgets | M8 | O3 |
| 5.5 L2, L3, scenarios, MIX, scans, CRASH | M7/M8 | O6–O10, O12, ST3 |
| 5.6 DIFF-RPC | M8 | O14 |
| 5.7 light clients | M5 (SPV smoke), M8 (dashj) | O16, P6b, SB18 |
| 5.8 dashmate | M7 | PL1 |
| 6.1–6.4 benchmarks | M8 (6.4 continuous) | PF*, RL6 |

---

## 2. Workstreams and staffing

| Stream | Scope | wk 0–8 | wk 8–16 | wk 16–24 | wk 24–32 | wk 32–44 | wk 44–58 |
|---|---|---|---|---|---|---|---|
| W1 Base SDK + convergence | BS3–BS18, CV1–CV4 (CV5 superseded by SB9/SB16) | 3 | 2 | 1 | 0.5 | 0.5 | 0.5 |
| W2 Consensus (pure) | CS0–CS6 | 2.5 | 1 | 0.5 | — | — | — |
| W3 Storage + chainstate | ST*, CH* | 1.5 | 2 | 2 | 1.5 | 0.5 | 0.5 |
| W4 Evo/LLMQ consensus + protocol math | EV*, LQ* | 0.5 | 2 | 2 | 1.5 | 0.5 | — |
| W5 P2P | P1–P8 | 0.5 | 1.5 | 1.5 | 2 | 1 | 0.5 |
| W6 Services | AR*, ND1, RP*, MP*, MN*, IX*, ZQ1 | 1 | 1 | 1 | 2 | 1.5 | 1 |
| W7 Verification | O1–O17 | 2 | 2 | 1.5 | 1.5 | 1 | 1 |
| W8 Wallet + import | WL*, IM* | — | — | 0.5 | 2 | 1.5 | 0.5 |
| W9 Masternode overlay | SP*, GV*, CJ*, AM1, QM1*, DK1, SG*, LK*, GS1, PL1 | — | — | 1 | 2 | 3 | 1 |
| W10 Performance (standing) | PF0–PF6 | 0.5 | 0.5 | 0.5 | 1 | 1 | 1 |
| W11 Release/ops | RL1–RL7 | — | — | — | 0.5 | 1 | 2 |
| **Implementers** | | **11.5** | **12** | **11.5** | **14.5** | **11.5** | **8** |
| **Reviewer load** (CON/STATE/LLMQ/P2P reviews per week, 2 each) | | ≈ 8 | ≈ 12 | ≈ 12 | ≈ 14 | ≈ 10 | ≈ 6 |

- **Headcount.** The peaks exceed v1's 10 implementers. The manager either staffs to them or accepts that the dates in §1 slip. The serial chain (§3), not headcount, sets the dates, so under-staffing first hits the off-path streams (W6, W8, W11).
- **Review is a delivery dependency** (F18). The reviewer load row is a planning input, and the manager tracks review queue age in STATUS.
- **Fuzz farm.** Runs at Nice 19 / idle IO / CPUWeight 10; each leg runs until plateau (DEC-184), so cores go to targets still gaining coverage.
- **Soaks** are calendar-bound (§3).
- The desktop D-track (dashwallet-desktop) is staffed separately; its node-program dependencies are in §5 SW-DESK.
- **SB rows (SB0 rev6.1, §4.12)** add about 35 agent-weeks (5 S, 10 M, 5 L; SB18b and SB-ST excluded), most of it between wk 1 and wk 27. They draw mainly on W3 (SB4, SB5, SB10, SB13, SB14), W4 (SB7, SB8, SB11, SB15, SB19) and W5 (SB6, SB9, SB16). CH1 (L → M) and P2a, whose network half moved to SB6, return about 2 weeks. The rows above were not re-derived for this: it is about 1.3 more implementers on average through wk 27, plus the reviews that come with them.

---

## 3. Critical path (rev1, computed)

**How.** The task graph of §4 was evaluated as a DAG:
- **durations:** S = 1, M = 1.5, L = 3 weeks, plus design (D +1, D2 +2) and review-and-fix latency (+1 for CON/STATE/LLMQ/P2P, +0.5 for OTH);
- **dependencies:** merge dependencies serialize; interface dependencies (`i:`) let work overlap and constrain only the merge;
- **design steps** start in week 1, after A1-rev1 approval;
- **calendar items** are added on top: the 7-day M4 smoke, the ≥ 2-week devnet soak, and the 30-day testnet soak.

The model script is kept with the planning files (`node-program/tools/cpm.py`); after SB0 the plan of record is `tools/cpm_plan.py` (the SB0 recheck below). Opus M-9 recomputed v1's path without latency and got 26.5 weeks to M4, against v1's target of 20. With latency and v1's dependencies the path was ≈ 44 weeks. The levers below bring it to ≈ 30.

**The critical chain:**

```
BS3 ─► BS4a ─► BS6a ─► EV2a ─► LQ1a ─► LQ1b ─► LQ2 ─► LQ3 ─► CH7b ─┬─► REL-1.1 replay campaign ══► M4 (≈ wk 30)
                                                                    ├─► QM1a ─► SG1a ─► SG1b ─┐
                                                                    └─► MP1a ─► LK1b ─► LK1c ─┴─► O12b ─► devnet soak (2 wk) ─► RL4 testnet 30 d ══► M7 (≈ wk 50–52) ─► RL5 ══► M8
```

The two masternode branches are **co-critical** (zero slack each). Slack was measured by delaying each task in the model and checking whether RL4 moves.

**O17/DEC-37 recheck (2026-10-08).** The release-profile additions were run through the model: IX2, QM1b and O10 from M to L, and the new tasks CH7c and BS6c (EV1 takes BS6c as an interface dependency; a merge dependency would cost the chain 0.5 weeks). No milestone moves: IX2 and QM1b keep slack and CH7c sits beside the chain. O13c (M to L) and WL8 are outside the model, since nothing in it depends on them. O13b absorbs eight more probes at zero slack for M5 exit (§9).

**SB0 recheck (2026-10-09, DEC-119).** `tools/cpm_plan.py` is the plan-of-record model after the SB0 integration. `cpm.py` keeps the A1-rev1 table unchanged, because `cpm_ev_addendum.py` pins its blob and SB0 §13.4 is stated against it. `cpm_plan.py` applies the following on top:
- SB0 rev6.1's rows and edges (§4.12, and the rows marked "(SB0 rev6.1)");
- DEC-103's CH2a/b/c split;
- the EV addendum's LQ1a + `i:SB7` and LQ2 + SB8 (rev5, pending);
- the P2a-core/P2a-net split;
- RP-V24's interface edges;
- the removal of the DROPPED rows BS6c, CH7c and O13b.

| Point (week) | Plan of record | Dropped rows kept (SB0 §13.4's conventions) | Merge edges instead of SB4's `i:CS2`/`i:CS3` and CH1-node's `i:SB5` | SB0 §13.4 runs A–B / C |
|---|---|---|---|---|
| M2 exit | **18.5** | 18.5 | 21.5 | 18.5 / 21.5 |
| REL11 | **30.0** | 30.0 | 31.0 | 30.0 / 31.0 |
| M5_exit | **39.5** | 41.5 | 40.5 | 41.5 / 42.5 |
| RL4 | 49.3 | 49.3 | 50.3 | 49.3 / 50.3 |

- **M2** is limited by O13a: P1 (9.0) → SB6 (13.0) → P2a-core (13.5) → P2b (16.0) → O13a (18.5). P2a-net merges at 14.0, with 4.5 weeks of slack.
- **REL11** keeps the EV chain: BS6a (6.5) → EV0 (7.0) → EV2a (11.0) → LQ1a (13.5) → LQ1b → LQ2 → LQ3 → CH7b (28.0).
- **M5_exit** falls from 41.5 to 39.5 only because the model now drops O13b (DEC-83). The committed model still held M5_exit on it. The new limit is MP1a → MN1 → RP3a. SB0 itself moves M5 by 0. The SPV product rows end by 26.5 (SB18), with 11.5–13.5 weeks of slack against M5.
- **New near-critical rows:**
  - P1, SB6, P2a-core, P2b and O13a: 0 against M2;
  - SB4, SB5 and CH1-node: 1.5 against M2;
  - SB8: 1.5 against REL11, through the addendum's LQ2 + SB8 edge (SB0 §13.4 had 16.0 before that edge);
  - CH2b: 2.5 against REL11.
- **Sensitivity, not adopted:** SB6 on `i:P1` instead of P1 would bring M2 to 17.0, which would then be limited by SB4 → SB5 → CH1-node → P2a-core.
- **Pending (EV addendum Opus r7 O5):** if LQ1a keeps the model-only conformance work and grows to L, REL11 is 31.5 (the addendum §5's run P3). The manager either splits that work into an S row, which ends around wk 15 against CH7b's start at wk 24, or adopts 31.5.
- The model has no progress state. Rows already merged (BS7, CS0, CS2, BS5) finish at their week-0 model dates, so the real slack of the chains they start is larger.

| Point | Computed | Plan target |
|---|---|---|
| CH1-node (headers; SB0 rev6.1) | wk 11.5 | M2 exit wk 18.5 (with SB4–SB6, P2a-core/net, P2b, O13a) |
| SB6 → P2a-core → P2b → O13a (SB0 rev6.1) | wk 13.0 → 13.5 → 16.0 → 18.5 | M2 exit wk 18.5 |
| CH3b (non-evo chainstate) | wk 19 | M3 exit wk 23 (with CRASH, O2b) |
| EV2b (DML) | wk 15 | — |
| LQ3 (LLMQ verification) | wk 24 | — |
| CH7b (evo/LLMQ in chainstate) | wk 28 | — |
| SB18 (rdashd `-spv`; SB0 rev6.1) | wk 26.5 | M5 (SPV smoke) |
| REL-1.1 replay campaign | wk 30 | M4 replay exit wk 30 |
| M4 smoke (live CL + 7-day shadow) | wk 34.5 | M4 exit wk 35 |
| M5 services | wk 39.5 (SB0 recheck; 41.5 while the model held the dropped O13b) | M5 exit wk 44 (REL-1.2 needs 30 days after LK1b/GV1b) |
| LK1c (CL/IS signers) | wk 37 | — (co-critical with SG1b) |
| O12b (mixed-quorum harness) | wk 39.5 | — |
| Devnet soak end | wk 43 | — |
| RL4 30-day testnet soak end | wk 49.3 | M7 exit wk 52 |

**Near-critical tasks** (≤ 1.5 weeks of slack, measured):
- BS11a-t, CS0 and EV0 (0.5 week);
- CS3, EV1, EV2b, EV3 and CH7a (1 week);
- P5 → O13b, M5's fingerprint lab (1.5 weeks).

The CH1 → CH3a → P3 → P5a → AM1 chain has more than 2 weeks of slack.

**Levers applied** (M-9 and F17; each is safe under the A1 rev1 design):
1. **Types first.** BS11a-t (the BLS types) lets BS6a proceed while BS11a (the relic legacy port) is in progress. BS6a lands types first, and EV0 gives EV1 and EV2a a shared interface.
2. **LQ1a starts on EV2a's model** with the in-memory `EvoStore` (M-9c), not after EV2b.
3. **Interface dependencies for CH1, P2a, P3, P5a, O2b, CH7a and AM1.** Each develops against its predecessor's API and merges after it.
4. **Splits.** O9 → O9a/b (O9a is not blocked on CH3a), P6 → P6a/b (AM1 needs only P5a), QM1 → QM1a/b (signing needs only the quorum manager), CS5 → CS5a/b (CH2 needs only the coin codecs).
5. **Headers2 and the small control messages moved to BS7**, so P2a does not wait for BS8.
6. **D/D2 design steps start in Wave 1b** (§8), before their predecessors finish.
7. **The MN track is decoupled from REL-1.1** (M-9a). It still depends on CH7b and on M5's services for the testnet soak: a testnet masternode needs inbound P2P, the mempool, IS and governance.

**Calendar-bound items:** the M4 shadow smoke (7 days), REL-1.2 (7/30 days, starting in M5), the devnet soak (≥ 2 weeks), the testnet soak (≥ 30 days) and the mainnet pilot (≥ 14 days).

**Schedule-risk items on the path,** which get extra review capacity and early design: BS4a, BS6a, EV2a/b (D2), LQ1a/b, LQ2, LQ3, CH7a/b, MP1a (D), LK1b, LK1c, SG1a/b, BS11a (the legacy port, which gates CH7b's merge).

**Off the critical path:** the wallet (M6), indexes beyond txindex, ZMQ/REST/stats, privacy networks, pruning, the convergence track, `rdash-cli tx/util`, and BS13/BS14/BS16/BS18.

---

## 4. Task graph

Columns: **ID · Task · MS** (milestone the task must land in) **· Crates/repo · Deps** (§0 notation) **· Size · Risk · Design · Model · PRD · Acceptance (tests · BG)**. BS1 (merged), BS2 and B1 are not re-planned. Rows marked *(running)* are Wave 1 tasks already under way; their rev1 deltas are given as briefing notes for the manager. The appendix maps every story ID to the tasks that deliver it.

### 4.1 W1: Base SDK (A1 §1.7 rev1) and convergence

| ID | Task | MS | Crates/repo | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| BS3 *(running)* | Fork plumbing and workspace wiring: `node/base-sdk`; `[patch]` → `file://` rev for 8 crates; fold BS2's pin; `bump-base-sdk.sh`, `check-base-sdk-pins.sh`; committed `Cargo.lock`; `cargo deny` R2 + pure-crate bans; CI groups; retire base-sdk's empty `node/integration` branch. *Rev1 delta:* no `no_std` job (X-1) | M0 | base-sdk; rdc root, `deny.toml`, `contrib/node/`, `.github/ci-groups.yml` | BS1 | S | OTH | — | O-h | NFR-CMP-02, NFR-SEC-06 | `cargo tree -d` shows one base-sdk source; the pin check fails on a planted second pin; the deny rule rejects a dummy node crate depending on `dashcore` and a pure crate using `std::fs`; BS2 regtest sets green · BG: dash-spv, `spv-headers-*` unchanged |
| BS4a | Decode-domain shapes (A1 §1.4 #1–#9 + the m-17 sweep): `TxOut.value` i64, `CoreBool`, `MAX_SIZE` payload cap, 14-type `LlmqType`, `P2pMsg::Unknown`, empty-payload leniency, optional `version` suffix fields, consumption reporting, `ProtoCtx { version, bls, … }`, `DDE-SWEEP.md`. No BLS decisions | M1 | base-sdk types/primitives/p2p-core (+ rdc adaptation) | BS3 | M | CON | — (A1 §1.4) | O-xh | FR-CON-04, FR-P2P-01/02 | a unit test per divergence using byte strings Core accepts; the sweep table complete; base-sdk `just test lint` · BG: crit base-sdk mainnet block decode not slower |
| BS4b | Decode-domain evidence: differential decode targets vs the oracle for `Transaction`, `Block`, `BlockHeader` (`oracle_{tx,block,header}_decode`, O1) and every payload type 1–12 × version under both BLS flags (`oracle_payload_decode`, O2a; types 10–12 on the v24 shim only) (decode verdicts and field values; re-encoding is CS0's) | M1 | node-fuzz, base-sdk | BS4a, O1, O2a, O3 | S | CON | — | **impl S-h; rev Sol** | NFR-CEQ-04 | each target ≥ 24 CPU-h clean; BS2 full history · BG: — |
| BS5 | `dash-params` completion (NetworkKind + Devnet, ChainParamsBuilder with all overrides, LLMQ tables and roles, every param in A1 §1.7) | M1 | base-sdk params | BS3, BS4a, O1 | L | CON | — | O-h | FR-CON-03, -23, FR-LLMQ-01 | equality with Core's `CChainParams` JSON dump (O1 `oracle_chainparams_json`) for 4 networks × 12 override combinations on both pins; devnet genesis equal to Core's for 3 names; override parse errors byte-equal; owns quirk 34 (hard-coded pins; CH3a and LQ2 test the rule sites) · BG: — |
| BS6a | `dash-primitives` v24 shapes, types first, with BLS fields re-typed as `BlsEager`/`BlsLazy` + scheme source (A1 §1.4.1 table); corpus round trips | M1 | base-sdk primitives | BS4a, BS11a-t | M | CON | — (A1 §1.4.1) | O-xh | FR-CON-04, -05, -25, -26, -29 | L0 trivially_valid/invalid; BS2 equality on testnet v24 blocks and oracle-synthesized v24 txs; per-field scheme-source tests · BG: crit decode |
| BS6b | Per-payload `sig_hash_bytes()`; `diff_payload_<type>` (12 types (1–12; 10–12 v24 only) × versions); AU v2 stable-txid vectors per profile | M1 | base-sdk primitives, node-fuzz | BS6a, O2a | M | CON | — | **impl O-m; rev Sol→O-h** | FR-CON-25, -26 | L1 `diff_payload_*` ≥ 24 CPU-h, seeded per BLS field (A1 §1.4.1); sighash vectors from the oracle · BG: — |
| BS6c | **DROPPED (DEC-82): no v23.1 layout.** v23.1 ProTx payload layout as a decoder parameter (O17 EVO-14, DEC-37): version-3 ProRegTx/ProUpRegTx decoded with one `scriptPayout` (v23.1) or the payout list (v24), and v23's lenient vs v24's strict DomainPort decoding; base-sdk takes a plain layout value, never `Release` **Also (DEC-71):** SIG-05 (`bad-cbtx-version` derivation) through the single decode-context API, and a structured `PayloadError` (a `DecodeError` plus an offset, not a `String`); key CS0's `Tx::payload` slot by the layout input. | M2–M3 | base-sdk primitives | BS6a | S | CON | — | O-xh | FR-CON-25 | L0 v3 ProRegTx/ProUpRegTx vectors decoded under both layouts; `bad-protx-payload` vs `bad-protx-version` against both references (O10) · BG: — |
| BS7 | `dash-p2p-core` framing, inventory, short IDs and send gate, protocol constants, `block`/`tx` un-stubbed, BIP37/152/157 messages, **headers2 stream codec and the small control messages** (`sendheaders2`, `getheaders2`, `senddsq`, `qsendrecsigs`, `qwatch`, `ssc`, `getmnlistd`; moved from BS8, rev1); base-sdk V2 framing reused by `dash-p2p-transport` | M1 | base-sdk p2p-core | BS4a | M | P2P | — | **impl S-h; rev Sol** | FR-P2P-01…04, -10 | L1 `diff_v1_framing`, `diff_headers2`, `diff_p2p_<cmd>` for the BS7 set at handler level (A1 §2.14 policy table); `-capturemessages` header corpus round-trip; short-ID gate table equals Core's · BG: crit headers2 decode |
| BS8 | Overlay wire types I (CL, IS, recsig, FinalCommitment v1–4, QuorumSnapshot, QRInfo, SML entry v1–3, MnListDiff, MNAUTH, spork, platformban, qgetdata/qdata), every BLS field typed per A1 §1.4.1 | M1 | base-sdk primitives, p2p-core | BS6a, BS7, BS11a-t | L | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-P2P-01, FR-EVO-07/08, FR-LLMQ-04/11, FR-CL-01, FR-IS-01 | L1 differential per type (incl. SML entry per protocol band; `diff_clsig`, `diff_p2p_qfcommit`, `diff_mnauth` seeded per BLS field); regenerated fixtures (O4) round-trip; `messages.py` vectors · BG: — |
| BS9a | Overlay wire types II: DKG msgs (DYNBITSET, IES blobs), sig-share family (AUTOBITSET) | M2–M5 | base-sdk | BS8 | M | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-LLMQ-03/09 | L1 per command incl. padding-bit rejection and the `MaxDKGMessageSize` walk · BG: — |
| BS9b | Overlay wire types III: CoinJoin (`dsq` hash vs sighash; `CoreBool` canonicalised to 0x01 for hashing, m-18), governance (65/96-byte vote rule) | M2–M5 | base-sdk | BS8 | M | P2P | — | **impl S-h; rev Sol** | FR-CJS-01, FR-GOV-01/07 | L1 per command · BG: — |
| BS10 | All 74 commands typed with `decode_with_policy`; `Unknown` passthrough | M5 | base-sdk p2p-core | BS9a, BS9b | M | P2P | — | **impl S-h; rev Sol** | FR-P2P-01 | `-capturemessages` corpus (mainnet + testnet, every peer-version bucket) round-trips byte-identically; `diff_p2p_<cmd>` ×74 ≥ 24 CPU-h · BG: — |
| BS11a-t | BLS decode **types**: `BlsEager`/`BlsLazy`/`SchemeSource`/`BlsValue`; the wrapper algorithm for basic; lazy `Get()`/re-serialization rules; explicit scheme parameter | M1 | base-sdk pkc | BS3 | S | LLMQ | — (A1 §1.4.1) | O-xh | FR-CON-30 | unit tests for every basic-scheme row of the outcome table; quirks 35 and 37 scaffolds · BG: — |
| BS11a | BLS decode domain, **legacy** (design: designs/BS11a-legacy-bls.md): decode closure on the BS11a-t predicate + blst (no relic decode port), lazy `Exotic` classification; a transliteration of the relic arithmetic Core runs on `Exotic` operands (legacy-flag verifies, aggregation, IES DH), as the `Exotic` branch of BS12's serial verify; the three Core-shaped verifies with `HashPubKeys`; sk `modOrder`; the IES key from the basic encoding | M1 (needed by CH7b) | base-sdk pkc | BS11a-t; BS12's serial verify for slice 4 (slice 2 builds on the pin's ABI-14 `oracle_bls_exec`); acceptance O2a | L | LLMQ | D | **D O-h; impl O-h; rev Sol→O-h** | FR-CON-30 | L1 `diff_bls_decode` (every class × G1/G2 × pk/sig/sk × scheme), `diff_bls_arith` and `diff_bls_verify` (Exotic seeds, both flags) and the model target `bls_engine_model`, each certified per DEC-184/186; quirks 35, 45 · BG: crit decode not slower than today's dash-pkc; legacy-flag verify ≤ dashd (BS11a D64) |
| BS11b | `dash-pkc` API completion on BS11a's `BlsElem` (secure-aggregation weights and the three verifies are BS11a's): aggregates, threshold split/derive/recover, multi-recipient IES, `to_scheme`, hash-of-serialization, basic-only batch verify | M1 | base-sdk pkc | BS11a, i:BS12 | M | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-CON-30 | L1 `diff_bls_*` (multi-op targets through BS12's `oracle_bls` entry points, N13) ≥ 24 CPU-h; dashbls `test.cpp` vectors (BS12); quirk 22 · BG: crit BLS verify/aggregate vs B1 `microbench` BLS rows |
| BS11c | dash-pkc Core-style `core::sign(sk: &BlsSkValue, hash, flag)` (CBLSSecretKey::Sign) for both flags (LQ4 P-1, DEC-616 #9) | M5 | base-sdk pkc | BS11b | S | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-CON-30 | `bls_oracle/sign.json5` rows under both flags; `just comb` per commit · BG: — |
| BS12 | BLS vector corpus with provenance (dashbls via the oracle, incl. the multi-op `oracle_bls` shim entry points (aggregate, secure aggregate, threshold split/recover, IES; A1 §4.4) as a DEC-16 follow-up on the O2a shim branches (N13); legacy hash-to-G2; every outcome-table class; regtest DKG transcript; threshold; IES) **Also (DEC-79):** a serial, pool-free BLS verify in dash-pkc on blst's low-level Pairing API, allowed in pure crates by construction and selftested to spawn no threads; gates EV1/LQ1a verifying inside pure crates. | M1 | base-sdk pkc tests; Core `node/oracle-shim` | O2a | M | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-CON-30, NFR-CEQ-02 | every corpus file has `PROVENANCE.md`; all corpora pass · BG: — |
| BS13 | base-sdk perf helpers (`encoded_len`, streaming SHA256d, `decode_with_ranges`) | M1–M2 | base-sdk types, primitives | BS4a | S | **CON** (hashing helpers, F18) | — | **impl S-h; rev Sol** | NFR-PERF-03 | BS2 green (no behaviour change); hash equality fuzz vs the non-streaming path · BG: crit decode + txid of a mainnet block; instructions not higher |
| BS14 | `dashcore::bridge` (feature `base-sdk-bridge`) **Note (DEC-71):** blst is a hard dependency of the bridge (via dash-pkc); its thread pool is compiled on every hosted target (DEC-79). | **M6** | dash (dashcore) | BS6b, BS15, V24a | M | OTH | — | **impl S-h; rev Sol** | NFR-CMP-01 | round trip over the BS2 full-history corpus; display-direction tests; Platform import-list compile smoke; FFI tests unchanged · BG: dash-spv |
| BS15 | X11 `BlockHash` unconditional in dashcore via **`dash-x11`** (XH1, DEC-61; replaces the planned dash-pow AES-NI dispatch, which XH1 measured as useless); make `dash-x11` publishable; backend switch · (SB0 rev6.1) an aarch64 measurement (QEMU instruction counts) before `dash-x11` becomes the mobile default (SB0 §11.3); `dash-pow` leaves SB2's opt-level list at the switch | M1 | dash, hashes; dash-x11 | BS1, XH1 | S | CON | — | **impl S-h; rev Sol** | FR-CON-13 | all mainnet header hashes equal across backends and the oracle; dash-spv dashd suites · BG: `spv-headers-*` counters not higher; B1 `microbench` X11 decides the default |
| XH1 | **`dash-x11`**: Core-class X11 (portable ports of Core's 11 functions + x86_64 AES-NI/SSE/AVX2 backends, runtime detection, multi-vector self-test, observable fallback per DEC-61) (DEC-51b) | M1 | dash-x11 | — | M | CON | — | O-xh | FR-CON-13, NFR-PERF | differential vs Core's own X11 objects at both pins; every mainnet/testnet header; Core vectors; ≤ Core instructions per header (measured 0.23×, portable 0.88×) · BG: PF0 `x11-header-80b*` per backend |
| BS16 | `dash_network::Network` ↔ `NetworkKind` | M1 | dash-network | BS5 | S | OTH | — | **impl S-h; rev Sol** | NFR-CMP-01 | `FFINetwork` discriminants and serde names unchanged; FFI tests · BG: — |
| V24a *(running; v1's BS17)* | Full v24 wire format in `dashcore` (#1136/#1138 landed, #1137 ported, types 10–12, ProRegTx v3, AssetLock/Unlock v2 with the stable txid, payload length prefix) with Core-produced vectors. *Rev1 delta:* the stable txid in `dashcore` must be per network (v24 rule on testnet; absent in v23.1.8 on mainnet) or documented as testnet-only; X-4 suggests the manager keep its scope minimal | M1 (running) | dash (dashcore) | — | M | CON | — | O-h | FR-CON-04/05/25 (SPV side); R5 R-1 | BS2 equality vs dashd on testnet v24 blocks; dash-spv syncs testnet past V24 txs; bincode layouts pinned · BG: dash-spv, `spv-headers-*` |
| BS18 | dashcore SPV protocol correctness (70242 + gates, inventory 20, service flags, 3 MiB/32 MiB limits, no BIP144, DIP-0020 opcode names) | M2–M5 | dash (not `dash-spv/src/network/`) | — | M | P2P | — | **impl S-h; rev Sol** | FR-P2P-01/02 (SPV) | dash-spv dashd suites green; inventory, service and opcode tables equal Core's · BG: dash-spv |
| CV1 | #400 segwit/taproot removal | post-M4 | dash | — | M | OTH | — | **impl S-h; rev Sol** | NFR-CMP-01 | Platform impact note; dash-spv suites · BG: dash-spv |
| CV2 | ECDSA through `dash-pkc` (lax-DER helper) | post-M4 | dash, crypto | BS3 | M | OTH | — | **impl S-h; rev Sol** | R3 1.6 | key-wallet + FFI tests; lax-DER parity vs oracle · BG: dash-spv |
| CV3 | Network/LLMQ types/payload wrappers onto base-sdk **Note (DEC-71):** blst is a hard dependency through dash-pkc; its thread pool is compiled on every hosted target (DEC-79). · (SB0 rev6.1) Its network and LLMQ wrappers start from `dash-spv::bridge` (SB12a, from M3); moving dashcore's types onto base-sdk shrinks the bridge toward the identity (DEC-112 O2) | post-M4 | dash, dash-network, dash-spv | BS5, BS6b, SB12a | L | OTH | D | **D O-h; impl O-m; rev Sol** | R3 3.2–3.4 | Platform compile smoke; BS2 · BG: dash-spv |
| CV4 | Hash newtypes; Tx/Block; Script/Amount/Address; dash-spv P2P on `P2pMsg` (3 PRs) · (SB0 rev6.1) SB16 does not subsume the `P2pMsg` part (its facade still names `NetworkMessage`, SB0 §3.2.1); CV4 makes `dash-spv::bridge` (SB12a) the identity and deletes it; the hash-newtype work stays (DEC-112 O2) | post-M7 | dash, dash-spv | CV3 | L | OTH | D | **D O-h; impl O-m; rev Sol** | R3 3.5–3.9 | as CV3; the bridge becomes the identity · BG: dash-spv |
| CV5 | **Superseded (SB0 rev6.1, DEC-119): by SB9 (`dash-filters`, the BIP158 half) and SB16 (`SpvRouter` on `dash-p2p`, the network half); the product is SB18 (rdashd `-spv`). Not scheduled.** Was: optional dash-spv client mode on `dash-p2p-transport` + Dash BIP158 builder | post-M7 | dash-spv | CV4, P1, IX1 | L | P2P | D | **superseded** | — | dash-spv suites · BG: `spv-headers-*` not slower |
| SB0 | **APPROVED (DEC-119) at rev6.1 092080a5; its rows are §4.12.** **Shared SPV/node base design** (DEC-81): layering for dash-spv (mobile/FFI) + rdashd (SPV mode + full validation) on one network loop, one header/sync flow, one MN-list/quorum/ChainLock pipeline; R2 replacement; storage engine under mobile constraints (DEC-85); CV5 placement; dash-spv #902 coordination; rework list for P2a, CH1, EV0/EV2, LQ1, A1 | M1 (gating) | designs/SB0-shared-base.md | A1, P2a, CH1/CH2, ST1–ST3a, EV0/EV2, LQ1 designs; dash-spv, dash-spv-ffi | L | ARCH | **D2** (Fable ∥ Opus → reconcile) | **DX (Fable h / Opus xh); rev Sol** | DEC-81 | GPT xhigh + Opus xhigh design review; a plan revision for every held row · BG: — |
| RP-V24 | **Collapse to v24** (DEC-82): remove `Release::V23_1`, `-releaseprofile`, profile rows and per-profile behaviour; v23.1 pin out of gates, fuzz registry and farm; tests at the v24 pin; docs (O17, A1 §2.25, PLAN acceptance) | M1 | dash-consensus, dashd-store, node-l1, node-fuzz, contrib/node | O2a merged, BS re-pin merged | M | CON | — | **impl S-h; rev Sol→O-h** | DEC-82 | gate green; check-release-refs updated; no consensus behaviour change at v24 (vectors + oracle diff) · BG: PF0 not worse |
| BSX | **base-sdk leniency catalogue + API-shape review** (DEC-86): every Core-leniency in node/base-sdk (BS4a, BS5, BS6a, BS7, BS11a-t) classified a/b/c with Core evidence; node-side-layer proposals for (a); Core-bug list for pasta (embargo rules); API shapes vs kwvg patterns; the upstream PR series plan | M1 | node/base-sdk, dash-consensus | — | M | OTH | — | **impl O-m; rev Sol** | DEC-86 | GPT xhigh review; pasta gets the (b) list and ambiguities · BG: — |
| BSX-PR | **Upstream PR series to dashpay/base-sdk** (DEC-91): small units in kwvg conventions, leniency-free first; embargo scrub of full history; manager opens and babysits; fold review changes back into node/base-sdk + rdc re-pin | M1→ | dashpay/base-sdk | BSX (per unit) | L | OTH | — | **impl S-h; rev Sol** | DEC-91 | each PR merged upstream; fork converged (BASE-SDK-FORK.md table) · BG: — |

### 4.2 W2: Consensus, pure (A1 §2.4)

| ID | Task | MS | Crates | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| CS0 | `dash-consensus` skeleton: `Tx`/`Block` wrappers (stable txid per `ReleaseProfile`), range decoders, `core_serialize` as the sole consensus encoder (**PLAN §7 handling**), `ValidationError`, `ConsensusCtx` (with `BlsCtx`, `ReleaseProfile`). The CS0 owner also delivers the private EMBARGO-1 artifacts (quirk 1 vectors, scan (g), the private L3 operator definition; §7.2) | M0–M1 | dash-consensus | BS3, i:BS4a | M | CON | D | O-xh | FR-CON-01, -04, -05, -31, -33 | L1 `diff_tx_decode`/`diff_block_decode` (re-encode via `core_serialize`, txid, instance, merkle + mutated) ≥ 24 CPU-h; BS2 full history through the wrappers; byte ranges from the decode cursor (no BS13 dependency; BS13 swaps in later); quirk 1 passes against the private corpus (§7) · BG: crit `block_decode_hash` vs B1 `microbench` deserialize rows |
| CS1 | Script interpreter, sighash (every tx field encoded through `primitives::canonical`), sigops, DIP0020, CHECKDATASIG, ECDSA consensus layer | M0–M1 | dash-consensus::script | — | L | CON | D | O-xh | FR-CON-10, -11, -12 | L0 `script_tests` (1,282), `sighash.json`, dip0020/checkdatasig = 100%; L1 `diff_script_verify`, `diff_sighash` ≥ 24 CPU-h (72 at release); quirks 4–7 · BG: crit verify inputs/s vs B1 `microbench` script rows |
| CS2 | PoW (BTC/KGW/DGW, min-diff, 68589 tolerance), `fp` (A1 §4.5 rev1: release-binary inspection first, then the admissible-domain gate), subsidy, MN payment schedule, platform share, treasury | M0–M1 | dash-consensus::{pow, fp, subsidy, payments} | O1 | L | CON | D (fp) | O-xh | FR-CON-14, -15, -20, -21 | L0 `pow_tests`, `subsidy_tests`, BRR; L1 `diff_next_work`, `diff_subsidy_payments`; historical sweep (every mainnet/testnet `nBits` and `getblockstats` subsidy); the admissible regtest/devnet domain enumerated; quirks 8, 9, 11, 12, 15 · BG: crit `next_work` |
| CS3 | Tx/block/header checks (context-free and contextual; `time-too-new` source per profile), reject strings and classes, sigop limits | M1 | dash-consensus::{tx, block, header} | CS0, BS4a, O2a | M | CON | — | **impl O-h; rev Sol→O-h** | FR-CON-06, -07, -09, -10, -16, -17, -31 | L0 `tx_valid`/`tx_invalid`; L1 `diff_check_transaction`, `diff_check_block`, `diff_contextual_check_tx`, `diff_contextual_check_block_header` (CS2 §6.2's name), `diff_contextual_check_block` (std, per pin) through O2a's `oracle_contextual_check_*`; reject-string census vs Core source; quirks 2, 3, 10 · BG: crit `check_block` on a mainnet sample |
| CS4 | Deployments, Dash versionbits (dynamic threshold, EHF signal height), `ComputeBlockVersion`; `oracle_versionbits_state` on the O2a shim branches, built from the O2a context's seeding code (an O1b-style follow-up, DEC-25(3), N15) | M1–M3 | dash-consensus::deployments | BS5, CS0, O2a | M | CON | — | **impl O-h; rev Sol→O-h** | FR-CON-23; US-NODE-14, US-MINE-04 | L0 versionbits threshold; L1 `diff_versionbits`; deployment answers every 576 and at window boundaries (O7); quirks 31, 32 · BG: — |
| CS5a | Coin/undo codecs, `CoinsView` | M1 | dash-consensus::coins | CS0 | S | CON | — | O-h | FR-CON-08 | L1 `diff_codec_primitives` · BG: crit coin decode |
| CS5b | `check_tx_inputs`, sequence locks and finality, sigops with coins, MuHash3072 + `hash_serialized_2` | M1 | dash-consensus::{coins, tx} | CS3, O2a | M | CON | — | **impl O-m; rev Sol→O-h** | FR-CON-08, -09, FR-IDX-04 | L1 `diff_check_tx_inputs`, `diff_sequence_locks` (both std, per pin) through `oracle_check_tx_inputs`, `oracle_sequence_locks` (O2a), and `diff_sigops` through `oracle_transaction_sigops` (O1b); MuHash and `hash_serialized_2` equal the oracle at sampled heights (quirk 33); BIP68/112/113 vectors; the gate's private-seed replay passes · BG: — |
| CS6 | Sigcache, script-exec cache (parity: Core has both), check queue reporting **Core's result per mode** (generic `block-validation-failed` in parallel; the first failing input under `-par=1`; F20) | M3 | dashd-chainstate::{sigcache, checkqueue} | CS1, PF1 | M | CON | — | **impl O-m; rev Sol→O-h** | FR-CHS-10 | L3 multi-invalid blocks report Core's result in both modes; model test vs serial; sigcache hits vs twin · BG: counters for connect workloads; crit connect throughput ≥ 0.7 × cores × serial |

### 4.3 W3: Storage and chainstate (A1 §2.8–2.11, §3)

| ID | Task | MS | Crates | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| ST1 *(running)* | `dashd-store` + bake-off. *Rev1 deltas:* set `atomic_flush = true` on chainstate; measure ST1 §6a's D1–D4 durability costs; LevelDB arm optional (X-3); the "cannot regress" claim is gone; the chosen engine must also pass the tracked metrics (A1 §2.8) | M0 | dashd-store; dash-coreimport (blk/rev trace) | BS2 | M | STATE | D (bake-off protocol) | O-h | FR-STO-01/02/05 | `kv_model` fuzz; bake-off at dbcache 300 MiB and 4 GiB; disk-full on a loop image (D4); decision in `designs/ST1-bakeoff.md` · BG: the numbers become the store baseline |
| ST2 | Block files: Core-identical blk/rev, preallocation, `-tinyblk`/`-fastprune`, undo checksum, external-file scanner, `fdatasync` + directory `fsync`, **the link-count rule** (A1 §2.11) | M0–M1 | dashd-store::flatfile, dash-coreimport::blockfiles | BS2 | **M** (v1: S; F18) | STATE | — | O-h | FR-STO-04, FR-IMP-01 | byte-identical blk/rev vs dashd for `regtest-40000`; scanner parity on corrupted inputs (fuzz); a hardlinked file is never modified (test) · BG: crit append/read |
| ST3a | **Power-loss model harness** (rev1, F03/M-4): `FaultyEngine` (wraps the in-memory engine; models unsynced loss as a WAL prefix, injects ENOSPC/EIO per call) and `FaultyFlatFiles` (unsynced pages survive or vanish adversarially; sizes revert); reopen + invariant checks; integration into `coins_cache_model` | M0–M1 | dashd-store (test feature), node-fuzz | ST1, i:ST2 | M | STATE | D | **D O-h; impl O-m; rev Sol→O-h** | FR-STO-03 | the harness finds planted ordering bugs (a commit before fsync; an index before data) in a toy flush; self-tests · BG: — |
| ST3b | **Device and VM power-loss harness:** `dm-log-writes` record + prefix replay at every flush/FUA mark (and random points), double crash, `dm-flakey` fsync errors, disk full on a loop device; a KVM guest with `cache=none` hard-powered-off during IBD/reindex/reorg/flush; kill tests kept as smoke | M3 | `contrib/node/crash/`, dash-testkit | ST3a, ND1, CH2, i:CH3b | M | STATE | — | **impl O-h; rev Sol→O-h** | FR-STO-03, NFR-OPS-01 | M3 exit 2 passes; a deliberately broken ordering is caught; runs nightly (device) and weekly (VM) · BG: — |
| CH1-node | (SB0 rev6.1) **The node half of CH1** (SB0 §12.1; the shared half is SB4/SB5): the `ChainCommand` actor on V, including `apply_chainlock` from `dashd-locks`; the best-header hooks at Core's `InvalidateBlock`, `MarkConflictingBlock` and `InvalidChainFound` sites (DEC-112 O1); block-data status and positions on `dash-chain`'s shared entries; `ChainView` (+ `mn_list`, `best_chainlock`, `ibd`, `mtp`, `bls`, `epoch`); the block-index records in CH2's B1 batch. Header acceptance, the arena, the best header, the snapshot, the anchor and the feeds are `dash-chain`'s | M2 | dashd-chainstate | ST1, SB4, i:SB5 | M | STATE | **D** (with SB4's design; SB0 §12.1) | **D O-h; impl O-h; rev Sol→O-h** | FR-CHS-01, -02, -06; FR-CON-17, -18 | the CH1 row's tests, through V: full mainnet/testnet headers from oracle files with chainwork and `getchaintips` equal; parallel load; `-checkblockindex`; seqlock coherence stress; the best-header walk-backs at the three hook sites against the oracle (SB0 §4.1) · BG: counters for startup; in the window `startup_s`, `idle_rss_mib` ≤ dashd |
| CH1-node-LR | **Production load-time path on V (O2b design D32, §14 delta, added 10-18):** after `open_chainstate` (CH2b), the VerifyDB point and, at `check_level >= 3` with a non-empty coins view, Core's load-time `ResetBlockFailureFlags(nullptr)` (v24 `node/chainstate.cpp:415-417` emptiness test, `:436-441` VerifyDB, `:463-465` reset), which reconsiders `m_best_invalid` when it descends from the tip; RestartKind Clean/Kill reach it through the real startup path | M3 | dashd-chainstate, dashd-node | CH1-node, CH2b, CH5 (VerifyDB records), ND1 | S | STATE | in the O2b design (D32) | impl O-h; rev Sol→O-h | FR-CHS-01 | Core-regtest restart cases with a failed tip descendant and with an empty coins view give Core's `getchaintips` and best-invalid after restart; O2b-3b's `Restart` ops run over it (hard dependency of O2b-3b) |
| CH2a | **Coins cache and store** (design CH2 §7.1 a1–a3; DEC-103): PackedCoin, layer + layout gate, CoinsCache (active → frozen → prefetch → DB), BlockOverlay + fallible CoinsView, native exact ledger (DEC-99), CacheSizes per Core, CHAINSTATE_SCHEMA v1, EngineTuning/RocksTuning, GlobalFailStop, wipe_chainstate, delete_range | M3 | dashd-chainstate::coins, dashd-store | ST1, CS5a, CS0 | M | STATE | **D2** (designs/CH2-coins-flush.md) | **impl O-h; rev Sol→O-h** | FR-CHS-03, FR-STO-02/05 | design §7.1 a1–a3 acceptance · BG: PF0 `coins_micro` |
| CH2b | **Flush, recovery and subscribers** (design CH2 §7.2 b1–b3): FlushMachine, recovery, SubscriberHub/Driver under the SubscriberFeed contract F1–F11, prune slots, -dbcrashratio sync freezes (D14) · (SB0 rev6.1) the producer is `dash-chain::TipCell` (SB5), which `SubscriberHub` wraps; `Control` stays CH2's type, sent through `send_control`; `register` takes `Needs` (CH2 P8); the latch types come from `dash-kv` once SB10 moves them; b1/b2 code against SB5's stubbed API | M3 | dashd-chainstate::{flush,recovery,subscriber}, dashd-store | CH2a, ST2, ST3a, i:SB5 | L | STATE | **D2** | **impl O-h; rev Sol→O-h** | FR-CHS-04, FR-STO-03/04 | design §7.2 · BG: PF0 coins/flush workloads |
| CH2c | **Coins performance evidence and L1** (design CH2 §7.3): wall-clock tuning, W-MC, DEC-43 proxy (block-boundary native ≤ B, rss_anon_peak ≤ dashd), `diff_coins_ops` | M3 | dashd-chainstate, dashd-store-bakeoff, node-l1 | CH2b, PF0, O2a-CO | M | OTH (perf) + CON (L1) | — | **impl O-m; rev Sol** | NFR-PERF-01/03 | design §7.3 · BG: DEC-43 proxy |
| O2a-CO | Oracle follow-up for CH2c: `oracle_coins_ops` (per-op results incl. DynamicMemoryUsage and HaveCoinInCache, DEC-94 Q3) | M3 | Core oracle-shim branches; dash-oracle-sys | O2a | S | CON (adapter logic) | — | **impl O-m; rev Sol** | NFR-CEQ-04 | design §7.4 · BG: — |
| CH3a | `connect_block`/`disconnect_block` (non-evo), undo, assumevalid scope (m-1 set), BIP30/34, script flags, subsidy/value bound, check-queue integration | M3 | dashd-chainstate | CH1-node (SB0 rev6.1), CH2, CS1, CS3, CS4, CS5b, CS6, O9a | L | CON | D (CH3a + b) | **D O-h; impl O-h; rev Sol→O-h** | FR-CON-19, -24, FR-CHS-03 | L2 replay mainnet 0→1,028,159 and testnet pre-DIP3 (UTXO MuHash, `undo_hash`, block stats per block, `-assumevalid=0`); L3 non-evo operators 100% · BG: `reindex-chainstate-*` counters vs dashd (pre-DIP3 window) |
| CH3b | `activate_best_chain`, candidates, reorg + disconnect pool, invalidate/reconsider/precious, invalid-chain handling, CL enforcement entry points · O17: ChainLock-conflict flag persistence per profile (CONS-11); disconnected transactions go to the mempool's disconnect pool through `MempoolSink`, whose accounting is per profile (CONS-25) | M3 | dashd-chainstate | CH3a | L | STATE | (CH3 D) | **impl O-h; rev Sol→O-h** | FR-CHS-02, -05, -07 | regtest reorg suites; FT `rpc_invalidateblock`, `rpc_preciousblock`, `rpc_getchaintips`, `p2p_invalid_block`; the F01 restart case; reorg-across-flush crash test; CONS-25: Core-generated `DisconnectedBlockTransactions` vectors byte-identical at rc.3 and rc.4 (DEC-479, DEC-489) · BG: — |
| CH4 | IBD/reindex pipeline: S1 inside the awaited accept, S2 epoch-fenced prefetch (A1 §3.4), W, E | M3 | dashd-chainstate | CH3a, PF1 | L | STATE | — | **impl O-h; rev Sol→O-h** | NFR-PERF-01/03 | xcheck compares every lookup with a direct read; `coins_cache_model` interleavings (spend/create/flush completion/prefetch completion/eviction/reorg); event-order test · BG: counters; in the window `reindex-chainstate-mainnet(-noav)`, `ibd-*-seed(-noav)` ≤ dashd |
| CH5 | `-reindex(-chainstate)` (mirroring Core's `ContextualCheckBlock` skip, m-2), `-loadblock`, `verifychain` 0–4 (with its effects), `-checkblocks/-checklevel`, regtest Core-datadir auto-import (incl. `indexes/`, m-5), mainnet refusal, live-copy safety (m-6) · O17: under the v23.1 profile, level-3 `verifychain` leaks the BLS flag (CONS-09) and level 4 applies v23's old-budget check (MNO-12) | M3 | dashd-chainstate, dash-coreimport | CH3b, ST2 | M | STATE | — | **impl O-m; rev Sol→O-h** | FR-CHS-08/09, FR-STO-06, FR-STO-07; US-IMP-01, US-NODE-19 | FT `feature_reindex`, `feature_loadblock`, `feature_dbcrash`; `verifychain` results equal dashd's; FT cache/copy from a dashd node 0 works; the nightly copy-under-write test · BG: `reindex-mainnet` counters |
| CH6a | Core datadir **safe** import: stopped/locked or snapshot source, staging, sealed-file hardlinks, CoW tails, source hashing, verification report | M4 | dash-coreimport, dashd-chainstate | CH5 | M | STATE | — | **impl O-m; rev Sol→O-h** | FR-IMP-02/06; US-IMP-02 | safe import of the v23.1.8 reference + `-reindex-chainstate` digests equal; the source's files unchanged through import, destination mining/reorg/restart and source restart · BG: import wall clock recorded (new metric) |
| CH6b | Core datadir **fast** import (provisional): chainstate/evodb transcoding; a mandatory source report; complete canonical-state comparison; corrupt/missing-record fixtures; bounded connect/disconnect/restart differential vs a safe import | M6 | dash-coreimport, dashd-chainstate | CH6a, CH7b, EV4-serve (SB0 rev6.1) | M | STATE +SEC (untrusted DB parser) | — | **impl O-m; rev Sol→O-h** | FR-IMP-02/06; US-NODE-18 (chain) | the A1 §2.10 evidence list; until it passes, `fast` prints "provisional" and refuses without a source report · BG: — |
| CH7a | Evo integration: special-tx orchestration, CbTx MN root and credit pool, payments/value via `ChainHelper` (NoCheck), MNHF, BLS context (`for_block`) and undo, `EvoDelta` in the B1 batch, **the effect channel** (A1 §2.10.1) and the views (§2.10.2) · BLS scheme scoped per attempt (`bls_ctx_for_block`) and published as `ChainView.bls` only when a connect commits; no process-wide scheme and no persisted flag (CH7 D9) · Emit Core's `LOG_TIME_SECONDS("write evodb cache to disk")` pair around B1's durable write, which carries the EvoDelta (Core's text; printed before the coin batches, where Core prints it after the coins flush) (CH2 D17, CH7 D20). Slices CH7a-1…5 per CH7 §8 (DEC-531); CH7a now also carries the LLMQ null rules and the quorum root. | M4 | dashd-chainstate::evo | CH3a, EV1 (CH7a-2a none, behind the `Ev1Checks` seam; CH7a-2b binds EV1-2 `BlockPayloads`, EV1-3 dispatcher + covenant, EV1-5 `check_cbtx`; EV1-5 for CH7a-4; DEC-581), EV3, EV4-serve (SB0 rev6.1), i:CH3b, i:O2b | L | CON | D (CH7a + b) | **D O-h; impl O-h; rev Sol→O-h** | FR-CON-19, -21, -22, -27, -29, -32; FR-CHS-03/04; FR-EVO-01…09 | L2 mainnet 1,028,160→V20 windows + full testnet: `protx list` (sampled), `masternode payments` (sampled), credit pool, deployments equal; `diff_chain` with evo ops; effect parity for the §2.10.1 table; the scoped-scheme tests of CH7 D9 (`ChainView.bls` equals `bls_ctx_after(tip)` across V19 in both directions, with TestBlockValidity, failed attempts and VerifyDB in between); quirks 13–16, 24–27 · BG: `reindex-chainstate-mainnet` counters not worse than the previous head |
| CH7b | LLMQ integration: commitments in blocks (eager BLS decode, null rules), quorum root, rotation snapshots as effects, CL enforcement + `ChainHelper` CL/IS plumbing, `ReplayChainHelper` (failed attempts too). CL-conflict scenarios moved to LK1a (F17) · (SB0 rev6.1; EV addendum rev5 §2.3–§2.6, §4.3, applies on approval) the `LlmqSource` and facade map `MemberError` per addendum §2.3; the attempt handle and overlay, the gate, `release_attempt(EffectsApplied)`, the `VContext`/`OnV`/`OnValidation` capability with whole-route V dispatch for everything except the `RawRows` allow-list, and `begin_attempt` reachable only with `VContext` (DEC-113, DEC-117); the precompute loop; read-only `peek`, `LlmqSource` and `llmq_S` row reader for mnxcheck (SB19) · Slices per CH7 §8 (DEC-531): CH7b-2 is the production facade, the mined rows and `erased_mined`, the quorum object, the chain-aware `ScanQuorums` and G1q removal; CH7b-3 is the CL verify, `CbTxClCell` and `ReplayChainHelper`; the null rules and the quorum root moved to CH7a | M4 | dashd-chainstate | CH7a, LQ2, LQ3, BS11a (acceptance); for CH7b-3 also the CH2 prune-lock set and the CH5 startup data check (DEC-518) | L | CON | (CH7 D) | **impl O-h; rev Sol→O-h** | FR-CON-18, -28, -29, -32; FR-CHS-05; FR-LLMQ-01/02/04/05 | `diff_chain` with QcTx and rotation; segment replays across DIP24 and V19; the REL-1.1 campaign (M4 exit) uses this task's build; addendum §2.4's node latch test; addendum §2.6 cases 1–6 on the production handle (case 6 with real threads, plus 6-D) and the compile-fail suite; `peek` leaves cache state and access order unchanged · BG: REL-1.3 counters |
| CH7c | **DROPPED (DEC-82: v24 only).** v23.1 quorum-root cache semantics (`UngatedRules.quorum_root_cache`, O17 DKG-08): process-wide `merkleRootQuorums` caches with v23.1.8's keys and lifetime across connect, disconnect and template calls; the v24 variant invalidates on every mined-commitment write | M5 | dashd-chainstate, dash-evo::cbtx | CH7b, EV4 | S | CON | — | **DROPPED (DEC-82)** | FR-CON-29 | `bad-cbtx-quorummerkleroot` and template parity after a changed-commitment reorg, against a fresh and a warmed v23.1.8 reference (O10, O15) · BG: — |
| CH8 | Pruning (prune locks, incompatibilities, `pruneblockchain`; unlink after the Durable tip batch) | M5 | dashd-chainstate, dashd-indexes | CH5, IX1 | M | STATE | — | **impl O-m; rev Sol→O-h** | FR-STO-04, FR-IDX-05; US-NODE-05, US-LCS-06 | FT `feature_pruning`, `feature_index_prune`, `feature_fastprune`; CRASH with pruning · BG: pruned `disk_total_gib` ≤ dashd |

### 4.4 W4: Evo and LLMQ consensus, LLMQ protocol math (A1 §2.5–2.6)

| ID | Task | MS | Crates | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| EV0 | **Unheld (DEC-122, EV addendum approved at rev6 8a99b94d).** (SB0 rev6.1; EV addendum §1, §3) `dash-evo` (shared) and `dash-dml` (node-only) interfaces, types first, reshaped in seven commits on the EV0 branch. `dash-evo` keeps `ctx`, `effect` (`Effect` + `EffectSink`), `bls` (`BlsCtx` plumbing), the LQ1 views and member trait, `QuorumSnapshot`, `MinedCommitmentReader` and `ProTxHash`/`MnType`; `dash-dml` takes `MnList`/`Dmn`, `EvoStore` with ancestry-resolved reads (`at: BlockId`), the in-memory store, `EvoDelta`, credit pool, MNHF and `imbl`. Generic `QuorumMembers<E>` and `MemberError<E>` (`Missing`, `View(E)`, `UnknownLlmqType`), with store-independent view errors and the fallible `LlmqChain` | M1–M2 | dash-evo, dash-dml | CS0, i:BS6a | S | CON | — (A1 §2.5, §2.10.1; EV addendum) | **impl O-m; rev Sol→O-h** | FR-EVO-01 | API review by the EV1/EV2a/LQ1a owners; compile-only consumers; the addendum §3 commit acceptances: both crates in `node-pure`, `dash-evo`'s normal closure free of `imbl` and `dash-dml`, the shared addendum §2.4 test, the cross-crate entry test and the two-call overlay test (EV0 r3: Sol, then Opus high) · BG: — |
| EV1 | Special-tx contextual checks (12 types × versions; 10–12 v24 only), NetInfo validation, shared-collateral covenant · O17: ProTx check order (EVO-16), pre-DIP3 reject string (EVO-17), AU window class (MPIX-16) and the v3 decode rule from BS6c (EVO-14) | M2–M3 | dash-dml::specialtx (SB0 rev6.1; EV addendum §1.5) | EV0, CS3, BS6a, O2a, i:O2a-LQ1 | L | CON | D | **D O-h; impl O-h; rev Sol→O-h** | FR-CON-25, -26, FR-EVO-06 | L0 trivially_valid/invalid (both BLS variants, exact codes); L1 `diff_check_special_tx` (std, per pin, under each profile's rules) through `oracle_check_special_tx` (O2a: `CheckSpecialTxInner` on the mempool path, with no chain and no asset-unlock index set, against a synthetic list, coins, ancestry, deployments and BLS flag; for types 1–6, 8 and 10–12 this is the block-path code). MNHF (7), asset unlocks (9) and the block-path chain/index-set variants go through O2a-LQ1's quorum-manager context; until it lands they are L2/L3 only (v1's `oracle_dml_apply` did not cover this, F15); reject-string census; quirks 21, 26, 27 · BG: crit special-tx checks |
| EV2a | **Unheld (DEC-122, EV addendum approved at rev6 8a99b94d).** (SB0 rev6.1; EV addendum §4.1) `MnList` data model (imbl, unique properties, payee-queue structure), diff/snapshot codecs (canonical EvoSnapshot), `EvoStore` + in-memory store, in `dash-dml`; adds `impl MnListView`/`ScratchMnList for MnList` (`Entry = Arc<Dmn>`); `MnList::project_sml` moves to EV4-serve (DEC-109 (2)) | M2–M3 | dash-dml | EV0, i:O8, i:O2a-EV | L | CON | **D2** (EV2a + b) | **D O-h; impl O-h; rev Sol→O-h** | FR-EVO-01, FR-STO-08 | snapshot bytes → `CanonicalMNListHash` equals the patched v24 oracle on imported lists; codec fuzz round-trip; quirk 23; LQ1 §2.2's view contract tests for `MnList` · BG: crit list clone/diff |
| EV2b | **Held with EV2a until the EV addendum is approved.** (SB0 rev6.1; EV addendum §5) `build_new_list_from_block` in Core's order, PoSe, payee selection (queue + reference twin), confirmations, collateral spends (incl. ordinary txs); members through `&DmlMemberSource`, errors through `NodeMemberError` | M3 | dash-dml | EV2a, EV1, PF1 (the payee queue's budget), O2a-EV, i:LQ1a (trait only), i:O8 | L | CON | (EV2 D2) | **impl O-h; rev Sol→O-h** | FR-EVO-02…05 | L1 `diff_dml_apply` ≥ 24 CPU-h; `mnlist_payee_queue` fuzz; per-block DML digest equal over a 50k window from 1,028,160; quirks 13, 24, 25 · BG: crit `build_new_list_from_block` on mainnet blocks ≥ 2× the twin |
| EV3 | Payments (GetBlockTxOuts, payee/value validation, superblock hooks), credit-pool eras, MNHF signals (the shared-collateral spend rule is EV1's; CH3a calls it at ConnectBlock, EV1 D7) · O17: asset-unlock index membership (CONS-10) and the old-budget gate (MNO-12) per profile | M3–M4 | dash-evo::payments (TxOut-list rules); dash-dml::{payments, creditpool, mnhf} (SB0 rev6.1; EV addendum §1.5; DEC-276) | EV2b, O2a-GV | L | CON | — | **impl O-h; rev Sol→O-h** | FR-CON-21, -22, -26, -27, FR-EVO-09 | `oracle_block_value_payee`, `oracle_credit_pool_step` (O2a-GV); `getcreditpoolinfo` equal from 1,987,776 (v24 oracle); L1 `diff_credit_pool`; F3/F6 leniency tests; quirks 14, 15, 31 · BG: — |
| EV4-serve | (SB0 rev6.1; EV addendum §1.5, §5) **The node's SML serving half of EV4**; EV4-core (SML entry and hash, the roots, the CbTx check, the coinbase-CL predicate) is SB7. `MnList::project_sml` (Core `CDeterministicMN::to_sml_entry`, `v24 evo/deterministicmns.cpp:43-46`; list build `:255-259`; DEC-109 (2)), the incremental `merkleRootMNList` cache (cached + reference), the mnlistdiff builder in CHAMP order (every band; quirk 41), the evodb importer | M4 | dash-dml (projection, root cache, mnlistdiff builder); dash-coreimport::evodb | EV2b, BS8, SB7 | M | CON | — | **impl O-m; rev Sol→O-h** | FR-EVO-07 (with SB7), -08, -12 | mnlistdiff byte-identical vs oracle `getmnlistd` in every version band; `project_sml` equals Core's `to_sml_entry` bytes on imported lists; quirk 17 (L0 `evo_simplifiedmns_tests` and the CbTx MN root over all history are SB7's) · BG: crit incremental vs full SML root ≥ 10× |
| LQ1a | **Unheld (DEC-122, EV addendum approved at rev6 8a99b94d).** (SB0 rev6.1; EV addendum §4.2) Non-rotated member selection, modifier (pre/post V20), `scan_quorums` (ancestry-resolved), `select_quorum_for_signing`, with the addendum's store-independent fallible signatures (addendum §2.2, §2.5), the attempt-overlay contract (addendum §2.6), `members_uncached` (non-rotated, crate-internal) and `SIGN_HEIGHT_OFFSET`; the SML adapter for SPV's `membersSig` input is SB8's | M3–M4 | dash-evo::llmq | EV2a, BS5, O2a-LQ1, i:SB7 | M | LLMQ | D (LQ1a + b) | **D O-h; impl O-h; rev Sol→O-h** | FR-LLMQ-01, -02, -05 | L1 `diff_quorum_members` (non-rotated), `diff_select_quorum`; members per mined quorum (sampled digest); quirks 29, 30; the not-held tests and addendum §2.5's negative controls; the node missing-list test; addendum §2.6 cases 1–6 on `ModelAttempt`; `members_uncached` ≡ the model facade on cold captures, with state untouched; addendum §4.3's warm-cache case on the model · BG: — |
| LQ1b | **Held with LQ1a until the EV addendum is approved.** (SB0 rev6.1; EV addendum §5) DIP24 rotation: quarters, skip lists and modes, **snapshots as effects** (emitted before any commitment check), first V20 cycle; `UsedSet` in `dash-dml`; the rotated `members_uncached`, made `pub`. LQ1b gates SB11; LQ1b-F's dash-spv test PR moves to SB15 | M4 | dash-evo::llmq | LQ1a, i:EV2b | L | LLMQ | (LQ1 D) | **impl O-h; rev Sol→O-h** | FR-LLMQ-02 | L1 rotated members ≥ 72 CPU-h; the 11 regenerated rotated captures, kept as shared vectors (the dash-spv test PR is SB15's; SB0 §12.1); `quorum rotationinfo` per cycle (digest); effect emission in failed and proposal paths (`diff_chain`); quirk 28; the not-held snapshot and previous-cycle controls; the rotated `members_uncached` ≡ the facade; addendum §2.6 cases rerun with real rotation · BG: — |
| LQ2 | Commitments v1–4 (eager BLS decode per A1 §1.4.1; `verify`/`verify_null` with `IsNull ⇔ !IsValid`; commitment hash under `BlsCtx`), in-block rules, mined index, `merkleRootQuorums` over configured types, the mineable-pool effects · (SB0 rev6.1; EV addendum §1.5, §5) consumes SB8's one `CFinalCommitment::Verify` (DEC-109 (4)); the block processor lives in `dash-dml` | M4 | dash-evo::llmq::commitment; dash-dml (blockprocessor) | LQ1b, BS8, O2a-LQ1, SB8 | L | LLMQ | — | **impl O-h; rev Sol→O-h** | FR-CON-28, -29, FR-LLMQ-04 | `quorum listextended` digest; quorum root equals CbTx over all history; L3 QcTx BLS-encoding operators (null and non-null); basic batch vs individual twin; legacy individual; the `fJustCheck` duplicate rotated commitment rule per profile (A1 §2.25 row 24); quirks 18–20, 36 · BG: crit commitment verification per block |
| LQ3 | SignHash, request IDs, recsig/CL/IS verification (lazy BLS, wire-byte hashes), qrinfo builder | M4 | dash-evo::llmq | LQ2, BS11b | M | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-LLMQ-08, -11, FR-CL-01, FR-IS-01/03 | L0 `evo_islock_tests`; every historical coinbase CL verifies; qrinfo byte-identical vs oracle incl. the < 70236 path; quirk 38 · BG: crit CL verify |
| LQ4a | Pure DKG: contribute, complain (v24 bad-connection criteria, A1 §2.6; DEC-82/DEC-616), justify (IES), batched signature verification (DEC-616) · O17 DKG-01's P2a part is done (`dash-p2p` conn.rs records VERSION services/relay) | M5–M7 | dash-llmq | LQ1b, BS9a, BS11b, BS12, O17 | L | LLMQ | D (LQ4a + b) | **D O-h; impl O-h; rev Sol→O-h** | FR-LLMQ-03 | in-process DKG with 3–100 members (incl. ≥ 32-member mid-phase verification); `dkg_session` fuzz (drops and lies); BS12 transcript replay: qcomplaint/qjustify byte-exact per design §4.3 T3-C (own qcontribs adopted, justify-lie keys replayed); 12/7 Core capture; v24 criteria tests incl. 70240/70241 bad and 70242 good · BG: crit contribution/verification time |
| LQ4b | Pure DKG: commit, finalize, single-member path, sim errors | M5–M7 | dash-llmq | LQ4a, BS11c (P-1), P-4 (BL-475); LQ4b-2 also O2a-DKG | L | LLMQ | (LQ4 D) | **impl O-h; rev Sol→O-h** | FR-LLMQ-03/04 | 400-member in-process DKG; `bls_llmq_100` corpus + BS12/rc.4 transcript replays (qpcommit/qfcommit per design §4.3 T3-C; commit-lie by equivalence over all lieTypes; LQ4b-1 merges only after the labelled rc.4 rerun agrees, DEC-619); in-process `dkgsimerror` matrix incl. every lieType (live comparison vs dashd is DK1's, DEC-616 #2); LQ4b-2: `diff_dkg_session` ≥ 24 CPU-h incl. ≥ 33-member scripts and the members-access count · BG: crit finalize time |
| LQ5 | Sig-share recovery math (member order, MSM Lagrange + naive twin), quorum topology | M5–M7 | dash-llmq | LQ3 | M | LLMQ | — | **impl O-h; rev Sol→O-h** | FR-LLMQ-07, -09 | MSM vs twin; topology equals the oracle (ring, mesh, initiator) for sampled quorums · BG: crit LLMQ_400_60 recovery ≥ 2× naive |

### 4.5 W5: P2P (A1 §2.14, §4.10)

| ID | Task | MS | Crates | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| P1 | `dash-p2p-transport`: base-sdk V1/V2 framing and short IDs; #264's handshake/state machine (responder role, inbound v1 detection, short-ID gating, v1 fallback, Dash salt); **transport work only** | M2 | dash-p2p-transport | BS7 | M | P2P | — | O-h | FR-P2P-03, -04 | v2 interop with dashd both directions; transport state-machine fuzz; garbage-length KS vs dashd; behaviours 9, 10 · BG: crit frame encode/decode |
| P2a-core | (SB0 rev6.1) **The node processor's core on `dash-p2p`** (P2a rev3 §7.2, phases A, C and E, reshaped by SB0 §3 and §12.1; the network half is SB6): the `PeerManager` actor loop (P2a §3.1), `process_messages`/`send_messages` in Core's order, the PQ quirks, both receive queues, the 74-row registry with the M2 stubs and the `NetHandler` dispatch, the misbehaviour framework and P2a's scores (P2a §3.10), `LocalIdentity` from `fingerprint.rs` passed to SB6's handshake functions (v24, DEC-82), headers/headers2 sync with the hash-once path (P2a §3.4), outbound eviction and the stale tip (P2a §3.9), `getpeerinfo` state stats. It uses SB6's connection table, gates, decode policy, ping and `NetCore::push`, and `dash-clock`; the shutdown order is the host's (ND1) | M2 | dashd-p2p | P1, O17, i:SB6, i:CH1-node, i:RP-V24 | M | P2P | **D2** (P2a rev3, DEC-56; reshaped by SB0 §12.1) | **impl O-h; rev Sol→O-h** | FR-P2P-05 (with SB6), -08 (headers), -16 | `sim` transcripts equal the dashd fixtures for every connection type through the first `getheaders` (v24); handler-level `diff_p2p_*` for the M2 command set through SB6's handshake and gates, ≥ 24 CPU-h (`version`, `verack`, `send*`, `ping`, `pong`, unknown, `m2_stubs`, `sequence`, `multi_handshake`, `headers`, `headers2`, `inv`, `getdata`, `notfound`); the 74-row registry table test; the `denialofservice_tests` port; P2a §2.2.1's liveness test; behaviours 1–7, 11, 25 (outbound) and +C, with P2a-net's connections; O13a's outbound transcripts at M2 exit · BG: `ibd-*` `header_sync_s` (window); counters for header sync |
| P2a-net | (SB0 rev6.1) **The node's connection drivers and address relay on `dash-p2p`** (P2a rev3 §7.3, phases D and F; phase B's AddrMan port and the dial steps are SB6's): the `opencon`, `addcon` and `dnsseed` threads driving SB6's sans-io `*_step` functions through `NetCore::dial_tick`; anchors, feelers, extra peers, `-connect`/`-addnode`/`-seednode`, the fixed-seed table, `InitBinds` with `bind_failure_fatal`, `open_one_try` (`addconnection`, `addnode onetry`), `setnetworkactive`, `NodeStats`; `peers.dat`/`anchors.dat` in Core's format through SB6's `NetStore`; the node's relay over SB6's addr intake and AddrMan (`RelayAddress`, `MaybeSendAddr` in `send_messages`, the address-response cache, local addresses, `-externalip`) | M2 | dashd-p2p | P1, O17, i:SB6, i:P2a-core | S | P2P | (P2a D2) | **impl O-m; rev Sol→O-h** | FR-P2P-06, -07 (with SB6) | `connman_sched` fuzz; the `sim` ladder scenarios (slot limits, the netgroup rule, one feeler, regtest churn) under the virtual clock; loopback `-connect`, `-addnode`, `-seednode`, feelers via `open_one_try` and the anchors round trip; the fixed-seed table test; `diff_p2p_{addr,addrv2,getaddr}` ≥ 24 CPU-h with the seeded shuffle; the joint gate with P2a-core, `diff_p2p_multi_headers` and `diff_p2p_multi_relay` ≥ 24 CPU-h (P2a §7.3a); FT `p2p_add_connections`, `feature_anchors`, `p2p_dns_seeds`, `p2p_addrfetch` and `p2p_addr_relay` (outbound) at M2 exit; P2a's addrman fuzz and bucket-math parity are SB6's `diff_addrman` · BG: — **Note 10-20 (manager):** the `diff_p2p_{addr,addrv2,getaddr}` ≥ 24 CPU-h exit is superseded by DEC-252 / P2a-net D39 (recorded uncertified at ~1.5 CPU-h farm plus one certify window each). The M2-exit Core functional tests stay on this row and wait for RP1a's RPC surface on rdashd. |
| P2b | (SB0 rev6.1) Inbound minimal over SB6's `inbound` feature (listener, inbound limits, `AttemptToEvictConnection`): accept, the handshake as responder, the pre-version/pre-verack gates, headers/getheaders serving (rev1, F17) | M2 | dashd-p2p | P2a-core, SB6 | M | P2P | — | **impl O-m; rev Sol→O-h** | FR-P2P-05, -10 (headers) | M2 exit 2 (inbound transcripts); behaviours 3–6 inbound · BG: — |
| P3 | Block download, stalling, compact-block client, block-relay-only, IBD from the network (blocks awaited through `ChainCommand`); ProcessBlock's tail (np:3717-3732, erasing the block source) runs only after `BlockChecked` delivery, or the erase moves into `block_checked` (P2a-core D65), and the hook's `ibd` is V's value, never the published view | M3 | dashd-p2p | P2a-core, P2a-net (SB0 rev6.1), i:CH3b | L | P2P | — | **impl O-h; rev Sol→O-h** | FR-P2P-08 | IBD testnet from public peers; FT `p2p_ibd_stalling`, `p2p_compactblocks` (client), `p2p_block_sync`; behaviour 24 · BG: `ibd-testnet-seed` (window) |
| P4 | Inbound full: eviction (MN protections), banman, misbehaviour (per profile), permissions, serving (blocks, getblocks, merkleblock, BIP37 + Dash matching, notfound), NODE_NETWORK_LIMITED, `-capturemessages` · O17: block read vs prune race per profile with a common I/O-fault fallback (P2P-17); unsolicited-object scoring with its grace window and type binding (P2P-12) | M5 | dashd-p2p | P3, P2b | L | P2P | — | **impl O-h; rev Sol→O-h** | FR-P2P-06, -10, -11, -18; US-NODE-06, US-LCS-02/03/06, US-DEV-08 | FT tier-3 p2p set (both pins); L6 behaviours 6, 10–15, 17–19, 26, +D · BG: `rpc-mainnet` `getpeerinfo`; CPU at 125 peers ≤ dashd (counters + window) |
| P5a | **Overlay intake for a following node** (rev1, M-10.1): the `NetHandler` registry and Core's order; `clsig` and `spork` intake and relay over outbound peers | M4 | dashd-p2p + handlers | P2a-core (SB0 rev6.1), SP0, i:P3 | M | P2P | — | **impl O-m; rev Sol→O-h** | FR-P2P-13, FR-CL-01 | live testnet: clsig and spork received and relayed like dashd; behaviour 23 · BG: — |
| P5 | Tx relay, **the v24 `TxRequestTracker`** (DEC-82: no v23 legacy `ObjectDownloadState`), Poisson trickle, orphans (v24 work-set owner) · O17: A1 row 9's constants corrected (P2P-05/-06); P2P-08/-09/-11 take the v24 side | M5 | dashd-p2p | P4, MP1a, O17 | **M** (DEC-82: one implementation) | P2P | — | **impl O-h; rev Sol→O-h** | FR-P2P-09 | FT `p2p_tx_download`, `p2p_tx_privacy`, `p2p_leak_tx`; behaviour 16 KS · BG: `mempool-accept` counters |
| P6a | Overlay relay and serving I: isdlock, govobj/govsync (non-MN), dstx/dsq relay, platformban, `MSG_ASSET_UNLOCK` (profile) | M5 | dashd-p2p + overlay handlers | P5a, P4 | M | P2P | — | **impl O-m; rev Sol** | FR-P2P-10, -13; US-CJS-05 | behaviours 8, 17, 29–31; FT `p2p_dstx` (relay) · BG: — |
| P6b | Overlay serving II: mnlistdiff/qrinfo (per-version bands; QRINFO caps per profile), cfilters · O17: QRINFO request rules (DKG-12/RPC-39, `P2pRules.qrinfo_request`) and the missing-block score (DKG-13) · (SB0 rev6.1) cfilters are built with `dash-filters` (SB9) | M5 | dashd-p2p | P4, EV4-serve, LQ3, IX1, BS10, SB9 | M | P2P | — | **impl O-m; rev Sol** | FR-P2P-10; US-LCS-01/04/05 | SPV smoke (O16); behaviours 20–22, +B; FT `p2p_blockfilters` · BG: — |
| P7 | MN connection API, quorum connection sets, probes, watch connections, the 100 ms MN send throttle, the MNAUTH inbound cap (profile) | M7 | dashd-p2p, dashd-masternode | AM1, LQ5 | M | P2P | — | **impl O-m; rev Sol** | FR-P2P-14, FR-LLMQ-07 | FT `feature_llmq_connections` (mixed B); behaviour 28 · BG: — |
| P8 | Privacy networks (SOCKS5, Tor control + onion, I2P SAM 3.1, CJDNS, `-onlynet`, `-proxyrandomize`), asmap, NAT-PMP/UPnP | M5–M8 | dashd-p2p | P4 | L | P2P | — | **impl O-m; rev Sol→O-h** | FR-P2P-12; US-NODE-07/08 | FT `feature_proxy`, `p2p_i2p_*`, `feature_asmap`, `feature_bind_*`, `feature_discover` · BG: — |

### 4.6 W6: Services (args, node, RPC, CLI, mempool, mining, indexes, ZMQ/REST)

| ID | Task | MS | Crates | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| AR1 | `dash-args` (ArgsManager port, full option table, `settings.json`) | M0–M1 | dash-args | — | M | OTH | — | **impl S-h; rev Sol** | FR-CFG-01/02; US-NODE-03 | port of Core's `getarg_tests`/`settings_tests`; every Core option name recognized; FT `feature_config_args`, `feature_includeconf`, `feature_settings` (M2) · BG: — |
| AR2 | `dash-rpc-spec` generator, per pin (names, args, aliases, verbatim help, conversion table, whitelist names, PRD §5 classes) | M0–M1 | dash-rpc-spec | — | M | OTH | — | **impl S-h; rev Sol** | FR-RPC-05/07, FR-CLI-01 | `help <cmd>` byte-equal to each pinned release; conversion table equals `rpc/client.cpp` · BG: — |
| AR3 | `rdash-cli` (generic client, `-getinfo`, `-netinfo`, `-addrinfo`, `-generate`, `-rpcwait`, `-stdin*`, `-named`, `-rpcwallet`, `-color`) | M0–M2 | rdash-cli | AR1, AR2 | M | OTH | — | **impl S-h; rev Sol** | FR-CLI-01…04; OW-01/02 | XCLI vs dash-cli against dashd (works before rdashd exists); `interface_bitcoin_cli` (M5) · BG: — |
| AR4 | `rdash-cli tx` / `util grind`; `rdash-wallet info/create/dump/createfromdump` | M8 | rdash-cli, rdash-wallet | AR3, WL3 | M | OTH | — | **impl S-h; rev Sol** | FR-CLI-05, REL-3.3 | `bitcoin-util-test.json`; `tool_wallet` · BG: — |
| ND1 | `dashd-node` + `rdashd`: init order (MN soft-sets/InitErrors, v24), shutdown (both flushes run after an abort, exit 1; a failed shutdown flush is an AbortNode: DEC-241), `NodeClock`/mocktime, scheduler, Core-format logging + asserted-log list, pid/lock, daemon, notify hooks, datadir layout (DEC-82: no `-releaseprofile`) · O17: the v24 side of `InitRules` (P2P-24/-25/-29/-33..-38, WAL-29, CONS-23, P2P-31, DKG-06) · (SB0 rev6.1) host services for the shared crates (`dash-clock`, `AbortSink`, `BlockingIo`, `Resolver`, the shutdown order) and the `-spv` InitErrors (CH2 P8); no dash-spv dependency (SB0 L2; rdashd composes `-spv`, SB18) | M2 | dashd-node, rdashd | AR1, CH1-node, i:RP-V24 | M | **STATE** (clock, init, shutdown; F18) | — | **impl O-m; rev Sol→O-h** | FR-CFG-03…07, FR-STO-06; US-NODE-12/19 | FT `feature_filelock`, `feature_shutdown`, `feature_logging`, `feature_startupnotify`; stderr empty on stop; Core-datadir refusal test · BG: `startup-regtest` counters; `startup_s`, `shutdown_s` ≤ dashd (window) |
| RP0 | `dashd-rpc` core (HTTP, auth, bind/allow, whitelists, queues, JSON-RPC 1.x dispatch, squashing, named args, batches, `/wallet/`, warmup, error codes, number format) · O17 RPC-10: batch authorisation under `-rpcwhitelist` per profile | M2 | dashd-rpc | AR2, ND1 | M | OTH | — | **impl O-m; rev Sol** | FR-RPC-01…05, -08, -10; US-INT-13/14 | FT `interface_rpc`, `interface_http`, `rpc_users`, `rpc_whitelist`, `rpc_named_arguments`, `rpc_help`, `rpc_bind`; DIFF-RPC on errors and help; the `feature_shutdown` RPC form (`getrpcinfo` with active commands), the `.cookie` check and the `stop` RPC path, moved from ND1 (DEC-201) · BG: `rpc-regtest` counters |
| RP1a | Tier-1 RPCs (`getblockhash` and `submitblock` land early from ND1, DEC-279): blockchain read, control, util, hidden test RPCs (setmocktime, mockscheduler, waitfor*, syncwithvalidationinterfacequeue, generate*, invalidate/reconsider, submitblock/header, getblockstats, gettxoutsetinfo) | M3 | dashd-rpc | RP0, CH3b | M | **STATE** (block submission, invalidation; F18) | — | **impl O-m; rev Sol→O-h** | FR-RPC-06/07; US-INT-10/11/12, US-NODE-16/17, US-DEV-03/05 | FT tier-1 RPC subset; DIFF-RPC for the category · BG: `rpc-mainnet` `getblock` counters |
| RP1b | The rest of RP1: dumptxoutset, scantxoutset, getspecialtxes, txoutproof, rawtx read (getchaintxstats and getblockheaders were served in RP1a-2b; DEC-862) | M5 | dashd-rpc | RP1a, IX1 | M | OTH | — | **impl S-h; rev Sol** | US-INT-08/15 | DIFF-RPC; PRD §5 MUST rows for these · BG: `getrawtransaction` counters |
| RP2a | Evo/quorum read RPCs needed by O15 and M4: `protx diff/list/info`, `quorum list/info/listextended`, `getbestchainlock` (rev1, M-10.3) | M4 | dashd-rpc | RP1a, CH7b | M | OTH | — | **impl S-h; rev Sol** | US-MN-10, US-PLAT-02 | DIFF-RPC on the v24 oracle (mainnet + testnet) and the v23.1.8 reference (mainnet shapes) · BG: `protx list` counters |
| RP2b | The rest of RP2: masternode/governance/spork/mnsync read RPCs, `evodb verify/repair` (STATE) | M5 | dashd-rpc | RP2a, GV1b, SP1 | M | OTH (repair: STATE) | — | **impl S-h; rev Sol** | FR-GOV-11; US-GOV-01, US-PLAT-05, US-NODE-13, US-DEV-06 | DIFF-RPC; Platform P-1 fixtures; `rpc_masternode`/`rpc_quorum` read paths (mixed A) · BG: `masternodelist` counters |
| RP3a | Mempool/rawtx-write/mining RPCs: sendrawtransaction, testmempoolaccept, submitpackage, prioritisetransaction, getblocktemplate (Core's cache policy), submitblock paths | M5 | dashd-rpc | MP1a, MN1 | M | OTH (GBT proposal: CON review) | — | **impl O-m; rev Sol→O-h** | FR-MEM-10, FR-MIN-03/04; US-INT-04 | FT `rpc_rawtransaction`, `rpc_packages`, `mining_*`; P-1 fixtures; OW-14/17 · BG: `gbt-regtest` counters |
| RP3b | Platform/LLMQ RPCs: quorum sign/platformsign/verify/getrecsig/hasrecsig/isconflicting/selectquorum, submitchainlock, verifychainlock/islock, getquorumproofchain/verify · O17 RPC-39: the `quorum rotationinfo` cap per profile | M7 | dashd-rpc | SG1a, LK1c | M | OTH | — | **impl S-h; rev Sol** | FR-LLMQ-10/12, FR-CL-05; US-PLAT-03/05 | FT `rpc_verifychainlock`, `rpc_verifyislock`, `feature_quorum_proof_chain`; P-2 fixtures; OW-22 · BG: — |
| MP1a | Mempool: entries, indexes, Dash conflict maps, **serial-equivalent ATMP** (stage A parallel, stage B sequencer re-running every cheap mutable check; A1 §2.12), policy/standardness per profile, IS hooks · O17 MPIX-16: AU window failure class | M5 | dashd-mempool | CH7b | L | CON | D (MP1a + b) | **D O-h; impl O-h; rev Sol→O-h** | FR-MEM-01…04, -09, -10 | FT `mempool_accept`, `mempool_reorg`, `p2p_invalid_tx` (both pins); L3 `testmempoolaccept` mutations 100%; `mempool_model` parallel ≡ serial property (F07 races); DIFF-RPC `testmempoolaccept` · BG: `mempool-accept` counters; `tx_accept_tps` ≥ dashd (window) |
| MP1b | Mempool: eviction, rolling minimum fee, expiry, trimming, orphanage, packages, persistence (ours + Core `mempool.dat` read), sequence numbers · O17 CONS-25: adopts CH3b's `DisconnectedBlockTransactions` (NullMempool's pool → `::default()`); the mempool half of `MaybeUpdateMempoolForReorg`, including CE4-17 (CH3b D183; DEC-489)  · CH7 §11.2 (DEC-531): the sink answers `has_mempool` true so V's `reorg_drain` moves `dmn_tip` before every add=true drain; MP1b never sets `dmn_tip` itself; in Core's calls made while `tipIndex` lags (the re-add's addUncheckedProTx and removeForBlock's ProTx conflict removal) it reads the `tip_list` argument, and elsewhere `ChainView.mn_list` | M5 | dashd-mempool | MP1a | M | STATE | (MP1 D) | **impl O-m; rev Sol→O-h** | FR-MEM-04, -05, -07, -11 | the remaining `mempool_*` (15 total, incl. `mempool_persist` with a Core file) · BG: `mempool_rss_mib` ≤ dashd |
| MP2 | Fee estimator (`CBlockPolicyEstimator` port; float sites allowlisted, A1 §4.5) + persistence | M5 | dashd-mempool | MP1a | M | OTH | — | **impl O-m; rev Sol** | FR-MEM-08; US-INT-09 | FT `feature_fee_estimation`, `rpc_estimatefee`; `estimatesmartfee` DIFF-RPC on a replayed mainnet tx stream · BG: — |
| MN0 | Minimal regtest miner (empty-mempool CreateNewBlock, CbTx fields, `generate*`); slices MN0-1/2a/2b/3 (DEC-612); `GetMineableCommitmentsTx` lives in dash-dml | M3 | dashd-mining, dash-dml | CH3a; MN0-2b also CH7a-4 | L | CON | — | **impl O-m; rev Sol→O-h** | FR-MIN-05; US-MINE-05 | blocks accepted by dashd; `mining_basic` subset · BG: — |
| MN1 | Full BlockAssembler, GBT (all fields, refusals, longpoll, proposal), `submitblock`, **template cache with Core's policy** and the **exact-block DML memo** (A1 §2.13) · O17 MPIX-21: per-package special-tx checks per profile | M5 | dashd-mining | MP1a, CH7b, GV1b, LQ2 | L | CON | — | **impl O-h; rev Sol→O-h** | FR-MIN-01…04; US-MINE-01…04, US-GOV-07 (miner) | alternating-miner regtest 10^5 blocks with equal digests; template contents equal after each same-tip dependency change; FT `mining_*`; L3 GBT-proposal parity · BG: `gbt-regtest` counters ≤ dashd |
| IX1 | `BaseIndex` + txindex (compact) + blockfilterindex (Dash BIP158) + coinstatsindex; locators committed after `ChainStateFlushed`, idempotent re-delivery · O17 MPIX-11: rewind on disconnect per profile · (SB0 rev6.1) blockfilterindex builds with `dash-filters` (SB9) | M4–M5 | dashd-indexes | CH3b (CH7a for special-tx elements), SB9 | L | STATE | — | **impl O-m; rev Sol→O-h** | FR-IDX-01, -03, -04, -06; US-INT-07/08, US-LCS-01 | L0 `blockfilters.json` + oracle filters for ProTx/asset-lock blocks; FT `feature_txindex`, `rpc_getblockfilter`, `feature_coinstatsindex`, `feature_utxo_set_hash`; CRASH with indexes · BG: `disk_txindex_gib` ≤ dashd |
| IX2 | addressindex/spentindex/timestampindex + mempool address/spent indexes · O17: both index lifecycles (`IndexRules.dash_index_model`, MPIX-08/-10): v23.1 needs `-reindex` to enable an index, forces `-checklevel=4` and accepts `-prune`; v24 catches up in the background and refuses `-prune` with address/spent index. IX2 owns these init checks | M5 | dashd-indexes, dashd-mempool | IX1, MP1a | **L** (was M; O17/DEC-37) | STATE | — | **impl O-m; rev Sol→O-h** | FR-IDX-02, FR-MEM-06; US-INT-02/03 | FT `feature_addressindex`, `feature_spentindex`, `feature_timestampindex`; OW-19 · BG: `getaddressutxos` counters; `disk_*_gib` ≤ dashd |
| ZQ1 | ZMQ (19 topics in `dashd-rpc` feature `zmq`, ordering, sequences), REST (all endpoints), notifications, statsd | M5 | dashd-rpc (zmq, rest), dashd-node | CH7b, MP1a, LK1b | M | OTH | — | **impl S-h; rev Sol** | FR-ZMQ-01/02, FR-REST-01; US-INT-05/06, US-NODE-11/15, US-WAL-12, US-GOV-06 | FT `interface_zmq`, `interface_zmq_dash`, `interface_rest`, `feature_notifications`; Platform ZMQ frozen fixtures · BG: — |

### 4.7 W7: Verification and oracles (A1 §4; R4 V1–V9)

| ID | Task | MS | Crates/repo | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| O1 *(running)* | Oracle shim v1 (`node/oracle-shim-v23.1`, `-v24.0`) + `dash-oracle-sys` + `build-oracle.sh` with cache (R4 V2). *Rev1 delta:* none; O2a extends it | M0 | Core branches; dash-oracle-sys | — | M | CON | — | O-h | NFR-CEQ-04 | each v1 entry point has a smoke differential test on BS2 corpus samples; reproducible builds per pin (hash recorded) · BG: — |
| O1c | Oracle shim: stateful peer-session and addrman entries (DEC-48 Q1, DEC-51e; a DEC-16 local patch on both pins, scheduled with P2a phase A; P2a §6.2): `oracle_peer_session_new/add_peer/deliver/send_pass/outcome/free` (multi-peer sessions built from Core's `TestingSetup` + `ConnmanTestMsg` + `PeerManager` harness; capture/substitute markers; one session per process), the seeded-RNG patch (the OS-seeded `addr` shuffle, `GetRand`, ConnMan SipHash seeds; every draw logged), `oracle_addrman_new/op/canonical` (+ `bucket`, `chance`, `netgroup`) | M2 | Core branches `node/oracle-shim-*`; dash-oracle-sys | O1 | M | P2P | — | **impl O-m; rev Sol** | NFR-CEQ-04, FR-P2P-05…08 | a smoke differential test per entry point on both pins; the seeded RNG draws replay identically across two runs; `diff_addrman` and `diff_p2p_multi_*` (P2a §6.2) run against it; reproducible pins · BG: — |
| O2a | Oracle shim v2 + **rev1 contextual entry points** (`oracle_bls_decode/verify`, `oracle_check_special_tx`, `oracle_verify_commitment`, `oracle_check_tx_inputs`, contextual tx/header/block checks, sequence locks, sigops, `oracle_payload_decode`; block value/payee moved to O2a-GV; result class, reason and debug message (no `dos`: neither pin has one; A1 §4.4 erratum)) + **the coverage matrix** `compat/COVERAGE.md` (F15) | M1–M2 | Core branches; dash-oracle-sys | O1 | L | CON (adapter logic, M-7) | — | **impl S-h; rev Sol** | NFR-CEQ-04 | a smoke differential test per entry point; each adapter cross-checked against the stock binary's RPC/functional behaviour on samples; `oracle_dml_apply` reproduces `protx listdiff` on 1,000 sampled mainnet blocks; the matrix reviewed by the manager · BG: — |
| O2a-EV | Oracle follow-up for the DML (EV2 §6.3 items 1–2, DEC-50(3)): `oracle_dml_block`, the validated-block layer (Core's block-path `CheckSpecialTxInner` loop, then `RebuildListFromBlock`, then `CDeterministicMNManager::ProcessBlock`, profile-selected; returns the rejecting stage, Core's state and canonical list bytes; no raw-helper refusal is applied before Core's check loop; includes v24's shared-collateral template check, N8); the eight list entry points `oracle_mnlist_canonical`, `oracle_mnlist_diff`, `oracle_mnlist_apply_diff`, `oracle_state_diff`, `oracle_mnlist_core_order(ops)`, `oracle_mnlist_payee`, `oracle_mnlist_projected` (with deployment context), `oracle_unique_property_hash`; canonical-bytes output for `oracle_dml_apply` (v24 shim; O2a's returns evodb bytes in reload order); `oracle_sml_entry_hash` and the `merkleRootMNList` calculation (N9); `oracle_special_tx_sign_hash`, for BS6b's sighash vectors (T3) | M2–M3, before EV2a-3 (codec and hash targets) and EV2b-2 (block targets) | Core `node/oracle-shim-*` branches; dash-oracle-sys | O2a | M | CON (adapter logic, M-7) | — (EV2 §6.3) | **impl O-m; rev Sol** | NFR-CEQ-04 | a smoke differential test per entry point on both pins (codec and order entry points on the v24 shim only); `oracle_dml_block` equals `protx listdiff` on ≥ 200 sampled mainnet blocks and returns Core's reject string and stage for one crafted block per EV2 §6.5 operator; `oracle_mnlist_canonical` hash equals O8 `getcanonicalmnlist` at ≥ 20 heights; committed `oracle_unique_property_hash` vectors; reproducible pins · BG: — |
| O2a-LQ1 | The LQ1 §6.2 oracle context: `oracle_llmq_ctx_new/free`, `add_header`, `set_mn_list`, `set_coinbase`, `set_snapshot`, `set_mined`, `type_enabled`, `members`, `precompute`, `scan`, `select`, `restart` and `rows_digest`; the DEC-16 record-only trace hooks with the trace format; the canonical `llmq_S` row digest; the fixture loader for LQ1a-A's Core bench. Inside these contexts O2a's `GetAllQuorumMembers` wrap stays off, so Core's own function, caches and hooks run. **Extension**, same task, second phase, before M4 (N7, N10, N11, N12, N22), using the context's CQuorumManager: `oracle_check_special_tx_block` (chain overload and asset-unlock index set), covering MNHF (7) and asset unlock (9); the in-block commitment rules (`CQuorumBlockProcessor::ProcessBlock`, incl. O17 DKG-07); `oracle_quorums_root`; `oracle_sign_hash` and `oracle_request_id`; the CbTx best-CL check | M3 (phase 1, before LQ1a-A merges) → M4 (extension) | Core `node/oracle-shim-*` branches; dash-oracle-sys | O2a | L | LLMQ/CON | — (LQ1 §6.2, §8 Q2) | **impl O-m; rev Sol** | NFR-CEQ-04, FR-LLMQ-01/02 | the LQ1 §6.2 digest known-answer vectors; the hooks change no control flow (reviewed; Core's `llmq_snapshot_tests`/`llmq_utils_tests` pass with hooks on); members equal `quorum info` recorded on an O7-style replica for every mined quorum in sampled windows, including mainnet's first V20 cycle (1,737,792) and testnet; **the `oracle_dml_apply` listdiff check re-run on ≥ 1,000 mainnet blocks from 1,899,072 to the tip**, with members from `oracle_llmq_members` (closes O2a deviation 8), plus ≥ 200 testnet blocks on the v24 pin; MNHF and asset-unlock verdicts equal Core's on mined transactions and on one crafted reject per early check (finding 3) · BG: — |
| O2a-DKG | Oracle follow-up for LQ4 (design §4.2): `GetAllQuorumMembers` members hook (inject + count, O2a-LQ1 MEMBERS trace format), recorded decisions at MakeNewKey/InitEncrypt/GetRandBool/GetRand<int>(5), dkgdb write trace; `oracle_dkg_preverify`, `oracle_dkg_batch_sigs`, `oracle_dkg_session_open/step/close`, `oracle_dkg_finalize`; ABI bump | M5 | Core oracle-shim branches; dash-oracle-sys | O2a-LQ1 | M | LLMQ | — | **impl O-m; rev Sol** | NFR-CEQ-04 | shim selftests; the 8 BS12 sessions reproduced from recorded decisions; access counts for a 3-member round equal LQ4 §3.4.1 · BG: — |
| O2a-GV | The EV3 oracle prerequisites: `oracle_block_value_payee` (`IsBlockValueValid`, `IsBlockPayeeValid` and `GetBlockTxOuts`, per pin, with a governance context holding the sync flag (the F3/F6 leniencies), spork values (v23.1 CSporkManager with SPORK_9; v24's SuperblockManager inputs), superblock triggers as payment lists, the old-budget gate (MNO-12), the tip's MN list and the platform share); `oracle_credit_pool_step` (N14) | M3–M4, before EV3's gate | Core `node/oracle-shim-*` branches; dash-oracle-sys | O2a, i:SP0 (spork-value semantics) | M | CON | a short context-format note reviewed with EV3's brief | **impl O-m; rev Sol** | NFR-CEQ-04, FR-CON-21, -22, -27 | verdicts and expected outputs equal the mined coinbases at sampled heights, including the superblock window from 614,820 and every 576th block from DIP3, at both pins; one crafted reject per Core reject string (`bad-cb-amount`, `bad-cb-payee`, the superblock strings); the not-synced leniency per pin; v24 `strict_multiplicity` with V24 on; `oracle_credit_pool_step` equals `getcreditpoolinfo` from 1,987,776 (the v24 oracle) · BG: — |
| O2b | **`diff_chain`** (rev1, M-7, F15, F04): a Core regtest chainstate linked into the shim (in-memory DBs, mocktime, compressed heights) vs `dashd-chainstate` (in-memory engine); generated operation sequences incl. failures, proposals, restarts, reorgs; per-step verdict, digest and effect comparison | M3 (non-evo) → M4 (evo) | Core branch `node/oracle-shim-*`; node-fuzz | O2a, i:CH3a | L | CON | D | **D O-h; impl O-h; rev Sol** | NFR-CEQ-04, -06, -08 | finds a planted DML ordering bug and a planted effect omission; ≥ 24 CPU-h (M3) and ≥ 72 CPU-h (M4) clean; the disposable-datadir proposal experiments recorded · BG: — |
| O3 | `node-fuzz` scaffolding, per-PR smoke runner, nightly farm (8 h × 16 cores outside bench windows), triage automation (tx/block-codec divergences routed to the private corpus by default, §7.5) | M0→ | node-fuzz | O1 | M | OTH | — | O-h | NFR-CEQ-04, NFR-SEC-02 | every target runs a per-PR smoke; divergences auto-minimized into regression vectors + STATUS tasks · BG: — |
| O4 *(running)* | Core vectors + transcribed embedded vectors + regenerated mnlistdiff/qrinfo fixtures (R4 V1, F12) | M0 | `test-vectors/`, dash-conformance::vectors, dash-spv fixtures | BS2 | M | CON | — | S-h (Opus review) | NFR-CEQ-01/02 | `PROVENANCE.md` per file; loaders match Core's harness semantics without its two bugs; the F12 test uses a Core-produced fixture · BG: — |
| O5 | Quirk-register scaffolding (34 R4 quirks + rev1's 35–38 with owners; quirk 1 owned by CS0, quirk 34 by BS5, 35/37 by BS11a-t, 36 by LQ2, 38 by LQ3) | M0 | pure crates, dash-conformance | — | S | CON | — | **impl S-h; rev Sol** | FR-CON-34 | 38 tests present; ignored count tracked in STATUS · BG: — |
| O6 | Chain scans (a)–(h) via the oracle (R4 V5); scan (g) by the CS0 owner or the manager (§7) | M0 | dash-conformance (`scan`) | BS2 | S | CON | — | **impl S-h; rev Sol** | NFR-CEQ-13 | results recorded (scan (g) handled per §7); scan (h) result recorded; quirk owners updated · BG: — |
| O7 | **DROPPED (DEC-82: v24 only).** **v23.1.8 mainnet reference** under the link-count rule (sealed files hardlinked, tails copied/reflinked, the oracle's files hashed unchanged) + StateDigest v1 spec (field classes) + golden extractor from the **v24** oracles + comparator (R4 V3) | M0–M1 | `/work/chains` reference instance; dash-conformance (`digest`) | BS2 | M | STATE | D (spec) | **DROPPED (DEC-82)** | NFR-CEQ-06; DEC-05 | the reference synced with every index; goldens for both networks reproducible (two runs identical); the sampled v23.1.8 cross-checks pass on history; comparator tests · BG: never during bench windows |
| O8 | `node/state-digest-v24.0` Core patch (`-statedigestlog` with field classes and failed-attempt records, `-recordstatedecisions`) + full-state digests; **decisions recorded on the live v24 oracles** (m-12). v1's v23.1 branch is dropped (M-8) | M2 | Core branch `node/state-digest-v24.0` | O7 | M | STATE | — | **impl O-m; rev Sol** | NFR-CEQ-06 | the patched oracle produces digests for mainnet and testnet; decision files from live operation · BG: — |
| O9a | Replay tool + digest comparator + first-mismatch dump (rev1 split, F17) | M2–M3 | dash-conformance (`replay`) | O7, i:CH1-node (SB0 rev6.1) | M | STATE | — | **impl O-m; rev Sol** | NFR-CEQ-06 | replays a recorded synthetic chain against goldens; mismatch dump names the field · BG: — |
| O9b | Segment snapshots + per-PR window selection + `-consensusxcheck` wiring | M3–M4 | dash-conformance, chainstate hooks | O9a, CH3a, O8 | M | STATE | — | **impl O-m; rev Sol** | NFR-CEQ-06 | snapshots at R4 §2.3.4 heights validated against the oracle; PR window run < 60 min · BG: — |
| O10 | L3 mutation engine (~45 operators incl. BLS encodings and per-profile AU) + GBT-proposal/submitblock/testmempoolaccept harness (cycle-base operators on disposable datadirs; profile rows against the v23.1.8 reference) + `bad-*` census + multi-failure precedence (R4 V7) · O17/DEC-37: paired witnesses for every `UngatedRules` row against both references; DKG-08 and CONS-09 with a fresh and a warmed reference | M3–M5 | dash-conformance (`mutate`) | O9a, MN0 (MP1a for txs) | **L** (was M; O17/DEC-37) | CON | — | **impl O-m; rev Sol** | NFR-CEQ-07 | operators per A1 §4.7; ≥ 1 case per Core `bad-*` string; ~500-mutation per-PR subset · BG: — |
| O11 | Functional-test compat runner per pin (`node/func-compat-v24.0`, `-v23.1`; DTF helper patches and mixed-cache handling, m-5), passlist ratchets, expected-failures files, FT-MAP, asserted-log extraction (R4 V6) · O17: each tag's tests under its profile; asserted-log changes (`[error]`, WAL-28, P2P-25); the v23.1 framework's `sporkupdate SPORK_3_INSTANTSEND_BLOCK_FILTERING 0` | M2 | Core branches; `node-program/compat/` | ND1, RP0 | M | OTH | — | **impl O-m; rev Sol** | NFR-CEQ-05; US-DEV-07 | per-node-index binary selection incl. DTF helpers; FT-MAP covers 266 tests (distinct script stems at rc.3; test_runner.py lists 328 script/argument variants) → story IDs; matrices per pin in STATUS · BG: — |
| O12a | Mixed-network harness, mode A (rdashd non-MN nodes in every DTF test) | M5 | dash-testkit, func-compat | O11, O16 | M | LLMQ | — | **impl O-m; rev Sol** | NFR-CEQ-09 | mode A passlist automated · BG: — |
| O12b | Mixed-network harness, mode B + alternating + devnet compose (topology, workload, chaos, metrics) (R4 V8); consider a separate VM for the soak (§5 SW-OPS) | M7 | dash-testkit, `contrib/node/devnet/` | O12a, AM1, DK1, SG1b, LK1c, QM1b | M | LLMQ | — | **impl O-m; rev Sol** | NFR-CEQ-09; REL-2.1/2.2 | the six Stage-A criteria automated; devnet up with 20 MN + 8 evo + wallets; chaos scripts; divergence paging. The soak run itself (M7 exit 2) also needs CJ1 and GS1 for its CoinJoin and governance workload · BG: — |
| O13a | **Shrunk (DEC-83): protocol-parity transcripts only, no identity rows.** (SB0 rev6.1) Unchanged for the node, behind P2b; the SPV handshake transcripts are SB16's. Fingerprint lab v0: handshake transcripts per profile (outbound and inbound) | M2 | dash-conformance, `contrib/node/fingerprint` | P2b | M | P2P | — | **impl O-m; rev Sol** | NFR-FP-01 | behaviours 1–5, 9, 10 diff-empty per profile · BG: — |
| O13b | **DROPPED (DEC-83).** Fingerprint lab v1: all behaviours + +A…+E (cross-peer ordering, scheduling under load, per-profile rows), KS timer tests incl. relay latencies (M-6), passive capture n-grams, node classifier · O17: every P2P `profile` row, incl. DKG-05/-12/-13, INTG-01, MPIX-16, P2P-17, SIG-14, SIG-18 | M5 | same | O13a, P6a, P6b, P5 | L | P2P | — | O-h | NFR-FP-01…04; REL-2.6 (node) | all behaviours diff-empty; KS p > 0.05; AUC ≤ 0.55 (node) · BG: — |
| O13c | **DROPPED (DEC-83).** Fingerprint lab v2: masternode viewpoint probes and classifier · O17: masternode-viewpoint probes per profile (DKG-03, -06, -09..-11, MNO-17..-20) | M7 | same | O13b, AM1 | **L** (was M; O17/DEC-37) | P2P | — | O-h | NFR-FP-02; REL-2.6 (MN) | behaviour 28 diff-empty; AUC ≤ 0.55 from an MN peer · BG: — |
| O14 | DIFF-RPC harness + `compat/ALLOWED-DIFFS.md` + Platform P-1 golden fixtures; pins UniValue's float formatting rule from the pinned source (m-8) · O17: fixtures for the RPC `profile` rows; `ALLOWED-DIFFS` seeded only with O17 §5.11's O14 list; the same `-deprecatedrpc` set on both pins | M3–M5 | dash-conformance (`rpcdiff`) | RP1a | M | OTH | — | **impl O-m; rev Sol** | FR-RPC-06; REL-5.6; PRD §5.3 | corpus from `dash-rpc-spec`; numbers compared textually; registry limited to 3 classes; P-1 fixtures from regtest (dashmate config) + testnet · BG: — |
| O15 | Shadow comparator (external; consumes `-statedigestlog`) + divergence tripwire wiring; the v23.1.8 reference as the mainnet peer · O17: a DKG-08 or CONS-25 divergence from the v23.1.8 reference is an rdashd bug, given the same process history since start on both sides (record restarts) | M4 (smoke) → M5 (REL-1.2) | dash-conformance (`shadow`), dashd-node | CH7b, P3, P5a, RP1a, RP2a | S | STATE | — | **impl O-m; rev Sol** | NFR-CEQ-10, NFR-SEC-08; US-NODE-22, US-DEV-10 | per-block comparison on mainnet + testnet; alarms → STATUS; a forced divergence stops MN signing · BG: — |
| O16 | `dash-testkit` (wraps dash-spv `test-utils`; `RustNode`, `MixedNetwork`, mocktime, `DASHD_IMPL=rdashd`) · (SB0 rev6.1) also runs the dash-spv suites *as* rdashd `-spv`, with SB18 | M2 | dash-testkit | ND1 | M | OTH | — | **impl O-m; rev Sol** | US-DEV-09; REL-5.7 | dash-spv suites runnable against rdashd; no code moved out of dash-spv · BG: — |
| O17 | **Profile rows collapse to v24 (DEC-82, RP-V24); the audit stays as history.** **PIN-DELTA audit + `ReleaseProfile` spec** (rev1, M-2): diff v23.1.8 ⇄ v24.0.x over R4's consensus-relevant files; classify every delta (gated / profile / irrelevant) with hunks; finish rows 23–24 (the governance hunks; the `fJustCheck` duplicate-commitment rule); the profile struct and per-call-site table; re-run at every re-pin (SW-V24) | M0 | `designs/O17-pin-delta.md`, `designs/O17-release-profile.md`; A1 §2.25 | — | M | CON | — | O-xh | DEC-05; NFR-FP-01; FR-CON-02 | every delta in the diff-stat accounted for; reviewed by a second Opus and by GPT (r2; DEC-37 applied); consumers listed per row (report §5; per task: spec §10) · BG: — |

### 4.8 W8: Wallet and import/export (A1 §2.20–2.21; DEC-02, DEC-11)

| ID | Task | MS | Crates | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| WL1 | key-wallet extensions: passphrase end to end, `MnemonicCompat::DashCore`, imported/watch-only/seedless accounts, encryption at rest (argon2id + XChaCha20), message signing | M5–M6 | key-wallet, key-wallet-manager | — | L | OTH **+SEC** | D | **D O-h; impl O-h; rev Sol→O-h** | FR-WAL-01…04, -15; US-WAL-02/06/10/15 | key-wallet tests incl. Core's BIP39 checksum-quirk vectors; FFI unchanged; G-SEC review · BG: dash-spv wallet benches |
| WL2a | `dashd-wallet` service: multiwallet, SQLite schema, events with relevance pre-filter + bridge, `WalletInterface` additions (#854), rescans bounded by birth height, lock and unlock, locator after `ChainStateFlushed` · (SB0 rev6.1) SB18b then puts `dashd-wallet` behind `WalletInterface` for rdashd `-spv` | M6 | dashd-wallet, key-wallet-manager | WL1, MP1a, BS14 | L | STATE | D (WL2a + b) | **D O-h; impl O-m; rev Sol→O-h** | FR-WAL-01…03, -07, -09, -13; US-WAL-01/04/08 | reorg rollback tests; wallet DB crash safety (ST3 model level); first half of tier 4 · BG: `rescan-mainnet` (window); `ibd-*` unaffected with the wallet off |
| WL2b | Core's 71 wallet RPCs with DEC-02 wallet-kind semantics | M6 | dashd-wallet | WL2a, RP1a | L | OTH | (WL2 D) | **impl S-h; rev Sol** | FR-WAL-05, -14; US-WAL-01…05/07/08/11/13/14 | DIFF-RPC wallet scenarios with identical seeds; the rest of tier 4; OW-11 · BG: `rpc-regtest` wallet calls |
| WL3 | Core wallet import (SQLite, BDB 4.8 reader, dumps, hdchain/cj_salt quirks; port `dw-compat`) + `rdash-wallet import-core`/`import-mnemonic` | M6 | dash-coreimport::wallet, rdash-wallet | WL2a | L | OTH **+SEC** (keys, untrusted BDB) | — | **impl O-h; rev Sol→O-h** | FR-IMP-03, -05, -06; US-IMP-03…06/13, US-WAL-11 | M6 exit 2 fixtures; the import report lists unmapped records; BDB reader fuzzing; G-SEC review · BG: — |
| WL4 | PSBT, multisig, descriptor engine, signing with all sighash types | M6 | key-wallet, dashd-wallet | WL2a | L | OTH **+SEC** | — | **impl O-h; rev Sol→O-h** | FR-WAL-06/08; US-WAL-09 | FT `rpc_psbt`, `rpc_createmultisig`/`wallet_multisig`; OW-21 · BG: — |
| WL5 | ProTx and governance wallet ops (every type incl. v3, shared 10–12, BLS operator, EdDSA), collateral lock, `masternode outputs`, `gobject prepare/submit/vote-many/vote-alias` | M6–M7 | key-wallet, dashd-wallet | WL2a, EV1 | L | OTH **+SEC** | — | **impl O-m; rev Sol→O-h** | FR-WAL-10/11; US-MN-01…03, -16…18, -21; US-SHR-01…06; US-GOV-02…04/08/09; US-PLAT-01 | FT `feature_dip3_*` wallet paths, `feature_masternode_shares`, `feature_masternode_payout_shares`, `feature_governance` wallet parts; OW-04…08 · BG: — |
| WL6 | CoinJoin client (`dash-coinjoin` client state machine, rounds, salt, `IsFullyMixed`, sessions, V24 rebalance, RPCs) | M6–M7 | dash-coinjoin, dashd-wallet | WL2a, CJ0 | L | OTH | — | **impl O-m; rev Sol** | FR-CJC-01…04, FR-WAL-12; US-CJ-01…07/09 | FT `rpc_coinjoin`; mixing with dashd MN coordinators; OW-13 · BG: — |
| WL7 | Remaining wallet RPCs and SHOULDs, `-createwalletbackups`, `-walletnotify`, external-signer stub | M6–M8 | dashd-wallet | WL2b | M | OTH | — | **impl S-h; rev Sol** | FR-WAL-13/14; US-WAL-12…14 | the remaining `wallet_*`; REL-3.2 wallet rows · BG: — |
| WL8 | **DROPPED (DEC-82: v24 only).** v23.1-profile wallet behaviours (O17, DEC-37): resubmission (WAL-31), PSBT signing under a mixing-only unlock (WAL-08), freed-input reconciliation (WAL-35), CoinJoin unconfirmable filter (WAL-36), rounds memo (WAL-37), CoinJoin pending locks (MNO-21), no fast rescan (WAL-28) | M6–M7 | dashd-wallet | WL2a, WL6 | M | OTH **+SEC** | — | **DROPPED (DEC-82)** | FR-WAL-05, FR-CJC-01…04 | v23.1.8 wallet functional tests under `-releaseprofile=23.1` (O11); O14 fixtures: abandon and requery, CoinJoin locks, mixing-unlock PSBT · BG: — |
| IM1a | Core-format **detection in loaders** needed by M5 functional tests: `peers.dat`, `anchors.dat`, `banlist.json`, `mempool.dat`, `fee_estimates.dat`, `settings.json` (rev1 split, F17) | M5 | owning crates' loaders + dash-coreimport | — | S | OTH **+SEC** (untrusted files) | — | **impl S-h; rev Sol→O-h** | FR-P2P-19, FR-MEM-07 | FT `feature_addrman`, `mempool_persist` with Core files; loader fuzzing · BG: — |
| IM1b | The rest of flat-file and net-state import (governance.dat, mncache.dat, netfulfilled.dat, sporks.dat, dash.conf, onion/I2P keys) | M6 | dash-coreimport + owning crates' loaders | IM1a | M | OTH **+SEC** | — | **impl S-h; rev Sol→O-h** | FR-IMP-04; US-IMP-07/08 | round-trip fixtures from the oracle datadirs · BG: — |
| IM2 | Export tools (linearize-style blocks; dashd-loadable `dumpwallet`/descriptors) + end-to-end `-importcoredatadir` migration | M6–M8 | rdash-wallet, rdashd, dash-coreimport | CH6a, WL3, IM1b | M | OTH | — | **impl S-h; rev Sol** | FR-IMP-05; US-NODE-18, US-IMP-09 | M6 exit 3 round trip · BG: — |

### 4.9 W9: Masternode overlay (A1 §2.15)

| ID | Task | MS | Crates | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| SP0 | **Spork manager** (rev1 split, M-10.1): store, verification and the signer model per `ReleaseProfile` (v23 multi-key incl. SPORK_3/9; v24 single key), mainnet hard-coded values, `getsporks` on connect (v24 dedup), `SporkView` · O17: mainnet sporks are hard-coded under both profiles (every spork 0 = active, SPORK_21 = 1); the v23 table and multi-key model serve O11; `sporks.dat` is import-only (MNO-04) | M4 | dashd-masternode::sporks | P2a-core (SB0 rev6.1), BS8, O17 | M | P2P | — | **impl O-m; rev Sol→O-h** | FR-SPK-01…03 | FT `feature_sporks` (both pins); live testnet and mainnet spork state equals the oracles' · BG: — |
| SP1 | Masternode sync (`SyncManager` tick) + netfulfilled + mncache meta (incl. the platform-ban-reset effect) | M5 | dashd-masternode | SP0, P4 | M | P2P | — | **impl O-m; rev Sol** | FR-P2P-13/15, FR-EVO-10/11; US-NODE-02/13, US-DEV-04 | behaviour 8 timing KS; `mnsync status` DIFF-RPC · BG: — |
| GV1a | `dash-governance` pure (hashes, validator, trigger format/parse, IsValid, best SB, payments limit, tallies, rate buffer) | M5 | dash-governance | EV2b | M | CON | — | **impl O-m; rev Sol→O-h** | FR-GOV-01, -02, -05, -06, -09 | L0 `proposals_valid/invalid`; L1 `diff_gov`, `diff_gov_vote` · BG: — |
| GV1b | Governance runtime (objects, votes, orphans, store, govsync handler, collateral via txindex, the trigger store = `ChainHelper` superblock back end; reorg handling of triggers, m-3) · O17 MNO-08: rejected-vote records per profile | M5 | dashd-governance | GV1a, IX1, P5a, i:P4 | L | CON | D | **D O-h; impl O-m; rev Sol→O-h** | FR-GOV-03/04/07/10, FR-CON-22/32; US-GOV-01/05/10 | FT `feature_governance_objects`, `p2p_govsync_bloom`, `feature_governance` (non-MN, mixed A); superblock synced/unsynced scenarios vs dashd (M5 exit 6) · BG: — |
| CJ0 | `dash-coinjoin` pure (shared validation, session state machines, rounds); `is_collateral_valid` as a trait implemented in `dashd-coinjoin` (ATMP test-accept, m-16) | M5 | dash-coinjoin | BS9b | M | CON | — | **impl O-m; rev Sol→O-h** | FR-CJS-02/05, FR-CJC-02/04 | L1 `diff_coinjoin`; DSTX relay rules with P6a (`p2p_dstx` relay part) · BG: — |
| AM1 | Active MN manager, MNAUTH (sign hash per A1 §1.4.1), platformban, MN conduct (FR-AMN; init per profile), refuse inbound until synced, operator-key hygiene · O17: provides `InitRules.mn_init` (P2P-35)  · CH7 §11.2 (DEC-531): on a masternode, defer AM1's own startup-tip handling until its `Init` equivalent has run (Core init.cpp:2493, :2569-2578) | M7 | dashd-masternode | P5a, SP0, EV2b, i:P4, i:CH7a | L | P2P **+SEC** (operator key) | — | **impl O-h; rev Sol→O-h** | FR-AMN-01…05, FR-P2P-14; US-MN-04/09, US-PLAT-07 | FT `rpc_mnauth`, `p2p_platform_ban` (mixed B), `feature_masternode_params*` (both pins); the MN reaches READY; key never logged and zeroized (test) · BG: MN idle CPU counters |
| QM1a | Quorum manager runtime (cache, building from mined commitments, vvec/sk-share store, cleanup, warm-up) | M7 | dashd-quorums | LQ3, CH7b | M | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-LLMQ-05/06 | quorums equal `quorum list/info` on regtest and testnet · BG: — |
| QM1b | Quorum data recovery (`qgetdata`/`qdata`, limits per profile), connection maintenance · O17: QGETDATA decoding, unregistered-type score and tracking (DKG-09..-11), observer pushes (DKG-05) and the startup hook (DKG-06) per profile | M7 | dashd-quorums | QM1a, P7 | **L** (was M; O17/DEC-37) | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-LLMQ-06; US-MN-12 | FT `feature_llmq_data_recovery`, `p2p_quorum_data` (mixed B) · BG: — |
| DK1 | DKG runtime (phase handlers, clock and sleeps, intake rules (v24, DEC-82), popping batches and applying scores (batched verification itself is LQ4a's, DEC-616); live `dkgsimerror` comparison vs dashd moved here from LQ4b, dkgdb, `dkgsimerror`, `dkgstatus/dkginfo`, mineable commitments; DKG messages on Core's ticks) · O17: DKG intake (DKG-03), observer pushes (DKG-05) and the startup hook (DKG-06), v24 · LQ4 design §3.4 contract: members accesses at Core's points (§3.4.1), null session before Init, GETDATA phase windows, fully connected nodes only in ConnSnapshot, dkgdb fail-stop | M7 | dashd-quorums | LQ4b-1, QM1a, P7 | L | LLMQ | D | **D O-h; impl O-h; rev Sol→O-h** | FR-LLMQ-03; US-MN-11 | FT `feature_llmq_dkgerrors`, `feature_llmq_simplepose`, `feature_llmq_rotation`, `feature_llmq_dkg_intake` (mixed B, alternating, all-Rust; v24 pin); every Rust member valid · BG: `mn-dkg-regtest` `dkg_round_s` ≤ dashd (window) |
| SG1a | Signing manager + recsig DB + conflict/vote rules + EHF signer · O17 SIG-18: non-masternode recsig relay per profile | M7 | dashd-quorums | QM1a | M | LLMQ | D (SG1a + b) | **D O-h; impl O-m; rev Sol→O-h** | FR-LLMQ-08/10; US-MN-19 | FT `feature_mnehf`, `feature_llmq_signing` (part) · BG: — |
| SG1b | Sig-shares manager (sessions, batching, recovery members, backoff, spork 21); **outputs on Core's ticks** (100 ms send/cleanup, 10 ms dispatcher, ≤ 32/≤ 4; A1 §2.15) | M7 | dashd-quorums | SG1a, LQ5 | L | LLMQ | (SG1 D) | **impl O-h; rev Sol→O-h** | FR-LLMQ-09 | FT `feature_llmq_signing` (mixed); behaviour 28 cadence KS; recsig relay latency KS · BG: `mn-recsig-regtest` (window) |
| LK1a | ChainLock node role: clsig handler, best CL, enforcement (Core's 0 s / 5 s schedule), `ChainHelper` CL back end, coinbase-CL processing per profile, `-chainlocknotify`; **owns the CL-conflict scenarios** moved from CH7b · (SB0 rev6.1) CL/IS verification comes from `dash-evo::locks` (SB8); the node role is unchanged Also closes P5a's four out-of-IBD clsig differences (relay, the +10 score, notfound on getdata, absence from the BIP35 mempool reply; DEC-329/DEC-332). | M4 | dashd-locks | LQ3, CH7b, P5a, SP0, SB8 | M | LLMQ | — | **impl O-h; rev Sol→O-h** | FR-CL-01/03/04/05, FR-CHS-05 | live CL following (M4 exit 4); CL-conflict scenarios; FT `feature_llmq_chainlocks` (mixed A, as a non-MN) · BG: — |
| LK1b | InstantSend node role: manager, DB, verification windows, conflicts (IS gate per profile), the IS-removal effect, mempool hooks, `ChainHelper` IS back end, **isdlock relay on the worker tick** · O17 SIG-14: batch handling of a missing quorum per profile | M5 | dashd-locks | LQ3, MP1a, P6a | M | LLMQ | — | **impl O-h; rev Sol→O-h** | FR-IS-01, -03…06, FR-MEM-09 | FT `p2p_instantsend` (mixed A), the non-MN parts of `feature_llmq_is_cl_conflicts`; NFR-CEQ-08 IS scenarios (M5 exit 6); reorg handling of the IS mined index, archive and recsig truncation, idempotent under re-delivery (A1 §2.9 m-3, §3.5.7); isdlock relay latency KS · BG: — |
| LK1c | CL and IS signers (signing conditions per profile, retroactive signing, mining interplay) | M7 | dashd-locks | SG1a, LK1a, LK1b | M | LLMQ | — | **impl O-m; rev Sol→O-h** | FR-CL-02, FR-IS-02; US-MN-14 | FT `feature_llmq_chainlocks`, `feature_llmq_is_retroactive`, `feature_llmq_is_cl_conflicts` (mixed B) · BG: `mn-islock-regtest`, `mn-chainlock-regtest` (window) |
| GS1 | Governance signer (trigger creation and voting) | M7 | dashd-governance | GV1b, AM1 | M | CON | — | **impl O-m; rev Sol→O-h** | FR-GOV-08; US-MN-15, US-GOV-07 | FT `feature_governance`, `feature_governance_cl` (mixed B); a superblock is paid from a Rust-created trigger on devnet · BG: — |
| CJ1 | CoinJoin server runtime, DSTX manager, DSQ modes · O17: DSTX check order (INTG-01) and four server admission switches (MNO-17..-20) per profile | M7 | dashd-coinjoin | CJ0, AM1, MP1a | L | P2P | — | **impl O-h; rev Sol→O-h** | FR-CJS-01…05; US-MN-13, US-CJS-01…04/06 | FT `p2p_dstx` (mixed B); dashd client ↔ Rust server and Rust client ↔ dashd server sessions; reorg handling of the DSTX confirmed height (A1 §2.9 m-3) · BG: `mn-coinjoin-regtest` (window) |
| PL1 | Platform integration: frozen P-1 fixtures, dashmate end to end with rdashd as Core, whitelisted RPC users | M7 | tests, `contrib/node/dashmate` | RP2b, RP3b, ZQ1 | M | OTH | — | **impl S-h; rev Sol** | US-PLAT-02/08/09; REL-5.8 | a dashmate local network with rdashd passes Platform's e2e; P-1 fixtures · BG: — |

### 4.10 W10: Performance (standing; A1 §3.11)

| ID | Task | MS | Scope | Deps | Size | Risk | Design | Model | PRD | Acceptance |
|---|---|---|---|---|---|---|---|---|---|---|
| PF0 | **Deterministic-counter gate** (rev1, M-11): `perf stat` instructions/syscalls (PMU in the VM with `perf_event_paranoid ≤ 2`), a counting allocator, `/proc/<pid>/io`, cachegrind/`iai-callgrind` fallback; fixed workloads; noise floors; the per-PR comparison against the previous head; dashd baselines on the same workloads | M0 | `node-bench`, `contrib/node/perfgate/`, CI | B1 | M | OTH | — | O-h | NFR-PERF-02(g), REL-6.4 | 5-run noise floors recorded; a planted 1% instruction regression is caught; runs in < 20 min per PR · — |
| PF1 | Profile dashd 23.1.8 and 24.0.x (B1 `-debug=bench` parse, perf/flamegraphs): serial fractions, the removable-cost budget per §3.9 row; then rdashd at each milestone | M0–M3 | `research/PF1-*.md` | B1 | M | OTH | — | **impl S-h; rev Sol** | NFR-PERF-01 | the Amdahl budget per metric published **before** CH4, CS6 and the payee queue are merged · — |
| PF2–PF6 | as v1 (allocator, IBD tuning, mempool/GBT, RSS/disk/resources incl. the 4-vCPU profile, BLS/LLMQ latency) | M3–M8 | — | PF1 | — | OTH | — | **impl S-h; rev Sol** | NFR-PERF, NFR-RES | per §5 SW-PERF |

### 4.11 W11: Release and operations

| ID | Task | MS | Scope | Deps | Size | Risk | Design | Model | PRD | Acceptance |
|---|---|---|---|---|---|---|---|---|---|---|
| RL1 | Packaging: signed tier-1 artifacts, systemd units, upgrade-in-place across two schema versions, downgrade refusal, tier-2 build smoke pack | M5–M8 | `contrib/node/release` | — | M | OTH **+SEC** (signing) | — | **impl O-m; rev Sol→O-h** | NFR-SEC-06, NFR-PORT-01, NFR-OPS-03; US-NODE-20/21 | reproducible signed artifacts; upgrade and downgrade-refusal tests |
| RL2 | Operator docs: install, configure, migrate both ways, run an MN, the registry, limitations | M8 | docs | — | M | OTH | — | **impl S-h; rev Sol** | NFR-OPS-01…04 | docs exercised against the OW-* scripts; crash safety (NFR-OPS-01) is proven by ST3's harness, log compatibility (NFR-OPS-02) by ND1 |
| RL3 | Internal security review (P2P, consensus, RPC, wallet; `security-review` skill; multiple reviewers) | M7 | all crates | M7 content | M | OTH | — | **impl review task; rev Sol xh → O-h** | NFR-SEC-01…09 | findings fixed or explicitly accepted; report in `reviews/` |
| RL4 | Testnet MN fleet (≥ 4 MNs + 1 evonode via dashmate), the 30-day soak, the hot-swap drill | M7 | operations | the O12b devnet soak green (with CJ1, GS1); the M5 services (M5 exits 1–5); §10 Q2 | L (30-day calendar) | LLMQ | — | **impl O-m; rev Sol** | REL-2.3/2.4; US-MN-20 | M7 exit 3 and 4 |
| RL5 | Mainnet pilot (≥ 1 MN, 14 days) | M8 | operations | RL3, RL4; §10 Q1; mainnet on v24 (DEC-82) | M | LLMQ | — | **impl S-h; rev Sol** | REL-2.5 | M8 exit 1 |
| RL6 | Benchmark report vs 2×, with explanations (wall clock from the dedicated host or reserved windows; PF0 history attached) | M8 | `bench/` | PF* | M | OTH | — | **impl S-h; rev Sol** | REL-6 | M8 exit 5 |
| RL7 | Traceability: `compat/STORIES.md` (168 stories → tests per surface) and `compat/OPERATOR-WORKFLOWS.md` scripts (OW-01…22) | M5–M8 | `node-program/compat` | O11, O14 | M | OTH | — | **impl S-h; rev Sol** | REL-3.1, REL-4.1 | every row green at M8 |

### 4.12 SB: shared SPV/node base (SB0 rev6.1, DEC-119)

The rows of SB0 §13.1 in this section's format, with SB0's sizes, risk classes, edges and DEC-92 tiers. How to read them:
- References are SB0's unless named otherwise: "SB0 §x", "SB0 Q1"–"Q8" (SB0 §15.1's questions, not §10's), and DEC-112's O1–O6, m2–m7 and n2. "addendum §x" is the EV addendum (rev5 726ff9a1).
- **L1–L9 here are SB0 §2.3's layer rules** (over SB0 §2.1's layers B, P, Pn, S, V, N, W, A, T), not §0's test levels; L0 stays the vector level. F1–F11 and CH2 P8 are CH2's `SubscriberFeed` contract and properties (SB0 §6.2–§6.3). R1–R8 are P2a §2.5.3's snapshot requirements. The DEC-105, DEC-107 and DEC-115 items and R4-M1, R6-M1…M3 and R7-O1 are SB0 review findings (SB0 §16).
- The EV addendum notes on SB8, SB11 and SB19 apply once the addendum is approved.
- SB-ST is contingent: it runs only if SQLite is refused (SB0 §9.6, Q2). SB18b is at M6, after WL2a.
- External gates, outside the CPM (§3): SB14 needs #1104 merged or carried, and SB16 needs #902 merged or SB0 Q1's answer.
- Start order: §8.3 (Wave 2) and SB0 §13.3.

| ID | Task | MS | Crates/repo | Deps | Size | Risk | Design | Model | PRD | Acceptance (tests · BG) |
|---|---|---|---|---|---|---|---|---|---|---|
| SB1 | **Layer, purity and mobile gates** (L1–L9 tooling): `layers.toml` + `check-layers.py` (two feature graphs for `allow_w`), `shared-clippy.toml` + `check-shared.sh`, `mobile-deny.toml` + `check-mobile.sh`, size gate with `contrib/mobile/android-env.sh`; the ratcheted `layers-baseline.toml` (SB0 §2.3); removes R2's enforcement; CI groups | M1 | `contrib/node`, `contrib/mobile`, CI | — | S | OTH | — (SB0 §2.3) | **impl O-m; rev Sol** | DEC-81; NFR-CMP-03 | every rule green on integration against the baseline list; each planted failure of SB0 §2.3 fails its rule (direct and transitive normal/build N→V, V→N, V→Pn; an unlisted wrapper crate; hot-path W reachability; `thread::spawn` or `spawn_blocking` in `dash-chain`; +300 KiB); the W→W chain passes; a stale baseline entry fails · BG: L7 baseline recorded |
| SB2 | **Mobile release profile**: `[profile.mobile]` (fat LTO, 1 CGU, `opt-level = "s"`; `opt-level = 3` on the linked hashing paths: `dash-pow` until BS15's switch, `dash-x11`, `dash-sha256`, `dashcore_hashes`, `blst`, and the C backends' `-O3`); L7 baseline re-recorded | M1 | workspace `Cargo.toml`, `dash-spv-ffi`, `key-wallet-ffi` | SB1 | S | OTH | — | **impl S-h; rev Sol** | NFR-CMP-03 | builds for aarch64-android; the amended profile's stripped size is measured and recorded (target ≤ 9.0 MB) · BG: `spv-headers-*` instruction counts (QEMU aarch64) not higher than the release profile |
| SB3 | **dash-spv hygiene**: `dash-spv-cli` (clap out of the library), `log-file` feature, `env-filter` optional, `tokio` features trimmed; closes the L6 entries of `layers-baseline.toml` | M1–M2 | dash-spv, dash-spv-ffi | SB1 | S | OTH | — | **impl S-h; rev Sol** | NFR-CMP-01, -03 | L6 green for dash-spv-ffi defaults with no L6 baseline entry left; `spv`/`ffi` groups and dashd suites green · BG: L7 delta reported |
| SB4 | **`dash-chain` core** (CH1's shared half): arena, concurrent index, `accept` with every Core header rule incl. CL conflict and the v24 `NodeClock` time check, best header with Core's update rules and V's hooks, SPV's fork-choice target (SB0 §4.1, DEC-112 O1), `ChainReadSnapshot` R1–R8 + `tip_seq`, `IndexAccess`, `AnchoredAncestry` + the rule-sized `RuleContext` (SB0 §4.1.1), `BlockIndexRecord` codec, `take_dirty`, `load` with PoW check, anchor (absolute work, shipped pack, post-DGW checkpoints, candidate rule and the fallback candidate, SB0 §4.3.1 steps 1–2), `CheckpointView` (the node's is Core's chainparams list, DEC-112 n2) | M2 | dash-chain | CS0, i:CS2, i:CS3, i:BS5 | L | STATE | **D** (SB4 brief from SB0 §4) | **D O-h; impl O-h; rev Sol→O-h** | FR-CHS-01, -02, -06; FR-CON-17, -18 | Core L0 header vectors reject with Core's strings from genesis and from an anchor; `time-too-new` with ±3,600 s peer offsets and `setmocktime` (SB0 §4.2); full mainnet/testnet headers from oracle files, from genesis, windowed and unwindowed: chain work and `getchaintips` equal; **genesis-origin contexts (SB0 §4.1.1, DEC-105 F2)**: the BTC retarget at next height 576 (ancestor 0), the KGW boundary (a read beyond 25 entries; then the KGW → DGW switch at 34,140) and testnet's min-difficulty walk-back, each unwindowed, windowed at w = 10,000 and windowed at a test w below the KGW reach (cold loads), with equal verdicts; **the maximum-length KGW walk (SB0 §4.1.1, DEC-107 M1)**: parent 19,999 at uniform 150 s spacing (times 1,000,000 + 150·h, bits `0x1e010000`), 4,032 contributing entries down to 15,968 and the exit guard's read of 15,967, with the guard read resident and, at a test w, cold; the target equals complete ancestry's, and a 4,032-entry context panics in debug (negative control); the post-DGW 25-entry anchor control; a branch forking below the anchor (`BelowAnchor`); the candidate cases of SB0 §4.3.1, and no API moves an `Established` anchor; R1–R8 sim tests (P2a §2.5.3); seqlock coherence stress; **the best-header vectors (SB0 §4.1, DEC-112 O1)**, each comparing `getblockchaininfo.headers` and the first `getheaders` locator entry with the node oracle: a ChainLock on the active a1000 while header-only b1005 has more work (Core keeps b1005), and a higher-work header stored with `CONFLICT` followed by a restart (the load recompute picks it); the `InvalidateBlock`, `MarkConflictingBlock` and `InvalidChainFound` walk-backs; a first-seen tie; SPV's `fork_target` in the same cases (a1000 after the lock, never a failed or conflicting header); **fork-target eligibility (SB0 §4.1, DEC-115 R6-M1)**: Sol r6's three branches from height 80 (A100, B110 active, C105 stale) with a lock on A100 give A100, never C105; a lock on the already-active block, with nothing marked, keeps a heavier branch forking below it from becoming the target; a property test that every published SPV tip descends from `cl_floor`; the node's checkpoint rule over Core's list (DEC-112 n2) · BG: PF0 `chain_accept_headers` ≤ Core's `ProcessNewBlockHeaders`; node test: a ChainLock on a header-only block, and an enforcement reorg, leave the node tip trailing the floor with no `ChainError`, matched against the oracle (R7-O1) |
| SB5 | **`dash-chain` feeds and residency**: `TipCell` (F1–F3, F4 producer part, F5 + producer wakes, F6, F8 producer part, F10 with inactive registration, F11 with generational ids), the `SubscriberFeed` reader with staging, `FeedSource::subscribe_at`, `Needs`/`Refused` (CH2 P8), `ChainEvent` and a separate `StatusEvent` feed, resident window, cold records with stable ids and skip ids, snapshot heights by ancestry from the snapshot's own tip (SB0 §4.4), `HeaderStore` | M2–M3 | dash-chain | SB4 | M | STATE | (SB4 D) | **impl O-h; rev Sol→O-h** | FR-CHS-02; DEC-81 | CH2 §4.4's feed properties (X17–X24 analogues) plus SB0 §6.1's ports: X28, F11 with pending fanout and slot reuse, compression within capacity, space and deregistration wakes; `subscribe_at` linearization stress (T35 analogue); windowed ≡ unwindowed over SB0 §4.4's scenarios (stale branches below the floor, restart, lagging readers, >10,000-block reorg without a CL, cold-read error); **a lagging reader holding a snapshot across a reorg deeper than w (DEC-105 F3)**: its resident and cold heights all lie on the snapshot tip's branch while live queries answer the new one; **eviction after commit (DEC-112 m2)**: with w below a batch, no entry leaves the window before its record commits, and no cold read trips the latch on a healthy store; **R6-M1 across restart and cold records**: the three-branch case reopened after the lock, and again with a w below the branches' depth so that the eligibility walk reads cold records, gives A100 · BG: PF0 `feed_publish` |
| SB6 | **`dash-p2p` core** (replaces P2a's network half): connection table, per-peer tasks and queues, unconditional `push` with Core's retained-memory charge and pause state (SB0 §3.1), P1, handshake functions, gates, decode policy, ping, addr intake, AddrMan, BanList, sans-io dial steps, seeds through `Resolver`, `NetStore`, `inbound` | M2 | dash-p2p | P1, O17 | L | P2P | — (P2a §2.2–2.3 and P2a §3.2–3.9; SB0 §3) | **impl O-h; rev Sol→O-h** | FR-P2P-03…07 | `diff_addrman`; handshake transcripts through a minimal driver; SB0 §3.1's send tests (a handler crossing `-maxsendbuffer`, concurrent overlay pushes, partial V1/V2 writes at Core's byte count); sim at 125 peers; gate and decoder fuzz; builds for aarch64-android under L5/L6 · BG: — |
| SB7 | **Shared SML core** in `dash-evo` (EV4-core): SML entry and hash, roots, CbTx partial-merkle check, coinbase-CL predicate; `dash/src/sml` verification moved, with dashcore adapter functions at the old signatures (SB0 §5.5, DEC-112 O2) | M2–M3 | dash-evo, dash | EV0 | M | CON | — (EV4 row; SB0 §5) | **impl O-h; rev Sol→O-h** | FR-EVO-07; FR-CON-29 | L0 `evo_simplifiedmns_tests`; CbTx MN root equal over all history; dashcore's existing sml tests through the adapters · BG: crit incremental vs full SML root |
| SB8 | **Shared lock and commitment verification**: `dash-evo::locks` (request id, sign hash, signer selection over the full authenticated `QuorumScanView` with each entry's status, then the `Verified` check on the selected commitment only (SB0 §5.2 step 6), BS12 serial BLS); Core's `CFinalCommitment::Verify` with signatures; `impl MnListView for SmlList` as SPV's `membersSig` input (SB0 §5.1) | M3 | dash-evo | SB7, LQ1a, BS12 | M | LLMQ | — (LQ1 §2.2.4; SB0 §5) | **impl O-h; rev Sol→O-h** | FR-CL-01; FR-LLMQ-02 | L0 CL/IS vectors; commitment verification vectors; LQ1 fixtures under both views; the SB0 §5.1 mutation gate: a changed signing-key multiset → `Unverified`; **a signer permutation** and a non-signer-only change → verdict unchanged, and no member list exposed (API test) (DEC-105 f1); missing-property (collateral, owner key) and rotation (wrong snapshot row) cases; **the SB0 §5.2 step 6 selection gate (DEC-105 F4)**: non-rotated, an `Unverified` Core winner → unavailable (never the best-scoring `Verified` one), and an `Unverified` non-winner → the winner verifies as in Core; rotated, the newest commitment at the signer's index `Unverified` with an older `Verified` one at that index → unavailable; locks verify only against the selected commitment's key; **Sol r3's case (SB0 §5.2.1, DEC-107 M4)**: the sets at 2,500,147 and 2,500,148 around a CL-type mine at 2,500,148, with a constructed scoring winner that the mine replaces, select as Core does on each set; **EV addendum rev5 (m3; applies on approval):** SB8 gates on verifier-only tests and LQ1's non-rotated fixtures under both views, and the rotated fixtures and the wrong-snapshot-row cases move to SB11 · BG: — |
| SB9 | **`dash-filters`**: GCS codec, Dash element set, filter-header chain check; `dash/src/bip158.rs` moved, with dashcore adapter functions at the old signatures (DEC-112 O2; CV5's filter half) | M3 | dash-filters, dash | CS0 | M | P2P | — | **impl S-h; rev Sol** | FR-IDX-03; US-LCS-01 | L0 `blockfilters.json`; oracle filters for ProTx/asset-lock blocks; dashcore's bip158 tests through the adapters · BG: — |
| SB10 | **`dash-kv` split + `dash-kv-sqlite`**: move the contract and latch types; SQLite engine per SB0 §9.4 (pinned snapshots, read pool, one writer); SPV-store `Sut` in ST3a; `sqlite_wal_prefix.rs` | M3 | dash-kv, dash-kv-sqlite, dashd-store | ST3a, CH2a | M | STATE | — (SB0 §9.4) | **impl O-h; rev Sol→O-h** | FR-STO-03; NFR-CMP-03 | the engine contract suite on both engines, incl. SB0 §9.4's snapshot interleaving and its negative control; ST3a explore/sweep on the SQLite `Sut`; the prefix regression; `dashd-store` tests unchanged after the move · BG: — |
| SB11 | **`dash-mnlist`**: engine per SB0 §5.3, trust chain steps 1–7 with `Skipped` types (by configured use), L_est from `LlmqConfig`, `coverage` of the tip's and the horizon's oldest authenticated sets at initial sync and after each new commitment, with the later-miss diagnostic (SB0 §4.3.1), the recent-lock horizon H_keep with retained and fetched sets, clipped to its authenticated domain at genesis and A_q (SB0 §5.2.1), retention of lists and quorum rows with `lookup_quorum`, W_P, the two-target fetch for the served types and the lifetime invariant with its params test and config refusal (SB0 §5.3, DEC-112 O3, DEC-115, DEC-116), the canonical scan and select-then-`Verified` (SB0 §5.2 step 6), ancestry through `AnchoredAncestry`, `take_dirty` rows, shadow mode API, `LlmqConfig` per client, with LQ1's `LlmqCtx` built per call; **EV addendum rev5 (applies on approval):** `MnViewError`, the addendum §2.3's SPV mapping, the rotated `members_uncached` | M4 | dash-mnlist | SB5, SB8, SB10, LQ1b | L | LLMQ | — (SB0 §5) | **impl O-h; rev Sol→O-h** | FR-LLMQ-01, -02, -05; DEC-81 | the 11 rotated captures; regtest-mn and testnet captures through steps 1–7; a mutated diff/qrinfo/CL is rejected at its step; a below-floor read is `Unavailable`; **SB0 §4.3.1's coverage cases (DEC-105 F1)**: repeated null rounds of one rotated index (Sol r2's witness), a very old surviving index with an unknown base, non-rotated A successes with null gaps, each against its regular-DKG control; a `Skipped` type's history not counted; a later miss (a reorg that restores an older commitment): `Unavailable` and the diagnostic, with the anchor and every row unchanged; **the H_keep gates (SB0 §5.2.1, DEC-107 M4)**: Sol r3's 2,500,148 / 2,500,155 case from a retained set and after one fetch, a delayed CL immediately before and after a mine, an ISLock across a cycle boundary, a fresh start on the new side of each boundary, outside the horizon unavailable with no request, a failed fetch unavailable, one fetch for many locks at the same block; **bootstrap from genesis (Sol r4 R4-M1)**, with regtest and devnet parameters: tips 0, 7, 8 and 20 with no request, A_q + 7 / A_q + 8, tip 450 requesting 432 and 442 only, A_q + H_keep ± 1, the first commitment mined, a DIP8-after-DIP3 regtest gap, and devnet's first rotated cycles; **retention and Platform's lookup (SB0 §5.3, DEC-112 O3)**: a list dropped once nothing needs it and the tip list kept; quorum rows pruned at tip − R inside `dash-mnlist`'s `WriteSet`, never by clearing a table; **after pruning and a restart (R6-M3)**, for a served non-rotated commitment replaced at r with every verification input available: h = r + W_P − 1 fetches the diffs to h − W_P and h and is found, and h = r + W_P is not found; the same answers from a row before pruning and from the LRU; two lookups sharing a target fetch it once; a lower target below the anchor; a non-served type after pruning returns not-found with no request (DEC-116); the params test over every network's served type in the minimum-lifetime form, a devnet configuration that breaks it refused at load, and stock regtest and devnet loading; a failed fetch, a need below the anchor and h above the tip return `Unavailable` (DEC-444, DEC-447), and a failed holder input is re-asked once by the next `fetch_lookup` (BL-390, DEC-455); no lookup ever returns another quorum's key; **EV addendum rev5 (applies on approval):** addendum §2.4's SPV tests, and the rotated fixtures and wrong-snapshot-row cases moved from SB8 · BG: — |
| SB12a | **`dash-spv::bridge`** (SB0 §5.5, DEC-112 O2): the dashcore ↔ base-sdk and `dash-consensus` conversions for headers, blocks, cfheaders, cfilters, mnlistdiff, qrinfo, CLs and ISLocks; each header hashed once, inside `dash-consensus` by `Header::new`, its only constructor (SB12a D17, DEC-224 as amended: no carried hash crosses; dash-spv takes header and hash back from the result; X11 via `dash-x11`, D16); the dashcore shapes behind Platform's queries (`latest_masternode_list`, `get_quorum_at_height`) | M3 | dash-spv, dash-consensus | CS0, SB7, SB9 | S | CON | — (SB0 §5.5) | **impl O-h; rev Sol→O-h** | NFR-CMP-01; DEC-81 | round trips over captured mainnet and testnet messages of each kind, byte-equal; no header hashed twice (a counting X11 wrapper over a 2,000-header batch and a `headers2` batch); a mismatched hash fails the debug assertion; Platform's two queries return the same values as before over the same captures · BG: `spv-headers-*` not slower |
| SB12 | **dash-spv kernel rules on existing paths**: header, block and filter rules from `dash-consensus`/`dash-filters`, through `dash-spv::bridge` (SB12a); anchor windows in checkpoint data. Scope per PLAN §7 handling (CANDIDATE-SPV) | M3 | dash-spv | SB4, SB9, SB12a | M | CON | — | **impl O-h; review routing per PLAN §7 handling** | FR-CHS-02, -06; DEC-81 | Core rule vectors through dash-spv's headers path; dashd suites · BG: `spv-headers-*` not slower |
| SB13 | **dash-spv SQLite store + migrator v3** (SB0 §9.5): schema with full header records and the `active` projection, the single store writer and `WriteSet` assembly, with no wallet locator (SB0 §9.5, DEC-112 O4); the thread sources of SB0 §7 (DEC-112 O5): validation and wallet matching under `pool.install`, blst `no-threads` in dash-spv-ffi and its `check-mobile.sh` check, the FFI runtime's explicit `worker_threads` and `max_blocking_threads`, `spv-clippy.toml` | M4 | dash-spv, dash-spv-ffi, key-wallet-manager, `contrib/mobile` | SB10 | M | STATE | — | **impl O-h; rev Sol→O-h** | FR-STO-03; NFR-CMP-01 | migration of v2 stores (mainnet-sized fixture) with flat memory; crash points via the SPV `Sut` (between a header step and a subscriber commit), and in each database independently with a faulty wallet store (DEC-112 O4); the store half of R6-M2's witness: after power loss the durable floor and tip are A70 and A100, and no B record survives; the thread-count test (SB0 §7) over the old MN path, with dashcore's verifier and blst `no-threads` (DEC-112 O5); a planted `no-threads` in an N package's graph fails `check-mobile.sh`; dashd suites · BG: `spv-startup` |
| SB14 | **dash-spv on `dash-chain`**: headers manager as driver, managers as feed subscribers behind a SyncEvent adapter, reorgs without restart, `handle_fork` removed; the wallet's resume point and synced-hash check, with the additive `WalletInterface` methods in key-wallet-manager (SB0 §9.5, DEC-112 O4) | M4 | dash-spv, key-wallet-manager | SB5, SB12, SB13, #1104 merged or carried (DEC-112 m7) | L | STATE | — | **impl O-h; rev Sol→O-h** | FR-CHS-02; DEC-81 | fork tests without restart; `dashd_sync` restart/disconnect suites; FFI `dashd_sync`; **wallet progress (SB0 §9.5, DEC-112 O4)**: the wallet's synced hash on the active chain (nothing to do), on a stale branch (one `truncate_above` at the fork) and unknown after a lost Buffered suffix; **the Buffered-reorg witness (SB0 §9.5, DEC-115 R6-M2)**: durable A100 with the floor at A70, a Buffered reorg at 80 published as B110, the wallet durably at B110, power loss: `truncate_above(70)`, never 100, and with no durable floor `truncate_above(anchor)`; next to it, the straight-extension control (A101…A110 lost, the wallet at A110) takes the same rule, and its rescanned wallet equals one synced straight; a `WalletInterface` without the hash method logs once; crash points in each database independently; the thread-count test (SB0 §7) · BG: `spv-headers-*`, `spv-filters-*` not slower |
| SB15 | **dash-spv MN on `dash-mnlist`**: sync managers as adapters; persisted verified state; the initial-sync driver (headers-only `Candidate`, coverage, fallback, `Established`; SB0 §4.3.1 step 2, SB0 §9.5) and the horizon fetch path (SB0 §5.2.1); Platform's queries through the bridge with SB0 §5.3's lookup (DEC-112 O3); MN sync off (DEC-112 m3); dashcore's `quorum_validation` and `message_verification` features dropped (DEC-112 O5); LQ1b-F's in-crate test | M4 | dash-spv | SB11, SB14 | M | LLMQ | — | **impl O-h; rev Sol→O-h** | FR-LLMQ-02; DEC-81 | `dashd_masternode` suite; replay-free restart; the 11 rotated captures; anchor selection end to end on regtest and devnet (SB0 §4.3.1); **initial-sync fallback end to end (DEC-105 F1, DEC-107)**: forced DKG failures at startup, with no MN, lock or subscriber row written and no lock verified before coverage holds; a crash in `Candidate` before and after a fallback reopens with no such state and completes; **a later miss**: the reorg that restores a commitment below the anchor leaves the anchor, the lock rows and every subscriber locator unchanged, reports its locks unavailable and raises the diagnostic (log line and FFI event); SB0 §5.2.1's delayed-lock cases against regtest dashd; **regtest from genesis (Sol r4 R4-M1)**: `Established` at tip 20 with no MN request and at tip 450 with requests to 432 and 442 only, subscribers running before any quorum, `NoQuorum` until the first commitment and verification after it, and the domain sliding once the chain passes A_q + H_keep; **Platform's lookup end to end (SB0 §5.3, DEC-112 O3)**, for a quorum rotated out about a day earlier (≈ 576 blocks on mainnet parameters, beyond R) and after a restart: with network, the two-target fetch and found; on regtest's Platform type after pruning and a restart, h = r + W_P − 1 found and h = r + W_P not found (R6-M3); with no network, not-found and never another quorum's key; a lookup within W_P of a replacement answered from the rows with no network; **MN sync off (DEC-112 m3)**: `Established` at the candidate with CL and IS `NoQuorum`, and `start_from_height` used as the birth height above the sync floor; the thread-count test on the `dash-mnlist` path (SB0 §7) · BG: — |
| SB16 | **`SpvRouter` on `dash-p2p`** behind #902's facade, **extended** (SB0 §3.2.1; CV5's network half): `RequestLedger` with `RequestId`/`AttemptId`, bounded subscriptions, `cancel`, terminal `RequestFailed`, misbehaviour → discourage, AddrMan/bans in the store, `SpvDialPolicy`, L9 registry; `legacy-net` for one release | M4–M5 | dash-spv | SB6, SB14, #902 merged or SB0 Q1 | L | P2P | — (SB0 §3) | **impl O-h; rev Sol→O-h** | FR-P2P-05, -07; NFR-CMP-04 | dashd suites on both `N`; #902's tests migrated per SB0 §3.2.1; stalled subscriber, multi-topic fanout, retries then terminal failure, late responses after `cancel`; SPV handshake transcripts; peer-churn sim; the thread-count test (SB0 §7) · BG: `spv-*` cells not slower |
| SB17 | **FFI event translation + mobile cells**: `events.rs`; `spv-filters-*`, `spv-startup`, `spv-size`; emulator idle-RSS smoke (`agentbox-api36`); the measurements SB0 §8 labels pending; Mac iOS build and durability check in QA | M5 | dash-spv-ffi, node-bench | SB14, SB15, SB16 | M | OTH | — | **impl O-m; rev Sol** | NFR-CMP-01; REL-5.7 | `ffi-c-tests` header compile; a baseline-header C caller against the new library (SB0 §10.1); callback ABI unchanged; emulator smoke and thread count (SB0 §7); idle and synced RSS within SB0 §8.2's bounds under SB0 §5.3's retention (DEC-112 O3); the real-device SQLite write-rate cell, under SB0 Q6 and run by pasta (DEC-112 m6) · BG: the new cells recorded |
| SB18 | **rdashd `-spv`**: composition in `rdashd` with key-wallet (`WalletInterface` via key-wallet-manager; SB0 Q4, DEC-112 O6), RPC subset, the `-spv` InitErrors (CH2 P8), `spv` bench cell; full mode never opens `spv/` (no import, SB0 §4.7) | M5 | rdashd, dashd-node | SB15, SB16, ND1 | M | STATE | — | **impl O-m; rev Sol→O-h** | DEC-81; US-LCS-07 | dashd suites as rdashd `-spv` (O16); `-spv -txindex` refused; a full start over an `-spv` datadir syncs headers from genesis with no open under `spv/` · BG: rdashd `-spv` ≤ dash-spv binary on the same wallet and peers |
| SB18b | **`dashd-wallet` for `-spv`** (DEC-112 O6): an adapter in `rdashd` that puts `dashd-wallet` behind `WalletInterface` within L3's `allow_w`, replacing SB18's key-wallet for RPC parity | M6 | rdashd | SB18, WL2a | S | STATE | — | **impl O-m; rev Sol→O-h** | DEC-81; US-LCS-07 | SB18's suites as rdashd `-spv` with `dashd-wallet`; the wallet RPCs in WL2a's scope against the oracle · BG: — |
| SB19 | **mnxcheck node wiring**: shadow engine task, `getmnlistd`/`getqrinfo` cadence, comparison with `project_sml`, metric; **EV addendum rev5 §4.3 (applies on approval):** one V command (`OnV`) per comparison, `peek` on a hit, `ShadowRows` + `NoMemo` + a discarding sink on a miss, `NotComparable` after pruning | M4–M5 | dashd-quorums or dashd-node | SB11, EV4-serve, CH7a, CH7b (DEC-515; code after CH7a-3 and CH7b-4, acceptance after CH7b-3, DEC-517) | S | LLMQ | — | **impl O-m; rev Sol** | FR-EVO-07/08 | on by default in regtest/devnet/testnet CI; a planted projection bug is reported; addendum §4.3's tests · BG: — |
| SB-ST | **Contingent** (only if SQLite is refused): segment crash model, persisted hash index, step marker (SB0 §9.6) | M3 | dash-spv | ST3a | S | STATE | — | **impl O-m; rev Sol→O-h** | FR-STO-03 | ST3a explore/sweep on the segment `Sut` · BG: — |

---

## 5. Standing workstreams

These run continuously from the milestone shown. Each has an owner agent in STATUS.md and a weekly line in the status report.

| ID | Workstream | From | What it does | Cadence and outputs |
|---|---|---|---|---|
| **SW-PERF** (W10) | Profiling and performance | M0 | **PF0** the deterministic-counter gate on every PR (A1 §3.11). **PF1** profiles dashd 23.1.8 and 24.0.x **before** the costly optimisations (CH4's pipeline, CS6's caches, the payee queue) and sets each metric's Amdahl budget (F11); then rdashd at each milestone. **PF2** allocator decision before M5. **PF3** IBD pipeline, cache and flush tuning, including the freeze threshold under the single budget (M3–M4). **PF4** mempool/GBT (M5). **PF5** RSS, disk and resource bounds, including the aggregate memory budget and a 4-vCPU / 8 GB profile (M5–M8; NFR-RES-01…05, FR-STO-09). **PF6** BLS and LLMQ latency (M7). Every optimisation follows A1 §3.10 | per-PR counter report; nightly counter workloads; wall-clock scenarios only in reserved windows or on the dedicated host; per-metric trend table in STATUS (REL-6.4); the RL6 report |
| **SW-FUZZ** | Continuous differential and non-differential fuzzing | M0 (O3) | Keep every target running, `diff_chain` included. Triage divergences into regression vectors and tasks; tx/block-codec divergences go to the private corpus by default (§7.5). Grow corpora from BS2, captures and FT traffic. Keep the release budgets (≥ 24/72 CPU-h) | nightly 8 h × 16 cores outside bench windows; the open-divergence count in STATUS (must be 0 at each milestone) |
| **SW-FT** | Functional-test porting and passlist ratchet | M2 (O11) | Grow the v24.0 passlist tier by tier (A1 Appendix D). Keep `rust_expected_failures.txt` shrinking. Keep FT-MAP current. Keep the asserted-log list in sync with the pinned Core. Flake hunting (< 1% over 10 runs for REL-5.3) | pass count per tier in STATUS each week; a tier gate per milestone |
| **SW-UPSTREAM** | Upstream sync | M0 | §6: the weekly merge of rust-dashcore `dev`; the weekly rebase of `node/base-sdk`; rebases of the Core `node/*` branches when pins move | one sync PR per week per repo; a STATUS row per sync |
| **SW-V24** | Core change tracking, V24/`evo_shares`, re-pins (NFR-CEQ-11, FR-CON-02) | M0 | Weekly diff of Core's `v23.1.x` and `v24.0.x` release branches and `develop` for consensus- and behaviour-relevant files. Each change becomes a task with a risk class. On a new tag in the pinned v24 line, run the **re-pin procedure** (DEC-82: one pin, no `ReleaseProfile`): update the pin and re-run the diff of the old and new tag over the files **O17 PIN-DELTA** audited; re-extract goldens; rebase the oracle-shim, state-digest and func-compat branches; run a full replay; re-record the Platform P-1 fixtures; refresh the vectors | weekly `research/core-watch-<date>.md`; no unreviewed consensus or profile change in any pin at release (NFR-CEQ-11) |
| **SW-OPS** | agentbox scheduling, capacity and oracle health | M0 | Keep the BS2 oracles healthy and synced (DEC-82: no v23.1.8 reference). Run the capacity plan below. Reserve the wall-clock benchmark windows and pause every other job during them. Watch disk | the schedule and capacity table in STATUS; any variance alarm pauses the job that caused it |
| **SW-DESK** | Interface to the desktop D-track (dashwallet-desktop; FR-DSK-01…06) | M1 | Track what the desktop needs: V24a (v24 payloads for SPV; the production fix is upstream, §10 Q5), WL1, WL6/CJ0 (FR-CJC-04), WL5 (DEC-01), P6b (rdashd as a trusted SPV peer; US-WAL-19). **Nothing related to EMBARGO-1 ever enters dashwallet-desktop**, which is pushed to GitHub (DEC-13) | dependency rows in STATUS for both tracks |

**SW-OPS capacity plan** (Opus M-11):
- **The box.** agentbox has 32 vCPU (2 × 16), 157 GB RAM and one 2 TB disk, with 411 GB used on 2026-10-08. The load average was already ≈ 27–32 in Phase 0.
- **One-time:** set `kernel.perf_event_paranoid=2` for PF0's counters. This is a box setting; the manager confirms it.

| Load | vCPU | RAM | Disk | When |
|---|---|---|---|---|
| BS2 oracles (mainnet + testnet, v24) | 2–4 steady | 12–16 GB | ≈ 300 GB | always |
| v23.1.8 mainnet reference (O7) | 1–2 steady; 8 during its reindex | 6–8 GB | ≈ 150 GB (tails copied, sealed files linked) | always, from M0 |
| Implementers building (6–12 × `CARGO_BUILD_JOBS=12`) | bursts to 32 | 2–4 GB each | 20–40 GB `target/` each | continuous |
| Fuzz farm | 16 × 8 h | 16 GB | ≈ 20 GB corpora | nightly window |
| Replays (nightly testnet, weekly mainnet) | 8–16 | 16–24 GB | 20–40 GB | nightly / weekly, never in bench windows |
| Functional passlists (two pins) | 8–16 | 16 GB | tmp | nightly |
| ST3 device and VM crash runs | 4 | 8 GB | ≈ 50 GB images | nightly (device), weekly (VM) |
| Devnet soak (20 MN + 8 evonodes with Tenderdash/Drive) | 8–16 steady | 40–60 GB | ≈ 100 GB | ≥ 2 weeks from ≈ wk 41 |
| Wall-clock bench windows | exclusive | — | — | reserved (e.g. weekends), or the dedicated host (§10 Q4) |

**Reading the plan.**
- From ≈ wk 30, builds, fuzzing, replays, passlists and the devnet soak together exceed the box at peak.
- The manager schedules around this. For the soak, the manager asks pasta for **a separate VM** (§10 Q4) rather than squeezing it in.
- Wall-clock benchmark claims made while any of these loads run are not evidence.

---

## 6. Upstream sync

### 6.1 rust-dashcore `dev` → `node/integration`

- **Cadence.** Weekly (Monday). Also off-cycle when upstream merges something we carry locally (#1136–#1138, #264, #400, #1078) or a security fix.
- **Procedure** (manager, in `~/workspace/rdc-wt/integration`):
  1. `git fetch origin`, then `git merge --no-ff origin/dev` into `node/integration`. **Never rebase `node/integration`**: its history holds the `--no-ff` task merges and is the base of open task branches.
  2. Resolve conflicts per §6.2.
  3. Run the gate:
     - G0;
     - BS2 on the regtest sets and the testnet sample;
     - the dash-spv dashd suites;
     - `spv-headers-*`;
     - when `dash/`, `hashes/` or `key-wallet*` changed: the bridge round trip (BS14) and the segment-replay windows of the crates that use them;
     - when Cargo dependencies changed: `check-base-sdk-pins.sh` and `cargo deny`.
  4. Commit as `chore(node): merge upstream dev <short-sha>`. Record the sync in STATUS (upstream range, conflicts, gate result).
  5. Open task branches rebase onto the new head before their own merge (PROCESS.md).
- **If the gate fails** and the fix isn't trivial within one working day, **abort the merge** (leave `node/integration` at its last green head) and open an `adapt to upstream <sha>` task. `node/integration` is never left red.

### 6.2 Conflict policy

| Situation | Rule |
|---|---|
| Files only upstream changed | take upstream |
| New node crates and `node-program/` | cannot conflict (new directories) |
| Existing crates we changed additively (dashcore `bridge` feature, BS15/V24a/BS18, key-wallet extensions WL1/WL5, `WalletInterface` additions, `ci-groups.yml`, root `Cargo.toml`) | keep upstream's structure and re-apply our minimal additive change on top; if the result changes behaviour, the conflict resolution itself goes through review as a task |
| Upstream fixed the same thing differently | **upstream wins** and our change is dropped, unless BS2 or the conformance tests show upstream's version is wrong; then keep ours, add a test, and note it for pasta (upstreaming is pasta's call, DEC-09) |
| Upstream merged a PR we cherry-picked (#1136–#1138, #264, #400) | resolve to upstream's version; delete our cherry-picked commits' effects; re-run BS2 |
| `Cargo.lock` | regenerate with minimal updates (`cargo update -p` only for the conflicting packages), then the pin and deny checks |
| `dash-spv/src/network/` (#902) | we never touch it, so it never conflicts; if our change ever needs it, that's a design escalation |

### 6.3 base-sdk fork (`node/base-sdk`)

- **Cadence.** Weekly, after open BS task branches are merged or parked.
- **Procedure:**
  1. Tag the current head `node-base-sdk-YYYY-MM-DD`; pinned revisions stay reachable.
  2. Rebase `node/base-sdk` onto upstream `develop`.
  3. Run base-sdk `just test lint`.
  4. Run `contrib/node/bump-base-sdk.sh`, which opens a rust-dashcore re-pin PR (`chore(node): re-pin base-sdk <sha>`) running G0 + BS2 + the BS4/6/7/8/11 differential smoke.
  5. A breaking upstream API change becomes one adaptation PR.
- **Conflict rule:** upstream wins for upstream code. Our commits are re-shaped to stay upstream-shaped (A1 §1.6).
- `docs/node-program/BASE-SDK-FORK.md` is updated with each commit's status.
- **Post-merge push (DEC-239).** After every pin move or landing on `node/base-sdk`, push the branch fast-forward to fork `node-sdk-integration` and add or move the dated tag. Run no build first, but run the embargo grep first. Stop and escalate if it is not a fast-forward or the grep hits.

### 6.4 Dash Core `node/*` branches

- **Branches:**
  - `node/oracle-shim-v23.1` and `node/oracle-shim-v24.0`;
  - **`node/state-digest-v24.0` only**: rev1 drops the v23.1 backport (M-8);
  - `node/func-compat-v24.0` and `node/func-compat-v23.1`.
- **Rebasing:** they are rebased onto the pinned tags when a pin moves (SW-V24), never onto `develop`. `develop` gets a weekly *watch* build of the shim only (non-blocking) so drift is seen early.
- **Functional suites** run from the v24.0 tree (DEC-82: no v23.1 tree, no `-releaseprofile`).
- These branches are never pushed (DEC-09).

---

## 7. Embargo handling (EMBARGO-1)

EMBARGO-1 is an unfixed Dash Core security issue (DEC-14; DEC-09). Rev1 rewrites this section after Opus M-1 found that v1's codename described the issue and that its mechanics were git-tracked and had reached GitHub through agent memory.

### 7.1 Naming

- The codename **"EMBARGO-1"** is opaque, and so is every task name. Code, tests, briefs, commits, reviews, STATUS lines, T3 thread titles, delegated prompts, memory notes and test names use only the codename. Implementation briefs refer only to **"PLAN §7 handling"**.
- **No tracked text may associate EMBARGO-1 with a specific field, value, mechanism or consequence.**
- Where the scrub removed a passage, the replacement is "EMBARGO-1: see ~/private/embargo/EMBARGO-1.md". Stubs sit at section level, or in a document's header banner, never beside the specific field, so their position points at nothing.
- The quirk test is `quirk_01_embargo_1`, and the code marker is the comment `EMBARGO-1` with nothing else in it.

### 7.2 Who may read the private file

The file is `~/private/embargo/EMBARGO-1.md`. The directory is mode 700 and the file 600; the repro is in `EMBARGO-1-repro/`. Readers must be **Claude models**, and only in these roles:
1. the manager;
2. the implementer of **CS0**. That person also owns the private EMBARGO-1 deliverables: quirk 1's vectors, scan (g), the private L3 operator definition and the private corpus layout. Keeping these with one task keeps the reader set to one implementer;
3. the **two Claude (Opus) reviewers** of CS0's marked module;
4. a Claude implementer whose brief explicitly says "PLAN §7 handling: read access granted". The only planned case is the lockstep task when upstream fixes the issue (§7.8).

**Never:** cross-provider reviewers (GPT-6.1-Sol or any non-Claude model), any agent whose brief does not grant access, or the desktop track.

A brief that grants access says so in one line and gives the path; it never restates content. Other tasks that touch the area get only neutral API instructions:
- **CS1:** "encode every tx field through `primitives::canonical`";
- **every consensus task:** "port every condition and integer expression verbatim, including signedness and casts";
- **O3:** "route codec divergences to the private corpus".

Their correctness is checked by the private tests, which run in the gate.

### 7.3 Where the code lives

- The replication lives **only** in rust-dashcore `dash-consensus`, in one small module that the private file names. Other code reaches it through a neutral API.
- It is **not** in `node/base-sdk`, which stays upstream-shaped, and **not** in `dashcore`/dash-spv, whose SPV behaviour is unchanged during the embargo.
- Every related hunk, test, fuzz target and corpus loader carries the `EMBARGO-1` marker.

### 7.4 Review

- The marked module is reviewed by the two Claude reviewers with read access.
- Cross-provider reviewers review the rest of the PR from a diff the manager produces **excluding the marked module** (`git diff -- . ':(exclude)<module path>'`). They check behaviour through the private corpus only: the gate reports pass/fail of the `EMBARGO-1` tests and of the private-corpus fuzz replay, and reviewers see the results, never the inputs.
- Review briefs tell reviewers not to speculate about excluded code, and never include R4's private material or the private path.
- RL3, the internal security review, follows the same split.

### 7.5 Test data

- Vectors, corpora, scan results and the L3 operator's inputs live in `~/private/embargo/` (not git).
- Tests load them through `DASH_EMBARGO_DIR`. The agentbox gate asserts the variable is set and the corpus is non-empty, so the private tests always run here and never skip silently.
- Fuzz triage (O3) routes minimized divergences from the tx/block-codec targets to the private corpus **by default**. The manager releases non-embargo ones to git after checking them.

### 7.6 Channels

- **Never pushed:** `node/integration`, `node/base-sdk` and the Core `node/*` branches (DEC-09). dashwallet-desktop *is* pushed (DEC-13), so nothing related may enter that repository: no code, docs, commit text or issue.
- **Agent memory and agent-config.** Memory notes give only the codename and the private path; the current note already complies. The manager asks pasta to add an embargo-term denylist to agent-config's pre-commit hook. The term list itself lives in the private file, not in agent-config.
- **T3 thread titles, delegated prompts and review briefs:** codename only.
- **Benchmark reports, release notes, B1 reports and any future public artifact:** no mention beyond the codename, and they go through the §7.7 scrub.
- No agent web-searches or posts about the issue. GitHub comments are never posted for node work.

### 7.7 Upstreaming gate

Before any rust-dashcore, base-sdk or Dash Core content leaves the box, which happens only when pasta asks:
- `grep -r 'EMBARGO-1'` over the range, excluding marked hunks;
- a review of the commit messages in the range;
- pasta's explicit clearance (DEC-14).

The manager takes no outward action.

### 7.8 When upstream fixes it

A lockstep task (SW-V24) changes the rule **per pinned release** through `ReleaseProfile`: the old behaviour stays for pins that have it, and the new behaviour applies to pins that include the fix. The task re-runs the full replay and the private operator, and lifts the marker once pasta clears disclosure.

### 7.9 Residual exposure

This is recorded in the private file (§4 there) and in the A1-rev1 dispositions (M-1):
- `node/integration` history from the R4 commit on;
- the task-branch worktrees cut before the scrub;
- local session transcripts;
- agent-config history (commits 47121b2..ca37fd5);
- round 1 of A1r, which gave the GPT reviewer R4 with the original section.

Rewriting histories is pasta's call (§10).

---

## 8. Wave 1 (running), Wave 1b and Wave 2 (post-SB0)

### 8.1 Wave 1, running (do not re-plan)

| Task | State | Rev1 delta for the manager to pass on (no restart needed) |
|---|---|---|
| BS1 | merged 9494ed8e | — |
| BS2 | nearly done | none |
| B1 | running | add the dashd 24.0.x baseline (F11, NFR-PERF-02(d)); expose the workloads PF0 will count |
| BS3 | running | drop the `no_std` job; add the pure-crate `cargo deny` bans (X-1) |
| O1 | running | none; O2a extends it (rev1 entry points, coverage matrix) |
| O4 | running | none |
| ST1 | running | set `atomic_flush = true` on chainstate; keep §6a D1–D4; the LevelDB arm is optional (X-3); record that the chosen engine must also pass the tracked metrics (A1 §2.8); the crash harness is ST3a/b, not ST1 |
| V24a (= BS17's dashcore part) | running | the AU v2 stable txid differs per release (v24 unconditional, absent in v23.1.8): implement it as v24/testnet behaviour and document it; keep scope minimal (X-4); BS17 is dropped from the plan |

### 8.2 Wave 1b: start now (safe under the revised design)

A task qualifies for Wave 1b when the design inputs it needs are settled by A1 rev1 and none of its merge dependencies is far away. The list is in start priority, for when fewer slots are free than tasks. **★CP** marks the critical path.

| # | Task | ★CP | Why it is safe now | Model | First deliverable |
|---|---|---|---|---|---|
| 1 | **BS11a-t** BLS decode types (S) | ★CP | A1 §1.4.1 fixes the algorithm, the types and the per-field table; BS6a needs only the types | O-xh | `BlsEager`/`BlsLazy`/`SchemeSource` + basic-scheme wrapper tests |
| 2 | **BS4a** decode-domain shapes (M) | ★CP | DDE rows #1–#9 and the §2.14 policy table are settled; no BLS decisions | O-xh | `TxOut` i64, `CoreBool`, optional `version` fields, `ProtoCtx` |
| 3 | **O17** PIN-DELTA + `ReleaseProfile` spec (M) | near-CP (P2a, SP0, P5, LQ4a, AM1, MP1a read it) | read-only Core diff; A1 §2.25 is its starting table | O-xh | the classified delta report, finishing rows 23–24 |
| 4 | **CS0** node primitives + `core_serialize` (M, D) | ★CP (feeds EV0, CS3) | A1 §1.3 and §7 settle the canonical-bytes rule; it starts on BS3's wiring and adapts to BS4a | O-xh | the CS0 design; then `diff_tx_decode` clean on the BS2 corpus |
| 5 | **CS1** script interpreter + sighash (L, D) | near-CP (CH3a) | unchanged by review; independent of every BS task | O-xh | `script_tests.json` passing |
| 6 | **CS2** PoW + `fp` + subsidy + payments (L, D(fp)) | near-CP (CH1) | A1 §4.5 rev1 defines the gate (binary inspection, then the admissible domain); DGW work needs only O1 | O-xh | release-binary `pow` disassembly report; DGW exact |
| 7 | **O3** fuzz farm + triage (M) | verification | needed by BS4b and every L1 gate; triage routing defined in §7.5 | O-h | per-PR smoke runner on O1's first targets |
| 8 | **ST2** block files (M) | near-CP (CH2) | A1 §2.11 (fsync rules, the link-count rule) and §3.5 I1 are settled | O-h | byte-identical `regtest-40000` blk/rev |
| 9 | **ST3a** power-loss model harness (M, D) | near-CP (CH2) | A1 §3.5.8 defines it; it builds on ST1's merged trait and ST2's interface | O-xh | `FaultyEngine` catching a planted commit-before-fsync bug |
| 10 | **PF0** counter gate (M) | — (gates every PR) | A1 §3.11 defines it; the PMU works in the VM (measured) | O-h | per-PR counter report with noise floors |
| 11 | **O7** v23.1.8 reference + StateDigest spec (M, D) | verification | A1 §4.6 rev1 settles the golden source and the field classes; build it under the link-count rule | O-h | the reference syncing; the spec in review |
| 12 | **O2a** shim v2 + coverage matrix (L) | verification (BS6b, BS11a, BS12, EV1) | A1 §4.4 rev1 lists the entry points | O-xh | `oracle_bls_decode` + the matrix skeleton |

**Design-only starts** (design slots, not implementer slots; each design is reviewed before its implementation starts, PROCESS.md):

| Design | Kind | Inputs |
|---|---|---|
| CH2 | D2 (Fable + Opus) | A1 §3.4–§3.5, ST1's API |
| EV2a/b | D2 | A1 §2.5, §2.10.1 |
| P2a | D2 | A1 §2.14, §2.25 |
| CH3a/b | D | A1 §2.10 |
| CH7a/b | D | A1 §2.10.1–2 |
| LQ1a/b | D | A1 §2.5, §2.10.1 |
| O2b `diff_chain` | D | A1 §4.4 |
| MP1a/b | D | A1 §2.12 |
| BS11a | D | A1 §1.4.1; relic sources |

**Not yet** (wait for a merge, or for design approval):
- BS5, BS6a and BS7 wait for BS4a (BS6a also for BS11a-t).
- CS3 waits for CS0 and BS4a.
- EV0 waits for CS0 and BS6a types.
- CH1 can start its arena and actor once ST1 merges, against CS2/CS3/BS5 interfaces. It is the first slot to fill when one frees up.

**The critical chain to staff first and review first:** BS3 → BS4a → BS6a (with BS11a-t) → EV0 → EV2a → LQ1a → LQ1b → LQ2 → LQ3 → CH7b (§3).

### 8.3 Wave 2 (post-SB0)

SB0 is approved (DEC-119). This wave adds §4.12's rows, the reworked rows marked "(SB0 rev6.1)" in §4, and P1, which is not an SB0 row but has not started. The table is in start-priority order. A row **starts now** when every merge dependency is merged and every interface dependency's API is agreed, per STATUS on 2026-10-09. Slack is merge slack in weeks against the first milestone a delay would move (M2 exit, REL11, M5_exit or RL4), from `tools/cpm_plan.py` (§3, "SB0 recheck"). **★M2** marks zero slack against M2, and **★CP** zero slack on the REL11 chain. Tiers follow DEC-92 (§0). This list supersedes §8.2's "Not yet" notes for CH1 and EV0.

| # | Task (size) | Starts | Slack | Tier | First deliverable |
|---|---|---|---|---|---|
| 1 | **P1** `dash-p2p-transport` (M) | **now**: BS7 is merged (180f0ff1) | ★M2, 0 | impl O-h; rev Sol (§4: O-h; P2P class) | #264's handshake and state machine on base-sdk's V1/V2 framing; v2 interop with dashd |
| 2 | **SB1** layer, purity and mobile gates (S) | **now**: no dependencies | 17.0 (M2); it guards every shared crate, so SB0 §13.3 runs it first | impl O-m; rev Sol | `layers.toml` and `check-layers.py` green against the baseline list; R2's enforcement removed |
| 3 | **SB4** `dash-chain` core (L, D) | **now**, design first: CS0, CS2 and BS5 are merged; for `i:CS3`, SB4 codes against CS3's signatures and merges after CS3 | 1.5 (M2) | D O-h; impl O-h; rev Sol→O-h | the SB4 design from SB0 §4, reviewed before implementation |
| 4 | **SB9** `dash-filters` (M) | **now**: CS0 is merged | 16.0 (M5) | impl S-h; rev Sol | the GCS codec and Dash element set passing `blockfilters.json` (L0), with dashcore's adapters |
| 5 | **SB6** `dash-p2p` core (L) | after P1 (O17 is merged) | ★M2, 0 | impl O-h; rev Sol→O-h | SB0 §3.1's `NetCore` API as a types-only PR, then the connection table |
| 6 | **P2a-core** (M) | after P1; `i:SB6` (the API is SB0 §3.1), `i:CH1-node`, `i:RP-V24` | ★M2, 0 (coding float 2.0) | impl O-h; rev Sol→O-h | the actor loop and the 74-row registry in `sim`, against SB6's stubbed `NetCore` |
| 7 | **EV0** reshape (S) | on the EV addendum's approval (CS0 merged; `i:BS6a` already met) | ★CP, 0 | impl O-m; rev Sol→O-h | the addendum §3's seven commits on `node/ev0-evo-interfaces` |
| 8 | **EV2a** (L) | after EV0 | ★CP, 0 | impl O-h; rev Sol→O-h (design approved, EV2 rev4) | EV2a-1 in `dash-dml`, with `impl MnListView for MnList` |
| 9 | **LQ1a** (M) | after EV2a and O2a-LQ1 (BS5 is merged); `i:SB7` | ★CP, 0 | impl O-h; rev Sol→O-h (design approved, LQ1 rev4) | LQ1a-B's not-held test source and chain view |
| 10 | **SB5** (M) | after SB4 | 1.5 (M2) | impl O-h; rev Sol→O-h | `TipCell` and the `SubscriberFeed` reader, stubbed first for CH1-node and CH2b |
| 11 | **CH1-node** (M, D with SB4) | after SB4; `i:SB5` (ST1 is merged) | 1.5 (M2) | D O-h; impl O-h; rev Sol→O-h | `ChainCommand` on V over `dash-chain` |
| 12 | **P2b** (M) | after P2a-core and SB6 | ★M2, 0 | impl O-m; rev Sol→O-h | the responder handshake over SB6's `inbound` |
| 13 | **O13a** (M) | after P2b | ★M2, 0 | impl O-m; rev Sol | v24 handshake transcripts, outbound and inbound |
| 14 | **SB7** (M) | after EV0 | 3.5 (REL11) | impl O-h; rev Sol→O-h | the SML entry, hash and roots moved from `dash/src/sml`, with dashcore's adapters |
| 15 | **SB8** (M) | after SB7, LQ1a and BS12 | 1.5 (REL11, through LQ2) | impl O-h; rev Sol→O-h | the one `CFinalCommitment::Verify`, with verifier-only tests |
| 16 | **CH2b** (L) | after CH2a (implementing) and ST3a (Sol CP3 running, then Opus high and Phase E); ST2 is merged; `i:SB5` | 2.5 (REL11) | impl O-h; rev Sol→O-h | b1/b2 against SB5's stubbed API |
| 17 | **P2a-net** (S) | after P1; `i:SB6`, `i:P2a-core` | 4.5 (M2) | impl O-m; rev Sol→O-h | the ConnMan drivers over SB6's dial steps, in `sim` |
| 18 | **ND1** (M) | after AR1 (in the A/B calibration, DEC-93) and CH1-node; `i:RP-V24` | 4.5 (M2) | impl O-m; rev Sol→O-h | the host services (`dash-clock`, `AbortSink`, `BlockingIo`, `Resolver`) |
| 19 | **SB2**, **SB3** (S each) | after SB1 | > 20 | impl S-h; rev Sol | `[profile.mobile]` with its measured size; `dash-spv-cli` |
| 20 | **SB10** (M) | after ST3a and CH2a | 13.5 (M5) | impl O-h; rev Sol→O-h | the mechanical `dash-kv` move, then `dash-kv-sqlite` |
| 21 | **SB12a** (S) | after SB7 and SB9 | 13.5 (M5) | impl O-h; rev Sol→O-h | header and block conversions, each header hashed once |
| 22 | **SB12** (M) | after SB4, SB9 and SB12a | 13.5 (M5) | impl O-h; review routing per PLAN §7 handling | Core's header rules on dash-spv's headers path |
| 23 | **SB13** (M) | after SB10 | 13.5 (M5) | impl O-h; rev Sol→O-h | the SQLite schema and migrator v3 |
| 24 | **O16** (M) | after ND1 | 18.5 (RL4) | impl O-m; rev Sol | dash-spv suites run against rdashd |

Later, in SB0 §13.3's order: EV2b, LQ1b, EV4-serve, SB11, SB14 (with #1104), SB15, SB16 (with #902 or SB0 Q1), SB19, SB17 and SB18; then SB18b at M6. SB-ST runs only if SQLite is refused.

**What the waiting rows wait on** (STATUS, 2026-10-09):
- **The EV addendum** (rev5 726ff9a1; Sol r6 APPROVE; Opus high r7 APPROVE-WITH-CHANGES, aeedda94; then the must-fix edits and approval): EV0, EV2a and LQ1a, which stay held. Through them, EV2b, LQ1b, SB7, SB8, SB12a, SB12, EV4-serve, SB11 and everything after wait too. This is the REL11 chain: EV0, EV2a and LQ1a have zero slack.
- **CS3** (fix r1 done; narrow Sol r2 running, then Opus high, PF0 and merge): SB4's merge (`i:CS3`). Through it, SB5, CH1-node and SB12 wait, and so does P2a-core's merge (`i:CH1-node`). SB4 itself starts now. EV1, outside this wave, also waits on CS3.
- **CS1-b** (CS1 before merge: fix r1 and the 4 × 24 CPU-h v24 campaign, then a narrow Sol r2, Opus high, PF0 and merge): no Wave 2 row. CS1 feeds CH3a and CS6 (M3).
- **RP-V24** (Sol r1 running, then Opus high, PF0 and merge): P2a-core and ND1 wait through `i:RP-V24`, since their `ReleaseProfile` code collapses to v24 (DEC-82). O13a's transcripts become v24-only. P2a-core cannot merge before P1 and SB6, nor ND1 before CH1-node, so RP-V24 is not expected to bind (2.5 weeks of slack against M2).
- **BS7 follow-ups:** none blocks a start, because P1 and SB6 build on the merged BS7 (180f0ff1).
  - BL-39's remaining fuzz CPU-h gate M1 exit 3.
  - BL-43 and BL-51 are pin-tooling items.
  - Review changes to BSX-PR's U3 (`p2p_frame`, #59) and U5 (`p2p_ctrl`) fold back into `node/base-sdk` (DEC-91), and P1 and SB6 take them at the next re-pin.
- **Other in-flight rows:** P1 (unstarted) gates SB6, P2a-core and P2a-net; CH2a and ST3a gate CH2b and SB10; AR1 gates ND1; BS12 gates SB8; O2a-LQ1 gates LQ1a; BS6b gates CV3.

---

## 9. Risks (beyond PRD §6.2 R-0…R-15, which stay in force)

| Risk | Impact | Mitigation | Escalation trigger |
|---|---|---|---|
| base-sdk fork drift or a heavy upstream churn week | rebase cost, a stalled BS track | weekly rebase (§6.3), tagged pins, upstream-shaped commits, one adaptation PR per bump | rebase not green within 2 days |
| The decode-domain fixes conflict with upstream design (kwvg) | the fork diverges permanently | keep the changes minimal and policy-typed (`CoreBool`, an i64 value accessor, `BlsEager`/`BlsLazy`); record in BASE-SDK-FORK.md | upstream rejects the approach (only if pasta opens a discussion) |
| **The relic transliteration for `Exotic` operands is subtly wrong** (rev1; BS11a design) | a consensus split on legacy BLS fields (v1 ProUpServ/ProUpRev sigs, v1 operator keys, v1/v2 commitments) under the legacy verify flag, or a wrong IES key | transliterate relic's functions verbatim as configured; the engine-vs-blst model target and the engine-vs-Core `diff_bls_arith`/`diff_bls_verify`, certified per DEC-184/186; relic's error code observed before dashbls clears it (per-wrapper raw traces from pre-call snapshots), outside decode, for normal, successfully allocated inputs; scan (h) as evidence | any divergence, or any nonzero pre-clear relic error code |
| **The `ReleaseProfile` grows beyond estimate** (rev1) | v23.1.8 MN behaviour late, so the mainnet pilot waits | O17 sized every row (170 `profile` after DEC-37); MN rows are planned work (P5's two trackers, SP0, LQ4a, AM1, DK1, QM1b, CJ1); the fallback is waiting for mainnet's v24 upgrade (§10 Q1) | O17 finds > 10 more MN-relevant rows (DEC-37 result: 8, not fired) |
| **DEC-37 probes on a zero-slack task** (O17) | O13b (zero slack for M5 exit) absorbs eight more P2P probes (DKG-05, -12, -13, INTG-01, MPIX-16, P2P-17, SIG-14, SIG-18) without a size change | each is a scripted single-message check with an expected reply or score, under both profiles; if O13b runs long they split into a follow-up task after M5 exit, and M5's fingerprint gate covers them then | O13b's estimate passes 3 weeks |
| **Power-loss harness complexity** (rev1) | CH2 or CH7a cannot show FR-STO-03 | ST3a first (model level, cheap); `dm-log-writes` needs root and loop devices, which agentbox has; the VM harness is weekly | ST3b not running by M3 |
| The engine choice misses a metric | perf regression | the ST1 bake-off plus CH2's multi-CF workload; the narrow engine trait; the MDBX swap path; the engine must pass every tracked metric | any tracked metric fails after tuning |
| The `PeerManager` actor is a throughput bottleneck at 125 peers | P2P/RPC metrics | Core has the same single thread; parallel transport and speculative hashing; PF1 profiling; pipelining only provably independent messages, with L6 ordering probes | P4 BG fails (CPU at 125 peers > dashd) |
| RocksDB files copied mid-write by the functional framework | flaky tier-2 tests | regtest: no auto-compaction, explicit flush points, a nightly copy-under-write test (A1 §2.9) | any FT copy failure in the nightly run |
| The floating-point reference is host-dependent | an ambiguous "Core answer" | inspect the release binaries first; an admissible-domain gate; `x*x` if folded | any admissible-domain mismatch |
| Testnet MN reachability not provided in time | REL-2.3 slips | §10 Q2 raised at wk 36; the Stage B devnet soak first | no answer by wk 40 |
| **Shared-box capacity and benchmark credibility** (M-11) | noisy wall clock, slow gates, unconvincing "never slower" | PF0 counters per PR; reserved windows; the SW-OPS capacity plan; a dedicated host and a soak VM (§10 Q4) | B1 variance above the NFR-PERF-02 bound in a reserved window |
| **Review capacity** (F18) | queues stall the critical path | the reviewer-load row (§2); review queue age in STATUS; split tasks | any CON review older than 5 working days |
| **EMBARGO-1 leakage** (M-1) | premature disclosure | §7: an opaque codename, the scrub, a minimal reader set, Claude-only review of the marked module, the denylist hook | any tracked file or prompt with mechanics (the scrub's residual check finds it) |
| **Schedule** (M-9) | Done-1 ≈ wk 30, M7 ≈ wk 52 even with the levers | §3's levers; early design steps; the decoupled MN track; weekly re-computation of the model in STATUS | the critical chain slips > 2 weeks against §3 |
| **SB0's external gates** (DEC-119) | SB16 waits on #902 or SB0 Q1; the shared crates and SQLite need the dash-spv maintainers (SB0 Q2); SB14 needs #1104 merged or carried | SB0 §10.3's #902 coordination; `legacy-net` for one release; SB-ST as the storage fallback (§4.12); the SPV rows have 11.5–13.5 weeks of slack against M5 (§3) | SB0 Q2 unanswered by about wk 9–10, or Q1 by about wk 16 (SB0 §15.1) |

---

## 10. Questions for pasta

Product and irreversible decisions are settled in DECISIONS.md (DEC-01…DEC-17). The open items are true blockers or escalations, detailed with defaults in **A1 §8**:

- **Q1:** mainnet MN pilot collateral, key custody and an inbound-reachable host. It is raised at M7 exit per DEC-06 and blocks REL-2.5. **Rev1:** the pilot also needs the v23.1.8 `ReleaseProfile` to be complete for the MN rows, or mainnet to move to v24 first. Pasta is told now, so the choice can follow mainnet's upgrade timing.
- **Q2:** inbound reachability for the testnet MN fleet (≥ 4 MNs + 1 evonode), plus a tDASH top-up if the faucet can't supply ~8,000. Needed by about wk 44 (rev1 schedule); blocks REL-2.3/2.4.
- **Q3:** tier-2 (macOS, Windows) build and smoke access. Needed at M8; blocks only NFR-PORT-01's tier-2 half.
- **Q4 (rev1, M-11; a DEC-06-style escalation, not blocking):**
  - **a dedicated benchmark host**, an idle machine of agentbox's class. It makes the "never slower than dashd" wall-clock evidence credible; until then, per-PR counter gates and reserved windows on agentbox stand in. Wanted by the M3 exit (≈ wk 23);
  - **a separate VM for the devnet soak** (≈ wk 41).
- **Q5 (rev1, M-10.4):** whether to push the non-embargoed upstream PRs #1136–#1138. The production fix for mobile and desktop SPV wallets on V24 activation lives there, and testnet V24 signalling started 2026-09-24. This touches no node work.
- **SB0 (DEC-119):** SB0 §15.1's open questions, with the deadlines given there: Q2, pre-clearing the shared dash-spv crates and SQLite with the maintainers (by about wk 9–10); Q1, #902 first (by about wk 16); Q6, the Mac iOS checks (SB17 only); Q8, the Platform lookup grace window.
- **EMBARGO-1 history (rev1, M-1):** whether to rewrite agent-config history (commits 47121b2..ca37fd5) and the pre-scrub `node/integration` history, and whether to add the denylist pre-commit hook to agent-config. Both histories are local or private; nothing is pushed in the meantime.

---

## Appendix: story traceability (US → milestone → tasks)

N/C surfaces are delivered by this plan. D-surface parts are delivered by the desktop D-track (dashwallet-desktop ROADMAP from D3). The 21 former "D: pending decision" rows follow DEC-01: wallet-holder flows come to the desktop after the DashPay core; operator flows stay N + C. `compat/STORIES.md` (RL7) turns this table into per-test evidence.

| Story IDs | Milestone (N/C) | Tasks |
|---|---|---|
| US-NODE-01 | M2 (headers) → M4 | CH1-node, CH2–CH7, P2a-core, P2a-net, P2b, P3, ND1; SB4–SB6 (SB0 rev6.1) |
| US-NODE-02 | M5 | SP0, SP1, GV1b, RP2b |
| US-NODE-03 | M2 → M5 | AR1, ND1 |
| US-NODE-04 | M3 | CH2, CH5, ST3a/b, ND1 |
| US-NODE-05 | M5 | CH8 |
| US-NODE-06, -09, -10 | M5 | P4, RP1a/b |
| US-NODE-07, -08 | M8 (P8 from M5) | P8 |
| US-NODE-11 | M5 | ZQ1 |
| US-NODE-12 | M2 → M5 | ND1 |
| US-NODE-13 | M5 | SP0, SP1, RP2b |
| US-NODE-14 | M4 | CS4, RP1a |
| US-NODE-15 | M8 | ZQ1 (statsd) |
| US-NODE-16, -17 | M5 / M3 | RP1a/b, IX1 / CH3b |
| US-NODE-18 | M6 | CH6a/b, IM1b, IM2, WL3 |
| US-NODE-19 | M2 | ND1, CH5 |
| US-NODE-20, -21 | M8 | RL1 |
| US-NODE-22 | M4 (smoke) → M5 | O15 |
| US-WAL-01…15 (N, C) | M6 | WL1–WL8, WL2b RPCs |
| US-WAL-19 (N side) | M5 | RP0–RP2b (RPC link) |
| US-WAL-16…18, -20…22, D parts of 01…15 | desktop D-track | SW-DESK |
| US-DPY-01…12 | desktop D-track | SW-DESK |
| US-MN-08 | M4 (consensus) → M7 (live) | EV3, CH7a, RL4 |
| US-MN-10 | M4–M5 | RP2a, RP2b |
| US-MN-01…03, -16…18, -21 | M6 | WL5, WL1 |
| US-MN-04…07, -09, -11…15, -19, -20 | M7 | AM1, QM1a/b, DK1, SG1a/b, LK1c, CJ1, GS1, RL4 |
| US-SHR-01…06 | M7 (consensus at M4) | EV1, EV3, WL5, RL4 |
| US-GOV-01, -05, -06, -10 | M5 | GV1a/b, RP2b, ZQ1 |
| US-GOV-02…04, -08, -09 | M6 | WL5 |
| US-GOV-07 | M7 | GS1, MN1 |
| US-GOV-11 | desktop D-track | — |
| US-CJ-01…06, -09 | M6 | WL6, WL8, CJ0 |
| US-CJ-07 | M7 | WL6 + V24 testnet |
| US-CJ-08 | desktop D-track | — |
| US-CJS-05 | M5 | CJ0, P6a |
| US-CJS-01…04, -06 | M7 | CJ1 |
| US-MINE-05 | M3 | MN0 |
| US-MINE-01…04 | M5 | MN1, CS4, RP3a |
| US-INT-01…15 | M5 | RP1a/b, RP2a/b, RP3a, IX1, IX2, ZQ1, MP2, LK1a/b |
| US-PLAT-06 | M4 | EV3 |
| US-PLAT-02, -08 | M4–M5 | RP2a/b, RP0 |
| US-PLAT-01, -03…05, -07, -09 | M7 | WL5, SG1a, RP3b, AM1, PL1 |
| US-LCS-01…07 | M5 (dashj at M8) | P4, P6a/b, IX1, O16; SPV mode: SB9, SB14–SB18 (SB0 rev6.1) |
| US-IMP-01 | M3 | ST2, CH5 |
| US-IMP-02 | M4 | CH6a |
| US-IMP-03…09, -13 | M6 | WL3, IM1a/b, IM2 |
| US-IMP-10 | M8 | WL3 (Electrum-Dash import, P2) |
| US-IMP-11, -12 | desktop D-track | — |
| US-DEV-03 | M3 | RP1a, ND1 |
| US-DEV-10 | M4 | O15, O9a/b |
| US-DEV-04, -05, -07…09 | M5 | SP1, RP1a/b, O11, P4, O16 |
| US-DEV-01, -02 | M7 | O12b |
| US-DEV-06 | M8 | RP2b (`evodb verify/repair`) |
