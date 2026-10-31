# BS6b — Sol review, round 1

2026-10-09. Tier: `rev Sol→O-h`; this is the first gate. PLAN §7 handling: codenames only.

Reviewed base-sdk `4b4e76b..738c1c6` (4 commits) in `bsdk-wt/bs6b` and rust-dashcore
`1dbaf70f..0fd303b0` (8 commits) in `rdc-wt/bs6b`. Compared the implementation with
Dash Core tag `v24.0.0-rc.3` (`da567d7dba945143eee4d2ce1621c21b32089989`), not the
current Dash worktree's HEAD. Executed differential checks against the pinned v24 oracle
`a9a483e51efdadeee37cf40ff8697da62b3cd012`, with archive pinning enforced.

The supported signed shapes and stable asset-unlock txid passed independent verification.
The changes are not ready to satisfy BS6b's acceptance gate: the committed seeds omit
distinct BLS outcomes, and the required campaign has not completed. There is also a
minor unsupported-version mismatch in the new public signing API.

## Findings

### R1-B1 — blocker: the required differential campaign is outstanding

**Location:** `docs/node-program/PLAN.md:517`; rust-dashcore `node-fuzz/targets.toml:86`.

BS6b requires at least 24 clean CPU-hours **per** `diff_payload_1..12` target. The task
report and STATUS identify this as queued work, not completed evidence. The successful
smoke and proptest runs cannot establish this acceptance criterion. A merge now would
declare a consensus codec gate passed before its required differential exploration.

**Fix:** run the farm campaign with the final corrected seed set and reviewed revisions;
record a receipt for each of the 12 v24 legs, its measured CPU-hours, revisions, and zero
unresolved divergences. A pending farm job is acceptable scheduling, but is not an
acceptance waiver. Do not mark BS6b complete or merge on the present evidence.

### R1-M1 — major: one sample per label does not seed every BLS outcome class

**Location:** rust-dashcore `node-l1/seeds/gen.py:88`, especially lines 91–93;
`node-l1/seeds/PROVENANCE.md:25`.

`samples()` deduplicates solely on `entry["class"]`. That label describes the input
construction, and does not uniquely identify Core's outcomes. Eight G1 labels and
17 G2 labels have multiple `(legacy outcome, basic outcome)` pairs in the source corpus.
Keeping their first entry silently discards the other outcomes.

For example, `sub_basic_s0` has both `(INVALID, VALID)` and `(VALID, VALID)` samples.
The generator keeps the former. The source corpus's sole dual-valid G2 sample is absent
from every committed signature-bearing target (types 2, 4, 5, 6, 7 and 9), including both
global-flag legs of CbTx and asset unlock. This misses the explicit A1 §1.4.1 case where a
canonical basic signature is also a valid, different legacy point. A decoder regression
specific to that case can evade the promised field-by-field seeding. Baseline signatures
are not a checked substitute for injecting that class into each field.

I regenerated the existing corpus exactly: **1,786**, rather than the claimed 1,784,
binary seeds. A scratch generator retaining every distinct source input produced
**4,768** seeds; all 4,768 passed the actual L1 comparison. This is a coverage defect,
not evidence that those omitted inputs currently decode incorrectly.

**Fix:** retain all distinct source samples, or deduplicate using a key that preserves
each input class and its outcome pair. Regenerate the committed seeds before the farm
campaign. Add a coverage assertion for every type/version/BLS-field/scheme combination;
the current replay only asserts that each target has at least one accepted seed, so it
cannot detect missing classes or missing individual files. Fail seed generation when an
expected field cannot be located instead of silently returning an empty field list.

### R1-m1 — minor: see PLAN §7 handling

Moved to the private registry. Resolved by DEC-141.

### R1-m2 — minor: the supported CbTx v1 layout has no committed seed

**Location:** rust-dashcore `node-l1/seeds/gen.py:105` and
`node-l1/seeds/diff_payload_5/`.

The generator uses only shapes present in its two fixture inputs. The committed type-5
corpus contains versions 2, 3 and 4, but no version 1, although Core and base-sdk decode
the distinct v1 layout. Thus the types × versions claim is not fully represented by the
seeds. Version mutation may reach it later, but does not supply an asserted seed for that
layout. Types 5 and 8 have no payload sighash; that does not exempt their decode targets.

**Fix:** add a provenance-backed Core-made CbTx v1 seed and assert the expected supported
version matrix alongside the per-field coverage check.

## Correctness and differential independence

- **Provider types 1–4:** supported-version field order, version-selected BLS serialization,
  signature omission, payout-list text, address prefixes and signed-message framing match
  Core's provider serializers, `MakeSignString`, `PayoutListToString`, and
  `CMessageSigner::VerifyMessage`. Shared registration's consent preimage matches
  `CProRegTx::MakeSharedRegConsentHash`, including prevout/output hashes, sequences,
  registrar configuration, share count/table and penalty terms.
- **Type 6:** the digest is Core's `BuildCommitmentHash`, not the full commitment payload.
  Its explicit scheme argument correctly models the public key's serialization under
  Core's process-wide flag; commitment version alone is not used to choose that encoding.
  The quorum and member verification sites use that same digest. The corpus verifies
  the quorum signature, not the aggregate members signature independently.
- **Types 7 and 9:** the signature is zeroed in a copy of the complete transaction;
  request IDs serialize the correct prefix and integer width; `llmq_sign_hash` matches
  Core's type/quorum/id/message serialization. AU v2 correctly signs the complete
  unsigned instance serialization, retaining requested height and quorum hash, rather
  than its stable txid (`src/evo/assetlocktx.cpp:222`).
