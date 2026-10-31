//! P2b in `sim`: `getheaders`/`getheaders2` serving (Core's `GETHEADERS`/`GETHEADERS2` branch,
//! net_processing.cpp:4760-4855) to inbound peers that `NetCore::accept` admitted. The live
//! comparison with dashd is `dashd_inbound_transcripts.rs`; these cover what the fixtures leave
//! out: the 8000 limit at its edge, a stop block off the active chain, and the loading state.

use std::sync::Arc;

use dash_p2p::CommandString;
use dash_p2p::conn::Permissions;
use dash_p2p_core::{GetHeaders, Headers, Headers2, ProtocolVersion};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::{ChainAccess, EntryFacts, Work};
use dashd_p2p::processing::serve::{STALE_RELAY_AGE_LIMIT, block_proof_equivalent_time};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer, SimRead};
use dashd_p2p::{Registry, Seams};

const BIT11: u64 = 0x800;
const NETWORK: u64 = 1;

/// More than `STALE_RELAY_AGE_LIMIT`, in seconds.
const MONTH_AND_A_DAY: u32 = 31 * 24 * 60 * 60;

/// A node whose active chain is genesis plus `n` blocks, out of IBD; returns the entries by height.
fn node(n: usize) -> (Sim, Vec<EntryFacts>) {
    let p =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let sim =
        Sim::with(p, id, ProcessorOptions::default(), Registry::new(), Seams::default(), |_| {});
    let chain = chain_on(&sim, &sim.chain.genesis(), n, 0x11, 150);
    sim.chain.set_tip(chain[n].id);
    sim.chain.set_ibd(false);
    (sim, chain)
}

/// `n` entries on `base` (salted by `salt`), `spacing` seconds apart, base first.
fn chain_on(sim: &Sim, base: &EntryFacts, n: usize, salt: u8, spacing: u32) -> Vec<EntryFacts> {
    let mut out = vec![*base];
    for h in SimChain::build(base, n, spacing, salt) {
        out.push(sim.chain.add(&[h]));
    }
    out
}

/// Inbound peer `n` at `version` with `services`, after the handshake and a pass.
fn inbound(sim: &mut Sim, n: u8, version: i32, services: u64, perms: Permissions) -> SimPeer {
    let p = sim.accept(n, perms).expect("an inbound slot");
    sim.handshake(
        &p,
        PeerVersionMsg {
            version,
            services,
            ..PeerVersionMsg::v24(SIM_START)
        },
    );
    sim.pass();
    sim.sent(&p);
    p
}

fn locator(hashes: &[BlockHash], stop: BlockHash) -> Vec<u8> {
    let mut buf = Vec::new();
    GetHeaders {
        version: ProtocolVersion(70242),
        locator_hashes: hashes.to_vec(),
        hash_stop: stop,
    }
    .encode(&mut buf);
    buf
}

/// A hash no entry has.
fn unknown() -> BlockHash {
    BlockHash::from_lendian([7; 32])
}

fn headers_bytes(hs: &[BlockHeader]) -> Vec<u8> {
    let mut buf = Vec::new();
    Headers {
        headers: hs.to_vec(),
    }
    .encode(&mut buf);
    buf
}

fn headers2_bytes(hs: &[BlockHeader]) -> Vec<u8> {
    let mut buf = Vec::new();
    Headers2::reply(hs).encode(&mut buf);
    buf
}

/// The control: `p` is still served (from genesis, with the 2000 limit) after a request the
/// node ignored.
fn served_still(sim: &mut Sim, p: &SimPeer, c: &[EntryFacts]) {
    let r = ask_one(sim, p, "getheaders", locator(&[c[0].hash], BlockHash::ZERO));
    let n = (c.len() - 1).min(2000);
    assert_eq!(r, (CommandString::HEADERS, headers_bytes(&headers_of(&c[1..=n]))));
}

fn headers_of(entries: &[EntryFacts]) -> Vec<BlockHeader> {
    entries.iter().map(|e| e.header).collect()
}

/// Sends `cmd` and returns the node's `headers`/`headers2` replies.
fn ask(sim: &mut Sim, p: &SimPeer, cmd: &str, payload: Vec<u8>) -> Vec<(CommandString, Vec<u8>)> {
    sim.deliver(p, &[(cmd, payload)]);
    sim.pass();
    sim.sent(p)
        .into_iter()
        .filter(|(c, _)| *c == CommandString::HEADERS || *c == CommandString::HEADERS2)
        .collect()
}

