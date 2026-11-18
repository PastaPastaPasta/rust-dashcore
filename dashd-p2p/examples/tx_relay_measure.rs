//! Controlled G4 timing and request-transcript measurement driver.

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::Duration;

use dash_consensus::{Tx, TxRef};
use dash_p2p::{CommandString, ConnectionType, Permissions};
use dash_p2p_core::{GetData, Inv, NotFound};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{Transaction, TxType};
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::TxInfo;
use dashd_p2p::sim::transcript::{hex, unhex};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimTxPool};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let pool = SimTxPool::default();
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::with(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            txpool: Arc::new(pool.clone()),
            ..Seams::default()
        },
        |_| {},
    );
    sim.chain.set_ibd(false);
    if args[1] == "timing" {
        let tx: TxRef =
            Arc::new(Tx::new(Transaction::new(1, TxType::Spend, vec![], vec![], 0, vec![])));
        pool.insert(
            TxInfo {
                tx,
                time_s: SIM_START,
            },
            1,
            1,
        );
        let peers: Vec<_> = (1..=20)
            .map(|n| {
                let p = sim.connect(n, ConnectionType::Manual, Permissions::NONE);
                sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
                p
            })
            .collect();
        let mut intervals = vec![Vec::new(); 20];
        let mut last = [None; 20];
        let mut tick = 0;
        while intervals.iter().any(|samples| samples.len() < 500) {
            tick += 1;
            sim.clock.advance(Duration::from_secs(1));
            for (i, p) in peers.iter().enumerate() {
                if intervals[i].len() == 500 {
                    continue;
                }
                sim.shared.with_state_mut(p.handle.id(), |st| st.send_mempool = true);
                sim.lp.logic_mut().send_messages(&p.handle);
                if sim.sent(p).iter().any(|(cmd, _)| *cmd == CommandString::INV) {
                    if let Some(previous) = last[i] {
                        intervals[i].push(tick - previous);
                    }
                    last[i] = Some(tick);
                }
            }
        }
        let samples: Vec<_> = intervals.into_iter().flatten().collect();
        println!("{samples:?}");
    } else {
        let trace = std::fs::read_to_string(&args[2]).unwrap();
        let batches: Vec<_> = trace.lines().map(unhex).collect();
        let p = sim.connect(1, ConnectionType::Manual, Permissions::NONE);
        sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
        println!("READY");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).unwrap();
        let mut responses = Vec::new();
        for payload in batches {
            let want = Inv::decode(&mut &payload[..]).unwrap();
            sim.deliver(&p, &[("inv", payload)]);
            sim.pass();
            let got: Vec<_> = sim
                .sent_at_least(&p, 1)
                .into_iter()
                .filter(|(cmd, _)| *cmd == CommandString::GETDATA)
                .flat_map(|(_, bytes)| {
                    GetData::decode(&mut &bytes[..]).unwrap().inventory().clone()
                })
                .collect();
            assert_eq!(got, *want.inventory());
            let mut bytes = Vec::new();
            NotFound::new(got).encode(&mut bytes);
            responses.push(hex(&bytes));
            sim.deliver(&p, &[("notfound", bytes)]);
            sim.pass();
        }
        println!(
            "DONE {}",
            dashd_p2p::sim::transcript::hex(&dash_consensus::hash::sha256(
                responses.join("\n").as_bytes()
            ))
        );
    }
}
