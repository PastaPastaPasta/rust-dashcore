#!/usr/bin/env python3
"""The debug-log lines Dash Core's functional tests assert (A1 §2.18, DEC-201 Q15).

Extracts every fragment passed to `assert_debug_log` (expected and unexpected) and
`busy_wait_for_debug_log` in the pinned Core tree's `test/functional/*.py`, the `reject_reason=`
of every call (`send_blocks_and_test` and `send_txs_and_test` in test_framework/p2p.py assert it
with `assert_debug_log`, and the tests' own helpers pass it through), and the strings in
`DIRECT_READS` that a test looks for in debug.log by hand. The invalid-transaction templates of
`test/functional/data/invalid_txs.py` reach the assertions through an attribute
(`TEMPLATE_READS`); those calls take every template's reason. It keeps them in
`dashd-node/tests/asserted_log_lines.txt` with an owner and a status per fragment, for an
`implemented` row the runtime test that proves it (DEC-204), and for a `linked` row the backlog
item that flips it (DEC-209). The ratchet over that list is
`dashd-node/tests/asserted_log_lines.rs` (no Core tree needed): it checks each cited test exists
where the gate runs it, and scans rdashd's literals as a tripwire, not as proof.

  asserted-logs.py generate --core ~/workspace/dash   # rewrite the list (keeps the KEPT columns)
  asserted-logs.py check --core ~/workspace/dash      # fail if the list is out of sync with Core
  asserted-logs.py selftest                           # the resolver on the forms it follows
  asserted-logs.py pin                                # CORE_TAG=... CORE_COMMIT=... (for CI)

Fragments are read with `ast` and resolved to every string they can take:
- a string, bytes or f-string literal as is (an f-string keeps its `{expr}` placeholders), and
  `%`/`.format` templates the same way;
- a list or tuple: its elements; `a if c else b`: both branches; `x[i]` (not a slice): every
  element of the list, tuple or dict literal `x` is;
- a name: every value it is bound to in the function (or module) that binds it, by `=`, by
  `.append`/`.insert`/`.extend`, by a `for` loop over a list (each element), or by a tuple `for`
  target over a list of tuples (its column; a name the loop walks is taken at its last binding
  before the loop, which must be an `=` in the loop's block or one enclosing it, with no
  binding later in a loop around it); a parameter: every argument the file's calls of that
  function pass it (a method's: the calls on `self`), or its default; a name also bound or
  changed any other way (`+=`, `del`, another mutating method, an addition from another scope,
  `with ... as`, `except ... as`, `import`, `global`, a lambda parameter, a comprehension
  target, a walrus in a comprehension, a `match` capture) is not resolved, nor a parameter a
  call on another object, a `*args` or a `**kwargs` may pass, nor one of a function passed as
  a value. Aliases (`b = a; b.append(x)`) are not tracked;
- a call of a function of the file: every value it returns.
Anything else fails generation, unless `EXPANSIONS` lists the strings for its file and line:
those rules are pinned and reviewed with the list. So does an argument form not read: a `*` or
`**` spread into a call whose arguments are read (`CALLS`, and the file's functions with a
positional `reject_reason`), a `reject_reason` passed to such a function by position, a
keyword of `KEYWORDS` written as a string (a dict spread into a call), and a `CALLS` helper
named but not called (an alias, `functools.partial`).

The list is a tracking ledger (DEC-204): every fragment Core asserts has a row, and an
`implemented` row cites the runtime test that proves it. The list is not an inventory of
rdashd's log lines; Core logs many lines no test asserts.

Rows the tests do not produce (source not under test/functional/, such as ruling 3's
`src/init.cpp` line) are kept as written.
"""

import argparse
import ast
import re
import string
import subprocess
import sys
import tempfile
from pathlib import Path

REF = "v24.0.0-rc.4"
LIST = Path(__file__).resolve().parents[2] / "dashd-node" / "tests" / "asserted_log_lines.txt"
CALLS = {"assert_debug_log", "busy_wait_for_debug_log"}
# Fragments a test checks against debug.log without the helpers: (file, the name bound to them).
DIRECT_READS = {("test/functional/feature_governance_cl.py", "expected_msg")}
# Calls that assert an invalid-transaction template's reason: (file, the argument's source) ->
# "tx" (relay: every template's `reject_reason`) or "block" (in a block: `block_reject_reason or
# reject_reason`, for the templates not `valid_in_block`).
TEMPLATE_READS = {
    ("test/functional/p2p_invalid_tx.py", "template.reject_reason"): "tx",
    ("test/functional/feature_block.py",
     "template.block_reject_reason or template.reject_reason"): "block",
}
TEMPLATES = "test/functional/data/invalid_txs.py"
# Assertions the resolver cannot follow: (file, line, the argument's source) -> the strings they
# assert, as (kind, text). Each entry is pinned to REF and reviewed with the list; one Core no
# longer has fails generation.
EXPANSIONS = {}
# REF's commit (the peeled tag): `check` and `generate` refuse a Core tree where REF names another.
# CI fetches the pair `asserted-logs.py pin` prints.
REF_COMMIT = "0e7382df80d0bc6811068d076b232a5a26d34a3c"
COLUMNS = ["status", "owner", "sense", "kind", "source", "fragment", "match", "proof", "until"]
# The columns every row has; `match`, `proof` and `until` are optional and trail.
REQUIRED = COLUMNS.index("match")
# The columns a regeneration keeps from the old list.
KEPT = ("status", "owner", "match", "proof", "until")
# The keywords whose arguments are read. Written as a string (a dict spread into a call), one
# would pass a fragment unseen.
KEYWORDS = {"reject_reason", "expected_msgs", "unexpected_msgs"}
STATUSES = {"implemented", "pending", "linked"}