/// Sends `cmd` and returns the node's one `headers`/`headers2` reply.
fn ask_one(sim: &mut Sim, p: &SimPeer, cmd: &str, payload: Vec<u8>) -> (CommandString, Vec<u8>) {
    let reply = ask(sim, p, cmd, payload);
    assert_eq!(reply.len(), 1, "one reply: {:?}", reply.iter().map(|r| &r.0).collect::<Vec<_>>());
    reply.into_iter().next().unwrap()
}

/// The 2000 limit for `getheaders`, and for `getheaders2` below 70235; 8000 for `getheaders2`
/// from 70235 (np:1210-1216). The reply type is the request's, whatever bit 11 says.
#[test]
fn limits_by_command_and_version() {
    let (mut sim, c) = node(8001);
    let g = [c[0].hash];
    let new = inbound(&mut sim, 1, 70242, NETWORK | BIT11, Permissions::NONE);
    let old = inbound(&mut sim, 2, 70234, NETWORK, Permissions::NONE);

    let r = ask_one(&mut sim, &new, "getheaders", locator(&g, BlockHash::ZERO));
    assert_eq!(r, (CommandString::HEADERS, headers_bytes(&headers_of(&c[1..=2000]))));
    let r = ask_one(&mut sim, &new, "getheaders2", locator(&g, BlockHash::ZERO));
    assert_eq!(r, (CommandString::HEADERS2, headers2_bytes(&headers_of(&c[1..=8000]))));
    let r = ask_one(&mut sim, &new, "getheaders2", locator(&[c[8000].hash], BlockHash::ZERO));
    assert_eq!(r, (CommandString::HEADERS2, headers2_bytes(&headers_of(&c[8001..]))));
    let r = ask_one(&mut sim, &old, "getheaders2", locator(&g, BlockHash::ZERO));
    assert_eq!(r, (CommandString::HEADERS2, headers2_bytes(&headers_of(&c[1..=2000]))));
}

/// The fork is the first locator entry on the active chain (unknown and side-chain entries are
/// skipped; none known means genesis); `hashStop` ends the run, inclusive; a locator at the tip
/// gets an empty reply.
#[test]
fn locator_fork_and_stop() {
    let (mut sim, c) = node(30);
    let side = chain_on(&sim, &c[10], 5, 0x22, 150);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);

    let r = ask_one(
        &mut sim,
        &p,
        "getheaders",
        locator(&[unknown(), side[5].hash, c[20].hash, c[3].hash], BlockHash::ZERO),
    );
    assert_eq!(r.1, headers_bytes(&headers_of(&c[21..])));
    let r = ask_one(&mut sim, &p, "getheaders", locator(&[unknown()], BlockHash::ZERO));
    assert_eq!(r.1, headers_bytes(&headers_of(&c[1..])));
    let r = ask_one(&mut sim, &p, "getheaders", locator(&[c[0].hash], c[7].hash));
    assert_eq!(r.1, headers_bytes(&headers_of(&c[1..=7])));
    let r = ask_one(&mut sim, &p, "getheaders", locator(&[c[30].hash], BlockHash::ZERO));
    assert_eq!(r.1, headers_bytes(&[]));
}

/// A null locator returns the stop block alone when it is on the active chain; an unknown stop
/// gets no reply, and the peer is still served afterwards.
#[test]
fn null_locator() {
    let (mut sim, c) = node(10);
    let p = inbound(&mut sim, 1, 70242, NETWORK | BIT11, Permissions::NONE);
    let r = ask_one(&mut sim, &p, "getheaders", locator(&[], c[4].hash));
    assert_eq!(r.1, headers_bytes(&[c[4].header]));
    let r = ask_one(&mut sim, &p, "getheaders2", locator(&[], c[0].hash));
    assert_eq!(r.1, headers2_bytes(&[c[0].header]));
    assert!(ask(&mut sim, &p, "getheaders", locator(&[], unknown())).is_empty());
    served_still(&mut sim, &p, &c);
}

