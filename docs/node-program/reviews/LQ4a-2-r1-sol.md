# LQ4a-2 review (Sol, round 1)

Verdict: **ACCEPT-WITH-NITS**

I found no phase-3/4 implementation divergence from Dash Core v24.0.0-rc.4 in the reviewed paths. Complaint construction and receive handling match `src/active/dkgsession.cpp:185-332` and `src/llmq/dkgsession.cpp:268-355`; justification construction and verification match `src/active/dkgsession.cpp:334-422` and `src/llmq/dkgsession.cpp:357-492`; the connection fold matches `src/active/dkgsession.cpp:226-290`; and wire field order/hash behavior matches `src/llmq/dkgmessages.h:87-145`.

## Findings

### n-1 — The uncaptured simple-PoSe complaint does not verify its target bitset

- **Location:** `dash-llmq/src/dkg/session/vectors/replay.rs:300-320`, especially `:311-315`.
- **Core reference:** `src/active/dkgsession.cpp:292-332` (the complaint carries indexed `badMembers` and `complainForMembers` bits); the capture's debug count is emitted by `:307-319`.
- **Scenario:** The type-102/member-2 simple-PoSe run is the one complaint absent from the trimmed capture. The replay checks only the debug-log counts `[2, 0]` and deliberately skips the payload comparison. A mutant that moves one of the two `badMembers` bits to a different member while preserving the count passes this test (and the fold checks), even though it votes on the wrong node.
- **Fix:** Add an independently derived expected target set for this uncaptured run (for example, reconstruct it from the recorded fold/member metadata and assert the complete complaint payload), or retain the full Core complaint bytes for this one message. Keep the count assertion as a secondary check.

Counts: M = 0, m = 0, n = 1.

## What I ran

- Read the approved LQ4 design sections 1–4 and the LQ4a-2 slice.
- Compared `git diff 23c7cf126..698019ad6` in the frozen worktree with Core `v24.0.0-rc.4` (`49caf372bb`) for complaint, justification, receive, connection/PoSe, and wire serialization paths.
- Checked `/work` capacity before copying (`243G` free), then tested a reflink scratch copy at `/work/scratch/lq4a2-rv-sol`.
- `TMPDIR=/work/scratch/lq4a2-rv-sol/.tmp CARGO_BUILD_JOBS=8 nice -n 19 cargo test -p dash-llmq --lib --all-features` — **118 passed**.
- `git diff --check 23c7cf126 698019ad6` — clean.
