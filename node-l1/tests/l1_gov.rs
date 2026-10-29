//! `diff_gov` and `diff_gov_vote` (GV1a) against Dash Core v24's in-process oracle.

use dash_oracle_sys::Pin;
use node_l1::gov::{compare_gov, compare_gov_vote, gov_seed_inputs, gov_vote_seed_inputs};
use node_l1::{Outcome, target};

const PIN: Pin = Pin::V24_0;

#[test]
fn registered_on_v24() {
    for name in ["diff_gov", "diff_gov_vote"] {
        let t = target(name).expect("registered");
        assert_eq!(t.pins, &[PIN]);
        assert_eq!((t.compare)(&[], PIN), Ok(Outcome::OutOfDomain));
    }
}

/// Each target's public seeds (`node-fuzz/public-seeds`, written by the `gov_seeds` example)
/// are current; the census runs them.
#[test]
fn public_seeds_are_current() {
    use bitcoin_hashes::sha256;
    for (target, seeds) in
        [("diff_gov", gov_seed_inputs()), ("diff_gov_vote", gov_vote_seed_inputs())]
    {
        let dir = format!("{}/../node-fuzz/public-seeds/{target}", env!("CARGO_MANIFEST_DIR"));
        let mut names = std::collections::BTreeSet::new();
        for input in &seeds {
            let digest = sha256::Hash::hash(input).to_byte_array();
            let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            let path = format!("{dir}/{name}");
            assert_eq!(std::fs::read(&path).ok().as_ref(), Some(input), "{path}: rerun gov_seeds");
            names.insert(name);
        }
        let on_disk = std::fs::read_dir(&dir).expect("the seed directory").count();
        assert_eq!(on_disk, names.len(), "stale seeds in {dir}: rerun gov_seeds");
    }
}

/// Splitmix64 bytes from `seed`.
fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed;
    (0..n.div_ceil(8))
        .flat_map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(n)
        .collect()
}

/// Every case compares more than it skips, on the seeds and on random inputs, and reaches both
/// verdicts.
#[test]
fn census() {
    let mut counts = std::collections::BTreeMap::<(&str, u8, String), usize>::new();
    let mut seeds_skipped = 0;
    let mut run = |name: &'static str, input: &[u8], seed: bool| {
        let compare = if name == "diff_gov" {
            compare_gov
        } else {
            compare_gov_vote
        };
        let outcome = compare(input, PIN).unwrap_or_else(|d| panic!("{name} {input:02x?}: {d}"));
        seeds_skipped += usize::from(
            seed && outcome == Outcome::OutOfDomain
                && input.get(1).is_some_and(|_| name == "diff_gov_vote" || input[0] % 2 == 1),
        );
        *counts.entry((name, input[0] % 2, format!("{outcome:?}"))).or_default() += 1;
    };
    for input in gov_seed_inputs() {
        run("diff_gov", &input, true);
    }
    for input in gov_vote_seed_inputs() {
        run("diff_gov_vote", &input, true);
    }
    for seed in 0..400u64 {
        let mut input = bytes(seed, 64 + (seed as usize % 200));
        // Mostly fixture sources, so the edits are reached.
        input[2] &= 0x3f;
        // A script's byte 3 is its collateral modes, every value of which is wanted.
        if !(seed % 2 == 1 && input[0] % 2 == 1) {
            input[3] &= 0x7f;
        }
        run(
            if seed % 2 == 0 {
                "diff_gov"
            } else {
                "diff_gov_vote"
            },
            &input,
            false,
        );
    }
    // A fixture-built object, vote or script is always inside the domain.
    assert_eq!(seeds_skipped, 0, "seeds outside the domain");
    for ((name, case, outcome), n) in &counts {
        eprintln!("{name} case {case}: {outcome} {n}");
    }
    for (name, case) in
        [("diff_gov", 0), ("diff_gov", 1), ("diff_gov_vote", 0), ("diff_gov_vote", 1)]
    {
        let n = |o: &str| counts.get(&(name, case, o.to_owned())).copied().unwrap_or(0);
        assert!(n("BothAccept") > 0, "{name} case {case}: no acceptance");
        // Random scripts mostly run out of bytes; the other cases compare more than they skip.
        if (name, case) != ("diff_gov_vote", 1) {
            assert!(n("BothAccept") + n("BothReject") > n("OutOfDomain"), "{name} case {case}");
        }
    }
}

