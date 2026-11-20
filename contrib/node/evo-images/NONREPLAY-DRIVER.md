# Recorder state that block replay does not rebuild: the chain driver's rows

The recorder README's non-replayable-state table (plan §9, Q-2) takes these rows from the chain driver
(`driver.py`). "Carried" means the committed image holds it and the replay check fails if it is missing;
"not read" means no O2b-5 session reads it, so the image does not carry it.

| State (where in the recording) | Rebuilt by replaying blocks 1..T? | Disposition | Replay-side check |
|---|---|---|---|
| Member secret key shares of every quorum (each masternode's DKG data: `quorum info <t> <h> true` `secretKeyShare`) | No. They come from DKG messages over P2P, and the replay node is not a member | **Carried** as each quorum's threshold sk in `keys.json` `quorums[].sk`, recovered from the shares at capture with the sk·G = pk check. The individual member shares are **not read**: the session signs with the quorum sk, and members' `membersSig` with the operator sks in `mns` | Q5 on the replay node: every quorum of the §8.4 set has an sk, its sk·G equals the replay node's `quorum info` pk. Q6: every quorum the replay node can select for signing at T is in the set |
| Verification vectors and member public key shares (`pubKeyShare` in `quorum info`) | No (a commitment holds only the vvec hash) | **Not read** | none; the replay node's `quorum info` shows no `pubKeyShare` |
| DKG session state (phases, contributions, complaints, premature commitments), including a cycle in progress at T (S3/S3is: cycle 1296 at stage 4) | No | **Not read**: commitments for windows after T come from the session's own builder | none |
| Mineable commitment pool (node0's commitments received by QFCOMMIT) | No | **Not carried, and empty at every T**: the driver mines each final commitment in the first block that can hold it, and capture requires that no required session has a final commitment that is not mined by T (whether or not T is inside a mining window) and that node0 offers no non-null commitment for T + 1. So the recording's pool equals the replay node's empty pool | Q7 (recording side, at capture) |
| node0's mempool: the masternodes' further MNHF bit-13 signals. Masternodes without the first recovered sig re-sign with each newly selected LLMQ_TEST quorum, so the recording holds more than one signal tx. The driver releases exactly one for block 650 and keeps the others at a −0.1 DASH modified fee | No | **Not read**: no session reads the recorder's mempool, and after block 650 they can never be mined (the bit is signalled). They stay in node0's mempool at every cut and survive restarts through `mempool.dat` | C12 (block 650 holds the only MNHF tx in 1..T) |
| Quorum snapshots, member lists, mined-commitment index, `quorum list` | Yes (computed again for every connected block) | Rebuilt | Q1–Q3 run on the replay node; the manifest's `quorum list` at T and CbTx `merkleRootQuorums` are compared |
| Recorder-only sporks SPORK_17 / SPORK_21 | No (spork messages) | **Not read**: they only switch masternode DKG participation on. Listed in `recording.json` `recorder_sporks`, never in `image_sporks` | C16: every spork not in `image_sporks` is at its default after the replay |
| Recovered signatures | No | The driver signs nothing itself (`signing_ready` only runs `quorum selectquorum`). The recovered signatures in the recording come from ChainLocks, the MNHF signal and asset unlocks, which are the recorder's rows | — |

Notes for the recorder's own rows:

- **ChainLock for T.** A replayed v24.0.0 node does not set its best ChainLock from the CbTx while it is not
  masternode-synced or while SPORK_19 is off (`chainlock/handler.cpp` `ProcessCoinbaseChainLock`), and the image's
  sporks are applied after `-loadblock`. The CLSIG for T itself only ever travels over P2P. If `getbestchainlock` at T
  is a replay-verified field, the manifest has to carry the CLSIG (height, hash, signature), and the replay check can
  verify it with `verifychainlock` instead of comparing `getbestchainlock`.
- **MNHF signal.** `ChainDriver.hold_ehf(13, 650)` holds every signal the masternodes create in node0's mempool and
  releases the first one for block 650 (row above for the rest). Before each block it waits, bounded, until Core's
  state is quiet: every MNHF tx in a masternode mempool is in node0's, every recovered sig a masternode holds for the
  request id (`quorum isconflicting`, then `quorum hasrecsig` per quorum msgHash) belongs to a signal node0 already
  has, and fewer than threshold members of the selected quorum lack one. node0's mempool is scanned once more right
  before `generate`. The per-block MNHF check stays the backstop. `ehf_state()["quiet"]` reports the wait.
- **Key material in logs.** The framework's RPC logger ("BitcoinRPC") records every RPC argument and result; with
  `--tracerpc` it prints them to stdout, and so to `run.log` and the journal. The driver turns that logger off for the
  calls that carry shares or sks (`quorum info … true`, `bls fromsecret`), and no driver error repeats a share or sk.
  `testkit-keys` gets the operator-key seed as a 0600 file path (`--seed-file`) and the shares on stdin, never on
  its argv. The recorder's own
  calls with an operator sk (for example `protx update_service`) should run under `driver.quiet_rpc_log()` too.
