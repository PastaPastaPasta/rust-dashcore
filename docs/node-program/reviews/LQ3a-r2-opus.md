# LQ3a lock messages: Opus-h review, round 2 (narrow)

**Range:** `49355f5ab..a3088713d` (`node/lq3a-locks`): e22f504d5 (D25 docs), d9cf6a106 (the fix, code head), a3088713d (D30 notes only). All three are signed (`%G?` = G), and `git diff --check` is clean.  
**Core reference:** Dash Core v24.0.0-rc.3 (`0364ed626e`)  
**Reviewer:** Opus 5.5, high effort (independent; no edits to the branch)

**Verdict: ACCEPT WITH NITS**

- D26 closes the positional half of r1 F1. The binding half is D27, which is recorded for LK1 and not built here. That matches r1's severity ("doesn't block LQ3a; LK1 should not consume the current shape as is").
- D28 closes F2: the capture now holds a Core-relayed `qsigrec` from the third-newest quorum, and the F2 mutant fails three capture tests. That holds on the committed fixture and on a fresh capture I ran.
- F3 and F4 are fixed, with nothing host-specific or private in them.
- No verdict changed relative to 49355f5ab. The code diff only renames arguments into struct fields.
- Two new nits, both about the new `both()` constructors: they make r1's schedule (a) the shortest call, and two of them are untested in the legacy direction (2 of my 11 mutants survive there).

## Findings

### F1 (r1). Partly closed. The remainder is D27, an LK1 item that isn't built

- **What changed:** named-field structs replace the positional pair:
  - `LockSchemes { read, check }`: `dash-evo/src/locks/mod.rs:448`, taken by `verify_qsigrec` (`recsig.rs:148`) and `verify_chainlock` (`mod.rs:472`);
  - `IslockSchemes { read, passes }`: `mod.rs:523`, taken by `verify_islock` (`mod.rs:583`);
  - `ClsigHashSchemes { read, write }`: `wire.rs:77`, taken by `clsig_hash` (`wire.rs:101`).
