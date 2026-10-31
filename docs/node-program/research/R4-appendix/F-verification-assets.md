> R4 appendix — sub-agent working notes, verified against Dash Core develop @ 15db478750 and rust-dashcore dev @ 67b307c6 (2026-10-08). Main report: ../R4-consensus-and-verification.md. Items marked 'unverified' were not confirmed empirically.

# F. Verification assets and network-fingerprint surface (Rust Dash full node / masternode)

Sources:
- Dash Core `develop` @ `15db478750`. This is **v24.1.0-dev** (`configure.ac:3-6`: MAJOR 24, MINOR 1, IS_RELEASE false).
- A Dash Core **v24.0.0-rc.3** release binary, downloaded to `/tmp/r4-verif-bin/dashcore-24.0.0-rc.3/`. I ran it on regtest in `/tmp/r4-verif-dd` to confirm options, RPC help, UTXO hashes and the P2P handshake.

All file:line citations are relative to `/work/home/agent/workspace/dash` unless they are prefixed `rust-dashcore/`. I modified neither repo.

Scripts I wrote, all under `/tmp/r4-verif-scripts/`:
- `utxohash.py` recomputes the UTXO hashes.
- `handshake.py` is a raw v1 P2P probe.
- `rpc_census.py` counts RPC usage. Its output is in `census.txt`.
- `test_matrix.txt` is the per-test wallet/DTF matrix.

---

## 0. Top findings

1. **Unit-test vectors that can be reused directly.** `src/test/data/` has 13 JSON files plus `asmap.raw`. The large ones are:
   - `script_tests.json`: 1282 vectors, including about 120 rows that use DIP0020 opcodes.
   - `tx_valid.json` / `tx_invalid.json`: 83 and 76 vectors.
   - `sighash.json`: 500 vectors.

   Two files are Dash-specific and pure data:
   - `trivially_valid.json` / `trivially_invalid.json`: ProTx special-tx vectors, 25 and 12 rows, with the expected reject reasons.
   - `proposals_valid.json` / `proposals_invalid.json`: governance proposal vectors.

   A Rust harness can consume all of these unchanged.
2. **Many vectors are embedded in C++ rather than JSON.** These need to be extracted by hand:
   - DGW difficulty: 24 real mainnet blocks to block 123457, plus devnet slow-block rules (`src/test/pow_tests.cpp:14-59`).
   - Subsidy, including v20: `subsidy_tests.cpp`.
   - BRR, MN-payment and v24 multiplicity amounts: `block_reward_reallocation_tests.cpp`.
   - SML merkle roots: `evo_simplifiedmns_tests.cpp:44-76`.
   - A real mainnet ISDLOCK: `evo_islock_tests.cpp:177`.
   - A real testnet quorum-proof archive: `quorum_proofs_tests.cpp:26`.
   - DIP14 vectors: `dip14_tests.cpp`.
   - The dynamic versionbits threshold formula: `dynamic_activation_thresholds_tests.cpp:20-27`.

   The release tarball ships `test_dash` (167 suites), so the C++ unit tests can be run as a reference without building.
3. **The UTXO hash algorithm is confirmed, including a quirk.** `gettxoutsetinfo` `hash_serialized_2` (the default hash type) has a C++ operator-precedence quirk at `src/kernel/coinstats.cpp:77`. The per-tx header VARINT is `(nHeight*2+fCoinBase) ? 1 : 0`, so it is almost always `1`. My independent Python implementation matches dashd **only** with the quirk (`918460f9…` with the quirk vs `0aeefa6b…` without). My `muhash` implementation also matches. A Rust node must reproduce the quirk exactly.
4. **Historical UTXO comparison needs coinstatsindex and muhash.** Querying past heights needs `-coinstatsindex` and `muhash` or `none`. `hash_serialized_2` is refused for a specific block (`src/rpc/blockchain.cpp:1253-1261`). The evo state can be queried at historical heights cheaply with `protx list registered true <height>`, `protx diff`, `quorum listextended <h>`, `getcreditpoolinfo <h>`, `getdeploymentinfo <hash>`, `masternode payments`, `getassetunlockstatuses [..] <h>` and `getchainlockbyheight`. Governance (`gobject list`), `getbestchainlock` and spork state can only be read at the tip and depend on node state.
5. **Replay comparisons will not exercise superblock payee validation.** dashd checks payees only when the node is mnsync-synced **and** the block is not chainlocked (`src/evo/chainhelper.cpp:41-52`, `validation.cpp:2840-2842`, `masternode/payments.cpp:356-389`). Otherwise it checks only the superblock max bound. IBD/replay never runs the full check. Only functional tests such as `feature_governance*` cover it.
6. **Functional tests.** There are 264 test scripts (261 unique in `BASE_SCRIPTS`):
   - 151 need no wallet (no DashTestFramework and no `skip_if_no_wallet`).
   - 83 need a wallet explicitly.
   - 34 use `DashTestFramework`, which always requires a wallet on node 0.

   The tests call 209 distinct RPC methods. Running a Rust node under the framework is realistic for three reasons:
   - The framework picks node binaries per node (`DASHD` env var or the `add_nodes(binary=[...])` argument).
   - Masternodes started with `-masternodeblsprivkey` force `-disablewallet` (`src/init.cpp:1094-1103`), so a Rust masternode needs **no wallet** if dashd stays as the wallet-holding controller on node 0.
   - `copy_datadir` silently ignores failures (`test/functional/test_framework/util.py:491-502`), so a Rust masternode with a different datadir layout would simply IBD from node 0.
7. **The proposed Tier-1 suite is small.** It is 35 consensus tests that need no wallet or only MiniWallet. They exercise only 52 RPC methods; adding the framework helpers brings it to about 60. Every framework node also needs these, which mostly come from the framework:
   - `setmocktime`, `mockscheduler`, `generatetoaddress`, `submitblock`, `invalidateblock`, `getpeerinfo` (with `subver`, `inbound`, and `bytesrecv_per_msg['pong']>=29`).
   - `debug.log` at `<datadir>/regtest/debug.log` for `assert_debug_log`, which 63 test files use.
   - The `.cookie` file at `<datadir>/regtest/.cookie`.
   - `dash.conf` keys such as `tinyblk=1`, `rpcdoccheck=1` and `peertimeout`.
8. **Handshake fingerprint, captured live from 24.0.0-rc.3.** dashd sends, in order: `version`, `sendaddrv2`, `verack`, then after our verack `sendheaders2` (or `sendheaders`), `sendcmpct(0,1)`, `senddsq(1)`, `ping`, `getheaders2`. About 0.5–2.2 s later it sends `getsporks`, and about 8 s later `govsync`. Other properties:
   - **No `feefilter` at all.** Dash never implemented BIP133; its BIP324 short-ID slot 5 is blank.
   - No `wtxidrelay`, no `reject`, and no `sendtxrcncl` (tx reconciliation is off by default).
   - The version message is 137 bytes with UA `/Dash Core:24.0.0/`. It carries an extra `mnauthChallenge` (32 bytes) and `fMasternode` (1 byte).
   - Services are `0x1c05` (NETWORK|BLOOM|NETWORK_LIMITED|HEADERS_COMPRESSED|P2P_V2). Protocol version is 70242.
9. **Libraries and build.**
   - libdashconsensus is still built (`--with-libs` default yes) and shipped in the release (`lib/libdashconsensus.so`). Its API is only `dashconsensus_verify_script(spk, spkLen, txTo, txToLen, nIn, flags, err)` with no amount argument, plus `dashconsensus_version()`. The allowed flags are only P2SH, DERSIG, NULLDUMMY, CLTV and CSV.
   - Autotools is the real build. `CMakeLists.txt` is a CLion-only dummy (`CMakeLists.txt:1-6`).
   - There is no differential consensus fuzz harness. The only differential fuzzer is `crypto_diff_fuzz_chacha20`. No fuzz target covers special-tx, BLS or LLMQ deserialization; the only Dash fuzz target is `dkg_message_framing`.
10. **Chain size and existing tooling.**
    - Mainnet (2026-10-08 03:05 UTC): height 2,551,641, 71.37M txs, blockchain about 43.3 GB (blockchair).
    - Testnet: height 1,568,316, already running v24 (protocol 70242).
    - The develop branch's `defaultAssumeValid` and `nMinimumChainWork` are at mainnet block 2,487,500 (`chainparams.cpp:221-222`, `m_assumed_blockchain_size=57`).
    - rust-dashcore already has a dashd driver (`dash-spv/src/test_utils/node.rs`) and a masternode-network/DKG-cycle harness (`masternode_network.rs`, 1099 lines). It uses dashd 23.1.0 plus `dashpay/regtest-blockchain` v0.0.4 data, which includes `regtest-mn`: 4 MNs and 8 mined DKG cycles.

---

## 1. C++ unit-test vectors

### 1.1 `src/test/data/` (all compiled into `test_dash` via `src/Makefile.test.include:19-…`)

