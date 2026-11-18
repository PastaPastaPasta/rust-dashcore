// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

// BS9 judge: Dash Core's own reads of the overlay P2P payloads of BS9 (DKG,
// signature share, CoinJoin and governance messages), for the base-sdk BS9
// differential. Built against a clean build of the pinned v24 tree by
// ../bs8-judge/build.sh;
// links only Core's objects (dashd's, less its main), and compiles
// llmq/net_dkg.cpp into itself for the DKG handler's size cap and walk,
// which are file-local there.
//
// Reads lines "<cmd> <stream_version> <bls_legacy> <hex> [a] [b] [c]" on
// stdin and writes one JSON object per line on stdout:
//   {"ok":true,"consumed":N,"reenc":"<hex>"}  Core read N bytes and wrote
//                                              the object back as reenc
//   ...,"hash":"<hex>"                        the inventory hash Core keys
//                                              the message by (uint256
//                                              bytes, little-endian)
//   ...,"sign_hash":"<hex>"                   the sign hash of each object
//                                              read, concatenated
//   {"ok":false,"error":"<what>"}             Core's read threw, or the
//                                              handler refused it first
// bls_legacy sets bls::bls_legacy_scheme, the process-wide scheme of eager
// BLS fields, for the line.
//
// DKG messages (qcontrib, qcomplaint, qjustify, qpcommit) take a, b and c
// as the quorum's size, minSize and threshold, and go through Core's
// pipeline: the prefix length check ("short"), MaxDKGMessageSize
// ("oversized"), CheckDKGMessageWireStructure ("framing"), then the
// worker's DeserializeCheckedDKGMessage (its exception, "trailing" or
// "structure"). Their inventory hash is the payload's.
//
// Signature share messages are read as NetSigning::ProcessMessage reads
// them: qsigsesann, qsigsinv, qgetsigs and qsigshare with
// UnserializeVectorWithMaxSize at their limits, qbsigs with
// UnserializeBatchedSigShares. sign_hash is each announcement's or share's
// SignHash.
//
// CoinJoin and governance messages are read as their handlers read them,
// at the line's stream version (dsa's flags byte depends on it), with
// mainnet's parameters selected (dsi's and dss's pool bound): dss with
// UnserializeVectorWithMaxSize at the pool bound (over it, "ERR_MAXIMUM",
// the handler's reply), dsf as a session id and a CTransaction, dsc as two
// int32s, govsync as a hash ("govsync hash: ", only logged) and then a
// CBloomFilter within its size constraints ("govsync filter: ", scored).
// The handlers ignore bytes after the message. hash is the key Core files the
// object under (dsq, govobj and govobjvote GetHash, dstx its txid) and
// sign_hash its GetSignatureHash.

#include <llmq/net_dkg.cpp>

#include <chainparams.h>
#include <coinjoin/coinjoin.h>
#include <common/bloom.h>
#include <governance/object.h>
#include <governance/vote.h>
#include <llmq/net_signing.h>
#include <llmq/signing_shares.h>
#include <util/strencodings.h>
#include <util/translation.h>

#include <cstdio>
#include <exception>
#include <functional>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

const std::function<std::string(const char*)> G_TRANSLATION_FUN = nullptr;

