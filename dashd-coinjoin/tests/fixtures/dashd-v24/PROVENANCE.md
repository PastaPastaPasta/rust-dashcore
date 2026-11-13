# CJ1 transcript provenance

Recorded 2026-11-12 on agentbox, from release **Dash Core v24.0.0**, Linux x86_64.
The plan pins the corresponding Core source to `a94ecf49c2` (v24.0.0); no custom Core build was used.

- Binary: `/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd`.
- `dashd --version`: `Dash Core version v24.0.0`.
- Binary SHA256: `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`.
- Obtained with `contrib/setup-dashd.py`, `DASHVERSION=24.0.0`, cache under the task scratch directory. The downloader checked the release archive against that release's `SHA256SUMS.asc`.
- Network image: `dashpay/regtest-blockchain` release **v0.0.4**, `regtest-mn.tar.gz`, the image used by `dash-spv::test_utils::masternode_network::MasternodeTestContext`.
- Original `network.json` file SHA256: `1df3ee4100b42803b887ef6c96fd34df29d7982a376aea5798a31678b5b71bfb`. Its internal metadata version is 0.0.1; initial height is 406, with four registered regular MNs and eight pre-generated DKG cycles.
- Python: 3.12.15; BLS: `py-ecc==8.0.0`, `G2Basic` (the BasicSchemeMPL ciphersuite). Each registered operator public key is checked against its real image secret before signing; returned DSQ/DSTX signatures are verified independently during recording and self-check.

## Regtest setup and commands

The Python adapter follows the named Rust network setup using its original datadirs and metadata: copy the controller wallet and each MN to private 0700 datadirs, choose high random loopback ports, start each MN with its actual operator key, connect it to the controller, submit `protx update_service` for each new service, and mine those updates. Mocktime begins at 1700000000; bumps plus the existing regtest `mnsync next` path reach blockchain sync and READY. Setup records include live deployment RPC output: **V24 is inactive**. The image enables DIP3 at 2 and v20/mn_rr at 100.

All runs use `-debug=coinjoin -debug=net`. The complete, exact argv of every participating node is the `fact` named `commands` in each fixture. The target node's argv is also reproduced below. Test-only private key values are represented as `<network.json>`: use that node's `bls_private_key`, or the top-level `spork_private_key`, from the downloaded metadata. Actual runtime paths and ports are retained. No private keys are committed.

Each scenario starts a fresh copy; controller funding is mined before client use. Coins and collateral admission answers are recorded as facts. Collateral inputs have 0.01 DASH, outputs 0.0099 DASH; denominated inputs/outputs have 0.00100001 DASH (denomination bit 16). The controller signs collateral and `ALL|ANYONECANPAY` final inputs. Regtest participant limits are min 2 / max 20. The explicit DSQ threshold case registers a fifth MN; it needs no fifth running daemon.

The lifecycle/reorg and removed-MN scenarios disable `SPORK_19_CHAINLOCKS_ENABLED` to isolate depth expiry and historical lookups. The reorg uses Core's `generateblock` with an empty transaction list for the alternate block and then explicitly includes the signed mix at the next height, before reconsidering the original branch. This avoids a tie selecting the original confirmation height.

The review refresh re-recorded every fixture with native outgoing connections, peer/anchor caches removed, and discovery/DNS/fixed seeds disabled. Every owned dashd was stopped and waited for; all run datadirs were deleted. Cleanup attempts every process before deleting directories and reports collected errors; partial construction is protected too.

Reproduction and verification (run from the worktree root):

```sh
uv venv /work/scratch/cj1/impl-transcripts/venv
uv pip install --python /work/scratch/cj1/impl-transcripts/venv/bin/python -r contrib/node/cj1-transcripts/requirements.txt
/work/scratch/cj1/impl-transcripts/venv/bin/python contrib/node/cj1-transcripts/capture.py setup
```

Long recordings used this wrapper, with each exact unit/scenario command below:

```sh
systemd-run --user --unit=UNIT --same-dir --collect bash -c 'P2A_QUIET=0.3 /work/scratch/cj1/impl-transcripts/venv/bin/python -u contrib/node/cj1-transcripts/capture.py capture SCENARIOS > /work/scratch/cj1/impl-transcripts/NAME.log 2>&1; echo $? > /work/scratch/cj1/impl-transcripts/NAME.rc'
```

