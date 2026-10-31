# BS6a review r2 (Opus): narrow closure check

**Verdict: APPROVE-WITH-NITS.**
- base-sdk `node/bsdk-bs6a-v24-shapes` at **73ab4b0** can merge into `node/base-sdk`.
- rdc `node/bs6a-adapt` at **43b8ebae** is ready to feed the combined BS5+BS6a re-pin (DEC-72), with the carry-over notes in §4.
- No blocker or major is open. The new items (N1–N4) are nits.

Scope:
- base-sdk 481f8c3..73ab4b0 (18 commits). Against r1's 9094d81:
  - 14 commits rebased, of which 3 changed: 4bfb8fc (F6), 550a249 (PLAN §7 handling; see the manager) and b64d9f1 (a comment in `gen.py`);
  - 4 new commits: ac9634d, 60d6090, 8685005, 73ab4b0.
- rdc 30be7902..43b8ebae (6 commits).
- Rulings: DEC-71, DEC-72 and DEC-79.

Fresh worktrees and logs are in `/work/scratch/bs6a-r2-opus/` (`bsdk`, `rdc`, `bisect`, `pick`, `bench`, `*.log`).

## Correction to my round 1

r1 F1 said that every pooled path in blst 0.3.17 is behind blst's own `std` feature, so `threadpool` was "linked but unreachable". **That was wrong.**
- blst 0.3.17 has no `std` Cargo feature. Its features are `default`, `force-adx`, `no-threads`, `portable` and `serde-secret`.
- `build.rs:51-52` emits `cargo:rustc-cfg=feature="std"` on every target except none, uefi and wasm32-unknown.
- So the pool is compiled on every hosted target, and a single `Signature::verify` uses it.

DEC-79 is right to drop the "assert blst built without std" clause that came from my F1(b). Call-level bans are the only control short of `no-threads`.

## Re-verification

| Check | Result |
|---|---|
| base-sdk `cargo nextest run -p dash-primitives -p dash-pkc -p dash-types --all-features` | **873 passed**, 1 skipped |
| base-sdk clippy `-D warnings`: primitives, pkc and types, each `--all-targets --all-features` and `--no-default-features` | all 6 exit 0 |
| base-sdk `cargo doc -D warnings` (primitives, pkc) | exit 0 |
| `lint_commit.py -r 481f8c3..73ab4b0` | 18/18 PASS |
| Signatures | all 18 base-sdk and 6 rdc commits carry a `gpgsig` |
| rdc `check-pure.sh` at 43b8ebae | **exit 0** (2 crates; blst and threadpool allowed only through their wrappers) |
| rdc `selftest-checks.sh` at 43b8ebae | **all 13 DEC-71/79 cases ok** (list in §3). The 6 other failures are the expected provisional-pin ones: `fork rev 73ab4b0… is not on node/base-sdk`. They are the same 6 as the implementer's `selftest-dec79.log` |
| rdc `cargo check --locked --workspace --all-targets --all-features` at **each** of the 6 commits (my own run, `bisect.log`) | exit 0, 0 warnings, at every commit |
| rdc `cargo test -p dash-conformance` | 75 passed, 0 failed |
| Bench (§5) | reproduced |

## 1. Round-1 item dispositions

