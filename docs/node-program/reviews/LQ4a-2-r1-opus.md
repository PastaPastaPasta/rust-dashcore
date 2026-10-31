# LQ4a-2 code review, round 1 (Opus-h)

**Target:** `node/lq4a-2` at `698019ad6` (frozen worktree `rdc-wt/lq4a-2`), diff `23c7cf126..698019ad6` (f11dd92e5 + 698019ad6). Core reference v24.0.0-rc.4 (`49caf372bb`); `a/` = `src/active/`, `l/` = `src/llmq/`.

**Verdict: ACCEPT-WITH-NITS**

I found no divergence from Core in phases 3 and 4. The complaint targets, both bitsets, the signed hash and the field order, the justify set, the revealed shares, the check order, MarkBadMember timing and the spork23 fold all match rc.4 line for line (table below). The replays are strong: byte-exact complaints and justifications plus `validMembers` against Core's own pcommits would catch a wrong valid-member set, a wrong complaint target or a reordered payload field. Two receive-side outputs that Core makes are not pinned by any test, and two mutants survive (m-1, m-2). Neither changes a verdict today.

Counts: 0 major, 2 minor, 3 nit.

## What was compared against Core

| Area | Rust | Core | Result |
|---|---|---|---|
| `VerifyAndComplain` order (non-member exit, pending drain, no-contribution → `MarkBadMember`, fold, send) | `session/complain.rs:21-44` | `a/dkgsession.cpp:185-224` | equal |
| fold + else-if chain + independent metadata checks; first-node version; MN-only relay/filters; self skipped; spork gate | `conn.rs:104-147`, `complain.rs:48-64` | `a/dkgsession.cpp:226-290`, `l/options.cpp:26-47` | equal (Core's `badConnection = fShould…` is an assignment, but the function runs once per session, so `|=` is equivalent) |
| `SendComplaint`: `bad ∥ badConnection` → badMembers, else `weComplain` → complainFor; nothing → no message; bitsets sized `params.size` | `complain.rs:68-98` | `a/dkgsession.cpp:292-332` | equal (members < size is exercised by the 4-of-5 simplepose sessions, byte-exact) |
| complaint wire: `type, quorumHash, proTxHash, DYNBITSET bad, DYNBITSET complainFor, sig`; sign hash = SerializeHash with default (zero) sig | `wire.rs` `write_complaint_without_sig`, `hash_with` | `l/dkgmessages.h:87-104`, `serialize.h` DYNBITSET (LSB-first) | equal |
| justification wire: prefix, compact count, `(u32 LE index, 32-byte key)*`, sig | `wire.rs` `write_justification_without_sig` | `l/dkgmessages.h:113-135` | equal |
| `ReceiveMessage(qcomplaint)`: loop to `members.size()`, votes/complaintsFromOthers keyed by complainer, `someoneComplain`, debug `complaintsFromMembers`, no bad-sender gate | `complain.rs:109-150` | `l/dkgsession.cpp:318-354` | equal |
| `VerifyAndJustify`: bad skip → votes ≥ threshold → empty → `!= 1` → complainFor[myIdx] | `justify.rs:27-68` | `a/dkgsession.cpp:334-374` | equal |
| `SendJustification`: member order, true share, `JUSTIFY_LIE` per `i ≠ me` then key, then `JUSTIFY_OMIT`, sign | `justify.rs:73-111` | `a/dkgsession.cpp:376-422` | equal (draw order pinned by `the_lie_draws_precede_the_omit_draw_and_skip_ourselves` under a strict `ReplayRng`) |
| `ReceiveMessage(qjustify)`: preamble → bad → every unasked entry marks bad (loop continues) → all shares verified against the justifier's vvec → per entry: false marks bad and the loop **continues**; true for us stores, clears `weComplain`, writes `qdkg_S`; true erases the complainer | `justify.rs:124-186` | `l/dkgsession.cpp:422-491` | equal |
| missing vvec → share false | `session.rs` `verify_justification_shares` default | `bls/bls_worker.cpp:721-733` (rc.4 null check) | equal |
| preamble (non-member, ≥ 2 no relay, set emplace, record, debug bit, 2nd → bad + relay unprocessed) shared by all three kinds | `contrib.rs` `receive_preamble` | `l/dkgsession.cpp:688-755` | equal |
| `VerifyAndCommit`'s marking | `justify.rs:192-221` | `a/dkgsession.cpp:424-466` | equal, except the member gate (n-2) |
| determinism | new code: no `HashMap`/`HashSet`, clock, float or global RNG; every draw goes through `env.rng` | — | clean |

Phase 3 and 4 make no draw except `COMPLAIN_LIE` (receive side, LQ4a-1) and the two justify hooks, as in Core.

## Findings

### m-1. The relayed inv kind of complaints and justifications is never asserted

**Rust:** `dash-llmq/src/dkg/session/contrib.rs:297` and `:301` (`receive_preamble`'s kind per phase). **Core:** `l/dkgsession.cpp:700-716` (`MSG_QUORUM_COMPLAINT` / `MSG_QUORUM_JUSTIFICATION`), relayed at `l/net_dkg.cpp:323-354`.

**Mutant (run, survived 117/117):** a complaint's `DkgInv` carries `DkgMsgKind::Justification`.

**Scenario:** DK1 maps `DkgInv.kind` to the inv type it relays. Under the mutant, every complaint goes out as `inv MSG_QUORUM_JUSTIFICATION <complaint hash>`. A Core peer asks for a justification by that hash, `GetJustification` misses, and the complaint never spreads by relay. Votes then reach `dkgBadVotesThreshold` on fewer nodes than in Core. `replay.rs:216` compares only `inv.hash`, and the only kind assertion (`batch/tests.rs:118`) covers contributions.

**Fix:** in `replay.rs` `feed`, assert `r.inv.kind == kind` for every relay. That pins all three kinds on every captured message.

### m-2. A later true share for us after an earlier false one is not pinned

**Rust:** `dash-llmq/src/dkg/session/justify.rs:166-181`. **Core:** `l/dkgsession.cpp:462-480`. After `MarkBadMember` for a false share, Core's loop keeps going. A later true share whose complainer is us still sets `receivedSkContributions`, clears `weComplain` and calls `WriteVerifiedSkContribution`, even though the justifier is now bad.

The port does this correctly. The problem is that nothing tests it.

**Mutant (run, survived 117/117):** skip the store/`weComplain` block once `member_state[idx].bad`.

**Scenario:** a justification `[(2, bad share), (0 = us, true share)]`. Core writes a `qdkg_S` row for the justifier and clears `weComplain`. The mutant writes nothing. Today's verdicts are the same, since the justifier is bad. But the dkgdb write stream is a T6 parity item (§4.3), and `weComplain` feeds the debug status. `a_false_share_marks_the_justifier_bad` puts the false share on *our* index and the true one on member 2, so it only pins the erase (mutant `e`, killed).

**Fix:** add the reversed order to that test: the false share for member 2 first, then the true share for us. Assert one `StoreRow::Sk` for member 1, `!we_complain(1)`, `is_bad(1)`, and that only member 2's complaint stays open.

### n-1. The uncaptured complaint is checked by its counts, not its bitsets

**Rust:** `replay.rs` `check` (the `logged` branch) for spork23, type 102, `01ab70c6…`, member 2.

I checked the fixture. The session has 3 members, the log says `badCount=2, complaintCount=0`, and the fold logs `badConnection=1` for both other members. The replay asserts that per member, but on the *session state*: it doesn't check the bitset built from that state.

So this run pins *which* members the session holds bad. It does not pin how `send_complaint` encodes them. A mutant that writes a bad bit at a shifted index keeps the counts and every per-member assertion, and on this run alone it passes. (Sol's n-1 says the same.) Today the 18 byte-exact captured complaints catch such a mutant, because every complaint goes through the same encoder.

Beyond the encoding, equal counts don't mean equal sets in general. Here the fold happens to fix the state completely. In another fixture, a member that is bad for having no contribution, or any `weComplain` bit, would be pinned by count alone.

**Fix:** derive the expected bitsets for an uncaptured complaint independently: `badMembers` = (members with no captured contribution) ∪ (logged `badConnection`), and complainFor = ∅ here. Assert them on `run.complaint` through `bad_member`/`complain_for`, and keep the counts as a cross-check against debug.log.

### n-2. `mark_open_complaints` has no member gate

**Rust:** `justify.rs:192`. **Core:** `a/dkgsession.cpp:426-428` (`if (!AreWeMember()) return std::nullopt;`).

It is test-only today, and every caller is a member session. When LQ4b-1 builds `verify_and_commit` on it, return before marking when `self.me` is `None`. Otherwise an observer marks open-complaint members bad and logs lines Core never logs.

### n-3. No Core capture exercises the fold's middle branches

The simplepose capture hits only "not connected" (spork21 on and off) and "outbound failed" (`8e5597b3`, member 3). The min-proto, no-relay and no-filters branches (`conn.rs:131-137`) are covered by the unit truth table and by `trim_lq4a2.py`'s `faults()`, but that is the same reading of Core written twice, not a Core result. Mutant `d` (`<` → `≤` at 70242) is killed by unit tests only.

That is enough for this slice. Record the gap in PROVENANCE, or add it to the LQ4b-1 rotation capture: one node with `-peerblockfilters=0` or an old-protocol peer would give Core-logged rows for those branches.

## Mutants run

The mutants were env-gated in my reflink copy (`/work/scratch/lq4a2-rv-opus`, one build, `RVMUT=x`; script `/work/scratch/lq4a2-rv-opus-tools/mutate.py`). Each ran over the full lib suite minus `honest_llmq_100_67`.

| # | Mutant | Result |
|---|---|---|
| a | complaint inv kind = Justification | **survived** (m-1) |
| b | no `qdkg_S`/`weComplain` clear once the justifier is bad | **survived** (m-2) |
| c | complainFor also set for a member voted bad (`else if` → `if`) | killed (1) |
| d | min proto `<` → `≤` | killed (10, incl. simplepose replay) |
| e | no complaint erase once the justifier is bad | killed (1) |
| f | bad votes counted one short | killed (3, incl. simplepose replay) |
| g | `JUSTIFY_LIE` drawn for our own index too | killed (draw-order test) |
| h | drop the first `bad` → relay-only return in receive(qjustify) | survived, **equivalent**: the second check at `justify.rs:157` returns before any state change. The only difference is the extra "didn't complain" log lines for an already-bad justifier, which Core never prints (`l/dkgsession.cpp:431-435`). No action |
| i | platform ban skipped after "not connected" | killed (1) |

I also traced these by reading against the replays, without running them. Each is caught by a byte-exact or `validMembers` replay assertion:
- `bad_connection` dropped from badMembers;
- the bitsets sized `n` instead of `params.size`;
- the bitset fields swapped;
- self not skipped in the fold;
- `complain_for` read as `bad_member` for the justify set;
- `JUSTIFY_OMIT` drawn before the lie loop;
- `write_sk` for every true share.

## Other checks
- **Re-delivered equal session hash (m6 (d)) on qjustify:** processing it again makes the justifier bad, because its complaints were already erased. Core does the same. It can't happen with valid fields: the BS9a eager read zeroes only non-canonical bytes, and PreVerify then bans them. No action.
- **Phase order:** complaints and justifications arriving before our own `verify_and_*` (Core processes the next phase's queue during its pre-phase sleep) aren't gated by `stage`, as in Core. The replays always feed after the start function. Outcomes don't depend on that order, only the log interleaving does.
- **Non-member:** a member's vvec that was never received gives false (rc.4 `bls_worker.cpp:725-733`). That is tested.

## What I ran
- Read the rc.4 sources (`a/dkgsession.cpp:185-467`, `l/dkgsession.cpp:266-492, 632-772`, `l/dkgmessages.h`, `l/options.cpp`, `bls/bls_worker.cpp:686-760`, `l/utils.cpp:753-777`) against the full diff, plus the design §0–§4 and the slice row.
- Spot-checked `test-vectors/lq4/` (PROVENANCE, `trim_lq4a2.py`'s fold/`sent` checks, and a dump of every simplepose session and fold). The benches only change the `payload(flag)` call.
- `df -BG /work`: 243G free. `cp -a --reflink=always` to `/work/scratch/lq4a2-rv-opus` (TMPDIR inside, `CARGO_BUILD_JOBS=8`, nice 19).
- `cargo test -p dash-llmq --lib --all-features` at 698019ad6: **118 passed**, 0 failed (408 s at load ~60).
- The 9 mutants above.
- The copy is left **mutated** (env-gated, inert without `RVMUT`), along with its target, for the manager.

## Agreement with Sol r1

Sol (ACCEPT-WITH-NITS, 0/0/1) also found no phase-3/4 divergence from rc.4. Their only finding is the same as my n-1. I agree with it, and their scenario (a mutant that moves one `badMembers` bit while keeping the count) is right for that run taken alone, so I sharpened my n-1 to match. My m-1 and m-2 (surviving mutants on the relayed inv kind, and on the later-true-share store write) and n-2/n-3 are not in Sol's review. Sol ran no mutants. Both of us got 118/118 on our own copies.
