//! The J0b-1 rows' probes and controls on the quorum chain, built before any validator sees
//! them.
//!
//! Every probe but 9-OK's is the setup tip's unsubmitted template with one transaction appended
//! ([`crate::probe_block`]). A type 9 reject on it answers from `ProcessSpecialTxsInBlock`'s
//! special-tx loop (`evo/specialtxman.cpp:931-934`), which runs before the CbTx's credit-pool
//! check (`:943`); the template's CbTx does not count the unlock, so the valid unlock in the
//! same place gives `bad-cbtx-assetlocked-amount` (`:1114`): each type 9 row's pool control
//! (`b_control` of a one-defect row, `b_pool_control` of a pair), which shows that the
//! special-tx check answered first. 9-OK's block is the producer's own template with the unlock
//! from its mempool, so that Core computes its CbTx and coinbase ([`QuorumChain::mined_with`]);
//! its control is that block with the unlock's signature made by another key.
//!
//! Signatures: a valid one by the key of the quorum the payload names; a bad one is the same
//! message signed by [`STRANGER`], a key no quorum holds, so each bad case differs from its
//! valid twin in the signature alone.

use std::path::Path;
use std::sync::Arc;

use dash_consensus::pow::PowParams;
use dash_consensus::primitives::Header;
use dash_consensus::{Block, Tx};

use super::{
    AGED_OUT, FIRST_HEIGHT_QUORUM, MINED_INDEX, MNHF, NEWEST, QuorumChain, Signal, TIP, Unlock,
    sign,
};
use crate::{EHF_BIT, Probe, UNKNOWN, probe_block};

/// The key material of a key no quorum holds.
const STRANGER: u8 = 0x6d;
/// An index no unlock of the fixture used.
const UNUSED_INDEX: u64 = 7;
/// A pool control's name: the valid unlock in the probe's place (see the module docs).
const IN_PLACE: &str = "the valid unlock in its place";

/// `block` without its last transaction, `tx`.
fn without(block: &Block, tx: &Tx) -> Block {
    let mut txs = block.txs().to_vec();
    assert_eq!(txs.pop().map(|t| t.txid()), Some(tx.txid()), "the unlock is the last tx");
    Block::new(Header::new(*block.header().raw()), txs)
}

