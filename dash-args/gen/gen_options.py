#!/usr/bin/env python3
"""Generate dash-args/src/options/generated.rs from Dash Core's sources.

Usage:
    python3 -I gen_options.py --core-src <v24.0.0-rc.5 src dir> \
        --help-dir <captured Linux help dir> --help-dir-windows <captured Windows help dir> \
        --out <generated.rs>

Option names, help parameters, flags, categories and registration order come from the
`AddArg`/`AddHiddenArgs`/`AddCommand` calls in Core's setup functions, with `#if` blocks
evaluated for the release build configuration. Core builds most help texts at runtime
(`strprintf` with defaults, translated strings), so the texts of visible options are taken
from the `-help`/`-help-debug` output of the release binaries (captured by
capture_core_outputs.py) and unwrapped; each program's help is then re-rendered from the
table and must match the capture byte for byte, for the Linux and the Windows configuration.
Hidden options carry no help text: Core never prints it.

A few help defaults depend on the platform Core was built for (`-uiplatform`, the GUI font
weights, the `-dbcache` maximum). Their values for every platform are read from the source,
checked against the captures, and emitted as one registration per platform.

See PROVENANCE.md.
"""

import argparse
import difflib
import re
import sys
from pathlib import Path

CORE_TAG = "v24.0.0-rc.5"
CORE_COMMIT = "8163290aa0307ed4bc2fc0ba15b78c20d61dfa88"

# Release build configuration (depends/ builds, `configure` defaults for the release).
# Linux and macOS share it; Windows lacks sockaddr_un and fork, and configure turns the external
# signer off for mingw.
UNIX = {
    "HAVE_SYSTEM": 1,
    "ENABLE_WALLET": 1,
    "ENABLE_ZMQ": 1,
    "USE_UPNP": 1,
    "USE_NATPMP": 1,
    "HAVE_SOCKADDR_UN": 1,
    "ENABLE_EXTERNAL_SIGNER": 1,
    "USE_BDB": 1,
    "USE_SQLITE": 1,
    "HAVE_DECL_FORK": 1,
}
WINDOWS = dict(UNIX, HAVE_SOCKADDR_UN=None, HAVE_DECL_FORK=0, WIN32=1, ENABLE_EXTERNAL_SIGNER=None)
CONFIGS = {"unix": UNIX, "windows": WINDOWS}

# Core setup functions: (Rust name, source file, signature regex, Core name).
FUNCTIONS = {
    "setup_help_options": ("util/system.cpp", r"^void SetupHelpOptions\(ArgsManager& args\)", "SetupHelpOptions"),
    "setup_server_args": ("init.cpp", r"^void SetupServerArgs\(ArgsManager& argsman\)", "SetupServerArgs"),
    "add_logging_args": ("init/common.cpp", r"^void AddLoggingArgs\(ArgsManager& argsman\)", "init::AddLoggingArgs"),
    "add_wallet_options": ("wallet/init.cpp", r"^void WalletInit::AddWalletOptions\(ArgsManager& argsman\) const", "WalletInit::AddWalletOptions"),
    "setup_chain_params_base_options": ("chainparamsbase.cpp", r"^void SetupChainParamsBaseOptions\(ArgsManager& argsman\)", "SetupChainParamsBaseOptions"),
    "setup_chain_params_options": ("chainparams.cpp", r"^void SetupChainParamsOptions\(ArgsManager& argsman\)", "SetupChainParamsOptions"),
    "setup_ui_args": ("qt/bitcoin.cpp", r"^static void SetupUIArgs\(ArgsManager& argsman\)", "SetupUIArgs"),
    "setup_cli_args": ("bitcoin-cli.cpp", r"^static void SetupCliArgs\(ArgsManager& argsman\)", "SetupCliArgs"),
    "setup_bitcoin_tx_args": ("bitcoin-tx.cpp", r"^static void SetupBitcoinTxArgs\(ArgsManager &argsman\)", "SetupBitcoinTxArgs"),
    "setup_wallet_tool_args": ("bitcoin-wallet.cpp", r"^static void SetupWalletToolArgs\(ArgsManager& argsman\)", "SetupWalletToolArgs"),
    "setup_bitcoin_util_args": ("bitcoin-util.cpp", r"^static void SetupBitcoinUtilArgs\(ArgsManager &argsman\)", "SetupBitcoinUtilArgs"),
}

