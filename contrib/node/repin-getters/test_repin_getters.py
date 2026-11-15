"""Fixtures for the tool's rules: write detection, constructor reorders, method getters.

Run from the repository root: `python3 -I -m unittest discover -s contrib/node/repin-getters -p 'test_*.py'`.
"""

import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True
sys.path.insert(0, HERE)

import bsdk_api  # noqa: E402
import repin_getters as rg  # noqa: E402

# One base-sdk-shaped package: `S` has a plain `new(a, b)`, `U` the same with `get_copy` getters,
# `C` a method named after a field that returns the field and one that does not.
BSDK_SRC = """
pub struct S {
  a: u32,
  b: u32,
}

impl S {
  pub fn new(a: u32, b: u32) -> Self {
    Self { a, b }
  }
}

pub struct U {
  #[getset(get_copy = "pub")]
  a: u32,
  #[getset(get_copy = "pub")]
  b: u32,
}

impl U {
  pub fn new(a: u32, b: u32) -> Self {
    Self { a, b }
  }
}

pub struct C {
  llmq_params: Vec<(u8, u32)>,
  n: u32,
  tag: Vec<u8>,
}

impl C {
  pub fn llmq_params(&self) -> impl Iterator<Item = (u8, &u32)> {
    self.llmq_params.iter().map(|(t, p)| (*t, p))
  }
  pub fn n(&self) -> u32 {
    self.n
  }
  pub fn tag(&self) -> &[u8] {
    &self.tag[..]
  }
}
"""


def std_library() -> str | None:
    """The toolchain's rust-src `library` directory, if installed."""
    try:
        sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True, text=True,
                                 check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None
    lib = os.path.join(sysroot, "lib", "rustlib", "src", "rust", "library")
    return lib if os.path.isdir(lib) else None


def source(text: str) -> rg.Source:
    src = rg.Source.__new__(rg.Source)
    src.path, src.text, src.orig, src._toks = "<fixture>", text, text, None
    return src


def api() -> bsdk_api.Api:
    with tempfile.TemporaryDirectory() as d:
        with open(os.path.join(d, "lib.rs"), "w") as fh:
            fh.write(BSDK_SRC)
        return bsdk_api.Api(bsdk_api.parse_package("fixture", d), {})


def context(line: str) -> str:
    """`access_context` of the first `.f` field in `line`."""
    text = f"fn t() {{ {line} }}"
    start = text.index(".f") + 1
    return rg.access_context(source(text), start, start + 1)


def literal(expr: str, name: str = "S"):
    """`plan_literal` on the `<name> { .. }` literal in `let _ = <expr>;`."""
    text = f"fn t() {{ let _ = {expr}; }}"
    src = source(text)
    toks = src.toks
    first = next(i for i, t in enumerate(toks) if t.text == name)
    brace = first + 1
    st = rg.CrateStats()
    return rg.plan_literal(src, first, brace, name, api(), st, "src/lib.rs"), st


