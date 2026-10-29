# P3 block-download rev11: Sol round 11 (r10 F1 only)

**Verdict: ACCEPT WITH NITS.** Reviewed `821f7c1e0..af5bdf16b` in detached worktrees against Dash Core v24.0.0-rc.3 (`0364ed626e`). D58 closes r10 F1's mixed snapshot/cache schedule. No builds or tests were run, as requested.

## Findings

### N1 — C5′ does not bound the number of `Ahead` retries

**Design:** P3 §3.6:1440–1449, 1603–1613; C5′:487–495. **Core:** `src/validation.cpp:4831–4837, 4897–4901`; `src/net_processing.cpp:6338–6340, 6473–6496`.

**Schedule:** P has moved U(T), then V starts a sequence of tip-extending accepts C₁, C₂, … . For each Cᵢ, P finds `Ahead`, drops its cell, waits for Cᵢ's publication, and drains the resulting `BlockChecked` before retrying. That drain makes C5′'s no-outstanding-signal condition true. V can then start Cᵢ₊₁ and store its stamp before P's next stamp read. P can repeat this indefinitely without entering sections 6–12, even though it acquires its cell on every attempt. C5′ gives V a drain between accepts; it does not reserve the cell or give P one successful region. Core's `SendMessages` holds `cs_main` for the whole region once it acquires it. This is a liveness qualification, not a finite-schedule counterexample to D58's frame ordering: progress needs a gap between accepts or a fairness mechanism if sustained accepts must be tolerated. State that assumption or specify a handoff if a bound is intended.

### N2 — The S1 pre-read seam alone is not a rev10 regression test

**Design:** P3 §6.1:2142–2151; S4-x (10):2240. **Core:** `src/net_processing.cpp:2228–2247, 6338–6340, 6480–6496`.

**Schedule:** P holds the cell and pauses *before* reading the stamp; V stores C's stamp and sets `recent = C`; P resumes. Both rev10 and rev11 read C's stamp and return `Ahead`, so this S1 seam passes with the old order. The S4-x (10) *after-read* pause distinguishes them: in rev10, let V finish the cache write before P's separate copy, giving the old locator from A, a fresh nonce for T, then V's `cmpctblock(C)`; in rev11, P already copied T before its stamp read, giving the cached T with no nonce draw. Make the after-read test's V-cache-write barrier explicit so it reliably fails on the rev10 order. Core's `cs_main` around sections 6–12 excludes the mixed result.

## F1 disposition

V stores the stamp before it updates `recent` and takes any peer cell (P3:613–630; Core `src/net_processing.cpp:2232–2264`). If P's copy sees C, V's `recent` mutex unlock synchronizes with P's lock, so V's earlier stamp store happens before P's later load. Atomic write-read coherence then forces that load to see C's stamp or a later one. **Relaxed store and load are sufficient** under this mutex ordering; the design states the required mutex and coherence rule at P3:1390–1399, though it does not name an atomic ordering. If P copies T and reads C's stamp, it retries after publication. If it copies T and reads the old stamp, V has not visited P and must wait on P's cell, so P's old-S region is Core's region-first schedule: locator from A, cached compact T with no new nonce, then C's announcement. A newer stamp with an old copy also retries. These are the two Core outcomes at `src/net_processing.cpp:6338–6383, 6473–6496` and `src/validation.cpp:4831–4832`.

The other pre-cell write in this callback is `highest_fast_announce` (P3:613–616; Core `src/net_processing.cpp:2234–2236`), but the head copies only the hash, block and compact encoding. `m_best_header` can change earlier in header acceptance (Core `src/validation.cpp:4701`; `src/node/blockstorage.cpp:110–115`), but the head reads its fixed published ChainView S, not V's live tree; C7(a) places the next publication after the callback. `best_header_sent` is written under P's cell, and the relay list is appended later through the separate `block_inv` cell. I found no other V pre-cell value read live by sections 6–12 in this F1 schedule.

The two changed files contain D58's ordering fix, its S1/S4-x (10) tests and citations, and the DEC-240/D55/D56 notes. I found no unrelated change in the delta.
