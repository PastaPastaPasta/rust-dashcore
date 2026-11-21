# O13a: fingerprint lab v0 (protocol-parity handshake transcripts), notes and D-rows

O13a (PLAN §4.7, M2 exit 2; shrunk by DEC-83) diffs rdashd's handshake transcripts against
dashd v24.0.0-rc.3's per profile: PRD §4.2 behaviours 1–5, 9 and 10, over V1 and V2 (BIP324),
outbound and inbound (P2b's minimal responder), the masternode probe excluded. Identity is out
of scope (DEC-83): rdashd's user agent is its own, so the user agent is masked and never
compared. Core line numbers are the release tag v24.0.0-rc.3's (DEC-82); `np:` is
`net_processing.cpp`, `net:` is `net.cpp`.

**What the lab claims (D13).** *Binary-level* parity for the seven live outbound scenarios
(the rdashd binary against dashd: regtest, testnet and mainnet over V1 and V2, and the V2
rekey). *dashd-p2p-level* parity, rdashd's processor and network core in `dashd_p2p::sim`, for
everything else: every inbound fixture (default, v2off, whitelist, testnet and mainnet, V1 and
V2), the blocksonly and whitelist profiles, listen0 and the V1 framing errors. The sim-only
cells are coverage gaps for the binary: BL-358 (live inbound, after BL-327) and BL-360 (live
`-blocksonly` and `-whitelist`, once the rdashd binary accepts them). An empty sim diff does not
say the binary's diff is empty.

## What a transcript is

The P2a/P2b fixture format (`contrib/node/p2a-transcripts/capture.py`'s docstring) is the
transcript: per segment (one input), the ordered messages the node sent each peer, with their
payloads. A transcript therefore carries the `version` fields, the order of `verack`,
`sendaddrv2`, `sendheaders(2)`, `sendcmpct`, `senddsq`, `ping`, `getheaders(2)`, `getaddr`, and
whatever else the node sends in the window, and whether and when it closes a connection. O13a
adds, for V2 connections, each message's type encoding (short ID or 12-byte long form), and
raw-byte inputs for the V1 framing cases. The diff compares each peer's messages in order within
a segment; the interleaving across peers inside one segment is not recorded, so not compared.

## The two legs

1. **The sim leg (tier 1, deterministic).** `contrib/node/fingerprint/capture.py` (which reuses
   P2a's node, peers, synchronisation and format, as P2b's tool does) captures dashd's
   transcript for each profile × scenario into `conformance/tests/fixtures/fingerprint-v24/`.
   `dash-conformance`'s test `fingerprint` replays each fixture's peer side against rdashd's
   processor and network core in `dashd_p2p::sim`, configured from the fixture's own dashd
   arguments, *records* rdashd's transcript (the same format), and diffs the two with the masks
   below. The test passes only when the diff equals the fixture directory's `KNOWN-DIFFS.json`
   exactly: every remaining difference names its BACKLOG row, and a listed difference that
   disappears fails too (a ratchet).
2. **The live leg (lab evidence).** The same tool drives the **rdashd binary** and dashd with
   identical arguments (`-connect=127.0.0.1:P`, so both dial the scripted peer as a manual
   connection) on regtest, testnet and mainnet magic, V1 and V2, and writes both transcripts;
   `dash-conformance fingerprint A B` applies the same masks and diff. Here the scripted
   peer speaks V2 through Core's own BIP324 code (the functional test framework's `v2_p2p.py`
   at v24.0.0-rc.3), so rdashd's real cipher, salt and rekeying are checked against Core's, which
   the sim leg (both ends `dash-p2p-transport`) cannot do.

## Masks (each with the Core line that makes the field random)

| Field | Mask | Core |
|---|---|---|
| `version` `nNonce` (8 bytes at offset 72) | zeroed | net:492 (outbound), net:1970 (inbound): `GetDeterministicRandomizer(RANDOMIZER_ID_LOCALHOSTNONCE).Write(id)`, keyed by `nSeed0/nSeed1` (`GetRand` at init); np:1565 sends it |
| `version` MNAUTH challenge (32 bytes after `fRelay`) | zeroed | np:1573-1574 `GetRandBytes(mnauthChallenge)` |
| `version` `strSubVer` | replaced by `<user agent>` (length and bytes); the fields after it are still compared at their own positions | not random: identity, out of scope by DEC-83 |
| `ping` nonce | zeroed | np:6177 `nonce = GetRand<uint64_t>()` (MaybeSendPing) |
| V2 ellswift key, garbage (length and bytes), garbage terminator, decoys | never part of a transcript: the capture records messages after the transport | net:1061-1065 `GenerateRandomGarbage` (`randrange(MAX_GARBAGE_LEN + 1)`), net:1101 `GenerateRandomKey()`, `GetRandHash()` |
| `version` `nTime` | **not masked**: both legs freeze the mock time (`-mocktime=1700000000`), so it is deterministic | np:1564 |

Nothing else is masked, and nothing is dropped: the replay's "later task" drops (P3's block
`getdata`, SP1's sync tick sends) are not applied here, so a message dashd sends in a window and
rdashd does not is a difference, with its BACKLOG row. A unit test flips every byte of a real
dashd `version` and checks that the diff sees each flip outside the three masked ranges, and
none inside them (masking never hides a protocol difference).

## Profiles

Each profile is a dashd argument set on top of the capture tool's base arguments (P2a's).
Every profile runs both directions where the profile admits one, so each is diffed in both
directions.

