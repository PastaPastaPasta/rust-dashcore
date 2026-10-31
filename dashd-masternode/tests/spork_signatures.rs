//! The spork message-form signature against Core's verdicts (SP0): `tests/fixtures/dashd-v24/
//! verifymessage.txt` holds regtest dashd v24's `signmessagewithprivkey` signatures of spork
//! messages and its `verifymessage` verdicts on altered ones (every header byte, high-S, bit
//! flips; `contrib/node/sp0-transcripts/verifymessage.py`). `CSporkMessage` signs and checks with
//! the same `CMessageSigner`, so ours must sign the same bytes and agree on every verdict.

use dash_evo::spork::{SigForm, SporkId, check_signature, sign};
use dash_p2p_core::Spork;
use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_p2p::sim::transcript::unhex;

mod common;

#[test]
fn message_signatures_match_core() {
    let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let key_id = *p.spork_signers.key_ids.last().unwrap().as_bytes();
    let key = common::regtest_key();
    let (mut signed, mut verdicts, mut valid) = (0, 0, 0);
    for line in include_str!("fixtures/dashd-v24/verifymessage.txt").lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let spork = Spork {
            spork_id: f[0].parse().unwrap(),
            value: f[1].parse().unwrap(),
            time_signed: f[2].parse().unwrap(),
            sig: unhex(f[3]),
        };
        match f[4] {
            "signed" => {
                let ours = sign(
                    SporkId(spork.spork_id),
                    spork.value,
                    spork.time_signed,
                    &key,
                    true,
                    SigForm::Message,
                );
                assert_eq!(ours, spork, "{line}");
                signed += 1;
            }
            verdict => {
                let core = verdict == "true";
                assert_eq!(check_signature(&spork, &key_id, SigForm::Message), core, "{line}");
                verdicts += 1;
                valid += usize::from(core);
            }
        }
    }
    assert_eq!((signed, verdicts, valid), (8, 2576, 264));
}
