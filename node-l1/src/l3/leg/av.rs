//! The AV fixture pairs (plan §7.4, M-10, Q13): three defective blocks Dᵢ, each valid but for
//! one defect, at [`CASE_BASE`] on the leg's own setup chain P (D49's arguments, `-par=1`).
//! - **AV-off** (`-assumevalid=0`) builds P and submits Dᵢ: Core refuses it on the P2
//!   ConnectBlock path (D1 CheckInputScripts#2, D2/D3 ProcessSpecialTxsInBlock#2), a trial
//!   bound and classed like any other. Its ops hold P's and Dᵢ's exact bytes.
//! - **AV-on** (`-assumevalid=Aᵢ`) replays those bytes: P, then the clock to `time(Aᵢ)`, Dᵢ's
//!   header and [`DESCENDANTS`] more ([`descendants`]) as header ops on both sides, the checks
//!   that make Core skip Dᵢ's scripts (v:2553-2580), and Dᵢ's bytes, which must connect.
//!
//! Both are census legs (§3.6): every body's gates as everywhere; a header op's body predicates
//! are n/a. Each header's parent state is recorded, never asserted (M-13 deviation, OD-L3-25 (a)):
//! dashd's `getdeploymentinfo <parent>` where it answers, else its error as the observation (from
//! V20 on Core reads the parent's block for its EHF signals and throws on a headers-only one,
//! mnhftx.cpp:302-304); V's side is [`V_PARENT_STATE`]. Proposals on the AV-on fixtures
//! (`duplicate-inconclusive` once Dᵢ's header is known) are not tested.

use serde_json::{Value, json};

use dash_params::ChainSetup;
use dash_types::Numeric as _;

use super::*;
use crate::chain::mutate::{AV_OPERATORS, MNHF_OPERATORS};

/// V's side of a header's parent state (OD-L3-25 (a)): V reports deployments in its post, at
/// its tip only, and has no public query at another block.
pub const V_PARENT_STATE: &str = "n/a: V reports deployments at its tip only";

/// Descendant headers on each Dᵢ: more than two weeks of proof-equivalent time at 150 s.
pub const DESCENDANTS: u32 = 8_100;
/// Two weeks in seconds: Core skips scripts only past this proof-equivalent time (v:2578).
pub const TWO_WEEKS: i64 = 60 * 60 * 24 * 7 * 2;

/// Plan §7.4's defects, and OD-L3-18's MNHF witness bodies (plan §9), which run as AV-off
/// fixtures do but on the census arguments alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Defect {
    /// A mandatory script failure in one transaction.
    D1,
    /// A raw type-5 coinbase of version 1.
    D2,
    /// Of version 2.
    D3,
    /// A pre-DIP3 body with one Core-mined MNHF signal.
    Signal,
    /// With two for one bit: an extraction error.
    Duplicate,
}

impl Defect {
    pub const ALL: [Defect; 3] = [Defect::D1, Defect::D2, Defect::D3];
    pub const MNHF: [Defect; 2] = [Defect::Signal, Defect::Duplicate];

    pub fn name(self) -> &'static str {
        match self {
            Defect::D1 => "d1",
            Defect::D2 => "d2",
            Defect::D3 => "d3",
            Defect::Signal => "mnhf-signal",
            Defect::Duplicate => "mnhf-dup",
        }
    }

    fn is_av(self) -> bool {
        Defect::ALL.contains(&self)
    }

    /// Its case: the operator and variant that build it, at [`CASE_BASE`].
    pub fn case(self, par: Par) -> Case {
        let (op, variant) = match self {
            Defect::D1 => (&AV_OPERATORS[0], &AV_OPERATORS[0].variants[0]),
            Defect::D2 => (&AV_OPERATORS[1], &AV_OPERATORS[1].variants[0]),
            Defect::D3 => (&AV_OPERATORS[1], &AV_OPERATORS[1].variants[1]),
            Defect::Signal => (&MNHF_OPERATORS[0], &MNHF_OPERATORS[0].variants[0]),
            Defect::Duplicate => (&MNHF_OPERATORS[0], &MNHF_OPERATORS[0].variants[1]),
        };
        Case {
            id: CaseId {
                op: op.name,
                variant: variant.name,
                site: Site::Reg,
                par,
            },
            op,
            variant,
            height: CASE_BASE,
            o10: None,
        }
    }
}