| Profile | dashd arguments | Scenarios | Why (behaviours) |
|---|---|---|---|
| `default` | — | outbound full-relay V1, V2 to a bit-12 peer; inbound V1 (V1 detection on a V2 node), inbound V2; `framing`: inbound V1 with a bad checksum, an invalid command, an oversized header (peer 1) and a wrong magic (peer 2); `rekey`: outbound V2, 230 pings each answered | 1–5, 9, 10 (drop-and-keep vs disconnect, the 3 MiB cap; rekey every 224 packets, whose cipher only the live leg checks) |
| `blocksonly` | `-blocksonly` | outbound V1, inbound V1 | 1 (`fRelay` 0), 5 (`senddsq` only when not rejecting transactions; `sendcmpct` by `CanRelay`) |
| `listen0` | `-listen=0` | outbound V1; inbound: the TCP connection is refused | 3 (no inbound at all), 1 (`addrMe` with no listener) |
| `v2off` | `-v2transport=0` | outbound V1 to a bit-12 peer; inbound V1; inbound V2 (the key read as V1 bytes: closed) | 2 (0x0C05), 9 (no V2 when off) |
| `whitelist` | `-whitelist=127.0.0.1` | inbound V1, inbound V2 | 4–5 with the implicit permissions (`noban`, `download`, `mempool`, `relay`) |
| `testnet`, `mainnet` | `-testnet` / none, `-connect=0` | outbound manual V1, V2 (`addconnection` is regtest-only); inbound V1, V2 | 1–5, 9 (magic in V1 framing and the V2 salt), ports and start heights per network |

## D-rows