namespace {

using Bytes = std::vector<unsigned char>;

std::string JsonString(const std::string& s)
{
    std::string out{"\""};
    for (unsigned char c : s) {
        if (c == '"' || c == '\\') {
            out += '\\';
            out += static_cast<char>(c);
        } else if (c < 0x20 || c > 0x7e) {
            char buf[8];
            std::snprintf(buf, sizeof(buf), "\\u%04x", c);
            out += buf;
        } else {
            out += static_cast<char>(c);
        }
    }
    return out + "\"";
}

std::string Ok(size_t consumed, const CDataStream& out, const std::string& extra = "")
{
    return "{\"ok\":true,\"consumed\":" + std::to_string(consumed) + ",\"reenc\":\"" +
           HexStr(MakeUCharSpan(out)) + "\"" + extra + "}";
}

std::string Fail(const std::string& error) { return "{\"ok\":false,\"error\":" + JsonString(error) + "}"; }

std::string Field(const std::string& name, const std::string& hex) { return ",\"" + name + "\":\"" + hex + "\""; }

// The DKG handler's checks and the worker's read, as NetDKG::ProcessMessage
// and ProcessPendingMessageBatch run them. The read mirrors
// DeserializeCheckedDKGMessage (llmq/net_dkg.cpp:370-389, in its anonymous
// namespace) step for step, keeping the exception for the corpus class:
// re-check it against that function on every pin bump.
template <typename Message>
std::string ReadDkg(const std::string& cmd, const Bytes& raw, int version, const Consensus::LLMQParams& params)
{
    if (raw.size() < sizeof(uint8_t) + sizeof(uint256)) return Fail("short");
    if (raw.size() > llmq::MaxDKGMessageSize(cmd, params)) return Fail("oversized");
    const CDataStream payload{raw, SER_NETWORK, version};
    if (!llmq::CheckDKGMessageWireStructure(cmd, payload, params)) return Fail("framing");
    CDataStream ds{raw, SER_NETWORK, version};
    Message msg;
    ds >> msg;
    if (!ds.empty()) return Fail("trailing");
    if (!llmq::CheckDKGMessageStructure(msg, params)) return Fail("structure");
    CHashWriter hw(SER_GETHASH, 0);
    hw.write(AsBytes(Span{raw}));
    CDataStream out{SER_NETWORK, version};
    out << msg;
    return Ok(raw.size(), out, Field("hash", HexStr(hw.GetHash())) + Field("sign_hash", HexStr(msg.GetSignHash())));
}

template <typename T>
std::string SignHashes(const std::vector<T>& objs)
{
    std::string out;
    for (const auto& o : objs) out += HexStr(o.buildSignHash().Get());
    return Field("sign_hash", out);
}

template <typename T>
std::string ReadVector(const Bytes& raw, int version, size_t max, bool sign_hashes)
{
    CDataStream ds{raw, SER_NETWORK, version};
    std::vector<T> msgs;
    if (!UnserializeVectorWithMaxSize(ds, msgs, max)) return Fail("vector size too large");
    CDataStream out{SER_NETWORK, version};
    out << msgs;
    if constexpr (std::is_same_v<T, llmq::CSigSesAnn> || std::is_same_v<T, llmq::CSigShare>) {
        if (sign_hashes) return Ok(raw.size() - ds.size(), out, SignHashes(msgs));
    }
    return Ok(raw.size() - ds.size(), out);
}

std::string ReadBatched(const Bytes& raw, int version)
{
    CDataStream ds{raw, SER_NETWORK, version};
    const auto msgs{llmq::UnserializeBatchedSigShares(ds)};
    CDataStream out{SER_NETWORK, version};
    out << msgs;
    return Ok(raw.size() - ds.size(), out);
}

// A CoinJoin or governance message: Core's read of `T`, its write-back, and
// the hashes `extra` names.
template <typename T>
std::string ReadObject(const Bytes& raw, int version, const std::function<std::string(const T&)>& extra = nullptr)
{
    CDataStream ds{raw, SER_NETWORK, version};
    T obj;
    ds >> obj;
    CDataStream out{SER_NETWORK, version};
    out << obj;
    return Ok(raw.size() - ds.size(), out, extra ? extra(obj) : "");
}

std::string Hashes(const uint256& hash, const uint256& sign_hash)
{
    return Field("hash", HexStr(hash)) + Field("sign_hash", HexStr(sign_hash));
}

// ReadObject with the object's GetHash and GetSignatureHash.
template <typename T>
std::string ReadHashed(const Bytes& raw, int version)
{
    return ReadObject<T>(raw, version, [](const T& o) { return Hashes(o.GetHash(), o.GetSignatureHash()); });
}

std::string ReadCoinJoinOrGov(const std::string& cmd, int version, const Bytes& raw)
{
    if (cmd == "dsa") return ReadObject<CCoinJoinAccept>(raw, version);
    if (cmd == "dsi") return ReadObject<CCoinJoinEntry>(raw, version);
    if (cmd == "dsf") {
        CDataStream ds{raw, SER_NETWORK, version};
        int32_t session;
        ds >> session;
        const CTransaction tx(deserialize, ds);
        CDataStream out{SER_NETWORK, version};
        out << session << tx;
        return Ok(raw.size() - ds.size(), out);
    }
    if (cmd == "dss") {
        CDataStream ds{raw, SER_NETWORK, version};
        std::vector<CTxIn> ins;
        if (!UnserializeVectorWithMaxSize(ds, ins, CoinJoin::GetMaxPoolInputOutputCount())) {
            return Fail("ERR_MAXIMUM");
        }
        CDataStream out{SER_NETWORK, version};
        out << ins;
        return Ok(raw.size() - ds.size(), out);
    }
    if (cmd == "dsc") {
        CDataStream ds{raw, SER_NETWORK, version};
        int32_t session, message;
        ds >> session >> message;
        CDataStream out{SER_NETWORK, version};
        out << session << message;
        return Ok(raw.size() - ds.size(), out);
    }
    if (cmd == "dssu") return ReadObject<CCoinJoinStatusUpdate>(raw, version);
    if (cmd == "dstx") {
        return ReadObject<CCoinJoinBroadcastTx>(raw, version, [](const CCoinJoinBroadcastTx& d) {
            return Hashes(d.tx->GetHash(), d.GetSignatureHash());
        });
    }
    if (cmd == "dsq") return ReadHashed<CCoinJoinQueue>(raw, version);
    if (cmd == "govobj") return ReadHashed<CGovernanceObject>(raw, version);
    if (cmd == "govobjvote") return ReadHashed<CGovernanceVote>(raw, version);
    if (cmd == "govsync") {
        // net_governance.cpp reads the hash outside its filter's try.
        CDataStream ds{raw, SER_NETWORK, version};
        uint256 prop;
        CBloomFilter filter;
        try {
            ds >> prop;
        } catch (const std::exception& e) {
            return Fail(std::string{"govsync hash: "} + e.what());
        }
        try {
            ds >> filter;
        } catch (const std::exception& e) {
            return Fail(std::string{"govsync filter: "} + e.what());
        }
        if (!filter.IsWithinSizeConstraints()) return Fail("govsync filter: size constraints");
        CDataStream out{SER_NETWORK, version};
        out << prop << filter;
        return Ok(raw.size() - ds.size(), out);
    }
    return Fail("unknown command " + cmd);
}

std::string Judge(const std::string& cmd, int version, const Bytes& raw, int a, int b, int c)
{
    Consensus::LLMQParams params{};
    params.size = a;
    params.minSize = b;
    params.threshold = c;
    if (cmd == "qcontrib") return ReadDkg<llmq::CDKGContribution>(cmd, raw, version, params);
    if (cmd == "qcomplaint") return ReadDkg<llmq::CDKGComplaint>(cmd, raw, version, params);
    if (cmd == "qjustify") return ReadDkg<llmq::CDKGJustification>(cmd, raw, version, params);
    if (cmd == "qpcommit") return ReadDkg<llmq::CDKGPrematureCommitment>(cmd, raw, version, params);
    using llmq::CSigSharesManager;
    if (cmd == "qsigsesann") {
        return ReadVector<llmq::CSigSesAnn>(raw, version, CSigSharesManager::MAX_MSGS_CNT_QSIGSESANN, true);
    }
    if (cmd == "qsigsinv" || cmd == "qgetsigs") {
        return ReadVector<llmq::CSigSharesInv>(raw, version, CSigSharesManager::MAX_MSGS_CNT_QSIGSHARES, false);
    }
    if (cmd == "qsigshare") {
        return ReadVector<llmq::CSigShare>(raw, version, CSigSharesManager::MAX_MSGS_SIG_SHARES, true);
    }
    if (cmd == "qbsigs") return ReadBatched(raw, version);
    return ReadCoinJoinOrGov(cmd, version, raw);
}

} // namespace

int main()
{
    std::ios::sync_with_stdio(false);
    if (!BLSInit()) return 1;
    SelectParams(CBaseChainParams::MAIN);
    std::string line;
    while (std::getline(std::cin, line)) {
        std::istringstream in{line};
        std::string cmd, hex;
        int version{0};
        int legacy{0};
        int a{0}, b{0}, c{0};
        if (!(in >> cmd >> version >> legacy >> hex)) {
            std::cout << Fail("bad request line") << '\n';
            continue;
        }
        in >> a >> b >> c;
        if (hex == "-") hex.clear();
        const auto raw{TryParseHex<unsigned char>(hex)};
        if (!raw) {
            std::cout << Fail("bad hex") << '\n';
            continue;
        }
        bls::bls_legacy_scheme.store(legacy != 0);
        try {
            std::cout << Judge(cmd, version, *raw, a, b, c) << '\n';
        } catch (const std::exception& e) {
            std::cout << Fail(e.what()) << '\n';
        }
        std::cout.flush();
    }
    return 0;
}
