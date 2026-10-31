# CH1-node round 2 — independent Sol review

Reviewed the CH1-node commits `7c0ce9059`, `5acd5182f`, `997f1d72d`, `653e948f1`, `84d4de98b`, `c2879f6f6`, `07e83e8f7`, and `ec1074d9e` in a separate detached worktree at `ec1074d9e`. I excluded the local SB5 merge and its commits. References below are Dash Core `v24.0.0-rc.3` (`0364ed626e`), chiefly `src/validation.cpp` (`v`) and `src/validationinterface.cpp` (`vi`). Design reference: CH1-node rev3.1 and DEC-156/157/158/192.

## Part A — cp1 findings

| cp1 | Disposition | Evidence |
|---|---|---|
| 1, no-wait guard lifetime | **Closed** | `reply.rs:35–65`: a checked thread-local count and a `!Send` guard keep a thread marked through guards dropped in either order. `Marked::poll` and `spawn_thread` propagate the marker; `ActorReply`, joins and `signals.sync` refuse waits. The R2 drop-to-zero mutant fails `nested_guards_dropped_out_of_order_keep_the_marker`; the R6 make-guard-`Send` mutant fails compilation. |
| 2, duplicate listener | **Closed** | `signals.rs:305–316` uses `Arc::ptr_eq` before appending. The R1 duplicate-registration mutant fails `registering_a_listener_again_keeps_one_registration`, for inline and queued callbacks; Core `vi:50–55` keys by pointer. |
| 3, queued replies after abort | **Closed for V** | `actor.rs:322–427` starts the reaper before V and returns a spawn error; `:345–357,473–531` hands lanes over and sets shutdown from a drop guard even when V's abort sink panics. The injected reaper failure, queued/later replies and panicking-sink tests pass. R3 and R7 regressions are killed. This disposition is specifically for V; Part B has a separate signal-thread finding. |
| 4, accepted-header log | **Closed** | `accept.rs:31–41,53–57,162–166` logs each newly inserted header at DEBUG in IBD and INFO otherwise, before the signal. R4 (remove DEBUG) and R5 (always INFO) fail `new_headers_log_the_saw_new_header_line`. Core `v:4706–4724`. D11 still understates the timing difference for a fatal batch error. |
| 5, exact table oracle | **Closed in its stated domain** | `tests/support.rs:151–218` computes exact `nChainTx`, candidate live keys and the complete required/allowed unlinked relation from entries. The rig invokes it after normal header and block operations and at a clean stop (`:460–485,533–540`). M5 now fails two tests. The no-prune domain is explicit; this is not a future pruned-state oracle. |

## Part B — findings

1. **Major — possible security note: see manager.** Location: `dashd-chainstate/src/chain/signals/fifo.rs:127–139`, `dashd-chainstate/src/chain/signals.rs:228–236`.
2. **Moderate — `BlockChecked` is omitted when the connector returns `Fatal`.** Location: `dashd-chainstate/src/chain/abc.rs:373–386`; test seam `tests/abc.rs:423–445,487–521`. Core `v:3427–3433` calls `BlockChecked` after `ConnectBlock` returns, including its system-error return. The existing `Breaks { abort: true }` route returns `Err(Fatal)` before the Rust signal call, so an observer sees no check result for that attempted connection. A scratch assertion on the existing fixture failed for `abort: true` (the recorder saw the prior successful check, but none for the failing block). Normalize the connector's fatal return into an error outcome/state and emit `BlockChecked` before propagating the fatal result; assert the signal in both branches of the existing system-error test.
3. **Moderate test gap — D24's negative control does not exercise either production wait it names.** Location: `dashd-chainstate/src/chain/tests/signals.rs:346–411`; `abc.rs:122–130`. Core `v:3752–3763` waits at the queue limit with `cs_main` released; V's real `VWait` services the bounded lane (`actor.rs:438–470`). The test instead calls `sync(&mut Blocking)` inside a synthetic class C command. It proves a bounded post can block if V does not service the lane, but it does not prove the DEC-192 scenario against N3's actual in-hold `retry_pending` drain. Add a Q=1 reorg test that fills the bounded lane while V is waiting for a marked subscriber's pop, then show a bounded post times out and the same `ChainHandle::send` completes through the marked lane. The direct LO-9 reply-await controls in §9.6 remain later integration work.
4. **Minor test gap — no check that an abort releases the tip hold.** Location: `dashd-chainstate/src/chain/tests/abc.rs:487–521`; implementation `abc.rs:545–559`. A scratch mutant that deliberately leaks `InHold` on `abc_step` error passed all 57 selected chain tests. The current implementation uses Rust drop on `?`, so I found no production leak. After the system-error fixture, assert the hold's tip lock is available within a bounded wait, including the `Err(Fatal)` connector route. Core returns from the `cs_main` scope on `v:3782–3785`.

### Control-flow assessment

`find_most_work_chain` walks back from the best candidate, marks failed/conflicting descendants, moves descendants of missing data to unlinked, and removes the unusable chain (`abc.rs:148–202` versus `v:3519–3578`). It updates `m_best_invalid` only for a failed chain; `invalid_block_found` and `invalid_chain_found` preserve Core's failed-block and best-header changes (`abc.rs:225–255` versus `v:2034–2055,2077–2089`). `abc_step` disconnects to the fork, connects in batches of at most 32, prunes candidates after successful connects, updates the mempool on every system error, and returns after forward progress (`abc.rs:260–351` versus `v:3601–3681`). The old-tip candidate is retained by the normal prune condition; erasing it on an invalid connect fails the reorg test.