| D | Decision | Why |
|---|---|---|
| D1 | Two legs (above): the sim leg is the tier-1 test (regtest, testnet and mainnet *parameters*, no socket outside the process); the live leg is lab evidence run by the tool (loopback only, ports below 32768; both binaries run outbound with `-listen=0 -connect=127.0.0.1:P` to the tool's listener, D12, while the inbound pairs' `-connect=0 -listen=1 -bind=127.0.0.1` waits for BL-327). | The brief's tier-1 rule (regtest-only, no public network) and its loopback pairs. The live leg is the only place rdashd's own binary, init and V2 cipher meet Core's peer code. |
| D2 | rdashd's binary does not listen yet (BL-327, train 27) and refuses `-blocksonly` and `-whitelist` at init (`init/support.rs`: `Unsupported("P2a-core")`, `Unsupported("P4")`). So the live leg covers outbound only, and every inbound and option profile is compared in the sim leg, whose node is configured with the processor's and network core's own options for those flags. Each refusal is a difference with its BACKLOG row. | Testing what exists; the processor already implements `-blocksonly` (`ProcessorOptions::blocks_only`) and the network core `-whitelist` (`NetConfig::whitelist`). |
| D3 | One mask implementation, in Rust: `dash_conformance::fingerprint::mask`, used by both legs' diff. The Python tool only records, and the recording replay writes rdashd's messages unmasked. P2a's `masked_hex` (the same fields, plus `cmpctblock`'s nonce) stays as the replays' own: it panics on a cut or oversized-length `version`, which the lab must report as a difference instead (the byte-flip test flips the user agent's length too). | Two mask implementations could disagree; the mutants run on the one that judges. |
| D4 | The recording replay: `sim::transcript::record` plays a fixture's inputs exactly as `replay` does and returns the node's transcript (dashd's `<` lines replaced by the node's, unmasked) instead of asserting per segment. `replay` is unchanged. | The brief asks for rdashd's transcripts, not just a pass/fail; the diff then reports every difference at once. |
| D5 | V2 in `sim`: `Sim::connect_v2` and `Sim::accept_v2` give the scripted peer a `dash_p2p_transport::Transport::v2` (responder for an outbound node, initiator for inbound); the handshake runs to completion when the peer is attached (the record that opens the connection). `deliver` encrypts through it; `sent` decrypts and reports each message's type form from its wire size (`raw_size − payload − 20`: 1 short, 13 long). | V2 inbound has no other venue before BL-327, and the format needs the type form (behaviour 9's short-ID use). |
| D6 | Format additions (the existing fixtures parse unchanged): `connect N TYPE SERVICES v2`, `accept N v2`; on a V2 connection each `<` message carries a fifth field, `short` or `long`; `accept N refused` (no listener); `> N raw HEX` (bytes written to the socket as they are, for framing errors). | Minimal extension of the P2a format. |
| D7 | The sim node's configuration comes from the fixture's `# args:` line: the profile flags the lab uses (`-regtest`/`-testnet`/none, `-blocksonly`, `-v2transport=0`, `-listen=0`, `-whitelist=…`, `-connect=0`) are mapped, the capture tool's base arguments are known, and any other argument fails the test. | A fixture cannot be replayed under a configuration it was not captured with. |
| D8 | Not observable in a handshake transcript, so covered elsewhere and named in the matrix: the garbage length distribution (`dash-p2p-transport` `garbage_length_is_uniform_up_to_the_limit`), short-ID gating by peer version (only `platformban` is gated, at 70240, net:989-1004; BL-293's tests), the outbound V2 choice by the peer's bit 12 (`dash-p2p` `dial.rs`, Core net:3289) and the V1 reconnect after an early V2 disconnect (`dial.rs`'s reconnect tests). | Behaviour 9's items that need many sessions or an addrman. |
| D9 | Capture fixes found by the first diffs, all in the tool, so neither node is touched: (a) packets that arrive with the peer's BIP324 version packet are read off the socket by the handshake, so the quiet-period `select` never sees them: they are decoded and recorded in the segment that opened the connection (before this fix dashd's outbound V2 `version` was recorded a segment late, after the tool's own `version`, in both legs); (b) the oversized-header case writes a whole 24-byte header, because a 20-byte one is completed by the tool's sync `ping` and the case then depends on the tool; (c) a long-form V2 type with bytes after its padding is an error, not a stripped name. In the sim, `deliver_raw` waits on `nRecvBytes` (`recv_bytes`), not the per-type counters, which a header alone never moves, and the V2 peer panics on any packet or message its transport refuses after the handshake (judged after each read, as the read that ends the handshake can carry app packets) and on a fallback to V1, instead of dropping either. | A transcript must record the node, not the tool, and nothing may be dropped unseen. |
| D10 | A difference's BACKLOG row is `BL-<n>`, or `pending:O13a-<n>` until the manager assigns a number (BACKLOG.md is the manager's); the test accepts both and requires one. | Implementers do not number BACKLOG rows. |
| D11 | An empty payload renders as `-` in a difference (`verack - long`), so the type form never reads as a payload. | Unambiguous text, one space per field. |
| D12 | The live leg runs both binaries with `-listen=0` and `-connect=127.0.0.1:P` (the tool's listener, below 32768); the brief's inbound live pairs (`-connect=0 -listen=1 -bind=127.0.0.1`) wait for BL-327, and their behaviours are covered by the sim leg's inbound fixtures meanwhile. | rdashd cannot listen before BL-327. |
| D13 | The exit claim is split by leg (the claim above): binary-level for the seven live outbound scenarios, dashd-p2p-level for the inbound, blocksonly and whitelist profiles (and listen0, framing). The matrix marks each sim-only cell as a binary coverage gap with its row: BL-358 (live inbound after BL-327), BL-360 (live blocksonly and whitelist once rdashd accepts the options). | Sol r1 (DEC-363): the sim leg drives rdashd's processor and network core, not its binary (init, option parsing, listener), so its empty diff cannot stand for the binary's. |
| D14 | Both V1 readers validate whole frames as Core's `V1Transport` does (net:731-739 header, net:796-807 message): the selected network's magic, the length cap (`MAX_PROTOCOL_MESSAGE_LENGTH`, 3 MiB, net.h:84), the checksum, the command (printable ASCII, then only NULs: `IsCommandValid`), and no bytes left over when a scenario ends. The capture tool (`p2a-transcripts/capture.py`'s `take_frames`, which the fingerprint tool uses) raises on any of them, and `capture.py selftest` holds the negative controls (wrong magic, another network's magic, bad checksum, unprintable command, bytes after the padding, oversized length, an incomplete trailing frame), run by the lab test `the_capture_tools_v1_reader_refuses_bad_frames`. The sim's V1 peer decodes the node's bytes through the production `dash_p2p_transport::Transport::v1` (the free `v1_frames`, which read command and payload unchecked, is now `Sim::v1_frames` over the same decoder, for `node-l1`'s byte-budgeted peers): a transport error or a dropped message panics, and `record` fails a scenario that ends with an incomplete frame buffered. `sim::tests` holds the same negative controls for the decoder and the incomplete-frame counter, and `sim_m2`'s `a_v1_peer_counts_the_bytes_of_a_frame_cut_short` counts a frame the node's closed socket cut short. The end-of-scenario check is `transcript::assert_whole_v1_frames`, which `record` runs on every peer; a transcript cannot stall the node mid-frame (only a peer's send budget can, which the format does not script), so the lab test `a_recording_that_ends_inside_a_v1_frame_fails` runs that check on a peer the node left 10 bytes into a frame and expects it to fail (DEC-371). The sim counts incomplete V1 frames only, where the capture tool also refuses a V2 packet left half-read. Validation changed no fixture (all 23 recaptured and compared under the masks: 0 differences each), and the live leg still gives 0 differences in all seven scenarios. | Sol r1: an unchecked reader could record a frame Core's peer would refuse, or drop a tail unseen, and the lab would call it parity. |

## Results

**Live leg, binary-level** (`capture.py live` with the D9 and D14 fixes, both binaries from fresh datadirs, 2026-10-22):
rdashd (this branch's debug build) and dashd v24.0.0-rc.3 give **0 differences** in all seven scenarios,
`live-{regtest,testnet,mainnet}-{v1,v2}` and `live-regtest-rekey` (230 pings across the
224-packet rekey, each answered). So rdashd's V1 framing, BIP324 cipher, salt per network,
rekey, short-ID choice and outbound handshake match Core's byte for byte after the masks.

**Sim leg, dashd-p2p-level** (tier 1, `cargo test -p dash-conformance --test fingerprint`):
all 23 fixtures have **no difference**; `KNOWN-DIFFS.json` is empty. (A first run showed one, rdashd's outbound
V2 `version` a segment early; it was the capture tool's, D9(a), and went away on recapture.)

## Matrix (profile × behaviour)

PRD §4.2 behaviours: 1 `version` layout and values, 2 service bits, 3 who speaks first, 4 the
messages after the peer's `version`, 5 the messages after `verack`, 9 BIP324, 10 V1 framing
errors. "=" means the diff is empty for
every fixture of that cell; "sim" and "live" name the leg. "= sim" alone is dashd-p2p-level
parity only (D13): the rdashd binary is not compared there. Each such cell is a coverage gap,
with its row in brackets where it has one ([358] BL-358, live inbound after BL-327; [360]
BL-360, live blocksonly and whitelist). The listen0 and v2off cells are sim-only as well: their
inbound halves fall under BL-358, and their outbound halves are not among the seven live
scenarios.

| Profile | 1 | 2 | 3 | 4 | 5 | 9 | 10 |
|---|---|---|---|---|---|---|---|
| default | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim (out, in), live (out); in: [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out: cipher, rekey, short IDs); in: sim [358] | = sim (framing) [358] |
| blocksonly | = sim [360] | = sim [360] | = sim [360] | = sim [360] | = sim [360] | — (V1 only) | — |
| listen0 | = sim | = sim | = sim (inbound refused) | = sim | = sim | — | — |
| v2off | = sim | = sim | = sim | = sim | = sim | = sim (V2 peer closed; V1 out to a bit-12 peer) | — |
| whitelist | = sim [360] | = sim [360] | = sim [360] | = sim [360] | = sim [360] | = sim (in V2) [360] | — |
| testnet | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | — |
| mainnet | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | = sim, live (out); in: sim [358] | — |

Behaviour 9 items outside a handshake transcript are covered by the tests named in D8. The
rdashd binary's refusals (D2: `-blocksonly`, `-whitelist`, listening) are not transcript
differences; they are the scope of P2a-core, P4 and BL-327, already rows of their own.
