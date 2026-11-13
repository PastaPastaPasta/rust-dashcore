# GV1b release-node recordings

Release dashd v24.0.0: `/work/home/agent/.rust-dashcore-test/dashcore-24.0.0/bin/dashd`. Binary SHA256:
`2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`. Acquired with
`DASHVERSION=24.0.0 python3 -I contrib/setup-dashd.py`.

The recorder imports P2A's node, transport and framing helpers. Its source
builder extends `dash-oracle-sys/scripts/gov_capture.py`, the builder behind
`gen-gov-regtest.py`: controller, four regular masternodes and two evonodes.
The functional framework is Core tag v24.0.0, commit
`a94ecf49c2dc2ae77346acda3b80c3707ab2430e`. The observer replays full blocks
from the frozen bootstrap into a fresh wallet-disabled release node.

## Reproduction

Run from this repository. These commands prepare the exact source tree and
Python package versions used. They require git, uv and a C compiler for the
small `dash-hash` extension; no Core build is involved.

```sh
DASHVERSION=24.0.0 python3 -I contrib/setup-dashd.py
install -d -m 700 /work/scratch/gv1b-captures/core
git clone --depth 1 --branch v24.0.0 https://github.com/dashpay/dash.git /work/scratch/gv1b-captures/dash-v24
git -C /work/scratch/gv1b-captures/dash-v24 archive v24.0.0 test | tar -x -C /work/scratch/gv1b-captures/core
uv venv --python 3.12.15 /work/scratch/gv1b-captures/venv
uv pip install --python /work/scratch/gv1b-captures/venv/bin/python3 annotated-types==0.8.0 cytoolz==1.2.0 dash-hash==1.5.1 eth-hash==0.8.0 eth-typing==6.0.0 eth-utils==6.0.0 py-ecc==8.0.0 pydantic==2.14.3 pydantic-core==2.50.4 toolz==1.2.0 typing-extensions==4.16.0 typing-inspection==0.4.5
/work/scratch/gv1b-captures/venv/bin/python3 -I contrib/node/gv1b-transcripts/capture.py --build-source /work/scratch/gv1b-captures/core
/work/scratch/gv1b-captures/venv/bin/python3 -I contrib/node/gv1b-transcripts/capture.py --check
/work/scratch/gv1b-captures/venv/bin/python3 -I contrib/node/gv1b-transcripts/capture.py --selftest
```

Without `--build-source`, recordings reuse `bootstrap.json.gz`. A new source
build changes wallet keys, transactions and hashes and must regenerate every
row. `--check` uses the frozen bootstrap, never a randomized source rebuild.
All datadirs are mode 0700 below `/work/scratch/gv1b-captures/`. Listeners use
probed ports below 32768. Observers stop in `finally` and delete their own
datadirs; the framework stops its source nodes and cleans up after success.

## Record grammar

UTF-8, one record per line. Blank lines and lines starting with `#` are
comments. `N` is a positive logical peer ID; `S`, `T`, `COUNT` and `SERVICES`
are nonnegative decimal integers (COUNT is positive). `H` is a 32-byte hash
in displayed hexadecimal order. `HEX` is even-length lowercase hexadecimal;
an empty message payload omits HEX. Writers omit trailing whitespace.
JSON occupies the rest of its line and follows the schemas below.

```text
mocktime T
ibd 0|1
time T
mockscheduler S
timeoffset SIGNED_SECONDS
accept N
connect N outbound-full-relay|manual SERVICES
> N CMD [HEX]
> N pong echo
< N CMD [HEX]
< N closed
rand bloom HEX
mnsync reset|next
invalidateblock H
reconsiderblock H
generate COUNT raw(51)
submitblock HEX
restart
check gobject JSON
check score JSON
check rpc JSON
log JSON_STRING
```

