# MN0-2a regtest captures

Ground truth for `CreateNewBlock` once DIP0003 is active, on chains without masternodes: the blocks
Dash Core mines there with `generateblock <address> []`, an empty transaction list, so each is
`CreateNewBlock`'s block on an empty mempool. Each holds the CbTx (versions 1 to 4, as the
deployments allow), the platform's `OP_RETURN` share from MN_RR on, and a null commitment for each
quorum in a mining window.

`capture_mn0_evo.py` mines seven fresh regtest chains, one block per mock-time step
(`1700000000 + height * 150`), to the same address:

| chain | arguments | tip | what it covers |
|---|---|---|---|
| `default` | none | 500 | DIP0003, V20 and MN_RR at 432 (DIP0008 from 1): CbTx v3, three DKG mining windows |
| `moved` | `-dip3params=2:2`, `-testactivationheight=dip0008@50`, `v20@150`, `mn_rr@200` | 270 | CbTx v1 (2..49), v2 (50..149), v3 without MN_RR (150..199), v3 with it (200..270) |
| `v24` | `-vbparams=v24:0:9223372036854775807:0:100:80:60:5:0` | 480 | V24 by signalling, no EHF, active at 300: the first CbTx (432) is v4 |
| `brr-late` | `-testactivationheight=brr@1000` | 460 | MN_RR (432) below BRR: the platform reward follows the payment increase steps (one at 440) |
| `mnrr-early` | `-dip3params=2:2`, `-testactivationheight=v20@10`, `mn_rr@10` | 60 | MN_RR below the reallocation start (20): the platform reward's 20 % base to 19, 75 % from 20 |
| `legacy-bls` | `-dip3params=2:2`, `-testactivationheight=v19@100`, `v20@150`, `mn_rr@150` | 200 | legacy-scheme null commitments (versions 1 and 2) below V19, then 3 and 4 |
| `v24-before-v20` | `-dip3params=2:2`, `-testactivationheight=v20@350`, `mn_rr@350`, the `v24` chain's `-vbparams` | 370 | V24 active at 300 without V20: CbTx v2 to 349, v4 from 350 |

No masternode is registered, so every commitment is null and no ChainLock exists.

## Rerun

```
python3 -I contrib/node/mn0-captures/capture_mn0_evo.py <dashd> <work-dir> <out-json>
python3 -I contrib/node/mn0-captures/capture_mn0_evo.py \
    ~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd /work/scratch/mn0/capture \
    dashd-mining/tests/data/mn0-evo-regtest.json
```

`<work-dir>` gets one datadir per chain and must not hold them already. The chains are
deterministic (mock time, a fixed address, no wallet), so a rerun with the same `dashd` gives the
same blocks; only `captured_utc` changes.

## Fixture (`dashd-mining/tests/data/mn0-evo-regtest.json`, sorted keys)

- `provenance`: `tool`, `dashd_subversion`, `dashd_sha256` (of the `dashd` that ran), `captured_utc`,
  `address`, `mocktime`, `rpc`.
- `chains`, in the table's order, each with `name`, `args`, `tip`, `dip3_height` (the first block
  whose coinbase is a CbTx), `headers` (every raw header, 0..=tip), `blocks` (the raw blocks from
  `dip3_height` to the tip), `script_pubkey` (the address's) and `deployments_at_tip` (Core's
  `getdeploymentinfo().deployments`).

The last four chains are MN0-2a r1's (Opus-h) probe chains. The committed copy was captured on
2026-10-28 with v24.0.0-rc.4 (`dashd_sha256`
`e1be2564c5f98b7cb97e60953e1ed6817eeb3b5ab36cf5473f32ad75b4f823a7`).
`dashd-mining/tests/evo_dashd.rs` assembles every block from `dip3_height` with
`dashd_mining::evo::block` and compares it with dashd's bytes.
