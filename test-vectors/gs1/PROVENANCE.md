# GS1 signer recordings (dashd v24.0.0, regtest)

- **Network:** regtest, under `-mocktime`; ports below 32768.
- **Recorder:** `contrib/node/gs1-transcripts/capture.py`, run with `python3 -I`
  from the venv the GV1b reproduction steps build.
- **Core version:** dashd v24.0.0 release,
  `~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd`, binary SHA256
  `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`.
- **Bootstrap:** the frozen GV1b chain,
  `dashd-governance/tests/fixtures/dashd-v24.0.0/bootstrap.json.gz` (179 blocks),
  then `G.setup.json.gz`, the blocks `--build-setup` added. Every case replays
  both into a fresh wallet-disabled release node before its transcript starts.
- **Record grammar:** GV1b's, as
  `dashd-governance/tests/fixtures/dashd-v24.0.0/PROVENANCE.md` defines it, plus
  four records:
  - `operator LABEL PROTX TXID:N`: the recording masternode's label, its
    ProRegTx hash and its collateral outpoint;
  - `expect-govobj N HEX` and `expect-govobjvote N HEX`: the message on the line
    before it, sent to peer N, is the recording masternode's own object or vote.
    Replays must emit these bytes;
  - `restart T` replaces GV1b's bare `restart`: the node stops and starts again
    on the same datadir and arguments, except `-mocktime=T`. G7 restarts at the
    current mock time, so no clock jump crosses the restart; G6-ibd restarts
    three days ahead, into IBD. The `time T` record that follows repeats T.

## Keys

- **Label scheme (Q4).** Each key is derived from a fixed label: the scalar is
  `sha256(label)` as a big-endian integer, reduced modulo the curve order (the
  BLS12-381 group order for operator keys, secp256k1's for the others).
  - `gs1-operator-0`: the recording masternode's operator key (ProTx service
    `127.0.0.1:29440`). Its node runs with this key.
  - `gs1-operator-1`: the second masternode (service `127.0.0.1:29442`,
    no node). It signs every trigger the recording masternode did not build:
    - G2: the trigger identical to M's plan, delivered first;
    - G3: the identical trigger for the second cycle (Core does not check that
      its signer is the projected payee, and operator-1 is not shown to be);
    - G4: the competing, the not-yet-mature (next cycle) and the outdated trigger;
    - G6: the live competing trigger for the next cycle, delivered while M is
      not READY;
    - G8: the identical trigger M votes NO on before the proposal is approved;
    - G10: the competing trigger;
    - G11: the competing trigger after the first reorg and the late one after
      the second;
    - G12: the trigger delivered between tips h-1 and h.
  - `gs1-operator-2`: the operator key G6 and G10 move the recording
    masternode to (ProUpRegTx).
  - `gs1-owner-N`, `gs1-voting-N`, `gs1-payout-N`, `gs1-collateral-N`,
    `gs1-change-N`: owner, voting, payout, collateral and change keys of
    masternode N; `gs1-change-proposals` and `gs1-payee-*` for the proposals.
- **Funding.** The two 1,000 DASH collaterals, the proposal collaterals and their
  fees spend the bootstrap's coinbase outputs (blocks 1-179) to
  `yYdShjQSptFKitYLksFEUSwHe4hnbar5rf`. Its key is Core's public test-framework value
  `PRIV_KEYS[0]`, `test/functional/test_framework/test_node.py:185` at Core v24.0.0. The recorder
  reads it from that file at run time (`--core`, a Dash Core git checkout with
  the `v24.0.0` tag), checks that it derives the address above and holds it in
  memory only. These fixtures carry its public key and signatures (in the
  setup blocks) and nothing else derived from it. Every block the setup and the
  cases mine pays its coinbase to `OP_TRUE`. The G6/G10 ProUpRegTx fee spends
  the recording masternode's registration change output (`gs1-change-0`).

## Replay contract

- **Relay batching.** dashd announces governance objects and votes from a
  relay task that runs every 1 s of wall time on regtest
  (`governance/net_governance.cpp:69-77`). The recorder waits 1.5 s of
  quiet after each tip, but under load an item can still move into another
  `inv` or past the next record. A replay compares the announced inventory,
  the fetched `getdata` items and the signer's `govobj`/`govobjvote` messages as
  a multiset per checkpoint segment (the records between two `check gobject`
  lines), not `inv` message boundaries or positions inside a segment. `--check`
  compares the same way. Inside a segment that spans several tips, the multiset
  does not pin the tip a signer message came at; the decision lines, assigned
  by the log-alignment rule below, pin it.