# Calls to other setup functions, as they appear in the bodies.
CALLS = {
    "SetupHelpOptions": "setup_help_options",
    "init::AddLoggingArgs": "add_logging_args",
    "g_wallet_init_interface.AddWalletOptions": "add_wallet_options",
    "SetupChainParamsBaseOptions": "setup_chain_params_base_options",
    "SetupChainParamsOptions": "setup_chain_params_options",
    "SetupServerArgs": "setup_server_args",
}

# Programs: (Rust variant, setup functions in call order, help captures (plain, debug)). The
# Windows dash-qt shows its help in a dialog, so only the other programs have Windows captures.
PROGRAMS = [
    ("Dashd", ["setup_server_args"], ("dashd-help.txt", "dashd-help-debug.txt")),
    ("DashQt", ["setup_server_args", "setup_ui_args"], ("dash-qt-help.txt", "dash-qt-help-debug.txt")),
    ("DashCli", ["setup_cli_args"], ("dash-cli-help.txt", None)),
    ("DashTx", ["setup_bitcoin_tx_args"], ("dash-tx-help.txt", None)),
    ("DashWallet", ["setup_wallet_tool_args"], ("dash-wallet-help.txt", None)),
    ("DashUtil", ["setup_bitcoin_util_args"], ("dash-util-help.txt", None)),
]

CATEGORIES = [
    "OPTIONS", "CONNECTION", "INDEXING", "MASTERNODE", "STATSD", "WALLET", "WALLET_FEE",
    "WALLET_HD", "WALLET_COINJOIN", "WALLET_DEBUG_TEST", "ZMQ", "DEBUG_TEST", "CHAINPARAMS",
    "NODE_RELAY", "BLOCK_CREATION", "RPC", "GUI", "COMMANDS", "REGISTER_COMMANDS", "HIDDEN",
]
RUST_CATEGORY = {c: "".join(w.capitalize() for w in c.split("_")) for c in CATEGORIES}
RUST_CATEGORY["WALLET_HD"] = "WalletHd"
HEADERS = {
    "OPTIONS": "Options:", "CONNECTION": "Connection options:", "INDEXING": "Indexing options:",
    "MASTERNODE": "Masternode options:", "STATSD": "Statsd options:",
    "ZMQ": "ZeroMQ notification options:", "DEBUG_TEST": "Debugging/Testing options:",
    "NODE_RELAY": "Node relay options:", "BLOCK_CREATION": "Block creation options:",
    "RPC": "RPC server options:", "WALLET": "Wallet options:", "WALLET_FEE": "Wallet fee options:",
    "WALLET_HD": "HD wallet options:", "WALLET_COINJOIN": "CoinJoin options:",
    "WALLET_DEBUG_TEST": "Wallet debugging/testing options:",
    "CHAINPARAMS": "Chain selection options:", "GUI": "UI Options:", "COMMANDS": "Commands:",
    "REGISTER_COMMANDS": "Register Commands:",
}
FLAGS = {"ALLOW_ANY": 0x01, "DISALLOW_NEGATION": 0x20, "DISALLOW_ELISION": 0x40,
         "DEBUG_ONLY": 0x100, "NETWORK_ONLY": 0x200, "SENSITIVE": 0x400}


def fail(msg):
    sys.exit(f"gen_options: {msg}")


# --- C++ lexing -----------------------------------------------------------------------------

def strip_comments(src):
    """Remove // and /* */ comments, keeping string and char literals and newlines."""
    out, i, n = [], 0, len(src)
    while i < n:
        c = src[i]
        if c in "\"'":
            j = i + 1
            while src[j] != c:
                j += 2 if src[j] == "\\" else 1
            out.append(src[i:j + 1])
            i = j + 1
        elif src.startswith("//", i):
            while i < n and src[i] != "\n":
                i += 1
        elif src.startswith("/*", i):
            j = src.index("*/", i + 2)
            out.append("\n" * src.count("\n", i, j))
            i = j + 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def skip_literal(text, i):
    """Index after the string or char literal starting at text[i]."""
    q = text[i]
    j = i + 1
    while text[j] != q:
        j += 2 if text[j] == "\\" else 1
    return j + 1


