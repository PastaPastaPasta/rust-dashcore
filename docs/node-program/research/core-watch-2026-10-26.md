# Core watch 2026-10-26

First weekly watch since rc.4 went unnoticed for a week (SW-V24 D8, task R15). Source: `dashpay/dash` through `gh api` REST on 2026-10-26 (GraphQL not used); the PR file lists come from `pulls/N/files`, sorted into areas by `contrib/node/pin-delta/inventory.py`'s `classify` (consensus = consensus/validation/primitives, net = p2p, evo, llmq = signing + DKG, mn-overlay = governance/CoinJoin/sporks). Titles and PR descriptions only: **nothing here is audited**; a row says what to look at, not what it does.

The cut-off is rc.4's commit time, 2026-10-16T21:28:46Z (`0e7382df80`, tag published 2026-10-19).

## 1. Tags, release candidates and branches

| ref | state |
|---|---|
| newest v24 tag | `v24.0.0-rc.4` (published 2026-10-19; commit `0e7382df80`). **No newer release candidate, and no `v24.0.0` final.** rc.1 09-26, rc.2 09-30, rc.3 10-07 |
| newest v23 tag | `v23.1.8` (2026-08-03). No v23.1.9 |
| `v24.0.x` | `0e7382df80`, identical to rc.4: no commit since the tag |
| `v23.1.x` | `728f505583`, identical to v23.1.8 (and to `master`) |
| `develop` | `6dedab5bc7`, 472 commits ahead and 59 behind rc.4 |
| other branches | `v23.0.x` `cdc5a63e52` (not compared) |

New release candidates since rc.4: **none**. The pin (rc.4) is current. Next check: any `v24.0.0-rc.5` / `v24.0.0` tag or a commit on `v24.0.x` (a plain `git ls-remote`-style compare of that branch against `0e7382df80`).

## 2. Merged since rc.4 and touching consensus, net, evo or llmq

All on `develop` except #7917, which is rc.4's own merge into `v24.0.x` (merged 10-17, after the tag's commit time; already audited as R0). Nothing merged to `v24.0.x` after it.

| PR | merged | watched files | title | note for the program |
|---|---|---|---|---|
| #7861 | 10-18 | consensus 4, evo 1, net 1 | feat: keep at least 4-days worth blocks after pruning | Raises the minimum number of blocks kept after pruning to 576 × 4 (Core's generic prune-height arithmetic). Touches `node/blockstorage`, `validation`, `kernel/chainparams`, `rpc/blockchain`, `evo/creditpool.h`. Read with the manager when develop is next diffed against the pin. develop only |
| #7936 | 10-23 | evo 2, llmq 4 | perf: parse each quorum commitment payload once per block | New LRU of parsed commitment payloads in the quorum block processor; `CheckLLMQCommitment` takes the parsed payload; the description says the `bad-qc-payload` reason is unchanged. Touches `evo/cbtx.cpp`, `evo/specialtxman.cpp`, `llmq/blockprocessor`, `llmq/commitment`. Claimed behaviour-neutral: verify when develop becomes a pin |
| #7926 | 10-21 | consensus 3, evo 1, llmq 1, net 1 | backport: bitcoin#28858, #28737, #28530, #28224 | Bitcoin backport batch; includes `kernel/disconnected_transactions`, `evo/specialtxman.cpp`, `instantsend/signing.cpp`, `net_processing.cpp` |
| #7931 | 10-25 | net 2 | backport: bitcoin#28071, #27838, #27171, #28558 | `net_processing` only |
| #7924 | 10-21 | net 2 | backport: bitcoin#28364 | `net_processing`, `txorphanage`, logging |
| #7923 | 10-18 | consensus 1, net 1 | backport: bitcoin#26140, #27482 | `net_processing`, `validation.h` |
| #7916 | 10-18 | consensus 1, llmq 4, mn-overlay 2 | backport: bitcoin#27800, #28125, #28232, #27695 | `llmq/dkgsessionmgr`, `net_quorum`, `quorums`, `quorumsman`, `governance/net_governance`, `streams.h`, `flat-database.h` |
| #7904 | 10-18 | consensus 1, net 3 | backport: bitcoin#22764, #28066, #28186 | `blockencodings`, `net_processing`, `protocol.cpp`, `node/miner`, `index/blockfilterindex`, `dbwrapper` |
| #7918 | 10-21 | net 1 | fix(net): announce sporks once per new peer | A develop-only clean-up of a duplicated spork-announcement loop that #7893 had brought in through two merges. rc.4 already carries the final revision (`a56ed47c98a`, cherry-picked from `45acdf30f3`), and the description says the wire output is unchanged. **No action for rc.4** |

