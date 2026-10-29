# Provenance of the oracle-written mncache vector (BL-451)

`populated.mncache.dat` was written by Dash Core's own `CMasternodeMetaMan` and `CFlatDB`
(`masternode/meta.cpp`, `flat-database.h`, unchanged from v24.0.0-rc.4 375c64d2317), linked
in-process through the ABI 30 shim: Core commit c3e388c24ce (`node/sp1-oracle-mnsync-v24.0`), the
pinned plain archive `libdash_oracle_v24_0.a` sha256 `106dac88586c3e3fddf1d695f0500184defdfbe55cc6d9f8cece3c36627a2f4c`
(`dash-oracle-sys/pins.txt`; `oracle.h` "masternode sync"). No dashd
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
