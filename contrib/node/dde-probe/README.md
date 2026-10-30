# dde-probe

Tooling behind BS4a's decode-domain evidence (`docs/node-program/DDE-SWEEP.md`). It asks Dash Core, at both pinned
releases, what it does with a byte string, so base-sdk's decoders can be tested against Core's answers rather than
hand-written expectations.

> DEC-82 (v24 only): the node no longer targets v23.1, and no checked-in vector depends on it. These tools keep
> their two-release harness as the record of how the BS4a evidence was made; the v23.1 half needs a v23.1.8
> build that is no longer part of any gate.

| File | What it does |
|---|---|
| `nodes.py start\|stop [DASHD_ARGS...]` | Runs two regtest dashds from the release tarballs: v23.1.8 on P2P/RPC 29811/29812, v24.0.0-rc.3 on 29821/29822 (`DDE_PORT_BASE` moves both; the default is 29800). Extra arguments (e.g. `-blocksonly=1`) go to both. With `DDE_PAIR=v24` both run v24.0.0-rc.3 (29921/29922 and 29931/29932 at base 29900): the node under test and a peer, for BS7's tools (DEC-82). Datadirs live under `DDE_PROBE_DIR` (default `/work/scratch/dde-probe`); `DDE_DASHD_V23` and `DDE_DASHD_V24` point at the unpacked releases. They never touch the oracles under `/work/chains` or the benchmark nodes under `/work/bench`. |
| `p2p.py` | A minimal V1 P2P client: frames messages, records replies, syncs with ping/pong, reads `getpeerinfo` and the node's `debug.log` (`-debug=net`), and parses `-capturemessages` files. `Peer.log()` keeps only the lines about its own connection (by the node's peer id; Core's peer-less "Exception … caught" line goes with the peer named just before it). |
| `probe_p2p.py OUT.json` | Connects v23.1.8 to v24 to capture a `version` payload Core sent. Then it sends every `version` variant (cut at each field, bool bytes `02`/`ff`, subver 256/257, negative time and version, an obsolete version with a broken tail, trailing bytes), the `headers`/`headers2` count and tx-count cases at several common versions, an unknown command, payloads on empty messages, and `senddsq`/`qsendrecsigs` with an empty and a `02` payload. It records each node's reaction. |
| `probe_gates.py PROBE.json OUT.json` | Handler state and pre-decode gates (DDE-SWEEP §4): a `pong` of 0, 1, 7, 8 (zero, mismatched) and 9 (matching + trailing) bytes while Core's ping is pending, `sendaddrv2` at four versions before and after `verack`, `sendtxrcncl` with reconciliation off and on, `filterload`/`filteradd` decode failures, the V1 command strings of `p2p.COMMANDS` around Core's `IsCommandValid`, and four blocks-only commands, with the log reason of each outcome. It restarts the nodes in the default, `-blocksonly=1` and `-txreconciliation=1` modes, leaves them in the default mode, and fails unless both releases show the outcomes the sweep states. |
| `probe_v2.py OUT.json` | V2 (BIP324) packets at the contents limit, short-ID and long-form, unassigned short IDs (0xfe, 33), the reserved slot 5 and the `p2p.COMMANDS` long-form types; it fails unless both releases show the outcomes the sweep states. It uses Core's functional-test framework for the encryption: `DDE_CORE_TEST_FRAMEWORK` names `test/functional` from `git archive v24.0.0-rc.3 test/functional`, and the Python running it needs the `dash_hash` module (`pip install dash_hash` in a venv). |
| `check_sources.py [DASH_REPO [SWEEP_MD]]` | Asserts, with `git show` at both tags, the Core code behind the claims no live probe reaches: the per-pin DKG intake order and shapes, the V1/V2 command byte ranges, and `ExtNetInfo`'s map. It also checks the sweep's DKG "Handler reads" cell: each message's field list must equal the one it derives from Core's serializers (`dkgmessages.h`, `bls_ies.h`) at both tags, and the stated premature-commitment signature count and 3-member size must equal what those fields give. It fails naming the claim that no longer holds. |
| `gen_corpus.py PROBE.json GATES.json V2.json` | Renders base-sdk's `pkgs/p2p_core/corpus/decode_domain.json5` from the three outputs, and fails if the two releases disagree or a `probe_p2p.py` case's log and connection state do not fit its label. |
| `probe_payload.py BSDK_ROOT OUT.json` | Special-payload cases through `decoderawtransaction`, which prints the payload object only when `GetTxPayload` succeeds: one trailing byte, CbTx `bestCLHeightDiff` at and above `MAX_SIZE`, and a ProUpServTx v3 whose NetInfo repeats a purpose (it asserts Core's map order at both pins). |
| `oracle/` | Transaction cases through the O1 oracle (both pins in-process), base-sdk 33fcc59 and BS4a (pinned by `rev`, `Cargo.lock` committed): Core's `tx_invalid.json` value vectors, and a mainnet CbTx with its payload padded up to and past `MAX_SIZE`. `dash-oracle-sys` is a path dependency on the repository root, and it inherits the workspace there: build from a copy of rust-dashcore `node/o1-oracle-sys` at 36c2b987 (`git archive 36c2b987`) with `oracle/` placed at `contrib/node/dde-probe/oracle`. That commit pins the ABI 4 shim archives cached under `/work/cache/oracle`. |
| `bench/` | The BS4a gate: decodes base-sdk's 34 mainnet corpus blocks with both base-sdk revisions (pinned by `rev`, `Cargo.lock` committed). Compare `perf stat -e instructions:u` counts, minus an `ITERS=0` run. |
| `bs8-judge/` | BS8's Core judge: `judge.cpp` reads each overlay message (`clsig`, `isdlock`, `qsigrec`, `qfcommit`, `mnauth`, `spork`, `platformban`, `qgetdata`, `qdata`, `getqrinfo`, `qrinfo`, `mnlistdiff`, an SML entry, a quorum snapshot) with Core's own serializers and writes it back, one line in and one JSON verdict out. `build.sh OUT [JUDGE.cpp]` links it (or another judge's source) against a fresh build of the v24.0 oracle pin (`dash-oracle-sys/pins.txt`); `OUT/MANIFEST` records the commit, the source, each archive and the binary. |
| `bs8/` | BS8's differential (`bs8-tool`): base-sdk's reads against the judge's. `cases` runs the fixed cases (`src/seeds.rs`: BLS samples of every class per field under both schemes, the `mnlistdiff` and SML entry layouts at each protocol band, read limits and flag bytes) and the O4 fixtures; `vectors` runs `vectors.txt`, Core's `messages.py` encodings (`gen_vectors.py`); `fuzz` mutates the cases per command (`diff_<cmd>`). A different write-back is counted and goes only to `BS8_PRIVATE_CLASSIFIER` (PLAN §7). `make_corpus.py` renders base-sdk's `pkgs/p2p_core/corpus/core_overlay.json5` from the `cases` output. It links the base-sdk fork the root pins (its `[patch]` repeats the root's table), `Cargo.lock` committed. |
| `bs9-judge/` | BS9's Core judge, built by `bs8-judge/build.sh OUT bs9-judge/judge.cpp`: reads the DKG messages (`qcontrib`, `qcomplaint`, `qjustify`, `qpcommit`) through `net_dkg.cpp`'s handler checks against a case's quorum parameters (its length, `MaxDKGMessageSize`, the framing walk, all compiled in from the pinned file) then the worker's whole read, and the signature share messages (`qsigsesann`, `qsigsinv`, `qgetsigs`, `qbsigs`, `qsigshare`) as `net_signing.cpp` does; and (BS9b) the CoinJoin (`dsa`, `dsi`, `dsf`, `dss`, `dsc`, `dssu`, `dstx`, `dsq`) and governance (`govobj`, `govobjvote`, `govsync`) messages as their handlers read them at the case's stream version, with mainnet's pool bound; prints the verdict, the bytes read, the write-back, the hash Core keys the object by (a DKG message's inventory hash, `GetHash()`, a mixed transaction's txid) and the sign hashes. |
| `bs9/` | BS9's differential (`bs9-tool`), BS8's pattern for those messages: `cases` runs `src/seeds.rs` (each DKG cap at +0/+1, walk bounds and padding bits by quorum, non-canonical counts, BLS samples of every class per field under both schemes, `VARINT` widths and overflow, both `AUTOBITSET` forms, list limits and the `qbsigs` total; for BS9b, `dsa` flags by version, special transactions, the pool counts at 180/181, `dsq`'s ready byte, signature sizes, the vote's 65/96 rule and the bloom filter's data cap); `fuzz` mutates them per command, and the stream version (`BS8_PRIVATE_CLASSIFIER` as in `bs8/`). `make_corpus.py ii|iii` renders base-sdk's `pkgs/p2p_core/corpus/core_overlay_ii.json5` or `core_overlay_iii.json5`. It pins the base-sdk fork revision under test directly, with no `[patch]`, `Cargo.lock` committed. |

| `bs7/` | BS7's helper (`bs7-tool`): `mine` regtest headers, `predict` base-sdk's `decode_with_policy` verdict (and, for `headers`/`headers2`, every decoded header hash), `roundtrip` a payload (re-encode, and for `headers2` Core's reply and announcement encoders), and with `--features oracle` the in-process `fuzz-tx`/`fuzz-block` targets (`diff_p2p_tx`/`diff_p2p_block`: base-sdk's handler read against the O1 oracle's deserializer at the v24 pin (DEC-82): the verdict, the bytes read on success and the error class on failure, plus the node's own read through CS0's `dash-consensus` (`decode_tx`/`decode_block`, `core_bytes`) on every accepted case; a base-sdk re-encoding that differs from Core's is counted apart, not as a mismatch, only when the node's `core_bytes` equal Core's (PLAN §7 handling; ask the manager); the rule does not tell one such difference from another). A fuzz run exits 1 on a mismatch, when no case ran, or when the private classifier fails. `predict` also prints each error's class in Core's terms (`src/class.rs`); an unclassified error matches nothing. Built from the root's base-sdk pin (BS7 is merged into the fork) through a `[patch]` that repeats the root's table and adds `dash-p2p-core`; the pin check and `bump-base-sdk.sh` treat it like `cs2-diff`. |
| `DDE_PAIR=v24 bs7_probe.py OUT.json` | BS7's Core-judged evidence on a fresh v24 pair started with `-debug=validation` (v24.0.0-rc.3 is the only parity reference, DEC-82): builds a regtest chain with eleven block versions (`-blockversion` segments) on the node under test and syncs it to the peer under `-capturemessages`, then collects the messages Core builds on request (`getheaders2` replies and announcements, compact, filtered and whole blocks, `blocktxn`, `inv`, and with `-peerblockfilters` the BIP157 replies), the handler verdict for every BS7 command (valid, cut, oversized, trailing), and the V1 framing cases. Each `merkleblock` and BIP157 reply carries what was requested (`expect`: the block, or the reply's fields from `getblockhash` and `getblockfilter`, the filter-header chain checked against Core's), which base-sdk's corpus test compares with the decoded reply. The probe announces its own user agent, `/dde-probe:bs7/` (DEC-83). `BS7_SKIP_BUILD=1` reuses an earlier run's chain. |
| `bs7_short_ids.py [DASH_REPO]` | Core's V2 short-ID tables and send gate at both tags from source (`git show`): asserts the code shapes (`IsValidV2ShortID`, `V2MessageMap`, `SetMessageToSend`'s gate, `m_peer_version`'s initial value), fails if the tags differ, and prints base-sdk's `short_ids.json5`. |
| `DDE_PAIR=v24 bs7_diff_headers2.py OUT.json SECONDS SEED` | `diff_headers2`, against the v24 node under test: mines fresh regtest headers on blocks Core has, encodes them as `headers2` with random compression choices and mutations, sends each to both nodes, and compares Core with `bs7-tool predict`: the verdict of a thrown message (DEC-98), and for a message base-sdk refuses as a header Core's `Compress` never builds (BSX D-07), that Core rejects it too; for a read one, the header count, the bytes read against an independent walk of the wire framing, every logged rejection against base-sdk's ordered headers and Core's index, hashless rejections against base-sdk's decode, and the connection's state. Negative controls at the end mutate base-sdk's side of matched cases; each must fail. It exits non-zero on a mismatch, a harness error, an uncaught control or a node without cases or controls. |
| `DDE_PAIR=v24 bs7_diff_framing.py OUT.json CASES SEED` | `diff_v1_framing`, against the v24 node under test: generated V1 messages (magic, length, checksum, command bytes) after a handshake; Core's verdict and reason (disconnect for magic or length, drop for checksum or command, deliver) against base-sdk's `V1Header` checks through `bs7-tool frame`. It exits non-zero on a mismatch, a harness error or a node without cases. |
| `bs7_oracle_headers2.py mainnet\|testnet OUT.json [SAMPLE_DIR]` | Walks the reference oracle's whole header chain with `getheaders2` over P2P (ORACLE.md allows local peers) and checks every Core-built reply through `bs7-tool roundtrip` and `getblockhash`; keeps short version-diverse replies for the corpus. |
| `bs7-bench/` | BS7's bench gate (PLAN BG: crit headers2 decode): decodes Core-built `headers2` replies with base-sdk before BS7 (481f8c3) and with BS7 (ab017ae), both pinned by `rev`. Compare `perf stat -e instructions:u` counts, minus an `ITERS=0` run. |
| `bs7_getblocktxn.py DASH_REPO OUT_DIR` | Compiles `bs7-getblocktxn/getblocktxn.cpp` against Core's own `blockencodings.h` at v24.0.0-rc.3 and prints base-sdk's `core_getblocktxn.json5`: for 432 payloads (edge cases, every prefix, seeded random suffixes), Core's verdict and, when it read the payload, the bytes read (DEC-98). |
| `bs7_exit.py` | The exit status shared by `bs7_diff_headers2.py` and `bs7_diff_framing.py`; run alone, their negative control: each command's `main` with a stubbed runner must exit 0 on a clean run and non-zero on a mismatch, a harness error or an empty run (`selftest-checks.sh` runs it). |
| `gen_bs7_corpus.py PROBE.json SAMPLE_DIR MAIN.json TEST.json OUT_DIR` | Renders base-sdk's `core_headers2.json5`, `core_messages.json5` and `core_verdicts.json5` from the v24 node under test, failing if a case's log does not fit its label. |

`oracle/`, `bench/`, `bs7/`, `bs7-bench/`, `bs8/` and `bs9/` are standalone crates (`[workspace]` of their own), not workspace
members. Build them from a copy outside the repository so their `target/` stays out of the tree; `bs8/` builds in place
(its `target/` is ignored), as it reads the node's `dash-consensus` by path.

Typical run:

```bash
export DDE_PROBE_DIR=/work/scratch/dde-probe
export DDE_DASHD_V23=$DDE_PROBE_DIR/dashcore-23.1.8 DDE_DASHD_V24=$DDE_PROBE_DIR/dashcore-24.0.0-rc.3
export DDE_CORE_TEST_FRAMEWORK=$DDE_PROBE_DIR/core-tf/test/functional   # git archive v24.0.0-rc.3 test/functional
python3 nodes.py start
python3 probe_p2p.py $DDE_PROBE_DIR/p2p.json
$DDE_PROBE_DIR/venv/bin/python probe_v2.py $DDE_PROBE_DIR/v2.json        # venv with dash_hash
python3 probe_gates.py $DDE_PROBE_DIR/p2p.json $DDE_PROBE_DIR/gates.json
python3 gen_corpus.py $DDE_PROBE_DIR/p2p.json $DDE_PROBE_DIR/gates.json $DDE_PROBE_DIR/v2.json > $DDE_PROBE_DIR/decode_domain.json5
python3 probe_payload.py ~/workspace/base-sdk $DDE_PROBE_DIR/payload.json
python3 check_sources.py ~/workspace/dash
python3 nodes.py stop
```

BS7's run on the v24 pair (ports 29921/29931, datadirs in RAM):

```bash
export DDE_PAIR=v24 DDE_PROBE_DIR=/dev/shm/bs7-probe DDE_PORT_BASE=29900 BS7_TOOL=<copy>/contrib/node/dde-probe/bs7/target/release/bs7-tool
python3 nodes.py start -debug=validation
python3 bs7_probe.py $OUT/probe.json                 # leaves the nodes in -peerblockfilters mode
for net in testnet mainnet; do python3 bs7_oracle_headers2.py $net $OUT/oracle-$net.json $OUT/samples; done
python3 gen_bs7_corpus.py $OUT/probe.json $OUT/samples $OUT/oracle-mainnet.json $OUT/oracle-testnet.json <base-sdk>/pkgs/p2p_core/corpus
python3 bs7_short_ids.py > <base-sdk>/pkgs/p2p_core/corpus/short_ids.json5
BS7_PROBE_JSON=$OUT/probe.json python3 bs7_diff_headers2.py $OUT/diff-headers2.json 1800 101
bs7-tool fuzz-tx seeds-tx.hex 7200 1; bs7-tool fuzz-block seeds-block.hex 7200 1   # --features oracle build
python3 nodes.py stop
```

`fuzz-tx`/`fuzz-block` take an optional private classifier by path from the
env (`BS7_PRIVATE_CLASSIFIER`, PLAN §7 handling): it receives the cases counted
apart and fails the run unless it exits 0. `contrib/node/check-private-tests.sh`
runs both targets with the private tree's classifier and checks its receipt.

The fuzz seeds are the `raw` transactions and blocks of base-sdk's `pkgs/primitives/corpus` (mainnet, Core-built).

A cold start takes minutes on slow disks; `nodes.py` waits up to 15 minutes per node. A regenerated corpus differs from
the committed one only in the bytes Core randomizes (the `version` time, nonce and mnauth challenge). Every `core`
outcome must match.

BS8's differential (the judge build takes about an hour cold; `BS8_REUSE=1` relinks):

```bash
bs8-judge/build.sh /work/scratch/bs8/judge
cd bs8 && cargo build --release && T=target/release/bs8-tool J=/work/scratch/bs8/judge/bs8_judge
BLS=~/workspace/base-sdk/pkgs/pkc/corpus/bls_wrapper.json5 RDC=$(git rev-parse --show-toplevel)
$T cases $J $BLS $RDC /work/scratch/bs8/cases.json
python3 make_corpus.py /work/scratch/bs8/cases.json /work/scratch/bs8/judge/MANIFEST core_overlay.json5
python3 gen_vectors.py ~/workspace/dash-v24.0-tree $BLS > vectors.txt  # git archive of the pin
$T vectors $J vectors.txt
BS8_PRIVATE_CLASSIFIER=/path/outside/the/tree $T fuzz $J $BLS $RDC qrinfo 3600 7
```

BS9's differential (the judge builds as BS8's does; `BS8_REUSE=1` relinks):

```bash
bs8-judge/build.sh /work/scratch/bs9/judge bs9-judge/judge.cpp
cd bs9 && cargo build --release && T=target/release/bs9-tool J=/work/scratch/bs9/judge/bs9_judge
git -C ~/workspace/base-sdk show <rev>:pkgs/pkc/corpus/bls_wrapper.json5 > /work/scratch/bs9/bls_wrapper.json5
$T cases $J /work/scratch/bs9/bls_wrapper.json5 /work/scratch/bs9/cases.json
python3 make_corpus.py ii /work/scratch/bs9/cases.json /work/scratch/bs9/judge/MANIFEST core_overlay_ii.json5
python3 make_corpus.py iii /work/scratch/bs9/cases.json /work/scratch/bs9/judge/MANIFEST core_overlay_iii.json5
BS8_PRIVATE_CLASSIFIER=/path/outside/the/tree $T fuzz $J /work/scratch/bs9/bls_wrapper.json5 qcontrib 3600 7
$T fuzz $J /work/scratch/bs9/bls_wrapper.json5 govobjvote 3600 7
```

`bs8/` and `bs9/` pin the base-sdk fork as `git = "file:///work/home/agent/workspace/base-sdk"`, the URL the root's
`[patch]` uses and `base_sdk_pins.py` requires of nested workspaces. With the fork checked out elsewhere (`$BASE_SDK`),
rewrite that URL in the copy you build from; the revisions stay as committed:

```bash
sed -i "s|file:///work/home/agent/workspace/base-sdk|file://$BASE_SDK|" Cargo.toml Cargo.lock
```

`bs8/` and `bs9/` pin the base-sdk fork as `git = "file:///work/home/agent/workspace/base-sdk"`, the URL the root's
`[patch]` uses and `base_sdk_pins.py` requires of nested workspaces. With the fork checked out elsewhere (`$BASE_SDK`),
rewrite that URL in the copy you build from; the revisions stay as committed:

```bash
sed -i "s|file:///work/home/agent/workspace/base-sdk|file://$BASE_SDK|" Cargo.toml Cargo.lock
```
