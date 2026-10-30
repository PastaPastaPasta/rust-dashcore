# O2a-LQ3 oracle lock verdicts (ABI 25): Opus-h review, round 1

**Ranges:**
- rdc `node/o2a-lq3-oracle`: `d9cf6a106..74abb9c42` (4 commits: 3a248d2e7, 8785258bc, af22da71b, 74abb9c42).
- Core `node/o2a-lq3-oracle-v24.0`: `5c9e9117a2..808f56f7d2` (3 commits).

**Core reference:** Dash Core v24.0.0-rc.3 (`0364ed626e`). Line numbers are rc.3 unless marked "pin".
**Reviewer:** Opus 5.5, high effort (independent; no edits, commits or pushes to either branch).

**Verdict: CHANGES REQUIRED**

The four Core entries are correct. Each calls Core's own functions, with the inputs, state and order of the
handler it models, and each substitution and refusal is exact or documented (scope 1 below). ABI 25 bookkeeping
is clean. The fixed SB4 vectors change only as D20 says.

The changes are needed elsewhere:
- **F1 (hygiene):** the Core branch history has to be cleaned before it is stacked.
- **F2, F3 (differential strength):** two cheap, test-only additions, with no re-pin. Today the node-l1
  differential passes two plausible Rust mutants in exactly the windows this row claims to compare: the
  active-quorum window's upper edge, and the ChainLock sign offset.

Everything else is Minor or a nit, with a schedule.

## Findings

### F1. Required before stacking (hygiene): the Core branch history is outside the public-text rule

- **Where:** Core `480859e190` (still in the branch history) and the commit message of `808f56f7d2`.
- **Problem:** see manager note.
  - `808f56f7d2`'s message fails the DEC-239 hygiene grep.
  - `480859e190` carries a stub line outside the permitted sites. `808f56f7d2` deletes it from the tree, but both
    commits would enter the bundle history through the train merge.
  - rdc is clean. Over the added diff and all four commit messages, the only hit is the banner stub at
    `docs/node-program/designs/O2a-LQ3-notes.md:5`.
- **Fix:** merge a squashed Core rider: one commit with the `808f56f7d2` tree and a neutral message. This costs
  nothing extra at stacking, because the bundle is renumbered and re-pinned there anyway (DEC-238, amended). The
  alternative is for the owner to squash now and re-pin: archive rebuild, pins.txt, SB4 provenance regen.
- **Schedule:** the train driver, at stacking, recorded in the bundle D-row. Otherwise the owner, before READY.

### F2. Required (differential): the active window's upper edge is never compared; `keepOldConnections + 1` survives

- **Where:**
  - `node-l1/tests/l1_llmq.rs:2374` `captured_recovered_sigs_match_cores_verdicts`, on `lq3-regtest-v24.0` only
    (`lock_chain`, `:1936-1943`).
  - Mutant site: `dash-evo/src/locks/recsig.rs:116`.
- **Core:** `IsQuorumActive` scans `keepOldConnections` quorums at the tip (`llmq/signing.cpp:701-712`), called
  at `:429` and again at `:556`. Shim: `oracle_llmq.cpp:1099` (pin).
- **Problem:**
  - The capture chain mines exactly three quorums per type, and `keepOldConnections` is 3. So every mined quorum
    is active, and no case names a quorum that is mined but inactive.
  - D19 closed the lower edge (`signingActiveQuorumCount`): the owner's mutant is killed, and I reproduced it.
  - The upper edge is open. Mutant **M1** (`keep_old_connections + 1`) passes all four differentials. Only the
    synthetic `recsig_tests::only_the_newest_keep_old_connections_quorums_are_active` kills it.
  - **Concrete schedule:** a `qsigrec` naming the fourth-newest `llmq_test` quorum is `INACTIVE_QUORUM` in Core
    (dropped, no score). Under M1 it is checked, and the bad signature is banned.
- **Fix (test-only, no re-pin):** `llmq-regtest-v24.0` mines five `llmq_test` quorums, so the two oldest are
  mined but inactive.
  1. Let `lock_chain` take a stem, and skip rotated commitments as the mined test does.
  2. Craft a `qsigrec` per mined type-100 quorum on that context. Any id, message and signature will do: Core
     answers `INACTIVE_QUORUM` before it reads the signature.
  3. Compare the result as the captured cases are compared, and assert that at least one `InactiveQuorum` comes
     from a mined quorum.
