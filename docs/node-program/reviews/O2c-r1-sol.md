# O2c r1 — Sol review

Reviewed 2026-10-09. rust-dashcore `node/o2c-oracle-p2p-decode`,
`42f957e8..c2bebe5d` (six commits); Core v24.0
`e4113fa396e2444d0b820e790a8a22117f018cc3` and v23.1
`ffa0ef1505b5cc219d5caba369eb100ef62068e9`. Review contract:
`briefs/REVIEW-COMMON.md`. PLAN §7 handling: codenames only; private evidence was
limited to aggregate pass/fail and counts. Implementation branches were not changed.

## Findings

### F1 — major, merge-blocking: BL-61 turns an oracle failure into an agreement

**Locations:** Core `src/test/oracle/oracle.cpp:2466`; rust-dashcore
`node-l1/src/overlay.rs:321` and `node-l1/src/overlay.rs:387`.

The inner `catch (const std::exception&)` maps `std::bad_alloc` from either
deserialization **or write-back** to `ORACLE_E_DECODE`. `core()` treats that status
as a Core refusal; when the node also refuses the input, `compare_case()` returns
`BothReject` before considering strict flags. Allocation failure says nothing about
whether Core accepts these bytes. In particular, failure while serializing an object
that Core has already read cannot establish a decode refusal.

**Independently reproduced against the pinned v24 plain archive.** A scratch
replacement for C++ `operator new` fails one selected allocation once. A synthetic
`getqrinfo` containing no base hashes, a zero request hash, and `extraShare = 2`
normally returns Core success, consumes 34 bytes, writes the flag back as 1, and
compares as `StrictFlag`. Failing the input-stream allocation returns
`ORACLE_E_EXCEPTION`, correctly producing `Divergence("oracle error")`. Failing any
of the next three allocations, of sizes 1, 33 and 66 during write-back, returns
`ORACLE_E_DECODE` and changes the same comparison to **`BothReject`**:

```text
baseline: Ok(StrictFlag)
fail_at 1: Err(Divergence(oracle error))
fail_at 2: Ok(BothReject)
fail_at 3: Ok(BothReject)
fail_at 4: Ok(BothReject)
```

This proves a false agreement and loss of the strict-flag count. An unexpected
node rejection of a Core-readable input would be hidden by the same branch. With
a node-readable input, the failure would instead fabricate a decode divergence.
This is an oracle reliability defect, not a finding against Core's production code.

**Fix:** let `std::bad_alloc` escape to outer `Guarded()`, which already maps it to
`ORACLE_E_EXCEPTION`; preferably narrow the inner catch to genuine serialization
failures (`std::ios_base::failure`). Do not classify other infrastructure exceptions
as input refusals. Add an allocation-failure regression covering both read and
write-back and require `ErrorKind::Exception` / an oracle-error comparison, then
rebuild and re-pin the archives. BL-61 also records the analogous existing
`oracle_payload_decode` catch; apply the same exception policy there when paying
that debt, without changing genuine decode refusals.

**Disposition:** BL-61 should be paid before merging O2c. Deferring it to a later
re-pin leaves the new differential targets capable of recording agreement without
a Core verdict. Passing the ordinary corpus does not test this path.

### F2 — minor: document the qdata codec/handler boundary completely

**Locations:** Core `src/test/oracle/oracle.h:607` and
`src/test/oracle/oracle.h:617`; `src/test/oracle/oracle.cpp:2385`;
rust-dashcore `node-l1/src/overlay.rs:241`.

The ABI says the entry reads as the handler does, including checks before or during
the read. The implemented qdata model is narrower: it composes bounded codec reads
and reports counts. Core's handler additionally requires exact vector and
contribution counts (`src/llmq/net_quorum.cpp:250`, `:330`), stops after a wrong
verification-vector count, and gates contribution decoding on node/quorum/member
state (`src/llmq/net_quorum.cpp:310`). The shim reads both requested vectors without
those state gates. The header explicitly describes the verification-vector size
exception, but does not equally explain contribution undercounts or the changed
stopping point.

