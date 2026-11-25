# LK1a C2 provenance

Captured by `contrib/node/lk1a-capture/capture.py` from our own regtest network.

- dashd: release `Dash Core version v24.0.0`, from `contrib/setup-dashd.py` with `DASHVERSION=24.0.0`
- dashd SHA-256: `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`
- Network driver: Dash Core functional test framework at tag `v24.0.0` (`938fba502824b9d850b7c710601456dc3d71603e`),
  controller plus four masternodes, `-testactivationheight=v19@300`, V20 at the framework's
  default height 100
- Controller chain height at capture end: 365

Gossiped CLSIGs exist on both sides of V19, but v24.0.0 coinbases carry ChainLocks only from V19 on
(the miner blanks a best ChainLock below the V19 height, node/miner.cpp:120-123).

N-cb has no peers and is fed every block by `submitblock` after mnsync is forced past BLOCKCHAIN
and SPORK_19 is set locally; its record is its first best-ChainLock change (all changes are listed
in `input.best_chainlock_changes`). N-p2p's only peer is the scripted peer; each case is an `inv`,
then the CLSIG if N-p2p asked for it with `getdata`, each followed by a ping/pong, and
`input.exchange` holds that case's messages in both directions (received payloads re-serialized by
the framework). N-p2p's `got inv: clsig ... have|new` debug line must agree with whether it asked.
N-p2p is fed blocks 1..hL before SPORK_19 is set, and blocks hL+1..hB between the legacy and the
basic cases; the coinbase route moves its best ChainLock forward during that feed, before the
bit-flip case (the value is in that case's `getbestchainlock_before`).

`state` is observed after each case: mnsync, tip and IBD, SPORK_19, then
`syncwithvalidationinterfacequeue` and the post-queue `getbestchainlock`; `scheme` follows the
observed `v19_active`. `block` is the block the CLSIG names. `derived` holds values computed from
the CLSIG height, not observed: the signing quorum start height h - 8 (SIGN_HEIGHT_OFFSET,
llmq/quorumsman.h:214).

| case | node | outcome | CLSIG height | scheme | node tip |
|---|---|---|---|---|---|
| `pre-v19-legacy-valid` | N-p2p | valid | 165 | legacy | 165 |
| `p2p-duplicate-clsig` | N-p2p | duplicate | 165 | legacy | 165 |
| `p2p-stale-clsig` | N-p2p | stale | 164 | legacy | 165 |
| `p2p-one-bit-flipped-signature` | N-p2p | invalid | 356 | basic | 356 |
| `post-v19-basic-valid` | N-p2p | valid | 356 | basic | 356 |
| `p2p-no-quorum-past-tip` | N-p2p | no_quorum | 365 | basic | 356 |
| `coinbase-route-best-change` | N-cb | valid | 312 | basic | 313 |
