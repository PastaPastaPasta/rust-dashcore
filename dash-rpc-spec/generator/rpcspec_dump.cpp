// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

// rpcspec_dump: prints the structure of every RPC command Dash Core registers, as JSON.
//
// Built against a Dash Core build tree by build-dump.sh and run by generate.py. It replays
// the registration order of init.cpp (control commands, RegisterAllCoreRPCCommands, the
// wallet's GetWalletRPCCommands, the wallet-only Dash tables, ZMQ) twice: once as a node
// with -disablewallet and once with the wallet enabled. Each command's RPCHelpMan is
// recovered from CRPCCommand::unique_id, which the plain constructor sets to the method's
// function pointer, so the dump holds the declared arguments (aliases, types, fallbacks,
// skip_type_check, hidden) rather than what the help text shows.
//
// Output, one object:
//   {"commands": [{"name", "category", "fn_id", "registered_without_wallet",
//                  "registered_with_wallet", "from_wallet_table", "args": [...], "help"}...],
//    "arg_map": <help dump_all_command_conversions of the wallet-enabled table>}
// `help` is RPCHelpMan::ToString(); generate.py does not use it as the help text (that comes
// from a running dashd) but checks that the two agree.

#include <chainparams.h>
#include <chainparamsbase.h>
#include <rpc/register.h>
#include <rpc/server.h>
#include <rpc/util.h>
#include <univalue.h>
#include <util/system.h>
#include <util/translation.h>
#include <wallet/rpc/wallet.h>
#include <zmq/zmqrpc.h>

#include <algorithm>
#include <cstdio>
#include <functional>
#include <map>
#include <set>
#include <string>
#include <variant>
#include <vector>

const std::function<std::string(const char*)> G_TRANSLATION_FUN = nullptr;

namespace {

// Standard-conforming access to private members: explicit instantiation may name them.
template <typename Tag, typename Tag::type M>
struct Rob {
    friend typename Tag::type get(Tag) { return M; }
};
struct HelpManArgs {
    using type = const std::vector<RPCArg> RPCHelpMan::*;
    friend type get(HelpManArgs);
};
template struct Rob<HelpManArgs, &RPCHelpMan::m_args>;
struct TableCommands {
    using type = std::map<std::string, std::vector<const CRPCCommand*>> CRPCTable::*;
    friend type get(TableCommands);
};
template struct Rob<TableCommands, &CRPCTable::mapCommands>;

const char* TypeName(RPCArg::Type t)
{
    switch (t) {
    case RPCArg::Type::OBJ: return "OBJ";
    case RPCArg::Type::ARR: return "ARR";
    case RPCArg::Type::STR: return "STR";
    case RPCArg::Type::NUM: return "NUM";
    case RPCArg::Type::BOOL: return "BOOL";
    case RPCArg::Type::OBJ_USER_KEYS: return "OBJ_USER_KEYS";
    case RPCArg::Type::AMOUNT: return "AMOUNT";
    case RPCArg::Type::STR_HEX: return "STR_HEX";
    case RPCArg::Type::RANGE: return "RANGE";
    }
    return "?";
}

UniValue ArgJson(const RPCArg& arg)
{
    UniValue o(UniValue::VOBJ);
    o.pushKV("names", arg.m_names);
    o.pushKV("type", TypeName(arg.m_type));
    UniValue fb(UniValue::VOBJ);
    if (const auto* opt = std::get_if<RPCArg::Optional>(&arg.m_fallback)) {
        fb.pushKV("kind", *opt == RPCArg::Optional::NO ? "required" : "omitted");
    } else if (const auto* hint = std::get_if<RPCArg::DefaultHint>(&arg.m_fallback)) {
        fb.pushKV("kind", "hint");
        fb.pushKV("value", *hint);
    } else {
        fb.pushKV("kind", "default");
        fb.pushKV("value", std::get<RPCArg::Default>(arg.m_fallback).write());
    }
    o.pushKV("fallback", fb);
    o.pushKV("skip_type_check", arg.m_opts.skip_type_check);
    o.pushKV("hidden", arg.m_opts.hidden);
    UniValue inner(UniValue::VARR);
    for (const auto& i : arg.m_inner) inner.push_back(ArgJson(i));
    o.pushKV("inner", inner);
    return o;
}

struct Registered {
    std::string category;
    RpcMethodFnType fn;
    bool from_wallet_table;
};

// name -> registrations in order (front() is what help and dispatch see first)
using Table = std::map<std::string, std::vector<Registered>>;

// Every v24 command uses CRPCCommand's plain constructor, whose unique_id is the RpcMethodFnType.
// A command built with the Actor constructor and some other id would make this call crash;
// a new pin that adds one needs another way to reach its RPCHelpMan.
void Collect(CRPCTable& t, const std::set<intptr_t>& wallet_ids, Table& out)
{
    for (const auto& [name, cmds] : t.*get(TableCommands())) {
        for (const CRPCCommand* c : cmds) {
            auto fn = reinterpret_cast<RpcMethodFnType>(c->unique_id);
            if (fn().m_name != name) {
                fprintf(stderr, "error: %s: unique_id is not its RPCHelpMan function\n", name.c_str());
                exit(1);
            }
            out[name].push_back({c->category, fn, wallet_ids.count(c->unique_id) > 0});
        }
    }
}

void AppendSpan(CRPCTable& t, Span<const CRPCCommand> cmds, std::set<intptr_t>& wallet_ids)
{
    for (const auto& c : cmds) {
        t.appendCommand(c.name, &c);
        wallet_ids.insert(c.unique_id);
    }
}

// The order of init.cpp's AppInitMain; the control commands come from the CRPCTable constructor.
void RegisterLikeInit(CRPCTable& t, bool wallet, std::set<intptr_t>& wallet_ids)
{
    gArgs.ForceSetArg("-disablewallet", wallet ? "0" : "1");
    RegisterAllCoreRPCCommands(t);
    if (wallet) {
        AppendSpan(t, wallet::GetWalletRPCCommands(), wallet_ids);
        for (const auto& cmds : {GetWalletCoinJoinRPCCommands(), GetWalletEvoRPCCommands(),
                                 GetWalletGovernanceRPCCommands(), GetWalletMasternodeRPCCommands()}) {
            AppendSpan(t, cmds, wallet_ids);
        }
    }
#if ENABLE_ZMQ
    RegisterZMQRPCCommands(t);
#else
#error "build Core with ZMQ: release builds register getzmqnotifications"
#endif
}

} // namespace

