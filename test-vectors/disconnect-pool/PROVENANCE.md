# Provenance of `test-vectors/disconnect-pool/`

CH3b b1's vectors (CH3 §4.3, D25; DEC-479): what Dash Core's `DisconnectedBlockTransactions`
(`kernel/disconnected_transactions.h`), its `RecursiveDynamicUsage` accounting
(`core_memusage.h`, `memusage.h`) and libstdc++'s `std::unordered_map` rehash policy compute.
`dashd-chainstate/tests/disconnect_pool_vectors.rs` replays every row against
`dashd_chainstate::reorg`. Nothing is hand-written.

| File | Rows |
|---|---|
| `buckets.txt` | `_Prime_rehash_policy::_M_next_bkt(n)` and the `_M_next_resize` it leaves, as runs covering every `n` from 0 to 2^64 − 1: each `n` below 2^24 called one by one, each run above checked at both ends and at 64 random `n` inside |
| `container.txt` | the pool's map type, `std::unordered_map<uint256, std::list<CTransactionRef>::iterator, SaltedTxidHasher>`, on its own: its `bucket_count()` growing one insert at a time to 2^21 entries, and after each of 150 random `reserve`/insert/repeated-key/erase/`clear` calls in 64 sequences |
| `pool.txt` | Core's object sizes and `MallocUsage(0..=100)`; 400 transactions with `GetHash()` and `RecursiveDynamicUsage` (21 script lengths from 0 to 10,000 bytes, among them 27, 28 and 29 either side of the 28-byte inline capacity, 0 to 100 inputs and 0 to 1,000 outputs, special transactions with payloads, coinbases, version-1 and version-2 asset unlocks and four re-signed twins); two bulk ranges (61,200 transactions `Bulk()` builds, each range's usage and a SHA-256 of its `GetHash()`s); 61 scenarios of `AddTransactionsFromBlock`, `removeForBlock` and `take()` with the pool's `DynamicMemoryUsage`, bucket count, size, `cachedInnerUsage`, the evicted or taken transactions in order and the queue after every call |

The scenarios: empty blocks on a new pool (`reserve(0)` still rehashes its single bucket); a txid
queued twice, with and without eviction (Core assumes it never happens; the map keeps its first
entry): a removal takes the first copy, a copy left without a map entry stays through a removal,
and evicting that copy erases the entry a later copy holds, which then stays through a removal
(`duplicate-txids-evicted-mapping`, under the largest budget at which only the last add evicts,
and evicts exactly the front and that copy); a re-signed version-2 asset unlock removing the
disconnected one through their shared stable txid, and both instances queued apart; a
200-transaction block mostly evicted, then one-transaction blocks that shrink the map from 211
buckets to 19; 48 random reorgs under budgets from 0 to Core's 20,000,000 bytes; and Core's budget
over the bulk ranges, 30 blocks of 2,000 transactions (about 26 MB of accounting, so the pool
evicts from the 23rd block on), then 40 blocks of 30 that shrink the map from 49,201 buckets to
45,481, the new chain's removals, and `take()`.

## How they were produced

The generator is a standalone Core program, built only on request:
`src/test/ch3b/gen_dbt_vectors.cpp` with `src/Makefile.ch3b.include` (an `include` line added to
`src/Makefile.am`). `generator/` here holds both files as committed. It runs Core's class
unmodified, reading its private list, map and cached usage through pointers to members taken in
explicit template instantiations, and builds each transaction the way `ReadBlockFromDisk` holds
it: serialized, then read back as a `CTransactionRef` (it checks that every `vin` and `vout`
capacity equals its size). The only randomness that reaches the files is splitmix64 with fixed
seeds (`SaltedTxidHasher`'s random salts move map entries between buckets, never the bucket
count, and no map is iterated); the files hold no path, time or Core version.

| | v24.0.0-rc.3 | v24.0.0-rc.4 |
|---|---|---|
| Core base | `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18` | `0e7382df80d0bc6811068d076b232a5a26d34a3c` |
| generator commit (local branch) | `0204a85a5462d6dce4d46b463422661a727b9e01` (`node/ch3b-dbt-vectors`) | `d2e0ce1dce3371802420813548d71207860eda72` (`node/ch3b-dbt-vectors-rc4`, the same commit cherry-picked) |
| `gen_dbt_vectors.cpp` sha256 | `8af7192ac7153be7f6286d8b8a0db530b635b65825cca74068569c93aaf669a2` | the same |
| `Makefile.ch3b.include` sha256 | `c4b715292745566b31830961d18f5e0d291a41f8cadc86f275cf5d63d424a469` | the same |

Both checkouts were built by g++ (Ubuntu 15.2.0-16ubuntu1) 15.2.0, the oracle's compiler,
`__GLIBCXX__` 20260321, on x86_64-pc-linux-gnu, against Core's depends tree (`depends/` is the
same tree, `6d59b8793ff4d2919aeb420a9e4e0e235696f2b2`, at both tags), configured as the oracle
configures (`contrib/node/build-oracle.sh`, without its hooks or flags):

```sh
make -C depends HOST=x86_64-pc-linux-gnu NO_QT=1 NO_WALLET=1 NO_ZMQ=1 NO_UPNP=1 NO_NATPMP=1 NO_USDT=1
./autogen.sh
CONFIG_SITE=$PWD/depends/x86_64-pc-linux-gnu/share/config.site CC=gcc CXX=g++ ./configure --prefix=/ \
  --disable-wallet --without-gui --disable-zmq --without-miniupnpc --without-natpmp --disable-bench \
  --disable-tests --disable-fuzz-binary --disable-man
make -C src dashbls/libdashbls.la
make -C src test/ch3b/gen_dbt_vectors
src/test/ch3b/gen_dbt_vectors <outdir>
```

The two runs (2026-10-26) printed byte-identical files:

| File | sha256 |
|---|---|
| `buckets.txt` | `03910ea52c35c488119bf6edfbd69ab1d4d12bad74701693c50f67e8f06d90a6` |
| `container.txt` | `209b2d5fba8a589e214ff98cb3eb28028e2ba85f822727c8d3a56b3bd82c1219` |
| `pool.txt` | `a0c874eda52411020c7fc1ea648d6f060339564e30e37309f36f876dd5a0da7d` |

The files Core's pool depends on (`kernel/disconnected_transactions.h`, `memusage.h`,
`core_memusage.h`, `prevector.h`, `serialize.h`, `primitives/transaction.{h,cpp}`,
`util/hasher.{h,cpp}`) are identical at the two tags.

## What they pin

The reference is Core built with libstdc++ on x86_64 Linux. The bucket counts are libstdc++'s
(GCC 15.2.0's `_Prime_rehash_policy`, its LP64 prime list), and the sizes and `MallocUsage` are
x86_64's. Core's figure differs under libc++ or MSVC, and under another libstdc++ policy; rdashd
keeps this one on every platform (CH3 D25, an allowed difference). A move of the reference
compiler regenerates these files and records the new run here.

Licence: MIT, with Dash Core's notice (`core/0364ed626ec2/COPYING`).
