# BS11a-2 Opus-h review, round 1 (crypto/consensus)

**Verdict: APPROVE WITH NITS.** No Blocker or Major. All five Sol r1 items are fixed as described (D35–D38, rev7.3). I found one Minor, a latent gap in focus 1: relic's code can leak out of `oracle_bls_exec` into later calls. It can't fire on any pinned path today. There are also four nits. Every pinned-path BLS result, call log and vector I checked matches Core.

Heads reviewed: rdc `node/bs11a-2` 5776f5a86 (`c1c91e4f6..5776f5a86`, without the design merge aa8e581e6), Core `node/bs11a-2-oracle-bls-v24.0` 88c3e58dc2 (`37b042eb11..88c3e58dc2`), base-sdk `node/bsdk-bs11a-2-relic-vectors` 956fde3.

## Sol r1 items

| Sol r1 | Status | Evidence |
|---|---|---|
| 1. D26 bypassed by a null script | **Fixed** | Core `oracle.cpp:2879-2886`: the entry check now runs under `ProcessStateMutex` before the null-script `E_ARG`. `oracle_test_bls_set_code` seeds the code. `bls_relic.rs:499-512` covers both a valid and a null script, and it passes. |
| 2. Indirect calls not guarded; clang preprocessed with gcc | **Fixed** | `throwsites.py:62-93,328-334` counts each `*` transfer with no GOT relocation. A reachable function with indirect transfers that `indirect.tsv` doesn't list fails the check, and so does a listed row that has none. `build-oracle.sh:305-312` passes the flavor's `${cc}`. I disassembled the 88c gcc `librelic.a` and found 11 indirect transfers in total; only 6, in the 4 audited functions, are reachable, and the 3 `notrack jmp` jump tables are in unreachable functions. gcc and clang `-fdirectives-only` both keep `#define` lines and line numbering (I checked this; D21's old premise no longer holds), so the per-flavor read is sound. |
| 3. Log comparison precondition | **Fixed** | `oracle.h` ABI 19 notes (the warm-up paragraph) and base-sdk `PROVENANCE.md` "Call logs". The only other `call_once` in the BLS path (`bls.cpp:395`) creates the secure allocator and makes no relic calls, so D30's two constants are the whole process-history dependence. |
| 4. P1/P4 inferred, not observed | **Fixed** | `relic.json5` row notes and `PROVENANCE.md` "Observed and inferred" + raw probe sha. |
| 5. Design throw table stale | **Fixed** | rev7.3 383b37e7d §3.3. The counts add up to the cached reports: 128+9+13+6+13+14+4 = 187 (gcc) and 120+…= 179 (clang). The four macro-body rows are in, and the three `#if`-dead rows are out. |

## Findings

### m1 (Minor): a code the shim reports is never cleared, so it leaks into the thread's next oracle call, and other entry points turn it into a verdict silently

- **Where.**
  - Core `src/test/oracle/oracle.cpp:3315-3317`: the record reads the code and leaves it set (D2, D18).
  - `oracle.cpp:3326`: `oracle_bls_exec` returns with that code still on the thread. The failing exits (`ScriptError` → `E_ARG`, `ios_base::failure` → `E_DECODE`, an unlogged throw, a lost log entry) also leave any code an earlier op in the same call set.
  - `oracle.cpp:1633`: the only clear at the end of a call is `RelicInjection`'s, and it clears only an injected code.
  - `oracle_bls_decode` (`:2769`) and `oracle_bls_verify` (`:2808`) do not run the D26 entry check.
- **Failure scenario (reproduced).** I linked a 40-line C probe against the pinned plain archive (`/work/scratch/bs11a-2-r1-opus/leak/leak.c`). On one thread:
  - An `oracle_bls_exec` script signs under sk = 1 (basic). The records have code 0, and `oracle_bls_verify(0, pk, sig, msg)` returns `ORACLE_OK`.
  - Then `oracle_test_bls_set_code(1)` stands in for an op that reported code 1 in its record. Now `oracle_bls_verify` on the same valid triple returns **`ORACLE_REJECT`**, and the next call returns `ORACLE_OK` again.
  - After seeding again, `oracle_bls_decode` of the valid public key returns `ORACLE_OK` with **`valid = 0`**.
  - In each case dashbls's `CheckRelicErrors` consumes the stale code and Core's wrapper catches the throw, so a wrong verdict comes back with no error.

  Once the leaked code reaches another `oracle_bls_exec`, D26 fails that later call instead. Either way the effect lands on the next input, not the one that set the code. A fuzz or diff consumer (BS11a-3/4, `diff_bls_decode`) then records a spurious divergence on input N+1 that won't reproduce alone. That is precisely the D8 violation the log exists to catch, with the blame shifted to the wrong input. On an `E_BUFFER` retry (`context.rs:841`), the retry fails instead of returning the records.
- **Why only Minor.** At the pin, no normal input sets a code: the static table classes every reachable site, and every record in `bls_relic.rs` and `relic.json5` has code 0. The injection hook clears what it sets. So this is an assurance gap, not an observed misresult.
- **Fix.**
  1. Keep the code sticky across ops within one script, which is Core-faithful.
  2. On every exit from `oracle_bls_exec`, after the last record is built and also on the error returns, write `core_get()->code = RLC_OK`. Each op's record has already reported the code.
  3. Give every entry point that reaches relic the D26 entry check, or at least `oracle_bls_decode`, `oracle_bls_verify` and the payload/LLMQ entry points that decode or verify BLS: a code already set fails the call with `E_EXCEPTION` and is cleared.
  4. Test it: `bls_exec_after_code`-style seeding, then `bls_verify` must give `Exception`, not `REJECT`.
  5. Amend D2/D26 and the `oracle.h` ABI 19 note ("read without clearing") to say the code is sticky within a call and cleared at its end.

### n1 (Nit): the drift guard checks only the declared callers

- **Where.** `build-oracle.sh:286-297`. The wrap set is U(`libdashbls_la-*.o` ∪ `libbitcoin_consensus_a-bls.o`) ∩ T(`librelic.a`).
- **Gap.** Other Core objects linked into the archive reference relic directly through inlined dashbls headers. At the pin, 20+ `libbitcoin_node_a-*.o` objects reference `ep_set_infty` and `ep2_set_infty`. I swept every `src/*.a` of the 88c build: these two are the only relic symbols referenced outside the callers, and both are in the set, so nothing is missed today.
- **Failure on a later pin.** A Core object outside the callers starts calling a relic symbol no caller uses. That symbol isn't wrapped, isn't logged, and isn't a throwsites root, and no check fails.
- **Fix.** Fail when any linked object's U ∩ T(relic) is outside the wrap set (the shim's own `bn_*` calls can be allowlisted).
- **Defense in depth.** `throwsites.py:46`'s `RELOC` ignores `R_X86_64_REX_GOTPCRELX` and `R_X86_64_64`. At the pin these name only data (`core_thread_initializer`, `core_init_ptr`), and the indirect-call audit covers any function-pointer use. Still, adding them as edges costs nothing.

