# BS11a-4 4b — review r1 (Opus, high)

- Fork: base-sdk `node/bsdk-bs11a-4` **aeb4d766** `pkc%feat: pair Exotic operands in Core's verifications` (on af5738e1 / f4b17c3a).
- rdc: **5c71f9fa2** `docs(node): record what BS11a-4 4b landed in D1, D2, D6 and D7` (`node/bs11a-4`).
- Core reference: v24.0.0-rc.3 `0364ed626e` (`~/workspace/dash`); dashbls and relic under `src/dashbls`.
- Reviewed in a detached scratch worktree (`/work/scratch/bs11a4-4b-review/src`) and a scratch rdc tree with the fork `[patch]` moved to aeb4d766 (`/work/scratch/bs11a4-4b-review/rdc`). No branch was touched.

## Verdict: CHANGES REQUIRED

The fork code is correct, and I found no parity defect in it. The one required change is in rdc. Moving the pin to aeb4d766 makes a dash-evo test fail (finding 1). Neither D2's "Landed (4b)" text nor the 4d row records this. It must be routed to the EV owner before 4d's pin bump. Once that is recorded and scheduled, the slice is ACCEPT WITH NITS.

Findings: **0 Blocker, 1 Major, 1 minor, 5 nit.** The two surviving mutants ruled under DEC-482 are not re-reported.

## Gates I ran

| Gate | Result |
|---|---|
| `cargo test -p dash-pkc --features full` (all targets) | 641 passed, 0 failed (lib 571, bls_dashbls 7, bls_oracle 14, bls_serial_threads 1, bls_wrapper 48 + 1 ignored) |
| `cargo clippy -p dash-pkc --all-targets --features full -- -D warnings` | clean |
| `cargo clippy -p dash-pkc --all-targets --no-default-features --features bls -- -D warnings` | clean |
| rdc `cargo test -p dash-evo -p dash-dml -p dash-coinjoin` with the fork at aeb4d766 | **1 failure**, dash-evo (finding 1); dash-dml and dash-coinjoin green |
| Mutant `CHUNK = 1` (native.rs:71), all bls tests | all pass, an equivalent mutant (finding 2) |

All builds used `-j 8`, with targets under `/work/scratch/bs11a4-4b-review/`, seeded by reflink from the implementer's targets.

## Core parity: checked and correct

| Claim | Core | Port | OK |
|---|---|---|---|
| `NativeVerify` multiplies one `pc_map_sim` per 250 pairs and returns false if the product ≠ 1 or the code ≠ OK | `schemes.cpp:326-350`; `pc_map_sim` → `pp_map_sim_oatep_k12` (`relic_pc.h:879`, `relic_pp.h:386`) | `native.rs:71-108` | yes |
| A pair with an infinity side is dropped; every operand is normalized before the Miller loop; with no pairs left the result is 1 and no FE runs | `relic_pp_map_k12.c:544-555` | `blst_ffi.rs` `miller_product` skips `is_inf`; `G1Operand::engine` maps blst's identity to `Ep::INFTY` (`native.rs:45-65`) | yes |
| One FE over the product of the chunks' Miller values equals Core's product of per-chunk pairings | FE = `pp_exp_k12` (`relic_pp_map_k12.c:572-580`) is a power map (4a D5, 10⁶ elements, k = 1, zero included); the simultaneous Miller value is the product of per-pair values (shared squaring, per-pair lines; 4a D3/D4) | `chunked_miller_product` + `final_exp` | yes, and the chunk size cannot be observed (finding 2) |
| Legacy `Verify`/`AggregateVerify` check no validity; basic ones check distinctness, then `IsValid` on sig and keys | `schemes.cpp:658-702` (legacy); `:416-431` → `:297-324` (basic aggregate), `:135-154` (basic single) | `core/mod.rs:207-230`: basic gate = distinct + `is_group`; legacy has no gate | yes |
| Wrapper checks: `VerifyInsecure` needs both wrappers valid; `VerifyInsecureAggregated` needs the sig valid, asserts non-empty equal lengths, then needs each key valid; `VerifySecureAggregated` checks nothing and passes `impl` | `bls.cpp:296-307`, `:314-339`, `:341-358` | `core/mod.rs:134-202` (`pairing_operand` = `impl` whatever its validity) | yes (see nit 4 on the assert) |
| `VerifySecure` sorts the flag serializations, hashes them, re-reads each with `FromBytes(bytes, flag)`, folds `publicKey += g1 * t` from the default element, then calls the scheme's `AggregateVerify` | `schemes.cpp:231-254`; the 4-arg `CoreMPL::VerifySecure` resolves the virtual `AggregateVerify(vector<G1Element>, vector<Bytes>, G2Element)` (`schemes.hpp:96-98`, overridden at `:269-271` → `schemes.cpp:683` for legacy, `:416` for basic) | `core/mod.rs:167-202` | yes |
| `t = SHA256(i ‖ SHA256(keys)) mod r` | `schemes.cpp:31-59` (`bn_mod_basic` by `g2_get_ord`) | `secure_weights` + `Fr::from(&t).to_bendian()`. This now reduces mod r, which matters for non-subgroup keys; the pre-4b code multiplied by the raw 256-bit t | yes |
| `+=` and `*` are `g1_add(a, a, b)` / `g1_mul` | `elements.cpp:212-217`, `:227-233`; the aliased `g1_add` is alias-safe for r = p (`relic_ep_add.c:150-186`, `:324-358` read p before they write r) | `sum.add(&key.mul(..))` (non-aliased; same result) | yes |
| Legacy re-read: never throws on written bytes, resets the code; basic re-read throws unless valid (BL-399) | `elements.cpp:24-30`, `:46-55` (throws only on a non-canonical `0xc0` lead, which `SerializeToArray` never writes, `:178-197`), `:84-85` + `bls.cpp:81-92` | `elem.rs:115-148` `redecode` | yes |
| All-zero legacy bytes read as (0, 2) | `elements.cpp:57-63` + `relic_ep_util.c:253-269` + `ep_upk` (sign = y > (p−1)/2) | `redecode`'s `None` arm rebuilds the Exotic from its basic bytes. I also checked an engine sum with x = 0 and y ≠ 2 (nit 6): it reads back as (0, 2), as in Core | yes |
| Batch fallback order | `bls_batchverifier.h:71-119` | `batch.rs` `verify`/`fall_back` (only the `unsupported` tracking was removed) | yes |

