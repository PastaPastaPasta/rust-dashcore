# Provenance of the dashd v24 transcript fixtures

Every `*.txt` here was written by `contrib/node/p2a-transcripts/capture.py` (its docstring defines
the format), run against a **release** regtest dashd v24.0.0-rc.3
(`dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz`, dashd sha256 `e204e316…a7ab5bb4`, as fetched by
`DASHVERSION=24.0.0-rc.3 contrib/setup-dashd.py`), on 2026-10-10:

```
P2A_DASHD=…/dashcore-24.0.0-rc.3/bin/dashd python3 contrib/node/p2a-transcripts/capture.py OUT_DIR
```

Nothing in them was edited by hand. Each file's header records the dashd version and arguments;
every node starts from a fresh datadir with `-mocktime=1700000000` (`sim::SIM_START`).

| Fixture | What the scripted peers do |
|---|---|
| `handshake-{inbound,outbound-full-relay,block-relay-only,addr-fetch,feeler,manual}` | one peer of that connection type (`addconnection`, or `addnode onetry` for manual) completes the handshake on a genesis-only node in IBD; the window runs to the first `getheaders`/`getheaders2` plus 10 s of silence (P2a §6.1) |
| `headers-2000` | an outbound peer without `NODE_HEADERS_COMPRESSED` answers the first `getheaders` with 2000 headers |
| `headers-2000-bit11` | the same with a bit-11 peer, answering `getheaders2` with 2000 plain `headers` (PQ-24) |
| `headers2-8000` | a bit-11 peer answers `getheaders2` with a full `headers2` batch of 8000 |
| `announce` | four inbound peers on a node 3 blocks past genesis (out of IBD); three announce the tip back and ask for `headers2`, `headers` and nothing; the fourth announces nothing; the node mines two blocks |
| `evict-slow-chain` | an outbound peer on a node 2 blocks past genesis answers with no headers and each ping; the mock time moves in 60 s steps until `ConsiderEviction` disconnects it |

The headers in the three `headers*` fixtures come from a second regtest dashd v24.0.0-rc.3 run by
the same tool (`miner_chain`, mock time `SIM_START - 3000`), mined with
`generatetodescriptor … raw(51)`. `headers2-8000` encodes them as dashd replies to `getheaders2`
(the first header kept out of the version list, `net_processing.cpp:4851-4855`), not as it
announces.

