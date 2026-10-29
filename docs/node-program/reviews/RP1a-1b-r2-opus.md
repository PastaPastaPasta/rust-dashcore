# RP1a-1b review, round 2 (Opus)

**Verdict: ACCEPT-WITH-NITS**. 0 major, 1 minor, 5 nits.

**Scope:** `node/rp1a-1b` at `50f9a262c` (commits `0337f4741`, `50f9a262c` and `1d28684ca`). The Core reference is v24.0.0-rc.4 `49caf372bb`, read with `git show 49caf372bb:<path>` because the shared checkout is on `develop`.

I didn't redo Sol r1's parity pass (mining.cpp, descriptor.cpp, key_io/base58, core_read, the 112-char bound). This round covers concurrency and liveness, untrusted-input bounds, API fitness for RP1a-3, D15–D19, and mutation strength.

## Findings

### m-1: dashd-node's TBV mapping has a wildcard and no test, and the retry loop never checks shutdown

- **File:** `dashd-node/src/rpc.rs:335-347` (`impl Miner for ChainSlot::test_block_validity`), together with `dashd-rpc/src/mining.rs:422-443` (the `generateblock` retry loop).
- **Core:** `rpc/mining.cpp:388-399` (rc.4) and `validation.cpp:4934` (the assert D17 replaces).
- **The mapping:** `Ok(Err(_)) => BlockCheck::TipMoved` maps *every* `NotTested` to "rebuild and retry".
  - Today only `NotBestPrev` can reach this arm, because `ChainHandle::test_block_validity` passes `lookup = false` (`dashd-chainstate/src/chain/actor.rs:407-416`, `proposal.rs:38-52`).
  - MN0's `ChainTemplateSource` handles the same reply strictly: `NotBestPrev` retries, and any other variant becomes `BlockState::Error(other.to_string())` (`dashd-mining/src/source.rs:134-136`).
- **Why it can hang:** the `generateblock` loop has no iteration bound and no `shutdown_requested()` check.
  - Suppose the arm ever sees a persistent variant. That happens if someone routes this call through `propose_block` (lookup on, so `Duplicate*` for a block that is already indexed), or if a later `NotTested` variant is added.
  - Then `generateblock` spins forever on an RPC worker, sending two or three V commands per turn.
  - `Node::shutdown` runs `rpc.stop()`, which joins the workers, before `steps.shutdown()` stops V (`node.rs:463-476`). V never drops the reply, so shutdown hangs.
  - Even with today's variants, the loop runs for as long as blocks keep landing between its two V turns, for example while V's queue is busy during IBD. That ends eventually, but nothing caps it.
- **Test strength (mutants, run in the reflink copy):**
  - `Ok(Err(_)) => BlockCheck::NoChain` (RVMUT=1) **survives** all 164 dashd-node lib tests. No test sends this impl a block whose parent isn't the tip. DIFF-RPC can't either, because its calls are serial.
  - `BlockState::Invalid(e) => BlockCheck::Valid` (RVMUT=2) survives all 164 dashd-node lib tests too. It is killed only by DIFF-RPC `generate_commands_match_dashd_v24` (`block_spend_again`: dashd answers -25 "TestBlockValidity failed: bad-txns-BIP30"). That matches D17's "DIFF-RPC covers it", but the test needs a dashd.
- **Suggested fix:**
  1. Match `Ok(Err(NotTested::NotBestPrev)) => BlockCheck::TipMoved` and `Ok(Err(other)) => BlockCheck::Invalid(other.to_string())`, mirroring source.rs.
  2. In the `generateblock` loop, check `g.shutdown_requested()` before each rebuild. On shutdown, answer as `GenerateBlock`'s shutdown return does: -1 "Failed to make block.".
  3. Add a dashd-node test on the real V. Take a template from `slot.create_new_block`, connect one block (for example with `generatetoaddress 1`), then pass the stale template to `<ChainSlot as Miner>::test_block_validity` and assert `TipMoved`. A valid block on the tip should give `Valid`. This kills RVMUT=1 without dashd.

### n-1: generateblock hand-rolls `DecodeHexTx`, and `decode_tx_exact` already exists

