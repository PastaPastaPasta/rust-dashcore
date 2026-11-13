// CH5-1a fixture tool: damages a stopped dashd's regtest datadir through Dash Core's own code.
//
// Built from Dash Core v24.0.0-rc.5 headers (git archive of the tag) and linked against the
// static libraries of an rc.5-based build (see PROVENANCE.md). Every byte it writes is written
// by Core's serializers: CCoinsViewDB/CCoinsViewCache::Flush for the coins database, and the
// CBlockUndo serializer plus HashWriter checksum exactly as UndoWriteToDisk writes them
// (node/blockstorage.cpp:511-540).
//
// Modes:
//   coin-value  <chainstate dir> <txid> <n> <delta>   rewrite a coin's nValue += delta
//   coin-remove <chainstate dir> <txid> <n>           erase a coin
//   undo-scan   <rev file> <hashes file>               list records: offset size prevhash
//   undo-script <rev file> <data offset> <prevhash> <vtxundo idx> <vprevout idx> <hash160 hex>
//                                                      restore that input as P2PKH(hash160)

#include <coins.h>
#include <hash.h>
#include <primitives/transaction.h>
#include <script/script.h>
#include <script/standard.h>
#include <streams.h>
#include <txdb.h>
#include <undo.h>
#include <util/strencodings.h>
#include <util/translation.h>
#include <clientversion.h>

#include <cstdio>
#include <fstream>
#include <iostream>
#include <stdexcept>

const std::function<std::string(const char*)> G_TRANSLATION_FUN = nullptr;

static uint256 ParseHash(const std::string& s)
{
    auto h = uint256S(s);
    if (h.GetHex() != s) throw std::runtime_error("bad hash " + s);
    return h;
}

static int CoinEdit(const fs::path& dir, const COutPoint& op, bool remove, CAmount delta)
{
    CCoinsViewDB db(dir, 8 << 20, false, false);
    CCoinsViewCache cache(&db);
    const uint256 best = cache.GetBestBlock();
    Coin coin;
    if (!cache.GetCoin(op, coin)) {
        std::cerr << "coin not found\n";
        return 2;
    }
    std::cout << "before: value=" << coin.out.nValue << " height=" << coin.nHeight
              << " coinbase=" << coin.fCoinBase << " script=" << HexStr(coin.out.scriptPubKey)
              << " best=" << best.GetHex() << "\n";
    cache.SpendCoin(op);
    if (!remove) {
        coin.out.nValue += delta;
        cache.AddCoin(op, std::move(coin), /*possible_overwrite=*/true);
    }
    if (!cache.Flush()) {
        std::cerr << "flush failed\n";
        return 3;
    }
    Coin after;
    CCoinsViewCache check(&db);
    if (check.GetCoin(op, after)) {
        std::cout << "after: value=" << after.out.nValue << "\n";
    } else {
        std::cout << "after: absent\n";
    }
    if (check.GetBestBlock() != best) throw std::runtime_error("best block moved");
    return 0;
}

struct Record {
    long header;  // offset of the message start
    long data;    // offset of the CBlockUndo
    uint32_t size;
    uint256 checksum;
};

static std::vector<Record> ScanRev(const std::string& path)
{
    std::vector<Record> out;
    FILE* f = fopen(path.c_str(), "rb");
    if (!f) throw std::runtime_error("open " + path);
    CAutoFile file(f, SER_DISK, CLIENT_VERSION);
    for (;;) {
        long header = ftell(file.Get());
        unsigned char magic[4];
        if (fread(magic, 1, 4, file.Get()) != 4) break;
        if (magic[0] == 0 && magic[1] == 0 && magic[2] == 0 && magic[3] == 0) break;  // preallocated tail
        uint32_t size;
        file >> size;
        long data = ftell(file.Get());
        fseek(file.Get(), size, SEEK_CUR);
        uint256 checksum;
        file >> checksum;
        out.push_back({header, data, size, checksum});
    }
    return out;
}

static CBlockUndo ReadUndo(const std::string& path, long data, uint32_t size)
{
    FILE* f = fopen(path.c_str(), "rb");
    if (!f) throw std::runtime_error("open " + path);
    CAutoFile file(f, SER_DISK, CLIENT_VERSION);
    fseek(file.Get(), data, SEEK_SET);
    CBlockUndo undo;
    file >> undo;
    if (ftell(file.Get()) != data + (long)size) throw std::runtime_error("size mismatch on read");
    return undo;
}

