# BS11a-t review — BLS decode types

Reviewed `node/bsdk-bs11at-bls-types` at `70fb3311fb0b2584dcc4b2034d736594c4f24e11`, all six commits after `33fcc59`, in `/work/home/agent/workspace/bsdk-wt/bs11at-bls-types`. Review date: 2026-10-08. Followed `briefs/REVIEW-COMMON.md`, A1 §1.4.1, A1r B-1, and DEC-24 D2. The source branch remained clean; no commits, pushes, GitHub comments, or running node instances were involved.

## Findings

No blocker, major, or minor implementation findings. One documentation nit:

**N-1 — nit — Default-state serialization does not adopt the requested scheme, contrary to the Rustdoc.**

Location: [pkgs/pkc/src/bls/wrap_lazy.rs:166](/work/home/agent/workspace/bsdk-wt/bs11at-bls-types/pkgs/pkc/src/bls/wrap_lazy.rs:166).

The documentation says that serializing under another scheme makes the object adopt that scheme. However:

```rust
let lazy = BlsLazyPk::new(BlsSchemeKind::Legacy);
assert_eq!(lazy.to_bytes_as(BlsSchemeKind::Basic), [0; 48]);
assert_eq!(lazy.scheme(), BlsSchemeKind::Legacy);
```

The same exception occurs after a failed `get()`: with neither current bytes nor an initialized object, `LazyState::serialize` takes its default-state branch and leaves the captured scheme unchanged. Both pinned Core versions behave identically. A caller relying on the documented scheme adoption would read the wrong scheme afterward.

Suggested fix: qualify the documentation to say that a **non-default** state adopts the requested scheme; a state with `buf_valid == false` and no initialized object writes zeros and retains its captured scheme. Keep the implementation unchanged. Concrete Core/Rust proof: [default-scheme-witness.json](/work/scratch/bs11at-review-gpt/default-scheme-witness.json).

## Legacy-rule assessment

I found no disagreement between the rule and the pinned relic implementations. The rule is supported by the actual arithmetic and error paths, rather than just the committed vector outcomes:

- `ep_read_bin` installs a raw sign bit in `y`; unsuccessful `ep_upk` leaves it unchanged. With the tested Core configurations (`CHECK` off, Montgomery reduction, 64-bit words), the raw value 1 represents `R^-1 mod p`, where `R = 2^384`. Independently computing its sign gives 1. Thus a non-residue G1 x below p round-trips under **either** legacy sign. The rule's handling of `BLST_POINT_NOT_ON_CURVE` is correct.
- G1's x=0 order-three exception is handled separately from a failed square root. The nonzero legacy sign encoding remains a valid wrapper object classified as `Exotic`; it does not become a usable subgroup point.
- `ep2_upk` also leaves the sign in `y.c0` on failure, with `y.c1 == 0`. `ep2_pck` derives its output sign from **y.c1 alone**, so only unsigned off-curve G2 encodings round-trip. On successful decompression with purely real y, signed encodings also fail the round-trip check. The code accounts for this in both schemes. Root selection in `ep2_upk` falls back to y.c0; recompression in `ep2_pck` does not. This distinction explains the implementation despite A1's imprecise G2 sign sentence.
- Coordinate bounds, spare bits, coordinate order, zero encodings, infinity encodings, subgroup checks in basic, and opposite-scheme fallback all matched Core. Secret-key modulo/order behavior also matched, including legacy INVALID versus basic VALID at r.

Small independent witnesses included off-curve G1 x=1 (legacy VALID with either sign), off-curve G2 x=(0,2) (legacy VALID unsigned, THROW signed), and secret key r. Outputs and complete input bytes are in [edge-witnesses.json](/work/scratch/bs11at-review-gpt/edge-witnesses.json).

The two probes were rebuilt against the O1 build trees at `/work/scratch/o1/devcache/dev/build/v23.1-plain` and `v24.0-plain`, corresponding to v23.1.8 and v24.0.0-rc.3. I verified ten relevant BLS/relic source files in each build against its release tag and checked both generated relic configurations. See [provenance.json](/work/scratch/bs11at-review-gpt/provenance.json); O1's published oracle manifests remain under `/work/cache/oracle`.

## Independent differential evidence

Every row below is a distinct byte string within its kind. Every input was evaluated under **both flags, against both Core releases**, comparing eager outcome and valid objects' serialization in both schemes. Seed: `0x601b511a7`.

| Kind | Distinct inputs | Legacy checks per Core release | Basic checks per Core release | Mismatches |
|---|---:|---:|---:|---:|
| Public key | 220,000 | 220,000 | 220,000 | 0 |
| Signature | 423,005 | 423,005 | 423,005 | 0 |
| Secret key | 220,000 | 220,000 | 220,000 | 0 |