/// What an AV-on fixture replays, from its AV-off fixture.
#[derive(Clone, Debug)]
pub struct Recorded {
    /// P: the setup blocks 1..[`CASE_BASE`], by height from 1.
    pub prefix: Vec<Vec<u8>>,
    /// Dᵢ's bytes.
    pub block: Vec<u8>,
    /// Its descendant headers ([`descendants`]); Aᵢ is the last.
    pub headers: Vec<[u8; 80]>,
}

impl Recorded {
    /// Aᵢ: the last descendant header's hash.
    pub fn assumed_valid(&self) -> Hash {
        header_hash(self.headers.last().expect("descendant headers"))
    }

    /// From an AV-off fixture's log: its setup blocks and its one trial's block, then
    /// [`descendants`] on that block.
    pub fn from_off(log: &LegLog) -> Result<Recorded, String> {
        let [trial] = log.trials.as_slice() else {
            return err(format!("{}: not one trial", log.name));
        };
        let blocks = |range: std::ops::Range<usize>| {
            log.ops[range].iter().filter_map(|o| match &o.op {
                ChainOp::Block(bytes) => Some(bytes.clone()),
                _ => None,
            })
        };
        let prefix: Vec<Vec<u8>> = blocks(0..trial.op).collect();
        let block = blocks(trial.op..trial.op + 1).next().ok_or("the trial is not a block")?;
        if prefix.len() as u32 != CASE_BASE - 1 {
            return err(format!("{}: {} setup blocks before Dᵢ", log.name, prefix.len()));
        }
        Ok(Recorded {
            prefix,
            headers: descendants(&block, DESCENDANTS)?,
            block,
        })
    }
}

/// An AV fixture: its defect, and for AV-on what it replays.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub defect: Defect,
    pub on: Option<Recorded>,
}

impl Fixture {
    pub fn name(&self) -> String {
        if !self.defect.is_av() {
            return self.defect.name().to_owned();
        }
        let side = if self.on.is_some() {
            "on"
        } else {
            "off"
        };
        format!("av-{side}-{}", self.defect.name())
    }

    /// `-assumevalid`: Aᵢ (Core's display order) on, 0 off; none for an MNHF witness body.
    pub fn assumevalid_arg(&self) -> Option<String> {
        if !self.defect.is_av() {
            return None;
        }
        Some(match &self.on {
            Some(on) => format!("-assumevalid={}", hex(&on.assumed_valid())),
            None => "-assumevalid=0".to_owned(),
        })
    }
}

/// `count` headers on `block`'s, each 150 s after its parent, with `block`'s version and bits,
/// a merkle root naming its index, and the first nonce that meets the bits.
pub fn descendants(block: &[u8], count: u32) -> Result<Vec<[u8; 80]>, String> {
    let d = BlockPlan::from_block(block)?;
    let mut prev = d.build().hash;
    let mut out = Vec::with_capacity(count as usize);
    for k in 1..=count {
        let mut root = [0u8; 32];
        root[..6].copy_from_slice(b"l3-av-");
        root[6..10].copy_from_slice(&k.to_le_bytes());
        let header = BlockPlan {
            prev,
            version: d.version,
            time: d.time + 150 * k,
            bits: d.bits,
            txs: Vec::new(),
            merkle_override: Some(root),
        }
        .build();
        prev = header.hash;
        out.push(header.header);
    }
    Ok(out)
}