`tests/dashd_transcripts.rs` replays them with `dashd_p2p::sim::transcript`, whose module docs
list the masks and the messages it drops (later tasks' sends).

## The inbound fixtures (P2b)

Every `inbound-*.txt` was written by `contrib/node/p2b-transcripts/capture.py` (P2b D9), which
reuses the P2a tool's node, peers, synchronisation and format, against the same release dashd
v24.0.0-rc.3 (sha256 `e204e316…a7ab5bb4`), on 2026-10-16:

```
P2A_DASHD=…/dashcore-24.0.0-rc.3/bin/dashd python3 contrib/node/p2b-transcripts/capture.py OUT_DIR
```

Nothing in them was edited by hand. Every peer connects to dashd (the `accept` record) from
127.0.0.1; extra dashd arguments are in each file's `# args:` line. `inbound-evict` and
`inbound-evict-21` were captured with the tool's all-peer quiet wait (P2b D13), the others with
P2a's per-peer one.

| Fixture | What the scripted peers do |
|---|---|
| `inbound-gates` | `-v2transport=0` (P2b D10): one peer sends `ping`, `getheaders`, `verack`, `sendheaders`, `getaddr`, `mempool` and an unknown command before `version`; then `version`; then `ping`, `getheaders`, `getaddr`, `inv`, `sendheaders2`, `sendcmpct`, `sendaddrv2` and a second `version` before `verack`; then `verack` and a third `version`; then `getheaders`/`getheaders2` on a genesis-only node (genesis locator, null locator with the genesis stop, an unknown stop) and a 102-entry locator |
| `inbound-versions` | nine peers: protocol 70222, 70235 with bit 11, 70234 without tx relay, `NODE_NONE`, the obsolete 70220, the masternode flag (the node is none), a `version` cut after `addr_recv`, a 257-byte user agent (then a good `version`), and `NODE_BLOOM` with bit 11 without relay |
| `inbound-serve` | the node mined 2100 blocks (out of IBD); a bit-11 peer at 70242 and a plain peer at 70234 ask `getheaders`/`getheaders2` across the 2000 limit, with `hashStop`, a null locator, unknown and stale locator entries, an unknown stop; the node mines a block after a request that ended one short of the tip; a 102-entry locator |
| `inbound-serve-minwork` | `-minimumchainwork=ff…ff` on a genesis-only node: the same two peers ask `getheaders`, `getheaders2` and a null-locator `getheaders` |
| `inbound-serve-download` | the same with `-whitelist=download@127.0.0.1` |
| `inbound-evict-21` | `-maxconnections=32` (21 inbound slots): 25 peers connect in turn, varying tx relay, services (`NODE_NETWORK` or `NODE_BLOOM`), whether they answer the node's ping, and the connect time (the mock time moves 1 s before every third); every candidate is protected, so dashd drops each peer past the 21st (P2b D12) |
| `inbound-evict` | `-maxconnections=40` (29 inbound slots): 33 peers as above; each past the 29th evicts one (peers 28, 27, 31 and 32 go) |

`tests/dashd_inbound_transcripts.rs` replays them, each with the fixture's dashd arguments.

## The inbound-policy fixtures (P4a)

Every fixture below was written by `contrib/node/p4a-transcripts/capture.py` (P4a D5, D10-D13),
which reuses the P2a tool's node, peers and format and P2b's all-peer quiet wait, against the
same release dashd v24.0.0-rc.3 (sha256 `e204e316…a7ab5bb4`), on 2026-10-17:

```
P2A_DASHD=…/dashcore-24.0.0-rc.3/bin/dashd python3 contrib/node/p4a-transcripts/capture.py OUT_DIR [SCENARIO...]
```

The tool's docstring defines the records it adds (`accept N from ADDR [perm FLAGS]`, `setban`,
`clearbanned`, `masternode`, `banlist`, `capture`). It runs itself, dashd and every peer inside a
private user and network namespace (`unshare -rn`) with public IPv4 /32s on `lo`, so dashd sees
routable, non-local peers in distinct /16 netgroups (D11); a peer with `from ADDR` dials from
ADDR, local port 40000+N. dashd's own ports are in each file's `# args:` line (`-port`/`-bind`
is the plain listener; `-whitebind` listeners follow). Nothing in the fixtures was edited by
hand. Every node starts from a fresh datadir with `-mocktime=1700000000`.

