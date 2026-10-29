//! `DigestTap` on V: the log of a reorg into a block the connector rejects (O9a-2), and each
//! `D.res` the seam's disconnect results map to (O8 §4.4).

use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

use dash_consensus::{Block, BlockId};
use dash_primitives::BlockHash;
use dash_statedigest::{
    Chain, FileKind, Hash256, Header, Kind, Lines, Origin, Outcome, Record, Value, Writer,
};

use super::abc::Rejects;
use super::support::{Mined, Rig, RigConfig, branch, child, regtest};
use crate::chain::digest_tap::DigestTap;
use crate::chain::{
    BlockConnector, BlockStatusWriter, ConnectCx, ConnectOutcome, DisconnectResult, Fatal,
    NullConnector, ProposalChecks, VContext, ViewTarget, genesis_block,
};

/// The log, shared with the test after the tap moves into V.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn header() -> Header {
    Header {
        file: FileKind::Digest,
        src: Writer::Rdashd,
        ver: b"test".to_vec(),
        chain: Chain::Regtest,
        run: b"1-1".to_vec(),
    }
}

/// The records after the log's `H`, which must be [`header`]'s.
fn outcomes(out: &Shared) -> Vec<Outcome> {
    let log = out.0.lock().unwrap().clone();
    let mut records = Lines::new(&log).map(|(_, r)| r.unwrap());
    assert_eq!(records.next(), Some(Record::Header(header())));
    records
        .map(|r| match r {
            Record::Outcome(o) => o,
            other => panic!("{other:?}"),
        })
        .collect()
}

fn hash(m: &Mined) -> Hash256 {
    Hash256::from_hex(m.hash().to_string().as_bytes()).unwrap()
}

/// Chain work `2·n` (regtest's `0x207fffff` proves 2 per block), display order.
fn work(n: u8) -> Value {
    let mut bytes = [0u8; 32];
    bytes[31] = 2 * n;
    Value::hash(&Hash256::from_display_bytes(bytes))
}

#[test]
fn the_tap_logs_each_connect_and_disconnect_in_order() {
    let p = regtest();
    let genesis = Mined {
        block: genesis_block(&p),
        height: 0,
    };
    let a1 = child(&p, &genesis, 0);
    let side = branch(&p, &genesis, 3, 1);
    let out = Shared::default();
    let tap =
        DigestTap::new(Rejects(HashSet::from([side[1].hash()])), out.clone(), header()).unwrap();
    let rig = Rig::with(RigConfig {
        connector: Some(Box::new(tap)),
        ..RigConfig::default()
    });
    rig.accept(&a1);
    rig.headers(&side);
    rig.accept(&side[0]);
    rig.accept(&side[2]);
    rig.accept(&side[1]);
    rig.sync();

    let outcomes = outcomes(&out);
    let seen: Vec<(Kind, i64, Hash256)> =
        outcomes.iter().map(|o| (o.kind(), o.id.h, o.id.b)).collect();
    assert_eq!(
        seen,
        [
            (Kind::Connect, 0, hash(&genesis)),
            (Kind::Connect, 1, hash(&a1)),
            (Kind::Disconnect, 1, hash(&a1)),
            (Kind::Connect, 1, hash(&side[0])),
            (Kind::Reject, 2, hash(&side[1])),
            (Kind::Disconnect, 1, hash(&side[0])),
            (Kind::Connect, 1, hash(&a1)),
        ]
    );
    for (n, o) in outcomes.iter().enumerate() {
        assert_eq!(o.id.org, Origin::Tip);
        assert_eq!(o.x().get("seq"), Some(&Value::Int(n as i64 + 1)), "one seq for C, R, D");
        assert!(o.o().get("fx").is_none(), "effects are not observed");
    }

    let [g, c, ..] = &outcomes[..] else {
        unreachable!()
    };
    assert_eq!(g.s().get("prev"), Some(&Value::Null), "genesis");
    assert_eq!(g.s().get("work"), Some(&work(1)));
    assert_eq!(c.s().get("prev"), Some(&Value::hash(&hash(&genesis))));
    assert_eq!(c.s().get("work"), Some(&work(2)));
    let header = a1.block.header();
    for (key, value) in [
        ("ver", i64::from(header.version())),
        ("time", header.time().into()),
        ("bits", header.bits().into()),
        ("nonce", header.nonce().into()),
        ("ntx", 1),
    ] {
        assert_eq!(c.s().get(key), Some(&Value::Int(value)), "{key}");
    }
    assert_eq!(c.s().iter().count(), 8, "the header keys only");

    let r = outcomes[4].s();
    assert_eq!(r.get("res"), Some(&Value::string("invalid")));
    assert_eq!(r.get("cls"), Some(&Value::string("consensus")));
    assert_eq!(r.get("why"), Some(&Value::string("bad-cb-amount")));
    assert_eq!(outcomes[2].s().get("res"), Some(&Value::string("ok")));
}

