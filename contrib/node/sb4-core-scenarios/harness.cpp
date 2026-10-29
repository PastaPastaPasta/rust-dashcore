// sb4_scenarios: Dash Core's verdicts for SB4 (dash-chain core).
//
// designs/SB4-dash-chain.md §15. Linked against the unlocalized server
// libraries and libtest_util.a of the v24.0 oracle pin (build.sh), so every
// field this program prints is computed by Core's own code. Core's code is
// changed at four seams only: chainlock::VerifyChainLock's four-argument
// overload returns Valid (no quorums exist in a test chain);
// Chainstate::EnforceBlock runs between two harness hooks that record its
// boundary and deliver queued signals (both `--wrap`); and Chainstate::ConnectBlock
// and DisconnectBlock (interposed, HX1 X0). check-seam.sh proves nothing else in
// Core's objects moved. Everything else the harness chooses is fixture, listed
// in README.md: TestingSetup, mock time, the signal delivery schedule
// (TraceListener, DEC-147), the spork signer off regtest, and the seed ops.
//
// usage:
//   sb4_scenarios run <scenario.json> [--delivery <policy>] [--headers <dir>]
//                                                  one scenario → vector JSON on stdout
//   sb4_scenarios headers <net> <file> <h> <op> …  real-header modes (H-M / H-T):
//       bench-pnbh <from> <count>   instructions:u of ProcessNewBlockHeaders
//       locator <n> <seed>          GetLocator at n pseudo-random heights
//       pnbh <from> <count>         one batch from the file after preloading
//
// Scenario files are data (scenarios/*.json); see README.md for the op list.

#include <chain.h>
#include <chainlock/chainlock.h>
#include <chainlock/clsig.h>
#include <chainlock/handler.h>
#include <chainparams.h>
#include <coinjoin/coinjoin.h>
#include <consensus/merkle.h>
#include <consensus/validation.h>
#include <evo/chainhelper.h>
#include <banman.h>
#include <governance/governance.h>
#include <hash.h>
#include <key.h>
#include <key_io.h>
#include <llmq/context.h>
#include <llmq/quorumsman.h>
#include <llmq/signing.h>
#include <masternode/sync.h>
#include <msg_result.h>
#include <net_processing.h>
#include <node/blockstorage.h>
#include <node/context.h>
#include <pow.h>
#include <primitives/block.h>
#include <rpc/register.h>
#include <rpc/request.h>
#include <rpc/server.h>
#include <scheduler.h>
#include <spork.h>
#include <streams.h>
#include <test/util/setup_common.h>
#include <txdb.h>
#include <univalue.h>
#include <util/strencodings.h>
#include <util/string.h>
#include <util/time.h>
#include <validation.h>
#include <validationinterface.h>
#include <version.h>

#include <cxxabi.h>
#include <elf.h>
#include <linux/perf_event.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <cstdlib>
#include <deque>
#include <exception>
#include <cstdio>
#include <fstream>
#include <functional>
#include <future>
#include <iostream>
#include <iterator>
#include <map>
#include <memory>
#include <new>
#include <optional>
#include <random>
#include <set>
#include <sstream>
#include <stdexcept>
#include <utility>
#include <string>
#include <vector>

// libtest_util.a references these (test/main.cpp defines them for test_dash).
const std::function<void(const std::string&)> G_TEST_LOG_FUN{};
const std::function<std::vector<const char*>()> G_TEST_COMMAND_LINE_ARGUMENTS{};
// Defined in libtest_util.a (test/util/setup_common.cpp:119), not declared in a header.
std::unique_ptr<PeerManager> MakePeerManager(CConnman& connman, node::NodeContext& node, BanMan* banman,
                                             bool ignore_incoming_txs);

// ---------------------------------------------------------------------------
/** Where sb4_anchor is linked; with its symbol table value, the load bias. */
extern "C" void sb4_anchor() {}

/** A function's address range in this process. */
struct FunctionRange {
    uintptr_t lo, hi;
    std::string name;
};

/** The functions that allocate BlockMap nodes at this pin: try_emplace inlined
 *  in AddToBlockIndex and InsertBlockIndex, and out-of-line instantiations for
 *  BlockMap's value type (operator[], which ReplayBlocks uses). */
static bool IsNodeAllocator(const std::string& fn)
{
    return fn == "node::BlockManager::AddToBlockIndex" || fn == "node::BlockManager::InsertBlockIndex" ||
           fn.find("std::pair<uint256 const, CBlockIndex>") != std::string::npos;
}

/** The Core functions that call the interposed seams (interpose-symbols.txt),
 *  which the transcript names (CallerOf). */
static bool IsSeamCaller(const std::string& fn)
{
    return fn == "Chainstate::ConnectTip" || fn == "Chainstate::DisconnectTip" || fn == "TestBlockValidity" ||
           fn == "CVerifyDB::VerifyDB" || fn == "Chainstate::ReplayBlocks";
}

/** The C++ functions of this binary the harness looks up (node allocators and
 *  seam callers), each with its demangled name (up to its parameter list) and
 *  address range in this process, from /proc/self/exe's symbol table, read
 *  once (leaked, for allocations during static destruction). */
static const std::vector<FunctionRange>& TrackedFunctions()
{
    static const auto* all = [] {
        std::ifstream f("/proc/self/exe", std::ios::binary | std::ios::ate);
        std::string elf(static_cast<size_t>(f.tellg()), '\0');
        f.seekg(0);
        f.read(elf.data(), elf.size());
        if (!f || elf.size() < sizeof(Elf64_Ehdr) || elf.compare(0, 4, ELFMAG) != 0) throw std::runtime_error("not an ELF binary");
        const auto* eh = reinterpret_cast<const Elf64_Ehdr*>(elf.data());
        const auto* sh = reinterpret_cast<const Elf64_Shdr*>(elf.data() + eh->e_shoff);
        auto* out = new std::vector<FunctionRange>;
        std::optional<uintptr_t> anchor;
        for (int i = 0; i < eh->e_shnum; ++i) {
            if (sh[i].sh_type != SHT_SYMTAB) continue;
            const char* names = elf.data() + sh[sh[i].sh_link].sh_offset;
            const auto* syms = reinterpret_cast<const Elf64_Sym*>(elf.data() + sh[i].sh_offset);
            for (size_t k = 0; k < sh[i].sh_size / sizeof(Elf64_Sym); ++k) {
                if (ELF64_ST_TYPE(syms[k].st_info) != STT_FUNC) continue;
                const std::string name = names + syms[k].st_name;
                if (name == "sb4_anchor") anchor = syms[k].st_value;
                if (syms[k].st_size == 0 || name.compare(0, 2, "_Z") != 0) continue;
                int status = 0;
                std::unique_ptr<char, void (*)(void*)> dm(abi::__cxa_demangle(name.c_str(), nullptr, nullptr, &status), std::free);
                if (status != 0) continue;
                std::string fn{dm.get()};
                fn = fn.substr(0, fn.find('('));
                if (IsNodeAllocator(fn) || IsSeamCaller(fn)) out->push_back({syms[k].st_value, syms[k].st_value + syms[k].st_size, fn});
            }
        }
        if (!anchor) throw std::runtime_error("no sb4_anchor in the symbol table");
        const uintptr_t bias = reinterpret_cast<uintptr_t>(&sb4_anchor) - *anchor;
        for (auto& r : *out) r.lo += bias, r.hi += bias;
        return out;
    }();
    return *all;
}

/** The looked-up functions whose name `want` accepts. */
static std::vector<FunctionRange> FunctionRanges(const std::function<bool(const std::string&)>& want)
{
    std::vector<FunctionRange> out;
    std::copy_if(TrackedFunctions().begin(), TrackedFunctions().end(), std::back_inserter(out), [&](const auto& r) { return want(r.name); });
    return out;
}

/** The range a return address lies in, or nullptr (a return address follows its call). */
static const FunctionRange* RangeOf(const std::vector<FunctionRange>& ranges, const void* ret)
{
    const auto a = reinterpret_cast<uintptr_t>(ret);
    for (const auto& r : ranges) {
        if (a > r.lo && a <= r.hi) return &r;
    }
    return nullptr;
}

// ---------------------------------------------------------------------------
// Heap perturbation (DEC-172, keyed by DEC-175 fix r4). Core decides some ties
// by pointer order (m_failed_blocks is a std::set<CBlockIndex*>;
// CBlockIndexWorkComparator and m_blocks_unlinked fall back to addresses), and
// those pointers are the CBlockIndex objects inside BlockMap's nodes, so a run
// shows one sample of the allocator's layout. With `--perturb-heap <mode>` this
// replacement of the global operator new (which libstdc++'s array, sized and
// nothrow forms call; the aligned ones do not) serves each BlockMap node, and
// nothing else, from a private region of PERTURB_SLOTS slots, never reusing
// one: the k-th node allocated (its ordinal, counted on the main thread, where
// the ops create every entry) goes to slot k under `up`, slot PERTURB_SLOTS-1-k
// under `down`, so any two entries swap places between the two, and under a
// numeric seed to the slot a seeded permutation gives k, so a middle one of
// three tied entries may lie lowest. A node's place is a function of the mode
// and its ordinal alone, so a mode reproduces its run in any process (Sol HX1
// r4 R4-3: a stream shared with every other allocation did not). A node is
// recognised by its size and its caller, the return address lying in one of
// the functions that allocate BlockMap nodes at this pin (IsNodeAllocator);
// --audit-index proves every index entry, and nothing else, came from the
// region. A region that runs out falls back to malloc, and the run then fails
// (PerturbedExit): later entries would not have been perturbed. Without the
// flag it is libstdc++'s operator new: malloc, then the new-handler loop.
using BlockMapNode = std::__detail::_Hash_node<node::BlockMap::value_type,
                                               std::__cache_default<uint256, BlockHasher>::value>;
enum class PerturbMode { Up, Down, Seeded };
static thread_local bool g_perturb{false}; // set for the main thread only
static PerturbMode g_perturb_mode{PerturbMode::Up};
static uint64_t g_perturb_seed{0};
// 2^24 slots of 256 bytes: 4 GiB of MAP_NORESERVE address space. A seeded
// permutation is two-level (blocks of PERTURB_BLOCK slots, then slots within
// a block, each by a 12-bit Feistel permutation), so a run touches whole
// 1 MiB blocks, not one page per node (a 2.5M-header preload).
constexpr size_t PERTURB_SLOT{256}, PERTURB_BITS{12}, PERTURB_BLOCK{size_t{1} << PERTURB_BITS};
constexpr size_t PERTURB_SLOTS{PERTURB_BLOCK * PERTURB_BLOCK};
static_assert(PERTURB_BITS % 2 == 0, "Permute's Feistel halves");
static_assert(sizeof(BlockMapNode) <= PERTURB_SLOT && alignof(BlockMapNode) <= 16);
static char* g_region{nullptr}; // written in main before any other thread starts
static uint64_t g_nodes_served{0}; // node ordinals handed out (main thread only)
static std::atomic<uint64_t> g_nodes_freed{0};
static bool g_region_exhausted{false};
// Leaked, so a node allocated during static destruction still finds it.
static std::vector<FunctionRange>& g_node_allocators{*new std::vector<FunctionRange>};
// `--audit-index` (allocation-audit.py, Sol HX1 r3/r4): each dump also prints, to
// stderr, the sizes of CBlockIndex and of the BlockMap node that holds it, and
// every index entry's address and region offset, and fails unless every entry,
// and nothing else, was served from the region.
static bool g_audit_index{false};

static uint64_t SplitMix(uint64_t z)
{
    z += 0x9e3779b97f4a7c15;
    z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9;
    z = (z ^ (z >> 27)) * 0x94d049bb133111eb;
    return z ^ (z >> 31);
}

/** A keyed bijection of [0, PERTURB_BLOCK): a four-round Feistel network. */
static size_t Permute(uint64_t key, size_t x)
{
    constexpr size_t half{PERTURB_BITS / 2}, mask{(size_t{1} << half) - 1};
    size_t l = x >> half, r = x & mask;
    for (uint64_t round = 0; round < 4; ++round) {
        const size_t next = l ^ (SplitMix(key ^ (round << 56) ^ r) & mask);
        l = r;
        r = next;
    }
    return l << half | r;
}

/** The slot of the node with ordinal k under the run's mode. */
static size_t PerturbSlot(uint64_t k)
{
    switch (g_perturb_mode) {
    case PerturbMode::Up: return k;
    case PerturbMode::Down: return PERTURB_SLOTS - 1 - k;
    case PerturbMode::Seeded: {
        const size_t block = k >> PERTURB_BITS;
        return Permute(SplitMix(g_perturb_seed), block) << PERTURB_BITS |
               Permute(SplitMix(g_perturb_seed ^ SplitMix(block + 1)), k & (PERTURB_BLOCK - 1));
    }
    }
    return k;
}