HEADER = f"""\
# Debug-log fragments Dash Core {REF}'s functional tests assert, one per row (tab-separated).
# Generated by contrib/node/asserted-logs.py from the Core tree; status, owner, match, proof and
# until are kept across regeneration. The ratchet is dashd-node/tests/asserted_log_lines.rs.
#
# A tracking ledger with cited runtime proof (DEC-204): every fragment the tests assert has a
# row, and an implemented row cites the test that shows it in rdashd's real output. The list does
# not inventory rdashd's log lines; Core logs many lines no test asserts.
#
# status    implemented: rdashd logs it where the test expects it (expected), or keeps it out of
#           the log where the test asserts so (unexpected), as `proof` shows. pending: not yet.
#           linked (DEC-209; an unexpected row only as DEC-280/282 allow: P3's, its BL until): the
#           line's emitter is in rdashd's build but not reached yet; `until` names the item.
# owner     the task that owns the line: it makes the node emit it (expected), or shows it stays
#           silent where the test asserts so (unexpected).
# sense     expected (must appear) or unexpected (must not appear where the test asserts it).
# kind      literal or fstring ({{expr}} placeholders stand for runtime values).
# source    where Core asserts it (first call in the file), or the Core line that emits it.
# fragment  the text, with \\t, \\n and \\\\ escaped.
# match     optional: the text the tripwire looks for in Rust instead (`{{}}` for runtime values;
#           `{{|}}` where the line joins several Rust literals). Between `{{|}}` marks, the pieces
#           must appear in order within one literal.
# proof     an implemented row's test, <crate>::<target>::<fn>: it runs the node (the process, or
#           its log sink) and reads the real log. The ratchet checks the gate runs it and its
#           file names the row's text; a reviewer judges what it asserts when a status changes.
# until     a linked row's backlog item, BL-<n> (and only a linked row's); it cites no proof.
#
# The ratchet also scans the string literals of rdashd's production build, as a tripwire and not
# as proof: an implemented or linked row whose text (or match) no active literal holds fails (a
# linked one is stale), and so does a pending row one holds (flip it, with a proof, once the
# owner has shown it).
"""

# Owners by test-file prefix for rows the generator adds (first match wins). Existing rows keep
# theirs; O11 re-triages from here.
OWNERS = [
    ("feature_init", "ND1"),
    ("feature_config_args", "ND1"),
    ("feature_logging", "ND1"),
    ("feature_shutdown", "ND1"),
    ("feature_abortnode", "CH1-node"),
    ("feature_addrman", "P2a-net"),
    ("feature_anchors", "P2a-net"),
    ("feature_asset_locks", "EV"),
    ("feature_llmq", "LQ"),
    ("feature_index", "IX"),
    ("feature_txindex", "IX"),
    ("feature_blockfilterindex", "IX"),
    ("feature_coinstatsindex", "IX"),
    ("feature_assumevalid", "CH3a"),
    ("feature_block", "CH1-node"),
    ("feature_cltv", "CH3a"),
    ("feature_csv_activation", "CH3a"),
    ("feature_", "O11"),
    ("p2p_v2", "P2a-core"),
    ("p2p_", "P2a-core"),
    ("mempool_", "MP"),
    ("rpc_", "RP0"),
    ("wallet_", "WL"),
    ("interface_", "RP0"),
    ("mining_", "MN"),
]


def owner_for(path):
    name = Path(path).name
    return next((o for p, o in OWNERS if name.startswith(p)), "O11")


def escape(text):
    out = text.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")
    # The list is one row per line: no other line break may stay raw.
    if any(c in out for c in "\r\x0b\x0c\x1c\x1d\x1e\x85\u2028\u2029"):
        sys.exit(f"a fragment holds a line break the list cannot escape: {text!r}")
    return out


def literal(node):
    """(kind, text) for a literal fragment node, else None."""
    if isinstance(node, ast.Constant) and isinstance(node.value, (str, bytes)):
        value = node.value
        return "literal", value.decode() if isinstance(value, bytes) else value
    if isinstance(node, ast.JoinedStr):
        parts = []
        for v in node.values:
            if isinstance(v, ast.Constant):
                parts.append(v.value)
            else:
                parts.append("{" + ast.unparse(v.value) + "}")
        return "fstring", "".join(parts)
    # "... %s ..." % args and "... {} ...".format(args): the same placeholders an f-string has.
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Mod):
        template = node.left
        if isinstance(template, ast.Constant) and isinstance(template.value, str):
            args = node.right.elts if isinstance(node.right, ast.Tuple) else [node.right]
            tokens = re.split(r"(%%|%[-+ #0-9.]*[sdirfxXeEgGc])", template.value)
            if any("%" in text for text in tokens[0::2]):
                return None  # `%(name)s` and the like
            parts = [tokens[0]]
            for spec, text in zip(tokens[1::2], tokens[2::2]):
                if spec == "%%":
                    parts[-1] += "%" + text
                else:
                    parts.append(text)
            if len(parts) == len(args) + 1:
                return "fstring", interleave(parts, args)
    if (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and node.func.attr == "format" and isinstance(node.func.value, ast.Constant)
            and isinstance(node.func.value.value, str) and not node.keywords):
        parts = [""]
        try:
            for text, field, _, _ in string.Formatter().parse(node.func.value.value):
                parts[-1] += text
                if field is not None:
                    if field:
                        return None  # `{0}`, `{name}`: not followed
                    parts.append("")
        except ValueError:
            return None
        if len(parts) == len(node.args) + 1:
            return "fstring", interleave(parts, node.args)
    return None


