// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

// BS5 chain-params dump: Core's own CChainParams for one chain and argument
// list, as JSON on stdout. Built against a pinned Core build tree (the oracle
// shim branches) by build.sh; links only Core's libraries.
//
// usage: chainparams_dump <chain> [args...]
//
// build.sh compiles in the pin and commit (BS5_PIN, BS5_COMMIT); every output
// carries them as "provenance" so run.py can check them against pins.txt and
// the build MANIFEST.
//
// The "params" object is the O1 oracle's ChainParamsJson() verbatim (copied
// from src/test/oracle/oracle.cpp), so run.py can check it equals
// oracle_chainparams_json from the pinned archive. "ext" holds the CChainParams
// members ChainParamsJson() leaves out. Selection follows oracle_set_network():
// ClearArgs, SetupChainParamsOptions, ParseParameters, SelectParams. The
// additions are the init.cpp options BS5 reads: -fastprune (regtest's
// nPruneAfterHeight), and -sporkaddr and -minsporkkeys (v23 only), which
// AppInitMain applies after the chain params ("spork", below).

#include <chainparams.h>
#include <chainparamsbase.h>
#include <key_io.h>
#include <script/standard.h>
#include <clientversion.h>
#include <deploymentinfo.h>
#include <llmq/params.h>
#include <streams.h>
#include <univalue.h>
#include <util/strencodings.h>
#include <util/system.h>
#include <util/translation.h>
#include <version.h>

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <exception>
#include <set>
#include <functional>
#include <string>
#include <vector>

const std::function<std::string(const char*)> G_TRANSLATION_FUN = nullptr;

#if !defined(BS5_PIN) || !defined(BS5_COMMIT)
#error "build.sh defines BS5_PIN and BS5_COMMIT"
#endif

