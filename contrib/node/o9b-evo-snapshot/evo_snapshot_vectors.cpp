// O9b S6a: Core v24.0.0-rc.4's EvoSnapshot vectors and verdicts, linked into a copy of test_dash
// by build.sh. Lines between the FIXTURES markers are src/test/evo_snapshot_tests.cpp lines
// 1-226 at the tag, verbatim (build.sh's MANIFEST records that file's blob id).
//
//   O9B_DUMP=<file>  test_dash_o9b --run_test=o9b_evo_snapshot_vectors/dump
//     one line per fixture: "<name> <kind> <hex> <verdict>", the verdict as for judge.
//   O9B_JUDGE=<in> O9B_VERDICTS=<out>  test_dash_o9b --run_test=o9b_evo_snapshot_vectors/judge
//     reads "<name> <kind> <hex>" lines and writes "<name> <sha256 of the input> <verdict>", where
//     <verdict> is one of
//       ok <rest> <sha256 of the re-encoding> [<GetEvoSnapshotHash> <CanonicalMNListHash>]
//       fail <class> <what()>
//     <rest> is the count of unread bytes, and hashes are uint256::ToString (reversed hex).
//     Kinds: snapshot, mnlist, mndiff, commitment, rotation:<LLMQType as a number>.
// ==== FIXTURES BEGIN ====
// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

#include <test/util/setup_common.h>

#include <clientversion.h>
#include <consensus/amount.h>
#include <consensus/merkle.h>
#include <evo/cbtx.h>
#include <evo/chainhelper.h>
#include <evo/creditpool.h>
#include <evo/evodb.h>
#include <evo/mnhftx.h>
#include <evo/netinfo.h>
#include <evo/snapshot.h>
#include <hash.h>
#include <llmq/blockprocessor.h>
#include <llmq/context.h>
#include <llmq/quorumsman.h>
#include <llmq/signhash.h>
#include <llmq/snapshot.h>
#include <llmq/utils.h>
#include <masternode/meta.h>
#include <node/context.h>
#include <streams.h>
#include <validation.h>
#include <versionbits.h>

#include <algorithm>
#include <cstddef>
#include <limits>
#include <map>
#include <set>
#include <string>
#include <type_traits>
#include <utility>
#include <vector>

#include <boost/test/unit_test.hpp>