/// `BlockRequestAllowed` for a null-locator stop off the active chain (np:2048-2055): only a
/// block that was once connected (`BLOCK_VALID_SCRIPTS`) is served, and only while the best
/// header (not the tip) is less than a month newer than it, in time and in proof-equivalent time.
#[test]
fn null_locator_off_the_active_chain() {
    let (mut sim, c) = node(10);
    let side = chain_on(&sim, &c[5], 3, 0x22, 150);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);
    let ask_side = |sim: &mut Sim| ask(sim, &p, "getheaders", locator(&[], side[2].hash));

    // Never connected: refused.
    assert!(ask_side(&mut sim).is_empty());
    served_still(&mut sim, &p, &c);
    // Connected once (a reorg to it and back), recent: served alone.
    sim.chain.set_tip(side[3].id);
    sim.chain.set_tip(c[10].id);
    assert_eq!(ask_side(&mut sim), [(CommandString::HEADERS, headers_bytes(&[side[2].header]))]);

    // A best header (headers only, the tip stays) more than a month newer in time but not in
    // work: refused by the time clause, and by the best header rather than the tip.
    chain_on(&sim, &c[10], 1, 0x33, MONTH_AND_A_DAY);
    assert_eq!(sim.chain.snapshot().tip.tip, c[10].id);
    assert!(ask_side(&mut sim).is_empty());
    served_still(&mut sim, &p, &c);
}

/// The proof-equivalent-time clause alone: a best chain a month of work ahead of the side block
/// but only hours newer in time.
#[test]
fn null_locator_off_the_active_chain_by_work() {
    let (mut sim, c) = node(10);
    let side = chain_on(&sim, &c[5], 3, 0x22, 150);
    sim.chain.set_tip(side[3].id);
    sim.chain.set_tip(c[10].id);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);
    // The unchecked `SimChain` gives each block work 1 and the tip's regtest proof is 2, so a
    // block is 75 s of proof-equivalent time.
    let month = (STALE_RELAY_AGE_LIMIT / 75) as usize + 10;
    let long = chain_on(&sim, &c[10], month, 0x33, 1);
    sim.chain.set_tip(long[month].id);
    assert!(long[month].time - side[2].time < STALE_RELAY_AGE_LIMIT as u32);
    assert!(ask(&mut sim, &p, "getheaders", locator(&[], side[2].hash)).is_empty());
    served_still(&mut sim, &p, &long);
}

/// Runs `f` on the chain inside the next handler that reads `LoadingBlocks()` (the `getheaders`
/// branch, after its snapshot was drained), then expires that snapshot's view, as CH1-node's
/// journal does after 2^16 status writes (BL-224).
fn inside_the_next_getheaders(sim: &Sim, f: impl FnOnce(&SimChain) + Send + 'static) {
    let chain = Arc::downgrade(&sim.chain);
    let f = parking_lot::Mutex::new(Some(f));
    sim.chain.set_read_hook(Some(Arc::new(move |read| {
        if read == SimRead::LoadingBlocks
            && let Some(f) = f.lock().take()
        {
            let chain = chain.upgrade().expect("the sim's chain");
            let held = chain.snapshot();
            f(&chain);
            chain.expire(&held);
        }
    })));
}

/// DEC-292 (D121): when the message's view expired, `BlockRequestAllowed` decides on the newest
/// view, tip facts and status together. The review's interleaving: a stale side block, once
/// connected, is invalidated before the message; inside the handler the best header moves more
/// than a month ahead and the block is reconsidered, then the message's view expires. The
/// message's best header with the newest view's `VALID_SCRIPTS` would serve it, which Core
/// refuses at every view between the two: failed before the reconsider, a month stale after.
#[test]
fn null_locator_decides_on_the_newest_view_when_the_snapshot_expired() {
    let (mut sim, c) = node(10);
    let side = chain_on(&sim, &c[5], 3, 0x22, 150);
    let e = side[2].id;
    sim.chain.set_tip(side[3].id);
    sim.chain.set_tip(c[10].id);
    sim.chain.set_failed(e, true);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);
    let ask_side = |sim: &mut Sim| ask(sim, &p, "getheaders", locator(&[], side[2].hash));
    assert!(ask_side(&mut sim).is_empty());

    // Reconsidered only: the newest view serves it (no refusal on expiry alone).
    inside_the_next_getheaders(&sim, move |chain| chain.set_failed(e, false));
    assert_eq!(ask_side(&mut sim), [(CommandString::HEADERS, headers_bytes(&[side[2].header]))]);

    // Failed again, then the review's interleaving: refused.
    sim.chain.set_failed(e, true);
    let tip = c[10];
    inside_the_next_getheaders(&sim, move |chain| {
        chain.add(&SimChain::build(&tip, 1, MONTH_AND_A_DAY, 0x33));
        chain.set_failed(e, false);
    });
    assert!(ask_side(&mut sim).is_empty());
    sim.chain.set_read_hook(None);
    served_still(&mut sim, &p, &c);
}

