// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

// The disconnect-pool vectors of rust-dashcore's CH3b (node program, CH3 §4.3): what Core's
// DisconnectedBlockTransactions, its memory accounting and libstdc++'s std::unordered_map
// compute, for the Rust port to match. Built only on request (Makefile.ch3b.include):
//
//   make -C src test/ch3b/gen_dbt_vectors && src/test/ch3b/gen_dbt_vectors <outdir>
//
// It writes buckets.txt, container.txt and pool.txt into <outdir>. Nothing in them depends on
// the Core commit, the time or the run, so the same source built by the same compiler prints the
// same bytes on every checkout whose pool, accounting and transaction format are the same. Each
// file's header names its columns.
//
// The pool is Core's own class, unmodified. Its map, list and cached usage are private; the
// generator reads them through pointers to members taken in explicit template instantiations,
// where access is not checked ([temp.spec.general]).

#include <core_memusage.h>
#include <crypto/common.h>
#include <crypto/sha256.h>
#include <kernel/disconnected_transactions.h>
#include <memusage.h>
#include <primitives/transaction.h>
#include <serialize.h>
#include <streams.h>
#include <uint256.h>
#include <util/hasher.h>
#include <util/strencodings.h>
#include <util/translation.h>
#include <version.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <functional>
#include <fstream>
#include <iterator>
#include <limits>
#include <list>
#include <memory>
#include <sstream>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

const std::function<std::string(const char*)> G_TRANSLATION_FUN = nullptr;

