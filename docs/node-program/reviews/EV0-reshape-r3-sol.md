# EV0 reshape — Sol review r3

**Range:** `git diff -M be483d04..c63d7328`, commits 1–7 on
`node/ev0-evo-interfaces`. **Tier:** `rev Sol→O-h`; Sol first.
**Binding:** EV addendum rev6 (`8a99b94d`, DEC-122), SB0 rev6.1
(DEC-119), DEC-113, DEC-116 and DEC-117. Reviewer contract:
`briefs/REVIEW-COMMON.md`.

The EV0 worktree was being updated concurrently. I reviewed the specified git
objects and ran validation from a `git archive c63d7328` snapshot at
`/work/scratch/ev0-reshape-r3-sol/source`. The branch under review was not edited,
committed or pushed. Commit `be483d04` itself was not opened or reviewed; its
separate PLAN §7 handling remains separate. No private material was opened.

## Findings

### R3-M1 — major: the node source alias rejects an attempt-bound member source

**Locations:** `dash-dml/src/store/mod.rs:98`,
`dash-dml/tests/consumers/ev2b.rs:41`.

`pub type DmlMemberSource = dyn QuorumMemberSource<...>` defaults the trait
object's lifetime to `'static`. The consumer's `source: &DmlMemberSource` retains
that bound; the lifetime of the outer reference does not remove it.

**Failure scenario:** construct the existing `lq1a::AttemptHandle` over a local
`MemEvoView`, then pass `&handle` to the existing EV2b
`members_to_punish`. Compilation fails with three E0597 errors: `store`, `chain`
and `view` must be borrowed for `'static`. The current consumer tests use
`FixedMembers`, which owns its members and therefore misses the failure.
CH7b's real `AttemptHandle<'v>` and capability-borrowing adapters must be scoped
to the attempt or V command under addendum §2.6 / DEC-117; this alias prevents
EV2b from accepting those sources.

This follows the addendum's illustrative §2.2 alias, but conflicts with its
binding scoped-capability requirement. Resolve the API rather than extending
the handle's lifetime.

**Suggested fix:** make the object lifetime explicit:

```rust
pub type DmlMemberSource<'a> =
    dyn QuorumMemberSource<Entry = Arc<Dmn>, Error = StoreError> + 'a;
// Consumer:
source: &DmlMemberSource<'_>
```

Add a compile-and-run consumer test connecting the borrowed `AttemptHandle` to
EV2b. I tested the two signature changes in scratch: the same reproduction
then compiled and all seven tests in that probe passed.

**Evidence:** `/work/scratch/ev0-reshape-r3-sol/lifetime-probe.rs`,
`logs/lifetime-probe-original.log` (exit 101),
`logs/lifetime-probe-fixed.log` (exit 0), relative to the same scratch directory.
The original source was restored after the experiment.

### R3-M2 — major: the new layering guard misses consumer-enabled dependency features

**Locations:** `contrib/node/check-pure.sh:112`,
`contrib/node/check-pure.sh:145`.

`cargo tree -p dash-evo --all-features` enables all features of the selected
package, not every feature of its dependencies. A consumer can enable an
additional feature of a shared dependency, and Cargo unifies that feature into
the dependency used by `dash-evo`. The forbidden normal edge then exists in the
consumer's graph but is absent from the guard's graph.

**Concrete reproduction, scratch only:**

1. Give the existing shared dependency `dash-consensus` an optional normal
   dependency `review_imbl = { package = "imbl", version = "=7.0.2", optional = true }`
   and `review-leak = ["dep:review_imbl"]`.
2. Under that feature, re-export `review_imbl::OrdMap` as `ReviewMap`; in
   `dash-evo`, re-export `dash_consensus` as `ReviewBridge`.
3. Run the complete, unmodified `contrib/node/check-pure.sh`. It exits **0** and
   reports both `layering: dash-evo's normal closure has neither dash-dml nor imbl`
   and `pure crates: 5 crate(s) ok`.
4. Compile a consumer with `--features dash-consensus/review-leak`. A test
   constructing and reading `dash_evo::ReviewBridge::ReviewMap<u8, u8>` passes;
   the corresponding normal dependency tree contains `imbl v7.0.2`.

The separate direct negative control uses a renamed optional `imbl` dependency
on `dash-evo`, activated by `test-utils`, and a re-export. The full script exits
**1** with the expected layering error there. Thus package renaming, re-exports
and a feature on the root are covered; the transitive-feature case is not.
There is no forbidden dependency in the unchanged reviewed tree.

**Suggested fix:** check the root's normal reachability in a graph resolved with
all workspace features, including features of its workspace dependencies, and
retain package identities rather than Rust import names. In the reproduction,
`cargo metadata --all-features` for the workspace followed by a normal-edge
walk from `dash-evo` correctly reaches `imbl`. Preserve exclusion of dev edges,
and add this transitive-feature/re-export fixture to the negative controls.
Use target-complete graph data for the layering check so mobile-only edges
cannot evade it either; SB1 still owns the full L2/L6 gates.

**Evidence:** `/work/scratch/ev0-reshape-r3-sol/run-layer-controls.py`;
`logs/layer-direct-negative.log` (exit 1),
`logs/layer-feature-bypass.log` (exit 0),
`logs/layer-feature-consumer.log` (exit 0, one test), and
`logs/feature-probe-{tree,consumer-tree}.txt`. All mutations were restored.

## Conformance and review boundaries

