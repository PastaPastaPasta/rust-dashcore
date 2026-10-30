# P2a-core, round 1 independent review

**Candidate:** `node/p2a-core-leg` at `fd893ef3cf853edcc816f61b90b89a1380377e62`, compared with `cc940c818`. The candidate worktree was not changed. I used the detached checkout at `/work/scratch/p2a-core-r1-sol/src`. **Reference:** Dash Core `v24.0.0-rc.3`; the local oracle branch changes none of the cited message-processing paths. The review follows `REVIEW-COMMON.md` and does not inspect private findings.

## Findings

### R1-B-1 — Blocker: the required fuzz gate is not certified at this head

**Location:** `node-fuzz/targets.toml:340-425`; `docs/node-program/DECISIONS.md:197`; campaign receipts at `/work/scratch/p2a-core/campaign/receipts/diff_p2p_*.txt:1-3`.

**Failure scenario:** A merge decision treats the five `DEC-184` plateau receipts as clean certification of the P2a-core phase E targets. Those receipts are from source heads `809251543` and `95f4ccbc`, oracle `66c4e6472f1c`, and fuzz archive `f36e2b59…`; the candidate is `fd893ef3c` with oracle `d4c16888b45a` and fuzz archive `514cae…`. They report 0 divergences, but each reports 2–5 `slow_units` under `crashes_incl_slow`. They are explicitly plateau stops, not a merged-corpus confirmation run. DEC-186 supersedes DEC-184's certification rule: the core gate needs a passing `fuzzctl certify` at the gated head for its six targets (the five single-command targets plus `diff_p2p_multi_handshake`), with the before/after replays, 3600 process CPU seconds, execution floor, zero findings and a valid final verdict. No such certificate was provided; this checkout's `fuzzctl.py` has no `certify` command yet. The two other registered multi targets are the later joint E/F gate, so this finding does not require their certification before the core-only merge.

**Core cite:** Not applicable to the evidence gate. The controlling sources are P2a §7.2 C/E, PLAN.md:604 and DEC-186.

**Fix:** Integrate the FZ1 certification command, triage the slow units, and supply passing candidate-head certificates for all six core-gated targets with the pinned oracle. Preserve the old plateau receipts as campaign history, not as the merge verdict.

### R1-M-1 — Major: the required cross-peer +C probes are absent

**Location:** `node-l1/tests/p2p_node.rs:215-250`; `node-l1/src/p2p.rs:320-355`; `dashd-p2p/tests/sim_sync.rs:370`.

**Failure scenario:** Peer A sends a valid header H, then peer B sends a one-header batch whose parent is H, or a block `inv(H)` in the same pass window. A processor that wrongly handles B first would ask B for missing headers or increment `unconnecting_headers`, yet the current tests still pass: the hand-written header test sends the good headers only from peer 0 after peer 1's unrelated bad headers and ends comparison when block download starts; the sim inv test uses one peer; the multi-header generator supplies arbitrary payload bytes of at most 255 bytes and has no structured chain construction for the 32-byte parent link. D38 says the +C probes remain in `sim_sync` and the differential sessions, but neither contains the specified connecting-header and block-inv probes. This is a phase E merge-gate requirement, not the later joint E/F relay gate.

**Core cite:** `net.cpp:3676-3714` shuffles peers each pass; `net_processing.cpp:3322-3358` distinguishes connecting from unconnecting headers; `net_processing.cpp:4531-4545,4582-4608` chooses the block-inv `getheaders` path. P2a §3.12 (+C) and §7.2 E specify both probes.

**Fix:** Add both valid-PoW regtest probes to `sim`, asserting the Core-visible outcomes for both shuffle orders; also run them through the Core-vs-node session leg or seed a structured multi-header target that can generate their parent-linked messages. Keep the session below P3's first block request so the comparison reaches peer B.

### R1-M-2 — Major: the registered multi-relay differential can pass with no address relay

**Location:** `node-l1/src/p2p_node.rs:10-18,376-384`; `node-l1/src/p2p.rs:406-409`; `dashd-p2p/src/processing/seams.rs:388-412`; `node-fuzz/targets.toml:362-370`.

**Failure scenario:** `diff_p2p_multi_relay` is registered as a live, `dashd-p2p`-owned target and its entry point says it compares address relay across peers. The current node leg installs `NoAddressRelay`, whose relay, send and `getaddr` methods are empty. The comparator discards every outbound `addr`/`addrv2` frame and omits `m_addrs_to_send`, `m_addr_processed`, the RNG draw log and AddrMan call log; the generator creates only eager readers. Thus a no-op relay can agree with Core on all compared fields while Core actually forwards an address and the node does not. D49 discloses the exclusions, but a passing target verdict cannot certify `RelayAddress` at the later joint E/F gate.