static uint256 Checksum(const uint256& prev, const CBlockUndo& undo)
{
    HashWriter hasher{};
    hasher << prev;
    hasher << undo;
    return hasher.GetHash();
}

static int UndoScan(const std::string& rev, const std::string& hashes_path)
{
    std::vector<uint256> hashes;
    std::ifstream in(hashes_path);
    for (std::string line; std::getline(in, line);) {
        if (!line.empty()) hashes.push_back(ParseHash(line));
    }
    for (const auto& r : ScanRev(rev)) {
        CBlockUndo undo = ReadUndo(rev, r.data, r.size);
        std::string prev = "?";
        for (const auto& h : hashes) {
            if (Checksum(h, undo) == r.checksum) {
                prev = h.GetHex();
                break;
            }
        }
        size_t inputs = 0;
        for (const auto& t : undo.vtxundo) inputs += t.vprevout.size();
        std::cout << r.data << " " << r.size << " " << prev << " txs=" << undo.vtxundo.size()
                  << " inputs=" << inputs << "\n";
    }
    return 0;
}

static int UndoScript(const std::string& rev, long data, const uint256& prev, size_t tx, size_t in,
                      const std::string& hash160)
{
    std::vector<Record> records = ScanRev(rev);
    const Record* rec = nullptr;
    for (const auto& r : records) {
        if (r.data == data) rec = &r;
    }
    if (!rec) throw std::runtime_error("no record at that offset");
    CBlockUndo undo = ReadUndo(rev, rec->data, rec->size);
    if (Checksum(prev, undo) != rec->checksum) throw std::runtime_error("checksum does not match prevhash");
    Coin& coin = undo.vtxundo.at(tx).vprevout.at(in);
    std::cout << "before: value=" << coin.out.nValue << " height=" << coin.nHeight
              << " script=" << HexStr(coin.out.scriptPubKey) << "\n";
    const auto bytes = ParseHex(hash160);
    if (bytes.size() != 20) throw std::runtime_error("hash160 must be 20 bytes");
    uint160 h;
    std::copy(bytes.begin(), bytes.end(), h.begin());
    coin.out.scriptPubKey = GetScriptForDestination(PKHash(h));
    std::cout << "after:  value=" << coin.out.nValue << " height=" << coin.nHeight
              << " script=" << HexStr(coin.out.scriptPubKey) << "\n";
    const uint32_t size = GetSerializeSize(undo, CLIENT_VERSION);
    if (size != rec->size) throw std::runtime_error("serialized size changed; refusing an in-place rewrite");
    FILE* f = fopen(rev.c_str(), "rb+");
    if (!f) throw std::runtime_error("open rb+ " + rev);
    CAutoFile file(f, SER_DISK, CLIENT_VERSION);
    fseek(file.Get(), rec->data, SEEK_SET);
    // UndoWriteToDisk's body after the index header (node/blockstorage.cpp:531-537).
    file << undo;
    file << Checksum(prev, undo);
    if (fflush(file.Get()) != 0) throw std::runtime_error("fflush");
    return 0;
}

int main(int argc, char** argv)
{
    try {
        const std::string mode = argc > 1 ? argv[1] : "";
        if (mode == "coin-value" && argc == 6) {
            return CoinEdit(fs::PathFromString(argv[2]), COutPoint(ParseHash(argv[3]), std::stoul(argv[4])), false,
                            std::stoll(argv[5]));
        }
        if (mode == "coin-remove" && argc == 5) {
            return CoinEdit(fs::PathFromString(argv[2]), COutPoint(ParseHash(argv[3]), std::stoul(argv[4])), true, 0);
        }
        if (mode == "undo-scan" && argc == 4) return UndoScan(argv[2], argv[3]);
        if (mode == "undo-script" && argc == 8) {
            return UndoScript(argv[2], std::stol(argv[3]), ParseHash(argv[4]), std::stoul(argv[5]),
                              std::stoul(argv[6]), argv[7]);
        }
        std::cerr << "usage: see the header comment\n";
        return 64;
    } catch (const std::exception& e) {
        std::cerr << "error: " << e.what() << "\n";
        return 1;
    }
}
