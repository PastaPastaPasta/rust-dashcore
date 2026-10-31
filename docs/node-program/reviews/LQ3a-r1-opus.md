# LQ3a lock messages: Opus-h review, round 1 (full branch)

**Range:** `a5d2a9c59..49355f5ab` (`node/lq3a-locks`, 6 signed commits, 16 files)  
**Core reference:** Dash Core v24.0.0-rc.3 (`0364ed626e`)  
**Reviewer:** Opus 5.5, high effort (independent; no edits to the branch)

**Verdict: ACCEPT WITH NITS**

No consensus or crypto defect found in LQ3a's code. Every request-id, sign-hash and inventory-hash construction matches Core, and so does every `qsigrec` reject reason and its order. Each read/check scheme split matches Core's flag reads. The decoders accept exactly Core's domain at the layer LQ3a owns. All 20 implementation-only mutants I ran are killed. The findings are one API-hardening item for LK1 and three capture and test nits.

## Findings

### F1. Minor (API, LK1 handoff): the read scheme is a bare positional `BlsCtx` next to the check scheme, and isn't bound to the message

- **Where:**
  - `dash-evo/src/locks/recsig.rs:149-155`: `verify_qsigrec(params, view, read: BlsCtx, verify: BlsCtx, recovered)`.
  - `dash-evo/src/locks/mod.rs:445-451`: `verify_chainlock(params, view, read: BlsCtx, verify: BlsCtx, clsig)`.
  - `dash-evo/src/locks/wire.rs:82`: `clsig_hash(clsig, read: BlsCtx, write: BlsCtx)`.
  - `dash-evo/src/locks/mod.rs:534-540`: `verify_islock(params, view, read: BlsCtx, passes, islock)`.
- **Core:**
  - The lazy field captures the flag when it is constructed, just before the read (`bls/bls.h:409-411`, `:477-481`), and `Get()` decodes under that flag (`:499-500`).
  - The eager field decodes under the flag at `vRecv >>` (`bls/bls.h:205-209`; `net_processing.cpp:5797`).
  - The check reads the flag again when it runs (`bls/bls.cpp:311`, `:335`).
- **Problem:** both arguments have the same type and sit in adjacent positions, and nothing ties `read` to the decoded message. The decoded `RecoveredSig`, `ClSig` and `IsdLock` carry no scheme. An LK1 worker that writes the natural call `verify_qsigrec(p, v, now, now, rec)`, swaps the two arguments, or passes the queued scheme twice compiles silently. Each of these gives a wrong verdict across the V19 switch. The functions' own crossing tests can't see a caller's mistake.
- **Schedules (confirmed with a temporary test against `verify_qsigrec`; the test was removed after the run):**
  - **(a) The worker re-samples `read` (`now, now`): a ban where Core accepts.**
    1. A peer sends a `qsigrec` whose signature is a basic signature's point in the legacy encoding, named for the verified quorum that signed it.
    2. Core reads it at `V19Height - 2` under legacy, and the lazy field holds that point. V19 connects. The worker checks under basic and **accepts**.
    3. `verify_qsigrec(.., LEGACY, BASIC, ..)` gives `Accept`, as Core does.
    4. `verify_qsigrec(.., BASIC, BASIC, ..)`, and the swapped `(BASIC, LEGACY)`, give `Ban(MalformedSignature)`. The sender is banned (`BanNode`).
  - **(b) The worker passes the queued `read` twice (`read, read`): an accept where Core bans.** This is D18's false accept. A real legacy signature read under legacy and checked after V19 connects is `Ban(InvalidSignature)` in Core. `verify_qsigrec(.., LEGACY, LEGACY, ..)` gives `Accept`.
  - **ChainLock analogue:** `verify_chainlock` with swapped arguments turns D23's Core-accepted case into `InvalidSignature`, and the peer is scored (`chainlock/handler.cpp:161`).
