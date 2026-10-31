# P2a rev2 — GPT closure check

**CHANGES-REQUIRED: one remaining major (G5), no blockers.** The other six major fixes are sound, subject to the small corrections below. No other major was found in the rev2 changes. This is a closure check of “Changes in rev2” and §10.3, not a full re-review.

Reviewed `designs/P2a-outbound.md` (1,737 lines; SHA256 `ba7b2ceb7b1e7ef7db248de4989209eb3dbd361e460954532e1a1a9be6743a69`), the r2 GPT findings, the reviewer contract, and DEC-55. The approved `null_locator` field and CH1 R8 need no further manager decision.

## G1–G7 dispositions

Core references below are to `src/` at the named tags. Both tags were freshly extracted from `/work/home/agent/workspace/dash`; overlapping sources also matched the previous review's extraction byte-for-byte.

| Finding | Closure and rechecked evidence |
|---|---|
| **G1** | **Algorithm fixed; minor wording remains.** Step 7 compares before hashing and stops at the mismatch; the decoder preserves `Uncompress`'s earlier work; the complete batch crosses CH1 only after continuity succeeds. `CheckHeadersAreContinuous`: v23 `net_processing.cpp:3183–3193`, v24 `:3167–3177`; decoding: v23 `:5259–5267`, v24 `:5335–5345`; both `primitives/block.cpp:120–123`. Plain mismatch probes at positions 2/1000/2000 perform 1/999/1999 hashes. See N1 for compressed/memoized counts. |
| **G2** | **Closed.** Nullable locator input, profile dispatch and the genesis-eviction session match Core. v23 `chain.cpp:32–57` substitutes the active tip; v24 `:32–54` returns an empty locator. `ConsiderEviction`: v23 `net_processing.cpp:5822–5857`, v24 `:5968–6003`. Both probes send the expected locator sizes (1/0), extend the response deadline and disconnect after the second stage. The precise v23 work-header assignment is **5828**, not the new citation's 5826. |
| **G3** | **Retained-memory contract fixed; minor formula correction remains.** Queue charge, successful handoff, transport charge and pause recomputation match v23 `net.cpp:135–141,1712–1720,1771,4837–4849` and v24 `:136–142,1702–1709,1765,4878–4890`. Transport methods at both pins retain payload/buffer capacity through partial sends; v24 anchors `:877–882,1664–1670` are correct. P1's required method is approved by DEC-55. See N2. |
| **G4** | **Closed.** One retained snapshot replaces independent loads, and R3/R4/HeadersResult use it. The cited getheaders walks and announcement membership checks share their enclosing `cs_main` scope: v23 `net_processing.cpp:4731–4809,6193,6277–6282`; v24 `:4784–4859,6338,6422–6428`. `best_header` is available through `snap.tip`; BLS context and mutable-status read semantics are now explicit. |
| **G5** | **Partially closed; major remains below.** Always running sections 6/10/11 fixes the reported eviction reset/clear and sync-start problems. The rechecked pre-expiry transitions are v23 `net_processing.cpp:5809–5830`, v24 `:5955–5976`; probes confirm reset and clearing. The replacement blanket skip of sections 7–9/12–13 is still unsafe. |
| **G6** | **Closed.** The runner now aligns address-key-translated `vRandom`, `nRandomPos` and RNG position before continuing exact streams; unaligned exchanges permit renumbering/order differences. Partial sampling: v23 `addrman.cpp:811–846`, v24 `:815–850`; pruning: v23 `:383–396`, v24 `:387–400`; `SwapRandom` updates both positions. Extracted GetAddr probes diverge before alignment and agree after alignment. |
| **G7** | **Closed.** C uses the handshake/gate/queue subset and truncates its fixture transcript before headers sync; multi_headers/multi_relay and the full window move to the joint E/F gate. Core's verack sends end with the applicable senddsq in v23 `net_processing.cpp:4178–4213`, v24 `:4210–4258`; sync sends belong to the later SendMessages scope. Genesis-only live parity removes the unavailable loader dependency. PLAN edits and the scheduling prerequisites are explicitly pending manager work. |

## Remaining major — G5: empty queues do not make the skipped seams idle

