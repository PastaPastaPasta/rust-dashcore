#include <bls/bls.h>
#include <llmq/signhash.h>

#include <array>
#include <cstdio>
#include <limits>
#include <string>

struct Ban {
    uint256 m_protx_hash;
    int32_t m_requested_height;
    CBLSSignature m_signature;
};
struct Commitment {
    uint256 quorumHash;
    CBLSPublicKey quorumPublicKey;
};
struct Quorum {
    Commitment* qc;
};

template <typename Range>
std::string Hex(const Range& bytes)
{
    std::string out;
    constexpr char alphabet[] = "0123456789abcdef";
    for (unsigned char byte : bytes) {
        out += alphabet[byte >> 4];
        out += alphabet[byte & 15];
    }
    return out;
}

void Record(const char* name, bool legacy, Consensus::LLMQType llmq_type,
            Ban ban_msg, Quorum* quorum, const CBLSSecretKey& key,
            const CBLSSignature* original = nullptr, bool corrupt = false)
{
// Core: v24.0.0 net_processing.cpp:3906-3911.
    ban_msg.m_signature = original ? *original : key.Sign(signHash.Get(), legacy);
    auto signature_bytes = ban_msg.m_signature.ToBytes(legacy);
    if (corrupt) {
        signature_bytes.back() ^= 1;
        ban_msg.m_signature.SetBytes(signature_bytes, legacy);
    }
    const bool valid = CORE_VERIFY;
    std::printf("{\"case\":\"%s\",\"legacy\":%s,\"llmq_type\":%u,"
                "\"protx\":\"%s\",\"height\":%d,\"quorum_hash\":\"%s\","
                "\"public_key\":\"%s\",\"signature\":\"%s\","
                "\"request_id\":\"%s\",\"msg_hash\":\"%s\",\"sign_hash\":\"%s\","
                "\"valid\":%s}\n",
                name, legacy ? "true" : "false", static_cast<unsigned>(llmq_type),
                Hex(ban_msg.m_protx_hash).c_str(), ban_msg.m_requested_height,
                Hex(quorum->qc->quorumHash).c_str(),
                Hex(quorum->qc->quorumPublicKey.ToBytes(legacy)).c_str(),
                Hex(signature_bytes).c_str(),
                Hex(request_id).c_str(), Hex(msg_hash).c_str(), Hex(signHash.Get()).c_str(),
                valid ? "true" : "false");
}

int main()
{
    std::array<uint8_t, 32> pro{}, hash{}, scalar{};
    for (size_t i = 0; i < pro.size(); ++i) {
        pro[i] = i;
        hash[i] = 63 - i;
    }
    scalar.back() = 1;
    CBLSSecretKey key;
    key.SetBytes(scalar, false);
    scalar.back() = 2;
    CBLSSecretKey other;
    other.SetBytes(scalar, false);
    for (bool legacy : {false, true}) {
        bls::bls_legacy_scheme.store(legacy);
        Ban ban{uint256{Span<const uint8_t>{pro}}, 123456, {}};
        Commitment qc{uint256{Span<const uint8_t>{hash}}, key.GetPublicKey()};
        Quorum q{&qc};
        const auto type = Consensus::LLMQType::LLMQ_TEST_PLATFORM;
        const auto data = std::make_pair(ban.m_protx_hash, ban.m_requested_height);
        const auto id = SerializeHash(std::make_pair(std::string("PlatformPoSeBan"), data));
        const auto sign_hash = llmq::SignHash(type, qc.quorumHash, id, SerializeHash(data));
        const auto sig = key.Sign(sign_hash.Get(), legacy);
        Record("valid", legacy, type, ban, &q, key, &sig);
        ban.m_protx_hash.begin()[0] ^= 1;
        Record("wrong_protx", legacy, type, ban, &q, key, &sig);
        ban.m_protx_hash.begin()[0] ^= 1;
        ++ban.m_requested_height;
        Record("wrong_height", legacy, type, ban, &q, key, &sig);
        --ban.m_requested_height;
        qc.quorumHash.begin()[0] ^= 1;
        Record("wrong_quorum_hash", legacy, type, ban, &q, key, &sig);
        qc.quorumHash.begin()[0] ^= 1;
        qc.quorumPublicKey = other.GetPublicKey();
        Record("wrong_key", legacy, type, ban, &q, key, &sig);
        qc.quorumPublicKey = key.GetPublicKey();
        Record("corrupted_signature", legacy, type, ban, &q, key, &sig, true);
        Record("wrong_type", legacy, Consensus::LLMQType::LLMQ_100_67, ban, &q, key, &sig);
        Record("type_100_67", legacy, Consensus::LLMQType::LLMQ_100_67, ban, &q, key);
        Record("type_25_67", legacy, Consensus::LLMQType::LLMQ_25_67, ban, &q, key);
        for (int32_t height : {0, -1, std::numeric_limits<int32_t>::min(), std::numeric_limits<int32_t>::max()}) {
            ban.m_requested_height = height;
            Record("height", legacy, type, ban, &q, key);
        }
    }
}