def interleave(parts, args):
    out = parts[0]
    for arg, part in zip(args, parts[1:]):
        out += "{" + ast.unparse(arg) + "}" + part
    return out


class Scope:
    """The names a function (or the module, a lambda, a comprehension) binds, each to its
    bindings: ("value", node, scope), ("added", node, scope) (`.append` and the like), ("each",
    iterable, scope), ("column", iterable, n, width, loop, scope), ("param", function, index,
    method, offset, scope) (`offset`: 1 when the call's receiver is the first parameter; `scope`:
    where the defaults are), or ("none", node) for a binding the resolver does not follow.
    `funcs` holds the functions it defines; `calls` (shared by the file's scopes) every call
    with its scope."""

    def __init__(self, parent):
        self.parent, self.names, self.funcs = parent, {}, {}
        self.calls = parent.calls if parent else []

    def find(self, table, name):
        scope = self
        while scope is not None:
            if name in getattr(scope, table):
                return getattr(scope, table)[name], scope
            scope = scope.parent
        return None, None


class Collector(ast.NodeVisitor):
    """Fills a scope per function from one file. Methods see the module's names, not their
    class's (Python's rule). What changes a name without binding it in the scope it happens in
    (`.append`, `global`, a walrus in a comprehension) lands on the scope that owns the name once
    the file is read (`finish`): an addition in the owner itself is followed, any other change
    makes the name unfollowed there."""

    def __init__(self, module):
        self.module, self.scope, self.scopes, self.classes = module, module, {}, set()
        self.comprehensions, self.deferred = set(), []

    def finish(self):
        for start, name, how, local in self.deferred:
            _, owner = start.find("names", name)
            owner = owner or self.module
            owner.names.setdefault(name, []).append(how if owner is local else ("none", how[1]))

    def bind(self, name, how):
        self.scope.names.setdefault(name, []).append(how)

    def unfollowed(self, target):
        for n in ast.walk(target):
            if isinstance(n, ast.Name):
                self.bind(n.id, ("none", n))

    def nested(self, node, names, children):
        """A lambda's or comprehension's own scope, binding `names` unfollowed."""
        child = Scope(self.scope)
        self.scopes[node] = child
        if not isinstance(node, ast.Lambda):
            self.comprehensions.add(child)
        outer, self.scope = self.scope, child
        for n in names:
            self.unfollowed(n)
        for c in children:
            self.visit(c)
        self.scope = outer

    def visit_FunctionDef(self, node):
        in_class = self.scope in self.classes
        # Defaults are evaluated where the `def` is (a method's in the class body); names in a
        # method's body resolve past the class body.
        defaults = self.scope
        child = Scope(self.scope.parent if in_class else self.scope)
        self.scopes[node] = child
        if not in_class:
            self.scope.funcs[node.name] = (node, child)
        static = any(isinstance(d, ast.Name) and d.id == "staticmethod"
                     for d in node.decorator_list)
        offset = 1 if in_class and not static else 0
        a = node.args
        for n, arg in enumerate(a.posonlyargs + a.args):
            child.names.setdefault(arg.arg, []).append(
                ("param", node, n, in_class, offset, defaults))
        for arg in a.kwonlyargs + [a.vararg, a.kwarg]:
            if arg is not None:
                child.names.setdefault(arg.arg, []).append(("none", arg))
        outer, self.scope = self.scope, child
        for statement in node.body:
            self.visit(statement)
        self.scope = outer

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_ClassDef(self, node):
        outer, self.scope = self.scope, Scope(self.scope)
        self.classes.add(self.scope)
        for statement in node.body:
            self.visit(statement)
        self.scope = outer

    def visit_Lambda(self, node):
        a = node.args
        params = [ast.Name(id=x.arg, lineno=node.lineno)
                  for x in a.posonlyargs + a.args + a.kwonlyargs + [a.vararg, a.kwarg] if x]
        self.nested(node, params, a.defaults + a.kw_defaults + [node.body])

    def visit_ListComp(self, node):
        parts = [c for g in node.generators for c in (g.iter, *g.ifs)]
        parts += [node.key, node.value] if isinstance(node, ast.DictComp) else [node.elt]
        self.nested(node, [g.target for g in node.generators], parts)

    visit_SetComp = visit_GeneratorExp = visit_DictComp = visit_ListComp

    def visit_Call(self, node):
        self.scope.calls.append((node, self.scope))
        f = node.func
        if isinstance(f, ast.Attribute) and (f.attr in MUTATORS or f.attr in ADDERS):
            receiver = f.value
            while isinstance(receiver, (ast.Attribute, ast.Subscript)):
                receiver = receiver.value
            if isinstance(receiver, ast.Name):
                adds = f.attr in ADDERS and node.args and receiver is f.value
                how = ("added", node.args[-1], self.scope) if adds else ("none", node)
                self.deferred.append((self.scope, receiver.id, how, self.scope))
        self.generic_visit(node)

    def visit_Delete(self, node):
        for target in node.targets:
            self.unfollowed(target)
            while isinstance(target, (ast.Attribute, ast.Subscript)):
                target = target.value
            if isinstance(target, ast.Name):
                self.deferred.append((self.scope, target.id, ("none", node), None))

    def visit_Import(self, node):
        for alias in node.names:
            self.bind((alias.asname or alias.name).split(".")[0], ("none", node))

    visit_ImportFrom = visit_Import

    def visit_Global(self, node):
        start = self.module if isinstance(node, ast.Global) else self.scope.parent
        for name in node.names:
            self.bind(name, ("none", node))
            self.deferred.append((start, name, ("none", node), None))

    visit_Nonlocal = visit_Global

    def visit_MatchAs(self, node):
        if node.name:
            self.bind(node.name, ("none", node))
        self.generic_visit(node)

    visit_MatchStar = visit_MatchAs

    def visit_MatchMapping(self, node):
        if node.rest:
            self.bind(node.rest, ("none", node))
        self.generic_visit(node)

    def visit_Assign(self, node):
        for target in node.targets:
            if isinstance(target, ast.Name):
                self.bind(target.id, ("value", node.value, self.scope))
            else:
                self.unfollowed(target)
        self.generic_visit(node)

    def visit_AnnAssign(self, node):
        if node.value is not None and isinstance(node.target, ast.Name):
            self.bind(node.target.id, ("value", node.value, self.scope))
        self.generic_visit(node)

    def visit_AugAssign(self, node):
        self.unfollowed(node.target)
        self.generic_visit(node)

    def visit_For(self, node):
        target = node.target
        if isinstance(target, ast.Name):
            self.bind(target.id, ("each", node.iter, self.scope))
        elif isinstance(target, ast.Tuple):
            for n, t in enumerate(target.elts):
                if isinstance(t, ast.Name):
                    self.bind(t.id, ("column", node.iter, n, len(target.elts), node, self.scope))
                else:
                    self.unfollowed(t)
        else:
            self.unfollowed(target)
        self.generic_visit(node)

    visit_AsyncFor = visit_For

    def visit_withitem(self, node):
        if node.optional_vars is not None:
            self.unfollowed(node.optional_vars)
        self.generic_visit(node)

    def visit_ExceptHandler(self, node):
        if node.name:
            self.bind(node.name, ("none", node))
        self.generic_visit(node)

    def visit_NamedExpr(self, node):
        if self.scope in self.comprehensions:
            # It binds in the enclosing function: not followed there.
            owner = self.scope
            while owner in self.comprehensions:
                owner = owner.parent
            owner.names.setdefault(node.target.id, []).append(("none", node))
            self.bind(node.target.id, ("none", node))
        else:
            self.bind(node.target.id, ("value", node.value, self.scope))
        self.generic_visit(node)


