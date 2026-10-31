#!/usr/bin/env python3
"""Tests of check-layers.py's backstops behind the dash-params bans (BS5-R
§2g): the Rust text reader, rules P1, P2, P4, P5 and P6, each with a hit and a
miss, the [[params.allow]] records, and P3's generated and hand-written
configs. The rules run over a throwaway Git repository, so they need no Cargo.
Run by selftest-checks.sh."""

import importlib.util
import re
import subprocess
import tempfile
import tomllib
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path

_spec = importlib.util.spec_from_file_location("check_layers", Path(__file__).with_name("check-layers.py"))
cl = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cl)

BANS = (Path(__file__).with_name("node-params-bans.toml")).read_text()
ROOT_CONF = 'msrv = "1.89"\n\ndisallowed-methods = [\n    { path = "a::b", reason = "root" },\n]\n'


def hits(rule, source):
    return [text for _, text in cl.text_hits(cl.rust_code(source), rule)]


class RustCode(unittest.TestCase):
    def test_comments_strings_and_chars_are_blanked_lines_kept(self):
        source = ('let a = "ChainParams"; // ChainParams\n'
                  '/* outer /* ChainParams */ still */ let b = r#"x "ChainParams" y"#;\n'
                  "let c = b'\"'; let d = c\"ChainParams\"; let e = br\"ChainParams\";\n"
                  "fn f<'a>(x: &'a str) -> char { 'C' } // \"\n"
                  "/// ChainParams\nlet g = ChainParams;\n")
        code = cl.rust_code(source)
        self.assertEqual(code.count("\n"), source.count("\n"))
        self.assertEqual(code.count("ChainParams"), 1)
        self.assertIn("let b =", code)
        self.assertIn("let e =", code)
        self.assertIn("fn f<'a>(x: &'a str)", code)
        self.assertEqual(list(cl.text_hits(code, "P5")), [(6, "ChainParams")])

    def test_raw_identifiers_and_prefixes_are_code(self):
        self.assertEqual(hits("P2", "let r#evo_shares = 1; let br = evo_shares;"), ["evo_shares", "evo_shares"])
        self.assertEqual([re.sub(r"\s", "", h) for h in hits("P2", "d.deployments.r#evo_shares; r#rules.evo_shares")],
                         ["deployments.evo_shares", "rules.evo_shares"])
        self.assertEqual([re.sub(r"\s", "", h) for h in hits("P6", "n.r#chain(); r#Network::r#addr(n);")],
                         [".chain()", "Network::addr"])


