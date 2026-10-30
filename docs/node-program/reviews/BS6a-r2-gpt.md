# BS6a round 2 narrow closure check

**Verdict: APPROVE-WITH-NITS.** The blocking pure-crate finding is closed under DEC-79. Exact Core serialization equivalence remains a nonblocking, partially closed minor finding.

Reviewed on 2026-10-09: base-sdk `73ab4b00e246172b38d0be002a56d2275e0ec6b5` (18 commits over `481f8c3`), rust-dashcore `43b8ebae36d7e4561179309b2bbef15e85b0a9b9`. Fresh detached worktrees under `/work/scratch/bs6a-r2-gpt/`. Implementation sources remain unchanged. The report was written early and updated with the completed checks.

| Round 1 finding | Disposition | Result |
|---|---|---|
| Major: pure gate fails; SDK wrappers bypass direct blst bans | **CLOSED** | Actual gate passes; 34 bans resolve; independent generic/re-export/function-pointer attempts fail; version and re-pin guards work. |
| Minor 2: context-free decode silently chooses BASIC | **CLOSED** | `BlsGlobal` retains bytes without selecting a scheme; `get(ctx)` explicitly supplies the use scheme. Two-pin/two-flag reject and resolved-field sample matches. |
| Minor 3: encoding uses captured decode scheme | **PARTIAL** | Eager version change and explicit global write-context conversion match Core. Ordinary global encoding retains bytes, and lazy encoding resolves before writing, unlike Core's untouched lazy serializer. |
| Nit 4: CbTx asset-unlock root documentation | **CLOSED** | `cbtx.rs:46–47` now names instance hashes of v2+ asset unlocks, in block order. |

Unknown ProTx versions for payload types 1 and 2: out of scope (PLAN §7).

**Pure boundary and re-audit stamp.** DEC-79 supersedes DEC-71's std assertion: I did not require it, `no-threads`, or a serial verify implementation in BS6a. Serial pool-free verification remains BS12's follow-up.

`contrib/node/check-pure.sh` passes for the two current pure crates, including all-features and no-default-features linting. The dependency exceptions restrict threadpool to blst, and blst to dash-pkc. I inspected the hosted blst 0.3.17 pool callers and SDK wrapper/trait paths; no additional current SDK entry point reaching the pool was found.

Five independently written attempted escapes were rejected by the actual candidate Clippy configuration with `-F clippy::disallowed_methods`: a new generic function wrapping `BlsPublicKey<S>::verify`, generic `S: BlsScheme` verification, generic `T: MultiPoint` validation, a call through `dash_pkc::__deps::blst`, and capture of `BlsPublicKey::<BlsScIetf>::verify` as a function pointer. The pointer capture itself is diagnosed. No successful pool path was found in the audited API.

All **29 pure-boundary selftest cases pass**, including resolution of all **34** pooled-method bans, SDK verification and aggregation wrappers, scheme-trait verification, blst re-export verification and pooled trait validation. The blst version mismatch fails. A real re-pin to the candidate's parent with no stamp update fails; the same re-pin with the stamp updated passes; removing the stamp fails. This confirms the requested mechanical re-audit guard, not an automated proof that a person actually performed the audit.

The full selftest script exits 1 with **six failures in its broader pin-policy tests**. Independently, `check-base-sdk-pins.sh` reports that `73ab4b0` is not on `node/base-sdk` or a node-base-sdk tag. Those clean/re-pin pin-policy cases need the planned merge; the pure-boundary section has zero failures. I did not move branches or add tags to bypass that policy.

**Scheme-free decoding.** I selected **118 existing CbTx v3 / AssetUnlock v1 corpus cases**, including zero, legacy/basic canonical, exotic, identity, malformed and short inputs. There are 44 accepted and 74 rejected cases. Across v23.1.8 and v24.0.0-rc.3 under both flags (**472 comparisons**), the candidate has **zero reject-verdict differences and zero resolved-signature differences**. `get(ctx)` was compared with the eager object Core actually serialized after the read. Core probe headers were checked unchanged against the respective tags. `SptxDecode` and the implicit BASIC decode path are removed.

**Remaining minor: serializer equivalence.** The original ProUpRev reproduction is repaired: decode the legacy v1 signature, change its payload version to 2, encode; the result matches Core at both pins. For the two global fields, `BlsGlobal::from_eager(&field.get(read_ctx), write_ctx)` produces exactly Core's serialization after the flag changes. The caller must materialize the field this way; ordinary CbTx/AssetUnlock `BaseCodec::encode` writes retained bytes and accepts no write context. That is now an explicit wire-shape representation rather than a silent BASIC default, but it is not by itself a flag-aware Core object serializer.

The lazy case remains different. At `pkgs/primitives/src/payload/bls.rs:113–114`, `lazy_bytes` calls `field.clone().get().to_bytes(scheme)`. Using the existing **`proupregtx_v1_pk_legacy`** case: decode without using the operator key, set version 2, encode. At both pins, Core writes **48 zeros**; Rust writes the transcoded valid basic key. The first differing payload byte is **36**. Core's version wrapper supplies the new scheme before any `Get()`, and `CBLSLazyWrapper::Serialize` uses its still-default object on a scheme mismatch (`src/bls/bls.h:446–457`, both pins). The candidate's own test first calls `get()` on this key, which hides that distinction. An unchanged malformed lazy key likewise retains its original bytes in Core before first use, while the candidate normalizes it to zeros.

This normalization is documented and intentional, but DEC-71 does not explicitly exempt it from the serialization-time Core contract. To close the finding, use the existing `to_bytes_as` behavior for a Core-compatible serializer, or explicitly approve/name the normalized serializer separately. This remains **nonblocking at the original minor severity**: no live node acceptance/hash divergence was demonstrated, and transaction hashing uses raw extra payload. I cannot confirm unrestricted Core encoding equivalence.

**Validation and evidence.** The independent probe built and ran against the exact Git pin. Both focused source tests pass: `version_sourced_fields_encode_under_the_version_at_encoding` and `payloads_encode_as_core_serializes_them`. The latter's broad name does not invalidate the untouched-lazy counterexample above. Only selected test cases were executed. `TMPDIR=/dev/shm/bs6a-r2-gpt-tmp` is mode 700; Cargo commands used at most eight jobs.

Artifacts in `/work/scratch/bs6a-r2-gpt/`: `pure.log`, `selftests.log`, `pins.log`, `bypass.log` and `probe/src/lib.rs`; `sample.json`, `rust-sample.json`, `sample-summary.json`, `core-v23.txt`, `core-v24.txt`; `encode-input.txt`, `encode-summary-v23.json`, `encode-summary-v24.json`, the rebuilt two-pin C++ encoder probe and its source; `focused-tests.log`. Both implementation worktrees have clean tracked status. No commits, pushes, PR changes or GitHub posts were made.