# Methods that add to a list in place (followed on a name of the scope that calls them), and
# those that change a list (or set, dict) otherwise.
ADDERS = {"append", "insert", "extend"}
MUTATORS = {"pop", "remove", "clear", "sort", "reverse", "update", "setdefault", "popitem",
            "discard", "add", "__setitem__", "__delitem__", "__iadd__"}
# Of the file being read: its calls (`Scope.calls`), for parameter resolution; its names and
# attributes not called, for functions passed as values; each node's parent, for the
# tuple-`for` rule.
FILE_CALLS = []
FILE_REFS = []
FILE_PARENTS = []


def returns(func):
    """The `return` statements of a function, not of the functions inside it."""
    out, todo = [], list(func.body)
    while todo:
        node = todo.pop()
        if isinstance(node, ast.Return):
            out.append(node)
        elif not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda,
                                   ast.ClassDef)):
            todo.extend(ast.iter_child_nodes(node))
    return out


def resolve(node, scope, active=frozenset()):
    """Every (kind, text) `node` can be in `scope`, or None when it cannot be followed."""
    found = literal(node)
    if found:
        return [found]
    if id(node) in active:
        return None
    active = active | {id(node)}
    if isinstance(node, (ast.List, ast.Tuple)):
        return union(resolve(e, scope, active) for e in node.elts)
    if isinstance(node, ast.IfExp):
        return union(resolve(b, scope, active) for b in (node.body, node.orelse))
    if isinstance(node, ast.Subscript):
        if isinstance(node.slice, ast.Slice):
            return None
        elements = members(node.value, scope, active)
        return None if elements is None else union(resolve(e, s, active) for e, s in elements)
    if isinstance(node, ast.Name):
        bindings, _ = scope.find("names", node.id)
        if not bindings:
            return None
        return union(bound(how, active) for how in bindings)
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and not node.keywords:
        func, _ = scope.find("funcs", node.func.id)
        if func is None:
            return None
        body, inner = func
        values = [r.value for r in returns(body)]
        if not values or None in values:
            return None
        return union(resolve(v, inner, active) for v in values)
    return None


def members(node, scope, active):
    """The (element, scope) pairs of the list, tuple or dict literal `node` is (a dict's values),
    or of every one a name is bound to; None for anything else (a string, say)."""
    if isinstance(node, (ast.List, ast.Tuple)):
        return [(e, scope) for e in node.elts]
    if isinstance(node, ast.Dict):
        return [(v, scope) for v in node.values]
    if isinstance(node, ast.Name) and id(node) not in active:
        bindings, _ = scope.find("names", node.id)
        out = []
        for how in bindings or [None]:
            found = members(how[1], how[2], active | {id(node)}) if how and how[0] == "value" \
                else None
            if found is None:
                return None
            out.extend(found)
        return out
    return None