/// node 0's network replayed on both lists: Core accepts its objects and votes and funds the
/// trigger at the next superblock height, and the node's model agrees op by op.
#[test]
fn replay_reaches_tallies_and_the_superblock() {
    use dash_oracle_sys::{GovContext, GovOp, GovOutput, Oracle};
    let fx = node_l1::gov::fixture().expect("the fixture reads");
    let oracle = Oracle::new(PIN);
    for (i, v) in fx.variants.iter().enumerate() {
        let mut ops: Vec<GovOp> = v.objects.iter().map(|o| GovOp::Object(o.bytes())).collect();
        ops.extend(v.votes.iter().map(|vote| GovOp::Vote(vote.bytes())));
        ops.push(GovOp::Superblock(180));
        assert_eq!(
            node_l1::gov::compare_script(&oracle, fx, v, true, 0, &ops),
            Ok(Outcome::BothAccept),
            "variant {i}"
        );
        let ctx = dash_oracle_sys::Context::new(
            0,
            fx.headers.clone(),
            false,
            vec![(dash_oracle_sys::DEPLOYMENT_V24, dash_oracle_sys::ThresholdState::Defined)],
            fx.now,
            dash_oracle_sys::Checkpoints::Disabled,
            dash_oracle_sys::CoinsView::Unchecked(vec![]),
            dash_oracle_sys::MnList::Supplied(v.list_bytes.clone()),
            vec![],
        );
        let gov = GovContext {
            synced: true,
            collaterals: fx
                .collaterals
                .iter()
                .map(|c| dash_oracle_sys::Collateral {
                    tx: c.tx_bytes.clone(),
                    block_hash: c.block_hash,
                    active: true,
                })
                .collect(),
        };
        let out = oracle.gov_ops(&fx.chain, &ctx, &gov, &ops).expect("Core runs the replay");
        let held = out.iter().filter(|o| matches!(o, GovOutput::Object(Some(x)) if x.held)).count();
        let accepted = out.iter().filter(|o| matches!(o, GovOutput::Vote(Some(x)) if x.ok)).count();
        assert_eq!(held, v.objects.len(), "variant {i}: objects held");
        assert!(
            accepted * 10 >= v.votes.len() * 9,
            "variant {i}: {accepted} of {} votes",
            v.votes.len()
        );
        let Some(GovOutput::Superblock(sb)) = out.last() else {
            panic!("no block output")
        };
        assert!(
            sb.triggered && sb.best.is_some() && !sb.payments.is_empty(),
            "variant {i}: {sb:?}"
        );
    }
}

/// `diff_gov`'s proposal selector reaches all four chains, each its own: no selector aliases
/// another, and the devnet is a named one.
#[test]
fn proposal_selectors_reach_four_chains() {
    let names = ["main", "test", "regtest", "devnet"];
    for (sel, kind) in node_l1::gov::NETWORKS.into_iter().enumerate() {
        let (chain, p) = node_l1::gov::params_of(sel as u8);
        assert_eq!(p.chain().network, kind, "selector {sel}");
        assert_eq!(chain.name, names[sel], "selector {sel}");
        // Only the low two bits select.
        assert!(std::ptr::eq(
            node_l1::gov::params_of(sel as u8 | 4),
            node_l1::gov::params_of(sel as u8)
        ));
    }
    let (devnet, p) = node_l1::gov::params_of(3);
    assert_eq!(devnet.args, ["-devnet=gv1a"]);
    assert!(p.chain().devnet_genesis.is_some(), "a devnet's own genesis");
    // Each proposal vector is seeded on every network, and Core both accepts and rejects on each.
    let mut outcomes = std::collections::BTreeSet::new();
    for s in gov_seed_inputs().iter().filter(|s| s[0] == 0) {
        outcomes.insert((
            (s[1] >> 2) & 3,
            format!("{:?}", compare_gov(s, PIN).expect("no divergence")),
        ));
    }
    for net in 0..4 {
        for o in [Outcome::BothAccept, Outcome::BothReject] {
            assert!(outcomes.contains(&(net, format!("{o:?}"))), "selector {net}: no {o:?}");
        }
    }
}