namespace {

UniValue HashHex(const uint256& hash) { return UniValue{hash.GetHex()}; }

// Verbatim from src/test/oracle/oracle.cpp (node/oracle-shim-v23.1, -v24.0).
UniValue ChainParamsJson(const CChainParams& chain)
{
    const Consensus::Params& c = chain.GetConsensus();
    UniValue consensus{UniValue::VOBJ};
    consensus.pushKV("hashGenesisBlock", HashHex(c.hashGenesisBlock));
    consensus.pushKV("hashDevnetGenesisBlock", HashHex(c.hashDevnetGenesisBlock));
    consensus.pushKV("nSubsidyHalvingInterval", c.nSubsidyHalvingInterval);
    consensus.pushKV("nMasternodePaymentsStartBlock", c.nMasternodePaymentsStartBlock);
    consensus.pushKV("nMasternodePaymentsIncreaseBlock", c.nMasternodePaymentsIncreaseBlock);
    consensus.pushKV("nMasternodePaymentsIncreasePeriod", c.nMasternodePaymentsIncreasePeriod);
    consensus.pushKV("nInstantSendConfirmationsRequired", c.nInstantSendConfirmationsRequired);
    consensus.pushKV("nInstantSendKeepLock", c.nInstantSendKeepLock);
    consensus.pushKV("nBudgetPaymentsStartBlock", c.nBudgetPaymentsStartBlock);
    consensus.pushKV("nBudgetPaymentsCycleBlocks", c.nBudgetPaymentsCycleBlocks);
    consensus.pushKV("nBudgetPaymentsWindowBlocks", c.nBudgetPaymentsWindowBlocks);
    consensus.pushKV("nSuperblockStartBlock", c.nSuperblockStartBlock);
    consensus.pushKV("nSuperblockStartHash", HashHex(c.nSuperblockStartHash));
    consensus.pushKV("nSuperblockCycle", c.nSuperblockCycle);
    consensus.pushKV("nSuperblockMaturityWindow", c.nSuperblockMaturityWindow);
    consensus.pushKV("nGovernanceMinQuorum", c.nGovernanceMinQuorum);
    consensus.pushKV("nGovernanceFilterElements", c.nGovernanceFilterElements);
    consensus.pushKV("nMasternodeMinimumConfirmations", c.nMasternodeMinimumConfirmations);
    consensus.pushKV("BIP34Height", c.BIP34Height);
    consensus.pushKV("BIP34Hash", HashHex(c.BIP34Hash));
    consensus.pushKV("BIP65Height", c.BIP65Height);
    consensus.pushKV("BIP66Height", c.BIP66Height);
    consensus.pushKV("BIP147Height", c.BIP147Height);
    consensus.pushKV("CSVHeight", c.CSVHeight);
    consensus.pushKV("DIP0001Height", c.DIP0001Height);
    consensus.pushKV("DIP0003Height", c.DIP0003Height);
    consensus.pushKV("DIP0003EnforcementHeight", c.DIP0003EnforcementHeight);
    consensus.pushKV("DIP0003EnforcementHash", HashHex(c.DIP0003EnforcementHash));
    consensus.pushKV("DIP0008Height", c.DIP0008Height);
    consensus.pushKV("BRRHeight", c.BRRHeight);
    consensus.pushKV("DIP0020Height", c.DIP0020Height);
    consensus.pushKV("DIP0024Height", c.DIP0024Height);
    consensus.pushKV("DIP0024QuorumsHeight", c.DIP0024QuorumsHeight);
    consensus.pushKV("V19Height", c.V19Height);
    consensus.pushKV("V20Height", c.V20Height);
    consensus.pushKV("MN_RRHeight", c.MN_RRHeight);
    consensus.pushKV("WithdrawalsHeight", c.WithdrawalsHeight);
    consensus.pushKV("MinBIP9WarningHeight", c.MinBIP9WarningHeight);
    consensus.pushKV("nRuleChangeActivationThreshold", uint64_t{c.nRuleChangeActivationThreshold});
    consensus.pushKV("nMinerConfirmationWindow", uint64_t{c.nMinerConfirmationWindow});
    UniValue deployments{UniValue::VARR};
    for (int i = 0; i < Consensus::MAX_VERSION_BITS_DEPLOYMENTS; ++i) {
        const Consensus::BIP9Deployment& d = c.vDeployments[i];
        UniValue dep{UniValue::VOBJ};
        dep.pushKV("name", VersionBitsDeploymentInfo[i].name);
        dep.pushKV("bit", d.bit);
        dep.pushKV("nStartTime", d.nStartTime);
        dep.pushKV("nTimeout", d.nTimeout);
        dep.pushKV("min_activation_height", d.min_activation_height);
        dep.pushKV("nWindowSize", d.nWindowSize);
        dep.pushKV("nThresholdStart", d.nThresholdStart);
        dep.pushKV("nThresholdMin", d.nThresholdMin);
        dep.pushKV("nFalloffCoeff", d.nFalloffCoeff);
        dep.pushKV("useEHF", d.useEHF);
        deployments.push_back(dep);
    }
    consensus.pushKV("vDeployments", deployments);
    consensus.pushKV("powLimit", HashHex(c.powLimit));
    consensus.pushKV("fPowAllowMinDifficultyBlocks", c.fPowAllowMinDifficultyBlocks);
    consensus.pushKV("fPowNoRetargeting", c.fPowNoRetargeting);
    consensus.pushKV("nPowTargetSpacing", c.nPowTargetSpacing);
    consensus.pushKV("nPowTargetTimespan", c.nPowTargetTimespan);
    consensus.pushKV("nPowKGWHeight", c.nPowKGWHeight);
    consensus.pushKV("nPowDGWHeight", c.nPowDGWHeight);
    consensus.pushKV("nMinimumChainWork", HashHex(c.nMinimumChainWork));
    consensus.pushKV("defaultAssumeValid", HashHex(c.defaultAssumeValid));
    consensus.pushKV("nMinimumDifficultyBlocks", c.nMinimumDifficultyBlocks);
    consensus.pushKV("nHighSubsidyBlocks", c.nHighSubsidyBlocks);
    consensus.pushKV("nHighSubsidyFactor", c.nHighSubsidyFactor);
    UniValue llmqs{UniValue::VARR};
    for (const auto& q : c.llmqs) {
        UniValue llmq{UniValue::VOBJ};
        llmq.pushKV("type", static_cast<int>(q.type));
        llmq.pushKV("name", std::string{q.name});
        llmq.pushKV("useRotation", q.useRotation);
        llmq.pushKV("size", q.size);
        llmq.pushKV("minSize", q.minSize);
        llmq.pushKV("threshold", q.threshold);
        llmq.pushKV("dkgInterval", q.dkgInterval);
        llmq.pushKV("dkgPhaseBlocks", q.dkgPhaseBlocks);
        llmq.pushKV("dkgMiningWindowStart", q.dkgMiningWindowStart);
        llmq.pushKV("dkgMiningWindowEnd", q.dkgMiningWindowEnd);
        llmq.pushKV("dkgBadVotesThreshold", q.dkgBadVotesThreshold);
        llmq.pushKV("signingActiveQuorumCount", q.signingActiveQuorumCount);
        llmq.pushKV("keepOldConnections", q.keepOldConnections);
        llmq.pushKV("keepOldKeys", q.keepOldKeys);
        llmq.pushKV("recoveryMembers", q.recoveryMembers);
        llmqs.push_back(llmq);
    }
    consensus.pushKV("llmqs", llmqs);
    consensus.pushKV("llmqTypeChainLocks", static_cast<int>(c.llmqTypeChainLocks));
    consensus.pushKV("llmqTypeDIP0024InstantSend", static_cast<int>(c.llmqTypeDIP0024InstantSend));
    consensus.pushKV("llmqTypePlatform", static_cast<int>(c.llmqTypePlatform));
    consensus.pushKV("llmqTypeMnhf", static_cast<int>(c.llmqTypeMnhf));

    UniValue base58{UniValue::VOBJ};
    base58.pushKV("PUBKEY_ADDRESS", HexStr(chain.Base58Prefix(CChainParams::PUBKEY_ADDRESS)));
    base58.pushKV("SCRIPT_ADDRESS", HexStr(chain.Base58Prefix(CChainParams::SCRIPT_ADDRESS)));
    base58.pushKV("SECRET_KEY", HexStr(chain.Base58Prefix(CChainParams::SECRET_KEY)));
    base58.pushKV("EXT_PUBLIC_KEY", HexStr(chain.Base58Prefix(CChainParams::EXT_PUBLIC_KEY)));
    base58.pushKV("EXT_SECRET_KEY", HexStr(chain.Base58Prefix(CChainParams::EXT_SECRET_KEY)));

    UniValue checkpoints{UniValue::VOBJ};
    for (const auto& [height, hash] : chain.Checkpoints().mapCheckpoints) {
        checkpoints.pushKV(strprintf("%d", height), hash.GetHex());
    }

    UniValue ret{UniValue::VOBJ};
    ret.pushKV("core_version", FormatFullVersion());
    ret.pushKV("networkID", chain.NetworkIDString());
    ret.pushKV("messageStart", HexStr(chain.MessageStart()));
    CDataStream genesis{SER_NETWORK, PROTOCOL_VERSION};
    genesis << chain.GenesisBlock();
    ret.pushKV("defaultPort", int{chain.GetDefaultPort()});
    ret.pushKV("pruneAfterHeight", chain.PruneAfterHeight());
    ret.pushKV("genesisHash", chain.GenesisBlock().GetHash().GetHex());
    ret.pushKV("genesisBlock", HexStr(MakeUCharSpan(genesis)));
    ret.pushKV("devnetGenesisHash", chain.DevNetGenesisBlock().GetHash().GetHex());
    ret.pushKV("extCoinType", chain.ExtCoinType());
    ret.pushKV("base58Prefixes", base58);
    ret.pushKV("checkpoints", checkpoints);
    ret.pushKV("consensus", consensus);
    return ret;
}
// End of the verbatim copy.

UniValue LlmqJson(const Consensus::LLMQParams& q)
{
    UniValue llmq{UniValue::VOBJ};
    llmq.pushKV("type", static_cast<int>(q.type));
    llmq.pushKV("name", std::string{q.name});
    llmq.pushKV("useRotation", q.useRotation);
    llmq.pushKV("size", q.size);
    llmq.pushKV("minSize", q.minSize);
    llmq.pushKV("threshold", q.threshold);
    llmq.pushKV("dkgInterval", q.dkgInterval);
    llmq.pushKV("dkgPhaseBlocks", q.dkgPhaseBlocks);
    llmq.pushKV("dkgMiningWindowStart", q.dkgMiningWindowStart);
    llmq.pushKV("dkgMiningWindowEnd", q.dkgMiningWindowEnd);
    llmq.pushKV("dkgBadVotesThreshold", q.dkgBadVotesThreshold);
    llmq.pushKV("signingActiveQuorumCount", q.signingActiveQuorumCount);
    llmq.pushKV("keepOldConnections", q.keepOldConnections);
    llmq.pushKV("keepOldKeys", q.keepOldKeys);
    llmq.pushKV("recoveryMembers", q.recoveryMembers);
    llmq.pushKV("max_store_depth", q.max_store_depth());
    llmq.pushKV("is_single_member", q.is_single_member());
    return llmq;
}

UniValue ExtJson(const CChainParams& chain)
{
    UniValue ext{UniValue::VOBJ};
    const CBaseChainParams& base = BaseParams();
    ext.pushKV("dataDir", base.DataDir());
    ext.pushKV("rpcPort", int{base.RPCPort()});
    ext.pushKV("onionServiceTargetPort", int{base.OnionServiceTargetPort()});
    ext.pushKV("defaultPlatformP2PPort", int{chain.GetDefaultPlatformP2PPort()});
    ext.pushKV("defaultPlatformHTTPPort", int{chain.GetDefaultPlatformHTTPPort()});
    ext.pushKV("assumedBlockchainSize", chain.AssumedBlockchainSize());
    ext.pushKV("assumedChainStateSize", chain.AssumedChainStateSize());
    UniValue seeds{UniValue::VARR};
    for (const auto& s : chain.DNSSeeds()) seeds.push_back(s);
    ext.pushKV("dnsSeeds", seeds);
    ext.pushKV("fixedSeeds", HexStr(chain.FixedSeeds()));
#if CLIENT_VERSION_MAJOR >= 24
    ext.pushKV("bech32PlatformHRP", chain.Bech32PlatformHRP());
    UniValue spork{UniValue::VARR};
    spork.push_back(chain.SporkAddress());
    ext.pushKV("sporkAddresses", spork);
#else
    ext.pushKV("requireStandard", chain.RequireStandard());
    UniValue spork{UniValue::VARR};
    for (const auto& a : chain.SporkAddresses()) spork.push_back(a);
    ext.pushKV("sporkAddresses", spork);
    ext.pushKV("minSporkKeys", chain.MinSporkKeys());
#endif
    ext.pushKV("defaultConsistencyChecks", chain.DefaultConsistencyChecks());
    ext.pushKV("requireRoutableExternalIP", chain.RequireRoutableExternalIP());
    ext.pushKV("isTestChain", chain.IsTestChain());
    ext.pushKV("isMockableChain", chain.IsMockableChain());
    ext.pushKV("mineBlocksOnDemand", chain.MineBlocksOnDemand());
    ext.pushKV("allowMultipleAddressesFromGroup", chain.AllowMultipleAddressesFromGroup());
    ext.pushKV("llmqConnectionRetryTimeout", chain.LLMQConnectionRetryTimeout());
    ext.pushKV("poolMinParticipants", chain.PoolMinParticipants());
    ext.pushKV("poolMaxParticipants", chain.PoolMaxParticipants());
    ext.pushKV("fulfilledRequestExpireTime", chain.FulfilledRequestExpireTime());
    ext.pushKV("creditPoolPeriodBlocks", chain.CreditPoolPeriodBlocks());

    const ChainTxData& tx = chain.TxData();
    UniValue txdata{UniValue::VOBJ};
    txdata.pushKV("nTime", tx.nTime);
    txdata.pushKV("nTxCount", tx.nTxCount);
    uint64_t rate_bits;
    static_assert(sizeof(rate_bits) == sizeof(tx.dTxRate));
    std::memcpy(&rate_bits, &tx.dTxRate, sizeof(rate_bits));
    txdata.pushKV("dTxRateBits", strprintf("%016x", rate_bits));
    ext.pushKV("chainTxData", txdata);

    CDataStream devnet_genesis{SER_NETWORK, PROTOCOL_VERSION};
    devnet_genesis << chain.DevNetGenesisBlock();
    ext.pushKV("devnetGenesisBlock", HexStr(MakeUCharSpan(devnet_genesis)));

    UniValue assumeutxo{UniValue::VARR};
    for (const auto& [height, data] : chain.Assumeutxo()) {
        UniValue au{UniValue::VOBJ};
        au.pushKV("height", height);
        au.pushKV("hash_serialized", data.hash_serialized.ToString());
#if CLIENT_VERSION_MAJOR >= 24
        au.pushKV("evo_hash", data.evo_hash.ToString());
#endif
        au.pushKV("nChainTx", uint64_t{data.nChainTx});
        assumeutxo.push_back(au);
    }
    ext.pushKV("assumeutxo", assumeutxo);

    UniValue info{UniValue::VARR};
    for (int i = 0; i < Consensus::MAX_VERSION_BITS_DEPLOYMENTS; ++i) {
        UniValue d{UniValue::VOBJ};
        d.pushKV("name", VersionBitsDeploymentInfo[i].name);
        d.pushKV("gbt_force", VersionBitsDeploymentInfo[i].gbt_force);
        info.push_back(d);
    }
    ext.pushKV("deploymentInfo", info);

    UniValue buried{UniValue::VOBJ};
    for (int i = Consensus::DEPLOYMENT_HEIGHTINCB; i <= Consensus::DEPLOYMENT_WITHDRAWALS; ++i) {
        const auto dep = static_cast<Consensus::BuriedDeployment>(i);
        buried.pushKV(DeploymentName(dep), chain.GetConsensus().DeploymentHeight(dep));
    }
    ext.pushKV("buriedDeployments", buried);

    UniValue llmqs{UniValue::VARR};
    for (const auto& q : chain.GetConsensus().llmqs) llmqs.push_back(LlmqJson(q));
    ext.pushKV("llmqs", llmqs);
    UniValue available{UniValue::VARR};
    for (const auto& q : Consensus::available_llmqs) available.push_back(LlmqJson(q));
    ext.pushKV("availableLlmqs", available);
#if CLIENT_VERSION_MAJOR >= 24
    ext.pushKV("MAX_LLMQ_SIZE", Consensus::MAX_LLMQ_SIZE);
#endif
    return ext;
}

// The spork signer state of CSporkManager, with its two setters copied
// verbatim from src/spork.cpp (v23.1.8 :314-336, v24.0.0-rc.3 :219-230)
// minus the lock and the log lines.
struct SporkSigners {
#if CLIENT_VERSION_MAJOR >= 24
    CKeyID sporkPubKeyID;
#else
    std::set<CKeyID> setSporkPubKeyIDs;
    int nMinSporkKeys{0};
#endif

