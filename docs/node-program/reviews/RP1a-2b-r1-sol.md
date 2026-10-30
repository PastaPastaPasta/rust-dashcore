# RP1a-2b round 1 review

Verdict: ACCEPT-WITH-NITS

Counts: M=0, m=1, n=2

## Findings

### m-1 — `ScriptToAsmStr` has no Core sighash-decoding mode

- **Location:** `dashd-rpc/src/core_write.rs:142-177` (`script_to_asm_str`), which always implements `ScriptToAsmStr(script, false)` and exposes no `fAttemptSighashDecode` equivalent.
- **Core:** `src/core_write.cpp:107-157` (`ScriptToAsmStr(const CScript&, bool)`), especially the `CheckSignatureEncoding`/`mapSigHashTypes` branch at `:132-151`.
- **Failing input:** a scriptSig containing the valid DER signature from `core_write/tests.rs::core_get_script_asm_vectors`, pushed with a final `01` SIGHASH_ALL byte (the test's `der_sig + "01"`), followed by the public-key push. Core with `fAttemptSighashDecode=true` prints the signature body followed by `[ALL]`; this helper can only print the complete pushed bytes as hexadecimal. No current RP1a-2b RPC calls the true mode (`ScriptToUniv` correctly uses false for scriptPubKey), so this is a helper/API completeness gap rather than a current `gettxout` mismatch.
- **Fix:** add the boolean mode and the strict-signature/known-sighash decoding branch, retaining false for `ScriptToUniv`; add Core's true-mode vectors. If true mode is intentionally deferred to transaction RPC work, state that boundary explicitly and keep the helper private to the false mode until then.

### n-1 — no between-turn regression test for the multi-read state commands

- **Location:** `dashd-rpc/src/blockchain/state.rs:208-228` and `:342-371`; the handler performs `getblockchaininfo`'s optional deployment read and `getchaintxstats`' ancestor read in separate V turns (`dashd-node/src/rpc.rs:695-745`).
- **Core:** `src/rpc/blockchain.cpp:1594-1631` and `:2074-2153`, where each command's related chain reads are under one `cs_main` scope.
- **Scenario/mutant:** arrange a test seam to publish a tip/reorg between the two V queries, then call `getblockchaininfo` with `-deprecatedrpc=softforks` or `getchaintxstats [1]`. The current tests exercise only a static fake state, so a regression that drops the tip-hash pin, the active-chain check, or the intended immutable-ancestor behavior would pass. The current implementation's hash pinning and immutable block facts appear to preserve a linearizable result, but that concurrency property is not directly protected.
- **Fix:** add a deterministic between-turn hook/test that asserts the reported tip's deployments and tx window remain tied to the final point, or consolidate the related reads into one `ChainQuery` and test that single-turn contract.

### n-2 — release-mode coins-lock behavior is not tested

- **Location:** `dashd-node/src/rpc.rs:325-341` (`coins_read`), particularly the `debug_assert!` around `try_lock`.
- **Core:** `src/rpc/blockchain.cpp:1365-1394`, where `gettxout` holds `cs_main` and reads the coins view directly.
- **Scenario/mutant:** build without debug assertions and invoke the coins callback while the cache mutex is held. The callback maps `WouldBlock` to `Aborted`, which is safe, but the only assertion that the callback runs on V is debug-only and the stopped/poisoned/contended paths have no release test. A future change could turn the release fallback into an unchecked access while debug tests still pass.
- **Fix:** add a release-compatible test seam for a contended or poisoned coins cell and assert `gettxout` returns the documented `-32500`/`Aborted` mapping; keep the debug assertion as an invariant diagnostic.

## What I checked and ran

- Compared the changed RPC handlers and chain seams with Dash Core `v24.0.0-rc.4`: `src/rpc/blockchain.cpp`, `src/core_write.cpp`, `src/script/standard.cpp`, `src/rpc/util.cpp`, `src/deploymentinfo.cpp`, and the LLMQ chain-lock path.
- Reviewed D30–D37 in `docs/node-program/designs/RP1a-notes.md` and `/work/scratch/rp1a-2b/mut-manifest.txt`; mutant 7's equivalence argument is sound because the command-spec bool check runs before the handler.
- Checked script push parsing, Solver templates, ScriptToUniv field order/presence, RPC argument type/range handling, header-only and off-chain behavior, deployment serialization, chain-lock serialization, chain reads, and stop handling. D30–D34 are explicit, evidence-backed scope masks; D37's listed expected failures are owned blockers and the DIFF session supplies the 2b coverage.
- Created the required reflink copy at `/work/scratch/rp1a-2b-rv-sol` (disk remained above the 230G floor).
- Ran `CARGO_BUILD_JOBS=8 TMPDIR=/work/scratch/rp1a-2b-rv-sol/tmp cargo test -p dashd-rpc --lib`: 187 passed.
- Ran the targeted state and core-write suites in the same copy: 13 state tests and 5 core-write tests passed.
- Ran `CARGO_BUILD_JOBS=8 TMPDIR=/work/scratch/rp1a-2b-rv-sol/tmp cargo test -p dashd-node --lib`: 165 passed, 0 failed, 3 ignored.
