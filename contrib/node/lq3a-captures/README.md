# LQ3a regtest captures

Ground truth for how Dash Core v24.0.0-rc.3 hashes its recovered-signature (`qsigrec`), ChainLock (`clsig`)
and InstantSend-lock (`isdlock`) P2P objects: the wire payload of each, next to the hash Core announced for it
in the `inv`, plus the quorums needed to re-verify the signatures.

`capture_lq3a.py` is a Dash Core functional test (`DashTestFramework`, 5 nodes, 4 masternodes, 4-of-3
`llmq_test` and `llmq_test_instantsend` quorums, spork 17 on). `run.sh` copies it into the exported
`test/functional/` of a Core tag and runs it against that tag's release binaries.

It mines three DKG cycles, so each type holds `keepOldConnections` (3) quorums, one more than
`signingActiveQuorumCount` (2). The third-newest `llmq_test` quorum then signs one recovered sig, named
explicitly with `quorum sign 100 <id> <msg> <quorumHash>`: Core relays it because `IsQuorumActive` scans
`keepOldConnections` quorums, though signing never selects that quorum.

Raw `P2PInterface` observers (one on every node) send `qsendrecsigs(true)`, record each `inv` of type 28
(recovered sig), 29 (ChainLock) or 31 (ISDLOCK), then send a single-entry `getdata` per inv, one at a time, and
keep the answering message's exact payload bytes. No payload is hashed during capture, so the pairing inv hash
to payload comes only from the `getdata` round trip. node0 runs `-watchquorums=1`: it never recovers a sig
itself, so it learns each one from a peer and announces it by `inv`; a masternode that recovered a sig itself
pushes an unsolicited `qsigrec` with no `inv`, and such payloads are not in the fixture. Captures from
different observers are merged by payload and must agree on the inv hash.

## Rerun

```
contrib/node/lq3a-captures/run.sh <dash-checkout> <tag> <bin-dir> <work-dir> <out-json>
contrib/node/lq3a-captures/run.sh ~/workspace/dash v24.0.0-rc.3 \
    ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin /work/scratch/lq3a/capture \
    dash-evo/tests/data/lq3a-regtest-captures.json
```