    bool SetSporkAddress(const std::string& strAddress)
    {
        CTxDestination dest = DecodeDestination(strAddress);
        const PKHash* pkhash = std::get_if<PKHash>(&dest);
        if (!pkhash) {
            return false;
        }
#if CLIENT_VERSION_MAJOR >= 24
        sporkPubKeyID = ToKeyID(*pkhash);
#else
        setSporkPubKeyIDs.insert(ToKeyID(*pkhash));
#endif
        return true;
    }

#if CLIENT_VERSION_MAJOR < 24
    bool SetMinSporkKeys(int minSporkKeys)
    {
        if (int maxKeysNumber = setSporkPubKeyIDs.size(); (minSporkKeys <= maxKeysNumber / 2) || (minSporkKeys > maxKeysNumber)) {
            return false;
        }
        nMinSporkKeys = minSporkKeys;
        return true;
    }
#endif
};

//! AppInitMain's spork block, verbatim (v23.1.8 init.cpp:1699-1714, v24.0.0-rc.3
//! :1746-1752) but with InitError/InitWarning recorded instead of raised.
UniValue SporkJson(const ArgsManager& args, const CChainParams& chainparams)
{
    SporkSigners sporkman;
    UniValue out{UniValue::VOBJ};
    const auto InitError = [&](const bilingual_str& str) {
        out.pushKV("error", str.original);
        return out;
    };
#if CLIENT_VERSION_MAJOR >= 24
    const std::string spork_address{args.GetArg("-sporkaddr", chainparams.SporkAddress())};
    if (args.GetArgs("-sporkaddr").size() > 1) {
        out.pushKV("warning", strprintf(_("More than one spork address is provided. Using %s."), spork_address).original);
    }
    if (!sporkman.SetSporkAddress(spork_address)) {
        return InitError(_("Invalid spork address specified with -sporkaddr"));
    }
    UniValue keys{UniValue::VARR};
    keys.push_back(HexStr(sporkman.sporkPubKeyID));
#else
    std::vector<std::string> vSporkAddresses;
    if (args.IsArgSet("-sporkaddr")) {
        vSporkAddresses = args.GetArgs("-sporkaddr");
    } else {
        vSporkAddresses = Params().SporkAddresses();
    }
    for (const auto& address: vSporkAddresses) {
        if (!sporkman.SetSporkAddress(address)) {
            return InitError(_("Invalid spork address specified with -sporkaddr"));
        }
    }

    int minsporkkeys = args.GetIntArg("-minsporkkeys", Params().MinSporkKeys());
    if (!sporkman.SetMinSporkKeys(minsporkkeys)) {
        return InitError(_("Invalid minimum number of spork signers specified with -minsporkkeys"));
    }
    UniValue keys{UniValue::VARR};
    for (const auto& id : sporkman.setSporkPubKeyIDs) keys.push_back(HexStr(id));
    out.pushKV("minSporkKeys", sporkman.nMinSporkKeys);
#endif
    out.pushKV("keyIDs", keys);
    return out;
}

} // namespace

