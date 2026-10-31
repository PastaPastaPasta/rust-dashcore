# Re-pin commit 16 (`1f3e6206`, CS0 `Tx::payload()`: one slot): Opus high pass

Reviewed 2026-10-09 as the sequential Opus-h pass after Sol's approval (DEC-92). The commit is merged on `node/integration` via `cb504fab`. Sources used:

- rust-dashcore `1f3e6206` and its parent;
- base-sdk `a1d4dd3`, which is the [patch] pin in `Cargo.lock`, read from `git archive` rather than the checkout on `develop`;
- Dash Core `v24.0.0-rc.3` (`da567d7d`) and `v23.1.8`, both read from `git archive` / `git show`.

## Verdict

**APPROVED. There is no consensus defect.** The change cannot alter any verdict, txid, sighash or encoding:

- The parent commit's two slots both called the same scheme-free `self.raw.decode_payload()`. The `bls` argument only picked a slot and never reached the decoder (`git show 1f3e6206~1:dash-consensus/src/primitives/tx.rs:139–149`).
- So the commit only drops a redundant second decode and the unused parameter.
- There are no production callers of `Tx::payload()`. The only calls are in the new test, which matches Sol's finding.

One Low finding (L1) is about test coverage. One note (N1) is about how far the doc's scheme-independence claim reaches.

## 1. Is a decode with no context scheme-independent for every payload type, as Core's verdict is?

**Yes, at both pins.** There are three ways a BLS field can depend on a scheme. I checked each one against Core.

**(a) Core's eager decode verdict is symmetric in the scheme.** In `CBLSWrapper::Unserialize(s, S)` (`bls/bls.h:184`, the same in v23.1.8), the steps are:
1. Decode the bytes under `S` and run `CheckMalleable`.
2. If that fails, retry under `!S`.
3. Throw only if both fail.

So the bytes are accepted iff they are zeros, canonical under `S`, or canonical under `!S`. That set is the same whichever scheme comes first. Only the resulting *value* depends on `S`: it is the point under `S`, or `Reset()` (invalid).

base-sdk mirrors this:
- `BlsGlobal::decode` accepts `is_zero || canonical(Basic) || canonical(Legacy)` and rejects the identity encoding, as Core does through `impl == ImplType()` → `Reset()` → zeros ≠ input.
- `BlsEager::decode(bytes, source)` returns the point under `source`, or invalid when the bytes are canonical only in the opposite scheme, or an error.

**(b) Which scheme each field uses, from every `CBLS*` member of a payload type at both pins:**

| Type | Field | Core | base-sdk at `a1d4dd3` | Depends on the flag? |
|---|---|---|---|---|
| 1 ProRegTx | `pubKeyOperator` | `CBLSLazyPublicKeyVersionWrapper(…, nVersion == LegacyBLS)` | `read_lazy(protx_source(version))` | No. A lazy read only stores bytes and can fail only on a short read. |
| 2 ProUpServTx | `sig` | `CBLSSignatureVersionWrapper(…, nVersion == LegacyBLS)` | `read_eager(protx_source(version))` | No |
| 3 ProUpRegTx | `pubKeyOperator` | lazy version wrapper, as type 1 | `read_lazy(protx_source(version))` | No |
| 4 ProUpRevTx | `sig` | eager version wrapper, as type 2 | `read_eager(protx_source(version))` | No |
| 5 CbTx | `bestCLSignature` | plain `READWRITE`, so the **global flag** | `read_global` → `BlsGlobalSig` | Verdict: no, by (a). Value: resolved at use. |
| 6 QC | `quorumPublicKey`, `quorumSig`, `membersSig` | version wrappers, legacy iff `nVersion ∈ {1,2}` | `read_eager(qc_source(version))` | No |
| 7 MnHf | `sig` | `CBLSSignatureVersionWrapper(…, /*legacy=*/false)` | `AlwaysBasic` | No |
| 9 AssetUnlock | `quorumSig` | plain `READWRITE`, so the **global flag** | `read_global` → `BlsGlobalSig` | Verdict: no, by (a). Value: resolved at use. |
| 12 ProUpSharedRegTx | `pubKeyOperator` | `CBLSLazyPublicKeyVersionWrapper(…, /*legacy=*/false)` (v24 `evo/providertx.h:534`) | `read_lazy(AlwaysBasic)` | No |

Types 8, 10 and 11 have no BLS member. The signatures in types 11 and 12 are `CompactSignature`, which is ECDSA. v23.1.8 has the same member set without types 10–12, and the same wrappers.

**(c) The ProTx unknown-version early return.** All four ProTx serializers bail out when `nVersion == 0 || nVersion > ProTxVersion::GetMax(/*is_basic_scheme_active=*/true, /*is_extended_addr=*/true)`. The arguments are hard-coded literals at both pins, not the global flag. The decoded shape of a ProTx therefore doesn't depend on the scheme either.

The commit's doc comment says the fields read under the global flag are the CbTx chainlock signature and the asset-unlock quorum signature. That is exactly the set above.

## 2. Could one shared decode give a wrong verdict or a wrong sighash/txid across callers with different `BlsCtx`?

**No.**

