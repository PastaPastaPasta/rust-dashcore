REWORK

Closure check of rev2 at `5e3cb089` (working design byte-identical). Read REVIEW-COMMON, both r2 reviews, retained GPT evidence, DEC-62/63/64/66, and relevant A1/Core/ST2 code. Small ST2-linked probes passed for both releases; abstract protocol/counterexample checks passed. Evidence: `/work/scratch/st3a-r3-gpt/{prune_probe.rs,prune-probe.log,protocol_checks.py,protocol-checks.log}`. These are design checks, not an implemented-harness or physical power-cut result.

1. **R2 finding 1 — Closed: prune witness reachability.** `designs/ST3a-power-loss.md:769–813,1018`. Actual ST2 gives 63 records, file 0 = heights 0–62, size 65,016. S3p/S5/S5b/S9/N9 select `{0}`; S5 replay intersects at 6–62. S5c's wrong first selection deletes 61–62 needed by flush 2. S11, S11r and S13 each discriminate correct `∅` from buggy `{0}`; S11r's file 1 has exactly 38 records. S11c requires its nightly selection to pass the disabled locator. Explicit preconditions prevent vacuous runs. Budget arithmetic reproduces: PR 29.48 CPU-s, nightly 2,085 s; these remain forecasts. The new subscriber-checker contradiction is finding 6 below.

2. **R2 finding 2 — Closed: T18/N8.** `designs/ST3a-power-loss.md:836,860`. T18 now removes the barrier **and** buffers Bk, admitting a retained finalizer with lost coins. N8 retains the barrier and forbids pruning: a lost Bk leaves B1's marker; a retained Bk has protected dependencies. Both recover correctly. T26 remains the interrupted Durable-finalizer case confirmed by the r2 native evidence.

3. **R2 finding 3 — Closed: R5 overlay.** `designs/ST3a-power-loss.md:602,856`. E includes surviving pending batches in WAL order, so retained Bk clears supersede B1 legitimately. The mandatory clean Append image and T31 distinguish legal supersession from recovery dropping an unsuperseded index record.

4. **R2 finding 4 — Closed: unconditional D1(f).** `designs/ST3a-power-loss.md:887`. W1 now distinguishes post-crash violations from the production policy diagnostic. Its separate T17 expectation regresses under DEC-64: finding 7.

5. **R2 finding 5 — Closed: concurrent barrier coverage.** `designs/ST3a-power-loss.md:245,299–304,701,1129`. Returned-before-started, successful barrier completion, then finalizer Append is normative. Scopes correctly include presumed effects before B1 and subscriber data plus completed cross-store B1, while excluding independent effects.

6. **Major — Permanent subscriber data requirements make correct pruning fail D1(b).** `designs/ST3a-power-loss.md:790`, against `:658,664,799,861`.

   Each subscriber block record declares `Block { undo: true }`, and D1 rechecks **every durable record** after every store barrier. In clean S3p, subscriber data and locator 72 are durable before Bp. Bp clears file 0's index positions, so the retained subscriber records for heights 1–62 immediately fail D1(b). N9 fails identically at Bk. Moving the locator to 72 does not erase those data records. Core's coinstats undo reads are append/rewind needs, not permanent retention requirements.

   **Fix:** make the standing block/undo requirements come from the durable locator's disconnect/connect path through the LCA to the chainstate's guaranteed recovery tip. Processed data records must not independently pin their historical blocks. Specify this in the toy extractor and require S3p/N9 to remain D1-clean through Bp/Bk and unlink. The small counterexample confirms that locator-path requirements pass while literal per-record requirements fail.

7. **Minor — W1 no longer hides T17's acknowledgement failure.** `designs/ST3a-power-loss.md:779,835,887`, against `:586,600–601`.

   T17 changes B1 to Buffered, but the normal step 5 still acknowledges `new` after B1 returns. Crash after the subscriber's next data Append, before any further chainstate barrier. Losing all pending chainstate writes is a valid **empty prefix** under W1; recovery returns `old`, violating R3/R4 despite A-PREFIX. Prefix recovery cannot supply the missing acknowledgement durability.

   **Fix:** remove T17 from W1's “no post-crash violation” list, or explicitly define a separate prefix-only T17 control that delays acknowledgement until a completed chainstate barrier. Preserve the actual T17's early-ack failure.

Summary: DEC-64/66's ordering and LCA lock geometry are sound. Early `ChainStateFlushed` is safe with completed Durable B1; locator loss only requires catch-up. Separate Bp is safe because one-batch B1 already commits the entire state, Bp atomically clears references, and unlink waits for Bp's return. No new baseline crash hole was found, but the two acceptance contradictions above require correction before implementation approval. **Final verdict: REWORK.**
