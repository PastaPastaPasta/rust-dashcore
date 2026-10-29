# LQ4a-1 code review, round 2 (Opus)

**Target:** `node/lq4a-1` at `b129bc537` (frozen; worktree checked clean at that head), diff `ba2d46fcf..b129bc537`. Core reference v24.0.0-rc.4 (`49caf372bb`); Core paths below are read from that tag (`a/` = `src/active/`, `l/` = `src/llmq/`).

**Scope:** where Sol r1 was thin. Covered here: API fitness for LQ4a-2, LQ4b and DK1; determinism and purity; test strength by mutation; PF0 bench methodology. Sol's confirmed areas (state machine, indexing, draw and encryption order, receive-side complaint decisions, 32-trigger, batch grouping, wire layout, hashes) were re-read only where a finding touches them.

**Verdict: ACCEPT-WITH-NITS**

No correctness defect against Core was found in the slice. The minors are test gaps that mutants expose (m-1 to m-3), a bench whose gated whole-process counters are mostly setup (m-4), and three API points that LQ4a-2 or DK1 will otherwise trip over (m-5 to m-7). None blocks the merge. m-6 should be fixed before LQ4a-2 dispatches the other three kinds; the rest can go in LQ4a-2.

Counts: 0 major, 7 minor, 2 nit.

---

## Findings

### m-1. The duplicate-sign-hash revert is not pinned by any test

**Rust:** `dash-llmq/src/dkg/session/batch.rs:247` (`if !seen.insert(*msg.signed_hash())` → `revert_to_single`). **Core:** `l/net_dkg.cpp:262-272`.

**Mutant (m7):** the condition is made never true, so a repeated sign hash goes into the aggregate like any other. **All 59 unit tests pass.** (The vectors and harness tests were not run on this mutant; their honest inputs never repeat a sign hash.)

**Why no test catches it:** `a_repeated_sign_hash_is_checked_message_by_message` (`batch/tests.rs:170`) sends identical payloads. `the_same_pro_tx_on_two_connections_is_two_senders` sends one honest and one forged signature over the same hash. In both cases the aggregate path and the per-message path reach the same verdicts. The revert changes the outcome only when the aggregate check passes while a per-message check fails, and no fixture builds that case.

**Scenario:** a later refactor drops or reorders the `seen` check (for example, moving it after `pks.push`, or keying it by session hash). Every test stays green while the batch's verdicts drift from Core's on exactly the input class Core's comment at :263-268 describes.

**Fix:** add a `Fake`-based `batch_verify_message_sigs` test with two messages from different senders over one sign hash. Give them two valid-point signatures that each fail alone but whose sum is the honest signature (honest + d and −d, built through the pkc G2 add). Assert that both senders are bad. Keep the existing identical-payload test as well.

### m-2. The all-same-sender shortcut is tested only on fully uniform or immediately mixed batches

**Rust:** `batch.rs:262-266`. **Core:** `l/net_dkg.cpp:289-298` (`std::adjacent_find` over **all** preverified messages).

**Mutant (m10):** take the shortcut when only `messages[0]` and `messages[1]` share a sender. **All 59 unit tests pass.**

**Scenario:** with a batch `[Peer 9 good, Peer 9 good, Peer 10 forged]`, the aggregate fails. Core finds mixed senders, falls back per message, and scores only 10. A shortcut test that looks at a prefix (or at first/last) scores 9, skips both of 9's valid contributions and receives 10's forgery. `a_bad_sender_loses_every_message_it_sent_in_the_batch` uses `[9 good, 9 forged, 10 good]`, where both conditions give the same answer.

**Fix:** add that batch shape and assert `misbehaving == [BadSignature{10}]`, two relays for 9's messages, and 10's message not received.

(Mutant m9, which disables the shortcut entirely, also survives. It is equivalent, since with distinct sign hashes an aggregate of valid signatures cannot fail, so it needs no test.)