| # | r1 item | Disposition | Evidence |
|---|---|---|---|
| F1 | major: DEC-68 unimplemented | **CLOSED** | Implemented as DEC-71/79 rule it (§3). `pure-deny.toml`: threadpool `wrappers = ["blst"]`, blst `wrappers = ["dash-pkc"]`. `pure-clippy.toml`: 34 bans. `check-pure.sh`: blst version guard and audit stamp. 13 selftests. My F1(b) is withdrawn (see the correction above). F1(c) was overtaken: the bans are needed today, not only "if a crate enables `blst/std`" |
| F2 | minor: commit order, nested pins, CS0 call site | **CLOSED** | The gate commit c62d5e99 comes first and builds at the old pin (scripts only). 083e829f folds the pin and the minimal adapt into one commit that builds. Every later commit builds (my own per-commit check). `cs2-diff` moves with the root. `dde-probe` is untouched (no diff under `contrib/node/dde-probe`). The CS0 call site needs **no** change any more: `decode_payload()` is arity-free again, matching CS0's `self.raw.decode_payload()`. What remains is stale code in CS0 (§4) |
| F3 | minor: SIG-05 owner and decode-context API | **CLOSED (ruled)** | DEC-71: SIG-05 → BS6c, single decode-context API. Bookkeeping: PLAN's BS6c row (PLAN.md:456) still says only "v23.1 ProTx layout … DomainPort". Add SIG-05 (`bad-cbtx-version` derivation) and r1's structured `PayloadError` (a `DecodeError` plus an offset, not a `String`), so the BS6c implementer sees them |
| F4 | minor: BASIC default; context-free alternative | **CLOSED** | Option (b) implemented: `BlsGlobal` (§2). `BlsCtx` is gone from `SpecialPayload::decode`, `Transaction::decode_payload` and `check()`. The footgun is gone by construction |
| F5 | minor: blst a hard dependency | **CLOSED (ruled)** | DEC-71 → a BS14/CV3 note. Bookkeeping: neither PLAN row (467, 475) mentions it yet |
| F6 | nit: semver label | **CLOSED** | 4bfb8fc is `types%feat!`. The CHANGELOG entry moved to "Changed" and tells callers to add an arm. `#[non_exhaustive]` was deferred as BL-42 (DEC-79) |
| F7 | nit: expose the version→scheme mapping | **CLOSED** | `protx_bls_scheme` and `commitment_bls_scheme` are public (8685005). The harness keeps its own copies on purpose, as an independent oracle, and asserts agreement over all 65,536 versions (`conformance/src/payload.rs:612-626`). That is the right shape for a conformance tool |
| F8 | nit: `decode_sptx` default body | **CLOSED** | `SptxDecode` is removed entirely (60d6090). This supersedes DEC-71's "loses its default body". No reference remains in base-sdk (grep) or in rdc |
| F9 | nit: lazy re-encode depends on call history | **CLOSED** | `bls::lazy_bytes` (`payload/bls.rs:113`) resolves a *clone* and writes the result under the version's scheme at encoding. Encoding, hashing and JSON are therefore independent of history, and the field is left untouched. It is documented on each `pub_key_operator` and tested (`lazy_keys_encode_alike_before_and_after_use`). It knowingly differs from Core for an unused invalid key (Core writes the raw bytes). This is not consensus-observable: `CProRegTx`/`CProUpRegTx`/`CProUpSharedRegTx::IsTriviallyValid` calls `pubKeyOperator.Get().IsValid()` (providertx.cpp:300, 468, 579, v24.0.0-rc.3) before any `SerializeHash(*this)` (`MakeSignString`, :392) |
| F10 | withdrawn | — | PLAN §7 handling; see the manager |
| F11 | minor: follow-up list | **CLOSED (ruled)**, with bookkeeping | DEC-71/79 assign every item: DEC-68 implementation (done here), CS0 call site (now moot, §4), SIG-05 → BS6c, AU v2 txid → BS6b (PLAN.md:455 carries it), blst → BS14/CV3, deployment-capped versions → EV1, BL-25's base-sdk part closed. Ledger drift remains: BACKLOG BL-25 still lists "add payouts, shares and `merkleRootAssetUnlocks`", which DEC-71 closed and the adapt branch does. Rows BS6c, BS14 and CV3 do not carry their DEC-71 items. The BS12 row (PLAN.md:465) does not carry DEC-79's serial pool-free verify, which gates EV1/LQ1a |

## 2. `BlsGlobal` (scheme-free decode, `get(BlsCtx)`) and the removal of `SptxDecode`