| Fixture | dashd arguments beyond P2a's | What the scripted peers do, and what dashd did |
|---|---|---|
| `inbound-permissions` | `-peerbloomfilters=0 -whitebind=noban@127.0.0.1:33960 -whitebind=mempool@127.0.0.1:33961 -whitebind=noban,mempool@127.0.0.1:33962` | genesis-only node (IBD); seven peers from 11.1.0.x each complete the handshake, then send `mempool` and/or an unsolicited `mnlistdiff` (empty payload). Plain port: peer 1 `mempool` → disconnected unscored ("mempool request with bloom filters disabled"); peer 2 `mnlistdiff` → +100, discouraged and disconnected. `noban`: peer 3 `mnlistdiff` ("Warning: not punishing noban peer") then `mempool` (ignored), stays. `mempool`: peer 4 `mempool` (served; the mempool is empty, nothing sent) then `mnlistdiff` → discouraged, disconnected. `noban,mempool`: peer 5 both, stays. Peer 6 dials the plain port from the discouraged 11.1.0.2 and is accepted (table far from full), then `mempool` → disconnected. Peer 7 dials the `noban` listener from the discouraged 11.1.0.4, is accepted and not punished |
| `inbound-whitelist` | `-peerbloomfilters=0 -whitelist=11.2.0.0/16 -whitelistrelay=1 -whitelist=mempool@11.4.0.1` | the same misbehaviour on the plain port: peers 1-2 inside 11.2.0.0/16 (implicit `noban`, `mempool`, `relay`) send `mempool` and `mnlistdiff` in both orders and stay; outside it peer 3's `mempool` disconnects it and peer 4's `mnlistdiff` discourages it; peer 5 at 11.4.0.1 (explicit `mempool` only) is served `mempool`, then discouraged for `mnlistdiff` |
| `inbound-bans` | `-maxconnections=15` (4 inbound slots) | `setban 11.1.0.1 add` (default `-bantime`) drops both peers connected from 11.1.0.1; a reconnect from it is dropped (banned); `setban 12.7.0.0/16 add 3600` drops 12.7.0.1 and 12.7.0.2, and 12.7.9.9 is dropped; `setban 11.1.0.1 remove`, reconnect accepted; `setban 13.9.0.1 add 60`, reconnect dropped, `time` +61 s, reconnect accepted; absolute `setban 14.3.0.1 add 1700007261 1` (its save sweeps the expired 13.9.0.1 entry), reconnect dropped; 15.5.0.1 is discouraged by an unsolicited `mnlistdiff`, reconnects at 2 inbound (accepted) and at 3 (dropped: discouraged); at 4 inbound a fifth peer finds every candidate protected and is dropped (full); `clearbanned`, then 12.7.0.1 is accepted. `banlist` lines after each change |
| `inbound-evict-groups` | `-maxconnections=41` (30 inbound slots) | 30 peers fill the table from 11.1.0.0/16 (1 peer), 12.2.0.0/16 (4), 13.3.0.0/16 (7) and 14.4.0.0/16 (18), the mock time moving 1 s before each connect; then 5 more connect and dashd evicts peers 30, 29, 32 (all 14.4/16), 28 (13.3/16) and 26 (14.4/16): the netgroup with the most unprotected peers, a tie broken by the group whose youngest member is youngest (32 over 28, then 28 over 26). Peers in the tool's absorbed set send relay=0 and answer the node's ping at once; the rest relay and never answer. That makes the evictions independent of dashd's random netgroup keys: `contrib/node/p4a-transcripts/evict_model.py <Core src>` runs Core's `node/eviction.cpp` over all 24 key orders and predicts exactly these five. Ties within a netgroup or a ping time still fall to libstdc++'s `std::sort` over the candidates in connect order (`m_nodes`), as in `inbound-evict`: the replay must keep that order |
| `inbound-serve-8000` | — | the node mined 8001 blocks (out of IBD); a bit-11 peer at 70242 sends `getheaders2` from the genesis locator (8000 `headers2`) and from block 8000 (one `headers2`: block 8001); a plain peer at 70234 sends `getheaders` from genesis (2000 `headers`). Peers dial from 127.0.0.1 (plain `accept`) |
| `inbound-capture` | `-capturemessages` | two peers (11.1.0.1:40001, 11.1.0.2:40002) complete the handshake; the mock time moves 5 s; peer 1 pings and answers the node's ping; `getheaders`/`getheaders2` from genesis (empty replies); peer 2's unsolicited `mnlistdiff` discourages and disconnects it. The `capture` lines are dashd's `message_capture/<IP>_<port>/msgs_{recv,sent}.dat`, whole, so they include the tool's sync pings (nonce `0x7EA5000000000000 + N`) and dashd's pongs |
| `inbound-qfcommit` | `-debug=llmq` (logging only) | genesis-only node (IBD); three peers from 11.1.0.1-3 complete the handshake. Peer 1 sends a decodable `qfcommit` (`qfcommit_100` of `dash-oracle-sys/tests/data/bs8-core-overlay.json5`, version 4) ten times: each scores +10 `unrequested quorum commitment` (debug.log: `CQuorumBlockProcessor::ProcessMessage -- unrequested commitment from peer=0`), the tenth reaches 100, and dashd discourages and disconnects it. Peer 2 sends it nine times: 90, still connected. Peer 3 sends `qfcommit_10` (version 1 in the v4 layout) ten times: each read throws (`ProcessMessages(qfcommit, 313 bytes): Exception 'malleable BLS object: iostream error'`) before the solicitation check, so nothing is scored and dashd never disconnects it |
| `inbound-evict-mn` | `-masternodeblsprivkey=<operator-key> -maxconnections=125` (114 inbound slots; the key implies `-disablewallet=1 -peerblockfilters=1 -blockfilterindex=basic`; the key is redacted in the file) | node A mined 510 blocks (past DIP3 enforcement at 500), registered A and a second masternode B (`protx register_fund`), mined 10 more, copied the chain to B and restarted both with their operator keys (B: `-port=33910`, its own key, otherwise P2a's arguments) and waited for `masternode status` READY. 114 scripted peers from 11.1.0.1 fill A's table, the mock time moving 1 s before every tenth peer; odd peers answer A's ping, even ones never do. B then dials A (`addnode … onetry`, from 127.0.0.1): its connect evicts peer 112, and once MNAUTH verifies it (`masternode 115 <B's proRegTx>`) it no longer counts against the limit, so peer 116 is accepted without an eviction; 117 and 118 evict 114 and 116. After 1 s the first-second protection tail: 119 and 120 connect silent (119 evicts 117; 120 evicts 111, because 119, younger than 1 s and with no first message, is protected), 119 completes its handshake and is evicted by 121's connect; after another 1 s 121 (still silent) is evicted by 122's. B is never evicted and stays verified to the end. Every peer's first ping after its verack is recorded (`> N ping`, nonce `0xF1A5000000000000 + N`), because the first post-verack message ends a peer's first-second protection; the tool pings all peers at once here rather than one by one. Evictions from debug.log, matching Core's `node/eviction.cpp` modelled over the same candidates |
| `inbound-ban-expiry` | — | genesis-only node (IBD); `setban 13.9.0.1 add 60` at the start (until 1700000060). A peer from 13.9.0.1 at `time` +59 is dropped (banned) and one at +60 is accepted and completes the handshake (`IsBanned` holds while `now < nBanUntil`); `setban 13.9.0.2 add 3600` at +60 saves a banlist.json still listing 13.9.0.1 (`SweepBanned` drops only `now > nBanUntil`), and `setban 13.9.0.3 add 3600` at +61 saves one without it. Captured 2026-10-19 by the same tool and dashd (BL-214, P4a D25) |

`capture.py` changed after some fixtures were captured (the parallel settle and recorded first ping, both used by `inbound-evict-mn` only, and refactors); none of these changes alters what the tool sends or records in the other scenarios, so they were not recaptured.

## The block-download fixtures (P3 S2)

Every `blocks-*.txt` was written by `contrib/node/p3-transcripts/capture.py` (P3 §6.3 items 1–2),
which reuses the P2a tool's node, peers and format with P2b's all-peer quiet wait, against the same
release dashd v24.0.0-rc.3. The tool refuses to run unless dashd's sha256 is
`e204e3163185b1bffcd3bf3532c0957d8cc11019eaf8f3dbc3b2fc82a7ab5bb4` (P3 D31) and records it in each
file's header. Captured on 2026-10-18:

```
P2A_CAPTURE_DIR=/work/scratch/p3/capture P2A_PORT_BASE=31900 \
  python3 contrib/node/p3-transcripts/capture.py OUT_DIR
```

Nothing in them was edited by hand. The node is genesis-only and in IBD; its peers are outbound
full-relay without `NODE_HEADERS_COMPRESSED`. The 1100 blocks come from a second regtest dashd
run by the same tool (`miner_blocks`, mock time `SIM_START - 2592000`, `generatetodescriptor …
raw(51)`, fetched with `getblock … 0`), so the chain is older than dashd's maximum tip age: the
node stays in IBD and never direct-fetches, and every request is section 12's `MSG_BLOCK`. The
peers serve reactively, one block per input, in the order dashd asked for them; each served block
is recorded as an input (`> N block HEX`), so the replay is scripted.

| Fixture | What the scripted peers do |
|---|---|
| `blocks-1100` | one peer answers `getheaders` with 1100 headers, then serves every block requested (16 in flight, then one request per block) until the node has all 1100 |
| `blocks-stall` | peer 1 (the sync peer) sends the 1100 headers, then peer 2; peer 1 withholds block 1 and both serve the rest until the 1024-block window is all requested and peer 2 has nothing in flight (dashd logs `Stall started peer=0`, its id for peer 1, not on the wire); the mock time moves to `SIM_START + 2` (no disconnect: Core's test is strict) and `SIM_START + 3` (dashd disconnects peer 1, raising the timeout to 4 s, and sends peer 2 `getheaders`, then `getdata` for block 1); peer 2 serves it and the rest to 1100 (dashd decays the timeout to 3 s, then 2 s) |

## The compact-block fixtures (P3 S3)

`cmpct-swap.txt` and `cmpct-buckets.txt` were written by the same tool (P3 §6.3 items 3 and 5)
against the same sha256-pinned release dashd, on 2026-10-19:

```
P2A_CAPTURE_DIR=/work/scratch/p3/capture P2A_PORT_BASE=31900 \
  python3 contrib/node/p3-transcripts/capture.py OUT_DIR cmpct-swap cmpct-buckets
```

(Each came from its own run naming only it; the environment variables shown are the tool's
defaults. `cmpct-buckets` takes about 38 minutes.)

Nothing in them was edited by hand. The node mined one block at `SIM_START` before the peers
connect, so it is out of IBD and its tip is recent. Block 2 comes from a second regtest dashd at
the same mock time that was given the node's block 1 by `submitblock` and mined one more
(`lacking_block`), so the node has its header and data from no one. No transaction reaches dashd,
so its mempool and its extra-transaction ring stay empty. P3's records are defined in the tool's
docstring and in `dashd_p2p::sim::transcript`: `close N` (the tool closed its socket and waited
until `getconnectioncount` was 0), generated `cmpctblock` inputs
(`gen:crowded:HEADER:N:CROWD:BUCKETS sha256:HEX`, rebuilt by the replay from BUCKETS, not from
the `bucket_count` it tests, and checked against the digest),
and, in `cmpct-buckets` only, dashd's payloads over 4096 bytes as `sha256:HEX:LEN`.

| Fixture | What the scripted peers do |
|---|---|
| `cmpct-swap` | one outbound peer without bit 11 sends `sendcmpct(0, 1)` and announces block 2 by `headers`; dashd asks `getdata(MSG_CMPCT_BLOCK)` (type 20); the peer's `cmpctblock` (nonce 0, coinbase prefilled, no short IDs) connects it, and dashd selects the peer as high-bandwidth (`sendcmpct(1, 1)`, `MaybeSetPeerAsAnnouncingHeaderAndIDs`; S4 compares it, S3 dropped it) |
| `cmpct-buckets` | 642 cases: for every n in 13..=65535 within one of a change of libstdc++'s bucket count (`bucket_count(n) != bucket_count(n - 1)`), and for 13 then 12 short IDs in one bucket, a fresh inbound peer (numbers cycle 1–200) handshakes, sends block 2's header as a `cmpctblock` with `crowded(n, crowd)` short IDs, nonce 0 and nothing prefilled, and is closed; dashd answers `getdata(MSG_BLOCK)` for 13 (`READ_STATUS_FAILED`, np:5179-5183) and `getblocktxn` for all n indexes for 12 (np:5198-5202) |

`tests/dashd_p3_transcripts.rs` replays them against a node running P3's `BlockDownload`, with
the replay's block-`getdata` drop off (`Hooks::downloads_blocks`).

`cmpct-announce.txt` was written by the same tool (P3 §6.3 item 4, S4) against the same
sha256-pinned release dashd, on 2026-10-19:

```
P2A_CAPTURE_DIR=/work/scratch/p3/capture P2A_PORT_BASE=31900 \
  python3 contrib/node/p3-transcripts/capture.py OUT_DIR cmpct-announce
```

Nothing in it was edited by hand. The node mined 103 blocks to `raw(51)` (OP_TRUE) at
`SIM_START`, so it is out of IBD and coinbases 1–3 are mature. One outbound peer sends
`sendcmpct(1, 1)` and a `getheaders` from the tip, which dashd answers with an empty `headers`
and which makes the tip its `pindexBestHeaderSent`. The tool then calls `generateblock` with three
transactions spending those coinbases' OP_TRUE outputs, and records the block's bytes as
`submitblock HEX` (a record for the RPC, before dashd's sends). dashd's `NewPoWValidBlock` sends
the peer that block's `cmpctblock` with a fresh nonce, the coinbase prefilled and three short IDs.
The replay masks the nonce and the short IDs, which differ by design; `compact.rs`'s encoder test
rebuilds dashd's bytes exactly from the block and dashd's nonce.

## The block-serving fixtures (P4b)

The `serve-*` fixtures were written by `contrib/node/p4b-transcripts/capture.py` (P4b D12),
which reuses P2a's node, peers and format and P3's all-peer settle and sha256 check, against the
same release dashd v24.0.0-rc.3 (sha256 `e204e316…a7ab5bb4`), on 2026-10-21: the first six in one
run, `serve-filters-protx` in a later run naming only it (review r1):

```
P2A_CAPTURE_DIR=/work/scratch/p4b/capture P2A_PORT_BASE=32700 \
  python3 contrib/node/p4b-transcripts/capture.py OUT_DIR
```

Nothing in them was edited by hand. Every node starts from a fresh datadir with
`-mocktime=1700000000` and mines its blocks itself (`generatetodescriptor … raw(51)`) before any peer
connects. Each block is recorded as `submitblock HEX` (dashd's `getblock HASH 0`) in place of
P2a's `chain HEADER`, so the replayed node stores the same bytes dashd serves; its tip is then
recent, so it is out of IBD and `CanDirectFetch` holds. Each peer is inbound from 127.0.0.1 and
advertises NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED | NODE_HEADERS_COMPRESSED. A local
peer at +100 is disconnected, not discouraged (Core's `IsLocal`), so each scored request has its
own peer.

| Fixture | dashd arguments beyond P2a's | What the scripted peers do, and what dashd did |
|---|---|---|
| `serve-blocks` | — | 20 blocks. `getdata` MSG_BLOCK of the tip (`block`); MSG_CMPCT_BLOCK of the tip (`cmpctblock`; the replay masks the nonce and short IDs) and of 6 deep (`block`, past MAX_CMPCTBLOCK_DEPTH); an unknown hash with block 5 (block 5 only, no `notfound`); MSG_FILTERED_BLOCK without a filter (nothing); blocks 1–3 in one `getdata` (three `block`s, in order) |
| `serve-getblocks` | — | 600 blocks. `getblocks` from genesis (an `inv` of blocks 1–500); `getdata` of block 500 (the block, then the tip's `inv`: the hashContinue); `getblocks` from block 550 stopping at 560 (an `inv` of blocks 551–559: the walk stops at 560 without sending it); a second peer's 102-hash locator (disconnected, np:4645) |
| `serve-filters` | — | 10 blocks. Peer 1: a 64-byte, 5-function filter holding block 5's coinbase txid; MSG_FILTERED_BLOCK of block 5 (`merkleblock`, then the coinbase `tx`) and of 6 (`merkleblock` alone); `filterclear`, then block 5 again (nothing). Peer 2: a 520-byte `filteradd` with no filter (disconnected). Peer 3: a 36,000-byte, 50-function filter and a 520-byte `filteradd` (kept). Peer 4: a 51-function filter; peer 5: a 36,001-byte filter (misformatted); peer 6: a 521-byte `filteradd` (all disconnected) |
| `serve-nobloom` | `-peerbloomfilters=0` | genesis only (IBD). `filterload`, `filteradd` and `filterclear`, each from its own peer: each disconnects it unscored |
| `serve-getblocktxn` | — | 20 blocks. `getblocktxn` of index 0 of the tip (`blocktxn`, the most recent block), of block 10 (10 deep: `blocktxn`) and of block 9 (11 deep: the whole `block`, through the getdata queue); an unknown block (nothing); indexes 0 and 1 of the tip, which has one transaction (disconnected) |
| `serve-limited` | `-prune=1` (manual pruning: NODE_NETWORK off, NODE_NETWORK_LIMITED on, nothing pruned) | 300 blocks. `getdata` of block 10, 290 below the tip (`block`), then of block 9, 291 below (disconnected: NODE_NETWORK_LIMITED_MIN_BLOCKS + 2) |
| `serve-filters-protx` | — | the node mined 510 blocks to its wallet (past DIP3 enforcement), then one block each (511–514) holding a ProRegTx (`protx register_fund`; the header names its hash and owner key ID), its `update_registrar` (new voting and payout addresses), `update_service` and `revoke`. Peer 1 loads a 256-byte, 11-function filter holding only the owner key ID with BLOOM_UPDATE_ALL and asks MSG_FILTERED_BLOCK of each: four `merkleblock`s, each followed by its special `tx` (the ProRegTx matches on the key ID and inserts its hash; the three updates match on it). Peer 2 does the same with BLOOM_UPDATE_NONE: only the ProRegTx is sent |

`tests/dashd_serve_transcripts.rs` replays them with each fixture's arguments (`serve-limited`
with prune mode on and NODE_NETWORK_LIMITED services).

## The blocks-only fixture (P5a D23)

`blocksonly-violations.txt` was written by `contrib/node/p5a-transcripts/capture.py`, which reuses
the P2a tool, against the same release dashd v24.0.0-rc.3 (sha256 `e204e316…a7ab5bb4`), on
2026-10-22:

```
P2A_DASHD=…/dashcore-24.0.0-rc.3/bin/dashd python3 contrib/node/p5a-transcripts/capture.py OUT_DIR
```

Nothing in it was edited by hand. The node runs with `-blocksonly=1` (rdashd's default until P5,
DEC-322) on a genesis-only node. Three outbound full-relay peers complete the handshake: dashd's
`version` carries relay 0 and no `senddsq` follows `verack` (compare
`handshake-outbound-full-relay`). Peer 2 then sends a `tx` and peer 3 an `inv` of the
`p2p_blocksonly.py:36` transaction; dashd closes both. Its `debug.log` had, for those two:

```
[net] tx sent in violation of protocol peer=1
[net] tx (0000000000000000000000000000000000000000000000000000000000001234) inv sent in violation of protocol, disconnecting peer=2
```

(dashd's peer ids start at 0.) `sim_p5a`'s blocks-only sims assert the same lines.
`MSG_ISDLOCK` is not in the fixture: a fresh regtest node has InstantSend off (SPORK_2 is unset),
so its `AlreadyHave` returns true before the blocks-only branch; turning the spork on needs a
spork key and an `spork` message signed with it, more than this fixture is worth. The sims cover
that branch.
