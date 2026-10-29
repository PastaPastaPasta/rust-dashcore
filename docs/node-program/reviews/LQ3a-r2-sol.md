# LQ3a lock verification: independent review, round 2

**Range:** `e539aff06..dabe7b65a` (`node/lq3a-locks`, signed head)  
**Core reference:** Dash Core v24.0.0-rc.3 (`0364ed626e`)  
**Verdict: CHANGES REQUIRED**

## Findings

### Major — An InstantSend worker can use different schemes in its two passes

`dash-evo/src/locks/mod.rs:542-552` passes one `verify` context to both `islock_pass` calls. Core calls `ProcessPendingInstantSendLocks` twice, with a separate `batchVerifier.Verify()` in each call (`src/instantsend/net_instantsend.cpp:312-327,339-353`); each verification reads `bls::bls_legacy_scheme` afresh (`src/bls/bls.cpp:335`). No `cs_main` lock spans both passes. Connecting the V19 activation block flips the flag to basic (`src/evo/specialtxman.cpp:983-986`; disconnect flips it back at :1008-1012).

**Concrete schedule:** An `isdlock` carrying a real legacy signature is read and queued at tip `V19Height - 2`. Its current-set pass runs under legacy and fails because a different quorum signed it. Block `V19Height - 1` then connects; the previous-set pass runs under basic and rejects the signature, scoring the peer (`src/instantsend/net_instantsend.cpp:189-202`). With a stable view selecting that previous quorum, `verify_islock(..., LEGACY, LEGACY, ...)` accepts it. A worker batch can also contain a current-set legacy lock that Core accepts in the first pass, so choosing basic for every lock does not reproduce both results. The same race exists in reverse on disconnect. The API needs a scheme for each verification pass, or a precisely documented snapshot boundary that the node can enforce; the cited D6 quorum-view snapshot does not make Core's atomic BLS flag stable across the two calls.

### Minor — The ChainLock crossing test does not detect a swapped read scheme

`dash-evo/src/locks/tests.rs:546-551` expects `InvalidSignature` for both crossing directions. Core eagerly reads `ChainLockSig::sig` with the flag at deserialization (`src/chainlock/chainlock.h:41-44`, `src/bls/bls.h:183-209`) and verifies the retained point later (`src/chainlock/handler.cpp:154-161`, `src/bls/bls.cpp:309-311`). **Concrete schedule:** Read legacy bytes before the V19 activation block, then verify under basic after it connects. If `verify_chainlock` reads the bytes under basic instead, they become the invalid object and still yield `InvalidSignature`; the reverse crossing has the same ambiguity. I changed only `sig.get(read)` to `sig.get(verify)` in a temporary detached-worktree mutant: the named crossing test still passed (exit 0). Add an assertion that distinguishes the eager point retained at read time, or test the two stages separately. The other three crossing tests distinguish a swapped/collapsed context by their verdict or hash.

### Minor — The BLS handoff paragraph places `clsig_hash` at handler time

`dash-evo/src/bls.rs:13-16` lists `clsig_hash` with checks whose context is taken when a worker or handler runs. Core hashes a `clsig` before calling the handler (`src/net_processing.cpp:5797-5798,5814-5815`), and the eager writer reads the flag at that hash (`src/bls/bls.h:170-180`). **Concrete schedule:** Read a legacy `clsig`, connect V19 before `SerializeHash`, then disconnect it before `ProcessNewChainLock`. Core's inventory hash uses basic encoding, while its signature check uses legacy. A caller following the paragraph's handler-time wording for the hash would supply legacy as `write` and compute a different inventory hash. State separately that `clsig_hash` takes the hash-time write context; the function's own documentation already says this correctly.

## M1/M2 and call-site assessment

- **M1 closes for one verification pass.** `verify_qsigrec` decodes the lazy bytes under `read` (`dash-evo/src/locks/recsig.rs:167`) and checks the quorum key under `verify` (:177-183). Core's `CRecoveredSig` has a lazy field (`src/llmq/signing.h:74-100`); its wrapper captures the flag when constructed and uses it in `Get()` (`src/bls/bls.h:408-411,477-480,492-511`). The signing worker checks later (`src/llmq/net_signing.cpp:243-257`) under its then-current flag (`src/bls/bls.cpp:335`). Connecting/disconnecting V19 changes that flag at the cited `specialtxman.cpp` lines. The recovered signature names its quorum, so no historical sign height should choose either scheme.
- **M2 closes.** `clsig_hash` decodes under `read` and writes under `write` (`dash-evo/src/locks/wire.rs:82-89`). Core's eager `ChainLockSig` read uses the current flag (`src/chainlock/chainlock.h:41-44`, `src/bls/bls.h:183-209`); `SerializeHash` later serializes under a fresh flag (`src/net_processing.cpp:5797-5798`, `src/bls/bls.h:170-180`). The legacy/basic and basic/legacy tests check the point's corresponding re-encoding.
- **Other helpers:** `verify_chainlock` supplies `read` to the eager field and `verify` to the check (`dash-evo/src/locks/mod.rs:446-462`). Its quorum selection uses the lock height, as Core does (`src/chainlock/clsig.cpp:253-269`); that height does not determine the process flag. `verify_islock` uses `read` for the lazy field and `verify` for each check (`dash-evo/src/locks/mod.rs:531,542-552`); its existing sign-height calculation remains independent of the flag. `verify_chainlock_sig`, `verify_recovered_sig`, and `islock_pass` receive an already read signature, so their single scheme is for checking only. All direct callers of the four changed public helpers in this revision are tests; the queued-job handoff is documented for later node integration and has no production call site yet.

## Validation

- `RUSTC_WRAPPER= CARGO_TARGET_DIR=/work/scratch/lq3a-r2-sol/target CARGO_BUILD_JOBS=12 cargo test -p dash-evo`: **exit 0**; 132 unit, 7 `cbtx`, 31 `cbtx_root_history`, and 2 `sml_core_vectors` tests passed; one expected history test ignored.
- Temporary ChainLock read-under-verify mutant: `cargo test -p dash-evo --lib a_chainlock_checked_across_the_v19_switch_is_invalid`: **exit 0**, 1 passed. The mutant was restored; the detached worktree was clean afterward.
- An initial test invocation through shared sccache stalled; I interrupted it (**exit 130**) and reran without the wrapper as above. The owner-reported GATE-FAST result was not rerun here.
