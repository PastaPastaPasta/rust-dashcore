"""Masternode metrics: defined now, runnable once the Rust node can be a masternode.

All of them need a local regtest masternode network. Two ways to get one:

1. The pre-generated network contrib/setup-dashd.py already downloads
   (dashpay/regtest-blockchain `regtest-mn`: network.json + one datadir per
   node, registered masternodes with BLS operator keys). Start every node on
   localhost with its datadir and -masternodeblsprivkey, -llmqtestparams etc.
   from network.json, then mine to the next DKG window.
2. Dash Core's functional framework (test/functional/test_framework,
   DashTestFramework.set_dash_test_params(num_nodes, masternodes_count, ...)
   with -llmqtestparams=3:2 / -llmqtestinstantsendparams), which already does
   MN registration, quorum formation (mine_quorum / mine_cycle_quorum),
   ChainLock and InstantSend activation. A bench "test" script in that
   framework records the timings below and writes node-bench JSON. Per-node
   binaries are selectable, so a mixed network (k Rust MNs + dashd MNs) uses
   the same script.

In both, the node under test plays (a) one of the masternodes and (b) the
non-MN observer whose view is timed; dashd-only and mixed networks are run
back to back on the same CPU slices.

These definitions are drafts: before the scenarios become runnable (B1b / the
masternode milestone) their end points must match PRD metrics 14-18 exactly -
recovery on the node under test (not an observer), the received ISDLOCK and
CLSIG messages rather than state polled through RPC, dsa -> dsc for CoinJoin
plus the saturated session rate and CPU per session. See README "PRD coverage".
"""

from . import scenario

PENDING = "needs the Rust node to run as a masternode (BLS operator, LLMQ, DKG); method defined, not runnable yet"

DEFS = {
    "mn-dkg": ("DKG round time", {
        "dkg_round_s": "From the block at the DKG start height (quorum height) to the block whose coinbase carries "
                       "the final commitment, on a 5-member llmq_test quorum with threshold 3 "
                       "(-llmqtestparams=5:3), blocks mined on demand as soon as each phase completes (phase "
                       "timing from `quorum dkgstatus`). 10 rounds; median.",
        "dkg_phase_<n>_s": "Per phase (initialize, contribute, complain, justify, commit, finalize) durations.",
    }),
    "mn-recsig": ("Recovered-signature latency", {
        "recsig_ms": "`quorum sign` sent to every member for a fresh (id, msgHash) → `quorum hasrecsig` true on "
                     "the observer. 200 requests; median and p99.",
    }),
    "mn-islock": ("InstantSend lock latency", {
        "islock_ms": "sendrawtransaction on the observer → its getrawtransaction(txid, true).instantlock == true "
                     "(polled at 5 ms; also the islock ZMQ notification when available). 200 independent txs; "
                     "median and p99.",
    }),
    "mn-chainlock": ("ChainLock creation latency", {
        "chainlock_ms": "generatetoaddress(1) on the observer → getbestchainlock().blockhash equals the new block. "
                        "50 blocks; median and p99.",
    }),
    "mn-coinjoin": ("CoinJoin round servicing", {
        "coinjoin_round_s": "Mixing wallets (one CoinJoin client per node, 3 participants per session) start "
                            "mixing a denomination; time from the first dsa to the dstx being broadcast by the "
                            "servicing masternode. 20 rounds; median.",
    }),
}


def _pending(ctx):  # never called: the registry reports PENDING first
    raise SystemExit(PENDING)


for _name, (_title, _metrics) in DEFS.items():
    scenario(f"{_name}-regtest", "masternode", _title, metrics=_metrics, pending=PENDING)(_pending)
