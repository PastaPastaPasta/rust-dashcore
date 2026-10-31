# Core net-state vectors (IM1a)

Files written by a real Dash Core **v24.0.0-rc.3** `dashd` (tag `v24.0.0-rc.3`, commit
`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`; binary `/work/chains/bin/dashcore-24.0.0-rc.3/bin/dashd`,
sha256 `e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4`), regtest, captured on
2026-10-09 with Core's own functional-test framework (the `test/` tree of that tag, run with
`python3 im1a-capture.py`, see `dash-coreimport/scripts/im1a-capture.py`). The node's datadir was
under `/dev/shm`. Nothing here was edited by hand; the files are copied byte for byte from
`<datadir>/regtest/` after a clean `stop`.

```sh
# one-off: Core's test tree and a venv with dash_hash
git -C ~/workspace/dash archive v24.0.0-rc.3 test/ | tar -x -C <work>/core   # + test/config.ini
python3 -m venv <work>/venv && <work>/venv/bin/pip install dash_hash
cp scripts/im1a-capture.py <work>/core/test/functional/
cd <work>/core/test/functional && DASHD=<bin>/dashd DASHCLI=<bin>/dash-cli \
  PYTHONPATH=. <work>/venv/bin/python im1a-capture.py --configfile=<work>/core/test/config.ini \
  --tmpdir=/dev/shm/im1a-ft/run --nocleanup --outdir=<out> --timeout-factor=4
```

## `empty/`

A fresh regtest node started and stopped: the files Core creates at start-up and shutdown with
nothing in them (`peers.dat` with an empty address manager, `anchors.dat` with no addresses,
`banlist.json` with an empty list, `mempool.dat` with no transactions, `fee_estimates.dat` with
zeroed statistics, `settings.json` with only the `_warning_` key).

## `populated/`

After the script's state was built (mock time `1417713337`, 180 blocks):

- `fee_estimates.dat`: 60 blocks of five self-transfers each at five fee rates (MiniWallet).
- `peers.dat`: 46 `addpeeraddress` calls: IPv4 new and tried entries, one IPv4 address with a
  second port, an IPv6 pair (one tried), a TORv3 address, an I2P address. Two calls were refused by
  Core (a documentation-prefix IPv6 address; an I2P address as tried), which `expected.json` records.
- `banlist.json`: `setban` of a /32, a /8 with an absolute time, an IPv6 /64, an address with a
  netmask (`1.2.3.4/255.255.0.0`, which Core writes as `1.2.0.0/16`) and an onion address.
- `settings.json`: two wallets created with `load_on_startup` (so a `wallet` array).
- `mempool.dat`: four unconfirmed transactions (all in the unbroadcast set, as no peer was
  connected), a `prioritisetransaction` delta of 12345 on the first, and deltas on two txids that
  are not in the pool (`-777`, `2**40`).
- `anchors.dat`: one block-relay-only P2P connection from 127.0.0.1 and one TORv3 connection
  through a SOCKS5 proxy (`addconnection`), written at `stop`.
- `expected.json`: ground truth read from the same node over RPC just before the stop:
  `getrawaddrman` (every new/tried entry with its bucket and source), `getnodeaddresses`,
  `listbanned`, `getrawmempool true`, `getmempoolentry` for each transaction,
  `estimaterawfee` at five targets, `getpeerinfo`, and the `addpeeraddress` results. The tests
  compare the parsed files with it.

## `nan-decay/`

R2-M1 (Sol r2): a `fee_estimates.dat` that Core **loaded and wrote back** with a NaN decay in all
three horizons. Core's `TxConfirmStats::Read` (`policy/fees.cpp`) throws only for
`decay <= 0 || decay >= 1`; both comparisons are false for NaN, so the file loads (neither "unable to read policy estimator data" nor "Failed to read fee
estimates" is logged) and `stop` writes the three NaN words back; a refused file would have been
written back with the default decays instead. The input was
`populated/fee_estimates.dat` with its three decay words replaced by a quiet NaN; the file here is
what the same `dashd` binary (sha256 `e204e316...ab5bb4`, above) wrote at `stop`, copied byte for
byte by `scripts/im1a-nan-decay.py` (regtest, `-disablewallet`, no network). It is the one file in
this tree whose input was edited by hand before Core read it; Core's output is what is kept.

`sha256sum` of the files:

```
ae957e07cb285ebe1e0c1c5687a50169356d95f418f963e3125cc0d6b1e8b327  empty/anchors.dat
b6df734c272c270f7480b6e463da1777fe4308e5c5ec2895bf9f5c5cd7e56aa1  empty/banlist.json
2389042312aeff7a7cb01cfd6dfb14730be578c40c17159f2a77e11fbf9a5503  empty/fee_estimates.dat
aa25e1d595a77c375b40b4796ea4ff044890efeebd4bd15d16d54387a5a8fc37  empty/mempool.dat
2f1a8e52811bbf2e8db0b2d30ac75e38aa65f8267dc7b0dce1b69accc8f48cb3  empty/peers.dat
9c8e70c58016680e9e338b17c4755b1faaabe52bc67822e7b6f30f4c98b76f7e  empty/settings.json
0a89342c409a69f246c443fc97752531d285cb135fb54a6b79dcca1c08de812d  populated/anchors.dat
24475c4d9e0c7e104019d5758a7cb7ccb5d0e6ce8257ca577d125ef74f895645  populated/banlist.json
4a08c991ee36ede7e3b3efc0fb0722503ffbaba8f72e93abda657b61a1e62875  populated/expected.json
d9cd2d09bd1749ca4fd34c040b03fb936d29641d3cba9c092330bfa8c9914d4a  populated/fee_estimates.dat
61f22816d4a2ca9cc414cc6d490ec66984afee4a6dba42952c4f6054ccbb091a  populated/mempool.dat
3515c8355332eadaf54fe00a64fef668c773f414b945f54a76a44b04a61c47e8  populated/peers.dat
0c538efab904ff9e28a1741d608915eab73956e417ee674f026aad776d1ca37c  populated/settings.json
b6ab1ba3fe39f8322229b2dd9315e290488dd4016fbffb815185a01d19c82ad6  nan-decay/fee_estimates.dat
```

Core v24 facts the capture confirms: `mempool.dat` is version 1 with no obfuscation key (the XOR key
came with version 2 in later Bitcoin Core releases); `peers.dat` is format 4 with compat byte 36;
`anchors.dat` entries are stored in the BIP155 form (`0x20035b60` version word).

Regenerating gives different bytes (random addrman key, wall clock in mock time only, random
transaction inputs); the tests read the values they compare from `expected.json`.
