#!/usr/bin/env python3
"""PF1: turn `perf script` output into folded stacks, subsystem tables and inclusive-function tables.

    perf script -i perf.data -F comm,tid,time,event,period,ip,sym,dso --no-inline \
        | perf_fold.py --out DIR [--window T0 T1] [--event instructions:u]

Writes, per event, into DIR:
    <event>.folded           stack;stack;... weight   (FlameGraph input, root first)
    <event>.subsystems.tsv   subsystem  weight  share   (owner view: malloc/memcpy/libc leaves charged to the caller's subsystem)
    <event>.subsystems_self.tsv   the same with libc and allocator leaves counted as their own subsystem
    <event>.threads.tsv      thread-group  weight  share
    <event>.inclusive.tsv    function-group  inclusive_weight  share
    <event>.top.tsv          symbol  self_weight  share  (leaf symbols)
Weights are the sample period: instructions for instructions:u, nanoseconds of on-CPU time for cpu-clock.

--window takes seconds on perf's clock (perf record -k monotonic). Samples outside it are dropped.
"""

import argparse
import collections
import os
import re
import sys

HEADER = re.compile(r"^(?P<comm>\S.*?)\s+(?P<tid>\d+)\s+(?P<time>\d+\.\d+):\s+(?P<period>\d+)?\s*(?P<event>\S+?):\s*$")
FRAME = re.compile(r"^\s+(?P<ip>[0-9a-f]+)\s+(?P<sym>.*?)\s*(?:\((?P<dso>[^)]*)\))?\s*$")