**Semantics: correct.** I checked them against Core's `CBLSWrapper::Unserialize(s, specificLegacyScheme)` (bls.h:184-203, v24.0.0-rc.3). Core first tries the flag's scheme. If that fails, it tries the other scheme and then `Reset()`s (the object is invalid). It throws only when both fail.
- `decode` (`wrap_global.rs:56`) is `is_zero || canonical(Basic) || canonical(Legacy)`. That is exactly the set Core does not throw on, under either flag. The verdict is symmetric, which r1 observed on 812 Core outcomes.
- `get(ctx)` (`:68`) gives `from_canonical(bytes, ctx)`, or invalid. Core gives the same object:
  - a valid object when the bytes are canonical in the flag's scheme;
  - invalid after `Reset()` when they are canonical only in the other scheme;
  - invalid for zeros (`CheckMalleable` compares zeros with zeros).
- `from_eager(field, ctx)` (`:84`) is `ToBytes(flag)`.
- The new pkc test `global_bls_fields_decode_without_the_flag` checks all three against **every** pk and sig row of the Core-generated `bls_wrapper.json5` corpus, under both flags:
  - the throw verdict;
  - the valid/invalid outcome;
  - the round trip under the read flag;
  - the bytes written under the other flag.

**API: right shape.**
- The wire bytes are the identity: `Eq`/`Hash` are byte equality. As a result `CoinbaseCommitment` and `AssetUnlock` regain `Eq + Hash`, which is a nice side effect.
- The scheme is supplied only where the value is read, so the A1 §1.4.1 flag question now lives at EV4/LQ1 call sites, not in decode.
- Serde is the hex of the wire bytes, and deserialize runs `decode`, so a malleable signature cannot enter through JSON.
- Nothing in base-sdk resolves a `BlsGlobal` under a hard-coded context (grep for `get(`/`BlsCtx::` in primitives `src`).

**Encoding of global fields is the wire bytes, not "Core's serialization-time scheme".** Core re-serializes under the flag at write time. So a signature canonical only in the other scheme would be written as zeros. I checked where Core re-serializes these payloads in v24:
- **CbTx:** only at mining (`miner.cpp:319`), with a fresh signature.
- **Asset unlock:** the signed message hash is built from a copy whose `quorumSig` is `CBLSSignature{}` (assetlocktx.cpp:222-229). The field's value never reaches a hash.
- The txid covers the raw `vExtraPayload`.

So this divergence is not observable. The `BlsGlobal` docs state it correctly ("Core serializes the object under the flag at that time; `from_eager` gives those bytes"). See N3 for one test doc that overclaims.

**`SptxDecode` removal: clean.**
- It is gone from `lib.rs`, `payload/mod.rs`, `cbtx.rs`, `assetunlock.rs` and `transaction.rs`, and the ten empty impls are deleted.
- The per-type `BaseCodec::decode` is the consensus decode for every type.
- `Transaction::check()` lost its "verdict does not depend on the scheme" comment, because that is now structural.
- rdc (conformance) uses `BlsGlobal::get(ORACLE_BLS)` in one place (`payload.rs:61`), with an accurate doc on `ORACLE_BLS`.

## 3. DEC-79 gate (bans, version guard, re-audit stamp, selftests)

**34 bans: complete for blst 0.3.17 and base-sdk 73ab4b0.** I enumerated them independently.
- **blst: 24 bans.** I mapped every `mt::da_pool()` call to its enclosing function:
  - `aggregate_verify` (lib.rs:1216), with `verify`, `fast_aggregate_verify` and `fast_aggregate_verify_pre_aggregated` above it;
  - `verify_multiple_aggregate_signatures` (:1337);
  - `blst_fp12::miller_loop_n` (:181);
  - `pippenger.rs`: `$points::from` (:82), and `MultiPoint for [affine]::{mult, add, validate}` (:141, :348, :414). These are reached by `p{1,2}_affines::{from, mult, add}` and by `Aggregate*::aggregate_with_randomness`, through `pks.validate()`/`pks.mult()`.

  That is 5 × 2 (`min_pk`, `min_sig`) + 2 × 2 + 1 + 6 + 3 = 24. All are in the list, and `MultiPoint::*` is banned at trait level.
