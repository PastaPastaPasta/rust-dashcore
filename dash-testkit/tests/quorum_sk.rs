//! `dash_testkit::keys::recover_quorum_sk` and `testkit-keys quorum-sk` (BL-875 §8.5).

use std::io::Write;
use std::process::{Command, Output, Stdio};

use dash_pkc::bls::{BlsPkBytes, BlsScIetf, BlsSecretKey, BlsShareId, BlsSkBytes};
use dash_testkit::keys::{QuorumSkError, derive_basic_bls_key, recover_quorum_sk};
use dashcore_hashes::sha256;
use getrandom::SysRng;
use rand_core::UnwrapErr;
use serde_json::{Value, json};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Four proTxHash-like ids, as `BlsShareId`s in Core's stored order.
fn protx_ids(tag: &str) -> Vec<BlsShareId> {
    (0..4)
        .map(|i| {
            let rpc_hex = sha256::Hash::hash(format!("{tag} protx {i}").as_bytes()).to_string();
            BlsShareId::from_hex(&rpc_hex).unwrap()
        })
        .collect()
}

/// A quorum key split 3-of-4 over `ids` with `BlsSecretKey::split`: (sk, pk, shares).
fn split_quorum(
    seed: u8,
    ids: &[BlsShareId],
) -> (BlsSkBytes, BlsPkBytes, Vec<(BlsShareId, BlsSkBytes)>) {
    let key = derive_basic_bls_key(&[seed; 32], 0);
    let sk = BlsSecretKey::<BlsScIetf>::try_from(key.sk.clone()).unwrap();
    let shares = sk
        .split(3, ids, &mut UnwrapErr(SysRng))
        .unwrap()
        .into_iter()
        .map(|share| (*share.id(), BlsSkBytes::from(share.secret_key().clone())))
        .collect();
    (key.sk, key.pk, shares)
}

/// The request `quorum-sk` reads, with each id as Core's RPC prints a proTxHash.
fn request(pk: &str, threshold: usize, shares: &[(BlsShareId, BlsSkBytes)]) -> String {
    let shares: Vec<Value> = shares
        .iter()
        .map(|(id, sk)| json!({"id": id.to_string(), "sk": hex(sk.as_bytes())}))
        .collect();
    json!({"pk": pk, "threshold": threshold, "shares": shares}).to_string()
}

fn quorum_sk(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_testkit-keys"))
        .arg("quorum-sk")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A refused request may exit before reading stdin.
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    child.wait_with_output().unwrap()
}

fn unhex<const N: usize>(s: &str) -> [u8; N] {
    assert_eq!(s.len(), 2 * N, "{s}");
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}