class WriteMethods(unittest.TestCase):
    """B-1: a `&mut self` method on a private field is a write, never a getter read."""

    CLASSES = {
        "Clone": ["clone_from(&y)"],
        "Vec": ["push(1)", "insert(0, 1)", "extend(it)", "append(&mut v)", "retain(|x| *x > 0)",
                "clear()", "truncate(1)", "drain(..)", "split_off(1)", "resize(2, 0)", "dedup()",
                "dedup_by_key(|x| *x)", "splice(.., it)", "reserve(1)"],
        "VecDeque": ["push_back(1)", "push_front(1)", "pop_back()", "pop_front()"],
        "slice": ["sort()", "sort_by(cmp)", "sort_unstable_by_key(|x| *x)", "swap(0, 1)", "reverse()",
                  "fill(0)", "copy_within(.., 0)", "iter_mut()", "get_mut(0)", "first_mut()",
                  "last_mut()", "split_at_mut(1)", "as_mut()"],
        "maps": ["entry(k)", "values_mut()", "pop_first()", "remove_entry(&k)"],
        "String": ["push_str(s)", "insert_str(0, s)", "make_ascii_lowercase()"],
        "Option": ["take()", "replace(v)", "get_or_insert(v)", "get_or_insert_with(f)",
                   "get_or_insert_default()", "as_deref_mut()"],
        "base-sdk": ["reset_signature()", "set_sig(s)"],
    }

    def test_each_write_method_class_is_a_write(self):
        for cls, calls in self.CLASSES.items():
            for call in calls:
                with self.subTest(cls=cls, call=call):
                    self.assertEqual(context(f"x.f.{call};"), "write")

    def test_splice_on_a_payload_is_a_write(self):
        # Found in the wild (rdashd tests/rpc_diff.rs bls_candidate, port commit 1289fd829): with
        # `splice` missing from the list, the auto pass rewrote this line to
        # `tx.extra_payload().splice(..)` on the shared getter, which rustc refused (E0596).
        text = "fn t() { tx.extra_payload.splice(at.., parse_hex(&sig)); }"
        start = text.index("extra_payload")
        self.assertEqual(rg.access_context(source(text), start, start + len("extra_payload")), "write")

    def test_a_method_from_the_derived_std_list_is_a_write(self):
        # `Option::take_if` (stable 1.80) on a `get_copy` Option would write to a temporary copy.
        self.assertIn("take_if", rg.STD_MUT_METHODS)
        self.assertEqual(context("x.f.take_if(|v| *v > 0);"), "write")

    def test_a_mut_named_method_is_a_write(self):
        for call in ["chunks_exact_mut(2)", "split_at_mut_checked(1)", "some_future_mut()"]:
            with self.subTest(call=call):
                self.assertEqual(context(f"x.f.{call};"), "write")

    def test_the_list_covers_the_toolchains_stable_mut_methods(self):
        lib = std_library()
        if lib is None:
            self.skipTest("rust-src is not installed")
        derived = subprocess.run(
            [sys.executable, "-I", os.path.join(HERE, "derive_mut_methods.py"), lib],
            capture_output=True, text=True, check=True,
        ).stdout.split()
        self.assertGreater(len(derived), 100)
        self.assertEqual(sorted(set(derived) - rg.MUT_METHODS), [])

    def test_by_value_and_shared_methods_stay_reads(self):
        for call in ["len()", "iter()", "clone()", "is_empty()", "to_vec()", "get(0)", "as_slice()"]:
            with self.subTest(call=call):
                self.assertEqual(context(f"let _ = x.f.{call};"), "read")


class LiteralReorder(unittest.TestCase):
    """B-2: a reorder that moves an initializer with effects is a hand site."""

    def test_pure_initializers_are_reordered(self):
        e, st = literal("S { b: y.0, a: &x.k }")
        self.assertIsNotNone(e)
        self.assertEqual(e.new, "S::new(&x.k, y.0)")
        self.assertEqual(st.hand_sites, [])

    def test_impure_initializers_out_of_order_are_a_hand_site(self):
        for expr in ["S { b: r.next(), a: r.next() }", "S { b: f(), a: 1 }", "S { b: v!(), a: x }",
                     "S { b: y?, a: x }", "S { b: { 1 }, a: x }", "S { b: v[0], a: x }"]:
            with self.subTest(expr=expr):
                e, st = literal(expr)
                self.assertIsNone(e)
                self.assertEqual(len(st.hand_sites), 1)
                self.assertIn("another order", st.hand_sites[0][4])

    def test_impure_initializers_in_order_are_converted(self):
        e, _ = literal("S { a: r.next(), b: r.next() }")
        self.assertIsNotNone(e)
        self.assertEqual(e.new, "S::new(r.next(), r.next())")


    def test_a_bound_base_before_an_impure_initializer_is_a_hand_site(self):
        # `..make()` is bound to a `let` ahead of the constructor call, so `f()` would run after it.
        e, st = literal("U { a: f(), ..make() }", "U")
        self.assertIsNone(e)
        self.assertEqual(len(st.hand_sites), 1)
        self.assertIn("the base would be evaluated before", st.hand_sites[0][4])

    def test_a_bound_base_with_pure_initializers_is_converted(self):
        e, st = literal("U { a: 1, ..make() }", "U")
        self.assertIsNotNone(e)
        self.assertEqual(st.hand_sites, [])


class MethodGetters(unittest.TestCase):
    """B-6: a method named after a field is its getter only when it returns the field."""

    def test_only_a_body_returning_the_field_is_a_getter(self):
        c = next(s for s in api().structs if s.name == "C")
        self.assertIsNone(c.field("llmq_params").getter)
        self.assertEqual(c.field("n").getter, "method")
        self.assertEqual(c.field("tag").getter, "method")

    def test_a_read_through_a_non_getter_method_is_a_hand_site(self):
        text = "fn t(c: &C) { let _ = c.llmq_params; }"
        src = source(text)
        start = text.index("llmq_params")
        d = rg.Diag.__new__(rg.Diag)
        d.code, d.file, d.line, d.start, d.end = "E0616", "src/lib.rs", 1, start, start + len("llmq_params")
        d.message, d.children = "field `llmq_params` of struct `C` is private", []
        st = rg.CrateStats()
        self.assertEqual(rg.plan_access(d, src, api(), st), [])
        self.assertIn("does not return the field", st.hand_sites[0][4])


if __name__ == "__main__":
    unittest.main()