- **Fix (before or with LK1):**
  - Capture `read` at decode and carry it with the message, for example `RecoveredSig::decode(r, read)` storing `read`, or a `Read<T> { msg: T, read: BlsCtx }` wrapper for the base-sdk `ClSig` and `IsdLock`.
  - Have the `verify_*` functions and `clsig_hash` take only the later scheme.
  - At minimum, use a named-field struct as `IslockPassSchemes` already does, for example `LockSchemes { read, check }` and `ClsigHashSchemes { read, write }`.
- **Severity:** no production caller exists yet, so this doesn't block LQ3a. LK1 should not consume the current shape as is.

### F2. Minor (test coverage): the captures can't tell `keepOldConnections` from `signingActiveQuorumCount`

- **Where:**
  - `contrib/node/lq3a-captures/capture_lq3a.py:213-218, 186-202`.
  - `dash-evo/src/locks/capture_tests.rs:145-162`.
- **Core:** `IsQuorumActive` scans `keepOldConnections` quorums (`llmq/signing.cpp:701-712`). For `llmq_test` that is 3, with `signingActiveQuorumCount` 2 (`llmq/params.h`, LLMQ_TEST).
- **Problem:** the capture mines two DKG cycles, so each type holds exactly two quorums, and both counts select the same set. The mutant `active-uses-signing-count` (`recsig.rs:116`, `keep_old_connections` → `signing_active_quorum_count`) passes every capture test. Only the synthetic `recsig_tests::only_the_newest_keep_old_connections_quorums_are_active` kills it.
- **Fix:** mine a third cycle before capturing. Have the quorum that is now third-newest sign one recovered signature, with `quorum sign 100 <id> <msg> <quorumHash>` naming it explicitly. Core then relays it as active, so the capture gets a Core-relayed `qsigrec` that only the `keepOldConnections` window accepts. Also assert `quorum list 10` has three entries per type.

### F3. Nit (capture reproducibility): `run.sh` probes `dashd -version` on the shared datadir

- **Where:** `contrib/node/lq3a-captures/run.sh:19`.
- **Problem:** `dashd -version` rewrites `~/.dashcore/settings.json`. A concurrent probe from another thread can make it exit 1 with empty stdout, and the script then refuses to run with `"" not "Dash Core version …"`. It fails closed, so the fixture can't be wrong, but a rerun can fail for no reason.
- **Fix:** run `"$bin/dashd" -datadir="$(mktemp -d)" -version`.

### F4. Nit (provenance): the scheme and the binary are asserted, not recorded

- **Where:** `capture_lq3a.py:315-335`.
- **Problem:** `bls_scheme: "basic"` is a literal. The fixture binds the run to Core only by the `-version` string. That is adequate, and the tests show the scheme indirectly, since every captured signature verifies under the basic scheme and none under legacy.
- **Fix (optional):** record `getblockchaininfo().softforks.v19` (active, height) and the sha256 of the `dashd` that ran, so a later reader can tie the fixture to the release binary without rerunning.

## Scope answers

### 1. Consensus and crypto correctness against Core

**Request ids.**
- `chainlock_request_id` is `SerializeHash(pair(string_view "clsig", int32))`, matching `chainlock/clsig.cpp:248-251`.
- `islock_request_id` is `"islock"` followed by `inputs` as a compact-size vector of outpoints, matching `instantsend/lock.cpp:18-24`.
- Both are checked against Core end to end. `capture_tests::each_lock_carries_a_recovered_sig_core_relayed` finds, for each captured `clsig` and `isdlock`, the Core-relayed `qsigrec` whose `id` equals our request id and whose `msgHash` is the block hash or txid. That `qsigrec` must also be accepted.

**Sign hash.** `sign_hash` writes the type byte, `quorumHash`, `id` and `msgHash`, matching `llmq/signhash.cpp:14-22`. LLMQType serializes as its `uint8_t` (`serialize.h:946-957`). The 12 captured recovered signatures verify under it. Dropping the type is killed by five tests, two of them capture tests.