- **Schedule:** this row, before READY.

### F3. Required (differential): the ChainLock sign offset is off by one and nothing notices; a failing pass's quorum is never compared

- **Where:**
  - `node-l1/tests/l1_llmq.rs:2208` (the height cases).
  - `:2067-2076` `core_lock_outcome`, which drops `pass_quorum` on a non-accept; this is D14's "a failing pass's
    quorum is not compared".
  - Mutant site: `dash-evo/src/locks/mod.rs:428`.
- **Core:** `chainlock/handler.cpp:154-156`. The start block is `SelectQuorumForSigningStartBlock(chain, nHeight)`
  at `nHeight - SIGN_HEIGHT_OFFSET` (`llmq/quorumsman.cpp:711-722`; `quorumsman.h:210-213`).
- **Problem:**
  - Mutant **M5** (`SIGN_HEIGHT_OFFSET - 1`) passes all four differentials.
  - Each captured ChainLock is accepted by the same quorum whether the scan starts at `h-8` or `h-7`, since no
    quorum is mined between them.
  - Every height-mutated case fails the signature on both sides. The quorum Core selected for it is in the
    verdict but is not compared.
  - LQ3a's unit tests kill M5. The oracle differential, whose purpose is the selection, does not.
- **Fix (test-only, no re-pin), both parts:**
  - (a) Add heights at each ChainLocks-type quorum's boundary, `mined + SIGN_HEIGHT_OFFSET - 1` and
    `mined + SIGN_HEIGHT_OFFSET`. That is 162/163, 186/187 and 210/211 on this chain.
    - I checked this in a scratch copy: the baseline passes, and M5 fails at `clsig 0 height 162` (Core
      `NotVerified { invalid: false }`, the mutant `invalid: true`).
  - (b) Compare `pass_quorum` on non-accepts as well. Compute ours with `dash_evo::llmq::select_start_height` and
    `select_from_scan` over the view's scan, or have LQ3a's rejection carry the selected quorum. This catches any
    selection drift on every `NotVerified` case, not only at boundaries.
- **Schedule:** (a) this row; (b) this row or with the D17 follow-up row.

### F4. Minor (fidelity of the ban decision): the `isdlock` comparison collapses the two passes into "any pass invalid"

- **Where:**
  - `node-l1/tests/l1_llmq.rs:2073`.
  - LQ3a's `combine` (`dash-evo/src/locks/mod.rs:671-694`) collapses the passes the same way. This is noted for
    LK1, not as a re-review of LQ3a.
- **Core:**
  - Pass 1 (`pass_status[0]`, `ban=false`) scores nobody.
  - Pass 2 (`pass_status[1]`, `ban=true`, offset `dkgInterval`) re-checks only the locks whose *message* went bad
    (`net_instantsend.cpp:340-353`).
  - In pass 2, a quorum that rejects the signature marks the source, scored
    `OLD_ACTIVE_SET_FAILURE_MISBEHAVIOR_SCORE` = 20 (`:189-199`). No quorum marks only the message, with no score
    (`:154-157`).
  - So Core's response is decided by the **second** pass's status alone:
    - (Invalid, NoQuorum) → no score;
    - (NoQuorum, Invalid) → 20.
  - The entry reports each pass separately (`pass_status[2]`), so the oracle already has what is needed.
- **Problem:**
  - The differential compares `invalid = any(Invalid)`, and LQ3a gives `InvalidSignature` for both orders. LK1
    cannot derive Core's score from LQ3a's verdict.
  - Only rotated quorums can give different statuses in the two passes; non-rotated passes both use
    `GetQuorum(type, cycleHash)`. This chain has no rotated quorums (D17), so nothing here is wrong today. It is the
    mainnet path, though.
- **Fix:** compare the last pass's status, which carries Core's ban decision, and have LQ3a/LK1 expose it (for
  example, `InvalidSignature { scored: bool }`).
- **Schedule:** with the D17 rotation follow-up row; flag it to LK1.

### F5. Minor (coverage, feeds D17): two mutants survive every test in the workspace or the differential