# Subsystem of a sample = the subsystem of its deepest (leaf-most) frame that matches a rule below. A leaf in
# LevelDB called from the EvoDB code is therefore "leveldb"; the inclusive table (INCLUSIVE) is what answers
# "how much does the EvoDB path cost in total". Rules are tried in order for each frame.
SUBSYSTEMS = [
    ("script-verify", r"VerifyScript|EvalScript|CScriptCheck|CheckInputScripts|SignatureHash|TransactionSignatureChecker|"
                      r"ExecuteWitnessScript|CheckECDSASignature|CheckSig|VerifyWitnessProgram|SigCache|CachingTransaction|"
                      r"^secp256k1_|GenericTransactionSignatureChecker|CScript::|CScriptNum"),
    ("x11", r"HashX11|^hash_x11|^sph_|^compress_big|^jh_|^groestl|^skein_|^blake_|^bmw|^shavite|^luffa|^cubehash|"
            r"^simd_|^echo_|^hamsi|^fugue|^keccak|^shabal|^whirlpool|^aes_"),
    ("sha256", r"sha256|SHA256|CSHA256|CHash256|Transform_|sha256d|^_ZN7CSHA256|^Lloop\d+_30$|^Ltop|^Lend"),
    ("ripemd160/hash160", r"ripemd|RIPEMD|CHash160|hash160"),
    ("bls", r"^bls::|bls_|^bn_|^bn\w*_|^fp\w*_|^ep\w*_|^pp\w*_|^g1_|^g2_|^relic|^core_|^md_|^pc_|CBLS|blst|^gmp|^__gmpn|mpn_|"
            r"\bmpz_|\bgmp|^dv_|^util_|^lzcnt|^arch_lzcnt|^get_bits|^L(loop|top|end)|^L\w+_\d+$|^fb_|^ed_|^eb_|^pc_|^rand_|^conf_"),
    ("llmq", r"llmq::|CQuorum|CChainLocks|CChainLockSig|CInstantSend|CSigningManager|CFinalCommitment|VerifyChainLock|"
             r"CRecoveredSig|CSigSharesManager|CDKG|CQuorumManager|ChainLock"),
    ("evodb/mn-lists", r"CEvoDB|CDeterministicMN|CSimplifiedMNList|BuildSimplifiedMNList|CDeterministicMNList|"
                       r"CDeterministicMNManager|CMNHFManager|CCreditPoolManager|CMasternodePayments|CGovernance|"
                       r"CMasternodeMetaMan|CMasternodeSync|CalcMerkleRoot.*MN|MasternodeList|CMnEngine|immer::"),
    ("special-tx", r"ProcessSpecialTx|CheckSpecialTx|CheckProRegTx|CheckProUpServTx|CheckProUpRegTx|CheckProUpRevTx|"
                   r"CheckCbTx|CCbTx|CheckAssetLockTx|CheckAssetUnlockTx|CheckMNHFTx|CheckCreditPool|GetTxPayload|"
                   r"CheckLLMQCommitment|CheckSpecialTxInner"),
    ("leveldb", r"leveldb::|snappy|crc32c|CDBWrapper|CDBBatch|CDBIterator|dbwrapper|CCoinsViewDB"),
    ("coins-cache", r"CCoinsViewCache|CCoinsMap|CCoinsViewBacked|AddCoins|SpendCoin|FetchCoin|AccessCoin|HaveCoin|"
                    r"UpdateCoins|^Coin::|CCoinsView|CCoinsCacheEntry|CoinsCache|Coin::"),
    ("block-io", r"ReadBlockFromDisk|UndoReadFromDisk|WriteUndoDataForBlock|SaveBlockToDisk|FlushUndoFile|FlushBlockFile|"
                 r"FlushStateToDisk.*File|OpenBlockFile|BufferedFile|LoadExternalBlockFile|FindBlockPos|FindUndoPos|"
                 r"^fsync|^fdatasync|__libc_pread|__libc_pwrite|_IO_f|^fwrite|^fread|^write$|^read$|^pread|^pwrite|"
                 r"^__libc_(read|write)|AutoFile|CAutoFile|FileCommit"),
    ("tx-block-codec", r"Unserialize|Serialize|DataStream|CDataStream|CTransaction::|CMutableTransaction|ComputeHash|"
                       r"CBlock::|CBlockHeader|CheckBlock|CheckTransaction|CheckBlockHeader|GetMerkleRoot|BlockMerkle|"
                       r"BlockWitnessMerkle|ComputeMerkleRoot|prevector"),
    ("txindex/indexes", r"BaseIndex|TxIndex|BlockFilterIndex|SpentIndex|AddressIndex|TimestampIndex|CTxIndex"),
    ("logging", r"LogPrintf|LogPrintStr|BCLog|FormatISO8601|FormatTime|tfm::|strprintf|LogPrint"),
    ("memory/libc", r"^malloc$|^free$|^calloc$|^realloc$|^_int_malloc|^_int_free|^operator new|^operator delete|^cfree|"
                    r"^posix_memalign|^__libc_malloc|^__libc_free|^memcpy|^memset|^memmove|^__memcpy|^__memset|^__memmove|"
                    r"^memcmp|^__memcmp|^_mid_memalign|^tcache|^arena|^__GI_|^_IO_|^__libc_|^_dl_|^pthread_|^__pthread|"
                    r"^__lll|^futex|^syscall|^clock_|^__clock|^nanosleep|^epoll"),
    ("validation", r"ConnectBlock|ConnectTip|ActivateBestChain|AcceptBlock|ProcessNewBlock|CChainState|Chainstate|"
                   r"ContextualCheck|CheckInputs|CheckTxInputs|GetBlockSubsidy|GetBlockValue|IsBlockValueValid|"
                   r"IsBlockPayeeValid|CheckBlockSigops|GetLegacySigOpCount|GetP2SHSigOpCount|DisconnectTip|"
                   r"UpdateTip|FlushStateToDisk|CChain::|CBlockIndex|BlockManager|ChainstateManager"),
    ("mempool", r"CTxMemPool|AcceptToMemoryPool|MemPoolAccept|ATMP|CTxMemPoolEntry|setEntries|txmempool|"
                r"CBlockPolicyEstimator|BlockAssembler|CreateNewBlock|TestBlockValidity|UpdateForDescendants|"
                r"CalculateMemPoolAncestors|CalculateDescendants|TrimToSize|removeForBlock"),
    ("rpc/http/net", r"HTTPRequest|http_|evhttp|^event_|^evbuffer|bufferevent|JSONRPC|CRPCTable|RPCHelpMan|UniValue|"
                     r"CConnman|ThreadSocketHandler|ThreadMessageHandler|ProcessMessage|PeerManager|rpc|^getblock|^getrawtr|^submit"),
]
SUBSYSTEMS = [(n, re.compile(p)) for n, p in SUBSYSTEMS]

