# The build's mainnet windows (EV2b D29, D32)

`mainnet-dml-1028160-1030159.bin`: mainnet's first 2,000 DIP3 blocks, 1,028,160..1,030,159, after
the parent 1,028,159, in `dash_dml::testkit::window`'s format: the headers and every special
transaction as mined, every other transaction after the coinbase as its input count plus the
prevouts that spend a masternode's collateral (3 in this window). 1,522,416 bytes, SHA-256
`4c24f5b3869a3f428d6c5a4cc7c69c48734b89d5c99a4c11db5b1a17b817a03f`.

Written from the cache of node-l1's L2 window (`node-l1/scripts/dml-window-fetch.py 1028159 1078159
blocks.gz`, `getblockhash` and `getblock <hash> 0` through the oracle):

```bash
zcat blocks.gz | head -2001 | cargo run -p dash-dml --release --example dml_window_fixture -- \
  2000 mainnet-dml-1028160-1030159.bin digests.txt
```

The generator checks that the window and the real blocks give the same `CanonicalMNListHash` at
every height, Core's in the L2 window's `digests.txt`, and that the reference twin agrees at the
end. The last list's hash,
`2e6ac0121845496697ee5d80baa5fa30d38d2f833fa8fe04aee7353f620e6000`, is Core's at 1,030,159 (node-l1's
window, EV2b D26); `benches/dml_build.rs` and `tests/windows.rs` check it.

`mainnet-dml-1371028-1373027.bin` (`dmlwin02`, EV2b D32): the 2,000 blocks 1,371,028..1,373,027 from
the list after 1,371,027 (the largest of EV2a's sampled mainnet lists below the oracle's 2,090,000
ceiling; 5,131 masternodes at the end), with the headers back to the oldest quorum base and the
members of its 94 non-null quorums as proTxHashes. 3,058,672 bytes, SHA-256
`7e6424b9fdbe8712930c63ffc35aa8ba3126928a4fde4ce6adefd8c98a352068`.

The start list is `protx listdiff 1 1371027` re-encoded as evodb stores it (internal ids and the
registration counter derived), and each quorum's members are CalculateQuorum over the list at its
base block, both from pure reads (`node-l1/scripts/dml-w2-fetch.py`). node-l1's window replays the
blocks from that list through Core's `RebuildListFromBlock` and ours, and writes Core's digests:

```bash
python3 -I node-l1/scripts/dml-w2-fetch.py 1371027 2000 window.gz
zcat window.gz | cargo run --release -p node-l1 --features oracle --example dml_window -- digests.txt
zcat window.gz | cargo run -p dash-dml --release --example dml_window_fixture -- \
  2000 mainnet-dml-1371028-1373027.bin digests.txt
```

The last list's hash, `c65d020d929c74508afdba3c5fcedaba1ff5bc16488313c5199c010e35e6539e`, is Core's
from that start list, not mainnet's own (whose list has the real ids and counter).

## The block processor's mining block (LQ2 D18)

`mainnet-qc-2089770.bin` (`lq2qcb01`): mainnet 2,089,770, which mines all 32 LLMQ_60_75
commitments of the cycle at 2,089,728: the headers 2,088,856..2,089,770, the 1,920 members of the
32 quorums (Core's disk form, `CalculateQuorum` by Core's context), and the 32 commitments as
mined, all from the Core-produced lq1-rot capture (`test-vectors/lq1-rot/mainnet-2089728.json.gz`,
its PROVENANCE.md). 728,117 bytes, SHA-256
`ff2716b32bc112aa250c6dedae5de28176865ea1621c017948e2e0ebf7d5c01b`.

```bash
python3 -I dash-dml/benches/data/make_qc_block.py \
  test-vectors/lq1-rot/mainnet-2089728.json.gz 2089770 dash-dml/benches/data/mainnet-qc-2089770.bin
```

`benches/qc_block.rs` checks that the block connects with a row per commitment before timing.