- **M2:** `cycleHeight + dkgInterval < tip` → `<=` (`dash-evo/src/locks/mod.rs:611`; Core
  `net_instantsend.cpp:146`).
  - It passes the four differentials and **all 174 dash-evo tests**.
  - The sign height only matters for rotated quorums, so the boundary is untested anywhere until a rotated capture
    exists. This makes D17's follow-up necessary, not optional.
  - Cheaper interim: an LQ3a unit test at `tip = cycle + dkgInterval` and `+ 1` over a rotated scan.
- **M6:** the cycle lookup moved ahead of `TriviallyValid` (`mod.rs:589-599`; Core `net_instantsend.cpp:253-279`).
  - Core scores a lock with no inputs and an unknown cycle 100 (`:262`, `:67`). The mutant scores it 1 (`:269`).
  - The differential mutates one field at a time, so it never pairs two reject paths. M6 survives it; LQ3a's
    `islock_trivial_checks_come_first` kills it.
  - **Fix:** add a few pairs, each early refusal combined with a later one: no inputs + unknown cycle, unknown
    cycle + malformed signature, off-boundary cycle + zero signature. For `qsigrec`, M4 (signature read moved ahead
    of `IsQuorumActive`) is already killed, because under the legacy scheme a captured basic signature reads
    malformed.
- **Schedule:** M6's pairs in this row (test-only); M2 with D17.

### F6. Minor (coverage, answers A1): no legacy-scheme acceptance is compared with Core

- **Where:** both fixtures. V19 is buried at height 1 (LQ3a D29), so every captured signature and every quorum key
  is basic.
- **Problem:**
  - Under `legacy_bls = 1`, Core's verdicts in the differential are rejections only:
    - 15 `qsigrec` and 5 `clsig` cases are ours-no-verdict;
    - the mined test has `SignaturesFailed`/no-verdict.
  - A Rust legacy-verification bug that rejects valid legacy signatures would pass. LQ3a D20 covers legacy
    acceptance only with base-sdk-made signatures, not Core verdicts.
- **A1:** I agree the crossing itself does not need oracle support: it is a race, and D12 is right that the entry
  has no faithful point to flip at. Legacy *acceptance* does need Core coverage, though.
- **Fix:** a capture on a regtest chain with V19 activated after the first DKG cycles
  (`-testactivationheight=v19@…`). This needs no ABI change.
- **Schedule:** with the D17 follow-up capture, so one new capture closes F4, F5/M2 and F6.

### F7. Nit (test): reject-path presence is asserted by kind prefix only

- **Where:** `node-l1/tests/l1_llmq.rs:2120` (`outcomes` keeps the text before `(`); `:2247`, `:2342`, `:2423`.
- **Problem:** I instrumented a copy. Today every path is reached:
  - `isdlock`: `TriviallyInvalid` (oversized included), `UnknownCycle`, `InvalidCycle`, `MalformedSignature`, and
    `NotVerified` both ways;
  - `qsigrec`: `UnknownType`, `InactiveQuorum`, `MalformedSignature`, `NotVerified{invalid}`;
  - `clsig`: `NotVerified` both ways.

  The asserts would still pass if a fixture change dropped all but one `Refused` status.
- **Fix:** assert each expected `LockStatus`, and `NotVerified { invalid: false }` where reachable. For
  `qsigrec`, Core's `PASS_NO_QUORUM` is unreachable with consistent state; say so in a comment.

### Nits

- **N1 (citations):**
  - `oracle_llmq.cpp:1025` (pin) cites `llmq/quorumsman.cpp:788`; the `SelectQuorumForSigning` call is `:790`.
  - `oracle.h:1757` and `oracle_llmq.cpp:1492` cite `evo/specialtxman.cpp:257` for `CheckLLMQCommitment`'s
    `Verify(checkSigs=false)`. That is the case label: the call is `:258`, and the `Verify(…, false)` itself is
    `llmq/commitment.cpp:301`.
  - `MinedSignatureChecksPass`'s moved comment (`oracle.cpp:1494`, pin) cites `blockprocessor.cpp:282-290`, which
    is neither the rc.3 lines (`:267-283`) nor the pin's, while the notes say references are rc.3.