/// The null connector, but every disconnect returns `result`.
struct Disconnects(Result<DisconnectResult, Fatal>);

impl BlockConnector for Disconnects {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        _: &ConnectCx<'_>,
        _: &mut VContext,
        _: ViewTarget<'_>,
        _: &Block,
        _: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        self.0.clone()
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        b: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, b, prev, pow, merkle)
    }
}

/// O8 §4.4 (CH3a D133): the tap's `D.res` is `unclean` for `DISCONNECT_UNCLEAN`, and `failed` for
/// `DISCONNECT_FAILED` and for an exception out of `DisconnectBlock`, as O8's patch writes them
/// (`AttemptScope::Disconnected`). The first two only end `InvalidateBlock`'s walk (`v:3966`), so
/// the tap goes on with the next connect; the `Fatal` stops V.
#[test]
fn the_tap_records_an_unclean_and_a_failed_disconnect() {
    const THROWN: &str = "DisconnectBlock threw";
    let cases = [
        (Ok(DisconnectResult::Unclean), "unclean"),
        (Ok(DisconnectResult::Failed), "failed"),
        (Err(Fatal::new(THROWN)), "failed"),
    ];
    for (result, res) in cases {
        let thrown = result.is_err();
        let p = regtest();
        let genesis = Mined {
            block: genesis_block(&p),
            height: 0,
        };
        let a1 = child(&p, &genesis, 0);
        let a2 = child(&p, &a1, 0);
        let out = Shared::default();
        let tap = DigestTap::new(Disconnects(result), out.clone(), header()).unwrap();
        let mut rig = Rig::with(RigConfig {
            connector: Some(Box::new(tap)),
            ..RigConfig::default()
        });
        rig.accept(&a1);
        let invalidated = rig.v.invalidate_block(a1.hash()).recv();
        let mut want: Vec<(Kind, i64, BlockHash)> = vec![
            (Kind::Connect, 0, genesis.hash()),
            (Kind::Connect, 1, a1.hash()),
            (Kind::Disconnect, 1, a1.hash()),
        ];
        if !thrown {
            assert_eq!(invalidated, Ok(Ok(())), "{res}");
            rig.accept(&a2);
            want.push((Kind::Connect, 2, a2.hash()));
        }
        let exit = rig.stop();
        assert_eq!(exit.fatal.map(|f| f.0), thrown.then(|| THROWN.to_owned()), "{res}");
        let outcomes = outcomes(&out);
        let seen: Vec<(Kind, i64, Hash256)> =
            outcomes.iter().map(|o| (o.kind(), o.id.h, o.id.b)).collect();
        let want: Vec<_> = want
            .iter()
            .map(|(k, h, b)| (*k, *h, Hash256::from_hex(b.to_string().as_bytes()).unwrap()))
            .collect();
        assert_eq!(seen, want, "{res}");
        let d = &outcomes[2];
        assert_eq!(d.id.org, Origin::Tip);
        assert_eq!(d.s().get("res"), Some(&Value::string(res)), "thrown {thrown}");
    }
}
