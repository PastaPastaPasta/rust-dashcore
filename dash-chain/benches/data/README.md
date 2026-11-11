# Real headers for the `accept/*` benches (SB4 §16, A-25)

Two slices of H-M (`tests/data/headers/README.md`: mainnet heights 0..=2,551,766, SHA-256
`58e6e286297e40e9b9e3afa1e5a734c27bd76be065631fb27d3a5653bf5f8251`), 80 bytes per header in
height order. The gate builds benches in detached worktrees that never see the untracked H-M
file, so the windows the benches need are checked in.

| File | Heights | Bytes | SHA-256 |
|---|---|---|---|
| `mainnet-1999950-2002000.bin` | 1,999,950..2,002,000 | 164,000 | `9f8b087701abca498b1367f25741af73600daf5f54b242df3fb23b246ef9ae49` |
| `mainnet-15900-22000.bin` | 15,900..22,000 | 488,000 | `f0f8bf574474b6a19c28161fed8f7da32e43e2dc1aa4efc330bd4c838303535f` |

Each is `H-M[80 × first .. 80 × end]`:

```bash
dd if=mainnet.bin of=mainnet-1999950-2002000.bin bs=80 skip=1999950 count=2050
dd if=mainnet.bin of=mainnet-15900-22000.bin bs=80 skip=15900 count=6100
```

DGW's batch is 2,000,000..2,002,000, with the 50 headers below it as context (DGW reads 24, the
median time 11). KGW's batch is 20,000..22,000, with 4,100 below: KGW walks back at most
`PastBlocksMax` = 4,032 entries. Below a slice the benches' index is synthetic, so it holds no
checkpoint hash and `accept` skips the checkpoint comparison Core's real pre-state makes: a
lookup per header at most, against ~57,000 instructions per header in the DGW region.

The SPV bench's anchor is height 1,999,999, hash
`000000000000001dc4e83bba91118506a4db6b9cad0dc2cbb45db216280b0632`, with chain work
`0x8f6a7c459d4e1aeaa76b`. That is `getblockheader`'s `chainwork` from the mainnet reference node
(`/Dash Core:24.0.0/`, docs/node-program/ORACLE.md), read with read-only RPC on 2026-10-10,
whose raw header at that hash equals H-M's at 1,999,999.