**Inventory hashes.**
- `RecoveredSig::hash` is over the wire bytes. Core's `SER_READ(UpdateHash)` serializes the lazy field with `bufValid` and its own `bufLegacyScheme`, so the raw bytes are written back (`llmq/signing.h:90-101`; `bls/bls.h:445-464`). This holds for all-zero bytes too, which Core writes as zeros with `!objInitialized && !bufValid`.
- `isdlock_hash` is over the re-serialized fields, which equal the wire bytes: `nVersion`, `inputs`, `txid`, `cycleHash`, `sig`, with no version-dependent fields (`instantsend/lock.h:41-48`).
- `clsig_hash` reads eagerly under `read` and writes the point under `write` (`bls/bls.h:170-209`; `net_processing.cpp:5797-5798`). The invalid object writes as zeros (`bls/bls.h:133-138`).
- Every captured hash matches.

**`qsigrec` reject order.** Core's order, without node state, is:
1. the type configured, else `BanNode` (`net_signing.cpp:56-58`);
2. `IsQuorumActive`, else drop (`signing.cpp:429`);
3. `GetQuorum`, else drop (`:439-445`);
4. in the worker, `GetQuorum` and `IsQuorumActive` again (`:548-561`);
5. `sig.Get().IsValid()`, else ban (`net_signing.cpp:245`);
6. the batch check, a ban if it fails (`:257-268`).

`verify_qsigrec` takes the same steps in the same order. Dropping, reordering or recasting each step is killed by at least one test (mutants 5-11 below). I agree with D5 and D6:
- At one tip, `ScanQuorums` builds every quorum it lists through the same `GetQuorum(type, pindex)` that `GetQuorum(type, hash)` reaches, and it returns empty if any build fails (`quorumsman.cpp:248-269`, `:482-537`).
- So "active but no quorum" is a partial view, and it gives no verdict. Banning a malformed signature before the local status is read follows Core, which never consults our commitment verification.

**Scheme reads (D18, D19, D21-D24).** I checked each flag read against Core:
- the lazy field's construction-time capture (`bls/bls.h:409-411`, `:466-481`, `:492-512`);
- the eager field's read-time decode (`:183-209`);
- the check-time `VerifyInsecure` (`bls/bls.cpp:309-311`) and `VerifyInsecureAggregated` (`:335`);
- the flips at `evo/specialtxman.cpp:983-986` and `:1008-1012`.

The splits are right. Mutants that collapse each split, swap the InstantSend passes, or read the InstantSend lock under the current pass's scheme are all killed.

**Quorum selection inputs.** LQ3a adds none. `qsigrec` is a lookup by name, as Core's is. `verify_chainlock` and `verify_islock` keep SB8's selection unchanged, apart from the scheme arguments.

### 2. Decode domain

- **`qsigrec`:** `RecoveredSig::decode` takes exactly 193 bytes. It accepts any type byte (`LlmqType` has an `Unknown(u8)` variant, so `to_base` round-trips for all 256 values) and any signature bytes. It returns `None` on a short read and leaves trailing bytes. That is Core's `vRecv >> CRecoveredSig` (`signing.h:96-101`): the lazy `Unserialize` reads 96 bytes without validating them (`bls/bls.h:466-475`), and `NetSigning::ProcessMessage` never checks for leftover bytes.
- **`clsig`:** `ClSig::decode` (base-sdk) accepts signature bytes that Core's eager reader throws on. LQ3a models that layer explicitly: `SigRead::Unreadable` and `clsig_hash` → `None`. Core's eager reader classifies as follows (`bls/bls.h:100-123`, `:183-203`, `:211-220`):
  - zeros → the invalid object;
  - canonical only in the other scheme → the invalid object;
  - canonical in neither scheme, including the identity encoding → throws.

  `read_eager_sig` maps these as Core does.