- `cj1r-verify-record`: `server dsq-intake dsq-modes dstx lifecycle recent dsq-server empty timeout mn-list removed-mn timeout-charge` (all passed).
- `cj1r-cl-connections`: `chainlock dsq-blocksonly` (ChainLock failed; independent blocks-only capture passed).
- `cj1r-threshold-funded`: `dsq-threshold` (passed).
- Review refresh: `cj1r-f2-server` recorded `server` as an observer proof (passed); `cj1r-review-refresh` then ran default `capture`, re-recording **all 14 published fixtures** (passed, exit 0). This replaces the earlier captures above. All five affected server scenarios use observer peer 90 to getdata every announced MSG_DSQ while cached, retaining Core's real signed body. No queue alias is assigned without an observed body.
- Offline review validation: `cj1r-review-check` runs `selfcheck` followed by `selftest`; `python3 -B ... structural-selfcheck` is the dependency-free structural check (BLS verification requires the documented venv).

```sh
PYTHONPYCACHEPREFIX=/work/scratch/cj1/impl-transcripts/pycache python3 -m py_compile contrib/node/cj1-transcripts/capture.py
/work/scratch/cj1/impl-transcripts/venv/bin/python contrib/node/cj1-transcripts/capture.py selfcheck
/work/scratch/cj1/impl-transcripts/venv/bin/python contrib/node/cj1-transcripts/capture.py selftest
git diff --check
```

## Fixtures

| Scenario set | Fixture | What Core shows |
| --- | --- | --- |
| 1 | [server-session.txt](server-session.txt) | Two clients complete dsa → ready DSQ → dsi → dsf → dss → dsc(MSG_SUCCESS), with MSG_DSTX inventory. All five BL-180 pre-decode guards, duplicate collateral (MNO-17), and dsa during SIGNING (MNO-20: ERR_MODE). |
| 1 | [server-empty-entry.txt](server-empty-entry.txt) | MNO-19: empty entry returns ERR_SIZE_MISMATCH and consumes exactly one collateral. |
| 1 | [server-mn-list.txt](server-mn-list.txt) | Revoke the serving registered MN, mine the revocation, and receive ERR_MN_LIST on dsa. |
| 1 | [server-recent.txt](server-recent.txt) | Signed DSQ for the serving MN causes ERR_RECENT on a new dsa. |
| 1 | [server-timeout-no-charge.txt](server-timeout-no-charge.txt) | Both participants omit entries; session resets on timeout with zero charged collaterals. |
| 1 | [server-timeout-charge.txt](server-timeout-charge.txt) | One participant submits an entry, one omits it. Two zero-charge attempts precede one charged attempt; participant 2 is the sole offender and the only charged participant. |
| 2 | [dstx-intake.txt](dstx-intake.txt) | Invalid structure (+10), unknown MN (+1), bad signature (+10), six accepted signed DSTXes and a dropped seventh. Known txid with invalid wrapper is already-have without score. MSG_DSTX relay; peer 8 gets a DSTX wrapper and peer 9 gets the plain transaction on MSG_TX getdata. Both answer commands and transaction bytes are compared. |
| 2 | [dstx-removed-mn.txt](dstx-removed-mn.txt) | Spend the registered collateral. Historical lookup accepts signed DSTXes at last-present depths 1 and 23, and drops at depth 24 with +1. |
| 3 | [dstx-lifecycle-reorg.txt](dstx-lifecycle-reorg.txt) | Acceptance; confirmation at 409; disconnect and mempool re-add (-1); alternate branch re-mines at 410. At depth 24 the cache remains and a bad-wrapper resend hits already-have; at depth 25 the cache expires and that resend scores +1. Includes map-size and confirmed-height log sequences. |
| 4 | [dsq-modes.txt](dsq-modes.txt) | 70233 true gets full DSQ, 70234/70242 true get MSG_DSQ inv, false peers get neither. Non-ready and ready queues, getdata serving and notfound. |
| 4 | [dsq-wallet-intake.txt](dsq-wallet-intake.txt) | Non-MN wallet intake: null proTx +100 and disconnect, unknown MN +10, bad signature +10; expired queue and duplicate silently drop; valid queue is served by getdata with no relay. |
| 4 | [dsq-server-intake.txt](dsq-server-intake.txt) | MN DSQ intake: null +100, unknown +10, bad signature +10, invalid denomination +10; expired/duplicate drops; valid signed queue relays and serves by getdata. |
| 4 | [dsq-wallet-threshold.txt](dsq-wallet-threshold.txt) | Register a fifth MN so enabled_count/5 = 1. A ready queue is silently threshold-dropped/notfound until another MN increments the global DSQ counter; the same ready queue then serves successfully, without non-MN relay. |
| 4 | [dsq-blocksonly.txt](dsq-blocksonly.txt) | Blocks-only non-MN has no DSQ queue: getdata is notfound, then an incoming DSQ closes the peer without a misbehavior score. |

