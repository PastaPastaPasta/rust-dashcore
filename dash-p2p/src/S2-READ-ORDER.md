# S2 read order

Each row is one handler. The read stays at the same point relative to the checks that run
before it and the checks that run after it. Nothing was reordered. Line numbers are the
source before this change (node/integration 44c1e135eb) and after it.

`policy::read` and `policy::decode` still enter `read_as_core` at the same place. Inside
that function the order is still: `limited_vector`, then `core_bools`, then the read, then
the `govobjvote` `InvalidValue` arm and `gov_rest`. The read itself changed from
`decode_msg` / `decode_with_policy` to `HandlerRead::read` / `read::decode`.

| Handler | Read before | Read after | Before the read | After the read |
|---|---|---|---|---|
| `policy::decode` `policy.rs` | `policy.rs:898` `P2pMsg::decode_with_policy` | `policy.rs:899` `read::decode` | `limited_vector`, `core_bools` | `gov_rest`; vote `InvalidValue` returns before that |
| `policy::read` `policy.rs` | `policy.rs:926` `T::decode_msg` | `policy.rs:976` `T::read` | same `read_as_core` prefix; other types have no prefix | cursor advanced only after success |
| `GovSyncRead::read` `policy.rs` | `policy.rs:989` `GovSync::decode_msg` | `policy.rs:1039` `GovSync::read` | none in the impl | `From<GovSync>` copies the bloom tail |
| `policy::read_headers2` `policy.rs` | `policy.rs:1013` `headers2_count` then `uncompress_headers2` | `policy.rs:1063-1064` same two calls | count over `headers_limit` is `TooMany` and reads no header | hook calls stay made on a later throw |
| `HandlerCx::read` `registry.rs` | `registry.rs:383` `policy::read` | `registry.rs:384` `policy::read` | none; the handler already ran its pre-read checks | `Throw` only; a refusal is still unreachable |
| `PeerLogic::inline_or_dispatch` `sendcmpct` `logic.rs` | `logic.rs:1113` `cx.read::<SendCmpct>` | `logic.rs:1114` same call | command match | `version == CMPCTBLOCKS_VERSION`, then store the high-bandwidth flag |
| `senddsq` `logic.rs` | `logic.rs:1122` `cx.read::<SendDsq>` | `logic.rs:1123` | command match | `send_dsq()` and `DSQ_INV` set `wants_dsq` |
| `qsendrecsigs` `logic.rs` | `logic.rs:1133` `cx.read::<QSendRecSigs>` | `logic.rs:1134` | command match | `wants_recsigs = send_rec_sigs()` |
| `inv` `logic.rs` | `logic.rs:1138` `cx.read::<Inv>` | `logic.rs:1139` | command match | `len > MAX_INV_SZ` scores and returns |
| `getdata` `logic.rs` | `logic.rs:1147` `cx.read::<GetData>` | `logic.rs:1148` | command match | `len > MAX_INV_SZ` scores and returns |
| `notfound` `logic.rs` | `logic.rs:1163` `cx.read::<NotFound>` | `logic.rs:1164` | command match | `len > NOTFOUND_LIMIT` scores and returns |
| `PeerLogic::on_clsig` `logic.rs` | `logic.rs:1411` `cx.read::<ClSig>` | `logic.rs:1412` same call | ChainLocks disabled returns first; `cx.ctx.bls()` is read at the line before the message | hash, then unrequested check, then `process_new_chainlock` |
| `Sync::on_headers` `sync.rs` | `sync.rs:173` `headers::read_headers2` | `sync.rs:174` same call | `loading_blocks` returns; `last_getheaders_us = 0`; `read_count`; count over `headers_limit` scores and returns | continuity and `process_headers` |
| `headers::read_headers` `headers.rs` | `headers.rs:55` `BlockHeader::decode` | `headers.rs:56` same call (import lines moved it) | the caller already checked the count | transaction-count `CompactSize` |
| `headers::read_headers2` `headers.rs` | `headers.rs:73` `policy::read_headers2` | `headers.rs:74` same call | caller checked the count | memo pairing; no extra check |
| `cmpct::read_cmpct` `node-l1/src/cmpct.rs` | `cmpct.rs:503` `policy::read::<CmpctBlock>` | `cmpct.rs:504` same call | none | empty remainder |
| `cmpct::read_blocktxn` `cmpct.rs` | `cmpct.rs:513` `policy::read::<BlockTxn>` | `cmpct.rs:514` same call | empty input returns before the read | empty remainder |

These handlers call `cx.read` and were not edited. Their checks stay on the same lines.

| Handler | Read | Before the read | After the read |
|---|---|---|---|
| `serve::on_getheaders` `serve.rs:209` and `:212` | `cx.read::<GetHeaders2>` / `GetHeaders` | command selects the type | locator length, then `loading_blocks` |
| `serve_blocks::on_getblocks` `serve_blocks.rs:108` | `cx.read::<GetBlocks>` | none | locator length |
| `serve_blocks::on_getblocktxn` `serve_blocks.rs:172` | `cx.read::<GetBlockTxn>` | none | recent block, then depth |
| `serve_blocks::on_filter` `serve_blocks.rs:271` and `:293` | `cx.read::<FilterLoad>` / `FilterAdd` | `NODE_BLOOM`, else disconnect | throw scores; then size / insert |
| `serve_filters` `serve_filters.rs:49`, `:61`, `:73` | `cx.read` of the three getcf types | command match | `prepare`, then the reply |
| `serve_evo` `serve_evo.rs:43` | `cx.read::<GetQrInfo>` | none | throw scores; then version and `qrinfo` |
| `blocks::on_cmpctblock` `blocks.rs:217` | `cx.read::<CmpctBlock>` | block sync off returns; `loading_blocks` returns | prev-hash lookup, then `process_headers` |
| `blocks::on_blocktxn` `blocks.rs:397` | `cx.read::<BlockTxn>` | `loading_blocks` returns | `process_compact_block_txns` |
| `stubs` `stubs.rs:74` and `:109` | `cx.read::<Commitment>` / `Ssc` | command match | unrequested commitment scores; ssc is logged |
| `platformban::process_message` `platformban.rs:77` | `cx.read::<PlatformBan>` | command match, bans enabled, quorums enabled, `blockchain_synced`, else return | eager signature read (basic scheme), then the ban checks |
| `Transactions::on_tx` (tx and dstx) `transactions.rs:85` | `cx.read::<DsTx>` (dstx only) | initial block download returns; txpool disabled, or dstx with coinjoin disabled, returns | the transaction is taken from the dstx; the tx handler continues |
| `coinjoin server` `server.rs:535`, `:568`, `:614`, `:647` | `cx.read` of `DsAccept`, `DsEntry`, `DsSignedInputs`, `DsQueue` | synced and masternode; `dsa`/`dsi`/`dss` pre-decode replies return first; `dss` peeks the count and may return before the read | session processing |
| `dsq::process_message` `dsq.rs:146` | `cx.read::<DsQueue>` | queue present and synced | inv response, then `client_process_dsq` |
| `sporks::on_spork` `handler.rs:41` | `cx.read::<Spork>` | none | throw scores; then validity and relay |
| `sync_net::process_message` `sync_net.rs:59` | `cx.read::<Ssc>` | already synced returns | log the counts |
| `mnauth::process_message` `handler.rs:122` | `cx.read::<MnAuth>` | command and `blockchain_synced` | tip lookup, then `decide` |

`PeerLogic`'s post-region continuations (`logic.rs:889`) build their `HandlerCx` with `PeerCtx` from the same
`peer.common_version()` and `obs.snap.bls`; they read no message themselves.