The routing predicate (`chunk_miller`, native.rs:75-87) sends a chunk to blst only when both sides of every pair are `Operand::Group`. `pairing_operand` (elem.rs:387-392) returns `Group` exactly when `BlsPoint::usable()` holds, and `usable()` runs the subgroup test for `OnCurve` values (wrap_codec.rs:104-110). So `Group` ⇔ dashbls `IsValid`, infinity included, for wrapper elements.

## Findings

### 1. Major — dash-evo test fails once rdc pins aeb4d766; D2 and the 4d row don't record it

- **Where:** rdc `dash-evo/src/llmq/commitment_tests.rs:609-658` (`a_single_member_check_without_a_verdict`; it fails at :657); `docs/node-program/designs/BS11a-4-notes.md` D2 (5c71f9fa2) and the 4d row.
- **What:** with the base-sdk `[patch]` rev moved from 4a6a9e4 to aeb4d766, `cargo test -p dash-evo --lib` gives 156 passed, 1 failed. The test's premise is a single-member check under the legacy flag with no BLS verdict (`OffSubgroupRoster`, the x = 4 legacy key at :581-585). 4b removes that premise: `verify_insecure` now answers on that key, so `queue_signatures` takes another arm and the assertion at :657 no longer holds. The test's new form is set by the manager with the 4d pin bump (DEC-485).
- **Also dead now:** `commitment.rs` :460, :503, :519, :547, :648; `dash-dml/src/blockprocessor.rs:405`; `dash-coinjoin/src/dstx.rs:339`, `queue.rs:306`. Their tests still pass, but every `Unsupported` arm is now unreachable.
- **Fix:**
  - Add to D2's "Landed (4b)": "rdc's dash-evo `a_single_member_check_without_a_verdict` changes outcome at the pin bump; owner EV, carried with 4d".
  - Add that test update to the 4d row's content.
  - Have the EV owner rewrite the test to Core's verdicts for that roster in the same commit as the pin bump, so the rdc workspace gate in 4d stays green. The dead arms stay with BL-400.

### 2. minor — the chunk-boundary test can't detect a wrong chunk size, and D6 over-states it

- **Where:** fork `pkgs/pkc/src/bls/native.rs:71` (`CHUNK`), `:142-148` (`cores_product`), `:153-171` (`the_router_gives_cores_product_across_chunks`); rdc D6 "Landed (4b)".
- **What:** FE is a power map, and the simultaneous Miller value of a chunk is the product of its pairs' values. So FE(Π chunks) = Π FE(chunk) for any partition, and engine and blst Miller values agree after FE on Group pairs. The chunk size therefore cannot be observed. With `CHUNK = 1`, every lib, bls_oracle, bls_dashbls and bls_serial_threads test passes. The reference `cores_product` also uses `CHUNK`, so the test can't pin 250 even in principle. The test is a good model check of Lemma E/U across a boundary, but it does not check the boundary. The same lemma shows that D1's "never mixed within a chunk" rule is stricter than needed: the cross-chunk product already mixes engine and blst Miller values.
- **Fix:**
  - Say in the test's doc and at `CHUNK` that 250 mirrors `schemes.cpp:337-341` but can't be observed (an equivalent mutant), and hard-code 250 in `cores_product` with that cite.
  - Reword D6 "Landed (4b)" to "model check across the 250-pair boundary; the partition is unobservable by D5".
  - Optional: route per pair rather than per chunk. Under the same lemma that is equally exact, and it saves up to 249 engine Miller loops when a chunk holds one Exotic operand. Keep per-chunk routing if Core's shape is preferred; then record why.

### 3. nit — the CodeQL exemption matches the bare name `Operand` in every opaque crate