### n2 (Nit): logged scripts always run twice

- **Where.** `dash-oracle-sys/src/context.rs:841` sizes the buffer at `script.len() * 36 + 256`.
- **Effect.** A logged SIGN is about 37 script bytes, but its record carries about 144 log entries (roughly 2.5 KB), so nearly every LOG script takes the `E_BUFFER` path and runs twice. That is correct, but it doubles the cost of every logged fuzz or selftest call. It also means the first, discarded run is what warms the D30 constants.
- **Fix.** Size by the LOG ops (a few KiB each).

### n3 (Nit): BS11a-2-notes D12's counts are superseded

- **Where.** D12 still says "373 reachable functions".
- **Why it's stale.** D36 and rev7.3 say 375 (gcc) and 372 (clang), after the audited indirect edges.
- **Fix.** Mark D12 "superseded by D36" so the notes don't carry two counts.

### n4 (Nit): D41 should say what moved the dgw count

- **Claim.** D41 lists the dgw `instructions_u` move without explaining it.
- **What I checked.**
  - The SB4 binary links no shim symbols, and Core's change is confined to `src/test/oracle/`.
  - Decompressing all 54 changed vectors: apart from `provenance`, only the two bench `instructions_u` values differ (dgw 491919306 → 493543371, kgw 870400641 → 870238313).
  - Rerunning the dgw bench (`bench-pnbh 2000000 2000`, load ~115) on the 88c binary gave 493529281, 492940840, 492718318 and 492351920. That is a 0.24% spread within one binary.
  - The 13f928f69a binary is the one whose regen committed 491919306, yet it now gives 493354998 and 493139693.
- **Conclusion.** The same pre-change binary moves as much as the new one, so the +0.33% is run-environment and heap-layout noise of the kind `regen.py:63-64,254` already documents. It is within `BENCH_TOLERANCE` (0.5%) and is not caused by the pin.
- **Fix.** Say "noise, not the pin" in D41.

## Focus areas