- **Log alignment.** `log` records are collected when a checkpoint runs and at
  shutdown or restart, not interleaved with messages. Within them the signer
  for tip h (`ActiveContext`, registered first, `init.cpp:2167,2194`) logs
  before `CGovernanceManager::UpdatedBlockTip -- nCachedBlockHeight: h`. Assign
  decision lines by position, not by height value: the lines after one
  `nCachedBlockHeight:` line, up to and including the next, belong to the tip
  that next line reports. Startup after a restart can log the same height twice
  (G7, 211), the heights go backwards at reorgs (G11), and G6-ibd logs no such
  line (no decision line is expected there).
- **Decision lines.** The signer lines a replay compares, per tip:
  `CreateSuperblockCandidate`, `CreateGovernanceTrigger --`,
  `VoteGovernanceTriggers --` and `VoteFundingTrigger --`.

## What each case pins

Every claim not listed below shows in the signer's own messages (`expect-*`)
or in the checkpoint JSON. These claims show only in the decision lines,
because a signer missing the named check would send the same bytes:

| Case | Claim | Decision lines the replay must match |
|---|---|---|
| G6 | outside the window (tips 220-225, M not the payee) | `CreateGovernanceTrigger --`: none from 220 to 229 (`signing.cpp:42-44` returns first); a signer without the gate logs `we are not the payee, skipping` at 221-225. Tip 226, M's payee tip outside the window, shows in messages: no `expect-govobj` |
| G6 | not READY (227-231, with operator-1's live trigger for 240) | `VoteGovernanceTriggers --` / `VoteFundingTrigger --`: none (`signing.cpp:184` returns silently); a signer without the gate logs `Voting NO-FUNDING for trigger:<live> failed` at 227-231, and its vote, for a removed masternode, is rejected (`object.cpp:461-467`). `CreateGovernanceTrigger -- we are not the payee, skipping` at 230 and 231 is Core's: the plan and payee check do not test READY |
| G7 | the restart restores voted-yes | `VoteGovernanceTriggers -- Not voting YES-FUNDING for trigger:<M's>, we voted yes for it already` at the first tip after the restart (212). From 212 through M's payee tip 218, each tip logs `VoteGovernanceTriggers -- Not voting NO-FUNDING for trigger:<M's>, we voted yes for it already` (`signing.cpp:251-256`) and no `CreateSuperblockCandidate`, `CreateGovernanceTrigger --` or YES-FUNDING line (`signing.cpp:45` returns first). A signer without the record check re-sends the stored YES, which dashd drops (same vote; `object.cpp:443-456`) |
| G8 | no YES after M's NO (the decision; the missing vote also shows in messages after the clock step) | `VoteGovernanceTriggers -- Not voting YES-FUNDING for trigger:<h>, we voted no for it already` at each window tip |
| G10 | no votes at the key-change tip | no `VoteGovernanceTriggers --` / `VoteFundingTrigger --` line at h; a stale-key NO on the competing trigger is rejected (`object.cpp:461-467`) and logs `failed`. `CreateGovernanceTrigger -- we are not the payee, skipping` at h is Core's (null proTxHash) |
| G11 | no re-creation after the reorg to 211 (voted-yes kept) | no `CreateGovernanceTrigger --` or `Not voting YES-FUNDING` line at the reconnected tips 212-219 (`signing.cpp:45` returns first); a signer that lost voted-yes logs `Not voting YES-FUNDING for trigger:<M's>, we voted yes for it already`. After the second reorg, below the superblock (voted-yes was reset at 220), that line at 219 is Core's restore path |

- **G6 unsynced** (tip 210) shows in the checkpoint: relay is silent while
  unsynced (`governance.cpp:112-140`), but a trigger built anyway would be in
  `unsynced-window-tip`'s object list.
- **G6-ibd** isolates IBD. In IBD dashd requests no governance inventory
  (`net_processing.cpp:4590-4593`), so the proposal and M's vote arrive out of
  IBD; a `restart` three days ahead puts the node in IBD, its startup Init
  sets READY (`init.cpp:2539`, `active/masternode.cpp:112-173`), and `mnsync
  next` reaches FINISHED, which a tip in IBD keeps (`masternode/sync.cpp:126-127`
  return before the IBD reset at 134-141). At the window tip, where M is the
  first payee, only `active/context.cpp:95-96` stops the signer: a signer
  without it would add a trigger, which the `ibd-window-tip` checkpoint would
  list.
- **G5** pins both orderings. The equal-yes E pair (`--build-setup` suffixes
  the names until this holds) sorts `ceb1acd131855fff3b20762971dd7f8cae2248bdccfe986f8e531fb26680be1b` first by `UintToArith256`
  (Core's order, `governance.cpp:1252-1256`; it fills the budget and the other
  does not fit) and `a43a1ff684cdaabb55e2d3a1b23c3ae0552d2386a41422dd8c4996b1495781df` first by memcmp, so a memcmp sort pays the
  other one. C `60f8c154a37d7caea10d8e95d866df50ed3f6344a83865488ee22f0d7cf4f324` has two yes votes and is filled first, but its hash is
  below the arith winner's, so the final sort (`signing.cpp:113-115`) puts the
  winner before C in the trigger.

## Reproduction

```sh
DASHVERSION=24.0.0 python3 -I contrib/setup-dashd.py
# venv: as in dashd-governance/tests/fixtures/dashd-v24.0.0/PROVENANCE.md
VENV/bin/python3 -I contrib/node/gs1-transcripts/capture.py --build-setup --core ~/workspace/dash
VENV/bin/python3 -I contrib/node/gs1-transcripts/capture.py
VENV/bin/python3 -I contrib/node/gs1-transcripts/capture.py --check
VENV/bin/python3 -I contrib/node/gs1-transcripts/capture.py --selftest
```

`--build-setup` is the only step that reads the funding key. A rebuilt setup
changes every hash and must be followed by every case. `--check` re-records
G1 from the frozen setup and compares it with the stored file, excluding
runtime ports, the version nonce and MNAUTH challenge, ping nonces, log wall
timestamps and `verificationprogress`, and comparing relayed items per
checkpoint segment (see Replay contract).

## Files

| File | Bytes | SHA256 |
|---|---|---|
| `G.setup.json.gz` | 8641 | `d7bafd0c1ab023fd2d7897ee5fca0f6777080af5f59456091515d8903239c3be` |
| `G1.txt` | 241980 | `45ed22e8ea46b7f7d9377831612bcb013fc6825370ea4e3e2e7d47898436f3fa` |
| `G10.txt` | 39389 | `fb9a502950a36abfe54d4340234b48d1576fb289a089ce37d8c39b01764db346` |
| `G11.txt` | 148990 | `d9354fc57013ddef1c9725c2558afce43e3725c68f6ce88eedc30fc94c230b4c` |
| `G12.txt` | 88833 | `af5393850c12e732b5b314ed03081abcf0695df53bef55d058dd3a0c60eb921d` |
| `G2.txt` | 39608 | `12d02f33a2356bf9ff678c149168eb1c7834819ff15e0c468b3f0bc8610f7cdc` |
| `G3.txt` | 171220 | `fb27b68fb949150aea861dffa9b75d53252282b3a806fa741d7d38b73a28ef03` |
| `G4.txt` | 123028 | `215d6be8230ab468daec4bce912fb6ba22848097f554603eec5e5706d6b4888d` |
| `G5.txt` | 50046 | `25f0c115d5eae90a91ec2dde012d9ded7a2210e40925c2717a76f9814deaa771` |
| `G6-ibd.txt` | 31443 | `60ef67dfd2fe27b8a3e4c85ac82595bcc7082de6b1161b9c9b0246d29d6ba275` |
| `G6.txt` | 170136 | `8040d4e6babbc1a3d6c1add458c1f85e4d92d6c47da84723bd42e05f8f36fdd3` |
| `G7.txt` | 85445 | `f829cc71309a27a5c2366b9fac0b2bcd04d741fe61cd7369e6192d533c248633` |
| `G8.txt` | 210308 | `b64a60a6bb122631ba7f345de509ec497a38c8000d6454d2ccf9b98a47393bd6` |
| `G9.txt` | 48888 | `e989f8b8e9690da05c1b9efc85accddc85daa47cc7709271b900e8fac696194d` |
