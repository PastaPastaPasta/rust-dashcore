//! The dashd-backed XCLI rows: both CLIs against live v24 regtest nodes.
//!
//! Fixture (all on private 0700 datadirs under `$TMPDIR`, free ports, frozen `-mocktime` so no
//! time-based node state moves while a row runs):
//!
//! * template: wallets `""` and `w1`, ~220 blocks, a transaction between them; stopped, then
//!   copied into every node below
//! * `a`: user/password, both wallets, one outbound (manual) and one inbound peer, some addrman
//!   entries; every read-only row runs here
//! * `p1`: cookie auth, no wallet; the cookie rows
//! * `d`: `-disablewallet`, started *after* the CLIs, for `-rpcwait`
//! * `m1`/`m2`: byte-identical copies; a mutating row runs `rdash-cli` on `m1` and `dash-cli` on
//!   `m2`, and the two must stay identical

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::support::node::{Auth, Node, copy_dir};
use crate::support::{Case, Invocation, Matrix, Tools, run, run_until};

struct Fixture<'a> {
    t: &'a Tools,
    home: &'a Path,
    m: &'a Matrix,
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Replace the digits after each `key` with `N`.
fn mask_digits(b: &[u8], key: &str) -> Vec<u8> {
    let s = String::from_utf8_lossy(b);
    let mut out = String::with_capacity(s.len());
    let mut rest: &str = &s;
    while let Some(i) = rest.find(key) {
        let (head, tail) = rest.split_at(i + key.len());
        out.push_str(head);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        out.push('N');
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out.into_bytes()
}

/// `getrpcinfo`: microseconds the running request has taken.
fn mask_duration(b: &[u8]) -> Vec<u8> {
    mask_digits(b, "\"duration\": ")
}

/// `getnettotals`: the node's clock in milliseconds when it answered.
fn mask_timemillis(b: &[u8]) -> Vec<u8> {
    mask_digits(b, "\"timemillis\": ")
}

impl Fixture<'_> {
    fn args(&self, node: &Node, more: &[&str]) -> Vec<String> {
        let mut a = node.cli_args();
        a.extend(strs(more));
        a
    }

    fn row(&self, node: &Node, group: &'static str, name: &str, more: &[&str], code: i32) {
        self.m.compare(
            self.t,
            self.home,
            &Case::new(group, name, Invocation::new(self.args(node, more)), code),
        );
    }

    fn row_stdin(
        &self,
        node: &Node,
        group: &'static str,
        name: &str,
        more: &[&str],
        stdin: &[u8],
        code: i32,
    ) {
        let inv = Invocation::new(self.args(node, more)).stdin(stdin);
        self.m.compare(self.t, self.home, &Case::new(group, name, inv, code));
    }

    fn volatile(&self, node: &Node, group: &'static str, name: &str, more: &[&str], code: i32) {
        let case = Case::new(group, name, Invocation::new(self.args(node, more)), code).volatile();
        self.m.compare(self.t, self.home, &case);
    }

    /// `rdash-cli` on `ours`, the stock `dash-cli` on `theirs`, two nodes in the same state.
    #[allow(clippy::too_many_arguments)] // the row is its description
    fn mirror(
        &self,
        ours: &Node,
        theirs: &Node,
        group: &'static str,
        name: &str,
        more: &[&str],
        stdin: &[u8],
        code: i32,
    ) {
        let a = run(&self.t.rdash, self.home, &Invocation::new(self.args(ours, more)).stdin(stdin));
        let b =
            run(&self.t.stock, self.home, &Invocation::new(self.args(theirs, more)).stdin(stdin));
        let mut problems = String::new();
        if b.code != Some(code) {
            problems.push_str(&format!(
                "premise failed: stock exit code {:?}, expected {code}\n{}",
                b.code,
                crate::support::describe("stock", &b)
            ));
        }
        // Ports and datadirs appear in no output of these rows; compare as they are.
        if a != b {
            problems.push_str(&format!("outputs differ\n  rdash: {a:?}\n  stock: {b:?}\n"));
        }
        // The nodes must still be the same node afterwards.
        let (ha, hb) = (ours.rpc(&["getbestblockhash"]), theirs.rpc(&["getbestblockhash"]));
        if ha != hb {
            problems.push_str(&format!("chains diverged: {ha} vs {hb}\n"));
        }
        self.m.record_named(group, name.to_string(), problems.is_empty(), problems);
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn mocktime_arg(secs: u64) -> String {
    format!("-mocktime={secs}")
}

pub fn run_all(t: &Tools, m: &Matrix) {
    let root = tempfile::tempdir().unwrap();
    let home = crate::support::home_dir();
    let base = now();
    let mt = mocktime_arg(base);
    let fx = Fixture {
        t,
        home: home.path(),
        m,
    };

    // --- template -------------------------------------------------------------------------
    let mut template = Node::new(
        t,
        root.path(),
        "template",
        Auth::UserPass("u", "p"),
        &[&mocktime_arg(base - 3000), "-persistmempool=0"],
    );
    template.start();
    template.rpc(&["createwallet", "", "false", "false", "", "false", "true", "true"]);
    template.rpc(&["createwallet", "w1", "false", "false", "", "false", "true", "true"]);
    template.rpc_wallet("", &["-generate", "110"]);
    template.rpc_wallet("w1", &["-generate", "105"]);
    let w1_addr = template.rpc_wallet("w1", &["getnewaddress"]);
    template.rpc_wallet("", &["sendtoaddress", &w1_addr, "7.25"]);
    template.rpc_wallet("w1", &["-generate", "2"]);
    template.stop();

    let copy = |name: &str, auth: Auth, extra: &[&str]| -> Node {
        let mut n = Node::new(t, root.path(), name, auth, extra);
        copy_dir(&template.datadir, &n.datadir);
        let _ = std::fs::remove_file(n.datadir.join(".lock"));
        n.name = name.to_string();
        n
    };
    let mut a = copy("a", Auth::UserPass("u", "p"), &[&mt, "-persistmempool=0"]);
    let mut p1 = copy("p1", Auth::Cookie, &[&mt, "-persistmempool=0", "-disablewallet"]);
    let mut p2 =
        copy("p2", Auth::UserPass("u", "p"), &[&mt, "-persistmempool=0", "-disablewallet"]);
    let mut d = copy("d", Auth::UserPass("u", "p"), &[&mt, "-persistmempool=0", "-disablewallet"]);
    let mut m1 = copy("m1", Auth::UserPass("u", "p"), &[&mt, "-persistmempool=0"]);
    let mut m2 = copy("m2", Auth::UserPass("u", "p"), &[&mt, "-persistmempool=0"]);

    a.start();
    p1.start();
    p2.start();
    m1.start();
    m2.start();

    // Peers for `a`: p1 is an outbound manual connection, p2 connects in.
    a.rpc(&["addnode", &format!("127.0.0.1:{}", p1.p2pport), "onetry"]);
    p2.rpc(&["addnode", &format!("127.0.0.1:{}", a.p2pport), "onetry"]);
    wait_for("a's two peers", Duration::from_secs(120), || a.rpc(&["getconnectioncount"]) == "2");
    a.rpc(&["addpeeraddress", "1.2.3.4", "9999"]);
    a.rpc(&["addpeeraddress", "5.6.7.8", "19999"]);
    a.rpc(&["addpeeraddress", "[2001:db8::2]", "9999"]);

    let hash5 = a.rpc(&["getblockhash", "5"]);
    let tip = a.rpc(&["getbestblockhash"]);
    let w1_addr = a.rpc_wallet("w1", &["getnewaddress"]);
    // An encrypted wallet for -stdinwalletpassphrase.
    a.rpc(&["createwallet", "enc", "false", "false", "secret phrase", "false", "true", "false"]);

    auth_rows(&fx, &a, &p1);
    rpc_rows(&fx, &a, &hash5, &tip, &w1_addr);
    stdin_rows(&fx, &a);
    getinfo_rows(&fx, &a, &p1);
    netinfo_rows(&fx, &a, &p1);
    addrinfo_rows(&fx, &a, &p1);
    rpcwait_rows(&fx, &mut d);
    generate_rows(&fx, &m1, &m2);

    for mut n in [m2, m1, d, p2, p1, a] {
        n.stop();
    }
}

fn auth_rows(fx: &Fixture, a: &Node, p1: &Node) {
    let g = "auth";
    fx.row(a, g, "user and password", &["getblockcount"], 0);
    fx.row(p1, g, "cookie from the datadir", &["getblockcount"], 0);
    let cookie = p1.datadir.join("regtest").join(".cookie");
    let cookie = cookie.display().to_string();
    let mut bare = vec!["-regtest".to_string(), format!("-rpcport={}", p1.rpcport)];
    bare.push(format!("-rpccookiefile={cookie}"));
    bare.push("getblockcount".into());
    fx.m.compare(fx.t, fx.home, &Case::new(g, "-rpccookiefile", Invocation::new(bare), 0));
    fx.row(a, g, "wrong password", &["-rpcpassword=wrong", "getblockcount"], 1);
    fx.row(a, g, "wrong user", &["-rpcuser=nobody", "getblockcount"], 1);
    fx.row(a, g, "empty password", &["-rpcpassword=", "getblockcount"], 1);
    fx.row(
        p1,
        g,
        "cookie node, wrong password",
        &["-rpcuser=u", "-rpcpassword=x", "getblockcount"],
        1,
    );
    // The datadir has no cookie and no user is given.
    let empty = fx.home.join("no-cookie");
    std::fs::create_dir_all(&empty).unwrap();
    let args = vec![
        "-regtest".to_string(),
        format!("-datadir={}", empty.display()),
        format!("-rpcport={}", a.rpcport),
        "getblockcount".to_string(),
    ];
    let case = Case::new(g, "no credentials found", Invocation::new(args), 1);
    // The message names the cookie path under the (identical) datadir, so it needs no masking.
    fx.m.compare(fx.t, fx.home, &case);
    fx.row_stdin(
        a,
        g,
        "-stdinrpcpass right",
        &["-rpcpassword=ignored", "-stdinrpcpass", "getblockcount"],
        b"p\n",
        0,
    );
    fx.row_stdin(a, g, "-stdinrpcpass wrong", &["-stdinrpcpass", "getblockcount"], b"nope\n", 1);
    fx.row_stdin(a, g, "-stdinrpcpass empty input", &["-stdinrpcpass", "getblockcount"], b"", 1);
}

/// A broad sample of read-only commands, positional and `-named`, successes and failures.
fn rpc_rows(fx: &Fixture, a: &Node, hash5: &str, tip: &str, w1_addr: &str) {
    let g = "rpc";
    let w1 = "-rpcwallet=w1";
    let blank = "-rpcwallet=";
    let quoted_hash5 = format!("'{hash5}'");
    let by_hash5 = format!("blockhash={hash5}");
    let by_tip = format!("blockhash={tip}");
    let by_addr = format!("address={w1_addr}");
    let by_outputs = format!("outputs={{\"{w1_addr}\":1}}");
    let positional: Vec<(&str, Vec<&str>, i32)> = vec![
        ("getblockcount", vec!["getblockcount"], 0),
        ("getbestblockhash", vec!["getbestblockhash"], 0),
        ("getblockhash 5", vec!["getblockhash", "5"], 0),
        ("getblockhash out of range", vec!["getblockhash", "99999"], 8),
        ("getblockhash negative", vec!["getblockhash", "-1"], 8),
        ("getblockhash not a number", vec!["getblockhash", "abc"], 1),
        ("getblockhash missing", vec!["getblockhash"], 1),
        ("getblockhash too many", vec!["getblockhash", "1", "2"], 1),
        ("getblock verbosity 0", vec!["getblock", hash5, "0"], 0),
        ("getblock verbosity 1", vec!["getblock", hash5], 0),
        ("getblock verbosity 2", vec!["getblock", hash5, "2"], 0),
        ("getblock verbosity true", vec!["getblock", hash5, "true"], 0),
        ("getblock verbosity 9", vec!["getblock", hash5, "9"], 0),
        ("getblock bad hash", vec!["getblock", "zz"], 8),
        (
            "getblock unknown hash",
            vec!["getblock", "00000000000000000000000000000000000000000000000000000000000000ff"],
            5,
        ),
        ("getblock quoted hash", vec!["getblock", &quoted_hash5], 8),
        ("getblockheader", vec!["getblockheader", tip], 0),
        ("getblockheader non-verbose", vec!["getblockheader", tip, "false"], 0),
        ("getblockstats", vec!["getblockstats", "5"], 0),
        ("getblockstats with stats", vec!["getblockstats", "5", "[\"height\",\"txs\"]"], 0),
        ("getblockchaininfo", vec!["getblockchaininfo"], 0),
        ("getchaintips", vec!["getchaintips"], 0),
        ("getdifficulty", vec!["getdifficulty"], 0),
        ("getmempoolinfo", vec!["getmempoolinfo"], 0),
        ("getrawmempool", vec!["getrawmempool"], 0),
        ("getrawmempool verbose", vec!["getrawmempool", "true"], 0),
        ("getnetworkinfo", vec!["getnetworkinfo"], 0),
        ("getconnectioncount", vec!["getconnectioncount"], 0),
        ("getmininginfo", vec!["getmininginfo"], 0),
        ("getnettotals", vec!["getnettotals"], 0),
        ("getmemoryinfo", vec!["getmemoryinfo"], 0),
        ("listbanned", vec!["listbanned"], 0),
        ("uptime", vec!["uptime"], 0),
        ("help overview", vec!["help"], 0),
        ("help getblock", vec!["help", "getblock"], 0),
        ("help two words", vec!["help", "protx", "info"], 0),
        ("help unknown", vec!["help", "nonsense"], 0),
        ("estimatesmartfee", vec!["estimatesmartfee", "6"], 0),
        ("estimaterawfee", vec!["estimaterawfee", "6"], 0),
        ("getmasternodecount alias path", vec!["masternode", "count"], 0),
        ("mnsync status", vec!["mnsync", "status"], 0),
        ("protx list", vec!["protx", "list"], 0),
        ("protx list wallet", vec!["protx", "list", "wallet"], 1),
        ("protx diff", vec!["protx", "diff", "1", "5"], 0),
        ("quorum list", vec!["quorum", "list"], 0),
        ("spork show", vec!["spork", "show"], 0),
        ("getgovernanceinfo", vec!["getgovernanceinfo"], 0),
        ("gobject count", vec!["gobject", "count"], 0),
        ("validateaddress good", vec!["validateaddress", w1_addr], 0),
        ("validateaddress bad", vec!["validateaddress", "nonsense"], 0),
        ("getaddressinfo", vec![w1, "getaddressinfo", w1_addr], 0),
        (
            "getdescriptorinfo",
            vec![
                "getdescriptorinfo",
                "pkh(02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5)",
            ],
            0,
        ),
        (
            "createrawtransaction",
            vec![
                "createrawtransaction",
                "[{\"txid\":\"0000000000000000000000000000000000000000000000000000000000000001\",\"vout\":0}]",
                "{\"yLkUmVnfpWK6wpderZKKkzsCJjdjYJkpZa\":1.5}",
            ],
            0,
        ),
        ("createrawtransaction bad", vec!["createrawtransaction", "[]", "{\"x\":1}"], 5),
        ("decoderawtransaction", vec!["decoderawtransaction", "0100000000000000000000"], 22),
        (
            "getrawtransaction unknown",
            vec![
                "getrawtransaction",
                "0000000000000000000000000000000000000000000000000000000000000001",
            ],
            5,
        ),
        ("gettxoutsetinfo", vec!["gettxoutsetinfo"], 0),
        (
            "gettxout",
            vec![
                "gettxout",
                "0000000000000000000000000000000000000000000000000000000000000001",
                "0",
            ],
            0,
        ),
        ("testmempoolaccept", vec!["testmempoolaccept", "[\"00\"]"], 22),
        (
            "signmessagewithprivkey",
            vec![
                "signmessagewithprivkey",
                "cVjFZ2uQLJhuTEzvbPuU4fmptfxnqkAs9fCQYUPmhkQMhFvUFmyB",
                "hello world",
            ],
            5,
        ),
        ("verifymessage", vec!["verifymessage", w1_addr, "AAAA", "x"], 0),
        ("getwalletinfo default wallet", vec![blank, "getwalletinfo"], 0),
        ("getwalletinfo w1", vec![w1, "getwalletinfo"], 0),
        ("getwalletinfo no wallet specified", vec!["getwalletinfo"], 19),
        ("getbalance", vec![w1, "getbalance"], 0),
        ("getbalances", vec![w1, "getbalances"], 0),
        ("listunspent", vec![w1, "listunspent"], 0),
        ("listunspent min conf", vec![w1, "listunspent", "1", "9999999", "[]", "true"], 0),
        ("listtransactions", vec![w1, "listtransactions", "*", "5"], 0),
        ("listwallets", vec!["listwallets"], 0),
        ("listwalletdir", vec!["listwalletdir"], 0),
        ("listlabels", vec![w1, "listlabels"], 0),
        ("listaddressgroupings", vec![w1, "listaddressgroupings"], 0),
        ("getaddressesbylabel", vec![w1, "getaddressesbylabel", "nonexistent"], 11),
        ("getreceivedbyaddress", vec![w1, "getreceivedbyaddress", w1_addr], 0),
        (
            "gettransaction unknown",
            vec![
                w1,
                "gettransaction",
                "0000000000000000000000000000000000000000000000000000000000000001",
            ],
            5,
        ),
        ("unknown method", vec!["nonsense"], 89),
        ("loadwallet missing", vec!["loadwallet", "does-not-exist"], 18),
        ("unknown method with args", vec!["nonsense", "1", "2"], 89),
        ("empty method", vec![""], 89),
        ("method with spaces", vec!["get block count"], 89),
        ("method uppercase", vec!["GETBLOCKCOUNT"], 89),
        ("too many args for getblockcount", vec!["getblockcount", "1"], 1),
        ("wrong type: string for number", vec!["getblockstats", "\"x\""], 1),
        ("wrong type: array for string", vec!["getblock", "[1]"], 8),
        ("null for required", vec!["getblock", "null"], 8),
        ("logging", vec!["logging"], 0),
        ("getrpcinfo", vec!["getrpcinfo"], 0),
        ("getindexinfo", vec!["getindexinfo"], 0),
        ("getbestchainlock", vec!["getbestchainlock"], 91),
        ("setmocktime without an argument", vec!["setmocktime"], 1),
    ];
    for (name, args, code) in &positional {
        let masked: Option<(&'static str, crate::support::Normalize)> = match *name {
            "getrpcinfo" => Some((
                "class 2: the running request's duration is measured with a steady clock",
                mask_duration,
            )),
            "getnettotals" => {
                Some(("class 2: the node reports its own clock in milliseconds", mask_timemillis))
            }
            _ => None,
        };
        if let Some((reason, mask)) = masked {
            let case = Case::new(g, *name, Invocation::new(fx.args(a, args)), *code)
                .registry(reason, mask);
            fx.m.compare(fx.t, fx.home, &case);
        } else {
            fx.row(a, g, name, args, *code);
        }
    }

    // The same commands by name.
    let named: Vec<(&str, Vec<&str>, i32)> = vec![
        ("getblockhash height=", vec!["-named", "getblockhash", "height=5"], 0),
        ("getblock blockhash= verbosity=", vec!["-named", "getblock", &by_hash5, "verbosity=2"], 0),
        ("getblock verbosity first", vec!["-named", "getblock", "verbosity=0", &by_hash5], 0),
        ("getblock one named, one positional", vec!["-named", "getblock", hash5, "verbosity=0"], 0),
        ("getblock unknown name", vec!["-named", "getblock", &by_hash5, "nonsense=1"], 8),
        (
            "getblock duplicate name",
            vec!["-named", "getblock", &by_hash5, "verbosity=0", "verbosity=1"],
            0,
        ),
        ("getblock positional after named", vec!["-named", "getblock", "verbosity=0", hash5], 0),
        (
            "getblockstats by name",
            vec!["-named", "getblockstats", "hash_or_height=5", "stats=[\"height\"]"],
            0,
        ),
        (
            "getblockheader verbose=false",
            vec!["-named", "getblockheader", &by_tip, "verbose=false"],
            0,
        ),
        ("help command=", vec!["-named", "help", "command=getblock"], 0),
        (
            "estimatesmartfee conf_target=",
            vec!["-named", "estimatesmartfee", "conf_target=6", "estimate_mode=ECONOMICAL"],
            0,
        ),
        (
            "listunspent by name",
            vec![w1, "-named", "listunspent", "minconf=1", "include_unsafe=false"],
            0,
        ),
        ("listtransactions by name", vec![w1, "-named", "listtransactions", "count=3"], 0),
        ("getaddressinfo address=", vec![w1, "-named", "getaddressinfo", &by_addr], 0),
        ("validateaddress address=", vec!["-named", "validateaddress", &by_addr], 0),
        (
            "createrawtransaction by name",
            vec!["-named", "createrawtransaction", "inputs=[]", &by_outputs],
            0,
        ),
        ("protx list type=", vec!["-named", "protx", "list", "type=wallet"], 8),
        ("protx diff by name", vec!["-named", "protx", "diff", "baseBlock=1", "block=5"], 8),
        ("sub-command alias name", vec!["-named", "masternode", "count"], 8),
        ("wrong name for type", vec!["-named", "getblockhash", "height=abc"], 1),
        ("missing required", vec!["-named", "getblockhash"], 1),
        ("-named unknown method", vec!["-named", "nonsense", "a=1"], 89),
    ];
    for (name, args, code) in &named {
        fx.row(a, "named", name, args, *code);
    }
}

fn stdin_rows(fx: &Fixture, a: &Node) {
    let g = "stdin";
    let hash5 = a.rpc(&["getblockhash", "5"]);
    fx.row_stdin(
        a,
        g,
        "-stdin with method on the command line",
        &["-stdin", "getblock"],
        format!("{hash5}\n0\n").as_bytes(),
        0,
    );
    fx.row_stdin(a, g, "-stdin fills in all arguments", &["-stdin", "getblockhash"], b"7\n", 0);
    fx.row_stdin(a, g, "-stdin no trailing newline", &["-stdin", "getblockhash"], b"7", 0);
    fx.row_stdin(a, g, "-stdin CRLF", &["-stdin", "getblockhash"], b"7\r\n", 0);
    fx.row_stdin(a, g, "-stdin empty input", &["-stdin", "getblockhash"], b"", 1);
    fx.row_stdin(a, g, "-stdin extra lines", &["-stdin", "getblockhash"], b"7\n8\n", 1);
    fx.row_stdin(
        a,
        g,
        "-stdin blank line is an empty argument",
        &["-stdin", "getblockhash"],
        b"\n",
        1,
    );
    fx.row_stdin(a, g, "-stdin JSON lines", &["-stdin", "getblockstats"], b"5\n[\"height\"]\n", 0);
    fx.row_stdin(a, g, "-stdin -named", &["-stdin", "-named", "getblockhash"], b"height=5\n", 0);
    fx.row_stdin(
        a,
        g,
        "-stdin with all of it on stdin (no method)",
        &["-stdin"],
        b"getblockcount\n",
        0,
    );
    fx.row_stdin(
        a,
        g,
        "-stdin with -rpcwallet",
        &["-stdin", "-rpcwallet=w1", "getbalance"],
        b"\n",
        32,
    );
    fx.row_stdin(
        a,
        g,
        "-stdin and -stdinrpcpass",
        &["-stdin", "-stdinrpcpass", "getblockhash"],
        b"p\n5\n",
        0,
    );

    // -stdinwalletpassphrase against an encrypted wallet.
    let enc = "-rpcwallet=enc";
    fx.row_stdin(
        a,
        g,
        "-stdinwalletpassphrase right",
        &[enc, "-stdinwalletpassphrase", "walletpassphrase", "60"],
        b"secret phrase\n",
        0,
    );
    fx.row(a, g, "walletlock", &[enc, "walletlock"], 0);
    fx.row_stdin(
        a,
        g,
        "-stdinwalletpassphrase wrong",
        &[enc, "-stdinwalletpassphrase", "walletpassphrase", "60"],
        b"nope\n",
        14,
    );
    fx.row_stdin(
        a,
        g,
        "-stdinwalletpassphrase empty line",
        &[enc, "-stdinwalletpassphrase", "walletpassphrase", "60"],
        b"\n",
        8,
    );
    fx.row_stdin(
        a,
        g,
        "-stdinwalletpassphrase empty input",
        &[enc, "-stdinwalletpassphrase", "walletpassphrase", "60"],
        b"",
        1,
    );
    fx.row_stdin(
        a,
        g,
        "-stdinwalletpassphrase + -stdin",
        &[enc, "-stdinwalletpassphrase", "-stdin", "walletpassphrase"],
        b"secret phrase\n60\n",
        0,
    );
    fx.row(a, g, "walletlock again", &[enc, "walletlock"], 0);
}

fn getinfo_rows(fx: &Fixture, a: &Node, p1: &Node) {
    let g = "getinfo";
    for color in ["", "-color=always", "-color=never", "-color=auto"] {
        let mut more = vec!["-getinfo"];
        if !color.is_empty() {
            more.push(color);
        }
        let name = if color.is_empty() {
            "multi-wallet node".to_string()
        } else {
            format!("multi-wallet node {color}")
        };
        fx.row(a, g, &name, &more, 0);
    }
    fx.row(a, g, "-rpcwallet=w1", &["-getinfo", "-rpcwallet=w1"], 0);
    fx.row(a, g, "-rpcwallet=", &["-getinfo", "-rpcwallet="], 0);
    fx.row(a, g, "-rpcwallet unknown", &["-getinfo", "-rpcwallet=nonexistent"], 0);
    fx.row(a, g, "-named", &["-getinfo", "-named"], 0);
    fx.row(
        a,
        g,
        "-getinfo -color=always -rpcwallet=w1",
        &["-getinfo", "-color=always", "-rpcwallet=w1"],
        0,
    );
    fx.row(p1, g, "node without a wallet", &["-getinfo"], 0);
    fx.row(p1, g, "node without a wallet, color", &["-getinfo", "-color=always"], 0);
    fx.row(a, g, "-getinfo with an argument", &["-getinfo", "x"], 1);
    // One wallet only.
    a.rpc(&["unloadwallet", "w1"]);
    a.rpc(&["unloadwallet", "enc"]);
    fx.row(a, g, "single-wallet node", &["-getinfo"], 0);
    fx.row(a, g, "single-wallet node, color", &["-getinfo", "-color=always"], 0);
    a.rpc(&["loadwallet", "w1"]);
    a.rpc(&["loadwallet", "enc"]);
    fx.row(a, g, "multi-wallet node again", &["-getinfo"], 0);
}

fn netinfo_rows(fx: &Fixture, a: &Node, p1: &Node) {
    let g = "netinfo";
    for level in ["", "0", "1", "2", "3", "4", "5", "255"] {
        let more: Vec<&str> = if level.is_empty() {
            vec!["-netinfo"]
        } else {
            vec!["-netinfo", level]
        };
        let name = if level.is_empty() {
            "default level".to_string()
        } else {
            format!("level {level}")
        };
        fx.volatile(a, g, &name, &more, 0);
    }
    for level in ["outonly", "o", "outonlyx", "out", "OUTONLY"] {
        fx.volatile(a, g, level, &["-netinfo", level], 1);
    }
    for level in ["2", "4"] {
        fx.volatile(
            a,
            g,
            &format!("level {level} with outonly after"),
            &["-netinfo", level, "outonly"],
            0,
        );
    }
    fx.volatile(a, g, "-rpcwallet is ignored", &["-netinfo", "4", "-rpcwallet=w1"], 0);
    fx.volatile(a, g, "-named", &["-netinfo", "-named"], 0);
    fx.volatile(p1, g, "cookie node, outbound-only view", &["-netinfo", "4"], 0);
    fx.volatile(p1, g, "cookie node, level 1", &["-netinfo", "1"], 0);
    fx.volatile(a, g, "-color has no effect", &["-netinfo", "4", "-color=always"], 0);
    fx.row(a, g, "-netinfo help", &["-netinfo", "help"], 0);
    fx.row(a, g, "-netinfo 3 extra", &["-netinfo", "3", "extra"], 0);
}

fn addrinfo_rows(fx: &Fixture, a: &Node, p1: &Node) {
    let g = "addrinfo";
    fx.row(a, g, "addrman with entries", &["-addrinfo"], 0);
    fx.row(a, g, "-addrinfo -named", &["-addrinfo", "-named"], 0);
    fx.row(a, g, "-addrinfo -rpcwallet", &["-addrinfo", "-rpcwallet=w1"], 0);
    fx.row(p1, g, "addrman of a cookie node", &["-addrinfo"], 0);
}

/// The CLIs start first and wait for the node to appear.
fn rpcwait_rows(fx: &Fixture, d: &mut Node) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let g = "rpcwait";
    let (t_ref, home) = (fx.t.clone(), fx.home.to_path_buf());
    // The four wait on the node's ports as they are when it starts. A dashd that could not bind
    // moves to other ports, where the four would never find it (BL-374): they are stopped and
    // started again on the new ports.
    let mut outputs = None;
    for attempt in 1..=5 {
        let args = |extra: &[&str]| {
            let mut v = d.cli_args();
            v.extend(strs(extra));
            v
        };
        let inv_wait = Invocation::new(args(&["-rpcwait", "-rpcwaittimeout=180", "getblockcount"]));
        let inv_info = Invocation::new(args(&["-rpcwait", "-rpcwaittimeout=180", "-getinfo"]));
        let cancel = AtomicBool::new(false);
        let ran = std::thread::scope(|s| {
            let spawn = |bin: std::path::PathBuf, inv: Invocation| {
                let (home, cancel) = (home.clone(), &cancel);
                s.spawn(move || run_until(&bin, &home, &inv, cancel))
            };
            let h1 = spawn(t_ref.rdash.clone(), inv_wait.clone());
            let h2 = spawn(t_ref.stock.clone(), inv_wait.clone());
            let h3 = spawn(t_ref.rdash.clone(), inv_info.clone());
            let h4 = spawn(t_ref.stock.clone(), inv_info.clone());
            // Let all four fail to connect a few times first.
            std::thread::sleep(Duration::from_secs(4));
            // A start that panics stops the four too, rather than leave them to their deadline.
            struct CancelOnUnwind<'a>(&'a AtomicBool);
            impl Drop for CancelOnUnwind<'_> {
                fn drop(&mut self) {
                    if std::thread::panicking() {
                        self.0.store(true, Ordering::Relaxed);
                    }
                }
            }
            let guard = CancelOnUnwind(&cancel);
            let up = d.start_once(attempt < 5);
            drop(guard);
            if !up {
                cancel.store(true, Ordering::Relaxed);
            }
            let [a, b, c, e] = [h1, h2, h3, h4].map(|h| h.join().unwrap());
            // Whatever the four said when the node never came up (all of them may have ended
            // already, against another process on the old port) is not the rows' answer.
            if !up {
                return None;
            }
            Some((a?, b?, c?, e?))
        });
        if let Some(ran) = ran {
            outputs = Some(ran);
            break;
        }
        eprintln!("rpcwait rows: dashd moved to other ports; the four CLIs start again");
    }
    let (a, b, c, e) = &outputs.expect("start_once panics on its last attempt");
    for (name, ours, stock) in
        [("getblockcount, node starts late", a, b), ("-getinfo, node starts late", c, e)]
    {
        let mut problems = String::new();
        if stock.code != Some(0) {
            problems.push_str(&format!(
                "premise failed: stock exit code {:?}\n{}",
                stock.code,
                crate::support::describe("stock", stock)
            ));
        }
        if ours != stock {
            problems.push_str(&format!("outputs differ\n  rdash: {ours:?}\n  stock: {stock:?}\n"));
        }
        fx.m.record_named(g, name.to_string(), problems.is_empty(), problems);
    }
    // The node answers now, with -rpcwait given anyway; and a wallet-less node answers -getinfo.
    fx.row(d, g, "-rpcwait when the node is up", &["-rpcwait", "getblockcount"], 0);
    fx.row(
        d,
        g,
        "-rpcwaittimeout when the node is up",
        &["-rpcwait", "-rpcwaittimeout=1", "getblockcount"],
        0,
    );
    fx.row(d, g, "-getinfo on a node without a wallet", &["-getinfo"], 0);
}

