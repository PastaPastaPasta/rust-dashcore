# Provenance of `test-vectors/lq1-rot/`

LQ1 §6.4's rotated-quorum captures (node program LQ1b): one LLMQ_60_75 cycle per era and
network, kept as shared vectors for the node (`node-l1`), `dash-mnlist` (SB11) and dash-spv
(SB15). Each holds what a node reads to compute the cycle's 32 quorums, the rows of the three
previous cycles, and what Dash Core computed from them. Nothing is hand-written.

| File | Era (LQ1 §6.3.2 item 4) |
|---|---|
| `mainnet-1737792.json.gz` | DIP0024 activation: the first rotated cycle, no previous row |
| `mainnet-1738656.json.gz` | the first cycle with all three previous rows |
| `mainnet-1899072.json.gz` | V19 (`skipRemovedMNs` flips on mainnet) |
| `mainnet-1987776.json.gz` | the first V20 cycle (work block below V20: the block-hash modifier) |
| `mainnet-1988064.json.gz` | the first cycle whose work block is past V20 (the CbTx ChainLock modifier) |
| `mainnet-2089728.json.gz` | the last cycle within the mainnet oracle bound (2,090,000; MN_RR is above it) |
| `testnet-769824.json.gz` | DIP0024 activation |
| `testnet-838368.json.gz` | not an LQ1 era: Core's early return (28e), 7 valid masternodes at the work block, fewer than a quarter (15), so no row and no new quarters |
| `testnet-850176.json.gz` | V19 (testnet skips removed masternodes from DIP0024 on: Core's testnet clause) |
| `testnet-905184.json.gz` | the first V20 cycle |
| `testnet-1067040.json.gz` | MN_RR |
| `testnet-1572480.json.gz` | the last cycle whose mining window the testnet oracle held at collection (tip 1,572,663) |

## Inputs (pure reads of the shared v24.0.0-rc.3 reference dashds)

`dash-oracle-sys/scripts/collect-llmq-rotation.py` reads, one request at a time, only
`getblockhash`, `getblockheader`, `getblock` (verbosity 2) and `protx listdiff` (LQ1 §6.3.6):

- the headers from the first rotated cycle's base − 3 × 288 − 8 to the last cycle's mining window;
- the list after every cycle's work block (base − 8), from `protx listdiff` applied cycle to
  cycle and checked against a full `protx listdiff 1 <h>` at the first cycle, every 8th cycle,
  the last cycle, the four work blocks of every capture cycle and wherever an operator key
  changed (a list is kept only once the next full read confirms it; on a mismatch the unconfirmed
  lists are read in full); re-encoded the way evodb stores it (`dml-listdiff-check.py`'s
  `encode_dmn`, the encoding the O2a-LQ1 samples use), except that internal ids are fixed per
  proTxHash across the whole collection, the way Core assigns them at registration. Rotation puts
  entries of several lists into one scratch list, and Core's `AddMN` refuses a duplicate internal
  id (an exception the rotation code swallows), so per-list positional ids drop masternodes.
  A list's head carries the number of ids the collection had assigned when it was encoded in
  place of Core's `nTotalRegisteredCount`, which the rotation never reads;
- every work block's coinbase transaction, and the non-null LLMQ_60_75 commitments mined in each
  cycle's mining window (`gen-llmq-regtest.py`'s `mined_commitments`).

## What Core computed

node-l1's `rotated_history_matches_core` (`node-l1/tests/l1_llmq_history.rs`, ignored; run with
`LQ1B_L2_DIR` and `LQ1B_L2_OUT`) replays every cycle from DIP0024 activation through Core's
LLMQ context (the oracle shim at `Pin::V24_0`, O2a-LQ1): per cycle a context loaded with the
headers, the lists and coinbases of the cycle's and the previous three cycles' work blocks and the
rows Core stored for those cycles, then `GetAllQuorumMembers` for all 32 indexes. It records the
row Core stores (`StoreSnapshotForBlock`, from the context's trace) and the members, and requires
on the way that the node's model gives the same members, trace and row (or, like Core, stores
none), and that every mined commitment verifies with Core's members, signatures included
(`CFinalCommitment::Verify`).

`dash-oracle-sys/scripts/gen-llmq-rotation-captures.py` packages the capture cycles from the
collection and the replay output: `rows` and `row` are Core's, `quorums[].members` (RPC-order
proTxHashes) are Core's, the commitments are the mined ones. The checked-in test
`rotated_captures_match_core` recomputes everything from the capture with Core and the model.

## Reproduce

```
python3 -I dash-oracle-sys/scripts/collect-llmq-rotation.py --out DIR --net mainnet   # and testnet
LQ1B_L2_DIR=DIR/mainnet LQ1B_L2_OUT=OUT cargo test --release -p node-l1 --features oracle \
  --test l1_llmq_history rotated_history_matches_core -- --ignored     # and DIR/testnet
python3 -I dash-oracle-sys/scripts/gen-llmq-rotation-captures.py --collection DIR --replay OUT \
  --out test-vectors/lq1-rot
```

## This collection (2026-10-15, Core v24.0.0-rc.3 reference nodes, pin d4c16888b4 shim)

| | mainnet | testnet |
|---|---|---|
| headers | 1,736,920 – 2,089,788 (every read ≤ 2,090,000) | 768,952 – 1,572,540 (tip 1,572,663) |
| cycles collected (incl. the three before DIP0024) | 1,226 | 2,791 |
| masternodes (internal ids) | 6,923 | 1,075 |
| full-read checks that found a diff-applied list wrong | 22 (30 lists re-read) | 18 (43 lists re-read) |
| cycles replayed through Core and the model | 1,223 (1,737,792 – 2,089,728) | 2,788 (769,824 – 1,572,480) |
| quorums, members and trace equal | 39,136 | 89,216 |
| rows equal (`rows_digest`) | 1,223 (`32d91779c9c97cebeff72133ca07e690290a687bfd7a37b6c22ac6deed7a5558`) | 2,787 (`624d5634ca1ac7b5f260d8d7a81832fb68738e187eef9b4ca01a8862b32f539c`); none at 838,368, as in Core |
| mined commitments verified with Core's members (signatures included) | 38,598, 0 rejected | 86,532, 0 rejected |

The capture `testnet-838368` was chosen after the collection, when the replay met Core's early
return there; the four lists it reads were then re-read in full and compared byte for byte
(registered count masked), all equal. The captures embed Core's members and rows, which
`rotated_captures_match_core` recomputes with Core and the model.