class TextRules(unittest.TestCase):
    def test_p1(self):
        self.assertEqual(hits("P1", "let a = Lp::LLMQ_TEST; let b = <LlmqParams>::LLMQ_TEST_DIP0024;"),
                         ["Lp::LLMQ_TEST", "::LLMQ_TEST_DIP0024"])
        self.assertEqual(hits("P1", 'let a = dash_params::MAX_LLMQ_SIZE; let b = "::LLMQ_TEST"; // ::LLMQ_TEST'),
                         [])

    def test_p2(self):
        self.assertEqual(hits("P2", "let Bip9Deployments { evo_shares, .. } = d; let h = a.bech32_hrp;"),
                         ["evo_shares", "a.bech32_hrp"])
        self.assertEqual(hits("P2", "(*p.consensus).deployments . evo_shares; f().evo_shares; Self::evo_shares"),
                         ["deployments . evo_shares", ").evo_shares", "Self::evo_shares"])
        self.assertEqual(hits("P2", 'let a = "-vbparams=evo_shares"; let b = evo_shares_active; /* bech32_hrp */'),
                         [])

    def test_p4(self):
        self.assertEqual(hits("P4", "let p = available_params(t); let v = types.map(dash_params::available_params);"
                                    "\nuse dash_params::available_params as ap;"),
                         ["available_params", "available_params", "available_params"])
        self.assertEqual(hits("P4", "use dash_params::{available_params, LlmqType};\n"
                                    "pub use dash_params::available_params;\n"
                                    '// available_params(t)\nlet s = "available_params(t)"; fn my_available_params() {}'),
                         [])

    def test_p5(self):
        self.assertEqual(hits("P5", "fn f(c: &ConsensusParams, a: AddrParams) -> &ChainParams where T: "
                                    "AsRef<Bip9Deployments> {}"),
                         ["ConsensusParams", "AddrParams", "ChainParams", "Bip9Deployments"])
        self.assertEqual(hits("P5", "ChainParamsBuilder::new(n, r); let c: &ConsensusSetup; let a: AddrSetup; "
                                    "// ChainParams"),
                         [])

    def test_p6(self):
        self.assertEqual(hits("P6", "Network::Main.chain(); Network::consensus(n); LlmqType::params(t); "
                                    "t.params (); c.consensus.llmq(t); let a = Network::addr;"),
                         [".chain()", "Network::consensus", "LlmqType::params", ".params ()", ".consensus.llmq(",
                          "Network::addr"])
        self.assertEqual(hits("P6", "use dash_params::{Network as N, LlmqType as T};\ntype Net = dash_params::Network;"
                                    "\nfn f(t: T) { N::addr(n); T::params(t); Net :: chain(n); n.addr(); }"),
                         ["N::addr", "T::params", "Net :: chain", ".addr()"])
        self.assertEqual(hits("P6", "use dash_params::{Network::{self as N, Main}, LlmqType::{self as T}};\n"
                                    "fn f(t: T) { N::addr(Main); T::params(t); }"),
                         ["N::addr", "T::params"])
        self.assertEqual(hits("P6", "use dash_params::{Network as Net, LlmqType as Type};\nuse self::Net as M;\n"
                                    "type N = Net;\ntype T = crate::Type;\n"
                                    "fn f(n: N, t: T) { N::chain(n); T::params(t); M::consensus(n); }"),
                         ["N::chain", "T::params", "M::consensus"])
        self.assertEqual(hits("P6", "use dash_params::Network::{self as First, self as N};\n"
                                    "use dash_params::LlmqType::{self as FirstType, Main, self as T};\n"
                                    "type A = dash_params :: Network;\ntype B = dash_params/* m */::LlmqType;\n"
                                    "fn f() { N::chain(n); T::params(t); A::addr(n); B::params(t); }"),
                         ["N::chain", "T::params", "A::addr", "B::params"])
        self.assertEqual(hits("P6", "let k = n as u32; u32::addr(k); type Q = Other; Q::chain(q);"), [])
        self.assertEqual(hits("P6", "s.consensus.llmq_params(); w.params(x); x.chain_id(); "
                                    "setup.consensus.deployments; // Network::chain\nN::addr(n); T::params(t); "
                                    "use dash_params::{Network, LlmqType as Type};"),
                         [])


