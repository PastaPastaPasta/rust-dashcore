// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

// EV2a codec probe: Dash Core's own masternode-list codecs (v24 evo/snapshot.h and the evodb
// serializers), linked from the v24.0 oracle pin, as a line-oriented oracle for dash-dml's codec
// vectors (designs/EV2-dml.md §2.9, §6.2). One request per stdin line, one answer per stdout line.
//
// Requests (hex is Core's byte order, as streamed):
//   canon <list>            UnserializeCanonicalMNList
//   cdiff <diff>            UnserializeCanonicalMNListDiff (default operation budget)
//   s3 <list>               CDeterministicMNList as evodb stores it (AddMN(bump=false) in file order)
//   d4 <diff>               CDeterministicMNListDiff as evodb stores it
//   apply <canon list> <canon diff> <block hash> <height>
//                           CDeterministicMNList::ApplyDiff
//   build <canon from> <canon to>
//                           CDeterministicMNList::BuildDiff
//   take <n> <list>         the first n masternodes of an evodb list in Core's iteration order,
//                           AddMN(bump=false) into a list at the same position and counter
//   synth                   Core-built fixture lists (see Synth below)
// Answers:
//   ok <canonical> <CanonicalMNListHash> <s3>   for lists (s3 = evodb bytes, CHAMP order)
//   ok <canonical diff> <d4>                    for diffs
//   err <what()>                                Core's exception text
//   list <name> <canonical> <hash> <s3>         synth lists
//   diff <name> <canonical diff> <d4>           synth diffs (BuildDiff between synth lists)

#include <bls/bls.h>
#include <chain.h>
#include <chainparams.h>
#include <evo/deterministicmns.h>
#include <evo/dmnstate.h>
#include <evo/netinfo.h>
#include <evo/providertx.h>
#include <evo/snapshot.h>
#include <key.h>
#include <script/script.h>
#include <streams.h>
#include <tinyformat.h>
#include <util/strencodings.h>
#include <util/translation.h>
#include <version.h>

#include <functional>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

// dashd defines it in bitcoind.cpp, which the probe replaces.
const std::function<std::string(const char*)> G_TRANSLATION_FUN = nullptr;