### m-3. A member session's equal-session-hash re-delivery (double push) is untested, and so is the `we_complain` arm of the pending filter

**Rust:** `contrib.rs:179` (push), `contrib.rs:213` (`if state.bad || state.we_complain`). **Core:** `l/dkgsession.cpp:234` and `:260` (a second `WriteVerifiedVvecContribution` and a second `emplace_back` on re-processing), `a/dkgsession.cpp:138`.

**Mutant (m3):** drop the `we_complain` arm of the filter. **All 59 unit tests pass.** Within LQ4a-1's paths the arm is reachable only by re-processing an equal session hash on a **member** session: the first delivery pushes `idx`, and the second processing draws `COMPLAIN_LIE` and sets `we_complain` while the first push is still pending. The m6 (d) test `an_equal_session_hash_in_other_bytes_is_processed_again` (`batch/tests.rs:265`) runs as an observer (`member_session(.., None, ..)` at :297), so it never pushes.

**Scenario:** the m6 (d) behaviour on a member is the one that writes rows. Core writes `qdkg_V` twice, pushes `idx` twice (so the duplicate counts toward the 32-trigger), and then writes `qdkg_E`/`qdkg_S` twice or skips the member, depending on the flags. The Rust code does the same today, but nothing pins it. LQ4a-2 adds `verify_and_complain`, which drains this list.

**Fix:** a member variant of the m6 (d) test with a re-delivery whose blobs decrypt. Assert `pending == [i, i]`, two `qdkg_V` rows, and two `qdkg_E` + `qdkg_S` rows after `verify_pending_contributions`. Then add a second variant: `SimErrorRates` complain-lie at 100 on the second processing only (or a `ReplayRng` with `Bool(ComplainLie, false)`, then `true`), and assert that the filter skips the member, with no `qdkg_E` row for it.

### m-4. The verify_pending PF0 rows are bit-stable, but their gated whole-process counters are about 96% fixture and setup

**Rust:** `benches/dkg.rs:283-295` (`primed()`, run per iteration as `iter_batched_ref` setup at :323), `:253-275` and `:300-301` (the `Pending` LazyLock: 32 production `contribute`s at the full quorum size, plus a primed session and a full trigger run as a self-check).

**Measured** in the scratch copy with the perfgate criterion shim patched in (`PERFGATE_MODE=counters`) and `perf stat -e instructions:u` around the process, two repeats each:

| row | `instructions` (region), per iter | `instructions_total` @1 iter | per extra iter (total) | process @1 iter |
|---|---|---|---|---|
| contribute/50_60 | 122.93 M | 202.1 M | 122.9 M | 202.4 M |
| contribute/400_60 | 1,047.4 M | 1,680.7 M | 1,048 M | 1,681.1 M |
| verify_pending/50_60 | 264.66 M | 7,437.9 M | 1,713 M | 7,438.2 M |
| verify_pending/400_60 | 1,983.3 M | 57,031.7 M | 11,434 M | 57,032.0 M |

Repeats agree to within about 20 instructions in the region and about 3k in the totals, so **stability is not the problem**: the rows will be bit-stable apart from the usual malloc-layout swing. The problems are what the gated counters measure, and what a row costs:
- At 400_60 the region is 3.5% of the authoritative `instructions_process`, and of `instructions_total` (gated at a 1% floor). Each iteration's setup (31 receives in `primed()`) is about 9.45 G, roughly 305 M per receive, and the fixture is about 45.6 G. At 50_60 the region is 3.6%.
- So a 2–3% change in `contribute` or in the decode/PreVerify/decrypt path fails `verify-pending-*` on `instructions_total` and `instructions_process`, which misattributes it to the verify. A change confined to the 32-entry verify has to exceed about 28% before it moves those two counters past their floor. Only the in-region `instructions` counter sees it.
- Cost: each verify_pending/400_60 process retires about 57 G instructions (seconds of CPU per run), counters and strace passes alike, 5 runs per side, plus 10 more per side on escalation. That is several minutes of PF0 queue time for one row.
- For the record: the region is the whole 32nd receive. At 50_60 about 18% of it is the single message's read, PreVerify, signature check and decrypt; at 400_60 about 15%. DEC-637 accepted that. The row name and the workloads.toml comment say so, which is fine.

