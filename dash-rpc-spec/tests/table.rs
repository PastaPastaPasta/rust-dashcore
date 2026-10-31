//! Invariants of the generated table that need no dashd.

use std::collections::BTreeMap;

use dash_rpc_spec::{
    COMMANDS, CONVERT_PARAMS, Class, Gating, command, convert_param, convert_param_by_name,
    has_convert_params, help,
};

/// `vRPCConvertParams` parsed from Core's client.cpp (tests/data, blob 260e3875 of
/// v24.0.0-rc.5), line by line as rpc_help.py's `process_mapping` reads it.
fn client_cpp_table() -> Vec<(String, usize, String, bool, bool)> {
    let text = include_str!("data/client.cpp");
    let start = text.find("static const CRPCConvertParam vRPCConvertParams[] =").unwrap();
    let end = start + text[start..].find("};").unwrap();
    let unquote = |s: &str| s.trim_matches('"').to_owned();
    let mut miner = false;
    let mut out = Vec::new();
    for line in text[start..end].lines().skip(1).map(str::trim) {
        if line == "#if ENABLE_MINER" {
            miner = true;
        } else if line.starts_with("#endif") {
            miner = false;
        } else if line.is_empty() || line == "{" || line.starts_with("//") {
            continue;
        } else if let Some(entry) = line.strip_prefix('{') {
            let entry = entry.trim_start();
            assert!(entry.starts_with('"'), "client.cpp: unparsed line {line:?}");
            let entry = &entry[..entry.rfind('}').unwrap()];
            let fields: Vec<&str> = entry.split(',').map(str::trim).collect();
            out.push((
                unquote(fields[0]),
                fields[1].parse().unwrap(),
                unquote(fields[2]),
                fields.get(3) == Some(&"true"),
                miner,
            ));
        } else {
            panic!("client.cpp: unparsed line {line:?}");
        }
    }
    out
}

#[test]
fn convert_params_equal_client_cpp() {
    let table: Vec<_> = CONVERT_PARAMS
        .iter()
        .map(|p| (p.method.to_owned(), p.index, p.name.to_owned(), p.preserve_str, p.miner_only))
        .collect();
    let client = client_cpp_table();
    assert_eq!(client.len(), 347);
    assert_eq!(table, client);
}

#[test]
fn commands_sorted_and_unique() {
    assert!(COMMANDS.windows(2).all(|w| w[0].name < w[1].name));
    for c in COMMANDS {
        assert_eq!(command(c.name).map(|x| x.name), Some(c.name));
    }
    assert!(command("nosuchcommand").is_none());
}

#[test]
fn names_and_whitelist_names() {
    for c in COMMANDS {
        assert_eq!(c.whitelist_name, c.name.replace(' ', ""), "{}", c.name);
        if let Some(sub) = c.subcommand() {
            assert!(!sub.contains(' '), "{}", c.name);
            // Dash registers the group's own help command under the bare method, except for
            // the hidden `evodb verify` and `evodb repair`.
            assert!(
                command(c.method()).is_some() || c.method() == "evodb",
                "{} has no parent command",
                c.name
            );
        }
    }
    let protx = command("protx register").unwrap();
    assert_eq!(
        (protx.method(), protx.subcommand(), protx.whitelist_name),
        ("protx", Some("register"), "protxregister")
    );
}

/// PRD §5.2 totals (289 commands: MUST 266, SHOULD 22, WON'T 1) and per-category counts, with
/// the two PRD gaps of generate.py's PRD_GAPS: v24 has no `getprioritisedtransactions` (mining,
/// MUST in the PRD) and registers `protx shared_register_prepare_evo` (evo, classified MUST).
#[test]
fn prd_classes() {
    let count = |class| COMMANDS.iter().filter(|c| c.class == class).count();
    assert_eq!(COMMANDS.len(), 289);
    assert_eq!((count(Class::Must), count(Class::Should), count(Class::Wont)), (266, 22, 1));
    let mut by_category: BTreeMap<&str, usize> = BTreeMap::new();
    for c in COMMANDS {
        *by_category.entry(c.category).or_default() += 1;
    }
    let expected = [
        ("addressindex", 5),
        ("blockchain", 36),
        ("control", 7),
        ("dash", 38),
        ("evo", 50),
        ("hidden", 32),
        ("mining", 6),
        ("network", 13),
        ("rawtransactions", 21),
        ("signer", 1),
        ("util", 8),
        ("wallet", 71),
        ("zmq", 1),
    ];
    assert_eq!(by_category.into_iter().collect::<Vec<_>>(), expected);
}

#[test]
fn hybrid_commands() {
    let hybrid: Vec<_> =
        COMMANDS.iter().filter(|c| c.gating == Gating::Hybrid).map(|c| c.name).collect();
    assert_eq!(hybrid, ["getcoinjoininfo", "protx info", "protx list"]);
    let getnewaddress = command("getnewaddress").unwrap();
    assert_eq!(getnewaddress.gating, Gating::Wallet);
    assert!(help("getnewaddress", true).starts_with("getnewaddress ( \"label\" )"));
    assert_eq!(help("getnewaddress", false), "help: unknown command: getnewaddress");
    assert_eq!(help("protx list", false), help("protx list", true));
}

/// Every conversion rule names a declared argument of its command at that position (for a
/// two-word method, position 0 is the sub-command).
#[test]
fn convert_params_name_declared_args() {
    for p in CONVERT_PARAMS {
        let c = command(p.method).unwrap_or_else(|| panic!("{}: not a command", p.method));
        let pos = p.index - usize::from(c.subcommand().is_some());
        let arg =
            c.args.get(pos).unwrap_or_else(|| panic!("{} {}: no argument", p.method, p.index));
        assert!(
            arg.aliases().any(|n| n == p.name),
            "{} {}: {} is not {}",
            p.method,
            p.index,
            arg.names,
            p.name
        );
    }
    assert!(has_convert_params("protx register"));
    assert!(!has_convert_params("getbestblockhash"));
    assert_eq!(convert_param("getblock", 1).map(|p| p.name), Some("verbosity"));
    assert_eq!(convert_param_by_name("getblock", "verbosity").map(|p| p.index), Some(1));
}

#[test]
fn help_overview_shape() {
    let overview = help("", true);
    let titles: Vec<&str> = overview.lines().filter(|l| l.starts_with("== ")).collect();
    // rpc_help.py's component list for a build with wallet, external signer and ZMQ.
    assert_eq!(
        titles,
        [
            "== Addressindex ==",
            "== Blockchain ==",
            "== Control ==",
            "== Dash ==",
            "== Evo ==",
            "== Mining ==",
            "== Network ==",
            "== Rawtransactions ==",
            "== Signer ==",
            "== Util ==",
            "== Wallet ==",
            "== Zmq ==",
        ]
    );
    assert!(!help("", false).contains("== Wallet =="));
    assert_eq!(help("foo", true), "help: unknown command: foo");
}
