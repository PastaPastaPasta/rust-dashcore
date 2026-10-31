# EV2a r1 — Sol review

Reviewed `node/ev2a-dml` at `44a23b04e2196690542bd2512f928f4b3f6f5975`, 14 commits over integration `76ae54e61`. Scope: PLAN EV2a; EV2 rev4 including the public BL-67 erratum; EV addendum FINAL rev6 §4.1; SB0 rev6.1; DEC-109/117/122; EV0's merged interface and the two requested memories. I inspected the commit-range changes and implementation, compared against Dash Core `v24.0.0-rc.3`, and applied the code-review-validator workflow. Private erratum material is outside this review; no `~/private/` material was read.

The reviewed branch was left clean and unchanged. Tests used its own target with `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and disk-backed `TMPDIR=/work/scratch/ev2a-r1-sol/tmp` (0700). The report is uncommitted.

## Findings

### R1-M2 — major: snapshot equality does not preserve later record reconstruction across a version reset

`dash-dml/src/codec.rs:176–206,954–965`; `dash-dml/src/diff.rs:220–244,318–337`; `dash-dml/src/record.rs:71–103`. The snapshot envelope stores only the version-projected canonical state. D18 then considers a version-3 state with a nonempty dormant `script_payout` equal to its decoded snapshot, which holds an empty script. If the online state later calls the public `reset_operator_fields` and is installed with `update`, its version becomes 1 but that script is retained. `MnListDiffRecord::build` does not set `SCRIPT_PAYOUT`, because the online before/after script is unchanged. Applying that record to the decoded snapshot leaves the rebuilt script empty. Both paths succeed, but their canonical hashes and `SameRecord` results differ.

A small, deterministic Rust-only check in `/work/scratch/ev2a-r1-sol/rebuild_check.rs` uses one regular MN, an empty known extended network-info object, a null operator key, a one-byte dormant payout, and the public reset/update APIs. Both snapshot and diff records pass through their encoders and decoders successfully. It reports `snapshot_same_record=true`, `payout_bit=false`, then `after_reset_same_record=false canonical_hash_equal=false`; the output is in `rebuild-check.log` beside the source. No operator-key scheme mismatch is involved. This establishes failure of the advertised unrestricted own-record/rebuild contract. Valid-chain reachability is not established by this check, and no conclusion about Core's behavior follows from it.

Fix: retain future-visible omitted fields in the own-record envelope, or supplement stored transitions so record application restores every field newly exposed by a version change. If EV2b instead owns a narrower reconstruction-safe input invariant, make it explicit and prove it at that boundary; the current unrestricted rebuild claim and D17 alone do not establish it. Add the version-3-to-reset sequence through encoded snapshots and encoded diff records to the rebuild suite.

### R1-M1 — major: nested budgets are charged after decoding and allocation

`dash-dml/src/codec.rs:464,491–509` (`Reader::net_info`, `Reader::item`). The canonical reader delegates nested objects to unbudgeted base-sdk decoders, then charges the resulting object's lengths. Core's `SnapshotBoundedInput` charges each consumed CompactSize before the corresponding allocation/read, including through the network-entry wrapper (`snapshot.h:171–225`). A collection or payout/share script exceeding the remaining 10,000-item entry budget can therefore be allocated and traversed before Rust refuses it. Charging the normalized result also loses counts from duplicate purpose groups or normalized invalid entries. The outer MN/operation counts do not provide this per-entry bound.

The pinned base-sdk source confirms the path: `pkgs/primitives/src/types/netinfo.rs:421` decodes groups before collapsing repeated purposes, and `pkgs/types/src/codec.rs:304` allocates vector batches without the snapshot-local budget. Its remaining-input bounds help, but do not implement Core's smaller per-entry limit or its early rejection.

Fix: decode these nested fields through the budgeted reader (or a decoder adapter that charges every consumed CompactSize before allocation), preserving Core's handling of network-entry read failures. Add small bounded regression tests for early budget exhaustion, including payout/share scripts and network groups. This finding is based on the code paths, without constructing a resource-exhaustion payload.

### R1-m1 — minor: the D16 importer comment falsely says the hash decodes keys

`dash-dml/src/codec.rs:1048`. `canonical_mn_list_hash` calls the unvalidated writer; `MnCold::operator_key_bytes` serializes a clone under the version's scheme and does not call `get()`. For an imported key captured under that same scheme, hashing retains its lazy state. A comparison with a trusted expected hash can still be an import integrity check, but computing the hash is not key materialization or canonical-reader validation. Fix the D16 contract to distinguish these operations and specify the required validation before use.

### R1-m2 — minor: the unique-probe row does not supply the required table comparison

`dash-dml/benches/mnlist.rs:73`; EV2 §5.5 and DEC-50(3). This row measures 1,000 allocations plus 1,000 probes: it constructs a boxed collateral pre-image inside each timed call. The exact-head PF0 artifact confirms 200,000 allocations and 7,200,000 allocated bytes over 200 iterations, or 1,000 allocations / 36,000 bytes per iteration. That is a useful measurement of the collateral lookup path, but does not isolate the map or compare SipHash, Fx and `OrdMap`, and it has no add/remove or adversarial-fixture leg. Those one-time comparisons were explicitly required by the design and decision; the four added rows do not establish their budgets.

Fix: keep the allocation-inclusive lookup row, add a precomputed-key probe row, and record the specified normal/adversarial and add/remove table comparisons. Do not use this artifact as evidence that SipHash meets the Fx/OrdMap selection budgets or that these operations are faster than Core.

### R1-m3 — minor: the reconstruction test overstates its snapshot/restart/reorg coverage

`dash-dml/tests/reconstruct.rs:109`; `dash-dml/API.md:70`. The suite is meaningful: records actually pass through both codecs, and comparisons include stored content, `IsEqual`, operator slots, counters, ids, counts and Core order. But it resolves only from the nearest snapshot available in one store, plus cached results; it does not force each earlier snapshot as an alternative base, rebuild a fresh store after restart, or disconnect/reconnect and register again. The test's “from any starting snapshot” claim and EV2 §6.2's broader restart/reorg coverage therefore exceed what is exercised. It also has no version-3 snapshot transition, which is why R1-M2 is missed.

Fix: add a reader wrapper selecting each eligible earlier snapshot, reconstruct a new store/cache from encoded records, and test disconnect/reconnect followed by registration. Assert byte/hash equality as well as the existing projections, and update the claim to the tested domain.

## Core comparison and contracts

The local Core checkout is not the requested tag. Comparisons used exact tag blobs at `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`, extracted to `/work/scratch/ev2a-r1-sol/core/`, for `deterministicmns.*`, `dmnstate.*`, and `snapshot.*`.

- **State and list diffs:** the field-mask values, tuple serialization order, VERSION coupling to operator-key/network-info fields, version-dependent fields, and literal masked `ApplyToState` behavior align with the tag outside the publicly specified BL-67 change. BuildDiff sorts additions by internal id, and ApplyDiff performs removals, additions, then updates with Core's predecessor-counter bump rule. The stored record restores the counter after hidden registrations and captures both changed collision buckets and touched buckets; order-only changes and dissolved buckets are represented. I found no additional defect in those mechanisms.
- **Ordering:** the implementation reconstructs CHAMP order over `ReadLE64(proTxHash)`, including the most-recently-set-first collision sequence. Operator first-match lookup and Core disk serialization use that order. Canonical list serialization correctly uses full-hash memcmp order, and canonical diff sections use their specified id orders. The payee queue uses Core's effective payment height followed by memcmp `proTxHash`; its structure is present, while payee selection and continuations belong to EV2b.
- **Unique properties:** the single serialization-preimage namespace, null rules, refcounts, add failure order, update/remove slot handling, and rollback behavior have substantive tests. Quirk 23 is registered and runs. Core-judged key preimages and the collision/history L1 checks support parity. The selected SipHash map is hidden behind a non-iterable API, so its randomized iteration cannot affect consensus output.
- **Views:** `list/tests.rs::view_contract` exercises unweighted total/enabled counts, absent-entry ban behavior, exactly-once all/valid visitation, empty scratch creation, duplicate and owner-clash rejection, and retained banned-entry state. External-crate entry tests cover `Arc<Dmn>` forwarding. These test the LQ1 §2.2 contract, rather than only trait compilation. The view's memcmp iteration is permitted: LQ1 explicitly does not require CHAMP order and sorts its selection inputs. The full `try_add` suite supplies the remaining failure-rule coverage.
- **Purity and layering:** the dependency graph keeps `dash-dml` and `imbl` out of shared `dash-evo`'s normal closure, including all features, all targets, and declared optional edges. Scoped bans and purity clippy passed in all-feature and no-default-feature library builds. No filesystem/network I/O or thread creation was introduced in the production DML path. The `imbl` version is pinned. `EvoStore`, the in-memory rewrite checks, and `resolve_list` retain EV0's store-error and ancestry boundary; durable storage remains a consumer's responsibility.

## Deviation judgments

The numbered references D12/D16/D17 appear in code or API comments; a complete EV2a D1–D19 register was not located. `dash-evo/API.md` §5 is the inherited EV0 register and uses different numbers. These judgments therefore address the six EV2a deviations described in the review brief and their observable implementation.

| Deviation | Judgment |
|---|---|
| **D12** | Acceptable temporary boundary: production requires a caller-supplied `NetInfoValidity`, and `ContextFreeNetInfo` is behind `test-utils`. EV1 must provide the network rules. The four deferred verdicts are not evidence of production network-info parity. |
| **D15** | The separate L1 branch is reasonable while the oracle re-pin is pending, and its relevant production DML code matches the reviewed head. This is a dependency exception, not completion of a reproducible pinned gate. Land/re-pin the tests and oracle before consensus merge. The development run does not replace the design's differential campaign. |
| **D16** | Retaining lazy raw keys in the evodb reader matches the intended import shape. The stated validation-before-use contract is inaccurate because hashing does not necessarily decode them; fix R1-m1. A trusted expected-hash check and materialization/reader validation are different operations. |
| **D17** | The documented captured-scheme precondition is a legitimate EV2b boundary obligation and should be enforced/tested there. Default keys intentionally permit a scheme difference. It does not cover the independent dormant-field reconstruction failure in R1-M2. Private BL-67 mechanics were excluded. |
| **D18** | Version-projected equality is a sound comparison of the current serialized state. It is insufficient to imply equivalence under future transitions; the own-record claim needs the R1-M2 fix or a proven narrower domain. |
| **D19** | Accept the per-entry validation scope. `encode_canonical_list` performs the tag's list-invariant checks, then serializes and reads back each MN's bytes in Core order. That matches `ValidateCanonicalMNInvariants` plus `ValidateMNEncoding` at `snapshot.cpp:93–122,228–229`; Core's validation scope does not justify the former blanket whole-list-readback promise. This judgment is subject to the nested-reader discrepancy in R1-M1. The own-record future-rebuild contract is separately broken by R1-M2. The fuzz apply mode's acceptance of broad canonical-refusal prefixes is a model allowance, not an oracle verdict comparison. |

## Validation and evidence limits

Independent checks, with logs under `/work/scratch/ev2a-r1-sol/`:

- `cargo test -p dash-dml --all-features`: **72 unit/integration tests and 2 doctests passed**. I then ran the ignored full-size import test separately: **1 passed**, checking **20 imported lists** against the committed Core bytes/hash evidence.
- `cargo clippy -p dash-dml --all-features --all-targets -- -D warnings`, workspace formatting check, and commit-range `git diff --check`: passed. Scoped dependency bans, layering, and purity clippy in both feature modes: passed.
- In `~/workspace/rdc-wt/ev2a-l1` at `44abb56bb`, `cargo test -p node-l1 --features oracle --test l1_dml`: **5 passed**, followed by the ignored full-size test: **1 passed**, again over **20 lists**. Environment included `DASH_ORACLE_ALLOW_UNPINNED=1` and `DASH_ORACLE_CACHE=/work/scratch/o2a-ev/oracle-cache/dev`; the linked development archive SHA256 was `80e010ab5cb8275d48611bc4f1e1ecc4f76eabc6b525e9c079f381c55ed93414`. The relevant production source differs from the reviewed branch only in codec comments; quirk-test placement also differs.
- The deterministic R1-M2 check compiled against the reviewed worktree's built dependencies and confirmed a later canonical-hash mismatch after successful own-record encoding/decoding.

The L1 tests cover canonical encoding/hash, mutated-reader verdicts, BuildDiff/ApplyDiff/state diffs, unique-key preimages, and 200 seeded mutation histories with 64-bit collisions. The mutation evidence contains 107 accepted and 86 refused cases agreeing, plus four D12 deferrals. The allowance for Core's generic diff-addition error can also cover an order error, so that exception is not independent proof of every addition-order verdict. No source or test evidence establishes complete hostile-input parity, particularly R1-M1.

The 20 mainnet fixtures are **RPC-reconstructed imports**, with assigned internal ids/registration counter, spanning heights 1,030,852 through 1,868,231. They provide useful full-size byte/hash parity evidence, but do not verify online derivation of those ids/counters or the design's every-576th-block and boundary coverage on both networks. The probe manifest identifies oracle commit `a9a483e51efdadeee37cf40ff8697da62b3cd012` and the linked archive hashes. Both this provenance and the explicit unpinned L1 warning belong in the eventual merge evidence.

I inspected the implementer's exact-head gate marker and logs instead of repeating the workspace gate under high load: the main run records **3,541 passed, 1 failed, 66 ignored**. The failure is `tests_multi_wallet::test_runtime_add_during_initial_sync`; the reported BL-20 rerun records **54 passed, 0 failed**. This is reviewed implementer evidence, distinct from my targeted runs.

The codec fuzz target is marked **model**, not oracle differential. Accessible exact-head evidence verifies the final top-up: **0.1793 CPU-hours, 5,936,126 executions, zero divergences/crashes** (`/work/scratch/ev2a/fuzz-topup.log`), plus **43 saved cases replayed with zero divergences** (`logs/replay2.log`). Earlier logs include failures corrected before this head. The reported aggregate **1.07 CPU-hours/~35.8 million executions** was not independently substantiated from the accessible summaries. In any event, model round trips do not satisfy EV2 §6.3's ≥24-CPU-hour oracle differential budgets or rule out a correlated writer/reader defect.

PF0's `/work/scratch/ev2a/pf0-final.json` names the exact reviewed head and five runs. The rows execute the intended Rust paths: clone plus 100 hot updates and diff construction, allocation-inclusive unique lookup, canonical decoding with key materialization, and the streamed unvalidated canonical hash. The fixture is a synthetic 4,000-MN list. These are useful initial baselines; there is no Core comparison, full codec encode/576-diff-window leg, or the required map comparison (R1-m2). The measurements do not establish a Core-relative speedup.

## Gate decision

R1-M1 and R1-M2 require changes or a concrete, proved narrowing of the own-record contract before the Sol gate is satisfied. The passing targeted and oracle smoke checks do not discharge those findings. The remaining pinned-oracle, fixture-coverage, and campaign evidence must also be tracked explicitly for merge; none is silently credited here.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