Merged on `develop` and not touching the watched areas (10): #7892 (test), #7907, #7928, #7903, #7933, #7932, #7927, #7929 (Bitcoin/GUI backports with no watched file), #7930 (build: revert sanitizer flags to libsecp256k1), #7937 (github-merge.py).

None of these is in `v24.0.x`, so none changes the rc.4 pin. They matter when the next pin comes from `develop` (v25) or if any is picked for a v24.0.x rc.5.

## 3. Open PRs touching consensus, net, evo or llmq (non-draft, 2026-10-26)

Listed for tracking only. Ones likely to be backport candidates for `v24.0.x` (fixes, recent) first.

| PR | opened | watched files | title | label |
|---|---|---|---|---|
| #7885 | 10-15 | consensus 1, llmq 4 | fix: purge cache of an overridden confirmed islock and bound the ConnectBlock conflict loop | pastaclaw:approved |
| #7835 | 10-07 | consensus 1, evo 4 | fix: defer Platform revival until block connection succeeds | needs rebase |
| #7815 | 10-07 | mn-overlay 2 | fix: bind CoinJoin signatures to the approved transaction | |
| #7819 | 10-07 | mn-overlay 4 | fix: preserve and bound orphan governance votes | |
| #7886 | 10-15 | mn-overlay 3, net 2 | fix: fetch governance orphan-vote parents through the request tracker and bound the orphan cache per masternode | |
| #7568 | 08-10 | mn-overlay 2 | fix: don't charge CoinJoin participants for failures the masternode can't attribute to them | |
| #7887 | 10-15 | consensus 3, evo 15, llmq 16, net 3 | feat: isolate Evo state per chainstate while an AssumeUTXO snapshot is active | pastaclaw:approved (91 files) |
| #7860 | 10-10 | consensus 1, mn-overlay 9, net 2 | feat: run governance on pruned nodes | |
| #7934 | 10-22 | consensus 2, evo 4, mn-overlay 2 | perf: optimize dmnlist construction and payee validation | |
| #7935 | 10-22 | consensus 1, evo 3, llmq 1 | perf: deserialize CbTx bestCLSignature lazily and avoid verifying duplicated chainlocks | |
| #7939 | 10-25 | consensus 1 | feat: optional zstd compression of block and undo files | |
| #7938 | 10-24 | consensus 3, net 2 | backport: bitcoin#29127, #29203, #28978, #27935, #26859, #28729, #29133 | |
| #7940 | 10-25 | net 6 | backport: bitcoin#28170, #29524, #28464, #28578, #28832, #28075 | |
| #7941 | 10-25 | net 1 | backport: bitcoin#28168, #28149, #25528, #28103 | |
| #7481 | 07-25 | consensus 1, llmq 1 | backport: add CMake build system alongside Autotools | |
| #5860 | 2024-02-03 | consensus 9 | feat: implement the dash analogue to BIP143 | old |
| #4974 | 2022-08-17 | evo 1 | fix: Guard mnauth by local masternode service | old |
| #7005 | 2025-11-23 | mn-overlay 1 | feat: implement exponential retention for wallet backups | old |
| #7420 | 2026-07-07 | mn-overlay 1 | test: cover CoinJoin client restart handling | |

Open and not touching the watched areas (11): #4629, #7216, #7623, #7638 (fix: exempt a MN's own ProRegTx from collateral-reuse mempool conflicts: **mempool policy, look at it with the evo rows**), #7734, #7763, #7823, #7889, #7905, #7925, #7942.

## 4. Follow-ups for the next watch

- `v24.0.x` at `0e7382df80`? Any `v24.0.0-rc.5` / `v24.0.0` tag, or a `v24.0.x` backport PR (base `v24.0.x`) opened? None open today.
- #7861 (prune retention) and #7936 (commitment-payload cache) are the develop merges closest to code we port; read them when develop is next diffed against the pin.
- The open consensus-area fixes #7885 and #7835 and the governance/CoinJoin fixes #7815, #7819, #7886 are the likeliest additions to a `v24.0.x` rc.5; re-run the sub-PR inventory (`inventory.py`, split mode) on that merge.
- Re-run: `gh api search/issues -f q='repo:dashpay/dash is:pr is:merged merged:>2026-10-26'` plus `pulls/N/files` for each hit; then the area classification above.