/// `hex` with its bytes in the opposite order.
fn reversed_hex(hex: &str) -> String {
    hex.as_bytes().rchunks(2).map(|pair| std::str::from_utf8(pair).unwrap()).collect()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Every share and key of `shares`, `sk` and `pk` stays out of the output's stderr.
fn assert_no_key_material(out: &Output, sk: &BlsSkBytes, shares: &[(BlsShareId, BlsSkBytes)]) {
    let err = stderr(out);
    assert!(!err.contains(&hex(sk.as_bytes())), "{out:?}");
    for (_, share) in shares {
        assert!(!err.contains(&hex(share.as_bytes())), "{out:?}");
    }
}

/// The 3-subsets of four shares, then all four.
fn subsets<T: Clone>(shares: &[T]) -> Vec<Vec<T>> {
    let mut out: Vec<Vec<T>> = (0..shares.len())
        .map(|drop| {
            shares.iter().enumerate().filter(|(i, _)| *i != drop).map(|(_, s)| s.clone()).collect()
        })
        .collect();
    out.push(shares.to_vec());
    out
}

#[test]
fn split_key_recovers_from_every_three_subset_and_matches_pk() {
    let (sk, pk, shares) = split_quorum(0x51, &protx_ids("every-subset"));
    let master = BlsSecretKey::<BlsScIetf>::try_from(sk.clone()).unwrap();
    assert_eq!(BlsPkBytes::from(master.public_key()), pk);
    for subset in subsets(&shares) {
        assert_eq!(recover_quorum_sk(&pk, &subset, 3).unwrap(), sk);
        let out = quorum_sk(&[], &request(&pk.to_string(), 3, &subset));
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(printed, json!({"sk": hex(sk.as_bytes())}));
    }
}

/// Ids given in stored order, where Core's RPC prints them reversed, name other participants.
#[test]
fn ids_in_stored_order_give_a_pk_mismatch_and_exit_1() {
    let (sk, pk, shares) = split_quorum(0x52, &protx_ids("stored-order"));
    for (id, _) in &shares {
        // `from_hex` and `Display` use the RPC order; `as_bytes` holds the stored order.
        assert_eq!(hex(id.as_bytes()), reversed_hex(&id.to_string()));
    }
    let reversed: Vec<_> = shares
        .iter()
        .map(|(id, share)| (BlsShareId::from_hex(&hex(id.as_bytes())).unwrap(), share.clone()))
        .collect();
    assert_eq!(recover_quorum_sk(&pk, &reversed[..3], 3), Err(QuorumSkError::PublicKeyMismatch));
    let out = quorum_sk(&[], &request(&pk.to_string(), 3, &reversed[..3]));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(stderr(&out).contains("sk·G != quorum pk"), "{out:?}");
    assert_no_key_material(&out, &sk, &shares);
}

/// Core v24.0.0's dashbls threshold example (test-vectors/evo-driver): ten shares whose ids are
/// in RPC order recover the key of Core's threshold public key.
#[test]
fn core_threshold_vector_recovers_through_the_cli() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/evo-driver/core-v24.json");
    let vectors: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let v = &vectors["dashbls_legacy_threshold"];
    // The legacy G1 form flags y's sign with 0x80; the basic form sets 0x80 and flags it with 0x20.
    let legacy = v["threshold_public_key_legacy"].as_str().unwrap();
    let mut pk: [u8; 48] = unhex(legacy);
    pk[0] = (pk[0] & 0x1f)
        | 0x80
        | if pk[0] & 0x80 != 0 {
            0x20
        } else {
            0
        };
    let shares: Vec<Value> = v["ids"]
        .as_array()
        .unwrap()
        .iter()
        .zip(v["secret_key_shares"].as_array().unwrap())
        .map(|(id, sk)| json!({"id": id, "sk": sk}))
        .collect();
    let req =
        |shares: &[Value]| json!({"pk": hex(&pk), "threshold": 6, "shares": shares}).to_string();

    let out = quorum_sk(&[], &req(&shares));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    let sk_hex = printed["sk"].as_str().unwrap();
    let sk = BlsSecretKey::<BlsScIetf>::try_from(BlsSkBytes::from(unhex(sk_hex))).unwrap();
    assert_eq!(hex(BlsPkBytes::from(sk.public_key()).as_bytes()), hex(&pk));

    let stored_order: Vec<Value> = shares
        .iter()
        .map(|s| json!({"id": reversed_hex(s["id"].as_str().unwrap()), "sk": s["sk"]}))
        .collect();
    let out = quorum_sk(&[], &req(&stored_order));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
}