def matching(text, i):
    """Index of the bracket closing the one at text[i]."""
    pairs = {"(": ")", "{": "}", "[": "]"}
    stack = [pairs[text[i]]]
    j = i + 1
    while stack:
        c = text[j]
        if c in "\"'":
            j = skip_literal(text, j)
            continue
        if c in pairs:
            stack.append(pairs[c])
        elif c in ")}]":
            if c != stack.pop():
                fail(f"unbalanced brackets near {text[max(0, j - 40):j + 40]!r}")
        j += 1
    return j - 1


def split_top(text, sep=","):
    """Split at top-level separators (outside brackets and literals)."""
    parts, depth, start, j = [], 0, 0, 0
    while j < len(text):
        c = text[j]
        if c in "\"'":
            j = skip_literal(text, j)
            continue
        if c in "({[":
            depth += 1
        elif c in ")}]":
            depth -= 1
        elif c == sep and depth == 0:
            parts.append(text[start:j].strip())
            start = j + 1
        j += 1
    parts.append(text[start:].strip())
    return parts


ESCAPES = {"n": "\n", "t": "\t", '"': '"', "\\": "\\", "'": "'"}


def string_literal(expr):
    """The value of a (concatenated) string literal expression, or None if it is not one."""
    expr = expr.strip()
    value, i = [], 0
    if not expr.startswith('"'):
        return None
    while i < len(expr):
        if expr[i].isspace():
            i += 1
            continue
        if expr[i] != '"':
            return None
        j = i + 1
        while expr[j] != '"':
            if expr[j] == "\\":
                if expr[j + 1] not in ESCAPES:
                    fail(f"unsupported escape in {expr!r}")
                value.append(ESCAPES[expr[j + 1]])
                j += 2
            else:
                value.append(expr[j])
                j += 1
        i = j + 1
    return "".join(value)


# --- Preprocessing --------------------------------------------------------------------------

# Every macro the configurations name; any other macro in a directive is an error.
KNOWN_MACROS = set().union(*CONFIGS.values())


def macro_defined(name, config):
    if name not in KNOWN_MACROS:
        fail(f"unknown macro {name}")
    return config.get(name) is not None


def eval_condition(expr, config):
    expr = expr.strip()

    def defined(m):
        return "1" if macro_defined(m.group(1), config) else "0"

    expr = re.sub(r"defined\s*\(\s*(\w+)\s*\)", defined, expr)

    def ident(m):
        name = m.group(0)
        if name.isdigit():
            return name
        if name not in KNOWN_MACROS:
            fail(f"unknown macro {name} in #if {expr}")
        return str(config.get(name) or 0)

    expr = re.sub(r"\b\w+\b", ident, expr)
    expr = expr.replace("&&", " and ").replace("||", " or ")
    expr = re.sub(r"!(?!=)", " not ", expr)
    return bool(eval(expr, {"__builtins__": {}}))


def preprocess(body, config):
    """Evaluate #if/#ifdef/#ifndef/#elif/#else/#endif lines in a function body."""
    out = []
    stack = []  # (active, taken) per nesting level
    for line in body.split("\n"):
        m = re.match(r"\s*#\s*(\w+)\s*(.*)$", line)
        if not m:
            out.append(line if all(a for a, _ in stack) else "")
            continue
        directive, rest = m.group(1), m.group(2)
        parent = all(a for a, _ in stack)
        if directive == "if":
            cond = eval_condition(rest, config)
            stack.append((parent and cond, cond))
        elif directive == "ifdef":
            cond = macro_defined(rest.split()[0], config)
            stack.append((parent and cond, cond))
        elif directive == "ifndef":
            cond = not macro_defined(rest.split()[0], config)
            stack.append((parent and cond, cond))
        elif directive == "elif":
            _, taken = stack.pop()
            parent = all(a for a, _ in stack)
            cond = not taken and eval_condition(rest, config)
            stack.append((parent and cond, taken or cond))
        elif directive == "else":
            _, taken = stack.pop()
            parent = all(a for a, _ in stack)
            stack.append((parent and not taken, True))
        elif directive == "endif":
            stack.pop()
        else:
            fail(f"unsupported directive #{directive}")
        out.append("")
    if stack:
        fail("unterminated #if")
    return "\n".join(out)


def function_body(core_src, rel, signature):
    src = strip_comments((core_src / rel).read_text())
    m = re.search(signature, src, re.M)
    if not m:
        fail(f"{signature} not found in {rel}")
    start = src.index("{", m.end())
    return src[start + 1:matching(src, start)]


# --- Setup function parsing -----------------------------------------------------------------

