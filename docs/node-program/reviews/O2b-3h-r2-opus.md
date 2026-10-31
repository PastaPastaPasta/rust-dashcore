# O2b-3h diff_chain generator and harness: Opus-h review, round 2

Reviewed: rdc `node/o2b-3h` at **935914d17**, stacked on the frozen O2b-3a head 59a1b323e (`git diff 59a1b323e..935914d17`: 5 commits, 10 files, +675 −146). Round 1 is `O2b-3a-r1-opus.md` (F1–F7, N1–N5). This stack answers F1–F3 and N1–N5. F4–F7 are not in it, and I did not re-review them. Plan rows D5–D10 and the §11 measurements are the owner's.

I used a detached worktree with a private 0700 target under `/work/scratch/o2b-3h-r2-opus`, `RUSTC_WRAPPER=`, `CARGO_BUILD_JOBS=12` and `DASH_ORACLE_CACHE=/work/cache/oracle`. The branch was not edited. The worktree and target are deleted.

## Verdict: ACCEPT WITH NITS

All of r1's F1–F3 and N1–N5 are closed, and my own runs agree with the owner's. Seed 7 is the seed where r1 saw three site-5 discards. On it, the 1k sample now has 0 discards, 0 unexpected replies and 0 replay mismatches on both schedules. The D10 predictions (`seen`, `stale`, refused load) were checked 272 times with no unexpected reply.

The rule-2 scope term is structural. It is consulted only when a RECONSIDER is generated, and the census reports it. It changes no expectation, no tie decision and no other op. The F3 injections really fail: two mutants of the comparator are both caught by `l1_chain`.

None of the findings below affects scoring. Two are worth settling before the merge:
- **F1:** the scope term's "after" disjunct is dead code, and nothing tests the bookkeeping that fills the set.
- **F2:** the injection table covers 7 of the comparator's 18 checks.

On D36, my view is that the twice-vs-once test measures per-execution nondeterminism, not order dependence. Neither cov nor ft can be gated on it as it stands. See the D36 section below for a replacement test and the risks either way.

## What I ran