- **dash-pkc: 10 bans.** The pooled calls are only in `scheme_ietf.rs`:
  - trait impls `verify` (:127), `fast_verify_aggregates` (:158) and `verify_aggregates` (:177);
  - `pub(crate)` inherents `verify_with` (:201) and `verify_possession` (:215).

  The public entry points reaching them are `BlsPublicKey::{verify, verify_with, verify_possession}`, `BlsSignature::{fast_verify_aggregates, secure_verify_aggregates, verify_aggregates}`, and `BlsScheme::{verify, fast_verify_aggregates, secure_verify_aggregates, verify_aggregates}`. The default `secure_verify_aggregates` calls `Self::verify`. All 10 are listed. No other base-sdk library at 73ab4b0 calls any of them (grep for `.verify(`/`verify_aggregates`/`verify_possession`/`verify_with` outside `pkgs/pkc`).
- **Not banned, correctly:** `pk_from_bytes` → single `PublicKey::validate` (serial), single-object `aggregate`, and the C FFI. The `BlsGlobal`/`BlsEager` decode path is serial.

**Version guard and stamp: correct and narrow.**
- `blst_audited` fails when a pure crate's normal graph (`--all-features`) links a blst other than the stamped one.
- The stamp check fails when `base_sdk_pins.py show`'s rev differs from the stamp, or when the stamp is missing. It runs even with an empty crate list, which is fine.
- `bump-base-sdk.sh` and `BASE-SDK-FORK.md` document the re-audit step.

**Selftests: all 13 pass, and each asserts the specific message.**
- `bans_resolve`: with `allow-invalid` stripped, no ban is "not a reachable function" for a crate using dash-pkc's BLS backend.
- A different blst release.
- A re-pin without a new stamp (fails), and with one (passes).
- A missing stamp.
- threadpool directly; threadpool through another crate.
- blst directly.
- A wrapper call; an aggregate wrapper; a `BlsScheme` trait call; a call through the `__deps::blst` re-export; a `MultiPoint` trait call.

**N1 (nit): the re-audit trigger is the pin, not the set of base-sdk crates a pure crate links.** The stamp procedure (`pure-clippy.toml:16-21`) audits "every base SDK library a pure crate links". Clippy does not lint dependency bodies. So if a pure crate starts linking another base-sdk library *at the same pin*, and that library calls a wrapper internally, the call escapes and no stamp change is forced. At 73ab4b0 there is no such library, so there is no hole today. **Fix:** word the stamp procedure as "every base-sdk library at the new revision" (it is one grep), or add the audited crate set to the stamp and compare it with the pure crates' base-sdk dependencies.

## 4. rdc commit order for the combined re-pin (DEC-72)

**Cherry-pickable, with mechanical conflicts plus one semantic one.** I applied each commit's diff (locks excluded) with `git apply --3way` onto `node/integration` (32dfcfe1 at the time; scratch `pick/`, nothing committed).
- `check-pure.sh`: one hunk. Integration's `5ce205c5` changed `lint` to take a per-crate conf dir. Keep `! blst_audited "$crate" ||` and integration's `lint "$crate" "$conf"`.
  - The stamp is read from `contrib/node/pure-clippy.toml` itself, so integration's TOML re-emit (which drops comments) does not lose it.
  - `allow-invalid = true` survives the merge.
- `selftest-checks.sh`: append/append beside integration's HashMap, own-`clippy.toml` and CS2 floating-point blocks. Keep both. The DEC-79 block uses only `pure`/`add_pure_member`, not the floating-point section's redefined `lint()`.
- `Cargo.toml` (the pin) and `conformance/Cargo.toml`: keep both `dash-consensus` and `dash-pkc`.
- **`conformance/tests/fixtures.rs:227` (semantic).** Integration's CS0 leg expects `consensus.merkle_root` and no `payload.decode`; the branch drops `payload.decode` differently. Re-derive the expected list by *running* the fixture test at R. Do not merge it by hand.