def bound(how, active):
    if how[0] == "none":
        return None
    if how[0] in ("value", "added", "each"):
        return resolve(how[1], how[2], active)
    if how[0] == "param":
        return argument(*how[1:], active)
    _, iterable, n, width, loop, scope = how
    found = rows(iterable, loop, scope)
    if found is None:
        return None
    return union(resolve(row.elts[n], s, active) if isinstance(row, (ast.List, ast.Tuple))
                 and len(row.elts) == width else None for row, s in found)


def line_of(how):
    return how[4].lineno if how[0] == "column" else how[1].lineno


def rows(iterable, loop, scope):
    """The (row, scope) pairs the tuple `for` target of `loop` walks: a literal list's elements,
    or those of the list a name is bound to. A name of the loop's own scope must be bound last
    before the loop by an `=` in the loop's block or one enclosing it; one of an enclosing scope
    (a module constant) must be bound once."""
    if isinstance(iterable, (ast.List, ast.Tuple)):
        return [(e, scope) for e in iterable.elts]
    if not isinstance(iterable, ast.Name):
        return None
    bindings, owner = scope.find("names", iterable.id)
    if not bindings:
        return None
    if owner is scope:
        how = max((h for h in bindings if line_of(h) < loop.lineno), key=line_of, default=None)
        if how is None or how[0] != "value" or not dominates(FILE_PARENTS[-1].get(how[1]), loop):
            return None
        # A binding later in a loop around this one reaches it on the next pass.
        if any(loop.lineno <= line_of(h) <= outer.end_lineno
               for outer in enclosing_loops(loop) for h in bindings
               if not (h[0] == "column" and h[4] is loop)):
            return None
    elif len(bindings) == 1:
        how = bindings[0]
    else:
        return None
    if how[0] != "value" or not isinstance(how[1], (ast.List, ast.Tuple)):
        return None
    return [(e, how[2]) for e in how[1].elts]


def enclosing_loops(node):
    """The loops around `node` in its function."""
    parents = FILE_PARENTS[-1]
    node = parents.get(node)
    while node is not None and not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        if isinstance(node, (ast.For, ast.AsyncFor, ast.While)):
            yield node
        node = parents.get(node)


def dominates(statement, node):
    """Whether `statement` comes before `node` in `node`'s block or one enclosing it."""
    parents = FILE_PARENTS[-1]
    parent = parents.get(statement)
    if not isinstance(statement, ast.stmt) or parent is None:
        return False
    block = next(v for _, v in ast.iter_fields(parent)
                 if isinstance(v, list) and any(x is statement for x in v))
    after = block[next(i for i, x in enumerate(block) if x is statement) + 1:]
    while node is not None:
        up = parents.get(node)
        if up is parent:
            return any(x is node for x in after)
        node = up
    return False


def argument(func, index, method, offset, defaults_scope, active):
    """Every value the file's calls of `func` pass its parameter `index`, or its default when a
    call leaves it out. A method's calls are those on `self` (a call of a same-named method on
    another object fails it, as does the function passed as a value); `offset` counts the
    receiver."""
    a = func.args
    params = a.posonlyargs + a.args
    name = params[index].arg
    defaults = dict(zip(params[len(params) - len(a.defaults):], a.defaults))
    position = index - offset
    if position < 0:
        return None  # `self`
    for ref in FILE_REFS[-1]:
        if (isinstance(ref, ast.Attribute) and method and ref.attr == func.name
                or isinstance(ref, ast.Name) and not method and ref.id == func.name):
            return None  # passed as a value (a callback, `functools.partial`): calls unseen
    out, calls = [], 0
    for call, scope in FILE_CALLS[-1]:
        f = call.func
        if method:
            if not (isinstance(f, ast.Attribute) and f.attr == func.name):
                continue
            if not (isinstance(f.value, ast.Name) and f.value.id == "self"):
                return None
        elif not (isinstance(f, ast.Name) and f.id == func.name):
            continue
        calls += 1
        if any(isinstance(x, ast.Starred) for x in call.args[:position + 1]):
            return None
        if position < len(call.args):
            value, at = call.args[position], scope
        else:
            value, at = next((k.value for k in call.keywords if k.arg == name), None), scope
            if value is None and any(k.arg is None for k in call.keywords):
                return None  # `**kwargs` may pass it
            if value is None:
                value, at = defaults.get(params[index]), defaults_scope
                if value is None:
                    return None
        found = resolve(value, at, active)
        if found is None:
            return None
        out.extend(f for f in found if f not in out)
    return out if calls else None


def union(parts):
    out = []
    for part in parts:
        if part is None:
            return None
        out.extend(p for p in part if p not in out)
    return out


def templates(source):
    """{"tx": [...], "block": [...]}: the reasons the invalid-transaction templates assert."""
    out = {"tx": [], "block": []}
    for cls in ast.parse(source).body:
        if not (isinstance(cls, ast.ClassDef)
                and any(isinstance(b, ast.Name) and b.id == "BadTxTemplate" for b in cls.bases)):
            continue
        attrs = {}
        for st in cls.body:
            target = st.target if isinstance(st, ast.AnnAssign) else (
                st.targets[0] if isinstance(st, ast.Assign) and len(st.targets) == 1 else None)
            if isinstance(target, ast.Name) and isinstance(st.value, ast.Constant):
                attrs[target.id] = st.value.value
        tx = attrs.get("reject_reason")
        block = attrs.get("block_reject_reason") or tx
        if tx:
            out["tx"].append(tx)
        if block and not attrs.get("valid_in_block"):
            out["block"].append(block)
    return out