def parse_flags(expr):
    expr = expr.strip()
    if expr in ("false", "0"):
        return []
    names = []
    for part in expr.split("|"):
        m = re.fullmatch(r"\s*ArgsManager::(\w+)\s*", part)
        if not m or m.group(1) not in FLAGS:
            fail(f"unsupported flags {expr!r}")
        names.append(m.group(1))
    return names


def parse_category(expr):
    m = re.fullmatch(r"\s*OptionsCategory::(\w+)\s*", expr)
    if not m or m.group(1) not in CATEGORIES:
        fail(f"unsupported category {expr!r}")
    return m.group(1)


def parse_list(expr):
    """A braced list of string literals."""
    expr = expr.strip()
    if not (expr.startswith("{") and expr.endswith("}")):
        fail(f"expected a braced list, got {expr!r}")
    items = []
    for item in split_top(expr[1:-1]):
        if not item:
            continue
        value = string_literal(item)
        if value is None:
            fail(f"non-literal hidden arg {item!r}")
        items.append(value)
    return items


CALL_RE = re.compile(
    r"(?P<obj>\w+)\.(?P<method>AddArg|AddHiddenArgs|AddCommand)\s*\("
    r"|(?P<fn>" + "|".join(re.escape(c) for c in CALLS) + r")\s*\(\s*(?:argsman|args|gArgs)\s*\)"
    r"|std::vector<std::string>\s+hidden_args\s*=\s*(?P<init>\{)"
    r"|hidden_args\.emplace_back\s*\("
)


def parse_body(fn, body):
    """The operations of one setup function body, in order."""
    ops = []
    hidden = None
    pos = 0
    while True:
        m = CALL_RE.search(body, pos)
        if not m:
            break
        if m.group("fn"):
            ops.append(("call", CALLS[m.group("fn")]))
            pos = m.end()
        elif m.group("init"):
            brace = m.start("init")
            close = matching(body, brace)
            hidden = parse_list(body[brace:close + 1])
            pos = close + 1
        elif m.group(0).startswith("hidden_args"):
            paren = m.end() - 1
            close = matching(body, paren)
            value = string_literal(body[paren + 1:close])
            if hidden is None or value is None:
                fail(f"{fn}: unexpected hidden_args.emplace_back")
            hidden.append(value)
            pos = close + 1
        else:
            paren = m.end() - 1
            close = matching(body, paren)
            args = split_top(body[paren + 1:close])
            method = m.group("method")
            if method == "AddArg":
                if len(args) != 4:
                    fail(f"{fn}: AddArg with {len(args)} arguments: {args}")
                name = string_literal(args[0])
                if name is None:
                    fail(f"{fn}: non-literal option name {args[0]!r}")
                ops.append(("arg", name, string_literal(args[1]), parse_flags(args[2]),
                            parse_category(args[3])))
            elif method == "AddCommand":
                name, help_text = string_literal(args[0]), string_literal(args[1])
                if name is None:
                    fail(f"{fn}: non-literal command {args[0]!r}")
                ops.append(("command", name, help_text))
            else:
                if args == ["hidden_args"]:
                    if hidden is None:
                        fail(f"{fn}: AddHiddenArgs(hidden_args) before its declaration")
                    ops.append(("hidden", list(hidden), True))
                else:
                    ops.append(("hidden", parse_list(args[0]), False))
            pos = close + 1
    # Every mention of the registration API must have been consumed.
    unparsed = re.sub(CALL_RE, "", body)
    for word in ("AddArg", "AddHiddenArgs", "AddCommand"):
        if re.search(r"\b" + word + r"\b", unparsed):
            fail(f"{fn}: {word} in an unrecognized form")
    # Any other use of the ArgsManager (a call to a helper not in CALLS, say) could hide
    # registrations. Reading chain parameters for help defaults registers nothing.
    unparsed = re.sub(r"\bCreateChainParams\(\s*argsman\s*,", "", unparsed)
    if re.search(r"\b(argsman|gArgs)\b", unparsed):
        fail(f"{fn}: the ArgsManager is used in an unrecognized form")
    if unparsed.count("hidden_args") != sum(1 for op in ops if op[0] == "hidden" and op[2]):
        fail(f"{fn}: hidden_args used in an unrecognized form")
    return ops


def parse_functions(core_src, config):
    result = {}
    for fn, (rel, signature, _) in FUNCTIONS.items():
        body = function_body(core_src, rel, signature)
        result[fn] = parse_body(fn, preprocess(body, config))
    return result


