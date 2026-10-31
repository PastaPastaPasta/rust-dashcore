# BS11a-t BLS decode types — GPT focused review, round 2

Reviewed `node/bsdk-bs11at-bls-types` at `97096e742c761035502dbbdb742b9af3748a7b76`, the six commits `e45ca9e..97096e7` after `70fb331`, in `/work/home/agent/workspace/bsdk-wt/bs11at-bls-types`. Date: 2026-10-08. Followed `briefs/REVIEW-COMMON.md` and the code-review-validator workflow. Read both round-1 reviews and DEC-46; the settled legacy rule is not reopened.

The branch under review remained clean. No source edits, commits, pushes, GitHub comments, or node instances were involved. Scratch evidence is under `/work/scratch/bs11at-r2-gpt/`; tests used `TMPDIR=/dev/shm/bs11at-r2-gpt-tmp`, mode 0700, removed after validation.

## Findings

Blocker 0 · major 0 · minor 1 · nit 0.

**M2-P — minor — An individual `get()` can retry indefinitely under continuing scheme changes.**

Location: [wrap_lazy.rs:101](/work/home/agent/workspace/bsdk-wt/bs11at-bls-types/pkgs/pkc/src/bls/wrap_lazy.rs:101), retry loop at line 181.

Concrete schedule:

1. Begin with a fresh nonzero raw basic signature/public key, `buf_valid = true`, `obj = None`.
2. `get()` snapshots and decodes it. Another shared reference serializes under legacy, losing the uninitialized raw buffer and leaving a current zero buffer. Publication correctly rejects the old decode.
3. On every subsequent iteration, `get()` snapshots zeros and the current scheme. Between snapshot and publication, another shared reference serializes under the opposite scheme. The buffer remains zero and `buf_valid` remains true, but the changed scheme makes `publish()` return `None` again.

This schedule can repeat indefinitely even while `get()` obtains both locks on every iteration. Serializers finish, so this is starvation/livelock of the individual getter, not a system-wide deadlock. Core's getter completes its decode and state transition once it acquires its mutex; the new implementation is linearizable but does not preserve that progress property. Once scheme churn stops, the Rust getter terminates correctly.

I replayed **100,000 consecutive rejected zero-buffer publications** using the exact `LazyState`/`cached`/`publish`/`serialize` definitions extracted from the reviewed file, with the actual library decoder. Publication succeeded immediately when churn stopped. This is a deterministic allowed schedule, not a spontaneous hang observed in the threaded race tests. Evidence: [check_publication.py](/work/scratch/bs11at-r2-gpt/check_publication.py), [publication.rs](/work/scratch/bs11at-r2-gpt/publication.rs), [publication.log](/work/scratch/bs11at-r2-gpt/publication.log).

Suggested fix: handle an uninitialized, current all-zero buffer under the snapshot lock by setting `buf_valid = false` and returning invalid. Core's `SetBytes` resets zeros under either scheme, so no decompression or scheme validation is needed in that case. Alternatively, allow an invalid zero-buffer decode to publish against current zeros despite a scheme change, preserving the current scheme. Either approach bounds retries for the exposed shared-reference transitions. A bounded retry fallback that decodes under the lock also works.

This is nonblocking for this focused review: it requires continuing opposite-scheme serialization before initialization, produces no incorrect value or state, and there is no documented starvation-free guarantee. The progress limitation should nevertheless be fixed or documented.

## M2: linearizability, mutation, ABA, and stale publication

The implementation is linearizable to **Core's fresh-wrapper state machine** for the operations exposed by the Rust API. The proof does not assume that a shared/const reference leaves the state unchanged.

Write the observable state as `(buf, buf_valid, scheme, obj)`. The mutex protects every shared-reference read and write of this state. Decoding outside the mutex changes only local values. `set()` and `set_scheme()` require exclusive Rust access and therefore cannot legally race these calls.

There are three completion cases:

- **Cached result at the first lock:** returning an initialized object or invalid default state linearizes at that locked observation.
- **Cached result at the publication lock:** another getter has already initialized the object or invalidated the buffer. Returning that current result linearizes at the publication lock. The local decode is discarded even if its old bytes/scheme differ.
- **No cached result:** `cached()` establishes `obj == None && buf_valid == true`. Publication then requires both the entire byte array and scheme to equal the snapshot. Under this same lock, success inserts the decoded object; failure clears `buf_valid`. This linearizes at the publication transition and is exactly a Core `Get()` on the current bytes/scheme: `SetBytes`, validity, round-trip check, then initialization or invalidation, with no opposite-scheme fallback.

A failed publication changes no state and has no observable effect. Retrying can therefore move the operation's linearization point later without violating real-time ordering. Every intervening serialization can be ordered before that successful publication, matching Core's mutation through a const reference. In particular, a serializer that first discards the raw buffer cannot be followed by resurrection of a decode of those discarded bytes.

**ABA:** a scheme can change `Basic → Legacy → Basic` while an uninitialized zero buffer remains current. I exercised this in `publication.rs`; publication of its invalid decode is correct. Equal current bytes plus equal current scheme imply the same deterministic decode and transition, so a generation counter is unnecessary. Nonzero raw bytes cannot disappear and then reappear through the available shared-reference API: without an initialized object, a scheme-changing serialization can only replace them with zeros. An initialized object is terminal for decoding, and `publish()` returns it before comparing snapshots. A failed getter similarly produces a default state that later publications recognize before snapshot comparison. Thus neither byte/scheme ABA nor a competing getter can publish a stale object.

The scope qualification matters: Core can `Unserialize` through a const reference into an already-used wrapper and retain a hidden stale `obj`. Rust exposes construction of a fresh wrapper, not that reuse operation. The new class documentation states this limit accurately. A returned Rust `BlsValue` is an owned snapshot; the proof concerns its value and wrapper effects, not C++ reference identity or subsequent replacement of a referenced object.

## M3: basic `SetBytes` and equality

I checked `CBLSWrapper::SetBytes`, dashbls `FromBytes`/`FromBytesUnchecked`/`CheckValid`, serialization, and relic compression/decompression/equality in both pinned Core releases.

The claim about the only possible acceptance difference from canonical basic decoding is correct:

- Wrong lengths and zeros yield invalid. Canonical infinity also yields an invalid wrapper. Malformed flags, coordinates outside the field, failed decompression, and points outside the subgroup remain rejected.
- G1 basic decoding preserves its input serialization for every accepted nonidentity subgroup point.
- G2 retains the subgroup check in `g2_decode_basic`. Relic decompression selects the sign using `y.c1`, falling back to `y.c0` when `y.c1 == 0`; compression uses `y.c1` alone. Consequently the only possible changed serialization of an accepted G2 input is a **set sign bit with `y.c1 == 0`**, which gets cleared. Coordinate order and all other flags/coordinates remain unchanged.
- Basic secret keys use `modOrder = false`, accept `0 < k <= r` including unreduced `r`, and serialize exactly as supplied. There is no secret-key round-trip exception.

At [wrap_codec.rs:255](/work/home/agent/workspace/bsdk-wt/bs11at-bls-types/pkgs/pkc/src/bls/wrap_codec.rs:255), canonical basic decoding is literally `decode_set_basic` filtered by serialization equality. The new constructor correctly omits this filter and returns invalid rather than throwing or trying another scheme. The `g2_basic_sign_of_real_y_serializes_cleared` unit test demonstrates the compression asymmetry on an actual real-y curve point and verifies that its subgroup check rejects it. Neither that test nor the differential establishes the existence or nonexistence of such a **subgroup** point; the implementation handles the conditional case without needing that claim.

`Valid::eq` at [wrap_value.rs:168](/work/home/agent/workspace/bsdk-wt/bs11at-bls-types/pkgs/pkc/src/bls/wrap_value.rs:168) now compares both serialization and points. This matches Core's element comparison on the represented domain:

