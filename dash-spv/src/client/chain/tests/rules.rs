//! Rule 2: Core's contextual header checks on the first header after the anchor, read from the
//! anchor's context records (`ContextualCheckBlockHeader`, `validation.cpp:4483-4527`). Each is
//! rejected with Core's result and reason and scored with rule 1's weight; nothing is inserted
//! and the peer is kept.

use dash_chain::HeaderStop;
use dash_consensus::pow::{get_next_work_required, PowParams};
use dash_consensus::BlockValidationResult as R;
use dash_types::Numeric;

use super::step::{bridged, mine, mine_bits, PEER, POW_LIMIT_BITS, SPACING};
use super::*;

const T: u32 = 1_000_000;
const NOW: u32 = 2_000_000;

/// The median time past at the anchor: the pack's last 11 headers are 150 s apart and end at
/// `T`, so their median is `T - 5 * 150`.
const MTP: u32 = T - 750;

/// The bits Core's `GetNextWorkRequired` asks of a header at `time` on the anchor: DGW over its
/// context records, when `time` is too early for devnet's minimum-difficulty rule.
fn bits_at(time: u32) -> u32 {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(false, None);
    let (chain, _) = open_chain(&mem(&dir), &s).unwrap();
    let (anchor, _) = chain.private_tip().unwrap();
    let pow = PowParams::from_chain_params(s.llmq.params()).unwrap();
    get_next_work_required(&chain, anchor.id, time, &pow).to_base()
}

#[tokio::test]
async fn contextual_checks_right_after_the_anchor() {
    let anchor = pack(20000, T);
    let early = bits_at(MTP);
    assert_ne!(early, POW_LIMIT_BITS, "DGW, not the minimum-difficulty rule");
    let after = mine(anchor.hash, T, 1, 4);
    // One second past the 11-record median, so a median over fewer records rejects it.
    let past_mtp = mine_bits(anchor.hash, MTP + 1 - SPACING, 1, 5, bits_at(MTP + 1));
    let cases = [
        // More than four spacings after the anchor, devnet asks powLimit (`pow.cpp:197`).
        (
            mine_bits(anchor.hash, T, 1, 1, POW_LIMIT_BITS - 1),
            R::InvalidHeader,
            "bad-diffbits",
            100,
            &after,
        ),
        (
            mine_bits(anchor.hash, MTP - SPACING, 1, 2, early),
            R::InvalidHeader,
            "time-too-old",
            100,
            &past_mtp,
        ),
        // `MAX_FUTURE_BLOCK_TIME` (`chain.h:24`) past the injected clock, and one second more.
        (mine(anchor.hash, NOW + 7_201 - SPACING, 1, 3), R::TimeFuture, "time-too-new", 0, &after),
    ];
    for (headers, result, reason, weight, next) in cases {
        let dir = tempfile::tempdir().unwrap();
        let h = mem(&dir);
        let s = setup(false, None);
        let host = SpvChainHost::open_on(h.clone(), s.clone()).await.unwrap();
        let r = host.handle().headers(PEER, bridged(&headers)).await.unwrap();
        let Some(HeaderStop::Invalid {
            index: 0,
            reject,
        }) = &r.stop
        else {
            panic!("{reason}: {:?}", r.stop);
        };
        assert_eq!((reject.result(), reject.reject_reason()), (result, reason));
        let want = HeaderMisbehaviour {
            result: Some(result),
            reason: reason.into(),
            weight,
        };
        assert_eq!(r.misbehaviour, Some(want), "{reason}");
        assert!(h.by_hash(&headers[0].hash()).unwrap().is_none(), "{reason}: inserted");
        assert_eq!(r.snap.tip.tip_hash, anchor.hash, "{reason}");

        let r = host.handle().headers(PEER, bridged(next)).await.unwrap();
        assert_eq!((r.stop, r.misbehaviour), (None, None), "{reason}: the peer is kept");
        assert_eq!(r.snap.tip.tip_hash, next[0].hash(), "{reason}");
        assert!(!s.fail.is_tripped());
    }
}