`ConnectTip` and `DisconnectTip` use Core's read, connector, IF_NEEDED flush, mempool, tip-step and `UpdateTip` order (`abc.rs:356–463` versus `v:3249–3325,3395–3510`), subject to finding 2 and the declared CH2b/CH3a/CH7a seams. ABC stages `BlockDisconnected` per step, then `BlockConnected` traces, then `UpdatedBlockTip`; it runs the synchronous tip signal inside the hold (`abc.rs:535–620` versus `v:3752–3853`, `vi:193–212,232–255`). A signal-order mutant fails three reorg/IBD tests. `ChainStateFlushed` is only a queued variant here; CH2b owns its emission. After each step `retry_pending` completes before the next step; `Parker` retains a permit across a notify-before-park race (`dash-chain/src/notify.rs:38–86`). `Hold::release` publishes one snapshot while locked; errors drop the hold without publishing (`dash-chain/src/chain.rs:2583–2611`). The released path stores `ChainView`, splices queued signals, updates `TipWatch`, then calls UI (`abc.rs:507–533`). This is the intended N1 publication boundary.

The FIFO detaches batches and dispatches them in order; synchronous signals run inline over a registration-ordered listener snapshot (`signals.rs:244–355`, `signals/fifo.rs:74–142`; Core `vi:43–55,193–276`). V's two lanes preserve FIFO separately, service R/M work at release points and defer class C work (`actor.rs:48–89,621–657`; DEC-192). No cross-lane order is promised. The exact-table check covers no-prune states only. I did not find another difference in candidate pruning or the normal signal order that changes Core's active-tip result in the tested domain.

### D22–D34 dispositions

| Row | Disposition |
|---|---|
| D22 | Accept: `NodeBlockIndex` avoids SB4's public name. |
| D23 | Accept for this stage: only V's queued signals exist; other producers join later. |
| D24 | **Challenge:** the synthetic non-servicing wait is insufficient; use N3's real in-hold drain as the negative and positive control (finding 3). |
| D25 | Accept as the recorded PF0/N9a deferral; not an N3 correctness gate. |
| D26 | Accept: the EV gate is absent; its eventual shared guard must carry `NoWaitGuard`. |
| D27 | Accept: a post-stop `sync` disconnects in the test. |
| D28 | Accept as a CH2b/N7 deferral. Apply DEC-166's later durable-release rule to `ChainStateFlushed`; the rev3.1 freeze-time wording is superseded. |
| D29 | Accept for the declared connector seam; finding 2 concerns the result path already present. |
| D30 | Accept the deployment warning/log deferral to CH3a. |
| D31 | Accept only as a staged `NoFlush` implementation: the in-hold loop has a retained producer wake and no CH2b `FlushDue` or `PruneClears` service yet. Recheck the wait model with CH2b. |
| D32 | Accept: `-stopatheight` sets shutdown and ND1 acts on it. |
| D33 | Accept the N4/N5 deferral. |
| D34 | Accept as a declared error-path deviation: an abandoned hold does not publish its staged callbacks. It needs no change at this checkpoint. |

### Mutation kills

Each edit was applied alone in the detached worktree, tested, restored, and the restored file touched so Cargo rebuilt the next case. The selected run is the all-feature `chain::` lib tests except the unrelated million-entry queue growth test (57 tests). A failing test or build is a kill; `X3` is a survivor.

| Mutant | Result | Evidence |
|---|---|---|
| M1: `>` → `>=` at the 288-block boundary | Killed | `unrequested_blocks_too_far_ahead_are_not_stored` |
| M2: remove the unrequested less-work gate | Killed | `unrequested_blocks_with_less_work_than_the_tip_are_not_stored` |
| M3: admit a ChainLock-conflicting candidate | Killed | `chainlock_conflicting_blocks_are_never_admitted` |
| M4: pop deferred commands from the back | Killed | `deferred_class_c_commands_run_in_arrival_order` |
| M5: leave a candidate's cached key stale | Killed | `a_re_keyed_candidate_moves_in_the_order`, `candidates_keep_one_key_per_member` |
| X1: erase the old-tip fallback candidate on invalid connect | Killed | `an_invalid_block_fails_its_chain_and_the_hold_returns_to_the_best_valid` |
| X2: stage `UpdatedBlockTip` before `BlockConnected` | Killed | three ABC reorg/IBD tests |
| X3: leak the tip hold on an `abc_step` error | **Survived** | 57/57 selected chain tests passed |
| X4: route unmarked senders to the marked lane | Killed | `a_bounded_send_from_a_marked_task_deadlocks_against_v` |
| R1/R2/R3/R4/R5/R6/R7: regress the five cp1 fixes | Killed | duplicate-listener, nested-guard, reaper-spawn, header-log level/presence checks failed; R6 failed compilation; R7 left `a_panicking_abort_sink_still_disconnects_queued_replies` running until the bounded 95 s mutant timeout |

### Validation

`cargo test -p dashd-chainstate --lib --all-features`: **174 passed**. Restored-source all-feature and default-feature selected chain suites: **57 passed each**. The implementer's reported full gate at this head was EXIT 0 (4,049 workspace tests, 39/39 fuzz smoke); I did not rerun that gate. Scratch-only targeted probes for findings 1 and 2 failed as predicted; their source edits were restored. All builds used the requested private target and disk TMPDIR. The detached worktree had no retained source changes after testing.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