def flatten(functions, roots):
    """Registration order of a program: (defining function, op) pairs."""
    out = []
    for fn in roots:
        for op in functions[fn]:
            if op[0] == "call":
                out.extend(flatten(functions, [op[1]]))
            else:
                out.append((fn, op))
    return out


# --- Help rendering and capture parsing -----------------------------------------------------

def format_paragraph(text, width, indent):
    """Core: FormatParagraph."""
    out, ptr, indented = [], 0, 0
    n = len(text)
    while ptr < n:
        lineend = text.find("\n", ptr)
        if lineend == -1:
            lineend = n
        linelen = lineend - ptr
        rem_width = width - indented
        if linelen <= rem_width:
            out.append(text[ptr:lineend + 1])
            ptr = lineend + 1
            indented = 0
        else:
            finalspace = max(text.rfind(" ", 0, ptr + rem_width + 1), text.rfind("\n", 0, ptr + rem_width + 1))
            if finalspace == -1 or finalspace < ptr:
                # No place to break; just include the entire word and move on.
                cands = [i for i in (text.find("\n", ptr), text.find(" ", ptr)) if i != -1]
                if not cands:
                    out.append(text[ptr:])
                    break
                finalspace = min(cands)
            out.append(text[ptr:finalspace] + "\n")
            if text[finalspace] == "\n":
                indented = 0
            elif indent:
                out.append(" " * indent)
                indented = indent
            ptr = finalspace + 1
    return "".join(out)


def help_opt(option, message):
    return "  " + option + "\n" + " " * 7 + format_paragraph(message, 79 - 7, 7) + "\n\n"


def available_args(flat):
    """Core's m_available_args after registration: category -> name -> (param, help, flags)."""
    cats = {}
    for fn, op in flat:
        if op[0] == "arg":
            _, name, help_text, flags, cat = op
            eq = name.find("=")
            arg_name, param = (name, "") if eq == -1 else (name[:eq], name[eq:])
            entries = cats.setdefault(cat, {})
            if arg_name in entries:
                fail(f"duplicate {arg_name} in {cat}")
            entries[arg_name] = (param, help_text, flags, fn)
        elif op[0] == "hidden":
            entries = cats.setdefault("HIDDEN", {})
            for name in op[1]:
                eq = name.find("=")
                arg_name, param = (name, "") if eq == -1 else (name[:eq], name[eq:])
                if arg_name in entries:
                    fail(f"duplicate hidden {arg_name}")
                entries[arg_name] = (param, "", ["ALLOW_ANY"], fn)
        elif op[0] == "command":
            entries = cats.setdefault("COMMANDS", {})
            if op[1] in entries:
                fail(f"duplicate command {op[1]}")
            entries[op[1]] = ("", op[2], ["COMMAND"], fn)
    return cats


def render_help(cats, show_debug, texts, variants, platform):
    usage = []
    for cat in CATEGORIES:
        if cat not in cats:
            continue
        if cat in HEADERS and (cat != "WALLET_DEBUG_TEST" or show_debug):
            usage.append(HEADERS[cat] + "\n\n")
        if cat == "HIDDEN":
            break
        for name in sorted(cats[cat], key=lambda s: s.encode()):
            param, _, flags, fn = cats[cat][name]
            if show_debug or "DEBUG_ONLY" not in flags:
                text = texts[(fn, cat, name + param)]
                if (fn, name + param) in variants:
                    text = platform_text(text, *variants[(fn, name + param)], platform)
                usage.append(help_opt(name + param, text))
    return "".join(usage)


def capture_body(path):
    text = path.read_text()
    i = text.find("\nOptions:\n\n")
    if i == -1:
        fail(f"{path}: no Options: header")
    return text[i + 1:]


