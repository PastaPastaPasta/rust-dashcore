# EV0 reshape — Opus high review (r3, after Sol)

**Range:** `node/ev0-evo-interfaces` 152c191c → 6ee04017, 16 commits (worktree
`rdc-wt/ev0-evo-interfaces`). **Tier:** `rev Sol→O-h`, Opus high second (DEC-92). **Binding:** EV
addendum rev6 (DEC-122), SB0 rev6.1 (DEC-119), DEC-113, DEC-117; `dash-evo/API.md` §7 A1–A10;
reviews `EV0-api-r1.md`, `EV0-api-r2.md`, `EV0-reshape-r3-sol.md`. Reviewer contract:
`briefs/REVIEW-COMMON.md`.

**PLAN §7.** Commit 0 (the record comparison) was reviewed under EMBARGO-5 handling. Everything
specific to EMBARGO-5 is in the private note named in PLAN §7's register for this review; this file
is codename-only. Commit 0 is `be483d04` in the brief and `1185530b` on the branch (same patch-id,
rebased).

The branch was not modified. Probes ran in an untracked file removed afterwards, and in a `git
archive` copy under `/work/scratch/ev0-reshape-opus-high/`.

## Findings

### M1 — major: no tracked test guards the second half of the record comparison

**Location:** `dash-dml/src/record.rs:67-73` (`same_operator_key`), its tests at `:160-280`.

Commit 0 made `SameRecord` require two things of the operator key: the same serialization under the
state's version scheme, **and** equality under Core's `CBLSLazyWrapper::operator==`
(`bls/bls.h:514` v24 / `:122` in the class at v23.1.8). The second condition is the one the
PLAN §7 rule depends on, and no tracked test exercises it.

**Concrete case (mutation run):** delete `&& a.pub_key_operator.clone() == b.pub_key_operator.clone()`.
All 33 tracked `dash-dml` tests still pass (16 unit, 10 consumers, 1 entry view, 6 store). The
only test that fails is in the private suite, and the private suite is in no gate. EV2a rewrites
exactly this area (the canonical codec, `between`, the online-vs-reconstructed check of EV2 §2.8),
so a silent revert there would bring back the comparison the rule forbids, and every gate would stay
green.

**Fix:** add a tracked guard that needs no scheme pair. A key set to a failed decode's object
(Core's `Set(invalidObj)`: `objInitialized`, serializes as zeros) is not the default key (also
zeros). Their serializations are equal and `operator==` is not:

```rust
let failed = BlsLazyPk::from_bytes(&[0xff; 48], BlsSchemeSource::Version(BlsSchemeKind::Basic)).get();
let set_invalid = state(2, BlsLazyPk::from_value(failed, BlsSchemeKind::Basic));
let default = state(2, BlsLazyPk::new(BlsSchemeKind::Basic));
assert_eq!((set_invalid.pub_key_operator.clone().to_bytes(), default.pub_key_operator.clone().to_bytes()), ([0; 48], [0; 48]));
assert!(!set_invalid.same_record(&default) && !default.same_record(&set_invalid));
```

Verified: this test fails on the mutant and passes on 6ee04017. Separately, the manager should wire
the private cases into a private gate before EV2a starts. Today they run only by hand.

### m1 — minor: `MnStateDiff`'s doc still points EV2a at a design section for the operator-key rule

**Location:** `dash-dml/src/diff.rs:36-37` ("EV2a adds `between(a, b)` … (EV2 §2.8, §3.7)").