namespace {

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

CDeterministicMNCPtr MN(uint64_t internal_id, uint8_t hash_suffix, MnType type, int version, uint8_t address_tag)
{
    auto state{std::make_shared<CDeterministicMNState>()};
    state->nVersion = version;
    state->nRegisteredHeight = 10 + internal_id;
    state->nLastPaidHeight = 20 + internal_id;
    state->nPoSePenalty = internal_id;
    state->keyIDOwner = CKeyID{H160(address_tag)};
    state->keyIDVoting = CKeyID{H160(address_tag + 20)};
    state->scriptPayout = CScript{} << OP_RETURN << std::vector<unsigned char>{address_tag, 1};
    state->scriptOperatorPayout = CScript{} << OP_RETURN << std::vector<unsigned char>{address_tag, 2};
    state->netInfo = NetInfoInterface::MakeNetInfo(version);
    BOOST_REQUIRE_EQUAL(state->netInfo->AddEntry(NetInfoPurpose::CORE_P2P,
                                                strprintf("1.1.1.%d:%d", address_tag, Params().GetDefaultPort())),
                        NetInfoStatus::Success);
    if (type == MnType::Evo) {
        state->platformNodeID = H160(address_tag + 40);
        BOOST_REQUIRE_EQUAL(state->netInfo->AddEntry(NetInfoPurpose::PLATFORM_P2P,
                                                    strprintf("2.2.2.%d:26657", address_tag)),
                            NetInfoStatus::Success);
        BOOST_REQUIRE_EQUAL(state->netInfo->AddEntry(NetInfoPurpose::PLATFORM_HTTPS,
                                                    strprintf("evo%d.example.org:443", address_tag)),
                            NetInfoStatus::Success);
    }

    auto dmn{std::make_shared<CDeterministicMN>(internal_id, type)};
    dmn->proTxHash = CollidingH(hash_suffix);
    dmn->collateralOutpoint = COutPoint(H(address_tag + 80), internal_id);
    dmn->nOperatorReward = address_tag * 10;
    state->UpdateConfirmedHash(dmn->proTxHash, H(address_tag + 100));
    dmn->pdmnState = std::move(state);
    return dmn;
}

CDeterministicMNList MNList(const uint256& block_hash, int height, bool reverse)
{
    CDeterministicMNList list{block_hash, height, 10};
    std::vector<CDeterministicMNCPtr> mns{
        MN(2, 3, MnType::Regular, ProTxVersion::LegacyBLS, 3),
        MN(5, 1, MnType::Evo, ProTxVersion::ExtAddr, 5),
        MN(7, 2, MnType::Regular, ProTxVersion::LegacyBLS, 7),
    };
    if (reverse) std::reverse(mns.begin(), mns.end());
    for (const auto& dmn : mns) list.AddMN(dmn, /*fBumpTotalCount=*/false);
    return list;
}

evo::MinedQuorumCommitment Commitment(Consensus::LLMQType type, uint8_t quorum, uint8_t mined, bool rotated,
                                      int16_t index = 0)
{
    llmq::CFinalCommitment commitment;
    commitment.nVersion = rotated ? llmq::CFinalCommitment::BASIC_BLS_INDEXED_QUORUM_VERSION
                                  : llmq::CFinalCommitment::BASIC_BLS_NON_INDEXED_QUORUM_VERSION;
    commitment.llmqType = type;
    commitment.quorumHash = H(quorum);
    commitment.quorumIndex = index;
    const auto& params{evo::SnapshotLLMQParams(type)};
    commitment.signers.resize(params.size);
    commitment.validMembers.resize(params.size);
    return {H(quorum), H(quorum + 120), std::move(commitment), H(mined)};
}

evo::EvoSnapshot SyntheticSnapshot(bool reverse_representation = false)
{
    evo::EvoSnapshot snapshot;
    snapshot.base_block_hash = H(42);
    snapshot.mn_list = MNList(snapshot.base_block_hash, 500, reverse_representation);
    snapshot.credit_pool.locked = 123456;
    snapshot.credit_pool.currentLimit = 700;
    snapshot.credit_pool.latelyUnlocked = 11;
    if (reverse_representation) {
        BOOST_REQUIRE(snapshot.credit_pool.indexes.Add(15));
        BOOST_REQUIRE(snapshot.credit_pool.indexes.Add(8));
        BOOST_REQUIRE(snapshot.credit_pool.indexes.Add(7));
        BOOST_REQUIRE(snapshot.credit_pool.indexes.Add(9));
        BOOST_REQUIRE(snapshot.credit_pool.indexes.Remove(9));
        snapshot.mnhf_signals.emplace(9, 30);
        snapshot.mnhf_signals.emplace(2, 12);
    } else {
        BOOST_REQUIRE(snapshot.credit_pool.indexes.Add(7));
        BOOST_REQUIRE(snapshot.credit_pool.indexes.Add(8));
        BOOST_REQUIRE(snapshot.credit_pool.indexes.Add(15));
        snapshot.mnhf_signals.emplace(2, 12);
        snapshot.mnhf_signals.emplace(9, 30);
    }

    evo::QuorumSnapshotData plain;
    plain.llmq_type = Consensus::LLMQType::LLMQ_TEST;
    plain.active_commitments = {Commitment(plain.llmq_type, 11, 51, false), Commitment(plain.llmq_type, 12, 52, false)};
    plain.safety_commitments = {Commitment(plain.llmq_type, 10, 50, false)};

    evo::QuorumSnapshotData rotated;
    rotated.llmq_type = Consensus::LLMQType::LLMQ_TEST_DIP0024;
    rotated.rotation_enabled = true;
    rotated.active_commitments = {Commitment(rotated.llmq_type, 31, 71, true, 0),
                                  Commitment(rotated.llmq_type, 32, 72, true, 1)};
    rotated.safety_commitments = {Commitment(rotated.llmq_type, 21, 61, true, 0),
                                  Commitment(rotated.llmq_type, 22, 62, true, 1)};
    for (uint8_t i{1}; i <= evo::EVO_SNAPSHOT_ROTATION_CYCLES; ++i) {
        const auto mode{i == 2 ? SnapshotSkipMode::MODE_SKIPPING_ENTRIES : SnapshotSkipMode::MODE_NO_SKIPPING};
        rotated.rotation_snapshots.push_back(
            {H(40 + i), H(100 + i), llmq::CQuorumSnapshot{{true, false, true, false}, mode, i == 2 ? std::vector<int>{1} : std::vector<int>{}}});
    }

    snapshot.quorums = {std::move(plain), std::move(rotated)};
    std::set<uint256> work_hashes;
    std::set<std::pair<Consensus::LLMQType, uint256>> modifier_keys;
    for (const auto& data : snapshot.quorums) {
        for (const auto* commitments : {&data.active_commitments, &data.safety_commitments}) {
            for (const auto& entry : *commitments) {
                work_hashes.insert(entry.work_block_hash);
                modifier_keys.emplace(data.llmq_type, entry.work_block_hash);
            }
        }
        for (const auto& entry : data.rotation_snapshots) {
            work_hashes.insert(entry.work_block_hash);
            modifier_keys.emplace(data.llmq_type, entry.work_block_hash);
        }
    }
    CDeterministicMNList previous{snapshot.mn_list};
    uint256 previous_hash{snapshot.base_block_hash};
    int height{499};
    for (const auto& work_hash : work_hashes) {
        auto list{MNList(work_hash, height--, reverse_representation)};
        snapshot.historical_mn_list_diffs.push_back({previous_hash, work_hash, list.GetHeightForSnapshotCodec(),
                                                     list.GetTotalRegisteredCount(), evo::CanonicalMNListHash(list),
                                                     previous.BuildDiff(list)});
        previous_hash = work_hash;
        previous = std::move(list);
    }
    for (const auto& [type, work_hash] : modifier_keys) {
        snapshot.quorum_modifiers.push_back({type, work_hash, H(static_cast<uint8_t>(150 + snapshot.quorum_modifiers.size()))});
    }
    if (reverse_representation) {
        std::reverse(snapshot.quorums.begin(), snapshot.quorums.end());
        std::reverse(snapshot.historical_mn_list_diffs.begin(), snapshot.historical_mn_list_diffs.end());
        std::reverse(snapshot.quorum_modifiers.begin(), snapshot.quorum_modifiers.end());
        for (auto& data : snapshot.quorums) {
            std::reverse(data.active_commitments.begin(), data.active_commitments.end());
            std::reverse(data.safety_commitments.begin(), data.safety_commitments.end());
            std::reverse(data.rotation_snapshots.begin(), data.rotation_snapshots.end());
        }
    }
    return snapshot;
}

CDataStream SerializeSnapshot(const evo::EvoSnapshot& snapshot)
{
    CDataStream stream{SER_DISK, CLIENT_VERSION};
    stream << snapshot;
    return stream;
}

void CheckInvalid(evo::EvoSnapshot snapshot) { BOOST_CHECK_THROW(snapshot.Validate(), std::ios_base::failure); }

// The snapshot decoder inserts attacker-chosen update IDs straight into this
// map, so its hash must not be the identity an adversary can drive into one
// bucket.
static_assert(std::is_same_v<decltype(CDeterministicMNListDiff::updatedMNs)::hasher, StaticSaltedHasher>);

} // namespace
// ==== FIXTURES END ====