def parse_capture(cats, show_debug, body, texts, source):
    """Walk a captured help message in the order Core prints it, collecting option texts."""
    expected = []
    for cat in CATEGORIES:
        if cat not in cats:
            continue
        if cat in HEADERS and (cat != "WALLET_DEBUG_TEST" or show_debug):
            expected.append(("header", HEADERS[cat] + "\n\n"))
        if cat == "HIDDEN":
            break
        for name in sorted(cats[cat], key=lambda s: s.encode()):
            param, _, flags, fn = cats[cat][name]
            if show_debug or "DEBUG_ONLY" not in flags:
                expected.append(("opt", (fn, cat, name + param), "  " + name + param + "\n" + " " * 7))
    pos = 0
    for k, item in enumerate(expected):
        if item[0] == "header":
            if not body.startswith(item[1], pos):
                fail(f"{source}: expected header {item[1]!r} at {body[pos:pos + 80]!r}")
            pos += len(item[1])
            continue
        _, key, prefix = item
        if not body.startswith(prefix, pos):
            fail(f"{source}: expected {prefix!r} at {body[pos:pos + 80]!r}")
        start = pos + len(prefix)
        # The entry ends at the "\n\n" followed by the next expected item (or the end).
        if k + 1 < len(expected):
            nxt = expected[k + 1]
            marker = "\n\n" + (nxt[1] if nxt[0] == "header" else nxt[2])
            end = body.find(marker, start)
        else:
            end = len(body) - 2 if body.endswith("\n\n") else -1
        if end == -1:
            fail(f"{source}: cannot find the end of {key}")
        wrapped = body[start:end]
        # Undo FormatParagraph: a line starting with the 7-column indent continues a wrap at a
        # space; any other line follows a newline that was in the text.
        lines = wrapped.split("\n")
        text = lines[0]
        for line in lines[1:]:
            text += " " + line[7:] if line.startswith(" " * 7) else "\n" + line
        if format_paragraph(text, 72, 7) != wrapped:
            fail(f"{source}: {key} does not round-trip")
        if key in texts and texts[key] != text:
            fail(f"{source}: {key} has two texts:\n{texts[key]!r}\n{text!r}")
        texts[key] = text
        pos = end + 2
    if pos != len(body):
        fail(f"{source}: unparsed trailing help {body[pos:pos + 80]!r}")


# --- Platform-dependent help values ---------------------------------------------------------

# The platforms the captures come from (x86_64 Linux and Windows releases).
CAPTURE_PLATFORMS = {"unix": {"os": "linux", "pointer_width": 64},
                     "windows": {"os": "windows", "pointer_width": 64}}


def source_match(core_src, rel, pattern):
    m = re.search(pattern, strip_comments((core_src / rel).read_text()), re.S)
    if not m:
        fail(f"{rel}: {pattern!r} not found")
    return m


def check_registration(core_src, fn, name, expr):
    """Check that fn's AddArg call for name builds its help with expr."""
    rel, signature, _ = FUNCTIONS[fn]
    body = function_body(core_src, rel, signature)
    start = body.find(f'argsman.AddArg("{name}"')
    if start == -1 or body.count(f'argsman.AddArg("{name}"') != 1:
        fail(f"{fn}: no single AddArg call for {name}")
    open_paren = body.index("(", start)
    if expr not in body[start:matching(body, open_paren) + 1]:
        fail(f"{fn}: the help of {name} no longer uses {expr}")