## Comparison and masking

Each fixture carries its own `mask` record. Raw peer inputs, returned bytes and original Core log lines are retained as evidence. Replay compares the `expect` projections and relevant `observe` values, with chain/MN/mempool fakes primed from `fact` and `rpc` records.

- Zero session IDs stay zero; nonzero IDs map consistently by first occurrence, preserving same-session relationships.
- Queue time and BLS signature are omitted from comparisons, with signatures verified from original bytes. Queue inv hashes map to aliases derived only from observed, verified bodies, since their hash includes time/signature. `queue_bodies` retains each raw signed body, its hash, observer, denomination, ProTx and readiness; expectations preserve same-queue relationships. Every announced server queue is fetched through observer peer 90. Explicit expired-time inputs remain unchanged.
- DSTX comparisons keep the signed transaction and ProTx identity, omitting wrapper BLS signature and sigTime. The unsigned `dsf` transaction bytes are compared exactly.
- Background handshake nonces/times, pings, address/header/spork/quorum traffic, log wall time and incidental Core peer IDs are evidence only. Score deltas, DSTX verdicts, map sizes and confirmation heights are re-derived from raw logs by self-check.
- Collateral hashes map to individual participant aliases in the separate `collaterals` fact. Counts and charged participants are re-derived from raw collateral transactions and mempool RPC snapshots; eligibility is checked against raw dsa/dsi inputs. The charged timeout has two zero-charge attempts then one charge, necessarily participant 2 (the sole offender). Empty-entry rejection consumes participant 1's collateral and nothing else. Random selection may be masked only within a set of multiple eligible offenders; no current fixture charges from such a set. Every attempt and its zero/one decision is retained.
- Completion IDs are re-derived from raw dsc replies. Scenario validation requires the essential inputs and outcomes, including dsa/dsi/dss inputs and blocks-only closure. Each fixture contains a SHA256/count manifest covering all raw `<`, `>`, `rpc` and `log` lines. It detects accidental evidence deletion or truncation in addition to semantic checks; it is not an authenticity signature.

The release's DSTX scores are **10 invalid / 1 unknown MN / 10 bad signature**, and its `IsValidForMixingTxes` check permits the counter at 5, admitting six transactions before dropping the seventh. These are measured release behaviors, even where the brief's shorthand says “>5”. Its 24-list historical scan includes depths 0..23, so last-present depth 23 succeeds and 24 fails.

## Pending and blocked

- **Deferred, post-V24:** rebalance/promotion/demotion sessions with rebalance-capable and legacy peers; PREMATURE at the one-block activation boundary; unbalanced DSTX relay as MSG_TX to older peers. These need CP's V24-active regtest MN image. They were not recorded on a pre-V24 chain.
- **Blocked, pre-V24 ChainLock expiry leg of set 3:** the supplied v0.0.4 image with release v24.0.0 did not produce a ChainLock. Attempts waited 60 ticks, then 120 ticks after 48 fresh DKG warmup blocks with SPORK_21 disabled, then 120 ticks with native outgoing connections enabled. `getbestchainlock` remained “Unable to find any ChainLock”; quorum diagnostics showed unavailable member connections, and no new usable signing quorum. This is an observed setup limitation, not a claim that Core cannot produce ChainLocks. The optional `chainlock` recorder remains available, with failure diagnostics in task scratch. No fixture was fabricated or published for this leg.
- **Not blocked on Python BLS:** real operator signing and verification worked for every recorded signed DSQ/DSTX case.