namespace {

using Queue = std::list<CTransactionRef>;
using Map = std::unordered_map<uint256, Queue::iterator, SaltedTxidHasher>;

// Pointers to the pool's private members.
template <typename Tag, typename Tag::type Member>
struct Expose {
    friend typename Tag::type Get(Tag) { return Member; }
};
struct QueueTag {
    using type = Queue DisconnectedBlockTransactions::*;
    friend type Get(QueueTag);
};
struct MapTag {
    using type = Map DisconnectedBlockTransactions::*;
    friend type Get(MapTag);
};
struct InnerTag {
    using type = uint64_t DisconnectedBlockTransactions::*;
    friend type Get(InnerTag);
};
template struct Expose<QueueTag, &DisconnectedBlockTransactions::queuedTx>;
template struct Expose<MapTag, &DisconnectedBlockTransactions::iters_by_txid>;
template struct Expose<InnerTag, &DisconnectedBlockTransactions::cachedInnerUsage>;

[[noreturn]] void Fail(const std::string& what)
{
    std::fprintf(stderr, "gen_dbt_vectors: %s\n", what.c_str());
    std::exit(1);
}

#define CHECK(cond)                                                                      \
    do {                                                                                 \
        if (!(cond)) Fail(std::string{"check failed at line "} + std::to_string(__LINE__) + ": " #cond); \
    } while (0)

// splitmix64, seeded per section: the only randomness that reaches the output. (SaltedTxidHasher's
// random salts move entries between buckets, never the bucket count, and no map is iterated.)
struct Rng {
    uint64_t state;
    uint64_t Next()
    {
        uint64_t z = (state += 0x9e3779b97f4a7c15ULL);
        z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ULL;
        z = (z ^ (z >> 27)) * 0x94d049bb133111ebULL;
        return z ^ (z >> 31);
    }
    uint64_t Below(uint64_t n) { return Next() % n; }
    uint64_t Range(uint64_t lo, uint64_t hi) { return lo + Below(hi - lo + 1); }
    std::vector<unsigned char> Bytes(size_t n)
    {
        std::vector<unsigned char> out(n);
        for (auto& b : out) b = static_cast<unsigned char>(Next());
        return out;
    }
};

std::string Header(const std::string& what)
{
    std::ostringstream s;
    s << "# " << what << "\n"
      << "# Produced by Dash Core's src/test/ch3b/gen_dbt_vectors.cpp (rust-dashcore CH3b); see PROVENANCE.md.\n"
      << "# g++ " << __VERSION__ << ", __GLIBCXX__ " << __GLIBCXX__ << ", sizeof(void*) " << sizeof(void*) << "\n";
    return s.str();
}

// Indices as runs: "a-b" from a to b in steps of one, up or down; "a" alone; comma-separated;
// "-" for none.
std::string Runs(const std::vector<uint64_t>& v)
{
    if (v.empty()) return "-";
    std::string out;
    size_t i = 0;
    while (i < v.size()) {
        size_t j = i;
        if (i + 1 < v.size() && (v[i + 1] == v[i] + 1 || v[i + 1] + 1 == v[i])) {
            const uint64_t step = v[i + 1] - v[i];
            j = i + 1;
            while (j + 1 < v.size() && v[j + 1] - v[j] == step) ++j;
        }
        if (!out.empty()) out += ",";
        out += std::to_string(v[i]);
        if (j > i) out += "-" + std::to_string(v[j]);
        i = j + 1;
    }
    return out;
}

// buckets.txt: _M_next_bkt over every size_t.
void WriteBuckets(std::ostream& out)
{
    out << Header("libstdc++'s std::__detail::_Prime_rehash_policy::_M_next_bkt(n) at max_load_factor 1.0, for every n from 0 to 2^64 - 1.")
        << "# Every n below 2^24 is called one by one; above, a run is [n, _M_next_bkt(n)] (the smallest listed prime at least n),\n"
        << "# checked at both ends and at 64 random n inside, and the last run reaches 2^64 - 1.\n"
        << "# run <first n> <last n> <buckets> <_M_next_resize after the call on a new policy>\n";
    struct Run {
        uint64_t first, last, buckets, resize;
    };
    const auto probe = [](uint64_t n) {
        std::__detail::_Prime_rehash_policy policy;
        const uint64_t buckets = policy._M_next_bkt(n);
        return std::pair<uint64_t, uint64_t>{buckets, policy._M_state()};
    };
    std::vector<Run> runs;
    const auto extend = [&](uint64_t first, uint64_t last, std::pair<uint64_t, uint64_t> at) {
        if (!runs.empty() && runs.back().last + 1 == first && runs.back().buckets == at.first && runs.back().resize == at.second) {
            runs.back().last = last;
        } else {
            runs.push_back({first, last, at.first, at.second});
        }
    };
    constexpr uint64_t EXHAUSTIVE{uint64_t{1} << 24};
    for (uint64_t n = 0; n < EXHAUSTIVE; ++n) extend(n, n, probe(n));
    Rng rng{24};
    for (uint64_t n = EXHAUSTIVE;;) {
        const auto at = probe(n);
        const uint64_t last = at.first >= n ? at.first : std::numeric_limits<uint64_t>::max();
        CHECK(probe(last) == at);
        for (int i = 0; i < 64; ++i) CHECK(probe(rng.Range(n, last)) == at);
        extend(n, last, at);
        if (last == std::numeric_limits<uint64_t>::max()) break;
        n = last + 1;
    }
    for (const auto& r : runs) out << "run " << r.first << " " << r.last << " " << r.buckets << " " << r.resize << "\n";
}

uint256 Key(uint64_t i)
{
    uint256 key;
    WriteLE64(key.begin(), i);
    return key;
}

// container.txt: the pool's map type, on its own.
void WriteContainer(std::ostream& out)
{
    out << Header("std::unordered_map<uint256, std::list<CTransactionRef>::iterator, SaltedTxidHasher>, the pool's map: its bucket_count().")
        << "# grow <size> <buckets>: a new map, one emplace of a new key at a time up to 2^21 entries; a row at 0 and wherever the count changes.\n"
        << "# seq <seed>: a new map, then 150 rows <op> <n> <size after> <buckets after>, where op is reserve (reserve(size + n)),\n"
        << "# insert (n emplaces of new keys), dup (an emplace of a present key), erase (n present keys) or clear (clear()).\n";
    Queue list{nullptr};
    const auto it = list.begin();
    {
        Map map;
        size_t buckets = map.bucket_count();
        out << "grow 0 " << buckets << "\n";
        for (uint64_t i = 0; i < (uint64_t{1} << 21); ++i) {
            CHECK(map.emplace(Key(i), it).second);
            if (map.bucket_count() != buckets) {
                buckets = map.bucket_count();
                out << "grow " << map.size() << " " << buckets << "\n";
            }
        }
    }
    for (uint64_t seed = 1; seed <= 64; ++seed) {
        Rng rng{seed};
        Map map;
        std::vector<uint64_t> present;
        uint64_t next_key = 0;
        const auto amount = [&] {
            switch (rng.Below(4)) {
            case 0: return rng.Range(0, 3);
            case 1: return rng.Range(4, 40);
            case 2: return rng.Range(41, 600);
            default: return rng.Range(601, 6000);
            }
        };
        out << "seq " << seed << "\n";
        for (int op = 0; op < 150; ++op) {
            const uint64_t pick = rng.Below(100);
            std::string name;
            uint64_t n = 0;
            if (pick < 30) {
                name = "reserve";
                n = amount();
                map.reserve(map.size() + n);
            } else if (pick < 60 || (pick < 65 && present.empty())) {
                name = "insert";
                n = amount();
                for (uint64_t i = 0; i < n; ++i) {
                    CHECK(map.emplace(Key(next_key), it).second);
                    present.push_back(next_key++);
                }
            } else if (pick < 65) {
                name = "dup";
                CHECK(!map.emplace(Key(present[rng.Below(present.size())]), it).second);
            } else if (pick < 95) {
                name = "erase";
                n = std::min<uint64_t>(amount(), present.size());
                for (uint64_t i = 0; i < n; ++i) {
                    std::swap(present[rng.Below(present.size())], present.back());
                    CHECK(map.erase(Key(present.back())) == 1);
                    present.pop_back();
                }
            } else {
                name = "clear";
                map.clear();
                present.clear();
            }
            out << name << " " << n << " " << map.size() << " " << map.bucket_count() << "\n";
        }
    }
}

// A transaction as a block on disk holds it: serialized, then read back the way
// ReadBlockFromDisk reads a block's vtx.
CTransactionRef ReadBack(const CMutableTransaction& mtx, std::vector<unsigned char>& bytes)
{
    CDataStream written(SER_DISK, PROTOCOL_VERSION);
    written << mtx;
    bytes.assign(UCharCast(written.data()), UCharCast(written.data()) + written.size());
    CDataStream read(Span<const uint8_t>{bytes}, SER_DISK, PROTOCOL_VERSION);
    CTransactionRef tx;
    read >> tx;
    CHECK(read.empty());
    CHECK(tx->vin.capacity() == tx->vin.size() && tx->vout.capacity() == tx->vout.size());
    return tx;
}

std::vector<unsigned char> Filled(size_t n) { return std::vector<unsigned char>(n, 0x51); }

// A bulk transaction (pool.txt's bulk rows): version 1, type 0, lock time 0; input j spends
// (LE64(k) || LE32(j) || 20 zero bytes, j) with a scriptSig of sig_len 0x51 bytes and sequence
// 0xffffffff; every output pays k duffs to spk_len 0x51 bytes.
CMutableTransaction Bulk(uint64_t k, size_t n_in, size_t n_out, size_t sig_len, size_t spk_len)
{
    CMutableTransaction mtx;
    mtx.nVersion = 1;
    mtx.nType = 0;
    mtx.nLockTime = 0;
    for (size_t j = 0; j < n_in; ++j) {
        uint256 prev;
        WriteLE64(prev.begin(), k);
        WriteLE32(prev.begin() + 8, static_cast<uint32_t>(j));
        const auto sig = Filled(sig_len);
        mtx.vin.emplace_back(COutPoint{prev, static_cast<uint32_t>(j)}, CScript(sig.begin(), sig.end()), 0xffffffff);
    }
    for (size_t j = 0; j < n_out; ++j) {
        const auto spk = Filled(spk_len);
        mtx.vout.emplace_back(static_cast<CAmount>(k), CScript(spk.begin(), spk.end()));
    }
    return mtx;
}

struct Corpus {
    // By index: pool.txt's tx rows, then its bulk rows.
    std::unordered_map<uint64_t, CTransactionRef> txs;
    std::unordered_map<const CTransaction*, uint64_t> index_of;
    // Corpus indices the random scenarios add: one transaction per txid.
    std::vector<uint64_t> addable;
    // The re-signed asset unlocks: (the original's index, the twin's).
    std::vector<std::pair<uint64_t, uint64_t>> twins;

    void Put(uint64_t i, CTransactionRef tx)
    {
        index_of.emplace(tx.get(), i);
        txs.emplace(i, std::move(tx));
    }
    std::vector<uint64_t> Indices(const std::vector<CTransactionRef>& v) const
    {
        std::vector<uint64_t> out;
        for (const auto& tx : v) out.push_back(index_of.at(tx.get()));
        return out;
    }
    std::vector<CTransactionRef> Refs(const std::vector<uint64_t>& v) const
    {
        std::vector<CTransactionRef> out;
        for (const auto i : v) out.push_back(txs.at(i));
        return out;
    }
};

constexpr size_t SCRIPT_LENGTHS[]{0, 1, 2, 27, 28, 29, 30, 44, 45, 46, 47, 48, 60, 61, 62, 72, 100, 107, 520, 1650, 10000};

CScript RandomScript(Rng& rng, size_t len)
{
    const auto bytes = rng.Bytes(len);
    return CScript(bytes.begin(), bytes.end());
}

// An input spending a random outpoint with a random len-byte scriptSig, drawn in that order.
void AddIn(Rng& rng, CMutableTransaction& mtx, size_t len, uint32_t sequence)
{
    uint256 hash;
    const auto bytes = rng.Bytes(32);
    std::memcpy(hash.begin(), bytes.data(), 32);
    const auto n = static_cast<uint32_t>(rng.Below(50));
    mtx.vin.emplace_back(COutPoint{hash, n}, RandomScript(rng, len), sequence);
}

// An output of a random value below max_value to a random len-byte script, drawn in that order.
void AddOut(Rng& rng, CMutableTransaction& mtx, uint64_t max_value, size_t len)
{
    const auto value = static_cast<CAmount>(rng.Below(max_value));
    mtx.vout.emplace_back(value, RandomScript(rng, len));
}

// The tx rows: shapes for RecursiveDynamicUsage, and transactions for the scenarios.
void BuildCorpus(Corpus& corpus, std::ostream& out)
{
    Rng rng{7};
    std::vector<CMutableTransaction> list;
    const auto small_len = [&] { return SCRIPT_LENGTHS[rng.Below(17)]; };
    // Every script length on both sides.
    for (const size_t len : SCRIPT_LENGTHS) {
        CMutableTransaction mtx;
        mtx.nVersion = 2;
        AddIn(rng, mtx, len, 0xfffffffe);
        AddOut(rng, mtx, 1000000, len);
        list.push_back(mtx);
    }
    // Input and output counts, including none; 1,000 outputs only beside at most one input, which
    // keeps pool.txt under the repository's 1,000 KB file limit.
    for (const size_t n_in : {0, 1, 2, 3, 7, 100}) {
        for (const size_t n_out : {0, 1, 2, 5, 100, 1000}) {
            if (n_out == 1000 && n_in > 1) continue;
            CMutableTransaction mtx;
            mtx.nVersion = static_cast<int16_t>(rng.Range(1, 2));
            mtx.nLockTime = static_cast<uint32_t>(rng.Below(500000));
            for (size_t j = 0; j < n_in; ++j) {
                const size_t len = small_len();
                AddIn(rng, mtx, len, 0xffffffff);
            }
            for (size_t j = 0; j < n_out; ++j) {
                const size_t len = n_out >= 100 ? SCRIPT_LENGTHS[rng.Below(7)] : small_len();
                AddOut(rng, mtx, 1000000, len);
            }
            list.push_back(mtx);
        }
    }
    // Special transactions: their payload is not counted.
    for (const uint16_t type : {0, 1, 2, 3, 4, 5, 6, 7, 8}) {
        CMutableTransaction mtx;
        mtx.nVersion = 3;
        mtx.nType = type;
        const size_t sig_len = small_len();
        AddIn(rng, mtx, sig_len, 0xffffffff);
        const size_t spk_len = small_len();
        AddOut(rng, mtx, 1000000, spk_len);
        if (type != 0) mtx.vExtraPayload = rng.Bytes(rng.Range(0, 400));
        list.push_back(mtx);
    }
    // Coinbase-shaped: a null prevout.
    for (int i = 0; i < 4; ++i) {
        CMutableTransaction mtx;
        mtx.nVersion = 3;
        mtx.nType = i % 2 == 0 ? 5 : 0;
        const size_t sig_len = rng.Range(2, 100);
        mtx.vin.emplace_back(COutPoint{}, RandomScript(rng, sig_len), 0xffffffff);
        for (uint64_t j = 0, n = rng.Range(1, 4); j < n; ++j) {
            const size_t len = small_len();
            AddOut(rng, mtx, 5000000000, len);
        }
        if (mtx.nType != 0) mtx.vExtraPayload = rng.Bytes(rng.Range(70, 200));
        list.push_back(mtx);
    }
    // Asset unlocks: version 1 payloads, and version 2 (a stable txid) each with a re-signed twin.
    std::vector<std::pair<size_t, size_t>> twin_slots;
    for (int i = 0; i < 6; ++i) {
        CMutableTransaction mtx;
        mtx.nVersion = 3;
        mtx.nType = TRANSACTION_ASSET_UNLOCK;
        for (uint64_t j = 0, n = rng.Range(1, 3); j < n; ++j) AddOut(rng, mtx, 100000000, 25);
        mtx.vExtraPayload = rng.Bytes(ASSET_UNLOCK_PAYLOAD_SIZE);
        mtx.vExtraPayload[0] = i < 2 ? 1 : ASSET_UNLOCK_STABLE_TXID_VERSION;
        list.push_back(mtx);
        if (i >= 2) {
            CMutableTransaction twin = mtx;
            const auto info = rng.Bytes(ASSET_UNLOCK_QUORUM_INFO_SIZE);
            std::copy(info.begin(), info.end(), twin.vExtraPayload.end() - ASSET_UNLOCK_QUORUM_INFO_SIZE);
            twin_slots.emplace_back(list.size() - 1, list.size());
            list.push_back(twin);
        }
    }
    // Ordinary transactions, for the scenarios' blocks.
    while (list.size() < 400) {
        CMutableTransaction mtx;
        mtx.nVersion = static_cast<int16_t>(rng.Range(1, 3));
        for (uint64_t j = 0, n = rng.Range(1, 4); j < n; ++j) {
            const size_t len = rng.Below(4) == 0 ? small_len() : rng.Range(100, 110);
            AddIn(rng, mtx, len, 0xffffffff);
        }
        for (uint64_t j = 0, n = rng.Range(1, 4); j < n; ++j) {
            const size_t len = rng.Below(4) == 0 ? small_len() : 25;
            AddOut(rng, mtx, 100000000, len);
        }
        list.push_back(mtx);
    }

    out << "# tx <index> <GetHash()> <RecursiveDynamicUsage> <serialization>\n";
    std::unordered_map<uint256, uint64_t, SaltedTxidHasher> first_with;
    for (uint64_t i = 0; i < list.size(); ++i) {
        std::vector<unsigned char> bytes;
        auto tx = ReadBack(list[i], bytes);
        out << "tx " << i << " " << tx->GetHash().GetHex() << " " << RecursiveDynamicUsage(*tx) << " " << HexStr(bytes) << "\n";
        if (first_with.emplace(tx->GetHash(), i).second) corpus.addable.push_back(i);
        corpus.Put(i, std::move(tx));
    }
    for (const auto& [original, twin] : twin_slots) {
        CHECK(corpus.txs.at(original)->GetHash() == corpus.txs.at(twin)->GetHash());
        CHECK(corpus.txs.at(original)->GetInstanceHash() != corpus.txs.at(twin)->GetInstanceHash());
        corpus.twins.emplace_back(original, twin);
    }
    CHECK(corpus.addable.size() + corpus.twins.size() == list.size());
}

struct BulkRange {
    uint64_t first, count;
    size_t n_in, n_out, sig_len, spk_len;
};

void BuildBulk(Corpus& corpus, std::ostream& out, const std::vector<BulkRange>& ranges)
{
    out << "# bulk <first index> <count> <inputs> <outputs> <scriptSig bytes> <scriptPubKey bytes> <RecursiveDynamicUsage of each>\n"
        << "#   <SHA256 of the count GetHash() values, each in its 32 internal bytes, in index order>: transactions Bulk() builds\n";
    for (const auto& r : ranges) {
        CSHA256 hasher;
        size_t usage = 0;
        for (uint64_t k = r.first; k < r.first + r.count; ++k) {
            std::vector<unsigned char> bytes;
            auto tx = ReadBack(Bulk(k, r.n_in, r.n_out, r.sig_len, r.spk_len), bytes);
            if (k == r.first) usage = RecursiveDynamicUsage(*tx);
            CHECK(RecursiveDynamicUsage(*tx) == usage);
            hasher.Write(tx->GetHash().begin(), 32);
            corpus.Put(k, std::move(tx));
        }
        unsigned char digest[CSHA256::OUTPUT_SIZE];
        hasher.Finalize(digest);
        out << "bulk " << r.first << " " << r.count << " " << r.n_in << " " << r.n_out << " " << r.sig_len << " " << r.spk_len << " "
            << usage << " " << HexStr(digest) << "\n";
    }
}

struct Op {
    enum Kind { ADD, REMOVE, TAKE } kind;
    std::vector<uint64_t> txs;
};

std::vector<uint64_t> Range(uint64_t first, uint64_t count)
{
    std::vector<uint64_t> out(count);
    for (uint64_t i = 0; i < count; ++i) out[i] = first + i;
    return out;
}

void RunScenario(const Corpus& corpus, std::ostream& out, const std::string& name, size_t max, std::vector<Op> ops, bool queue_rows)
{
    DisconnectedBlockTransactions pool{max};
    const auto& queue = pool.*Get(QueueTag{});
    const auto& map = pool.*Get(MapTag{});
    const auto& inner = pool.*Get(InnerTag{});
    const auto state = [&] {
        std::ostringstream s;
        s << pool.DynamicMemoryUsage() << " " << map.bucket_count() << " " << pool.size() << " " << inner;
        return s.str();
    };
    const auto queued = [&] {
        if (!queue_rows) return std::string{"*"};
        return Runs(corpus.Indices({queue.begin(), queue.end()}));
    };
    if (ops.empty() || ops.back().kind != Op::TAKE) ops.push_back({Op::TAKE, {}});
    out << "scenario " << name << " " << max << "\n";
    for (const auto& op : ops) {
        switch (op.kind) {
        case Op::ADD: {
            const auto evicted = pool.AddTransactionsFromBlock(corpus.Refs(op.txs));
            out << "add " << Runs(op.txs) << " " << state() << " " << Runs(corpus.Indices(evicted)) << " " << queued() << "\n";
            break;
        }
        case Op::REMOVE:
            pool.removeForBlock(corpus.Refs(op.txs));
            out << "remove " << Runs(op.txs) << " " << state() << " " << queued() << "\n";
            break;
        case Op::TAKE: {
            const auto taken = pool.take();
            out << "take " << Runs(corpus.Indices({taken.begin(), taken.end()})) << " " << state() << "\n";
            break;
        }
        }
    }
    out << "end\n";
}

constexpr size_t MAX_POOL{MAX_DISCONNECTED_TX_POOL_SIZE * 1000};

// What each add of ops evicts from a new pool of max bytes, in order, and the pool's largest
// DynamicMemoryUsage after a call.
std::pair<std::vector<std::vector<uint64_t>>, size_t> Replay(const Corpus& corpus, size_t max, const std::vector<Op>& ops)
{
    DisconnectedBlockTransactions pool{max};
    std::vector<std::vector<uint64_t>> evictions;
    size_t peak = 0;
    for (const auto& op : ops) {
        if (op.kind == Op::ADD) evictions.push_back(corpus.Indices(pool.AddTransactionsFromBlock(corpus.Refs(op.txs))));
        if (op.kind == Op::REMOVE) pool.removeForBlock(corpus.Refs(op.txs));
        if (op.kind == Op::TAKE) pool.take();
        peak = std::max(peak, pool.DynamicMemoryUsage());
    }
    pool.take();
    return {evictions, peak};
}

// pool.txt: the corpus, then the scenarios.
void WritePool(std::ostream& out)
{
    using memusage::MallocUsage;
    out << Header("Dash Core's DisconnectedBlockTransactions (kernel/disconnected_transactions.h) and RecursiveDynamicUsage (core_memusage.h).")
        << "# const <name> <value>\n"
        << "const sizeof_CTxIn " << sizeof(CTxIn) << "\n"
        << "const sizeof_CTxOut " << sizeof(CTxOut) << "\n"
        << "const sizeof_CScript " << sizeof(CScript) << "\n"
        << "const sizeof_list_node " << sizeof(memusage::list_node<CTransactionRef>) << "\n"
        << "const sizeof_unordered_node " << sizeof(memusage::unordered_node<std::pair<const uint256, Queue::iterator>>) << "\n"
        << "const MAX_DISCONNECTED_TX_POOL_SIZE " << MAX_DISCONNECTED_TX_POOL_SIZE << "\n";
    for (size_t n = 0; n <= 100; ++n) out << "const MallocUsage(" << n << ") " << MallocUsage(n) << "\n";

    Corpus corpus;
    BuildCorpus(corpus, out);
    constexpr uint64_t BIG{100000}, SMALL{200000};
    BuildBulk(corpus, out, {{BIG, 60000, 1, 2, 107, 25}, {SMALL, 1200, 2, 1, 72, 23}});

    out << "# scenario <name> <max_mem_usage>: a new pool; then a row per call, each ending with the pool's state\n"
        << "#   <DynamicMemoryUsage()> <iters_by_txid.bucket_count()> <size()> <cachedInnerUsage>:\n"
        << "# add <vtx> <state> <evicted, in order> <queuedTx, front first, or * for not recorded>  (AddTransactionsFromBlock)\n"
        << "# remove <vtx> <state> <queuedTx or *>  (removeForBlock)\n"
        << "# take <taken, in order> <state>  (take())\n"
        << "# end\n";

    const auto& addable = corpus.addable;
    // A new pool and an empty block: reserve(0) still rehashes the single bucket.
    RunScenario(corpus, out, "empty-blocks", MAX_POOL, {{Op::REMOVE, {}}, {Op::ADD, {}}, {Op::REMOVE, {}}, {Op::ADD, {}}, {Op::ADD, {addable[0]}}, {Op::ADD, {}}}, true);
    // The same txid twice: the map keeps the first entry, the queue both (Core assumes this never happens).
    {
        const uint64_t a = addable[30], b = addable[31], c = addable[32];
        RunScenario(corpus, out, "duplicate-txids", MAX_POOL, {{Op::ADD, {a, b}}, {Op::ADD, {a}}, {Op::REMOVE, {a}}, {Op::REMOVE, {a}}, {Op::ADD, {c, a}}}, true);
        const size_t budget = RecursiveDynamicUsage(*corpus.txs.at(a)) + RecursiveDynamicUsage(*corpus.txs.at(b)) + 600;
        RunScenario(corpus, out, "duplicate-txids-evicted", budget, {{Op::ADD, {a}}, {Op::ADD, {b}}, {Op::ADD, {a}}, {Op::ADD, {c}}, {Op::REMOVE, {a}}, {Op::ADD, {a}}}, true);
        // A removal takes the copy the map names, the first queued: the queue ends b, c, a.
        RunScenario(corpus, out, "duplicate-txids-first-entry", MAX_POOL, {{Op::ADD, {a, b}}, {Op::ADD, {c}}, {Op::ADD, {a}}, {Op::REMOVE, {a}}}, true);
        // Once the map's copy has gone, a copy left queued without an entry stays through a
        // removal, and a later add maps the next copy: the queue ends x, a, y.
        const uint64_t x = addable[33], y = addable[34];
        RunScenario(corpus, out, "duplicate-txids-mapped-entry", MAX_POOL,
                    {{Op::ADD, {a}}, {Op::ADD, {x}}, {Op::ADD, {a}}, {Op::REMOVE, {a}}, {Op::ADD, {y}}, {Op::ADD, {a}}, {Op::REMOVE, {a}}}, true);
        // Evicting a copy with no map entry erases its txid's entry, which names a later copy;
        // that copy then stays through a removal. The budget is the largest under which the
        // last add (of the largest transaction) evicts x and the unmapped copy, and no add
        // before it evicts.
        uint64_t z = addable[35];
        for (const auto i : addable) {
            if (RecursiveDynamicUsage(*corpus.txs.at(i)) > RecursiveDynamicUsage(*corpus.txs.at(z))) z = i;
        }
        const std::vector<Op> ops{{Op::ADD, {x}}, {Op::ADD, {a}}, {Op::ADD, {a}}, {Op::REMOVE, {a}}, {Op::ADD, {y}}, {Op::ADD, {a}}, {Op::ADD, {z}}, {Op::REMOVE, {a}}};
        const std::vector<std::vector<uint64_t>> wanted{{}, {}, {}, {}, {}, {x, a}};
        size_t evicting = 0;
        for (size_t max = Replay(corpus, std::numeric_limits<size_t>::max(), ops).second; max > 0 && evicting == 0; --max) {
            if (Replay(corpus, max, ops).first == wanted) evicting = max;
        }
        CHECK(evicting > 0);
        RunScenario(corpus, out, "duplicate-txids-evicted-mapping", evicting, ops, true);
    }
    // A re-signed asset unlock in the new chain removes the disconnected one: the same GetHash().
    for (const auto& [original, twin] : corpus.twins) {
        RunScenario(corpus, out, "resigned-unlock-" + std::to_string(original), MAX_POOL,
                    {{Op::ADD, {addable[40], original, addable[41]}}, {Op::REMOVE, {twin}}, {Op::REMOVE, {original}}}, true);
    }
    // Both instances of one withdrawal queued, apart: the map names the first, which a removal of
    // either takes.
    {
        const auto& [original, twin] = corpus.twins.front();
        RunScenario(corpus, out, "resigned-unlock-both-queued", MAX_POOL, {{Op::ADD, {original}}, {Op::ADD, {addable[42]}}, {Op::ADD, {twin}}, {Op::REMOVE, {twin}}}, true);
    }
    // A large block mostly evicted, then one-transaction blocks: reserve(size + 1) shrinks the map.
    {
        std::vector<uint64_t> big(addable.begin() + 100, addable.begin() + 300);
        size_t budget = 0;
        for (size_t i = 0; i < 20; ++i) budget += RecursiveDynamicUsage(*corpus.txs.at(big[i])) + MallocUsage(32) + MallocUsage(48);
        std::vector<Op> ops{{Op::ADD, big}};
        for (size_t i = 300; i < 340; ++i) ops.push_back({Op::ADD, {addable[i]}});
        RunScenario(corpus, out, "shrink-after-eviction", budget + 400, ops, true);
    }
    // Random reorgs: disjoint blocks of addable transactions, removals of the new chain's blocks
    // (some queued, some evicted, some never added, and the re-signed twins), takes.
    for (uint64_t seed = 1; seed <= 48; ++seed) {
        Rng rng{1000 + seed};
        std::vector<uint64_t> pool_txs = addable;
        for (size_t i = pool_txs.size(); i > 1; --i) std::swap(pool_txs[i - 1], pool_txs[rng.Below(i)]);
        static constexpr size_t BUDGETS[]{0, 1, 600, 2000, 8000, 30000, 100000, MAX_POOL};
        const size_t max = rng.Below(3) == 0 ? rng.Range(1000, 200000) : BUDGETS[rng.Below(std::size(BUDGETS))];
        size_t next = 0;
        std::vector<uint64_t> added;
        std::vector<Op> ops;
        for (uint64_t i = 0, n = rng.Range(10, 60); i < n && next < pool_txs.size(); ++i) {
            const uint64_t pick = rng.Below(100);
            if (pick < 60) {
                uint64_t size = 0;
                switch (rng.Below(5)) {
                case 0: size = 0; break;
                case 1: size = 1; break;
                case 2: size = rng.Range(2, 5); break;
                case 3: size = rng.Range(6, 40); break;
                default: size = rng.Range(41, 120); break;
                }
                size = std::min<uint64_t>(size, pool_txs.size() - next);
                std::vector<uint64_t> block(pool_txs.begin() + next, pool_txs.begin() + next + size);
                next += size;
                added.insert(added.end(), block.begin(), block.end());
                ops.push_back({Op::ADD, block});
            } else if (pick < 95) {
                std::vector<uint64_t> block;
                for (uint64_t j = 0, n_rm = rng.Range(0, 30); j < n_rm; ++j) {
                    const uint64_t from = rng.Below(10);
                    if (from < 6 && !added.empty()) {
                        block.push_back(added[rng.Below(added.size())]);
                    } else if (from < 9 && next < pool_txs.size()) {
                        block.push_back(pool_txs[next + rng.Below(pool_txs.size() - next)]);
                    } else if (!corpus.twins.empty()) {
                        block.push_back(corpus.twins[rng.Below(corpus.twins.size())].second);
                    }
                }
                ops.push_back({Op::REMOVE, block});
            } else {
                ops.push_back({Op::TAKE, {}});
                // A block's transactions leave the pool once; the next adds are new ones.
            }
        }
        RunScenario(corpus, out, "random-" + std::to_string(seed), max, ops, true);
    }
    // Core's budget: 60,000 bulk transactions in 30 blocks of 2,000 (about 26 MB of accounting),
    // then 40 small blocks of 30 against the shrunken map, then the new chain's blocks, then take().
    {
        std::vector<Op> ops;
        for (uint64_t b = 0; b < 30; ++b) ops.push_back({Op::ADD, Range(BIG + 2000 * b, 2000)});
        for (uint64_t b = 0; b < 40; ++b) ops.push_back({Op::ADD, Range(SMALL + 30 * b, 30)});
        ops.push_back({Op::REMOVE, Range(SMALL + 30 * 39, 30)});
        ops.push_back({Op::REMOVE, Range(BIG, 4000)});
        ops.push_back({Op::REMOVE, Range(BIG + 2000 * 29, 2000)});
        ops.push_back({Op::REMOVE, Range(SMALL, 300)});
        RunScenario(corpus, out, "core-budget-bulk", MAX_POOL, ops, false);
    }
}

void WriteFile(const std::string& dir, const std::string& name, void (*write)(std::ostream&))
{
    std::ostringstream body;
    write(body);
    std::ofstream file{dir + "/" + name, std::ios::binary | std::ios::trunc};
    file << body.str();
    file.close();
    if (!file) Fail("cannot write " + dir + "/" + name);
}

} // namespace

int main(int argc, char** argv)
{
    if (argc != 2) Fail("usage: gen_dbt_vectors <outdir>");
    WriteFile(argv[1], "buckets.txt", WriteBuckets);
    WriteFile(argv[1], "container.txt", WriteContainer);
    WriteFile(argv[1], "pool.txt", WritePool);
    return 0;
}
