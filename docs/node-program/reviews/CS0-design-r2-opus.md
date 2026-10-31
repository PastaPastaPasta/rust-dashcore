# CS0 design review, round 2 (Opus)

- **Reviewed:** `designs/CS0-node-primitives.md` r1 (the changed sections and §16), the private companion note r1, and DEC-30.
- **Role:** designated EMBARGO-1 reviewer (PLAN §7.2 item 3). EMBARGO-1: see ~/private/embargo/EMBARGO-1.md. EMBARGO-1 findings are in `~/private/embargo/CS0-design-review-r2-opus.md`; this file gives them by ID only, as "see private review".
- **Scope:** I wrote only this file and the private review. I ran one check (Core's tx vector files on both tags), in `/dev/shm`.

**Verdict: APPROVE-WITH-CHANGES.** All round-1 findings are resolved, except one manager item (P-m3, open in PLAN). The r1 changes add one blocker (P2-B1, see private review) and two majors (M2-1 and M2-2). All three are design-text changes. The manager can check them against the revised §9.1, §9.4 and §9.5; no further full round is needed.

---

## 1. Round-1 findings

| ID | Status | Notes |
|---|---|---|
| B-1 | Resolved | The tracked driver knows only the public targets' fields, uses `deny_unknown_fields`, and reads cases only through `MANIFEST` with opaque names (§10). D7 is replaced by private seeds of the owners' own public targets, so nothing tracked changes at their merges (§10, DEC-30 N2). M2-2 covers one operational gap |
| M-1 | Resolved | §9.4 gives the loud skip through raw stderr when the variable is unset. When it is set, a missing, partial, altered or extra case fails the test. Count receipts newer than the gate start are checked against the manifests. No tracked file sets the variable. Targets refuse to run without it |
| M-2 | Resolved | One runner, `run.sh`, for every `node-fuzz` target. The corpus, crashes, logs and `fuzz-N.log` stay in a private run directory with explicit modes. An init-hook refusal never dumps an input. Divergences never panic. A self-test covers the fail and sink-failure paths. The twins get opaque seeds and no persistence. One shared comparison serves both forms (m-12). M2-1 covers the one place this is not yet applied |
| M-3 | Resolved, preferred form | `CompareWriter` decides `wire_is_canonical` exactly. See §2 |
| M-4 | Resolved | (a) Idempotence holds for all values, identity only where canonical, and `TxType` is built through `from_base`. (b) The stable-txid cases come from O1 on both pins, plus Core's `evo_assetlocks_tests` cases. P2-B1 changes where their expectations come from |
| M-5 | Resolved | Numeric type comparisons; `Tx::new` canonicalises the type; a general brief line for every consensus task |
| M-6 | Accepted in part; **the partial rejection is reasonable** | See §3 |
| P-M1 to P-M6 | Resolved | See private review §1 |
| m-1 to m-12 | Resolved | m-9 is forwarded to O17. P-m3 (PLAN wording) is open with the manager; see private review |

## 2. The exact comparison replaces the predicate soundly on both pins

- The serialization rules ported in `canonical::write_tx` are identical in `v23.1.8` and `v24.0.0-rc.3`, so the decoder correctly takes no rules (§5.1). Header and block encoding are identical too.
- `CompareWriter` compares exactly the bytes `core_write` emits against the wire range. `wire_is_canonical` therefore holds exactly when `wire == core_serialize(value)`, with no sufficiency argument and no dependence on base-sdk's decode domain.
- When the flag is false, the hashes are taken over `core_serialize`, which is Core's `GetHash()` (v23.1.8) and `GetInstanceHash()` (v24). The release-dependent txid only selects between hashes of the same bytes.
- §9.2's "every stored transaction and block is `wire_is_canonical`" is now a true, cheap check of the storage assumption.
- Core's `tx_valid.json` and `tx_invalid.json` on both tags (83 and 76 rows) all round-trip byte-exactly under Core's serializer, so §9.1's "`core_serialize` reproduces its bytes" is sound.
- The measured +1.8 % instructions is a reasonable price, and `block_decode_hash` re-measures it.

**Minor m2-1.** Specify that `CompareWriter` handles `written > wire.len()` (stop comparing and keep counting, with no slice indexing past the end) and that `matches()` requires `written == wire.len()`. The fuzz targets will reach that case.

## 3. M-6, the partial rejection

The rejection holds. Clippy's `disallowed-methods` resolves a trait-method call to the trait item, so banning `Hashable::hash` bans it for every implementation. base-sdk `33fcc59` has 18 `impl … Hashable for` sites, some macro-generated for several types, mostly non-consensus types. Carrying the rule across crates as a type (`CoreBytes`) is the stronger mechanism anyway.

Two refinements:
- **m2-2.** Some base-sdk methods that produce consensus hashes are **inherent**, so they can be banned node-wide per type, for example `dash_primitives::Block::merkle()`, which hashes leaves through base-sdk's own encoder. Add the inherent hash and merkle helpers of `Transaction`, `BlockHeader` and `Block` to a shared node-wide `disallowed-methods` list. The trait ban stays in `dash-consensus`.
- **m2-3.** Name `CoreBytes`'s constructors exhaustively. ST2 reads stored blocks back for relay. Core's `ReadBlockFromDisk` decodes and re-serializes, so the faithful port is `decode_block(..)?.core_bytes()`. State that ST2 goes through that path, and that there is no `from_vec`/`from_stored` escape hatch; one would void the compile-time property.

## 4. New findings

### Blocker

**P2-B1. The boundary table's recorded codec expectations (§9.1): see private review.**
- **Required:** the codec rows of the §9.1 boundary table (CompactSize, integer fields, shapes, truncation and suffixes, stable txid) take their expected values **live from O1 at test time** and never commit them. They run with the twins (`dash-conformance`, feature `oracle`, every PR) under §9.5's output rules.
- Rows that never decode a transaction can keep recorded expectations: merkle, the `ValidationError` strings, the txid display.
- Keep the rows' coverage as specified; do not narrow it.

This is the same rule the L1 targets already follow: codec expectations come from the oracle and stay out of git.

### Majors

**M2-1. §9.1's property tests are outside §9.5's output discipline.**
- §9.1 says a failing property "prints the seed only". With tracked generators, a printed seed reproduces the input exactly.
- §9.5 says seeds print as a fixed placeholder, but its lint and macro rules list only the quirk driver, the replay, the sink and the twins.
- proptest's default `failure_persistence` writes `proptest-regressions/` beside the test source, inside the worktree.

**Required:** the twins' rules apply to the `dash-consensus` properties too:
- a placeholder seed `Debug`;
- `failure_persistence: None`;
- no `assert_eq!`/`prop_assert_eq!`/`assert_ne!` on values derived from inputs;
- the §9.5 lint set and the reviewer grep.

Fix §9.1's sentence to match. If reproducers are wanted, the property writes them to the sink when `DASH_EMBARGO_DIR` is set; `dash-consensus` can dev-depend on the sink helper.

**M2-2. The private-seed replay fails every other gate between "seeds written" and "owner merges".**
- DEC-30 N2 has a designated reader write a task's private seeds after its target exists on the task's branch and before that task merges.
- §9.4 makes the replay fail on "a seed directory whose target does not exist", and every gate reads the same private tree.
- In that window, every other task's gate fails, which invites someone to relax the check.

**Required:** each manifest entry has a state, `pending` or `required`.
- A `pending` target is replayed when it exists in the tree under test and is skipped otherwise.
- The manager flips it to `required` as a merge-checklist step (DEC-30).
- A `required` target that is missing fails the replay.
- The public output stays the single aggregate line, and the receipt's count is checked against the entries that apply to the tree under test.

### Minors

- **m2-1, m2-2, m2-3:** §2 and §3 above.
- **m2-4. Concurrent gates and receipts.** "Newer than the gate's start" can be satisfied by another gate's run. Have the gate pass a run id (for example `DASH_EMBARGO_RECEIPT_ID`) and have the receipt carry it.
- **m2-5. Init-hook argument check.** Check every path-valued libFuzzer argument, not only `-artifact_prefix`:
  - the first positional corpus directory, which is where libFuzzer writes;
  - `-exact_artifact_path`;
  - `-merge` output;
  - `-minimize_crash`.

  All must be inside the run or corpus directories. Account for libFuzzer's last-occurrence-wins flag parsing, and have the runner invoke the built binary rather than `cargo fuzz run`, which adds its own `-artifact_prefix`.
- **m2-6. Tracked residuals in other documents (manager): see private review.** Some tracked PLAN and CS1 text still reflects r0's D7. DEC-30 N2 already covers CS1; the PLAN items are listed privately.

## 5. Disposition table

| ID | Severity | Section to change |
|---|---|---|
| P2-B1 | Blocker | §9.1 (boundary table expectations), §3 layout (table moves beside the twins or gains the `oracle` dev-dependency) |
| M2-1 | Major | §9.1, §9.5 |
| M2-2 | Major | §9.4, §10; private note §3.3 |
| P2-M1, P2-M2 | Major | private note (see private review) |
| m2-1 to m2-6 | Minor | as listed |