# Inclusive groups: share of samples that have any frame matching the pattern (a sample counts once per group).
INCLUSIVE = [
    ("ConnectBlock (whole)", r"ChainstateManager::ConnectBlock|CChainState::ConnectBlock|Chainstate::ConnectBlock|\bConnectBlock\b"),
    ("ConnectTip", r"ConnectTip"),
    ("ActivateBestChain", r"ActivateBestChain"),
    ("CheckInputScripts / script checks", r"CheckInputScripts|CScriptCheck::operator|ThreadScriptCheck|CCheckQueue.*Loop"),
    ("VerifyScript", r"\bVerifyScript\b"),
    ("ProcessSpecialTxsInBlock", r"ProcessSpecialTxsInBlock"),
    ("CheckSpecialTx", r"CheckSpecialTx"),
    ("CDeterministicMNManager::ProcessBlock", r"CDeterministicMNManager::ProcessBlock|BuildNewListFromBlock"),
    ("CheckCbTx", r"CheckCbTx"),
    ("CChainLocks / VerifyChainLock", r"VerifyChainLock|CChainLockSig"),
    ("llmq::* (any)", r"llmq::"),
    ("CMNHFManager", r"CMNHFManager"),
    ("CCreditPoolManager", r"CCreditPoolManager"),
    ("IsBlockValueValid / payee", r"IsBlockValueValid|IsBlockPayeeValid|GetMasternodePayment|CMasternodePayments"),
    ("CheckBlock (context-free)", r"\bCheckBlock\b"),
    ("CEvoDB", r"CEvoDB"),
    ("BLS (any bls::* / relic)", r"^bls::|bls::"),
    ("UpdateCoins / AddCoins / SpendCoin", r"UpdateCoins|AddCoins|SpendCoin"),
    ("CCoinsViewCache::FetchCoin", r"FetchCoin"),
    ("FlushStateToDisk", r"FlushStateToDisk"),
    ("LevelDB (any)", r"leveldb::"),
    ("UpdatedBlockTip / notifications", r"UpdatedBlockTip|SyncTransaction|BlockConnected|CallFunctionInValidationInterfaceQueue|CMainSignals"),
    ("ReadBlockFromDisk", r"ReadBlockFromDisk"),
    ("UndoReadFromDisk / WriteUndo", r"UndoReadFromDisk|WriteUndoDataForBlock"),
    ("AcceptToMemoryPool", r"AcceptToMemoryPool|MemPoolAccept"),
    ("CreateNewBlock", r"CreateNewBlock"),
    ("TestBlockValidity", r"TestBlockValidity"),
    ("getblocktemplate RPC", r"getblocktemplate"),
    ("LogPrintf", r"LogPrintStr|LogPrintf|BCLog"),
    ("LoadBlockIndex / init", r"LoadBlockIndex|LoadBlockIndexDB|AppInitMain|InitBlockIndex|LoadChainTip|VerifyDB"),
    ("startup: RecalculateAndRepairDiffs (MN-list check)", r"RecalculateAndRepairDiffs"),
    ("startup: VerifySnapshotPair / IsEqual", r"VerifySnapshotPair|CDeterministicMNList::IsEqual"),
    ("startup: CDeterministicMNManager::Init / UpgradeDB", r"CDeterministicMNManager::(Init|Upgrade|Migrate|Load)|UpgradeDBIfNeeded"),
    ("startup: LoadBlockIndexGuts", r"LoadBlockIndexGuts"),
    ("startup: VerifyDB (-checkblocks)", r"\bVerifyDB\b|CVerifyDB"),
    ("startup: LoadChainTip / ReplayBlocks / RollforwardBlock", r"LoadChainTip|ReplayBlocks|RollforwardBlock|RewindBlockIndex"),
    ("CDeterministicMNList::AddMN / UpdateMN / ApplyDiff", r"CDeterministicMNList::(AddMN|UpdateMN|ApplyDiff|RemoveMN)"),
]
INCLUSIVE = [(n, re.compile(p)) for n, p in INCLUSIVE]

