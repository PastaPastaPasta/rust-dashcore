# Provenance of the oracle-written mncache vector (BL-451)

`populated.mncache.dat` was written by Dash Core's own `CMasternodeMetaMan` and `CFlatDB`
(`masternode/meta.cpp`, `flat-database.h`, unchanged from v24.0.0-rc.4 375c64d2317), linked
in-process through the ABI 30 shim: Core commit c3e388c24ce (`node/sp1-oracle-mnsync-v24.0`), the
pinned plain archive `libdash_oracle_v24_0.a` sha256 `106dac88586c3e3fddf1d695f0500184defdfbe55cc6d9f8cece3c36627a2f4c`
(`dash-oracle-sys/pins.txt`; `oracle.h` "masternode sync"). The pin's move to 6959e3b906f
(`node/bl477-govnode-v24.0`, plain archive sha256
`e1299760619b4faf06a056276ca6bd664092a74f7cec6d186c216ae14a3e3539`; BL-477's governance-node
fix, one commit on c3e388c24ce; like every oracle branch it lives only in the local Dash Core
repository, unpushed) writes the same bytes: `core_written_vector_is_current` passes on
it unchanged. So does the move to 35e3bcf11a (`node/sp1-oracle-loadcache-v24.0`, ABI 31, plain
archive sha256 `eb9efe1604b495ac22ac2ab3784c64c5099abc1be92d7b4ca44fe25ae85f7eb2`; BL-497's
`LoadCache` flag for `oracle_flatdb_load` and the file right after it, two commits on
6959e3b906f, local only). No dashd
capture holds a live platform ban, a nonzero `nMixingTxCount` or a used-masternodes queue (a ban
needs a platform quorum's signed `platformban`, the other two a mixing CoinJoin client; SP1 D17),
so this file gives SP1's codec those fields in Core's bytes.

`node-l1/tests/l1_mnsync.rs` `core_written_mncache()` makes it on regtest:

1. `oracle_mncache_step` runs a script on a fresh `CMasternodeMetaMan`: `AllowMixing(A)`,
   `AllowMixing(B)`, `DisallowMixing(A)` twice, `AddGovernanceVote(A, 11…)` twice,
   `AddGovernanceVote(B, 22…)`, `SetLastOutboundAttempt(C, 1700000000)`,
   `SetLastOutboundSuccess(C, 1700000005)`, `SetLastOutboundAttempt(C, 1700000010)`,
   `SetPlatformBan` for B at height 120 and for C at 130 (null signatures), `ResetPlatformBan(C,
   140)`, `AddUsedMasternode` A, B, C, A, then `RemoveUsedMasternodes(1)`. A, B and C are the
   protx hashes `a1…`, `b2…` and `c3…`.
2. Its store body (Core's `SER_DISK` serialization), in `CFlatDB`'s envelope (`magicMasternodeCache`,
   regtest's message start, SHA256d), is loaded by `oracle_flatdb_load` in a private datadir:
   `CoreRead` returns `Ok` and `LoadCache(true)` succeeds.
3. The manager's destructor `Store()`s it; the file it leaves is this one, byte for byte.

The result: A has `nLastDsq` 1, `nMixingTxCount` 2 and one object voted on twice; B has `nLastDsq`
2, one vote and a live ban updated at 120; C has one outbound attempt (1700000010), a success
(1700000005) and a reset ban (not banned, updated at 140); `nDsqCount` 2; the used queue is B, C.

sha256 `5152f945074a294fbef09d958a7d425de67804d0f2c3116ae3dd888e2ad958f1`. The test
`core_written_vector_is_current` fails if the pinned Core writes anything else;
`BL451_WRITE_VECTORS=1` rewrites the file.