def fragments(path, source, reasons, used, unresolved):
    """(sense, kind, line, text) for every asserted fragment in one test file. `reasons` is
    `templates()`; the `TEMPLATE_READS` and `EXPANSIONS` keys met go in `used`, and assertions
    neither resolved nor in `EXPANSIONS` in `unresolved`."""
    tree = ast.parse(source)
    module = Scope(None)
    collector = Collector(module)
    collector.visit(tree)
    collector.finish()
    FILE_CALLS.append(module.calls)
    called = {id(c.func) for c in ast.walk(tree) if isinstance(c, ast.Call)}
    FILE_REFS.append([n for n in ast.walk(tree)
                      if isinstance(n, (ast.Name, ast.Attribute)) and id(n) not in called])
    FILE_PARENTS.append({c: n for n in ast.walk(tree) for c in ast.iter_child_nodes(n)})
    out = []

    def add(sense, value, scope):
        read = (path, ast.get_source_segment(source, value))
        if read in TEMPLATE_READS:
            used.add(read)
            out.extend((sense, "literal", value.lineno, t) for t in reasons[TEMPLATE_READS[read]])
            return
        found = resolve(value, scope)
        expansion = (path, value.lineno, read[1])
        if found is None and expansion in EXPANSIONS:
            used.add(expansion)
            found = EXPANSIONS[expansion]
        if found is None:
            unresolved.append(f"{path}:{value.lineno}: {read[1]}")
            return
        out.extend((sense, kind, value.lineno, text) for kind, text in found)

    def refuse(node, why):
        unresolved.append(f"{path}:{node.lineno}: {why} {ast.get_source_segment(source, node)}")

    # The file's functions with a positional `reject_reason`: its index past `self`.
    readers = {}
    for d in ast.walk(tree):
        if isinstance(d, (ast.FunctionDef, ast.AsyncFunctionDef)):
            names = [a.arg for a in d.args.posonlyargs + d.args.args]
            if "reject_reason" in names:
                bound = int(names[:1] in (["self"], ["cls"]))
                readers[d.name] = names.index("reject_reason") - bound

    def visit(node, scope):
        scope = collector.scopes.get(node, scope)
        if isinstance(node, ast.Call):
            f = node.func
            name = f.attr if isinstance(f, ast.Attribute) else getattr(f, "id", None)
            # Arguments of a call whose arguments are read must be read: a spread, or a
            # `reject_reason` passed by position, is refused.
            if name in CALLS or name in readers:
                spreads = [a for a in node.args if isinstance(a, ast.Starred)]
                spreads += [k.value for k in node.keywords if k.arg is None]
                for spread in spreads:
                    refuse(spread, "spread:")
                if name in readers and len(node.args) > readers[name]:
                    refuse(node.args[readers[name]], "reject_reason by position:")
            for kw in node.keywords:
                value = kw.value
                # `reject_reason=reject_reason` passes a helper's parameter on.
                if kw.arg == "reject_reason" and not (
                        isinstance(value, ast.Constant) and value.value is None
                        or isinstance(value, ast.Name) and value.id == "reject_reason"):
                    add("expected", value, scope)
            if isinstance(node.func, ast.Attribute) and node.func.attr in CALLS:
                args = {"expected": node.args[0] if node.args else None,
                        "unexpected": node.args[1] if len(node.args) > 1 else None}
                for kw in node.keywords:
                    if kw.arg == "expected_msgs":
                        args["expected"] = kw.value
                    elif kw.arg == "unexpected_msgs":
                        args["unexpected"] = kw.value
                for sense, value in args.items():
                    if value is not None:
                        add(sense, value, scope)
        for child in ast.iter_child_nodes(node):
            visit(child, scope)

    visit(tree, module)
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and node.value in KEYWORDS:
            refuse(node, "keyword as a string:")
    # An assertion helper not called where it is named (an alias, `functools.partial`) is called
    # unseen.
    for node in ast.walk(tree):
        name = node.attr if isinstance(node, ast.Attribute) else getattr(node, "id", None)
        if name in CALLS and isinstance(node, (ast.Attribute, ast.Name)) and id(node) not in called:
            refuse(node, "passed as a value:")
    for file, name in sorted(DIRECT_READS):
        if file == path:
            found = [f for s in collector.scopes.values() if name in s.names
                     for f in resolve(ast.Name(id=name), s) or []]
            if not found:
                sys.exit(f"{file}: {name} is no longer bound to literals")
            line = next(n.lineno for n in ast.walk(tree)
                        if isinstance(n, ast.Name) and n.id == name)
            out.extend(("expected", kind, line, text) for kind, text in found)
    return out


def git(core, *args):
    return subprocess.run(["git", "-C", str(core), *args], check=True,
                          capture_output=True, text=True).stdout


def core_files(core, ref):
    names = git(core, "ls-tree", "--name-only", f"{ref}:test/functional").split()
    for name in sorted(n for n in names if n.endswith(".py")):
        path = f"test/functional/{name}"
        yield path, git(core, "show", f"{ref}:{path}")