**Concrete example:** with the contributions mask, error NONE, a zero contribution
count, and valid-member count 1, the shim and node-l1 return `BothAccept` (verified
in the scratch probe). Core's handler, when its preceding state checks permit that
read, refuses the count. With both masks, an under-sized verification vector can
make Core stop before a malformed contribution body that the shim still attempts
to decode.

This matches BS8's judge and base-sdk's explicitly staged `read_data` contract;
it is not a new regression in that codec model. **Fix:** state clearly in the ABI
and Rust API docs that success means the modeled codec reads succeeded, and list
exact-count checks, conditional contribution reads, and subsequent handler
validation as outside that verdict. Preserve the separate counts. Full handler
parity needs a separately specified stateful oracle rather than interpreting this
entry's `Read`/`BothAccept` as peer-message acceptance. Nonblocking for O2c's
declared BS8 judge replacement.

## Shim fidelity and comparison soundness

All 14 dispatches use Core's concrete deserializers; no Rust decoder is used to
construct the Core verdict. Both streams use `SER_NETWORK` and the explicit peer
version. The fuzz header covers Core's pinned `MIN_PEER_PROTO_VERSION` 70221
through `PROTOCOL_VERSION` 70242, including layout boundaries 70225, 70227,
70228, 70229 and 70230. v23.1 returns `ORACLE_E_UNSUPPORTED`, and the overlay
targets advertise only v24.0.

| Command | Core object / modeled read | BLS and write-back |
| --- | --- | --- |
| `clsig` | `chainlock::ChainLockSig` | Eager signature under supplied global scheme |
| `isdlock` | `instantsend::InstantSendLock` | Lazy signature preserves wire bytes |
| `qsigrec` | `llmq::CRecoveredSig`, including its deserialize-time hash update | Lazy signature preserves wire bytes |
| `qfcommit` | `llmq::CFinalCommitment` | Eager key/signatures; versions 1/2 select legacy, 3/4 basic; indexed layout selected by object version |
| `mnauth` | `CMNAuth` | Eager signature under global scheme |
| `spork` | `CSporkMessage` | Signature byte vector, with Core's own bounds |
| `platformban` | `PlatformBanMessage` | Eager signature explicitly basic, independent of global scheme |
| `qgetdata` | `llmq::CQuorumDataRequest` after Core's pre-read length check | More than 67 bytes refused; short mandatory fields throw |
| `qdata` | Request plus mask-selected `UnserializeVectorWithMaxSize` reads | Eager verification and ephemeral keys; actual counts reported; F2 defines the boundary |
| `getqrinfo` | `llmq::CGetQuorumRotationInfo` | Core's bounded base-hash vector and bool serializer |
| `qrinfo` | `llmq::CQuorumRotationInfo` | Core's snapshots/diffs/commitments and optional extra cycle |
| `mnlistdiff` | `CSimplifiedMNListDiff` | Versioned layout; eager CL-signature map; lazy SML operator keys |
| `smlentry` | `CSimplifiedMNListEntry` | Versioned standalone object; lazy operator key |
| `qsnapshot` | `llmq::CQuorumSnapshot` | Core's bitset and skip-list serialization |

For ordinary reads the shim reports consumed bytes and leaves trailing bytes alone.
`qgetdata` is the intentional exception: its length check precedes any read and
catches an error byte or trailing garbage. qdata reads an available error byte;
absent error becomes UNDEFINED, explicit UNDEFINED is consumed but omitted on
write-back, and a non-NONE error leaves the body unread. Oversized qdata counts are
refused before decoding elements. These paths match the judge model.

`ProcessStateMutex()` and `ScopedBlsScheme` cover both read and write-back and
restore the previous process-wide flag on exception paths, using the same lock as
the other state-changing oracle calls.

`overlay.rs` compares verdict, consumed bytes, all qdata fields, and complete
write-back. Its eager BLS prediction uses base-sdk's Core-form classifier, including
zeros and opposite-scheme invalidation; lazy fields remain wire bytes. Commitment
schemes come from their versions. CL-signature map reconstruction canonicalizes
keys, preserves the first duplicate, uses Core's serialized-key double-SHA256
ordering, and keeps each decoded index set. Nested diff/rotation write-back follows
Core's layout and uses the node's `core_bytes` path for the coinbase transaction.
No additional defect was found in these comparisons.