int main(int argc, char* argv[])
{
    if (argc < 2) {
        std::fprintf(stderr, "usage: %s <chain> [args...]\n", argv[0]);
        return 2;
    }
    UniValue out{UniValue::VOBJ};
    UniValue provenance{UniValue::VOBJ};
    provenance.pushKV("pin", BS5_PIN);
    provenance.pushKV("commit", BS5_COMMIT);
    provenance.pushKV("core_version", FormatFullVersion());
    out.pushKV("provenance", provenance);
    std::vector<const char*> args{"chainparams_dump"};
    for (int i = 2; i < argc; ++i) args.push_back(argv[i]);
    gArgs.ClearArgs();
    SetupChainParamsOptions(gArgs);
    gArgs.AddArg("-fastprune", "", ArgsManager::ALLOW_ANY | ArgsManager::DEBUG_ONLY, OptionsCategory::DEBUG_TEST);
#if CLIENT_VERSION_MAJOR < 24
    gArgs.AddArg("-minsporkkeys=<n>", "", ArgsManager::ALLOW_ANY, OptionsCategory::DEBUG_TEST);
#endif
    gArgs.AddArg("-sporkaddr=<dashaddress>", "", ArgsManager::ALLOW_ANY, OptionsCategory::DEBUG_TEST);
    std::string error;
    if (!gArgs.ParseParameters(args.size(), args.data(), error)) {
        out.pushKV("parse_error", error);
    } else {
        try {
            SelectParams(argv[1]);
            out.pushKV("params", ChainParamsJson(Params()));
            out.pushKV("ext", ExtJson(Params()));
            out.pushKV("spork", SporkJson(gArgs, Params()));
        } catch (const std::exception& e) {
            out.pushKV("error", std::string{e.what()});
        }
    }
    std::printf("%s\n", out.write().c_str());
    return 0;
}