**Carry-over notes for the re-pin owner** (`/work/scratch/bs5/repin-plan.md`):
- **Include 43b8ebae.** The plan lists only the first five bs6a-adapt commits. The stamp commit must come along, and at R its stamp must name **R** after the re-audit.
  - BS5's base-sdk delta 481f8c3..`node/base-sdk` touches only `pkgs/params`. So the re-audit amounts to diffing 73ab4b0..R across `pkgs/pkc` and any library a pure crate links (N1).
  - The six "wanted pass" pin selftests go green only at R.
- **CS0 `Tx::payload(bls)`** (`dash-consensus/src/primitives/tx.rs:35, :137` on integration) still builds, because the arity matches. But its two per-scheme `OnceLock` slots are now redundant, and its doc ("base-sdk's payload decoder takes no scheme yet; BS6a makes it real without an API change") is false. The plan already proposes one slot with `bls` moved to the points of use. Check this against EV0's `BlsCtx` contract. When BS6c adds a layout input, key the slot by that layout (r1 F3).

## 5. Bench claim (−0.0007%)

**Holds at 73ab4b0.** I used a standalone bench of my own (scratch `bench/`): `instructions:u`, the 34-block mainnet corpus × 200, net of an `ITERS=0` run, 3 runs each, at load ≈45.

| | 481f8c3 | 73ab4b0 | Δ |
|---|---|---|---|
| Block decode (net, mean of 3) | 893,530,050 | 893,523,695 | −6,355 (**−0.00071%**) |
| Payload decode, 20 iterations (informational) | 1,645,592 | 1,631,372,861 | ×991 |

- Run-to-run spread is about 1,000 instructions.
- The implementer's `bench-r2-block.txt` gives −6,527 (−0.00073%), and their payload figure nets to the same 1.631G.
- The payload cost equals r1's: the scheme-free decode adds nothing for valid basic signatures, because the `||` short-circuits. It is Core-equivalent work (G2 decompression and subgroup checks), and Core does the same in `GetTxPayload`.

## New items

- **N1 (nit):** the re-audit trigger is the pin, not the linked-crate set (§3).
- **N2 (nit):** `BlsGlobal::get` (`wrap_global.rs:68`) does a full canonical decode on every call: G2 decompression and a subgroup check, about 1.2M instructions. Say so in its doc, so EV4/LQ1 keep the returned `BlsEager` instead of calling `get` twice.
- **N3 (nit):** the doc of `payloads_encode_as_core_serializes_them` (`pkgs/primitives/tests/payload_core.rs:655-658`) overclaims. It says the coinbase and asset-unlock signatures encode "as Core serializes them … whichever scheme they are canonical in". Core writes zeros for a signature canonical only in the other scheme than its flag. The test is right only for a flag under which the bytes are canonical, or for the transaction/wire image. Reword it, e.g. "encode as their wire bytes, which is what Core writes under a flag the signature is canonical in; Core never re-serializes these fields into a hash (§2)".
- **N4 (nit, manager ledger):** bring PLAN rows BS6c, BS12, BS14 and CV3 and BACKLOG BL-25 in line with DEC-71/79 (table F3, F5, F11).

## Summary

- Every r1 item is CLOSED: F1, F2, F4, F6, F7, F8 and F9 by code; F3, F5 and F11 by DEC-71/79 rulings, with ledger updates outstanding (N4). F10 stays withdrawn (PLAN §7 handling; see the manager).
- `BlsGlobal` matches Core's decode, resolve and write semantics, and it is checked against the full Core-generated wrapper corpus under both flags. Removing `SptxDecode` is complete and leaves a simpler, footgun-free decode API.
- The DEC-79 gate's 34 bans match an independent enumeration from blst 0.3.17 and dash-pkc at 73ab4b0. The version guard and stamp work, and all 13 selftests pass.
- The rdc branch builds at every commit and cherry-picks onto integration with mechanical conflicts plus one fixture list to re-derive. The re-pin plan must add 43b8ebae and restamp at R.
- The −0.0007% block-decode bench reproduces (−0.00071%).

**Verdict: APPROVE-WITH-NITS.**
