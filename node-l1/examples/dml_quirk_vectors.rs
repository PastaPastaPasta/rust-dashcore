//! Writes `dash-dml/tests/data/ev2b-quirks.txt` (EV2b D25): blocks over crafted contexts that show
//! quirks 13, 24, 25 and 42, each with Core's result (`oracle_dml_apply`), which
//! `dash-dml/tests/quirks_ev2b.rs` replays without the oracle and `l1_dml_apply` re-checks
//! against Core.
//!
//! - 13: the crafted lists with every masternode banned (no payee), and with every masternode
//!   paid at one height and unconfirmed (the smallest proTxHash is paid).
//! - 24: an accepted QcTx whose failed members are banned in the list first: the penalty still
//!   grows; and one after a ProRegTx over a list padded to 160 masternodes, whose penalty is
//!   `max(100, total)` of the list as rebuilt so far (161, not 160).
//! - 25: an accepted ProRegTx that replaces a masternode by collateral, and an accepted block
//!   that updates one masternode of the predecessor list twice, which with only those two
//!   transactions swapped gives another result.
//! - 42: a block that throws inside the build (`failed-procspectxsinblock`) whose transactions
//!   in the other order give another result.
//!
//! Every vector is recorded only if our build agrees with Core on it.
//!
//! `cargo run -p node-l1 --features oracle --example dml_quirk_vectors`

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::primitives::decode_tx;
use dash_dml::codec::core_disk;
use dash_dml::testkit::replay::{CoreVerdict, Vector, replay};
use dash_dml::testkit::{fresh, list_disk_bytes, numbered};
use dash_dml::{DmlEvent, Dmn, MnHot, MnList, ProTxHash};
use dash_oracle_sys::{Context, MnList as CoreList, Oracle, Pin};
use dash_params::ChainSetup;
use dash_primitives::SpecialPayload;
use node_l1::dml_apply::{
    assemble, cases, chain_for, core_verdict, generate, oracle_fields, payload, replay_context,
    split_block, splitmix_bytes,
};

/// The vector of `block` over `ctx` if Core gives a result and ours agrees with it, with our
/// trace and new list. The context keeps the fewest of its last headers (1, 8, 32, 128 or all)
/// with which Core gives the same result, which keeps the file small.
fn vector(
    oracle: &Oracle,
    chain_v24: bool,
    params: &ChainSetup,
    ctx: &Context,
    block: &[u8],
    quirk: u32,
    name: String,
) -> Option<(Vector, Vec<DmlEvent>, Option<MnList>)> {
    let chain = chain_for(chain_v24);
    let full = core_verdict(oracle, &chain, ctx, block)?;
    let ctx = [1, 8, 32, 128]
        .into_iter()
        .filter(|&keep| keep < ctx.headers.len())
        .map(|keep| {
            let mut trimmed = ctx.clone();
            let drop = ctx.headers.len() - keep;
            trimmed.headers.drain(..drop);
            trimmed.first_height += drop as i32;
            trimmed
        })
        .find(|trimmed| core_verdict(oracle, &chain, trimmed, block).as_ref() == Some(&full))
        .unwrap_or_else(|| ctx.clone());
    let (core_payee, core) = full;
    let replay_ctx = replay_context(&ctx)?;
    let mut events = Vec::new();
    let ours = replay(params, &replay_ctx, block, &mut events).ok()?;
    if !ours.agrees_with(&core, core_payee) || ours.twin.is_err() {
        eprintln!("skipped (ours disagrees): {name}");
        return None;
    }
    let vector = Vector {
        quirk,
        name,
        ctx: replay_ctx,
        block: block.to_vec(),
        core_payee,
        core,
        oracle: oracle_fields(&ctx, chain_v24)?,
    };
    Some((vector, events, ours.result.ok()))
}

/// `block` with its special transactions (all but the coinbase) in reverse order; `None` for
/// fewer than two.
fn reversed(block: &[u8]) -> Option<Vec<u8>> {
    let (header, parts) = split_block(block)?;
    if parts.len() < 3 {
        return None;
    }
    let mut txs = vec![parts[0]];
    txs.extend(parts[1..].iter().rev());
    Some(assemble(header, &txs))
}

/// The masternode a ProUpServTx, ProUpRegTx or ProUpRevTx (raw bytes) updates.
fn updated(tx: &[u8]) -> Option<ProTxHash> {
    let tx = decode_tx(&mut &tx[..]).ok()?.into_value();
    let hash = match payload(&tx)? {
        SpecialPayload::ProviderUpdateService(p) => p.pro_tx_hash,
        SpecialPayload::ProviderUpdateRegistrar(p) => p.pro_tx_hash,
        SpecialPayload::ProviderUpdateRevoke(p) => p.pro_tx_hash,
        _ => return None,
    };
    Some(ProTxHash::from(*hash.as_bytes()))
}

/// `block` with the first two transactions that update `pro_tx` swapped and the others in place;
/// `None` for fewer than two.
fn swapped(block: &[u8], pro_tx: &ProTxHash) -> Option<Vec<u8>> {
    let (header, mut parts) = split_block(block)?;
    let mut at = (1..parts.len()).filter(|&i| updated(parts[i]) == Some(*pro_tx));
    let (a, b) = (at.next()?, at.next()?);
    parts.swap(a, b);
    Some(assemble(header, &parts))
}