def platform_variants(core_src):
    """(function, option) -> (regex whose group is the value in the help text,
    [(Rust cfg predicate or None, platform predicate, value)]), the variants covering every
    platform once."""
    variants = {}

    check_registration(core_src, "setup_ui_args", "-uiplatform", "GUIUtil::defaultUIPlatform()")
    m = source_match(core_src, "qt/guiutil.cpp",
                     r'std::string defaultUIPlatform\(\)\s*\{\s*#if defined\(Q_OS_MACOS\)\s*return "(\w+)";'
                     r'\s*#elif defined\(Q_OS_WIN\)\s*return "(\w+)";\s*#else\s*return "(\w+)";\s*#endif\s*\}')
    variants[("setup_ui_args", "-uiplatform")] = (r"; default: (\w+)\)$", [
        ('target_os = "macos"', lambda p: p["os"] == "macos", m[1]),
        ("windows", lambda p: p["os"] == "windows", m[2]),
        ('not(any(windows, target_os = "macos"))', lambda p: p["os"] not in ("macos", "windows"), m[3]),
    ])

    # GUIUtil::defaultWeightArg maps the registered font's default weight through mapWeightArgs;
    # SetupUIArgs runs before any font is registered, so it maps TARGET_WEIGHT_*.
    font = "qt/guiutil_font.cpp"
    kv = source_match(core_src, font, r"const auto mapWeightArgs = \[\]\(\) \{\s*std::map<int, QFont::Weight> kv\{(.*?)\};")
    weight_arg = {w: k for k, w in re.findall(r"\{(\d+), QFont::(\w+)\}", kv[1])}
    for slot, getter in (("BOLD", "GetWeightBoldDefault"), ("NORMAL", "GetWeightNormalDefault")):
        source_match(core_src, font, getter + r"\(\) const\s*\{\s*if \(auto it = m_weights\.find\(m_font\); "
                     r"it != m_weights\.end\(\)\) \{ return it->second\.m_\w+_default; \}\s*return TARGET_WEIGHT_"
                     + slot + r";\s*\}")
    bold = source_match(core_src, font, r"constexpr QFont::Weight TARGET_WEIGHT_BOLD\{QFont::(\w+)\};")
    normal = source_match(core_src, font, r"constexpr QFont::Weight TARGET_WEIGHT_NORMAL\{\s*#ifdef Q_OS_MACOS\s*"
                          r"QFont::(\w+)\s*#else\s*QFont::(\w+)\s*#endif\s*\};")
    for name, slot in (("-font-weight-bold", "Bold"), ("-font-weight-normal", "Normal")):
        check_registration(core_src, "setup_ui_args", name, f"GUIUtil::defaultWeightArg(GUIUtil::FontWeight::{slot})")
    variants[("setup_ui_args", "-font-weight-bold")] = (r"\(default: (\d+)\)$", [
        (None, lambda p: True, weight_arg[bold[1]]),
    ])
    variants[("setup_ui_args", "-font-weight-normal")] = (r"\(default: (\d+)\)$", [
        ('target_os = "macos"', lambda p: p["os"] == "macos", weight_arg[normal[1]]),
        ('not(target_os = "macos")', lambda p: p["os"] != "macos", weight_arg[normal[2]]),
    ])

    check_registration(core_src, "setup_server_args", "-dbcache=<n>", "nMaxDbCache")
    m = source_match(core_src, "txdb.h", r"static const int64_t nMaxDbCache = sizeof\(void\*\) > 4 \? (\d+) : (\d+);")
    variants[("setup_server_args", "-dbcache=<n>")] = (r"\(\d+ to (\d+), default", [
        ('target_pointer_width = "64"', lambda p: p["pointer_width"] == 64, m[1]),
        ('not(target_pointer_width = "64")', lambda p: p["pointer_width"] != 64, m[2]),
    ])
    return variants


def platform_text(text, pattern, options, platform):
    """text with the value pattern locates replaced by the platform's value."""
    values = [value for _, applies, value in options if applies(platform)]
    if len(values) != 1:
        fail(f"{len(values)} values for {platform} in {text!r}")
    return substitute(text, pattern, values[0])


def substitute(text, pattern, value):
    found = list(re.finditer(pattern, text, re.M))
    if len(found) != 1:
        fail(f"{pattern!r} matches {len(found)} times in {text!r}")
    return text[:found[0].start(1)] + value + text[found[0].end(1):]


# --- Rust emission --------------------------------------------------------------------------

def rust_str(s):
    out = []
    for c in s:
        if c == "\\":
            out.append("\\\\")
        elif c == '"':
            out.append('\\"')
        elif c == "\n":
            out.append("\\n")
        elif c == "\t":
            out.append("\\t")
        elif ord(c) < 0x20 or ord(c) == 0x7F:
            out.append(f"\\u{{{ord(c):x}}}")
        else:
            out.append(c)
    return '"' + "".join(out) + '"'


def rust_flags(flags):
    return " | ".join(flags) if flags else "0"


def op_lines(fn, op, texts, variants):
    if op[0] == "call":
        return [f"{op[1]}(argsman);"]
    if op[0] == "hidden":
        names = ", ".join(rust_str(n) for n in op[1])
        return [f"argsman.add_hidden_args(&[{names}]);"]
    if op[0] == "command":
        return [f"argsman.add_command({rust_str(op[1])}, {rust_str(op[2])});"]
    _, name, literal, flags, cat = op
    if cat == "HIDDEN":
        text = ""
    else:
        text = texts.get((fn, cat, name))
        if text is None:
            text = literal
        if text is None:
            fail(f"{fn}: no help text for {name} (not captured and not a literal)")
        if literal is not None and literal != text:
            fail(f"{fn}: literal help of {name} differs from the capture")
    line = lambda text: f"argsman.add_arg({rust_str(name)}, {rust_str(text)}, {rust_flags(flags)}, {RUST_CATEGORY[cat]});"
    if (fn, name) not in variants:
        return [line(text)]
    pattern, options = variants[(fn, name)]
    lines = []
    for cfg, _, value in options:
        if cfg is not None:
            lines.append(f"#[cfg({cfg})]")
        lines.append(line(substitute(text, pattern, value)))
    return lines