- **Commit order / split:** the view impls land while `Dmn` and `MnList` are
  local to `dash-evo`, then move with them to `dash-dml`. The shared `Arc<T>`
  forwarding impl resolves the orphan-rule issue. The moved state/store bodies
  have import and documentation changes rather than new algorithms;
  `ProTxHash`, `MnType` and `RawMnType` retain their bodies in their shared home.
  Both crates are in `node-pure`; the exact `imbl` pin stays node-only.
- **Errors:** `Missing` becomes node `Store(Inconsistent)` with Core's text;
  `View` preserves the store error; unknown type remains the exception class.
  `BrokenInvariant` maps to `Inconsistent`. The moved mined reader retains its
  complete-chain restriction and its ancestry/family/height-0 contracts.
- **SPV surface:** the consumer uses shared list/entry/chain traits and shared
  error/member values without importing node types. It checks missing lists,
  error propagation, withheld predecessors as `View(NotHeld)`, below-floor
  predecessors as `View(BelowFloor)`, genesis and the known null branch. The
  stub member function is not a production SPV `QuorumMemberSource` adapter;
  that adapter remains SB8/SB11 work.
- **Core v24.0.0-rc.3:** checked `CDeterministicMNList::GetCounts`,
  `ForEachMNShared`, `HasMN` and `IsMNPoSeBanned`, `IsBanned`, the stored
  confirmed-hash scoring input, collateral tie-breaking, rotation's ancestry
  input and the mined-index reads against the tag's source. The new views
  expose the entry's own state, use non-banned validity and preserve false for
  an absent ban lookup. The view deliberately leaves iteration order
  unspecified as LQ1 §2.2 requires; Core-order reproduction remains EV2a work.
- **Overlay:** one test handle owns one `RefCell` overlay; each `rows()` borrow
  is tied to that handle. Case 1 gives `[2,3]`; separate per-call handles give
  `[2,1]`. Case 2 preserves both overwrite effects in order, returns the latest
  overlay row and leaves backing unchanged until application.
- **DEC-113 / DEC-117:** production `MembersFacade`, the RwLock gate,
  `VContext`, `OnV`, sealed `OnValidation`, `EffectsApplied` and real V-loop
  constructors are not implemented by these seven commits. Commit 5's public
  `AttemptHandle::begin` exists only in an integration-test target, not in a
  library or a feature-exported constructor. I do not treat this stub as
  satisfying CH7b's capability or concurrency gate. CH7b must enforce the
  exclusive gate through log application on every outcome; dispatch before
  gate/cache acquisition; nested capability reuse; private, stateless,
  non-transferable V-context minting; abort before release on failed log
  application; and the shared snapshot-cache critical section across read and
  insert or write and insert. I verified the latter ordering against Core's
  `llmq/snapshot.cpp:301–329`. These obligations remain in the binding addendum;
  finding R3-M1 must be fixed so the EV0 API can support them.
- **Downstream fit / deviations:** EV2a owns exact `AddMN` acceptance and its
  view contract tests. A7's `unimplemented!` is explicit and avoids inventing
  consensus behavior before EV2a; no tested consumer calls it. A8 is accurate
  for CH7a's behavior: its diff contains import changes only. LQ1a's full
  algorithms and fallible predicates remain downstream work. DEC-116's
  served-type lifetime rule is not implemented or altered by this reshape.

## Independent validation

Builds and tests used `RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=12`.
`TMPDIR=/dev/shm/ev0-reshape-r3-sol-tmp` was mode **700**. The review's target
directory was `/work/scratch/ev0-reshape-r3-sol/source/target`, separate from
every task worktree's target. Logs and probes remain under the review's scratch
directory. SPV and FFI dashd tests were enabled with `contrib/setup-dashd.py`,
with this review's own temporary datadirs and retained-log path. All scratch
source mutations were checked against `c63d7328` and restored before the
workspace run. The TMPDIR was removed after validation; no review-owned dashd
processes remained.

| Check | Result |
|---|---|
| `cargo test --locked -p dash-evo -p dash-dml --all-features` | PASS: 48 tests, 0 failures |
| Complete `contrib/node/check-pure.sh`, unchanged snapshot | PASS: 5 pure crates |
| Renamed optional direct dependency + re-export negative control | Expected FAIL: exit 1, layering error |
| Transitive-feature + re-export negative control | Unexpected PASS: exit 0; R3-M2 |
| Feature-enabled consumer of the leaked type | PASS: 1 test; confirms R3-M2 |
| Borrowed handle → EV2b consumer, original alias | Expected compile probe fails: E0597; R3-M1 |
| Same probe with lifetime-aware alias / consumer | PASS: 7 tests |
| `cargo fmt --all --check` | PASS |
| `python3 .github/scripts/ci_config.py verify-groups` | PASS |
| `git diff -M be483d04..c63d7328 --check` | PASS |
| Crate clippy, `--all-features --all-targets -- -D warnings` | PASS |
| Crate clippy, `--no-default-features --all-targets -- -D warnings` | PASS |
| Crate docs, `--all-features --no-deps`, `RUSTDOCFLAGS="-D warnings"` | PASS |
| `cargo test --locked -p dash-oracle-sys --features oracle --no-fail-fast` | PASS: 62 tests, 0 failures |
| `cargo test --locked -p node-l1 --features oracle --no-fail-fast` | PASS: 26 tests, 0 failures; oracle-diff total 88/0 |
| `cargo test --locked --workspace --no-fail-fast` | PASS: 3,245 tests, 0 failures; exit 0 |

## Verdict

**CHANGES-REQUIRED** — fix R3-M1 and R3-M2, and rerun their regression controls.

**Sol gate satisfied: no**