/// The header ops' parent states (OD-L3-25 (a)), recorded and not asserted: dashd's
/// `getdeploymentinfo <parent>` (each deployment's `active` and BIP9 `status`) or its error,
/// V's [`V_PARENT_STATE`], in runs of headers with one observation.
#[derive(Default)]
struct ParentStates {
    recorded: u32,
    unreadable: u32,
    runs: Vec<(u32, u32, String)>,
}

impl ParentStates {
    fn push(&mut self, height: u32, state: &Outcome) {
        let seen = match state {
            Ok(info) => {
                self.recorded += 1;
                let deployments = info["deployments"].as_object().into_iter().flatten();
                let digest: serde_json::Map<String, Value> = deployments
                    .map(|(name, d)| (name.clone(), json!([d["active"], d["bip9"]["status"]])))
                    .collect();
                format!("recorded: {}", Value::Object(digest))
            }
            Err(e) => {
                self.unreadable += 1;
                match e.message.contains("failed-getehfforblock-read") {
                    true => {
                        format!("unreadable: {} (headers-only ancestor; mnhftx.cpp:304)", e.message)
                    }
                    false => format!("unreadable: {} (code {:?})", e.message, e.code),
                }
            }
        };
        match self.runs.last_mut() {
            Some((_, to, last)) if *last == seen && *to + 1 == height => *to = height,
            _ => self.runs.push((height, height, seen)),
        }
    }

    fn report(&self) -> Value {
        let headers = self.recorded + self.unreadable;
        json!({
            "headers": headers,
            "recorded": self.recorded,
            "unreadable": self.unreadable,
            "n/a": headers,
            "dashd": self.runs.iter().map(|(from, to, seen)| json!({"from": from, "to": to, "parent_state": seen})).collect::<Vec<_>>(),
            "v": V_PARENT_STATE,
            "body_predicates": "n/a",
            "deviation": "M-13 (OD-L3-25 (a)): header ops' parent state recorded, not compared",
        })
    }
}

/// Core's `GetBlockProofEquivalentTime(Aᵢ, Dᵢ, Aᵢ)` over the header line `line` (Dᵢ first):
/// every header's work from its bits, summed.
pub fn proof_equivalent_time(line: &[[u8; 80]], params: &ChainSetup) -> Result<i64, String> {
    use dash_consensus::HeaderCtx;
    use dash_consensus::pow::{PowParams, block_proof, block_proof_equivalent_time};
    let pow = PowParams::from_chain_params(params).map_err(|e| format!("pow params: {e:?}"))?;
    let ctx = |head: &[u8; 80], height: u32, chain_work| HeaderCtx {
        height,
        hash: dash_primitives::BlockHash::from(header_hash(head)),
        time: u32::from_le_bytes([head[68], head[69], head[70], head[71]]),
        bits: dash_num::CompactTarget::new(u32::from_le_bytes([
            head[72], head[73], head[74], head[75],
        ])),
        version: i32::from_le_bytes([head[0], head[1], head[2], head[3]]),
        mtp: 0,
        chain_work,
    };
    let (first, last) = (line.first().ok_or("no headers")?, line.last().ok_or("no headers")?);
    let mut work = dash_num::Arith256::ZERO;
    let from = ctx(first, 0, work);
    for head in &line[1..] {
        work = work + block_proof(ctx(head, 0, work).bits);
    }
    let to = ctx(last, line.len() as u32 - 1, work);
    Ok(block_proof_equivalent_time(&to, &from, &to, &pow))
}

