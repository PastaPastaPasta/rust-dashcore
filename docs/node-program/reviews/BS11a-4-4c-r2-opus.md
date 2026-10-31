**Verdict: ACCEPT WITH NITS**

# BS11a-4 4b r1-fix re-check and 4c review — r2 (Opus, high)

- Fork: base-sdk `node/bsdk-bs11a-4` `4a6a9e45..84917742` (`bcd494ae` port, `c20d2e04` probe and corpus, `cec0ee36` 4b with the r1 fixes, `84917742` 4c `resolve`). Fork worktree clean.
- rdc: `node/bs11a-4` head `5840956e4` (new since r1: `841bdc7f9`, `40390f253`, `bc207cd0c`, `0be95b550`, `5840956e4`). The rdc worktree has uncommitted 4d work in `node-fuzz/` and `node-l1/`. Nothing I built depends on it, and I left it untouched.
- Core reference: v24.0.0-rc.3 `0364ed626e`, read with `git show 0364ed626e:<path>`. `~/workspace/dash` is checked out at `15db4787500`, not at the pin.
- Inputs: r1 (`reviews/BS11a-4-4b-r1-opus.md`), DEC-482, DEC-485, DEC-492.

Findings: **0 Blocker, 0 Major, 1 Minor, 3 nit.**

## Gates I ran