- **N2 (trailing bytes):**
  - The doc at `l1_llmq.rs:2041` says "Core's deserializer … leaves bytes" → Unreadable. Core's handlers never
    check for leftover bytes; they process the message as read (`net_processing.cpp:5796-5797`,
    `net_instantsend.cpp:246`, `net_signing.cpp:51`).
  - `ORACLE_E_TRAILING` is the shim's refusal, so the "a byte after" case compares two refusals, a tautology.
  - Treat `Trailing` like `Argument` (no Core verdict), or drop the case. Trailing-byte parity belongs to the P2P
    decode corpus, not here.
- **N3 (ChainLock header):**
  - `oracle_llmq_verify_chainlock`'s doc should say "on a node with ChainLocks enabled", as the `isdlock` doc says
    for InstantSend (`net_processing.cpp:5795`).
  - It should also say that the unrequested-CLSIG check (`:5807-5813`, `UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE` = 10,
    `msg_result.h:21`) is peer state, outside the entry.
- **N4 (responses):** `oracle.h` documents each status but not Core's response. A column would anchor LK1:
  - `OVERSIZED`/`TRIVIALLY_INVALID`/`INVALID_CYCLE` 100;
  - `UNKNOWN_CYCLE` 1;
  - `UNKNOWN_TYPE` BanNode;
  - `INACTIVE_QUORUM` dropped;
  - `MALFORMED_SIG`: `isdlock` dropped with no score and no retry (`net_instantsend.cpp:122-126` marks the source
    in the unscored pass, and only bad messages are retried, `:341-353`); `qsigrec` BanNode (`net_signing.cpp:245-247`,
    `:265-267`);
  - a `clsig` that does not verify: 10 (`chainlock/handler.cpp:160-161`);
  - `isdlock` passes: F4.
- **N5:** `IsLockVerdict` hashes before the size check (`oracle_llmq.cpp:1038-1040`, pin); Core hashes after it
  (`net_instantsend.cpp:253-258`). It is harmless, since the hash is defined either way, but worth a comment
  if kept.
- **N6 (renumbering):** when the bundle renumbers ABI 25, the prose mentions in `oracle.h`, `ffi.rs` and `llmq.rs`
  ("ABI 25") also need updating, not only the `#define` and the smoke assert.

## Scope answers

### 1. Core's own paths, inputs, state and order

- **`clsig`** (`oracle_llmq.cpp:1007`, pin):
  - The entry calls `chainlock::VerifyChainLock(params, chain, qman, clsig)` (`chainlock/clsig.cpp:253-261`). That
    is `VerifyRecoveredSig(type, chain, qman, nHeight, id, blockHash, sig)` with the default `SIGN_HEIGHT_OFFSET`
    (`quorumsman.cpp:849-855`; `quorumsman.h:229-231`).
  - The handler builds the same start block itself and calls the `pindexStart` overload
    (`chainlock/handler.cpp:154-156`; `clsig.cpp:263-270`), so the two are identical.
  - The read is eager: `vRecv >> clsig` (`net_processing.cpp:5796-5797`), the same object as `ReadMessage`.
  - Inventory hash: `::SerializeHash(clsig)`, as at `:5798`.
  - The pass quorum is a second `SelectQuorumForSigning` at the same state. This is deterministic, and
    `VerifyRecoveredSig` selects the same quorum (`quorumsman.cpp:790`).
  - The refusal below `INT_MIN + 8` matches Core's `signHeight - signOffset` (`:717`), and is documented.
  - The best-ChainLock pre-filter (`handler.cpp:147`), the seen map and GETDATA authorisation are node or peer
    state, documented as D7/D11.
