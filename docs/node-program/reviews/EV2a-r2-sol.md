# EV2a r2 — Sol review

Narrow follow-up review of `node/ev2a-dml` at `14f6bed587d1fed69c27d735661685b4a662dcc1`, over integration `76ae54e61`. Scope is closure of R1-M1, R1-M2, and R1-m1–m3, including DEC-182 and the new D20 domain refusal. During review the implementer advanced the worktree to `233d06af58142437d81669c681b49dd54c54a597`; I inspected that additional commit. It only records DEC-182 in architecture text and benchmark result labels, with no change to measured workloads or production/test source. SHA256 comparison against the requested head confirms the codec, types, list, diff and reconstruction sources remained identical (see `review-provenance.json`). Accepted r1 items are not reopened. I follow REVIEW-COMMON.md with the prompt’s disk-backed TMPDIR override. PLAN §7 is codenames only; private material and private-erratum mechanics are out of scope. No private material has been read.

Branch source is read-only. Evidence goes under `/work/scratch/ev2a-r2-sol/`; commands use `nice -n 10`, `CARGO_BUILD_JOBS=8`, `RUSTC_WRAPPER=`, and `TMPDIR=/work/scratch/ev2a-r2-sol/tmp` (0700). This report remains uncommitted.

## Findings and closure status

### R1-M1 — closed

`dash-dml/src/codec.rs:442–556`: direct CompactSize reads charge before loops/reads; payout script lengths and both share script lengths are peeked and charged before the base-sdk decoder can allocate them. Extended network info reads raw groups and counts itself, charges a domain length before `NIEntry::decode`, and only then collapses duplicate purposes. Thus normalized invalid entries and discarded duplicate groups cannot refund the budget. Invalid/truncated CompactSizes fall through to a decoder that rejects them; a missing second share length cannot cause an uncharged successful allocation.