/// `Superblock::is_valid` against Core's `CSuperblock::IsValid` on the payments fixture's mined
/// superblocks: exact, missing, reordered and duplicate outputs, a payment repeated once per
/// output and once more, the budget at and over the limit, the block value and a trigger off the
/// height, each with both V24 matching modes. Both verdicts are reached in both modes, and V24
/// splits a duplicate payment.
#[test]
fn superblock_is_valid_against_core() {
    use node_l1::gov_superblock::{compare_superblock, superblock_cases};
    let expected = |kind: &str, is_v24: bool| match kind {
        "off-height" => None,
        "exact" | "two in order" | "duplicate, two outputs" | "budget at the limit" => Some(true),
        "duplicate, one output" | "repeated per output" => Some(!is_v24),
        "repeated past the outputs" => Some(false),
        "missing" | "reordered" | "budget over the limit" | "over the block value" => Some(false),
        _ => panic!("{kind}"),
    };
    let (mut reached, mut kinds) =
        (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    for c in superblock_cases() {
        let answer = compare_superblock(&c).unwrap_or_else(|d| panic!("{}: {d}", c.name));
        assert_eq!(answer.valid, expected(&c.kind, answer.is_v24), "{}", c.name);
        reached.insert((answer.is_v24, answer.valid));
        kinds.insert(c.kind.clone());
    }
    for is_v24 in [false, true] {
        for valid in [None, Some(false), Some(true)] {
            assert!(reached.contains(&(is_v24, valid)), "is_v24 {is_v24}: no {valid:?}");
        }
    }
    // Every edit is built, the output-count branch reached on its own among them.
    let all = [
        "exact",
        "off-height",
        "missing",
        "two in order",
        "reordered",
        "duplicate, one output",
        "duplicate, two outputs",
        "repeated per output",
        "repeated past the outputs",
        "budget at the limit",
        "budget over the limit",
        "over the block value",
    ];
    assert_eq!(kinds, all.into_iter().map(str::to_owned).collect());
}

/// Collaterals 1, 5 and 6 blocks deep (`object.cpp:853-873`): Core postpones the proposals in
/// the window and accepts them at 6, and the node's records and its manager model (the
/// postponement path) agree, op by op.
#[test]
fn short_confirmations_postpone() {
    use dash_oracle_sys::{GovOp, GovOutput, Oracle};
    let oracle = Oracle::new(PIN);
    for (confirmations, missing) in [(1, true), (5, true), (6, false)] {
        let fx = node_l1::gov::fixture_confirmed(confirmations).expect("the fixture reads");
        let v = &fx.variants[0];
        let proposals: Vec<_> = v.objects.iter().filter(|o| o.obj_type == 1).collect();
        assert!(!proposals.is_empty());
        let verdict = if missing {
            Outcome::BothReject
        } else {
            Outcome::BothAccept
        };
        for o in &proposals {
            let compared =
                node_l1::gov::compare_object(&oracle, &fx, v, &o.bytes(), fx.now, true, 0);
            assert_eq!(compared, Ok(verdict), "{confirmations}");
        }
        let mut ops: Vec<GovOp> = proposals.iter().map(|o| GovOp::Object(o.bytes())).collect();
        ops.extend(proposals.iter().filter_map(|o| o.hash()).map(GovOp::State));
        assert!(
            node_l1::gov::compare_script(&oracle, &fx, v, true, 0, &ops).is_ok(),
            "{confirmations}"
        );
        // Core's own outcomes: postponed (and not held) in the window, held at 6.
        let core = oracle
            .gov_ops(&fx.chain, &fx.context(v, fx.now), &fx.gov_context(true, 0), &ops)
            .expect("Core's script");
        let objects: Vec<_> = core
            .iter()
            .filter_map(|out| match out {
                GovOutput::Object(Some(o)) => Some((o.postponed, o.held)),
                _ => None,
            })
            .collect();
        assert_eq!(objects, vec![(missing, !missing); proposals.len()], "{confirmations}");
    }
}