| File | Content | Entries | Consumer | Reuse |
|---|---|---|---|---|
| `script_tests.json` | `[scriptSig, scriptPubKey, flags, expected_error, comment?]`. Flags used: STRICTENC 970, P2SH 957, MINIMALDATA 115, NULLFAIL 40, DERSIG 37, DISCOURAGE_UPGRADABLE_NOPS 12, CSV 6, LOW_S/NULLDUMMY/SIGPUSHONLY/CLEANSTACK 3 each. Results: OK 740, INVALID_STACK_OPERATION 126, BAD_OPCODE 87, … CHECKDATASIGVERIFY 5, SPLIT_RANGE 3, DIV/MOD_BY_ZERO… Contains DIP0020 opcodes: BIN2NUM 30, AND/OR/XOR 25 each, DIV 20, SPLIT 17, NUM2BIN 15, MOD 13, CAT 12, CHECKDATASIG(VERIFY) 8+8. | 1334 rows (52 comments, 1282 vectors) | `script_tests.cpp:849,886` | Direct. Note the DIP0020 opcodes are **always enabled** in the interpreter: the flag is retired (`script/interpreter.h:101-105`), and only the CVE-2010-5137 set is disabled (`interpreter.cpp:387-393`). |
| `tx_valid.json` | `[[prevouts…], txhex, flags]`. Flags: NONE 38, LOW_S 24, CLEANSTACK 19, CONST_SCRIPTCODE 13, STRICTENC 10, DERSIG 7, SIGPUSHONLY 6… | 162 rows (79 comments, 83 vectors) | `transaction_tests.cpp:157` | Direct (flags are listed as *excluded* flags in BTC semantics) |
| `tx_invalid.json` | same format. Flags: P2SH 66, CLTV 22, CSV 16, CONST_SCRIPTCODE 12, BADTX 10 (CheckTransaction failures), NULLDUMMY 4 | 154 rows (78 comments, 76 vectors) | `transaction_tests.cpp:241` | Direct |
| `sighash.json` | `[raw_tx, script, input_index, hashType, sighash]`. Legacy sighash only (Dash has no segwit or forkid). | 500 + 1 header | `sighash_tests.cpp:162` | Direct |
| `trivially_valid.json` | Dash ProTx: `[txhash, type, bls_scheme("basic"/"legacy"), txhex]`. 7× proregtx, 6× proupservtx, 6× proupregtx, 6× prouprevtx | 25 + header | `evo_trivialvalidation.cpp:123` (fixture `TestChain100V19Activation` only selects the BLS scheme / V19 context) | Direct. The data is pure; Rust only needs to apply "basic vs legacy" BLS parsing. |
| `trivially_invalid.json` | same plus `expected_error, notes`. Errors: bad-protx-version 3, bad-protx-evo-version 2, bad-protx-key-null 2, -payee-reuse, -payee, -operator-reward, -payload, -reason | 12 + header | `evo_trivialvalidation.cpp:134` | Direct |
| `proposals_valid.json` / `proposals_invalid.json` | Governance proposal JSON (`name, payment_address, payment_amount, start/end_epoch, type, url`) → valid flag | 6 / 19 | `governance_validators_tests.cpp:40,70` | Direct (needs mainnet address and URL validation rules) |
| `key_io_valid.json` / `key_io_invalid.json` | Dash addresses/WIF: main 10+8 privkeys, test 8+8, regtest 8+8; 50 invalid strings | 50 / 50 | `key_io_tests.cpp` | Direct |
| `base58_encode_decode.json` | hex ↔ base58 | 14 | `base58_tests.cpp` | Direct |
| `bip39_vectors.json` | `[entropy, mnemonic, seed, xprv]` (Trezor vectors, passphrase "TREZOR") | 24 | `bip39` tests | Direct (wallet only) |
| `blockfilters.json` | BIP158 `[height, hash, block, prevScripts, prevHeader, filter, header, note]`. Uses Bitcoin testnet3 blocks (0, 2, 3, 49291, 180480, 926485, 987876) but **Dash X11 block hashes**; the SipHash key is derived from the hash. | 7 + header | `blockfilter_tests.cpp:138-186` | Direct |
| `asmap.raw` | 59-byte asmap for addrman tests | 1 | `addrman_tests.cpp:14` | P2P only |

### 1.2 Dash-specific unit tests in `src/test/`

There is no `specialtx/` directory. Each entry below gives what the test covers, its fixture, and whether its data can be extracted to JSON.

**Extractable as data** (pure functions or embedded vectors):
- `pow_tests.cpp` (14 cases), `get_next_work`. DGW over 24 real mainnet `(nTime, nBits)` pairs ending at height 123456; block 123457 must give `0x1b1441de` (`:14-36`). Devnet slow-block rules: 10m+1s gives `0x1c00c8f8`, 2h+1s gives `0x207fffff` (`:38-59`). The BTC retarget tests are commented out. Also covers CheckProofOfWork edge cases and chainparams sanity for MAIN/TEST/REGTEST/DEVNET. **Easy to extract.**
- `subsidy_tests.cpp`, `block_subsidy_test`. Eleven `(nPrevBits, nPrevHeight, fV20Active) → subsidy` points on mainnet params, for example 4249/0x1c4a47c4 gives 50000000000 (pre-v20) and 500000000 (v20); 420480 gives 388010205 vs 344897960 (`:15-97`). **Extract directly.**
- `dynamic_activation_thresholds_tests.cpp`. Dash's EHF/BIP9 dynamic threshold, `threshold(attempt)=max(th_start - attempt²·window/100/5, th_end)` (`:20-27`). The cases need a TestChain, but the formula plus the expected thresholds per period can be turned into data.
- `evo_simplifiedmns_tests.cpp`, `simplifiedmns_merkleroots`. Fifteen synthetic SML entries (`proRegTxHash=i`, `confirmedHash=i`, `0.0.0.i:i`, …) with expected per-entry hashes and merkle root `0bae2176…4022` (`:19-76`). Extractable, but the Rust side must build the identical netInfo serialization.
- `evo_islock_tests.cpp`, `deserialize_instantlock_from_realdata2` (`:177-192`). A real ISDLOCK hex with expected txid, cycleHash, quorum hash and sign hash. Also covers `geninputlockrequestid_*` (request-ID format). **Extractable.**
- `quorum_proofs_tests.cpp` (`QuorumProofsRegtestSetup`). `real_testnet_wire_and_crypto` embeds a real testnet archive proof for heights 1548500→1549547 (`:26-…`). Also used by functional `feature_quorum_proof_chain.py` through `test/functional/data/quorum_proof.json`. **Extractable.**
- `dip14_tests.cpp`. Seven cases, 16 hex vectors (DIP14 256-bit derivation). **Extractable.**
- `bls_tests.cpp` (20 cases). Mostly round-trip and aggregation property tests for both schemes plus SetHexStr edge cases. Only 2 hex constants; needs generation against `dashbls`. Partly extractable (scheme-specific serialization round trips).
- `llmq_hash_tests.cpp`. BuildCommitmentHash determinism and sensitivity; property tests, no golden values.
- `llmq_commitment_tests.cpp` (11 cases). Serialization, versions, JSON and bitset semantics. Synthetic; outputs could be dumped to make goldens.
- `llmq_snapshot_tests.cpp` (13) and `evo_snapshot_tests.cpp` (31). Quorum snapshot / qrinfo serialization. The evo snapshot v3 has `populated_v3_golden_value` (`:317`). The new canonical evo-state snapshot format is in `src/evo/snapshot.{h,cpp}`: `EVO_SNAPSHOT_VERSION 3`, marker "DASHEVO\0", and `CanonicalMNListHash` (`snapshot.h:301`). It is **not exposed via RPC yet**: the only users are snapshot.cpp and tests, and the regtest chainparams carry `EvoSnapshotHash{uint256{}}` placeholders (`chainparams.cpp:878-886`). If it is ever exposed, it becomes a canonical full-MN-state hash for differential testing.
- `evo_netinfo_tests.cpp` (16). MnNetInfo/ExtNetInfo rules on main and regtest, wire compatibility and oversize rejection. Inputs are strings; mostly extractable.
- `evo_sharedmn_tests.cpp` (13). **v24 shared-collateral masternodes** (ProDisTx, ProUpShareTx, ProUpSharedRegTx). Synthetic.
- `checkdatasig_tests.cpp` (1) and `dip0020opcodes_tests.cpp` (5 cases, about 33 helper invocations). Stacks and scripts are built in C++ (`CScript() << op`). Transcribable into script_tests-style JSON.
- `llmq_params_tests`, `llmq_utils_tests` (17, including deterministic outbound connection selection), `masternode_payments_tests` (7; payee-matching rules, synthetic), `governance_vote_wire_tests`, `governance_validators_tests`, `coinjoin_queue_tests`, `evo_utils_tests`. Small, logic-level, transcribable.

**Need a C++ chain fixture** (`TestChainSetup`, `TestChain100Setup`, `TestChainDIP3Setup` (`evo_deterministicmns_tests.cpp:1764-1782`), `TestChainV19Setup`, `TestChainV24*Setup`, `src/test/util/setup_common.h:120-210`). These become behavioral specs, not vectors:
- `evo_deterministicmns_tests.cpp`: 3664 lines, 40 cases (DIP3 legacy/basic ProTx lifecycle, v19/v24 key-scheme migration, cross-scheme key-reuse rejection, mempool reorg/conflicts, `mn_payment_multiplicity_v24_boundary`, SML cache).
- `block_reward_reallocation_tests.cpp`. Regtest chain to `BRRHeight`, with expected `GetValueOut` and MN payments at successive heights: 28847249686/14423624841, 141734128/106300596 (0.75), 164512828 at height 2358 (`:120-229`). The numbers can be extracted as `(height, coinbase value, MN payment)`.
- `evo_assetlocks_tests.cpp` (12: v1/v2 asset lock/unlock, cbtx merkleRootAssetUnlocks, credit-pool v24 unlock limit, mempool single-claimant).
- `evo_mnhf_tests.cpp` (2), `evo_cbtx_tests.cpp` (2), `llmq_chainlock_tests.cpp` (13), `llmq_blockprocessor_tests.cpp`, `llmq_dkg_tests.cpp`, `llmq_qgetdata_tests.cpp`, `governance_*` (inv, superblock, vote processing and sync), `spork_tests.cpp`, `coinjoin_*`, `evo_db_tests.cpp`, `evo_mnauth_tests.cpp`.
- `versionbits_tests.cpp`: BTC-derived, mostly `BasicTestingSetup` with mock block-index chains.

**Reference runner:** the release tarball includes `bin/test_dash`. `--list_content` shows 167 suites, and `--run_test=pow_tests,subsidy_tests,evo_simplifiedmns_tests` passed locally. When a Rust port is unclear, run `test_dash` to confirm the expected behavior.

---

## 2. Fuzzing, libdashconsensus, build system

- There are 115 files and 168 `FUZZ_TARGET`s in `src/test/fuzz/`. The consensus-relevant ones are:
  - `script` (`script.cpp:38`), `script_flags` (`:18`; tx plus spent outputs plus flags from a serialized stream), `script_interpreter`, `eval_script` (`:12`), `signature_checker` (`:46`), `script_ops`, `scriptnum_ops`, `script_sigcache`, `script_bitcoin_consensus` (`:15`, which calls `dashconsensus_verify_script`).
  - `transaction`, `decode_tx`, `primitives_transaction`, `tx_in`, `tx_out`, `block` (`block.cpp:25`), `block_header`, `chain`, `pow` (`pow.cpp:25`), `versionbits` (`versionbits.cpp:114`).
  - `coins_view`, `coinscache_sim`, `utxo_snapshot`, `utxo_total_supply`, `muhash`, `tx_pool(_standard)`, `load_external_block_file`.
  - `deserialize.cpp` has 42 BTC-style deserializers: block, header, txundo, blockundo, coins, inv, addrv1/v2, bloomfilter, diskblockindex, txoutcompressor and so on. **None** covers Dash special-tx payloads, BLS or LLMQ objects.
  - The only Dash-specific target is `dkg_message_framing` (`dkg_message_framing.cpp:136`).
  - P2P targets: `process_message` (env `LIMIT_TO_MESSAGE_TYPE`, all NetMsgTypes including Dash ones; `process_message.cpp:30-62`), `process_messages`, `p2p_transport_serialization` (v1, v2 and mixed), `bip324_cipher_roundtrip`, `protocol`.