def emit_function(fn, per_config, texts, variants):
    """One Rust setup function; ops that differ between configurations get cfg attributes."""
    unix = [op_lines(fn, op, texts, variants) for op in per_config["unix"][fn]]
    windows = [op_lines(fn, op, texts, variants) for op in per_config["windows"][fn]]
    key = lambda lines: "\n".join(lines)
    matcher = difflib.SequenceMatcher(a=[key(x) for x in unix], b=[key(x) for x in windows], autojunk=False)
    body = []
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag == "equal":
            for lines in unix[i1:i2]:
                body.extend(lines)
            continue
        for cfg, ops in (("not(windows)", unix[i1:i2]), ("windows", windows[j1:j2])):
            for lines in ops:
                for line in lines:
                    # Before each statement, after any platform attribute of its own.
                    if not line.startswith("#["):
                        body.append(f"#[cfg({cfg})]")
                    body.append(line)
    rel, _, core_name = FUNCTIONS[fn]
    out = [f"/// Core: `{core_name}` (src/{rel}).", f"pub fn {fn}(argsman: &ArgsManager) {{"]
    out += ["    " + line for line in body]
    out.append("}")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--core-src", required=True, type=Path)
    ap.add_argument("--help-dir", required=True, type=Path)
    ap.add_argument("--help-dir-windows", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    a = ap.parse_args()

    per_config = {name: parse_functions(a.core_src, cfg) for name, cfg in CONFIGS.items()}
    variants = platform_variants(a.core_src)
    help_dirs = {"unix": a.help_dir, "windows": a.help_dir_windows}

    # SetupHelpOptions is ported by hand in args.rs; check the port still matches the source.
    expected_help_ops = [("arg", "-?", "Print this help message and exit", [], "OPTIONS"),
                         ("hidden", ["-h", "-help"], False)]
    for cfg in per_config.values():
        if cfg["setup_help_options"] != expected_help_ops:
            fail(f"SetupHelpOptions changed: {cfg['setup_help_options']}")

    def captures(config, variant, plain, debug):
        if config == "windows" and variant == "DashQt":
            return []
        return [(help_dirs[config] / name, show_debug) for name, show_debug in ((plain, False), (debug, True)) if name]

    texts = {}
    for config in CONFIGS:
        for variant, roots, (plain, debug) in PROGRAMS:
            cats = available_args(flatten(per_config[config], roots))
            for path, show_debug in captures(config, variant, plain, debug):
                parse_capture(cats, show_debug, capture_body(path), texts, f"{config} {path.name}")
    # Fill in texts only visible in another program's capture, then check every program's
    # help re-renders to its capture.
    for config in CONFIGS:
        for variant, roots, (plain, debug) in PROGRAMS:
            cats = available_args(flatten(per_config[config], roots))
            for cat, entries in cats.items():
                for name, (param, literal, _flags, fn) in entries.items():
                    key = (fn, cat, name + param)
                    if cat != "HIDDEN" and key not in texts:
                        if literal is None:
                            fail(f"no help text for {key}")
                        texts[key] = literal
            for path, show_debug in captures(config, variant, plain, debug):
                rendered = render_help(cats, show_debug, texts, variants, CAPTURE_PLATFORMS[config])
                if rendered != capture_body(path):
                    fail(f"{config} {path.name}: re-rendered help differs")

    used = sorted({f for cfg in per_config.values() for ops in cfg.values()
                   for op in ops if op[0] == "arg" for f in op[3]}, key=FLAGS.get)
    out = [
        f"// @generated by dash-args/gen/gen_options.py from Dash Core {CORE_TAG} ({CORE_COMMIT}).",
        "// Do not edit: regenerate (see gen/PROVENANCE.md).",
        "",
        "use crate::args::OptionsCategory::*;",
        "use crate::args::{ArgsManager, setup_help_options};",
        "",
    ]
    out += [f"const {f}: u32 = ArgsManager::{f};" for f in used]
    for fn in FUNCTIONS:
        if fn != "setup_help_options":
            out += ["", emit_function(fn, per_config, texts, variants)]
    a.out.write_text("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
