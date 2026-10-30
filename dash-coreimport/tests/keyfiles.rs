//! IM1b: the onion and I2P key files and the `dash.conf` files Dash Core v24 wrote (see
//! `tests/data/flatfiles/v24.0.0-rc.3/PROVENANCE.md`). Unlike `flatfiles.rs`, nothing here needs
//! the `full` readers, so these run with `--no-default-features --features keys` too.

use std::path::PathBuf;

use dash_coreimport::config;
use dash_coreimport::keys::{I2pPrivateKey, OnionV3Key};
use dash_coreimport::netstate::{FormatError, NetAddr};
use dash_network::Network;
use serde_json::Value;

/// A file of the rc.3 capture.
fn data(dir: &str, file: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/flatfiles/v24.0.0-rc.3")
        .join(dir)
        .join(file);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

// ---- onion and I2P keys --------------------------------------------------------------------

fn keys_expected() -> Value {
    serde_json::from_slice(&data("keys", "expected.json")).unwrap()
}

#[test]
fn the_onion_key_core_wrote_round_trips_byte_for_byte() {
    let file = data("keys", "onion_v3_private_key");
    let key = OnionV3Key::parse(&file).unwrap().expect("a key");
    assert_eq!(key.expanded_secret().len(), 64);
    assert_eq!(key.to_file_bytes().as_bytes(), file.as_slice());
    assert_eq!(format!("{key:?}"), "OnionV3Key(<redacted>)");
    // The empty file is Core's "no key yet".
    assert!(OnionV3Key::parse(b"").unwrap().is_none());
}

#[test]
fn the_i2p_key_core_wrote_names_the_address_core_logged() {
    let file = data("keys", "i2p_private_key");
    let key = I2pPrivateKey::parse(&file).unwrap();
    let want =
        keys_expected()["i2p_private_key"]["b32_address_core_logged"].as_str().unwrap().to_owned();
    assert_eq!(key.b32_address(), want);
    assert_eq!(key.destination().len(), 391);
    assert_eq!(key.as_bytes(), file.as_slice());
    assert!(matches!(key.address(), NetAddr::I2p(_)));
    assert_eq!(format!("{key:?}"), "I2pPrivateKey(<redacted>)");
}

#[test]
fn malformed_onion_keys_are_refused() {
    let good = data("keys", "onion_v3_private_key");
    let mut cases: Vec<Vec<u8>> = vec![
        b"ED25519-V3:".to_vec(),
        b"RSA1024:AAAA".to_vec(),
        good[..good.len() - 1].to_vec(),
        [good.as_slice(), b"\n"].concat(),
        [good.as_slice(), b" Flags=Detach"].concat(),
    ];
    // The URL-safe base64 alphabet is not Core's.
    for url_safe in *b"-_" {
        let mut case = good.clone();
        case[20] = url_safe;
        cases.push(case);
    }
    // A control-port command smuggled into the base64.
    let mut injected = good.clone();
    injected[20] = b'\r';
    injected[21] = b'\n';
    cases.push(injected);
    // Non-canonical trailing bits in the last base64 group.
    let mut bits = good.clone();
    bits[good.len() - 3] = b'B';
    cases.push(bits);
    // Padding outside the last group: the fuzzer's `keys.rs` panic was a `====` group.
    for (at, pad) in
        [(11usize, &b"===="[..]), (15, b"A==="), (11, b"AA=="), (95, b"===="), (96, b"=")]
    {
        let mut case = good.clone();
        case[at..at + pad.len()].copy_from_slice(pad);
        cases.push(case);
    }
    for case in cases {
        assert!(
            OnionV3Key::parse(&case).is_err(),
            "{:?}",
            String::from_utf8_lossy(&case[..case.len().min(30)])
        );
    }
}

#[test]
fn malformed_i2p_keys_are_refused() {
    let good = data("keys", "i2p_private_key");
    assert!(I2pPrivateKey::parse(&[]).is_err());
    assert!(I2pPrivateKey::parse(&good[..385]).is_err());
    // A certificate length that runs past the end of the key.
    let mut long = good.clone();
    long[385] = 0xff;
    long[386] = 0xff;
    assert!(I2pPrivateKey::parse(&long).is_err());
    assert!(matches!(
        I2pPrivateKey::parse(&vec![0u8; dash_coreimport::keys::MAX_I2P_KEY_FILE_SIZE + 1]),
        Err(FormatError::TooLarge { .. })
    ));
    // Arbitrary short inputs never panic.
    for len in 0..good.len() {
        let _ = I2pPrivateKey::parse(&good[..len]);
    }
}

#[cfg(unix)]
#[test]
fn keys_are_written_0600_and_never_over_an_existing_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let onion = OnionV3Key::parse(&data("keys", "onion_v3_private_key")).unwrap().unwrap();
    let path = dir.path().join("onion_v3_private_key");
    onion.write(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read(&path).unwrap(), data("keys", "onion_v3_private_key"));
    // No clobber: the existing key stays.
    std::fs::write(dir.path().join("other"), b"x").unwrap();
    let err = onion.write(&path).unwrap_err();
    assert!(!err.is_installed());
    assert_eq!(err.source_error().unwrap().kind(), std::io::ErrorKind::AlreadyExists);
    // Nor through a symlink.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(dir.path().join("other"), &link).unwrap();
    assert!(onion.write(&link).is_err());
    assert_eq!(std::fs::read(dir.path().join("other")).unwrap(), b"x");

    let i2p = I2pPrivateKey::parse(&data("keys", "i2p_private_key")).unwrap();
    let path = dir.path().join("i2p_private_key");
    i2p.write(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read(&path).unwrap(), data("keys", "i2p_private_key"));
    // No temporary file is left behind.
    let mut names: Vec<_> =
        std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(names, ["i2p_private_key", "link", "onion_v3_private_key", "other"]);
    // Reading back through the reader.
    assert_eq!(I2pPrivateKey::read(&path).unwrap().b32_address(), i2p.b32_address());
    assert!(OnionV3Key::read(&dir.path().join("onion_v3_private_key")).unwrap().is_some());
}

