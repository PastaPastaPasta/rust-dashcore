// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

// Records what Dash Core's script layer computes for the cases CS1's tests check (node program,
// CS1 design §7.2): EvalScript and VerifyScript with BaseSignatureChecker and with a deterministic
// mock checker, FindAndDelete, both GetSigOpCount overloads, GetScriptOp's cursor, CScriptNum and
// scriptnum_tests' CScriptNum10, CheckSignatureEncoding, CPubKey::Verify / CheckLowS /
// ecdsa_signature_parse_der_lax, ScriptErrorString, GetOpName, CScript's push serialization and
// CheckMinimalPush.
//
// The program generates its cases itself: named regressions from the CS1 design reviews and a
// seeded random corpus, with signatures made by the libsecp256k1 Core bundles. It prints one JSON
// object to stdout. record.py builds it against each pinned Core release, checks that both print
// the same bytes and writes dash-consensus/tests/data/script_recorded.json.
//
// Test tooling only: never part of a Core build.

#include <crypto/common.h>
#include <crypto/sha256.h>
#include <hash.h>
#include <pubkey.h>
#include <script/interpreter.h>
#include <script/script.h>
#include <script/script_error.h>
#include <test/scriptnum10.h>
#include <uint256.h>
#include <util/strencodings.h>

#include <secp256k1.h>

#include <cstdint>
#include <cstdio>
#include <algorithm>
#include <cstdlib>
#include <iterator>
#include <random>
#include <set>
#include <string>
#include <vector>

// pubkey.cpp; not declared in a header.
int ecdsa_signature_parse_der_lax(secp256k1_ecdsa_signature* sig, const unsigned char* input, size_t inputlen);

using valtype = std::vector<unsigned char>;
using Stack = std::vector<valtype>;