# "Business" anchors: the nearest-to-leaf frame matching one of these says which node activity a sample belongs to.
# Order matters only for frames matching several (first listed wins within one frame).
ANCHORS = [
    ("CheckInputScripts (script verification)", r"CheckInputScripts|CScriptCheck::operator|BlsCheck::operator"),
    ("ProcessSpecialTxsInBlock: quorum commitments (m_qblockman)", r"CQuorumBlockProcessor|ProcessCommitment|CFinalCommitment"),
    ("ProcessSpecialTxsInBlock: MN list (m_dmnman)", r"CDeterministicMNManager|BuildNewListFromBlock|CDeterministicMNList|CDeterministicMNListDiff"),
    ("ProcessSpecialTxsInBlock: CbTx merkle roots / chainlock", r"CalcCbTxMerkleRoot|CheckCbTx|CSimplifiedMNList|VerifyChainLock|CChainLockSig|ChainLock"),
    ("ProcessSpecialTxsInBlock: other", r"ProcessSpecialTxsInBlock|CMNHFManager|CCreditPoolManager|CheckSpecialTx"),
    ("UpdateCoins / AddCoins / SpendCoin", r"UpdateCoins|AddCoins|SpendCoin"),
    ("CheckTxInputs / CheckInputs (coin lookups)", r"CheckTxInputs|Consensus::CheckTxInputs|CheckInputs|HaveInputs|GetValueIn"),
    ("IsBlockValueValid / IsBlockPayeeValid / subsidy", r"IsBlockValueValid|IsBlockPayeeValid|GetBlockSubsidy|CMasternodePayments|GetMasternodePayment"),
    ("IS filter / sigops / BIP30 checks in ConnectBlock", r"CheckBlockSigops|GetLegacySigOpCount|GetP2SHSigOpCount|HaveCoin|CInstantSendManager|IsConflicted"),
    ("Write undo / block files", r"WriteUndoDataForBlock|SaveBlockToDisk|FlushUndoFile|FlushBlockFile|FindUndoPos|FindBlockPos"),
    ("FlushStateToDisk (coins/index write)", r"FlushStateToDisk|CCoinsViewCache::Flush|BatchWrite"),
    ("ReadBlockFromDisk (+ PoW check on read)", r"ReadBlockFromDisk|LoadExternalBlockFile|UndoReadFromDisk"),
    ("CheckBlock / CheckBlockHeader (context-free)", r"CheckBlockHeader|\bCheckBlock\b|CheckProofOfWork|CheckTransaction"),
    ("ContextualCheckBlock*/AcceptBlock*/ProcessNewBlock", r"ContextualCheck|AcceptBlock|ProcessNewBlock|BlockMerkleRoot|CheckMerkleRoot"),
    ("UpdateTip / validation signals", r"UpdateTip|UpdatedBlockTip|BlockConnected|SyncTransaction|CMainSignals|CallFunctionInValidationInterfaceQueue"),
    ("LogPrintf", r"LogPrintStr|LogPrintf|BCLog"),
    ("ConnectBlock (other)", r"ConnectBlock"),
    ("ConnectTip/ActivateBestChain (other)", r"ConnectTip|ActivateBestChain"),
    ("AcceptToMemoryPool", r"AcceptToMemoryPool|MemPoolAccept"),
    ("BlockAssembler::CreateNewBlock", r"CreateNewBlock|BlockAssembler"),
    ("TestBlockValidity", r"TestBlockValidity"),
    ("RPC dispatch", r"HTTPReq|JSONRPC|CRPCTable|UniValue|rpc|http_"),
    ("startup: LoadBlockIndex/VerifyDB/init", r"LoadBlockIndex|VerifyDB|AppInitMain|LoadChainTip|CDeterministicMNManager::Init|CEvoDB"),
]
ANCHORS = [(n, re.compile(p)) for n, p in ANCHORS]