`mocktime` describes process startup; `time` only sets the wall mock clock.
`mockscheduler` independently advances Core's steady scheduler in seconds.
`timeoffset` supplies the adjusted-time offset observed in `getnetworkinfo`;
the preceding version messages show how the release node obtained it.
`accept` and `connect` create logical peers; `restart` drops connections and
restarts the same datadir with the original arguments and startup mock time.
Peer IDs may be reused after restart. `mnsync`, invalidation, reconsideration,
generation and submission are inputs. Their results appear in `check rpc`.
`submitblock` carries the entire serialized block, including transactions.

P2A checks frame magic, length, command padding and checksums. Its internal
synchronization pings/pongs are omitted; ordinary node pings remain. A final
incomplete frame is an error. Per-peer outgoing message order is retained.
`rand bloom` is exactly four bytes, copied from the tweak field of the next
outgoing nonempty `govsync` bloom filter, in wire order. It is never generated
by the recorder. Empty-filter fetches consume no random input. The Python
parser verifies each random input against the actual outgoing payload.

`check gobject` is an object with exactly these keys:

- `label`: checkpoint name;
- `objects`: unmodified `gobject list all` result, keyed by object hash;
- `votes`: object hash to unmodified `gobject getcurrentvotes` result;
- `budget`: `height` (next superblock) and `amount` (`getsuperblockbudget`);
- `sync`: unmodified `mnsync status` result;
- `chain`: unmodified `getblockchaininfo` result.

Every object checkpoint first drains the validation notification queue and
has a matching `check score`. Its keys are `label` and `peers`. Each peer
entry has `core_id`, `score` and `connected`. Live scores come directly from
`getpeerinfo.banscore`, enabled with `-deprecatedrpc=banscore`. A disconnected
peer retains its last RPC score, updated by any later original `Misbehaving`
log transition. It is never assigned a guessed terminal score.

`check rpc` has `method`, `params`, and exactly one of `result` or `error`.
It is an observation, never an instruction to invoke the method. Its method
allowlist is in `parse_records`; the full result/error is kept. In particular,
mining records precede the result and subsequent submit/lookup observations
for the same block bytes. Resubmitting an already mined block normally
returns `duplicate`. `log` retains an original release log line as a JSON
string. Legacy `rpc` and `checkpoint` records are rejected.

`--selftest` parses every `R*.txt`, validates record arities, hex sizes, JSON
schemas, peer references, checkpoint score coverage and bloom input bindings,
and runs malformed-record controls. This slice changes Python only; the Rust
parser and replay hooks belong to S3b.

## Fixed retention and comparison rules

The bootstrap retains only fields selected in the recorder and uses gzip
mtime zero. Shared bootstrap blocks are not repeated in each transcript;
R6–R9 and their subcases retain submit/lookup answers in companion compressed
JSON files. ChainLock subcases use the separate chain in `chainlock.blocks`.
Later submitted blocks carry full hex and both RPC answers in the transcript.
R9's governance cache is copied unchanged after a clean stop, before restart.
Logs use only the fixed substring filter in `Transcript.logs`, never hand
selection. They are appended in original order at shutdown; R9 also flushes
them immediately before restart and after RPC readiness to expose load order.

Block probes replace the last coinbase payee or add one whole superblock
budget, then recompute merkle root and proof of work with Core's Python block
codec. Trigger probes use real source MN keys, accepted and serialized back
by a separate release node. Signed funding votes use BLS operator keys for
triggers. The competing payees have equal amounts and different scripts, so
their blocks distinguish the selected trigger. All verdicts and check values
come from observed release dashd; no expected verdict is written as a fixture.

The ChainLock observers both receive the unchanged signed enable spork from
the first matching outgoing `spork` in the committed SP0 recording
`dashd-masternode/tests/fixtures/dashd-v24/sporks-receive.txt`. That recording
obtained the packet from a release-node signer using Core's regtest test key.
The locked observer announces/requests the real `clsig` before delivery;
its verified lock is checked before the block body arrives.