- **Where:** fork `maint/codeql/rust/lib/policy.qll:262-268`.
- **What:** The justification holds. `Operand<G1Affine, Ep>` holds relic coordinates, which aren't canonical, so a derived `Eq`/`Hash` would compare coordinates rather than points; this is the same reason `Ep`/`Ep2`/`BlsRepr` are exempt. But `Operand` is a generic name, and any future `Operand` type in an opaque crate would be exempted silently. Today only `pkgs/pkc/src/bls/native.rs:25` defines one.
- **Fix:** anchor the exemption to the file with the existing idiom (`filters.qll:42,65`): `nameOf(t) = "Operand" and fileOf(t).getAbsolutePath().matches("%/pkc/src/bls/native.rs")`. Alternatively, rename the type `PairingOperand`.

### 4. nit — the test doc says Core gives false where Core asserts

- **Where:** fork `pkgs/pkc/src/bls/core/mod.rs:397-400` (doc of `wrapper_checks_come_before_the_pairing`).
- **What:** Core's `VerifyInsecureAggregated` *asserts* non-empty, equal-length spans (`bls.cpp:319`); it doesn't return false. dashbls's own argument check would answer **true** for (no keys, no messages, infinite signature) (`schemes.cpp:69-70`). The port's `Invalid` is a reasonable mapping of the assert, but "Core checks … the list lengths" and "an empty list … gives false" describe behaviour Core doesn't have.
- **Fix:** "Core asserts non-empty lists of equal length (bls.cpp:319); the port answers Invalid there", and the same one-liner on `verify_insecure_aggregated` (core/mod.rs:147-148).

### 5. nit — `is_group` ⇔ `IsValid` holds for `pairing_operand`, but not for `into_operand`

- **Where:** fork `native.rs:37-42`; `elem.rs:373-381` (`into_operand`); the basic gate at `core/mod.rs:211-217`.
- **What:** `into_operand` maps any `BlsArith::Relic` to `Engine`, including an engine result that is valid (infinity, or in the subgroup). The basic gate on the folded secure key is exact only because a basic-flag fold has only Group keys (redecode refuses the rest). That is correct today, but nothing states it.
- **Fix:** one sentence at `into_operand`, or at the basic gate: "a basic-flag fold holds only Group keys, so `is_group` is `IsValid` here". Alternatively, `debug_assert!(flag == Legacy || matches!(sum, Operand::Group(_)))`.

### 6. nit — the redecode test misses x = 0 with y ≠ 2; the fallback is implicit

- **Where:** fork `elem.rs:128-148` (the `None => (basic, point)` arm at :138); test `elem.rs:984-1020`.
- **What:** the arm is correct only because `from_point(…, Exotic, …)` rebuilds the point from the basic bytes, which reads x = 0 with a clear sign as (0, 2), as relic's `g1_read_bin` does. The test checks only (0, 2) itself. In scratch I built an engine sum with x = 0 and y ≠ 2: relic's mixed addition of an off-curve (x₁, 0) and a subgroup Q with x_Q = −2·x₁ gives X = 0 (`relic_ep_add.c:150-186`). Its legacy bytes are all zero, and `redecode` returns (0, 2), as Core does.
- **Fix:**
  - Add that case to the test. It is a decode check only, with no verification.
  - Add a comment on the arm: "the Exotic rebuild reads the bytes as relic does, so any x = 0 with a clear sign comes back as (0, 2)".

### 7. nit — the 4b evidence row names a quirk 45 replay that no fork test names

- **Where:** rdc BS11a-4-notes 4b row (evidence "quirk 45 replay (D12)"); fork `tests/bls_oracle.rs:701` (`assert_edge`).
- **What:** the edge corpus (80 rows: insecure 20, aggregated 24, secure 36; 40 per flag) is the flag × class matrix of D12, and 4b makes it run with no gaps. But no test or doc in the fork says that this is the quirk 45 replay, so 4d's `tested` stamp has nothing to cite.
- **Fix:** a doc line on the `#[test]` that runs `assert_edge`, naming quirk 45 (D12), with the aggregate-then-verify scripts it relies on.

## Test strength (summary)

- **Chunk boundary:** sound as a model check; it cannot detect a wrong chunk size (finding 2).
- **Oracle replays:** `assert_edge`, `relic`, `arith`, `ops` and `quirk22` now assert every row, with no gap list, and the RLC_OK code check after every step is kept. `assert_edge` and `assert_relic` require at least one engine-routed row. The counters count only valid wrappers (`point()`); that's harmless, since they only check reach.
- **No-thread test:** `verify_engine` (bls_serial_threads.rs:108-121) drives all three verifies through the engine and the legacy fold with an x = 4 key, three times, inside the thread count. That is adequate.
- **Batch:** the old `Unsupported` tests are replaced by an insecure/secure pair with expected verdicts and bad sets.
- **Mutants:** the two survivors are accepted under DEC-482 and not re-reported. `CHUNK` is an equivalent mutant (finding 2).

## Scratch to delete at cleanup

`/work/scratch/bs11a4-4b-review/{src,rdc,target,rdc-target,tmp}`: two detached worktrees, from base-sdk and rust-dashcore (`git worktree remove`), plus their targets.