namespace {

std::string Hex(const CDataStream& s) { return HexStr(MakeUCharSpan(s)); }

CDataStream Stream(const std::string& hex)
{
    if (!IsHex(hex)) throw std::runtime_error("probe: bad hex");
    const auto bytes{ParseHex(hex)};
    return CDataStream{bytes, SER_DISK, CLIENT_VERSION};
}

void RequireEmpty(const CDataStream& s)
{
    if (!s.empty()) throw std::ios_base::failure("probe: trailing bytes");
}

std::string ListAnswer(const CDeterministicMNList& list)
{
    CDataStream canonical{SER_DISK, CLIENT_VERSION};
    evo::SerializeCanonicalMNList(canonical, list);
    CDataStream s3{SER_DISK, CLIENT_VERSION};
    s3 << list;
    const uint256 hash{evo::CanonicalMNListHash(list)};
    return Hex(canonical) + " " + HexStr(hash) + " " + Hex(s3);
}

std::string DiffAnswer(const CDeterministicMNListDiff& diff)
{
    CDataStream canonical{SER_DISK, CLIENT_VERSION};
    evo::SerializeCanonicalMNListDiff(canonical, diff);
    CDataStream d4{SER_DISK, CLIENT_VERSION};
    d4 << diff;
    return Hex(canonical) + " " + Hex(d4);
}

CDeterministicMNList Canon(const std::string& hex)
{
    auto s{Stream(hex)};
    auto list{evo::UnserializeCanonicalMNList(s)};
    RequireEmpty(s);
    return list;
}

CDeterministicMNListDiff CDiff(const std::string& hex)
{
    auto s{Stream(hex)};
    auto diff{evo::UnserializeCanonicalMNListDiff(s)};
    RequireEmpty(s);
    return diff;
}

// --- Synth: lists built with Core's own types and AddMN/UpdateMN/RemoveMN. ---

uint256 H(uint8_t value)
{
    uint256 hash;
    hash.begin()[0] = value;
    return hash;
}

uint256 CollidingH(uint8_t suffix)
{
    uint256 hash;
    std::fill_n(hash.begin(), 8, 0xa5);
    hash.begin()[8] = suffix;
    return hash;
}

uint160 H160(uint8_t value)
{
    uint160 hash;
    hash.begin()[0] = value;
    return hash;
}

CBLSPublicKey OperatorKey(uint8_t n)
{
    // The scalar n: always below the group order.
    std::vector<uint8_t> scalar(32, 0);
    scalar[31] = n;
    const CBLSSecretKey sk{Span<const unsigned char>{scalar}};
    if (!sk.IsValid()) throw std::runtime_error("probe: operator secret");
    return sk.GetPublicKey();
}

struct Spec {
    uint64_t id;
    uint256 pro_tx;
    MnType type;
    int version;
    uint8_t tag;
};

CDeterministicMNCPtr MN(const Spec& spec)
{
    auto state{std::make_shared<CDeterministicMNState>()};
    state->nVersion = spec.version;
    state->nRegisteredHeight = 10 + spec.id;
    state->nLastPaidHeight = 20 + spec.id;
    state->nPoSePenalty = spec.id;
    state->keyIDOwner = CKeyID{H160(spec.tag)};
    state->keyIDVoting = CKeyID{H160(spec.tag + 20)};
    const bool legacy{spec.version == ProTxVersion::LegacyBLS};
    state->pubKeyOperator.Set(OperatorKey(spec.tag), legacy);
    state->scriptOperatorPayout = CScript{} << OP_RETURN << std::vector<unsigned char>{spec.tag, 2};
    state->netInfo = NetInfoInterface::MakeNetInfo(spec.version);
    if (state->netInfo->AddEntry(NetInfoPurpose::CORE_P2P,
                                 strprintf("1.1.%d.%d:%d", spec.tag / 200 + 1, spec.tag, Params().GetDefaultPort())) !=
        NetInfoStatus::Success) {
        throw std::runtime_error("probe: core address");
    }
    if (spec.version >= ProTxVersion::ExtAddr) {
        MasternodePayoutShare a, b;
        a.scriptPayout = CScript{} << OP_RETURN << std::vector<unsigned char>{spec.tag, 1};
        a.reward = 6000;
        b.scriptPayout = CScript{} << OP_RETURN << std::vector<unsigned char>{spec.tag, 3};
        b.reward = 4000;
        state->payouts = {a, b};
        state->nEarlyPeriodBlocks = spec.tag;
        state->nEarlyPenalty = 1000 * spec.tag;
    } else {
        state->scriptPayout = CScript{} << OP_RETURN << std::vector<unsigned char>{spec.tag, 1};
    }
    if (spec.type == MnType::Evo) {
        state->platformNodeID = H160(spec.tag + 40);
        if (spec.version >= ProTxVersion::ExtAddr) {
            if (state->netInfo->AddEntry(NetInfoPurpose::PLATFORM_P2P, strprintf("2.2.2.%d:26657", spec.tag)) !=
                    NetInfoStatus::Success ||
                state->netInfo->AddEntry(NetInfoPurpose::PLATFORM_HTTPS,
                                         strprintf("evo%d.example.org:443", spec.tag)) != NetInfoStatus::Success) {
                throw std::runtime_error("probe: platform address");
            }
        } else {
            state->platformP2PPort = 26656;
            state->platformHTTPPort = 443;
        }
    }
    auto dmn{std::make_shared<CDeterministicMN>(spec.id, spec.type)};
    dmn->proTxHash = spec.pro_tx;
    dmn->collateralOutpoint = COutPoint(H(spec.tag + 80), spec.id);
    dmn->nOperatorReward = spec.tag * 10;
    state->UpdateConfirmedHash(dmn->proTxHash, H(spec.tag + 100));
    dmn->pdmnState = std::move(state);
    return dmn;
}

void Synth()
{
    std::vector<std::pair<std::string, CDeterministicMNList>> lists;
    auto emit = [&](const std::string& name, const CDeterministicMNList& list) {
        std::cout << "list " << name << " " << ListAnswer(list) << "\n";
        lists.emplace_back(name, list);
    };

    // Every state version and type; ids out of order; a gap in the ids.
    CDeterministicMNList mixed{H(42), 500, 12};
    mixed.AddMN(MN({2, H(3), MnType::Regular, ProTxVersion::LegacyBLS, 3}), false);
    mixed.AddMN(MN({5, H(1), MnType::Evo, ProTxVersion::ExtAddr, 5}), false);
    mixed.AddMN(MN({7, H(2), MnType::Regular, ProTxVersion::BasicBLS, 7}), false);
    mixed.AddMN(MN({11, H(9), MnType::Evo, ProTxVersion::BasicBLS, 9}), false);
    mixed.AddMN(MN({0, H(4), MnType::Regular, ProTxVersion::ExtAddr, 4}), false);
    emit("mixed", mixed);

    // The next block: a hot update, a cold update (key + version), a removal and an addition.
    CDeterministicMNList next{mixed};
    next.SetBlockHash(H(43));
    next.SetHeight(501);
    {
        auto dmn{next.GetMN(H(3))};
        auto state{std::make_shared<CDeterministicMNState>(*dmn->pdmnState)};
        state->nLastPaidHeight = 501;
        state->nConsecutivePayments = 2;
        next.UpdateMN(*dmn, state);
    }
    {
        auto dmn{next.GetMN(H(2))};
        auto state{std::make_shared<CDeterministicMNState>(*dmn->pdmnState)};
        state->ResetOperatorFields();
        state->nVersion = ProTxVersion::BasicBLS;
        state->pubKeyOperator.Set(OperatorKey(77), false);
        state->BanIfNotBanned(501);
        next.UpdateMN(*dmn, state);
    }
    next.RemoveMN(H(1));
    next.AddMN(MN({12, H(6), MnType::Regular, ProTxVersion::LegacyBLS, 6}), true);
    emit("next", next);

    // Colliding 64-bit hashes (one CHAMP collision node): insert order, an update that moves a key
    // to the front, a removal, and a re-insertion.
    CDeterministicMNList collide{H(50), 600, 0};
    for (uint8_t i : {3, 1, 2, 5}) {
        collide.AddMN(MN({collide.GetTotalRegisteredCount(), CollidingH(i), MnType::Regular, ProTxVersion::BasicBLS,
                          static_cast<uint8_t>(110 + i)}),
                      true);
    }
    collide.AddMN(MN({collide.GetTotalRegisteredCount(), H(8), MnType::Regular, ProTxVersion::BasicBLS, 120}), true);
    emit("collide", collide);
    CDeterministicMNList collide2{collide};
    collide2.SetBlockHash(H(51));
    collide2.SetHeight(601);
    {
        auto dmn{collide2.GetMN(CollidingH(2))};
        auto state{std::make_shared<CDeterministicMNState>(*dmn->pdmnState)};
        state->nPoSePenalty += 10;
        collide2.UpdateMN(*dmn, state);
    }
    collide2.RemoveMN(CollidingH(1));
    collide2.AddMN(MN({collide2.GetTotalRegisteredCount(), CollidingH(9), MnType::Evo, ProTxVersion::BasicBLS, 119}),
                   true);
    emit("collide2", collide2);

    // An empty DIP3 list.
    emit("empty", CDeterministicMNList{H(60), 700, 3});

    for (size_t i{0}; i + 1 < lists.size(); ++i) {
        for (size_t j{0}; j < lists.size(); ++j) {
            if (i == j) continue;
            const auto diff{lists[i].second.BuildDiff(lists[j].second)};
            std::cout << "diff " << lists[i].first << ">" << lists[j].first << " " << DiffAnswer(diff) << "\n";
        }
    }
}

std::string Handle(const std::string& line)
{
    std::istringstream in{line};
    std::string op;
    in >> op;
    if (op == "canon") {
        std::string hex;
        in >> hex;
        return "ok " + ListAnswer(Canon(hex));
    }
    if (op == "cdiff") {
        std::string hex;
        in >> hex;
        return "ok " + DiffAnswer(CDiff(hex));
    }
    if (op == "s3") {
        std::string hex;
        in >> hex;
        auto s{Stream(hex)};
        CDeterministicMNList list;
        s >> list;
        RequireEmpty(s);
        return "ok " + ListAnswer(list);
    }
    if (op == "d4") {
        std::string hex;
        in >> hex;
        auto s{Stream(hex)};
        CDeterministicMNListDiff diff;
        s >> diff;
        RequireEmpty(s);
        return "ok " + DiffAnswer(diff);
    }
    if (op == "take") {
        size_t n;
        std::string hex;
        in >> n >> hex;
        auto s{Stream(hex)};
        CDeterministicMNList list;
        s >> list;
        RequireEmpty(s);
        CDeterministicMNList part{list.GetBlockHash(), list.GetHeight(), list.GetTotalRegisteredCount()};
        list.ForEachMNShared(/*onlyValid=*/false, [&](const auto& dmn) {
            if (part.GetCounts().total() < n) part.AddMN(dmn, /*fBumpTotalCount=*/false);
        });
        return "ok " + ListAnswer(part);
    }
    if (op == "apply") {
        std::string list_hex, diff_hex, block_hex;
        int height;
        in >> list_hex >> diff_hex >> block_hex >> height;
        auto list{Canon(list_hex)};
        const auto diff{CDiff(diff_hex)};
        const auto block_bytes{ParseHex(block_hex)};
        const uint256 block{Span{block_bytes}};
        CBlockIndex index;
        index.phashBlock = &block;
        index.nHeight = height;
        list.ApplyDiff(&index, diff);
        return "ok " + ListAnswer(list);
    }
    if (op == "build") {
        std::string from_hex, to_hex;
        in >> from_hex >> to_hex;
        return "ok " + DiffAnswer(Canon(from_hex).BuildDiff(Canon(to_hex)));
    }
    throw std::runtime_error("probe: unknown request " + op);
}

} // namespace

int main(int argc, char** argv)
{
    const std::string chain{argc > 1 ? argv[1] : "main"};
    SelectParams(chain);
    ECC_Start();
    BLSInit();
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line == "synth") {
            Synth();
            std::cout << "end\n" << std::flush;
            continue;
        }
        std::string answer;
        try {
            answer = Handle(line);
        } catch (const std::exception& e) {
            answer = std::string{"err "} + e.what();
        }
        std::cout << answer << "\n" << std::flush;
    }
    ECC_Stop();
    return 0;
}
