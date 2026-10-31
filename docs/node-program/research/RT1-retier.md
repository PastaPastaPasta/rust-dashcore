# RT1: PLAN §4 S-h re-tier under DEC-130

DEC-130 (from `reviews/sonnet-ab.md`) keeps `impl S-h` only where acceptance is fully enumerated by the row, its brief or its A1 section. A row moves to `impl O-m` when it needs:
- **(a)** all-platform behaviour (DEC-84) beyond what its vectors cover;
- **(b)** "complete" modelling of a Core structure where completeness isn't enumerated;
- **(c)** gate, checker or guard tooling that other tasks' correctness relies on.

Scope: every §4 row whose Model cell read `impl S-h` at 6705e182 (56 rows). Rows marked **(done)** have finished implementation (STATUS at 6705e182); they are classified for the record and their Model cell is unchanged. Running tasks (IM1a) finish at their settings (DEC-92). The `rev …` part of every cell is unchanged. §8's start-list rows mirror §4 and were updated for SB1, O13a and O16.

`tools/cpm_plan.py` does not read the Model column: it loads `tools/cpm.py` (durations and edges only) and applies the SB0 row changes. No duration changed, so M2, REL11 and M5 are unchanged.

| ID | Result | Clause | Reason (row text) |
|---|---|---|---|
| BS4b (done) | stays | — | fixed oracle target set: tx/block/header + payload 1–12 × version × BLS flag |
| BS7 (done) | stays | — | closed command list; `diff_v1_framing`, `diff_headers2`, `diff_p2p_<cmd>` for "the BS7 set" |
| BS9b | stays | — | named commands (`dsq`, governance votes) with stated rules; "L1 per command" |
| BS10 | stays | — | closed list: "all 74 commands"; corpus round-trip and `diff_p2p_<cmd>` ×74 |
| BS13 | stays | — | stated surface (`encoded_len`, streaming SHA256d, `decode_with_ranges`); "hash equality fuzz vs the non-streaming path" |
| BS14 | stays | — | mechanical bridge; "round trip over the BS2 full-history corpus", display tests, compile smoke |
| BS15 (done) | stays | — | "all mainnet header hashes equal across backends and the oracle" |
| BS16 (done) | stays | — | "`FFINetwork` discriminants and serde names unchanged" |
| BS18 | stays | — | "inventory, service and opcode tables equal Core's"; dash-spv dashd suites |
| CV1 | stays | — | mechanical removal (#400); dash-spv suites |
| CV2 | stays | — | stated surface; "lax-DER parity vs oracle" |
| RP-V24 (done) | stays | — | mechanical collapse with a stated surface; "no consensus behaviour change at v24 (vectors + oracle diff)" |
| BSX-PR | stays | — | per-unit upstream PRs of already-reviewed fork code; "each PR merged upstream" |
| AR1 (done) | moves | (a) | "every Core option name", `settings.json` on every platform; the A/B decided on Windows fidelity beyond the vectors |
| AR2 (done) | moves | (b) | "`help <cmd>` byte-equal", conversion table: nested and hidden args not enumerated (A/B) |
| AR3 | stays | — | closed option list in the row; "XCLI vs dash-cli against dashd", `interface_bitcoin_cli` |
| AR4 | stays | — | fixed vectors: `bitcoin-util-test.json`, `tool_wallet` |
| RP0 | moves | (b) | Core's whole HTTP/JSON-RPC server (queues, squashing, error codes, number format) with "DIFF-RPC on errors and help"; the error/format space isn't enumerated |
| RP1b | stays | — | named RPC list; "PRD §5 MUST rows for these" |
| RP2a | stays | — | named RPC list; DIFF-RPC on the v24 oracle |
| RP2b | stays | — | RPC set fixed by PRD §5 and AR2's spec; DIFF-RPC and named FTs |
| RP3b | stays | — | named RPC list; `rpc_verifychainlock`, `rpc_verifyislock`, `feature_quorum_proof_chain` |
| MP2 | moves | (a) | "float sites allowlisted, A1 §4.5": estimator output on every platform, beyond the one replayed stream |
| ZQ1 | stays | — | closed lists: "19 topics", "all endpoints"; named FTs and frozen fixtures |
| O2a (done) | moves | (c) | the oracle entry points and "the coverage matrix" every CON task's differential relies on |
| O5 (done) | moves | (c) | quirk-register checker; the A/B decided on the checker's workspace-wide one-test-per-id rule |
| O6 | stays | — | closed list: "chain scans (a)–(h)"; results recorded |
| O8 | moves | (c) | "full-state digests" and decision records that O9a/O9b/O15's comparisons rely on |
| O9a | moves | (c) | "replay tool + digest comparator"; the checker for every replay |
| O9b | moves | (c) | "per-PR window selection + `-consensusxcheck`": a per-PR gate |
| O11 | moves | (c) | "functional-test compat runner", "passlist ratchets", expected-failures: the FT gate |
| O12a | moves | (c) | mixed-network harness; "mode A passlist automated" gates NFR-CEQ-09 |
| O12b | moves | (c) | mode B harness with "divergence paging"; Stage-A criteria automated |
| O13a | moves | (c) | fingerprint lab: the transcript checker the P2 handshake tasks are judged by |
| O14 | moves | (c) | "DIFF-RPC harness + `compat/ALLOWED-DIFFS.md`": the acceptance tool of every RPC row |
| O15 | moves | (c) | "shadow comparator + divergence tripwire"; "a forced divergence stops MN signing" |
| O16 | moves | (c) | `dash-testkit` (`RustNode`, `MixedNetwork`, `DASHD_IMPL=rdashd`) under every rdashd test |
| WL2b | stays | — | closed list: "Core's 71 wallet RPCs"; DIFF-RPC with identical seeds |
| WL7 | stays | — | "the remaining `wallet_*`" FTs and REL-3.2 wallet rows |
| IM1a | stays | — | closed file list (six loaders); `feature_addrman`, `mempool_persist` with Core files; loader fuzzing. Running |
| IM1b | stays | — | closed file list; "round-trip fixtures from the oracle datadirs" |
| IM2 | stays | — | one scenario: "M6 exit 3 round trip" |
| PL1 | stays | — | Platform's e2e and P-1 fixtures are the oracle |
| PF1 (done) | stays | — | research output: "the Amdahl budget per metric published" |
| PF2–PF6 | stays | — | "as v1", "per §5 SW-PERF"; none of (a)–(c) |
| RL1 | moves | (a) | "signed tier-1 artifacts", "tier-2 build smoke pack", upgrade and downgrade refusal across platforms |
| RL2 | stays | — | docs "exercised against the OW-* scripts" |
| RL5 | stays | — | operations; "M8 exit 1" |
| RL6 | stays | — | report; "M8 exit 5" |
| RL7 | stays | — | closed lists: 168 stories, OW-01…22 |
| SB1 | moves | (c) | "layer, purity and mobile gates"; "it guards every shared crate" (§8) |
| SB2 | stays | — | stated profile; "builds for aarch64-android", size measured and recorded |
| SB3 | stays | — | stated surface; "closes the L6 entries of `layers-baseline.toml`" |
| SB9 | stays | — | L0 `blockfilters.json`, oracle filters, dashcore's bip158 tests |
| SB17 | moves | (a) | FFI and mobile behaviour on iOS and Android ("Mac iOS build and durability", real-device cell) beyond the emulator smoke |
| SB-ST | moves | (a) | segment crash model for dash-spv on every platform's fsync semantics, beyond ST3a's sweep on agentbox |

## Totals

| | stays | moves | total |
|---|---|---|---|
| open rows (Model cell edited if moved) | 30 | 16 | 46 |
| done rows (record only) | 6 | 4 | 10 |
| **all** | **36** | **20** | **56** |

By clause: (a) AR1†, MP2, RL1, SB17, SB-ST = 5; (b) AR2†, RP0 = 2; (c) O2a†, O5†, O8, O9a, O9b, O11, O12a, O12b, O13a, O14, O15, O16, SB1 = 13. († done.)

Moved and edited in PLAN §4: RP0, MP2, O8, O9a, O9b, O11, O12a, O12b, O13a, O14, O15, O16, RL1, SB1, SB17, SB-ST.