Commit 0 removed the operator-key rule from the `core_eq` and `MnStateDiff` docs ("EV2a takes it
under PLAN §7 handling"), but this pointer still sends the reader to EV2 §3.7, which states an
operator-key comparison for `between`. Under PLAN §7 that statement must not be followed (private
note §2). **Fix:** cite §2.8 and §3.7 for the `VERSION` bit only, and say "operator-key comparison:
PLAN §7 handling". A matching erratum to the EV2 design is the manager's (private note §2).

### m2 — minor: `MnCold::core_eq` is not Core's comparison when an earlier field differs

**Location:** `dash-dml/src/types.rs:182-204`; `dash-evo/API.md` §5 item 9.

The doc says `core_eq` compares "as v23's `CDeterministicMNStateDiff` makes it". Core has no
whole-state `operator==`, though. Its only state comparison is the diff constructor
(`evo/dmnstate.h` v23:222-240, v24:250-281), which compares **every** member, so
`pubKeyOperator != …` always runs, along with its decode side effect. `core_eq` chains with `&&`,
so it never reaches the key when `version`, `registered_height`, … `key_id_owner` differ.

**Probe:** a v1 state with raw `0xff…` key vs a v2 state. `core_eq` returns false and the key still
serializes as `0xff…`; Core's comparison leaves it serializing as zeros. EV2a's `operator_changed`
and `between` are where this matters.

**Fix:** compute the key comparison first and unconditionally (`let key_eq = a.key == b.key;`),
then the fields. Alternatively, drop the diff-constructor claim and require EV2a's `between` to
compare every member.

### m3 — minor: `SameRecord for MnStateDiff` compares fields outside the mask

**Location:** `dash-dml/src/record.rs:111-115`, `dash-dml/src/diff.rs:38-46`.

`same_record` compares the diff's whole `hot` and `cold`, but Core's stored form serializes only the
masked members (`dmnstate.h` v23:251-273). `WriteDerived` compares those bytes
(`evo/evodb.h` v24:159-205). EV0 documents `cold` as "the state the set fields come from". Suppose
EV2a's `between` keeps `b`'s state there (an `Arc` clone, the cheap choice) and the decoder
default-fills unmasked fields, as Core's own diff object does. Then an online diff and the same diff
decoded from its row compare different. A legitimate rewrite, such as a reconnect after a reorg,
then becomes `StoreError::Inconsistent` and D4 aborts the node, and EV2 §2.8's check reports a false
mismatch.

**Fix:** compare only the masked members, with the key through `same_operator_key` under the
diff's version when `PUB_KEY_OPERATOR` is set. Alternatively, state as a `MnStateDiff` invariant
that unmasked members hold Core's defaults.

### m4 — minor: the `compile_fail,E0597` doctest does not pin its error code on stable

**Location:** `dash-dml/src/store/mod.rs:135-162`; `dash-dml/API.md` §3.

Rustdoc checks `compile_fail` error codes only on nightly. **Probe:** on the pinned 1.98.1, a
`compile_fail,E0597` doctest whose body fails with E0425 passes. This negative control therefore
passes for any compile error, such as a renamed import. The R3-M1 regression itself is still
caught: the positive doctest and `ev2b.rs::an_attempt_bound_source_reaches_the_member_walk` fail on
the old alias. **Fix:** say in the doc that the code is advisory, or move the case to a trybuild
test with a `.stderr`. **Heads-up for CH7b:** its compile-fail suite (addendum §2.6, cases 1–6)
must use trybuild with stderr snapshots for the same reason.

### m5 — minor: the layering guard misses a forbidden edge that no workspace crate turns on

**Location:** `contrib/node/check-layering.py:24-40`.

The guard walks the *resolved* graph, so an optional dependency that no workspace member enables is
not in it.

**Probe** (archive copy): `dash-evo` depends on a non-member `review-outside` that has
`leak = ["dep:review_imbl"]` and re-exports `imbl`, and nothing in the workspace enables `leak`.
`check-layering.py` exits 0. `cargo tree -p dash-evo -e normal --features review-outside/leak`
then lists `imbl v7.0.2`. A consumer outside this workspace (the mobile SDK, Platform) can turn the
feature on and get `imbl` through `dash-evo`. **Fix:** also walk the *declared* normal dependencies
(`packages[].dependencies`, optional ones included, matched by package name) from every package in
`dash-evo`'s closure. Add the selftest case (the existing `outside_leak` without the `dash-dml`
line).

### m6 — minor: record how shared verification reads a member's operator key

**Location:** `dash-evo/src/llmq/view.rs:14-33`; `llmq/members.rs:39-44`.

`MnEntryView` is LQ1 §2.2.1's text and has no operator-key read. SB8's shared
`CFinalCommitment::Verify` checks `membersSig` against the operator keys at the signer positions of
a reconstructed member list (SB0 §5.1; `llmq/commitment.cpp` v24:76-93). The provenance rule in
`members()` says consumers read keys from the returned entry. Generic code needs some way to do
that. Adding a required method to `MnEntryView` later would break every implementor: `Dmn` and
SB8's SML entry. On the node the read also has Core's lazy `Get()` side effect on the shared state.
**Fix:** record in API.md §7 that the key read is either an additive extension trait or keys the
caller extracts (node-side, in `dash-dml`), and that `MnEntryView` stays as LQ1's text.

### Nits

- **n1** `check-layering.py` matches a deny-list of names. Forks under another package name (`im`,
  a renamed git fork of `imbl`), future node-only crates, and source inclusion (`#[path]`,
  `include!` into `dash-dml/src`) are all outside it. SB1's L2/L6 gates should use the addendum's
  §1.2 allow-list of `dash-evo` dependencies instead.