// ---- dash.conf -----------------------------------------------------------------------------

#[test]
fn the_oracle_confs_read_with_the_values_core_would_see() {
    let mainnet =
        config::parse_dash_conf(&data("oracle-mainnet", "dash.conf"), Network::Mainnet, false)
            .unwrap();
    assert_eq!(mainnet.args().get_int_arg("-dbcache", 0).unwrap(), 12000);
    assert!(mainnet.args().get_bool_arg("-txindex", false).unwrap());
    assert!(!mainnet.args().get_bool_arg("-listenonion", true).unwrap());
    assert_eq!(mainnet.args().get_arg("-rpcport", "").unwrap(), "27998");
    assert!(mainnet.includes.is_empty());

    // The testnet file puts the P2P and RPC settings in `[test]`: only the testnet read sees them.
    let conf = data("oracle-testnet", "dash.conf");
    let testnet = config::parse_dash_conf(&conf, Network::Testnet, false).unwrap();
    assert_eq!(testnet.args().get_arg("-rpcport", "").unwrap(), "37998");
    assert_eq!(testnet.args().get_arg("-bind", "").unwrap(), "127.0.0.1:37999");
    assert_eq!(testnet.args().get_int_arg("-dbcache", 0).unwrap(), 4000);
    let main_read = config::parse_dash_conf(&conf, Network::Mainnet, false).unwrap();
    assert!(!main_read.args().is_arg_set("-rpcport"));
    assert_eq!(main_read.args().get_int_arg("-dbcache", 0).unwrap(), 4000);
    assert!(testnet.sections().contains_key("test"));
}

#[test]
fn conf_errors_follow_core() {
    let parse = |text: &str| config::parse_dash_conf(text.as_bytes(), Network::Mainnet, false);
    // An unknown option is an error unless invalid keys are ignored.
    assert!(parse("definitelynotanoption=1\n").is_err());
    let lenient =
        config::parse_dash_conf(b"definitelynotanoption=1\n", Network::Mainnet, true).unwrap();
    assert!(!lenient.args().is_arg_set("-definitelynotanoption"));
    // A section header only: no value.
    assert!(parse("[main]\nrpcport\n").is_err());
    // includeconf is reported, not followed.
    let inc = parse("includeconf=/etc/passwd\n[main]\nincludeconf=other.conf\n").unwrap();
    // Core reads the network's section first, then the top of the file.
    assert_eq!(inc.includes, ["other.conf", "/etc/passwd"]);
    // Only the values after the last `noincludeconf` of a section count, and other networks'
    // sections are not read: Core's `add_includes`.
    let text = "includeconf=a\nnoincludeconf=1\nincludeconf=b\n[test]\nincludeconf=t\n";
    let main = config::parse_dash_conf(text.as_bytes(), Network::Mainnet, false).unwrap();
    assert_eq!(main.includes, ["b"]);
    let test = config::parse_dash_conf(text.as_bytes(), Network::Testnet, false).unwrap();
    assert_eq!(test.includes, ["t", "b"]);
    // A devnet reads the `[devnet]` section whatever its name is.
    let dev = config::parse_dash_conf(
        b"[devnet]\nrpcport=1234\n[mydev]\nrpcport=1\n",
        Network::Devnet,
        false,
    );
    assert_eq!(dev.unwrap().args().get_arg("-rpcport", "").unwrap(), "1234");
    // A repeated option keeps every value in the settings, and `GetArg` serves the first of a
    // config file's values (Core's `util::GetSetting` over `ro_config`).
    let dup = parse("rpcport=1\nrpcport=2\n").unwrap();
    assert_eq!(dup.args().get_args("-rpcport").unwrap().len(), 2);
    assert_eq!(dup.args().get_arg("-rpcport", "").unwrap(), "1");
    // Not UTF-8, and over the cap.
    assert!(config::parse_dash_conf(&[0xff, 0xfe, b'a'], Network::Mainnet, false).is_err());
    let huge = vec![b'#'; config::MAX_DASH_CONF_SIZE + 1];
    assert!(matches!(
        config::parse_dash_conf(&huge, Network::Mainnet, false),
        Err(FormatError::TooLarge { .. })
    ));
}

#[test]
fn conf_reader_never_panics_on_damaged_confs() {
    let conf = data("oracle-testnet", "dash.conf");
    for len in 0..conf.len() {
        let _ = config::parse_dash_conf(&conf[..len], Network::Testnet, false);
        let _ = config::parse_dash_conf(&conf[..len], Network::Testnet, true);
    }
    for at in 0..conf.len() {
        let mut flipped = conf.clone();
        flipped[at] ^= 0x80;
        let _ = config::parse_dash_conf(&flipped, Network::Testnet, true);
    }
}