StrictFlag is gated to exactly `smlentry`, `mnlistdiff`, `qrinfo`, and `getqrinfo`
(`overlay.rs:356`, `:391`). It requires a bool refusal, normalization of the offending
byte(s) to 1, successful node decoding, and equality of the entire resulting read
with Core. Thus a different verdict/consumed/qdata/write-back mismatch cannot be
waived as a strict flag. The prefix-search heuristic can fail to recognize an
exception, but the final complete comparison prevents it from hiding an additional
mismatch. F1 is the separate failure path before this gating.

## Validation and reproducibility

Reviewer Rust test executions used `nice -n 10`, `RUSTC_WRAPPER=`, and a mode-700
`TMPDIR` under `/dev/shm`. Private inputs were not loaded by these suites.

- Required command: `cargo test --locked -p node-l1 -p dash-oracle-sys --features oracle --test l1_overlay --test p2p -- --nocapture`.
  **8/8 tests passed:** 206 judge cases reproduced, 37 O4 fixtures round-tripped
  (including their one-byte-short rejection checks), and 4,200 mutations with zero
  divergences. The corpus had exactly 11 declared strict flags. Mutation totals:
  1,160 reads, 2,773 refusals, 267 strict flags.
- Additional oracle ABI smoke suite: **18/18 passed**, including ABI 7 and pin checks.
- `fuzzctl check`: PASS, 23 targets / 28 smoke legs / 28 farm legs.
  `fuzzctl selftest`: **50/50 passed**. `cargo fmt --all --check` and range
  `git diff --check`: PASS.
- Existing implementer aggregate smoke receipt: **28 passed / 0 failed**, zero
  divergences, crashes, sink failures or triage errors. Existing private-test
  aggregate: **3 receipt PASS lines**. These are receipt checks, not independent
  reruns of the private replay or fuzz smoke.
- The committed corpus is byte-identical to
  `base-sdk@d53e0927907919a0a29c679c73e79288ac68f393`'s
  `pkgs/p2p_core/corpus/core_overlay.json5`.
- Both Core branch heads equal `pins.txt`. Independently recomputed SHA256s of all
  four cached archives match both `pins.txt` and their manifests:

```text
v23.1 plain 240145c54bb34033f822a19ca923efeede21aa821f20ea106e0ae36fa2eff214
v23.1 fuzz  ece32ca57d9e8ba7458ebb03818f7a91dd6ddca660bb418a6347ecbc99c02833
v24.0 plain 6695a37886af55f6556bde2f1f3623cc5cd9d0210298621e5478f7ca0fe0cab4
v24.0 fuzz  0d7b52dd110de348c2391d3d0fc35aaa9d74fb1d9de422dcae5224825a50c28a
```

The manifest recipe hash also matches the reviewed `build-oracle.sh`
(`428e540f4eba350696f34d9cc8c393aa77d7060fb4a5a42747d3d0a6ec60b801`), and the
plain/fuzz toolchains agree with the pin-file documentation. Existing logs
`/work/scratch/o2c/logs/repin.log` and `repin-fuzz.log` record matching hashes from
two different clean roots per pin/flavor. Those roots have been removed; I did
not independently rebuild the archives, so the two-build reproducibility claim
is supported by those logs, while current archive identity was independently
verified.

Reviewer logs and synthetic fault-injection sources are under
`/work/scratch/o2c-r1-sol/`.

## Merge conditions and verdict

BL-55 remains a merge-order dependency: merge BS8 and re-pin base-sdk before
claiming the pins/selftest gate is green. This review does not waive that gate.
BL-62 is real but nonblocking here: `fuzzctl check` compares registered names,
not target pin sets; the current sets match and `Harness::init` refuses a
disallowed runtime pin. Keep the static pin-set check as tracked follow-up work.

O2c successfully replaces the BS8 judge for its declared codec model and the
normal comparison paths are sound. Its error classification is not yet sound:
the pinned ABI 7 archive demonstrably turns an allocation failure into a false
agreement. Pay F1/BL-61, re-pin, and rerun the differential suites before approval.

**Verdict: CHANGES-REQUIRED.**

**Sol gate satisfied: no**
