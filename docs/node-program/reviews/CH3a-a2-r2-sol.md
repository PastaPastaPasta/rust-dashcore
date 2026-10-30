# CH3a a2 connector seam, Sol r2

Scope: read-only review of `cc24921cf..721815def` and D56–D57 in `CH3a-notes.md`, against the r1 finding and the EV addendum §2.6. I treat the CH7b facade signatures in that addendum as the intended production contract; CH7b's production facade has not landed in this slice.

## Findings

No blocking correctness finding in this delta. The r1 lifetime blocker is resolved: `EvoHooks::begin_attempt` now borrows a separately owned `Facade` and V's context for the same `'v`, while its receiver remains `&mut self` (`connect/evo.rs:113-174`). Thus the returned attempt can hold CH7b's private-facade `AttemptHandle<'v>` and a separate facade reference for `release_attempt`, without borrowing the hooks. The added `Ch7aShape` implementation and `Connector::bracket` compile with the specified `begin_attempt(&'v self, &'v mut VContext, ...)` / `release_attempt` signatures, then call mutable hooks and `finish_attempt` while the gate is held (`chain/tests/attempt.rs:29-159,277-313`). No unsafe code or new lock is needed for the seam. The test runs on V and checks successful, H11-error, and drop release paths (`:325-395`). This is a stub of the intended CH7b facade, not an integration test with a production CH7b implementation.

**Nit — one pointer clone per attempt.** D56 and `Connector::bracket` explicitly use `Arc::clone(&self.facade)` before calling a whole-`&mut self` helper (`chain/tests/attempt.rs:285-293`). The literal “without clones” check is therefore not met by the proposed connector shape. This is a reference-count operation, not a clone of facade state, a heap allocation, or another lock acquisition. It does not revive the lifetime blocker or violate r1's no-new-allocation/lock constraint. If zero reference-count operations is a hard performance requirement, split the connector's field borrows and make the inner helper operate on those fields; the present whole-`self` bracket cannot borrow its facade field across that call.

**Nit — drop-path wording.** The test manually drops an attempt and verifies that its gate is released (`chain/tests/attempt.rs:357-367`); it does not unwind or test the a3 latch guard. Describe this as an unreleased-handle drop, or add the unwind/latch assertion in a3. The a3 abort/latch work remains deferred by the brief.

H11 is fallible at the seam: `digest_fields` returns `Result<EvoDigest, Fatal>` (`connect/evo.rs:287-295`), the example bracket propagates the error after `finish_attempt`, and its failure test observes an error while effects are still applied (`chain/tests/attempt.rs:263-273,285-311,352-355`). `NoEvo` returns `Ok(EvoDigest::default())` because it makes no store read (`connect/evo.rs:457-464`); a real read failure cannot be silently converted to `None` by this interface. Production H11 read handling awaits CH7a.

The only non-test Rust changes are this interface, its `NoEvo` adaptation, and documentation/re-export text. No existing connector path is newly routed through these hooks. The changed `NoEvo` test call adds the unit facade argument and preserves its previous outcomes. `git diff --check` is clean. I relied on the supplied GATE-FAST tier-1 log for build/test validation: it identifies `721815def`, reports clippy and test-build success, 5,320 tests with zero failures, and `GATE EXIT 0`. I did not launch a second build.

VERDICT: APPROVE-WITH-NITS
