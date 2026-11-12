# Multi-peer block fixture provenance

Generated 2026-11-12T10:46:25.634164+00:00.

- Generator: `contrib/node/p3-multi-blocks/make_fixture.py`, first committed in `c926208fac058859034f0a0933d4e9b50bed1259` (generated from source base `e8700fde97e5c9b8a76d573be930245bb7b70b86`).
- Generator SHA256: `d15ed63d6ddeda70f72e5c8243f0c28fa3e67c9e16e4aaf5f3aa98855762bd6d`.
- Binary: `/work/home/agent/.rust-dashcore-test/dashcore-24.0.0/bin/dashd`; SHA256 `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`.
- Version output SHA256: `8740858a08f4848c5ffd579aa040401457785ca9ab82ef29f54ba7f8c8da9b0a`.
- Version: `Dash Core version v24.0.0`.
- Command: `python3 contrib/node/p3-multi-blocks/make_fixture.py --dashd /work/home/agent/.rust-dashcore-test/dashcore-24.0.0/bin/dashd --scratch /work/scratch/p3/bl238/tmp-b/fixture`.
- Output: `node-l1/fixtures/p3_multi_blocks.bin`, SHA256 `7d642f2a7d376a770fe769b600a50bc4c4554664fcfae6b3ef563c2670793997`, 728670 bytes.
- Chain A: 1100 blocks, mock time 1697408000 before generation; `blocks-1100.txt`: equal.
- B prefix: heights 1..100, mock time `SIM_START - 1000 + height - 1` before each block.
- B workload: heights 101..124, mock time `SIM_START - 900 + height - 101` before each block.
- Workload transaction counts (including coinbase): [2, 1, 1, 2, 3, 1, 5, 9, 1, 2, 17, 1, 33, 1, 1, 2, 4, 1, 3, 1, 2, 1, 1, 6].
- B side: heights 101..103 on prefix tip, mock time `SIM_START - 880 + height - 101`.
- Prefix and coinbase-only blocks use `generatetodescriptor raw(51)`; workload uses `generateblock raw(51)`.
- Block 101 fans coinbase 1 into 96 OP_TRUE outputs; later spends consume distinct outputs.
- No workload transaction is 64 bytes long.
