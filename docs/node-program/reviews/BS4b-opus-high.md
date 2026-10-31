# BS4b — Opus-high review

**Verdict: APPROVE-WITH-NITS.** No blocker or major. Three minors, none of which makes a reported number wrong. Finishing the `bs4b-r1` campaign is still the separate merge gate.

- **Reviewed:** `node/bs4b-decode-evidence`, `e8cdec55 → 6d509894` (8 commits), read-only at `~/workspace/rdc-wt/bs4b`.
- **Contract:** `briefs/REVIEW-COMMON.md`. Prior rounds: `BS4b-r1-sol.md` and `BS4b-r2-sol.md` (APPROVE). Their closed findings are not repeated here.
- **Core sources:** `v24.0.0-rc.3` (`da567d7d`). The serializer headers are extracted to `/work/scratch/bs4b-opus-high/core/`.
- **PLAN §7 handling:** codenames only. Only the count fields of the private receipts were read.

## What I ran

All runs used `TMPDIR=/dev/shm/bs4b-opus-tmp` (mode 700), `RUSTC_WRAPPER=` and `nice 10`. The log is `/work/scratch/bs4b-opus-high/tests.log`.

| Check | Result |
|---|---|
| `gen-bsdk-seeds.py --check` | `277 seeds ok` |
| `cargo test -p node-l1 --test bsdk_recorded` | 5/5 pass, including the 3 floor negative controls |
| `cargo test -p node-l1 --features oracle --test l1_bsdk_decode` | 4/4 pass: rows, recorded = oracle, `extra_payload_at_max_size`, census at 40/seed |
| `cargo test -p node-l1 --features oracle --test private_seed_replay` | 4/4 pass. The private replay skipped because `DASH_EMBARGO_DIR` was unset; the gate runs it. |
| `fuzzctl.py check` / `selftest` | ok (13 targets, 18 + 18 legs) / 51 tests ok |
| Full-history summaries `/work/scratch/bs4b/full/*/summary.json` | See below. |
| Read-only oracle spot checks (`contrib/oracle-cli`) | Non-null v3/v4 commitments and v3 CbTx ChainLock signatures exist inside the run's range (finding n2). |

**Full-history summaries:**
- `bsdk_blocks == blocks` and `bsdk_txs == txs` on both chains.
- `bsdk_payloads` equals the sum of `special_txs`: 1,332,050 on mainnet and 2,248,740 on testnet.
- 0 mismatches, 0 unverified.
- Mainnet type 5 = 1,066,511 = 2,094,670 − 1,028,160 + 1, which is exactly every block since DIP3 activation. Testnet type 5 = 1,562,316 = every block since height 7000.
- `REVISION` is `b368bf46`. `git diff b368bf46 6d509894 -- node-l1/src conformance dash-oracle-sys Cargo.toml Cargo.lock` is empty, so the run's claim to apply at head holds.

## Answers to the five questions

### 1. The judges

**Transactions, blocks and headers.** The judge is a real differential.
- `Reader` (`node-l1/src/bsdk_decode.rs:85-158`) splits Core's re-serialization by hand in Core's wire order. The extra-payload condition is `nVersion >= 3 && nType != 0`, as in Core's `SerializeTransaction`.
- base-sdk's decoded value is written field by field.
- The verdict and `consumed` are compared first.
- A wrong Core length becomes `"core serialization"`; it is never a silent pass.

**Payloads.** The verdict and `consumed_all` are compared, then the whole codec encoding with the BLS slots patched in.

**BLS placement.**
- **Global signatures:** placed from the end of the codec's bytes. The tail is `8` for CbTx v3 and `8 + 32` from v4 (Core `cbtx.h:58-66` reads `merkleRootAssetUnlocks` for every version ≥ 4, so v5 is right). The asset-unlock tail is `0` (`assetlocktx.h:109-118`: `quorumSig` ends the payload at every version).
- **Lazy keys:** placed at the first byte where the codec's encodings with the key unset and with `VALID_KEY` differ. `VALID_KEY[0] = 0x97 ≠ 0`, so that byte is the key's first byte.

**Placement errors are loud.** Each failure mode of placement produces a false divergence, never a false agreement:
- An unplaceable lazy key leaves zeros against Core's raw bytes.
- An out-of-range slot is skipped.
- A wrong offset leaves the real field unpatched.