- **`isdlock`:** the base-sdk codec reads any version, as Core does. The lazy read is never `Unreadable`.
- **Points:** subgroup and identity checks are BS12's `BlsGlobalSig` / `BlsLazySig` domain, which this branch doesn't change.
- **Result:** I found no byte string that decodes on one side and not on the other at the layer LQ3a owns.

### 3. Captures

- **Provenance:** `run.sh` exports `test/` from the tag and refuses binaries whose `-version` isn't exactly the tag. The fixture records `core_tag` and `dashd_version`, and `the_capture_is_cores_v24_regtest_in_the_basic_scheme` checks them. The inv-hash/payload pairing comes only from the GETDATA round trip, and nothing is hashed during the capture. Core's own `quorum verify` / `verifychainlock` confirm each `qsigrec` and `clsig`.
- **Rerun:** see "What I ran". Each run mines new quorums, so I checked reproducibility by running the unchanged capture tests against the fresh fixture.
- **Do the tests compare?** Yes. `every_inventory_hash_is_cores` compares all 18 announced hashes. `every_captured_recovered_sig_is_accepted` runs the full verdict against the listed quorums. `a_changed_recovered_sig_is_not_accepted` and `each_lock_carries_a_recovered_sig_core_relayed` cover the negatives and the lock-to-recsig link. The request-id swap, `RecoveredSig::hash` without the type, `isdlock_hash` without the cycle hash, and the sign hash without the type each fail at least one capture test. The coverage gap is F2.

### 4. API soundness for LK1, and Sol r3's nits

Sol r3's two nits need only wording. Every combination they describe is one Core can show:
- The flag flips inside `ConnectBlock` before the tip moves, and back inside the disconnect before the tip moves back (`specialtxman.cpp:983`, `:1008`).
- So (old view, new flag) and (new view, old flag) are both reachable.
- A node that samples each pass's scheme at the pass start, or derives it from the same snapshot as its view, shows an interleaving Core can also show.
- Core's eighth-push sub-batch verify (Sol r3 F1) is batch composition, which is outside the single-message API (D2).

The misuse that does silently pick the wrong scheme is positional (F1), and it needs a type change, not wording.

### 5. Security

Nothing new. The batch-aggregation boundary is the existing manager item that D2 cites, and I did not re-describe it. No private note was written.

## Mutants (implementation-only; `cargo test -p dash-evo --lib locks::` per mutant, file restored with a fresh mtime)

