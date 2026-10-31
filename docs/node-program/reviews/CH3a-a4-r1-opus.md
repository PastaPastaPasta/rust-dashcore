# CH3a a4 (ConnectBlock on the check queue): independent consensus review, round 1 (Opus)

| | |
|---|---|
| Reviewed | `node/ch3a-a4` at 09bdfaaf7, only the a4 delta: `git diff ab99abbe6 50119c96c` (632687d60, d90a153a8, 2143d281e, cebd0b6dc, 2558235fb, 4f01626d9, 3051a40b4, 50119c96c). 09bdfaaf7 only follow-merges docs. |
| Against | Dash Core `v24.0.0-rc.3`: `validation.cpp` (`v`) :2153-2238 `CheckInputScripts` (probe :2174, `txdata.Init` :2179, `AccessCoin` :2184, `CScriptCheck` copy :2194, exec insert :2227-2230), :2469-2935 `ConnectBlock` (`parallel_script_checks` :2480, `txsdata` :2643-2648, control :2649, H4 :2661-2669, loop :2687-2768, `vChecks`/`Add` :2749-2760, `Wait` :2778-2781); `checkqueue.h` `CCheckQueue::Loop`/`Add`/`Wait`, `CCheckQueueControl` (:205-249: `nullptr` Add/Wait, destructor); `init.cpp` :1546-1568 (cache sizing, `-par`). |
| Context | CH3 §2.4, §2.9, D10, D41, D43, D51; CH3a notes D65, D68, D71, D78-D79, D127-D131 on the branch. |
| Method | Read line by line against Core. Two `git archive` copies of 09bdfaaf7 in /work/scratch/ch3a-a4-review, each with a target reflink-seeded from the implementer's (all deleted afterwards). Clean copy: `cargo test --locked -p dashd-chainstate -p dash-consensus` all green (dashd-chainstate lib 620/620 in 672 s at load ~110, dash-consensus lib 156/156, every integration and doc test). `contrib/node/ch3a-connect-census.py --core <rc.3 tree>`: 0 failures (12 literal rejects, 3 computed, 5 AbortNode, 17 log lines). Mutant copy: 15 env-gated hand mutants (`A4MUT=n`, one build), each run against `chain::tests::connector`, `checkqueue` and `connect::serial` (75 tests), and each survivor against the whole lib (620). I did not rerun PF0 or the D131 spread measurements. |
| Verdict | **ACCEPT WITH NITS.** No Blocker or Major. The verdicts, their order and their classes match rc.3 at both -par values, and so does the cache policy. Two Minors (a serial peak-memory divergence that D129 misstates, and a test gap that PF0 does not cover either) can be fixed or recorded without another round. |

## 1. Branch-for-branch parity

Everything below matches rc.3.

