//! Writes a bench window ([`dash_dml::testkit::window`], EV2b D29) from mainnet blocks: reads
//! `<height> <hex>` lines on stdin (the cache of node-l1's L2 window, `zcat blocks.gz`), the
//! first being the parent, and writes the `COUNT` blocks after it to `OUT`.
//!
//! The kept prevouts are those the real blocks' build spends as collaterals (its
//! `CollateralSpent` lines). Before writing, it builds the window and the real blocks from the
//! default list and checks that every block's list has the same `CanonicalMNListHash`, which is
//! Core's in `CORE_DIGESTS` (the L2 window's `<height> <hash>` lines, EV2b D26), and that the
//! reference twin's last list agrees with the window's.
//!
//! `zcat blocks.gz | cargo run -p dash-dml --release --example dml_window_fixture -- COUNT OUT CORE_DIGESTS`
//!
//! Input from `node-l1/scripts/dml-w2-fetch.py` (EV2b D32) starts with its `list`, `header` and
//! `quorum` lines and makes a `dmlwin02` window from that list; CORE_DIGESTS is then the digest
//! file `node-l1`'s `dml_window` writes from the same input.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead};

use dash_consensus::primitives::{CoreSerialize, decode_block};
use dash_dml::codec::{canonical_mn_list_hash, core_disk};
use dash_dml::reference::xcheck;
use dash_dml::testkit::replay::{mainnet, payee_rules, unhex};
use dash_dml::testkit::window::{Window, WindowBlock, WindowStart, WindowTx, encode};
use dash_dml::{DmlEvent, NoTrace};
use dash_primitives::TxType;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, count, out, core_digests] = &args[..] else {
        panic!("usage: dml_window_fixture COUNT OUT CORE_DIGESTS");
    };
    let count: usize = count.parse().expect("COUNT");
    let core: BTreeMap<i32, String> = std::fs::read_to_string(core_digests)
        .expect("CORE_DIGESTS")
        .lines()
        .map(|line| {
            let (height, hash) = line.split_once(' ').expect("<height> <hash>");
            (height.parse().expect("height"), hash.to_owned())
        })
        .collect();
    let mut start = WindowStart::default();
    let mut read = Vec::new();
    for line in io::stdin().lock().lines() {
        let line = line.expect("stdin");
        let mut fields = line.split(' ');
        let (kind, value) = (fields.next().expect("a kind"), fields.next().expect("a value"));
        let hex = |h: &str| unhex(h.trim()).expect("hex");
        match kind {
            "list" => start.list = hex(fields.next().expect("a list")),
            "header" => start
                .prefix
                .push(hex(fields.next().expect("a header")).try_into().expect("80 bytes")),
            "quorum" => start.quorums.push((
                value.parse().expect("llmqType"),
                hex(fields.next().expect("a quorum hash")).try_into().expect("32 bytes"),
                fields
                    .map(|m| core_disk::decode_dmn(&mut &hex(m)[..]).expect("a member").pro_tx_hash)
                    .collect(),
            )),
            height => read.push((height.parse::<i32>().expect("height"), hex(value))),
        }
    }
    let mut lines = read.into_iter();
    let (parent_height, parent) = lines.next().expect("the parent");
    let parent: [u8; 80] = parent[..80].try_into().expect("a header");
    let start = (!start.list.is_empty()).then_some(start);

    // The real blocks, every transaction after the coinbase as mined (a block whose bytes are
    // Core's serialization re-serializes each transaction to its mined bytes).
    let mut real = Vec::with_capacity(count);
    for (i, (height, bytes)) in lines.take(count).enumerate() {
        assert_eq!(height, parent_height + 1 + i as i32, "consecutive heights");
        let block = decode_block(&mut &bytes[..]).expect("a block");
        assert!(block.wire_is_canonical(), "block {height} is Core's serialization");
        real.push(WindowBlock {
            header: bytes[..80].try_into().expect("a header"),
            txs: block.value().txs()[1..]
                .iter()
                .map(|tx| WindowTx::Raw(tx.raw().core_serialize()))
                .collect(),
        });
    }
    assert_eq!(real.len(), count, "fewer blocks than COUNT");
    let full = Window::decode(&encode(parent_height, &parent, start.as_ref(), &real))
        .expect("the real window");
    let cores = |height: i32, list: &dash_dml::MnList| {
        let digest = canonical_mn_list_hash(list).to_string();
        assert_eq!(Some(&digest), core.get(&height), "Core's list at {height}");
    };
    let mut events = Vec::new();
    full.build(full.start().clone(), &mut events, |i, list| cores(full.height(i), list))
        .expect("mainnet's blocks build");
    let spent: BTreeSet<_> = events
        .iter()
        .filter_map(|e| match e {
            DmlEvent::CollateralSpent {
                collateral,
                ..
            } => Some(*collateral),
            _ => None,
        })
        .collect();

    // The window: special transactions (and any typed one) as mined, the others as shapes.
    let mut kept_inputs = 0;
    let shaped: Vec<WindowBlock> = real
        .into_iter()
        .zip(full.blocks())
        .map(|(block, decoded)| WindowBlock {
            header: block.header,
            txs: block
                .txs
                .into_iter()
                .zip(&decoded.txs()[1..])
                .map(|(raw, tx)| {
                    if tx.is_special_tx_version() || tx.tx_type() != TxType::Spend {
                        return raw;
                    }
                    let kept: Vec<_> = (0u32..)
                        .zip(tx.inputs())
                        .filter(|(_, input)| spent.contains(&input.prevout))
                        .map(|(at, input)| (at, input.prevout))
                        .collect();
                    kept_inputs += kept.len();
                    WindowTx::Shape {
                        inputs: tx.inputs().len() as u32,
                        kept,
                    }
                })
                .collect(),
        })
        .collect();
    let bytes = encode(parent_height, &parent, start.as_ref(), &shaped);
    let window = Window::decode(&bytes).expect("the window reads back");
    let last = window
        .build(window.start().clone(), &mut NoTrace, |i, list| cores(window.height(i), list))
        .expect("the window builds");
    let twin = window.twin_start();
    let twin = window.build_ref(twin).expect("the twin builds it");
    xcheck(&last, &twin, payee_rules(mainnet(), last.height())).expect("the twin agrees");

    std::fs::write(out, &bytes).expect("OUT");
    println!(
        "{count} blocks after {parent_height}: {} bytes, {} collaterals spent, {kept_inputs} kept \
         inputs; {} masternodes at {}, CanonicalMNListHash {}",
        bytes.len(),
        spent.len(),
        last.len(),
        last.height(),
        canonical_mn_list_hash(&last)
    );
}