| # | Mutant | Result | Killing tests |
|---|---|---|---|
| 1 | `RecoveredSig::decode` swaps `id`/`msgHash` | KILLED | 5, including all 3 capture tests that decode a `qsigrec` |
| 2 | `verify_qsigrec` passes `msgHash, id` to the check | KILLED | 6, including 2 capture tests |
| 3 | lazy read under `verify` | KILLED | `a_signature_checked_across_the_v19_switch_bans` |
| 4 | check under `read` | KILLED | same |
| 5 | drop the inactive-quorum ignore | KILLED | 4, including `a_changed_recovered_sig_is_not_accepted` |
| 6 | drop the unconfigured-type ban | KILLED | 2 |
| 7 | malformed-signature ban before the active check | KILLED | 2 |
| 8 | malformed-signature ban after the status check | KILLED | `an_unreadable_signature_bans_whatever_the_quorums_status` |
| 9 | view `None` → `Ignore` | KILLED | `an_active_quorum_the_view_cannot_give_is_no_verdict` |
| 10 | active set from `signingActiveQuorumCount` | KILLED (synthetic test only; F2) | `only_the_newest_keep_old_connections_quorums_are_active` |
| 11 | unavailable active set → inactive | KILLED | `an_active_set_the_view_lacks_gives_no_verdict` |
| 12 | `RecoveredSig::hash` without the type byte | KILLED | `every_inventory_hash_is_cores`, quirk 38 |
| 13 | `clsig_hash` writes under `read` | KILLED | the M2 crossing test |
| 14 | `clsig_hash` over the raw bytes | KILLED | 2 |
| 15 | `isdlock_hash` without `cycleHash` | KILLED | 4, including `every_inventory_hash_is_cores` |
| 16 | eager invalid → `Unreadable` | KILLED | 3 |
| 17 | InstantSend passes' schemes swapped | KILLED | `an_islocks_passes_check_under_the_flag_each_reads` |
| 18 | InstantSend lock read under `passes.current` | KILLED | `an_islock_checked_across_the_v19_switch_is_invalid` |
| 19 | ChainLock read under `verify` (Sol r2's mutant) | KILLED | `a_chainlock_checked_across_the_v19_switch_is_invalid` |
| 20 | `sign_hash` without the type byte | KILLED | 5, including 2 capture tests |

## What I ran

- `git diff --check a5d2a9c59..49355f5ab`: clean. All 6 commits are signed (`%G?` = G).
- `RUSTC_WRAPPER= CARGO_BUILD_JOBS=12 CARGO_TARGET_DIR=/work/scratch/lq3a-r1-opus/target cargo test -p dash-evo` on 49355f5ab: **exit 0**. 133 unit tests, 7 `cbtx`, 31 `cbtx_root_history` (1 ignored, as expected) and 2 `sml_core_vectors`.
- `cargo test -p node-l1 --features oracle --test l1_llmq`, with the pinned oracle archive `5c9e9117a2` installed in `/work/cache/oracle`: **exit 0**, 21 passed (322 s at load ~150).
- **Mutant sweep:** 20 implementation-only mutants (table above), each through `cargo test -p dash-evo --lib locks::`. 20 killed, 0 survived, 0 compile errors. Afterwards the worktree was clean and the sources touched.
- **Capture reproducibility:** `contrib/node/lq3a-captures/run.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin /work/scratch/lq3a-r1-opus/capture …/rerun.json` with `LQ3A_PORTSEED=4711`: **exit 0**, "Tests successful".
  - The fresh fixture holds 12 `qsigrec`, 3 `isdlock` and 3 `clsig`, all `core_verify` true or null, with 2 quorums per type and tip 190.
  - Its quorums, keys and signatures are new, as expected.
  - I copied it over `dash-evo/tests/data/lq3a-regtest-captures.json` in my detached worktree only, and the **unchanged** capture tests passed 5/5 (`cargo test -p dash-evo --lib locks::capture_tests`). I then restored the committed fixture.
- **F1 scratch test:** a temporary test appended to `recsig_tests.rs` and run with `--nocapture`, then restored. Its output:
  - Core's `(LEGACY, BASIC)` → `Accept`;
  - re-sampled `(BASIC, BASIC)` → `Ban(MalformedSignature)`;
  - swapped `(BASIC, LEGACY)` → `Ban(MalformedSignature)`;
  - D18's `(LEGACY, LEGACY)` on a legacy signature → `Accept`, where Core's `(LEGACY, BASIC)` → `Ban(InvalidSignature)`.
- **Core sources read at the tag** (`git show v24.0.0-rc.3:…`):
  - `llmq/net_signing.cpp`, `llmq/signing.{h,cpp}`, `llmq/signhash.cpp` and `llmq/quorumsman.cpp` (ScanQuorums, GetQuorum);
  - `bls/bls.h`, `bls/bls.cpp` and `bls/bls_batchverifier.h`;
  - `instantsend/{lock.h,lock.cpp,net_instantsend.cpp}`;
  - `chainlock/{chainlock.h,clsig.cpp,handler.cpp}`;
  - `net_processing.cpp:5790-5818` and `evo/specialtxman.cpp:978-1014`.
- I also read the base-sdk pin 8382b76 `p2p_core/src/msg/chainlock.rs` (the `ClSig` / `IsdLock` codecs) and Sol's r1-r3 reviews.
- The detached worktree, target, capture work dir and scratch files under `/work/scratch/lq3a-r1-opus` were deleted afterwards.