THREADS = [
    # Dash prefixes thread names with the program name ("b-scriptch.3" in Core, "d-scriptch.3" here); the script
    # check workers also run the BLS check queue (CCheckQueue<llmq::utils::BlsCheck>), see the folded stacks.
    ("script/BLS check workers (scriptch.N)", r"scriptch"),
    ("block import / ActivateBestChain (loadblk)", r"loadblk|import"),
    ("message handler (msghand)", r"msghand"),
    ("http/rpc workers", r"httpworker|http"),
    ("scheduler", r"scheduler"),
    ("shutdown", r"shutoff|shutdown"),
    ("index threads", r"index"),
    ("LLMQ/DKG/sigs threads", r"llmq|dkg|sigs|quorum|isman|chainlock|cl-"),
    ("BLS worker pool", r"bls"),
    ("init / main thread", r"^dashd$"),
]
THREADS = [(n, re.compile(p)) for n, p in THREADS]


BG_GROUP = "unnamed background thread (LevelDB compaction)"


def thread_group(comm):
    for name, rx in THREADS:
        if rx.search(comm):
            return name
    return "other: " + comm


_SYM = {}


def sym_info(sym):
    """Per-symbol classification, memoised: (subsystem or None, anchor or None, set of inclusive groups)."""
    r = _SYM.get(sym)
    if r is None:
        sub = next((n for n, rx in SUBSYSTEMS if rx.search(sym)), None)
        anc = next((n for n, rx in ANCHORS if rx.search(sym)), None)
        inc = frozenset(n for n, rx in INCLUSIVE if rx.search(sym))
        r = _SYM[sym] = (sub, anc, inc)
    return r


def anchor_of(frames):
    for sym, dso in frames:
        anc = sym_info(sym)[1]
        if anc:
            return anc
    return "(no anchor)"


def subsystem_of(frames, transparent=False):
    """frames: leaf first. Returns the subsystem of the leaf-most frame matching a rule.

    With `transparent`, frames of the memory/libc subsystem (malloc, free, memcpy, ...) are skipped when a
    caller frame matches another subsystem: allocation and copying are charged to the code that asks for them.
    """
    for sym, dso in frames:
        name = sym_info(sym)[0]
        if name and not (transparent and name == "memory/libc"):
            return name
    # No symbol matched: fall back to the leaf's dso.
    sym, dso = frames[0] if frames else ("", "")
    base = os.path.basename(dso or "")
    if base.startswith("libc") or base.startswith("ld-"):
        return "memory/libc"
    if base.startswith("libstdc++"):
        return "memory/libc"
    if base.startswith("[kernel") or base == "[vdso]":
        return "kernel"
    return "other"


def parse(stream):
    cur = None
    for line in stream:
        line = line.rstrip("\n")
        if not line:
            if cur is not None:
                yield cur
                cur = None
            continue
        if line[0] not in " \t":
            m = HEADER.match(line)
            if not m:
                cur = None
                continue
            ev = m["event"]
            ev = "instructions:u" if ev.startswith("instructions") else "cpu-clock" if ev.startswith("cpu-clock") else ev
            cur = {"comm": m["comm"], "tid": int(m["tid"]), "time": float(m["time"]),
                   "period": int(m["period"] or 1), "event": ev, "frames": []}
        elif cur is not None:
            m = FRAME.match(line)
            if m:
                sym = re.sub(r"\+0x[0-9a-f]+$", "", m["sym"])
                cur["frames"].append((sym, m["dso"] or ""))
    if cur is not None:
        yield cur