R10 extracts each unchanged signed mutation transaction from its source block,
sends it as `tx`, checks mempool acceptance, then mines it on the observer.
This tests key changes at heights 187–189 without importing the unrelated DKG
chain and its later governance objects. It mines through the next superblock
at 200 and retains its full bytes, mining result, resubmission and lookup.

`--check` compares R1 exactly after the listed exclusions: runtime listener
ports, outgoing version nonce and MNAUTH challenge, ordinary ping nonce, log
wall timestamp and blockchain `verificationprogress` (a wall-clock estimate).
No governance hash, payload, score, count, height, mock time, log outcome or
scheduler delta is masked. Stored files are not rewritten by normalization.

R2 retains and marks non-requester inventories with
`# declared-difference: R2-govsync-cross-peer requester=2 recipient=N`.
This slice adds no ALLOWED-DIFFS row. R9's immediate startup checkpoint precedes
mock-time restoration, peer setup, forced sync advancement and a new block.
Its logs show trigger reconstruction/far-future testing before the first
cached-height update. Core initializes `nCachedBlockHeight` to zero; no RPC
exposes that initial value, so no invented numeric observation is emitted.

## Coverage

- R1: unsolicited drop, two announcers, global request forgetting observed by
  the second delivery being logged as unrequested, proposals/trigger, invalid
  collateral +20 and batched relay.
- R2: full sync/SSC10, fetch, bloom vote sync/SSC11, repeat penalties, repeated
  absent-parent empty-filter requests with unchanged score, oversized bloom,
  unsynced ignore and marked cross-peer announcements.
- R3: immediate/periodic orphan requests, arrival, expiry, malformed +100,
  bad signature +20 fully synced and zero in GOVERNANCE sync. `R3-offset`
  observes +600 adjusted offset from four distinct outbound loopback peers,
  then a parent arriving before the wall deadline but after the adjusted
  deadline, without scheduler cleanup in between.
- R4: rejection at zero confirmations, postponed at 1–5, automatic acceptance
  at six without resending.
- R5: per-object vote requests, extracted bloom tweaks, SSC replies and natural
  FINISHED timing. Request order and blooms remain as sent by dashd.
- R6: correct/wrong-payee/over-budget verdicts; `R6-rank` and `R6-tie` use two
  triggers and different-paying blocks. The lower internal hash starts with
  weighted YES 4 and NO 3 (absolute 1); the higher hash has YES 2 (absolute 2).
  Adding one YES to the lower hash makes an absolute-2 tie. Internal order is
  Core's little-endian byte comparison, not displayed-hash order. Both wrong
  payees and excess value really return `bad-cb-amount` in v24.0.0.
- R7: unsynced max bounds and independently judged sibling blocks. A real regtest quorum/ChainLock and control distinguish fully synced governance bypass.
- R8: execution, pure disconnect, alternate branch/reactivation, the shared
  regtest expiry boundary and far-future trigger after deep reorg. A 20-block
  cycle cannot distinguish mainnet's two lifetimes; that is S1b coverage.
- R9: clean-stop cache, startup load order and immediate trigger/template state,
  all current votes, and the same next block accepted. Startup marks the trigger
  too far in the future; its later deletion is retained rather than corrected.
- R10: voting-key update, operator-key update while registered, removal,
  invalidation snapshots and the next superblock's mining/submit/lookup verdicts.
  Resubmission of each mined block is `duplicate`.

No internal oracle stream was recorded. The existing optional build is
documented in `contrib/node/statedigest/build.sh` (`--out` directory), with
`record-ft.py` and `dashd-wrapper` documenting recording. No such binary was
built or located. Direct callback-idempotence inputs remain outside these
release-node transcripts.

## Artifacts