/// DEC-292 (S6b r2 n-1): the restart reads the work test at the newest view too. Inside the
/// window the active chain falls below `nMinimumChainWork`: Core at that view sends the empty
/// `headers` (np:4785-4792), where the message's tip, still above it, would serve the side block.
#[test]
fn null_locator_restart_reads_the_work_test_at_the_newest_view() {
    let (mut sim, c) = node(10);
    let side = chain_on(&sim, &c[5], 3, 0x22, 150);
    sim.chain.set_tip(side[3].id);
    sim.chain.set_tip(c[10].id);
    sim.chain.set_min_chain_work(c[10].chain_work);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);
    let ask_side = |sim: &mut Sim| ask(sim, &p, "getheaders", locator(&[], side[2].hash));
    assert_eq!(ask_side(&mut sim), [(CommandString::HEADERS, headers_bytes(&[side[2].header]))]);

    let low = c[3].id;
    inside_the_next_getheaders(&sim, move |chain| chain.set_tip(low));
    assert_eq!(ask_side(&mut sim), [(CommandString::HEADERS, headers_bytes(&[]))]);
    sim.chain.set_read_hook(None);
    sim.chain.set_tip(c[10].id);
    served_still(&mut sim, &p, &c);
}

/// DEC-292 (S6b r2 n-1): the restart reads `CChain::Contains` at the newest view too. Inside the
/// window the side branch becomes the active chain through a month of blocks: Core at that view
/// serves the side block as active, where the message's active chain with the newest best header
/// would refuse it as a month stale.
#[test]
fn null_locator_restart_reads_the_active_chain_at_the_newest_view() {
    let (mut sim, c) = node(10);
    let side = chain_on(&sim, &c[5], 3, 0x22, 150);
    sim.chain.set_tip(side[3].id);
    sim.chain.set_tip(c[10].id);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);
    let ask_side = |sim: &mut Sim| ask(sim, &p, "getheaders", locator(&[], side[2].hash));
    let served = [(CommandString::HEADERS, headers_bytes(&[side[2].header]))];
    assert_eq!(ask_side(&mut sim), served);

    // Ten blocks, each a month and a day apart, outweigh the main chain's five past the fork.
    let side_tip = side[3];
    inside_the_next_getheaders(&sim, move |chain| {
        let mut last = side_tip;
        for h in SimChain::build(&side_tip, 10, MONTH_AND_A_DAY, 0x44) {
            last = chain.add(&[h]);
        }
        chain.set_tip(last.id);
    });
    assert_eq!(ask_side(&mut sim), served);
    sim.chain.set_read_hook(None);
    assert!(
        sim.chain.snapshot().tip.best_header_time - side[2].time > STALE_RELAY_AGE_LIMIT as u32
    );
}

/// `pindexBestHeaderSent` after each way the branch ends (np:4826-4838): the last header sent
/// when the limit or `hashStop` ended the reply, the tip when the chain did (also for an empty
/// reply), the stop block itself off the active chain; unchanged when nothing is sent.
#[test]
fn best_header_sent() {
    let (mut sim, c) = node(2500);
    let side = chain_on(&sim, &c[5], 3, 0x22, 150);
    sim.chain.set_tip(side[3].id);
    sim.chain.set_tip(c[2500].id);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);
    let sent = |sim: &mut Sim, loc: &[BlockHash], stop: BlockHash| {
        ask(sim, &p, "getheaders", locator(loc, stop));
        sim.shared.with_state(p.handle.id(), |s| s.headers().best_header_sent).unwrap()
    };
    let g = c[0].hash;
    assert_eq!(sent(&mut sim, &[g], BlockHash::ZERO), Some(c[2000].id), "the limit");
    assert_eq!(sent(&mut sim, &[g], c[7].hash), Some(c[7].id), "the stop");
    assert_eq!(sent(&mut sim, &[c[2000].hash], BlockHash::ZERO), Some(c[2500].id), "the chain");
    assert_eq!(
        sent(&mut sim, &[c[7].hash], c[7].hash),
        Some(c[2007].id),
        "a stop behind the start"
    );
    assert_eq!(sent(&mut sim, &[], c[9].hash), Some(c[9].id), "a null locator");
    assert_eq!(sent(&mut sim, &[c[2500].hash], BlockHash::ZERO), Some(c[2500].id), "at the tip");
    assert_eq!(sent(&mut sim, &[], side[2].hash), Some(side[2].id), "off the active chain");
    assert_eq!(sent(&mut sim, &[], unknown()), Some(side[2].id), "an unknown stop");
    sim.chain.set_min_chain_work(Work::MAX);
    assert_eq!(sent(&mut sim, &[g], BlockHash::ZERO), Some(side[2].id), "too little work");
}

