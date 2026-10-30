//! Writes the EV2a targets' public seeds (`node_l1::dml`), each file one target input named by
//! the first 8 bytes of its SHA-256, to `node-fuzz/public-seeds/<target>/`:
//! - `diff_mnlist_codec`: Core's lists (`dash-dml/tests/data/codec/lists.txt`) in both
//!   encodings, and Core's canonical reader's mutation inputs (`verdicts.txt`, `canon` rows);
//! - `diff_mnlist_diff`: Core's `BuildDiff` between those lists (`diffs.txt`) in both encodings,
//!   on their source list; the diff reader's mutation inputs (`cdiff` rows) on the first list
//!   (`collide`); and edit scripts on every list;
//! - `diff_mnlist_order`: operation histories;
//! - `diff_unique_hash`: `node_l1::dml::unique_seeds`, a valid value of every property type
//!   and class `l1_dml` checks;
//! - `diff_dml_apply` (EV2b): each crafted case of `node_l1::dml_apply` with one operation of
//!   each kind, and with a short operation script; one input per shared-masternode path
//!   (`node_l1::dml_apply::SHARE_PATHS`), found by a bounded search; and share updates and a
//!   spend of the derived case's shared masternode (`shared-case.json`, EV2b D23);
//! - `diff_dml_block` (EV2b D54): checked sequences (`node_l1::dml_apply::checked`) of each
//!   case (the crafted ones and testnet's) with one operation of each kind, and with operation
//!   scripts as they are, with V24 forced on and with unchecked coins (`diff_dml_apply`'s seeds
//!   are the target's too); and (D56) one probe transaction of each kind for each step a probe
//!   can take (`FLAG_PROBE`), where Core's check loop rejects it at a step the node implements;
//! - `mnlist_payee_queue` (EV2b): EV2 §6.2's payee fixtures (`node_l1::dml_payee::fixtures`) and
//!   operation scripts in every era.
//!
//! Edit scripts and histories are splitmix64 bytes from fixed seeds. The evodb list encoding
//! is Core's write-back of each list (the oracle's `mnlist_canonical`), so the example links
//! the oracle.
//!
//! `cargo run -p node-l1 --features oracle --example dml_seeds`

use std::collections::BTreeMap;

use bitcoin_hashes::sha256;
use dash_dml::specialtx::reasons::BAD_SHARED_COLLATERAL_SPEND;
use dash_oracle_sys::{Chain, ListEncoding, Oracle, Pin};
use node_l1::dml_apply::checked::{FLAG_CHECKED, FLAG_PROBE};
use node_l1::dml_apply::{
    FLAG_FORCE_V24, FLAG_UNCHECKED_COINS, PROUPSHARE, PROUPSHAREDREG, SHARED_PROREG, splitmix_bytes,
};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../dash-dml/tests/data/codec");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn records(file: &str) -> Vec<Vec<String>> {
    std::fs::read_to_string(format!("{DATA}/{file}"))
        .expect(file)
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split(' ').map(str::to_string).collect())
        .collect()
}

/// The shared-masternode paths (`SHARE_PATHS`) a `diff_dml_apply` input's block could reach,
/// from its transactions alone: every path needs a shared registration, then the update or
/// spend the path names.
fn reachable(bytes: &[u8]) -> Vec<&'static str> {
    use dash_dml::specialtx::reasons::{
        BAD_PROUPSHARE_PAYEE_REUSE, BAD_PROUPSHAREDREG_PAYEE_REUSE, BAD_SHARED_COLLATERAL_SPEND,
    };
    use dash_primitives::SpecialPayload;
    use node_l1::dml_apply::{
        PROUPSHARE, PROUPSHAREDREG, SHARE_OWNER_DUP_KEY, SHARED_PROREG, SHAREDREG_DUP_KEY,
    };
    let Some(input) = node_l1::dml_apply::generate(bytes) else {
        return vec![];
    };
    let mut out = Vec::new();
    let mut shared = false;
    for tx in input.txs() {
        match tx.payload().and_then(Result::ok).map(|d| &d.payload) {
            Some(SpecialPayload::ProviderRegister(p)) if !p.shares.is_empty() => {
                shared = true;
                out.extend([SHARED_PROREG, SHARE_OWNER_DUP_KEY]);
            }
            Some(SpecialPayload::ProviderUpdateShare(_)) if shared => {
                out.extend([PROUPSHARE, BAD_PROUPSHARE_PAYEE_REUSE]);
            }
            Some(SpecialPayload::ProviderUpdateSharedRegistrar(_)) if shared => {
                out.extend([PROUPSHAREDREG, BAD_PROUPSHAREDREG_PAYEE_REUSE, SHAREDREG_DUP_KEY]);
            }
            _ if shared && !tx.inputs().is_empty() => out.push(BAD_SHARED_COLLATERAL_SPEND),
            _ => {}
        }
    }
    out
}