/// `ctx` with its list padded to `total` masternodes by `numbered` ones registered at height 1
/// and never paid.
fn padded(ctx: &Context, total: usize) -> Option<Context> {
    let CoreList::Supplied(bytes) = &ctx.mn_list else {
        return None;
    };
    let mut list = core_disk::decode_list_s3(&mut &bytes[..]).ok()?;
    for i in 0..total.checked_sub(list.len())? {
        let base = numbered(10_000 + i as u32);
        let mut cold = (*base.cold).clone();
        cold.registered_height = 1;
        let dmn = Dmn {
            hot: MnHot::default(),
            cold: Arc::new(fresh(cold)),
            ..(*base).clone()
        };
        list.try_add(Arc::new(dmn), true).ok()?;
    }
    let mut ctx = ctx.clone();
    ctx.mn_list = CoreList::Supplied(list_disk_bytes(&list));
    Some(ctx)
}

/// The tip's height of `ctx`.
fn tip_height(ctx: &Context) -> i32 {
    ctx.first_height + ctx.headers.len() as i32 - 1
}

/// `ctx` with its list replaced by `f` applied to each of its masternodes.
fn with_list(ctx: &Context, f: impl Fn(&mut Dmn)) -> Option<Context> {
    let CoreList::Supplied(bytes) = &ctx.mn_list else {
        return None;
    };
    let list = core_disk::decode_list_s3(&mut &bytes[..]).ok()?;
    let mut changed = MnList::new(*list.block_hash(), list.height(), list.total_registered());
    for dmn in list.iter() {
        let mut dmn = (**dmn).clone();
        f(&mut dmn);
        changed.try_add(Arc::new(dmn), false).ok()?;
    }
    let mut ctx = ctx.clone();
    ctx.mn_list = CoreList::Supplied(list_disk_bytes(&changed));
    Some(ctx)
}