/// Below the minimum chain work every request gets an empty reply typed by the peer's bit 11
/// (np:4783-4795), unless the peer has the download permission.
#[test]
fn min_chain_work() {
    let (mut sim, c) = node(5);
    sim.chain.set_min_chain_work(Work::MAX);
    let bit11 = inbound(&mut sim, 1, 70242, NETWORK | BIT11, Permissions::NONE);
    let plain = inbound(&mut sim, 2, 70242, NETWORK, Permissions::NONE);
    let g = locator(&[c[0].hash], BlockHash::ZERO);
    for cmd in ["getheaders", "getheaders2"] {
        assert_eq!(
            ask_one(&mut sim, &bit11, cmd, g.clone()),
            (CommandString::HEADERS2, headers2_bytes(&[]))
        );
        assert_eq!(
            ask_one(&mut sim, &plain, cmd, g.clone()),
            (CommandString::HEADERS, headers_bytes(&[]))
        );
    }
}

/// The download permission lifts the minimum-work rule.
#[test]
fn min_chain_work_download_permission() {
    let (mut sim, c) = node(5);
    sim.chain.set_min_chain_work(Work::MAX);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::DOWNLOAD);
    let r = ask_one(&mut sim, &p, "getheaders", locator(&[c[0].hash], BlockHash::ZERO));
    assert_eq!(r.1, headers_bytes(&headers_of(&c[1..])));
}

/// While importing or reindexing the request is ignored (np:4771-4774), without a disconnect.
#[test]
fn loading_blocks() {
    let (mut sim, c) = node(5);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);
    sim.chain.set_loading_blocks(true);
    assert!(ask(&mut sim, &p, "getheaders", locator(&[c[0].hash], BlockHash::ZERO)).is_empty());
    assert!(!p.handle.disconnect_requested());
    sim.chain.set_loading_blocks(false);
    served_still(&mut sim, &p, &c);
}

/// More than `MAX_LOCATOR_SZ` (101) locator entries disconnect the peer (np:4765-4769), even
/// with the download permission; 101 are served.
#[test]
fn locator_size() {
    let (mut sim, c) = node(5);
    let p = inbound(&mut sim, 1, 70242, NETWORK, Permissions::NONE);
    let r = ask_one(&mut sim, &p, "getheaders", locator(&[c[0].hash; 101], BlockHash::ZERO));
    assert_eq!(r.1, headers_bytes(&headers_of(&c[1..])));
    assert!(!p.handle.disconnect_requested());
    for (n, cmd) in [(2, "getheaders"), (3, "getheaders2")] {
        let q = inbound(&mut sim, n, 70242, NETWORK, Permissions::DOWNLOAD);
        assert!(ask(&mut sim, &q, cmd, locator(&[c[0].hash; 102], BlockHash::ZERO)).is_empty());
        assert!(q.handle.disconnect_requested());
    }
}

/// `GetBlockProofEquivalentTime` (chain.cpp:147-162): the work difference in seconds of the tip's
/// proof, signed; `r * spacing` wraps at 2^256 as `arith_uint256`'s does, then saturates.
#[test]
fn proof_equivalent_time() {
    let bits = 0x207f_ffff; // regtest: each block's proof is 2
    let w = Work::from_u64;
    assert_eq!(block_proof_equivalent_time(w(20), w(10), bits, 150), 5 * 150);
    assert_eq!(block_proof_equivalent_time(w(10), w(20), bits, 150), -5 * 150);
    assert_eq!(block_proof_equivalent_time(w(10), w(10), bits, 150), 0);
    assert_eq!(block_proof_equivalent_time(Work::MAX, w(0), 0x1d00_ffff, 150), i64::MAX);
    assert_eq!(block_proof_equivalent_time(w(0), Work::MAX, 0x1d00_ffff, 150), -i64::MAX);
    // 2^255 * 2 wraps to 0: a saturating product would give `i64::MAX` here.
    assert_eq!(block_proof_equivalent_time(Work::ONE.wrapping_shl(255), w(0), bits, 2), 0);
}