1. **ABI 19 C surface.**
   - Arguments are validated: reserved flag bits are checked per op, PAIR is allowed only on 8/9 and INPLACE only on 12–15, register kinds are checked, and a PAIR list other than two registers or an invalid PAIR operand is refused with `d` emptied.
   - The Rust walker (`context.rs` `bls_script_logged`) matches Core's operand order op by op, including the INPLACE receiver placement for DH, SK/PK_SHARE and RECOVER_SIG.
   - The thread-local trace, the depth counter (`_Thread_local` in `relic_wrap.c`), the injection, and `lost` → `bad_alloc` are per thread. TLS relocations confirm relic's `core_ctx` and `first_ctx` are per thread, and the process-state mutex covers the scheme flag.
   - The leak between calls is m1. I found no leak between threads.
2. **Element ops vs Core.**
   - EQ is `fValid == r.fValid && impl == r.impl` (`bls.h:77-80`). IMPL_EQ is the dashbls `operator==` on the impls. ISVALID gives both bits separately.
   - ADD/NEG/MUL act on the impls whatever the validity, and return a fresh valid wrapper with no cached hash, as `oracle.h` states.
   - SER_HASH caches in the register. The tests show INPLACE and PAIR results don't inherit a cached hash.
   - Nothing conflates validity and element (D69). The base-sdk check `is_valid` → `01 ‖ usable` applies only to decoded operands, where the wrapper bit is 1 by construction.
   - P1–P5 and the fixed G1/G2 rows match Core: every code is 0, and the logs are empty only for decode.
   - I re-derived the `fp2_srt:83,106` and SSWU/SvdW `unreachable_arith` arguments; they are correct for p ≡ 3 mod 4 with non-residue −1. The 9 `no_memory` sites are `RLC_ALLOCA` null checks, which can't fire under `ALLOC = AUTO` (alloca).
3. **throwsites.py soundness.** Sound at the pin: direct and relocation edges, clone folding, the same-file inlined-static fixpoint, macro-body ownership through nested macros, and fail-closed handling of unaudited indirect transfers, header sites, unbalanced braces and unused table rows.
   - The audit table is hand-maintained, but the pinned `map_fn` and `lzcnt_ptr` targets match the source.
   - relic's real build defines only `-D_GNU_SOURCE` (plus `HAVE_BUILD_INFO` and the gmp include) on Linux, so the script's flags match.
   - The only compiler conditionals are in `arch/lzcnt.inc`, which has no throw sites.
   - Remaining gaps: n1.
4. **Call-log determinism.**
   - Comparing logs across processes is safe once each process is warmed by one legacy hash to G2, and that is the only history dependence I found.
   - Codes and outputs are independent of history, except that a leaked code (m1) makes the next input's outcome depend on the previous one.
   - The E_BUFFER rerun (n2) is invisible to the rule.
5. **SB4 regen.** Explained by the pin only through provenance; the `instructions_u` move is noise within tolerance (n4).
6. **base-sdk 956fde3.**
   - The commit is signed (`%G?` = G), its subject is 64 chars, and its body is 3 lines under 73 chars, with no Claude trailer.
   - I built my own probe from `build.sh` against the pinned plain archive (ec49447f…) and ran `gen.py --only relic`. The output is **byte-identical** to the committed `relic.json5` (sha256 2edcf2e5…), which matches PROVENANCE.
   - The worktree stayed clean.

No possible security note: nothing here concerns Dash Core itself. m1 is the oracle harness's call hygiene.

## What I ran

- Detached worktree `/work/scratch/bs11a-2-r1-opus/rdc` at 5776f5a86, with its own `CARGO_TARGET_DIR` (reflink-seeded) and mode 0700.
- `cargo test -p dash-oracle-sys --features oracle` against the installed 88c3e58dc2 plain archive: **all suites pass**, including `bls_relic` 8/8, `contextual` 20/20, `llmq` 28/28 and `smoke` 18/18.
- `cargo test -p node-l1 --features oracle --test l1_bls --test l1_llmq`: **5/5 and 13/13 pass**.
- Archive SHA-256s match `pins.txt`. The MANIFEST `relic_tools` hashes match the branch's `wrap.py`, `throwsites.py`, `throwsites.tsv` and `indirect.tsv`. The plain and fuzz wrap sets are identical (83 symbols).
- `objdump -dr` of the 88c `librelic.a` for the relocation and indirect-transfer census, and an `nm` sweep of every Core `src/*.a` for relic references.
- gcc and clang `-fdirectives-only` behaviour test.
- C leak probe (m1).
- Rebuilt the base-sdk probe and regenerated `relic.json5`.
- SB4 JSON diff of faf237227 and dgw bench reruns (n4).

The pinned archives in `/work/cache/oracle/88c3e58dc2…` were not touched.