- **File:** `dashd-rpc/src/mining.rs:400-405`.
- **Core:** `core_read.cpp` `DecodeHexTx`.
- **Problem:** `parse_hex(..).and_then(decode_tx + input.is_empty())` re-implements `dash_consensus::decode_tx_exact` (`dash-consensus/src/primitives/decode.rs:180`, "Core's `DecodeTx`"). RP1a-3, RP1b and RP3a (decoderawtransaction, sendrawtransaction, the rawtx reads) will need the same hex-to-tx step.
- **Suggested fix:** add `decode_hex_tx(hex) -> Option<Tx>` beside `decode_hex_blk` and `decode_hex_block_header`, over `decode_tx_exact`, and call it here.

### n-2: CHECK_NONFATAL is written as `assert!`

- **File:** `dashd-rpc/src/mining.rs:425`, `dashd-rpc/src/descriptor.rs:46` and `:580`.
- **Core:** `util/check.h` CHECK_NONFATAL, which throws `NonFatalCheckError` and answers -1 "Internal bug detected: ...".
- **Problem:** if one of these checks fails, the panic unwinds into the HTTP worker's `catch_unwind` (`http/mod.rs:229`) and the client gets "Unhandled request" instead of Core's -1 text. `commands::internal_bug` exists for exactly this, and `dash.rs:54` uses it. All three checks are unreachable today, so the impact is only on the diagnostic.
- **Suggested fix:** return `Err(internal_bug(...))` for the mining.rs check. The descriptor ones can stay as `debug_assert`s, or use the same helper.

### n-3: shapes RP1a-3 will have to change

RP1a-3 needs getdescriptorinfo, deriveaddresses, validateaddress, createmultisig and verifymessage. The port's parser and expansion fit them. These shapes don't:

