# GV1a review (r1)

**Revision reviewed:** `node/gv1a` at `6c9a71ca7`  
**Diff:** `ac41eae465..6c9a71ca7`  
**Scope:** `dash-governance`, the node-L1 governance harness/tests, public seeds (skimmed), and `GV1a-notes.md`.

## Verdict

**ACCEPT WITH NITS**

**Finding counts:** Blocker 0, Major 0, Minor 4, nit 0.

The implementation agrees with the exercised Core v24.0.0-rc.3 behavior. I found no normal-input parity defect in the hashes, proposal validator, trigger parser, payment-limit arithmetic, best-superblock ordering, vote tallies, or rate calculations. The issues below are non-blocking but should be addressed before treating the API and test matrix as complete.

## Findings

### 1. Minor — network vote decoding reads an unbounded signature before applying Core's bound

**Location:** `dash-governance/src/vote.rs:41-43`

`decode_network` first calls `GovVote::decode`, which decodes the signature `Vec<u8>`, and only then checks for 65 or 96 bytes. The base SDK vector decoder derives the count and reserves/reads that many elements (`base-sdk/pkgs/types/src/codec.rs:307-320`). Core's network serializer uses `LIMITED_VECTOR(obj.vchSig, BLS_SIG_SIZE)` before reading the vector and then checks the two exact sizes (`src/governance/vote.h:190-203`). Thus an input with a signature count greater than 96 and enough payload is rejected by both APIs, but Rust needlessly allocates and consumes the oversized vector first. The P2P policy already rejects this count at `dash-p2p/src/policy.rs:1190-1215`, so this is primarily a standalone API/resource issue.

**Fix:** pre-read the CompactSize at the fixed signature offset (84 bytes), reject counts above `BLS_SIG_SIZE` before `GovVote::decode`, or add a bounded network decoder. Add a regression with count 97 and a complete payload (and one with a large CompactSize) so the bound is tested independently of the final length check.

### 2. Minor — the differential network matrix omits devnet and aliases selector 3 to mainnet

**Location:** `node-l1/src/gov.rs:323-335, 767`; seed generation at `node-l1/src/gov.rs:1422-1427`

`params_of` has only mainnet, testnet, and regtest entries, and `network % 3` maps the fourth flag value back to mainnet. The seed generator likewise emits `net in 0..3`. The production crate does take address prefixes/Platform HRP and governance parameters from the supplied `ChainParams` (`dash-governance/src/lib.rs:39-82`), which is the right design; Core has distinct chain parameter construction for mainnet, testnet, devnet, and regtest (`src/chainparams.cpp:252-264`, `448-459`, `621-633`, `895-907`). Consequently the L1 coverage does not exercise devnet's parameter path (including its address decoding, cycle/start/budget/subsidy values), and arbitrary fuzz flags with selector 3 silently test mainnet instead.

**Fix:** make the harness table four-way, construct a named devnet with `ChainOption::Devnet`, generate devnet seeds, and assert that all four selectors reach their intended chain. Keep the production `GovParams` parameter-driven path unchanged.

### 3. Minor — no Core-linked test exercises `Superblock::is_valid` / EV3 coinbase matching

**Location:** `dash-governance/src/trigger.rs:127-136`; the L1 parsed-trigger comparison at `node-l1/src/gov.rs:894-910` and block comparison at `node-l1/src/gov.rs:1198-1218`

The crate delegates `Superblock::is_valid` to EV3, but the governance L1 record compares parsing, expiry, payment limits, and selected payments; it never submits a coinbase transaction to the `is_valid` path. Core's `CSuperblock::IsValid` has independent branches for invalid height, output-count/order matching, payment-budget limits, block-value limits, and the V24 duplicate-output scan (`src/governance/superblock.cpp:248-334`). Those branches can regress without failing the current governance differential tests. The existing L0 fixture reaches a funded superblock, but does not distinguish these coinbase acceptance/rejection cases.

**Fix:** add an oracle comparison (or a focused Core-vector replay) that builds coinbases around the selected payments and checks off-height, exact/over-limit budget, missing/reordered/duplicate outputs, block-value, and both `is_v24` matching modes. Include the valid and invalid outcomes in `l1_gov`.

### 4. Minor — `RateCheckBuffer` exposes invariants that Core keeps private

**Location:** `dash-governance/src/rate.rs:18-23, 48-50`

`timestamps`, `start`, `end`, and `empty` are public, so callers can construct `start`/`end` outside `[0, RATE_BUFFER_SIZE)`; `held` then indexes the fixed array using those values and can panic. Core keeps the corresponding vector and indices private (`src/governance/governance.h:66-73`) and only exposes operations; its serialized fields are an implementation detail (`src/governance/governance.h:150-153`). This is an API-hygiene/invariant issue for a pure crate, not a consensus mismatch in the normal GV1b persistence path.

**Fix:** make the ring fields private and provide `new`/`add`/accessors plus a `from_parts` or persistence-specific constructor that validates vector length and indices. If GV1b must deserialize Core's raw representation, keep that conversion at a narrow boundary rather than exposing arbitrary construction to every caller.

## Parity and arithmetic review

- Object, data, and signature hashes use the base wire serialization and Core's hash order; the reference implementations are `src/governance/common.cpp:23-39` and `src/governance/object.cpp:543-554`.
- Vote hash/signature-hash/string and ECDSA/BLS key selection match `src/governance/vote.cpp:145-209` and the static validity gates at `src/governance/vote.cpp:211-250`.
- Proposal field order, JSON partial-root behavior, error text, and expiration checks match `src/governance/object.cpp:31-301, 999-1033`; the Core proposal vectors are replayed by `dash-governance/tests/l0_proposals.rs`.
- Trigger parsing, amount fixed-point parsing, expiry horizons, and byte-exact `GetHexStrData` follow `src/governance/superblock.cpp:22-64, 75-225, 339-413`. Payment-limit selection and EV3 validation are delegated through the same parameterized subsidy path used by Core (`src/governance/superblock.cpp:145-163, 248-334`).
- Best-superblock selection uses internal hash bytes and a strict `>` update, matching Core's `std::map<uint256>` walk and tie behavior in `src/governance/superblock.cpp:556-576` (map comparison is `memcmp` in `src/uint256.h:55-59`).
- Tally thresholds, weighted counts, vote replacement, and sentinel flags follow `src/governance/object.cpp:375-481, 880-997`.
- Rate-buffer ring behavior and the rate threshold follow `src/governance/governance.h:66-153` and `src/governance/governance.cpp:732-788`. Extreme signed-clock inputs that would overflow Core's C++ arithmetic are outside the oracle's defined domain; I did not count those undefined cases as implementation findings.

The remaining test-strength gap beyond the numbered findings is that the differential corpus does not independently target malformed persisted rate-buffer state or undefined-overflow clock/height inputs; the latter have no defined Core result to assert against.

## Validation

All commands were run in the GV1a worktree in the foreground with `CARGO_TARGET_DIR=/work/scratch/gv1a-sol-r1/target` and `CARGO_BUILD_JOBS=8`:

- `cargo test -p dash-governance` — passed (29 unit tests, 1 proposal-vector test, 2 regtest replay tests, 3 superblock tests, 7 tally tests).
- `cargo clippy -p dash-governance --all-targets -- -D warnings` — passed.
- `cargo test -p node-l1 --features oracle --test l1_gov` — passed (4 tests, including the 21-second census and Core oracle replay).

No branch files were edited, committed, or pushed.