fn main() -> std::io::Result<()> {
    let mut seeds = BTreeMap::<&str, Vec<Vec<u8>>>::new();
    let oracle = Oracle::new(Pin::V24_0);
    let chain = Chain::main();

    let lists = records("lists.txt");
    let names: Vec<&str> = lists.iter().map(|f| f[1].as_str()).collect();
    for f in &lists {
        let list = unhex(&f[2]);
        let core = oracle.mnlist_canonical(&chain, ListEncoding::Canonical, &list).expect(&f[1]);
        seeds.entry("diff_mnlist_codec").or_default().push([&[0][..], &list].concat());
        seeds.entry("diff_mnlist_codec").or_default().push([&[1][..], &core.disk].concat());
    }
    for f in records("verdicts.txt") {
        let input = unhex(&f[1]);
        match f[0].as_str() {
            "canon" => {
                seeds.entry("diff_mnlist_codec").or_default().push([&[0][..], &input].concat())
            }
            _ => seeds.entry("diff_mnlist_diff").or_default().push([&[1, 0][..], &input].concat()),
        }
    }
    for f in records("diffs.txt") {
        let from = f[1].split('>').next().expect("from>to");
        let pick = names.iter().position(|n| *n == from).expect("a listed list") as u8;
        for (mode, diff) in [(1, &f[2]), (2, &f[3])] {
            seeds
                .entry("diff_mnlist_diff")
                .or_default()
                .push([&[mode, pick][..], &unhex(diff)].concat());
        }
    }
    for pick in 0..names.len() as u8 {
        for script in 0..4 {
            let edits = splitmix_bytes(0xd1ff_0000 + u64::from(pick) * 16 + script, 32);
            seeds.entry("diff_mnlist_diff").or_default().push([&[0, pick][..], &edits].concat());
        }
    }
    for history in 0..16 {
        seeds
            .entry("diff_mnlist_order")
            .or_default()
            .push(splitmix_bytes(0x0e72_a000 + history, 48));
    }

    // diff_dml_apply: each crafted case with one operation of each kind (arguments 0), as it is
    // and with V24 forced on (flags 2, EV2b D22), and with short operation scripts.
    let apply = seeds.entry("diff_dml_apply").or_default();
    for case in 0..node_l1::dml_apply::cases().len() as u8 {
        for kind in 0..10 {
            apply.push(vec![case, 0, kind]);
            apply.push(vec![case, FLAG_FORCE_V24, kind]);
        }
        let script = splitmix_bytes(0x0e2b_5000 + u64::from(case), 24);
        apply.push([&[case, 1][..], &script].concat());
    }

    // diff_dml_apply: one input per shared-masternode path (EV2b D22), the first splitmix64
    // input on a pre-V24 case with V24 forced on whose build reaches it; Core must agree.
    // Building is costly, so only inputs whose block could reach a path not yet found are built.
    let pre_v24: Vec<u8> = (0..node_l1::dml_apply::cases().len() as u8)
        .filter(|&i| !node_l1::dml_apply::cases()[usize::from(i)].v24)
        .collect();
    let mut found = BTreeMap::new();
    for attempt in 0..2_000_000u64 {
        if found.len() == node_l1::dml_apply::SHARE_PATHS.len() {
            break;
        }
        if attempt % 50_000 == 0 {
            eprintln!("share search: {attempt} inputs, {:?}", found.keys());
        }
        let mut input = splitmix_bytes(0x0e2b_7000_0000 + attempt, 8 + (attempt % 41) as usize);
        input[0] = pre_v24[usize::from(input[0]) % pre_v24.len()];
        input[1] = FLAG_FORCE_V24;
        if !reachable(&input).iter().any(|p| !found.contains_key(p)) {
            continue;
        }
        for path in node_l1::dml_apply::share_paths(&input) {
            found.entry(path).or_insert_with(|| input.clone());
        }
    }
    for path in node_l1::dml_apply::SHARE_PATHS {
        let input = found.get(path).unwrap_or_else(|| panic!("no input reaches {path}"));
        node_l1::dml_apply::compare_apply(input, Pin::V24_0)
            .unwrap_or_else(|d| panic!("{path}: {d}"));
        apply.push(input.clone());
    }

    // diff_dml_apply: a share update, a shared registrar update and a collateral spend of the
    // shared masternode the derived case's list already holds (EV2b D23), with no shared
    // registration in the block; Core must agree.
    let derived = node_l1::dml_apply::cases()
        .iter()
        .position(|c| c.prior.is_some())
        .expect("the derived shared case") as u8;
    let held_paths = [PROUPSHARE, PROUPSHAREDREG, BAD_SHARED_COLLATERAL_SPEND];
    let mut held = BTreeMap::new();
    for attempt in 0..200_000u64 {
        if held.len() == held_paths.len() {
            break;
        }
        let mut input = splitmix_bytes(0x0e2b_9000_0000 + attempt, 8 + (attempt % 25) as usize);
        input[0] = derived;
        input[1] = 0;
        input[2] = [8, 9, 4][(attempt % 3) as usize];
        if reachable(&input).contains(&SHARED_PROREG) {
            continue;
        }
        for path in node_l1::dml_apply::share_paths(&input) {
            if held_paths.contains(&path) {
                held.entry(path).or_insert_with(|| input.clone());
            }
        }
    }
    for path in held_paths {
        let input =
            held.get(path).unwrap_or_else(|| panic!("no derived-case input reaches {path}"));
        node_l1::dml_apply::compare_apply(input, Pin::V24_0)
            .unwrap_or_else(|d| panic!("derived {path}: {d}"));
        apply.push(input.clone());
    }

    // diff_dml_block: checked sequences of every case (testnet's is one past the crafted ones):
    // one operation of each kind, and 160-byte scripts (a few blocks) under each flag.
    let block = seeds.entry("diff_dml_block").or_default();
    for case in 0..=node_l1::dml_apply::cases().len() as u8 {
        for kind in 0..11 {
            block.push(vec![case, FLAG_CHECKED, kind]);
        }
        for (n, flags) in [0, FLAG_FORCE_V24, FLAG_UNCHECKED_COINS].into_iter().enumerate() {
            let script = splitmix_bytes(0x0e2b_5400 + u64::from(case) * 4 + n as u64, 160);
            block.push([&[case, FLAG_CHECKED | flags][..], &script].concat());
        }
        // D56: a probe (`Walk::make_probe`'s step and arm, its byte a multiple of 4) on each
        // operation kind it changes.
        let probes: [(u8, u8, &[u8]); 16] = [
            (0, 4, &[0, 4, 5, 6]),
            (1, 2, &[0, 7]),
            (2, 1, &[0, 4, 5, 6]),
            (3, 1, &[0, 5]),
            (4, 1, &[0]),
            (5, 1, &[6]),
            (6, 1, &[0, 4]),
            (7, 1, &[4, 5, 6]),
            (8, 1, &[0]),
            (9, 1, &[6]),
            (10, 1, &[6]),
            (11, 1, &[0]),
            (12, 1, &[4]),
            (13, 1, &[4]),
            (14, 1, &[0, 4]),
            (15, 1, &[0]),
        ];
        for (step, arms, kinds) in probes {
            for arm in 0..arms {
                for &kind in kinds {
                    block.push(vec![case, FLAG_CHECKED | FLAG_PROBE, 4 * (step + 16 * arm), kind]);
                }
            }
        }
        // Step 13's second arm, an invalid extended entry, on both kinds carrying addresses:
        // under V24, with odd draws after the kind (version 3, so extended addresses).
        for kind in [0, 4] {
            let probe = [case, FLAG_CHECKED | FLAG_PROBE | FLAG_FORCE_V24, 4 * (13 + 16), kind];
            block.push([&probe[..], &[1; 8]].concat());
        }
        // D57 (Opus-h r1): a registrar update to a masternode of shares (step 7's second arm),
        // and under V24 a registrar update down to version 1 (step 9) and a version-2 service
        // update to an Evo (step 12's second arm).
        block.push(vec![case, FLAG_CHECKED | FLAG_PROBE, 4 * (7 + 16), 5]);
        for (step, kind) in [(9, 5), (12 + 16, 4)] {
            block.push(vec![case, FLAG_CHECKED | FLAG_PROBE | FLAG_FORCE_V24, 4 * step, kind]);
        }
    }

    // mnlist_payee_queue: the design fixtures, and 12-operation scripts in every era.
    let payee = seeds.entry("mnlist_payee_queue").or_default();
    payee.extend(node_l1::dml_payee::fixtures().into_iter().map(|(_, input)| input));
    for era in 0..6u8 {
        for script in 0..4u64 {
            let ops = splitmix_bytes(0x0e2b_6000 + u64::from(era) * 8 + script, 2 + 6 * 12);
            payee.push([&[era][..], &ops].concat());
        }
    }

    seeds
        .entry("diff_unique_hash")
        .or_default()
        .extend(node_l1::dml::unique_seeds().into_iter().map(|(_, input)| input));

    let out = format!("{}/../node-fuzz/public-seeds", env!("CARGO_MANIFEST_DIR"));
    for (target, inputs) in seeds {
        let dir = format!("{out}/{target}");
        std::fs::create_dir_all(&dir)?;
        let mut written = std::collections::BTreeSet::new();
        for input in inputs {
            let digest = sha256::Hash::hash(&input).to_byte_array();
            let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            std::fs::write(format!("{dir}/{name}"), &input)?;
            written.insert(name);
        }
        eprintln!("{target}: {} seeds", written.len());
    }
    Ok(())
}