| File | Bytes | SHA256 |
|---|---:|---|
| `R1.txt` | 33327 | `e30848db7fd6635e163b5ce1b1ec0a582d2ca3793ecc15c6f201c8841bc72503` |
| `R10.txt` | 203076 | `9e4fc72196856d331556accd4e76bf14986a4055ca3bb046ea749d0e158330e1` |
| `R2.txt` | 212236 | `4f3cbd5f5394253fdded40cafcef6c369113234d23c2852116bba528e2c5f8cb` |
| `R3-offset.txt` | 17336 | `66de41d8c8ba0fe9574d150bd4780bc3987d8365a19d22a4fc088b8d69e7809d` |
| `R3.txt` | 35866 | `3d104c97e7ee1577635f67f776934d3c1c5a046463e81ac657d21cf6053b6118` |
| `R4.txt` | 171952 | `acf83beb124d16d944bfb42b78286e08133970874e8ca450fa7eed27ef8dad1a` |
| `R5.txt` | 110847 | `b172d34605ae4a1c6d83fb2aedbc3bd85bdd0506881dd658105bed60455c8c59` |
| `R6-rank.bootstrap.json.gz` | 34936 | `49ab9ff7c4703518936de900f61346676425c5127f2cb5ea5afafa5823329a3a` |
| `R6-rank.txt` | 36586 | `0ff478fcbf1174011d93f695e6241f55cbd37234b2785a4ab66b7f0743bbdd24` |
| `R6-tie.bootstrap.json.gz` | 34935 | `e385d97d75f1f2064df412109c1fd0ceadd22fa842b75c97d70e33cb4468983b` |
| `R6-tie.txt` | 37451 | `a205e4d43095120d5e1d37f342eb4ab02c4fa2851efdfe920d418ea6de4b7f73` |
| `R6.bootstrap.json.gz` | 34931 | `1a93f584f78165320cb7e4c8c6d8c1f9975fdd688bf6786669f4fbc680839f91` |
| `R6.txt` | 96896 | `bb956caa7bc2055bd270d0bd87779597e09cb384396bcd221aa4e954a48e70a5` |
| `R7-chainlocked.bootstrap.json.gz` | 58854 | `664fdb41b2236eee2da2d618850e3a00f462a7e45e0c1688026e1fd98ef0659e` |
| `R7-chainlocked.txt` | 24541 | `625c16e1187260e9de4b96782ac420f6b604dbcdaa05a628f2883bbb0c64a9a9` |
| `R7-control.bootstrap.json.gz` | 58850 | `d2574673a8769fa8cc27b63b11e4490803f6396ff962febf0897778eb5b831f8` |
| `R7-control.txt` | 20249 | `0d5d7bfa085c19489a79f548dc7488222f986ef77601426926ed1fb8c67f965d` |
| `R7.bootstrap.json.gz` | 34931 | `09a789be46b933fcd2957c00d030af33ccb678122136d9b7e8dfcecca6d69e57` |
| `R7.txt` | 108716 | `b6637c00446d1180608f57564cccccdd0633ca9c81ee55662d5c51f6536e2805` |
| `R8-future.bootstrap.json.gz` | 34938 | `226d5036efaebedcf2dc7a7a0493026bf4593ce0fd577d95a4a9cd709eac59eb` |
| `R8-future.txt` | 88924 | `6dd49f08af1dd423500e37afb330877752d516a770f2718b3ce79ef558353052` |
| `R8.bootstrap.json.gz` | 34931 | `4a95601fead9d01e930e3279ac638fc0a7f08783fafb7e4e7ffeecca95e72bf6` |
| `R8.txt` | 245408 | `9f29094f02724835f08e5b7e9d2e634bdd98c65f27725da870d0de887882de87` |
| `R9.bootstrap.json.gz` | 34931 | `88e7d00c25fef53ce4d1c51ed04bfa66825ddbcd708279d1bdee38493f572fcc` |
| `R9.governance.dat` | 8403 | `0522c819345d67dcee11e870e0bd0c5541129409ad4f8554019fecbdc0e0257d` |
| `R9.txt` | 98493 | `5f6778ba40eb31231e4eb09bdcf8eddd32468fe46810c43f2cf89cf8987f4fba` |
| `bootstrap.json.gz` | 91524 | `0930012746f6f228d85eee67c765a6629e0896981e6e06e159d13fb63da414a9` |