static void PerturbInit(const std::string& mode)
{
    if (mode == "up") {
        g_perturb_mode = PerturbMode::Up;
    } else if (mode == "down") {
        g_perturb_mode = PerturbMode::Down;
    } else {
        g_perturb_mode = PerturbMode::Seeded;
        g_perturb_seed = std::stoull(mode);
    }
    g_node_allocators = FunctionRanges(IsNodeAllocator);
    for (const char* fn : {"node::BlockManager::AddToBlockIndex", "node::BlockManager::InsertBlockIndex"}) {
        if (std::none_of(g_node_allocators.begin(), g_node_allocators.end(), [&](const auto& r) { return r.name == fn; })) {
            throw std::runtime_error(std::string{"--perturb-heap: no "} + fn + " in the symbol table (re-pin?)");
        }
    }
    void* region = mmap(nullptr, PERTURB_SLOTS * PERTURB_SLOT, PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    if (region == MAP_FAILED) throw std::runtime_error("--perturb-heap: mmap failed");
    g_region = static_cast<char*>(region);
    g_perturb = true;
}

/** A BlockMap node's slot when `ret`, the allocation's return address, lies in
 *  a node allocator, or nullptr (another allocation, or the region is used up:
 *  the caller then allocates as usual). */
static void* PerturbedNode(const void* ret)
{
    if (!RangeOf(g_node_allocators, ret)) return nullptr;
    if (g_nodes_served == PERTURB_SLOTS) return g_region_exhausted = true, nullptr;
    return g_region + PerturbSlot(g_nodes_served++) * PERTURB_SLOT;
}

// noinline: the return address must be the allocating Core function's.
__attribute__((noinline)) void* operator new(std::size_t n)
{
    if (g_perturb && n == sizeof(BlockMapNode)) {
        if (void* p = PerturbedNode(__builtin_return_address(0))) return p;
    }
    for (;;) {
        if (void* p = std::malloc(n ? n : 1)) return p;
        std::new_handler handler = std::get_new_handler();
        if (!handler) throw std::bad_alloc();
        handler();
    }
}

/** A run's exit status: a perturbed run whose region ran out fails. */
static int PerturbedExit(int rc)
{
    if (rc == 0 && g_region_exhausted) {
        std::cerr << "sb4_scenarios: --perturb-heap: the region ran out, so later index entries were not perturbed\n";
        return 1;
    }
    return rc;
}

/** The region is never reused, so a slot of it is not freed (on any thread). */
static bool InRegion(const void* p)
{
    return g_region && static_cast<const char*>(p) >= g_region &&
           static_cast<const char*>(p) < g_region + PERTURB_SLOTS * PERTURB_SLOT;
}

void operator delete(void* p) noexcept
{
    if (!InRegion(p)) return std::free(p);
    g_nodes_freed.fetch_add(1, std::memory_order_relaxed);
}
void operator delete(void* p, std::size_t) noexcept
{
    if (!InRegion(p)) return std::free(p);
    g_nodes_freed.fetch_add(1, std::memory_order_relaxed);
}

// ---------------------------------------------------------------------------
// The seam: chainlock::VerifyChainLock(Consensus::Params const&,
// llmq::CQuorumManager const&, chainlock::ChainLockSig const&, CBlockIndex const*)
// as called by ChainlockHandler::ProcessNewChainLock. Linked with
// -Wl,--wrap=<mangled name> (wrap-symbols.txt); the definition below is the
// __wrap_ symbol. It answers Valid unless a `cl {sig: "bad"}` op asks for
// Invalid (a CLSIG whose signature fails, handler.cpp:156-164).
static std::atomic<int> g_verify_calls{0};
static std::atomic<llmq::VerifyRecSigStatus> g_verify_status{llmq::VerifyRecSigStatus::Valid};
extern "C" llmq::VerifyRecSigStatus
__wrap__ZN9chainlock15VerifyChainLockERKN9Consensus6ParamsERKN4llmq14CQuorumManagerERKNS_12ChainLockSigEPK11CBlockIndex(
    const Consensus::Params&, const llmq::CQuorumManager&, const chainlock::ChainLockSig&, const CBlockIndex*)
{
    ++g_verify_calls;
    return g_verify_status.load();
}

// The second seam: Chainstate::EnforceBlock(BlockValidationState&, CBlockIndex
// const*), called from ChainlockHandler::EnforceBestChainLock
// (chainlock/handler.cpp:296). The wrapper runs the real function between two
// harness hooks: one marks the pass in the trace, the other hands the state at
// that boundary (before the handler's ActivateBestChain, :302) to the scenario,
// where SB4's node rows compare (rev3, Opus O-m2), and is a delivery point of
// the scenario's schedule (TraceListener).
static std::function<void(const CBlockIndex*)> g_enforce_entry;
static std::function<void(Chainstate&, const CBlockIndex*)> g_enforce_boundary;
extern "C" void __real__ZN10Chainstate12EnforceBlockER20BlockValidationStatePK11CBlockIndex(
    Chainstate*, BlockValidationState&, const CBlockIndex*);
extern "C" void __wrap__ZN10Chainstate12EnforceBlockER20BlockValidationStatePK11CBlockIndex(
    Chainstate* self, BlockValidationState& state, const CBlockIndex* pindex)
{
    if (g_enforce_entry) g_enforce_entry(pindex);
    __real__ZN10Chainstate12EnforceBlockER20BlockValidationStatePK11CBlockIndex(self, state, pindex);
    if (g_enforce_boundary) g_enforce_boundary(*self, pindex);
}

// The interposed seams (HX1 X0, interpose-symbols.txt): Chainstate::ConnectBlock
// and Chainstate::DisconnectBlock. Their callers (ConnectTip, DisconnectTip,
// TestBlockValidity, CVerifyDB::VerifyDB, ReplayBlocks) share their object, so
// build.sh makes each definition weak, adds __real_<sym> at its address and
// links <sym> to the __wrap_ definition below (check-seam.sh). Each wrapper runs
// the real function, then reports the call to the transcript hook (X1) with its
// return address, from which the transcript names the calling Core function.
// The result goes back to Core unchanged.
// Invariant: the callers were compiled in the same object as the real functions
// (-O2, interprocedural analysis of the real callee), and the proof covers the
// link, not those assumptions. A hook may therefore read only what the real
// function reads and returns (the block, its hash, the result and the state),
// never other Core state such as pindex->nStatus or the active chain, which a
// caller may not have stored yet at the call.
using BlockCallHook = std::function<void(const char* phase, const void* ret, const uint256& hash, UniValue result,
                                         const BlockValidationState* state)>;
static BlockCallHook g_block_call;
extern "C" bool __real__ZN10Chainstate12ConnectBlockERK6CBlockR20BlockValidationStateP11CBlockIndexR15CCoinsViewCacheb(
    Chainstate*, const CBlock&, BlockValidationState&, CBlockIndex*, CCoinsViewCache&, bool);
extern "C" bool __wrap__ZN10Chainstate12ConnectBlockERK6CBlockR20BlockValidationStateP11CBlockIndexR15CCoinsViewCacheb(
    Chainstate* self, const CBlock& block, BlockValidationState& state, CBlockIndex* pindex, CCoinsViewCache& view,
    bool just_check)
{
    const void* ret = __builtin_return_address(0);
    const bool ok = __real__ZN10Chainstate12ConnectBlockERK6CBlockR20BlockValidationStateP11CBlockIndexR15CCoinsViewCacheb(
        self, block, state, pindex, view, just_check);
    if (g_block_call) g_block_call(just_check ? "check" : "connect", ret, block.GetHash(), UniValue(ok), &state);
    return ok;
}
static const char* DisconnectName(DisconnectResult r)
{
    switch (r) {
    case DISCONNECT_OK: return "ok";
    case DISCONNECT_UNCLEAN: return "unclean";
    case DISCONNECT_FAILED: return "failed";
    }
    return "?";
}

extern "C" DisconnectResult __real__ZN10Chainstate15DisconnectBlockERK6CBlockPK11CBlockIndexR15CCoinsViewCache(
    Chainstate*, const CBlock&, const CBlockIndex*, CCoinsViewCache&);
extern "C" DisconnectResult __wrap__ZN10Chainstate15DisconnectBlockERK6CBlockPK11CBlockIndexR15CCoinsViewCache(
    Chainstate* self, const CBlock& block, const CBlockIndex* pindex, CCoinsViewCache& view)
{
    const void* ret = __builtin_return_address(0);
    const DisconnectResult r =
        __real__ZN10Chainstate15DisconnectBlockERK6CBlockPK11CBlockIndexR15CCoinsViewCache(self, block, pindex, view);
    if (g_block_call) {
        g_block_call("disconnect", ret, block.GetHash(),
                     UniValue(DisconnectName(r)), nullptr);
    }
    return r;
}

/** The Core function a return address lies in, for the interposed seams' callers
 *  (interpose-symbols.txt), by this binary's own symbol table: "Chainstate::ConnectTip",
 *  "TestBlockValidity", ... An address outside them is "unknown". */
std::string CallerOf(const void* ret)
{
    static const auto ranges = FunctionRanges(IsSeamCaller);
    const FunctionRange* r = RangeOf(ranges, ret);
    return r ? r->name : "unknown";
}

// ---------------------------------------------------------------------------
// Private access (explicit instantiation may name private members).
template <typename Tag, typename Tag::type M>
struct Rob {
    friend typename Tag::type get(Tag) { return M; }
};
#define SB4_ROB(NAME, TYPE, MEMBER)                \
    struct NAME {                                  \
        using type = TYPE;                         \
        friend type get(NAME);                     \
    };                                             \
    template struct Rob<NAME, &MEMBER>;

SB4_ROB(RobClScheduler, std::unique_ptr<CScheduler> chainlock::ChainlockHandler::*, chainlock::ChainlockHandler::scheduler)
SB4_ROB(RobClEnabled, std::atomic<bool> chainlock::ChainlockHandler::*, chainlock::ChainlockHandler::isEnabled)
SB4_ROB(RobClUpdatedBlockTip, void (chainlock::ChainlockHandler::*)(const CBlockIndex*, const CBlockIndex*, bool),
        chainlock::ChainlockHandler::UpdatedBlockTip)
SB4_ROB(RobClCleanup, void (chainlock::ChainlockHandler::*)(), chainlock::ChainlockHandler::Cleanup)
SB4_ROB(RobMarkConflicting, bool (Chainstate::*)(BlockValidationState&, CBlockIndex*), Chainstate::MarkConflictingBlock)
SB4_ROB(RobCheckpoints, CCheckpointData CChainParams::*, CChainParams::checkpointData)
SB4_ROB(RobChainlocksCs, Mutex chainlock::Chainlocks::*, chainlock::Chainlocks::cs)
SB4_ROB(RobBestChainLock, chainlock::ChainLockSig chainlock::Chainlocks::*, chainlock::Chainlocks::bestChainLock)
SB4_ROB(RobBestChainLockHash, uint256 chainlock::Chainlocks::*, chainlock::Chainlocks::bestChainLockHash)
SB4_ROB(RobBestChainLockWithKnownBlock, chainlock::ChainLockSig chainlock::Chainlocks::*,
        chainlock::Chainlocks::bestChainLockWithKnownBlock)

namespace {

[[noreturn]] void Fail(const std::string& msg) { throw std::runtime_error(msg); }

/** Under --perturb-heap, every perturbed run's check that the perturbation
 *  reached the index and nothing else: every entry lies in the region, and the
 *  region holds as many live slots as there are entries (so a pin or toolchain
 *  that allocates nodes elsewhere, or off the main thread, fails the run rather
 *  than replaying on the plain heap). Each dump and a header job's end run it. */
void CheckRegion(const node::BlockMap& index) EXCLUSIVE_LOCKS_REQUIRED(cs_main)
{
    if (!g_perturb) return;
    const size_t outside = std::count_if(index.begin(), index.end(), [](const auto& e) { return !InRegion(&e.second); });
    if (outside) Fail("--perturb-heap: " + std::to_string(outside) + " index entries outside the region");
    const uint64_t live = g_nodes_served - g_nodes_freed.load();
    if (live != index.size()) {
        Fail("--perturb-heap: " + std::to_string(live) + " live region slots for " + std::to_string(index.size()) +
             " index entries (the region served something else)");
    }
}

std::string ReadFile(const std::string& path)
{
    std::ifstream f(path, std::ios::binary);
    if (!f) Fail("cannot read " + path);
    std::stringstream ss;
    ss << f.rdbuf();
    return ss.str();
}

std::string ResultName(BlockValidationResult r)
{
    switch (r) {
    case BlockValidationResult::BLOCK_RESULT_UNSET: return "unset";
    case BlockValidationResult::BLOCK_CONSENSUS: return "consensus";
    case BlockValidationResult::BLOCK_RECENT_CONSENSUS_CHANGE: return "recent-consensus-change";
    case BlockValidationResult::BLOCK_CACHED_INVALID: return "cached-invalid";
    case BlockValidationResult::BLOCK_INVALID_HEADER: return "invalid-header";
    case BlockValidationResult::BLOCK_MUTATED: return "mutated";
    case BlockValidationResult::BLOCK_MISSING_PREV: return "missing-prev";
    case BlockValidationResult::BLOCK_INVALID_PREV: return "invalid-prev";
    case BlockValidationResult::BLOCK_TIME_FUTURE: return "time-future";
    case BlockValidationResult::BLOCK_CHECKPOINT: return "checkpoint";
    case BlockValidationResult::BLOCK_CHAINLOCK: return "chainlock";
    }
    return strprintf("result-%d", static_cast<int>(r));
}

UniValue StateJson(const BlockValidationState& state)
{
    UniValue o(UniValue::VOBJ);
    o.pushKV("valid", state.IsValid());
    o.pushKV("class", ResultName(state.GetResult()));
    o.pushKV("reason", state.GetRejectReason());
    o.pushKV("debug", state.GetDebugMessage());
    return o;
}

/** What a scenario built: every block or header gets a label (`A100`, `G`). */
struct Built {
    CBlock block;
    int height;
    std::string parent;
};

/** A derived class may name its base's protected members; the pointers then
 *  dispatch virtually on any CValidationInterface (here, the ChainLock handler). */
struct ValidationCall : CValidationInterface {
    static void Header(CValidationInterface& v, const CBlockIndex* p) { (v.*&ValidationCall::AcceptedBlockHeader)(p); }
    static void Tip(CValidationInterface& v, const CBlockIndex* n, const CBlockIndex* f, bool ibd)
    {
        (v.*&ValidationCall::UpdatedBlockTip)(n, f, ibd);
    }
    static void TxAdded(CValidationInterface& v, const CTransactionRef& tx, int64_t t, uint64_t seq)
    {
        (v.*&ValidationCall::TransactionAddedToMempool)(tx, t, seq);
    }
    static void Connected(CValidationInterface& v, const std::shared_ptr<const CBlock>& b, const CBlockIndex* p)
    {
        (v.*&ValidationCall::BlockConnected)(b, p);
    }
};

/** Records Core's chain signals (the tip trace) and delivers the ChainLock
 * handler's share of them under an explicit schedule (DEC-147).
 *
 * dashd registers the handler directly (init.cpp:2153), so the validation
 * thread delivers each queued callback whenever it gets to it. That timing
 * matters in one place: the handler's UpdatedBlockTip schedules
 * CheckActiveState + EnforceBestChainLock (the tip task) unless one is pending
 * (tryLockChainTipScheduled, handler.cpp:197-206), and every enforcement emits
 * tip signals of its own (MarkConflictingBlock, validation.cpp:4141, and
 * ActivateBestChain). A signal delivered after the running tip task clears its
 * flag schedules another pass, so with the lock on a header-only branch dashd
 * repeats enforcement until the validation thread happens to deliver in time
 * (2 to 81 passes in ten runs). A repeat is not always a no-op: a pass can
 * leave state the next pass changes again. dashd re-runs enforcement until a
 * pass changes nothing (each tip signal schedules one, and Start()'s timer runs
 * one every 5 s), so vectors assert the trace up to each lock's first
 * EnforceBlock boundary and the state at that fixed point; what the passes
 * leave on the way is recorded as a transient (DEC-167, which replaces
 * DEC-147's idempotent repeats).
 *
 * Here the queued callbacks the handler listens to (UpdatedBlockTip,
 * BlockConnected, TransactionAddedToMempool) wait in an inbox, in queue order,
 * and the scenario hands them to the handler's real methods at defined points
 * (Scenario::Deliver); the handler's own compare-exchange decides at delivery,
 * as in dashd. AcceptedBlockHeader is synchronous in Core
 * (validationinterface.cpp:269) and goes straight through.
 *
 * SynchronousUpdatedBlockTip runs on the emitting thread; UpdatedBlockTip,
 * BlockConnected and BlockDisconnected are queued and delivered in FIFO order.
 * Core emits every SynchronousUpdatedBlockTip immediately followed by the queued
 * UpdatedBlockTip with the same arguments (validation.cpp:3821-3822,
 * :4036-4037, :4141-4142), so the emission order is the queued stream with each
 * UpdatedBlockTip preceded by its synchronous twin; Take() rebuilds it and fails
 * if the pairing does not hold. */
class TraceListener final : public CValidationInterface
{
public:
    std::function<std::string(const uint256&)> label;
    Mutex m;
    std::vector<std::string> queued GUARDED_BY(m);
    std::vector<std::string> sync GUARDED_BY(m);
    //! While set, the handler's queued callbacks wait in the inbox.
    std::atomic<CValidationInterface*> handler{nullptr};

    //! A harness event in the trace (an enforcement pass), in emission order.
    void Mark(const std::string& e)
    {
        LOCK(m);
        queued.push_back(e);
    }

    size_t Pending()
    {
        LOCK(m);
        return inbox.size();
    }

    //! Hands the first n waiting callbacks to the handler, in queue order, on this thread.
    void Deliver(size_t n)
    {
        std::vector<std::function<void(CValidationInterface&)>> batch;
        {
            LOCK(m);
            n = std::min(n, inbox.size());
            batch.assign(std::make_move_iterator(inbox.begin()), std::make_move_iterator(inbox.begin() + n));
            inbox.erase(inbox.begin(), inbox.begin() + n);
        }
        auto* h = handler.load();
        if (!h && !batch.empty()) Fail("callbacks queued for a handler that is gone");
        for (auto& f : batch) f(*h);
    }

    std::vector<std::string> Take()
    {
        LOCK(m);
        std::vector<std::string> out;
        size_t si = 0;
        for (const auto& e : queued) {
            if (e.rfind("UpdatedBlockTip", 0) == 0) {
                if (si == sync.size() || sync[si] != "Synchronous" + e) throw std::runtime_error("signal pairing broken at " + e);
                out.push_back(sync[si++]);
            }
            out.push_back(e);
        }
        if (si != sync.size()) throw std::runtime_error("unpaired " + sync[si]);
        queued.clear();
        sync.clear();
        return out;
    }

private:
    std::deque<std::function<void(CValidationInterface&)>> inbox GUARDED_BY(m);

    void Enqueue(std::function<void(CValidationInterface&)> f) EXCLUSIVE_LOCKS_REQUIRED(m)
    {
        if (handler.load()) inbox.push_back(std::move(f));
    }

    std::string Tip(const char* name, const CBlockIndex* n, const CBlockIndex* f, bool ibd) const
    {
        return strprintf("%s(%s, %s, %d)", name, n ? label(n->GetBlockHash()) : "null", f ? label(f->GetBlockHash()) : "null", ibd);
    }

protected:
    void SynchronousUpdatedBlockTip(const CBlockIndex* n, const CBlockIndex* f, bool ibd) override
    {
        LOCK(m);
        sync.push_back(Tip("SynchronousUpdatedBlockTip", n, f, ibd));
    }
    void UpdatedBlockTip(const CBlockIndex* n, const CBlockIndex* f, bool ibd) override
    {
        LOCK(m);
        queued.push_back(Tip("UpdatedBlockTip", n, f, ibd));
        Enqueue([=](CValidationInterface& v) { ValidationCall::Tip(v, n, f, ibd); });
    }
    void AcceptedBlockHeader(const CBlockIndex* p) override
    {
        if (auto* h = handler.load()) ValidationCall::Header(*h, p);
    }
    void TransactionAddedToMempool(const CTransactionRef& tx, int64_t t, uint64_t seq) override
    {
        LOCK(m);
        Enqueue([=](CValidationInterface& v) { ValidationCall::TxAdded(v, tx, t, seq); });
    }
    void BlockConnected(const std::shared_ptr<const CBlock>& b, const CBlockIndex* pindex) override
    {
        LOCK(m);
        queued.push_back("BlockConnected(" + label(pindex->GetBlockHash()) + ")");
        Enqueue([=](CValidationInterface& v) { ValidationCall::Connected(v, b, pindex); });
    }
    void BlockDisconnected(const std::shared_ptr<const CBlock>& b, const CBlockIndex* pindex) override
    {
        LOCK(m);
        queued.push_back("BlockDisconnected(" + label(pindex->GetBlockHash()) + ")");
    }
    //! Synchronous (validationinterface.cpp), on the thread that checked the block.
    void BlockChecked(const CBlock& b, const BlockValidationState& s) override
    {
        if (checked) checked(b, s);
    }

public:
    std::function<void(const CBlock&, const BlockValidationState&)> checked;
};

//! Regtest spork key matching Params().SporkAddress() (test/llmq_chainlock_tests.cpp).
constexpr const char* REGTEST_SPORK_PRIVKEY{"cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK"};
constexpr int64_t SPORK_OFF{4070908800};
//! random:<seed> delivery flushes once a drain has seen this many enforcement passes.
constexpr int RANDOM_PASS_CAP{12};
//! Settle gives up (a harness failure) after running this many passes without a fixed point.
constexpr int SETTLE_CAP{20};

/** TestingSetup (test/util/setup_common.cpp:368-411, wallet parts absent from
 *  this build), except that the ChainstateManager gets dashd's
 *  check_block_index default (DefaultConsistencyChecks, regtest only:
 *  validation.cpp:6768) instead of the fixture's `true`. A devnet's genesis
 *  load (LoadGenesisBlock's devnet block) trips CheckBlockIndex's empty-chain
 *  assertion, which dashd never runs off regtest. */
struct ScenarioSetup : ChainTestingSetup {
    ScenarioSetup(const std::string& chain, const std::vector<const char*>& args)
        : ChainTestingSetup(chain, args, /*dash_dbs_in_memory=*/true)
    {
        m_coins_db_in_memory = false;
        m_block_tree_db_in_memory = false;
        // ChainTestingSetup's ChainstateManager, and the handler bound to it, again
        // with dashd's option.
        m_node.clhandler.reset();
        m_node.chainman.reset();
        const ChainstateManager::Options opts{.chainparams = Params(), .check_block_index = Params().DefaultConsistencyChecks()};
        m_node.chainman = std::make_unique<ChainstateManager>(opts);
        m_node.chainman->m_blockman.m_block_tree_db = std::make_unique<CBlockTreeDB>(m_cache_sizes.block_tree_db, true);
        m_node.clhandler = std::make_unique<chainlock::ChainlockHandler>(*m_node.chainlocks, *m_node.chainman,
                                                                         *m_node.mempool, *m_node.mn_sync);
        // The rest is TestingSetup's constructor.
        RegisterAllCoreRPCCommands(tableRPC);
        LoadVerifyActivateChainstate();
        m_node.dstxman = std::make_unique<CDSTXManager>(*Assert(m_node.chainlocks));
        m_node.banman = std::make_unique<BanMan>(m_args.GetDataDirBase() / "banlist", nullptr, DEFAULT_MISBEHAVING_BANTIME);
        m_node.peerman = MakePeerManager(*m_node.connman, m_node, m_node.banman.get(), /*ignore_incoming_txs=*/false);
        CConnman::Options options;
        options.m_msgproc = m_node.peerman.get();
        options.socketEventsMode = ::g_socket_events_mode;
        m_node.connman->Init(options);
    }

    /** ChainTestingSetup::LoadVerifyActivateChainstate (test/util/setup_common.cpp:317-368)
     *  up to its ActivateBestChain, which ActivateLoaded runs: the reload looks
     *  at the loaded candidates in between. No scenario reindexes. */
    void LoadVerifyChainstate()
    {
        auto& chainman{*Assert(m_node.chainman)};
        if (m_node.peerman) {
            CConnman::Options connman_options;
            connman_options.socketEventsMode = ::g_socket_events_mode;
            m_node.connman->Init(connman_options);
            m_node.peerman.reset();
        }
        node::ChainstateLoadOptions options{ChainstateLoadOptionsForTest()};
        if (options.reindex || options.reindex_chainstate) throw std::runtime_error("reload: a reindex is not modelled");
        auto [status, error] = LoadChainstate(chainman, m_cache_sizes, options, *m_node.evodb, *m_node.dmnman,
                                              m_node.llmq_ctx, m_node.chain_helper);
        assert(status == node::ChainstateLoadStatus::SUCCESS);
        std::tie(status, error) = VerifyLoadedChainstate(chainman, options, *Assert(m_node.evodb), [](bool bls_state) {
            LogPrintf("LoadVerifyActivateChainstate: bls_legacy_scheme=%d\n", bls_state);
        });
        assert(status == node::ChainstateLoadStatus::SUCCESS);
    }

    void ActivateLoaded()
    {
        BlockValidationState state;
        if (!Assert(m_node.chainman)->ActiveChainstate().ActivateBestChain(state)) {
            throw std::runtime_error(strprintf("ActivateBestChain failed. (%s)", state.ToString()));
        }
    }

    ~ScenarioSetup()
    {
        // TestingSetup's destructor.
        m_node.peerman.reset();
        m_node.banman.reset();
        m_node.dstxman.reset();
        if (m_node.clhandler) m_node.clhandler->Stop();
        if (m_node.connman) m_node.connman->Stop();
        m_node.govman.reset();
        m_node.chain_helper.reset();
        m_node.llmq_ctx.reset();
    }
};

std::vector<const char*> CArgs(const std::vector<std::string>& v)
{
    std::vector<const char*> out;
    for (const auto& s : v) out.push_back(s.c_str());
    return out;
}

//! The block ConnectTip's BlockChecked (validation.cpp:3432) is about to report,
//! set by the ConnectBlock call before it on the same thread.
thread_local uint256 t_tip_checked;

class Scenario
{
public:
    Scenario(const std::string& chain, const std::vector<std::string>& args, const std::string& delivery = "boundary-flush")
        : m_args_store(args), m_setup(std::make_unique<ScenarioSetup>(chain, CArgs(m_args_store))),
          m_check_block_index(Params().DefaultConsistencyChecks())
    {
        auto& node = m_setup->m_node;
        if (delivery.rfind("random:", 0) == 0) {
            m_rng.emplace(std::stoull(delivery.substr(7)));
        } else if (delivery != "boundary-flush") {
            Fail("unknown delivery policy " + delivery);
        }
        // Setup's own chain signals (genesis activation) are delivered before
        // the trace starts, so it never sees half of a signal pair.
        SyncWithValidationInterfaceQueue();
        m_trace.label = [this](const uint256& h) { return Label(h); };
        // Each enforcement pass is bracketed in the trace: `EnforceBlock(X) #k`
        // where it starts (the signals emitted before it are in the trace
        // first) and `EnforceBlock(X) #k end` when the next pump finds its task
        // over, so the pass's own signals (MarkConflictingBlock's, then
        // ActivateBestChain's) lie between; `#k boundary` marks where the real
        // EnforceBlock returned. Passes are counted per lock from one drain to
        // the next (a main-thread `enforce` op's own pass opens the count of
        // the drain that follows), and Settle's passes continue the step's
        // count: #1 is exact up to its boundary, the rest is transient
        // (DEC-167; ClosePass records where each pass ended).
        // The hooks run inside Core's code on the handler's thread: an error is
        // kept for Drain to report rather than thrown through Core.
        g_enforce_entry = [this](const CBlockIndex* locked) {
            Guard([&] {
                SyncWithValidationInterfaceQueue();
                ClosePass();
                const std::string l = Label(locked->GetBlockHash());
                const int in_drain = ++m_passes[l], in_step = ++m_step_passes[l];
                // A settle pass exists only because an earlier one moved the
                // state, so it is never exact, even for a lock first enforced here.
                m_pass_no = m_settling ? std::max(in_step, 2) : in_drain;
                ++m_passes_run;
                m_open_pass = strprintf("EnforceBlock(%s) #%d", l, m_pass_no);
                m_pass_from = StateSnapshot();
                m_trace.Mark(*m_open_pass);
            });
        };
        g_enforce_boundary = [this](Chainstate& cs, const CBlockIndex* locked) {
            Guard([&] { Boundary(cs, locked); });
        };
        // X1: every ConnectBlock/DisconnectBlock call from here on, and the
        // acceptance-time BlockChecked verdicts (TraceListener::BlockChecked).
        // Core's own log lines count the calls independently of the seams:
        // ConnectTip logs "Load block from disk" before its ConnectBlock call and
        // DisconnectTip "Disconnect block" after its DisconnectBlock call
        // (validation.cpp:3424, :3288; BCLog::BENCHMARK, on under -debug), and
        // TestBlockValidity names itself when it fails before ConnectBlock.
        g_block_call = [this](const char* phase, const void* ret, const uint256& hash, UniValue result,
                              const BlockValidationState* state) {
            Guard([&] { RecordCall(phase, CallerOf(ret), hash, std::move(result), state); });
        };
        m_trace.checked = [this](const CBlock& b, const BlockValidationState& s) { Guard([&] { Checked(b, s); }); };
        m_log_callback = LogInstance().PushBackCallback([this](const std::string& line) {
            if (line.find("  - Load block from disk: ") != std::string::npos) ++m_log_connect_tip;
            if (line.find("- Disconnect block: ") != std::string::npos) ++m_log_disconnect_tip;
            // CVerifyDB::VerifyDB (validation.cpp:5054-5168): at level >= 3 it
            // disconnects each of the block_count blocks it reports, at 4 also
            // reconnects them, unless it skipped level 3 for the cache size.
            const auto scan = [&line](const char* lead, const char* fmt, auto*... out) {
                const size_t at = line.find(lead);
                return at != std::string::npos && sscanf(line.c_str() + at, fmt, out...) == int(sizeof...(out));
            };
            if (int depth, level; scan("Verifying last ", "Verifying last %d blocks at level %d", &depth, &level)) {
                m_log_verify_level = level;
                m_log_verify_skipped = false;
            }
            if (line.find("Skipped verification of level >=3") != std::string::npos) m_log_verify_skipped = true;
            if (int blocks; scan("Verification: No coin database", "Verification: No coin database inconsistencies in last %d blocks", &blocks) &&
                !m_log_verify_skipped) {
                if (m_log_verify_level >= 3) m_log_verify_disconnect += blocks;
                if (m_log_verify_level >= 4) m_log_verify_connect += blocks;
            }
            // Chainstate::ReplayBlocks logs each block before disconnecting it (validation.cpp:5274).
            if (line.find("Rolling back ") != std::string::npos) ++m_log_replay_disconnect;
            if (line.find("TestBlockValidity: Consensus::") != std::string::npos ||
                line.find("TestBlockValidity: conflicting with chainlock") != std::string::npos) {
                ++m_log_tbv_early;
            }
        });
        RegisterValidationInterface(&m_trace);
        // init.cpp:2153: the handler listens to validation signals in dashd;
        // here through m_trace (TraceListener). Its 5 s timer (Start()) is not
        // started: the `timer` op runs the same body at a chosen point.
        m_trace.handler = node.clhandler.get();
        Park();
        if (Params().NetworkIDString() == CBaseChainParams::REGTEST) {
            if (!node.sporkman->SetSporkAddress(Params().SporkAddress())) Fail("SetSporkAddress");
            if (!node.sporkman->SetPrivKey(REGTEST_SPORK_PRIVKEY)) Fail("SetPrivKey");
        } else {
            // No fixture can sign with another network's spork key, so the
            // harness signs SPORK_19 with a key of its own, installed as dashd's
            // -sporkaddr/-sporkkey would (init.cpp). A declared fixture seam:
            // only the spork signer changes.
            const uint256 seed = Hash(MakeUCharSpan(std::string{"sb4-core-scenarios spork key"}));
            CKey key;
            key.Set(seed.begin(), seed.end(), /*fCompressedIn=*/true);
            if (!node.sporkman->SetSporkAddress(EncodeDestination(PKHash(key.GetPubKey())))) Fail("SetSporkAddress");
            if (!node.sporkman->SetPrivKey(EncodeSecret(key))) Fail("SetPrivKey");
        }
        const CBlockIndex* genesis = WITH_LOCK(cs_main, return Chainman().ActiveChain().Genesis());
        m_built["G"] = Built{Params().GenesisBlock(), 0, ""};
        SetLabel(genesis->GetBlockHash(), "G");
        if (!Params().GetConsensus().hashDevnetGenesisBlock.IsNull()) {
            // A devnet's block 1 is fixed (CheckBlockHeader's devnet-genesis rule).
            m_built["DG"] = Built{Params().DevNetGenesisBlock(), 1, "G"};
            SetLabel(Params().DevNetGenesisBlock().GetHash(), "DG");
        }
        SnapshotPrev();
    }

    ~Scenario()
    {
        try {
            if (Node().clhandler) Drain();
        } catch (const std::exception& e) {
            std::cerr << "sb4_scenarios: teardown: " << e.what() << "\n";
        }
        Unpark();
        m_trace.handler = nullptr;
        g_enforce_entry = nullptr;
        g_enforce_boundary = nullptr;
        g_block_call = nullptr;
        m_trace.checked = nullptr;
        LogInstance().DeleteCallback(m_log_callback);
        UnregisterValidationInterface(&m_trace);
    }

    ChainstateManager& Chainman() { return *m_setup->m_node.chainman; }
    node::NodeContext& Node() { return m_setup->m_node; }
    CScheduler& Scheduler() { return *(Node().clhandler.get()->*get(RobClScheduler{})); }

    /** Holds the handler's scheduler thread, so whatever an op makes the handler
     *  schedule runs in Drain(), after the op, never in the middle of it. */
    void Park()
    {
        std::promise<void> parked;
        m_release = std::promise<void>{};
        Scheduler().scheduleFromNow(
            [&parked, release = m_release.get_future().share()] {
                parked.set_value();
                release.wait();
            },
            std::chrono::milliseconds{0});
        parked.get_future().get();
        m_parked = true;
    }

    void Unpark()
    {
        if (!m_parked) return;
        m_parked = false;
        m_release.set_value();
    }

    std::string Label(const uint256& h) const
    {
        LOCK(m_labels_mutex); // read by validation callbacks on other threads
        auto it = m_labels.find(h);
        return it == m_labels.end() ? "#" + h.ToString() : it->second;
    }

    UniValue LabelOrNull(const CBlockIndex* p) const { return p ? UniValue(Label(p->GetBlockHash())) : UniValue(); }

    const Built& GetBuilt(const std::string& label) const
    {
        auto it = m_built.find(label);
        if (it == m_built.end()) Fail("unknown label " + label);
        return it->second;
    }

    CBlockIndex* Index(const std::string& label)
    {
        const uint256 h = GetBuilt(label).block.GetHash();
        CBlockIndex* p = WITH_LOCK(cs_main, return Chainman().m_blockman.LookupBlockIndex(h));
        if (!p) Fail("label " + label + " is not in the block index");
        return p;
    }

    /** One delivery point of the schedule: hands waiting callbacks to the
     *  handler. `boundary-flush` delivers all of them. `random:<seed>` delivers
     *  a pseudo-random prefix (at least one when must_progress), until a
     *  drain has seen RANDOM_PASS_CAP enforcement passes; then it flushes, so
     *  the drain ends. */
    void Deliver(bool must_progress)
    {
        size_t n = m_trace.Pending();
        if (m_rng && m_drain_passes < RANDOM_PASS_CAP && n > 0) {
            const size_t k = std::uniform_int_distribution<size_t>(0, n)(*m_rng);
            n = must_progress ? std::max<size_t>(k, 1) : k;
        }
        m_trace.Deliver(n);
    }

    //! Runs an EnforceBlock hook inside Core: keeps the first exception for Drain.
    template <typename F>
    void Guard(F&& f) noexcept
    {
        try {
            f();
        } catch (...) {
            if (!m_hook_error) m_hook_error = std::current_exception();
        }
    }

    /** One ConnectBlock/DisconnectBlock call (X1): phase (connect, check =
     *  fJustCheck, disconnect), the calling Core function, the block, its
     *  occurrence (1-based, per phase and block), Core's result and state. */
    void RecordCall(const char* phase, const std::string& site, const uint256& hash, UniValue result,
                    const BlockValidationState* state)
    {
        if (site == "unknown") throw std::runtime_error(strprintf("%s of %s from an unlisted caller", phase, Label(hash)));
        LOCK(m_calls_mutex);
        UniValue e(UniValue::VOBJ);
        e.pushKV("step", m_step);
        e.pushKV("phase", phase);
        e.pushKV("site", site);
        e.pushKV("block", Label(hash));
        e.pushKV("hash", hash.ToString());
        e.pushKV("occurrence", ++m_occurrence[{phase, hash}]);
        e.pushKV("result", result);
        e.pushKV("state", state ? StateJson(*state) : UniValue());
        m_transcript.push_back(e);
        if (site == "Chainstate::ConnectTip") t_tip_checked = hash;
    }

    /** A BlockChecked signal: ConnectTip's (right after its ConnectBlock call, on
     *  the same thread) is counted; any other is an acceptance-time verdict
     *  (ProcessNewBlock's CheckBlock or AcceptBlock failure, validation.cpp). */
    void Checked(const CBlock& b, const BlockValidationState& s)
    {
        const uint256 hash = b.GetHash();
        LOCK(m_calls_mutex);
        if (!t_tip_checked.IsNull()) {
            if (t_tip_checked != hash) throw std::runtime_error("BlockChecked for another block than ConnectTip's");
            t_tip_checked.SetNull();
            ++m_tip_checked;
            return;
        }
        UniValue e(UniValue::VOBJ);
        e.pushKV("step", m_step);
        e.pushKV("block", Label(hash));
        e.pushKV("hash", hash.ToString());
        e.pushKV("state", StateJson(s));
        m_accept_checks.push_back(e);
    }

    void SnapshotPrev()
    {
        m_prev_status = Statuses();
        m_prev_entries = Entries();
    }

    std::string BlockHex(const std::string& label)
    {
        CDataStream s(SER_NETWORK, PROTOCOL_VERSION);
        s << GetBuilt(label).block;
        return HexStr(s);
    }

    /** A block given to ProcessNewBlock (blocks_submitted); a proposal only goes to
     *  TestBlockValidity, which stores nothing, so it is listed apart (blocks_proposed). */
    void Submitted(const std::string& label) { m_submitted.pushKV(label, BlockHex(label)); }

    /** The run's X1 record: the transcript, acceptance-time verdicts and Core's own counts. */
    UniValue Calls()
    {
        LOCK(m_calls_mutex);
        UniValue out(UniValue::VOBJ), tr(UniValue::VARR), ac(UniValue::VARR), counts(UniValue::VOBJ);
        for (const auto& e : m_transcript) tr.push_back(e);
        for (const auto& e : m_accept_checks) ac.push_back(e);
        counts.pushKV("connect_tip", m_log_connect_tip.load());
        counts.pushKV("disconnect_tip", m_log_disconnect_tip.load());
        counts.pushKV("verify_db_connect", m_log_verify_connect.load());
        counts.pushKV("verify_db_disconnect", m_log_verify_disconnect.load());
        counts.pushKV("replay_disconnect", m_log_replay_disconnect.load());
        counts.pushKV("test_block_validity", m_tbv_calls - m_log_tbv_early.load());
        counts.pushKV("tip_block_checked", m_tip_checked);
        out.pushKV("transcript", tr);
        out.pushKV("accept_checks", ac);
        out.pushKV("core_counts", counts);
        return out;
    }

    /** The EnforceBlock boundary: the state the real EnforceBlock left, then a delivery point. */
    void Boundary(Chainstate& cs, const CBlockIndex* locked)
    {
        SyncWithValidationInterfaceQueue();
        if (!m_open_pass) Fail("an EnforceBlock boundary outside a pass");
        m_trace.Mark(*m_open_pass + " boundary");
        {
            LOCK(cs_main);
            UniValue b(UniValue::VOBJ);
            b.pushKV("locked", Label(locked->GetBlockHash()));
            b.pushKV("pass", m_pass_no);
            b.pushKV("repeat", m_pass_no > 1);
            b.pushKV("tip", LabelOrNull(cs.m_chain.Tip()));
            b.pushKV("best_header", BestHeader());
            UniValue st(UniValue::VOBJ);
            for (const auto& [hash, s] : Statuses()) st.pushKV(Label(hash), static_cast<int64_t>(s));
            b.pushKV("statuses", st);
            LOCK(m_boundary_mutex);
            m_boundaries.push_back(b);
        }
        ++m_drain_passes;
        Deliver(/*must_progress=*/false);
    }

    /** The state a pass can change: tip, best header, failed blocks, the
     *  ChainLock state and every nStatus. */
    std::string StateSnapshot()
    {
        const auto [clsig, floor] = Node().chainlocks->GetBestChainlockWithPindex();
        LOCK(cs_main);
        std::set<std::string> failed;
        for (const CBlockIndex* p : Chainman().m_failed_blocks) failed.insert(Label(p->GetBlockHash()));
        std::string out = strprintf("tip %s best %s floor %s cl %s failed", LabelOrNull(Chainman().ActiveTip()).write(),
                                    BestHeader().write(), LabelOrNull(floor).write(), clsig.ToString());
        for (const auto& l : failed) out += " " + l;
        for (const auto& [hash, st] : Statuses()) out += strprintf(" %s:%d", Label(hash), st);
        return out;
    }

    /** Ends the open pass: its end marker, and where it left the state (a
     *  transient unless it is the fixed point) on its boundary record. */
    void ClosePass()
    {
        if (!m_open_pass) return;
        m_trace.Mark(*std::exchange(m_open_pass, std::nullopt) + " end");
        m_last_pass_changed = StateSnapshot() != m_pass_from;
        UniValue end(UniValue::VOBJ);
        {
            LOCK(cs_main);
            end.pushKV("tip", LabelOrNull(Chainman().ActiveTip()));
            end.pushKV("best_header", BestHeader());
        }
        end.pushKV("changed_state", m_last_pass_changed);
        LOCK(m_boundary_mutex);
        if (m_boundaries.empty()) Fail("a pass ended without its EnforceBlock boundary");
        m_boundaries.back().pushKV("pass_end", end);
    }

    /** DEC-167: once an op's passes are drained, runs the timer's
     *  EnforceBestChainLock on the handler's scheduler (and drains what it
     *  schedules) until a pass leaves the state as it found it, as dashd's
     *  next tip task or 5 s timer would. CheckActiveState stays out: it would
     *  observe the spork where no op asked for it (A-29(ix)); the `timer` op
     *  runs the whole timer body. */
    void Settle()
    {
        if (m_step_passes.empty()) return;
        m_settling = true;
        const int first = m_passes_run;
        while (m_last_pass_changed) {
            if (m_passes_run - first >= SETTLE_CAP) Fail(strprintf("no enforcement fixed point after %d settle passes", SETTLE_CAP));
            const int before = m_passes_run;
            auto* h = Node().clhandler.get();
            Scheduler().scheduleFromNow([h] { h->EnforceBestChainLock(); }, std::chrono::milliseconds{0});
            Drain();
            // EnforceBestChainLock returns early when the handler is off or the
            // lock has no block: the state the last pass left is then no fixed point.
            if (m_passes_run == before) Fail("a pass moved the state and the next EnforceBestChainLock made none");
        }
        m_settling = false;
    }

    /** Runs what the op left: releases the handler's scheduler, which runs the
     *  tasks the op scheduled and then a pump task. The pump (on the scheduler
     *  thread, so no handler task is running) syncs the validation queue and
     *  delivers the inbox (Deliver), and reschedules itself behind whatever that
     *  scheduled, until no task and no callback is left. Every task is
     *  scheduled from that one thread, so the order is the scheduling order. */
    void Drain()
    {
        auto& sched = Scheduler();
        SyncWithValidationInterfaceQueue();
        std::promise<void> idle;
        int rounds = 0;
        std::function<void()> pump = [&] {
            try {
                SyncWithValidationInterfaceQueue();
                if (m_hook_error) std::rethrow_exception(std::exchange(m_hook_error, nullptr));
                // Every task the handler runs is followed by a pump (each pump
                // reschedules itself behind what is queued), so an open pass
                // has ended here.
                ClosePass();
                std::chrono::steady_clock::time_point first, last;
                Deliver(/*must_progress=*/sched.getQueueInfo(first, last) == 0);
                if (sched.getQueueInfo(first, last) == 0 && m_trace.Pending() == 0) return idle.set_value();
                if (++rounds > 100000) Fail("the handler's scheduler did not go idle");
                sched.scheduleFromNow(pump, std::chrono::milliseconds{0});
            } catch (...) {
                idle.set_exception(std::current_exception());
            }
        };
        sched.scheduleFromNow(pump, std::chrono::milliseconds{0}); // while parked: after the op's tasks
        Unpark();
        idle.get_future().get();
        Park();
        m_drain_passes = 0;
        m_passes.clear();
    }

    /** m_best_header's label, or {"address_tie": [labels]} while it is still a
     *  reload's pick that tied others on work and nSequenceId (every loaded
     *  entry has id 0; validation.cpp:5359, blockstorage.cpp:32-49): Core
     *  compared addresses, so the view names the set (D9). Outside a load,
     *  AddToBlockIndex keeps the first seen of equal work, an order, not an
     *  address. */
    UniValue BestHeader() EXCLUSIVE_LOCKS_REQUIRED(cs_main)
    {
        if (Chainman().m_best_header != m_best_tie_pick) m_best_tie.clear();
        if (!m_best_tie.empty()) {
            UniValue labels(UniValue::VARR);
            for (const auto& l : m_best_tie) labels.push_back(l);
            UniValue o(UniValue::VOBJ);
            o.pushKV("address_tie", labels);
            return o;
        }
        return LabelOrNull(Chainman().m_best_header);
    }

    /** A diagnostic label (DEC-175): some ways Core may have decided by heap
     *  address in this run. It is incomplete (a live tie after a precious reset
     *  is not noted) and decides nothing: regen.py replays every run under
     *  perturbed heaps and cuts it if any two runs differ. Two kinds, each noted
     *  once, at the first step it holds:
     *  `failed_blocks`, two or more entries in m_failed_blocks (a pointer-ordered
     *  set AcceptBlockHeader walks), checked at every op's start and end and
     *  before every block or header submission; and `loaded_tie`, two loaded
     *  entries equal on work (all have nSequenceId 0, so
     *  CBlockIndexWorkComparator compares their addresses wherever they meet). */
    UniValue PossiblySensitive() const
    {
        UniValue out(UniValue::VARR);
        for (const auto& r : m_sensitive) out.push_back(r);
        return out;
    }

    void NoteSensitive(const std::string& kind)
    {
        for (const auto& r : m_sensitive) {
            if (r.rfind(kind + " ", 0) == 0) return;
        }
        m_sensitive.push_back(strprintf("%s at step %d", kind, m_step));
    }

    void NoteFailedBlocks()
    {
        if (WITH_LOCK(cs_main, return Chainman().m_failed_blocks.size()) >= 2) NoteSensitive("failed_blocks");
    }

    void NoteLoadedTies()
    {
        LOCK(cs_main);
        std::set<arith_uint256> work;
        for (const auto& [_, p] : Chainman().BlockIndex()) {
            if (!work.insert(p.nChainWork).second) return NoteSensitive("loaded_tie");
        }
    }

    /** After a load: the labels tying `pick` under CBlockIndexWorkComparator
     *  (equal work and nSequenceId; the comparator then compares addresses). */
    std::set<std::string> AddressTie(const CBlockIndex* pick) EXCLUSIVE_LOCKS_REQUIRED(cs_main)
    {
        std::set<std::string> tie;
        if (!pick) return tie;
        for (const auto& [hash, p] : Chainman().BlockIndex()) {
            if (p.nChainWork == pick->nChainWork && p.nSequenceId == pick->nSequenceId &&
                p.IsValid(BLOCK_VALID_TREE)) {
                tie.insert(Label(hash));
            }
        }
        return tie.size() > 1 ? tie : std::set<std::string>{};
    }

    /** X3: every index entry's (nSequenceId, nChainTx). */
    std::map<uint256, std::pair<int32_t, uint64_t>> Entries()
    {
        LOCK(cs_main);
        std::map<uint256, std::pair<int32_t, uint64_t>> out;
        for (const auto& [hash, index] : Chainman().BlockIndex()) out[hash] = {index.nSequenceId, index.nChainTx};
        return out;
    }

    /** `--audit-index`'s lines: the pointers Core orders by address (D9) are the
     *  CBlockIndex objects inside BlockMap's nodes, which a re-pin may resize.
     *  It prints each entry's address and region offset, then CheckRegion. */
    void AuditIndex()
    {
        LOCK(cs_main);
        const auto& index = Chainman().BlockIndex();
        const uint64_t live = g_nodes_served - g_nodes_freed.load();
        std::cerr << "HX1_SIZE index=" << sizeof(CBlockIndex) << " node=" << sizeof(BlockMapNode)
                  << " align=" << alignof(BlockMapNode) << " slot=" << PERTURB_SLOT << " step=" << m_step
                  << " entries=" << index.size() << " live_slots=" << live << "\n";
        for (const auto& [hash, p] : index) {
            const auto* q = reinterpret_cast<const char*>(&p);
            std::cerr << "HX1_INDEX step=" << m_step << " label=" << Label(hash) << " pointer="
                      << static_cast<const void*>(q) << " region=" << InRegion(q)
                      << " offset=" << (InRegion(q) ? q - g_region : -1) << "\n";
        }
        CheckRegion(index);
    }

    /** X3 (designs/CH1-node.md §11): the selection state after an op. `entries`
     *  are the (nSequenceId, nChainTx) pairs that changed since the last dump
     *  (unprojected nStatus is the view's status_changes); `candidates` is
     *  setBlockIndexCandidates in its comparator's order (least work first);
     *  `unlinked` is m_blocks_unlinked's (parent, child) pairs, each parent's
     *  children in multimap order; `cl` the Chainlocks members as stored. Two
     *  orders come from heap addresses (D1, D9), and the dump flags them:
     *  adjacent candidates equal on work and nSequenceId (`candidate_address_ties`,
     *  which only loaded entries, all id 0, can be), and the order of distinct
     *  parents in m_blocks_unlinked, a pointer-keyed multimap
     *  (`unlinked_parents_address_ordered`; the dump lists them by label so two
     *  runs compare byte for byte). */
    UniValue Dump()
    {
        if (g_audit_index) AuditIndex();
        WITH_LOCK(cs_main, CheckRegion(Chainman().BlockIndex()));
        UniValue d(UniValue::VOBJ);
        const auto now = Entries();
        UniValue entries(UniValue::VOBJ);
        for (const auto& [hash, e] : now) {
            auto it = m_prev_entries.find(hash);
            if (it != m_prev_entries.end() && it->second == e) continue;
            UniValue pair(UniValue::VARR);
            pair.push_back(e.first);
            pair.push_back(e.second);
            entries.pushKV(Label(hash), pair);
        }
        m_prev_entries = now;
        d.pushKV("entries", entries);
        {
            LOCK(cs_main);
            UniValue cands(UniValue::VARR), ties(UniValue::VARR);
            const CBlockIndex* prev = nullptr;
            for (const CBlockIndex* p : Chainman().ActiveChainstate().setBlockIndexCandidates) {
                cands.push_back(Label(p->GetBlockHash()));
                if (prev && prev->nChainWork == p->nChainWork && prev->nSequenceId == p->nSequenceId) {
                    UniValue tie(UniValue::VARR);
                    tie.push_back(Label(prev->GetBlockHash()));
                    tie.push_back(Label(p->GetBlockHash()));
                    ties.push_back(tie);
                }
                prev = p;
            }
            d.pushKV("candidates", cands);
            if (!ties.empty()) d.pushKV("candidate_address_ties", ties);
            // Parents by label (their multimap order is their addresses'), each
            // parent's children in multimap order (insertion order for one key).
            std::map<std::string, std::vector<std::string>> by_parent;
            for (const auto& [parent, child] : Chainman().m_blockman.m_blocks_unlinked) {
                by_parent[Label(parent->GetBlockHash())].push_back(Label(child->GetBlockHash()));
            }
            UniValue unlinked(UniValue::VARR);
            for (const auto& [parent, children] : by_parent) {
                for (const auto& child : children) {
                    UniValue pair(UniValue::VARR);
                    pair.push_back(parent);
                    pair.push_back(child);
                    unlinked.push_back(pair);
                }
            }
            d.pushKV("unlinked", unlinked);
            if (by_parent.size() > 1) d.pushKV("unlinked_parents_address_ordered", true);
        }
        auto& cl = *Node().chainlocks;
        auto sig = [&](const chainlock::ChainLockSig& s) {
            return s.IsNull() ? UniValue() : UniValue(strprintf("%s@%d", Label(s.getBlockHash()), s.getHeight()));
        };
        UniValue c(UniValue::VOBJ);
        {
            LOCK(cl.*get(RobChainlocksCs{}));
            c.pushKV("best", sig(cl.*get(RobBestChainLock{})));
            const uint256& hash = cl.*get(RobBestChainLockHash{}); // the CLSIG's hash, not a block's
            c.pushKV("best_hash", hash.IsNull() ? UniValue() : UniValue(hash.ToString()));
            c.pushKV("best_with_known_block", sig(cl.*get(RobBestChainLockWithKnownBlock{})));
        }
        d.pushKV("cl", c);
        return d;
    }

    std::map<uint256, uint32_t> Statuses()
    {
        LOCK(cs_main);
        std::map<uint256, uint32_t> out;
        for (const auto& [hash, index] : Chainman().BlockIndex()) out[hash] = index.nStatus;
        return out;
    }

    /** Names preloaded real headers R<height> (genesis stays G) so ops can build on them. */
    void AdoptPreloaded(const std::vector<CBlockHeader>& headers)
    {
        for (size_t h = 1; h < headers.size(); ++h) {
            const std::string label = "R" + std::to_string(h);
            m_built[label] = Built{CBlock(headers[h]), static_cast<int>(h), h == 1 ? "G" : "R" + std::to_string(h - 1)};
            SetLabel(headers[h].GetHash(), label);
        }
        SnapshotPrev();
    }

    // ---- building ----------------------------------------------------------

    int64_t MedianTimePast(const std::string& label) const
    {
        std::vector<int64_t> times;
        std::string cur = label;
        for (int i = 0; i < 11 && !cur.empty(); ++i) {
            const Built& b = GetBuilt(cur);
            times.push_back(b.block.nTime);
            cur = b.parent;
        }
        std::sort(times.begin(), times.end());
        return times[times.size() / 2];
    }

    CBlock MakeBlock(const std::string& parent, int height, const std::string& tag, const UniValue& o)
    {
        const Built& p = GetBuilt(parent);
        CMutableTransaction cb;
        cb.nVersion = 1;
        cb.vin.resize(1);
        cb.vin[0].prevout.SetNull();
        // invalid: "cb-amount" (more than any subsidy: ConnectBlock rejects it) or
        // "cb-height" (BIP34's height off by one: ContextualCheckBlock rejects it
        // before DIP3, validation.cpp:4580).
        const std::string invalid = o.exists("invalid") ? o["invalid"].get_str() : "";
        if (!invalid.empty() && invalid != "cb-amount" && invalid != "cb-height") Fail("unknown invalid kind " + invalid);
        cb.vin[0].scriptSig = CScript() << (invalid == "cb-height" ? height + 1 : height)
                                        << std::vector<unsigned char>(tag.begin(), tag.end());
        const CAmount value = invalid == "cb-amount" ? 21000000 * COIN : 0;
        cb.vout.emplace_back(value, CScript() << OP_TRUE);
        CBlock b;
        b.nVersion = o.exists("version") ? o["version"].getInt<int32_t>() : 0x20000000;
        b.hashPrevBlock = p.block.GetHash();
        // The first block after genesis starts an hour before the mock clock.
        int64_t t = parent == "G" ? GetTime() - 3600 : p.block.nTime + 1;
        if (o.exists("time")) {
            const std::string spec = o["time"].get_str();
            const auto plus = spec.find('+');
            const auto minus = spec.find('-');
            const auto pos = plus != std::string::npos ? plus : minus;
            const std::string base = spec.substr(0, pos);
            const int64_t off = pos == std::string::npos ? 0 : std::stoll(spec.substr(pos));
            if (base == "parent") t = p.block.nTime + off;
            else if (base == "mtp") t = MedianTimePast(parent) + off;
            else if (base == "now") t = GetTime() + off;
            else Fail("bad time spec " + spec);
        }
        b.nTime = static_cast<uint32_t>(t);
        // bits: hex, "powlimit", or "next" (Core's GetNextWorkRequired on the
        // parent, which must be in the index). Default: powlimit on regtest,
        // which never retargets; next elsewhere.
        const std::string bits = o.exists("bits") ? o["bits"].get_str()
                                 : Params().NetworkIDString() == CBaseChainParams::REGTEST ? "powlimit"
                                                                                           : "next";
        if (bits == "powlimit") {
            b.nBits = UintToArith256(Params().GetConsensus().powLimit).GetCompact();
        } else if (bits == "next") {
            const CBlockIndex* prev = Index(parent);
            LOCK(cs_main);
            b.nBits = GetNextWorkRequired(prev, &b, Params().GetConsensus());
        } else {
            b.nBits = static_cast<uint32_t>(std::stoul(bits, nullptr, 16));
        }
        b.vtx = {MakeTransactionRef(std::move(cb))};
        b.hashMerkleRoot = BlockMerkleRoot(b);
        const bool want_valid_pow = !(o.exists("pow") && o["pow"].get_str() == "fail");
        while (CheckProofOfWork(b.GetHash(), b.nBits, Params().GetConsensus()) != want_valid_pow) ++b.nNonce;
        return b;
    }

    std::vector<std::string> Expand(const UniValue& list) const
    {
        std::vector<std::string> out;
        for (const auto& v : list.getValues()) {
            const std::string s = v.get_str();
            const auto dots = s.find("..");
            if (dots == std::string::npos) {
                out.push_back(s);
                continue;
            }
            const std::string lo = s.substr(0, dots), hi = s.substr(dots + 2);
            size_t i = 0;
            while (i < lo.size() && !isdigit(lo[i])) ++i;
            const std::string prefix = lo.substr(0, i);
            const int a = std::stoi(lo.substr(i)), b = std::stoi(hi.substr(prefix.size()));
            for (int h = a; h <= b; ++h) out.push_back(prefix + std::to_string(h));
        }
        return out;
    }

    // ---- op implementations ------------------------------------------------

    UniValue OpBuild(const UniValue& o)
    {
        const std::string branch = o["branch"].get_str();
        std::string parent = o["parent"].get_str();
        const int count = o["count"].getInt<int>();
        std::vector<std::string> made;
        for (int i = 0; i < count; ++i) {
            const int height = GetBuilt(parent).height + 1;
            const std::string label = branch + std::to_string(height);
            if (m_built.count(label)) Fail("label reused: " + label);
            CBlock b = MakeBlock(parent, height, branch, o);
            // Default times run one second apart; keep the mock clock just
            // ahead of them so long chains stay out of IBD and in the future
            // window. An explicit "time" never moves the clock.
            if (!o.exists("time") && b.nTime + 60 > GetTime()) SetMockTime(b.nTime + 60);
            SetLabel(b.GetHash(), label);
            m_built[label] = Built{b, height, parent};
            made.push_back(label);
            parent = label;
        }
        UniValue r(UniValue::VOBJ);
        r.pushKV("built", made.front() + ".." + made.back());
        // Long runs list only their ends.
        const auto shown = [&](const std::string& l) { return made.size() <= 64 || l == made.front() || l == made.back(); };
        UniValue hashes(UniValue::VOBJ);
        for (const auto& l : made) {
            if (shown(l)) hashes.pushKV(l, GetBuilt(l).block.GetHash().ToString());
        }
        r.pushKV("hashes", hashes);
        // Every block's 80-byte header, so a consumer can replay the scenario.
        UniValue headers(UniValue::VOBJ);
        for (const auto& l : made) {
            CDataStream s(SER_NETWORK, PROTOCOL_VERSION);
            s << GetBuilt(l).block.GetBlockHeader();
            headers.pushKV(l, HexStr(s));
        }
        r.pushKV("headers", headers);
        if (o.exists("bits")) {
            UniValue bits(UniValue::VOBJ);
            for (const auto& l : made) {
                if (shown(l)) bits.pushKV(l, strprintf("%08x", GetBuilt(l).block.nBits));
            }
            r.pushKV("bits", bits);
        }
        const std::string submit = o.exists("submit") ? o["submit"].get_str() : "none";
        if (submit == "connect") {
            UniValue c = Connect(made);
            r.pushKV("connect", c);
        } else if (submit == "headers") {
            r.pushKV("pnbh", Pnbh(made));
        } else if (submit != "none") {
            Fail("bad submit " + submit);
        }
        return r;
    }

    UniValue Connect(const std::vector<std::string>& labels)
    {
        UniValue r(UniValue::VARR);
        for (const auto& l : labels) {
            bool new_block = false;
            auto block = std::make_shared<const CBlock>(GetBuilt(l).block);
            NoteFailedBlocks();
            Submitted(l);
            const bool ok = Chainman().ProcessNewBlock(block, /*force_processing=*/true, &new_block);
            Drain();
            if (!ok || !new_block) r.push_back(strprintf("%s ok=%d new=%d", l, ok, new_block));
        }
        return r; // only the exceptions, so long runs stay small
    }

    /** One ProcessNewBlock call with a chosen force_processing (false: AcceptBlock
     *  stores an unrequested block only if it adds work and is not too far ahead,
     *  validation.cpp AcceptBlock), and its result. */
    UniValue OpPnb(const UniValue& o)
    {
        const std::string l = o["block"].get_str();
        const bool force = o["force"].get_bool();
        bool new_block = false;
        NoteFailedBlocks();
        Submitted(l);
        const bool ok = Chainman().ProcessNewBlock(std::make_shared<const CBlock>(GetBuilt(l).block), force, &new_block);
        Drain();
        UniValue r(UniValue::VOBJ);
        r.pushKV("ok", ok);
        r.pushKV("new_block", new_block);
        return r;
    }

    /** One ProcessNewBlockHeaders batch, with the block-tree records it caused. */
    UniValue Pnbh(const std::vector<std::string>& labels)
    {
        std::vector<CBlockHeader> headers;
        for (const auto& l : labels) headers.push_back(GetBuilt(l).block.GetBlockHeader());
        NoteFailedBlocks();
        Flush();
        const auto before = BlockTreeRecords();
        BlockValidationState state;
        const CBlockIndex* last = nullptr;
        const bool ok = Chainman().ProcessNewBlockHeaders(headers, state, &last);
        Drain();
        Flush();
        const auto after = BlockTreeRecords();
        UniValue r(UniValue::VOBJ);
        r.pushKV("ok", ok);
        r.pushKV("state", StateJson(state));
        r.pushKV("last_accepted", LabelOrNull(last));
        UniValue recs(UniValue::VOBJ);
        for (const auto& [hash, bytes] : after) {
            auto it = before.find(hash);
            if (it == before.end() || it->second != bytes) recs.pushKV(Label(hash), HexStr(bytes));
        }
        r.pushKV("records_written", recs);
        return r;
    }

    void Flush()
    {
        LOCK(cs_main);
        Chainman().ActiveChainstate().ForceFlushStateToDisk();
    }

    /** The block tree DB's 'b' records, re-serialized by Core (CDiskBlockIndex). */
    std::map<uint256, std::vector<unsigned char>> BlockTreeRecords()
    {
        LOCK(cs_main);
        std::map<uint256, std::vector<unsigned char>> out;
        auto& db = *Chainman().m_blockman.m_block_tree_db;
        std::unique_ptr<CDBIterator> it(db.NewIterator());
        it->Seek(std::make_pair(uint8_t{'b'}, uint256()));
        for (; it->Valid(); it->Next()) {
            std::pair<uint8_t, uint256> key;
            if (!it->GetKey(key) || key.first != 'b') break;
            CDiskBlockIndex disk;
            if (!it->GetValue(disk)) Fail("unreadable block index record");
            CDataStream s(SER_DISK, CLIENT_VERSION);
            s << disk;
            out[key.second] = std::vector<unsigned char>(UCharCast(s.data()), UCharCast(s.data() + s.size()));
        }
        return out;
    }

    chainlock::ChainLockSig MakeClsig(const UniValue& o, std::string& label_out)
    {
        uint256 hash;
        int height;
        if (o.exists("block")) {
            label_out = o["block"].get_str();
            const Built& b = GetBuilt(label_out);
            hash = b.block.GetHash();
            height = b.height;
        } else {
            label_out = "?" + o["unknown"].get_str();
            hash = Hash(MakeUCharSpan(label_out));
            SetLabel(hash, label_out);
            height = o["height"].getInt<int>();
        }
        if (o.exists("height")) height = o["height"].getInt<int>();
        return chainlock::ChainLockSig{height, hash, CBLSSignature{}};
    }

    UniValue OpCl(const UniValue& o)
    {
        std::string label;
        const auto clsig = MakeClsig(o, label);
        UniValue r(UniValue::VOBJ);
        r.pushKV("lock", strprintf("%s@%d", label, clsig.getHeight()));
        const std::string via = o.exists("via") ? o["via"].get_str() : "p2p";
        if (via == "p2p") {
            // A CLSIG message is dropped unread while SPORK_19 is off
            // (net_processing.cpp:5794), as is a coinbase lock (handler.cpp:105).
            if (!Node().chainlocks->IsEnabled()) {
                r.pushKV("dropped", "spork19-off");
                return r;
            }
        } else if (via == "submitchainlock") {
            // The RPC (rpc/quorums.cpp:1362-1397) has no spork gate. Replayed
            // from its height checks on (:1366-1373); its signature decoding and
            // check (the 3-argument VerifyChainLock) are not run, as no quorum
            // exists.
            if (clsig.getHeight() <= 0) {
                r.pushKV("error", "invalid block height");
                return r;
            }
            const int best = Node().chainlocks->GetBestChainLock().getHeight();
            if (clsig.getHeight() <= best) {
                r.pushKV("dropped", strprintf("height <= best %d", best));
                return r;
            }
        } else {
            Fail("cl: via must be p2p or submitchainlock, not " + via);
        }
        const std::string sig = o.exists("sig") ? o["sig"].get_str() : "valid";
        if (sig != "valid" && sig != "bad") Fail("cl: sig must be valid or bad, not " + sig);
        const int calls_before = g_verify_calls;
        g_verify_status = sig == "bad" ? llmq::VerifyRecSigStatus::Invalid : llmq::VerifyRecSigStatus::Valid;
        const auto res = Node().clhandler->ProcessNewChainLock(/*from=*/-1, clsig, *Node().llmq_ctx->qman,
                                                               ::SerializeHash(clsig));
        g_verify_status = llmq::VerifyRecSigStatus::Valid;
        Drain();
        r.pushKV(sig == "bad" ? "verify_failed" : "verify_bypassed", g_verify_calls - calls_before);
        r.pushKV("relayed", !res.m_inventory.empty());
        return r;
    }

    UniValue OpSeedCl(const UniValue& o)
    {
        std::string label;
        const auto clsig = MakeClsig(o, label);
        const CBlockIndex* pindex = WITH_LOCK(cs_main, return Chainman().m_blockman.LookupBlockIndex(clsig.getBlockHash()));
        const bool updated = Node().chainlocks->UpdateBestChainlock(::SerializeHash(clsig), clsig, pindex);
        UniValue r(UniValue::VOBJ);
        r.pushKV("lock", strprintf("%s@%d", label, clsig.getHeight()));
        r.pushKV("updated", updated);
        return r;
    }

    UniValue OpSpork19(const UniValue& o)
    {
        const std::string v = o["value"].get_str();
        // A signed spork replaces the previous one only with a later signing
        // time (spork.cpp:110-124): step the mock clock first.
        SetMockTime(GetTime() + 1);
        const auto inv = Node().sporkman->UpdateSpork(SPORK_19_CHAINLOCKS_ENABLED, v == "on" ? 0 : SPORK_OFF);
        if (!inv) Fail("UpdateSpork failed");
        UniValue r(UniValue::VOBJ);
        r.pushKV("spork19", v);
        return r;
    }

    UniValue OpNotify(const UniValue& o)
    {
        auto* h = Node().clhandler.get();
        const CBlockIndex* n = o["new"].isNull() ? nullptr : Index(o["new"].get_str());
        const CBlockIndex* f = o["fork"].isNull() ? nullptr : Index(o["fork"].get_str());
        // The scheduler is parked, so whatever UpdatedBlockTip schedules is
        // still queued when we look.
        (h->*get(RobClUpdatedBlockTip{}))(n, f, o["ibd"].get_bool());
        std::chrono::steady_clock::time_point first, last;
        const bool scheduled = Scheduler().getQueueInfo(first, last) > 0;
        Drain();
        UniValue r(UniValue::VOBJ);
        r.pushKV("scheduled", scheduled);
        return r;
    }

    UniValue OpTimer()
    {
        // ChainlockHandler::Start's 5 s body, on the handler's own scheduler.
        auto* h = Node().clhandler.get();
        Scheduler().scheduleFromNow(
            [h] {
                h->CheckActiveState();
                h->EnforceBestChainLock();
                (h->*get(RobClCleanup{}))();
            },
            std::chrono::milliseconds{0});
        Drain();
        return UniValue();
    }

    UniValue Rpc(const std::string& method, const UniValue& params)
    {
        if (RPCIsInWarmup(nullptr)) SetRPCWarmupFinished(); // as rpc_tests.cpp:62 does
        JSONRPCRequest req;
        req.context = CoreContext{Node()};
        req.strMethod = method;
        req.params = params;
        return tableRPC.execute(req);
    }

    UniValue OpRpcBlock(const std::string& method, const std::string& label, const UniValue& extra = UniValue())
    {
        UniValue params(UniValue::VARR);
        params.push_back(GetBuilt(label).block.GetHash().ToString());
        if (!extra.isNull()) params.push_back(extra);
        UniValue r(UniValue::VOBJ);
        try {
            Rpc(method, params);
            r.pushKV("ok", true);
        } catch (const UniValue& e) {
            r.pushKV("error", e["message"].get_str());
        }
        Drain();
        return r;
    }

    /** getblocktemplate's proposal mode (BIP23, rpc/mining.cpp:696-724), Core's
     *  caller of TestBlockValidity (fJustCheck): its BIP22 verdict, null if valid. */
    UniValue OpPropose(const std::string& label)
    {
        const std::string hex = BlockHex(label);
        m_proposed.pushKV(label, hex);
        UniValue req(UniValue::VOBJ), params(UniValue::VARR);
        req.pushKV("mode", "proposal");
        req.pushKV("data", hex);
        params.push_back(req);
        UniValue r = Rpc("getblocktemplate", params);
        static const std::set<std::string> before_tbv{"duplicate", "duplicate-invalid", "duplicate-inconclusive",
                                                      "inconclusive-not-best-prevblk"};
        if (!r.isStr() || !before_tbv.count(r.get_str())) ++m_tbv_calls;
        Drain();
        UniValue out(UniValue::VOBJ);
        out.pushKV("verdict", r);
        return out;
    }

    UniValue OpMarkConflicting(const std::string& label)
    {
        BlockValidationState state;
        bool ok;
        {
            LOCK(cs_main);
            ok = (Chainman().ActiveChainstate().*get(RobMarkConflicting{}))(state, Index(label));
        }
        BlockValidationState abc;
        Chainman().ActiveChainstate().ActivateBestChain(abc);
        Drain();
        UniValue r(UniValue::VOBJ);
        r.pushKV("ok", ok);
        r.pushKV("state", StateJson(state));
        return r;
    }

    UniValue OpCheckpoint(const UniValue& o)
    {
        auto& params = const_cast<CChainParams&>(Params());
        const Built& b = GetBuilt(o["block"].get_str());
        (params.*get(RobCheckpoints{})).mapCheckpoints[b.height] = b.block.GetHash();
        UniValue r(UniValue::VOBJ);
        r.pushKV("checkpoint", strprintf("%d %s", b.height, o["block"].get_str()));
        return r;
    }

    UniValue OpLocator(const std::string& label)
    {
        const CBlockLocator loc = WITH_LOCK(cs_main, return GetLocator(Index(label)));
        UniValue r(UniValue::VARR);
        for (const auto& h : loc.vHave) r.push_back(Label(h));
        return r;
    }

    UniValue OpDiskIndex(const UniValue& list)
    {
        UniValue r(UniValue::VOBJ);
        LOCK(cs_main);
        for (const auto& l : Expand(list)) {
            CDataStream s(SER_DISK, CLIENT_VERSION);
            s << CDiskBlockIndex{Index(l)};
            r.pushKV(l, HexStr(s));
        }
        return r;
    }

    UniValue OpReload(bool check_block_index, bool checkpoints = true)
    {
        m_check_block_index = check_block_index;
        // A node restart (validation_chainstatemanager_tests' SimulateNodeRestart
        // plus init.cpp's order): flush, drop the chainstate, the ChainLock
        // state and handler (dashd starts with neither), reload from disk.
        // Sporks survive, as sporks.dat does (init.cpp:1967-1974).
        auto& node = Node();
        Drain();
        Unpark();
        m_trace.handler = nullptr;
        {
            LOCK(cs_main);
            Chainman().ActiveChainstate().ForceFlushStateToDisk();
        }
        node.clhandler.reset();
        {
            LOCK(cs_main);
            // validation_chainstatemanager_tests' DashChainstateSetupClose
            node.chain_helper.reset();
            node.llmq_ctx.reset();
            Chainman().ResetChainstates();
            const ChainstateManager::Options opts{
                .chainparams = ::Params(), .check_block_index = check_block_index, .checkpoints_enabled = checkpoints};
            node.chainman.reset();
            node.chainman.reset(new ChainstateManager(opts));
        }
        // Everything that refers to the ChainLock state goes with it.
        node.dstxman.reset();
        node.chainlocks = std::make_unique<chainlock::Chainlocks>(*node.sporkman);
        m_setup->LoadVerifyChainstate();
        NoteLoadedTies();
        {
            LOCK(cs_main);
            m_best_tie_pick = Chainman().m_best_header;
            m_best_tie = AddressTie(m_best_tie_pick);
        }
        m_setup->ActivateLoaded();
        node.clhandler = std::make_unique<chainlock::ChainlockHandler>(*node.chainlocks, *node.chainman, *node.mempool,
                                                                        *node.mn_sync);
        node.dstxman = std::make_unique<CDSTXManager>(*node.chainlocks);
        // The load's own signals predate the handler, as in dashd.
        SyncWithValidationInterfaceQueue();
        m_trace.handler = node.clhandler.get();
        Park();
        Drain();
        return UniValue(UniValue::VOBJ);
    }

    // ---- the per-step view -------------------------------------------------

    UniValue View()
    {
        UniValue v(UniValue::VOBJ);
        auto& chainman = Chainman();
        {
            LOCK(cs_main);
            const CBlockIndex* tip = chainman.ActiveTip();
            v.pushKV("tip", LabelOrNull(tip));
            const UniValue best = BestHeader();
            v.pushKV("best_header", best);
            v.pushKV("locator0", !chainman.m_best_header || best.isObject()
                                     ? best
                                     : UniValue(Label(GetLocator(chainman.m_best_header).vHave[0])));
            std::set<std::string> failed;
            for (const CBlockIndex* p : chainman.m_failed_blocks) failed.insert(Label(p->GetBlockHash()));
            UniValue f(UniValue::VARR);
            for (const auto& l : failed) f.push_back(l);
            v.pushKV("failed_blocks", f);
        }
        UniValue cl(UniValue::VOBJ);
        const auto best = Node().chainlocks->GetBestChainLock();
        const auto [known, floor] = Node().chainlocks->GetBestChainlockWithPindex();
        cl.pushKV("best", best.IsNull() ? UniValue() : UniValue(strprintf("%s@%d", Label(best.getBlockHash()), best.getHeight())));
        cl.pushKV("best_known", known.IsNull() ? UniValue() : UniValue(strprintf("%s@%d", Label(known.getBlockHash()), known.getHeight())));
        cl.pushKV("floor", LabelOrNull(floor));
        cl.pushKV("observed", (Node().clhandler.get()->*get(RobClEnabled{})).load());
        cl.pushKV("spork_active", Node().chainlocks->IsEnabled());
        v.pushKV("cl", cl);

        const auto now = Statuses();
        UniValue st(UniValue::VOBJ);
        for (const auto& [hash, s] : now) {
            auto it = m_prev_status.find(hash);
            if (it == m_prev_status.end() || it->second != s) st.pushKV(Label(hash), static_cast<int64_t>(s));
        }
        for (const auto& [hash, s] : m_prev_status) {
            if (!now.count(hash)) st.pushKV(Label(hash), "absent");
        }
        m_prev_status = now;
        v.pushKV("status_changes", st);
        v.pushKV("dump", Dump());

        {
            LOCK(m_boundary_mutex);
            if (!m_boundaries.empty()) {
                UniValue bs(UniValue::VARR);
                for (auto& b : m_boundaries) bs.push_back(std::move(b));
                m_boundaries.clear();
                v.pushKV("enforce_block_boundaries", bs);
            }
        }
        UniValue trace(UniValue::VARR);
        const auto events = m_trace.Take();
        // DEC-167: pass #1 from its boundary marker to its end marker, and
        // each later pass from its marker to its end marker, are transients;
        // their number and content depend on the delivery schedule. The rest
        // of the trace is exact.
        if (m_open_pass) Fail("an enforcement pass is still open");
        UniValue spans(UniValue::VARR);
        const auto ends_with = [](const std::string& s, const std::string& x) {
            return s.size() >= x.size() && s.compare(s.size() - x.size(), x.size(), x) == 0;
        };
        for (size_t i = 0; i < events.size(); ++i) {
            const auto& e = events[i];
            if (e.rfind("EnforceBlock(", 0) != 0 || ends_with(e, " end")) continue;
            const bool boundary = ends_with(e, " boundary");
            if (boundary != ends_with(e, " #1 boundary") || (!boundary && ends_with(e, " #1"))) continue;
            const std::string end = e.substr(0, boundary ? e.size() - 9 : e.size()) + " end";
            size_t j = i + 1;
            while (j < events.size() && events[j] != end) ++j;
            if (j == events.size()) Fail("unterminated " + e);
            UniValue span(UniValue::VARR);
            span.push_back(static_cast<int64_t>(i));
            span.push_back(static_cast<int64_t>(j));
            spans.push_back(span);
        }
        for (const auto& e : events) trace.push_back(e);
        v.pushKV("trace", trace);
        v.pushKV("trace_transient_spans", spans);

        // getchaintips through Core's RPC, then §10.2's projection: equal
        // heights sorted by hash (D9), block-data statuses → header-valid.
        const UniValue tips = Rpc("getchaintips", UniValue(UniValue::VARR));
        std::vector<std::tuple<int64_t, std::string, UniValue>> rows;
        for (const auto& t : tips.getValues()) {
            const uint256 h = uint256S(t["hash"].get_str());
            const std::string status = t["status"].get_str();
            const std::string projected =
                status == "headers-only" || status == "valid-fork" || status == "valid-headers" ? "header-valid" : status;
            UniValue row(UniValue::VOBJ);
            row.pushKV("tip", Label(h));
            row.pushKV("height", t["height"].getInt<int64_t>());
            row.pushKV("branchlen", t["branchlen"].getInt<int64_t>());
            row.pushKV("status", status);
            row.pushKV("projected", projected);
            rows.emplace_back(-t["height"].getInt<int64_t>(), h.ToString(), row);
        }
        std::sort(rows.begin(), rows.end(), [](const auto& a, const auto& b) {
            return std::tie(std::get<0>(a), std::get<1>(a)) < std::tie(std::get<0>(b), std::get<1>(b));
        });
        UniValue ct(UniValue::VARR);
        for (auto& r : rows) ct.push_back(std::get<2>(r));
        v.pushKV("chaintips", ct);
        return v;
    }

    UniValue Step(const UniValue& o)
    {
        const std::string op = o["op"].get_str();
        ++m_step;
        NoteFailedBlocks();
        // Every step carries its complete view.
        if (o.exists("brief")) Fail(strprintf("step %d: `brief` is not supported", m_step));
        m_passes.clear();
        m_step_passes.clear();
        m_last_pass_changed = false;
        m_submitted = UniValue(UniValue::VOBJ);
        m_proposed = UniValue(UniValue::VOBJ);
        UniValue result;
        if (op == "build") result = OpBuild(o);
        else if (op == "pnbh") result = Pnbh(Expand(o["headers"]));
        else if (op == "connect") result = Connect(Expand(o["blocks"]));
        else if (op == "cl") result = OpCl(o);
        else if (op == "seed-cl") result = OpSeedCl(o);
        else if (op == "spork19") result = OpSpork19(o);
        else if (op == "notify") result = OpNotify(o);
        else if (op == "timer") result = OpTimer();
        else if (op == "check-active-state") {
            Node().clhandler->CheckActiveState();
            Drain();
        } else if (op == "enforce") {
            Node().clhandler->EnforceBestChainLock();
            Drain();
        } else if (op == "invalidate") result = OpRpcBlock("invalidateblock", o["block"].get_str());
        else if (op == "reconsider") result = OpRpcBlock("reconsiderblock", o["block"].get_str(), o["ignore_chainlocks"]);
        else if (op == "precious") result = OpRpcBlock("preciousblock", o["block"].get_str());
        else if (op == "pnb") result = OpPnb(o);
        else if (op == "mark-conflicting") result = OpMarkConflicting(o["block"].get_str());
        else if (op == "propose") result = OpPropose(o["block"].get_str());
        else if (op == "checkpoint") result = OpCheckpoint(o);
        else if (op == "mocktime") {
            SetMockTime(GetTime() + o["advance"].getInt<int64_t>());
        } else if (op == "locator") result = OpLocator(o["block"].get_str());
        else if (op == "diskindex") result = OpDiskIndex(o["blocks"]);
        else if (op == "reload") result = OpReload(m_check_block_index, !o.exists("checkpoints") || o["checkpoints"].get_bool());
        else Fail("unknown op " + op);
        Settle();
        if (m_hook_error) std::rethrow_exception(std::exchange(m_hook_error, nullptr));
        NoteFailedBlocks();
        UniValue step(UniValue::VOBJ);
        step.pushKV("op", o);
        if (!result.isNull()) step.pushKV("result", result);
        if (!m_submitted.empty()) step.pushKV("blocks_submitted", m_submitted);
        if (!m_proposed.empty()) step.pushKV("blocks_proposed", m_proposed);
        step.pushKV("view", View());
        return step;
    }

private:
    std::vector<std::string> m_args_store;
    std::unique_ptr<ScenarioSetup> m_setup;
    bool m_check_block_index; // the ChainstateManager option, kept across reloads
    TraceListener m_trace;
    std::map<std::string, Built> m_built;
    mutable Mutex m_labels_mutex;
    std::map<uint256, std::string> m_labels GUARDED_BY(m_labels_mutex);

    void SetLabel(const uint256& h, const std::string& l)
    {
        LOCK(m_labels_mutex);
        m_labels[h] = l;
    }
    std::map<uint256, uint32_t> m_prev_status;
    std::map<uint256, std::pair<int32_t, uint64_t>> m_prev_entries;
    std::promise<void> m_release;
    bool m_parked{false};
    std::vector<std::string> m_sensitive; // the diagnostic label's reasons (PossiblySensitive)
    const CBlockIndex* m_best_tie_pick{nullptr}; // the last reload's m_best_header, and
    std::set<std::string> m_best_tie;           // the entries it tied (BestHeader)
    std::optional<std::mt19937_64> m_rng; // random:<seed> delivery
    int m_drain_passes{0};                // enforcement passes in the current drain
    std::map<std::string, int> m_passes;  // per locked block, since the last drain ended
    std::map<std::string, int> m_step_passes; // per locked block, in this op (Settle numbers by it)
    bool m_settling{false};
    std::optional<std::string> m_open_pass; // the trace marker of the pass running now
    int m_pass_no{0};                       // its number
    std::string m_pass_from;                // StateSnapshot() where it began
    bool m_last_pass_changed{false};        // the last closed pass moved the state
    int m_passes_run{0};                    // every pass so far (Settle's progress and cap)
    std::exception_ptr m_hook_error;          // from a seam hook, for Drain or Step to rethrow
    int m_step{0};                            // the op running now (0: setup)
    UniValue m_submitted{UniValue::VOBJ};     // label -> block hex, this op's block submissions
    UniValue m_proposed{UniValue::VOBJ};      // label -> block hex, this op's getblocktemplate proposals
    // X1's transcript (RecordCall), acceptance-time verdicts (Checked) and counters.
    Mutex m_calls_mutex;
    std::vector<UniValue> m_transcript GUARDED_BY(m_calls_mutex);
    std::map<std::pair<std::string, uint256>, int> m_occurrence GUARDED_BY(m_calls_mutex);
    std::vector<UniValue> m_accept_checks GUARDED_BY(m_calls_mutex);
    int m_tip_checked GUARDED_BY(m_calls_mutex){0};
    int m_tbv_calls{0}; // proposals that reached TestBlockValidity
    std::atomic<int> m_log_connect_tip{0}, m_log_disconnect_tip{0}, m_log_tbv_early{0};
    std::atomic<int> m_log_verify_level{0}, m_log_verify_connect{0}, m_log_verify_disconnect{0}, m_log_replay_disconnect{0};
    std::atomic<bool> m_log_verify_skipped{false};
    std::list<std::function<void(const std::string&)>>::iterator m_log_callback;
    Mutex m_boundary_mutex;
    std::vector<UniValue> m_boundaries GUARDED_BY(m_boundary_mutex);
};

} // namespace

namespace {

/** Instructions retired in user space by this thread (PF0's instructions:u). */
class InstructionCounter
{
public:
    InstructionCounter()
    {
        perf_event_attr attr{};
        attr.type = PERF_TYPE_HARDWARE;
        attr.size = sizeof(attr);
        attr.config = PERF_COUNT_HW_INSTRUCTIONS;
        attr.disabled = 1;
        attr.exclude_kernel = 1;
        attr.exclude_hv = 1;
        m_fd = static_cast<int>(syscall(SYS_perf_event_open, &attr, 0, -1, -1, 0));
        if (m_fd < 0) Fail("perf_event_open failed (perf_event_paranoid > 2?)");
    }
    ~InstructionCounter() { close(m_fd); }
    void Start()
    {
        ioctl(m_fd, PERF_EVENT_IOC_RESET, 0);
        ioctl(m_fd, PERF_EVENT_IOC_ENABLE, 0);
    }
    uint64_t Stop()
    {
        ioctl(m_fd, PERF_EVENT_IOC_DISABLE, 0);
        uint64_t n = 0;
        if (read(m_fd, &n, sizeof(n)) != sizeof(n)) Fail("perf read failed");
        return n;
    }

private:
    int m_fd;
};

std::vector<CBlockHeader> ReadHeaders(const std::string& path, size_t count)
{
    std::ifstream f(path, std::ios::binary);
    if (!f) Fail("cannot read " + path);
    std::vector<CBlockHeader> out;
    out.reserve(count);
    std::vector<unsigned char> buf(80);
    while (out.size() < count && f.read(reinterpret_cast<char*>(buf.data()), 80)) {
        CDataStream s(buf, SER_NETWORK, PROTOCOL_VERSION);
        CBlockHeader h;
        s >> h;
        out.push_back(h);
    }
    return out;
}

/** Feeds headers [1, end) through ProcessNewBlockHeaders in P2P-sized batches. */
void Preload(ChainstateManager& chainman, const std::vector<CBlockHeader>& headers, size_t end)
{
    for (size_t i = 1; i < end; i += 2000) {
        const std::vector<CBlockHeader> batch(headers.begin() + i, headers.begin() + std::min(end, i + 2000));
        BlockValidationState state;
        if (!chainman.ProcessNewBlockHeaders(batch, state)) Fail("preload rejected at " + std::to_string(i) + ": " + state.ToString());
    }
    SyncWithValidationInterfaceQueue();
}

int RunHeaders(int argc, char** argv)
{
    // headers <main|test> <file> <op> <args…>
    if (argc < 5) Fail("usage: headers <main|test> <file> <op> ...");
    const std::string which = argv[2];
    if (which != "main" && which != "test") Fail("network must be main or test, not " + which);
    const std::string net = which == "main" ? CBaseChainParams::MAIN : CBaseChainParams::TESTNET;
    const std::string file = argv[3], op = argv[4];
    auto arg = [&](int i) {
        if (5 + i >= argc) Fail(op + ": missing argument " + std::to_string(i + 1));
        return static_cast<size_t>(std::stoull(argv[5 + i]));
    };
    const size_t need = op == "locator" ? arg(0) : arg(0) + arg(1);
    if (need == 0 || (op != "locator" && arg(1) == 0) || (op == "locator" && arg(2) == 0)) Fail(op + ": empty request");
    const auto headers = ReadHeaders(file, need);
    if (headers.size() < need) Fail("header file too short");
    int64_t max_time = 0;
    for (const auto& h : headers) max_time = std::max<int64_t>(max_time, h.nTime);
    Scenario sc(net, {"-checkblockindex=0"});
    SetMockTime(max_time + 3600);
    sc.OpReload(/*check_block_index=*/false); // Core skips CheckBlockIndex off regtest too
    auto& chainman = sc.Chainman();
    if (headers[0].GetHash() != Params().GenesisBlock().GetHash()) Fail("file does not start at genesis");
    UniValue out(UniValue::VOBJ);
    out.pushKV("network", net);
    out.pushKV("mode", op);
    if (op == "bench-pnbh") {
        const size_t from = arg(0), count = arg(1);
        Preload(chainman, headers, from);
        const std::vector<CBlockHeader> batch(headers.begin() + from, headers.begin() + from + count);
        InstructionCounter ctr;
        BlockValidationState state;
        ctr.Start();
        const bool ok = chainman.ProcessNewBlockHeaders(batch, state);
        const uint64_t n = ctr.Stop();
        if (!ok) Fail("bench batch rejected: " + state.ToString());
        out.pushKV("from", from);
        out.pushKV("count", count);
        out.pushKV("instructions_u", n);
    } else if (op == "pnbh") {
        const size_t from = arg(0), count = arg(1);
        Preload(chainman, headers, from);
        const std::vector<CBlockHeader> batch(headers.begin() + from, headers.begin() + from + count);
        BlockValidationState state;
        const CBlockIndex* last = nullptr;
        const bool ok = chainman.ProcessNewBlockHeaders(batch, state, &last);
        out.pushKV("from", from);
        out.pushKV("count", count);
        out.pushKV("ok", ok);
        out.pushKV("state", StateJson(state));
        out.pushKV("last_accepted", last ? UniValue(last->GetBlockHash().ToString()) : UniValue());
        LOCK(cs_main);
        out.pushKV("best_header", chainman.m_best_header->GetBlockHash().ToString());
        out.pushKV("best_header_height", chainman.m_best_header->nHeight);
        out.pushKV("best_header_chainwork", chainman.m_best_header->nChainWork.GetHex());
    } else if (op == "locator") {
        const size_t n = arg(0);
        uint64_t seed = arg(1), samples = arg(2);
        Preload(chainman, headers, n);
        LOCK(cs_main);
        UniValue rows(UniValue::VARR);
        for (uint64_t i = 0; i < samples; ++i) {
            seed = seed * 6364136223846793005ULL + 1442695040888963407ULL; // Knuth MMIX LCG
            const int height = static_cast<int>((seed >> 33) % n);
            const CBlockIndex* p = chainman.m_best_header->GetAncestor(height);
            UniValue row(UniValue::VOBJ);
            row.pushKV("height", height);
            UniValue hs(UniValue::VARR);
            for (const auto& h : GetLocator(p).vHave) {
                const CBlockIndex* q = chainman.m_blockman.LookupBlockIndex(h);
                hs.push_back(q->nHeight);
            }
            row.pushKV("locator_heights", hs);
            rows.push_back(row);
        }
        out.pushKV("tip", static_cast<int64_t>(n - 1));
        out.pushKV("rows", rows);
    } else {
        Fail("unknown headers op " + op);
    }
    WITH_LOCK(cs_main, CheckRegion(chainman.BlockIndex()));
    std::cout << out.write(1) << "\n";
    return 0;
}

int RunScenario(const std::string& path, const std::string& delivery, const std::string& headers_dir)
{
    UniValue doc;
    if (!doc.read(ReadFile(path))) Fail("bad JSON in " + path);
    std::vector<std::string> args;
    if (doc.exists("args")) {
        for (const auto& a : doc["args"].getValues()) args.push_back(a.get_str());
    }
    const std::string net = doc["network"].get_str();
    if (net != CBaseChainParams::REGTEST && net != CBaseChainParams::MAIN && net != CBaseChainParams::TESTNET &&
        net != CBaseChainParams::DEVNET) {
        Fail("unknown network " + net);
    }
    UniValue steps(UniValue::VARR), calls, sensitive;
    {
        Scenario sc(net, args, delivery);
        // A preload of real headers (CS2's dumps): the clock follows them, as in
        // `headers` mode.
        std::vector<CBlockHeader> preload;
        if (doc.exists("preload")) {
            if (headers_dir.empty()) Fail("preload needs --headers <dir>");
            const size_t count = doc["preload"]["count"].getInt<size_t>();
            preload = ReadHeaders(headers_dir + "/" + doc["preload"]["file"].get_str(), count);
            if (preload.size() < count || count == 0) Fail("header file too short");
            if (preload[0].GetHash() != Params().GenesisBlock().GetHash()) Fail("header file does not start at genesis");
            int64_t max_time = 0;
            for (const auto& h : preload) max_time = std::max<int64_t>(max_time, h.nTime);
            SetMockTime(max_time + 3600);
        } else {
            SetMockTime(Params().GenesisBlock().nTime + 10'000'000);
        }
        // CheckBlockIndex walks the whole index after every header; scenarios
        // with thousands of headers turn it off (as Core does off regtest).
        if (doc.exists("check_block_index") && !doc["check_block_index"].get_bool()) sc.OpReload(false);
        UniValue setup(UniValue::VOBJ);
        setup.pushKV("op", "setup");
        if (!preload.empty()) {
            Preload(sc.Chainman(), preload, preload.size());
            sc.Drain();
            sc.AdoptPreloaded(preload);
            UniValue r(UniValue::VOBJ);
            r.pushKV("preloaded", strprintf("R1..R%d", preload.size() - 1));
            setup.pushKV("result", r);
        }
        setup.pushKV("view", sc.View());
        steps.push_back(setup);
        for (const auto& o : doc["ops"].getValues()) steps.push_back(sc.Step(o));
        calls = sc.Calls();
        sensitive = sc.PossiblySensitive();
    }
    UniValue out(UniValue::VOBJ);
    out.pushKV("name", doc["name"].get_str());
    out.pushKV("rows", doc["rows"]);
    out.pushKV("network", net);
    out.pushKV("delivery_policy", delivery);
    out.pushKV("steps", steps);
    for (const auto& k : {"transcript", "accept_checks", "core_counts"}) out.pushKV(k, calls[k]);
    out.pushKV("possibly_address_sensitive", sensitive);
    std::cout << out.write(1) << "\n";
    return 0;
}

} // namespace

int main(int argc, char** argv)
{
    try {
        if (argc >= 3 && argv[1] == std::string("--perturb-heap")) {
            // DEC-172: a perturbed heap for the whole run, setup included.
            PerturbInit(argv[2]);
            argc -= 2;
            argv += 2;
        }
        if (argc >= 2 && argv[1] == std::string("--audit-index")) {
            if (!g_perturb) Fail("--audit-index needs --perturb-heap");
            g_audit_index = true;
            --argc;
            ++argv;
        }
        if (argc >= 2 && argv[1] == std::string("--commit")) {
            std::cout << SB4_CORE_COMMIT << "\n";
            return 0;
        }
        if (argc >= 3 && argv[1] == std::string("run")) {
            std::string delivery = "boundary-flush", headers_dir;
            for (int i = 3; i < argc; i += 2) {
                if (i + 1 >= argc) Fail(std::string{argv[i]} + ": missing value");
                if (argv[i] == std::string("--delivery")) delivery = argv[i + 1];
                else if (argv[i] == std::string("--headers")) headers_dir = argv[i + 1];
                else Fail(std::string{"unknown option "} + argv[i]);
            }
            return PerturbedExit(RunScenario(argv[2], delivery, headers_dir));
        }
        if (argc >= 2 && argv[1] == std::string("headers")) return PerturbedExit(RunHeaders(argc, argv));
        std::cerr << "usage: sb4_scenarios [--perturb-heap up|down|<seed>] [--audit-index] run <scenario.json> [--delivery boundary-flush|random:<seed>] [--headers <dir>] | headers <main|test> <file> <op> ... | --commit\n";
        return 2;
    } catch (const std::exception& e) {
        std::cerr << "sb4_scenarios: " << e.what() << "\n";
        return 1;
    } catch (const UniValue& e) {
        std::cerr << "sb4_scenarios: RPC error " << e.write() << "\n";
        return 1;
    }
}
