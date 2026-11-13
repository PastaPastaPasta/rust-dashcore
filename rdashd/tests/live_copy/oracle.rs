//! CH5-5's reopen oracle (§6.3.2): a copy's persistent rows against a twin's, never a replay.
//! Each step returns what failed, so the damage self-tests can name the step that catches them.

use dash_primitives::BlockHash;
use dash_testkit::livecopy::{Rows, read_cf, rows};
use dashd_chainstate::evo::codec::{
    decode_credit_data, decode_index, decode_mined, decode_mnhf, decode_pool,
    decode_quorum_snapshot,
};
use serde_json::json;

use crate::common::*;
use crate::fixture::{
    Datadir, Expect, Fixture, Submission, call, codec, stop_checked, text, try_call, try_rdashd,
};

/// Step 1: C0's checks on a second copy, no marker, and the 11 fingerprints.
pub fn step1(copy: &Datadir) -> Result<Rows, String> {
    rows(&copy.datadir)
}

/// Step 2: rdashd started on the copy at the default checks serves the source's tip, every
/// block, and the source's UTXO set hash and count at that tip; then it stops cleanly, and the
/// copy's fingerprints are still `before`.
pub fn step2(copy: &Datadir, f: &Fixture, before: &Rows) -> Result<(), String> {
    let node = try_rdashd(copy, f.extra, None).map_err(|out| {
        format!("step 2: the start failed ({}): {}", out.status, read(&debug_log(&copy.datadir)))
    })?;
    let mut failures = Vec::new();
    if let Err(e) = served(&node, f) {
        failures.push(e);
    }
    if let Err(e) = stop_checked(node) {
        failures.push(e);
    }
    let after = step1(copy)?;
    if after != *before {
        failures.push(format!("the stop changed {:?}", after.differing(before)));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("step 2: {}", failures.join("; ")))
    }
}

/// Step 2's checks on the running node: its tip, every block, and the UTXO set's hash and count.
fn served(node: &RpcNode, f: &Fixture) -> Result<(), String> {
    let got = Expect::try_of(node)?;
    let mut failures = Vec::new();
    if got.tip != f.expect.tip {
        failures.push(format!("tip {} != {}", got.tip, f.expect.tip));
    }
    for (height, want) in f.hashes.iter().enumerate().take(got.height as usize + 1) {
        let hash = text(&try_call(node, "getblockhash", json!([height]))?);
        if hash != *want {
            failures.push(format!("block {height} is {hash}, not {want}"));
        }
        if let Err(e) = try_call(node, "getblock", json!([hash])) {
            failures.push(e);
        }
    }
    if got.hash_serialized_2 != f.expect.hash_serialized_2 {
        failures.push(format!(
            "hash_serialized_2 {} != {}",
            got.hash_serialized_2, f.expect.hash_serialized_2
        ));
    }
    if got.txouts != f.expect.txouts {
        failures.push(format!("txouts {} != {}", got.txouts, f.expect.txouts));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Step 3: all 11 fingerprints equal the twin's, `meta` included; the allowlist is empty.
pub fn step3(copy: &Rows, twin: &Rows) -> Result<(), String> {
    let differing = copy.differing(twin);
    if differing.is_empty() {
        Ok(())
    } else {
        Err(format!("step 3: {differing:?} differ"))
    }
}

/// Step 4: the decoded rows. Coins count `txouts`; every `block_index` and evo row decodes.
pub fn step4(copy: &Datadir, rows: &Rows, f: &Fixture) -> Result<(), String> {
    let coins = rows.cf("coins").count;
    if coins != f.expect.txouts {
        return Err(format!("step 4: {coins} coins rows, txouts {}", f.expect.txouts));
    }
    let codec = codec();
    let read = |cf: &str| read_cf(&copy.datadir, cf).map_err(|e| format!("step 4: {e}"));
    let fail = |cf: &str, key: &[u8], e: String| format!("step 4: {cf} {key:02x?}: {e}");
    for (key, value) in read("block_index")? {
        dash_chain::BlockIndexRecord::decode(&value)
            .map_err(|e| fail("block_index", &key, format!("{e:?}")))?;
    }
    for (key, value) in read("mn_diff")? {
        codec.decode_diff(&value).map_err(|e| fail("mn_diff", &key, e.to_string()))?;
    }
    for (key, value) in read("mn_snapshot")? {
        let hash = BlockHash::from(
            <[u8; 32]>::try_from(&key[..]).map_err(|_| fail("mn_snapshot", &key, "key".into()))?,
        );
        codec
            .decode_snapshot(&value, &hash)
            .map_err(|e| fail("mn_snapshot", &key, e.to_string()))?;
    }
    for (key, value) in read("credit_pool")? {
        let decoded = if key.len() == 33 {
            decode_credit_data(&value).map(drop)
        } else {
            decode_pool(&value).map(drop)
        };
        decoded.map_err(|e| fail("credit_pool", &key, e.to_string()))?;
    }
    type Decode = fn(&[u8]) -> Result<(), String>;
    let rest: [(&str, Decode); 4] = [
        ("mnhf", |v| decode_mnhf(v).map(drop).map_err(|e| e.to_string())),
        ("llmq_mined", |v| decode_mined(v).map(drop).map_err(|e| e.to_string())),
        ("llmq_mined_h", |v| decode_index(v).map(drop).map_err(|e| e.to_string())),
        ("llmq_snapshot", |v| decode_quorum_snapshot(v).map(drop).map_err(|e| e.to_string())),
    ];
    for (cf, decode) in rest {
        for (key, value) in read(cf)? {
            decode(&value).map_err(|e| fail(cf, &key, e))?;
        }
    }
    Ok(())
}

/// Step 5: the next block goes to the copy and the twin, and step 3 holds after it. The evo
/// fixture's is dashd's, with Core's CbTx roots; the plain fixture's is mined by the copy.
pub fn step5(copy: &Datadir, twin: &Datadir, f: &Fixture) -> Result<(), String> {
    let (on_copy, on_twin) = (try_rdashd(copy, f.extra, None), try_rdashd(twin, f.extra, None));
    let start = |r: Result<RpcNode, std::process::Output>, who: &str| {
        r.map_err(|out| format!("step 5: the {who} did not start: {}", stderr(&out)))
    };
    let (on_copy, on_twin) = (start(on_copy, "copy")?, start(on_twin, "twin")?);
    let next = match &f.next {
        Some(next) => {
            let answer = next.to(&on_copy);
            if answer != "null" {
                return Err(format!("step 5: the copy answered {answer} to {}", next.hash));
            }
            next.clone()
        }
        None => {
            let mined = call(&on_copy, "generatetodescriptor", json!([1, "raw(51)"]));
            Submission::block(&on_copy, &text(&mined[0]))
        }
    };
    let answer = next.to(&on_twin);
    on_copy.stop();
    on_twin.stop();
    if answer != "null" {
        return Err(format!("step 5: the twin answered {answer} to {}", next.hash));
    }
    step3(&step1(copy)?, &step1(twin)?).map_err(|e| format!("step 5: after {}: {e}", next.hash))
}

/// The whole oracle on `copy`, against a copy of the fixture's twin.
pub fn check(copy: &Datadir, f: &Fixture) -> Result<(), String> {
    let twin = f.twin.copy();
    let rows = step1(copy)?;
    step2(copy, f, &rows)?;
    step3(&rows, &step1(&twin)?)?;
    step4(copy, &rows, f)?;
    step5(copy, &twin, f)
}
