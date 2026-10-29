# O2b-2 ChainSession (dash-oracle-sys): Opus-h review, round 1

Reviewed: rdc `node/o2b-2` a5d2a9c59..**4940a5d2d** (3 commits, all `%G? = G`: e4a75b3f8, 9c1e2c7bf, 4940a5d2d). I used a detached worktree with a private 0700 `CARGO_TARGET_DIR` under `/work/scratch/o2b-2-r1-opus`, `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12`, and the pinned plain archive (Core 5c9e9117a2, ABI 22, `/work/cache/oracle`). Core citations are to that pin's `src/test/oracle/oracle_chain.cpp` and `oracle.h`. I read them to check the binding's contract, not to re-review O2b-1.

## Verdict: ACCEPT WITH NITS

The binding is sound. Every `unsafe` call passes pointers that outlive the call. The buffer retry matches oracle.h's stateful-call rule. The drop order is right: `chain_free`, then the thread mark, then the lock. The poison paths are covered by a child process. Reports decode strictly, and nothing Rust-side crosses the FFI as a callback.

Two things checked out beyond Sol's rounds:
- **Cross-process determinism.** I ran one program in three separate processes (different pid, TMPDIR/datadir and ASLR heap). It covered the genesis run, save, restore with the suffix, and a fresh session from the image plus the suffix. All three gave the identical sha256 over 686,379 report bytes, with no taint.
- **Tie carrying.** A tie forced from Rust is reported exactly where oracle.h I33 says.

What remains is the contract O2b-3 will lean on:
- **F1:** the taint is a per-step signal, and nothing in the binding makes it sticky. An image doesn't record it either.
- **F2:** four implementation mutants survive. Two of them would hide F1-class mistakes.

Neither blocks the merge. F1's doc and latch should land before O2b-3a starts using the API.

## Findings

**F1. Minor: taint is per step and not sticky. Neither `ChainSession` nor `CoreImage` remembers it, so a caller can score tie-decided state by accident.**
- `chain.rs:519-523`: `ChainReport::tainted()` is `!post.tie.is_empty()` for that report only. The doc says "unscored from here", but nothing enforces it.
- Core reports each tie site once, on the op that fired it:
  - `BeginOp` takes the carried ties into the op (`oracle_chain.cpp:927-930`).
  - `DropLoadReport` carries only the ties of a load that has no report: `oracle_chain_new`'s, and save's reload (`:917-925`, oracle.h:1973-1975).
  - A restore clears the carried ties and reports its own load's ties (`:1322-1323`).
  - So the step after a tainted one shows `tie: []`, although its state is what the address order picked (`node/blockstorage.cpp:33-45`, O2b D30).
