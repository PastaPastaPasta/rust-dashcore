// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

// BS8 judge: Dash Core's own reads of the overlay P2P payloads, for the
// base-sdk BS8 differential. Built against a clean build of the pinned v24
// tree by build.sh; links only Core's objects (dashd's, less its main).
//
// Reads lines "<cmd> <stream_version> <bls_legacy> <hex> [max_a] [max_b]"
// on stdin and writes one JSON object per line on stdout:
//   {"ok":true,"consumed":N,"reenc":"<hex>"}  Core read N bytes and wrote
//                                              the object back as reenc
//   ...,"hash":"<hex>"                        and, for the objects Core
//                                              hashes, its hash (uint256
//                                              bytes, little-endian)
//   {"ok":false,"error":"<what>"}             Core's read threw, or the
//                                              handler refused it first
// bls_legacy sets bls::bls_legacy_scheme, the process-wide scheme of eager
// BLS fields, for the line. The stream version is the peer's common
// version. Core reads mnlistdiff, qrinfo, SML entries and snapshots only
// from its own replies and RPC, never from peers; for those this is Core's
// serializer at the stream version.
//
// Every read is the type's Unserialize under SER_NETWORK, as the handlers
// do, except where the handler does more before or during the read:
//   qgetdata  the length check before the read ("oversized")
//   qdata     the request, then, for error NONE, the verification vector
//             (max_a, Core's threshold) and the encrypted contributions
//             (max_b, the valid member count) as NetQuorum reads them;
//             "vvec_size" reports a vector whose size is not max_a
// smlentry and qsnapshot are not messages; they read one object, for the
// per-field differential.
//
// The hash is the one Core keys the object by: GetHash() for qsigrec,
// spork and platformban, CalcHash() (the merkleRootMNList leaf) for
// smlentry, and ::SerializeHash, the inventory hash, for clsig, isdlock and
// qfcommit.

#include <bls/bls.h>
#include <bls/bls_ies.h>
#include <chainlock/chainlock.h>
#include <evo/mnauth.h>
#include <evo/simplifiedmns.h>
#include <evo/smldiff.h>
#include <instantsend/lock.h>
#include <llmq/commitment.h>
#include <llmq/quorums.h>
#include <llmq/signing.h>
#include <llmq/snapshot.h>
#include <masternode/meta.h>
#include <serialize.h>
#include <spork.h>
#include <streams.h>
#include <util/strencodings.h>
#include <util/translation.h>
#include <version.h>

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

template <typename T>
uint256 SerHash(const T& obj) { return ::SerializeHash(obj); }

template <typename T>
std::string Read(const Bytes& raw, int version, uint256 (*hash)(const T&) = nullptr)
{
    CDataStream ds{raw, SER_NETWORK, version};
    T obj;
    ds >> obj;
    const size_t consumed{raw.size() - ds.size()};
    CDataStream out{SER_NETWORK, version};
    out << obj;
    return Ok(consumed, out, hash ? ",\"hash\":\"" + HexStr(hash(obj)) + "\"" : "");
}

std::string ReadQGetData(const Bytes& raw, int version)
{
    llmq::CQuorumDataRequest request;
    if (raw.size() > GetSerializeSize(request, version)) return Fail("oversized");
    return Read<llmq::CQuorumDataRequest>(raw, version);
}

std::string ReadQData(const Bytes& raw, int version, size_t max_vvec, size_t max_contrib)
{
    CDataStream ds{raw, SER_NETWORK, version};
    llmq::CQuorumDataRequest request;
    ds >> request;
    CDataStream out{SER_NETWORK, version};
    out << request;
    std::string extra{",\"error_code\":" + std::to_string(static_cast<int>(request.GetError()))};
    if (request.GetError() != llmq::CQuorumDataRequest::Errors::NONE) {
        return Ok(raw.size() - ds.size(), out, extra);
    }
    if (request.GetDataMask() & llmq::CQuorumDataRequest::QUORUM_VERIFICATION_VECTOR) {
        std::vector<CBLSPublicKey> vvec;
        if (!UnserializeVectorWithMaxSize(ds, vvec, max_vvec)) return Fail("vvec over max");
        if (vvec.size() != max_vvec) extra += ",\"vvec_size\":" + std::to_string(vvec.size());
        out << vvec;
    }
    if (request.GetDataMask() & llmq::CQuorumDataRequest::ENCRYPTED_CONTRIBUTIONS) {
        std::vector<CBLSIESEncryptedObject<CBLSSecretKey>> contributions;
        if (!UnserializeVectorWithMaxSize(ds, contributions, max_contrib)) return Fail("contributions over max");
        out << contributions;
    }
    return Ok(raw.size() - ds.size(), out, extra);
}

std::string Judge(const std::string& cmd, int version, const Bytes& raw, size_t max_a, size_t max_b)
{
    if (cmd == "clsig") return Read<chainlock::ChainLockSig>(raw, version, SerHash);
    if (cmd == "isdlock") return Read<instantsend::InstantSendLock>(raw, version, SerHash);
    if (cmd == "qsigrec") {
        return Read<llmq::CRecoveredSig>(raw, version, [](const llmq::CRecoveredSig& o) { return o.GetHash(); });
    }
    if (cmd == "qfcommit") return Read<llmq::CFinalCommitment>(raw, version, SerHash);
    if (cmd == "mnauth") return Read<CMNAuth>(raw, version);
    if (cmd == "spork") return Read<CSporkMessage>(raw, version, [](const CSporkMessage& o) { return o.GetHash(); });
    if (cmd == "platformban") {
        return Read<PlatformBanMessage>(raw, version, [](const PlatformBanMessage& o) { return o.GetHash(); });
    }
    if (cmd == "qgetdata") return ReadQGetData(raw, version);
    if (cmd == "qdata") return ReadQData(raw, version, max_a, max_b);
    if (cmd == "getqrinfo") return Read<llmq::CGetQuorumRotationInfo>(raw, version);
    if (cmd == "qrinfo") return Read<llmq::CQuorumRotationInfo>(raw, version);
    if (cmd == "mnlistdiff") return Read<CSimplifiedMNListDiff>(raw, version);
    if (cmd == "smlentry") {
        return Read<CSimplifiedMNListEntry>(raw, version, [](const CSimplifiedMNListEntry& o) { return o.CalcHash(); });
    }
    if (cmd == "qsnapshot") return Read<llmq::CQuorumSnapshot>(raw, version);
    return Fail("unknown command " + cmd);
}

} // namespace

int main()
{
    std::ios::sync_with_stdio(false);
    if (!BLSInit()) return 1;
    std::string line;
    while (std::getline(std::cin, line)) {
        std::istringstream in{line};
        std::string cmd, hex;
        int version{0};
        int legacy{0};
        size_t max_a{0}, max_b{0};
        if (!(in >> cmd >> version >> legacy >> hex)) {
            std::cout << Fail("bad request line") << '\n';
            continue;
        }
        in >> max_a >> max_b;
        if (hex == "-") hex.clear();
        const auto raw{TryParseHex<unsigned char>(hex)};
        if (!raw) {
            std::cout << Fail("bad hex") << '\n';
            continue;
        }
        bls::bls_legacy_scheme.store(legacy != 0);
        try {
            std::cout << Judge(cmd, version, *raw, max_a, max_b) << '\n';
        } catch (const std::exception& e) {
            std::cout << Fail(e.what()) << '\n';
        }
        std::cout.flush();
    }
    return 0;
}