**Accept-set model.** base-sdk decodes without the flag, which is correct. Core's accept set for global and eager fields does not depend on the flag. `bls.h:184-203` (`CBLSWrapper::Unserialize`) tries the given scheme, then the opposite one. It throws only when both fail, and it `Reset()`s (serializes zeros) when the opposite scheme succeeds. The judge models the zeros through `BlsGlobal::get(ctx)` for global fields and through the codec for eager ones.

**One class the judge cannot see:** lazy-key validity (m1).

### 2. The DDE #10 floor

**Completeness.** It is complete against Core v24. Every `CBLS*` member of a v24 payload class is in `BLS_FIELDS`, with Core's read versions (the 11 fields of `bsdk_rows.rs:121-188`):

| Type | Field | Versions | Core source |
|---|---|---|---|
| 1, 3 | `pubKeyOperator` | 1–3 | `providertx.h:180,346`. Versions outside 1–3 bail out after reading the version. |
| 12 | `pubKeyOperator` | every version | `providertx.h:534`, `legacy=false`, no version gate |
| 2, 4 | `sig` | 1–3 | `providertx.h:301,419` |
| 5 | `bestCLSignature` | ≥ 3 | `cbtx.h:62` |
| 6 | `quorumPublicKey`, `quorumSig`, `membersSig` | every commitment version | `commitment.h:110-113`. Legacy only at 1 and 2. |
| 7 | `sig` | every version | `mnhftx.h:49`, fixed basic scheme |
| 9 | `quorumSig` | every version | `assetlocktx.h:117` |

- The `vchJoinSigs` / `vchSigs` of types 1, 10 and 12 are `CompactSignature` (`providertx.h:52`, ECDSA). They are correctly absent.
- Types 8, 10 and 11 have no BLS member.

**Vacuity.**
- **Eager and global cells:** the "neither" row must be refused under both flags (`bsdk_rows.rs:217`). If the row's edit had missed the field, that refusal would not happen, so the neither row also proves the placement for the other three classes.
- **Lazy cells (40 cells, types 1, 3 and 12):** these are vacuous with respect to validity (m1). They still enforce acceptance, `consumed_all`, raw-byte reproduction, and that every byte outside the key equals Core's.

### 3. The `pins` field

**Semantics.** Sound.
- The CS0 targets keep `&Pin::ALL`. The four BS4b targets are `&[Pin::V24_0]`.
- `private_seed_replay` iterates `registered.pins`.
- `fuzzctl check` compares the Rust pins with `targets.toml`, and the gate runs it (`/work/scratch/gate.sh:39`, step `fuzz-check`).
- This also closes O2c's BL-62 for BS4b's form.

**Mechanics.** The change clashes with O2c's independent version of the same field (m3). It needs a follow-up in CS3 and is neutral under RP-V24.

### 4. The implementer's coverage limits

**No non-null v3/v4 commitment rows.** This does not matter before merge.
- The witnesses are judged by Core.
- The full-history run already judged hundreds of thousands of real non-null v3/v4 commitments and v3 CbTx signatures from both chains, under both flags.
- I found examples inside the run's range:
  - mainnet 2,094,611: v3, LLMQ 4;
  - mainnet 2,094,666 and 2,094,667: v4, LLMQ 5, indexes 0, 1 and 31;
  - testnet 1,569,347: v3, LLMQ 1, 4 and 6;
  - mainnet 2,094,665: CbTx v3 with `bestCLSignature`.
- The evidence doc understates this (n2).

**Lazy validity compared only through the written bytes.** The statement is inaccurate, and the gap is real but has an owner (m1). It is not merge-blocking.

### 5. The `bsdk-judges` conformance leg

**Reproducible.**
- The revision, run identity, base-sdk revision, oracle version and tip hash are all recorded.
- The tip moves, but the summaries pin the range.
- The counters prove the leg ran on every block, transaction and special payload.

**Honest, with two caveats.** Header, block and transaction judgements take the chain's bytes as Core's side. That is stated in the doc, and Sol accepted it. However:
- The payload leg would count a best-chain payload that both sides reject as agreement (m2).
- The doc's "every field" wording overstates what a payload comparison checks (n1).

## Findings

### m1 (minor) — the lazy-key cells cannot fail on validity, and the doc misnames the owner