Total: **863,005 inputs / 1,726,010 flag-specific eager comparisons per Core release**. Neither Core release disagreed with the other.

For all 643,005 curve-point inputs I additionally compared `SetBytes` + validity + `CheckMalleable` directly to `BlsValue::from_canonical` under each flag. These comparisons prevent eager fallback from concealing a discrepancy in one decoder.

The distribution included subgroup walks, on-curve points outside the subgroup, all eight leading-bit combinations, both G2 coordinate layouts, zero components, non-residues, one-bit mutations, random byte strings, coordinates around p and powers of two, x ≥ p, canonical and noncanonical infinity, and scalar boundaries around r and 2r. At least 100,000 random draws were added after the structured inputs for each kind.

Two additional checks exercised representation rather than just acceptance:

- **20,944 G1 and 27,488 G2 inputs**: compared Core subgroup validity with `Usable`/`Exotic` classification and compared full uncompressed coordinates for usable points, under both flags.
- **1,600 G1 and 7,040 G2 inputs**: independently re-encoded actual torsion points and torsion-plus-subgroup points in both layouts, with all leading flags, and tested purely real and purely imaginary G2 y. This covered every torsion family in the generator, including the large G2 cofactor factor. Eager, direct canonical, and native-point comparisons all agreed.

Scripts, inputs, outputs, counts, and checksum files are retained under `/work/scratch/bs11at-review-gpt/`. The principal artifacts are [differential.py](/work/scratch/bs11at-review-gpt/differential.py), [summary.json](/work/scratch/bs11at-review-gpt/summary.json), [native-summary.json](/work/scratch/bs11at-review-gpt/native-summary.json), and [torsion-summary.json](/work/scratch/bs11at-review-gpt/torsion-summary.json).

## Lazy mutation and concurrency

I compared the state machine against `CBLSLazyWrapper`, including raw-byte retention, zeroing after failed `Get`, serialization before initialization in the opposite scheme, captured-scheme changes, hash changes, default versus explicitly initialized invalid objects, cloning, and equality side effects. Longer randomized runs contained up to 30 operations plus explicit initial/final hash and serialization checks.

- **40,000 lazy scripts** and **20,000 prepared pairs**, each pair compared in both orders on fresh objects, matched both Core releases exactly. Results: [lazy-summary.json](/work/scratch/bs11at-review-gpt/lazy-summary.json).
- **320,000 shared-reference operations across 16 threads**, including reversed comparisons, cloning, `get`, hashing, and serialization, completed with the expected initialized value. Another **1,000 concurrent `get`/opposite-scheme serialization races** produced only the two legal outcomes: preserve the decoded point or lose the uninitialized raw buffer. Results: [concurrency.log](/work/scratch/bs11at-review-gpt/concurrency.log).

The `Mutex` protects all shared mutable Rust state; `K::Point: Send + Sync` supports the exposed `Send`/`Sync` types. `get()` returns an owned snapshot rather than an unlocked reference to mutable storage. Address ordering for dual-lock comparisons avoids deadlock, and both locks are released before the fallback `get()` calls, as in Core. `set` and `set_scheme` require exclusive Rust access. I found no shared-reference mutation or concurrency defect. The spin lock differs from Core's blocking mutex in scheduling behavior; the observable sequential state transitions agree.

## Local validation and scope

- `cargo test -p dash-pkc --features full`: **444 unit tests and 37 wrapper tests passed**; the generated-sample test was initially ignored.
- `cargo clippy -p dash-pkc --features full --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo build -p dash-pkc --no-default-features`: passed, including context types without the BLS backend.
- `git diff --check 33fcc59...HEAD`: passed.
- Regenerated the committed corpus with freshly rebuilt probes for both releases: **byte-for-byte identical**, SHA-256 `c9580aeb6d54b07270c7ada558fcf67e2339d470afb2b9859be34521525c08b5`.
- `BLS_WRAPPER_FUZZ=/work/scratch/bs11at-review-gpt/expected.jsonl cargo test --release -p dash-pkc --features full --test bls_wrapper -- --ignored generated_differential_sample --nocapture`: **passed, 863,005 inputs under both flags**. See [ignored-differential-test.log](/work/scratch/bs11at-review-gpt/ignored-differential-test.log).

This review covers BS11a-t's decode types, serialization, equality, and lazy state transitions. It does not establish exotic-point verification/aggregation parity or replace BS11a's longer differential-fuzz acceptance campaign. Those are separate from the decode rule validated here. Approval assumes the pinned Core release configurations recorded above.

**Verdict: APPROVE-WITH-NITS**