int main()
{
    SelectParams(CBaseChainParams::REGTEST);

    Table without, with;
    // no_ids stays empty: the wallet tables are appended only when the wallet is enabled.
    std::set<intptr_t> no_ids, wallet_ids;
    {
        static CRPCTable t;
        RegisterLikeInit(t, /*wallet=*/false, no_ids);
        Collect(t, no_ids, without);
    }
    static CRPCTable t_wallet;
    RegisterLikeInit(t_wallet, /*wallet=*/true, wallet_ids);
    Collect(t_wallet, wallet_ids, with);

    std::set<std::string> names;
    for (const auto& [n, _] : without) names.insert(n);
    for (const auto& [n, _] : with) names.insert(n);

    // Core's help overview prints a method once per handler (setDone over unique_id).
    std::map<RpcMethodFnType, int> fn_ids;
    UniValue commands(UniValue::VARR);
    for (const auto& name : names) {
        const auto w = with.find(name);
        const auto& regs = w != with.end() ? w->second : without.at(name);
        const RPCHelpMan h = regs.front().fn();
        const bool from_wallet{std::any_of(regs.begin(), regs.end(), [](const auto& r) { return r.from_wallet_table; })};
        UniValue o(UniValue::VOBJ);
        o.pushKV("name", name);
        o.pushKV("category", regs.front().category);
        o.pushKV("fn_id", fn_ids.try_emplace(regs.front().fn, (int)fn_ids.size()).first->second);
        o.pushKV("registered_without_wallet", without.count(name) > 0);
        o.pushKV("registered_with_wallet", w != with.end());
        o.pushKV("from_wallet_table", from_wallet);
        UniValue args(UniValue::VARR);
        for (const auto& a : h.*get(HelpManArgs())) args.push_back(ArgJson(a));
        o.pushKV("args", args);
        o.pushKV("help", h.ToString());
        commands.push_back(o);
    }

    JSONRPCRequest req;
    UniValue result(UniValue::VOBJ);
    result.pushKV("commands", commands);
    result.pushKV("arg_map", t_wallet.dumpArgMap(req));
    printf("%s\n", result.write(1).c_str());
    return 0;
}