- **`isdlock`** (`:1034`):
  - Order and checks are `NetInstantSend::ProcessMessage`'s (`net_instantsend.cpp:253-279`): size, then
    `TriviallyValid`, then cycle resolution through the block index (`ResolveCycleHeight`, `:74-88`; the cache only
    holds indexed heights), then the boundary.
  - For the one lock, it runs `BuildVerificationBatch` (`:116-160`):
    - the lazy `sig.Get()` validity, before the request id;
    - sign height `cycleHeight + dkgInterval - 1` once `cycleHeight + dkgInterval < tip`, else `-1`;
    - the quorum from `SelectQuorumForSigning(…, id, signHeight, offset)` when rotated, else
      `GetQuorum(type, cycleHash)`;
    - `SignHash{type, quorumHash, id, txid}`.
  - Pass 2 (`pass_status[1]`, offset `dkgInterval`) runs only when pass 1 did not verify, as
    `ProcessPendingISLocks` retries a lock (`:339-353`). Whether the one-lock reduction is exact: see manager note.
  - Substitutions:
    - the tip height for `m_is_manager.GetTipHeight()` (D5);
    - `UNKNOWN_CYCLE` only on a genesis-start context, else `ORACLE_E_CONTEXT` (D8);
    - "a node with InstantSend enabled" (`:243`).

    All three are documented.
  - The recovered-sig shortcut (`:131`) is node state (D11).
  - The lazy signature captures the call's scheme: `ScopedBlsScheme` wraps `compute`, where the message is built
    (`oracle_llmq.cpp:686-689`; `bls/bls.h:409-411`, `:477-481`, `:500`).
- **`qsigrec`** (`:1086`):
  - The type check is `net_signing.cpp:56`.
  - Then `IsQuorumActive` and `GetQuorum` (`llmq/signing.cpp:429-445`). The pending pass repeats both at the same
    tip (`:549-561`).
  - Then the lazy read (`net_signing.cpp:245-248`) and the check over `buildSignHash` with the quorum's key
    (`:250-251`). The entry's `SignHash{type, quorum->qc->quorumHash, id, msgHash}` is the same, since `GetQuorum`
    returns the commitment for that base hash.
  - The order matches Core's, including no quorum before a malformed signature.
  - The seen-hash, pending-reconstructed and backpressure checks (`:435-471`) are node state (D11).
- **Single-message substitution for Core's batch verifier (D4):** see manager note.
- **Mined** (`:1478`):
  - Stage 1 is `CheckLLMQCommitment`'s `Verify(…, false)` (`commitment.cpp:301`).
  - Stage 2 is `ProcessBlock`'s queued checks, on a worker-less `CCheckQueue`, and only `Wait()` decides
    (`blockprocessor.cpp:267-283`), as in Core.
  - The documented refusals (null commitment, unknown base, members domain) and the unconfigured-type stage
    difference are accurate.
  - One coverage point: see manager note.
- **Read and check scheme:** one `legacy_bls` sets both (D12). This is a documented simplification; F6 covers the
  coverage consequence.
- **Error mapping:** `DecodeError` and `TrailingError` both derive from `ArgError`, so `Stateful` rethrows them
  without poisoning the context (`oracle_llmq.cpp:690-693`, pin). `WithLlmq` maps them before `ArgError`
  (`:637-645`). `UnservedRead` does not poison either.

### 2. The differential

- It compares, on every case:
  - the outcome class;
  - the accepting quorum;
  - the inventory hash, wherever both sides read the payload.

  For `qsigrec` and `clsig`, the classes equal Core's responses. For `isdlock`, the reject classes map one to one
  onto Core's scores, except the per-pass ban decision (F4) and the failing pass's quorum (F3).
- A wrong accept or reject, a wrong reject reason, or a wrong hash on either side fails it.
- The no-verdict allowances (legacy only, Core not verifying) are tight.
- **Seeds and captures:**
  - Every reject path is reached (F7).
  - The active window's lower edge is covered (D19); the upper edge is not (F2).
  - V19 crossings are out by design. Legacy acceptance is uncovered (F6).
  - Rotation is uncovered (D17; F4, F5).
- **Mutants** (Rust, `dash-evo/src/locks`, run against the four differentials, plain pin):

| Mutant | Site | Differential | dash-evo unit tests |
|---|---|---|---|
| D19 (owner): `keep_old_connections` → `signing_active_quorum_count` | `recsig.rs:116` | **killed** (`qsigrec 9 as captured`) | (not rerun) |
| M1: `keep_old_connections + 1` | `recsig.rs:116` | **survives** (F2) | killed |
| M2: sign height `<` → `<=` | `mod.rs:611` | **survives** (F5) | **survives** (all 174) |
| M3: no `dkgInterval` boundary check | `mod.rs:603` | **killed** (`cycle off a boundary`) | — |
| M4: `qsigrec` signature read before `IsQuorumActive` | `recsig.rs:158-167` | **killed** (`quorum hash flipped`, legacy) | — |
| M5: ChainLock offset `8 - 1` | `mod.rs:428` | **survives** (F3; killed once 162/163… are added) | killed |
| M6: cycle lookup before `TriviallyValid` | `mod.rs:589-599` | **survives** (F5) | killed |

