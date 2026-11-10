# dash-consensus benches

`cargo bench -p dash-consensus --bench next_work` (CS2 §3.9, §6.5). It measures
GetNextWorkRequired on DGW, a BTC retarget, the min-difficulty walk and KGW, then the subsidy
and the masternode payment. The DGW, BTC and min-difficulty windows come from the recorded Core
corpus; the KGW ones are boundary windows of `tests/common`.

`next_work/kgw-next-work-4032` is KGW's worst case: all 4032 blocks of its window, with no
break before the last. PF0's counter gate guards it once PF0 merges. Declare it in
`contrib/node/perfgate/workloads.toml`:

```toml
kgw-next-work-4032    = { bench = "dash-consensus/next_work", id = "next_work/kgw-next-work-4032" }
```

`cargo bench -p dash-consensus --bench checks` (CS3). It measures `check_block` with the proof of
work and merkle checks on Core's benchmark block block813851, alone and after `decode_block`
(Core's `DeserializeAndCheckBlockTest`). Declare it in `contrib/node/perfgate/workloads.toml`:

```toml
check-block-813851    = { bench = "dash-consensus/checks", id = "block813851/check_block" }
```

`cargo bench -p dash-consensus --bench deployments` (CS4). It measures the versionbits reads on a
4032-block regtest chain where v24 (window 100, EHF) starts, locks in and activates:
`deployments/per_block_warm` is, for every block, `DeploymentActiveAfter` of V24 and EVO_SHARES
and `ComputeBlockVersion` once the cache holds every period (one lookup per deployment, no
allocation; `tests/deployments_alloc.rs` checks the latter), and `deployments/first_walk_cold`
is v24's state at the tip from an empty cache. PF0 gates both
(`deployments-per-block-warm`, `deployments-first-walk-cold` in
`contrib/node/perfgate/workloads.toml`).