- **Swaps by position:** fixed. A swap now has to be spelled `LockSchemes { read: x, check: y }` at the call site, so it shows.
- **`Read<T>`:** not in the tree. The brief describes D27 as binding the read scheme to the decoded message, but D27 (`LQ3a-notes.md` row D27) is explicitly "LK1 item, not built here". `RecoveredSig`, `ClSig` and `IsdLock` still carry no scheme. So the brief's question "does `Read<T>` capture the scheme at the same point as Core" can only be answered for the recorded design (see Scope 1).
- **r1 schedules re-run against the new API** (a temporary test in my detached worktree, removed afterwards). All of them still compile, with r1's verdicts:

  | Schedule | Call | Verdict | Core |
  |---|---|---|---|
  | (a) a basic point in the legacy encoding, read at `V19Height - 2`, checked after V19 connects | `LockSchemes { read: LEGACY, check: BASIC }` | `Accept` | `Accept` |
  | (a) re-sampled at the check | `LockSchemes::both(BASIC)` | `Ban(MalformedSignature)` | — |
  | (a) swapped, spelled out | `LockSchemes { read: core.check, check: core.read }` | `Ban(MalformedSignature)` | — |
  | (b) a real legacy signature, read before V19 connects and checked after | `LockSchemes { read: LEGACY, check: BASIC }` | `Ban(InvalidSignature)` | `Ban(InvalidSignature)` |
  | (b) the queued read passed twice | `LockSchemes::both(LEGACY)` | `Accept` | — (D18's false accept) |

- **Status:** acceptable for LQ3a, since no production caller exists (`grep` finds none outside `dash-evo/src/locks/`). LK1 must build D27 before it calls these functions. The next item is a cheap guard until then.

### R2-1. Nit (API): `both()` makes r1's schedule (a) the shortest call

- **Where:**
  - `mod.rs:457`: `LockSchemes::both`, documented "One scheme for the read and the check: no switch between them."
  - `mod.rs:532`: `IslockSchemes::both`.
  - `wire.rs:86`: `ClsigHashSchemes::both`.
- **Problem:** an LK1 worker holding only the scheme it sampled at the check will write `verify_qsigrec(p, v, LockSchemes::both(now), &rec)`. That compiles, reads naturally, and is exactly schedule (a) above: a ban where Core accepts. Queued twice, `both(queued)` is schedule (b): an accept where Core bans. The doc states the precondition ("no switch between them"), but a worker can't know that from the scheme it has. Only a caller that sampled once for both the read and the check can claim it.
- **Fix:** in the three `both` docs, say whom it is for, for example: "for a caller that reads and checks under one sample of the flag; a queued check passes the `read` its message was decoded with (D27), never `both` of the scheme at the check." Alternatively, keep `both` and make D27's `Read<T>` the only way to supply `read`. Doc only; no verdict changes.

### R2-2. Nit (test coverage): `LockSchemes::both` and `IslockSchemes::both` are never tested on a legacy-valid message

- **Mutants that survive** `cargo test -p dash-evo --lib locks::` (43/43 pass):
  - #9: `LockSchemes::both` sets `check: BlsCtx::BASIC` instead of `bls` (`mod.rs:457-461`).
  - #10: `IslockSchemes::both` sets `read: BlsCtx::BASIC` instead of `bls` (`mod.rs:532-536`).
- **Why they survive:**
  - Every `both(LEGACY)` call in the tests reads a basic-encoded signature under legacy (`recsig_tests.rs:127`, `tests.rs:508`, and the legacy-flag branch of `capture_tests::a_changed_recovered_sig_is_not_accepted`). Those reach `MalformedSignature` or the invalid object whatever `check` is.
  - The crossing tests that do use legacy-valid messages spell their structs out.
  - `ClsigHashSchemes::both` is covered, because the crossing test uses `both(LEGACY)` on a legacy encoding (mutant #11 killed).
- **Schedule:**
  1. Apply mutant #9.
  2. A caller verifies a pre-V19 `qsigrec` carrying a real legacy signature from the named quorum, under `LockSchemes::both(LEGACY)` (regtest or devnet before V19, or a node that saw no switch).
  3. It gets `Ban(InvalidSignature)` where Core accepts, and the suite stays green.
- **Fix:** route the crossing tests' "no switch" cases through `both`:
  - `recsig_tests.rs:270` `at(&legacy, LEGACY, LEGACY)` → `LockSchemes::both(BlsCtx::LEGACY)`;
  - the matching `(LEGACY, LEGACY)` cases in `tests.rs:558` and `an_islock_checked_across_the_v19_switch_is_invalid` (`tests.rs:878`).

  Or add a one-line field-equality test per `both`.

## Scope answers

### 1. Do D26/D27 close F1? Capture points and check timing

- **Swap or re-sample:** see F1. A swap by position no longer compiles. A swap spelled out with fields, a re-sample, and a queued scheme passed twice all still compile, because nothing ties `read` to the message (D27).
- **Where `read` is sampled, per the recorded design** (D27: "LK1 captures `read` when it decodes a message"):
  - **`qsigrec`:** `NetSigning::ProcessMessage` does `make_shared<CRecoveredSig>()` and then `vRecv >> *recoveredSig` (`llmq/net_signing.cpp:50-51`). The lazy constructor samples the flag (`bls/bls.h:409-411`), `Unserialize(s)` reads under that `bufLegacyScheme` (`:477-481`), and `Get()` decodes under it (`:499-500`). Sampling at decode is the same point.
  - **`isdlock`:** the same pair at `instantsend/net_instantsend.cpp:245-246`. Same point.
  - **`clsig`:** the eager field decodes under the flag at `vRecv >> clsig` (`net_processing.cpp:5797`; `bls/bls.h:205-209`). Same point.
  - **Copies:** the lazy `operator=` copies `bufLegacyScheme` (`bls/bls.h:419-436`). A queued copy keeps the read scheme, which is what "carries it with the message" models.
- **Two LK1 notes for D27** (not findings against LQ3a):
  - A `clsig`'s `read` feeds both `ClsigHashSchemes` (`net_processing.cpp:5798`) and `LockSchemes` (`chainlock/handler.cpp:156`), so it is duplicated across two structs. A `Read<ClSig>` should supply both.
  - Core's recovered-sig DB reload (`CRecoveredSigsDb::ReadRecoveredSig`, `llmq/signing.cpp:97-112`) unserializes into an object the caller just built, so a reloaded signature takes the reload's flag, not the one it was first read under. An LK1 `Read<T>` that is persisted and reloaded must re-sample, not restore the stored `read`.
- **Check-time sampling:** unchanged. `schemes.check` is used only as the scheme argument of the final verification (`recsig.rs:181`, `mod.rs:484`), and each pass's scheme only in its own `islock_pass` call (`mod.rs:619-621`). That is Core's check-time read (`bls/bls.cpp:311`, `:335`). The functions don't sample; the caller supplies the scheme, as before. The D25 doc in `bls.rs:13-25` now says correctly that this is not `bls_ctx_after` of the visible tip.

### 2. Verdicts relative to 49355f5ab

- **Code diff:** the non-test, non-comment diff in `recsig.rs`, `mod.rs` and `wire.rs` replaces `read`/`verify`/`write`/`passes` with `schemes.read`/`schemes.check`/`schemes.write`/`schemes.passes`, and renames the islock loop variable from `verify` to `check`.
- **Untouched:** request ids, `sign_hash`, `RecoveredSig::hash`, `isdlock_hash`, the body of `clsig_hash` apart from its fields, the `qsigrec` step order (`recsig.rs:153-187`), and each InstantSend pass's scheme (`passes.current` at offset 0, `passes.previous` at the interval).
- **Behaviour check:** I ran the head's capture tests against the **49355f5ab** fixture. The four verdict and hash tests (`every_inventory_hash_is_cores`, `every_captured_recovered_sig_is_accepted`, `a_changed_recovered_sig_is_not_accepted`, `each_lock_carries_a_recovered_sig_core_relayed`) pass. Only the two tests that need the new provenance fail, as expected (`capture_tests.rs:129` for `v19_deployment`; `:183` for three quorums).
- **Mutants:** mutants #2-#8 and #11 (below) re-kill r1's scheme mutants on the new API.

### 3. D28: the oldest active quorum under keepOldConnections

- **Does Core treat that quorum as active?** Yes. The capture has the third-newest `llmq_test` quorum sign `(0x1100, 0x2100)` by naming it on each member (`capture_lq3a.py:236-245`). Recovery needs threshold 3 of 4 shares, so at least two shares cross the network. Core's `PreVerifySigShareQuorum` drops shares from a quorum that fails `IsQuorumActive` (`llmq/signing_shares.cpp:338`), and `IsQuorumActive` scans `keepOldConnections` quorums (`llmq/signing.cpp:701-712`). So the recovery itself shows that Core considers the quorum active.
- **Is it what an observer received?** Yes. The capture asserts `getrecsig` names that quorum, and that an observer's GETDATA round trip holds a `qsigrec` with those fields (`capture_lq3a.py:329-332`).
- **Does it separate the two counts?** `quorum list 10` gives exactly 3 quorums per type (asserted). With `signingActiveQuorumCount` = 2 the oldest is outside the signing window, so the capture tells `keepOldConnections` from `signingActiveQuorumCount`. It can't tell `keepOldConnections` from "every quorum", because no fourth quorum exists. The synthetic `only_the_newest_keep_old_connections_quorums_are_active` still covers that, which is fine.
- **The new test** (`capture_tests.rs:177`) is deterministic. It reads only the committed fixture, takes the oldest quorum by its index in the fixture's newest-first `quorum list`, and checks it against `provenance.keep_old_connections_recsig`, including `id` and `msg_hash`.
- **F2 mutant (`recsig.rs:116`, `keep_old_connections` → `signing_active_quorum_count`):** KILLED by `a_changed_recovered_sig_is_not_accepted`, `a_recovered_sig_of_the_oldest_active_quorum_is_accepted`, `every_captured_recovered_sig_is_accepted`, and the synthetic test. That holds on the committed fixture and on my fresh capture.

### 4. F3/F4: anything host-specific or private?

- **None.**
  - `run.sh:17-21` probes `-version` in a fresh `mktemp -d` datadir and removes it.
  - `dashd_sha256` is the sha256 of the release `dashd`: `e204e316…5bb4`, the same as `~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd`.
  - `v19_deployment` is Core's own `getdeploymentinfo` record (`{active: true, height: 1, type: buried}`).
- **Paths and hosts:** neither the committed fixture nor my rerun contains `/work`, `/home`, a user name, `localhost`, `127.0.0.1` or `.dashcore` (grep count 0). `captured_utc` is the only time-dependent field.
- **Test strength:** the test checks that the hash has the right form but not its value (noted as "not applied" in the notes). That's reasonable, because a re-capture on another machine with the same release should give the same hash anyway.

### 5. Tests and mutants

See "What I ran". `locks::` and the capture tests pass.

## Mutants (implementation-only; `cargo test -p dash-evo --lib locks::` per mutant, file restored with a fresh mtime)

| # | Mutant | Result | Killing tests |
|---|---|---|---|
| 1 | active set from `signingActiveQuorumCount` (F2, `recsig.rs:116`) | KILLED | 3 capture tests and `only_the_newest_keep_old_connections_quorums_are_active` |
| 2 | `qsigrec` lazy read under `schemes.check` | KILLED | `a_signature_checked_across_the_v19_switch_bans` |
| 3 | `qsigrec` check under `schemes.read` | KILLED | same |
| 4 | ChainLock read under `schemes.check` | KILLED | `a_chainlock_checked_across_the_v19_switch_is_invalid` |
| 5 | ChainLock check under `schemes.read` | KILLED | same |
| 6 | `clsig_hash` writes under `schemes.read` | KILLED | `a_clsig_hashed_across_the_v19_switch_writes_the_other_encoding` |
| 7 | InstantSend lock read under `schemes.passes.current` | KILLED | `an_islock_checked_across_the_v19_switch_is_invalid` |
| 8 | InstantSend passes' schemes swapped | KILLED | `an_islocks_passes_check_under_the_flag_each_reads` |
| 9 | `LockSchemes::both` check fixed to `BASIC` | **SURVIVED** | — (R2-2) |
| 10 | `IslockSchemes::both` read fixed to `BASIC` | **SURVIVED** | — (R2-2) |
| 11 | `ClsigHashSchemes::both` write fixed to `BASIC` | KILLED | `a_clsig_hashed_across_the_v19_switch_writes_the_other_encoding` |

## What I ran

- **Setup:** a detached worktree of `~/workspace/rdc-wt/integration` at a3088713d. `CARGO_TARGET_DIR=/work/scratch/lq3a-r2-opus/target` (mode 0700), reflink-seeded from the owner's `rdc-wt/lq3a-locks/target`, which was the only LQ3a target on disk; `/work/scratch/lq3a` has none. `RUSTC_WRAPPER=` and `CARGO_BUILD_JOBS=12`.
- **Full suite:** `cargo test -p dash-evo` at a3088713d: **exit 0**. 134 unit tests (one more than r1: the D28 test), 7 `cbtx`, 31 `cbtx_root_history` (1 ignored, as before) and 2 `sml_core_vectors`.
- **Commits:** a3088713d touches only `LQ3a-notes.md`; e22f504d5 touches only comments and docs.
- **Mutant sweep:** the 11 mutants above. 9 killed, 2 survived, 0 compile errors. The worktree was clean afterwards.
- **F1 scratch test:** appended to `recsig_tests.rs`, run with `--nocapture`, then restored with `git checkout` and `touch`. Its output is the table in F1.
- **Old fixture:** the head's `locks::capture_tests` against `git show 49355f5ab:dash-evo/tests/data/lq3a-regtest-captures.json`: 4 passed, and the 2 provenance and quorum-count tests failed, as expected. Restored afterwards.
- **Capture rerun:** `contrib/node/lq3a-captures/run.sh ~/workspace/dash v24.0.0-rc.3 ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin /work/scratch/lq3a-r2-opus/capture /work/scratch/lq3a-r2-opus/rerun.json`, run with `LQ3A_PORTSEED=5813` under `systemd-run --user`: **exit 0**, "Tests successful".
  - The fresh fixture holds 13 `qsigrec`, 3 `isdlock` and 3 `clsig`, all `core_verify` true or null.
  - It has 3 quorums per type at heights 192, 168 and 144, tip 214, with the oldest-quorum recsig recorded and captured.
  - It has the same `dashd_sha256` and `v19_deployment` as the committed fixture.
  - I copied it over the fixture in my worktree only. The **unchanged** capture tests passed 6/6, and mutant #1 was killed on it by the same 4 tests. I then restored the committed fixture.
- **Core sources read at the tag:**
  - `bls/bls.h` (eager `Unserialize`, lazy constructor, `operator=`, `Unserialize`, `Get`);
  - `llmq/net_signing.cpp:47-62`;
  - `llmq/signing.cpp` (`IsQuorumActive` callers, `:701-712`, `ReadRecoveredSig`);
  - `llmq/signing_shares.cpp:330-342`;
  - `instantsend/net_instantsend.cpp:237-250`;
  - `net_processing.cpp:5793-5800`.
- **Cleanup:** the detached worktree, the target and the capture work dir under `/work/scratch/lq3a-r2-opus` were deleted afterwards. The brief, the logs and `rerun.json` are kept there.