- **Types 10–12:** dissolution's domain string, inputs, sequences, outputs, actor and
  signature count match `MakeSignHash`; share and shared-registrar updates omit exactly
  the signature fields/count that Core excludes under `SER_GETHASH`.
- **Stable txid:** the version/type/145-byte-payload predicate and zeroing of the final
  132 bytes match `IsAssetUnlockWithStableTxid` and `ComputeStableUnlockHash`.
  `instance_hash()` retains the complete serialization. No activation gate belongs in
  this hashing predicate. The 0fd303b0 snapshot changes remove the old txid substitution
  and assert real equality; the independently run conformance tests passed.
- **Real differential:** `compare_payload` calls Core's own deserializer/serializer through
  the oracle, compares decode verdicts and full-consumption status, then compares bytes.
  The adapter does not obtain expected bytes from the oracle to manufacture its answer.
  Its raw lazy-key and global-signature patches correspond to the deliberate differences
  between base-sdk's representation and a fresh Core object. Valid resolving lazy keys
  remain compared through base-sdk's encoder. In a scratch copy, disabling only `patch()`
  caused **315 serialization divergences** on the 1,786 committed seeds; the normal
  comparison passed. The comparison is not self-certifying, but its adapters and seed
  coverage should remain explicit review obligations.

## Deviations and upstream readiness

**D1:** accept Core-signature verification as independent evidence for the supported
signed shapes. There are 237 transaction fixtures covering all **22** supported signed
type/version pairs: types 1–4 at v1–3, commitment versions 1–4, MnHF v1, AU v1/v2,
and types 10–12 at v1. There is no missing currently supported signed version in this
matrix. The test also exercises shared-registration consent separately from external
collateral's text signature. Changing the scratch test's computed hash by one bit caused
the Core-signature test to fail, as expected.

This evidence does **not** prove every decodable or publicly constructible version or
every malformed BLS field. R1-m1 is a concrete counterexample to such a broader claim.
The original oracle-sighash-vector criterion remains a follow-up for O2a-EV; it must not
be represented as completed direct hash comparison. When that entry point exists, add
direct vectors for boundaries, explicit BLS contexts, and the currently unsigned cases.

**D2:** farm execution may follow implementation review, but the 24 CPU-hour acceptance
requirement remains binding. This review does not grant a waiver or a merge approval
before its receipts exist.

The base-sdk changes are appropriately contained in primitives, retain `no_std` builds,
make BLS context explicit where needed, and keep generic message helpers crate-private.
The new public APIs are otherwise coherent with the payload types. R1-m1 should be
addressed before upstream presentation. The provisional pin is intentional and is a
manager merge obligation; the expected pins/selftest failures are not additional code
findings. No GitHub action is part of this review.

## Independent validation

All Rust/test invocations used `TMPDIR=/dev/shm/bs6b-r1-sol-tmp` (verified mode 700),
`RUSTC_WRAPPER=`, and an unset `CARGO_TARGET_DIR`. Base-sdk additionally used
`EIO_BACKEND=posix`. The two worktrees used their own `target/bs6b-r1-sol` directories;
scratch experiments and logs are under `/work/scratch/bs6b-r1-sol/`.

- base-sdk: `cargo nextest run --config-file maint/nextest.toml --profile=ci --workspace
  --features full --target-dir target/bs6b-r1-sol`: **1,352 passed, 1 skipped**, including
  the 237-transaction signature corpus and four stable-txid vectors.
- base-sdk: `cargo build --workspace --no-default-features` and
  `cargo clippy --all-targets --features full`, with the same explicit target directory:
  **passed**. Existing dash-pow warnings remain; this was not a `-D warnings` clippy run.
- rust-dashcore: `cargo test -p node-l1 --features oracle --no-fail-fast`, with the explicit
  target directory: **29 passed**. The focused `l1_payload` run independently passed
  **3/3**, including the 1,024-case property and four AU fixtures × eight boundary edits.
  The real private corpus was not supplied to these runs; they are not private-test receipts.
- rust-dashcore: `cargo test -p dash-conformance --lib --test txid_rule --test v24_findings`:
  **50 passed**. `cargo clippy -p node-l1 --features oracle --all-targets -- -D warnings`:
  **passed**. Both used the explicit review target directory.
- Both repositories: `cargo fmt --all --check` and commit-range `git diff --check`:
  **passed**. `python3 -I node-fuzz/fuzzctl.py check`: **passed**, 21 targets and
  26 registered smoke/farm legs. I did not rerun the actual smoke campaign.
- Scratch controls: exact seed regeneration; 4,768-sample expanded replay (**0 divergences**);
  disabled BLS patches (**315 divergences**); one-bit tampered sighash (**expected failure**);
  the R1-m1 control (private registry).
- base-sdk lint: initially missing tooling was supplied in an isolated scratch environment.
  Cargo, commit, JavaScript, Python, Rust, symlink and Semgrep checks **passed**; Semgrep
  reported 0 findings. Markdown lint **failed** with the current allowed pymarkdownlnt,
  including pre-existing MD049/MD060 diagnostics and MD060 on the new
  `contrib/vectors/sighash/README.md:8` table. Nix formatting was unavailable. This does
  not reproduce the claimed entirely green lint run; pin/reconcile the lint environment
  and table formatting before upstreaming. CodeQL and the complete node/private gate
  were not independently rerun.

The reviewed branches were not modified, committed or pushed. This review file is the
requested uncommitted output. No marked private module or private security document was
read, and no reference dashd instance was started, stopped or changed.

**Sol gate satisfied: no.** Repair the seed coverage and supply the required campaign
evidence before treating the gate as satisfied; address the minor API/seed findings too.

**Verdict: CHANGES-REQUIRED.**