The later Rust transcript reader and replay tests are intentionally absent from this change.

## Exact target-node command lines

### dsq-blocksonly.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/controller -regtest -server -listen=1 -bind=127.0.0.1:58304 -port=58304 -rpcport=58076 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:58304 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 '-sporkkey=<network.json>' -blocksonly=1 -wallet=wallet
```

### dsq-modes.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/mn1 -regtest -server -listen=1 -bind=127.0.0.1:50408 -port=50408 -rpcport=45044 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:50408 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -disablewallet=1 -txindex=1 '-masternodeblsprivkey=<network.json>'
```

### dsq-server-intake.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/mn1 -regtest -server -listen=1 -bind=127.0.0.1:57254 -port=57254 -rpcport=52827 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:57254 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -disablewallet=1 -txindex=1 '-masternodeblsprivkey=<network.json>'
```

### dsq-wallet-intake.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/controller -regtest -server -listen=1 -bind=127.0.0.1:60484 -port=60484 -rpcport=51285 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:60484 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 '-sporkkey=<network.json>' -wallet=wallet
```

### dsq-wallet-threshold.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/controller -regtest -server -listen=1 -bind=127.0.0.1:58225 -port=58225 -rpcport=54443 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:58225 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 '-sporkkey=<network.json>' -wallet=wallet
```

### dstx-intake.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/controller -regtest -server -listen=1 -bind=127.0.0.1:54310 -port=54310 -rpcport=49206 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:54310 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 '-sporkkey=<network.json>' -wallet=wallet
```

### dstx-lifecycle-reorg.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/controller -regtest -server -listen=1 -bind=127.0.0.1:60162 -port=60162 -rpcport=45067 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:60162 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 '-sporkkey=<network.json>' -wallet=wallet
```

### dstx-removed-mn.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/controller -regtest -server -listen=1 -bind=127.0.0.1:55304 -port=55304 -rpcport=47248 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:55304 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 '-sporkkey=<network.json>' -wallet=wallet
```

### server-empty-entry.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/mn1 -regtest -server -listen=1 -bind=127.0.0.1:48612 -port=48612 -rpcport=42406 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:48612 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -disablewallet=1 -txindex=1 '-masternodeblsprivkey=<network.json>'
```

### server-mn-list.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/mn1 -regtest -server -listen=1 -bind=127.0.0.1:53451 -port=53451 -rpcport=45459 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:53451 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -disablewallet=1 -txindex=1 '-masternodeblsprivkey=<network.json>'
```

### server-recent.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/mn1 -regtest -server -listen=1 -bind=127.0.0.1:53173 -port=53173 -rpcport=45062 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:53173 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -disablewallet=1 -txindex=1 '-masternodeblsprivkey=<network.json>'
```

### server-session.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/mn1 -regtest -server -listen=1 -bind=127.0.0.1:46502 -port=46502 -rpcport=60226 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:46502 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -disablewallet=1 -txindex=1 '-masternodeblsprivkey=<network.json>'
```

### server-timeout-charge.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/mn1 -regtest -server -listen=1 -bind=127.0.0.1:59454 -port=59454 -rpcport=59492 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:59454 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -disablewallet=1 -txindex=1 '-masternodeblsprivkey=<network.json>'
```

### server-timeout-no-charge.txt

```sh
/work/scratch/cj1/impl-transcripts/cache/dashcore-24.0.0/bin/dashd -datadir=/work/scratch/cj1/impl-transcripts/run-316062/mn1 -regtest -server -listen=1 -bind=127.0.0.1:42310 -port=42310 -rpcport=45200 -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 -rpcuser=p2a -rpcpassword=p2a -listenonion=0 -discover=0 -dnsseed=0 -fixedseeds=0 -upnp=0 -natpmp=0 -v2transport=0 -debug=coinjoin -debug=net -logips=0 -externalip=127.0.0.1:42310 -mocktime=1700000000 -printtoconsole=0 -par=1 -fallbackfee=0.00001 -dip3params=2:2 -testactivationheight=v20@100 -testactivationheight=mn_rr@100 -disablewallet=1 -txindex=1 '-masternodeblsprivkey=<network.json>'
```