class Repo(unittest.TestCase):
    """A throwaway repository: a node package `node-a` (generated config), a hand-written one
    `node-b`, an upstream W package naming dash-params, and a standalone package `tool`."""

    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(prefix="check-layers-test.")))
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / "contrib/node").mkdir(parents=True)
        (self.root / "clippy.toml").write_text(ROOT_CONF)
        (self.root / "contrib/node/node-params-bans.toml").write_text(BANS)
        self.layers_toml([])
        self.packages = [self.package("node-a", "N"), self.package("node-b", "Pn"), self.package("upstream", "W"),
                         self.package("plain", "N", params=False)]
        self.layer = {"node-a": "N", "node-b": "Pn", "upstream": "W", "plain": "N"}
        self.write("tool/Cargo.toml", '[package]\nname = "tool"\n\n[patch."x"]\ndash-params = { path = "p" }\n')
        self.write("node-b/clippy.toml", 'msrv = "1.89"\ndisallowed-methods = [\n' + "".join(
            f'  {{ reason = "{b["reason"]}", path = "{b["path"]}" }},\n'
            for b in tomllib.loads(BANS)["disallowed-methods"]) + "]\n")
        saved = cl.ROOT, cl.NODE
        cl.ROOT, cl.NODE = self.root, self.root / "contrib/node"
        self.addCleanup(lambda: (setattr(cl, "ROOT", saved[0]), setattr(cl, "NODE", saved[1])))
        self.assertEqual(self.check(write=True)[0], [])

    def write(self, rel, text):
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def package(self, name, layer, params=True):
        self.write(f"{name}/Cargo.toml", f'[package]\nname = "{name}"\n')
        self.write(f"{name}/src/lib.rs", "")
        return {"name": name, "manifest_path": str(self.root / name / "Cargo.toml"),
                "dependencies": [{"name": "dash-params"}] if params else []}

    def layers_toml(self, allow, standalone=("tool",)):
        records = "".join(f'\n[[params.allow]]\nrule = "{r}"\npath = "{p}"\nreason = "{why}"\n'
                          + "".join(f'match = "{m}"\n' for m in match)
                          for r, p, why, *match in allow)
        self.write("contrib/node/layers.toml", '[params]\nhand-written = ["node-b"]\nstandalone = ['
                   + ", ".join(f'"{s}"' for s in standalone) + "]\n" + records)

    def check(self, write=False):
        """The violations as `rule [root] edge` strings, and what the rules printed."""
        class Layers:
            by_name = self.layer.get
        out, printed = cl.Violations(), StringIO()
        with redirect_stdout(printed):
            cl.params_rules(out, Layers(), self.packages, write)
        return sorted(f"{r} [{root}] {edge}" for (r, root), edges in out.found.items() for edge in edges), \
            printed.getvalue()

    def test_clean_repository(self):
        self.assertEqual(self.check()[0], [])
        conf = (self.root / "node-a/clippy.toml").read_text()
        self.assertTrue(conf.startswith(cl.GENERATED))
        self.assertFalse((self.root / "upstream/clippy.toml").exists())
        self.assertFalse((self.root / "plain/clippy.toml").exists())

    def test_text_rules_in_node_and_standalone_packages(self):
        self.write("node-a/src/lib.rs", "fn f() {\n    let p = Lp::LLMQ_TEST;\n    let c = t.params();\n}\n")
        self.write("plain/tests/t.rs", "let a: AddrParams = x;\n")
        self.write("tool/src/main.rs", "fn main() { let h = Network::Main.chain().addr_params.bech32_hrp; }\n")
        self.write("upstream/src/lib.rs", "let a: ChainParams = Network::Main.chain();\n")
        self.assertEqual(self.check()[0], ["P1 [node-a] node-a/src/lib.rs:2", "P2 [tool] tool/src/main.rs:1",
                                           "P5 [plain] plain/tests/t.rs:1", "P6 [tool] tool/src/main.rs:1"])

    def test_allowlist_file_line_and_stale(self):
        self.write("node-a/src/lib.rs", "let a = available_params(t);\nlet b = available_params(u);\n")
        self.write("node-b/src/lib.rs", "let a = available_params(t);\nlet b = available_params(u);\n")
        self.layers_toml([("P4", "node-a/src/lib.rs", "a Core site"), ("P4", "node-b/src/lib.rs:2", "a Core site"),
                          ("P1", "node-a/src/lib.rs", "nothing here", "*")])
        found, printed = self.check()
        self.assertEqual(found, ["P1 [layers.toml] node-a/src/lib.rs `*`", "P4 [node-b] node-b/src/lib.rs:1"])
        self.assertIn("allowed P4 node-a/src/lib.rs (2 hits): a Core site", printed)
        self.assertIn("allowed P4 node-b/src/lib.rs:2 (1 hit): a Core site", printed)

    def test_allowlist_match(self):
        self.write("node-a/src/lib.rs", "let a = r.evo_shares;\nlet b = a.bech32_hrp;\nlet c = llmq::LLMQ_TEST;\n"
                                        "let d = LlmqParams::LLMQ_TEST;\nlet e = deployments.evo_shares;\n")
        self.layers_toml([("P2", "node-a/src/lib.rs", "a node flag", "r.evo_shares"),
                          ("P1", "node-a/src/lib.rs", "own consts", "llmq::LLMQ_*"),
                          ("P1", "node-a/src/lib.rs:4", "one line", "Other::*")])
        found, printed = self.check()
        self.assertEqual(found, ["P1 [layers.toml] node-a/src/lib.rs:4 `Other::*`", "P1 [node-a] node-a/src/lib.rs:4",
                                 "P2 [node-a] node-a/src/lib.rs:2", "P2 [node-a] node-a/src/lib.rs:5"])
        self.assertIn("allowed P2 node-a/src/lib.rs `r.evo_shares` (1 hit): a node flag", printed)
        self.assertIn("allowed P1 node-a/src/lib.rs `llmq::LLMQ_*` (1 hit): own consts", printed)

    def test_allowlist_record_shape(self):
        for bad in ([("P3", "x.rs", "configs have no allowlist")], [("P4", "x.rs:two", "a reason")],
                    [("P4", "x.rs", " ")], [("P2", "x.rs", "a whole file needs a match")],
                    [("P1", "x.rs", "a whole file needs a match")], [("P2", "x.rs", "an empty match", " ")]):
            self.layers_toml(bad)
            with self.assertRaises(SystemExit):
                self.check()

    def test_p3_generated(self):
        conf = self.root / "node-a/clippy.toml"
        conf.write_text(conf.read_text().replace("DEC-903", "DEC-0"))
        self.assertEqual(self.check()[0], ["P3 [node-a] node-a/clippy.toml"])
        conf.unlink()
        self.assertEqual(self.check()[0], ["P3 [node-a] node-a/clippy.toml"])
        self.assertEqual(self.check(write=True)[0], [])
        self.assertEqual(self.check()[0], [])

    def test_p3_generated_text(self):
        text = cl.generated_conf(ROOT_CONF, BANS)
        want = tomllib.loads(ROOT_CONF)["disallowed-methods"] + tomllib.loads(BANS)["disallowed-methods"]
        self.assertEqual(tomllib.loads(text)["disallowed-methods"], want)
        self.assertTrue(text.startswith(cl.GENERATED_HEADER + 'msrv = "1.89"\n'))
        bare = cl.generated_conf('msrv = "1.89"\n', BANS)
        self.assertEqual(tomllib.loads(bare)["disallowed-methods"], tomllib.loads(BANS)["disallowed-methods"])

    def test_p3_hand_written(self):
        conf = self.root / "node-b/clippy.toml"
        text = conf.read_text()
        conf.write_text(text.replace("DEC-903", "DEC-0", 1))
        self.assertEqual(self.check()[0], ["P3 [node-b] node-b/clippy.toml dash_params::ConsensusParams::llmq"])
        conf.write_text("\n".join(l for l in text.splitlines() if "Network::chain" not in l) + "\n")
        self.assertEqual(self.check()[0], ["P3 [node-b] node-b/clippy.toml dash_params::Network::chain"])
        conf.unlink()
        self.assertEqual(len(self.check()[0]), len(tomllib.loads(BANS)["disallowed-methods"]))

    def test_p3_package_set(self):
        self.packages.append(self.package("node-c", "S"))
        self.layer["node-c"] = "S"
        self.assertEqual(self.check()[0], ["P3 [node-c] node-c/clippy.toml"])
        self.packages[-1]["dependencies"] = []
        self.write("node-c/clippy.toml", (self.root / "node-a/clippy.toml").read_text())
        self.assertEqual(self.check()[0], ["P3 [node-c] stale"])
        self.write("contrib/node/layers.toml", (self.root / "contrib/node/layers.toml").read_text()
                   .replace('["node-b"]', '["node-b", "upstream"]'))
        (self.root / "node-c/clippy.toml").unlink()
        self.assertEqual(self.check()[0], ["P3 [upstream] hand-written"])

    def test_p6_standalone_set(self):
        self.write("other/Cargo.toml", '[package]\nname = "other"\n\n[dev-dependencies]\nparams = '
                   '{ package = "dash-params", path = "p" }\n')
        self.write("quiet/Cargo.toml", '[package]\nname = "quiet"\n')
        self.layers_toml([], standalone=("tool", "gone"))
        self.assertEqual(self.check()[0], ["P6 [gone] stale", "P6 [other] unlisted"])


if __name__ == "__main__":
    unittest.main()