**Locations:**
- `node-l1/src/bsdk_decode.rs:329-337` (`lazy`), `:422-440` (`expected_payload_bytes`)
- `node-l1/tests/support/bsdk_rows.rs:478`
- `docs/node-program/BS4B-EVIDENCE.md:73,145`

**Why the judge is blind to validity.** Core serializes a lazy key's raw `vecBytes` whenever `bufValid` is set (`bls.h:446-459`). Only `Get()` clears `bufValid` (`bls.h:492-511`), and the oracle never calls `Get()`. So Core always writes the raw bytes. On base-sdk's side:
- **Key invalid:** the codec writes zeros (`lazy_bytes` = `get().to_bytes(scheme)`, base-sdk `payload/bls.rs:113`), and the judge patches the raw bytes back.
- **Key valid:** the codec writes `to_bytes(scheme)`, which equals the raw bytes.

So base-sdk's validity verdict never changes the result. The doc's line 145, "compared only through the bytes it writes", is therefore not accurate: it is not compared at all.

**Concrete failure scenario.** Suppose `protx_bls_scheme` mapped ProRegTx v1 to basic instead of legacy. Core uses legacy for v1 (`providertx.h:180`: `nVersion == ProTxVersion::LegacyBLS`, and `Get()` has no opposite-scheme fallback, `bls.h:500-507`). With that bug:
1. The "type 1 v1 field 0: valid legacy" row becomes invalid for base-sdk.
2. The codec writes zeros, and the judge patches the raw bytes back.
3. The row is `BothAccept` and the floor is satisfied.

The same holds for every lazy class, including "neither scheme".

**Today's state.** The mapping is correct: base-sdk `protx_bls_scheme(version) = from_legacy(version == 1)`, and `proupsharedregtx.rs:55` uses `AlwaysBasic`, matching Core's `legacy=false`. So this is a coverage gap, not a defect.

**Smaller point.** The "neither" bytes of the lazy rows (`wire[0] ^= 0xe0; wire[1] ^= 0x5a`) are never checked to be invalid. For eager and global fields, the enforced refusal proves it; for lazy fields, nothing does.

**Fix (doc plus two cheap asserts):**
1. In §1 and §3.1.1, say the lazy cells enforce acceptance, consumption and byte identity, not validity.
2. Name the Core-differential owner of per-version lazy validity: **EV1**'s `diff_check_special_tx` (both BLS variants, `bad-protx-operator-key`). BS6a's per-field scheme-source unit tests are its non-differential check. The base-sdk wrapper vectors cover the wrapper algorithm, not the payload's version→scheme choice.
3. In `bls_rows`, assert that base-sdk finds the lazy "neither" bytes invalid in both schemes and the lazy "valid" bytes valid in exactly the version's scheme. That turns base-sdk's verdict into a recorded fact the next re-pin can't silently change.
4. Optional, for later: an O2a flag making `oracle_payload_decode` call `pubKeyOperator.Get()` before re-serializing would make Core's validity observable. Core then writes zeros for an invalid key.

### m2 (minor) — the conformance payload leg treats "both reject" on chain data as agreement

**Location:** `conformance/src/bsdk_judges.rs:82-96`.