/// The J0b-1 rows' probes and controls (see the module docs).
pub fn probes(chain: &QuorumChain, binary: &Path, scratch: &Path, pow: &PowParams) -> Vec<Probe> {
    let mut out = Vec::new();
    let t = &chain.template;
    let mut add = |row: &'static str, name: &str, expect: &'static str, tx: Arc<Tx>| {
        out.push(Probe {
            row,
            name: name.to_owned(),
            expect,
            block: probe_block(t, &[tx], pow),
        })
    };
    let q = &chain.quorums;
    let stranger = sign::key(STRANGER);
    let stale = *chain.stale[0].hash().as_bytes();
    let p2pkh = &chain.p2pkh;

    // Type 7: bit 14 (evo_shares, in its window, an EHF deployment). A signal naming a block
    // that is no MNHF quorum is signed by the MNHF quorum's key over its own payload.
    let mnhf = &q[MNHF];
    let signal = |quorum: [u8; 32]| Signal {
        bit: EHF_BIT,
        quorum,
    };
    let valid = signal(mnhf.hash).signed(&mnhf.key);
    let forged = signal(mnhf.hash).signed(&stranger);
    let platform_block = q[AGED_OUT].hash;
    add("7-OK", "the MNHF quorum's signature", "b", valid.clone());
    add("7-OK", "another key's signature", "b_control", forged.clone());
    add("7-S2", "unknown quorum hash", "b", signal(UNKNOWN).signed(&mnhf.key));
    add("7-S2", "the MNHF quorum", "b_control", valid.clone());
    add("7-S3", "the stale block", "b", signal(stale).signed(&mnhf.key));
    add("7-S3", "the MNHF quorum", "b_control", valid.clone());
    add("7-S4", "a Platform quorum's base", "b", signal(platform_block).signed(&mnhf.key));
    add("7-S4", "the MNHF quorum", "b_control", valid.clone());
    add("7-S5", "another key's signature", "b", forged.clone());
    add("7-S5", "the MNHF quorum's signature", "b_control", valid);
    add("7-P2", "unknown hash, another key", "b", signal(UNKNOWN).signed(&stranger));
    add("7-P2", "the MNHF quorum, another key", "b_control", forged.clone());
    add("7-P3", "the stale block", "b", signal(stale).signed(&mnhf.key));
    add("7-P3", "a Platform quorum's base", "b_control", signal(platform_block).signed(&mnhf.key));
    add(
        "7-P4",
        "a Platform quorum's base, another key",
        "b",
        signal(platform_block).signed(&stranger),
    );
    add("7-P4", "the MNHF quorum, another key", "b_control", forged);

    // Type 9: the valid unlock is version 1, an unused index, one output, requested at the
    // parent's height, under the newest Platform quorum.
    let newest = &q[NEWEST];
    let base = Unlock {
        version: 1,
        index: UNUSED_INDEX,
        outputs: 1,
        requested: TIP,
        quorum: newest.hash,
    };
    let good = |u: Unlock| u.signed(&newest.key, p2pkh);
    let bad = |u: Unlock| u.signed(&stranger, p2pkh);
    let late = Unlock {
        requested: TIP + 1,
        ..base
    };
    let mined = Unlock {
        index: MINED_INDEX,
        ..base
    };
    let unknown = Unlock {
        quorum: UNKNOWN,
        ..base
    };
    let aged = &q[AGED_OUT];

    add("9-S1", "the mined unlock's index", "b", good(mined));
    add("9-S1", IN_PLACE, "b_control", good(base));
    add("9-S2", "unknown quorum hash", "b", good(unknown));
    add("9-S2", IN_PLACE, "b_control", good(base));
    let not_platform = Unlock {
        quorum: mnhf.hash,
        ..base
    };
    add("9-S3a", "the MNHF quorum's base", "b", not_platform.signed(&mnhf.key, p2pkh));
    add("9-S3a", IN_PLACE, "b_control", good(base));
    let aged_out = Unlock {
        quorum: aged.hash,
        ..base
    };
    add("9-S3b", "the aged-out Platform quorum", "b", aged_out.signed(&aged.key, p2pkh));
    add("9-S3b", IN_PLACE, "b_control", good(base));
    add("9-S4a", "requested one above the parent", "b", good(late));
    add("9-S4a", IN_PLACE, "b_control", good(base));
    let expired = Unlock {
        requested: TIP - 48,
        ..base
    };
    add("9-S4b", "the parent at requestedHeight + 48", "b", good(expired));
    add("9-S4b", IN_PLACE, "b_control", good(base));
    add("9-S5", "another key's signature", "b", bad(base));
    add("9-S5", IN_PLACE, "b_control", good(base));

    add(
        "9-P1",
        "the mined index, unknown hash",
        "b",
        good(Unlock {
            index: MINED_INDEX,
            ..unknown
        }),
    );
    add("9-P1", "unknown hash", "b_control", good(unknown));
    add("9-P1", IN_PLACE, "b_pool_control", good(base));
    let late_not_platform = Unlock {
        requested: TIP + 1,
        ..not_platform
    };
    add(
        "9-P3",
        "the MNHF quorum's base, too late",
        "b",
        late_not_platform.signed(&mnhf.key, p2pkh),
    );
    add("9-P3", "too late", "b_control", good(late));
    add("9-P3", IN_PLACE, "b_pool_control", good(base));
    add("9-P4", "too late, another key", "b", bad(late));
    add("9-P4", "another key", "b_control", bad(base));
    add("9-P4", IN_PLACE, "b_pool_control", good(base));

    for version in [0, 3] {
        let name = format!("payload version {version}");
        add(
            "9-P5a.3",
            &format!("{name}, too late"),
            "b",
            good(Unlock {
                version,
                ..late
            }),
        );
        add(
            "9-P5a.4",
            &format!("{name}, another key"),
            "b",
            bad(Unlock {
                version,
                ..base
            }),
        );
        add(
            "9-P5c",
            &format!("{name}, the mined index"),
            "b",
            good(Unlock {
                version,
                ..mined
            }),
        );
    }
    for version in [1, 2] {
        let name = format!("payload version {version}");
        add(
            "9-P5a.3",
            &format!("{name}, too late"),
            "b_control",
            good(Unlock {
                version,
                ..late
            }),
        );
        add(
            "9-P5a.4",
            &format!("{name}, another key"),
            "b_control",
            bad(Unlock {
                version,
                ..base
            }),
        );
        add(
            "9-P5c",
            &format!("{name}, the mined index"),
            "b_control",
            good(Unlock {
                version,
                ..mined
            }),
        );
    }
    for row in ["9-P5a.3", "9-P5a.4", "9-P5c"] {
        add(row, IN_PLACE, "b_pool_control", good(base));
    }
    let outputs = |outputs: usize, u: Unlock| Unlock {
        outputs,
        ..u
    };
    add("9-P5b.3", "33 outputs, too late", "b", good(outputs(33, late)));
    add("9-P5b.3", "32 outputs, too late", "b_control", good(outputs(32, late)));
    add("9-P5b.3", IN_PLACE, "b_pool_control", good(outputs(32, base)));
    add("9-P5b.4", "33 outputs, another key", "b", bad(outputs(33, base)));
    add("9-P5b.4", "32 outputs, another key", "b_control", bad(outputs(32, base)));
    add("9-P5b.4", IN_PLACE, "b_pool_control", good(outputs(32, base)));
    add("9-P5d", "33 outputs, the mined index", "b", good(outputs(33, mined)));
    add("9-P5d", "32 outputs, the mined index", "b_control", good(outputs(32, mined)));
    add("9-P5d", IN_PLACE, "b_pool_control", good(outputs(32, base)));

    // The 9-R rows' block halves: version 2, the parent at requestedHeight + 48, the quorum in
    // the scan at the parent; 9-R1 and 9-R3's first recipe under the quorum scanned at the
    // first height, 9-R2 and 9-R3's second under the one mined after it.
    let first = &q[FIRST_HEIGHT_QUORUM];
    let retry = |quorum: &super::Quorum| Unlock {
        version: 2,
        requested: TIP - 48,
        quorum: quorum.hash,
        ..base
    };
    let in_window = |u: Unlock| Unlock {
        requested: TIP,
        ..u
    };
    add("9-R1", "scanned at the first height", "b", retry(first).signed(&first.key, p2pkh));
    add(
        "9-R1",
        "requested at the parent",
        "b_control",
        in_window(retry(first)).signed(&first.key, p2pkh),
    );
    add("9-R2", "mined after the first height", "b", retry(newest).signed(&newest.key, p2pkh));
    add(
        "9-R2",
        "requested at the parent",
        "b_control",
        in_window(retry(newest)).signed(&newest.key, p2pkh),
    );
    for (name, quorum) in [("first-height quorum", first), ("later quorum", newest)] {
        add("9-R3", &format!("{name}, another key"), "b", bad(retry(quorum)));
        add(
            "9-R3",
            &format!("{name}, another key, requested at the parent"),
            "b_control",
            bad(in_window(retry(quorum))),
        );
    }
    add("9-R3", IN_PLACE, "b_pool_control", good(in_window(retry(newest))));

    // 9-OK: version 1 and 2, mined by the producer's own template.
    for version in [1, 2] {
        let u = Unlock {
            version,
            ..base
        };
        let tx = good(u);
        let block = chain.mined_with(binary, scratch, &tx, pow);
        let forged = probe_block(&without(&block, &tx), &[bad(u)], pow);
        out.push(Probe {
            row: "9-OK",
            name: format!("payload version {version}, the producer's block"),
            expect: "b",
            block: Arc::new(block),
        });
        out.push(Probe {
            row: "9-OK",
            name: format!("payload version {version}, another key's signature"),
            expect: "b_control",
            block: forged,
        });
    }
    out
}
