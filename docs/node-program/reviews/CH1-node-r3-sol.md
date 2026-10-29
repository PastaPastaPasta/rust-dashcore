# CH1-node round 3 — independent Sol review

Reviewed only `ec1074d9e..c8515e6b2` in a detached worktree at `c8515e6b2`; the local SB5 merge and its commits were excluded. The four commits `e9c6a9543`, `9448a40b7`, `a59cce408`, and `c8515e6b2` each verify `G`. Core comparison is `v24.0.0-rc.3` (`0364ed626e`), chiefly `src/validation.cpp` (`v`).

## Findings

1. **Minor test-harness nit — the F1 setup can spin indefinitely.** `dashd-chainstate/src/chain/tests/signals.rs:597–599`. The loop waiting for `holding && callbacks_pending >= 13` has no deadline. A test-only threshold mutant (`13` → `14`) leaves it spinning; the external 30-second cap kills the test. Put a deadline on the setup loop and release `go` before failing, so teardown can join the listener. The earlier bounded `recv_timeout` at line 592 also needs that release-on-failure discipline if it fails before `go` is sent. This is not a production-path finding.

No new production finding in the reviewed range.

## Round-2 closure

| Finding | Disposition | Evidence |
|---|---|---|
| F1, listener panic | **Closed for CH1's producer set.** possible security note: see manager. | `signals.rs:235–260` stores linked shutdown flags before the sink; `fifo.rs:128–168` closes from `CloseOnDrop` after the sink, including sink unwind. Queued and detached entries are taken under L2s but dropped and uncounted after the lock is released. The running entry was already uncounted once. The normal-sink and panicking-sink tests see `(message, 13, true)`, both barriers disconnect and pending ends at zero. The flag remains set for idle V and for a Q=1 mid-hold V in my scratch probe. |
| F2, `BlockChecked` on connector `Fatal` | **Closed.** | `abc.rs:373–400` maps `Err(Fatal)` to `BlockState::Error(msg)`, emits `BlockChecked`, logs the `ConnectTip: ConnectBlock … failed` line, then propagates. This is Core `v:2239–2243,3427–3436`. `a_system_error_mid_reorg_updates_the_mempool_then_aborts` checks the signal in both connector branches. |
| F3, D24 negative control | **Closed.** | `tests/signals.rs:346–465` uses a Q=1, five-step reorg in one production hold. S retains its selected first event until after posting, so later fanout must use `retry_pending` inside that hold (`abc.rs:122–130`). The bounded lane is filled to `COMMAND_CAPACITY`; its post times out. With `ChainHandle::send`, the marked post returns while the published tip is still the pre-hold tip, records marked depth 1, and runs once after the hold. Both routing-direction mutants are caught. |
| F4, aborted hold | **Closed.** | `tests/abc.rs:514–541` retries `ChainFeeds::register` until success or a 120-second deadline after each of the `SystemError` and `Err(Fatal)` routes. A leaked-hold mutant fails with `abort false: Err(HoldActive)`. |

**F1 path details.** A listener panic with either sink outcome follows flag → sink → close. `CloseOnDrop` acquires L2s only to mark closed and take the queued list; it appends the detached batch, uncounts it, and drops entries after L2s is gone. A V in `limit_queue` gets a disconnected barrier and retains the already set flag for ABC's `ShutdownRequested` check. An idle V also gets the flag. My Q=1 scratch probe held S's first reorg event, panicked a listener with a panicking sink, observed the flag and pre-hold published tip, then released S; the reorg completed and the flag was still set. The producer-wake-only mid-hold park does not wake solely for shutdown; that remains D31's declared staged boundary.

`V::spawn` links the flag before it starts the V thread (`actor.rs:362–427`), so no V-produced queued signal can panic before linking. The test rig starts the signals thread before V, but has no queued signal in that interval. `link_shutdown` does not latch an earlier panic from a hypothetical independent producer; adding a pre-V producer would need linking before `start` or a latched panic state. D23 says CH1 has only V's queued producer.

## D-row judgments

| Row | Judgment |
|---|---|
| D11 | **Keep.** On a fatal `HeaderChain::accept` batch error, `process_headers` returns before `res.inserted` exists, so V emits neither its “Saw new header” line nor `AcceptedBlockHeader` for an inserted prefix. Core does both per header before the later failure. The per-header callback out of SB4's `accept` is the right ask. Qualify “logs none” as V's accepted-header line; other error logs may exist. |
| D24 | **Replace by the in-hold controls.** The synthetic non-servicing control is gone, and the two new tests exercise the actual Q=1 fanout drain. |
| D28 | **Accept.** DEC-166 supersedes rev3.1: hand `ChainStateFlushed` to the port only after B1 is durable, with the locator captured at `at_site`, never at freeze. CH1 has no producer yet. |
| D31 | **Accept only as staged `NoFlush`.** Recheck the in-hold wait and shutdown model when CH2b adds `FlushDue` and `PruneClears`. |

## Mutation and scratch probes

Each edit ran alone in the detached worktree. I saved its source file, restored it afterward, verified `cmp=0`, and touched it before the next case. `exit=124` means the external cap stopped a running test, not a zero-test filter.

| Mutant/probe | Result | Witness |
|---|---|---|
| F1 close only after sink returns | Caught, cap at 25 s | Panicking-sink wait test ran and did not disconnect. |
| F1 set flag after sink | Caught, test failure | Probe saw `false` instead of `true` at the sink. |
| F1 leave detached batch counted | Caught, test failure | Post-close pending assertion failed. |
| F2 return on `Fatal` before signal | Caught, test failure | Both-branch `BlockChecked` assertion failed. |
| F3 marked sender uses bounded lane | Caught, cap at 30 s | Positive in-hold test ran and could not post. |
| F3 unmarked sender uses marked lane | Caught, test failure | Negative test's after-hold command-count assertion failed. |
| F4 forget hold on `abc_step` error | Caught, test failure at 120 s | `register` remained `HoldActive`. |
| Q=1 mid-hold panic, panicking sink (scratch-only test) | Passed | Flag latched before and after the held reorg completed. An initial probe with default Q was discarded and rerun at Q=1. |
| F1 setup threshold 13 → 14 (test-only) | Caught, cap at 30 s | Demonstrates the unbounded setup spin in the minor finding. |

The F1/F2/F4 round-2 scratch questions were rerun through the fixed targeted tests and inverse mutants. The final restored-source chain suites pass.

## Validation

- `cargo test -p dashd-chainstate --lib --all-features`: 177 passed; default features: 177 passed.
- `chain::` lib suite: 61 passed in each of five consecutive runs with all features and five with default features; final restored-source run passed 61/61 for both.
- `cargo clippy -p dashd-chainstate --all-targets --all-features -- -D warnings` and default-feature equivalent: both passed.
- `cargo fmt --all -- --check`, final source diff and `git diff --check ec1074d9e..c8515e6b2`: passed.
- The reviewed range changes only `dashd-chainstate`; no commit in it changes or depends on SB5's feeds bench fix. The manager-reported bench failure is outside this range and is not charged to CH1-node.

**Verdict:** APPROVE-WITH-NITS
Sol gate satisfied: yes