- **Differential harness: none.** `crypto_diff_fuzz_chacha20` is the only "diff" target. CI only *builds* fuzz (`.github/workflows/build.yml:244-250`, `linux64_fuzz`). There is no Dash corpus repo; `test/fuzz/test_runner.py:147` points at bitcoin-core/qa-assets.
  - Opportunity: drive `script_flags`/`script` corpora through libdashconsensus and the Rust interpreter side by side. Also add Dash special-tx deserialize targets to both sides.
- **libdashconsensus** is still built:
  - `configure.ac:774-778` (`--with-libs`, default yes), `:1763-1767` (generates `libdashconsensus.pc`).
  - `src/Makefile.am:66,1414-1426`. Sources are cleanse, crypto base, crypto sph (X11) and `libbitcoin_consensus`. It links dashbls, secp256k1 and gmp.
  - `libdashconsensus.pc.in` gives `-ldashconsensus`.
  - API in `src/script/bitcoinconsensus.h`: `BITCOINCONSENSUS_API_VER 0`; errors OK/TX_INDEX/TX_SIZE_MISMATCH/TX_DESERIALIZE/INVALID_FLAGS; flags P2SH(1<<0), DERSIG(1<<2), NULLDUMMY(1<<4), CLTV(1<<9), CSV(1<<10).
  - Exported functions: `int dashconsensus_verify_script(const unsigned char* scriptPubKey, unsigned int, const unsigned char* txTo, unsigned int, unsigned int nIn, unsigned int flags, dashconsensus_error*)` and `unsigned int dashconsensus_version()`. There is no amount or spent-outputs variant; `verify_flags` is at `bitcoinconsensus.cpp:68`.
  - The release `lib/libdashconsensus.so.0.0.0` exports both symbols (checked with `nm -D`) and also leaks `sph_*` X11 symbols. This makes it a cheap FFI oracle for script verification from Rust.
- Also available:
  - `--with-experimental-kernel-lib` builds `libdashkernel` (`configure.ac:780-784`).
  - `--enable-experimental-util-chainstate` builds `dash-chainstate` (`configure.ac:767-771`, `Makefile.am:159,1217-1223`). Not in release tarballs; see §6.
- **Build system:** autotools is the real build. `CMakeLists.txt:1-6` says: "This CMakeLists.txt is not meant to actually work! … Building must still be done … using the automake build chain".

---

## 3. Python functional test framework

### 3.1 Launching nodes

- **Binary paths** (`test_framework.py:272-287`):
  - `dashd` from `$DASHD`, `dash-cli` from `$DASHCLI`, `dash-util` from `$DASHUTIL`, `dash-wallet` from `$DASHWALLET`. The default is `$BUILDDIR/src/<bin>` from `test/config.ini` (template `test/config.ini.in`).
  - Each node can use a different binary through `add_nodes(..., binary=[...], binary_cli=[...], versions=[...])` (`:520-560`). The "previous releases" machinery uses this, and so can a mixed dashd/Rust network. Previous-release binaries come from `test/get_previous_releases.py` (v20.1.1, v21.1.1, …).
  - `--dashd-arg` adds extra args to every node (`:216-217`).
- **Command line** (`test_node.py:108-160`):
  - Always: `-datadir=… -logtimemicros -debug -debugexclude=libevent -debugexclude=leveldb -debugexclude=rand -uacomment=testnode<i> -par=2 -rpcthreads=2 -maxsigcachesize=1`.
  - `-mocktime=<t>` when mocktime is set.
  - `-disablewallet` when no wallet is used.
  - Version-gated: `-logthreadnames`, `-logsourcelocations`, `-loglevel=trace`, `-parbls=2`, `-v2transport=0|1`, `-createwalletbackups=0`.
- **Config file** `dash.conf` (`util.py:402-436`): `regtest=1`, `[regtest]`, `port=`, `rpcport=`, `rpcservertimeout=99000`, `rpcdoccheck=1`, `fallbackfee=0.00001`, `server=1`, `keypool=1`, `discover=0`, `dnsseed=0`, `fixedseeds=0`, `listenonion=0`, `peertimeout=999999999`, `printtoconsole=0`, `upnp=0`, `natpmp=0`, `shrinkdebugfile=0`, `tinyblk=1`, `unsafesqlitesync=1`, `connect=0`. `bind=127.0.0.1` is added by `add_nodes` when localhost-only.
- **Ports** (`util.py:324-371`): `p2p_port(n)=PORT_MIN(11000, env TEST_RUNNER_PORT_MIN)+n+(20*portseed)%(10000-1-20)`, `rpc_port=p2p+10000`, `tor=p2p+20000`.
- **RPC auth:** cookie authentication. The framework reads `rpcuser`/`rpcpassword` from dash.conf if present, otherwise `<datadir>/<chain>/.cookie` (`util.py:465-509`). The test chain folder is `regtest`.
- **Startup wait** (`test_node.py:289-359`):
  - Polls 4 times per second: `getblockcount` until it succeeds, tolerating RPC errors -28 (warm-up) and -342, ECONNREFUSED and a missing cookie.
  - Then waits until `getmempoolinfo()['loaded']` is true.
  - The process exiting during init raises `FailedToStartError`.
- **Stop:** the `stop` RPC; exit code must be 0 and stderr empty unless expected (`test_node.py:403-440`).
- **Chain cache** (`test_framework.py:943-1022`):
  - Node 0 mines 199 blocks: 8×25, alternating 3 deterministic keys plus `ADDRESS_BCRT1_P2SH_OP_TRUE`, with mocktime starting at `TIME_GENESIS_BLOCK=1417713337` (`blocktools.py:39`) and +156 s per block.
  - The cache **keeps only `chainstate`, `blocks`, `indexes`, `evodb`, `llmq`** and copies the datadir to every node.
  - So a Rust node must either keep its state under those names or build its own cache (it can, because the cache is built through RPC).
