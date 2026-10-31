# P3 block-download design, independent round-1 review

Revision: node/p3-design 2b7a6ef2e. Reference: Dash Core v24.0.0-rc.3. This is a design review; P3 implementation and its proposed tests do not yet exist. Validation below is source comparison. A targeted existing dashcore compact-count test was attempted in an isolated target but stopped during a cold dependency build; it did not produce a test result.

## Findings

### M1 — Major — D5's empty-mempool comparison omits Core's extra-transaction ring

Design: P3-block-download.md:850-864, 1402-1405; P3-notes.md D5. Core: net_processing.cpp:1922-1931, 4997-4999, 5016-5020; blockencodings.cpp:141-170; net_processing.cpp:5191-5203.

A Core mempool can be empty while vExtraTxnForCompact is nonempty. For example, send a child transaction before its parent. Core accepts the child into the orphanage and calls AddToCompactExtraTransactions, without putting it in the mempool. Then announce a compact block containing parent and child. Core's InitData can fill the child's short-ID slot from the extra ring and sends getblocktxn for the parent only. P3's NoMempool plus permanently empty ring requests both indexes. The peer can distinguish the messages immediately; this does not require a statistical inference. Rejected small transactions give another way to populate the ring while the mempool remains empty.

Fix: state the interim equivalence as “empty mempool and empty extra ring,” record the concrete wire deviation, and test the orphan-then-compact sequence. If P3 must meet the UA parity gate before P5, P3 needs the Core extra-ring insertion paths or the gate must explicitly wait for P5.

### M2 — Major — the synchronous-signal queue has no memory bound or producer back-pressure

Design: P3-block-download.md:375-403, 975-1001, 1358. Core: net_processing.cpp:2228-2268 and 2314-2343; validation.cpp:3432, 3796-3800, 4831-4832, 4891.

The bounded(1) channel bounds only doorbell tokens. SyncSignal itself is stored in a Mutex<Vec<...>> with no cap. If the network actor is delayed in a section-7 disk read or compact reconstruction while V connects a stored fork for another origin, each connected block can enqueue BlockChecked; NewPoWValidBlock also retains an Arc<Block>. The 1024 request window and SB6 receive flood limit do not bound signals from V or another origin. Core invokes both callbacks inline, so the producer cannot accumulate a separate backlog.

Fix: specify and test a lossless bound on pending signal bytes and how V yields at safe points when that bound is reached, preserving C1 and signal order. If a finite bound is already implied by the host, prove it against reorg and RPC/reindex paths and include it in the memory budget. The S1 wait test alone does not cover this.

### M3 — Major — the Core compact differential is optional at the point parity is claimed

Design: P3-block-download.md:1249-1255, 1359-1363, 1398-1401; P3-notes.md D7. Core: blockencodings.cpp:88-110; net_processing.cpp:5174-5189.

S3 can ship ShortIdIndex and the compact client on units, sim ports, one transcript and a fuzz smoke; S5 may leave both Core differentials parked with smoke = 0 and still recommend P3 READY. Those tests can all agree with the same mistaken bucket table or InitData rule. A crafted compact block can then make Core return READ_STATUS_FAILED and request a full block while P3 sends getblocktxn, or the reverse. The design itself names diff_cmpct_initdata as the check that holds the libstdc++ rule to Core.

The proposed fallback when GCC 15 and Guix bucket tables differ also cannot serve as a differential gate: pinning Guix while comparing against a GCC-15-built oracle makes a correctly pinned port disagree with that oracle on constructed bucket cases.

Fix: make E4 and a compact-handler Core differential required before compact-client parity/READY is claimed, or replace them with release-toolchain Core fixtures covering every branch and a documented deferred parity gate. Build the E4 oracle with the reference Guix libstdc++ if the two tables differ; keep the GCC 15 probe diagnostic only.

### m4 — Minor — the bucket sweep stops below Core's accepted wire maximum

Design: P3-block-download.md:255-271, 1253-1255, 1377-1379. Core: blockencodings.h:116-123; blockencodings.cpp:55-59, 88-110.

The proposed n = 1..20,000 probe misses accepted compact messages with 20,001..65,535 total transactions. Core's decoder rejects only BlockTxCount > 65,535, and InitData's block-size bound can allow counts in the untested range. A mistaken prime-table entry there changes the bucket >12 decision and the fallback wire message.

Fix: sweep the full accepted count range and prime-policy transition points against the release toolchain; include crafted 13-per-bucket inputs near transitions.

### m5 — Minor — “no extra RTT during IBD” is false for direct fetch

Design: P3-block-download.md:850-859; Core: net_processing.cpp:1428-1431, 3247-3255, 5052-5055.

CanDirectFetch checks tip time, not IBD state. HeadersDirectFetchBlocks can request MSG_CMPCT_BLOCK with a recent tip and a valid parent even while IsInitialBlockDownload remains true for another reason. With NoMempool, that path can need getblocktxn during IBD. This is a performance and test-coverage error, not a different porting rule.

Fix: include an IBD=true direct-fetch compact case in S2-e/S3, and qualify the latency claim.

### m6 — Minor — the merkle-checked handoff is missing from the stated API

Design: P3-block-download.md:333, 1097-1102; Core: validation.cpp:4326, 4362, 4434-4456.

Section 5 says BlockSubmit carries a “merkle checked” flag to V, but the BlockSubmit definition has no such field. With the declared API, the pre-V IsBlockMutated pass and V's CheckBlock pass both compute the merkle root. That is the performance deviation the text later calls temporary, and it affects the stated 2% budget on large blocks.

Fix: add the flag to BlockSubmit and make A-4b a measured S1/S6 gate, or remove the claim that the handoff exists and budget for the repeated pass.

## Checks and disposition

I compared the named scheduling, in-flight, stall CAS/decay, floating-point timeout, direct-fetch, compact-client, HB-list, ProcessBlock-tail and announcement branches with the v24 source. I found no separate ordering defect in D1/D2 provided C1-C4 are enforced by the real CH1-node actor. The two-phase wait correctly drops the peer lock before draining BlockChecked and polls the reply before sleeping. The 6,500 testnet stop is below the known 7,000 readiness guard; the full-tip and unmodified Python FT runs remain explicitly late-closing. D12 was excluded as requested.

The design needs M1-M3 resolved before the stated parity and back-pressure claims are accepted. The minor items should be fixed in the implementation plan and tests.

VERDICT: ACCEPT-WITH-CHANGES