**Problem.** `judge_payload` returns `Ok(BothReject)` when the oracle and base-sdk both refuse. The leg only records `Err`. A best-chain payload was accepted by Core, so under either flag it must decode with `consumed_all` (Core's global and eager accept sets do not depend on the flag, `bls.h:184-203`).

**Failure scenario.** A shim or flag regression that makes Core's `GetTxPayload` path and base-sdk refuse the same historical payload shape would still be counted in `bsdk_payloads` with 0 records. The counters can't tell this apart from agreement. No such case is evident today: the census requires chain-cut seeds to be accepted under both flags.

**Fix.** In the leg, require `Ok(BothAccept)` under both flags (`consumed_all` then follows). Record anything else as `bsdk.payload` "chain payload not accepted (type N flag)". It is a one-line change and costs nothing at run time.

### m3 (minor, merge coordination) — `Target.pins` clashes with O2c's independent version; the `fuzzctl` parser accepts only BS4b's form

**Locations:**
- `node-l1/src/lib.rs:274-332`
- `node-fuzz/fuzzctl.py:142-151`
- O2c `node/o2c-oracle-p2p-decode:node-l1/src/lib.rs:185-193`

**The conflict.** O2c (`c2bebe5d`) adds the same field in a different shape: after `compare`, with named consts `BOTH` and `V24`. BS4b's `registered_pins` regex needs `name: "…",\s*pins:` immediately followed by `&Pin::ALL` or `&[…]`. Run on O2c's `lib.rs`, it parses **nothing**: `registered_names` finds 20 targets and `registered_pins` returns `{}`. Whichever branch merges second gets:
- a textual conflict in `Target` and every entry;
- if O2c's form wins, a `fuzz-check` failure for every L1 target.

This fails loudly (the gate's `fuzz-check` step), not silently.

**CS3.** `node/cs3-checks` (`284b118c`, older base) lists its 5 targets as `pins = ["v24.0"]` in `targets.toml`, with no Rust pins. Its replay credits a seed when some pin compares it, with v23 treated as out of domain. After BS4b, those entries need `pins: &[Pin::V24_0]`, or `fuzz-check` fails.

**RP-V24.** Neutral. `Pin::ALL = [V24_0]` there and `PINS = ("v24.0",)`, so the two forms coincide.

**Fix.** The manager fixes one shape before the second of BS4b/O2c merges. Either:
- make `registered_pins` independent of field order and resolve `const NAME: &[Pin] = …;`; or
- preferably, move the cross-check into a node-l1 unit test that reads `node-fuzz/targets.toml` and compares `TARGETS[*].pins` directly, with no regex.

Also record that BS4b closes BL-62 for whichever form survives.

### n1 (nit) — "every field the base-sdk object holds" overstates the payload comparison

**Location:** `BS4B-EVIDENCE.md:22`.

**What is actually compared.** For payloads, values are compared through base-sdk's own codec round trip. A transposition of two same-width fields in base-sdk's struct would be invisible, because decode and encode use the same order. Examples are ProRegTx `keyIDOwner`/`keyIDVoting` (20/20 bytes) and the commitment's `quorumHash`/`quorumVvecHash` (32/32).

**Who covers named fields.** Named-field values against Core are BS2's `getblock 2` leg (`conformance/src/verbose.rs:863+`). It covers every block with types 1–4 and 7–12, but only one block in 1000 for types 5 and 6.

**Fix.** Say so in §1.

### n2 (nit) — §4 understates what the full-history run covers

**Location:** `BS4B-EVIDENCE.md:147`.

"Whose chain samples are null" refers to the seed sources only. The full-history run judged real non-null v3/v4 commitments and v3 CbTx ChainLock signatures on both chains; examples are in answer 4.

**Fix.** State that in §4. Optionally cut one recent `getblock` into `dash-oracle-sys/tests/data` so that 7 of the 25 witness triples (commitment v3/v4 × 3 fields, CbTx v3) get natural seeds.

### n3 (nit) — the campaign receipts on disk are stale

**State.** `…/.farm/bs4b-r1/receipts/*.json` were written at 19:11 UTC with every count at 0. The unit `node-fuzz-farm-bs4b-r1` is still active (checked at 22:00 UTC). The "10.1 of 96 CPU-h" figure is not in any receipt. The campaign branch `8250ad6a` = `b368bf46` + a `targets.toml` weight commit; the targets' sources equal head's.

**Fix.** At completion, regenerate the receipts (`make-receipts.py 8250ad6a 6d509894`). Record `credited_cpu_seconds ≥ 86,400` and `divergences = crashes = sink_failures = 0` per target in STATUS before merging.

### n4 (nit) — cite BL-44 at the lazy patch

**Location:** `BS4B-EVIDENCE.md:23`.

The lazy slot hides a known representation divergence: an unchanged invalid lazy key re-encodes as zeros in base-sdk, but as its raw bytes in Core. That divergence is BL-44, which is open. Citing it in §1 lets a reader see that the judge deliberately accommodates the divergence, and that removing BL-44 later means removing the patch.

## Verdict

**APPROVE-WITH-NITS.**
- **m1 and m2:** small doc and assert changes. They can land before merge or as the first follow-up.
- **m3:** a merge-order decision for the manager across BS4b, O2c and CS3. The failure is loud, not silent.
- **Campaign:** merging still waits for its completion receipts (n3).