- **The control** (`connect/serial.rs:591`, `:231-241`, `:648`):
  - `parallel = script_checks && queue.has_threads()` is `fScriptChecks && parallel_script_checks` (`v:2480`, `v:2649`). Without it the control is `control_inline`: no lock and no pool. Its `Add` is never reached from `ConnectBlock`, because the inline branch passes `None` (Core's `pvChecks == nullptr`), and its verdict is then `true`. That is Core's `CCheckQueueControl(nullptr)`: `Add` does nothing and `Wait` returns true.
  - H4 runs inside the scope, as `v:2661-2669` does. Nothing is queued before H4 in either implementation, so its position cannot be observed (M4 below is equivalent).
- **The verdict's precedence** (`:813-824`):
  1. A `Fatal` from inside the scope (`step?`). Core has no in-band `Fatal`: a coins read error throws and aborts, and the flags assert aborts too. Ranking it first is the only choice that never turns a fault into a verdict. The control has waited by then (`queue.control` drains before it returns).
  2. `Step::Stop`, a verdict before the loop ended. This is Core's `return state.Invalid(..)` from inside the loop, with the destructor's `Wait()` result dropped (`checkqueue.h` `~CCheckQueueControl`).
  3. `!all_ok`, then `ERROR: ConnectBlock: CheckQueue failed` at INFO and `block-validation-failed` / `BLOCK_CONSENSUS` with no debug message, before step 18 (`v:2778-2781`; `checkqueue/script.rs:301`).
- **Which failure is reported when several fail.**
  - Serial: the first transaction in block order, and in it the first failing input. `dash_consensus::script::check_input_scripts` stops at the first failure, as Core's `!check()` return does. This is unchanged from D68.
  - Queued: no transaction's reason can come out. `unprobed`'s parallel branch can fail only on `InputCheck::new`, and its only error is `InvalidScriptFlags` (`dash-consensus/src/script/input.rs:66-77`). That maps to `Fatal`, Core's assert. So with threads every script failure reaches `all_ok`, as in Core. A later L2–L6 failure wins (row 2) and an earlier one wins (row 3), in both implementations.
- **Copy order.** Rust copies all of a transaction's inputs (`:186-199`) before it runs any of them. Core interleaves `AccessCoin`, copy and check per input (`v:2181-2225`), so a serial Core run that fails at input 0 never reads input 1. This cannot be observed: `have_inputs` and the `prev_heights` pass (`:683`, `:718-721`) have already pulled every input into the overlay, so `copy`'s reads cannot fault.
- **Cache semantics** (`checkqueue/script.rs:193-285`):
  - The probe is `contains(entry, !cache_full_script_store)` before any coin read or `txdata` build (`v:2174`, `v:2179`, `v:2184`).
  - A coinbase returns before the probe (`v:2159`).
  - The parallel branch never inserts the exec entry (`cacheFullScriptStore && !pvChecks`, `v:2227`).
  - Every `ScriptCheck` carries `cache_sig_store` (`v:2194`, `CScriptCheck`'s `cacheStore`).
  - At the tip, `store = false` (`fCacheResults = fJustCheck`, `v:2750`) feeds both flags. a6's `just_check` maps onto it unchanged, with one wrinkle: a storing serial run then hashes the entry twice (N3).
  - Below `-assumevalid`, the `if !coinbase && script_checks` guard (`:752`) skips the probe and the copy, as Core's `fScriptChecks &&` short-circuit does.
- **Lifetimes, determinism and liveness.**
  - `slots` (`:639-643`) and `&scratch.spent` are borrowed for `'s` from outside `with_control`. `rayon::in_place_scope` returns only after every spawned helper has, including when `f` or a check panics (rayon waits for the scope's jobs before it resumes the unwind). `reset(&mut self)` at `:814` therefore cannot compile inside the scope.
  - No `unsafe` was added (`git diff … -- '*.rs' | grep '^+.*unsafe'` is empty).
  - `Bump` is `!Sync`. Only the master allocates in it, and the workers read `&[u8]` slices that bumpalo never moves.
  - `Shared` (`all_ok`) is built per control, and the inline `Cell` is per call, so no block can see another block's verdict. The control mutex's poison is ignored (`checkqueue/mod.rs:258`), and `a_panicking_check_surfaces_and_the_queue_survives` pins that.
  - Nothing inside the scope opens a second control on the same queue: the only `queue.control` callers are `serial.rs:237` and the `script_checks` bench. So the "one control at a time" rule cannot deadlock.
  - Nothing is cancelled mid-block, but Core cannot interrupt a control either.
- **CS6 (632687d60).**
  - `control()` without a pool now calls `control_inline`, which has the same body as the old inline arm.
  - `check_input_scripts` is `exec_entry` + `exec_hit` + `unprobed(.., Some(entry))`, and its insert condition (`cache_full_script_store && entry`) is the old one with the operands swapped. No existing caller's behaviour changes. The −0.11% on `script_checks` is codegen.
- **Census:** `block-validation-failed` and `CheckQueue failed` have left `EXEMPT` and are found in the code and in `queue.rs`.

## 2. Tests and mutants

`queue.rs` runs CH3 §2.4's eight rows at `-par=1` and `-par=4`, plus H8's fault after a queued failure, the tip store policy, and the probe/hit/coinbase work counts. The D71 H4 panic runs in both modes. `checkqueue/script/tests.rs` pins the probe/unprobed split and the store matrix through the pair.

| # | Mutant | Result |
|---|---|---|
| M1 | probe never soft-erases (`exec_hit`, `script.rs:234`) | killed (`a_probe_soft_erases_…`, `an_exec_hit_is_erased_once_…`) |
| M2 | a parallel run inserts the exec entry when storing (after `queue.add`, `script.rs:260`) | killed (`the_store_flags_decide_…`, `queued_checks_are_never_stored_as_executed`, `a_transaction_is_checked_and_stored_as_itself`) |
| M3 | with threads, check inline (the transaction's reason instead of `block-validation-failed`) | killed (7 queue tests) |
| M4 | H4 before (outside) the control scope | **survives**: equivalent. Nothing is queued before H4, and the D71 panic test passes either way |
| M5 | the queue's verdict before an early `Step::Stop` (after a `Fatal`) | killed (rows 1, 2) |
| M5b | the queue's verdict before a `Fatal` too | killed (H8 fault test, rows 1, 2) |
| M6 | signature-hash data built before the probe | killed (`an_exec_hit_…_builds_no_sig_data`, `a_probe_soft_erases_…`) |
| M7 | every input checked against input 0's copied script | killed (`the_tip_connect_stores_nothing`) |
| M9 | `SpentScripts::reset` never called by `connect` | **survives** in the full lib (see N1; PF0 catches it) |
| M10 | the queue's control below `-assumevalid` too (`parallel` ignores `script_checks`) | **survives** in the full lib (m2) |
| M11 | with threads, no exec probe | killed (`the_tip_connect_…`, `an_exec_hit_…`) |
| M12 | the tip connect stores signatures | killed |
| M13 | a serial tip connect stores the exec entry | killed |
| M17 | the queue's verdict ignored | killed (5 queue tests) |
| M19 | below `-assumevalid`, probe and copy anyway (no check) | **survives** in the full lib (m2) |

The implementer's four (Fatal→verdict, copy-before-probe, no cap, no grow-to-fit) overlap M5b, M6 and the arena test. I did not rerun them.

## 3. Findings

### Blocker

None.

### Major

None.

### Minor

- **m1. At `-par=1` the arena holds every spent script of the block until the block ends; Core holds one input's.**
  - The inline branch copies into the block-wide arena (`serial.rs:756-757` → `:186-199`), and `reset` runs only at `:814`. Core's serial `CScriptCheck` (`v:2194`) is destroyed at the end of each input's iteration. Pre-a4 (D68) kept one transaction's copies in a reused `Vec`.
  - D129's rationale, "Core frees its copies with the checks. An arena kept across blocks gives them the same lifetime", is true with threads (roughly: Core's workers free each batch as it runs, Rust frees all at the end). It is not true at `-par=1`.
  - The 16 MiB cap bounds what is *kept*, not the *peak*. A mined block of minimal 41-byte inputs, each spending a ~10,000-byte nonstandard output (`AddCoin` keeps any non-`IsUnspendable` script, `coins.cpp:72`), copies about 2 MB / 41 × 10 KB ≈ 0.5 GB during one connect.
  - Core's own `blockundo` holds the same scripts at that moment (`v:2765-2767`), and so does Rust's streamed undo. So this is at most about 2× Core's undo-side peak, it needs about 0.5 GB of earlier nonstandard outputs plus a mined block, and it is not consensus. It is a resource divergence in the serial mode, which is the default on the gate and in PF0.
  - Fix (either):
    - (a) In inline mode, copy into a second `SpentScripts` (or the pre-a4 `Vec` + ranges) that is reset after each transaction. The inline copies are dead once `input_scripts` returns, and the borrow checker accepts a `&mut` reset there because nothing queued borrows them. This also keeps the serial copy buffer small and hot.
    - (b) Keep the shape, and correct D129 to state the `-par=1` peak and its bound.
  - Re-measure the serial rows if (a) is taken.
- **m2. Nothing pins that a block below `-assumevalid` takes no queue control and does no probe and no copy at `-par=4` (M10, M19 survive). PF0 cannot see M10 either.**
  - `assumevalid_skips_the_queue_too` (`chain/tests/connector/queue.rs:332`) asserts only the verdict.
  - Under M10, every below-AV block at `-par>1` would allocate `slots`, take the control mutex and open a rayon scope. That is IBD's common path with the default `-par`.
  - D131 leaves out a `par4/*/av-on` row on the premise that it "repeats the serial row (no queue, `v:2649`)". That premise holds only while this code is right, so the regression would pass both the tests and PF0.
  - M19 (probe and copy below AV) would soft-erase exec-cache entries that Core never touches there. The self-review claims "below assumevalid there is no probe and no copy", but no test checks it.
  - Fix: in the skipped half of `assumevalid_skips_the_queue_too`, at both -par values:
    - Assert the exec cache's ops are `(0, 0, 0)` and `seen.scripts` is `(h, 0, 0)`. This kills M19.
    - Assert no parallel control was opened. Either add a test-only control counter on `CheckQueue`, the way `SaltedCache` counts ops, or record `parallel` in `last_script_work`. This kills M10.

### Nits

- **N1. M9 (`connect` never resets the arena) survives the unit tests.** `spent_scripts_keep_one_chunk_up_to_the_cap` calls `SpentScripts::reset` directly. PF0 would catch it: the bench's unmeasured warm connect (`connect_block_micro.rs:916-928`) leaves the arena full, so `p2pkh-500/av-off`'s gated allocs would rise. An optional cheap pin: connect the same signed block twice in `the_tip_connect_stores_nothing` and assert that the arena's `allocated_bytes` is unchanged by the second connect.
- **N2. M4 is equivalent.** `a_hook_panicking_mid_attempt_…` (`connector.rs:2356`) says that at `-par=4` "the panic unwinds through CS6's control too". That is true of the code, but the test cannot tell. No change needed. If you keep the sentence, say it describes the structure and is not pinned.
- **N3 (a6, forward). D130 says the pair "hashes the entry once". That holds for `check_input_scripts`, not for `ConnectBlock`'s split path.**
  - `input_scripts` discards the probe's entry, and `unprobed` recomputes it for a storing serial run (`checkqueue/script.rs:280`, `entry.or_else(|| exec_entry(tx, opts))`). Core hashes once (`v:2170-2172`, reused at `v:2230`).
  - It costs nothing at a4 (`store = false`). Once a6 sets `just_check`, a `-par=1` `TestBlockValidity` pays one extra SHA-256 per non-coinbase miss.
  - Fix in a6: have `exec_cache_probe` return the entry on a miss (for example `Result<(), Option<(&ScriptCaches, CacheEntry)>>`) and pass it to `check_input_scripts_unprobed`.
- **N4. PF0 headroom (brief item 7).** At `-par=1` I found no avoidable structural work per block:
  - `slots` is `Vec::new()`, which does not allocate (`:639-643`).
  - `control_inline` is one `Cell`.
  - `reset` on an untouched `Bump` is a few loads.
  - `ScriptOptions` and the sigop limit are now built once per block, not once per transaction.

  The ~240 instructions a block D128 attributes to "the control's scope" are codegen (the loop body moved into a generic closure returning `Result<Step, Fatal>`). I did not profile it. If a later slice pushes `empty/av-off` over 0.5%, callgrind `connect` before and after with `#[inline(always)]` on `with_control` and `SerialCore::connect`'s closure before restructuring.

## 4. Possible security note

None from this slice.