impl Leg<'_> {
    /// The fixture's run: P to [`CASE_BASE`] − 1 (built off, replayed on), then Dᵢ.
    pub(super) fn fixture(&mut self, fixture: &Fixture, log: &mut LegLog) -> Result<(), String> {
        let case = self.cases[0];
        for h in 1..case.height {
            self.clock(T0 + 150 * i64::from(h), log)?;
            let recorded = fixture.on.as_ref().map(|on| on.prefix[h as usize - 1].as_slice());
            self.setup_block(h, recorded, log)?;
        }
        match &fixture.on {
            None => {
                self.clock(T0 + 150 * i64::from(case.height), log)?;
                self.trial(&case, 0, Kind::Cold, log)
            }
            Some(on) => self.av_on(&case, on, log),
        }
    }

    fn clock(&mut self, mocktime: i64, log: &mut LegLog) -> Result<(), String> {
        self.mocktime = mocktime;
        self.rpc("setmocktime", json!([mocktime]))?;
        self.step(ChainOp::SetTime(mocktime), None, Ok(Value::Null), log)?;
        Ok(())
    }

    /// AV-on at Dᵢ's height: the clock to `time(Aᵢ)`, Dᵢ's header and its descendants, the
    /// skip's preconditions on both sides, then Dᵢ's bytes as a trial that must connect.
    fn av_on(&mut self, case: &Case, on: &Recorded, log: &mut LegLog) -> Result<(), String> {
        let name = &log.name.clone();
        let plan = BlockPlan::from_block(&on.block)?;
        let built = plan.build();
        if built.bytes != on.block {
            return err(format!("{name}: Dᵢ does not rebuild to its bytes"));
        }
        let h = case.height;
        let prev = self.model.tip();
        if plan.prev != prev || self.height != h - 1 {
            return err(format!("{name}: Dᵢ is not on P's tip"));
        }
        let a = *on.headers.last().ok_or("no descendant headers")?;
        let a_time = u32::from_le_bytes([a[68], a[69], a[70], a[71]]);
        self.clock(i64::from(a_time), log)?;
        let line: Vec<[u8; 80]> = std::iter::once(built.header).chain(on.headers.clone()).collect();
        let mut parent = prev;
        let mut dip3_from = None;
        let mut states = ParentStates::default();
        for (k, head) in (0u32..).zip(&line) {
            let hash = header_hash(head);
            let state = self.reply("getdeploymentinfo", json!([hex(&parent)]))?;
            if let Ok(info) = &state
                && info["deployments"]["dip0003"]["active"] == true
                && dip3_from.is_none()
            {
                dip3_from = Some(h + k);
            }
            states.push(h + k, &state);
            let dashd = self.reply("submitheader", json!([to_hex(head)]))?;
            let (step, _, delta) = self.step(ChainOp::Header(*head), None, dashd.clone(), log)?;
            if dashd != Ok(Value::Null) {
                return err(format!("{name}: header {} was refused: {dashd:?}", h + k));
            }
            // OD-L3-24's tripwire; and a header op runs no body (§3.6).
            if !step.reply.checked.is_empty() {
                return err(format!("{name}: V raised BlockChecked for header {}", h + k));
            }
            if delta.values().any(|d| *d != 0) {
                return err(format!(
                    "{name}: header {} moved V's E-evo counters: {delta:?}",
                    h + k
                ));
            }
            self.model.header(hash, parent);
            parent = hash;
        }
        let a_hash = header_hash(&a);
        let top = h + DESCENDANTS;
        let info = self.rpc("getblockchaininfo", json!([]))?;
        if info["headers"] != top || self.post.best_header != Some(Some(block_at(top, a_hash))) {
            return err(format!(
                "{name}: best headers dashd {} {}, V {:?}; expected {top} {}",
                info["headers"],
                info["bestblockhash"],
                self.post.best_header,
                hex(&a_hash)
            ));
        }
        let tips = self.rpc("getchaintips", json!([]))?;
        let row = planes::tip_row(&tips, &a_hash).ok_or(format!("{name}: Aᵢ is no chain tip"))?;
        if row["status"] != "headers-only"
            || row["height"] != top
            || row["branchlen"] != top - h + 1
        {
            return err(format!("{name}: Aᵢ's chain tip row {row}"));
        }
        let header = self.rpc("getblockheader", json!([hex(&a_hash)]))?;
        let minimum = to_hex(&self.v.params().consensus.minimum_chain_work.to_bendian());
        let work = header["chainwork"].as_str().unwrap_or_default();
        if work.len() != minimum.len() || work < minimum.as_str() {
            return err(format!("{name}: Aᵢ's chainwork {work} is below {minimum}"));
        }
        let equivalent = proof_equivalent_time(&line, self.v.params())?;
        if equivalent <= TWO_WEEKS {
            return err(format!(
                "{name}: {equivalent} s of proof-equivalent time, not > two weeks"
            ));
        }
        log.av = Some(json!({
            "assumevalid": hex(&a_hash),
            "headers": line.len(),
            "best_header": [top, hex(&a_hash)],
            "chainwork": work,
            "minimum_chain_work": minimum,
            "proof_equivalent_time": equivalent,
            "dip0003_active_from_header": dip3_from,
            "header_gates": states.report(),
            "proposals": "not tested (duplicate-inconclusive once Dᵢ's header is known)",
        }));
        let trial = TrialId {
            case: case.id,
            kind: Kind::Cold,
        };
        let op = Op {
            trial,
            plan: &plan,
            built: &built,
            mode: Mode::Submit,
            want: Expect::Accept,
            height: h,
            prev,
            mtp: self.mediantime(&hex(&prev))?,
        };
        // Its acceptance is the fixture's end: no rollback.
        self.execute(&op, log)?.ok_or(format!("{name}: Dᵢ did not connect"))?;
        if self.model.tip() != built.hash {
            return err(format!("{name}: Dᵢ is not the model's tip"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The MNHF bodies' transactions are Core's (the fixture's), rebuilt byte for byte, and
    /// `extract_signals` finds one signal in `mnhf-signal`'s body and fails on `mnhf-dup`'s.
    #[test]
    fn the_mnhf_bodies_carry_cores_signal() {
        let first = &crate::chain::mutate::mnhf_signals()[0];
        let signal = TxPlan::decode(&first.hex).expect("the recorded signal decodes");
        assert_eq!(dash_consensus::core_serialize(&*signal.tx()), first.hex, "Core's bytes");
        for (copies, want) in [(1, Ok(vec![first.version_bit as u8])), (2, Err(()))] {
            let mut txs = vec![build::coinbase(201, 0, 1)];
            txs.extend(std::iter::repeat_n(signal.clone(), copies));
            let built = BlockPlan {
                prev: [7; 32],
                version: 0x2000_0000,
                time: 1_700_030_000,
                bits: 0x207f_ffff,
                txs,
                merkle_override: None,
            }
            .build();
            let block = dash_consensus::primitives::decode_block(&mut built.bytes.as_slice())
                .expect("the body decodes")
                .into_value();
            let got = dash_dml::mnhf::extract_signals(&block);
            match want {
                Ok(bits) => assert_eq!(got.expect("one signal"), bits),
                Err(()) => {
                    let e = got.expect_err("a duplicate");
                    assert!(format!("{e:?}").contains("bad-mnhf-duplicates-in-block"), "{e:?}");
                }
            }
        }
    }

    /// OD-L3-25 (a): a parent state is recorded as dashd answers it, a thrown read as its error,
    /// never blank; equal observations on consecutive headers share a run; V is n/a throughout.
    #[test]
    fn parent_states_record_the_error_as_the_observation() {
        let mut states = ParentStates::default();
        let info = json!({"deployments": {"dip0003": {"active": true}, "testdummy": {"active": false, "bip9": {"status": "started"}}}});
        let thrown = Err(RpcError {
            code: Some(-1),
            message: "failed-getehfforblock-read".to_owned(),
        });
        for h in 201..=203 {
            states.push(h, &Ok(info.clone()));
        }
        states.push(204, &thrown);
        states.push(205, &thrown);
        let report = states.report();
        assert_eq!((report["headers"].clone(), report["recorded"].clone()), (json!(5), json!(3)));
        assert_eq!((report["unreadable"].clone(), report["n/a"].clone()), (json!(2), json!(5)));
        let runs = report["dashd"].as_array().expect("runs");
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0]["from"].clone(), runs[0]["to"].clone()), (json!(201), json!(203)));
        assert_eq!(
            runs[0]["parent_state"],
            r#"recorded: {"dip0003":[true,null],"testdummy":[false,"started"]}"#
        );
        assert_eq!(
            runs[1]["parent_state"],
            "unreadable: failed-getehfforblock-read (headers-only ancestor; mnhftx.cpp:304)"
        );
        assert_eq!(report["v"], V_PARENT_STATE);
    }

    /// The descendants chain on their block, 150 s apart, each meeting its bits; 8,100 of them
    /// are more than two weeks of proof-equivalent time on regtest.
    #[test]
    fn descendants_line_up_past_two_weeks() {
        let block = BlockPlan {
            prev: [7; 32],
            version: 0x2000_0000,
            time: 1_700_030_000,
            bits: 0x207f_ffff,
            txs: vec![build::coinbase(201, 0, 1)],
            merkle_override: None,
        }
        .build();
        let headers = descendants(&block.bytes, DESCENDANTS).expect("headers");
        assert_eq!(headers.len(), DESCENDANTS as usize);
        let mut prev = block.hash;
        for (k, head) in (1u32..).zip(&headers) {
            assert_eq!(head[4..36], prev, "header {k}'s parent");
            let time = u32::from_le_bytes([head[68], head[69], head[70], head[71]]);
            assert_eq!(time, 1_700_030_000 + 150 * k);
            assert_eq!(head[72..76], 0x207f_ffffu32.to_le_bytes());
            prev = header_hash(head);
        }
        let line: Vec<[u8; 80]> = std::iter::once(block.header).chain(headers.clone()).collect();
        let params = params::node_params(&[]).expect("regtest params");
        let t = proof_equivalent_time(&line, &params).expect("a time");
        assert_eq!(t, 150 * i64::from(DESCENDANTS));
        assert!(t > TWO_WEEKS);
        assert!(proof_equivalent_time(&line[..8000], &params).expect("a time") < TWO_WEEKS);
    }

    /// Each defect's case is its operator's at the first `Reg` height, refused off the AV
    /// path as plan §7.4 says.
    #[test]
    fn the_defects_are_the_planned_cases() {
        let reasons: Vec<_> = Defect::ALL
            .iter()
            .map(|d| match d.case(Par::One).expect(Mode::Submit) {
                Expect::Reject {
                    reason,
                    ..
                } => reason,
                other => panic!("{d:?}: {other:?}"),
            })
            .collect();
        assert_eq!(
            reasons,
            ["mandatory-script-verify-flag-failed", "bad-cbtx-payload", "bad-cbtx-payload"]
        );
        for d in Defect::ALL {
            let case = d.case(Par::One);
            assert_eq!((case.height, case.id.site), (CASE_BASE, Site::Reg));
            assert!(catalogue().all(|o| o.name != case.op.name), "{d:?} is outside the catalogue");
        }
        let off = Fixture {
            defect: Defect::D2,
            on: None,
        };
        assert_eq!(
            (off.name(), off.assumevalid_arg()),
            ("av-off-d2".into(), Some("-assumevalid=0".into()))
        );
        for d in Defect::MNHF {
            let case = d.case(Par::One);
            assert!(case.height < crate::l3::live::DIP3_HEIGHT, "{d:?} is a pre-DIP3 body");
            let fixture = Fixture {
                defect: d,
                on: None,
            };
            assert_eq!((fixture.name(), fixture.assumevalid_arg()), (d.name().into(), None));
        }
    }
}