**Core cite:** `net_processing.cpp:2716-2734` selects relay peers, `:4388-4482` processes address intake, and `:6186-6252` sends queued addresses. P2a §7.3a requires `RelayAddress` in the joint gate.

**Fix:** Before using this target for the joint gate, install P2a-net's relay, feed both legs the same relay randomness, and compare address frames and queued relay state. Until then, label its result as intake/flag coverage only and exclude it from relay certification. This does not claim P2a-net must land in the core-only branch.

### R1-m-1 — Minor: the full-section `SendMessages` twin test is missing

**Location:** `dashd-p2p/tests/sim_m2.rs:632-692`; `dashd-p2p/src/processing/logic.rs:748-805`.

**Failure scenario:** The tests exercise sections separately and combine only sections 12–13 for a `getdata` batch. A pass with pending announcement, inv relay, timeout/eviction and object requests is not compared with Core as one section-complete pass. An accidental early return or reorder conditional on two pending kinds of work can escape the isolated tests. P2a §7.2 E explicitly names the full-section twin test.

**Core cite:** `net_processing.cpp:6300-6898`, `PeerManagerImpl::SendMessages`, including its early return before handshake and its section order.

**Fix:** Add a Core-vs-node pass with several sections active at once and assert the complete outbound sequence and state changes after that pass.

### R1-m-2 — Minor: the 1000-item batching test follows the implementation constant

**Location:** `dashd-p2p/tests/sim_m2.rs:635-658`; `dashd-p2p/src/processing/seams.rs:167,199-209`.

**Failure scenario:** A scratch mutation changing `MAX_GETDATA_SZ` from 1000 to 1001 left `getdata_requests_are_batched_at_1000` green, because its input and expected output both use `MAX_GETDATA_SZ`. A separate mutation that delayed the actual flush was caught. The current code has the correct 1000 boundary; this is a test blind spot.

**Core cite:** `net_processing.cpp:6874-6878` flushes at `MAX_GETDATA_SZ` (1000).

**Fix:** Pin literal boundary cases at 999, 1000 and 1001 entries, including the shared block-plus-object batch.

### R1-n-1 — Nit: D6's inline-deviation count is stale

**Location:** `docs/node-program/designs/P2a-core-notes.md:12` (D6).

The note says four inline rows deviate, then correctly lists five: `addr`, `addrv2`, `inv`, `getdata`, `getaddr`. `registry.rs:443-451` correctly tests all five. Change the prose count to five. **Core cite:** none; documentation arithmetic only.

## Checks and limits

- `cargo test --locked -p dashd-p2p --all-features`: 20 unit, 38 M2 sim and 32 sync tests passed. `cargo test --locked -p node-l1 --all-features --test p2p_node`: 9 passed. None of the four formerly ignored P2a tests remains ignored.
- `cargo fmt --all -- --check`, `cargo clippy --locked -p dashd-p2p --all-targets --all-features -- -D warnings`, and `python3 -I node-fuzz/fuzzctl.py check` passed. `git diff --check` reports trailing spaces on transcript records with an empty payload; those spaces are part of the current text fixture syntax.
- Four scratch mutants on initial sync, header-response rearming, getdata flush and discouragement threshold were rejected by targeted tests. The 1000-to-1001 constant mutant survived as R1-m-2 describes. All scratch source mutations were restored.
- The 74 rows match Core's `protocol.cpp:99-175` order; inline, unlisted and duplicate registration panic. The registry marks its 15 non-inline and five inline interim behaviours as deviations and names the closing task. The actor's per-pass shuffle, work flag, 100 ms masternode throttle, `ProcessGetData` sequencing, `SendMessages` sections and pre-handshake return, and the headers hash-once path match the reviewed Core paths. The misbehaviour cell is a leaf lock; I found no demonstrated state-lock/send-queue/wake cycle. D3/D59 explicitly defer the real N6 `ChainViewHandle` adapter to P2a-node; this review did not treat the local sim mirror as an integrated ChainView test.
- P2P-16's old v23 adjusted-clock skew variant is superseded by DEC-82's v24-only profile. The V24 node-clock use was checked; no v23 skew finding is raised.

CHANGES-REQUIRED