- **n2** `dash-evo/API.md` §6, "Purity gate and blst", is stale. `check-pure.sh` now passes with no
  local exception (run at 6ee04017: 5 crates ok).
- **n3** `spv.rs`'s §2.5 shape test asserts the right answer, but has no wrong-view control (a chain
  that answers `Ok(None)` for a withheld block gives `Ok(false)`). LQ1a-B owns the real controls.
- **n4** The brief cites commit 0 as `be483d04`. Record `1185530b` beside it in the gate log.

## Focus questions

1. **Interfaces for EV2a/EV2b and LQ1.**
   - **Views.** LQ1 §2.2's traits are in place, infallible (DEC-109 (3)), with the `Arc<T>`
     forwarding impl. `Dmn`/`MnList` implement them; `tests/entry_view.rs` is a cross-crate check.
   - **Overlay ownership.** The test `AttemptHandle` owns one `RefCell` overlay, and every call's
     `AttemptRows` borrows it. This is §2.6's shape and works with `members(&self, …)`.
   - **Attempt handle.** `DmlMemberSource<'a>` closes R3-M1: a borrowed handle reaches EV2b's walk.
   - **Errors.** `MemberError<E>` and `NodeMemberError` match §2.2/§2.3, `Missing` maps to
     `Inconsistent` with Core's text, and `MinedCommitmentReader::Error: From<BrokenInvariant>` is
     in place.
   - **Fallible reads.** `LlmqChain` carries the M2 contract in its doc, and `CompleteChain` never
     errs.
   - I found no forced breaking change for EV2a, EV2b or LQ1a. The open points are for SB8: m6 and
     m3.
2. **Layering.** The guard catches every Cargo-graph route Sol and the selftests plant: a
   dependency feature, a root feature, a rename, a re-export, a target-only table, and a non-member
   feature that a member enables. Dev edges pass. Inactive optional edges (m5), name evasion and
   source inclusion (n1) are not covered.
3. **Commit 0 vs EMBARGO-5.** The comparison is what the rule requires at every call site in the
   range: the store rewrite check, `MnList`/`Dmn`/diff records, and EV2 §2.8's comparator. dash-pkc's
   `BlsLazy: PartialEq` is Core's `operator==` line for line, and the class is the same at both
   pins. The private suite passes 4/4. Gaps: no tracked guard (M1), a stale pointer (m1), `core_eq`
   (m2). The detail is in the private note.
4. **A7 (`unimplemented!` `try_add`).** Acceptable until EV2a. Nothing reaches it before EV2a:
   LQ1a depends on EV2a, and SB8 implements its own scratch list. Any non-panicking stub would
   invent an acceptance rule where Core swallows `AddMN`'s failures (quirk 28b), which is a silent
   divergence and worse than a panic. EV2a-1's acceptance should name its removal (A7 already does).
5. **Test gaps.** M1 is the main one. §2.6 case 1 has its per-call negative control and case 2 its
   ordered log; cases 3–6 belong to LQ1m/CH7b by design. §2.4's shared tests are present. Also see
   m4 and n3.

## Validation

| Check | Result |
|---|---|
| `cargo test -p dash-evo -p dash-dml --all-features` | PASS: 43 tests (dash-dml 33, dash-evo 10) plus 2 doctests (one compile-fail) |
| the same, `--no-default-features` | PASS |
| `~/private/embargo/EMBARGO-5-tests/run.sh` | PASS 4/4 |
| `contrib/node/check-pure.sh` | PASS, 5 crates; layering ok |
| `contrib/node/selftest-checks.sh` | PASS: 113 ok, 0 FAIL; all 8 layering cases as expected (R3-M2's bypass fails the guard) |
| `cargo fmt --all --check`; clippy both crates, `--all-features` and `--no-default-features`, `--all-targets -D warnings`; `cargo doc -D warnings`; `ci_config.py verify-groups`; `git diff --check` | all PASS |
| Mutation: operator-key `==` removed | tracked suite 33/33 PASS (M1); private 3/4; scheme-free guard FAIL as wanted |
| Probes | `core_eq` short-circuit (m2); rustdoc `compile_fail` code on stable (m4); inactive optional `imbl` edge (m5) |

`RUSTC_WRAPPER=` and `TMPDIR=/dev/shm/ev0oh-tmp` (mode 700, removed afterwards). The workspace and
oracle-diff runs were not repeated; the implementer's 6ee04017 gate reports 3,280 tests and 88/0.

## Verdict

**CHANGES-REQUIRED.** Fix M1 (one tracked test) and m1 (one doc line). m2–m6 may go to EV2a and SB8
if the manager records them. Sol's R3-M1 and R3-M2 are closed.