- Equal usable points have equal deterministic Core serializations, so the byte precheck introduces no false negative. The affine equality calls blst's `blst_p1_affine_is_equal` / `blst_p2_affine_is_equal`, rather than comparing Rust object identity or projective representations.
- If a real-y subgroup point and its negation serialize identically, point comparison still distinguishes them, as Core's `g2_cmp` does.
- Legacy exotic values retain the DEC-46 representation: accepted canonical bytes determine the normalized relic coordinates, and usable/exotic classification is deterministic. Comparing equal bytes plus the classification introduces no new legacy discrepancy.
- Secret points are `()`, so secret equality remains the constant-time byte comparison. Invalid wrappers still equal each other only. Hashing just the serialization remains lawful; unequal points may collide, but equal values have equal hashes.

## Documentation and `serialize_hash`

The public/private rename and test call sites are complete. The implementation is still double SHA-256 of serialization under the captured scheme, with raw bytes covered before initialization and zeros after a failed decode, matching Core's lazy `GetHash`. No hash-trait implementation was introduced.

The corrected `to_bytes_as` documentation now includes the default/failed-decode exception to scheme adoption. The new fresh-wrapper limitation and explanation of bare scheme arguments are accurate. The changelog describes the round-trip-free basic constructor. No additional documentation or naming findings.

## Validation

- `cargo test --offline -p dash-pkc --features full`: **448 unit tests and 41 wrapper tests passed**, one generated test ignored.
- Repeated all three threaded tests (`contended_lazy_gets_agree`, `lazy_get_racing_other_scheme_serialization_ends_in_a_core_order`, `concurrent_lazy_comparisons_finish`) **30 times each in debug and release**, using the corresponding locally built test binaries. Per profile: **1,500 eight-thread first-decode contests / 12,000 getters**, **15,000 get/other-scheme serialization races**, and **120,000 reversed comparisons**. All passed. Evidence: [repeat-races-summary.json](/work/scratch/bs11at-r2-gpt/repeat-races-summary.json), [repeat-races.log](/work/scratch/bs11at-r2-gpt/repeat-races.log).
- `BLS_WRAPPER_FUZZ=/work/scratch/bs11at-fix/fuzz-v24.jsonl cargo test --offline --release -p dash-pkc --features full --test bls_wrapper -- --ignored generated_differential_sample --nocapture`: **300,000 rows passed**, checking both eager flags and basic `SetBytes`. Evidence: [differential-300k.log](/work/scratch/bs11at-r2-gpt/differential-300k.log).
- Independently rebuilt the updated Core probe against `/work/home/agent/workspace/dash-wt/oracle-shim-v23.1` and `oracle-shim-v24.0` and replayed all **300,000 inputs against each**, checking eager outcomes/serialization under both flags and basic `SetBytes`. Both releases matched every row: **0 mismatches**. The build trees have no changes under `src/bls`/`src/dashbls` relative to their respective `v23.1.8` / `v24.0.0-rc.3` tags; relic is CHECK off, Montgomery reduction, 64-bit words. Evidence: [recheck_core.py](/work/scratch/bs11at-r2-gpt/recheck_core.py), [core-recheck-summary.json](/work/scratch/bs11at-r2-gpt/core-recheck-summary.json).
- Sample distribution: **100,000 rows per kind**; distinct byte strings: **70,696 public keys, 81,948 signatures, 18,289 secret keys**. Repeated generated rows are not counted as distinct coverage. Input SHA-256: `51bcb4aaa94879d0ff9daa2a3b4c51931f5e328c75b88b0f52833d1922588d21`.
- `cargo clippy --offline -p dash-pkc --features full --all-targets -- -D warnings`, `cargo fmt --all -- --check`, `cargo build --offline -p dash-pkc --no-default-features`, and `git diff --check 70fb331..97096e7`: **passed**.

These checks address the three requested changes. They do not replace the remaining BS11a arithmetic/verification campaign or the manager's wider integration gate.

**Verdict: APPROVE-WITH-NITS**
