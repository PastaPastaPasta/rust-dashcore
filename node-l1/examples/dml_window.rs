//! EV2b's mainnet window (`node_l1::dml_apply::window`): reads `<height> <block hex>` lines on
//! stdin, consecutive, the first being the block before the window, and applies each later one
//! to our list chain and to Core's. Writes `<height> <CanonicalMNListHash>` per block to the
//! digest file and stops at the first difference.
//!
//! `fetch-window.py 1028159 1078159 | cargo run --release --config 'profile.release.package.dashd-masternode.debug-assertions=true' -p node-l1 --features oracle
//! --example dml_window -- <digest file>`
//!
//! A window from a supplied list (EV2b D32, `node-l1/scripts/dml-w2-fetch.py`) starts with
//! `list <height> <hex>`, `header <height> <hex>` and `quorum <llmqType> <hash> <member>...`
//! lines; both chains then start at that list after the parent block. A `quorum` line after
//! the parent block adds its members for the blocks after it (`dml-w2-fetch.py --stream`).
//!
//! A window from EV2b's own O8 replica (D37, D38, `node-l1/scripts/dml-replica-fetch.py`) gives
//! its start list as `canonical <height> <hex>` (`getcanonicalmnlist`'s bytes) and its quorums as
//! `quorumh <llmqType> <hash> <proTxHash>...`: the build reads only a member's proTxHash, so each
//! member is a template masternode under it. `--testnet` reads testnet's blocks (D38).

use std::io::{BufRead, Write};

use dash_dml::ProTxHash;
use dash_dml::codec::{ContextFreeNetInfo, decode_canonical_list};
use dash_dml::testkit::replay::ReplayQuorum;
use dash_dml::testkit::{list_disk_bytes, member_bytes};
use node_l1::dml_apply::window::{Net, Window};

fn main() -> std::process::ExitCode {
    let net = if std::env::args().any(|a| a == "--testnet") {
        Net::Testnet
    } else {
        Net::Mainnet
    };
    let path = std::env::args()
        .skip(1)
        .find(|a| a != "--testnet")
        .expect("usage: dml_window [--testnet] <digest file>");
    let mut digests = std::io::BufWriter::new(std::fs::File::create(path).expect("digest file"));
    let mut window: Option<Window> = None;
    let mut expected = None;
    let (mut first, mut count) = (None, 0u64);
    let (mut list, mut headers, mut quorums) = (None, Vec::new(), Vec::new());
    // The height of a `canonical` line's list (its own, equal to the line's): the parent block's.
    let mut list_height = None;
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("stdin");
        let mut fields = line.split(' ');
        let (Some(kind), Some(value)) = (fields.next(), fields.next()) else {
            continue;
        };
        let hex = |h: &str| node_l1::dml::unhex(h.trim()).expect("hex");
        let Ok(height) = kind.parse::<i32>() else {
            match kind {
                "list" => {
                    list = Some(hex(fields.next().expect("a list")));
                    list_height = None;
                }
                // The replica's lists are ones Core accepted: context-free address checks.
                "canonical" => {
                    let height = value.parse::<i32>().expect("a height");
                    let bytes = hex(fields.next().expect("a list"));
                    let decoded = decode_canonical_list(&mut &bytes[..], &ContextFreeNetInfo)
                        .expect("the replica's list");
                    assert_eq!(decoded.height(), height, "the canonical list's own height");
                    list_height = Some(height);
                    list = Some(list_disk_bytes(&decoded));
                }
                "header" => headers
                    .push(hex(fields.next().expect("a header")).try_into().expect("80 bytes")),
                "quorum" | "quorumh" => {
                    let quorum_hash =
                        hex(fields.next().expect("a quorum hash")).try_into().expect("32 bytes");
                    let members = fields.map(hex);
                    let quorum = ReplayQuorum {
                        llmq_type: value.parse().expect("llmqType"),
                        quorum_hash,
                        members: match kind {
                            "quorumh" => members
                                .map(|h| {
                                    let h: [u8; 32] = h.try_into().expect("32 bytes");
                                    member_bytes(ProTxHash::from(h))
                                })
                                .collect(),
                            _ => members.collect(),
                        },
                    };
                    match window.as_mut() {
                        Some(window) => window.add_quorum(quorum),
                        None => quorums.push(quorum),
                    }
                }
                _ => {}
            }
            continue;
        };
        let block = hex(value);
        assert!(expected.is_none_or(|e| e == height), "heights are not consecutive at {height}");
        expected = Some(height + 1);
        let Some(window) = window.as_mut() else {
            assert!(list_height.is_none_or(|h| h == height), "the list is not the parent's");
            let mut start = match list.take() {
                Some(list) => {
                    headers.push(block[..80].try_into().expect("a header"));
                    Window::from_list(net, std::mem::take(&mut headers), list)
                        .expect("the start list")
                }
                None => Window::new(net, &block, height).expect("the parent block"),
            };
            std::mem::take(&mut quorums).into_iter().for_each(|q| start.add_quorum(q));
            window = Some(start);
            continue;
        };
        match window.step(&block) {
            Ok(step) => {
                let hex: String = step.digest.iter().rev().map(|b| format!("{b:02x}")).collect();
                writeln!(digests, "{} {hex}", step.height).expect("digest file");
                first.get_or_insert(step.height);
                count += 1;
                if count.is_multiple_of(1000) {
                    eprintln!("window: {count} blocks equal, at {}", step.height);
                }
            }
            Err(e) => {
                digests.flush().expect("digest file");
                println!("WINDOW DIFF after {count} equal blocks: {e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    digests.flush().expect("digest file");
    match first {
        Some(first) => println!("WINDOW OK {count} blocks {first}..{}", first + count as i32 - 1),
        None => println!("WINDOW EMPTY"),
    }
    std::process::ExitCode::SUCCESS
}
