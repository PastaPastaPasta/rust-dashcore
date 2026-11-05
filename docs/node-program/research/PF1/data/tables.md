> Note: an earlier version of this file carried a "CPU-time share" column for the inclusive tables that was a lookup bug (always 0.00 %); it is removed here. The inclusive and anchor tables are regex-over-signature artifacts (report §0, §11).

### connect-big

4 blocks, 15.47G instructions:u = **3.87G/block**; 3.87G/block without the unnamed LevelDB background thread; 7501.0 tx/blk, 7500.0 txin/blk; CPU 2.9 s over 4.5 s wall (parallelism 0.65)

| subsystem (instructions:u) | share |
|---|---|
| script-verify | 85.03% |
| sha256 | 8.61% |
| coins-cache | 2.72% |
| tx-block-codec | 0.93% |
| validation | 0.74% |
| block-io | 0.70% |
| leveldb | 0.62% |
| ripemd160/hash160 | 0.27% |
| mempool | 0.16% |
| evodb/mn-lists | 0.12% |
| x11 | 0.04% |
| rpc/http/net | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| ConnectTip | 99.42% |
| ActivateBestChain | 99.42% |
| LoadBlockIndex / init | 99.30% |
| ConnectBlock (whole) | 93.25% |
| CheckInputScripts / script checks | 90.11% |
| VerifyScript | 89.14% |
| ReadBlockFromDisk | 3.41% |
| CCoinsViewCache::FetchCoin | 1.36% |
| CheckBlock (context-free) | 0.58% |
| UpdatedBlockTip / notifications | 0.58% |
| UpdateCoins / AddCoins / SpendCoin | 0.54% |
| LevelDB (any) | 0.31% |
| ProcessSpecialTxsInBlock | 0.04% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 99.42% |
| scheduler | 0.58% |