def read_list(path):
    rows = []
    for n, line in enumerate(path.read_text().split("\n"), 1):
        if not line or line.startswith("#"):
            continue
        cells = line.split("\t")
        if not REQUIRED <= len(cells) <= len(COLUMNS):
            sys.exit(f"{path}:{n}: expected {REQUIRED} to {len(COLUMNS)} tab-separated columns")
        cells += [""] * (len(COLUMNS) - len(cells))
        if cells[0] not in STATUSES:
            sys.exit(f"{path}:{n}: status {cells[0]!r} is not one of {sorted(STATUSES)}")
        rows.append(dict(zip(COLUMNS, cells)))
    return rows


def key(row):
    return (row["source"].rsplit(":", 1)[0], row["sense"], row["fragment"])


def generate(core, old_rows):
    old = {key(r): r for r in old_rows}
    rows, seen = [], set()
    commit = git(core, "rev-parse", f"{REF}^{{commit}}").strip()
    if commit != REF_COMMIT:
        sys.exit(f"{core}: {REF} is {commit}, not the pinned {REF_COMMIT}")
    reasons, used, unresolved = templates(git(core, "show", f"{REF}:{TEMPLATES}")), set(), []
    for path, source in core_files(core, REF):
        for sense, kind, line, text in fragments(path, source, reasons, used, unresolved):
            if not text:
                continue  # `expected_msgs=[""]` asserts nothing
            row = {"status": "pending", "owner": owner_for(path), "sense": sense, "kind": kind,
                   "source": f"{path}:{line}", "fragment": escape(text), "match": "",
                   "proof": "", "until": ""}
            k = key(row)
            if k in seen:
                continue
            seen.add(k)
            if k in old:
                keep(row, old[k])
            rows.append(row)
    if unresolved:
        sys.exit("assertions the resolver cannot follow (resolve them, or add a reviewed "
                 "EXPANSIONS entry):\n  " + "\n  ".join(unresolved))
    stale = (set(TEMPLATE_READS) | set(EXPANSIONS)) - used
    if stale:
        sys.exit(f"TEMPLATE_READS or EXPANSIONS entries {REF} no longer has: {sorted(stale)}")
    # Rows from outside the functional tests stay as written.
    rows += [r for r in old_rows if not r["source"].startswith("test/functional/")]
    return rows


def keep(row, old):
    """A regenerated `row` with the KEPT columns of its `old` one."""
    row.update({c: old[c] for c in KEPT})


def render(rows):
    lines = [HEADER.rstrip("\n")]
    for r in rows:
        cells = [r[c] for c in COLUMNS]
        while len(cells) > REQUIRED and not cells[-1]:
            cells.pop()
        lines.append("\t".join(cells))
    return "\n".join(lines) + "\n"


SELFTEST = """
VERSION = 'v1'
DEFAULT = 'default-def'
from elsewhere import imported
def helper(name):
    return [f'opened {name}', f'version {VERSION}']
class T:
    DEFAULT = 'default-class'
    def one(self):
        msg = "one"
        with node.assert_debug_log([msg]): pass
        with node.assert_debug_log(helper(x)): pass
        chosen = (['if-a'] if cond else ['if-b'])
        with node.assert_debug_log(chosen): pass
        table = [[], ['indexed-a'], ['indexed-b']]
        with node.assert_debug_log(table[kind.value]): pass
        pairs = [m1, m2]
        pairs = [(m1, 'column-a'), (m2, 'column-b')]
        for request, text in pairs:
            with node.assert_debug_log(expected_msgs=[text]): pass
        for reason in ['param-a', 'param-b']:
            self.check(reason)
        wait_until(lambda: self.check('param-lambda'))
        with node.assert_debug_log([], unexpected_msgs=['never']): pass
        lines = ['listed']
        if wallet:
            lines.append(b'appended')
        lines.extend(['extended'])
        for line in lines:
            with node.busy_wait_for_debug_log([line]): pass
        with node.assert_debug_log(['100%% at %s' % x, '{} of {{n}}'.format(y)]): pass
        self.static('static-a')
        DEFAULT = 'default-caller'
        self.defaulted()
    def two(self):
        msg = "two"
        with node.assert_debug_log([msg]): pass
    def check(self, why):
        with node.assert_debug_log([why]): pass
    @staticmethod
    def static(why):
        with node.assert_debug_log([why]): pass
    def defaulted(self, why=DEFAULT):
        with node.assert_debug_log([why]): pass
    def three(self, arg):
        with node.assert_debug_log([arg]): pass
        grown = ['grown']
        grown += ['more']
        with node.assert_debug_log(grown): pass
        with node.assert_debug_log([expanded_50]): pass
        shrunk = ['s', 't']
        shrunk.remove('s')
        with node.assert_debug_log(shrunk): pass
        if c:
            rows = [(1, 'branch-a')]
        else:
            rows = [(2, 'branch-b')]
        for _, text in rows:
            with node.assert_debug_log([text]): pass
        word = 'abcdef'
        with node.assert_debug_log([word[:3], word[0]]): pass
        check = lambda m: node.assert_debug_log([m])
        with node.assert_debug_log([imported]): pass
        self.other('o')
        peer.other('not-a-log')
        self.kw(**opts)
    def other(self, why):
        with node.assert_debug_log([why]): pass
    def kw(self, why='kw-default'):
        with node.assert_debug_log([why]): pass
MODULE_MSGS = ['module-a']
MODULE_MSGS.append('module-b')
SHARED = ['shared-a']
GLOBAL_MSGS = ['g1']
def set_global():
    global GLOBAL_MSGS
    GLOBAL_MSGS = ['g2']
class U:
    def four(self):
        with node.assert_debug_log(MODULE_MSGS): pass
        SHARED.append('from-a-method')
        with node.assert_debug_log(SHARED): pass
        with node.assert_debug_log(GLOBAL_MSGS): pass
        nested = [['n-a'], ['n-b']]
        nested[0].append('n-c')
        with node.assert_debug_log(nested[i]): pass
        run(self.callback, 1)
        self.callback('cb')
        pairs = [(1, 'back-a')]
        while more:
            for _, text in pairs:
                with node.assert_debug_log([text]): pass
            pairs = [(2, 'back-b')]
        wm = 'walrus-a'
        found = [(wm := x) for x in xs]
        with node.assert_debug_log([wm]): pass
    def callback(self, why):
        with node.assert_debug_log([why]): pass
class V:
    def five(self):
        with node.assert_debug_log(*spread): pass
        with node.busy_wait_for_debug_log(['seen'], **opts): pass
        self.send(b, 'by-position')
        opts = {'reject_reason': 'keyed'}
        node.send_blocks_and_test(b, node, reject_reason='by-keyword')
        aliased = node.assert_debug_log
    def send(self, blocks, reject_reason=None):
        node.send_blocks_and_test(blocks, node, reject_reason=reject_reason)
"""