fn main() -> std::io::Result<()> {
    let oracle = Oracle::new(Pin::V24_0);
    let mut out: Vec<Vector> = Vec::new();

    // Quirk 13, over every crafted case with masternodes: a coinbase-only block.
    let mut banned_done = false;
    let mut tie_done = false;
    for case in cases().iter().filter(|c| c.prior.is_none()) {
        let Some((header, parts)) = split_block(&case.block) else {
            continue;
        };
        let block = assemble(header, &parts[..1]);
        let height = tip_height(&case.ctx);
        if !banned_done
            && let Some(ctx) = with_list(&case.ctx, |d| d.hot.ban_if_not_banned(height))
            && let Some((v, _, _)) = vector(
                &oracle,
                case.v24,
                &case.params,
                &ctx,
                &block,
                13,
                format!("{}: every masternode banned, no payee", case.name),
            )
            && v.core_payee.is_none()
            && matches!(v.core, CoreVerdict::Accept(_))
        {
            out.push(v);
            banned_done = true;
        }
        if !tie_done
            && let Some(ctx) = with_list(&case.ctx, |d| {
                d.hot = MnHot::default();
                d.hot.last_paid_height = height - 5;
                let mut cold = (*d.cold).clone();
                cold.confirmed_hash = Default::default();
                cold.confirmed_hash_with_pro_tx = Default::default();
                d.cold = Arc::new(fresh(cold));
            })
            && let Some((v, _, _)) = vector(
                &oracle,
                case.v24,
                &case.params,
                &ctx,
                &block,
                13,
                format!("{}: a tie at one pay height, all unconfirmed", case.name),
            )
            && v.core_payee.is_some()
        {
            out.push(v);
            tie_done = true;
        }
    }

    // Quirks 24, 25 and 42 from generated blocks.
    let mut found: BTreeSet<&str> = BTreeSet::new();
    let want = ["24", "24 built", "25 replaced", "25 twice", "42"];
    for attempt in 0..400_000u64 {
        if want.iter().all(|w| found.contains(w)) {
            break;
        }
        if attempt % 20_000 == 0 {
            eprintln!("attempt {attempt}: {found:?}");
        }
        let input_bytes = splitmix_bytes(0x0e2b_a000_0000 + attempt, 8 + (attempt % 41) as usize);
        let Some(input) = generate(&input_bytes) else {
            continue;
        };
        let chain = input.chain();
        let params = &input.case.params;
        let Some((_, core)) = core_verdict(&oracle, &chain, &input.ctx, &input.block) else {
            continue;
        };
        match core {
            CoreVerdict::Accept(_) => {
                let name = |what: &str| format!("{}: {what} ({})", input.case.name, attempt);
                let Some((v, events, _)) = vector(
                    &oracle,
                    input.v24,
                    params,
                    &input.ctx,
                    &input.block,
                    25,
                    name("accepted"),
                ) else {
                    continue;
                };
                if !found.contains("25 replaced")
                    && events.iter().any(|e| matches!(e, DmlEvent::Replaced { .. }))
                {
                    out.push(Vector {
                        name: name("a ProRegTx replaces a masternode by collateral"),
                        ..v.clone()
                    });
                    found.insert("25 replaced");
                }
                let mut touched: BTreeMap<ProTxHash, usize> = BTreeMap::new();
                for e in &events {
                    if let DmlEvent::Updated {
                        pro_tx,
                        ..
                    }
                    | DmlEvent::Revoked {
                        pro_tx,
                        ..
                    }
                    | DmlEvent::ShareUpdated {
                        pro_tx,
                        ..
                    }
                    | DmlEvent::SharedRegistrarUpdated {
                        pro_tx,
                        ..
                    } = e
                    {
                        *touched.entry(*pro_tx).or_default() += 1;
                    }
                }
                if !found.contains("25 twice")
                    && let CoreList::Supplied(prev) = &input.ctx.mn_list
                    && let Ok(prev) = core_disk::decode_list_s3(&mut &prev[..])
                    && let Some(swapped) = touched
                        .iter()
                        .filter(|(pro_tx, n)| **n >= 2 && prev.get(pro_tx).is_some())
                        .find_map(|(pro_tx, _)| swapped(&input.block, pro_tx))
                    && let Some((b, _, _)) = vector(
                        &oracle,
                        input.v24,
                        params,
                        &input.ctx,
                        &swapped,
                        25,
                        name("one masternode updated twice, swapped"),
                    )
                    && b.core != v.core
                {
                    out.push(Vector {
                        name: name("one masternode updated twice, in block order"),
                        ..v.clone()
                    });
                    out.push(b);
                    found.insert("25 twice");
                }
                // Quirk 24: ban the punished members first.
                let punished: Vec<ProTxHash> = events
                    .iter()
                    .filter_map(|e| match e {
                        DmlEvent::Punished {
                            pro_tx,
                            ..
                        } => Some(*pro_tx),
                        _ => None,
                    })
                    .collect();
                // Quirk 24's count: a QcTx after a ProRegTx, over 160 masternodes.
                let first_punished =
                    events.iter().position(|e| matches!(e, DmlEvent::Punished { .. }));
                if !found.contains("24 built")
                    && let Some(at) = first_punished
                    && events[..at].iter().any(|e| matches!(e, DmlEvent::Added { .. }))
                    && let Some(ctx) = padded(&input.ctx, 160)
                    && let Some((v24, events, Some(_))) = vector(
                        &oracle,
                        input.v24,
                        params,
                        &ctx,
                        &input.block,
                        24,
                        name("a QcTx after a ProRegTx counts it (max(100, total) as rebuilt)"),
                    )
                    && events
                        .iter()
                        .any(|e| matches!(e, DmlEvent::Punished { max, .. } if *max > 160))
                {
                    out.push(v24);
                    found.insert("24 built");
                }
                if !found.contains("24") && !punished.is_empty() {
                    let height = tip_height(&input.ctx);
                    let Some(ctx) = with_list(&input.ctx, |d| {
                        if punished.contains(&d.pro_tx_hash) {
                            d.hot.pose_penalty = 10;
                            d.hot.ban_if_not_banned(height);
                        }
                    }) else {
                        continue;
                    };
                    if let Some((v24, _, Some(new))) = vector(
                        &oracle,
                        input.v24,
                        params,
                        &ctx,
                        &input.block,
                        24,
                        name("a QcTx punishes masternodes already banned"),
                    ) && punished
                        .iter()
                        .all(|p| new.get(p).is_some_and(|d| d.hot.pose_penalty > 10))
                    {
                        out.push(v24);
                        found.insert("24");
                    }
                }
            }
            CoreVerdict::Exception(_) if !found.contains("42") => {
                let Some(swapped) = reversed(&input.block) else {
                    continue;
                };
                let name = |what: &str| format!("{}: {what} ({attempt})", input.case.name);
                let (Some((a, _, _)), Some((b, _, _))) = (
                    vector(
                        &oracle,
                        input.v24,
                        params,
                        &input.ctx,
                        &input.block,
                        42,
                        name("block order"),
                    ),
                    vector(&oracle, input.v24, params, &input.ctx, &swapped, 42, name("reversed")),
                ) else {
                    continue;
                };
                if a.core != b.core {
                    out.push(a);
                    out.push(b);
                    found.insert("42");
                }
            }
            _ => {}
        }
    }
    eprintln!("found {found:?}, quirk 13: banned {banned_done}, tie {tie_done}");

    let mut text = String::from(
        "# EV2b D25: Core's results (oracle_dml_apply, v24.0.0-rc.3) for blocks that show quirks\n\
         # 13, 24, 25 and 42. Written by `cargo run -p node-l1 --features oracle --example\n\
         # dml_quirk_vectors`; replayed by dash-dml/tests/quirks_ev2b.rs.\n",
    );
    for v in &out {
        text += &v.write();
    }
    let path = format!("{}/../dash-dml/tests/data/ev2b-quirks.txt", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(&path, text)?;
    eprintln!("{} vectors -> {path}", out.len());
    Ok(())
}
