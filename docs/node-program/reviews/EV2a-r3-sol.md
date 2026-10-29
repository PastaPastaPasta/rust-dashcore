# EV2a r3 — narrow Sol review

Reviewed exact `node/ev2a-dml` head `c34a88ec6770c96ef7c49f3620818857a45767ea`, rebased onto integration `3e7a3c05e`, in detached worktree `/work/scratch/ev2a-r3-sol/wt`. Scope: the six requested commits since r2, preservation of the r2 implementation, and the new pinned-oracle fuzz targets. Accepted r2 findings and D20 are not reopened. I used the code-review-validator workflow and exact Dash Core `v24.0.0-rc.3` source, peeled commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`.

No reviewed source was edited, committed or pushed. No GitHub actions or reads under `~/private` occurred. Builds and test programs used `nice -n 10`, `CARGO_BUILD_JOBS=8`, an independent target per workspace, and disk-backed `TMPDIR=/work/scratch/ev2a-r3-sol/tmp` at 0700. Standalone review checks were written outside the worktree. This report is uncommitted. The manager's private-suite wiring is outside this review.

## New findings

### R3-M1 — major: the new differential targets reduce some of Core's outputs

**Locations:** `node-l1/src/dml.rs:190–191,220–224,317–318,823–826`; compare the output fields in `dash-oracle-sys/src/mnlist.rs:29–36,75–81,116–121`.

The requested full-output check is not satisfied:

- The canonical list leg decodes `core.disk` and checks only `IsEqual`. That projection deliberately omits internal IDs and the registration counter; it also does not compare collision order. The original fixture test still additionally compares the imported canonical bytes (`node-l1/tests/l1_dml.rs:75`), but the new shared fuzz comparison drops that assertion. The disk-input leg compares canonical bytes/hash and ignores `core.disk` entirely. A standalone check constructs two admitted lists with identical members/state but different IDs and counters: `IsEqual=true`, while canonical bytes and hashes differ. Thus the predicate at line 191 can accept incomplete evodb reconstruction. No source mutation was needed to demonstrate this blind spot.
- The built-diff leg compares canonical bytes, then decodes/re-encodes `core.disk`. That normalization loses the evodb writer's update order. A standalone check reverses two serialized updates: the bytes change, but both evodb decodes re-encode to identical canonical bytes, satisfying this predicate. EV2 §2.9/§6.2 requires byte comparison; for Core-produced BuildDiff outputs, canonical diff and v24 dmn_D4 bytes are identical.
- `diff_unique_hash` compares only `core.preimage`, never `core.hash`. EV2 §2.2 explicitly requires `SHA256d(UniqueKey.bytes()) == GetUniquePropertyHash(v)`, and §6.3 names the hash as this target's output. The production choice of preimage keys/SipHash does not remove that test-only requirement.

**Failure scenario:** a reader regression loses imported counters/IDs or collision order, or a writer changes evodb ordering, while the reduced comparisons and fuzz success remain green. The unique target cannot establish its specified hash output at all. This is a gate-strength finding, not a demonstrated production consensus divergence.

**Fix:** compare the complete canonical content of every decoded Core list output and consume its entire input; compare Core's disk list bytes/order with a test-only SER_DISK writer over Rust's `core_order()` (or an equivalent complete check). Compare the built diff directly to both Core byte outputs. Retain the preimage check and also compare SHA256d of Rust's preimage with `core.hash`. Add guards that fail on counter/ID/order changes and an altered hash. Preserve the already accepted D3 refusal-text projection and explicitly documented D12/D17/D20 domain exclusions.

### R3-m1 — minor: the legacy operator-key seeds select the service operation

**Locations:** `node-l1/src/dml.rs:783,810`; `node-l1/examples/dml_seeds.rs:123`.

The target takes `kind % 6` before separating bit 7, while the generator supplies `0x85` for legacy operator keys. `0x85 % 6 == 1`, so both the valid legacy key and default legacy key seeds exercise service decoding, rather than operator hashing. The aggregate seed test allows `BothReject` and therefore misses the mistake.

An independent pinned-oracle check passes the same valid legacy key directly to `UniqueProperty::OperatorKey { legacy: true }`: Core accepts it. The target returns `BothReject` with selector `0x85`, and `BothAccept` with selector `0x83` (whose full-byte modulo happens to reach the operator branch). Mutation can eventually reach legacy hashing, but the advertised committed legacy seeds do not exercise it.

**Fix:** select the property using `(kind & 0x7f) % 6`, retain bit 7 as the scheme flag, and replay/regenerate the seeds. Assert that each intended property/scheme seed reaches that operation and accepts, rather than accepting aggregate agreement counts alone.

### R3-n1 — nit: two raw corpus files are treated as text by diff checks

**Locations:** `node-fuzz/public-seeds/diff_mnlist_order/f0ffa687b8b268b0:1`; `node-fuzz/public-seeds/diff_unique_hash/13eb332698fde4df:1`.

`git diff --check 3e7a3c05e...HEAD` reports trailing whitespace in these arbitrary binary inputs. The same check excluding the public corpus passes. Mark the binary corpus paths as binary in `.gitattributes` so text checks do not reject valid input bytes. Do not change the seed bytes just to satisfy a text checker.

## Closure of M1/m1/n1/n2

- **Opus M1 — closed for the tracked guard.** `dc9a58b9a` adds `a_point_only_key_change_is_recorded` to the ordinary unit-test suite. Every other state field is equal; both keys serialize to zero under Legacy and Basic, and their captured bytes also match. The failed-decode object is explicitly invalid. The only true clause is object/point inequality, and the required mask is exactly `PUB_KEY_OPERATOR | VERSION` (`0x401`) in both directions. The test passes at this head. A standalone reproduction verifies both byte clauses are false and the point clause is true: deleting the point clause or using a bytes-only comparison necessarily yields mask zero and fails this guard. This establishes mutant rejection without editing or rebuilding mutated branch source. Private-suite gating remains the manager's separately assigned work.
- **Opus m1 — closed.** `d072833c7` checks captured scheme for every non-default key in `MnCold::version_hidden_field`; `try_add`, cold-state `update`, and I-S all use it. Refusal is `VersionHiddenField { field: "pubKeyOperator", ... }`. The committed tests check both mismatches, defaults under both schemes, an atomic failed version update, a corrected recapture, and independent I-S detection. API.md no longer calls D17 an unenforced caller precondition. The defensive version-scheme diff clause remains.
- **Opus n1 — closed as an accurate ownership note.** The V24 gate/token belongs to EV2b's call boundary; EV2a implements no activation-aware caller. The method documentation continues to require gating. Exact-tag registration/service/registrar callers gate on V24; shared registrar/check paths are reachable only after their V24 checks. The comment does not pretend a token was added.
- **Opus n2 — closed.** The unique-key cache now documents the assumption that later decode preserves identity and places valid stored keys at the importer/transaction-check boundary. This accurately describes the unchanged cache, rather than claiming D17 itself validates BLS points.

## D17 versus rc.3 and the oracle legs

The default/non-default test mirrors `bls.h:514–527`: equality against the default exits before decoding. A default key remains admissible under either scheme, including keys left by `ResetOperatorFields` under the global flag.

Exact-tag write paths preserve the non-default invariant:

- ProReg and ProUpReg payload readers capture under the payload version (`providertx.h:180,346`); registration copies that state (`dmnstate.h:74–89`).
- `SetStateVersion` re-captures a non-default key with `Set(Get(), version == LegacyBLS)` before its early return (`specialtxman.cpp:69–71`). V24 service migration uses this path; pre-V24 service updates retain the old version (`:110–136`). Registrar updates call it before installing the state (`:537–582`).
- Revocation resets to a default key and restores the target version when needed (`:599–614`). Shared registrar updates restore the old extended version through SetStateVersion before installation (`:665–700`). Share-reward, confirmation and hot-state changes retain an already-admitted scheme.
- State/snapshot/evodb reads select the key scheme from the stored state version (`dmnstate.h:108`); a mismatched captured scheme is not an alternate encoding of an accepted serialized state.

Core's raw in-memory helpers can accept hand-built states/diffs outside this production domain; claiming they cannot would be incorrect. The independent check builds a canonical VERSION-only foreign diff from v1 to v2, retaining a non-default legacy key. Both canonical readers accept the bytes; pinned Core ApplyDiff accepts, Rust refuses atomically for `pubKeyOperator`. Such a diff is not Core's own BuildDiff for a completed version migration. This is the D17 extension of the already accepted own-record/D20 domain restriction, not evidence that a valid real-chain list is refused. The pinned L1 tests, 200 collision histories and 20 full-size mainnet fixtures all pass with D17 enforced.

## Fold, rebase and fuzz checks

**L1 fold (1c60edbab, 6efaa217b):** the folded `l1_dml.rs` at `6efaa217b` is byte-identical to former L1 head `bc83665708a0dafabd4b4e142cf86b92f8c05f43`. Its verdict allowances, assertions, 200 histories, unique-property cases and nested-budget checks were not weakened. The final commit extracts identical formatting/error helpers and adds seed replay; the original fixture assertions remain. R3-M1 concerns the new fuzz comparator, not a lost old test.

**Pin:** both local linking evidence and copied archives match `dash-oracle-sys/pins.txt`, oracle commit `d694a47043d90726b30b7ca4994d85fc5e483d5d`:

- plain: `e3bdd706e451554f0f0e6ace19008360dff11ab6152a5f61fda0cf1e34b60204`;
- fuzz: `56b03a462542adc121986c22f7bb1b2ac62377b5281d561fc4b8d0cd5f8b523f`.

The build copies each archive into OUT_DIR, hashes that copy, and links there; these copies were independently hashed. No ALLOW_UNPINNED override was used; the standalone check asserts ARCHIVES_PINNED. The pin adds the oracle/test seams to rc.3, not alternative DML production code. This closes the former development-archive evidence limitation for the executed L1 legs.

**Rebase:** `git range-diff 76ae54e61..233d06af5 3e7a3c05e..b8552ac9e` maps all 24 r2 commits. Twenty-three patches are identical; the codec commit differs only in Cargo/model-registration context inherited from integration. The entire `dash-dml` subtree and existing DML fuzz source/seeds are byte-identical between r2 head and `b8552ac9e`. The only subsequent production changes are the requested D17 enforcement and comments. Nothing from r2 was lost.

**Registration and seeds:** the equivalent registry is `node-fuzz/targets.toml`, with four matching target files, Cargo bins, `node_l1::TARGETS` entries and v24.0 pins. `fuzzctl check` passes: 45 targets, 44 smoke legs, 44 farm legs. Each new target has a 30-CPU-second smoke budget, nonzero nightly weight and a private route. Public committed seed counts are codec 211, diff 220, order 16, unique 14. Every seed executes; outcomes are respectively `(121 accept,86 reject,4 outside domain)`, `(74,141,5)`, `(9,7,0)`, `(11,2,1)`. The last target's aggregate success hides R3-m1.

| Target | Full comparison assessment |
|---|---|
| diff_mnlist_codec | Canonical list bytes/hash and verdict are compared; evodb output uses a reduced projection or is omitted (R3-M1). |
| diff_mnlist_diff | Applied list bytes/hash and each state diff/applied state are compared; built evodb diff bytes are normalized (R3-M1). D12/D17/D20 exclusions are explicit. |
| diff_mnlist_order | Full returned operation failure index/class/text, or complete ordered hashes plus canonical list bytes, are compared. No reduced list projection. Payee/projection implementations are EV2b work. |
| diff_unique_hash | Preimage and decode agreement are compared; returned hash is omitted (R3-M1), and intended legacy seeds route incorrectly (R3-m1). |

**DEC-184/186 applicability:** the four entries are compatible with FZ1's generic registry/plateau/certification mechanism. I loaded the reviewed registry through FZ1 head `1e06f2f2e`, ran its registry check, and inspected engine/floor selection: all four select one process, J=1, default minimum 1,000,000 executions. No certification ran. The requested c34a88ec6 base itself predates FZ1 and has no certify subcommand: FZ1 must be integrated before an actual gated-head confirmation. Live plateau is only a stop heuristic. Acceptance requires the separate confirmation at the gated head, 3600 seconds of fuzzer CPU after INITED, before/after corpus replays with equal cov and at most 0.1% ft growth, no NEW, clean fail-closed verdict/findings, and the execution floor. Smoke and seed replay do not satisfy that gate; R3-M1/m1 must be fixed before its evidence is meaningful. The diff smoke produced 2,621 executions in about 38 CPU seconds (~69 executions/CPU-second), below the ~278/CPU-second needed for the default confirmation floor. A short evolving-corpus smoke does not prove the eventual rate, but neither does it show that floor is achievable: review the structured target/corpus and, if justified, the DEC-186 per-target `min_certify_execs` override when preparing confirmation.

## Independent validation and limits

- `cargo test -p dash-dml --all-features`: **81 unit/integration tests and 2 doctests passed**, 2 unit tests ignored.
- `cargo test -p node-l1 --features oracle --test l1_dml -- --nocapture`: **7 passed, 1 ignored**.
- Ignored pinned L1 full-size leg with `/work/scratch/ev2a/vec/core-lists.txt`: **1 passed; 20 imported mainnet lists checked**. These remain RPC-reconstructed import fixtures, not online chain replay.
- Targeted clippy for dash-dml/node-l1 with oracle, all targets, warnings denied: passed. Workspace formatting check: passed. Text commit-range diff check: passed; full corpus diff check has R3-n1.
- Standalone `review_checks.rs`: M1 clause isolation, IsEqual blind spot, evodb diff normalization, selector routing against pinned Core, and atomic D17 foreign-diff refusal all reproduced successfully.
- `fuzzctl smoke --only diff_mnlist_codec diff_mnlist_diff diff_mnlist_order diff_unique_hash --parallel 1 --no-selftest --no-triage`, with a fresh 0700 scratch sink: **4/4 passed**, diff_mnlist_codec 35,088; diff_mnlist_diff 2,621; diff_mnlist_order 50,680; diff_unique_hash 1,032,049 executions; **zero divergences, crashes, sink failures or refusals**. Aggregate fuzzer CPU was 187 seconds against a 120-second floor. Total elapsed time including fresh native compilation was 1,787 seconds; each actual fuzzing leg was short. Runner selftest/triage were intentionally excluded from this scoped smoke. Every verdict names the clean exact head and pinned fuzz archive. This is execution evidence, not certification.

Evidence is under `/work/scratch/ev2a-r3-sol/`: `rebase-range-diff.txt`, `rebase-integrity.json`, `provenance.json`, `dml-tests.log`, `l1-dml.log`, `l1-fullsize.log`, `clippy.log`, `fmt.log`, `review_checks.rs`, `review-checks.log`, `fz1-compatibility.log`, `fuzz-smoke.log`, `fuzz-verdicts.json`, saved plain/fuzz build-link outputs, and the scratch-only fuzz sink. Both generated Cargo `target/` directories have been deleted; no `target/` remains under this scratch directory. The detached source worktree is clean and unchanged. Logs, the standalone checks and scratch-only fuzz evidence are retained.

## Gate decision

The requested M1/m1/n1/n2 changes are satisfactory; D17 does not reject the examined Core-produced chain-state domain, the old L1 fold is intact, the executed oracle legs are pinned, and the r2 implementation survived the rebase. The new fuzz comparisons fail the requested full-output contract, and the legacy selector loses its intended seeded coverage. Fix R3-M1 and R3-m1 before the Sol r3 gate is satisfied. No production consensus divergence was demonstrated in this narrow review.

**Verdict:** CHANGES-REQUIRED

Sol gate satisfied: no