def selftest():
    """The resolver on each form it follows, and on the ones it must refuse."""
    unresolved = []
    EXPANSIONS[("t.py", 50, "[expanded_50]")] = [("literal", "expanded")]
    used = set()
    got = fragments("t.py", SELFTEST, {}, used, unresolved)
    del EXPANSIONS[("t.py", 50, "[expanded_50]")]
    assert used == {("t.py", 50, "[expanded_50]")}, used
    texts = sorted((sense, text) for sense, _, _, text in got)
    want = sorted([("expected", t) for t in [
        "one", "opened {name}", "version {VERSION}", "if-a", "if-b", "indexed-a", "indexed-b",
        "column-a", "column-b", "two", "param-a", "param-b", "param-lambda", "listed",
        "appended", "extended", "100% at {x}", "{y} of {n}", "static-a", "default-class",
        "expanded", "module-a", "module-b", "seen", "by-keyword"]]
        + [("unexpected", "never")])
    assert texts == want, texts
    # A parameter no call passes; names grown with `+=` or cut with `.remove`; a loop over a list
    # bound in two branches; a slice and an index into a string; a lambda parameter; an import;
    # a method also called on another object; a parameter a `**kwargs` may pass; a module list
    # added to from a method, or rebound under `global`; a list changed through an element; a
    # list rebound later in a loop around the walk; a walrus in a comprehension; a method passed
    # as a value; a `*` or `**` spread into a call whose arguments are read; a `reject_reason`
    # passed by position; a keyword written as a string; an assertion helper not called.
    assert unresolved == [
        "t.py:46: [arg]", "t.py:49: grown", "t.py:53: shrunk", "t.py:59: [text]",
        "t.py:61: [word[:3], word[0]]", "t.py:62: [m]", "t.py:63: [imported]", "t.py:68: [why]",
        "t.py:70: [why]", "t.py:82: SHARED", "t.py:83: GLOBAL_MSGS", "t.py:86: nested[i]",
        "t.py:92: [text]", "t.py:96: [wm]", "t.py:98: [why]", "t.py:101: spread: *spread",
        "t.py:101: *spread", "t.py:102: spread: opts",
        "t.py:103: reject_reason by position: 'by-position'",
        "t.py:104: keyword as a string: 'reject_reason'",
        "t.py:106: passed as a value: node.assert_debug_log"], unresolved
    # A linked row (DEC-209) survives a render, a read and a regeneration: its status, owner and
    # until, with its empty match and proof kept in place.
    linked = {"status": "linked", "owner": "CH1-node", "sense": "expected", "kind": "literal",
              "source": "t.py:1", "fragment": "text", "match": "", "proof": "", "until": "BL-122"}
    line = render([linked]).splitlines()[-1]
    assert line == "linked\tCH1-node\texpected\tliteral\tt.py:1\ttext\t\t\tBL-122", line
    with tempfile.TemporaryDirectory() as d:
        listed = Path(d) / "list.txt"
        listed.write_text(render([linked]))
        [read] = read_list(listed)
    assert read == linked, read
    fresh = dict(linked, status="pending", owner="ND1", until="")
    keep(fresh, read)
    assert fresh == linked, fresh
    print("selftest ok")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("mode", choices=["generate", "check", "selftest", "pin"])
    ap.add_argument("--core", type=Path, help="a Dash Core git repository")
    ap.add_argument("--list", type=Path, default=LIST)
    a = ap.parse_args()
    if a.mode == "selftest":
        return selftest()
    if a.mode == "pin":
        return print(f"CORE_TAG={REF}\nCORE_COMMIT={REF_COMMIT}")
    if a.core is None:
        ap.error("--core is required")
    old_rows = read_list(a.list) if a.list.exists() else []
    text = render(generate(a.core, old_rows))
    if a.mode == "generate":
        a.list.write_text(text)
        print(f"{a.list}: {text.count(chr(10)) - HEADER.count(chr(10))} rows")
    elif a.list.read_text() != text:
        sys.exit(f"{a.list} is out of sync with {REF}; run asserted-logs.py generate")


if __name__ == "__main__":
    main()