`<bin-dir>` must hold `dashd` and `dash-cli` whose `-version` is exactly the tag (probed with a fresh
temporary datadir, because `-version` writes the datadir's `settings.json`). The work dir gets a venv with
`dash_hash`, the exported tests and `ft/run` (datadirs and logs, `--nocleanup`). `LQ3A_PORTSEED` (default 2311)
and `LQ3A_TIMEOUT_FACTOR` (default 8) tune the run. Every run mines fresh quorums, so keys, signatures and
hashes differ each time; the committed fixture is one capture.

### Rotated, legacy-scheme capture (BL-218)

`LQ3A_SCRIPT=capture_lq3_rotation.py` runs the second capture instead: 9 nodes, 8 masternodes, V19 deferred to
height 2000 (every signature legacy-scheme), rotated InstantSend (`llmq_test_dip0024`) over two rotation
cycles, and three locks signed at chosen heights (its docstring has the schedule). Its fixture has the same
layout with `bls_scheme` "legacy", type `"103"` beside `"100"`, and `provenance.cycles` and
`provenance.locks` (each lock's signing tip and the cycle Core reports for it). The committed copy is
`dash-oracle-sys/tests/data/lq3r-regtest-v24.0.captures.json`; PROVENANCE.md there has the commands.

## Fixture (`dash-evo/tests/data/lq3a-regtest-captures.json`, sorted keys)

- `provenance`: `dashd_version`, `dashd_sha256` (of the `dashd` that ran), `v19_deployment` (Core's
  `getdeploymentinfo().deployments.v19` at the end), `core_tag`, `tool`, `extra_args` (per node, every
  chain-param arg), `captured_utc`, `keep_old_connections_recsig` (type, quorum hash, id and msg hash of the
  third-newest quorum's recovered sig), plus observers, quorum size/threshold and the heights of the two
  context snapshots.
- `network` ("regtest"), `tip_height` (after generation), `bls_scheme` ("basic").
- `quorum_list`: raw `quorum list 10` (identical before and after generation, asserted).
- `quorums`: by LLMQ type (`"100"` llmq_test, `"104"` llmq_test_instantsend), newest first as `quorum list`
  returns them: `hash`, `public_key`, `height`, `mined_block`, `index`. Both types hold the same three quorum
  hashes because the two DKGs run at the same blocks.
- `messages`: `command`, `inv_hash`, `payload`, `core_verify`, and for `qsigrec`/`clsig` the decoded `fields`.

### Byte order

- `payload` is the exact wire message payload, hex, in wire order.
- `inv_hash` is `%064x` of the framework's `CInv.hash`, i.e. display (reversed) order of the 32 wire bytes.
  Hash check: `inv_hash == sha256d(payload)[::-1].hex()`.
- Every other 32-byte hash (`hash`, `mined_block`, `fields.quorum_hash`, `id`, `msg_hash`, `block_hash`) is
  display (reversed) order, as the RPCs print and take them. In `payload` the same fields are little-endian
  wire order. `public_key` and `sig` are plain BLS bytes, not reversed.
- `qsigrec` payload: llmqType (1) | quorumHash (32) | id (32) | msgHash (32) | sig (96).
  `clsig` payload: height (i32 LE) | blockHash (32) | sig (96).
- `core_verify` is Core's own verdict: `quorum verify` for `qsigrec`, `verifychainlock` for `clsig`,
  `null` for `isdlock` (not checked).

## Lane (c): quorum keys, recovery and signing-manager transcripts

`LQ3A_SCRIPT=capture_lane_c.py` records Core v24.0.0 with eight regular masternodes,
three dynamically registered Evo masternodes and a non-member controller. It uses
`llmq_test=7:4`, rotated `llmq_test_dip0024=4:3` and `llmq_test_platform=3:2`.
Each ring/mesh phase records DKG status, commitment transactions, both `quorum info`
secret-share flags, contribution payloads, offline shares followed by native signing,
and Core's recovered signatures. The recorder sends no DKG, QDATA or commitment
messages. Its only observer requests are QSENDRECSIGS and GETDATA.

Use a fresh work directory for every attempt. Run the harness as a user unit, with
`TMPDIR` inside that directory (the version probe uses it too):

```sh
work=/work/scratch/lane-c-capture
mkdir -m 700 -p "$work"
systemd-run --user --unit=lane-c-capture --same-dir --collect \
  --setenv=LQ3A_SCRIPT=capture_lane_c.py \
  --setenv=LQ3A_PORTSEED=12314 --setenv=TMPDIR="$work" \
  nice -n 19 contrib/node/lq3a-captures/run.sh \
  ~/workspace/dash v24.0.0 ~/.rust-dashcore-test/dashcore-24.0.0/bin \
  "$work" "$work/raw.json" --seed=2311 --randomseed=2311
journalctl --user -u lane-c-capture -f
python3 test-vectors/lane-c/trim_lane_c.py "$work/raw.json" test-vectors/lane-c
```

The seed fixes request ids, message hashes, submission order and the framework PRNG;
Core generates fresh cryptographic keys, so separate captures differ. Trimming the
same raw capture must produce identical bytes. Keep the raw JSON, message captures,
framework log and debug logs. The provenance document beside the committed fixture
records the actual run and hashes.

Schema 2 follows SG1 R3/R4 rev4. Each controlled phase issues its inputs, then holds
mocktime and all inputs fixed until `hasrecsig` is true everywhere for its expected
signatures, or a cleanup's post-delete log line appears. Convergence polls are
`barrier` events. Exact RPC observations follow the barrier. Only the first
threshold submits for a fresh id and an isolated sign after a barrier have exact
sign booleans; other submits are barrier inputs. There are no racy observations.

Native log lines supply provenance, independently ordered per node by byte
position. They do not establish write completion. Remote writes require a prior
intake line and matching captured QSIGREC bytes; local writes retain their native
recovery line. Duplicate writes and conflicting recovered signatures fail the
capture. Expiry starts at the write phase's held mocktime: +60 is exact, and
+61/+6 waits for deletion before observing absence. The capture also records
vote-only expiration, nine blocks of quorum selection, subscriber versus silent
recsig relay, and native signature receive order without send-pass boundaries.
Each signature-message capture is paired with its native `received` log line,
including the peer id and log offset, so reconnects to one address stay distinct.

For a parser failure after nodes stop, preserve the stopped checkpoint and reparse
its unchanged logs and message captures with the exported framework on `PYTHONPATH`:

```sh
PYTHONPATH="$work/core/test/functional" "$work/venv/bin/python" \
  contrib/node/lq3a-captures/capture_lane_c.py --reprocess \
  "$work/raw.json" "$work/ft/run"
```

Reprocessing runs the same hard evidence checks. A duplicate or missing native
write requires a fresh capture.

`quorum_info.variants` stores distinct public RPC objects. For each node,
`quorum_info.nodes[node].variant` selects its object; `secretKeyShare`, when present,
is added only for the `true` RPC flag. The JSON stores hashes in RPC display order
and message payloads as exact wire bytes. `chain-v24.0.0.hex` includes genesis,
then one raw block per height. Operator keys and secret shares are disposable
regtest material.
