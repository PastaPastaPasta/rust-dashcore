// The A4 fixture's damage (CH5-1b): one coin in a stopped dashd's coins database rewritten to its
// value plus `delta` duffs, through Core's own record layout (`txdb.cpp` `CoinEntry`, `coins.h`
// `Coin`, `compressor.h` `CTxOutCompressor`, `serialize.h` VARINT) and the database's obfuscation
// key (`dbwrapper.cpp`). Everything else in the record is kept byte for byte. Tables are written
// uncompressed, as Core's LevelDB is built. Build: g++ -O2 -o coinvalue coinvalue.cpp -lleveldb
// Usage: coinvalue <chainstate dir> <txid, display hex> <vout> <delta>
#include <leveldb/db.h>
#include <leveldb/write_batch.h>

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <string>

static uint64_t read_varint(const std::string& s, size_t& at) {
    uint64_t n = 0;
    while (true) {
        unsigned char ch = s.at(at++);
        n = (n << 7) | (ch & 0x7F);
        if (ch & 0x80) {
            n++;
        } else {
            return n;
        }
    }
}

static std::string varint(uint64_t n) {
    unsigned char tmp[10];
    int len = 0;
    while (true) {
        tmp[len] = (n & 0x7F) | (len ? 0x80 : 0x00);
        if (n <= 0x7F) break;
        n = (n >> 7) - 1;
        len++;
    }
    std::string out;
    for (int i = len; i >= 0; i--) out.push_back(static_cast<char>(tmp[i]));
    return out;
}

static uint64_t compress_amount(uint64_t n) {
    if (n == 0) return 0;
    int e = 0;
    while (((n % 10) == 0) && e < 9) {
        n /= 10;
        e++;
    }
    if (e < 9) {
        int d = (n % 10);
        n /= 10;
        return 1 + (n * 9 + d - 1) * 10 + e;
    }
    return 1 + (n - 1) * 10 + 9;
}

static uint64_t decompress_amount(uint64_t x) {
    if (x == 0) return 0;
    x--;
    int e = x % 10;
    x /= 10;
    uint64_t n = 0;
    if (e < 9) {
        int d = (x % 9) + 1;
        x /= 9;
        n = x * 10 + d;
    } else {
        n = x + 1;
    }
    while (e) {
        n *= 10;
        e--;
    }
    return n;
}

static std::string xored(std::string v, const std::string& key) {
    if (key.empty()) return v;
    for (size_t i = 0; i < v.size(); i++) v[i] ^= key[i % key.size()];
    return v;
}

int main(int argc, char** argv) {
    if (argc != 5) {
        fprintf(stderr, "usage: coinvalue <chainstate> <txid> <vout> <delta>\n");
        return 2;
    }
    std::string hex = argv[2];
    if (hex.size() != 64) return 2;
    std::string txid(32, '\0');
    for (int i = 0; i < 32; i++) {
        // Display order is the reverse of the stored one.
        txid[31 - i] = static_cast<char>(std::stoi(hex.substr(2 * i, 2), nullptr, 16));
    }
    uint64_t vout = std::strtoull(argv[3], nullptr, 10);
    int64_t delta = std::strtoll(argv[4], nullptr, 10);

    leveldb::Options options;
    options.compression = leveldb::kNoCompression;
    leveldb::DB* db = nullptr;
    leveldb::Status st = leveldb::DB::Open(options, argv[1], &db);
    if (!st.ok()) {
        fprintf(stderr, "open: %s\n", st.ToString().c_str());
        return 1;
    }
    std::string obf_key = std::string("\x0e\x00obfuscate_key", 15);
    std::string obf_val;
    std::string key;
    if (db->Get(leveldb::ReadOptions(), obf_key, &obf_val).ok()) {
        // A serialized vector: its one-byte length (8), then the key.
        key = obf_val.substr(1);
    }
    std::string coin_key = "C" + txid + varint(vout);
    std::string raw;
    st = db->Get(leveldb::ReadOptions(), coin_key, &raw);
    if (!st.ok()) {
        fprintf(stderr, "no coin: %s\n", st.ToString().c_str());
        return 1;
    }
    std::string v = xored(raw, key);
    size_t at = 0;
    uint64_t code = read_varint(v, at);
    size_t amount_at = at;
    uint64_t value = decompress_amount(read_varint(v, at));
    std::string rest = v.substr(at);
    uint64_t now = static_cast<uint64_t>(static_cast<int64_t>(value) + delta);
    std::string out = v.substr(0, amount_at) + varint(compress_amount(now)) + rest;
    leveldb::WriteBatch batch;
    batch.Put(coin_key, xored(out, key));
    st = db->Write(leveldb::WriteOptions(), &batch);
    if (!st.ok()) {
        fprintf(stderr, "write: %s\n", st.ToString().c_str());
        return 1;
    }
    printf("height=%llu coinbase=%llu value=%llu -> %llu\n",
           static_cast<unsigned long long>(code >> 1), static_cast<unsigned long long>(code & 1),
           static_cast<unsigned long long>(value), static_cast<unsigned long long>(now));
    delete db;
    return 0;
}