#include <util/strencodings.h>

#include <cstdlib>
#include <fstream>
#include <functional>
#include <sstream>
#include <typeinfo>

#include <boost/core/demangle.hpp>

namespace {

uint256 Sha256(Span<const std::byte> bytes)
{
    uint256 hash;
    CSHA256().Write(UCharCast(bytes.data()), bytes.size()).Finalize(hash.begin());
    return hash;
}

template <typename T>
std::string Reencoded(const T& value)
{
    CDataStream out{SER_DISK, CLIENT_VERSION};
    out << value;
    return Sha256(out).ToString();
}

// Decodes `bytes` as `kind` the way the snapshot path does and renders Core's verdict.
std::string Judge(const std::string& kind, const std::vector<unsigned char>& bytes)
{
    CDataStream s{MakeByteSpan(bytes), SER_DISK, CLIENT_VERSION};
    try {
        std::string extra;
        std::string reencoded;
        if (kind == "snapshot") {
            evo::EvoSnapshot snapshot;
            s >> snapshot;
            reencoded = Reencoded(snapshot);
            extra = " " + evo::GetEvoSnapshotHash(snapshot).ToString() + " " +
                    evo::CanonicalMNListHash(snapshot.mn_list).ToString();
        } else if (kind.rfind("reconstruct:", 0) == 0) {
            // The decoded snapshot's history replayed again under a record budget of its own.
            evo::EvoSnapshot snapshot;
            s >> snapshot;
            std::map<uint256, CDeterministicMNList> lists;
            std::string error;
            if (!evo::ReconstructHistoricalMNLists(snapshot, lists, error, std::stoull(kind.substr(12)))) {
                throw std::ios_base::failure(error);
            }
            reencoded = Reencoded(snapshot);
            extra = " " + std::to_string(lists.size());
        } else if (kind == "mnlist") {
            const auto list{evo::UnserializeCanonicalMNList(s)};
            CDataStream out{SER_DISK, CLIENT_VERSION};
            evo::SerializeCanonicalMNList(out, list);
            reencoded = Sha256(out).ToString();
            extra = " " + evo::CanonicalMNListHash(list).ToString();
        } else if (kind == "mndiff") {
            const auto diff{evo::UnserializeCanonicalMNListDiff(s)};
            CDataStream out{SER_DISK, CLIENT_VERSION};
            evo::SerializeCanonicalMNListDiff(out, diff);
            reencoded = Sha256(out).ToString();
        } else if (kind == "commitment") {
            reencoded = Reencoded(evo::ReadMinedQuorumCommitment(s));
        } else if (kind.rfind("rotation:", 0) == 0) {
            const auto type{static_cast<Consensus::LLMQType>(std::stoi(kind.substr(9)))};
            const auto entry{evo::ReadRotationSnapshot(s, evo::SnapshotLLMQParams(type))};
            CDataStream out{SER_DISK, CLIENT_VERSION};
            evo::WriteRotationSnapshot(out, entry);
            reencoded = Sha256(out).ToString();
        } else {
            throw std::invalid_argument("unknown kind " + kind);
        }
        return "ok " + std::to_string(s.size()) + " " + reencoded + extra;
    } catch (const std::ios_base::failure& e) {
        return std::string{"fail ios_base::failure "} + e.what();
    } catch (const std::exception& e) {
        return "fail " + boost::core::demangle(typeid(e).name()) + " " + e.what();
    }
}

std::string Line(const std::string& name, const std::string& kind, const std::vector<unsigned char>& bytes)
{
    return name + " " + kind + " " + HexStr(bytes) + " " + Judge(kind, bytes) + "\n";
}

template <typename Write>
std::vector<unsigned char> Encode(Write&& write)
{
    CDataStream out{SER_DISK, CLIENT_VERSION};
    write(out);
    return {UCharCast(out.data()), UCharCast(out.data()) + out.size()};
}

std::vector<unsigned char> EncodeSnapshot(const evo::EvoSnapshot& snapshot)
{
    return Encode([&](CDataStream& out) { out << snapshot; });
}

} // namespace