- **Mocktime** (`:883-908`): `bump_mocktime` calls `setmocktime` and then `mockscheduler(t)` on every node. A Rust node must implement both (`rpc/node.cpp:250`, `:893`).
- **Debug-log assertions:** `assert_debug_log` (`test_node.py:490`) reads `<datadir>/regtest/debug.log`. 63 test files use `assert_debug_log`, `wait_for_debug_log` or related. Heaviest users: `feature_config_args` 21, `p2p_filter` 10, `feature_llmq_dkg_intake` 10, `p2p_invalid_messages` 9, `p2p_sendtxrcncl` 8, `feature_asset_locks` 8, `p2p_v2_transport` 7, … A Rust node has to emit the same log strings, or those assertions need a shim.
- **connect_nodes** (`test_framework.py:705-746`): `addnode ip onetry`, then finds the peer in `getpeerinfo` by `subver` (which equals the other node's `getnetworkinfo().subversion`, unique through `-uacomment=testnodeN`) and `inbound`. It then waits for `bytesrecv_per_msg['pong'] >= 29` **on both sides**. So both nodes must ping immediately after verack, and getpeerinfo must keep per-message byte counters.
- **Python dependency:** `dash_hash` (X11) is imported in `messages.py:33`. There is no Python BLS; BLS keys come from the node via `bls generate`.

### 3.2 P2P harness (`p2p.py`, `messages.py`, `v2_p2p.py`)

- `P2P_VERSION=70242`, `P2P_SERVICES=NODE_NETWORK|NODE_HEADERS_COMPRESSED`, UA `/python-p2p-tester:0.0.3/`, relay 1 (`p2p.py:104-114`).
- The message map covers every Dash command (`p2p.py:~120-200`): clsig, dsa/dsc/dsf/dsi/dsq/dss/dssu/dstx, getheaders2/headers2/sendheaders2, getmnlistd/mnlistdiff, getqrinfo/qrinfo, getsporks/spork, govobj/govobjvote/govsync, isdlock, mnauth, platformban, qbsigs/qcomplaint/qcontrib/qdata/qfcommit/qgetdata/qgetsigs/qjustify/qpcommit/qsendrecsigs/qsigrec/qsigsesann/qsigshare/qsigsinv/qwatch, senddsq, ssc.
- Dash classes in `messages.py`: `CompressibleBlockHeader` (headers2 compression, `:749-920`), `CCbTx`, `CAssetLockTx`, `CAssetUnlockTx`, `CMnEhf`, `CSimplifiedMNListEntry`, `CFinalCommitment(Payload)`, `CGovernanceObject`, `CGovernanceVote`, `CRecoveredSig`, `CSigShare`, `CBLSPublicKey`, `CBLSIESEncryptedSecretKey`, `CCoinJoinBroadcastTx`, `msg_dstx`, `msg_getmnlistd`, `msg_getqrinfo`, `msg_mnlistdiff`, `msg_clsig`, `msg_isdlock`, `msg_platformban`, `CSigSharesInv`, `msg_qsigsinv`, `msg_qsigshare`, `msg_qsendrecsigs`, `msg_qwatch`, `msg_qgetdata`, `msg_qdata`, `msg_sendheaders2`, `msg_getheaders2`, `msg_headers2`.
  - This is a ready-made **independent Python serializer** to cross-check Rust encodings.
- `v2_p2p.py:24-97`: the BIP324 short-ID table, with Dash IDs starting at 128 (128=spork, 129=getsporks, …).
- `test/functional/data/`:
  - `invalid_txs.py`: 13 `BadTxTemplate`s with reject reasons such as `bad-txns-vout-empty`, `bad-txns-inputs-duplicate`, `bad-txns-in-belowout`, `mandatory-script-verify-flag-failed (Invalid OP_IF construction)`, `bad-blk-sigops, out-of-bounds SigOpCount`. Used by feature_block and p2p_invalid_tx.
  - `blockheader_testnet3.hex`: 257 real testnet headers, for p2p_dos_header_tree.
  - `quorum_proof.json`, `rpc_getblockstats.json` (golden stats), `rpc_decodescript.json`, `rpc_bip67.json`, `rpc_psbt.json`.
- `feature_utxo_set_hash.py:70-71` hard-codes for the deterministic cache chain `hash_serialized_2 = 1d640be3…9eac` and `muhash = c2ca3b52…e78e`. It also recomputes muhash with `test_framework/crypto/muhash.py`. This is a **golden acceptance vector**.

### 3.3 DashTestFramework (`test_framework.py:1495-2620`)

- **Setup:**
  - `set_dash_test_params(num_nodes, mn_count, extra_args, evo_count)` (`:1532-1560`) sets `setup_clean_chain=True`, `v20_height=mn_rr_height=100`, `llmq_size=3`, `threshold=2`, `llmq_size_dip0024=4` (no `-llmqtestparams` needed by default), and `disable_autoconnect=False`.
  - Every node gets `dip3params=2:2`, `testactivationheight=v20@100` and `mn_rr@100` (`:1525-1530`).
  - Node 0 gets `sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK` (`:1514`).
  - `set_dash_llmq_test_params` adds `-llmqtestparams/-llmqtestinstantsendparams/-llmqtestplatformparams=size:threshold` (`:1611-1617`).
  - Other args used by tests: `-vbparams=…:window:thresholdstart:thresholdmin:falloffcoeff:mnactivation`, `-llmqtestinstantsenddip0024=llmq_test_instantsend`, `-budgetparams=10:10:10`, `-whitelist`, `-spentindex`, `-watchquorums`, `-llmqchainlocks`, `-disablegovernance`, `-pushversion`, `-sporkaddr`.
- **Masternode creation** (`:1722-1800`, `MasternodeInfo` `:1179-1495`):
  - Node 0 (wallet) funds collateral with `sendtoaddress` (and `lockunspent`).
  - It alternates `protx register_fund` / `protx register` (or `register_evo`, `register_fund_evo`, `*_legacy` before v19) with submit true/false, using `sendrawtransaction` for the prepared case.
  - It calls `protx update_service` when operator reward > 0.
  - Node 0 is stopped, its datadir copied with `copy_datadir` (blocks, chainstate, evodb, llmq; errors ignored), and node 0 restarted.
  - MNs start with `-masternodeblsprivkey=<op key>`, which implies `-disablewallet`, `-peerblockfilters`, `-blockfilterindex=basic` (`src/init.cpp:1094-1103`).
  - `force_finish_mnsync` loops `mnsync status` / `mnsync next` (`util.py:566-572`).
  - MNs connect to node 0 only; then `generate 8`, `sporkupdate SPORK_2_INSTANTSEND_ENABLED 0`, `sporkupdate SPORK_19_CHAINLOCKS_ENABLED 0`, `wait_for_sporks_same` (`spork show`), and `masternodelist status` must be all ENABLED (`:1900-1925`).
  - `connect_nodes` wraps the connection in `setmnthreadactive False/True` (`:1516-1523`).
- **mine_quorum / mine_cycle_quorum** (`:2235-2540`):
  - `spork show`, `quorum list`, `quorum info`, `getblocktemplate`.
  - `quorum dkgstatus` fields used: `session[].{llmqType,quorumHash,status{phase,…}}`, `quorumConnections[].{llmqType,quorumHash,quorumConnections[{proTxHash,connected,outbound}]}`, `minableCommitments[].{llmqType,quorumHash,quorumPublicKey}`.
  - `protx info` `.metaInfo.lastOutboundSuccessElapsed/lastOutboundAttemptElapsed` (probes), `protx diff 1 <h>` `.mnList[].proRegTxHash` (`:2364`), `quorum dkginfo`, `quorum rotationinfo`.
  - Signing helpers: `quorum sign/hasrecsig/getrecsig/selectquorum/platformsign/verify`, `getbestchainlock`, `getrawtransaction` (`instantlock`), `getblock` (`chainlock`), `verifyislock`.

### 3.4 RPC surface census (`/tmp/r4-verif-scripts/census.txt`)

The script regex-scans `.<rpc>(` across `test/functional` (tests plus framework), excluding `log.debug`, against 215 known methods (`help` output plus hidden methods).

- **209 distinct methods are used.** The never-used ones are `cleardiscouraged`, `echojson`, `getblockheaders`, `getquorumproofchain`, `importelectrumwallet`, `voteraw`.
- **By number of files** (call sites in parentheses):
  - `generate` 161 (745). This is the framework helper and maps to `generatetoaddress`; the `generate` RPC itself throws (`rpc/mining.cpp:443-450`).
  - getnewaddress 85 (680), getbestblockhash 83 (333), getblockcount 80, getblock 66, sendtoaddress 65, sendrawtransaction 60, createwallet 46, getrawmempool 42, getblockhash 42, generatetoaddress 40, invalidateblock 39, getaddressinfo 37, createrawtransaction 37, getpeerinfo 36, getbalance 33, setmocktime 33, signrawtransactionwithwallet 32, getblockheader 32, getrawtransaction 31, **sporkupdate 30**, listunspent 28, getwalletinfo 26, submitblock 25, gettransaction 24, unloadwallet 24, getnetworkinfo 22, loadwallet 21, decoderawtransaction 20, getblockchaininfo 20, **quorum 18 (146)**, fundrawtransaction 18, importdescriptors 18, **protx 17 (187)**, getrawchangeaddress 17, importprivkey 17, reconsiderblock 17, getmempoolinfo 16, walletpassphrase 16, testmempoolaccept 15, listtransactions 15, getblocktemplate 15, encryptwallet 15, …, gettxoutsetinfo 8 (40), masternode 7, mockscheduler 7, gobject 6 (110), masternodelist 6, getbestchainlock 5, bls 5, mnauth 4, mnsync 4, spork 4, getislocks 3, verifychainlock 2, getassetunlockstatuses 1, getcreditpoolinfo 1, getspecialtxes 1, verifyquorumproofchain 1, getchainlockbyheight 1, dumptxoutset 1, submitchainlock 1.
- **Dash sub-commands** (files/calls): protx info 12/71, quorum info 9/13, quorum list 8/27, quorum selectquorum 7/15, protx diff 6/9, protx list 5/19, bls generate 5/14, quorum getrecsig 5/9, gobject list 4/35, quorum sign 4/17, spork show 4/10, quorum hasrecsig 4/10, gobject submit 4/7, mnsync status 4/4, gobject get 3/24, masternode payments 3/9, masternode list 3/7, gobject prepare 3/3, quorum verify 2/13, gobject count 2/12, protx listdiff 2/8, quorum dkginfo 2/5, quorum rotationinfo 2/5, masternode status 2/4, quorum isconflicting 2/4, quorum platformsign 2/3, mnsync next 2, masternode winners 2, protx shared_sign/combine/update_registrar_prepare/update_share/dissolve (v24, 1 file each), quorum dkgsimerror 1/13, quorum getdata 1/5, quorum dkgstatus (framework), protx register/register_fund/update_service/update_registrar/revoke (framework, via a variable command name).
- **Wallet RPCs:** 71 distinct wallet methods are used. Per file, `/tmp/r4-verif-scripts/per_file_wallet.txt`.
- **Built-in dynamic alternative:** `--coveragedir` makes the framework record every RPC it calls (`test_framework.py:209-210`, `coverage.py`).

### 3.5 Categorization and tiers

There are 264 scripts:
- 79 `feature_*`, 52 `p2p_*`, 46 `rpc_*`, 58 `wallet_*`, 15 `mempool_*`, 10 `interface_*`, 3 `mining_*`, plus `tool_wallet`. (`ls test/functional/*.py` gives 268 files including runner, cache, combine and example.)

Wallet and DTF dependence (`/tmp/r4-verif-scripts/test_matrix.txt`):
- 151 have neither DTF nor `skip_if_no_wallet`. Many of these use `MiniWallet`, a Python wallet signing raw transactions with no node wallet.
- 83 require the node wallet.
- 34 are DashTestFramework (wallet needed on node 0 only).

`EXTENDED_SCRIPTS` (not run by default) are `feature_pruning`, `feature_dbcrash`, `feature_index_prune`, `wallet_pruning` (`test_runner.py:90-97`).

**Tier 1: core consensus and validation, single node, no wallet or MiniWallet, P2P plus RPC** (35 tests, 52 RPC methods):
- `feature_block` (the big BTC-derived block consensus test via P2P with `reject_reason`s; args `-acceptnonstdtxn=1 -testactivationheight=bip34@2 …`, `feature_block.py:85-95`).
- `feature_cltv`, `feature_dersig`, `feature_csv_activation`, `feature_nulldummy`, `feature_bip68_sequence`.
- `feature_utxo_set_hash` (golden hashes), `feature_coinstatsindex`.
- `feature_new_quorum_type_activation` (Dash dynamic versionbits: `-vbparams=testdummy:0:999999999999:0:10:8:6:5:0`), `feature_versionbits_warning`.
- `feature_assumevalid`, `feature_minchainwork`, `feature_reindex`, `feature_loadblock`.
- `feature_quorum_proof_chain` (data file).
- `p2p_invalid_block`, `p2p_invalid_tx`, `p2p_unrequested_blocks`, `p2p_sendheaders`, `p2p_sendheaders_compressed`, `p2p_compactblocks`, `p2p_dos_header_tree` (testnet headers, so real X11 and DGW), `p2p_mutated_blocks`.
- `rpc_invalidateblock`, `rpc_preciousblock`, `rpc_getchaintips`, `rpc_blockchain`, `rpc_getblockstats` (golden JSON), `rpc_txoutproof`, `rpc_dumptxoutset`.
- `mempool_accept`, `mempool_reorg`, `mempool_spend_coinbase`, `mempool_resurrect`, `mining_basic`.

Its RPC set is: getbestblockhash, generatetoaddress, getblockcount, getblock, invalidateblock, getblockhash, gettxoutsetinfo, getrawmempool, sendrawtransaction, getchaintips, getblockheader, setmocktime, submitblock, submitheader, verifytxoutproof, getblocktemplate, getnetworkhashps, getblockchaininfo, preciousblock, gettxoutproof, verifyquorumproofchain, getpeerinfo, getblockstats, getdeploymentinfo, reconsiderblock, getmininginfo, testmempoolaccept, getnetworkinfo, quorum list, getmempoolinfo, getchaintxstats, gettxout, prioritisetransaction, waitforblockheight, generatetodescriptor, createrawtransaction, signrawtransactionwithkey, createmultisig, validateaddress, generateblock, setnetworkactive, verifychain, getdifficulty, getrawtransaction, decoderawtransaction, dumptxoutset. A few wallet calls (createwallet, importprivkey, settxfee, send, sendtoaddress) appear in optional sub-paths.

Framework baseline on top of that: addnode, disconnectnode, getpeerinfo (`subver`, `inbound`, `bytesrecv_per_msg`, `id`, `services`, `dstaddr/dstport`), getnetworkinfo (`subversion`), getmempoolinfo (`loaded`), syncwithvalidationinterfacequeue, mockscheduler, stop, getrawmempool.

**Tier 2: Dash consensus with masternodes and quorums** (DTF, 29 tests, 115 methods including wallet ones on node 0):
- `feature_dip3_deterministicmns` (plain BTF but needs a wallet), `feature_dip4_coinbasemerkleroots` (cbtx merkle roots via `protx diff`/mnlistdiff P2P).
- `feature_llmq_signing`, `feature_llmq_chainlocks`, `feature_llmq_chainlocks_automatic`, `feature_llmq_rotation` (9 nodes, 8 MNs), `feature_llmq_is_cl_conflicts`, `feature_llmq_is_retroactive`, `p2p_instantsend`.
- `feature_asset_locks`, `feature_mnehf` (EHF).
- `feature_governance`, `feature_governance_cl` (superblocks).
- `feature_llmq_simplepose`, `feature_llmq_dkgerrors`, `feature_llmq_evo`, `feature_llmq_singlenode`.
- `feature_protx_version`, `feature_masternode_shares`, `feature_masternode_payout_shares` (v24).
- `rpc_verifyislock`, `rpc_verifychainlock`, `rpc_quorum`, `feature_sporks`, `p2p_quorum_data`, `feature_llmq_data_recovery` (7 nodes), `feature_llmq_connections` (11 nodes), `feature_llmq_dkg_intake`.

Node and MN counts per test are in §3.3 or from grep, for example chainlocks 5/4, signing 6/5, rotation 9/8, dip4 4/3, mnehf 2/1.

The masternode-side RPC surface is: quorum dkgstatus, quorum sign/verify/hasrecsig/getrecsig/selectquorum/platformsign/getdata/isconflicting/info/list/listextended/memberof/rotationinfo/dkginfo/dkgsimerror, getbestchainlock, verifychainlock, verifyislock, submitchainlock, getislocks, gettxchainlocks, protx info/list/diff/listdiff, masternode status/payments/winners/count, masternodelist, mnsync status/next/reset, spork show/active, setmnthreadactive, mnauth, getassetunlockstatuses, getcreditpoolinfo, getspecialtxes, getchainlockbyheight, getsuperblockbudget, getgovernanceinfo, gobject list/get/count/getcurrentvotes.

**Tier 3: P2P behavior, fingerprint and policy, node ops.** Use these after Tier 1/2, as fingerprint-conformance tests:
- `p2p_leak` (no messages before verack except sendaddrv2), `p2p_fingerprint` (stale-block hiding), `p2p_ping`, `p2p_timeouts`, `p2p_addr_relay`, `p2p_addrv2_relay`, `p2p_getaddr_caching`, `p2p_v2_transport`, `p2p_v2_encrypted`, `p2p_v2_misbehaving`, `p2p_eviction`, `p2p_tx_download`, `p2p_ibd_stalling`, `p2p_ibd_txrelay`, `p2p_blocksonly`, `p2p_filter`, `p2p_blockfilters`, `p2p_node_network_limited`, `p2p_invalid_messages`, `p2p_dstx`, `p2p_govsync_bloom`, `p2p_platform_ban`, `p2p_sendtxrcncl`, `p2p_message_capture`, `p2p_disconnect_ban`, `p2p_permissions`.
- `mempool_*` (policy), `mining_getblocktemplate_longpoll`, `mining_prioritisetransaction`, `feature_pruning`, `feature_maxuploadtarget`, `feature_maxtipage`, `feature_config_args`, `interface_rest`, `interface_rpc`, `interface_http`, `interface_zmq(_dash)`, `rpc_net`, `rpc_setban`, `rpc_misc`, and so on.
- All 58 `wallet_*` tests apply only if a wallet is implemented.

**Feasibility checklist for driving a Rust node with the framework:**
1. **CLI and config.** Accept or ignore every arg and dash.conf key in §3.1, including `-uacomment`, `-maxsigcachesize`, `-parbls`, `-createwalletbackups`, `tinyblk`, `rpcdoccheck`, `unsafesqlitesync`, `keypool`, `fallbackfee`. Tests also pass about 250 distinct args. Top ones: `-whitelist` 71, `-testactivationheight` 50, `-wallet` 40, `-txindex` 35, `-prune` 33, `-dip3params` 15, `-vbparams` 11, `-disablegovernance` 12, `-coinstatsindex` 9, `-masternodeblsprivkey` 8, `-sporkkey` 3, `-llmqtest*params`, `-budgetparams`.
2. **Data directory.** `<datadir>/regtest/` with `.cookie`, `debug.log` and the cache dir names above.
3. **RPC.** JSON-RPC 1.0/2.0 over HTTP with cookie auth. Error codes must match, because tests use `assert_raises_rpc_error(code, msg)`, which checks exact strings. Warm-up returns -28.
4. **P2P.** The handshake must match §5, including immediate ping/pong and per-message byte accounting in getpeerinfo.
5. **Test RPCs.** `setmocktime`, `mockscheduler`, `generatetoaddress`, `generateblock`, `generatetodescriptor`, `invalidateblock`, `reconsiderblock`, `submitblock`, `submitheader`, `syncwithvalidationinterfacequeue`, `waitforblockheight`.
6. **Wallet.** Either implement one, or run mixed networks with dashd as node 0 (wallet, sporkkey, mining) and Rust nodes as simple nodes or masternodes. Rust MNs need no wallet. DTF's `create_raw_tx` and `prepare_masternode` run on node 0.
7. **Debug log.** Matching `debug.log` strings are needed for about 63 tests. Alternatively, patch `assert_debug_log` per node.

---

## 4. State-comparison RPCs (per-height dashd ↔ Rust diffing)

"Hist." means the RPC can be queried at an arbitrary past height. "Cost" is a rough per-call cost on mainnet.

| RPC (file:line) | Params | Key output fields | Hist.? | Cost / notes |
|---|---|---|---|---|
| `getblockhash h`, `getblock hash verbosity` (`rpc/blockchain.cpp:956`) | verbosity 0 (hex) / 1 / 2 (+tx JSON, `fee` if undo) / 3 (+`prevout{generated,height,value,scriptPubKey}`) | hash, confirmations, height, version(Hex), merkleroot, time, mediantime, nonce, bits, difficulty, chainwork, nTx, prev/next, **chainlock** (node state; exclude), size, tx[], **cbTx{version,height,merkleRootMNList,merkleRootQuorums,bestCLHeightDiff,bestCLSignature,creditPoolBalance,merkleRootAssetUnlocks}** | yes | O(1) disk read. Verbosity 0 gives the raw-block byte comparison. |
| `getblockheader hash verbose` (`:609`) | | hash…nTx, chainlock | yes | cheap |
| `getrawtransaction txid verbosity blockhash` | txindex is **on by default in Dash** (`index/txindex.h:10`, help "-txindex default: 1") | special-tx JSON: proRegTx, proUpServTx, proUpRegTx, proUpRevTx, cbTx, qcTx{commitment}, mnhfTx, assetLockTx{creditOutputs}, assetUnlockTx{index,fee,requestedHeight,quorumHash,quorumSig,instanceHash}, proDisTx, proUpShareTx, proUpSharedRegTx; instantlock / instantlock_internal / chainlock (node state) | yes | cheap |
| `getspecialtxes blockhash type count skip verbosity` (`:2456`) | type −1 = all | txids, hex or JSON | yes | cheap |
| `getblockstats hash_or_height [stats]` (`:2212`) | | avgfee, …, subsidy, totalfee, txs, utxo_increase(_actual), utxo_size_inc(_actual) | yes (needs undo) | block plus undo read |
| `gettxoutsetinfo hash_type hash_or_height use_index` (`:1177`) | hash_type `hash_serialized_2` (default) / `muhash` / `none` | height, bestblock, txouts, bogosize, hash_serialized_2 or muhash, total_amount; without index: transactions, disk_size; with index: total_unspendable_amount, block_info{prevout_spent, coinbase, new_outputs_ex_coinbase, unspendable, unspendables{genesis_block, scripts, unclaimed_rewards}} | **only with `-coinstatsindex` and only `muhash`/`none`** (`:1253-1261`) | With the index, O(1) per height after it syncs. Without it, a full UTXO scan at the tip only (minutes on mainnet). |
| `dumptxoutset path` (hidden, `:3140`) | | coins_written, base_hash, base_height, path, txoutset_hash (= hash_serialized_2), nchaintx | tip only | Format: `SnapshotMetadata{base_blockhash, coins_count}` (`node/utxo_snapshot.h:43`), then repeated `COutPoint` plus `Coin` in the compressed DB encoding (`rpc/blockchain.cpp:3239-3250`). Gives the full UTXO set for set-diffing. Mainnet assumeutxo data is empty; regtest has heights 110/200 (`chainparams.cpp:341,878-886`). |
| `getblockchaininfo` | | chain, blocks, headers, bestblockhash, difficulty, time, mediantime, verificationprogress, initialblockdownload, chainwork, size_on_disk, pruned…; `softforks` **only with `-deprecatedrpc=softforks`** | tip | |
| `getdeploymentinfo [blockhash]` (`:1697`) | | per deployment: type buried/bip9, height, active, bip9{bit,start_time,timeout,**ehf**, ehf_height, activation_height, min_activation_height, status, since, status_next, statistics{period,threshold,elapsed,count,possible}, signalling} | **yes** | cheap. Regtest list: bip34, bip66, bip65, bip147, csv, dip0001, dip0003, dip0008, dip0020, dip0024, realloc, v19, v20, mn_rr, withdrawals (buried); v24, evo_shares (bip9+EHF); testdummy. |
| `getchaintips [count branchlen]` (`:1828`) | | height, hash, difficulty, chainwork, branchlen, forkpoint, status (invalid / headers-only / valid-headers / valid-fork / active / **conflicting** (CL)) | tip view | |
| `protx list type detailed height` (`rpc/evo.cpp:1628`) | type registered / valid / evo / wallet | detailed DMN JSON: proTxHash, collateralHash/Index, type, state{version, service (deprecated), addresses{core_p2p, platform_p2p, platform_https}, registeredHeight, lastPaidHeight, consecutivePayments, PoSePenalty, PoSeRevivedHeight, PoSeBanHeight, revocationReason, ownerAddress, votingAddress, platformNodeID, platformHTTPPort, shares, earlyPeriodBlocks, earlyPenalty, payouts, payoutAddress, pubKeyOperator, operatorPayoutAddress} (`evo/core_write.cpp`) | **yes** | Loads the MN list from evodb: nearest snapshot (every `DISK_SNAPSHOT_PERIOD=576` blocks, `evo/deterministicmns.h:760`) plus up to 575 diffs. Gives the **full** DMN state, a superset of the cbTx merkleRootMNList commitment. |
| `protx info proTxHash [blockHash]` (`:1753`) | | DMN JSON plus metaInfo (node-local) | yes | cheap |
| `protx diff baseBlock block [extended]` (`:1854`), `protx listdiff` (`:1898`) | heights or hashes | nVersion, baseBlockHash, blockHash, cbTxMerkleTree, cbTx, deletedMNs, mnList[SML entries: nVersion, nType, proRegTxHash, confirmedHash, addresses, pubKeyOperator, votingAddress, isValid, platform*, payoutAddress / payouts / shares (extended)], deletedQuorums, newQuorums[full commitment], merkleRootMNList, merkleRootQuorums, quorumsCLSigs | yes | The same data as P2P `mnlistdiff`, so it is good for diff-chain checks. Observed quirk: on regtest `protx diff 1 100` (pre-DIP3) returns `blockHash: 000…0`. |
| `quorum list [count]` (`rpc/quorums.cpp:50`) | | per llmq type: hashes | tip | |
| `quorum listextended [height]` (`:109`) | | per type: quorumHash → {creationHeight, quorumIndex, minedBlockHash, numValidMembers, healthRatio} | **yes** | |
| `quorum info llmqType hash [includeSkShare]` (`:249`) | | height, type, quorumHash, quorumIndex, minedBlock, members[{proTxHash, pubKeyOperator, valid, pubKeyShare}], quorumPublicKey, secretKeyShare | any mined quorum | |
| `quorum memberof proTxHash [count]` (`:445`), `quorum selectquorum type id` (`:817`) | | | tip | |
| `quorum rotationinfo blockRequestHash [extraShare] [baseBlockHashes]` (`:959`) | | qrinfo-equivalent JSON | yes | heavy |
| `quorum dkgstatus` (`:336`) | | session, quorumConnections, minableCommitments | node state | MN conformance only |
| `getbestchainlock` (`blockchain.cpp:253`) | | blockhash, height, signature, known_block, hex | tip and node state | |
| `getchainlockbyheight h` (`quorums.cpp:1404`) | | height, blockhash, signature, cbtx_height (read from coinbases on disk) | **yes** | per-height CL embedded in cbTx |
| `getassetunlockstatuses [idx…] [height]` (`rpc/rawtransaction.cpp:781`) | ≤100 indexes | {index, status chainlocked / mined / mempooled / unknown, instantlock} | **yes** (height) | |
| `getcreditpoolinfo [height]` (`blockchain.cpp:1760`) | | height, blockhash, balance, currentlimit, window{blocks, height, balance, unlocked} | **yes** | reads the 576-block window (100 on regtest) |
| `masternode payments [blockhash] [count]` (`rpc/masternode.cpp:353`) | count ±N | per block: height, blockhash, amount, masternodes[{proTxHash, amount, payees[{address, script, amount}]}] | **yes** | per-block payee selection check |
| `masternode winners [count] [filter]` (`:286`) | | height → payee | near tip (projected) | |
| `getblocktemplate` | | adds masternode[{payee, script, amount}], masternode_payments_started/enforced, superblock[], superblocks_started/enabled, coinbase_payload | tip | miner-equivalence check |
| `gobject list [signal] [type]` (`rpc/governance.cpp:716`), `gobject getcurrentvotes` (`:773`), `getgovernanceinfo`, `getsuperblockbudget idx` | | governance objects and votes | **tip only; P2P-synced, not chain state** | `getsuperblockbudget` is deterministic per height |
| `getmerkleblocks filter blockhash [count]` (`blockchain.cpp:829`) | | merkleblock hex[] | yes | BIP37 serving parity |
| `verifychain [checklevel nblocks]` (`:1415`) | | bool | | self-check only |

Per-height comparison cadence:
- **Every block, cheap:** `getblock 0` (raw-byte equality is trivial if the Rust node serves blocks), `getblockheader`, `getblockstats` (utxo_increase, subsidy, totalfee), `gettxoutsetinfo muhash <h>` (with coinstatsindex on both sides), `masternode payments <hash>`, `getchainlockbyheight`.
- **Every N blocks** (for example each 576-block DMN snapshot boundary, or every DKG interval): `protx list registered true <h>`, `protx diff <h-N> <h>`, `quorum listextended <h>`, `getcreditpoolinfo <h>`, `getdeploymentinfo <hash>`.
- **At replay end (tip):** `gettxoutsetinfo` (hash_serialized_2), `dumptxoutset` full-set diff, `getchaintips`, `getbestchainlock`, `gobject list`.

### 4.1 UTXO hash serialization (verified against a live dashd)

Source: `src/kernel/coinstats.cpp`.
- Iteration order: the LevelDB coins cursor, i.e. raw key order. Keys are `'C' || txid (32 bytes, internal/LE byte order) || VARINT(vout)`. Outputs are grouped per txid into a `std::map<uint32,Coin>` (`:116-154`), so the order is **txid bytes ascending (internal order), then vout ascending**. The chainstate never contains unspendable (OP_RETURN) outputs.
- **hash_serialized_2** (`HashWriter`, which is double SHA-256):
  - `ss << hashBlock` (best block, internal order) (`:185-188`).
  - For each txid group: `txid(32) || VARINT((nHeight*2+fCoinBase) ? 1 : 0)` **← quirk** (`:77`). Then for each output: `VARINT(vout+1) || CScript (compactsize len + bytes) || VARINT_NONNEGATIVE_SIGNED(nValue)`. Then `VARINT(0)` after the last output (`:72-88`).
  - Final hash is `GetHash()`, displayed byte-reversed.
  - VARINT is the Bitcoin MSB base-128 "minus one" encoding.
- **muhash** (`:92-99`, `TxOutSer` `:51-58`). Insert, for each UTXO, `outpoint(txid32 || u32le vout) || u32le(nHeight*2+fCoinBase) || CTxOut(i64le value || compactsize script)`. Each element maps to a Num3072: SHA256(data) is the ChaCha20 key, take 384 bytes of keystream with nonce 0, read as a little-endian 3072-bit number. Multiply all elements mod 2^3072−1103717. Final value is SHA256 of the 384-byte LE value, displayed reversed. The hash is order-independent and does not commit to the block hash.
- **Verification run:** regtest, 110 coinbase-only blocks, dashd 24.0.0-rc.3.

  ```
  dashd hash_serialized_2  = 918460f97be4b829192fba63e38853725b9bf9d2705edc7daf1dcca266ff40c8
  python (with quirk)      = 918460f9…40c8   MATCH
  python (without quirk)   = 0aeefa6b…cd30   mismatch
  dashd muhash             = 29ba70042ec5ed7a5b3e0ca445b318bdfdba95643382cb52d44e8d9f7eb92963
  python muhash            = 29ba7004…2963   MATCH
  ```

  Reproduce with `python3 -I /tmp/r4-verif-scripts/utxohash.py <dash-cli … args>`.
- **coinstatsindex** (`src/index/coinstatsindex.cpp:117-210`) keeps a running MuHash per block.
  - It inserts new outputs, skipping `IsUnspendable` scripts but adding their value to `unspendables.scripts`.
  - It removes spent outputs using undo data.
  - The genesis subsidy goes to `unspendables.genesis_block`.
  - `unclaimed_rewards = (prevout_spent + subsidy) − (new_outputs_ex_coinbase + coinbase + unspendable)` (`:208`), using `GetBlockSubsidy(pindex)` (`:124`). Asset unlocks create outputs without prevouts, so this can go negative; the Rust side must mirror it exactly.

---

## 5. P2P and fingerprinting surface

### 5.1 Constants

- **Message types** (`src/protocol.cpp:18-94`):
  - BTC: version, verack, addr, addrv2, sendaddrv2, inv, getdata, merkleblock, getblocks, getheaders, tx, headers, block, getaddr, mempool, ping, pong, notfound, filterload, filteradd, filterclear, sendheaders, sendcmpct, cmpctblock, getblocktxn, blocktxn, getcfilters, cfilter, getcfheaders, cfheaders, getcfcheckpt, cfcheckpt, sendtxrcncl.
  - Dash: spork, getsporks, dsa, dsi, dsf, dss, dsc, dssu, dstx, dsq, senddsq, ssc, govsync, govobj, govobjvote, getmnlistd, mnlistdiff, qsendrecsigs, qfcommit, qcontrib, qcomplaint, qjustify, qpcommit, qwatch, qsigsesann, qsigsinv, qgetsigs, qbsigs, qsigrec, qsigshare, qgetdata, qdata, clsig, isdlock, mnauth, getheaders2, sendheaders2, headers2, getqrinfo, qrinfo, platformban.
  - **Absent:** feefilter, wtxidrelay, reject, sendpackages.
- **Inventory types** (`protocol.h:497-527`): TX 1, BLOCK 2, FILTERED_BLOCK 3, SPORK 6, DSTX 16, GOVERNANCE_OBJECT 17, GOVERNANCE_OBJECT_VOTE 18, CMPCT_BLOCK 20, QUORUM_FINAL_COMMITMENT 21, QUORUM_CONTRIB 23, QUORUM_COMPLAINT 24, QUORUM_JUSTIFICATION 25, QUORUM_PREMATURE_COMMITMENT 26, QUORUM_RECOVERED_SIG 28, CLSIG 29, ISDLOCK 31, DSQ 32, PLATFORM_BAN 33, **ASSET_UNLOCK 34** (new in 70242). Retired: 4, 5, 22, 27, 30.
- **Versions** (`src/version.h:14-77`):
  - `PROTOCOL_VERSION 70242`, `INIT_PROTO_VERSION 209`, `MIN_PEER_PROTO_VERSION 70221`, `MIN_MASTERNODE_PROTO_VERSION 70242`.
  - Feature versions: ADDRV2 70223, BLS_SCHEME 70225, DMN_TYPE 70227, SMNLE_VERSIONED 70228, MNLISTDIFF_VERSION_ORDER 70229, MNLISTDIFF_CHAINLOCKS 70230, NO_LEGACY_ISLOCK 70231, DSQ_INV 70234, INCREASE_MAX_HEADERS2 = BIP324_DASH_BASELINE 70235, EFFICIENT_QRINFO 70236, ISDLOCK_CYCLEHASH_UPDATE 70237, PLATFORM_BAN 70238, QFCOMMIT_STALE_REPROP_BAN 70239, PLATFORMBAN_V2_SHORT_ID 70240, COINJOIN_REBALANCE 70241, ASSET_UNLOCK_INV 70242.
  - What the network runs: the mainnet insight server reports 70235 (v22.0); testnet insight reports 70242 (v24.0.0).
- **Service bits** (`protocol.h:314-331`): NETWORK 1<<0, BLOOM 1<<2, COMPACT_FILTERS 1<<6, NETWORK_LIMITED 1<<10, **HEADERS_COMPRESSED 1<<11**, P2P_V2 1<<12. No WITNESS bit.
  - Base value `NETWORK_LIMITED|HEADERS_COMPRESSED` (`init.cpp:1125`).
  - `+P2P_V2` if `-v2transport`, which **defaults to true** (`net.h:142`, `init.cpp:1236`).
  - `+COMPACT_FILTERS` if `-peerblockfilters` (default off, but on for masternodes) (`:1245`, `:1099`).
  - `+BLOOM` if `-peerbloomfilters`, **default true** (`net_processing.h:44`, `init.cpp:1371`).
  - `+NETWORK` when not pruning (`:2354`).
  - Observed: `0x1c05`.
- **User agent:** `FormatSubVersion("Dash Core", CLIENT_VERSION, uacomments)` produces `/Dash Core:24.0.0(comment1; comment2)/` (`clientversion.cpp:23,54-83`). The version string is `major.minor.build` with no rc suffix: rc.3 reports `/Dash Core:24.0.0/`. On devnets `devnet.<name>` is auto-prepended to the comments (`init.cpp:1828-1840`). Max length is 256 (`net.h:86`).
- **Version message** (`net_processing.cpp:1560-1592`), serialized with INIT_PROTO_VERSION:
  - `nVersion, services, nTime, addrYou(services+ip+port, no time), addrMe(services, CService()), nonce, strSubVer, nStartingHeight, fRelay, **mnauthChallenge (uint256, random)**, **fMasternode (bool = m_masternode_connection)**`.
  - `-pushversion` overrides nVersion on non-mainnet chains only.
  - Total 137 bytes for the stock UA.

### 5.2 Handshake and message order

Code: `net_processing.cpp:3931-4254`. Observed on 24.0.0-rc.3 with an inbound v1 connection:
1. dashd sends `version` immediately on inbound accept, after the peer's version.
2. On the peer's `version`: `sendaddrv2` (if common version ≥70223, `:4052`), [`sendtxrcncl` only if `-txreconciliation` is on (default false, `node/txreconciliation.h:15`) and version ≥70235, `:4094`], `verack` (`:4099`), [`getaddr` to **outbound** peers only, `:4123`].
3. On the peer's `verack`: [`mnauth` if we are a masternode and this is not a probe, `:4202`], `sendheaders2` if the peer advertised NODE_HEADERS_COMPRESSED else `sendheaders` (`:4209`; `UsesCompressedHeaders` `:1195-1198`), `sendcmpct(high_bandwidth=0, version=1)` if CanRelay (`:4217`), `senddsq(true)` unless rejecting txs (`:4222`), [`qwatch` if watching quorums and the peer is a quorum node, `:4226`].
4. From `SendMessages` (`:6296`): immediate `ping` (`:6165`, PING_INTERVAL 2 min). `getheaders2`/`getheaders` if the peer can serve blocks and either no sync has started or our best header is within max_tip_age (`:6364-6390`). Addr broadcast (AVG 30 s; local-address self-announce about every 24 h).
5. Masternode-sync ticks (`node/sync_manager.cpp`, tick 6 s, `masternode/sync.h:22-23`): `getsporks` once per peer address ("spork-sync" netfulfilled, `:204-212`), then `govsync` during the governance asset, and `mempool` to outbound non-block-relay peers after blockchain sync when `-syncmempool` (default 1) (`:230-240`). During sync dashd disconnects peers it already "full-synced" from recently (`:197-201`).

Captured output from `handshake.py`:

```
peer services 0x0:     version(137B) sendaddrv2 verack sendheaders sendcmpct(000100000000000000) senddsq(01) ping | +2.2s getsporks | +8.2s govsync(42B)
peer services 0x0c05:  version sendaddrv2 verack sendheaders2 sendcmpct senddsq ping getheaders2(613B) | +0.5s getsporks
```

No `feefilter` was ever sent. This matches the code: there is no FEEFILTER in Dash, and the BIP324 slot 5 is commented "FEEFILTER is not implemented in Dash" (`net.cpp:~900`).

### 5.3 BIP324

- On by default (`DEFAULT_V2_TRANSPORT{true}`, `net.h:142`).
- Short IDs: BIP324 1..28 for BTC messages (slot 5 blank), and **Dash IDs 128..168**: spork, getsporks, senddsq, dsa, dsi, dsf, dss, dsc, dssu, dstx, dsq, ssc, govsync, govobj, govobjvote, getmnlistd, mnlistdiff, qsendrecsigs, qfcommit, qcontrib, qcomplaint, qjustify, qpcommit, qwatch, qsigsesann, qsigsinv, qgetsigs, qbsigs, qsigrec, qsigshare, qgetdata, qdata, clsig, isdlock, mnauth, getheaders2, sendheaders2, headers2, getqrinfo, qrinfo, platformban (`net.cpp:~900-1035`).
- The `platformban` short ID is gated on version ≥70240 (`V2_NEW_SHORT_ID_VERSIONS`).
- `addnode … v2transport` is the RPC parameter.

### 5.4 Timers and limits (`net.h`, `net_processing.cpp:85-222`)

- **Connection:** `TIMEOUT_INTERVAL` 20 min (ping timeout), `PING_INTERVAL` 2 min, `DEFAULT_PEER_CONNECT_TIMEOUT` 60 s (handshake), `FEELER_INTERVAL` 2 min, `MAX_OUTBOUND_FULL_RELAY` 8, `MAX_BLOCK_RELAY_ONLY` 2, `MAX_FEELER` 1, `MAX_ADDNODE` 8, `DEFAULT_MAX_PEER_CONNECTIONS` 125, anchors 2.
- **Message sizes:** `MAX_PROTOCOL_MESSAGE_LENGTH` 3 MiB, `MAX_INV_SZ` 50000, `MAX_GETDATA_SZ` 1000, `MAX_LOCATOR_SZ` 101.
- **Headers:** `MAX_HEADERS_UNCOMPRESSED_RESULT` 2000, **`MAX_HEADERS_COMPRESSED_RESULT` 8000** when version ≥70235 and headers2 (`validation.h:81-82`, `GetHeadersLimit` `net_processing.cpp:1209-1214`). `HEADERS_DOWNLOAD_TIMEOUT` 15 min + 1 ms/header, `HEADERS_RESPONSE_TIME` 2 min, `MAX_UNCONNECTING_HEADERS` 10.
- **Block download:** `BLOCK_DOWNLOAD_WINDOW` 1024, `MAX_BLOCKS_IN_TRANSIT_PER_PEER` 16, stalling timeout 2 s→64 s, `MAX_BLOCKS_TO_ANNOUNCE` 8, `MAX_CMPCTBLOCK_DEPTH` 5, `MAX_BLOCKTXN_DEPTH` 10, `NODE_NETWORK_LIMITED_MIN_BLOCKS` 288, `CHAIN_SYNC_TIMEOUT` 20 min, `STALE_CHECK_INTERVAL` 150 s, `EXTRA_PEER_CHECK_INTERVAL` 45 s, `MINIMUM_CONNECT_TIME` 30 s.
- **Fingerprint protection:** stale-block hiding (`STALE_RELAY_AGE_LIMIT` 30 days, `HISTORICAL_BLOCK_AGE` 7 days).
- **Inventory trickle:** inbound Poisson mean 5 s, outbound 2 s (1 s to verified MN outbound peers) (`:6594-6606`). NoBan peers and MN↔MN links trickle immediately. Rate `INVENTORY_BROADCAST_PER_SECOND` 7, max per 1 MB block 140, `INVENTORY_MAX_RECENT_RELAY` 3500. Only TX and DSTX are queued for trickling; non-tx invs go out via `vInventoryOtherToSend` (`:1244-1255`).
- **Object re-request intervals:** QUORUM_RECOVERED_SIG 15 s, CLSIG 5 s, ISDLOCK 10 s (`:1594-1605`). `GETDATA_TX_INTERVAL` 60 s, `NONPREF_PEER_TX_DELAY` 2 s.
- **Addr relay:** `MAX_ADDR_TO_SEND` 1000, `MAX_PCT_ADDR_TO_SEND` 23 %, token bucket 0.1/s, max 1000 (BTC semantics).
- **Misbehavior:** Dash **still uses scores**, with `DISCOURAGEMENT_THRESHOLD` 100 (`net_processing.h:47`).
  - Block results (`MaybePunishNodeForBlock` `:1965-2025`): CONSENSUS/MUTATED 100, CACHED_INVALID 100 (conditional), INVALID_HEADER/CHECKPOINT/INVALID_PREV 100, **MISSING_PREV and BLOCK_CHAINLOCK 10**, TIME_FUTURE 0.
  - TX_CONSENSUS 100.
  - Size violations score 20: oversize addr, inv, getdata, headers, notfound (`:4404,4508,4614,5321,5749`).
  - Non-connecting headers 20, invalid header 10, non-continuous headers 20, getblocktxn out-of-bounds 100, invalid cmpctblock 100, mutated block 100, bad bloom filter 100.
  - Unrequested mnlistdiff 100, malformed getqrinfo 100, unrequested qrinfo 100, malformed or invalid spork 100.
  - Unrequested clsig and qfcommit use `UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE` (`:5807`); invalid dstx uses `result.score`.
  - Wrong devnet name: 100 if outbound, 1 if inbound (`:4033-4035`).
  - Ban time 24 h (`banman.h:19`).
- **Eviction:** BTC's `SelectNodeToEvict`. In addition, an **active masternode never evicts inbound peers with a verified ProRegTx, or recently connected peers awaiting MNAUTH** (`net.cpp` `GetEvictionCandidates`). An active MN refuses inbound connections until the blockchain is synced (`net.cpp:1963-1967`).

### 5.5 Fingerprint checklist (observable behaviors a Rust node must match)

1. Version: 137-byte layout with `mnauthChallenge` and `fMasternode` trailing fields; services `0x1c05` (+`0x40` on MNs); UA `/Dash Core:X.Y.Z/`; protocol 70242 (or whatever release is being emulated).
2. Before verack, nothing except `sendaddrv2` and `sendtxrcncl` (which is off). This is what `p2p_leak.py` checks.
3. After verack: `sendheaders2|sendheaders`, then `sendcmpct(0,1)`, `senddsq(1)`, `ping`, then `getheaders2|getheaders`. `getaddr` only to outbound peers. `mnauth` first if a masternode. `qwatch` if a watcher.
4. Mnsync side-effects: `getsporks` once per address, `govsync` during governance sync, `mempool` to outbound peers after sync, and disconnecting recently-synced peers during sync.
5. Never sends `feefilter`, `wtxidrelay` or `reject`. Ignores unknown messages.
6. BIP324 on by default with Dash short IDs (slot 5 empty); v1 fallback.
7. Headers2 compression and 8000-header batches to peers ≥70235.
8. Trickle timing (5 s / 2 s / 1 s for MNs; immediate for MN↔MN and NoBan peers); non-tx invs are immediate.
9. Ping every 2 min, 20 min timeout, 60 s handshake timeout. With mocktime, `peertimeout` is measured in mock time.
10. Misbehavior scoring, with the specific points above, and the 24 h discouragement.
11. Stale-block hiding (>30 days) and NODE_NETWORK_LIMITED serving depth of 288.
12. `getpeerinfo` keys and byte accounting (`bytesrecv_per_msg`), which the framework relies on.

---

## 6. Tools

- **`contrib/linearize/`** (`linearize-hashes.py`, `linearize-data.py`; needs the `dash_hash` module). Builds a fork-free `bootstrap.dat` / blk files for `-loadblock` replay into both nodes.
- **`contrib/message-capture/message-capture-parser.py`** together with dashd `-capturemessages`. Dumps per-peer sent and received message logs, giving byte-level P2P traces for diffing Rust against dashd.
- **`contrib/devtools/utxo_snapshot.sh`**: dumptxoutset helper.
- **`contrib/testgen/gen_key_io_test_vectors.py`**: regenerates key_io vectors.
- `contrib/seeds`, `contrib/zmq`, `contrib/tracing` (USDT; `interface_usdt_*` tests).
- **`test/util/test_runner.py` plus `test/util/data/bitcoin-util-test.json`**: 77 cases (72 `./dash-tx`, 5 `./dash-util`) with 40 `.hex`/`.json` fixtures. These are tx construction and decoding goldens, usable for raw-tx JSON/encoding parity.
- **`dash-chainstate`** (`src/bitcoin-chainstate.cpp`), experimental and not in release tarballs; build with `--enable-experimental-util-chainstate`.
  - Usage: `dash-chainstate DATADIR`. **Mainnet is hard-coded** (`SelectParams(MAIN)`, `:59`).
  - It loads chainstate, evodb, `CDeterministicMNManager`, `LLMQContext`, `CInstantSendManager` and `Chainlocks`, all with empty sporks (`:84-104`). It runs `VerifyLoadedChainstate` and `ActivateBestChain`, then reads **hex blocks from stdin**, calls `ProcessNewBlock`, and prints `BlockValidationState` (`:138-240`).
  - It is a usable headless reference validator for single mainnet blocks or block streams. Caveats: there are no sporks, governance or mnsync, so superblock checks are bounds-only, and there is no ChainLock/IS state. Its accept/reject verdicts can be compared one-to-one with a Rust validator on crafted blocks.
- **dashd flags for reference replays** (confirmed via `-help -help-debug` on rc.3):
  - `-reindex`, `-reindex-chainstate`, `-loadblock=<file>`, `-stopatheight=<h>` (stop at an exact height and then query tip-only RPCs such as hash_serialized_2 and dumptxoutset), `-stopafterblockimport`.
  - `-checkblocks=<n>` (default 6, 0 means all) and `-checklevel=0..4` (default 3).
  - `-assumevalid=0` (mainnet default `0x…119fe428…b492` at block 2487500).
  - `-par=<n>` (max 15), `-parbls`, `-dbcache=4..16384` (default 300).
  - `-coinstatsindex`, `-txindex` (default **1**), `-addressindex`, `-spentindex`, `-timestampindex`, `-blockfilterindex`.
  - `-forceevodbrepair`, `-disablegovernance`, `-watchquorums`, `-capturemessages`, `-pushversion`, `-mocktime`, `-maxtipage`.
  - Regtest knobs: `-testactivationheight=name@h` (bip147, bip34, dersig, cltv, csv, brr, dip0001, dip0008, dip0024, v19, v20, mn_rr), `-vbparams=…`, `-dip3params=a:e`, `-llmq*`, `-budgetparams`, `-highsubsidyblocks/-factor`, `-minimumdifficultyblocks`, `-powtargetspacing`, `-sporkkey`, `-sporkaddr`, `-devnet=<name>`.
- **Release tarball** (`dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz`, 66 MB): `bin/{dashd, dash-cli, dash-qt, dash-tx, dash-util, dash-wallet, test_dash}`, `lib/libdashconsensus.so`, `include/bitcoinconsensus.h`. It is the fastest route to a reference oracle; no build is needed.
- **rust-dashcore `contrib/setup-dashd.py`:**
  - Downloads `dashcore-${DASHVERSION:-23.1.0}-<arch>-linux-gnu.tar.gz` from GitHub releases and `dashpay/regtest-blockchain` `${TEST_DATA_VERSION:-v0.0.4}` variants into `${CACHE_DIR:-~/.rust-dashcore-test}`.
  - The variants are `regtest-40000` (38.8 MB, 40K-block chain), `regtest-200` (1.8 MB) and `regtest-mn` (6.7 MB; "4-masternode network with 8 successfully-mined DKG cycles (`llmq_test` + `llmq_test_dip0024`)", `network.json` plus per-node datadirs).
  - It exports `DASHD_PATH`, `DASHD_TEST_DATA`, `DASHD_MN_DATADIR`.
  - Latest releases: 23.1.8 (2026-08-03); v24.0.0-rc.3 (2026-10-07).
- **rust-dashcore test utilities** (`dash-spv/src/test_utils/`):
  - `node.rs` (604 lines): `DashCoreNode`/`DashCoreConfig` spawn dashd with `-regtest -datadir -port -rpcport -server -rpcbind/-rpcallowip/-bind=127.0.0.1 -listen=1 -txindex=0 -blockfilterindex=1 -peerblockfilters=1 -peerbloomfilters=1 -whitelist=127.0.0.1 -debug=all` (`:154-190`). Cookie RPC via `dashcore_rpc`. Port lanes are 20000–32000, seeded by PID (`:18-55`). Helpers: get_block_count, generate_blocks, send_to_address, send_many, create_signed_transaction, connect_to_node, disconnect_peer, set_network_active, set_mocktime, invalidate_block, get_block_template, try_rpc_call.
  - `masternode_network.rs` (1099 lines): starts the `regtest-mn` controller plus MNs with `-masternodeblsprivkey` (`:175`), with `bump_mocktime`, `move_blocks`, `mine_dkg_cycle(_with_hook)` (`:629-643`), `wait_for_chainlocked_block`, `mine_blocks_and_wait_for_chainlock`. This is a Rust port of the DashTestFramework helpers and can be reused directly as a differential harness that drives dashd and a Rust node side by side.

---

## 7. Chain size facts (for estimating replay time)

- **Mainnet**, live on 2026-10-08:
  - insight.dash.org: `blocks 2551641` (insight server v22.0, protocol 70235).
  - blockchair: best height 2,551,641 at 2026-10-08 03:05 UTC; 71,371,716 transactions; 234.8M outputs ever created; `blockchain_size` 43,270,032,294 bytes (≈43.3 GB); 552 blocks/24 h; 11.5K tx/24 h.
  - Chainparams: `chainTxData` = 69,770,814 tx at block 2,487,500, timestamp 1781318234 (2026-06-13), 0.157 tx/s (`chainparams.cpp:346-351`). `m_assumed_blockchain_size=57` GB and `m_assumed_chain_state_size=1` GB (`:237-238`).
  - Activation heights (`chainparams.cpp:178-195`): BIP34 951, DIP0001 782208, DIP0003 1028160 (enforced 1047200), DIP0008 1088640, BRR 1374912, DIP0024 1737792, V19 1899072, V20 1987776, MN_RR 2128896. V24 and evo_shares are BIP9+EHF with `nStartTime=NEVER_ACTIVE` (TODO) on develop mainnet (`:212-219`).
  - Checkpoints run up to 2487500.
- **Testnet:** insight.testnet.networks.dash.org reports `blocks 1568316`, version 240000, protocol 70242. Chainparams: 8,182,713 tx at block 1,380,000; assumed size 10 GB.
- **Rough replay planning:** about 2.55M mainnet blocks and 71M txs. That is roughly 1/12 of Bitcoin's tx count, so a full-validation replay (`-assumevalid=0`) is plausibly hours, not days, on this 32-vCPU box. This is my estimate; I did not measure it.
  - Script validation is ECDSA only; there is no Schnorr.
  - BLS verification starts at DIP3 (quorum commitments and CLs in cbTx).
  - X11 PoW hashing of 2.55M headers is negligible.

---

## 8. Recommendations for the verification plan

1. **Vector harness (week 1):** port `src/test/data/*.json` verbatim. Transcribe the pow/subsidy/BRR/SML/ISLOCK/quorum-proof/DIP14 embedded vectors. Use `libdashconsensus.so` through FFI as a script oracle.
2. **Differential fuzzing:** run the `script_flags` input format through both libdashconsensus and the Rust interpreter. Add Rust and C++ deserialize targets for every special tx (ProReg/UpServ/UpReg/UpRev, CbTx v1-v3, QcTx, MnHf, AssetLock/Unlock v1/v2, ProDis/ProUpShare/ProUpSharedReg) and compare round-trip bytes. The fuzz deserializers and test_framework `messages.py` give a third, Python implementation.
3. **Chain replay diff:** run dashd and the Rust node with `-coinstatsindex -txindex`, both fed the same linearized blocks via `-loadblock`. Compare per block: block hash and validity, `getblockstats`, `gettxoutsetinfo muhash h`, and `masternode payments`. Every 576 blocks, compare `protx list registered true h` and `quorum listextended h`. At checkpoints, compare `hash_serialized_2` (tip-only, reproducing the quirk) and do a full `dumptxoutset` set-diff. Keep the superblock and IS/CL node-state caveats from §0.5 in mind.
4. **Functional acceptance:** run Tier 1 against a pure Rust node. Run Tier 2 as **mixed networks**: dashd node 0 with the wallet, sporkkey and miner, and Rust nodes as MNs (no wallet needed) through `add_nodes(binary=…)`. Use Tier 3 plus `handshake.py`-style probes and `-capturemessages` traces for fingerprint conformance.

Sources used for the chain-size facts:
- [BitInfoCharts Dash](https://bitinfocharts.com/dash/)
- [Blockchair Dash](https://blockchair.com/dash)
- [insight.dash.org status](https://insight.dash.org/insight-api/status)
- [testnet insight status](https://insight.testnet.networks.dash.org/insight-api/status)
