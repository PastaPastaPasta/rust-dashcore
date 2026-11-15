# Core v24.0.0-rc.4 flat file vectors (IM1b, BL-420)

Every file here was written by Dash Core, none was edited. They come from a real
**v24.0.0-rc.4** `dashd` (tag `v24.0.0-rc.4`, commit `0e7382df80d0bc6811068d076b232a5a26d34a3c`;
binary `~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd`, sha256
`e1be2564c5f98b7cb97e60953e1ed6817eeb3b5ab36cf5473f32ad75b4f823a7`), captured on 2026-10-27, and are
copied byte for byte from the node's datadir after a clean stop. rc.4 writes `governance.dat` as
`CGovernanceManager-Version-17` (rc.3 and v23 write `-16`, see `../v24.0.0-rc.3/PROVENANCE.md`); the
other three files keep the versions they had.

The set is the same as rc.3's, made by the same script and recipe, only with the rc.4 binaries and
Core's `v24.0.0-rc.4` functional-test tree (`git archive v24.0.0-rc.4 test/ share/rpcauth`):

```sh
B=~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin
DASHD=$B/dashd DASHCLI=$B/dash-cli PYTHONPATH=. <venv>/bin/python im1b-capture-governance.py \
  --configfile=<core>/test/config.ini --tmpdir=<run>/populated --nocleanup --outdir=<out> \
  --timeout-factor=6 --portseed=4419          # the empty set: add --empty
```

`empty/` is a plain one-node regtest node started and stopped at once; `populated/` is node 0 of the
seven-node network at height 179 with the proposals, votes, sporks and peers described in the rc.3
file. `populated/expected.json` is the RPC ground truth read from the same node just before the stop
(`getblockcount` 179).

| file | sha256 |
|---|---|
| `populated/governance.dat` | `e5f1f3395df4bd711e369521674d3b8d6e6ab69ed77940700292141dc0d9836a` |
| `populated/mncache.dat` | `e1d4bcdb09b012bc7682ca8d3fbe4692fc765406b3fe44f89bb9d1c27c3bd060` |
| `populated/netfulfilled.dat` | `963107f61b3b79b45917de145d7d4e537d0ddb6816dfc4c718491efd98db9f64` |
| `populated/sporks.dat` | `35bc6505c25b163a21088b06beffc0a5f32e3ba1a4ad78298c7205f32551f3dc` |
| `populated/expected.json` | `2f0c4c382c7acd7063ddf7d775cc171d370eeefde91e0b28ac4bb92c8bf1a721` |
| `empty/governance.dat` | `6f34af5cc63dc0bbcc209696af5cc451607f6e09128042d6c9d7fd9aa40d5a65` |
| `empty/mncache.dat` | `9986c1030eff3cb878bc9b206f14a09cd64553cab887db8b8730a22f19a0eb17` |
| `empty/netfulfilled.dat` | `723dcd105d40f970e6f3066327c326ee53ae81bb58e46eabf927de047824158c` |
| `empty/sporks.dat` | `9961d71a63a97ad7346de85a2ba7eb42b72c5bfbaedd46728777bb71e84428b8` |