| top leaf symbols | share |
|---|---|
| secp256k1_fe_mul_inner | 41.31% |
| secp256k1_fe_sqr_inner | 28.94% |
| Lloop1_30 | 5.97% |
| secp256k1_gej_double | 3.69% |
| secp256k1_gej_add_ge_var | 2.64% |
| SipHashUint256Extra(unsigned long, unsigned long, uint256 const&, unsi | 1.71% |
| secp256k1_modinv64_var | 1.44% |
| Lloop2_30 | 1.40% |
| secp256k1_ecmult_strauss_wnaf.constprop.0 | 1.36% |
| secp256k1_gej_add_zinv_var | 0.85% |

### dwarf-w1520000-serial-av

399 blocks, 92.00G instructions:u = **230.58M/block**; 230.58M/block without the unnamed LevelDB background thread; CPU 15.4 s over 15.9 s wall (parallelism 0.97)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.17% |
| other | 2.30% |
| leveldb | 1.04% |
| sha256 | 0.65% |
| x11 | 0.61% |
| script-verify | 0.48% |
| evodb/mn-lists | 0.39% |
| rpc/http/net | 0.13% |
| llmq | 0.13% |
| block-io | 0.04% |
| tx-block-codec | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| BLS (any bls::* / relic) | 36.78% |
| llmq::* (any) | 34.39% |
| ProcessSpecialTxsInBlock | 32.91% |
| ConnectBlock (whole) | 27.74% |
| CheckInputScripts / script checks | 19.35% |
| CheckCbTx | 5.96% |
| CChainLocks / VerifyChainLock | 5.96% |
| ConnectTip | 3.00% |
| ActivateBestChain | 2.09% |
| CheckSpecialTx | 1.09% |
| LoadBlockIndex / init | 1.04% |
| LevelDB (any) | 0.96% |
| CheckBlock (context-free) | 0.96% |
| CCreditPoolManager | 0.65% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 97.70% |
| init / main thread | 2.30% |

| top leaf symbols | share |
|---|---|
| __gmpn_addmul_1_coreisbr | 25.17% |
| __gmpn_mul_basecase_coreisbr | 23.87% |
| dv_cmp | 7.61% |
| fp_rdcn_low | 6.78% |
| core_get | 5.30% |
| __gmpn_add_n_coreisbr | 4.91% |
| [no-stack] | 2.30% |
| fp_addm_low | 2.26% |
| fp_prime_get | 2.17% |
| __gmpn_sub_n_coreisbr | 2.09% |

### dwarf-w421000-serial-noav

599 blocks, 25.76G instructions:u = **43.01M/block**; 43.01M/block without the unnamed LevelDB background thread; CPU 5.1 s over 5.4 s wall (parallelism 0.93)

| subsystem (instructions:u) | share |
|---|---|
| bls | 77.02% |
| other | 9.63% |
| sha256 | 4.19% |
| script-verify | 3.57% |
| leveldb | 1.86% |
| x11 | 1.86% |
| evodb/mn-lists | 0.93% |
| rpc/http/net | 0.31% |
| coins-cache | 0.16% |
| llmq | 0.16% |
| tx-block-codec | 0.16% |
| logging | 0.16% |

| inclusive (instructions:u) | share |
|---|---|
| ConnectBlock (whole) | 33.23% |
| llmq::* (any) | 32.61% |
| ProcessSpecialTxsInBlock | 31.83% |
| BLS (any bls::* / relic) | 31.68% |
| CheckInputScripts / script checks | 30.75% |
| ConnectTip | 10.71% |
| ActivateBestChain | 6.52% |
| VerifyScript | 6.37% |
| LoadBlockIndex / init | 5.90% |
| LevelDB (any) | 1.24% |
| CDeterministicMNManager::ProcessBlock | 1.24% |
| CheckSpecialTx | 0.62% |
| CheckBlock (context-free) | 0.47% |
| UpdatedBlockTip / notifications | 0.47% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 90.37% |
| init / main thread | 9.63% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 20.96% |
| __gmpn_addmul_1_coreisbr | 19.25% |
| [no-stack] | 9.63% |
| fp_rdcn_low | 6.06% |
| dv_cmp | 5.90% |
| core_get | 4.66% |
| __gmpn_add_n_coreisbr | 2.80% |
| Lloop1_30 | 2.33% |
| fp_addm_low | 2.02% |
| __gmpn_sub_n_coreisbr | 2.02% |

### dwarf-w925000-serial-av

399 blocks, 10.44G instructions:u = **26.17M/block**; 26.17M/block without the unnamed LevelDB background thread; CPU 2.4 s over 2.1 s wall (parallelism 1.15)

| subsystem (instructions:u) | share |
|---|---|
| bls | 60.15% |
| other | 27.97% |
| evodb/mn-lists | 4.60% |
| x11 | 3.83% |
| sha256 | 1.53% |
| leveldb | 0.77% |
| coins-cache | 0.38% |
| validation | 0.38% |
| llmq | 0.38% |

| inclusive (instructions:u) | share |
|---|---|
| BLS (any bls::* / relic) | 23.37% |
| ProcessSpecialTxsInBlock | 22.22% |
| ConnectBlock (whole) | 21.84% |
| llmq::* (any) | 15.71% |
| ActivateBestChain | 9.20% |
| ConnectTip | 8.81% |
| CCreditPoolManager | 6.90% |
| CDeterministicMNManager::ProcessBlock | 6.90% |
| CheckBlock (context-free) | 6.13% |
| CheckSpecialTx | 4.60% |
| LoadBlockIndex / init | 4.21% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 2.30% |
| ReadBlockFromDisk | 1.53% |
| UpdatedBlockTip / notifications | 1.15% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 72.03% |
| init / main thread | 27.97% |

| top leaf symbols | share |
|---|---|
| [no-stack] | 27.97% |
| __gmpn_addmul_1_coreisbr | 16.09% |
| __gmpn_mul_basecase_coreisbr | 16.09% |
| fp_rdcn_low | 5.36% |
| dv_cmp | 4.60% |
| __gmpn_add_n_coreisbr | 3.07% |
| core_get | 1.53% |
| __gmpn_sub_n_coreisbr | 1.53% |
| fp_prime_get_rdc | 1.15% |
| fp_prime_get | 1.15% |

### dwarf-w925000-serial-noav

299 blocks, 62.96G instructions:u = **210.57M/block**; 210.57M/block without the unnamed LevelDB background thread; CPU 9.1 s over 8.5 s wall (parallelism 1.07)

| subsystem (instructions:u) | share |
|---|---|
| bls | 91.23% |
| other | 5.72% |
| sha256 | 0.76% |
| leveldb | 0.76% |
| x11 | 0.70% |
| evodb/mn-lists | 0.44% |
| llmq | 0.19% |
| rpc/http/net | 0.13% |
| validation | 0.06% |

| inclusive (instructions:u) | share |
|---|---|
| BLS (any bls::* / relic) | 34.24% |
| llmq::* (any) | 31.32% |
| ProcessSpecialTxsInBlock | 30.56% |
| ConnectBlock (whole) | 24.97% |
| CheckInputScripts / script checks | 16.84% |
| CheckCbTx | 5.84% |
| CChainLocks / VerifyChainLock | 5.84% |
| ConnectTip | 2.41% |
| ActivateBestChain | 1.59% |
| CheckBlock (context-free) | 0.95% |
| LoadBlockIndex / init | 0.76% |
| CDeterministicMNManager::ProcessBlock | 0.70% |
| LevelDB (any) | 0.64% |
| CCreditPoolManager | 0.44% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 94.28% |
| init / main thread | 5.72% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 24.27% |
| __gmpn_addmul_1_coreisbr | 24.02% |
| dv_cmp | 7.50% |
| [no-stack] | 5.72% |
| fp_rdcn_low | 5.40% |
| __gmpn_add_n_coreisbr | 5.08% |
| core_get | 4.07% |
| fp_prime_get | 3.37% |
| __gmpn_sub_n_coreisbr | 2.35% |
| fp_addm_low | 2.35% |

### getblock-v0

7.06G instructions:u, 2000 getblock v0 = **3.53M/op**

| subsystem (instructions:u) | share |
|---|---|
| rpc/http/net | 53.87% |
| sha256 | 30.59% |
| block-io | 9.26% |
| x11 | 3.65% |
| tx-block-codec | 1.87% |
| special-tx | 0.34% |
| validation | 0.25% |
| bls | 0.17% |

| inclusive (instructions:u) | share |
|---|---|
| ReadBlockFromDisk | 44.94% |

| thread group | instr share |
|---|---|
| http/rpc workers | 100.00% |

| top leaf symbols | share |
|---|---|
| json_escape(std::__cxx11::basic_string<char, std::char_traits<char>, s | 32.88% |
| Lloop1_30 | 22.34% |
| void std::vector<std::byte, zero_after_free_allocator<std::byte> >::_M | 8.67% |
| _IO_fread | 5.27% |
| Lloop2_30 | 5.18% |
| HexStr[abi:cxx11](Span<unsigned char const>) | 3.48% |
| __memmove_avx_unaligned_erms_rtm | 2.12% |
| _int_free_chunk | 1.87% |
| CSHA256::Write(unsigned char const*, unsigned long) | 1.87% |
| __libc_malloc2 | 1.70% |

### getblock-v1

4.45G instructions:u, 2000 getblock v1 = **2.23M/op**

| subsystem (instructions:u) | share |
|---|---|
| sha256 | 46.09% |
| rpc/http/net | 26.15% |
| block-io | 15.63% |
| x11 | 6.06% |
| tx-block-codec | 2.43% |
| llmq | 2.29% |
| validation | 0.54% |
| special-tx | 0.40% |
| bls | 0.13% |
| memory/libc | 0.13% |
| other | 0.13% |

| inclusive (instructions:u) | share |
|---|---|
| ReadBlockFromDisk | 70.49% |
| llmq::* (any) | 6.06% |

| thread group | instr share |
|---|---|
| http/rpc workers | 100.00% |

| top leaf symbols | share |
|---|---|
| Lloop1_30 | 31.81% |
| Lloop2_30 | 10.65% |
| _IO_fread | 8.89% |
| json_escape(std::__cxx11::basic_string<char, std::char_traits<char>, s | 5.80% |
| _int_free_chunk | 3.77% |
| __libc_malloc2 | 3.23% |
| CSHA256::Write(unsigned char const*, unsigned long) | 2.56% |
| __memmove_avx_unaligned_erms_rtm | 2.56% |
| _int_malloc | 1.89% |
| _int_free_merge_chunk | 1.35% |

### getblock-v2

49.99G instructions:u, 2000 getblock v2 = **24.99M/op**

| subsystem (instructions:u) | share |
|---|---|
| rpc/http/net | 72.38% |
| sha256 | 9.73% |
| txindex/indexes | 7.25% |
| bls | 3.72% |
| block-io | 2.44% |
| tx-block-codec | 2.11% |
| llmq | 1.10% |
| x11 | 0.82% |
| script-verify | 0.19% |
| validation | 0.18% |
| special-tx | 0.04% |
| other | 0.02% |

| inclusive (instructions:u) | share |
|---|---|
| llmq::* (any) | 57.17% |
| ReadBlockFromDisk | 6.54% |
| UndoReadFromDisk / WriteUndo | 1.88% |

| thread group | instr share |
|---|---|
| http/rpc workers | 100.00% |

| top leaf symbols | share |
|---|---|
| json_escape(std::__cxx11::basic_string<char, std::char_traits<char>, s | 15.60% |
| EncodeBase58[abi:cxx11](Span<unsigned char const>) | 7.38% |
| Lloop1_30 | 6.71% |
| __memmove_avx_unaligned_erms_rtm | 4.97% |
| malloc | 4.15% |
| _int_free_chunk | 4.05% |
| std::vector<std::__cxx11::basic_string<char, std::char_traits<char>, s | 3.43% |
| __libc_malloc2 | 3.43% |
| UniValue::UniValue(UniValue const&) | 3.37% |
| cfree@GLIBC_2.2.5 | 3.12% |

### ibd-100k-av-par1

1261.22G instructions:u, 100000 block = **12.61M/op**

| subsystem (instructions:u) | share |
|---|---|
| x11 | 73.90% |
| sha256 | 4.54% |
| tx-block-codec | 3.68% |
| bls | 3.68% |
| validation | 2.98% |
| evodb/mn-lists | 2.38% |
| mempool | 1.97% |
| coins-cache | 1.73% |
| leveldb | 1.66% |
| rpc/http/net | 1.45% |
| block-io | 1.19% |
| logging | 0.31% |

| inclusive (instructions:u) | share |
|---|---|
| ActivateBestChain | 14.70% |
| ConnectTip | 13.08% |
| ConnectBlock (whole) | 7.25% |
| ProcessSpecialTxsInBlock | 3.11% |
| UpdatedBlockTip / notifications | 2.68% |
| CDeterministicMNManager::ProcessBlock | 2.67% |
| CheckBlock (context-free) | 1.59% |
| LevelDB (any) | 1.54% |
| FlushStateToDisk | 1.45% |
| LoadBlockIndex / init | 1.02% |
| UndoReadFromDisk / WriteUndo | 0.99% |
| CCoinsViewCache::FetchCoin | 0.57% |
| CEvoDB | 0.54% |
| llmq::* (any) | 0.44% |

| thread group | instr share |
|---|---|
| message handler (msghand) | 95.74% |
| shutdown | 1.49% |
| other: d-net | 1.38% |
| scheduler | 1.02% |
| init / main thread | 0.37% |
| LLMQ/DKG/sigs threads | 0.00% |

| top leaf symbols | share |
|---|---|
| sph_cubehash512_addbits_and_close | 17.66% |
| compress_big | 10.39% |
| jh_core | 9.31% |
| groestl_big_core | 7.17% |
| sph_luffa512_addbits_and_close | 4.80% |
| fft64.constprop.0 | 4.39% |
| groestl_big_close.constprop.0 | 3.53% |
| sph_cubehash512 | 3.33% |
| sph_luffa512 | 3.23% |
| Lloop1_30 | 3.13% |

### ibd-100k-av-par8

1261.34G instructions:u, 100000 block = **12.61M/op**

| subsystem (instructions:u) | share |
|---|---|
| x11 | 73.88% |
| sha256 | 4.54% |
| bls | 3.71% |
| tx-block-codec | 3.64% |
| validation | 3.02% |
| evodb/mn-lists | 2.41% |
| mempool | 1.96% |
| coins-cache | 1.77% |
| leveldb | 1.66% |
| rpc/http/net | 1.46% |
| block-io | 1.16% |
| logging | 0.27% |

| inclusive (instructions:u) | share |
|---|---|
| ActivateBestChain | 14.71% |
| ConnectTip | 13.16% |
| ConnectBlock (whole) | 7.31% |
| ProcessSpecialTxsInBlock | 3.17% |
| UpdatedBlockTip / notifications | 2.72% |
| CDeterministicMNManager::ProcessBlock | 2.71% |
| LevelDB (any) | 1.55% |
| CheckBlock (context-free) | 1.55% |
| FlushStateToDisk | 1.45% |
| LoadBlockIndex / init | 1.02% |
| UndoReadFromDisk / WriteUndo | 0.95% |
| CCoinsViewCache::FetchCoin | 0.57% |
| CEvoDB | 0.54% |
| llmq::* (any) | 0.44% |

| thread group | instr share |
|---|---|
| message handler (msghand) | 95.74% |
| shutdown | 1.50% |
| other: d-net | 1.39% |
| scheduler | 1.02% |
| init / main thread | 0.35% |
| LLMQ/DKG/sigs threads | 0.00% |

| top leaf symbols | share |
|---|---|
| sph_cubehash512_addbits_and_close | 17.86% |
| compress_big | 10.29% |
| jh_core | 9.31% |
| groestl_big_core | 7.15% |
| sph_luffa512_addbits_and_close | 4.82% |
| fft64.constprop.0 | 4.33% |
| groestl_big_close.constprop.0 | 3.56% |
| sph_cubehash512 | 3.36% |
| sph_luffa512 | 3.15% |
| Lloop1_30 | 3.11% |

### ibd-100k-noav-par1

3603.89G instructions:u, 100000 block = **36.04M/op**

| subsystem (instructions:u) | share |
|---|---|
| script-verify | 28.18% |
| x11 | 25.91% |
| bls | 25.59% |
| sha256 | 13.77% |
| tx-block-codec | 1.41% |
| validation | 0.96% |
| evodb/mn-lists | 0.84% |
| mempool | 0.68% |
| coins-cache | 0.63% |
| leveldb | 0.62% |
| rpc/http/net | 0.55% |
| block-io | 0.41% |

| inclusive (instructions:u) | share |
|---|---|
| ActivateBestChain | 45.81% |
| ConnectTip | 45.25% |
| ConnectBlock (whole) | 43.22% |
| CheckInputScripts / script checks | 40.43% |
| VerifyScript | 40.29% |
| ProcessSpecialTxsInBlock | 1.40% |
| UpdatedBlockTip / notifications | 0.94% |
| CDeterministicMNManager::ProcessBlock | 0.93% |
| CheckBlock (context-free) | 0.57% |
| LevelDB (any) | 0.55% |
| FlushStateToDisk | 0.38% |
| LoadBlockIndex / init | 0.36% |
| UndoReadFromDisk / WriteUndo | 0.35% |
| llmq::* (any) | 0.33% |

| thread group | instr share |
|---|---|
| message handler (msghand) | 98.50% |
| shutdown | 0.53% |
| other: d-net | 0.49% |
| scheduler | 0.36% |
| init / main thread | 0.12% |
| LLMQ/DKG/sigs threads | 0.00% |

| top leaf symbols | share |
|---|---|
| secp256k1_fe_mul_inner | 12.80% |
| secp256k1_fe_sqr_inner | 9.78% |
| Lloop1_30 | 9.54% |
| __gmpn_mul_basecase_coreisbr | 6.90% |
| __gmpn_addmul_1_coreisbr | 6.72% |
| sph_cubehash512_addbits_and_close | 6.24% |
| compress_big | 3.64% |
| jh_core | 3.28% |
| Lloop2_30 | 2.53% |
| groestl_big_core | 2.49% |

### regtest-gbt-20000

3.58G instructions:u, 6 getblocktemplate = **596.00M/op**

| subsystem (instructions:u) | share |
|---|---|
| rpc/http/net | 58.89% |
| leveldb | 9.40% |
| tx-block-codec | 9.23% |
| coins-cache | 9.06% |
| validation | 4.03% |
| mempool | 4.03% |
| sha256 | 2.68% |
| script-verify | 1.51% |
| evodb/mn-lists | 0.50% |
| llmq | 0.34% |
| special-tx | 0.34% |

| inclusive (instructions:u) | share |
|---|---|
| getblocktemplate RPC | 66.78% |
| CreateNewBlock | 35.23% |
| CEvoDB | 28.52% |
| TestBlockValidity | 28.52% |
| ConnectBlock (whole) | 25.00% |
| CCoinsViewCache::FetchCoin | 15.44% |
| LevelDB (any) | 8.56% |
| CheckBlock (context-free) | 3.36% |
| UpdateCoins / AddCoins / SpendCoin | 2.85% |
| CheckInputScripts / script checks | 2.85% |
| llmq::* (any) | 0.34% |
| CMNHFManager | 0.34% |
| ProcessSpecialTxsInBlock | 0.34% |
| CheckSpecialTx | 0.17% |

| thread group | instr share |
|---|---|
| http/rpc workers | 100.00% |

| top leaf symbols | share |
|---|---|
| json_escape(std::__cxx11::basic_string<char, std::char_traits<char>, s | 19.97% |
| __memmove_avx_unaligned_erms_rtm | 5.54% |
| SipHashUint256Extra(unsigned long, unsigned long, uint256 const&, unsi | 4.70% |
| void std::vector<std::byte, zero_after_free_allocator<std::byte> >::_M | 3.69% |
| __memcmp_avx2_movbe_rtm | 3.69% |
| malloc | 3.19% |
| cfree@GLIBC_2.2.5 | 2.68% |
| _int_free_chunk | 2.68% |
| std::vector<std::__cxx11::basic_string<char, std::char_traits<char>, s | 2.35% |
| GetScriptOp(prevector<28u, unsigned char, unsigned int, int>::const_it | 2.01% |

### regtest-gbt-5000

1.82G instructions:u, 6 getblocktemplate = **303.00M/op**

| subsystem (instructions:u) | share |
|---|---|
| rpc/http/net | 65.02% |
| leveldb | 8.91% |
| tx-block-codec | 8.58% |
| coins-cache | 7.26% |
| mempool | 4.29% |
| sha256 | 1.65% |
| script-verify | 1.32% |
| validation | 0.99% |
| special-tx | 0.66% |
| llmq | 0.66% |
| evodb/mn-lists | 0.33% |
| x11 | 0.33% |

| inclusive (instructions:u) | share |
|---|---|
| getblocktemplate RPC | 63.70% |
| CreateNewBlock | 28.71% |
| CEvoDB | 23.76% |
| TestBlockValidity | 23.76% |
| ConnectBlock (whole) | 20.46% |
| CCoinsViewCache::FetchCoin | 12.54% |
| LevelDB (any) | 7.92% |
| UpdateCoins / AddCoins / SpendCoin | 3.63% |
| CheckBlock (context-free) | 2.31% |
| CheckInputScripts / script checks | 1.98% |
| ProcessSpecialTxsInBlock | 0.66% |
| llmq::* (any) | 0.66% |
| CheckSpecialTx | 0.33% |

| thread group | instr share |
|---|---|
| http/rpc workers | 100.00% |

| top leaf symbols | share |
|---|---|
| json_escape(std::__cxx11::basic_string<char, std::char_traits<char>, s | 23.76% |
| __memmove_avx_unaligned_erms_rtm | 4.95% |
| void std::vector<std::byte, zero_after_free_allocator<std::byte> >::_M | 4.62% |
| malloc | 3.63% |
| cfree@GLIBC_2.2.5 | 3.63% |
| __libc_malloc2 | 3.63% |
| __memcmp_avx2_movbe_rtm | 3.30% |
| UniValue::UniValue(UniValue const&) | 2.64% |
| UniValue::writeObject(unsigned int, unsigned int, std::__cxx11::basic_ | 2.64% |
| SipHashUint256Extra(unsigned long, unsigned long, uint256 const&, unsi | 2.64% |

### regtest-mempool-5000-c8

4.13G instructions:u, 5000 tx = **825.60k/op**

| subsystem (instructions:u) | share |
|---|---|
| script-verify | 47.67% |
| rpc/http/net | 16.57% |
| coins-cache | 11.05% |
| sha256 | 9.59% |
| leveldb | 4.51% |
| mempool | 3.34% |
| tx-block-codec | 2.62% |
| validation | 2.47% |
| memory/libc | 0.87% |
| ripemd160/hash160 | 0.58% |
| other | 0.29% |
| llmq | 0.29% |

| inclusive (instructions:u) | share |
|---|---|
| AcceptToMemoryPool | 75.73% |
| CheckInputScripts / script checks | 57.12% |
| VerifyScript | 55.81% |
| CCoinsViewCache::FetchCoin | 6.98% |
| LevelDB (any) | 4.07% |
| LoadBlockIndex / init | 1.16% |
| UpdatedBlockTip / notifications | 1.02% |
| llmq::* (any) | 0.29% |

| thread group | instr share |
|---|---|
| http/rpc workers | 98.84% |
| scheduler | 1.16% |

| top leaf symbols | share |
|---|---|
| secp256k1_fe_mul_inner | 20.35% |
| secp256k1_fe_sqr_inner | 17.88% |
| Lloop1_30 | 7.41% |
| (anonymous namespace)::sha512::Transform(unsigned long*, unsigned char | 6.25% |
| std::__cxx11::basic_string<char, std::char_traits<char>, std::allocato | 2.47% |
| secp256k1_gej_double | 2.18% |
| malloc | 2.03% |
| Lloop2_30 | 1.89% |
| SipHashUint256Extra(unsigned long, unsigned long, uint256 const&, unsi | 1.74% |
| ChaCha20Aligned::Keystream(Span<std::byte>) | 1.31% |

### regtest-mempool-5000

4.25G instructions:u, 5000 tx = **850.80k/op**

| subsystem (instructions:u) | share |
|---|---|
| script-verify | 47.67% |
| rpc/http/net | 14.25% |
| sha256 | 11.42% |
| coins-cache | 9.59% |
| leveldb | 5.92% |
| tx-block-codec | 3.39% |
| validation | 2.68% |
| mempool | 2.68% |
| ripemd160/hash160 | 0.71% |
| bls | 0.71% |
| memory/libc | 0.42% |
| other | 0.28% |

| inclusive (instructions:u) | share |
|---|---|
| AcceptToMemoryPool | 76.73% |
| CheckInputScripts / script checks | 58.39% |
| VerifyScript | 56.28% |
| CCoinsViewCache::FetchCoin | 7.19% |
| LevelDB (any) | 4.80% |
| LoadBlockIndex / init | 0.71% |
| UpdatedBlockTip / notifications | 0.42% |
| FlushStateToDisk | 0.28% |
| llmq::* (any) | 0.14% |
| CheckSpecialTx | 0.14% |

| thread group | instr share |
|---|---|
| http/rpc workers | 99.29% |
| scheduler | 0.71% |

| top leaf symbols | share |
|---|---|
| secp256k1_fe_mul_inner | 22.00% |
| secp256k1_fe_sqr_inner | 15.80% |
| Lloop1_30 | 8.60% |
| (anonymous namespace)::sha512::Transform(unsigned long*, unsigned char | 5.22% |
| secp256k1_gej_double | 2.82% |
| std::__cxx11::basic_string<char, std::char_traits<char>, std::allocato | 2.68% |
| Lloop2_30 | 1.83% |
| cfree@GLIBC_2.2.5 | 1.55% |
| JSONUTF8StringFilter::push_back(unsigned char) | 1.13% |
| __memcmp_avx2_movbe_rtm | 0.99% |

### startup-regtest

1.39G instructions:u

| subsystem (instructions:u) | share |
|---|---|
| other | 51.29% |
| leveldb | 38.79% |
| coins-cache | 3.02% |
| sha256 | 2.59% |
| block-io | 1.72% |
| rpc/http/net | 0.86% |
| evodb/mn-lists | 0.43% |
| memory/libc | 0.43% |
| tx-block-codec | 0.43% |
| script-verify | 0.43% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 43.53% |
| CEvoDB | 40.95% |
| LevelDB (any) | 37.07% |
| startup: VerifyDB (-checkblocks) | 29.74% |
| UpdateCoins / AddCoins / SpendCoin | 25.43% |
| CCoinsViewCache::FetchCoin | 25.43% |
| ReadBlockFromDisk | 3.45% |
| llmq::* (any) | 0.43% |
| UndoReadFromDisk / WriteUndo | 0.43% |
| CheckBlock (context-free) | 0.43% |

| thread group | instr share |
|---|---|
| init / main thread | 99.57% |
| shutdown | 0.43% |

| top leaf symbols | share |
|---|---|
| (anonymous namespace)::sha512::Transform(unsigned long*, unsigned char | 46.12% |
| __memcmp_avx2_movbe_rtm | 6.47% |
| leveldb::InternalKeyComparator::Compare(leveldb::Slice const&, leveldb | 3.45% |
| CSHA512::Write(unsigned char const*, unsigned long) | 3.02% |
| leveldb::SkipList<char const*, leveldb::MemTable::KeyComparator>::Find | 2.59% |
| leveldb::BlockBuilder::Add(leveldb::Slice const&, leveldb::Slice const | 2.59% |
| malloc | 2.16% |
| Lloop1_30 | 2.16% |
| crc32c::ExtendSse42(unsigned int, unsigned char const*, unsigned long) | 2.16% |
| SipHashUint256Extra(unsigned long, unsigned long, uint256 const&, unsi | 2.16% |

### startup-testnet

35.18G instructions:u

| subsystem (instructions:u) | share |
|---|---|
| validation | 81.48% |
| leveldb | 11.49% |
| tx-block-codec | 3.46% |
| other | 2.40% |
| bls | 0.38% |
| evodb/mn-lists | 0.32% |
| rpc/http/net | 0.15% |
| sha256 | 0.14% |
| block-io | 0.09% |
| memory/libc | 0.05% |
| script-verify | 0.03% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 92.80% |
| CEvoDB | 42.45% |
| startup: LoadBlockIndexGuts | 27.51% |
| LevelDB (any) | 11.22% |
| startup: LoadChainTip / ReplayBlocks / RollforwardBlock | 2.27% |
| startup: VerifyDB (-checkblocks) | 0.22% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.09% |
| ReadBlockFromDisk | 0.03% |
| UndoReadFromDisk / WriteUndo | 0.03% |
| CCoinsViewCache::FetchCoin | 0.03% |
| CheckInputScripts / script checks | 0.02% |
| VerifyScript | 0.02% |
| AcceptToMemoryPool | 0.02% |

| thread group | instr share |
|---|---|
| init / main thread | 98.00% |
| shutdown | 1.88% |
| http/rpc workers | 0.09% |
| block import / ActivateBestChain (loadblk) | 0.03% |

| top leaf symbols | share |
|---|---|
| base_uint<256u>::operator>>=(unsigned int) | 20.53% |
| base_uint<256u>::CompareTo(base_uint<256u> const&) const | 12.14% |
| CBlockTreeDB::LoadBlockIndexGuts(Consensus::Params const&, std::functi | 9.94% |
| base_uint<256u>::operator/=(base_uint<256u> const&) | 7.67% |
| node::CBlockIndexWorkComparator::operator()(CBlockIndex const*, CBlock | 5.66% |
| std::_Rb_tree<CBlockIndex*, CBlockIndex*, std::_Identity<CBlockIndex*> | 4.40% |
| (anonymous namespace)::sha512::Transform(unsigned long*, unsigned char | 2.25% |
| __libc_malloc2 | 1.77% |
| _int_free_chunk | 1.57% |
| GetBlockProof(CBlockIndex const&) | 1.52% |

### w1140000-par8-noav

1999 blocks, 353.56G instructions:u = **176.87M/block**; 173.50M/block without the unnamed LevelDB background thread; 1.69 tx/blk, 0.65 txin/blk; CPU 60.2 s over 46.7 s wall (parallelism 1.29)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.80% |
| leveldb | 2.50% |
| sha256 | 0.59% |
| x11 | 0.49% |
| evodb/mn-lists | 0.45% |
| logging | 0.32% |
| llmq | 0.19% |
| memory/libc | 0.19% |
| script-verify | 0.15% |
| rpc/http/net | 0.13% |
| validation | 0.06% |
| coins-cache | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 2.84% |
| ActivateBestChain | 2.83% |
| ConnectTip | 2.81% |
| ConnectBlock (whole) | 2.49% |
| LevelDB (any) | 2.39% |
| ProcessSpecialTxsInBlock | 2.05% |
| llmq::* (any) | 1.56% |
| LogPrintf | 0.33% |
| ReadBlockFromDisk | 0.29% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.27% |
| CDeterministicMNManager::ProcessBlock | 0.24% |
| CheckInputScripts / script checks | 0.21% |
| CheckCbTx | 0.19% |
| CChainLocks / VerifyChainLock | 0.19% |

| thread group | instr share |
|---|---|
| script/BLS check workers (scriptch.N) | 51.24% |
| block import / ActivateBestChain (loadblk) | 46.84% |
| unnamed background thread (LevelDB compaction) | 1.90% |
| scheduler | 0.02% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.53% |
| __gmpn_addmul_1_coreisbr | 23.89% |
| dv_cmp | 7.57% |
| fp_rdcn_low | 5.83% |
| __gmpn_add_n_coreisbr | 4.94% |
| core_get | 4.59% |
| fp_prime_get | 3.41% |
| fp_addm_low | 2.42% |
| __gmpn_sub_n_coreisbr | 2.37% |
| __gmpn_mul_n | 1.23% |

### w1140000-serial-av-nobench

1999 blocks, 43.18G instructions:u = **21.60M/block**; 18.25M/block without the unnamed LevelDB background thread; CPU 8.6 s over 7.8 s wall (parallelism 1.1)

| subsystem (instructions:u) | share |
|---|---|
| bls | 72.95% |
| leveldb | 15.83% |
| x11 | 4.21% |
| evodb/mn-lists | 4.13% |
| sha256 | 1.57% |
| validation | 0.53% |
| block-io | 0.19% |
| llmq | 0.18% |
| coins-cache | 0.17% |
| tx-block-codec | 0.15% |
| logging | 0.08% |
| special-tx | 0.01% |

| inclusive (instructions:u) | share |
|---|---|
| LevelDB (any) | 15.77% |
| LoadBlockIndex / init | 11.63% |
| ActivateBestChain | 11.56% |
| ConnectTip | 11.42% |
| ConnectBlock (whole) | 9.37% |
| ProcessSpecialTxsInBlock | 6.84% |
| llmq::* (any) | 3.35% |
| ReadBlockFromDisk | 2.29% |
| CDeterministicMNManager::ProcessBlock | 2.24% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 2.13% |
| CCreditPoolManager | 1.18% |
| CheckBlock (context-free) | 0.96% |
| UpdatedBlockTip / notifications | 0.85% |
| CheckSpecialTx | 0.76% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 84.42% |
| unnamed background thread (LevelDB compaction) | 15.51% |
| scheduler | 0.07% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 20.24% |
| __gmpn_addmul_1_coreisbr | 19.49% |
| dv_cmp | 5.25% |
| fp_rdcn_low | 5.22% |
| __gmpn_add_n_coreisbr | 3.29% |
| core_get | 2.90% |
| fp_prime_get | 1.99% |
| fp_addm_low | 1.75% |
| __gmpn_sub_n_coreisbr | 1.63% |
| __memcmp_avx2_movbe_rtm | 1.39% |

### w1140000-serial-av

1999 blocks, 44.14G instructions:u = **22.08M/block**; 18.72M/block without the unnamed LevelDB background thread; 1.69 tx/blk, 0.65 txin/blk; CPU 10.3 s over 10.1 s wall (parallelism 1.02)

| subsystem (instructions:u) | share |
|---|---|
| bls | 71.51% |
| leveldb | 15.54% |
| x11 | 4.09% |
| evodb/mn-lists | 3.79% |
| logging | 2.05% |
| sha256 | 1.64% |
| validation | 0.42% |
| coins-cache | 0.37% |
| tx-block-codec | 0.16% |
| block-io | 0.15% |
| llmq | 0.12% |
| memory/libc | 0.08% |

| inclusive (instructions:u) | share |
|---|---|
| LevelDB (any) | 15.48% |
| LoadBlockIndex / init | 13.40% |
| ActivateBestChain | 13.34% |
| ConnectTip | 13.21% |
| ConnectBlock (whole) | 10.40% |
| ProcessSpecialTxsInBlock | 6.99% |
| llmq::* (any) | 3.33% |
| ReadBlockFromDisk | 2.50% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 2.16% |
| LogPrintf | 2.08% |
| CDeterministicMNManager::ProcessBlock | 2.00% |
| CCreditPoolManager | 1.20% |
| UpdatedBlockTip / notifications | 0.92% |
| CheckBlock (context-free) | 0.83% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 84.72% |
| unnamed background thread (LevelDB compaction) | 15.21% |
| scheduler | 0.07% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 19.87% |
| __gmpn_addmul_1_coreisbr | 19.18% |
| fp_rdcn_low | 4.95% |
| dv_cmp | 4.45% |
| __gmpn_add_n_coreisbr | 3.38% |
| core_get | 3.02% |
| fp_addm_low | 2.19% |
| fp_prime_get | 2.08% |
| __gmpn_sub_n_coreisbr | 1.60% |
| __memmove_avx_unaligned_erms_rtm | 1.55% |

### w1140000-serial-noav-nobench

1999 blocks, 352.31G instructions:u = **176.24M/block**; 172.87M/block without the unnamed LevelDB background thread; CPU 57.6 s over 60.3 s wall (parallelism 0.96)

| subsystem (instructions:u) | share |
|---|---|
| bls | 95.13% |
| leveldb | 2.48% |
| sha256 | 0.57% |
| x11 | 0.53% |
| evodb/mn-lists | 0.43% |
| llmq | 0.20% |
| script-verify | 0.17% |
| memory/libc | 0.15% |
| rpc/http/net | 0.12% |
| validation | 0.08% |
| block-io | 0.04% |
| tx-block-codec | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 2.73% |
| ActivateBestChain | 2.70% |
| ConnectTip | 2.69% |
| ConnectBlock (whole) | 2.41% |
| LevelDB (any) | 2.37% |
| ProcessSpecialTxsInBlock | 1.96% |
| llmq::* (any) | 1.53% |
| ReadBlockFromDisk | 0.33% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.26% |
| CDeterministicMNManager::ProcessBlock | 0.23% |
| CheckInputScripts / script checks | 0.23% |
| VerifyScript | 0.20% |
| CCreditPoolManager | 0.17% |
| CChainLocks / VerifyChainLock | 0.15% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.06% |
| unnamed background thread (LevelDB compaction) | 1.91% |
| scheduler | 0.03% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.72% |
| __gmpn_addmul_1_coreisbr | 24.18% |
| dv_cmp | 7.45% |
| fp_rdcn_low | 5.81% |
| __gmpn_add_n_coreisbr | 5.05% |
| core_get | 4.48% |
| fp_prime_get | 3.22% |
| __gmpn_sub_n_coreisbr | 2.51% |
| fp_addm_low | 2.45% |
| __gmpn_mul_n | 1.25% |

### w1140000-serial-noav

1999 blocks, 353.45G instructions:u = **176.82M/block**; 173.44M/block without the unnamed LevelDB background thread; 1.69 tx/blk, 0.65 txin/blk; CPU 57.7 s over 58.1 s wall (parallelism 0.99)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.58% |
| leveldb | 2.58% |
| sha256 | 0.58% |
| x11 | 0.57% |
| evodb/mn-lists | 0.48% |
| logging | 0.30% |
| llmq | 0.20% |
| memory/libc | 0.18% |
| script-verify | 0.16% |
| rpc/http/net | 0.14% |
| validation | 0.08% |
| coins-cache | 0.05% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.23% |
| ActivateBestChain | 3.21% |
| ConnectTip | 3.20% |
| ConnectBlock (whole) | 2.82% |
| LevelDB (any) | 2.46% |
| ProcessSpecialTxsInBlock | 2.20% |
| llmq::* (any) | 1.65% |
| ReadBlockFromDisk | 0.33% |
| LogPrintf | 0.31% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.26% |
| CDeterministicMNManager::ProcessBlock | 0.24% |
| CheckInputScripts / script checks | 0.23% |
| VerifyScript | 0.21% |
| CChainLocks / VerifyChainLock | 0.19% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.07% |
| unnamed background thread (LevelDB compaction) | 1.91% |
| scheduler | 0.02% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.49% |
| __gmpn_addmul_1_coreisbr | 24.21% |
| dv_cmp | 7.49% |
| fp_rdcn_low | 5.81% |
| __gmpn_add_n_coreisbr | 4.98% |
| core_get | 4.38% |
| fp_prime_get | 3.23% |
| __gmpn_sub_n_coreisbr | 2.42% |
| fp_addm_low | 2.41% |
| __gmpn_mul_n | 1.35% |

### w1340000-par8-noav

1999 blocks, 320.45G instructions:u = **160.30M/block**; 157.46M/block without the unnamed LevelDB background thread; 1.8 tx/blk, 0.08 txin/blk; CPU 54.8 s over 47.2 s wall (parallelism 1.16)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.55% |
| leveldb | 2.68% |
| sha256 | 0.61% |
| x11 | 0.57% |
| evodb/mn-lists | 0.51% |
| logging | 0.35% |
| llmq | 0.20% |
| memory/libc | 0.17% |
| validation | 0.10% |
| rpc/http/net | 0.08% |
| tx-block-codec | 0.05% |
| coins-cache | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.42% |
| ActivateBestChain | 3.40% |
| ConnectTip | 3.38% |
| ConnectBlock (whole) | 3.01% |
| LevelDB (any) | 2.54% |
| ProcessSpecialTxsInBlock | 2.53% |
| llmq::* (any) | 1.96% |
| LogPrintf | 0.36% |
| ReadBlockFromDisk | 0.32% |
| CDeterministicMNManager::ProcessBlock | 0.30% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.28% |
| CCreditPoolManager | 0.16% |
| CheckCbTx | 0.16% |
| CChainLocks / VerifyChainLock | 0.16% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 57.26% |
| script/BLS check workers (scriptch.N) | 40.95% |
| unnamed background thread (LevelDB compaction) | 1.77% |
| scheduler | 0.01% |
| LLMQ/DKG/sigs threads | 0.00% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.79% |
| __gmpn_addmul_1_coreisbr | 23.70% |
| dv_cmp | 7.25% |
| fp_rdcn_low | 5.93% |
| __gmpn_add_n_coreisbr | 4.99% |
| core_get | 4.50% |
| fp_prime_get | 3.28% |
| fp_addm_low | 2.61% |
| __gmpn_sub_n_coreisbr | 2.44% |
| __gmpn_mul_n | 1.19% |

### w1340000-serial-av-nobench

1999 blocks, 38.09G instructions:u = **19.06M/block**; 16.32M/block without the unnamed LevelDB background thread; CPU 9.8 s over 10.1 s wall (parallelism 0.97)

| subsystem (instructions:u) | share |
|---|---|
| bls | 72.37% |
| leveldb | 14.55% |
| x11 | 5.34% |
| evodb/mn-lists | 4.17% |
| sha256 | 1.59% |
| validation | 0.60% |
| block-io | 0.38% |
| tx-block-codec | 0.24% |
| logging | 0.22% |
| llmq | 0.20% |
| coins-cache | 0.19% |
| special-tx | 0.05% |

| inclusive (instructions:u) | share |
|---|---|
| LevelDB (any) | 14.47% |
| LoadBlockIndex / init | 13.40% |
| ActivateBestChain | 13.25% |
| ConnectTip | 13.12% |
| ConnectBlock (whole) | 10.46% |
| ProcessSpecialTxsInBlock | 7.26% |
| llmq::* (any) | 3.34% |
| ReadBlockFromDisk | 2.98% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 2.41% |
| CDeterministicMNManager::ProcessBlock | 2.14% |
| CCreditPoolManager | 1.73% |
| CheckBlock (context-free) | 1.45% |
| UpdatedBlockTip / notifications | 1.13% |
| CEvoDB | 0.61% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 85.48% |
| unnamed background thread (LevelDB compaction) | 14.36% |
| scheduler | 0.16% |

| top leaf symbols | share |
|---|---|
| __gmpn_addmul_1_coreisbr | 19.63% |
| __gmpn_mul_basecase_coreisbr | 18.76% |
| fp_rdcn_low | 5.69% |
| dv_cmp | 4.49% |
| __gmpn_add_n_coreisbr | 3.62% |
| core_get | 2.99% |
| fp_prime_get | 2.11% |
| fp_addm_low | 2.05% |
| __memmove_avx_unaligned_erms_rtm | 1.78% |
| __gmpn_sub_n_coreisbr | 1.53% |

### w1340000-serial-av

1999 blocks, 39.71G instructions:u = **19.87M/block**; 16.81M/block without the unnamed LevelDB background thread; 1.8 tx/blk, 0.08 txin/blk; CPU 9.5 s over 10.1 s wall (parallelism 0.94)

| subsystem (instructions:u) | share |
|---|---|
| bls | 69.81% |
| leveldb | 15.88% |
| x11 | 4.52% |
| evodb/mn-lists | 4.22% |
| logging | 2.08% |
| sha256 | 1.71% |
| validation | 0.50% |
| block-io | 0.42% |
| coins-cache | 0.33% |
| llmq | 0.26% |
| tx-block-codec | 0.11% |
| memory/libc | 0.08% |

| inclusive (instructions:u) | share |
|---|---|
| LevelDB (any) | 15.79% |
| LoadBlockIndex / init | 14.91% |
| ActivateBestChain | 14.79% |
| ConnectTip | 14.64% |
| ConnectBlock (whole) | 11.66% |
| ProcessSpecialTxsInBlock | 7.84% |
| llmq::* (any) | 3.46% |
| ReadBlockFromDisk | 2.64% |
| CDeterministicMNManager::ProcessBlock | 2.36% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 2.22% |
| LogPrintf | 2.15% |
| CCreditPoolManager | 1.45% |
| CheckBlock (context-free) | 0.95% |
| CEvoDB | 0.86% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 84.47% |
| unnamed background thread (LevelDB compaction) | 15.41% |
| scheduler | 0.12% |

| top leaf symbols | share |
|---|---|
| __gmpn_addmul_1_coreisbr | 19.22% |
| __gmpn_mul_basecase_coreisbr | 19.01% |
| fp_rdcn_low | 5.42% |
| dv_cmp | 4.67% |
| __gmpn_add_n_coreisbr | 3.44% |
| core_get | 2.69% |
| fp_prime_get | 1.98% |
| __memmove_avx_unaligned_erms_rtm | 1.92% |
| fp_addm_low | 1.57% |
| __memcmp_avx2_movbe_rtm | 1.56% |

### w1340000-serial-noav-nobench

1999 blocks, 318.94G instructions:u = **159.55M/block**; 156.72M/block without the unnamed LevelDB background thread; CPU 56.4 s over 58.9 s wall (parallelism 0.96)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.94% |
| leveldb | 2.65% |
| sha256 | 0.53% |
| x11 | 0.52% |
| evodb/mn-lists | 0.52% |
| llmq | 0.21% |
| memory/libc | 0.20% |
| rpc/http/net | 0.11% |
| validation | 0.10% |
| block-io | 0.05% |
| tx-block-codec | 0.05% |
| script-verify | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.00% |
| ActivateBestChain | 2.98% |
| ConnectTip | 2.96% |
| ConnectBlock (whole) | 2.66% |
| LevelDB (any) | 2.49% |
| ProcessSpecialTxsInBlock | 2.37% |
| llmq::* (any) | 1.87% |
| ReadBlockFromDisk | 0.32% |
| CDeterministicMNManager::ProcessBlock | 0.28% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.28% |
| CCreditPoolManager | 0.21% |
| CChainLocks / VerifyChainLock | 0.17% |
| CheckCbTx | 0.17% |
| UpdatedBlockTip / notifications | 0.10% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.20% |
| unnamed background thread (LevelDB compaction) | 1.78% |
| scheduler | 0.02% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.08% |
| __gmpn_addmul_1_coreisbr | 23.87% |
| dv_cmp | 7.64% |
| fp_rdcn_low | 6.11% |
| __gmpn_add_n_coreisbr | 5.09% |
| core_get | 4.63% |
| fp_prime_get | 3.28% |
| fp_addm_low | 2.58% |
| __gmpn_sub_n_coreisbr | 2.49% |
| __gmpn_mul_n | 1.25% |

### w1340000-serial-noav

1999 blocks, 319.93G instructions:u = **160.05M/block**; 157.32M/block without the unnamed LevelDB background thread; 1.8 tx/blk, 0.08 txin/blk; CPU 51.3 s over 53.4 s wall (parallelism 0.96)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.49% |
| leveldb | 2.60% |
| sha256 | 0.61% |
| x11 | 0.59% |
| evodb/mn-lists | 0.57% |
| logging | 0.35% |
| llmq | 0.21% |
| memory/libc | 0.17% |
| rpc/http/net | 0.13% |
| validation | 0.11% |
| tx-block-codec | 0.05% |
| coins-cache | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.57% |
| ActivateBestChain | 3.54% |
| ConnectTip | 3.51% |
| ConnectBlock (whole) | 3.09% |
| ProcessSpecialTxsInBlock | 2.61% |
| LevelDB (any) | 2.43% |
| llmq::* (any) | 2.00% |
| LogPrintf | 0.36% |
| ReadBlockFromDisk | 0.33% |
| CDeterministicMNManager::ProcessBlock | 0.32% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.28% |
| CCreditPoolManager | 0.18% |
| CChainLocks / VerifyChainLock | 0.17% |
| CheckCbTx | 0.17% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.26% |
| unnamed background thread (LevelDB compaction) | 1.70% |
| scheduler | 0.04% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.72% |
| __gmpn_addmul_1_coreisbr | 24.22% |
| dv_cmp | 7.34% |
| fp_rdcn_low | 5.63% |
| __gmpn_add_n_coreisbr | 4.76% |
| core_get | 4.27% |
| fp_prime_get | 3.23% |
| fp_addm_low | 2.48% |
| __gmpn_sub_n_coreisbr | 2.47% |
| __gmpn_mul_n | 1.29% |

### w1520000-par8-noav

1999 blocks, 353.80G instructions:u = **176.99M/block**; 173.69M/block without the unnamed LevelDB background thread; 1.83 tx/blk, 1.19 txin/blk; CPU 66.6 s over 51.1 s wall (parallelism 1.3)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.58% |
| leveldb | 2.50% |
| sha256 | 0.62% |
| x11 | 0.55% |
| evodb/mn-lists | 0.41% |
| logging | 0.32% |
| script-verify | 0.29% |
| llmq | 0.22% |
| memory/libc | 0.16% |
| rpc/http/net | 0.12% |
| validation | 0.10% |
| coins-cache | 0.05% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 2.99% |
| ActivateBestChain | 2.96% |
| ConnectTip | 2.94% |
| ConnectBlock (whole) | 2.56% |
| LevelDB (any) | 2.38% |
| ProcessSpecialTxsInBlock | 2.07% |
| llmq::* (any) | 1.62% |
| CheckInputScripts / script checks | 0.40% |
| VerifyScript | 0.38% |
| LogPrintf | 0.33% |
| ReadBlockFromDisk | 0.33% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.25% |
| CDeterministicMNManager::ProcessBlock | 0.18% |
| CCreditPoolManager | 0.18% |

| thread group | instr share |
|---|---|
| script/BLS check workers (scriptch.N) | 51.64% |
| block import / ActivateBestChain (loadblk) | 46.47% |
| unnamed background thread (LevelDB compaction) | 1.86% |
| scheduler | 0.03% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.17% |
| __gmpn_addmul_1_coreisbr | 23.89% |
| dv_cmp | 7.57% |
| fp_rdcn_low | 6.18% |
| __gmpn_add_n_coreisbr | 5.11% |
| core_get | 4.66% |
| fp_prime_get | 3.20% |
| fp_addm_low | 2.43% |
| __gmpn_sub_n_coreisbr | 2.29% |
| __gmpn_mul_n | 1.28% |

### w1520000-serial-av-nobench

1999 blocks, 352.54G instructions:u = **176.36M/block**; 173.11M/block without the unnamed LevelDB background thread; CPU 61.8 s over 65.7 s wall (parallelism 0.94)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.78% |
| leveldb | 2.55% |
| sha256 | 0.66% |
| x11 | 0.59% |
| evodb/mn-lists | 0.40% |
| script-verify | 0.32% |
| llmq | 0.18% |
| memory/libc | 0.17% |
| rpc/http/net | 0.13% |
| validation | 0.10% |
| coins-cache | 0.03% |
| block-io | 0.03% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.10% |
| ActivateBestChain | 3.08% |
| ConnectTip | 3.07% |
| ConnectBlock (whole) | 2.73% |
| LevelDB (any) | 2.40% |
| ProcessSpecialTxsInBlock | 2.01% |
| llmq::* (any) | 1.63% |
| CheckInputScripts / script checks | 0.43% |
| VerifyScript | 0.40% |
| ReadBlockFromDisk | 0.34% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.27% |
| CDeterministicMNManager::ProcessBlock | 0.19% |
| CCreditPoolManager | 0.18% |
| CheckCbTx | 0.16% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.13% |
| unnamed background thread (LevelDB compaction) | 1.84% |
| scheduler | 0.03% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.70% |
| __gmpn_addmul_1_coreisbr | 24.13% |
| dv_cmp | 7.49% |
| fp_rdcn_low | 6.15% |
| __gmpn_add_n_coreisbr | 5.03% |
| core_get | 4.60% |
| fp_prime_get | 3.25% |
| fp_addm_low | 2.41% |
| __gmpn_sub_n_coreisbr | 2.36% |
| __gmpn_mul_n | 1.15% |

### w1520000-serial-av

1999 blocks, 353.68G instructions:u = **176.93M/block**; 173.66M/block without the unnamed LevelDB background thread; 1.83 tx/blk, 1.19 txin/blk; CPU 63.8 s over 67.2 s wall (parallelism 0.95)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.59% |
| leveldb | 2.52% |
| sha256 | 0.68% |
| x11 | 0.52% |
| evodb/mn-lists | 0.39% |
| logging | 0.30% |
| script-verify | 0.29% |
| llmq | 0.21% |
| memory/libc | 0.16% |
| validation | 0.11% |
| rpc/http/net | 0.10% |
| coins-cache | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.35% |
| ActivateBestChain | 3.33% |
| ConnectTip | 3.32% |
| ConnectBlock (whole) | 2.93% |
| LevelDB (any) | 2.41% |
| ProcessSpecialTxsInBlock | 2.13% |
| llmq::* (any) | 1.72% |
| CheckInputScripts / script checks | 0.42% |
| VerifyScript | 0.39% |
| LogPrintf | 0.31% |
| ReadBlockFromDisk | 0.28% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.26% |
| CDeterministicMNManager::ProcessBlock | 0.21% |
| CheckCbTx | 0.18% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.13% |
| unnamed background thread (LevelDB compaction) | 1.85% |
| scheduler | 0.02% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.18% |
| __gmpn_addmul_1_coreisbr | 24.05% |
| dv_cmp | 7.50% |
| fp_rdcn_low | 6.25% |
| __gmpn_add_n_coreisbr | 5.09% |
| core_get | 4.57% |
| fp_prime_get | 3.24% |
| __gmpn_sub_n_coreisbr | 2.44% |
| fp_addm_low | 2.33% |
| __gmpn_mul_n | 1.25% |

### w1520000-serial-noav-nobench

1999 blocks, 352.57G instructions:u = **176.37M/block**; 173.06M/block without the unnamed LevelDB background thread; CPU 64.1 s over 66.8 s wall (parallelism 0.96)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.86% |
| leveldb | 2.54% |
| sha256 | 0.61% |
| x11 | 0.50% |
| evodb/mn-lists | 0.40% |
| script-verify | 0.30% |
| llmq | 0.21% |
| memory/libc | 0.19% |
| rpc/http/net | 0.13% |
| validation | 0.09% |
| tx-block-codec | 0.06% |
| coins-cache | 0.04% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 2.94% |
| ActivateBestChain | 2.91% |
| ConnectTip | 2.89% |
| ConnectBlock (whole) | 2.59% |
| LevelDB (any) | 2.42% |
| ProcessSpecialTxsInBlock | 1.94% |
| llmq::* (any) | 1.61% |
| CheckInputScripts / script checks | 0.41% |
| VerifyScript | 0.38% |
| ReadBlockFromDisk | 0.27% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.26% |
| CDeterministicMNManager::ProcessBlock | 0.19% |
| CheckCbTx | 0.17% |
| CChainLocks / VerifyChainLock | 0.17% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.10% |
| unnamed background thread (LevelDB compaction) | 1.88% |
| scheduler | 0.03% |
| LLMQ/DKG/sigs threads | 0.00% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.47% |
| __gmpn_addmul_1_coreisbr | 23.91% |
| dv_cmp | 7.59% |
| fp_rdcn_low | 6.27% |
| __gmpn_add_n_coreisbr | 5.10% |
| core_get | 4.76% |
| fp_prime_get | 3.21% |
| __gmpn_sub_n_coreisbr | 2.36% |
| fp_addm_low | 2.30% |
| __gmpn_mul_n | 1.16% |

### w1520000-serial-noav

1999 blocks, 353.62G instructions:u = **176.90M/block**; 173.63M/block without the unnamed LevelDB background thread; 1.83 tx/blk, 1.19 txin/blk; CPU 62.9 s over 66.7 s wall (parallelism 0.94)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.60% |
| leveldb | 2.50% |
| sha256 | 0.64% |
| x11 | 0.49% |
| evodb/mn-lists | 0.38% |
| script-verify | 0.33% |
| logging | 0.33% |
| llmq | 0.19% |
| memory/libc | 0.17% |
| rpc/http/net | 0.13% |
| validation | 0.09% |
| tx-block-codec | 0.05% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.28% |
| ActivateBestChain | 3.26% |
| ConnectTip | 3.25% |
| ConnectBlock (whole) | 2.91% |
| LevelDB (any) | 2.38% |
| ProcessSpecialTxsInBlock | 2.07% |
| llmq::* (any) | 1.65% |
| CheckInputScripts / script checks | 0.43% |
| VerifyScript | 0.41% |
| LogPrintf | 0.33% |
| ReadBlockFromDisk | 0.27% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.25% |
| CDeterministicMNManager::ProcessBlock | 0.18% |
| CChainLocks / VerifyChainLock | 0.16% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.14% |
| unnamed background thread (LevelDB compaction) | 1.85% |
| scheduler | 0.02% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.23% |
| __gmpn_addmul_1_coreisbr | 23.72% |
| dv_cmp | 7.66% |
| fp_rdcn_low | 6.18% |
| __gmpn_add_n_coreisbr | 5.10% |
| core_get | 4.64% |
| fp_prime_get | 3.24% |
| fp_addm_low | 2.43% |
| __gmpn_sub_n_coreisbr | 2.39% |
| __gmpn_mul_n | 1.24% |

### w421000-par8-noav

1999 blocks, 79.09G instructions:u = **39.57M/block**; 37.10M/block without the unnamed LevelDB background thread; 1.62 tx/blk, 2.96 txin/blk; CPU 13.5 s over 11.3 s wall (parallelism 1.2)

| subsystem (instructions:u) | share |
|---|---|
| bls | 78.57% |
| leveldb | 7.08% |
| sha256 | 4.84% |
| script-verify | 3.65% |
| x11 | 1.77% |
| evodb/mn-lists | 1.34% |
| logging | 1.31% |
| llmq | 0.30% |
| coins-cache | 0.24% |
| validation | 0.21% |
| tx-block-codec | 0.16% |
| rpc/http/net | 0.16% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 8.05% |
| ActivateBestChain | 8.02% |
| ConnectTip | 7.98% |
| CheckInputScripts / script checks | 7.46% |
| VerifyScript | 7.43% |
| LevelDB (any) | 6.87% |
| ConnectBlock (whole) | 6.40% |
| ProcessSpecialTxsInBlock | 3.85% |
| llmq::* (any) | 2.34% |
| LogPrintf | 1.36% |
| CDeterministicMNManager::ProcessBlock | 1.09% |
| ReadBlockFromDisk | 0.65% |
| CheckBlock (context-free) | 0.52% |
| UpdatedBlockTip / notifications | 0.39% |

| thread group | instr share |
|---|---|
| script/BLS check workers (scriptch.N) | 70.93% |
| block import / ActivateBestChain (loadblk) | 22.80% |
| unnamed background thread (LevelDB compaction) | 6.22% |
| scheduler | 0.04% |
| init / main thread | 0.01% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 21.35% |
| __gmpn_addmul_1_coreisbr | 21.25% |
| dv_cmp | 5.78% |
| fp_rdcn_low | 4.97% |
| core_get | 3.47% |
| __gmpn_add_n_coreisbr | 3.36% |
| Lloop1_30 | 3.10% |
| fp_prime_get | 2.64% |
| fp_addm_low | 1.98% |
| secp256k1_fe_mul_inner | 1.49% |

### w421000-serial-av-nobench

1999 blocks, 8.20G instructions:u = **4.10M/block**; 2.66M/block without the unnamed LevelDB background thread; CPU 2.1 s over 2.6 s wall (parallelism 0.82)

| subsystem (instructions:u) | share |
|---|---|
| leveldb | 35.99% |
| bls | 23.77% |
| x11 | 16.68% |
| evodb/mn-lists | 12.22% |
| sha256 | 4.46% |
| validation | 2.12% |
| coins-cache | 1.83% |
| block-io | 1.10% |
| tx-block-codec | 0.73% |
| logging | 0.51% |
| llmq | 0.37% |
| mempool | 0.07% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 41.33% |
| ActivateBestChain | 40.60% |
| ConnectTip | 39.87% |
| LevelDB (any) | 35.84% |
| ConnectBlock (whole) | 26.99% |
| ProcessSpecialTxsInBlock | 14.19% |
| CDeterministicMNManager::ProcessBlock | 10.53% |
| ReadBlockFromDisk | 5.63% |
| CheckBlock (context-free) | 5.27% |
| UpdatedBlockTip / notifications | 4.61% |
| llmq::* (any) | 3.80% |
| CheckSpecialTx | 2.85% |
| CEvoDB | 1.68% |
| IsBlockValueValid / payee | 1.68% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 64.08% |
| unnamed background thread (LevelDB compaction) | 35.19% |
| scheduler | 0.73% |

| top leaf symbols | share |
|---|---|
| __gmpn_addmul_1_coreisbr | 7.32% |
| __gmpn_mul_basecase_coreisbr | 7.17% |
| leveldb::BlockBuilder::Add(leveldb::Slice const&, leveldb::Slice const | 4.61% |
| sph_cubehash512_addbits_and_close | 4.32% |
| Lloop1_30 | 3.44% |
| __memmove_avx_unaligned_erms_rtm | 3.37% |
| __memcmp_avx2_movbe_rtm | 3.29% |
| immer::detail::hamts::champ_iterator<std::pair<uint256, std::shared_pt | 2.49% |
| leveldb::(anonymous namespace)::BytewiseComparatorImpl::Compare(leveld | 2.41% |
| fp_rdcn_low | 2.12% |

### w421000-serial-av

1999 blocks, 9.76G instructions:u = **4.88M/block**; 3.09M/block without the unnamed LevelDB background thread; 1.62 tx/blk, 2.96 txin/blk; CPU 2.9 s over 3.5 s wall (parallelism 0.84)

| subsystem (instructions:u) | share |
|---|---|
| leveldb | 37.45% |
| bls | 20.66% |
| x11 | 14.45% |
| evodb/mn-lists | 9.90% |
| logging | 8.24% |
| sha256 | 3.87% |
| coins-cache | 2.21% |
| validation | 1.35% |
| block-io | 0.68% |
| tx-block-codec | 0.55% |
| llmq | 0.31% |
| memory/libc | 0.06% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 42.93% |
| ActivateBestChain | 42.62% |
| ConnectTip | 42.25% |
| LevelDB (any) | 37.08% |
| ConnectBlock (whole) | 29.46% |
| ProcessSpecialTxsInBlock | 12.85% |
| CDeterministicMNManager::ProcessBlock | 8.61% |
| LogPrintf | 8.30% |
| ReadBlockFromDisk | 4.37% |
| UpdatedBlockTip / notifications | 4.06% |
| CheckBlock (context-free) | 3.94% |
| llmq::* (any) | 2.83% |
| CheckSpecialTx | 2.15% |
| IsBlockValueValid / payee | 1.85% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 63.04% |
| unnamed background thread (LevelDB compaction) | 36.65% |
| scheduler | 0.31% |

| top leaf symbols | share |
|---|---|
| __gmpn_addmul_1_coreisbr | 6.64% |
| __gmpn_mul_basecase_coreisbr | 5.54% |
| __memmove_avx_unaligned_erms_rtm | 3.87% |
| sph_cubehash512_addbits_and_close | 3.32% |
| __memcmp_avx2_movbe_rtm | 3.26% |
| immer::detail::hamts::champ_iterator<std::pair<uint256, std::shared_pt | 2.83% |
| Lloop1_30 | 2.71% |
| leveldb::PutVarint32(std::__cxx11::basic_string<char, std::char_traits | 2.64% |
| leveldb::(anonymous namespace)::BloomFilterPolicy::CreateFilter(leveld | 2.46% |
| compress_big | 2.40% |

### w421000-serial-noav-nobench

1999 blocks, 77.59G instructions:u = **38.82M/block**; 36.32M/block without the unnamed LevelDB background thread; CPU 11.6 s over 10.9 s wall (parallelism 1.07)

| subsystem (instructions:u) | share |
|---|---|
| bls | 79.50% |
| leveldb | 7.25% |
| sha256 | 5.06% |
| script-verify | 3.77% |
| x11 | 1.72% |
| evodb/mn-lists | 1.33% |
| llmq | 0.32% |
| validation | 0.19% |
| memory/libc | 0.19% |
| coins-cache | 0.17% |
| rpc/http/net | 0.15% |
| logging | 0.10% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 13.78% |
| ActivateBestChain | 13.76% |
| ConnectTip | 13.70% |
| ConnectBlock (whole) | 12.25% |
| CheckInputScripts / script checks | 7.79% |
| VerifyScript | 7.70% |
| LevelDB (any) | 7.05% |
| ProcessSpecialTxsInBlock | 3.29% |
| llmq::* (any) | 2.16% |
| CDeterministicMNManager::ProcessBlock | 1.07% |
| ReadBlockFromDisk | 0.65% |
| UpdatedBlockTip / notifications | 0.48% |
| CheckBlock (context-free) | 0.42% |
| CheckSpecialTx | 0.30% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 93.55% |
| unnamed background thread (LevelDB compaction) | 6.43% |
| scheduler | 0.02% |

| top leaf symbols | share |
|---|---|
| __gmpn_addmul_1_coreisbr | 22.28% |
| __gmpn_mul_basecase_coreisbr | 21.33% |
| dv_cmp | 6.04% |
| fp_rdcn_low | 4.30% |
| __gmpn_add_n_coreisbr | 3.60% |
| Lloop1_30 | 3.25% |
| core_get | 3.08% |
| fp_prime_get | 2.64% |
| fp_addm_low | 1.93% |
| secp256k1_fe_mul_inner | 1.52% |

### w421000-serial-noav

1999 blocks, 78.74G instructions:u = **39.39M/block**; 36.89M/block without the unnamed LevelDB background thread; 1.62 tx/blk, 2.96 txin/blk; CPU 13.9 s over 13.8 s wall (parallelism 1.0)

| subsystem (instructions:u) | share |
|---|---|
| bls | 78.50% |
| leveldb | 7.31% |
| sha256 | 4.93% |
| script-verify | 3.69% |
| x11 | 1.72% |
| evodb/mn-lists | 1.30% |
| logging | 1.22% |
| llmq | 0.31% |
| coins-cache | 0.30% |
| validation | 0.21% |
| tx-block-codec | 0.18% |
| memory/libc | 0.13% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 15.04% |
| ActivateBestChain | 14.98% |
| ConnectTip | 14.94% |
| ConnectBlock (whole) | 13.27% |
| CheckInputScripts / script checks | 7.58% |
| VerifyScript | 7.54% |
| LevelDB (any) | 7.00% |
| ProcessSpecialTxsInBlock | 3.97% |
| llmq::* (any) | 2.45% |
| LogPrintf | 1.28% |
| CDeterministicMNManager::ProcessBlock | 1.13% |
| ReadBlockFromDisk | 0.61% |
| UpdatedBlockTip / notifications | 0.58% |
| CheckBlock (context-free) | 0.35% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 93.60% |
| unnamed background thread (LevelDB compaction) | 6.34% |
| scheduler | 0.06% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 21.28% |
| __gmpn_addmul_1_coreisbr | 20.70% |
| dv_cmp | 5.63% |
| fp_rdcn_low | 5.07% |
| core_get | 3.74% |
| __gmpn_add_n_coreisbr | 3.47% |
| Lloop1_30 | 3.26% |
| fp_prime_get | 2.35% |
| fp_addm_low | 1.97% |
| secp256k1_fe_mul_inner | 1.62% |

### w925000-par8-noav

1999 blocks, 358.72G instructions:u = **179.45M/block**; 176.25M/block without the unnamed LevelDB background thread; 1.93 tx/blk, 1.0 txin/blk; CPU 58.0 s over 41.4 s wall (parallelism 1.4)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.66% |
| leveldb | 2.42% |
| sha256 | 0.61% |
| evodb/mn-lists | 0.53% |
| x11 | 0.50% |
| logging | 0.33% |
| script-verify | 0.24% |
| llmq | 0.20% |
| memory/libc | 0.15% |
| rpc/http/net | 0.13% |
| validation | 0.08% |
| coins-cache | 0.05% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.06% |
| ActivateBestChain | 3.05% |
| ConnectTip | 3.03% |
| ConnectBlock (whole) | 2.65% |
| LevelDB (any) | 2.27% |
| ProcessSpecialTxsInBlock | 2.16% |
| llmq::* (any) | 1.63% |
| LogPrintf | 0.34% |
| ReadBlockFromDisk | 0.30% |
| CheckInputScripts / script checks | 0.29% |
| CDeterministicMNManager::ProcessBlock | 0.28% |
| VerifyScript | 0.27% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.26% |
| CheckCbTx | 0.18% |

| thread group | instr share |
|---|---|
| script/BLS check workers (scriptch.N) | 53.01% |
| block import / ActivateBestChain (loadblk) | 45.19% |
| unnamed background thread (LevelDB compaction) | 1.78% |
| scheduler | 0.02% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.65% |
| __gmpn_addmul_1_coreisbr | 23.69% |
| dv_cmp | 7.67% |
| fp_rdcn_low | 6.04% |
| __gmpn_add_n_coreisbr | 4.89% |
| core_get | 4.45% |
| fp_prime_get | 3.34% |
| fp_addm_low | 2.36% |
| __gmpn_sub_n_coreisbr | 2.33% |
| __gmpn_mul_n | 1.28% |

### w925000-serial-av-nobench

1999 blocks, 40.70G instructions:u = **20.36M/block**; 17.13M/block without the unnamed LevelDB background thread; CPU 9.1 s over 9.2 s wall (parallelism 0.98)

| subsystem (instructions:u) | share |
|---|---|
| bls | 72.08% |
| leveldb | 16.19% |
| x11 | 4.08% |
| evodb/mn-lists | 4.04% |
| sha256 | 1.92% |
| validation | 0.46% |
| coins-cache | 0.32% |
| block-io | 0.28% |
| tx-block-codec | 0.22% |
| logging | 0.21% |
| llmq | 0.13% |
| memory/libc | 0.03% |

| inclusive (instructions:u) | share |
|---|---|
| LevelDB (any) | 16.08% |
| LoadBlockIndex / init | 12.21% |
| ActivateBestChain | 12.12% |
| ConnectTip | 11.98% |
| ConnectBlock (whole) | 9.40% |
| ProcessSpecialTxsInBlock | 7.09% |
| llmq::* (any) | 3.36% |
| ReadBlockFromDisk | 2.52% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 2.46% |
| CDeterministicMNManager::ProcessBlock | 2.42% |
| CCreditPoolManager | 1.30% |
| UpdatedBlockTip / notifications | 0.88% |
| CheckBlock (context-free) | 0.83% |
| CheckSpecialTx | 0.69% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 84.05% |
| unnamed background thread (LevelDB compaction) | 15.86% |
| scheduler | 0.09% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 19.47% |
| __gmpn_addmul_1_coreisbr | 19.46% |
| fp_rdcn_low | 5.81% |
| dv_cmp | 4.64% |
| core_get | 3.45% |
| __gmpn_add_n_coreisbr | 3.14% |
| fp_addm_low | 2.12% |
| fp_prime_get | 1.99% |
| __memmove_avx_unaligned_erms_rtm | 1.80% |
| __memcmp_avx2_movbe_rtm | 1.61% |

### w925000-serial-av

1999 blocks, 41.63G instructions:u = **20.83M/block**; 17.65M/block without the unnamed LevelDB background thread; 1.93 tx/blk, 1.0 txin/blk; CPU 8.0 s over 8.4 s wall (parallelism 0.96)

| subsystem (instructions:u) | share |
|---|---|
| bls | 70.15% |
| leveldb | 15.62% |
| x11 | 4.50% |
| evodb/mn-lists | 4.22% |
| logging | 1.97% |
| sha256 | 1.86% |
| validation | 0.58% |
| coins-cache | 0.33% |
| block-io | 0.32% |
| llmq | 0.23% |
| tx-block-codec | 0.13% |
| other | 0.03% |

| inclusive (instructions:u) | share |
|---|---|
| LevelDB (any) | 15.51% |
| LoadBlockIndex / init | 14.73% |
| ActivateBestChain | 14.66% |
| ConnectTip | 14.48% |
| ConnectBlock (whole) | 11.34% |
| ProcessSpecialTxsInBlock | 7.44% |
| llmq::* (any) | 3.52% |
| ReadBlockFromDisk | 2.36% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 2.35% |
| CDeterministicMNManager::ProcessBlock | 2.23% |
| LogPrintf | 2.02% |
| CCreditPoolManager | 1.24% |
| UpdatedBlockTip / notifications | 1.08% |
| CheckBlock (context-free) | 0.91% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 84.67% |
| unnamed background thread (LevelDB compaction) | 15.26% |
| scheduler | 0.07% |

| top leaf symbols | share |
|---|---|
| __gmpn_addmul_1_coreisbr | 19.63% |
| __gmpn_mul_basecase_coreisbr | 18.92% |
| dv_cmp | 4.42% |
| fp_rdcn_low | 4.38% |
| __gmpn_add_n_coreisbr | 3.14% |
| core_get | 3.11% |
| fp_prime_get | 2.18% |
| fp_addm_low | 2.00% |
| __memcmp_avx2_movbe_rtm | 1.79% |
| __memmove_avx_unaligned_erms_rtm | 1.50% |

### w925000-serial-noav-nobench

1999 blocks, 357.35G instructions:u = **178.77M/block**; 175.58M/block without the unnamed LevelDB background thread; CPU 58.6 s over 59.3 s wall (parallelism 0.99)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.99% |
| leveldb | 2.46% |
| sha256 | 0.60% |
| x11 | 0.50% |
| evodb/mn-lists | 0.46% |
| script-verify | 0.25% |
| memory/libc | 0.22% |
| llmq | 0.19% |
| rpc/http/net | 0.12% |
| validation | 0.06% |
| tx-block-codec | 0.04% |
| block-io | 0.03% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 2.94% |
| ActivateBestChain | 2.90% |
| ConnectTip | 2.89% |
| ConnectBlock (whole) | 2.62% |
| LevelDB (any) | 2.34% |
| ProcessSpecialTxsInBlock | 2.03% |
| llmq::* (any) | 1.60% |
| CheckInputScripts / script checks | 0.33% |
| VerifyScript | 0.30% |
| ReadBlockFromDisk | 0.30% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.29% |
| CDeterministicMNManager::ProcessBlock | 0.24% |
| CChainLocks / VerifyChainLock | 0.16% |
| CheckCbTx | 0.16% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.18% |
| unnamed background thread (LevelDB compaction) | 1.78% |
| scheduler | 0.04% |
| LLMQ/DKG/sigs threads | 0.00% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.72% |
| __gmpn_addmul_1_coreisbr | 23.94% |
| dv_cmp | 7.45% |
| fp_rdcn_low | 6.30% |
| __gmpn_add_n_coreisbr | 5.08% |
| core_get | 4.40% |
| fp_prime_get | 3.19% |
| fp_addm_low | 2.62% |
| __gmpn_sub_n_coreisbr | 2.25% |
| __gmpn_mul_n | 1.28% |

### w925000-serial-noav

1999 blocks, 358.51G instructions:u = **179.35M/block**; 176.15M/block without the unnamed LevelDB background thread; 1.93 tx/blk, 1.0 txin/blk; CPU 51.1 s over 51.8 s wall (parallelism 0.99)

| subsystem (instructions:u) | share |
|---|---|
| bls | 94.58% |
| leveldb | 2.39% |
| sha256 | 0.61% |
| evodb/mn-lists | 0.54% |
| x11 | 0.52% |
| logging | 0.28% |
| script-verify | 0.26% |
| memory/libc | 0.22% |
| llmq | 0.21% |
| rpc/http/net | 0.12% |
| validation | 0.09% |
| coins-cache | 0.07% |

| inclusive (instructions:u) | share |
|---|---|
| LoadBlockIndex / init | 3.33% |
| ActivateBestChain | 3.31% |
| ConnectTip | 3.30% |
| ConnectBlock (whole) | 2.94% |
| LevelDB (any) | 2.26% |
| ProcessSpecialTxsInBlock | 2.18% |
| llmq::* (any) | 1.58% |
| CDeterministicMNManager::ProcessBlock | 0.34% |
| CheckInputScripts / script checks | 0.32% |
| VerifyScript | 0.31% |
| ReadBlockFromDisk | 0.30% |
| LogPrintf | 0.28% |
| CDeterministicMNList::AddMN / UpdateMN / ApplyDiff | 0.28% |
| CChainLocks / VerifyChainLock | 0.16% |

| thread group | instr share |
|---|---|
| block import / ActivateBestChain (loadblk) | 98.19% |
| unnamed background thread (LevelDB compaction) | 1.78% |
| scheduler | 0.02% |

| top leaf symbols | share |
|---|---|
| __gmpn_mul_basecase_coreisbr | 25.77% |
| __gmpn_addmul_1_coreisbr | 24.23% |
| dv_cmp | 7.36% |
| fp_rdcn_low | 5.63% |
| __gmpn_add_n_coreisbr | 4.85% |
| core_get | 4.51% |
| fp_prime_get | 3.33% |
| fp_addm_low | 2.58% |
| __gmpn_sub_n_coreisbr | 2.23% |
| __gmpn_mul_n | 1.20% |