- **No state carries over between callers.**
  - `BlsGlobal` holds only `[u8; N]`, and `get(&self, ctx)` is a pure function of the bytes and `ctx`.
  - `BlsLazy::get(&self)` is pure. Its mutators (`set`, `set_scheme`, `serialize`) take `&mut self`, which can't be reached through `&PayloadDecode`.
  - `lazy_bytes` resolves a clone.
  - So the cached `PayloadDecode` can't capture the first caller's scheme. Neither can Core's `bufLegacyScheme` / `objInitialized` lazy quirk, since base-sdk doesn't model it as shared mutable state.
- **txid and sighash use raw bytes.** Core's `CTransactionSignatureSerializer` writes `::Serialize(s, txTo.vExtraPayload)`, the raw vector (`script/interpreter.cpp:1438`). The transaction serialization writes `vExtraPayload` raw too (`primitives/transaction.h:277`). CS0 mirrors both:
  - `canonical::write_extra_payload` writes `tx.extra_payload` bytes.
  - `instance_hash` and `stable_unlock_hash` hash `core_serialize`.
  - Nothing in `dash-consensus/src` re-encodes a `SpecialPayload`. A grep for `decode_payload`, `SpecialPayload`, `PayloadDecode` and `BlsGlobal` finds only `tx.rs`.
- **Re-encoding is a pure function of the decode, which doesn't depend on the scheme.**
  - base-sdk encodes a `BlsGlobal` as its wire bytes. Core would write `ToBytes(S)` of the object it read under `S`. That gives zeros for bytes canonical only under `!S`, so the two differ.
  - This is a pre-existing base-sdk/DEC-71 contract ("Core may not read back", `wrap_global.rs`). This commit doesn't introduce or change it. No CS0 path consumes the re-encoding.
  - Future EV consumers that hash a re-encoded CbTx or asset-unlock payload must use `BlsGlobal::from_eager(get(ctx), ctx)`, not `to_bytes()`. That applies with or without this commit.
- **Concurrency.** `get` → decode → `get_or_init` can decode twice when two threads race. The decode is deterministic and the first value wins, so this is harmless.

## 3. Is the "decode once" test meaningful?

**Only partly; see L1.**

## Findings

### Low L1: `the_payload_is_decoded_once` caches only an error and touches no scheme

`dash-consensus/src/primitives/tx.rs:237–250`.

- `special()` carries `extra_payload = [1, 2, 3]`. CbTx's decoder reads `nVersion` (2 bytes) and then a 4-byte `nHeight`, so the first call returns `Err(PayloadError)`.
- The pointer-equality check therefore shows that a cached **error** is reused. It doesn't show that a successful decode is reused.
- The test doesn't assert which case it hit. A future change to the fixture or decoder that made the decode succeed would pass silently, and so would one that made it fail.
- Its doc says "whatever the caller's BLS scheme", but no scheme appears in the test. The API no longer takes one, so the test can't exercise that claim.

What it does check is valid: the value is decoded once (the same address each call) and a Spend returns `None`.

Suggested fix:
- Assert `first` is the `Err` arm, or rename the test to say it caches the error.
- Add Sol's Core-recorded probe (`/work/scratch/repin-sol/logs/repin_review_probe.rs`) as a test. It decodes CbTx and asset-unlock payloads with basic and with legacy signatures and resolves each one basic/legacy/basic/legacy from the same cached `PayloadDecode` against Core's bytes. That is the test that covers the DEC-71 claim.

Non-blocking; same as Sol's S1.

### Note N1: the claim covers only the BLS scheme, not the release layout

- The doc now reads "decoded at most once, as nothing in it depends on the BLS scheme". That is true.
- The decode does depend on the **release's** payload layout (the v23.1.8 vs v24 CbTx/AU/ProTx versions and types 10–12). The single slot is not keyed by release.
- The parent's two slots weren't keyed by release either, and the commit message defers layout keying to BS6c. So nothing regresses.
- The `Tx` struct doc says "No field depends on the release". If BS6c makes `decode_payload` take a release, that doc line and this slot have to change together.

Recorded so BS6c doesn't miss it. No action is needed in this commit.

## Validation

- `TMPDIR=/dev/shm/c16-opus/tmp cargo test -p dash-consensus` in `~/workspace/rdc-wt/integration`:
  - HEAD was `759142ca`. Its `dash-consensus/` is identical to `1f3e6206` apart from `tests/data/PROVENANCE.md` (+8 lines, docs), and `Cargo.lock` has no package changes.
  - Result: **exit 0. 28 test binaries (doc tests included): 225 passed, 0 failed, 0 ignored.** That includes `primitives::tx::tests::the_payload_is_decoded_once ... ok`. The full log is in `/dev/shm/c16-opus/test.log`.
  - The run waited behind other threads' builds on the shared target lock. One attempt was stopped at the harness's 30-minute background limit before it finished compiling, and the re-run completed. A detached-worktree run at the exact commit was started as a fallback, then stopped and removed (`git worktree remove`) once the shared run passed.
- I didn't re-run Sol's probe. Its log (`/work/scratch/repin-sol/logs/payload-probe.log`) and source were inspected. The Core-recorded expectations show the basic signature resolving to zeros under legacy and the legacy signature to zeros under basic, which matches Core's `Reset()` on an opposite-scheme read.