| Run | Result |
|---|---|
| `cargo test -p node-l1 --features oracle --test l1_chain` | 2 passed (S0, S1), EXIT 0. The Core-vs-Core compare ran on every replayed step, and `injected` ran on both schedules |
| `cargo test -p node-l1 --features oracle --lib chain` | 12 passed, among them the two new F2 tie tests and the rule-2 scope test |
| `cargo test --manifest-path node-fuzz/Cargo.toml --lib` | 22 passed, among them `only_a_dispatching_process_skips_the_start` |
| `chain_sample S0 1000 7` and `chain_sample S1 1000 7` (release, under `systemd-run --user`; r1's seed) | EXIT 0 on both. **0 unexpected, 0 discards** (r1, same seed: 3 at site 5), 47 replays with 0 mismatches, 7.3 inputs/CPU-s under load 55–105 |
| Comparator mutants (scratch, reverted): (a) the `post.tip` check deleted; (b) the events check replaced by `plane(true, …)` | Both make `l1_chain` S0 fail at the injection's `expect_err("a difference")` (`l1_chain.rs:74`) |

Census at seed 7, S0, with r1's 3a numbers for comparison:

| Row | 3a (r1) | 3h |
|---|---|---|
| `reconsider_after_failure` | 1,263 (the not-own case included) | 21 |
| `reconsider_not_own` | (inside the row above) | 1,199 |
| `reconsider_scope` | — | 25 |
| `reconsider:ok` | 36 | 40 |
| `cl_record` replies checked (`seen` 38 / `stale` 112 / ok) | 0 (`Any`) | 874 |
| `restart` replies checked (122 refused loads) | 0 (`Any`) | 1,520 replies, all checked except restarts while down (F3) |
| discards | 3 (site 5) | 0 |

r1 counted 22 real failure marks in the after-failure row. The new 21 matches that, so F1's narrowing removes exactly the inert classes. Of the 43 reconsiders F1 freed, the scope term takes 25 back, as DEC-256 intends.

## r1 findings: closure

| r1 | Status | Evidence |
|---|---|---|
| **F1** (the predicate counts rejects that mark nothing) | Closed | `marks_failure` (`world.rs:666`) leaves out header-stage strings (the list includes `prev-blk-not-found`, `bad-prevblk*` and `duplicate-invalid`), the Pow/Check-stage operators and `bad-chainlock`. `observe_checked` (`world.rs:555`) leaves out `missing_prev`, `mutated`, `chainlock` and the op's own block, whose reply decides. Dropping `duplicate-invalid` (935914d17) is right: every path that marks a block failed is either counted when it marks, or is an own invalidate or a ChainLock conflict, none of which rule 2 counts. So a re-delivery adds no new failure. The split into `reconsider_not_own` and `reconsider_after_failure` makes the census readable. `ties::restart_deficit` reads the same flag, so it gains the same rows |
| **F2** (seq 0 for a block stored under a parent without data) | Closed | `World::store_data` (`world.rs:241`) gives a unique id only when the whole ancestry has data. It then walks the stored descendants and skips `Stranded` ones, so a stranded block's own descendants also stay 0, which matches the unlinked queue. A precious block that links is renumbered, as Core renumbers it. The two new tests pin the unlinked and stranded cases. 0 site-5 discards on seed 7 (r1: 3) and in the owner's 10k (3a: 10) |
| **F3** (`chain::compare` dead and untested) | Closed, with F2 below | `strict()` compares every replayed step pair. `injected()` checks that each injected difference fails and that its field is named. The module doc now records the strict events plane and the incomplete-records pass as 3b choices, which answers r1's two questions |
| **N1** (unexpected replies neither logged nor kept) | Closed | `kept_unexpected` (`run.rs:358`) logs the reason and keeps the input bytes, with a 0700 directory and a cap of 64 per process |
| **N2** (literal 16 in chain_sample) | Closed | `replayed()` is shared, and `REPLAY_EVERY` is `pub` |
| **N3** (`Any` for CL_RECORD and RESTART) | Closed, with F3 below | D10. `seen` is keyed by (height, hash). The signature is always zero, so that pair fixes the CLSIG hash. The set is filled whatever the reply, as Core inserts before its stale check, and it is cleared at a load. The shim runs no scheduler, so Core's time-based cleanup of the seen set never runs, and the size-based prune is covered by `CL_SEEN_KEPT` → `Any`. The refused-load check matches `VerifyLoadedChainstate`'s tip-time test against `coins_tip` |
| **N4** (two-name `enforce` parameter) | Mostly closed | `Refused` is an enum now. `enforce_counted` keeps two names, and both call sites pass `UNENFORCED` twice. That is acceptable |
| **N5** (test comments; F2 tie test) | Closed | |

## Rule 2 scope term (task 2)

- **Structural.** The set `stored_under_failed` is filled in `World::observe` (`world.rs:338-341`) for the op's own BLOCK, whatever the reply, when some ancestor in the model's tree is marked failed. The inputs are the program's own submissions, the tree, and failure marks already folded in from earlier steps. It never looks at Core's verdict on the op being judged, and it reads no Core internals.
- **Only reconsiders.** The only consumer is the RECONSIDER branch in `Gen::resolve` (`generate.rs:466`). That check runs after `reconsider_not_own` and `reconsider_after_failure`, so it narrows only reconsiders that rule 2 already admits. It is absent from `expect`, the tie model, enforcement and every other intent, so no other Core verdict class is suppressed.
- **Reported.** The census reports it as `avoided.reconsider_scope`: 25 of 1,000 inputs on seed 7, and 359 in the owner's 10k. The fuzz target writes it at every fold.
- **Over-avoids, never under-avoids.** The predicate (`world.rs:227`) counts every block under any failed ancestor of the target. That is wider than the set a reconsider resets (the target's subtree and its ancestors). It only over-avoids, which is the safe direction. The tracked wording is generic.

## Findings

**F1. Minor: the scope term's "after" disjunct is dead, and the code that fills the set has no test.**
- `world.rs:338-341` evaluates `under_failed` before and after `observe_block_reply`. That function changes only the op's own block: its `held`, `invalid` and `seq`, and through `store_data` its descendants' `seq`. It never changes an ancestor's `invalid`. Marks that this op's own activation makes on ancestors are folded in later, by `observe_checked` and `observe_tips`. So `before || under_failed(self)` always equals `before`.
- That result matches D7's wording ("submitted while an ancestor was marked failed"). Two possible fixes:
  - if that wording is the intended rule, drop the disjunct;
  - if the manager's scope also covers ancestors that this op's own activation marks, move the check after `observe_checked`/`observe_tips`.
  I have not tried to infer which one is meant. The manager or the owner should confirm.
- **Test gap:** `rule_2_scope_reaches_the_subtrees_a_reconsider_clears` (`ties.rs:267`) inserts into the set directly. Nothing exercises the insertion in `observe`, and no public seed reaches `reconsider_scope`; `l1_chain`'s `avoided` has only `reconsider_not_own` and `spork_not_admitted`. Deleting the insertion would pass every test, and only the sample's census would show the change.
- **Fix:** add a `World`-level test that folds a synthetic BLOCK step under a failed ancestor and asserts membership. If a public seed would say too much, a unit test is enough. Rename the test from "clears" to "reaches", because the predicate is wider than what a reconsider clears.

**F2. Minor: the injection table covers 7 of the comparator's 18 checks, and the Core-vs-Core compare is a reflexivity check only.**
- `injected()` covers `reply`, `records` (outcomes), `events`, `post.tip`, `post.utxo`, `post.tips` and `post.ibd`.
- It has no injection for `attempts`, `sparse records`, or nine post fields: `down`, `flush_delta`, `persisted`, `best_header`, `index`, `flushes`, `best_cl`, `sporks` and `deployments`.
- D9's "an injected difference per plane" holds at the design's four planes, and my mutants confirm that the covered checks really fail. But 3b relies on the field name to triage. A swapped or missing `plane(...)` line for an uncovered field would go unnoticed, and so would a wrong field label.
- **Fix:** each uncovered field takes one closure: toggle `down`, bump a `flushes` counter, clear `best_cl`, and so on. If the basic seed has no attempt or sparse record, say so in the test.
- **Reflexivity:** `strict()` runs `compare` only after it has asserted `again.raws == run.raws`. So it proves only that `compare` gives no false positive on equal input, which still covers decode determinism and the stream mode of `verifydb`. That is worth having. The doc should describe it as a reflexivity check rather than as an independent replay check.

**F3. Nit: D10's down-session case is not predicted.**
- D10 says a restart of a down session loads the tip of the last full flush.
- But `expect` returns `Any` for every RESTART while the world is down (`generate.rs:150`), and `coins_tip` is never consulted on that path.
- **Fix:** either predict it (`coins_tip` for both kinds when down) or reword D10 to say that a down session's restart is unchecked.

**F4. Nit: replay choice depends on the toolchain.**
- `input_hash` (`run.rs:340`) uses `DefaultHasher`. Rust does not promise that this is stable across releases.
- A toolchain bump would change which inputs are replayed. A replayed input's coverage is two runs, so that changes per-input coverage, and with it the corpus's INITED cov/ft, DEC-184 trends and certify baselines. None of these would have a code change to explain them.
- **Fix:** use a fixed hash (FNV-1a, or the first 8 bytes of a SHA-256 the crate already links), or list a toolchain bump as a re-baseline trigger next to the ignorelist one.

## Nits

- **N1.** `dispatches_only` (`node-fuzz/src/lib.rs:104`) misses `-minimize_crash=1`, which `run.sh:609` passes. That parent only re-runs the binary as children, so it builds the image for nothing. That costs time and memory but is not a correctness problem. `-set_cover_merge=1` follows the same control-file protocol as `-merge=1`, so add both prefixes.
- **N2.** The census is now folded every 4096 inputs and at exit (`run.rs:389`, `Drop for Session` at `:428`). A normal `exit()` (`-runs`, `-max_total_time`, SIGUSR1's graceful stop) runs the main thread's TLS destructors. libFuzzer's SIGINT and crash paths use `_Exit`, which does not. At `run.sh`'s wall cap (SIGINT), a process now loses up to 4,095 inputs of census, where 3a lost at most 63. Coverage and verdicts are unaffected. Either document this, or have the reader treat a missing stats file after a SIGINT stop as expected.
- **N3.** `kept_unexpected`'s first call creates the directory, and calls after the 64th return early. So an unexpected input's coverage depends slightly on how many came before it. It is harmless, since 0 unexpected replies have been seen, but it deserves a line in the D36 comment for completeness.
- **N4.** `stored_under_failed` also records blocks that Core never indexed, such as orphans and header-stage refusals, through the world's ancestry. That is fine because it only over-avoids. Say "whatever the reply, indexed or not" in the doc, so a reader doesn't take it for an oversight.

## D36 opinion (task 4, advisory)

**The test cannot pass as stated.** Once vs twice in one process compares one execution per input against the union of two executions per input. If an input's feature set varies from one execution to the next, and the owner measured ±4–87 ft per re-run, then E[twice] > E[once] even with perfect order independence. The across-process spread of the once runs does not capture this union bias. The bias is not confined to ft: in the owner's §11 table, 23 of 24 per-process cov differences are positive, and cov fails the restated bound in 3 of 8 cells. So "use cov only" does not by itself make diff_chain eligible under DEC-256's bound. It only shrinks the miss.

**Proposed replacement for the order-independence test.** Hold the number of executions fixed and vary only the order, on cov and ft both:
- **Arm X:** the once corpus, with every input run twice back to back. This needs a scratch build in which `replayed()` returns true, so each input runs AA.
- **Arm Y:** the twice corpus, with replays off. Each input still runs twice, but interleaved in libFuzzer's order.
- **Bound:** the across-process spread of X.

If the D5 changes made inputs independent of their predecessors, then X ≈ Y within that spread on both metrics. Any remaining ft gap against the once runs is nondeterminism and not leakage. If Y > X beyond the spread, an order dependence remains, and ft is right to block. The test costs as much as one more §11 table. I would gate D36 on this test, with ft included.

**Farm eligibility and DEC-184 tracking, after that test passes.** I would track diff_chain's plateau on cov, and either exclude ft or give it a measured noise floor. The floor can come from a pure replay loop of the corpus for 60 min with no mutation; if its ft growth exceeds DEC-184's 0.1%, ft as written cannot plateau for this target. Risks either way:

**If ft is excluded (or floored):**
- Progress that shows only in hit-count buckets becomes invisible, such as deeper loop counts or longer reorgs on paths already seen. The census rows (`reorg_3` and others) partly stand in for it.
- An order dependence that changes only hit counts would also go unnoticed. That is why the X/Y test must include ft.
- libFuzzer still admits inputs for noise features. That bloats the corpus and slows `-merge=1`/certify, without moving the cov signal. If the bloat is large, `-use_counters=0` aligns admission with edges, at the cost of hit-count guidance for the whole target, the Rust side included.
- Semantic leakage between inputs is still guarded independently, by byte-identical replays (raws include the restore report). 599 replays matched in the owner's 10k and 47 in mine.

**If ft is kept:**
- diff_chain stays out of the farm for good. No harness change can remove heap-address-dependent variation inside Core, and the draw reset at restore cut the ft excess by only about 15–20.
- If it were admitted anyway, ft noise would keep the 0.1% criterion from ever firing. That is the CPU burn DEC-184 exists to stop.

**Applies to both options:** cov is noisy too, by 3–22 across processes in the owner's table. Certify's "DONE cov = INITED cov" comes from two separate `-runs=0` replays, so it may fail spuriously for this target. The manager should rule on a tolerance, or on a union of several replays, together with the metric.

## Regressions and vacuous checks (task 5)

- **No regression in the harness.**
  - The session now starts in the init hook (`Target.start`), and every other target passes `None`.
  - `Runner::run` ends with a restore at T_RECENT, which the next T_RECENT input takes. A T_OLD input drops it and restores at T_OLD from the same reset state, so every input's coverage is a function of the input alone. Byte-identical replays, restore report included, back the claim that the reset restore reports the same as a fresh one.
  - On a shim fault the session is dropped, and the drop folds the census. A failed start in the init hook becomes the first input's shim fault, as documented.
- **Report buffer (2a033b9e7).** It keeps the retry semantics of `with_buffer`: on `ORACLE_E_BUFFER` it resizes to `len` and calls again. It reads only `[..len]`, so leftover bytes from earlier calls are never read. The buffer never shrinks, and is bounded by the largest report. That is fine.
- **Vacuous or weak checks:** F1 (the dead disjunct, and the insertion is untested) and F2 (partial field coverage, and the reflexivity-only compare). Nothing else.

## Tracked/private hygiene

- **Generic and clean.** The commit messages, D7, the code comments and the census keys say "rule 2 scope", "reconsider_scope" and "possible security note: see manager". Nothing points at a mechanism, and this review adds none.
- **One question for the manager.** The phrase "possible security note" is itself tracked public text. It marks the term as sensitive to any reader. If DEC-256 doesn't require that wording, "rule 2 scope (DEC-256)" says as much without the flag.