**Severity: major. Where:** `designs/P2a-outbound.md:33`, `:1401`, `:1727`.

The new predicate labels sections 7–9 and 12–13 queue-driven and skips them when announcement/inventory/request queues and in-flight blocks are empty. Section **12 starts downloads precisely when there is free capacity**, including zero blocks in flight. With P3 installed, a syncing block-serving peer can have known headers above the active tip and every listed queue empty. Core calls `FindNextBlocksToDownload`, records `BlockRequested` and sends getdata (v23 `net_processing.cpp:6664–6682`; v24 `:6822–6838`). The fast path suppresses the request and can keep suppressing it on every pass.

Section **8** also changes state without queued txs: when `m_next_inv_send_time < current_time`, it advances the trickle deadline and consumes randomness (v23 `:6454–6464`; v24 `:6599–6609`). Skipping it changes later announcement timing and RNG state once P5 is installed. M2's no-op seams cannot prove equivalence for those implementations.

**Fix:** always invoke the stateful seam sections, or explicitly restrict skipping them to the M2 no-op implementations and disable that optimization when P3/P5 are installed. Any broader fast path needs an owner-supplied no-work guarantee covering discovery, timers, flags, expiry and RNG. Add witnesses for an empty in-flight list with a downloadable block and an empty tx queue with an expired trickle timer, alongside the three existing G5 cases.

## Small corrections

**N1 — minor; `designs/P2a-outbound.md:47,889,925,929–934`.** Hash-once is authorized, but “never fewer,” equal counts on every no-PoW path, and unconditional k−1 mismatch counts are false for memoized headers2. Nine headers with an explicit wrong prev at position 5 and compressed prev fields elsewhere perform seven decoder hashes plus four continuity hashes in Core; rev2 reuses three and totals eight instead of eleven, before any PoW check. The algorithm is sound. State equality of required first evaluations/order, with authorized duplicate elimination; restrict k−1 to the plain continuity case and give the differential witness separate Core/Rust expected counts. Line 925 also retains the superseded unconditional zero-hash drop claim.

**N2 — minor; `designs/P2a-outbound.md:162,1115`.** The printed `MallocUsage(n) = ((n + 31) >> 4) << 4` omits Core's **n == 0 → 0** branch (`memusage.h:51–58`, both pins). A literal implementation charges an empty message 72 instead of 56 bytes. Add the branch and an empty-payload boundary case. Also spell out the empty transport baselines: V1 retains a 56-byte message object; V2 counts its 24-byte vector object, even after its buffer is released.

**N3 — nit; `designs/P2a-outbound.md:900,1724`.** Correct the v23 `m_work_header` assignment anchor from 5826 to **5828**. The surrounding range and behavior are correct.

The g1–g4 changes address the earlier minor requests; the corrected platformban/CoinJoin, peer-reference and MaybeSendAddr anchors were spot-checked. These do not reopen a major.

## Verification and limits

Scratch: `/work/scratch/p2a-gpt3/`. Reproducible checks:

```sh
install -d -m 700 /dev/shm/p2a-gpt3-tmp
TMPDIR=/dev/shm/p2a-gpt3-tmp python3 /work/scratch/p2a-gpt3/verify_core_contracts.py
TMPDIR=/dev/shm/p2a-gpt3-tmp python3 /work/scratch/p2a-gpt3/check_rev2.py
rmdir /dev/shm/p2a-gpt3-tmp
```

Both passed at v23.1.8 (`728f5055836c6d29806412fc7223ac8fe05af991`) and v24.0.0-rc.3 (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`). They compile extracted Core bodies/branches with instrumented collaborators using `g++ -std=c++20 -O2 -Wall -Wextra -Werror`. Checks cover prefix hash counts, compressed work, null-locator eviction stages, deadline reset/clear, partial sampling/alignment, retained V1/V2 allocations, empty tx trickle and empty-in-flight download initiation. Hashes/RNG/download discovery are collaborators, not an end-to-end node implementation; the snapshot and phase-gate conclusions are source/contract checks. No dashd instance or oracle service was changed. TMPDIR was mode 0700 and removed after use. No branch changes, commits, pushes or GitHub comments.

**Verdict: CHANGES-REQUIRED.** Close the G5 skip predicate; keep the other six fixes.