| Gate | Result |
|---|---|
| fork `cargo test -p dash-pkc --features full` | 643 passed, 0 failed (lib 573 = r1's 571 + `resolving_keys_changes_no_verdict` ×2; bls_dashbls 7; bls_oracle 14; bls_serial_threads 1; bls_wrapper 48 + 1 ignored) |
| fork `cargo clippy -p dash-pkc --all-targets`, `--features full` and `--no-default-features --features bls`, `-D warnings` | clean |
| `probe pairs 48 bs11a4-corpus`, rebuilt from the fork's `probe.c` with its `build.sh` against `librelic.a` sha256 `be445a04…` and blst 0.3.17 | **byte-identical** to `pkgs/pkc/corpus/bls_pairing.json5` (sha256 `287cf2d7…`, as PROVENANCE records). Every class appears, including the new `edge` (20) and `projective_offcurve` (28) |
| `resolve` mutants (one env-gated build in a scratch `git archive` export): no-op; every `OnCurve` → `Usable`; a non-subgroup `OnCurve` → `Exotic` | **all three killed** by `resolving_keys_changes_no_verdict`, in both cases (the first two at `core/mod.rs:476`, the third by the equality assert at `:477`) |
| rdc `cargo test -p dash-evo -p dash-dml -p dash-coinjoin` at `5840956e4` | all green; dash-evo lib 156 passed (r1: 157 with 1 failing; the failing test is deleted) |
| rdc `cargo test -p dash-evo --bench bls_core -- bls_verify` (criterion test mode) | the six D64 rows each run and assert `Valid`: Success ×6 |
| rdc `cargo clippy -p dash-evo -p dash-dml -p dash-coinjoin --all-targets --all-features -- -D warnings`; `cargo fmt --check -p dash-evo` | clean |
| rdc `contrib/node/check-quirks.py` | exit 0: 45 quirks, 33 tested, 12 scaffold |
| stale pins | `git grep 4a6a9e45` outside docs: none; all 12 pin sites are at `84917742…` |

Builds used `RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=8`, in the worktrees' own targets. The mutant build used a reflink-seeded target under `/work/scratch/bs11a-4/review/r2-mut/`.

## 1. r1 findings, as DEC-485 ruled

| r1 | Verdict | Evidence |
|---|---|---|
| **Major 1** (dash-evo `a_single_member_check_without_a_verdict` fails at the pin bump) | **Resolved.** | `40390f253` moves every `[patch]` to `84917742` and deletes the test with its `OffSubgroup`/`OffSubgroupRoster` helpers and the now-unused `BlsScChia` import, in the same commit (DEC-492 (2)). The commit body carries "premise removed by 4b (BL-400)". D2 "Landed (4b)" records it. The dead downstream `Unsupported` arms are deferred to BL-400 (D2, DEC-485 (1)). dash-evo is green, clippy is clean, and no reference to the helpers is left in code |
| **minor 2** (chunk size not observable) | **Resolved.** | `native.rs:69-73`: the `CHUNK` doc says the final exponentiation is multiplicative (D5) and the chunk size is not observable. `cores_product` hard-codes `250` with `schemes.cpp:337-341`, which is the loop at the pin. The test doc says it "cannot pin [`CHUNK`]". D1 keeps the 250 as Core's structure, "structural, not observable", and D6 says "model check" |
| **n3** (CodeQL `Operand` exemption) | **Resolved.** | `policy.qll:269-274`: a separate disjunct anchored with `fileOf(t).getAbsolutePath().matches("%/pkc/src/bls/native.rs")`; `BlsRepr`/`Ep`/`Ep2` keep the crate-wide form |
| **n4** (Core asserts, it doesn't return false) | **Resolved.** | `core/mod.rs:147-149` and the test doc at `:400-404` now cite the assert at `bls.cpp:319` (checked at the pin) and say that the port answers `Invalid` there |
| **n5** (basic fold holds only Group keys) | **Resolved.** | `core/mod.rs:209-213`, comment option |
| **n6** (x = 0, y ≠ 2 redecode) | **Resolved.** | `elem.rs:138-139` comment; the test at `elem.rs:998-1018` builds X = 0 sums for k = 1..3 and ±Q. It asserts that the bytes are zero but for the sign (so X = 0 is checked, not assumed), that the sum is not (0, ±2), and that redecode gives (0, 2) or (0, −2) by the sign bit |
| **n7** (name the quirk 45 replay) | **Resolved.** | `bls_oracle.rs:1210-1218` `quirk_45_exotic_bls_operands_by_flag` runs `assert_edge`, and its rstest case is removed, so the replay runs once. The `FILES` doc names it. But see Minor 1 on what the stamp claims |
| DEC-485 (4) (batch test doc) | **Resolved.** | `batch.rs:386-390` states only Core's order of operations (`bls_batchverifier.h:71-119,146-228`). The cancelling-pair outcome assertions are gone. The replacement asserts an ordinary bad message (`{2}`) and `!unsupported()` |

## 2. 4c `resolve` (fork `84917742`)

**It matches DEC-492 (3).**
- `BlsPoint::resolve` (`wrap_codec.rs:112-121`) turns an `OnCurve` point that passes `in_subgroup()` into `Usable` and leaves every other variant as it is.
- `BlsValue::resolve` (`wrap_value.rs:109-115`) delegates through `BlsWire::resolve`. Its default is a no-op, so secret keys are unaffected, and `impl_wire_point` overrides it for G1 and G2.

**No verdict can change.**
- `BlsElem::from_point` (`elem.rs:358-373`) maps `Usable(a)` and an in-subgroup `OnCurve(a)` to the same `group(a, tag)`, with the tag from the stored repr, so `resolve` changes no element or tag. A value whose repr is `Relic` returns its engine coordinates before the point is read (`elem.rs:351`), so that is unchanged too.
- `pairing_operand` (`elem.rs:383-389`) reads `usable()`, which gives the same `Some` before and after.
- `redecode`'s non-Exotic arm (`elem.rs:143`) reuses the stored point. So in `verify_secure_aggregated`, a resolved key skips the fold's subgroup test as well, which is where the D9 rows' saving comes from.

**Equality and hashing are unchanged.** `Element::eq` is bytes plus `BlsPoint::eq`, which compares `held()`, and `held()` is `Some` for both `Usable` and `OnCurve`. `Hash` covers the bytes only (`wrap_value.rs:233-250`, `wrap_codec.rs:130-140`).

**The rejected alternative would break equality.** Turning non-subgroup points into `Exotic` makes `held()` `None` and breaks equality, as D8's "Landed (4c)" says. The third mutant above confirms the test catches exactly that.

**The test is not tautological.**
- It asserts the state transition both ways (`Usable` for the subgroup key, still `OnCurve` for `nonsub_000`), equality with the unresolved key, and the verdicts of `verify_insecure` and `verify_secure_aggregated` against the unresolved ones, under both flags.
- It then pins the absolute verdict (`Valid` under legacy, `Invalid` under basic).
- The verdict comparisons are equal by construction (Lemma U), so the state and equality asserts carry the test. All three mutants die on them.

Citations: the 4c fork code adds none. The rdc notes' Core and relic spans I checked at `0364ed626e`:
- `schemes.cpp:337-341`, `bls.cpp:319` and `commitment.cpp:71/90/104` are exact.
- `relic_pp_map_k12.c:51-133` and `:523-599` are exact.
- `relic_fp12_mul.c:320-420` (now in `pp.rs`) covers the function plus `#endif` (the function ends at 418). Harmless.

## 3. D64 bench rows (rdc `bc207cd0c`)

They measure what they claim:
- **Unresolved rows.** `legacy_vectors` asserts that every key is `OnCurve` after legacy decoding.
- **Resolved rows.** `legacy_keys(…, true)` resolves the keys and asserts that every one is `Usable`.
- **Setup stays outside the timed closure.** That covers the fixture (a `LazyLock`, built on first use), the clone and the resolve. The closure calls `verify_insecure` for the one-key row and `verify_secure_aggregated` for 50 and 400, then `assert!(verdict.is_valid())` (`is_valid` is `matches!(self, Valid)`, `core/mod.rs:119-121`).
- **Nothing can be optimized away.** The verdict is consumed by the assert, and the work runs through opaque blst FFI calls, so it can't be hoisted out of the loop.
- **Signatures.** The secure rows' signature is the typed API's `secure_aggregate` over the same keys. All six rows pass in test mode.
- **Table entries.** `workloads.toml` maps the six ids one to one.
- **Ratios.** The D9 table's ratios follow from its counts: 14.17/38.55 = 0.37 … 384.44/1398.33 = 0.27, and 571.96/1398.33 = 0.41.

## 4. Fork 4a commits: unchanged in substance

The commit order is now port, then probe:
- `bcd494ae` against r1's `f4b17c3a` differs only by the corpus, which moves to the probe commit, and by docs in `pp.rs`/`blst_ffi.rs` ("model check of the port, not of relic", the `320-420` span).
- The tree at `c20d2e04` against r1's `f4b17c3a`:
  - the probe gains the `projective_offcurve` and `edge` classes, a 32-byte seed limit, and the command in the file header;
  - the corpus is regenerated, and the replay test also asserts the new classes, an all-infinity call and a multi-pair call;
  - the pairing code is unchanged.
- These changes strengthen 4a's evidence. The seed name is kept, as DEC-492 (4) accepts, and the corpus reproduces byte for byte.

## Findings

### Minor 1: quirk 45 is stamped `tested`, but its aggregate-then-verify clause has no test at the pin

- **Where:** rdc `docs/node-program/quirks.toml:445-446` (`5840956e4`); fork `pkgs/pkc/tests/bls_oracle.rs:1210-1214` (the doc of `quirk_45_exotic_bls_operands_by_flag`); notes D12 (`BS11a-4-notes.md`, "Landed (4b)").
- **What:**
  - The quirk's title has three clauses: the direct verifies under the basic flag; the same under the legacy flag; and "aggregation and the IES DH key compute with relic under either flag, so aggregate-then-verify paths apply the basic gate to the aggregate only".
  - The named test runs `assert_edge`, which covers the direct verifies only (insecure, aggregated and secure ops).
  - Its doc assigns aggregation and DH to `arith`'s rows, and aggregate-then-verify to `diff_bls_verify` (D10). That target does not exist at `5840956e4` (`git grep diff_bls_verify`: nothing), and D10 is 4d content.
  - No oracle row at the fork pin aggregates and then verifies: `arith.json5` has no verify step, and `relic.json5`'s verify steps are direct.
  - So the register's definition of `tested` ("the test runs and asserts Core's behaviour on Core's vectors") holds for two of the three clauses. The plan had put this stamp in 4d, beside D10. `5840956e4` moves it ahead of the evidence it cites.
- **Fix:** do one of these.
  - (a) Hold `status = "scaffold"` (and the conformance scaffold) until D10 lands, as the 4d row planned.
  - (b) Keep the stamp, but make the gap explicit and tracked: in the quirk test's doc and in D12, say that the aggregate-then-verify clause is evidenced by D10 when it lands, and put "quirk 45's aggregate-then-verify evidence (D10)" in the 4d row's content.

  Either way, don't cite a target that isn't in the tree as if it already ran.

### nit 1: notes churn between commits; the 4c and 4d rows don't match where the work landed

- **Where:** rdc `BS11a-4-notes.md:48` (D2), `:71` (4c row), `:72` (4d row); commits `841bdc7f9` and `0be95b550`.
- **What:**
  - `841bdc7f9` writes "deleted at 4d's pin bump" into D2 and the 4d row. Two commits later, `0be95b550` (titled "record D9's rows and Core's one-off count") rewrites both to "the pin bump … made with 4c". DEC-492 (2) had already moved the bump into 4c before `841bdc7f9` was written.
  - The 4d row still lists "pin, stamp re-audit, quirk 45 `tested`" as 4d content. Only the test deletion is marked "landed early with 4c", though `40390f253` and `5840956e4` land all four in this range. The 4c row lists none of them.
  - Optional: `docs/node-program/designs/LQ2-notes.md:64` (D11) still names `a_single_member_check_without_a_verdict` among the tests BS11b's runner "must pass unchanged".
- **Fix:**
  - On the next rewrite, write D2 and the 4d row as "4c" in `841bdc7f9`, and drop those hunks from `0be95b550`.
  - Move the pin, the stamp re-audit, the test deletion and quirk 45 into the 4c row, or mark each "landed with 4c" in the 4d row.
  - Optional: add "(deleted at BS11a-4 4c, D2)" after the test name in LQ2 D11.

### nit 2: D8's superseded text cites stale `elem.rs` lines

- **Where:** rdc `BS11a-4-notes.md:54` (D8, "`BlsElem::from_impl` rebuilds it … `elem.rs:311-331`").
- **What:** at `84917742`, lines 311-331 are `g2_group_eq` and the `impl_elem!` header. `from_impl` is at `elem.rs:345-356` and `from_point` at `:358-373`. The "Landed (4c)" text marks the sentence as superseded, but the cite still points readers at the wrong code.
- **Fix:** `elem.rs:345-373`.

### nit 3: the bench doc says the secure verify checks "an LLMQ quorum signature"

- **Where:** rdc `dash-evo/benches/bls_core.rs:226-228` (`LegacyVectors` doc).
- **What:** Core checks a commitment's **members** signature with `VerifySecureAggregated` (`commitment.cpp:90` at rc.3). The quorum signature is a `VerifyInsecure` against the quorum key (`:104`).
- **Fix:** "…for `verify_secure_aggregated`, which checks a commitment's members signature so (`llmq/commitment.cpp:90`)".

## Scratch to delete at cleanup

`/work/scratch/bs11a-4/review/r2-mut/` holds the `git archive` export with the env-gated mutant, its reflinked target, the rebuilt probe and the regenerated corpus. It is not a git worktree, so `rm -rf` is enough.
