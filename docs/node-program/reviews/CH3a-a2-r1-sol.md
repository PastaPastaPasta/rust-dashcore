# CH3a a2 connector seam review

Scope: `f7abb5ef3..cc24921cf` on `node/ch3a-a2`, read only. Compared the diff with CH3 rev2.1 D1–D4/R1 and the EV addendum's attempt contract.

## Findings

### Major — `EvoHooks` cannot produce the planned borrowed CH7b attempt

`dashd-chainstate/src/connect/evo.rs:110-130`: `begin_attempt<'v>(&mut self, v: &'v mut VContext, ...) -> Self::Attempt<'v>` ties the returned attempt to V's marker, but not to the hook object. The EV addendum §2.6 calls for `MembersFacade::begin_attempt(&'v self, &'v mut VContext, ...) -> AttemptHandle<'v>` where the handle holds both `&'v MembersFacade` and its `RwLockWriteGuard`. If CH7a's hooks own that facade, they cannot return the handle: a minimal implementation fails with “lifetime may not live long enough.” Changing the receiver to `&'v mut self` does not solve it, because every later H1–H11 call and `finish_attempt` also needs `&mut self`; a probe of that shape fails with E0499. `NoEvoAttempt` only borrows V and therefore does not exercise the intended ownership.

Resolve the facade/attempt ownership before freezing a2's seam. For example, give the opener a separately borrowed facade that outlives the attempt while hooks remain independently mutable, or use a genuinely owned attempt gate. Add a compile-only implementation using the planned borrowed `MembersFacade`/`AttemptHandle` shape and a subsequent hook call. This is an interface blocker for CH7a/CH7b even though current `NullConnector` behavior is unaffected.

## Checks and scope notes

- Existing-path changes match the stated exception: `ConnectTip` emits `block_checked` for `SystemError` and stops before it for `Fatal` (`chain/abc.rs:609-633`); `NullConnector` still raises `VALID_SCRIPTS` for non-genesis, connects, and returns a successful disconnect (`chain/seams.rs:660-697`). The changed test doubles preserve their old bool outcomes through `DisconnectResult`.
- `VContext` is created in the `dashd-v` closure and is `!Send`/`!Sync`; the initial `ChainView` still has sequence 1 and the same fields. `ViewTarget` remains separate from `AttemptOrigin`; `ScratchCoins` uses a checked engine downcast and no new hot-path allocation. The stack `RefCell` adds borrow checks, not a lock.
- `dashd-chainstate -> dash-evo` is an allowed N→P edge. Both lockfiles add the direct dependency only; layer and mobile-closure checks pass. `git diff --check` passes.
- Deferred per brief: a3 must wire D52's abort path to a latch-aware flush site, since `NoFlush::abort` would report a connector `SystemError` twice (`chain/seams.rs:119-123`, `chain/actor.rs:975-984`). `NoEvo::finish_attempt` rejects any effect (`connect/evo.rs:295-308`); the current production `NodeHelper` cannot emit one because `is_conflict` returns `None`.
- H11 currently promises to read digest fields through `EvoTarget` but returns plain `EvoDigest` (`connect/evo.rs:243-249`), while evo backing-store getters are fallible (`dash-dml/src/store/mod.rs:228-252`). Before a7, either make the H11 read fallible or show that all fields come from already computed attempt state, so a storage read fault cannot be silently represented as an absent field. I did not count this as a present-path failure.

Validation: `cargo test -p dashd-chainstate -p dashd-node` with `CARGO_BUILD_JOBS=6` and a private target was stopped after about 16 minutes while `librocksdb-sys` was still compiling; targeted tests are unverified under the brief's ~20-minute build limit. The private target was deleted.

VERDICT: CHANGES-REQUESTED