/// One share of another quorum, in each position: the subsets disagree or the key misses `pk`.
#[test]
fn shares_of_two_quorums_exit_1() {
    let ids = protx_ids("two-quorums");
    let (sk, pk, ours) = split_quorum(0x53, &ids);
    let (_, _, theirs) = split_quorum(0x54, &ids);
    let mut disagreed = 0;
    for j in 0..ids.len() {
        let mut mixed = ours.clone();
        mixed[j] = theirs[j].clone();
        let err = recover_quorum_sk(&pk, &mixed, 3).unwrap_err();
        assert!(
            matches!(err, QuorumSkError::SubsetsDisagree | QuorumSkError::PublicKeyMismatch),
            "{err:?}"
        );
        disagreed += usize::from(err == QuorumSkError::SubsetsDisagree);
        let out = quorum_sk(&[], &request(&pk.to_string(), 3, &mixed));
        assert_eq!(out.status.code(), Some(1), "share {j}: {out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        assert_no_key_material(&out, &sk, &mixed);
    }
    // When the foreign share sorts last the first subset is clean and recovers `sk`, so only the
    // subsets' equality catches it.
    assert!(disagreed >= 1);
}

#[test]
fn recover_refuses_bad_inputs() {
    let ids = protx_ids("bad-inputs");
    let (_, pk, shares) = split_quorum(0x55, &ids);
    assert_eq!(recover_quorum_sk(&pk, &shares, 1), Err(QuorumSkError::ThresholdBelowTwo));
    assert_eq!(
        recover_quorum_sk(&pk, &shares[..2], 3),
        Err(QuorumSkError::TooFewShares {
            shares: 2,
            threshold: 3
        })
    );
    let duplicated = [shares[0].clone(), shares[1].clone(), shares[2].clone(), shares[0].clone()];
    assert_eq!(recover_quorum_sk(&pk, &duplicated, 3), Err(QuorumSkError::DuplicateId));
    assert_eq!(
        recover_quorum_sk(&BlsPkBytes::from_bytes([0; 48]), &shares, 3),
        Err(QuorumSkError::InvalidPublicKey)
    );
    let mut invalid = shares.clone();
    invalid[2].1 = BlsSkBytes::from([0xff; 32]);
    assert_eq!(recover_quorum_sk(&pk, &invalid, 3), Err(QuorumSkError::InvalidShare(2)));
}

#[test]
fn cli_refuses_malformed_requests_without_quoting_them() {
    let (sk, pk, shares) = split_quorum(0x56, &protx_ids("malformed"));
    let good = request(&pk.to_string(), 3, &shares);
    let share = hex(shares[0].1.as_bytes());
    let bad = [
        String::new(),
        good.replace(&share, &format!("{share}00")),
        good.replace(&share, &share[..62]),
        good.replace(r#""threshold":3"#, r#""threshold":-3"#),
        good.replace(r#""threshold":3"#, r#""threshold":"3""#),
        good.replace(&share, &format!("{}\\n", &share[..62])),
        good.replacen('{', r#"{"extra":1,"#, 1),
        good.replacen('{', &format!(r#"{{"pk":"{pk}","#), 1),
        format!("{good} {good}"),
        "[".repeat(100_000),
        format!("{good}{}", " ".repeat(1 << 20)),
    ];
    for (i, input) in bad.iter().enumerate() {
        let out = quorum_sk(&[], input);
        assert_eq!(out.status.code(), Some(2), "{i}: {out:?}");
        assert!(out.stdout.is_empty(), "{i}");
        assert!(!stderr(&out).contains(&share[..62]), "{i}: {out:?}");
        assert_no_key_material(&out, &sk, &shares);
    }
    let out = quorum_sk(&["--shares", &share], &good);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(!stderr(&out).contains(&share), "{out:?}");
}

#[cfg(unix)]
#[test]
fn cli_input_file_needs_an_owner_only_regular_file() {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let (sk, pk, shares) = split_quorum(0x57, &protx_ids("input-file"));
    let good = request(&pk.to_string(), 3, &shares);
    // Removed on drop, so a failed assertion leaves nothing behind.
    let tmp = tempfile::Builder::new()
        .prefix("testkit-keys-quorum-sk-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let dir = tmp.path();
    let file = |name: &str, mode: u32| {
        let path = dir.join(name);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode & 0o700)
            .open(&path)
            .unwrap();
        f.write_all(good.as_bytes()).unwrap();
        f.set_permissions(std::fs::Permissions::from_mode(mode)).unwrap();
        path.to_str().unwrap().to_owned()
    };
    let (ok, group, other) = (file("ok", 0o600), file("group", 0o640), file("other", 0o604));

    let out = quorum_sk(&["--input-file", &ok], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed, json!({"sk": hex(sk.as_bytes())}));
    for refused in [&group, &other, dir.to_str().unwrap()] {
        let out = quorum_sk(&["--input-file", refused], "");
        assert_eq!(out.status.code(), Some(2), "{refused}: {out:?}");
        assert!(out.stdout.is_empty(), "{refused}");
        assert!(stderr(&out).contains("mode 0600 or stricter"), "{refused}: {out:?}");
        assert_no_key_material(&out, &sk, &shares);
    }

    // The same rule on stdin redirected from a file; a pipe still passes.
    let stdin_file = |path: &str| {
        Command::new(env!("CARGO_BIN_EXE_testkit-keys"))
            .arg("quorum-sk")
            .stdin(std::fs::File::open(path).unwrap())
            .output()
            .unwrap()
    };
    assert_eq!(stdin_file(&ok).status.code(), Some(0));
    let world = file("world", 0o644);
    for refused in [&world, &group, &other] {
        let out = stdin_file(refused);
        assert_eq!(out.status.code(), Some(2), "{refused}: {out:?}");
        assert!(out.stdout.is_empty(), "{refused}");
        assert!(stderr(&out).contains("stdin must be a regular file with mode 0600"), "{out:?}");
        assert_no_key_material(&out, &sk, &shares);
    }
    let out = quorum_sk(&[], &good);
    assert_eq!(out.status.code(), Some(0), "pipe: {out:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        json!({"sk": hex(sk.as_bytes())})
    );
}
