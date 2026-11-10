# Provenance of the O6 scan's real-chain fixtures

Every block here is a mainnet block read from the shared oracle's block files
(`/work/chains/mainnet/data/blocks/blk<file>.dat`, Dash Core v24.0.0-rc.3 oracle, no
`xor.dat`) by `dash-conformance scan`, and copied unedited: the record's block bytes, after its
8-byte magic and length, as lowercase hex. `record offset` is the offset of the record's magic in the
file. The transaction fixtures are the first transaction of the block, cut at its serialized
length. Used by `tests/scan_real.rs`.

## Scan (b): pre-DIP3 coinbases with type bytes (quirks 2 and 10, owner CS3)

The coinbase's 32-bit version field is `0x20000000` on the wire (`00000020`), which parses as
nVersion 0 with nType `0x2000`. Each has 1 input and 2 outputs, and none has a one-byte
coinbase scriptSig. They are before DIP3 (mainnet 1,028,160) and the block is on the best chain
of the oracle at tip 2,094,670.

| Height | Block hash | txid (coinbase) | blk file | record offset | Block bytes |
|---|---|---|---|---|---|
| 842284 | `000000000000000cdf5cc24c3beb0669b31e942d1301e07b53d6f0c7db10860d` | `53fa8159168128ed8c0d31c2c2b6466f32ca27a69ea6fb114c1655bdfc734ac6` | `blk00034.dat` | 128058112 | 16356 |
| 844394 | `000000000000002ee530aa44afb452e7ad03c1ef2c5dc1c935b67a7f2c8d3a7a` | `c811fe2d7b02261e1e0f48f731549d7cef891a27a4595253a44308e58d6ba6b2` | `blk00035.dat` | 20882128 | 261 |
| 866404 | `0000000000000019e35731a0bd438de286810bc597e48fdd2ff1699db8e0574b` | `5a78111e6f647204eb25744e022b906342ae4f3563794d1c2383e96a562245fc` | `blk00037.dat` | 42627956 | 4694 |

Files: `mainnet-<height>.block.hex` and `mainnet-<height>.coinbase.hex`.

## Scan (c): the old budget window (quirks 14, 15, 11)

| Height | Block hash | blk file | record offset | Block bytes | Role |
|---|---|---|---|---|---|
| 332320 | `000000000014702a849cddf3145c495086ef5235b8e1a08fbea8e9c17c7d149b` | `blk00007.dat` | 110347381 | 1533 | first block of the window (332320 = 20 × 16616); coinbase pays 118108031847 duffs, the oracle's `subsidy + totalfee` is 508031847 (excess 117600000000) |
| 332319 | `00000000000f9cd3088c91095c580b60b6cd41e275a0b1fefbfa8cf7533c070e` | `blk00007.dat` | 110405563 | 276 | the block before it, outside any window: not a candidate |

Files: `mainnet-332320.block.hex` and `mainnet-332319.block.hex`. The block hashes and the oracle's
reward come from `getblockhash`/`getblockstats` on the mainnet oracle.
