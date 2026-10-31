# BS11a-2 Sol review, round 2 (crypto/consensus)

Heads: rdc `1db9d8af7` (`node/bs11a-2-t15`), Core `2a26d31d21` (`node/bs11a-2-t15-oracle-v24.0`). Scope: the Opus nit fix from rdc `5776f5a86` / Core `88c3e58dc2`, and the train-15 oracle union. I reviewed the detached rdc worktree in `/work/scratch/bs11a-2-r2-sol/rdc/union`; no reviewed branch was edited.

## Findings

### m1 (Minor): pre-guard returns still carry relic's code into the next call

- **Where:** Core `src/test/oracle/oracle.cpp:1355` (`WithContext`), `src/test/oracle/oracle_chain.cpp:1224-1230,1259,1332-1336`, `src/test/oracle/oracle_llmq.cpp:600`, and `src/test/oracle/oracle_session.cpp:499-508`. The guard checks only after an entry reaches `Guarded` (`oracle_internal.h:80-96`). Some metadata and free entries also never run a guard (`oracle_chain.cpp:1279-1289,1330`; `oracle_llmq.cpp:1007-1014`).
- **Scenario:** On the pinned union archive, `oracle_test_bls_set_code(1)` followed by `oracle_chain_step(nullptr, …)` returns `ORACLE_E_ARG`; the subsequent malformed `oracle_bls_exec` returns `ORACLE_E_EXCEPTION` instead of its clean `ORACLE_E_ARG`. The same sequence holds for `oracle_credit_pool_step` with a null context and length 1. `oracle_chain_free(nullptr)` even returns `ORACLE_OK` with the code still set. I ran a standalone C probe linked to the pinned plain archive: `set=0, chain_step_null=-4, next_bls_exec=-2, clean_bls_exec=-4`; the credit-pool and null-free cases likewise left `-2` for the next call.
- **Impact:** This is outside the new tests' valid-call paths and does not turn a later BLS verdict silently: the next guarded call notices the code. It still breaks D42's claim that every entry clears it and can blame the next input for the earlier state. Put argument checks inside the outer guard for status-returning entries, or explicitly drain the thread code on early returns. Add a seeded-code test for invalid chain and credit-pool inputs and the free/null boundary.

### n5 (Nit): a reported `bls_exec` code replaces an unrelated error message

- **Where:** Core `src/test/oracle/oracle.cpp:1678-1683`: `RelicCallEnd(kReported, ret)` calls `TakeRelicCode`, which writes `g_last_error`, but returns `ret` unchanged.
- **Scenario:** The existing `a_code_left_when_the_call_ends` test injects code 1 in a DECODE and appends a bad op. It correctly receives `ErrorKind::Argument`, but a temporary assertion that the message contains `bad op` fails: the actual message is `relic's code is 1 when the call ends`. The op record already reported the relic code. Clear it without calling `Fail` under `kReported` when preserving an earlier return status; keep the present error message.

## Checks and focused conclusions

- **m1 fix:** For calls that enter `Guarded`, `RelicCallStart` reads and clears a pre-set per-thread `core_ctx->code` before the body, and `RelicCallEnd` clears a code left by any return or exception. `bls_exec` keeps the code sticky across ops and records it before the end clear. A valid `bls_verify` or `bls_decode` with a pre-set code now returns `E_EXCEPTION`, not a false verdict. The new chain session uses `Hooked` → `Guarded`; `credit_pool_step` uses `WithContext` → `WithNetwork` → `Guarded`. The exception is the pre-guard boundary in m1.
- **Three regressions:** At rdc `c2b66096c` with its pinned `88c3e58dc2` plain archive, `a_code_left_on_the_thread_fails_any_entry_point`, `a_left_code_turns_no_verdict`, and `a_code_left_when_the_call_ends` all fail (7/10 `bls_relic` pass). In the same detached test worktree with only `pins.txt` temporarily pointed to the cached `63d88bfe4f` archive, all 10 pass. The original `pins.txt` was restored. At the union pin, all 10 pass again.
- **n1 build guard:** The union plain and fuzz MANIFESTs name recipe SHA-256 `ff44f2e2…`, matching `contrib/node/build-oracle.sh`; their 83-symbol wrap sets are byte-identical and include `ep_set_infty` and `ep2_set_infty`. The recipe sweeps undefined relic text symbols in every linked non-relic, non-shim archive (`build-oracle.sh:303-311`), including references produced by indirect function pointers, macros, and inlined dashbls headers when those references remain external. `throwsites.py` separately audits reachable indirect transfers and `RLC_THROW` in expanded macros/headers. I found no unwrapped call at this pin. This guard is about linked undefined symbols; fully inlined code has no call to wrap, and the shim is explicitly exempt.
- **n2 one run:** The Rust binding reserves 8 KiB per logged op (`context.rs:870-875`), enough for the logged scripts tested here; `E_BUFFER` still retries exceptionally large logs. This changes no BLS verdict. A process's first cold legacy hash-to-G2 log can now include the two one-time curve constants which the old discarded run warmed; Core has the same first-call history, and the documented comparison precondition is to warm both processes before comparing logs.
- **Union:** `git show --remerge-diff 2a26d31d21` reports only the `ORACLE_ABI_VERSION` conflict in `oracle.h`, resolved to 22. The BLS fix is unchanged. `WithContext`, `WithLlmq`, and both session `Hooked` paths take `ProcessStateMutex` before `cs_main`; the LLMQ block reader and credit-pool disk reader are thread-local, and the credit-pool scope runs under the same process lock. I found no added lock inversion or reentrant call between the readers. The merge's session and credit-pool code is reachable and builds in the pinned archive.
- **Pins and SB4:** Cached plain and fuzz archive SHA-256s equal `pins.txt`: `3c4295f9…` and `83cffa5a…`. In the 54 changed SB4 gzip JSON files, 52 differ only at `provenance`; the other two differ only at `vector.instructions_u` (dgw `493543371 → 492410849`, −0.23%; kgw `870238313 → 871387029`, +0.13%). Both are within the stated 0.5% tolerance.
- **D46 race:** `configure.ac:889,895` generates and post-edits `relic_conf.h`; `src/Makefile.am:113` starts dashbls as a parallel sub-make without ordering every Core object that includes that header. A compile that opens it in the missing interval fails. `build-oracle.sh:245-246` propagates that failure, and its archive publication occurs only after a successful compile, link, and pinned SHA check. A failed build cannot silently publish different bytes.

## Tests

- Pinned union archive: `cargo test -p dash-oracle-sys --features oracle --test archive_check --test bls_relic --test smoke` — 8/8, 10/10, 18/18 pass.
- `cargo test -p node-l1 --features oracle --test l1_bls --test l1_llmq` — 5/5 and 17/17 pass.
- Standalone C pre-guard probe linked to the pinned union plain archive, and a temporary Rust error-message assertion in the detached worktree. The latter failed as described above; its edit was restored.
- A fresh plain-flavor rebuild was attempted in an isolated cache, then stopped during its dependency build under heavy shared load. It produced no archive, so I make no independent rebuilt-byte claim.

**Verdict: APPROVE WITH NITS.**