namespace {

// ---------------------------------------------------------------------------------------------
// Output: one JSON object, one record per line.
// ---------------------------------------------------------------------------------------------

std::string Hex(const unsigned char* p, size_t n)
{
    static const char* digits = "0123456789abcdef";
    std::string s;
    for (size_t i = 0; i < n; ++i) {
        s += digits[p[i] >> 4];
        s += digits[p[i] & 15];
    }
    return s;
}
std::string Hex(const valtype& v) { return Hex(v.data(), v.size()); }
std::string Hex(const CScript& s) { return Hex(s.data(), s.size()); }

std::string Str(const std::string& s)
{
    std::string out = "\"";
    for (char c : s) {
        if (c == '"' || c == '\\') out += '\\';
        out += c;
    }
    return out + "\"";
}

std::string StackJson(const Stack& stack)
{
    std::string s = "[";
    for (size_t i = 0; i < stack.size(); ++i) s += (i ? "," : "") + Str(Hex(stack[i]));
    return s + "]";
}

struct Section {
    const char* name;
    std::vector<std::string> records;
};
std::vector<Section> g_sections;

void Begin(const char* name) { g_sections.push_back({name, {}}); }
void Emit(const std::string& record) { g_sections.back().records.push_back(record); }

void Print()
{
    std::printf("{\n");
    for (size_t i = 0; i < g_sections.size(); ++i) {
        const Section& s = g_sections[i];
        std::printf("\"%s\": [\n", s.name);
        for (size_t j = 0; j < s.records.size(); ++j) {
            std::printf("%s%s\n", s.records[j].c_str(), j + 1 < s.records.size() ? "," : "");
        }
        std::printf("]%s\n", i + 1 < g_sections.size() ? "," : "");
    }
    std::printf("}\n");
}

// The recorder's own preconditions; unlike assert, kept in NDEBUG builds.
void Require(bool ok, const char* what)
{
    if (!ok) {
        std::fprintf(stderr, "record: %s\n", what);
        std::abort();
    }
}

// ---------------------------------------------------------------------------------------------
// Deterministic randomness: mt19937_64 is fully specified by the standard; no distributions.
// ---------------------------------------------------------------------------------------------

std::mt19937_64 g_rng(0xC51D5C0DEULL);
uint64_t Rand(uint64_t n) { return n ? g_rng() % n : 0; }
bool Chance(unsigned percent) { return Rand(100) < percent; }
valtype RandBytes(size_t n)
{
    valtype v(n);
    for (auto& b : v) b = static_cast<unsigned char>(g_rng());
    return v;
}

// ---------------------------------------------------------------------------------------------
// A deterministic stand-in for TransactionSignatureChecker, so that recorded cases reach the
// success side of CHECKSIG, CHECKMULTISIG, CLTV and CSV. A signature check passes depending on
// SHA-256 of the length-prefixed (signature, public key, scriptCode), so the verdicts also pin the
// scriptCode the interpreter hands the checker (after OP_CODESEPARATOR and FindAndDelete).
// dash-consensus/tests/support/script.rs has the same checker.
// ---------------------------------------------------------------------------------------------

class MockChecker : public BaseSignatureChecker
{
public:
    bool CheckSig(const valtype& sig, const valtype& pubkey, const CScript& code, SigVersion) const override
    {
        CSHA256 hasher;
        auto part = [&](const unsigned char* p, size_t n) {
            unsigned char len[8];
            WriteLE64(len, n);
            hasher.Write(len, 8).Write(p, n);
        };
        part(sig.data(), sig.size());
        part(pubkey.data(), pubkey.size());
        part(code.data(), code.size());
        unsigned char out[CSHA256::OUTPUT_SIZE];
        hasher.Finalize(out);
        return out[0] < 0xc0;
    }
    bool CheckLockTime(const CScriptNum& n) const override { return n.GetInt64() % 3 != 0; }
    bool CheckSequence(const CScriptNum& n) const override { return n.GetInt64() % 3 != 1; }
};

// ---------------------------------------------------------------------------------------------
// The recorders.
// ---------------------------------------------------------------------------------------------

void RecEval(const std::string& name, const CScript& script, Stack stack, unsigned int flags,
             const BaseSignatureChecker& checker = BaseSignatureChecker())
{
    std::string in = StackJson(stack);
    ScriptError err;
    bool ok = EvalScript(stack, script, flags, checker, SigVersion::BASE, &err);
    Emit("{\"name\":" + Str(name) + ",\"script\":" + Str(Hex(script)) + ",\"stack\":" + in +
         ",\"flags\":" + std::to_string(flags) + ",\"ok\":" + (ok ? "true" : "false") +
         ",\"error\":" + std::to_string(int(err)) + ",\"result_stack\":" + StackJson(stack) + "}");
}

void RecVerify(const std::string& name, const CScript& sig, const CScript& spk, unsigned int flags,
               const BaseSignatureChecker& checker = BaseSignatureChecker())
{
    // VerifyScript asserts CLEANSTACK => P2SH; the generator never produces that combination.
    Require(!(flags & SCRIPT_VERIFY_CLEANSTACK) || (flags & SCRIPT_VERIFY_P2SH), "CLEANSTACK without P2SH");
    ScriptError err;
    bool ok = VerifyScript(sig, spk, flags, checker, &err);
    Emit("{\"name\":" + Str(name) + ",\"script_sig\":" + Str(Hex(sig)) + ",\"script_pubkey\":" +
         Str(Hex(spk)) + ",\"flags\":" + std::to_string(flags) + ",\"ok\":" + (ok ? "true" : "false") +
         ",\"error\":" + std::to_string(int(err)) + "}");
}

void RecFindAndDelete(const std::string& name, const CScript& script, const CScript& pattern)
{
    CScript out = script;
    int n = FindAndDelete(out, pattern);
    Emit("{\"name\":" + Str(name) + ",\"script\":" + Str(Hex(script)) + ",\"pattern\":" + Str(Hex(pattern)) +
         ",\"count\":" + std::to_string(n) + ",\"result\":" + Str(Hex(out)) + "}");
}

void RecSigops(const std::string& name, const CScript& script)
{
    Emit("{\"name\":" + Str(name) + ",\"script\":" + Str(Hex(script)) + ",\"accurate\":" +
         std::to_string(script.GetSigOpCount(true)) + ",\"inaccurate\":" + std::to_string(script.GetSigOpCount(false)) + "}");
}

void RecSigopsWithSig(const std::string& name, const CScript& spk, const CScript& sig)
{
    Emit("{\"name\":" + Str(name) + ",\"script_pubkey\":" + Str(Hex(spk)) + ",\"script_sig\":" + Str(Hex(sig)) +
         ",\"count\":" + std::to_string(spk.GetSigOpCount(sig)) + "}");
}

// Every GetScriptOp call from the start until it fails: the opcode and the cursor after each.
void RecGetOp(const std::string& name, const CScript& script)
{
    std::string steps = "[";
    CScriptBase::const_iterator pc = script.begin();
    bool first = true;
    while (true) {
        opcodetype op;
        valtype data;
        bool ok = GetScriptOp(pc, script.end(), op, &data);
        steps += std::string(first ? "" : ",") + "{\"ok\":" + (ok ? "true" : "false") + ",\"opcode\":" +
                 std::to_string(int(op)) + ",\"data\":" + Str(Hex(data)) + ",\"cursor\":" +
                 std::to_string(pc - script.begin()) + "}";
        first = false;
        if (!ok) break;
    }
    Emit("{\"name\":" + Str(name) + ",\"script\":" + Str(Hex(script)) + ",\"steps\":" + steps + "]}");
}

void RecNumDecode(const valtype& vch, bool minimal, size_t max_size)
{
    std::string result;
    try {
        CScriptNum n(vch, minimal, max_size);
        result = "\"ok\":true,\"value\":" + std::to_string(n.GetInt64()) + ",\"getint\":" + std::to_string(n.getint());
    } catch (const scriptnum_error&) {
        result = "\"ok\":false";
    }
    Emit("{\"vch\":" + Str(Hex(vch)) + ",\"minimal\":" + (minimal ? "true" : "false") + ",\"max_size\":" +
         std::to_string(max_size) + "," + result + ",\"is_minimally_encoded\":" +
         (CScriptNum::IsMinimallyEncoded(vch, max_size) ? "true" : "false") + "}");
}

void RecNumEncode(int64_t value)
{
    Emit("{\"value\":" + std::to_string(value) + ",\"bytes\":" + Str(Hex(CScriptNum::serialize(value))) + "}");
}

void RecMinimallyEncode(const valtype& vch)
{
    valtype out = vch;
    bool changed = CScriptNum::MinimallyEncode(out);
    Emit("{\"vch\":" + Str(Hex(vch)) + ",\"changed\":" + (changed ? "true" : "false") + ",\"result\":" + Str(Hex(out)) + "}");
}

void RecSigEncoding(const std::string& name, const valtype& sig, unsigned int flags)
{
    ScriptError err = SCRIPT_ERR_OK;
    bool ok = CheckSignatureEncoding(sig, flags, &err);
    Emit("{\"name\":" + Str(name) + ",\"sig\":" + Str(Hex(sig)) + ",\"flags\":" + std::to_string(flags) +
         ",\"ok\":" + (ok ? "true" : "false") + ",\"error\":" + std::to_string(ok ? 0 : int(err)) + "}");
}

void RecEcdsa(const std::string& name, const valtype& pubkey, const uint256& msg, const valtype& sig)
{
    secp256k1_ecdsa_signature parsed;
    bool lax = ecdsa_signature_parse_der_lax(&parsed, sig.data(), sig.size());
    unsigned char compact[64] = {0};
    if (lax) secp256k1_ecdsa_signature_serialize_compact(secp256k1_context_static, compact, &parsed);
    bool verify = CPubKey(pubkey).Verify(msg, sig);
    Emit("{\"name\":" + Str(name) + ",\"pubkey\":" + Str(Hex(pubkey)) + ",\"msg\":" + Str(Hex(msg.begin(), 32)) +
         ",\"sig\":" + Str(Hex(sig)) + ",\"verify\":" + (verify ? "true" : "false") + ",\"low_s\":" +
         (CPubKey::CheckLowS(sig) ? "true" : "false") + ",\"lax_ok\":" + (lax ? "true" : "false") +
         ",\"lax_compact\":" + Str(lax ? Hex(compact, 64) : "") + "}");
}

// ---------------------------------------------------------------------------------------------
// Builders.
// ---------------------------------------------------------------------------------------------

CScript Raw(const valtype& bytes) { return CScript(bytes.begin(), bytes.end()); }
CScript Repeat(opcodetype op, int n)
{
    CScript s;
    for (int i = 0; i < n; ++i) s << op;
    return s;
}
CScript Cat(CScript a, const CScript& b)
{
    a.insert(a.end(), b.begin(), b.end());
    return a;
}
CScript Push(const valtype& v)
{
    CScript s;
    s << v;
    return s;
}

// A DER signature from big-endian integer bodies, as given (no normalization).
valtype Der(const valtype& r, const valtype& s)
{
    valtype v{0x30, static_cast<unsigned char>(4 + r.size() + s.size()), 0x02, static_cast<unsigned char>(r.size())};
    v.insert(v.end(), r.begin(), r.end());
    v.push_back(0x02);
    v.push_back(static_cast<unsigned char>(s.size()));
    v.insert(v.end(), s.begin(), s.end());
    return v;
}

// A 32-byte scalar as a minimal positive DER integer body.
valtype DerInt(const unsigned char* p32)
{
    valtype v(p32, p32 + 32);
    while (v.size() > 1 && v[0] == 0 && !(v[1] & 0x80)) v.erase(v.begin());
    if (v[0] & 0x80) v.insert(v.begin(), 0);
    return v;
}

const valtype ORDER{0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
                    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41};

// Big-endian 32-byte arithmetic for the scalar edge values.
valtype AddSmall(valtype v, int delta)
{
    int carry = delta;
    for (int i = 31; i >= 0 && carry; --i) {
        int x = v[i] + carry;
        carry = x < 0 ? -1 : (x > 255 ? 1 : 0);
        v[i] = static_cast<unsigned char>(x & 0xff);
    }
    return v;
}
valtype Half(const valtype& v)
{
    valtype out(32);
    int rem = 0;
    for (int i = 0; i < 32; ++i) {
        int x = rem * 256 + v[i];
        out[i] = static_cast<unsigned char>(x / 2);
        rem = x % 2;
    }
    return out;
}

struct Key {
    valtype compressed, uncompressed;
};

secp256k1_context* g_ctx;

Key MakeKey(unsigned char seed)
{
    unsigned char secret[32] = {0};
    secret[31] = seed;
    secp256k1_pubkey pk;
    Require(secp256k1_ec_pubkey_create(g_ctx, &pk, secret), "pubkey_create");
    Key k{valtype(33), valtype(65)};
    size_t len = 33;
    Require(secp256k1_ec_pubkey_serialize(g_ctx, k.compressed.data(), &len, &pk, SECP256K1_EC_COMPRESSED), "serialize");
    len = 65;
    Require(secp256k1_ec_pubkey_serialize(g_ctx, k.uncompressed.data(), &len, &pk, SECP256K1_EC_UNCOMPRESSED), "serialize");
    return k;
}

// A low-S signature by secret key `seed` over `msg`, as (r, s) 32-byte big-endian values.
std::pair<valtype, valtype> SignRS(unsigned char seed, const uint256& msg)
{
    unsigned char secret[32] = {0};
    secret[31] = seed;
    secp256k1_ecdsa_signature sig;
    Require(secp256k1_ecdsa_sign(g_ctx, &sig, msg.begin(), secret, nullptr, nullptr), "sign");
    unsigned char compact[64];
    secp256k1_ecdsa_signature_serialize_compact(g_ctx, compact, &sig);
    return {valtype(compact, compact + 32), valtype(compact + 32, compact + 64)};
}

valtype NegateScalar(valtype s)
{
    Require(secp256k1_ec_seckey_negate(g_ctx, s.data()), "negate");
    return s;
}

uint256 Msg(unsigned char b)
{
    uint256 m;
    for (int i = 0; i < 32; ++i) m.begin()[i] = static_cast<unsigned char>(b + i);
    return m;
}

// ---------------------------------------------------------------------------------------------
// Random scripts: pieces biased to what the interpreter and parser treat specially.
// ---------------------------------------------------------------------------------------------

const std::vector<opcodetype> ALPHABET = {
    OP_0, OP_1NEGATE, OP_RESERVED, OP_1, OP_2, OP_3, OP_16, OP_NOP, OP_VER, OP_IF, OP_NOTIF, OP_VERIF, OP_ELSE, OP_ENDIF,
    OP_VERIFY, OP_RETURN, OP_TOALTSTACK, OP_FROMALTSTACK, OP_2DROP, OP_2DUP, OP_3DUP, OP_2OVER, OP_2ROT, OP_2SWAP, OP_IFDUP,
    OP_DEPTH, OP_DROP, OP_DUP, OP_NIP, OP_OVER, OP_PICK, OP_ROLL, OP_ROT, OP_SWAP, OP_TUCK, OP_CAT, OP_SPLIT, OP_NUM2BIN,
    OP_BIN2NUM, OP_SIZE, OP_AND, OP_OR, OP_XOR, OP_EQUAL, OP_EQUALVERIFY, OP_1ADD, OP_1SUB, OP_NEGATE, OP_ABS, OP_NOT,
    OP_0NOTEQUAL, OP_ADD, OP_SUB, OP_DIV, OP_MOD, OP_BOOLAND, OP_BOOLOR, OP_NUMEQUAL, OP_NUMEQUALVERIFY, OP_NUMNOTEQUAL,
    OP_LESSTHAN, OP_GREATERTHAN, OP_LESSTHANOREQUAL, OP_GREATERTHANOREQUAL, OP_MIN, OP_MAX, OP_WITHIN, OP_RIPEMD160,
    OP_SHA1, OP_SHA256, OP_HASH160, OP_HASH256, OP_CODESEPARATOR, OP_CHECKSIG, OP_CHECKSIGVERIFY, OP_CHECKMULTISIG,
    OP_CHECKMULTISIGVERIFY, OP_NOP1, OP_CHECKLOCKTIMEVERIFY, OP_CHECKSEQUENCEVERIFY, OP_NOP4, OP_NOP10, OP_CHECKDATASIG,
    OP_CHECKDATASIGVERIFY, OP_INVERT, OP_MUL, static_cast<opcodetype>(0xbc), OP_INVALIDOPCODE};

valtype RandItem()
{
    switch (Rand(6)) {
    case 0: return {};
    case 1: {
        uint64_t small = Rand(4);
        return valtype{static_cast<unsigned char>(small ? Rand(17) : g_rng())};
    }
    case 2: return CScriptNum::serialize(static_cast<int64_t>(Rand(1 << 20)) - (1 << 19));
    case 3: return valtype{0x00, 0x80};
    default: return RandBytes(Rand(9));
    }
}

CScript RandScript(size_t pieces, bool allow_truncated)
{
    CScript s;
    for (size_t i = 0; i < pieces; ++i) {
        switch (Rand(12)) {
        case 0: case 1: case 2: case 3: case 4: s << ALPHABET[Rand(ALPHABET.size())]; break;
        case 5: case 6: case 7: s << RandItem(); break;
        case 8: s << CScriptNum(static_cast<int64_t>(Rand(40)) - 8); break;
        case 9: { // a non-minimal push
            valtype d = RandItem();
            s.push_back(OP_PUSHDATA1);
            s.push_back(static_cast<unsigned char>(d.size()));
            s.insert(s.end(), d.begin(), d.end());
            break;
        }
        case 10: s.push_back(static_cast<unsigned char>(g_rng())); break;
        default: {
            valtype item = RandItem();
            opcodetype op = ALPHABET[Rand(ALPHABET.size())];
            s << OP_IF << item << OP_ELSE << op << OP_ENDIF;
            break;
        }
        }
    }
    if (allow_truncated && Chance(10)) {
        static const valtype tails[] = {{0x05, 0xaa}, {0x4c}, {0x4c, 0x02, 0xaa}, {0x4d, 0x01}, {0x4d, 0x02, 0x00, 0xaa},
                                       {0x4e, 0x01, 0x02}, {0x4e, 0x01, 0x00, 0x00, 0x00}};
        const valtype& t = tails[Rand(7)];
        s.insert(s.end(), t.begin(), t.end());
    }
    return s;
}

unsigned int RandFlags()
{
    static const unsigned int defined[] = {SCRIPT_VERIFY_P2SH, SCRIPT_VERIFY_STRICTENC, SCRIPT_VERIFY_DERSIG,
                                           SCRIPT_VERIFY_LOW_S, SCRIPT_VERIFY_NULLDUMMY, SCRIPT_VERIFY_SIGPUSHONLY,
                                           SCRIPT_VERIFY_MINIMALDATA, SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS,
                                           SCRIPT_VERIFY_CLEANSTACK, SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY,
                                           SCRIPT_VERIFY_CHECKSEQUENCEVERIFY, SCRIPT_VERIFY_NULLFAIL,
                                           SCRIPT_VERIFY_CONST_SCRIPTCODE};
    unsigned int f = 0;
    for (unsigned int bit : defined) if (Chance(40)) f |= bit;
    if (Chance(10)) f |= 1U << Rand(32); // undefined bits are ignored
    return f;
}

// ---------------------------------------------------------------------------------------------
// Case lists.
// ---------------------------------------------------------------------------------------------

void ErrorStrings()
{
    Begin("error_strings");
    for (int code = 0; code <= SCRIPT_ERR_ERROR_COUNT + 1; ++code) {
        Emit("{\"code\":" + std::to_string(code) + ",\"text\":" + Str(ScriptErrorString(static_cast<ScriptError>(code))) + "}");
    }
}

void EvalCases(const Key& key)
{
    Begin("eval");
    const unsigned int NONE = SCRIPT_VERIFY_NONE;
    auto multisig0of20 = [] {
        CScript s;
        s << OP_0 << OP_0;
        for (int i = 0; i < 20; ++i) s << OP_0;
        return s << 20 << OP_CHECKMULTISIG;
    };
    // CHECKMULTISIG's key-count charge (GPT M3, Opus m-5).
    RecEval("multisig 0-of-20 after 180 NOPs: 201 ops", Cat(Repeat(OP_NOP, 180), multisig0of20()), {}, NONE);
    RecEval("multisig 0-of-20 after 181 NOPs: 202 ops", Cat(Repeat(OP_NOP, 181), multisig0of20()), {}, NONE);
    RecEval("multisig key count charged before the key-stack check", Cat(Repeat(OP_NOP, 181), CScript() << OP_0 << OP_0 << 20 << OP_CHECKMULTISIG), {}, NONE);
    RecEval("skipped multisig charges only its opcode", Cat(Repeat(OP_NOP, 197), CScript() << OP_0 << OP_IF << 20 << OP_CHECKMULTISIG << OP_ENDIF), {}, NONE);
    RecEval("executed multisig in a branch charges its keys", Cat(Cat(Cat(Repeat(OP_NOP, 197), CScript() << OP_1 << OP_IF), multisig0of20()), CScript() << OP_ENDIF), {}, NONE);
    RecEval("multisig NULLFAIL before NULLDUMMY with a partial cleanup",
            CScript() << OP_1 << valtype{0x7f} << OP_0 << OP_2 << OP_0 << OP_0 << OP_2 << OP_CHECKMULTISIG, {},
            SCRIPT_VERIFY_NULLFAIL | SCRIPT_VERIFY_NULLDUMMY);
    RecEval("multisig NULLDUMMY", CScript() << OP_1 << OP_0 << OP_0 << OP_CHECKMULTISIG, {}, SCRIPT_VERIFY_NULLDUMMY);
    RecEval("multisig without the dummy", CScript() << OP_0 << OP_0 << OP_CHECKMULTISIG, {}, NONE);
    RecEval("multisig key count 21", CScript() << 21 << OP_CHECKMULTISIG, {}, NONE);
    RecEval("checkmultisigverify fails", CScript() << OP_0 << OP_0 << OP_1 << key.compressed << OP_1 << OP_CHECKMULTISIGVERIFY, {}, NONE);
    RecEval("checkmultisigverify 0-of-0 succeeds", CScript() << OP_0 << OP_0 << OP_0 << OP_CHECKMULTISIGVERIFY, {}, NONE);
    RecEval("multisig key count -1", CScript() << OP_1NEGATE << OP_CHECKMULTISIG, {}, NONE);
    RecEval("multisig signature count above key count", CScript() << OP_0 << OP_2 << OP_0 << OP_CHECKMULTISIG, {}, NONE);
    RecEval("multisig key count not minimal under MINIMALDATA", CScript() << OP_0 << OP_0 << valtype{0x00} << OP_CHECKMULTISIG, {}, SCRIPT_VERIFY_MINIMALDATA);
    RecEval("multisig empty stack", CScript() << OP_CHECKMULTISIG, {}, NONE);
    // Encodings reached and unreached in CHECKMULTISIG (GPT V4): with BaseSignatureChecker every
    // check fails, so 2-of-2 stops after the first pair.
    {
        auto [r, s] = SignRS(1, Msg(1));
        valtype good = Der(DerInt(r.data()), DerInt(s.data()));
        good.push_back(SIGHASH_ALL);
        valtype bad{0x31, 0x01};
        RecEval("2-of-2: the unreached pair's bad encoding is never checked",
                CScript() << OP_0 << bad << good << OP_2 << key.compressed << key.compressed << OP_2 << OP_CHECKMULTISIG, {},
                SCRIPT_VERIFY_DERSIG);
        RecEval("2-of-2: the reached pair's bad encoding fails",
                CScript() << OP_0 << good << bad << OP_2 << key.compressed << key.compressed << OP_2 << OP_CHECKMULTISIG, {},
                SCRIPT_VERIFY_DERSIG);
        RecEval("2-of-2: NULLFAIL counts unused non-empty signatures",
                CScript() << OP_0 << good << OP_0 << OP_2 << key.compressed << key.compressed << OP_2 << OP_CHECKMULTISIG, {},
                SCRIPT_VERIFY_NULLFAIL);
        RecEval("multisig FindAndDelete hit under CONST_SCRIPTCODE beats SIG_DER",
                CScript() << OP_0 << bad << OP_1 << key.compressed << OP_1 << OP_CHECKMULTISIG << bad, {},
                SCRIPT_VERIFY_CONST_SCRIPTCODE | SCRIPT_VERIFY_DERSIG);
        // CHECKSIG encoding precedence (GPT V1): DER -> LOW_S -> hash type -> pubkey -> NULLFAIL.
        valtype high = Der(DerInt(r.data()), DerInt(NegateScalar(s).data()));
        high.push_back(SIGHASH_ALL);
        valtype badtype = good;
        badtype.back() = 0x04;
        valtype hybrid = key.uncompressed;
        hybrid[0] = 0x06 | (hybrid[64] & 1);
        valtype n_body{0x00};
        n_body.insert(n_body.end(), ORDER.begin(), ORDER.end());
        valtype s_is_n = Der(valtype{0x01}, n_body);
        s_is_n.push_back(SIGHASH_ALL);
        const unsigned int ALLENC = SCRIPT_VERIFY_DERSIG | SCRIPT_VERIFY_LOW_S | SCRIPT_VERIFY_STRICTENC;
        RecEval("checksig: non-DER is SIG_DER first", CScript() << bad << hybrid << OP_CHECKSIG, {}, ALLENC);
        RecEval("checksig: high S", CScript() << high << key.compressed << OP_CHECKSIG, {}, ALLENC);
        RecEval("checksig: high S without LOW_S", CScript() << high << key.compressed << OP_CHECKSIG, {}, SCRIPT_VERIFY_DERSIG);
        RecEval("checksig: undefined hash type", CScript() << badtype << key.compressed << OP_CHECKSIG, {}, ALLENC);
        RecEval("checksig: undefined hash type without STRICTENC", CScript() << badtype << key.compressed << OP_CHECKSIG, {}, SCRIPT_VERIFY_DERSIG | SCRIPT_VERIFY_LOW_S);
        RecEval("checksig: hybrid key under STRICTENC", CScript() << good << hybrid << OP_CHECKSIG, {}, ALLENC);
        RecEval("checksig: empty signature, bad key under STRICTENC", CScript() << OP_0 << valtype(34, 0x02) << OP_CHECKSIG, {}, SCRIPT_VERIFY_STRICTENC);
        RecEval("checksig: empty signature, bad key without STRICTENC", CScript() << OP_0 << valtype(34, 0x02) << OP_CHECKSIG, {}, NONE);
        RecEval("checksig: failed non-empty signature under NULLFAIL", CScript() << good << key.compressed << OP_CHECKSIG, {}, SCRIPT_VERIFY_NULLFAIL);
        RecEval("checksig: S = n passes LOW_S, then NULLFAIL", CScript() << s_is_n << key.compressed << OP_CHECKSIG, {}, ALLENC | SCRIPT_VERIFY_NULLFAIL);
        RecEval("checksigverify leaves false on failure", CScript() << OP_0 << key.compressed << OP_CHECKSIGVERIFY, {}, NONE);
        RecEval("checksig FindAndDelete hit under CONST_SCRIPTCODE beats SIG_DER",
                CScript() << bad << key.compressed << OP_CHECKSIG << bad, {}, SCRIPT_VERIFY_CONST_SCRIPTCODE | SCRIPT_VERIFY_DERSIG);
        RecEval("checksig empty signature deletes every OP_0 under CONST_SCRIPTCODE",
                CScript() << OP_0 << key.compressed << OP_CHECKSIG << OP_0 << OP_DROP, {}, SCRIPT_VERIFY_CONST_SCRIPTCODE);
    }
    // OP_CHECKDATASIG (quirk 4): the single SHA-256 of the message, the whole signature verified
    // laxly, DER plus exactly one trailing byte under DERSIG, no checker.
    {
        const valtype msg{'d', 'a', 's', 'h'};
        uint256 digest;
        CSHA256().Write(msg.data(), msg.size()).Finalize(digest.begin());
        auto [r, s] = SignRS(1, digest);
        const valtype der = Der(DerInt(r.data()), DerInt(s.data()));
        valtype high = Der(DerInt(r.data()), DerInt(NegateScalar(s).data()));
        high.push_back(0x01);
        valtype hybrid = key.uncompressed;
        hybrid[0] = 0x06 | (hybrid[64] & 1);
        const std::vector<std::pair<std::string, valtype>> variants = {
            {"no trailing byte", der},
            {"trailing 01", [&] { valtype v = der; v.push_back(0x01); return v; }()},
            {"trailing ff", [&] { valtype v = der; v.push_back(0xff); return v; }()},
            {"trailing 01 02", [&] { valtype v = der; v.push_back(0x01); v.push_back(0x02); return v; }()},
            {"high S + 01", high},
        };
        const unsigned int flagsets[] = {SCRIPT_VERIFY_NONE, SCRIPT_VERIFY_DERSIG, SCRIPT_VERIFY_STRICTENC,
                                         SCRIPT_VERIFY_DERSIG | SCRIPT_VERIFY_LOW_S, SCRIPT_VERIFY_NULLFAIL};
        for (const auto& [vname, sig] : variants) {
            for (unsigned int flags : flagsets) {
                RecEval("checkdatasig " + vname + ", flags " + std::to_string(flags),
                        CScript() << sig << msg << key.compressed << OP_CHECKDATASIG, {}, flags);
            }
        }
        valtype good = der;
        good.push_back(0x01);
        const valtype other{'d', 'a', 's', 'H'};
        RecEval("checkdatasig wrong message", CScript() << good << other << key.compressed << OP_CHECKDATASIG, {}, SCRIPT_VERIFY_NONE);
        RecEval("checkdatasig wrong message under NULLFAIL", CScript() << good << other << key.compressed << OP_CHECKDATASIG, {}, SCRIPT_VERIFY_NULLFAIL);
        RecEval("checkdatasigverify wrong message", CScript() << good << other << key.compressed << OP_CHECKDATASIGVERIFY, {}, SCRIPT_VERIFY_NONE);
        RecEval("checkdatasigverify valid", CScript() << good << msg << key.compressed << OP_CHECKDATASIGVERIFY, {}, SCRIPT_VERIFY_NONE);
        RecEval("checkdatasig hybrid key", CScript() << good << msg << hybrid << OP_CHECKDATASIG, {}, SCRIPT_VERIFY_DERSIG);
        RecEval("checkdatasig hybrid key under STRICTENC", CScript() << good << msg << hybrid << OP_CHECKDATASIG, {}, SCRIPT_VERIFY_STRICTENC);
        RecEval("checkdatasig empty signature under NULLFAIL", CScript() << OP_0 << msg << key.compressed << OP_CHECKDATASIG, {}, SCRIPT_VERIFY_NULLFAIL);
        RecEval("checkdatasig signed over SHA256d instead", [&] {
            auto [r2, s2] = SignRS(1, Hash(msg));
            valtype v = Der(DerInt(r2.data()), DerInt(s2.data()));
            v.push_back(0x01);
            return CScript() << v << msg << key.compressed << OP_CHECKDATASIG;
        }(), {}, SCRIPT_VERIFY_NONE);
    }
    // Numbers and failure stacks (GPT probes).
    RecEval("num2bin impossible encoding", CScript() << valtype{1, 0} << OP_0 << OP_NUM2BIN, {}, NONE);
    RecEval("num2bin negative size", CScript() << valtype{1} << OP_1NEGATE << OP_NUM2BIN, {}, NONE);
    RecEval("num2bin pads and moves the sign", CScript() << valtype{0x81} << OP_4 << OP_NUM2BIN, {}, NONE);
    RecEval("num2bin 521", CScript() << valtype{1} << CScriptNum(521) << OP_NUM2BIN, {}, NONE);
    RecEval("bin2num out of range", CScript() << valtype{1, 0, 0, 0, 1, 0} << OP_BIN2NUM, {}, NONE);
    RecEval("bin2num negative zero", CScript() << valtype{0, 0, 0x80} << OP_BIN2NUM, {}, NONE);
    RecEval("pick out of range pops the index", CScript() << OP_1 << OP_2 << OP_PICK, {}, NONE);
    RecEval("roll", CScript() << OP_1 << OP_2 << OP_3 << OP_2 << OP_ROLL, {}, NONE);
    RecEval("1ADD overflows to five bytes", CScript() << valtype{255, 255, 255, 127} << OP_1ADD, {}, NONE);
    RecEval("a five-byte result fails the next numeric op", CScript() << valtype{255, 255, 255, 127} << OP_1ADD << OP_1ADD, {}, NONE);
    RecEval("empty IF", CScript() << OP_IF, {}, NONE);
    RecEval("ELSE without IF", CScript() << OP_ELSE, {}, NONE);
    RecEval("unbalanced IF at the end", CScript() << OP_1 << OP_IF, {}, NONE);
    RecEval("repeated ELSE", CScript() << OP_1 << OP_IF << OP_2 << OP_ELSE << OP_3 << OP_ELSE << OP_4 << OP_ENDIF, {}, NONE);
    RecEval("div by zero", CScript() << OP_1 << OP_0 << OP_DIV, {}, NONE);
    RecEval("mod by zero", CScript() << OP_1 << OP_0 << OP_MOD, {}, NONE);
    RecEval("negative division truncates", CScript() << CScriptNum(-7) << OP_2 << OP_DIV << CScriptNum(-7) << OP_2 << OP_MOD, {}, NONE);
    RecEval("split at both ends", CScript() << valtype{1, 2, 3} << OP_0 << OP_SPLIT << valtype{4, 5} << OP_2 << OP_SPLIT, {}, NONE);
    RecEval("split out of range", CScript() << valtype{1, 2, 3} << OP_4 << OP_SPLIT, {}, NONE);
    RecEval("cat 521", CScript() << valtype(260, 1) << valtype(261, 2) << OP_CAT, {}, NONE);
    RecEval("and of different sizes", CScript() << valtype{1} << valtype{1, 2} << OP_AND, {}, NONE);
    RecEval("an empty script with 1,001 items", CScript(), Stack(1001), NONE);
    // CLTV and CSV with BaseSignatureChecker (every check fails).
    RecEval("CLTV off is NOP2, even with DISCOURAGE", CScript() << OP_1 << OP_CHECKLOCKTIMEVERIFY, {}, SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS);
    RecEval("CLTV negative", CScript() << OP_1NEGATE << OP_CHECKLOCKTIMEVERIFY, {}, SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY);
    RecEval("CLTV unsatisfied", CScript() << OP_1 << OP_CHECKLOCKTIMEVERIFY, {}, SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY);
    RecEval("CLTV five-byte operand", CScript() << valtype{0, 0, 0, 0, 1} << OP_CHECKLOCKTIMEVERIFY, {}, SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY);
    RecEval("CLTV six-byte operand", CScript() << valtype{0, 0, 0, 0, 0, 1} << OP_CHECKLOCKTIMEVERIFY, {}, SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY);
    RecEval("CLTV empty stack", CScript() << OP_CHECKLOCKTIMEVERIFY, {}, SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY);
    RecEval("CSV disable flag is a NOP", CScript() << valtype{0, 0, 0, 0x80, 0} << OP_CHECKSEQUENCEVERIFY, {}, SCRIPT_VERIFY_CHECKSEQUENCEVERIFY);
    RecEval("CSV bit 32 is not the disable flag", CScript() << valtype{0, 0, 0, 0, 1} << OP_CHECKSEQUENCEVERIFY, {}, SCRIPT_VERIFY_CHECKSEQUENCEVERIFY);
    RecEval("NOP1 discouraged", CScript() << OP_NOP1, {}, SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS);
    RecEval("NOP10 discouraged in a skipped branch", CScript() << OP_0 << OP_IF << OP_NOP10 << OP_ENDIF, {}, SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS);
    // Limits and skipped branches.
    RecEval("push 520", CScript() << valtype(520, 7), {}, NONE);
    RecEval("push 521", CScript() << valtype(521, 7), {}, NONE);
    RecEval("push 521 in a skipped branch", CScript() << OP_0 << OP_IF << valtype(521, 7) << OP_ENDIF, {}, NONE);
    {
        CScript big;
        for (int i = 0; i < 19; ++i) big << valtype(520, 1);
        CScript at = Cat(big, Repeat(OP_NOP, 63));
        Require(at.size() == 10000, "10,000-byte script");
        RecEval("script of 10,000 bytes", at, {}, NONE);
        RecEval("script of 10,001 bytes", Cat(at, Repeat(OP_NOP, 1)), {}, NONE);
    }
    RecEval("1,000 stack items", Repeat(OP_1, 1000), {}, NONE);
    RecEval("1,001 stack items", Repeat(OP_1, 1001), {}, NONE);
    RecEval("1,001 items across the altstack", Cat(Repeat(OP_1, 1000), CScript() << OP_TOALTSTACK << OP_1), {}, NONE);
    RecEval("201 ops", Repeat(OP_NOP, 201), {}, NONE);
    RecEval("202 ops", Repeat(OP_NOP, 202), {}, NONE);
    RecEval("OP_RESERVED does not count as an op", Cat(Repeat(OP_NOP, 201), CScript() << OP_0 << OP_IF << OP_RESERVED << OP_ENDIF), {}, NONE);
    RecEval("disabled opcode in a skipped branch", CScript() << OP_0 << OP_IF << OP_MUL << OP_ENDIF, {}, NONE);
    RecEval("VERIF in a skipped branch", CScript() << OP_0 << OP_IF << OP_VERIF << OP_ENDIF, {}, NONE);
    RecEval("VER in a skipped branch", CScript() << OP_0 << OP_IF << OP_VER << OP_ENDIF, {}, NONE);
    RecEval("RESERVED executed", CScript() << OP_RESERVED, {}, NONE);
    RecEval("unknown opcode 0xbc in a skipped branch", CScript() << OP_0 << OP_IF << static_cast<opcodetype>(0xbc) << OP_ENDIF, {}, NONE);
    RecEval("OP_CODESEPARATOR in a skipped branch under CONST_SCRIPTCODE", CScript() << OP_0 << OP_IF << OP_CODESEPARATOR << OP_ENDIF, {}, SCRIPT_VERIFY_CONST_SCRIPTCODE);
    RecEval("non-minimal push under MINIMALDATA", Raw({OP_PUSHDATA1, 0x01, 0x07}), {}, SCRIPT_VERIFY_MINIMALDATA);
    RecEval("non-minimal push in a skipped branch", Raw({OP_0, OP_IF, OP_PUSHDATA1, 0x01, 0x07, OP_ENDIF}), {}, SCRIPT_VERIFY_MINIMALDATA);
    RecEval("non-minimal number operand under MINIMALDATA", CScript() << valtype{1, 0} << OP_1ADD, {}, SCRIPT_VERIFY_MINIMALDATA);
    RecEval("truncated push", Raw({OP_1, 0x05, 0xaa}), {}, NONE);
    RecEval("checkdatasig with too few items", CScript() << OP_1 << OP_1 << OP_CHECKDATASIG, {}, NONE);
    RecEval("altstack round trip", CScript() << OP_1 << OP_2 << OP_TOALTSTACK << OP_FROMALTSTACK, {}, NONE);
    // Deep conditionals (CS1a GPT n2). OP_IF, OP_ELSE and OP_ENDIF each count towards the 201 ops;
    // OP_0 and OP_1 do not. Balanced: 2d ops. Open: d ops, then UNBALANCED_CONDITIONAL.
    for (int depth : {99, 100, 101}) {
        CScript s;
        for (int i = 0; i < depth; ++i) s << OP_1 << OP_IF;
        s << OP_1;
        for (int i = 0; i < depth; ++i) s << OP_ENDIF;
        RecEval("balanced IF depth " + std::to_string(depth), s, {}, NONE);
    }
    for (int depth : {200, 201, 202}) {
        CScript s;
        for (int i = 0; i < depth; ++i) s << OP_1 << OP_IF;
        RecEval("open IF depth " + std::to_string(depth), s, {}, NONE);
    }
    for (int elses : {100, 101, 102}) {
        CScript s;
        for (int i = 0; i < 100; ++i) s << OP_1 << OP_IF;
        RecEval("open IF depth 100 then " + std::to_string(elses) + " ELSE", Cat(s, Repeat(OP_ELSE, elses)), {}, NONE);
    }
    {
        // 100 levels under a false ancestor: the inner IFs consume nothing (an empty stack).
        CScript s = CScript() << OP_0 << OP_IF;
        for (int i = 0; i < 99; ++i) s << OP_IF;
        for (int i = 0; i < 100; ++i) s << OP_ENDIF;
        RecEval("balanced depth 100 under a false ancestor", s << OP_1, {}, NONE);
    }
    // Repeated ELSE at different levels below a false ancestor never executes anything; the
    // ancestor's own ELSE does.
    for (int level = 1; level <= 3; ++level) {
        for (int elses : {1, 2, 3}) {
            CScript s = CScript() << OP_0 << OP_IF;
            for (int i = 1; i <= 3; ++i) {
                s << (i % 2 ? OP_IF : OP_NOTIF);
                if (i == level) {
                    for (int e = 0; e < elses; ++e) s << OP_ELSE << OP_RETURN;
                }
            }
            for (int i = 1; i <= 3; ++i) s << OP_ENDIF;
            s << OP_ELSE << OP_2 << OP_ENDIF;
            RecEval("false ancestor, " + std::to_string(elses) + " ELSE at level " + std::to_string(level), s, {}, NONE);
        }
    }
    for (int elses : {1, 2, 3, 4}) {
        // A false child of a true ancestor: each ELSE toggles it.
        CScript s = CScript() << OP_1 << OP_IF << OP_0 << OP_IF << OP_RETURN;
        for (int e = 0; e < elses; ++e) s << OP_ELSE << (e % 2 ? OP_RETURN : OP_3);
        s << OP_ENDIF << OP_ENDIF;
        RecEval("true ancestor, false child, " + std::to_string(elses) + " ELSE", s, {}, NONE);
    }
    RecEval("numequalverify true", CScript() << OP_2 << OP_2 << OP_NUMEQUALVERIFY << OP_1, {}, NONE);

    // A seeded random corpus.
    for (int i = 0; i < 2000; ++i) {
        Stack stack;
        for (uint64_t n = Rand(5); n > 0; --n) stack.push_back(RandItem());
        CScript script = RandScript(1 + Rand(14), true);
        unsigned int flags = RandFlags();
        RecEval("random " + std::to_string(i), script, stack, flags);
    }
}

// Scripts that each miss exactly one condition of IsPayToScriptHash (a9 14 <20> 87, 23 bytes).
std::vector<std::pair<std::string, CScript>> NotP2shScripts(const valtype& h20)
{
    auto raw = [](valtype v) { return CScript(v.begin(), v.end()); };
    valtype good{OP_HASH160, 0x14};
    good.insert(good.end(), h20.begin(), h20.end());
    good.push_back(OP_EQUAL);
    valtype second = good;
    second[1] = 0x13; // 23 bytes, a9 first and 87 last, but not a 20-byte push
    valtype first = good;
    first[0] = OP_HASH256;
    valtype last = good;
    last[22] = OP_EQUALVERIFY;
    valtype longer = good;
    longer.push_back(OP_NOP);
    return {{"second byte 0x13", raw(second)}, {"first byte HASH256", raw(first)},
            {"last byte EQUALVERIFY", raw(last)}, {"24 bytes", raw(longer)}};
}

void VerifyCases()
{
    Begin("verify");
    const unsigned int P2SH = SCRIPT_VERIFY_P2SH;
    CScript redeem_true = CScript() << OP_1;
    CScript redeem_false = CScript() << OP_0;
    auto p2sh = [](const CScript& redeem) {
        uint160 h = Hash160(redeem);
        return CScript() << OP_HASH160 << valtype(h.begin(), h.end()) << OP_EQUAL;
    };
    auto serialized = [](const CScript& s) { return valtype(s.begin(), s.end()); };
    RecVerify("SIGPUSHONLY", CScript() << OP_NOP << OP_1, CScript() << OP_1, SCRIPT_VERIFY_SIGPUSHONLY);
    RecVerify("p2sh spend", CScript() << serialized(redeem_true), p2sh(redeem_true), P2SH);
    RecVerify("p2sh spend without P2SH", CScript() << serialized(redeem_false), p2sh(redeem_false), SCRIPT_VERIFY_NONE);
    RecVerify("p2sh redeem script evaluates false", CScript() << serialized(redeem_false), p2sh(redeem_false), P2SH);
    RecVerify("p2sh non-push scriptSig", CScript() << OP_NOP << serialized(redeem_true), p2sh(redeem_true), P2SH);
    RecVerify("p2sh with OP_RESERVED in the scriptSig", CScript() << OP_0 << OP_IF << OP_RESERVED << OP_ENDIF << serialized(redeem_true), p2sh(redeem_true), P2SH);
    RecVerify("cleanstack", CScript() << OP_1 << OP_1, CScript() << OP_1, P2SH | SCRIPT_VERIFY_CLEANSTACK);
    RecVerify("cleanstack after p2sh", CScript() << OP_1 << serialized(redeem_true), p2sh(redeem_true), P2SH | SCRIPT_VERIFY_CLEANSTACK);
    RecVerify("eval false on an empty stack", CScript(), CScript(), SCRIPT_VERIFY_NONE);
    RecVerify("eval false on negative zero", CScript() << valtype{0x80}, CScript(), SCRIPT_VERIFY_NONE);
    RecVerify("op count restarts for the scriptPubKey", Repeat(OP_NOP, 201), Cat(Repeat(OP_NOP, 200), CScript() << OP_1), SCRIPT_VERIFY_NONE);
    RecVerify("the altstack does not carry over", CScript() << OP_1 << OP_TOALTSTACK, CScript() << OP_FROMALTSTACK, SCRIPT_VERIFY_NONE);
    RecVerify("an open IF does not carry over", CScript() << OP_1 << OP_IF, CScript() << OP_ENDIF << OP_1, SCRIPT_VERIFY_NONE);
    // A redeem script that would fail if evaluated as P2SH, and scriptPubKeys that each miss one
    // IsPayToScriptHash condition, so only the plain evaluation runs.
    {
        const CScript failing = CScript() << OP_RETURN;
        const uint160 hf = Hash160(failing);
        const valtype h20(hf.begin(), hf.end());
        for (const auto& [name, spk] : NotP2shScripts(h20)) {
            RecVerify("not P2SH: " + name, CScript() << OP_1 << serialized(failing), spk, P2SH);
        }
    }
    // The CLTV pair of GPT F5, at the flags of each classification run (computed here, not from a
    // pin's header, so both pins record the same thing): first run, v23 retry, v24 retry.
    const unsigned int first = P2SH | SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY;
    for (unsigned int flags : {first, first & ~0x147DEU, first & ~0x141CAU}) {
        RecVerify("CLTV negative then RETURN, flags " + std::to_string(flags), CScript(),
                  CScript() << OP_1NEGATE << OP_CHECKLOCKTIMEVERIFY << OP_DROP << OP_RETURN, flags);
        RecVerify("CLTV negative only, flags " + std::to_string(flags), CScript(),
                  CScript() << OP_1NEGATE << OP_CHECKLOCKTIMEVERIFY << OP_DROP << OP_1, flags);
    }
    for (int i = 0; i < 1000; ++i) {
        unsigned int flags = RandFlags();
        if ((flags & SCRIPT_VERIFY_CLEANSTACK) && !(flags & P2SH)) flags |= P2SH;
        CScript sig = RandScript(Rand(5), false);
        CScript spk = Chance(30) ? p2sh(RandScript(1 + Rand(6), false)) : RandScript(1 + Rand(8), true);
        if (Chance(30) && Chance(50)) sig << serialized(RandScript(1 + Rand(4), false));
        RecVerify("random " + std::to_string(i), sig, spk, flags);
    }
}

void FindAndDeleteCases()
{
    Begin("find_and_delete");
    // Core's script_tests.cpp:1283 script_FindAndDelete, case by case, built as the test builds them.
    auto from_hex = [](const char* hex) {
        const valtype v = ParseHex(hex);
        return CScript(v.begin(), v.end());
    };
    const std::pair<CScript, CScript> core_cases[] = {
        {CScript() << OP_1 << OP_2, CScript()},
        {CScript() << OP_1 << OP_2 << OP_3, CScript() << OP_2},
        {CScript() << OP_3 << OP_1 << OP_3 << OP_3 << OP_4 << OP_3, CScript() << OP_3},
        {from_hex("0302ff03"), from_hex("0302ff03")},
        {from_hex("0302ff030302ff03"), from_hex("0302ff03")},
        {from_hex("0302ff030302ff03"), from_hex("02")},
        {from_hex("0302ff030302ff03"), from_hex("ff")},
        {from_hex("0302ff030302ff03"), from_hex("03")},
        {from_hex("02feed5169"), from_hex("feed51")},
        {from_hex("02feed5169"), from_hex("02feed51")},
        {from_hex("516902feed5169"), from_hex("feed51")},
        {from_hex("516902feed5169"), from_hex("02feed51")},
        {CScript() << OP_0 << OP_0 << OP_1 << OP_1, CScript() << OP_0 << OP_1},
        {CScript() << OP_0 << OP_0 << OP_1 << OP_0 << OP_1 << OP_1, CScript() << OP_0 << OP_1},
        {from_hex("0003feed"), from_hex("03feed")},
        {from_hex("0003feed"), from_hex("00")},
    };
    for (size_t i = 0; i < std::size(core_cases); ++i) {
        RecFindAndDelete("script_FindAndDelete " + std::to_string(i + 1), core_cases[i].first, core_cases[i].second);
    }
    // GPT M1 and Opus m-2.
    RecFindAndDelete("not idempotent: first pass", Raw({OP_1, OP_1, OP_2, OP_2}), Raw({OP_1, OP_2}));
    RecFindAndDelete("not idempotent: second pass", Raw({OP_1, OP_2}), Raw({OP_1, OP_2}));
    RecFindAndDelete("adjacent matches inside push data", Raw({0x01, 0x01}), Raw({0x01}));
    RecFindAndDelete("odd edge case", Raw({0x03, 0x02, 0xff, 0x03, 0x03, 0x02, 0xff, 0x03}), Raw({0x03}));
    RecFindAndDelete("empty pattern", Raw({OP_1, OP_2}), CScript());
    RecFindAndDelete("empty script", CScript(), Raw({OP_1}));
    RecFindAndDelete("canonical push deleted", CScript() << valtype{0xaa, 0xbb} << OP_1 << valtype{0xaa, 0xbb}, Push({0xaa, 0xbb}));
    RecFindAndDelete("non-canonical push not deleted", Raw({OP_PUSHDATA1, 0x02, 0xaa, 0xbb}), Push({0xaa, 0xbb}));
    RecFindAndDelete("match inside push data", CScript() << valtype{0x02, 0xaa, 0xbb, 0x01}, Push({0xaa, 0xbb}));
    RecFindAndDelete("tail kept after a parse failure", Raw({OP_1, OP_2, 0x05, OP_1, OP_2}), Raw({OP_2}));
    RecFindAndDelete("match at the failing tail", Raw({OP_1, 0x4d, 0x01}), Raw({0x4d}));
    RecFindAndDelete("empty signature pattern deletes every OP_0", Raw({OP_0, OP_1, OP_0, OP_0, OP_CHECKSIG}), Raw({OP_0}));
    RecFindAndDelete("codeseparators", Raw({OP_CODESEPARATOR, OP_1, OP_CODESEPARATOR, OP_CODESEPARATOR, 0x01, OP_CODESEPARATOR}), Raw({OP_CODESEPARATOR}));
    RecFindAndDelete("pattern longer than the script", Raw({OP_1}), Raw({OP_1, OP_1}));
    for (int i = 0; i < 2000; ++i) {
        CScript script = RandScript(1 + Rand(10), true);
        CScript pattern;
        uint64_t pick = Rand(4);
        if (pick < 2 && !script.empty()) { // one instruction of the script (half), as a canonical push or opcode
            CScriptBase::const_iterator pc = script.begin();
            std::vector<std::pair<opcodetype, valtype>> ops;
            opcodetype op;
            valtype data;
            while (script.GetOp(pc, op, data)) ops.emplace_back(op, data);
            if (!ops.empty()) {
                auto [o, d] = ops[Rand(ops.size())];
                if (o <= OP_PUSHDATA4) pattern << d; else pattern << o;
            }
        } else if (pick == 2 && script.size() > 1) { // an arbitrary slice
            size_t a = Rand(script.size());
            size_t b = a + 1 + Rand(std::min<size_t>(4, script.size() - a));
            pattern = CScript(script.begin() + a, script.begin() + b);
        } else {
            valtype v = RandBytes(1 + Rand(3));
            pattern = CScript(v.begin(), v.end());
        }
        RecFindAndDelete("random " + std::to_string(i), script, pattern);
    }
}

void SigopCases()
{
    Begin("sigops");
    RecSigops("checkdatasig counts nothing", CScript() << OP_CHECKDATASIG << OP_CHECKDATASIGVERIFY);
    RecSigops("multisig counts", CScript() << OP_2 << OP_CHECKMULTISIG << OP_16 << OP_CHECKMULTISIGVERIFY << OP_CHECKMULTISIG);
    RecSigops("OP_0 before multisig", CScript() << OP_0 << OP_CHECKMULTISIG);
    RecSigops("a key count pushed as data is not OP_N", CScript() << valtype{0x02} << OP_CHECKMULTISIG);
    RecSigops("stops at a parse failure", Raw({OP_CHECKSIG, 0x05, 0xaa, OP_CHECKSIG}));
    RecSigops("previous opcode inside a skipped branch", CScript() << OP_0 << OP_IF << OP_3 << OP_ENDIF << OP_CHECKMULTISIG);
    for (int i = 0; i < 800; ++i) RecSigops("random " + std::to_string(i), RandScript(1 + Rand(12), true));

    Begin("sigops_with_sig");
    CScript redeem = CScript() << OP_2 << OP_CHECKMULTISIG << OP_CHECKSIG;
    uint160 h = Hash160(redeem);
    CScript p2sh = CScript() << OP_HASH160 << valtype(h.begin(), h.end()) << OP_EQUAL;
    valtype ser(redeem.begin(), redeem.end());
    RecSigopsWithSig("non-P2SH falls back to the accurate count", CScript() << OP_2 << OP_CHECKMULTISIG, CScript() << OP_RETURN);
    RecSigopsWithSig("p2sh counts the last push", p2sh, CScript() << OP_0 << ser);
    RecSigopsWithSig("p2sh trailing OP_1 clears the data", p2sh, CScript() << ser << OP_1);
    RecSigopsWithSig("p2sh trailing OP_RESERVED clears the data", p2sh, CScript() << ser << OP_RESERVED);
    RecSigopsWithSig("p2sh trailing OP_1NEGATE clears the data", p2sh, CScript() << ser << OP_1NEGATE);
    RecSigopsWithSig("p2sh empty scriptSig", p2sh, CScript());
    RecSigopsWithSig("p2sh non-push opcode", p2sh, CScript() << ser << OP_NOP);
    RecSigopsWithSig("p2sh truncated push", p2sh, Raw({0x05, OP_CHECKSIG}));
    for (const auto& [name, spk] : NotP2shScripts(valtype(h.begin(), h.end()))) {
        RecSigopsWithSig("not P2SH: " + name, spk, CScript() << OP_0 << ser);
    }
    for (int i = 0; i < 800; ++i) {
        CScript spk = Chance(60) ? p2sh : RandScript(1 + Rand(6), true);
        CScript sig = RandScript(Rand(5), true);
        if (Chance(60)) {
            CScript redeem_like = RandScript(1 + Rand(6), true);
            sig << valtype(redeem_like.begin(), redeem_like.end());
        }
        RecSigopsWithSig("random " + std::to_string(i), spk, sig);
    }
}

void GetOpCases()
{
    Begin("get_op");
    // Every truncation form (Opus m-1).
    const std::vector<std::pair<std::string, valtype>> named = {
        {"empty", {}},
        {"direct push, short data", {0x05, 0xaa}},
        {"PUSHDATA1 without its size", {0x4c}},
        {"PUSHDATA1, short data", {0x4c, 0x02, 0xaa}},
        {"PUSHDATA2 without its size", {0x4d}},
        {"PUSHDATA2 with one size byte", {0x4d, 0x01}},
        {"PUSHDATA2, short data", {0x4d, 0x02, 0x00, 0xaa}},
        {"PUSHDATA4 with 0 size bytes", {0x4e}},
        {"PUSHDATA4 with 1 size byte", {0x4e, 0x01}},
        {"PUSHDATA4 with 2 size bytes", {0x4e, 0x01, 0x02}},
        {"PUSHDATA4 with 3 size bytes", {0x4e, 0x01, 0x02, 0x03}},
        {"PUSHDATA4, short data", {0x4e, 0x01, 0x00, 0x00, 0x00}},
        {"PUSHDATA4, huge size", {0x4e, 0xff, 0xff, 0xff, 0xff, 0x00}},
        {"complete pushes then a truncated one", {0x51, 0x01, 0x07, 0x4c, 0x00, 0x4d, 0x01}},
    };
    for (const auto& [name, bytes] : named) RecGetOp(name, Raw(bytes));
    for (int i = 0; i < 500; ++i) RecGetOp("random " + std::to_string(i), RandScript(1 + Rand(8), true));
}

void NumCases()
{
    Begin("script_num_decode");
    const std::vector<valtype> named = {{}, {0x00}, {0x80}, {0x01, 0x00}, {0xff, 0x00}, {0xff, 0x80}, {0x01, 0x80},
                                        {0xff, 0xff, 0xff, 0x7f}, {0xff, 0xff, 0xff, 0xff}, {0xff, 0xff, 0xff, 0xff, 0x00},
                                        {0, 0, 0, 0, 1}, {0, 0, 0, 0x80, 0}, {1, 2, 3, 4, 5, 6}};
    for (const auto& v : named) {
        for (size_t max : {4, 5}) {
            RecNumDecode(v, false, max);
            RecNumDecode(v, true, max);
        }
    }
    for (int i = 0; i < 1000; ++i) {
        valtype v = RandBytes(Rand(7));
        if (!v.empty() && Chance(40)) v.back() &= 0x80;
        // One draw per statement: argument evaluation order is unspecified. (This is the order GCC
        // used when the corpus was first recorded.)
        const size_t max_size = Chance(50) ? 4 : 5;
        const bool minimal = Chance(50);
        RecNumDecode(v, minimal, max_size);
    }
    Begin("script_num_encode");
    for (int64_t v : std::vector<int64_t>{0, 1, -1, 127, 128, -128, 255, -255, 256, 0x7fffffff, -0x7fffffff, 0x80000000,
                                          (int64_t{1} << 39) - 1, -((int64_t{1} << 39) - 1), INT64_MAX, -INT64_MAX, INT64_MIN}) {
        RecNumEncode(v);
    }
    for (int i = 0; i < 500; ++i) {
        uint64_t shift = 1 + Rand(63);
        int64_t v = static_cast<int64_t>(g_rng() >> shift); // at most 2^63 - 1, so negating is safe
        RecNumEncode(Chance(50) ? v : -v);
    }
    Begin("minimally_encode");
    const std::vector<valtype> mins = {{}, {0x00}, {0x80}, {0x01, 0x00}, {0x01, 0x80}, {0x81, 0x00}, {0x81, 0x80},
                                       {0, 0, 0, 0x80}, {0x7f, 0, 0}, {0xff, 0, 0, 0x80}, {1, 0, 0, 0, 0, 0, 0, 0, 0}};
    for (const auto& v : mins) RecMinimallyEncode(v);
    for (int i = 0; i < 800; ++i) {
        valtype v = RandBytes(Rand(12));
        for (auto& b : v) if (Chance(50)) b = Chance(50) ? 0x00 : 0x80;
        RecMinimallyEncode(v);
    }
}

void SignatureCases(const Key& key)
{
    // Signatures for the encoding and ECDSA checks: valid, high-S, edge scalars, lax-DER forms.
    std::vector<std::pair<std::string, valtype>> sigs;
    for (unsigned char m : {1, 2}) {
        auto [r, s] = SignRS(1, Msg(m));
        valtype rb = DerInt(r.data()), sb = DerInt(s.data());
        std::string tag = " (msg " + std::to_string(m) + ")";
        sigs.emplace_back("valid low S" + tag, Der(rb, sb));
        sigs.emplace_back("valid high S" + tag, Der(rb, DerInt(NegateScalar(s).data())));
        valtype padded = rb;
        padded.insert(padded.begin(), 0x00);
        sigs.emplace_back("padded R" + tag, Der(padded, sb));
        valtype garbage = Der(rb, sb);
        garbage.insert(garbage.end(), {0xde, 0xad});
        sigs.emplace_back("trailing garbage" + tag, garbage);
        if (m == 1) {
            // A long-form R length with 0..9 length bytes (Core's lax parser rejects 4 or more
            // significant ones), and the high-S scalar paired with an overflowing R.
            for (int n = 0; n <= 9; ++n) {
                valtype v{0x30, 0x00, 0x02, static_cast<unsigned char>(0x80 | n)};
                for (int i = 0; i + 1 < n; ++i) v.push_back(0x00);
                if (n > 0) v.push_back(static_cast<unsigned char>(rb.size()));
                v.insert(v.end(), rb.begin(), rb.end());
                v.push_back(0x02);
                v.push_back(static_cast<unsigned char>(sb.size()));
                v.insert(v.end(), sb.begin(), sb.end());
                sigs.emplace_back("R length with " + std::to_string(n) + " length bytes, " + std::to_string(n ? n - 1 : 0) + " of them zero", v);
            }
            // Four significant length bytes: Core's lax parser rejects them (secp256k1-sys's copy
            // would not before 8). Three significant ones parse but claim more than is there.
            sigs.emplace_back("R length with four significant length bytes", valtype{0x30, 0x00, 0x02, 0x84, 0x01, 0x00, 0x00, 0x00, 0x02, 0x01, 0x01});
            sigs.emplace_back("R length with three significant length bytes", valtype{0x30, 0x00, 0x02, 0x83, 0x01, 0x00, 0x00, 0x02, 0x01, 0x01});
            valtype bigr{0x00};
            bigr.insert(bigr.end(), ORDER.begin(), ORDER.end());
            sigs.emplace_back("high S with R = n", Der(bigr, DerInt(NegateScalar(s).data())));
            valtype seq{0x30, 0x82, 0x00, 0x00, 0x02, static_cast<unsigned char>(rb.size())};
            seq.insert(seq.end(), rb.begin(), rb.end());
            seq.push_back(0x02);
            seq.push_back(static_cast<unsigned char>(sb.size()));
            seq.insert(seq.end(), sb.begin(), sb.end());
            sigs.emplace_back("long-form sequence length", seq);
            valtype good = Der(rb, sb);
            for (size_t cut = 0; cut < good.size(); cut += 7) sigs.emplace_back("truncated to " + std::to_string(cut), valtype(good.begin(), good.begin() + cut));
        }
    }
    auto withn = [](int delta) {
        valtype v = AddSmall(ORDER, delta);
        valtype body{0x00};
        body.insert(body.end(), v.begin(), v.end());
        return body;
    };
    const valtype one{0x01};
    valtype half = Half(ORDER);
    sigs.emplace_back("r = 0", Der(valtype{0x00}, one));
    sigs.emplace_back("s = 0", Der(one, valtype{0x00}));
    sigs.emplace_back("r = n", Der(withn(0), one));
    sigs.emplace_back("s = n", Der(one, withn(0)));
    sigs.emplace_back("r = n - 1", Der(withn(-1), one));
    sigs.emplace_back("s = n - 1", Der(one, withn(-1)));
    sigs.emplace_back("r = n + 1", Der(withn(1), one));
    sigs.emplace_back("s = 2^256 - 1", Der(one, [] { valtype v{0x00}; v.insert(v.end(), 32, 0xff); return v; }()));
    sigs.emplace_back("s = (n-1)/2", Der(one, DerInt(half.data())));
    sigs.emplace_back("s = (n-1)/2 + 1", Der(one, DerInt(AddSmall(half, 1).data())));
    sigs.emplace_back("33-byte R", Der(valtype(33, 0x01), one));
    sigs.emplace_back("negative R", Der(valtype{0x80, 0x01}, one));
    sigs.emplace_back("empty", valtype{});
    sigs.emplace_back("one byte", valtype{0x30});
    for (int i = 0; i < 200; ++i) {
        valtype v = sigs[Rand(sigs.size())].second;
        if (!v.empty()) {
            uint64_t at = Rand(v.size());
            uint64_t bit = Rand(8);
            v[at] ^= static_cast<unsigned char>(1u << bit);
        }
        if (Chance(20) && !v.empty()) v.resize(Rand(v.size()));
        sigs.emplace_back("mutated " + std::to_string(i), v);
    }
    // After the mutations, which sample this list. The remaining IsValidSignatureEncoding rules,
    // and the lax parser's integer-length failures: input ending at the length, and a long-form
    // length with more length bytes than remain.
    const valtype three{0x01, 0x02, 0x03};
    sigs.emplace_back("zero-length R", Der(valtype{}, three));
    sigs.emplace_back("zero-length S", Der(three, valtype{}));
    sigs.emplace_back("negative S", Der(one, valtype{0x80, 0x01}));
    sigs.emplace_back("padded S", Der(one, valtype{0x00, 0x01}));
    sigs.emplace_back("ends at the R length", valtype{0x30, 0x00, 0x02});
    sigs.emplace_back("R length bytes past the end", valtype{0x30, 0x00, 0x02, 0x84, 0x01});
    sigs.emplace_back("ends at the S length", valtype{0x30, 0x00, 0x02, 0x01, 0x01, 0x02});
    sigs.emplace_back("S length bytes past the end", valtype{0x30, 0x00, 0x02, 0x01, 0x01, 0x02, 0x82, 0x01});
    sigs.emplace_back("S tag 03", valtype{0x30, 0x06, 0x02, 0x01, 0x01, 0x03, 0x01, 0x01});
    sigs.emplace_back("ends after R", valtype{0x30, 0x03, 0x02, 0x01, 0x01});

    Begin("ecdsa");
    valtype hybrid = key.uncompressed;
    hybrid[0] = 0x06 | (hybrid[64] & 1);
    valtype hybrid_wrong = hybrid;
    hybrid_wrong[0] ^= 1;
    valtype x_ge_p(33, 0xff);
    x_ge_p[0] = 0x02;
    valtype off_curve(33, 0x00);
    off_curve[0] = 0x02;
    off_curve[32] = 0x05;
    valtype bad_len = key.uncompressed;
    bad_len.resize(33);
    const std::vector<std::pair<std::string, valtype>> keys = {
        {"compressed", key.compressed}, {"uncompressed", key.uncompressed}, {"hybrid", hybrid},
        {"hybrid with the wrong parity", hybrid_wrong}, {"compressed x >= p", x_ge_p}, {"compressed off-curve x", off_curve},
        {"uncompressed header, 33 bytes", bad_len}, {"header 05", valtype(33, 0x05)}, {"empty", {}}};
    for (const auto& [kname, k] : keys) {
        for (unsigned char m : {1, 2}) {
            for (const auto& [sname, sig] : sigs) {
                if (sname.rfind("mutated", 0) == 0 && (kname != "compressed" || m != 1)) continue;
                RecEcdsa(kname + " / msg " + std::to_string(m) + " / " + sname, k, Msg(m), sig);
            }
        }
    }

    Begin("signature_encoding");
    const unsigned int flagsets[] = {SCRIPT_VERIFY_NONE, SCRIPT_VERIFY_DERSIG, SCRIPT_VERIFY_LOW_S, SCRIPT_VERIFY_STRICTENC,
                                     SCRIPT_VERIFY_DERSIG | SCRIPT_VERIFY_LOW_S | SCRIPT_VERIFY_STRICTENC};
    for (const auto& [sname, sig] : sigs) {
        // Every hash-type class for the named signatures; the mutated ones only with SIGHASH_ALL.
        const bool mutated = sname.rfind("mutated", 0) == 0;
        for (unsigned char hashtype : {0x01, 0x04, 0x81, 0x00}) {
            if (mutated && hashtype != 0x01) continue;
            valtype s = sig;
            s.push_back(hashtype);
            for (unsigned int flags : flagsets) RecSigEncoding(sname + " + " + std::to_string(hashtype), s, flags);
        }
    }
    for (unsigned int flags : flagsets) RecSigEncoding("empty", {}, flags);
}

void PushCases()
{
    // CScript << std::vector<unsigned char> (script.h:448) around every push-opcode boundary. The
    // data is n bytes of 0x5a, so only the opcode and length bytes are recorded.
    Begin("push_data");
    for (size_t n : {0, 1, 2, 75, 76, 255, 256, 65535, 65536, 70000}) {
        CScript s;
        s << valtype(n, 0x5a);
        Require(s.size() >= n && std::all_of(s.end() - n, s.end(), [](unsigned char b) { return b == 0x5a; }), "push data");
        Emit("{\"len\":" + std::to_string(n) + ",\"prefix\":" + Str(Hex(s.data(), s.size() - n)) + "}");
    }
    // CheckMinimalPush (script.cpp:386) for every push opcode and data of these shapes: (length,
    // byte), all bytes equal.
    Begin("check_minimal_push");
    std::vector<std::pair<size_t, unsigned char>> shapes{{0, 0}};
    for (unsigned char b : {0x00, 0x01, 0x10, 0x11, 0x7f, 0x80, 0x81, 0xff}) shapes.emplace_back(1, b);
    for (size_t n : {2, 75, 76, 255, 256, 65535, 65536}) shapes.emplace_back(n, 0x5a);
    for (const auto& [n, b] : shapes) {
        const valtype data(n, b);
        for (int op = 0; op <= OP_PUSHDATA4; ++op) {
            Emit("{\"len\":" + std::to_string(n) + ",\"byte\":" + std::to_string(b) + ",\"opcode\":" + std::to_string(op) +
                 ",\"minimal\":" + (CheckMinimalPush(data, static_cast<opcodetype>(op)) ? "true" : "false") + "}");
        }
    }
}

void OpNameCases()
{
    Begin("op_names");
    for (int op = 0; op <= 0xff; ++op) {
        Emit("{\"opcode\":" + std::to_string(op) + ",\"name\":" + Str(GetOpName(static_cast<opcodetype>(op))) + "}");
    }
}

void ScriptNum10Cases()
{
    // Core's scriptnum_tests (src/test/scriptnum_tests.cpp:14-17, 164, 177) compare CScriptNum
    // with CScriptNum10, Bitcoin Core 0.10's implementation, on every value its `creation` and
    // `operators` cases construct. Recorded: CScriptNum10's getvch() and getint() for each of them.
    Begin("scriptnum10");
    static const int64_t values[] = {0, 1, -2, 127, 128, -255, 256, (1LL << 15) - 1, -(1LL << 16), (1LL << 24) - 1,
                                     (1LL << 31), 1 - (1LL << 32), 1LL << 40};
    static const int64_t offsets[] = {1, 0x79, 0x80, 0x81, 0xFF, 0x7FFF, 0x8000, 0xFFFF, 0x10000};
    // No sum or difference here overflows int64_t, so every operand and result is kept.
    std::set<int64_t> nums;
    auto create = [&](int64_t n) {
        nums.insert(n);
        nums.insert(CScriptNum10(n).getint());
    };
    auto operators = [&](int64_t a, int64_t b) {
        for (int64_t n : {a, b, a + b, a - b, b - a, -a}) nums.insert(n);
    };
    for (size_t i = 0; i < std::size(values); ++i) {
        for (size_t j = 0; j < std::size(offsets); ++j) {
            create(values[i]);
            create(values[i] + offsets[j]);
            create(values[i] - offsets[j]);
            const int64_t vi = values[i], vj = values[j];
            const std::pair<int64_t, int64_t> pairs[] = {
                {vi, vi}, {vi, -vi}, {vi, vj}, {vi, -vj}, {vi + vj, vj}, {vi + vj, -vj}, {vi - vj, vj}, {vi - vj, -vj},
                {vi + vj, vi + vj}, {vi + vj, vi - vj}, {vi - vj, vi + vj}, {vi - vj, vi - vj}};
            for (const auto& [a, b] : pairs) operators(a, b);
        }
    }
    for (int64_t n : nums) {
        const CScriptNum10 num(n);
        Emit("{\"value\":" + std::to_string(n) + ",\"vch\":" + Str(Hex(num.getvch())) + ",\"getint\":" +
             std::to_string(num.getint()) + "}");
    }
}

// Signatures, keys and scripts for the mock checker. The signatures are real DER encodings, so
// the STRICTENC, DERSIG and LOW_S checks before CheckSig pass or fail as they would on chain; the
// checker's verdict then comes from the hash.
struct MockMaterial {
    std::vector<valtype> sigs;    // DER, no hash type
    std::vector<valtype> pubkeys; // compressed, uncompressed, hybrid, malformed
};

MockMaterial MakeMockMaterial()
{
    MockMaterial m;
    for (unsigned char seed = 1; seed <= 3; ++seed) {
        for (unsigned char msg = 1; msg <= 2; ++msg) {
            auto [r, s] = SignRS(seed, Msg(msg));
            m.sigs.push_back(Der(DerInt(r.data()), DerInt(s.data())));
            if (msg == 1) m.sigs.push_back(Der(DerInt(r.data()), DerInt(NegateScalar(s).data()))); // high S
        }
        const Key k = MakeKey(seed);
        m.pubkeys.push_back(k.compressed);
        m.pubkeys.push_back(k.compressed);
        m.pubkeys.push_back(k.uncompressed);
        valtype hybrid = k.uncompressed;
        hybrid[0] = 0x06 | (k.uncompressed[64] & 1);
        m.pubkeys.push_back(hybrid);
    }
    valtype padded = m.sigs[0]; // a non-DER signature: R with a needless 0x00
    padded.insert(padded.begin() + 4, 0x00);
    padded[1] += 1;
    padded[3] += 1;
    m.sigs.push_back(padded);
    valtype bad_prefix = m.pubkeys[0];
    bad_prefix[0] = 0x05;
    m.pubkeys.push_back(bad_prefix);
    m.pubkeys.push_back(valtype(33, 0x02));
    return m;
}

valtype MockSig(const MockMaterial& m)
{
    if (Chance(8)) return {};
    if (Chance(5)) return RandItem();
    valtype sig = m.sigs[Rand(m.sigs.size())];
    static const unsigned char hashtypes[] = {0x02, 0x03, 0x81, 0x82, 0x83, 0x00, 0x04, 0x84};
    sig.push_back(Chance(80) ? 0x01 : hashtypes[Rand(8)]);
    return sig;
}

valtype MockKey(const MockMaterial& m) { return Chance(90) ? m.pubkeys[Rand(m.pubkeys.size())] : RandItem(); }

void MockChecksig(CScript& s, const MockMaterial& m)
{
    const valtype sig = MockSig(m);
    s << sig << MockKey(m);
    if (Chance(20)) s << OP_CODESEPARATOR;
    s << (Chance(60) ? OP_CHECKSIG : OP_CHECKSIGVERIFY);
    if (Chance(30)) s << sig << OP_DROP; // a FindAndDelete target in the scriptCode
}

void MockMultisig(CScript& s, const MockMaterial& m)
{
    const int keys = Chance(10) ? static_cast<int>(Rand(21)) : static_cast<int>(Rand(5));
    const int sigs = static_cast<int>(Rand(keys + 2)); // sometimes one more than the keys
    s << (Chance(85) ? valtype{} : RandItem()); // the dummy
    std::vector<valtype> pushed;
    for (int i = 0; i < sigs; ++i) {
        pushed.push_back(MockSig(m));
        s << pushed.back();
    }
    s << CScriptNum(sigs);
    for (int i = 0; i < keys; ++i) s << MockKey(m);
    s << CScriptNum(keys);
    if (Chance(20)) s << OP_CODESEPARATOR;
    s << (Chance(60) ? OP_CHECKMULTISIG : OP_CHECKMULTISIGVERIFY);
    if (Chance(30) && !pushed.empty()) s << pushed[Rand(pushed.size())] << OP_DROP;
}

void MockLockTime(CScript& s)
{
    int64_t n = static_cast<int64_t>(Rand(12)) - 1;
    if (Chance(20)) n += int64_t{1} << 31; // CSV's disable bit
    s << CScriptNum(n) << (Chance(50) ? OP_CHECKLOCKTIMEVERIFY : OP_CHECKSEQUENCEVERIFY);
    if (Chance(60)) s << OP_DROP;
}

CScript MockScript(size_t pieces, const MockMaterial& m)
{
    CScript s;
    for (size_t i = 0; i < pieces; ++i) {
        switch (Rand(10)) {
        case 0: case 1: case 2: MockChecksig(s, m); break;
        case 3: case 4: case 5: MockMultisig(s, m); break;
        case 6: MockLockTime(s); break;
        case 7: s << OP_CODESEPARATOR; break;
        case 8: s << ALPHABET[Rand(ALPHABET.size())]; break;
        default: {
            CScript inner;
            MockChecksig(inner, m);
            s << (Chance(70) ? OP_IF : OP_NOTIF);
            s.insert(s.end(), inner.begin(), inner.end());
            s << OP_ELSE << OP_1 << OP_ENDIF;
            break;
        }
        }
    }
    return s;
}

void MockCases()
{
    // An RNG of its own, so these sections do not move when the others change.
    g_rng.seed(0xC51A0C4ECULL);
    const MockChecker mock;
    const MockMaterial m = MakeMockMaterial();
    const unsigned int STRICT = SCRIPT_VERIFY_STRICTENC | SCRIPT_VERIFY_DERSIG | SCRIPT_VERIFY_LOW_S | SCRIPT_VERIFY_NULLFAIL;
    valtype sig = m.sigs[0];
    sig.push_back(0x01);
    const valtype& pk = m.pubkeys[0];

    Begin("eval_mock");
    // Each named shape with a few hash types, so that both verdicts of the mock occur.
    for (unsigned char hashtype : {0x01, 0x02, 0x03, 0x81}) {
        valtype s = m.sigs[0];
        s.push_back(hashtype);
        const std::string tag = ", hash type " + std::to_string(hashtype);
        for (unsigned int flags : {static_cast<unsigned int>(SCRIPT_VERIFY_NONE), STRICT}) {
            const std::string f = ", flags " + std::to_string(flags);
            RecEval("checksig" + tag + f, CScript() << s << pk << OP_CHECKSIG, {}, flags, mock);
            RecEval("checksigverify" + tag + f, CScript() << s << pk << OP_CHECKSIGVERIFY << OP_1, {}, flags, mock);
            RecEval("checksig after a codeseparator" + tag + f,
                    CScript() << s << pk << OP_1 << OP_DROP << OP_CODESEPARATOR << OP_CHECKSIG, {}, flags, mock);
            RecEval("checksig with its signature in the scriptCode" + tag + f,
                    CScript() << s << pk << OP_CHECKSIG << s << OP_DROP, {}, flags, mock);
            RecEval("checksig with its signature in the scriptCode, CONST_SCRIPTCODE" + tag + f,
                    CScript() << s << pk << OP_CHECKSIG << s << OP_DROP, {}, flags | SCRIPT_VERIFY_CONST_SCRIPTCODE, mock);
        }
    }
    // Every ordered pair of three signatures against three keys, 2-of-3 and 1-of-3.
    for (size_t a = 0; a < 3; ++a) {
        for (size_t b = 0; b < 3; ++b) {
            valtype sa = m.sigs[3 * a], sb = m.sigs[3 * b]; // each key's low-S signature of message 1
            sa.push_back(0x01);
            sb.push_back(0x01);
            CScript keys = CScript() << m.pubkeys[0] << m.pubkeys[4] << m.pubkeys[8] << OP_3;
            const std::string tag = " sigs " + std::to_string(a) + "," + std::to_string(b);
            RecEval("multisig 2-of-3" + tag, Cat(CScript() << OP_0 << sa << sb << OP_2, keys) << OP_CHECKMULTISIG, {}, STRICT, mock);
            RecEval("multisigverify 2-of-3" + tag, Cat(CScript() << OP_0 << sa << sb << OP_2, keys) << OP_CHECKMULTISIGVERIFY << OP_1, {}, STRICT, mock);
            RecEval("multisig 1-of-3" + tag, Cat(CScript() << OP_0 << sa << OP_1, keys) << OP_CHECKMULTISIG, {}, STRICT, mock);
        }
    }
    for (int64_t n = -1; n <= 6; ++n) {
        const unsigned int flags = SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY | SCRIPT_VERIFY_CHECKSEQUENCEVERIFY;
        RecEval("cltv " + std::to_string(n), CScript() << CScriptNum(n) << OP_CHECKLOCKTIMEVERIFY, {}, flags, mock);
        RecEval("csv " + std::to_string(n), CScript() << CScriptNum(n) << OP_CHECKSEQUENCEVERIFY, {}, flags, mock);
        RecEval("csv " + std::to_string(n) + " with the disable bit",
                CScript() << CScriptNum(n + (int64_t{1} << 31)) << OP_CHECKSEQUENCEVERIFY, {}, flags, mock);
    }
    for (int i = 0; i < 600; ++i) {
        Stack stack;
        for (uint64_t n = Rand(3); n > 0; --n) stack.push_back(Chance(50) ? MockSig(m) : MockKey(m));
        CScript script = MockScript(1 + Rand(4), m);
        const unsigned int flags = RandFlags();
        RecEval("random " + std::to_string(i), script, stack, flags, mock);
    }

    Begin("verify_mock");
    auto p2sh = [](const CScript& redeem) {
        uint160 h = Hash160(redeem);
        return CScript() << OP_HASH160 << valtype(h.begin(), h.end()) << OP_EQUAL;
    };
    const uint160 pkh = Hash160(pk);
    const CScript p2pkh = CScript() << OP_DUP << OP_HASH160 << valtype(pkh.begin(), pkh.end()) << OP_EQUALVERIFY << OP_CHECKSIG;
    const CScript multisig = CScript() << OP_2 << m.pubkeys[0] << m.pubkeys[4] << m.pubkeys[8] << OP_3 << OP_CHECKMULTISIG;
    for (unsigned int flags : {static_cast<unsigned int>(SCRIPT_VERIFY_P2SH), SCRIPT_VERIFY_P2SH | STRICT | SCRIPT_VERIFY_CLEANSTACK}) {
        const std::string f = ", flags " + std::to_string(flags);
        RecVerify("p2pk" + f, CScript() << sig, CScript() << pk << OP_CHECKSIG, flags, mock);
        RecVerify("p2pkh" + f, CScript() << sig << pk, p2pkh, flags, mock);
        for (size_t a = 0; a < m.sigs.size(); a += 3) {
            valtype sa = m.sigs[a], sb = m.sigs[(a + 1) % m.sigs.size()];
            sa.push_back(0x01);
            sb.push_back(0x01);
            const std::string tag = " sigs " + std::to_string(a) + f;
            RecVerify("bare multisig" + tag, CScript() << OP_0 << sa << sb, multisig, flags, mock);
            RecVerify("p2sh multisig" + tag, CScript() << OP_0 << sa << sb << valtype(multisig.begin(), multisig.end()),
                      p2sh(multisig), flags, mock);
        }
    }
    for (int i = 0; i < 250; ++i) {
        unsigned int flags = RandFlags();
        if ((flags & SCRIPT_VERIFY_CLEANSTACK) && !(flags & SCRIPT_VERIFY_P2SH)) flags |= SCRIPT_VERIFY_P2SH;
        CScript sig_script;
        for (uint64_t n = Rand(4); n > 0; --n) {
            if (Chance(15)) sig_script << OP_0;
            else sig_script << (Chance(60) ? MockSig(m) : MockKey(m));
        }
        CScript spk;
        if (Chance(40)) {
            CScript redeem = MockScript(1 + Rand(2), m);
            if (Chance(85) && redeem.size() <= MAX_SCRIPT_ELEMENT_SIZE) sig_script << valtype(redeem.begin(), redeem.end());
            spk = p2sh(redeem);
        } else {
            spk = MockScript(1 + Rand(3), m);
        }
        RecVerify("random " + std::to_string(i), sig_script, spk, flags, mock);
    }
}

} // namespace

int main()
{
    g_ctx = secp256k1_context_create(SECP256K1_CONTEXT_NONE);
    Key key = MakeKey(1);
    ErrorStrings();
    EvalCases(key);
    VerifyCases();
    FindAndDeleteCases();
    SigopCases();
    GetOpCases();
    NumCases();
    SignatureCases(key);
    OpNameCases();
    PushCases();
    ScriptNum10Cases();
    MockCases();
    Print();
    secp256k1_context_destroy(g_ctx);
    return 0;
}
