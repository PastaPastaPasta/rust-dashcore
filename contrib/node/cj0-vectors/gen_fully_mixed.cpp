// Dumps Dash Core's IsFullyMixed coin flip (src/wallet/coinjoin.cpp,
// CWallet::IsFullyMixed) over a fixed grid of outpoints and salts as JSON, for
// dash-coinjoin's tests (CJ0). Built against the pinned Core sources by
// regen.sh with crypto/sha256.cpp; the stream, serialisation and hash are
// Core's own. The lines below are copied from IsFullyMixed (regen.sh checks
// that the pinned wallet/coinjoin.cpp still has them):
//   CDataStream ss(SER_GETHASH, PROTOCOL_VERSION);
//   ss << outpoint << nCoinJoinSalt;
//   CSHA256().Write(ss.data(), ss.size()).Finalize(nHash.begin());
//   ReadLE64(nHash.begin()) % 2 == 0  -> mix again
#include <crypto/common.h>
#include <crypto/sha256.h>
#include <primitives/transaction.h>
#include <streams.h>
#include <uint256.h>
#include <version.h>

#include <cinttypes>
#include <cstdint>
#include <cstdio>
#include <cstring>

static void hex(const unsigned char* p, size_t n)
{
    for (size_t i = 0; i < n; ++i) std::printf("%02x", p[i]);
}

int main()
{
    const uint32_t indexes[] = {0, 1, 2, 7, 255, 256, 0x7fffffff, 0xfffffffe, 0xffffffff};
    std::printf("{\n  \"rows\": [\n");
    bool first = true;
    for (int i = 0; i < 24; ++i) {
        uint256 txid;
        uint256 salt;
        for (int k = 0; k < 32; ++k) {
            txid.begin()[k] = static_cast<unsigned char>((i * 31 + k * 7 + 1) & 0xff);
            salt.begin()[k] = static_cast<unsigned char>((i * 13 + k * 3 + 5) & 0xff);
        }
        if (i == 0) salt.SetNull();
        for (uint32_t n : indexes) {
            const COutPoint outpoint(txid, n);
            CDataStream ss(SER_GETHASH, PROTOCOL_VERSION);
            ss << outpoint << salt;
            uint256 nHash;
            CSHA256().Write(reinterpret_cast<const uint8_t*>(ss.data()), ss.size()).Finalize(nHash.begin());
            const uint64_t le64 = ReadLE64(nHash.begin());
            // [txid bytes as stored, index, salt bytes as stored, ReadLE64, stops mixing]
            std::printf("%s   [\"", first ? "" : ",\n");
            hex(txid.begin(), 32);
            std::printf("\",%" PRIu32 ",\"", n);
            hex(salt.begin(), 32);
            std::printf("\",\"%" PRIu64 "\",%s]", le64, le64 % 2 == 0 ? "false" : "true");
            first = false;
        }
    }
    std::printf("\n  ]\n}\n");
    return 0;
}