BOOST_AUTO_TEST_SUITE(o9b_evo_snapshot_vectors)

BOOST_FIXTURE_TEST_CASE(dump, BasicTestingSetup)
{
    const char* path{std::getenv("O9B_DUMP")};
    if (!path) return;
    std::ofstream out{path, std::ios::binary | std::ios::trunc};
    BOOST_REQUIRE(out);

    const auto populated{SyntheticSnapshot()};
    BOOST_REQUIRE_EQUAL(GetEvoSnapshotHash(populated).ToString(),
                        "5b5d496a66d93775a6a6d1badf2ea2d5bbb25b58031fe0dd2c3c670674c6908d");
    out << Line("populated", "snapshot", EncodeSnapshot(populated));
    out << Line("populated_reverse", "snapshot", EncodeSnapshot(SyntheticSnapshot(/*reverse_representation=*/true)));

    evo::EvoSnapshot empty;
    out << Line("empty", "snapshot", EncodeSnapshot(empty));
    evo::EvoSnapshot list_only;
    list_only.base_block_hash = H(42);
    list_only.mn_list = MNList(list_only.base_block_hash, 500, false);
    out << Line("list_only", "snapshot", EncodeSnapshot(list_only));
    evo::EvoSnapshot signals{list_only};
    signals.mnhf_signals = {{0, 1}, {2, 12}, {9, 30}, {VERSIONBITS_NUM_BITS - 1, 77}};
    signals.credit_pool.locked = 5;
    BOOST_REQUIRE(signals.credit_pool.indexes.Add(3));
    out << Line("signals", "snapshot", EncodeSnapshot(signals));

    out << Line("mnlist_populated", "mnlist",
                Encode([&](CDataStream& s) { evo::SerializeCanonicalMNList(s, populated.mn_list); }));
    for (size_t i{0}; i < populated.historical_mn_list_diffs.size(); ++i) {
        out << Line("mndiff_" + std::to_string(i), "mndiff", Encode([&](CDataStream& s) {
                        evo::SerializeCanonicalMNListDiff(s, populated.historical_mn_list_diffs[i].diff);
                    }));
    }
    for (const auto& data : populated.quorums) {
        const auto type{std::to_string(static_cast<int>(data.llmq_type))};
        for (size_t i{0}; i < data.active_commitments.size(); ++i) {
            out << Line("commitment_" + type + "_active_" + std::to_string(i), "commitment",
                        Encode([&](CDataStream& s) { s << data.active_commitments[i]; }));
        }
        for (size_t i{0}; i < data.rotation_snapshots.size(); ++i) {
            out << Line("rotation_" + type + "_" + std::to_string(i), "rotation:" + type,
                        Encode([&](CDataStream& s) { evo::WriteRotationSnapshot(s, data.rotation_snapshots[i]); }));
        }
    }

    // rotation_bitset_larger_than_quorum_roundtrips's entry.
    const auto& params{evo::SnapshotLLMQParams(Consensus::LLMQType::LLMQ_TEST_DIP0024)};
    evo::QuorumSnapshotEntry entry;
    entry.cycle_base_block_hash = H(1);
    entry.work_block_hash = H(2);
    entry.snapshot.activeQuorumMembers.resize(params.size + 3);
    entry.snapshot.activeQuorumMembers[params.size + 1] = true;
    entry.snapshot.mnSkipListMode = SnapshotSkipMode::MODE_NO_SKIPPING;
    out << Line("rotation_bitset_larger_than_quorum",
                "rotation:" + std::to_string(static_cast<int>(Consensus::LLMQType::LLMQ_TEST_DIP0024)),
                Encode([&](CDataStream& s) { evo::WriteRotationSnapshot(s, entry); }));
    BOOST_REQUIRE(out.flush());
}

BOOST_FIXTURE_TEST_CASE(judge, BasicTestingSetup)
{
    const char* in_path{std::getenv("O9B_JUDGE")};
    const char* out_path{std::getenv("O9B_VERDICTS")};
    if (!in_path || !out_path) return;
    std::ifstream in{in_path};
    std::ofstream out{out_path, std::ios::binary | std::ios::trunc};
    BOOST_REQUIRE(in);
    BOOST_REQUIRE(out);
    std::string line;
    while (std::getline(in, line)) {
        if (line.empty() || line[0] == '#') continue;
        std::istringstream fields{line};
        std::string name, kind, hex;
        BOOST_REQUIRE(fields >> name >> kind);
        fields >> hex;
        const auto bytes{TryParseHex<unsigned char>(hex)};
        BOOST_REQUIRE_MESSAGE(bytes, "bad hex for " + name);
        out << name << " " << kind << " " << Sha256(MakeByteSpan(*bytes)).ToString() << " " << Judge(kind, *bytes)
            << "\n";
    }
    BOOST_REQUIRE(out.flush());
}

BOOST_AUTO_TEST_SUITE_END()