The ADDRv2 length exception is correct against **exact rc.3**, commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`: `netaddress.h:435` uses `COMPACTSIZE(address_size)`; `serialize.h:720` explicitly calls `ReadCompactSize<Stream>(s, RangeCheck)`, while `snapshot.h:212–225` provides only one-argument overloads, including the transparent network-entry `OverrideStream`. Group/entry counts, LIMITED_STRING domains, and script/vector lengths use the one-argument path and remain charged. This is a correction of r1’s charge list, not a missing Rust charge.

Independent `cargo test -p dash-dml --all-features` passed, including the 14 committed budget cases and all 8 reconstruction tests. L1 at `bc83665708a0dafabd4b4e142cf86b92f8c05f43` passed **6 tests, 1 ignored**, including `nested_budget_verdicts_match_the_oracle`. The production codec, state types, list implementation, and reconstruction test are byte-identical between the reviewed worktree and L1. L1 linked the explicitly allowed **development** archive (`80e010ab5cb8275d48611bc4f1e1ecc4f76eabc6b525e9c079f381c55ed93414`); it is not credited as a pinned-release gate.

### R1-M2 — closed

`dash-dml/src/types.rs:298–324` now describes the complete version projection: below v3 the payout/share lists and early fields must be default; at/above v3 scriptPayout and scalar platform ports must be default; network info must be legacy below v3 and extended at/above it. `try_add` checks before mutation; `update` checks before replacing any changed cold state, and I-S checks the domain independently. Pointer-equal cold-state updates inherit the already-enforced invariant. Diff application uses a cloned candidate and installs it only after every addition/update succeeds, so a domain refusal is atomic.

### R1-m1 — closed

`dash-dml/src/codec.rs:1080–1092` no longer says hashing decodes keys. It requires a trusted expected-hash comparison and successful canonical encoding/readback as separate import checks, and explains that hashing retains the captured raw bytes. That matches the actual writer/materialization paths.

### R1-m2 — closed

`dash-dml/benches/mnlist.rs:93–101` constructs all 1,000 keys outside the timed closure; the new PF0 row calls only `has_unique`, retaining the original allocation-inclusive row. The one-time `unique_map_compare` bench compares the actual UniqueKey/UniqueSlot shapes in SipHash and Fx persistent hash maps and OrdMap, with 100,000 operations, clone/get/insert or remove paths, normal keys, 512 verified Fx-colliding services, and a benign equal-shape control. The documented five-pair instruction medians and comparison counts cover the requested selection evidence. DEC-182 accepts the normal-probe cost and replaces the old bound; it is not a new finding here. The source and documented evidence close the original missing-comparison finding under DEC-182. A fresh run of the unchanged measurement workload (with the DEC-182 result labels from `233d06af5`) reproduced all three normal rows and the SipHash/Fx adversarial rows, each a median of five 100,000-operation runs:

| Map and keys | probe instructions | add instructions | remove instructions |
|---|---:|---:|---:|
| SipHash, normal | 412 | 12,049 | 11,994 |
| Fx, normal | 186 | 11,603 | 11,536 |
| OrdMap, normal | 859 | 15,975 | 13,887 |
| SipHash, 512 Fx-colliding | 388 | 12,110 | 11,996 |
| Fx, 512 Fx-colliding | 13,752 | 269,782 | 253,635 |

Fresh SipHash adversarial/normal ratios are **0.94×/1.005×/1.000×** for probe/add/remove, all below DEC-182’s 2× bound. The driver independently verified 512 distinct service preimages with one Fx hash and 512 SipHash hashes. After those rows completed, I intentionally stopped this optional repetition by the exact benchmark PID and its numeric descendants; the remaining adversarial OrdMap/benign rows and comparison-count table were **inspected in the recorded evidence and code, not rerun here**. The resulting cargo exit 101/SIGTERM is recorded as an intentional stop, not a passing full benchmark run or a code failure (`unique-map-compare.log`, `unique-map-compare-stop.json`).

### R1-m3 — closed

`dash-dml/tests/reconstruct.rs:51–108,133–258,435–608`: records are encoded into `Written`, then decoded before initial storage and each replay. `ForcedBase` exposes just one selected snapshot, `bases` enumerates only snapshots on the requested block’s ancestry plus no snapshot, and each connected/stale block is resolved separately without cache from each base. Hash and snapshot-record bytes are compared as well as SameRecord, IsEqual, operator slots, counts, ids, and Core order. The cache leg resolves the blocks in both orders with the same cache.

`forks_and_restarts_rebuild_equal` places snapshots on both siblings, disconnects/reconnects and checks equal rewrites, registers after reconnect using the ids from the surviving parent, retains the abandoned sibling, then creates a **new MemEvoStore** solely by decoding the saved byte records and rechecks every block/base before adding further blocks. This tests record-level restart/reconstruction, with the chain/header ancestry supplied, not a durable backend or transaction-level re-execution of reconnects. The narrowed API claim is accurate for that scope.

`version_3_transitions_rebuild_equal` covers v2→v3 (including Evo scalar ports), v1→v3 with key re-encoding, v3 registration with payout shares/collateral shares/early fields, reset followed by restoration to v3, an operator change, further payout/early-field changes, forced earlier bases, and a record-only store restart. All **8 reconstruction tests** passed independently. No accepted r1 item is reopened.

## Fresh adversarial checks

The original `/work/scratch/ev2a-r1-sol/rebuild_check.rs` was copied unchanged and compiled against exact-head cargo artifacts. It now exits at the original `try_add(...).unwrap()` with `VersionHiddenField { version: 3, field: "scriptPayout" }`, rather than reaching a successful divergent reconstruction. The adapted `review_check.rs` asserts that refusal and an atomic update refusal after resetting an admitted v3 state with payouts/shares without restoring its version.

Fresh budget cases, separate from the 14 committed cases, combine **2 network items + 1 payout + 4,997 payout-script bytes + 1 share + 2,000 refund bytes + 2,999 reward bytes = 10,000**. The full boundary input succeeds in both Rust and the Core probe. Raising only the last length by one, and omitting its body, fails with the exact nested-budget text; Rust leaves the refund payload unread because it peeks the reward length before decoding the share. A second fresh bypass attempt has 50 raw invalid domains of 200 bytes, of which the 50th body is absent: **1 group + 50 entries + 50×200 lengths = 10,051**. It must not charge their normalized Invalid representations; both readers refuse for budget exhaustion before attempting the absent body. The canonical updated-state-diff reader was checked separately with a PAYOUTS|SHARES-only mask: **1 payout + 4,997 payout-script bytes + 1 share + 2,002 refund bytes + 2,999 reward bytes = 10,000** succeeds, and raising the final length to 3,000 with its body absent produces the exact budget refusal in both readers. No fresh bypass succeeded.

Inputs, formulas, Rust source/logs, and full Core outputs are in `fresh-budget*`, `fresh-diff-budget*` and `review_check.rs*` under the scratch directory. I also reran all 14 committed cases directly through the probe and matched every recorded answer (`committed-budget-recheck.log`). The probe’s MANIFEST identifies development source `a9a483e51efdadeee37cf40ff8697da62b3cd012` and binary hash `b2e7af8f04ffa1c39b604615ffe75dcde74c4620cca1fbf74dfd0c550c6496e6`. Runtime comparisons are development-oracle evidence; the overload and write-site conclusions separately use exact rc.3 source.

## rc.3 state-domain write-site checks

Exact-tag source copies are under `core/` in the scratch directory. I checked these independent write paths:

- `evo/specialtxman.cpp:51–105`, **SetStateVersion**: migration converts a legacy payout into the payout list and clears scriptPayout; the network form follows the target version; conversion to extended clears scalar ports. The early return is safe on states already in the domain; ProUpServ’s pre-set version still performs payout migration before it.
- `evo/dmnstate.h:127–135`, **ResetOperatorFields**, together with its installers: reset itself temporarily drops to v1 and retains the v3-only fields. ProUpReg (`specialtxman.cpp:537–582`), ProUpRev (`599–614`), and ProUpSharedReg (`665–700`) restore the target version/network form before UpdateMN. The Rust comment correctly requires the caller to do this.
- `evo/specialtxman.cpp:110–136,491–495`, **ProUpServ**: assigns the payload’s network form, explicitly clears ports for an extended Evo payload, then uses max(old,payload) and SetStateVersion to restore/migrate the final state. A legacy Evo payload applied to an extended state passes through conversion, which clears ports again.
- `evo/specialtxman.cpp:537–582`, **ProUpReg**: after any reset, uses SetStateVersion and chooses exactly one payout representation for the final version, clearing the other before UpdateMN.
- `evo/specialtxman.cpp:599–614`, **ProUpRev**: post-v24 preserves max(old,payload), restores it after reset, then installs ban/revocation changes; pre-v24 states have no v3-only fields to retain.
- `evo/specialtxman.cpp:620–645,650–700`, **ProUpShare/ProUpSharedReg**: share-reward update clones an already-shared state without a version change; shared registrar restores the old extended version after reset.
- `evo/dmnstate.h:74–89` and `evo/providertx.h:163–222`, **registration**: copies the freshly decoded payload. The payload reader reads only the active payout/share/early-field representation and scalar ports only below v3; omitted fields remain default. NetInfoSerWrapper selects the version’s form.
- `evo/specialtxman.cpp:330–343`, **confirmed hash**: clones the admitted state and changes only the two confirmed hashes.

These checks support the restricted domain on Core-produced lists. They do not establish a guarantee for arbitrary hand-built Core objects.

## D20 judgment

The scratch check constructs a canonical, scriptPayout-only update for an admitted v3 MN. Both canonical diff readers accept it. Core ApplyDiff succeeds; Rust refuses with VersionHiddenField and leaves the list, position, and Core order unchanged. This is the documented new deviation, not a codec-equivalence claim.

**Accept D20 for the self-produced stored-record/reconstruction API.** An own BuildDiff between two admitted lists carries every field needed to reach the admitted target; checking that target adds no refusal on the verified Core-produced state histories. Allowing foreign diffs to insert future-visible hidden state would reintroduce R1-M2. The restriction is explicit on try_add/update/apply_diff and in the API’s reconstruction claim.

This does **not** complete BL-81’s future snapshot-ingest validation: that boundary must still canonical-read every reconstructed list before storing it, including keys, network validation, order and size rules beyond this domain check. BL-81’s current sentence that the apply path stays Core-exact needs to be read with the documented D20 exception (or reconciled when D20 is adopted). I do not claim arbitrary foreign-diff apply parity or expand this approval to a future ingest implementation.

## Validation and limits

Completed independent checks:

- `cargo test -p dash-dml --all-features`: **79 unit/integration tests and 2 doctests passed**, 2 unit tests ignored (full import and vector printer). Includes all 8 reconstruction tests and 14 Core-judged nested-budget cases.
- `cargo test -p node-l1 --features oracle --test l1_dml`: **6 passed, 1 ignored**, with the permitted development archive; no claim of pinned-release completion.
- `cargo clippy -p dash-dml --all-features --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check 44a23b04e..HEAD`: passed.
- Original r1 reproducer: refused at the initial add; adapted assertions, D20 atomicity, fresh list/diff budget cases, and direct probe replay of all 14 committed cases: passed.

The hasher comparison compiled successfully and independently reproduced the five rows above before an intentional stop; full completion is not claimed. The wider pinned-oracle/workspace/fuzz/campaign gates from r1 were not rerun or silently credited in this narrow closure round.

The reviewed source worktree is clean at the observed follow-up head. I did not edit, commit, push, check out, or otherwise mutate either code branch. The review report is the only authored repository file and remains uncommitted on integration. Private material was not read. Scratch evidence is retained; the empty temporary directory was removed after the checks.

## Gate decision

R1-M1, R1-M2 and R1-m1–m3 are closed. D20 is acceptable within the explicit self-produced-record/domain contract; future BL-81 ingest work still needs its full boundary validation. No new blocker, major, minor or nit was found in this narrow round. This satisfies the Sol follow-up gate, without reopening or crediting the broader accepted/pending r1 items.

**Verdict:** APPROVE

Sol gate satisfied: yes