def short(sym):
    """Readable frame name for flamegraphs: drop the argument list, `const` and `[clone ...]` suffixes."""
    sym = re.sub(r"( \[clone [^\]]*\])+$", "", sym)
    sym = re.sub(r" const$", "", sym)
    if sym.endswith(")"):
        depth = 0
        for i in range(len(sym) - 1, -1, -1):
            depth += (sym[i] == ")") - (sym[i] == "(")
            if depth == 0:
                if i > 0:
                    sym = sym[:i]
                break
    return sym.replace(";", ":") or "[unknown]"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--window", nargs=2, type=float, metavar=("T0", "T1"))
    ap.add_argument("--event", action="append", help="only these events (default: all seen)")
    ap.add_argument("--input", default="-")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)

    folded = collections.defaultdict(collections.Counter)
    subs = collections.defaultdict(collections.Counter)
    selfsubs = collections.defaultdict(collections.Counter)
    anchors = collections.defaultdict(collections.Counter)
    anchor_sub = collections.defaultdict(collections.Counter)
    sub_syms = collections.defaultdict(collections.Counter)
    thr = collections.defaultdict(collections.Counter)
    incl = collections.defaultdict(collections.Counter)
    leaf = collections.defaultdict(collections.Counter)
    total = collections.Counter()
    total_ex = collections.Counter()
    subs_ex = collections.defaultdict(collections.Counter)
    n = collections.Counter()
    first = last = None
    src = sys.stdin if a.input == "-" else open(a.input, errors="replace")
    for s in parse(src):
        ev = s["event"]
        if a.event and ev not in a.event:
            continue
        if a.window and not (a.window[0] <= s["time"] <= a.window[1]):
            continue
        w = s["period"]  # instructions for instructions:u, nanoseconds of CPU time for cpu-clock
        fr = s["frames"] or [("[no-stack]", "")]
        total[ev] += w
        n[ev] += 1
        first = s["time"] if first is None else min(first, s["time"])
        last = s["time"] if last is None else max(last, s["time"])
        g = thread_group(s["comm"])
        if g == "init / main thread" and len(fr) > 1 and fr[-1][0].endswith("clone3"):
            # an unnamed thread started by the main thread (it keeps the process name): LevelDB's background
            # compaction thread. A fixed per-run cost after opening a cloned datadir, not a per-block cost.
            g = BG_GROUP
        subs[ev][subsystem_of(fr, transparent=True)] += w
        selfsubs[ev][subsystem_of(fr)] += w
        sub = subsystem_of(fr, transparent=True)
        anc = anchor_of(fr)
        anchors[ev][anc] += w
        anchor_sub[ev][(anc, sub)] += w
        sub_syms[ev][(sub, fr[0][0])] += w
        thr[ev][g] += w
        if g != BG_GROUP:
            total_ex[ev] += w
            subs_ex[ev][sub] += w
        leaf[ev][fr[0][0]] += w
        names = [short(x[0]) for x in fr]
        folded[ev][";".join([g.replace(";", ":")] + names[::-1])] += w
        seen = set()
        for x in fr:
            seen |= sym_info(x[0])[2]
        for name in seen:
            incl[ev][name] += w

    for ev in total:
        base = os.path.join(a.out, ev.replace(":", "_").replace("/", "_"))
        with open(base + ".folded", "w") as f:
            for k, v in folded[ev].items():
                f.write(f"{k} {v}\n")
        for tag, table in (("subsystems", subs), ("subsystems_self", selfsubs), ("threads", thr), ("inclusive", incl), ("top", leaf)):
            with open(f"{base}.{tag}.tsv", "w") as f:
                for k, v in table[ev].most_common(60 if tag == "top" else None):
                    f.write(f"{k}\t{v}\t{100.0 * v / total[ev]:.2f}\n")
        with open(base + ".anchors.tsv", "w") as f:
            for k, v in anchors[ev].most_common():
                f.write(f"{k}\t{v}\t{100.0 * v / total[ev]:.2f}\n")
        with open(base + ".anchor_x_subsystem.tsv", "w") as f:
            for (anc, sub), v in anchor_sub[ev].most_common():
                f.write(f"{anc}\t{sub}\t{v}\t{100.0 * v / total[ev]:.2f}\n")
        with open(base + ".subsystem_symbols.tsv", "w") as f:
            per = collections.defaultdict(list)
            for (sub, sym), v in sub_syms[ev].most_common():
                if len(per[sub]) < 8:
                    per[sub].append((sym, v))
            for sub, items in per.items():
                for sym, v in items:
                    f.write(f"{sub}\t{sym[:140]}\t{v}\t{100.0 * v / total[ev]:.2f}\n")
        with open(base + ".subsystems_ex_bg.tsv", "w") as f:
            for k, v in subs_ex[ev].most_common():
                f.write(f"{k}\t{v}\t{100.0 * v / max(total_ex[ev], 1):.2f}\n")
        with open(base + ".total_ex_bg", "w") as f:
            f.write(f"{total_ex[ev]}\n")
        with open(base + ".total", "w") as f:
            f.write(f"{total[ev]}\t{n[ev]}\t{first}\t{last}\n")
        print(f"{ev}: weight {total[ev]} samples {n[ev]} span {last - first:.2f}s", file=sys.stderr)


if __name__ == "__main__":
    main()