**Fix (needs a new bench-change approval string, since DEC-637's was verbatim):**
1. Build the primed session once in the LazyLock and clone it per iteration. `DkgSession<E: Clone>` can derive `Clone`, since `QuorumMembers` is already an `Arc` slice. That cuts per-iteration setup to a copy.
2. Drop the self-check trigger run from `Pending::new`, or keep it under `debug_assertions` only.

The fixed 32 × `contribute` stays (about 33 G at 400), which still leaves `instructions_total` dominated by setup. Alternatively, record in workloads.toml that for these two rows only `instructions` is meaningful and the totals are fixture-dominated. Doing neither and anchoring as is will work, but the first `contribute` optimisation will look like a verify regression.

### m-5. `init` drops Core's "not enough members" line on refusal

**Rust:** `session.rs:374-378`: the line is logged into the session, and the session is then dropped in the `Err` path, so `take_events` can never see it. **Core:** `l/dkgsession.cpp:131` (the line is batched into `Init`'s logger and flushed when `Init` returns false), followed by `a/dkgsessionhandler.cpp:98-101` (`LogPrintf "quorum initialization failed …"`).

**Scenario:** DK1 reproduces Core's debug.log for an undersized session, such as a regtest quorum after dropouts or a type with too few MNs. The session's own record is lost, and DK1 has to re-format Core's text from `InitRefusal { have, min }`. That breaks the "events carry Core's text" contract at the one place it can't be recovered from the session. `too_few_members_refuse_before_any_member_work` (`session/tests.rs:10`) doesn't check the event.

**Fix:** return the events with the refusal, either as `InitRefusal::NotEnoughMembers { have, min, events: Vec<DkgEvent> }` or as `init(..) -> (Result<Self, InitRefusal>, Vec<DkgEvent>)`, and assert the line in the test.

### m-6. Three of the four PreVerify functions log into whichever record is current

**Rust:** `preverify.rs:175`. Only `pre_verify_contribution` calls `begin_record()`; `pre_verify_complaint` (:203), `pre_verify_justification` (:230) and `pre_verify_premature_commitment` (:267) do not. **Core:** every `PreVerifyMessage` overload opens its own `CDKGLogger` (`l/dkgsession.cpp:160`, :270, :359, :496).

**Scenario:** LQ4a-2 wires complaints into `process_batch`. A complaint from another quorum is rejected with "complaint for wrong quorum, rejecting", and that line gets the record id of the previous message's `ReceiveMessage` (records are sticky; `receive_contribution` never restores). The records are then not contiguous, and DK1 would batch the line into the wrong `QuorumDKG(…)` write. `assert_records_and_relays` (`batch/tests.rs:41`) would catch this only once the kind is dispatched, and today these functions are dead outside tests.

**Fix:** move `self.begin_record()` into `pre_verify_sender` (the common entry of all four), or into one dispatching wrapper. Add a unit assertion that each PreVerify rejection line has a fresh record id.

### m-7. `Contribution::payload()` freezes the read-time scheme; Core re-serializes under the scheme at serve time

**Rust:** `wire.rs:204` (`payload(&self)` writes the bytes "as the read left them"). DK1 serves GETDATA from `contribution(hash)` + `payload()` (the doc comment at :203 says so). **Core:** `CDKGSession::GetContribution` copies the stored object (`l/dkgsession.cpp:651-658`), and `ds << qc` writes each `CBLSWrapper` with `bls::bls_legacy_scheme.load()` at that moment (`src/bls/bls.h:178-181`).

**Scenario:** the m11 flag straddle that the design's T3 and T6-D cover: base before V19, with the tip crossing V19 inside Initialized..Contribute, the GETDATA window for qcontrib. A contribution received under the legacy flag is served by Core under the basic flag, so the vvec, ephemeral key and signature bytes differ. Rust serves the legacy bytes, and the peer's raw-hash seen-set and its own read then differ from Core's. On mainnet and testnet this is unreachable, since V19 is long active; on regtest and devnet it is reachable, and the design tests it.

**Fix:** `payload(&self, flag: BlsSchemeKind)`, re-encoding the BLS fields from the stored values, with `EnqueueOwn` and GETDATA both passing `env.bls.scheme()` of their moment. Alternatively, document in `payload` that DK1 must re-encode for serving, and give DK1 the entry point to do it. Add one test: read under legacy, serve under basic, and assert that the bytes equal a basic-flag serialization of the same values.

### n-1. `Misbehaviour` carries no event position, unlike `Relayed`

**Rust:** `batch.rs:57-62` (`Relayed { inv, at }`) and `batch.rs:67-87` (`Misbehaviour` has no `at`). **Core:** each `PeerMisbehaving` call follows a specific `LogPrint` (`l/net_dkg.cpp:405-408`, :414-416, :427-430), and net_processing's own "Misbehaving: peer=…" line lands right after it.

DK1 can recover the position today by matching each entry to the k-th "failed to deserialize / structure check", "banning node" or "failed to verify signature" line with `peer != -1`. That is text matching in the driver. Add `at: usize` (the number of `events` before the score), as `Relayed` has, so DK1 interleaves scores and relays from one shape.

### n-2. The deferred LLMQ-category `Init` lines need documented splice points

**Rust:** `session.rs:370-392`. **Core:** `l/dkgsession.cpp:121-128` (`DKGComposition …`, rotated types only, the **first** line of Init's record, before "not enough members") and `:137-145` (`forMember[…] relayMembers[…]`, immediately before "initialized as member").

Deferring these to DK1 (DEC-637) is sound: DK1 has the cycle base height, the members and `relay_members()`. But both lines sit **inside** record 0, between session-produced lines, and nothing in the API says where. Add a sentence to `DkgEvent`'s or `init`'s doc naming the two insertion points. Alternatively, have `init` take a flag for `LogAcceptDebug(BCLog::LLMQ)` and emit them itself; it already has every input except the cycle base height.

---

## Assessments with no finding

- **The event shape and DK1's per-relay log and network behaviour.** `Relayed.at` is right, including the nested case. The 32-trigger's `VerifyPendingContributions` record is rotated ahead of its caller's lines (`contrib.rs:180-187`), which matches the `CBatchedLogger` flush-on-destroy order (`src/batchedlogger.cpp:19-30`). `RelayInvToParticipants`' own record belongs after the receive record, at `at`, and DK1 has every input it prints: `relay_members()`, `my_index()`/`members()` for `ProTx()`, and `params()`/`quorum_index()`/`base()` for the header. Network effects are ordered correctly within a batch: every score precedes every relay, as in Core. With n-1 and m-5, the shape is sufficient.
- **Deferrals.** `verified_contributions` going to LQ4b-1 is sound: DK1 implements `DkgStore` only after LQ4b-1 (m5 dependency), so the added method is not a breaking change for a live implementor. Moving the intake checks to DK1 is sound: design §0 lists them as non-goals, and BS9a's framing walk and `MaxDKGMessageSize` live in base-sdk, which DK1 calls directly (`l/net_dkg.cpp:524-640` is all connection- and chain-state work). The Core LLMQ-category debug lines are covered by n-2.
- **Determinism.** No `HashMap`/`HashSet` (clippy.toml bans them). `members_map`, `contributions` and the per-member sets are `BTree*` and are only looked up, counted or emplaced. `relay_members` is a `BTreeSet<ProTxHash>`, whose derived byte order equals `uint256::Compare`'s memcmp (`src/uint256.h:55`), so DK1 prints it in Core's `std::set` order. Bad senders come out in `Sender` order, while Core's are in `unordered_set` order, which is unspecified; T6 has to compare `BadSignature` entries as a multiset (the doc at `batch.rs:225` already says so).
- **Floats.** There are exactly the two sites the design names, each with an `#[allow]` and a reason. `sim.rs` `get_rand_bool` matches `GetRandBool` (`src/random.cpp:653-661`: `uint64 * double`, then the comparison promotes `r`) with `percent / 100.0` matching `rate / 100` (`rpc/quorums.cpp:894`). `phase.rs` `sleep_before_phase_ms` matches `a/dkgsessionhandler.cpp:193-197` (an int64 product, then double `/ size`, `* factor`, `* idx`, truncation). Neither uses libm or FMA.
- **RNG seam.** `CoreRng::below` uses modulo-zone rejection where Core's `randrange` uses bit-mask rejection. Both are uniform, and production draws need no stream parity, so this is fine. Replay is what parity needs, and Sol's n-1/n-2 cover its tag checks.
- **Mutants that the unit tests catch** (each killed by at least one test outside `vectors::` and `reference::network`):
  - 32 → 33 trigger (4 tests);
  - no nested-record rotation (2);
  - filter without `bad` (2);
  - clear on length mismatch (1);
  - bad sender's messages not skipped (5);
  - complain-lie also for own (1);
  - decrypt failure not complained (2);
  - the preamble's ≥ 2 drop → ≥ 3 (2);
  - mark-bad at > 2 (3);
  - own share not pushed (8);
  - aggregate result ignored (5);
  - relay `at` before the receive (2);
  - `qdkg_S` written for invalid shares (1);
  - contribution-lie also for own (2);
  - a lie overwriting the true share (1);
  - `qdkg_E` written for filtered members (2);
  - a PreVerify ban not scored (1).

  Survivors: m7 (m-1), m10 (m-2), m3 (m-3), and m9 (equivalent).

---

## What I ran

All in `/work/scratch/lq4a1-rv-opus2` (a `cp -a --reflink=always` of the worktree), with `CARGO_BUILD_JOBS=12` and `TMPDIR=/dev/shm/lq4a1-rv-opus2-tmp`. The frozen worktree was not touched; `git status` is clean at `b129bc537`.

- `cargo test -p dash-llmq --all-features` on the unmodified copy: **73 + 1 passed, 0 failed** (lib in 383 s under box load; wire_firewall 1; doctests 0).
- 21 env-gated mutants (`A6MUT=n`, one build), each run on the lib test binary with `--skip vectors:: --skip reference::network`. Results are above; logs are `/work/scratch/lq4a1-rv-opus2-mut/m*.log` and `summary.txt`.
- The `benches/dkg.rs` build with perfgate's criterion shim (`contrib/node/perfgate/criterion-shim/v0.8`) patched in via `--config patch.crates-io.criterion.path=…`. Each row ran under `PERFGATE_MODE=counters` with `PERFGATE_ITERS` ∈ {1, 3}, twice, inside `perf stat -x, -e instructions:u`. Outputs are `/work/scratch/lq4a1-rv-opus2-mut/bench-*.json(.perf)`. The mutant switch was compiled into this bench but inactive; it adds an env read per contribution receive, which is negligible against the BLS work but means these numbers are not PF0 anchors.
- Core rc.4 sources were read from the tag (`git show 49caf372bb:…`) into `/work/scratch/lq4a1-rv-opus2-core/`.

Left for the manager:
- `/work/scratch/lq4a1-rv-opus2`: mutated sources in `dash-llmq/src/dkg/session{.rs,/batch.rs,/contrib.rs}`, a `Cargo.lock` re-pointed at the shim, and a 100 G reflinked target;
- `/work/scratch/lq4a1-rv-opus2-mut`;
- `/work/scratch/lq4a1-rv-opus2-core`;
- `/dev/shm/lq4a1-rv-opus2-tmp`.

Clippy, fmt, layers and purity were not re-run; Sol r1 ran them on the same head.