1. **Origins are dropped.** `descriptor.rs:362-373` checks the `[fingerprint/path]` and throws it away. getdescriptorinfo's `descriptor` (`desc->ToString()`) prints it, so `PubkeyProvider` needs an `Origin { fingerprint, path, inner }` variant.
2. **The checksum can't be required.** `check_checksum` (`descriptor.rs:115`) always behaves as `require_checksum = false`. deriveaddresses calls `Parse(.., require_checksum = true)` and needs "Missing checksum".
3. **No error text from address decoding.** `decode_destination` (`key_io.rs:93-97`) returns `Option` and, by its own doc, drops Core's `error_str`/`error_locations`. validateaddress returns both, and that needs the Platform bech32 decode plus `bech32::LocateErrors`. This is the only change that touches existing call sites (three of them).
4. **No encoders yet.** There is no `EncodeDestination`, `ExtractDestination` or `InferDescriptor` (createmultisig's `descriptor`). These are additive.

None of this is wrong for 1b. Either keep the origin now, which is cheap, or add a D-row so RP1a-3 plans the `decode_destination` signature change. A longer-term note: dashd-wallet (WL2) will want the descriptor and key_io code too. If so, a home outside dashd-rpc would avoid a wallet → rpc dependency.

### n-4: separator-dense descriptors allocate before any check

- **File:** `dashd-rpc/src/descriptor.rs:116` (`split('#').collect()`), and likewise `:296` (`/`), `:349` (`]`) and `:362`.
- **Measured** (debug build, reflink copy):
  - A 32 MiB `"#"` descriptor (the HTTP body limit) raised the test process's peak RSS from 101 to 597 MiB before "Multiple '#' symbols" (3.2 s).
  - `pkh(<pubkey>` followed by 32 MiB of `/` took 27.6 s, mostly `descriptor_checksum`, which costs O(n·|charset|) and is much slower in debug.
- **Core:** `Split` builds the same `std::vector<Span>`, and its checksum loop has the same cost. This is not a divergence: it's authenticated, and it's bounded by `MAX_BODY_SIZE`.
- **Suggested fix (optional):** `splitn(3, '#')` in `check_checksum` gives the same answers with O(1) pieces. The other splits are fine.

### n-5: stale DEC citation for the G1 divergence

- **File:** `dashd-rpc/src/mining.rs:33` and `dashd-rpc/src/mining/tests/generate.rs:251` cite "DEC-595 D4".
- **Problem:** the ruling that records the divergence is DEC-607 D4, with its text superseded by DEC-630 (1). D18 cites DEC-607.
- **Suggested fix:** cite DEC-607 D4 / DEC-630.

## Checked, no finding

- **Concurrency and liveness (focus 1):**
  - `grind` runs on the RPC worker. V is never held across the nonce loop. The template (one query turn plus one TBV turn), generateblock's TBV, and each `ProcessNewBlock` are separate V commands.
  - The slot mutex is taken only to clone the `Arc`/handle (`miner()`, `test_block_validity`) and is never held across a V wait.
  - `shutdown_requested` is an `AtomicBool` load (`shutdown.rs:73`), read once per nonce as Core's `ShutdownRequested()` is.
  - Shutdown order: `interrupt` → `rpc.stop()` (joins workers) → `detach` → V stops. So a grinding worker sees the flag and returns `Stopped` while V is still alive. The detached-slot answers (`Grind::Shutdown`, `NoChain`) are only reachable in tests.
  - A long generate holds one `-rpcthreads` worker, as in Core. It doesn't block other workers or V between its blocks.
- **Rejection on submit:** `Writer::process_new_block` maps a dropped reply (V's `AbortNode`) to `accepted = false`, which answers -32603 "ProcessNewBlock, block not accepted", as Core's `false` return does. A block accepted to disk but not connected still has its hash pushed, as in Core. The stub test `a_block_processnewblock_refuses_is_an_internal_error` covers the RPC side.
- **D17's rebuild:** it is a strict improvement on Core, which would hit `assert(pindexPrev == Tip())`. Rebuilding with the same transactions is a serial order Core could have run.
- **Untrusted input (focus 2):**
  - Descriptor recursion depth is at most 2 (`sh` only at the top). multi() key counts are checked after parsing, as Core does, and expansion runs only after the n ≤ 20 check. BIP32 derivation stops at depth 255 (`ExtHeader::child`), so expansion is at most 20 × 255 derivations however long the path.
  - Raw-tx decoding is bounded. Probes with a vin count of 2^32−1 or 2^64−1, a 32 MiB script length, and a huge vout count each failed in under 0.5 ms with -22. 100k decodable raw txs in one generateblock completed in 3.7 s (debug, stub miner), linear.
  - I found no panic reachable from RPC input: every index and slice in `func`, `expr`, `parse_pubkey*`, `check_checksum`, `decode_destination` and `decode_secret` is guarded, and slicing happens only after the checksum pass has restricted input to the ASCII charset. The `ExtPubKey::derive` hardened `debug_assert` is unreachable unmutated, because hardened paths go through the xprv branch.
- **D15–D19 vs the code:**
  - D15's ancestry checks out: c0eb7ac3c is the first parent of b3d1ae773, ca76c146f is the second parent, and 569991d6f is merged in d58cf532a. So do its dependency claims: dashd-rpc has no dashd-mining dependency, and dashd-node uses `test-support` only as a dev-dependency.
  - D17 and D18 match the code and the error texts. D19 (func-compat) I didn't re-run.
- **Mutants:**

  | Mutant | Result |
  |---|---|
  | Hardened-path detection dropped (`descriptor.rs:262`, RVMUT=3) | killed: 3 dashd-rpc tests |
  | Depth-255 `checked_add` → `wrapping_add` (`key_io.rs:186`, RVMUT=4) | killed: `derivation_stops_at_depth_255` |
  | TBV `Invalid` → `Valid` in dashd-node (RVMUT=2) | killed by DIFF-RPC only |
  | TBV `NotBestPrev` → `NoChain` in dashd-node (RVMUT=1) | survives (m-1) |

  I didn't mutate the generateblock retry, the submit flag or the combo pick myself. By reading, the stub tests (`templates == 2`), the dashd-node test (`getblockcount` 5 after `submit=false`) and the combo coinbase assert would catch them.

## What I ran

All of this ran in the reflink copy `/work/scratch/rp1a-1b-rv-opus2` with `CARGO_BUILD_JOBS=12` and `TMPDIR=/dev/shm/rp1a-1b-rv-opus2-tmp`. The mutants are env-gated (`RVMUT=n`), and two probe tests were added there only. The live worktree was not touched.

- `cargo test -p dashd-rpc --all-features`: baseline 134 passed (133 plus my probe). RVMUT=3: 3 failed. RVMUT=4: 1 failed.
- Probes: `rv_probe_huge_counts` (generateblock decode bounds) and `rv_probe_separator_dense` (n-4).
- `cargo test -p dashd-node --lib`: RVMUT=1 gave 164 passed and RVMUT=2 gave 164 passed (3 ignored each), so both mutants survive.
- With `eval $(python3 contrib/setup-dashd.py)`, `cargo test -p rdashd --test rpc_diff generate_commands_match_dashd_v24`: RVMUT=2 failed (1 of 71 differ: `block_spend_again`, so the mutant is killed). Baseline (RVMUT=0): 1 passed in 148 s.

The scratch copy is left in place for the manager.