/// The instant both mirror nodes are frozen at while the ban rows run; the replies carry it in
/// `ban_created` and `banned_until`. Only that both nodes get the same value matters.
const BAN_ROWS_MOCKTIME: u64 = 1_900_000_000;

/// `-generate` and other state-changing commands on two byte-identical nodes: `rdash-cli` runs on
/// `m1`, the stock `dash-cli` on `m2`, and the two must stay the same node.
fn generate_rows(fx: &Fixture, m1: &Node, m2: &Node) {
    let g = "generate";
    let both = |name: &str, more: &[&str], code: i32| fx.mirror(m1, m2, g, name, more, b"", code);
    // Two wallets loaded: the node cannot pick one, and it fails before the arguments are read.
    both("two wallets loaded: no wallet specified", &["-generate"], 19);
    both("two wallets loaded: -generate 0", &["-generate", "0"], 19);
    both("two wallets loaded: -generate abc", &["-generate", "abc"], 19);
    both("two wallets loaded: too many arguments", &["-generate", "1", "2", "3"], 19);
    both("-generate with -rpcwallet=w1", &["-rpcwallet=w1", "-generate"], 0);
    both("-generate 3 with -rpcwallet=w1", &["-rpcwallet=w1", "-generate", "3"], 0);
    both("-generate 2 100000 -rpcwallet=", &["-rpcwallet=", "-generate", "2", "100000"], 0);
    both("-generate 1 with maxtries 1", &["-rpcwallet=w1", "-generate", "1", "1"], 0);
    both("-generate -named", &["-rpcwallet=w1", "-named", "-generate", "2"], 0);
    both("-generate 0 with a wallet", &["-rpcwallet=w1", "-generate", "0"], 1);
    both("-generate abc with a wallet", &["-rpcwallet=w1", "-generate", "abc"], 1);
    both("-generate 1 2 3 with a wallet", &["-rpcwallet=w1", "-generate", "1", "2", "3"], 1);
    both("-generate to an unknown wallet", &["-rpcwallet=nonexistent", "-generate"], 18);
    // One wallet loaded: plain -generate works.
    for n in [m1, m2] {
        n.rpc(&["unloadwallet", "w1"]);
    }
    both("one wallet loaded: -generate", &["-generate"], 0);
    both("one wallet loaded: -generate 7", &["-generate", "7"], 0);
    both("one wallet loaded: -generate 1 0", &["-generate", "1", "0"], 0);
    both("one wallet loaded: -generate 00", &["-generate", "00"], 1);
    both("one wallet loaded: -generate 3.5", &["-generate", "3.5"], 0);
    // What the blocks changed.
    both("getblockcount afterwards", &["getblockcount"], 0);
    both("getbalances afterwards", &["-rpcwallet=", "getbalances"], 0);
    both("listtransactions afterwards", &["-rpcwallet=", "listtransactions", "*", "4"], 0);
    both("getnewaddress", &["-rpcwallet=", "getnewaddress"], 0);
    both("getnewaddress with label", &["-rpcwallet=", "getnewaddress", "label one"], 0);
    both("getnewaddress -named", &["-rpcwallet=", "-named", "getnewaddress", "label=two"], 0);
    both("setmocktime", &["setmocktime", "0"], 0);
    both("settxfee", &["-rpcwallet=", "settxfee", "0.0001"], 0);
    both("createwallet", &["createwallet", "fresh", "false", "true"], 0);
    both("createwallet -named", &["-named", "createwallet", "wallet_name=fresh2", "blank=true"], 0);
    both("unloadwallet", &["unloadwallet", "fresh"], 0);
    both("addnode add", &["addnode", "1.2.3.4:5", "add"], 0);
    both("addnode bad command", &["addnode", "1.2.3.4:5", "bogus"], 1);
    // `listbanned` prints `ban_created` and `time_remaining` from the node's clock, which has run
    // on the wall clock since `setmocktime 0`: the two CLIs' calls can straddle a second boundary
    // and differ by one. Freeze both nodes at the same instant for the ban rows, so the replies
    // stay compared byte for byte, and hand the wall clock back afterwards.
    for n in [m1, m2] {
        n.rpc(&["setmocktime", &BAN_ROWS_MOCKTIME.to_string()]);
    }
    both("setban", &["setban", "10.0.0.1", "add"], 0);
    both("listbanned", &["listbanned"], 0);
    both("clearbanned", &["clearbanned"], 0);
    for n in [m1, m2] {
        n.rpc(&["setmocktime", "0"]);
    }
    both("echo (plain strings)", &["echo", "a", "1", "true"], 0);
    both("echo", &["echo", "\"a\"", "1", "true", "[1]", "{\"a\":1}"], 0);
    both("echojson", &["echojson", "'\"a\"'", "1", "true", "[1]", "{\"a\":1}"], 0);
    both("echojson (not JSON)", &["echojson", "a"], 1);
    both(
        "invalidateblock unknown",
        &["invalidateblock", "00000000000000000000000000000000000000000000000000000000000000ff"],
        5,
    );

    // An encrypted wallet: the passphrase read from standard input goes after the method.
    for n in [m1, m2] {
        n.rpc(&["createwallet", "enc", "false", "false", "secret phrase"]);
    }
    let enc = "-rpcwallet=enc";
    let stdin_row = |name: &str, more: &[&str], stdin: &[u8], code: i32| {
        fx.mirror(m1, m2, g, name, more, stdin, code)
    };
    stdin_row(
        "-stdinwalletpassphrase unlocks",
        &[enc, "-stdinwalletpassphrase", "walletpassphrase", "60"],
        b"secret phrase\n",
        0,
    );
    both("walletlock", &[enc, "walletlock"], 0);
    stdin_row(
        "-stdinwalletpassphrase changes the passphrase",
        &[enc, "-stdinwalletpassphrase", "walletpassphrasechange", "new phrase"],
        b"secret phrase\n",
        0,
    );
    stdin_row(
        "the old passphrase no longer works",
        &[enc, "-stdinwalletpassphrase", "walletpassphrase", "60"],
        b"secret phrase\n",
        14,
    );
    stdin_row(
        "the new passphrase works",
        &[enc, "-stdinwalletpassphrase", "walletpassphrase", "60"],
        b"new phrase\n",
        0,
    );
    stdin_row(
        "-stdin supplies the timeout",
        &[enc, "-stdinwalletpassphrase", "-stdin", "walletpassphrase"],
        b"new phrase\n30\n",
        0,
    );
}