### 3. ABI 25 bookkeeping

- `pins.txt` v24.0 names `808f56f7d2` on `node/o2a-lq3-oracle-v24.0`.
- The sha256 of both cached archives equals the pin: plain `f0e49040…f5948`, fuzz `659c0b35…8c97b`.
- Both MANIFESTs name the same commit and branch, and both archives' `oracle.h` equal the Core head byte for byte.
- All four `oracle_v24_0_llmq_verify_*` symbols are exported.
- `ORACLE_ABI_VERSION` 25 = `ffi::ABI_VERSION` = the smoke assert; there are no other numeric sites.
- `OracleLockVerdict` matches the C struct (108 bytes, asserted on both sides).
- DEC-238 reserves 25 for O2a-LQ3, and D2 says what its amendment says.
- **SB4 regen:** I diffed all 54 vectors structurally (old against new, gunzipped JSON).
  - Only `provenance.core_commit` (`5c9e9117a2` → `808f56f7d2`), `binary_sha256` and the 19
    `archives_sha256` entries change, in every vector.
  - The only other changes are the two `instructions_u` values: dgw 492,352,743 → 492,316,321 (−0.0074%) and
    kgw 871,114,126 → 870,210,692 (−0.1037%).
- The fixture's `.captures.json` is byte-identical to `dash-evo/tests/data/lq3a-regtest-captures.json` (D19).

### 4. Public-text hygiene

- rdc: clean (F1).
- Core: F1; see manager note.

## What I ran

All runs used `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12` and `DASH_ORACLE_CACHE=/work/cache/oracle`, with no
`ALLOW_UNPINNED`. Each run had its own detached worktree and its own `CARGO_TARGET_DIR` under
`/work/scratch/o2a-lq3-r1-opus` (0700). Load was 30–150.

- **Plain pin:**
  - `cargo test -p dash-oracle-sys --features oracle`: 22 binaries, 215 passed, 0 failed (including
    `sb4_core_scenarios`, `smoke`, `llmq` 30/30).
  - `cargo test -p node-l1 --features oracle --test l1_llmq`: 25/25.
- **Fuzz pin:** the same two commands, with `DASH_ORACLE_FLAVOR=fuzz` and
  `RUSTFLAGS="-C link-arg=…/libclang_rt.fuzzer_no_main-x86_64.a -C link-arg=-lstdc++"`. A first attempt without
  the libFuzzer runtime failed to link (`__sanitizer_cov_*`), which is my harness, not the branch. Result:
  `dash-oracle-sys` 22 binaries, 215 passed, 0 failed; `l1_llmq` 25/25. The `l1_llmq` binary has 2115 `__sancov`
  symbols.
- **Wall-clock failures:** none in either flavour, so nothing needed a rerun.
- **Mutants:** M1–M6 and the owner's D19 mutant, against the four O2a-LQ3 differentials. I also ran
  `cargo test -p dash-evo --lib -- locks llmq` for M1, M2, M5 and M6, and the full `cargo test -p dash-evo` for M2.
- **Scratch-only checks:**
  - the M5 kill with boundary heights added;
  - the coverage instrumentation of `Tally` (F7).
- **Static checks:**
  - pins against the archives, MANIFESTs, `oracle.h` and `nm` exports;
  - the SB4 structural diff;
  - the hygiene grep over both diffs and all seven commit messages, and per commit;
  - Core rc.3 reading of `chainlock/*`, `instantsend/net_instantsend.cpp`, `llmq/{signing,net_signing,quorumsman,blockprocessor,commitment}.cpp`,
    `evo/specialtxman.cpp`, `bls/*` and `dashbls` `schemes.cpp`;
  - the pin base's diff from rc.3 in those files, which is oracle hooks and traces only and does not change any
    verdict these entries read.