- **Probe (scratch test, reverted).** Base: the test's `world()` (no chain call), the prefix of 110 blocks, then the program up to and including `spending.submit()`. At that point `spending` (height 114) and `fork[4]` have equal work, and the tip is `fork[4]`. The results:

  | Next op | `tie` |
  |---|---|
  | `RESTART CLEAN` | `[1,2,3,4]` |
  | then `ACTIVATE` | `[]` |
  | `save()`, then `ACTIVATE` | `[1,2,3,4]` (save's reload carried) |
  | `save()`, a refused frame `[14]`, then `ACTIVATE` | `[1,2,3,4]` (the carried ties survive a refused frame) |
  | `restore(image)` | `[2,3]` |
  | `chain_session(Some(image_saved_after_the_tie))`, first step | `[1,2,3,4]` |
  | second step | `[]` |

- **What goes wrong:**
  - A comparator that *filters* tainted steps (`if r.tainted() { continue }`) instead of *stopping* scores every later step. Those steps compare tie-decided Core state with Rust's arena order, and the result looks like a real divergence.
  - A prefix image that the per-process builder (design §2.3) saved after any tie-decided step also gives no signal later. It is enough for the build's equal-work rivals to be gone by the time of the save, for example after an `INVALIDATE` that tied at site 5 and further extension. Every input restored from that image then starts from address-ordered state, and its restore report can be clean.
- **Fix (O2b-2, before O2b-3a consumes the API):**
  1. Give `ChainSession` a latch, `tainted_since: Option<u64>` (the step index), set by `step` and `restore` when `post.tie` is non-empty.
  2. Clear the latch only on a restore of an untainted image whose own report is untainted.
  3. Expose it as `ChainSession::tainted()`.
  4. Stamp `CoreImage` with the latch at `save` (`CoreImage::tainted()`). The prefix-image builder can then assert it.
  5. Reword `ChainReport::tainted()` as "this op fired a tie (or carried a load's)", and point readers to the session latch.
- **If the latch is deferred to O2b-3:** add the doc text plus a handoff line ("stop at the first tainted report; images carry no taint, so assert none during an image build"), and a test from the probe above.

**F2. Minor: four implementation mutants survive the tests. Two of them sit on the F1 path.**
- **Mutant E, `tainted()` → `false`** (`chain.rs:522`): `--lib` 6/6, `chain_session` 10/10. No test reaches a positive taint. The tests' own `untainted()` reads `post.tie` directly (`tests/chain_session.rs:309-315`). Carried ties (after new or save) are untested too.
  - **Fix:** add the probe's schedule as a test. Assert `RESTART CLEAN` is tainted with `2 ∈ tie` and the next step is not. Assert a fresh session from that image has a tainted first step and an untainted second step. Assert `restore` reports its own sites.
- **Mutant A, `normalized()` also sets `header.run = "*"`** (`chain.rs:470`): all pass. `the_version_normalizes_out_of_a_report` (`tests/chain_session.rs:584-607`) checks `ver` and `records[1..]`, but not the header's other fields. Sol's point that canonical re-encoding can't silently normalize stands for the *encoder*, not for a second assignment.
  - **Fix:** `assert_eq!(n, &Header { ver: b"*".to_vec(), ..header.clone() })`.
- **Mutant L, drop `state.chain = None`** (`chain.rs:284`): all pass. No test in the binary makes a chain call *before* a session, so the cache is already `None`. The live code would fail loudly (`ORACLE_E_NETWORK`, `oracle.cpp:282`, since `ForgetNetwork` clears `g_network_selected`), not silently.
  - **Fix:** call `chainparams_json(&Chain::regtest())` before `session(None)` in `a_session_excludes_every_other_call_on_its_thread`.
- **Mutant G, keep the refusal's NUL** (`message.truncate(message_len)`, `chain.rs:327`): all pass, because the tests only use `contains("future")`.
  - **Fix:** `assert!(!message.ends_with('\0'))`, or compare `Refused`'s text with `ReloadRefused`'s for the same image and clock (also see F3).

**F3. Nit: one load refusal can produce three different messages, depending on how it is reached.**
- `ChainStart::Refused` gets up to 16 KiB (`chain.rs:300`).
- `SaveOutcome::ReloadRefused` goes through `Oracle::last_error`'s fixed 1024-byte buffer (`lib.rs:727-731`), so it is cut at 1023 bytes.
- RESTART puts the full text in `reply.result.message`.
- Separately, Core's sporks-cache refusal embeds the session's random datadir (`oracle_chain.cpp:781`: `"Failed to load sporks cache from %s"`). That one message differs per session and per process. Core-written images can't reach it today.
- **Fix:** read `last_error` with a grow-to-fit loop (`oracle_last_error` returns the full length, oracle.h:110), or note the cap. Log the Core path as an O2b-1 rider: say "sporks.dat" without the absolute path.

**F4. Nit: `ErrorKind::Exception` from `chain_session` means two different things, and only the message tells them apart.**
- It is Core's parameter-interaction refusal (`-prune` with `-txindex`/governance, D5; a bad `-minimumchainwork` via `ApplyArgsManOptions`, `oracle_chain.cpp:785`).
- It is also a start that threw part-way, or with the `teardown failed` prefix, a poisoned pin.
- O2b-3 schedules use fixed args, so this only bites a future options fuzzer.
- **Fix:** none needed now. If options are ever generated, map the parameter refusals to `ChainStart::Refused` in the shim (`ORACLE_REJECT` plus message) at the next ABI.

**F5. Nit (handoff): during a process-lifetime session, other threads' oracle calls block with no error.**
- Design §2.3 keeps one live session and restores it per input. D1 refuses every oracle call on the session's own thread. On other threads, every call that takes the pin's lock blocks until the session drops (`chain.rs:5-13`). Under O2b-3's model that is the process lifetime, so a helper thread that reaches for an oracle codec hangs instead of erroring.
- **Fix:** add a line to the plan's O2b-3 handoff: generator hashing (X11, merkle roots, txids) must come from `dashcore` or dash-consensus primitives on every thread, not only on the session's.

## API contract for O2b-3 (scope 1)

- **Errors vs divergences.** Each case is distinguishable without string matching, except F4:
  - Core verdicts are always `Ok(report)`: RPC errors are `reply.outcome = Err(RpcError{code: Some})`, and a refused load is `post.down` plus `result.status != "success"`.
  - A down session is `Err(RpcError{code: None, "session-down"})` with `post.down`. `Post::check` makes live fields absent exactly when down.
  - Harness refusals are `Err(Argument|Decode|Trailing)`: nothing ran, and the session stays usable (`frames_the_shim_refuses_run_nothing`).
  - A broken shim is `Err(Exception)`. It stays broken for every later call but `free` (`oracle_chain.cpp:520-548`): drop the session and start a new one from the image. Images are independent of sessions.
  - Decode or format violations panic. That is right for libFuzzer, because they are crashes in the contract.
- **Session-down after a refused frame.** A refused frame (Arg, Decode, Trailing) leaves the session up, and leaves carried ties in place (probe row 4). A refused *load* leaves it down until a RESTART or restore that loads. Both are tested, and `restore` from down works (`tests/chain_session.rs:495-505`).
- **Replay determinism across processes:** see the verdict. I measured it with 3 processes; within one process the existing tests already cover it. Nothing in a report depends on the pid or the path: the memory H line's `run` is the literal `"oracle"` (`statedigest.cpp:159`), and its `ver` is normalized (D3). The one path-bearing message is F3's.

## Soundness: `unsafe` and FFI (scope 2)

- **Buffer sizing and retry:**
  - `report()` sizes the buffer at `chain_report_bound()`. That is ≥ 1 MiB + (entries + 64) × 8 KiB, so the shim's "below bound before it runs" `E_BUFFER` cannot fire (`oracle_chain.cpp:1062-1083`).
  - The after-run `E_BUFFER` is retried by `with_buffer` with the same frame (step) or the same `&CoreImage` (restore; the shim's pending key is the image's address, `:1314-1317`).
  - The borrow spans both calls, and no other handle call sits between them, so the pending output can't go stale or be served to a different image at a reused address.
  - A broken session's bound is 0, which gives 16 bytes and then `Exception`. That's fine.
  - `chain_new`'s refusal `E_BUFFER` re-runs a full start. oracle.h allows that, because no session is made, and 16 KiB makes it practically unreachable.
- **Pointer validity:**
  - `argv` points into `args: Vec<CString>`, alive across both `new` calls.
  - The image is borrowed for the call.
  - `out`/`len` point to locals.
  - `save`'s image is owned (`NonNull` → `CoreImage`) before the status is read, so the error paths free it. That includes `Hooked` turning a completed save into `E_EXCEPTION` after `*out` was set (`oracle_chain.cpp:541-548`, `:1305`).
- **Drop order:**
  - `ChainSession::drop` calls `chain_free`, poisons on non-OK, then drops the fields in declaration order: `_hold` (restores the thread mark), then `state` (releases the lock). A poison is therefore set before any waiter can take the lock.
  - Images are plain maps (`oracle_chain.cpp:94-98`), and `chain_image_free` is a bare `delete`. Dropping an image before or after its session, on any thread, or after poisoning is safe. The borrow prevents a drop during a restore.
  - The pin is `'static`.
- **Std lock poisoning** (a panic while a session is live): `unwrap_or_else(into_inner)` is correct, because `Drop` already ran `chain_free` during the unwind.
- **Panics across FFI.** No Rust function is passed to C in `chain.rs`. Every shim entry runs under `Guarded`/`Hooked` (catch-all), except `chain_image_free`, a `noexcept` destructor. `catch_unwind` isn't needed.
- **`!Send`.** `RwLockWriteGuard` is already `!Send`, and `PhantomData<*const ()>` makes it explicit. `CoreImage: Send + Sync` is justified.
- **Child-process poisoning test.** Both paths (`free`; a refused option whose teardown fails) are covered.
  - The child is found with `--exact`, gets its fault env only through `Command`, has its TMPDIR removed, and ends with `_exit` so leaked script threads don't meet static destructors. The no-poison mutant (C) kills it.
  - `--exact` filtering is load-insensitive. The 500 ms cross-thread window in `a_session_excludes…` can only pass vacuously under load, never fail spuriously.

## dash-statedigest `FileKind::Memory` (scope 3)

A memory stream can't pass as, or be mistaken for, a golden or recording file:
- `check_recording` scans each of the three files with its expected kind and rejects an `H` of any other kind (`recording.rs:65-79`).
- Golden handling keys on `== FileKind::Golden` only (`conformance/src/digest/dump.rs:348`).
- `extract` demands `Digest` (`extract.rs:410`).
- Core writes no golden lines (golden is extract-only, and the memory sink takes the digest and decisions writers only, `statedigest.cpp:79-88,124-125`), so `Memory` holding `H C R D A` (no `S`) is exact.
- `conformance digest compare` would accept a memory file as an input and project its outcomes. That is harmless, and arguably what triage will want.

## Checks

- `cargo test -p dash-oracle-sys --features oracle --test chain_session --lib`: **10/10 + 6/6** passed (no reruns needed).
- `cargo test -p dash-statedigest`: all suites passed (22 + 27 + 9 + 12 + 6 + 27 + 5).
- Cross-process digest: 3/3 identical, `0073e958…17d2ed`, 686,379 bytes, tainted = 0. The scratch test was reverted.
- Forced-tie probe: the table in F1. The scratch test was reverted.

## Mutants (implementation only; none of them are Sol's no-image, wrong-image or dropped-strict-field mutants)

| # | Mutant (`dash-oracle-sys/src/chain.rs`) | Result |
|---|---|---|
| A | `normalized()` also sets `header.run = "*"` (:470) | **survives**: F2 |
| B | the session's thread mark is `Held::Exclusive`, not `Session` (:285) | killed: `a_session_excludes…` (`tx_decode` ran during the session) |
| C | no poison on a failed `chain_free` (:250) | killed: `a_failed_teardown_poisons_the_pin` (+ child) |
| D | `RawReport::split` accepts trailing bytes (:455) | killed: `a_report_splits_into_four_sections` |
| E | `ChainReport::tainted()` → `false` (:522) | **survives**: F1/F2 |
| F | poison on every start `Exception` (:322) | killed: 5 tests (`Poisoned` after the `-prune` refusal) |
| G | the refusal message keeps its NUL (:327) | **survives**: F2 |
| L | `state.chain = None` dropped at the session's start (:284) | **survives**: F2 (would fail loudly with `E_NETWORK`) |

Every mutant was reverted (`git checkout`). No edits were made to the branch, and nothing was committed or pushed.
