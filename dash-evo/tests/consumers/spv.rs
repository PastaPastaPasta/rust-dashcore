//! SPV (`dash-mnlist`, SB8/SB11) against the shared error contract (EV addendum §2.1-§2.5): a
//! toy SML list behind LQ1's views, a list source with a node-style and an SPV-style error, a
//! stub member computation, and a stub rotation predicate over an anchored chain view. Only
//! `dash_evo` is named, as `dash-mnlist` may name nothing else from EV0 (EV addendum §1.4).

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_evo::llmq::{
    AddMnRejected, CompleteChain, ListCounts, LlmqChain, MemberError, MnEntryView, MnListView,
    QuorumMembers, ScratchMnList,
};
use dash_evo::testkit::{MemChain, pro_tx};
use dash_evo::types::ProTxHash;
use dash_primitives::{OutPoint, TxHash};

// ---- A toy SML list behind LQ1's views (SB8 implements the real one) ----------------------------

/// An SML entry: no collateral (the null outpoint, LQ1 §2.2.4), validity as `isValid`.
#[derive(Clone, Debug)]
pub struct SmlEntry {
    pro_tx_hash: ProTxHash,
    collateral: OutPoint,
    is_evo: bool,
    is_valid: bool,
    confirmed_hash_with_protx: [u8; 32],
}

impl SmlEntry {
    fn new(n: u8, is_valid: bool) -> Arc<SmlEntry> {
        Arc::new(SmlEntry {
            pro_tx_hash: pro_tx(n),
            collateral: OutPoint {
                hash: TxHash::from([0u8; 32]),
                index: u32::MAX,
            },
            is_evo: false,
            is_valid,
            confirmed_hash_with_protx: [n; 32],
        })
    }
}

impl MnEntryView for SmlEntry {
    fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }
    fn collateral(&self) -> &OutPoint {
        &self.collateral
    }
    fn is_evo(&self) -> bool {
        self.is_evo
    }
    fn is_banned(&self) -> bool {
        !self.is_valid
    }
    fn confirmed_hash_is_null(&self) -> bool {
        false
    }
    fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
        &self.confirmed_hash_with_protx
    }
}

/// An SML list; its entries are `Arc<SmlEntry>` through the shared forwarding impl.
#[derive(Default)]
pub struct SmlList {
    entries: BTreeMap<ProTxHash, Arc<SmlEntry>>,
}

impl SmlList {
    fn of(entries: Vec<Arc<SmlEntry>>) -> SmlList {
        SmlList {
            entries: entries.into_iter().map(|e| (e.pro_tx_hash, e)).collect(),
        }
    }
}

fn for_each_sml(
    entries: &BTreeMap<ProTxHash, Arc<SmlEntry>>,
    only_valid: bool,
    f: &mut dyn FnMut(&Arc<SmlEntry>),
) {
    entries.values().filter(|e| !only_valid || e.is_valid).for_each(f);
}

impl MnListView for SmlList {
    type Entry = Arc<SmlEntry>;
    type Scratch = SmlList;
    fn counts(&self) -> ListCounts {
        ListCounts {
            total: self.entries.len(),
            enabled: self.entries.values().filter(|e| e.is_valid).count(),
        }
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.entries.contains_key(p)
    }
    fn is_pose_banned(&self, p: &ProTxHash) -> bool {
        self.entries.get(p).is_some_and(|e| !e.is_valid)
    }
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Arc<SmlEntry>)) {
        for_each_sml(&self.entries, only_valid, f);
    }
    fn new_scratch(&self) -> SmlList {
        SmlList::default()
    }
}

/// `try_add` checks the proTxHash only here; SB8's checks service and operator key too.
impl ScratchMnList for SmlList {
    type Entry = Arc<SmlEntry>;
    fn try_add(&mut self, e: &Arc<SmlEntry>) -> Result<(), AddMnRejected> {
        if self.entries.contains_key(&e.pro_tx_hash) {
            return Err(AddMnRejected);
        }
        self.entries.insert(e.pro_tx_hash, Arc::clone(e));
        Ok(())
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.entries.contains_key(p)
    }
    fn total(&self) -> usize {
        self.entries.len()
    }
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Arc<SmlEntry>)) {
        for_each_sml(&self.entries, only_valid, f);
    }
}

// ---- The two products' errors and their mappings (EV addendum §2.3) -----------------------------

/// SB11's `dash_mnlist::MnViewError`, in shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MnViewError {
    /// History below the anchored client's floor: no backfill, the anchor never moves.
    BelowFloor {
        height: u32,
    },
    /// An input not yet received (a list, a qrinfo snapshot): requested like `Missing`.
    NotHeld {
        at: BlockId,
    },
    /// The `dash-kv` latch has already tripped.
    Storage,
}

/// A node-style error, standing in for `dash_dml::StoreError` (this crate cannot name it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeStyleError(&'static str);

/// What SPV does with a member error (EV addendum §2.3, SPV column).
#[derive(Debug, PartialEq, Eq)]
pub enum SpvOutcome {
    /// Request the input for the commitment being verified; the quorum stays `Unverified`.
    Request(BlockId),
    /// `Unavailable`, with no request (and SB0 §4.3.1's diagnostic).
    Unavailable,
    /// The latch has tripped: stop.
    Stop,
    /// `MnReject` for the diff that carried an unknown type.
    Reject,
}

pub fn spv_outcome(e: MemberError<MnViewError>) -> SpvOutcome {
    match e {
        MemberError::Missing(at)
        | MemberError::View(MnViewError::NotHeld {
            at,
        }) => SpvOutcome::Request(at),
        MemberError::View(MnViewError::BelowFloor {
            ..
        }) => SpvOutcome::Unavailable,
        MemberError::View(MnViewError::Storage) => SpvOutcome::Stop,
        MemberError::UnknownLlmqType(_) => SpvOutcome::Reject,
    }
}

/// The node's reading (`dash_dml::NodeMemberError`): `Missing` and `View` are both fatal (D4).
pub fn node_is_fatal(e: &MemberError<NodeStyleError>) -> bool {
    matches!(e, MemberError::Missing(_) | MemberError::View(_))
}

// ---- A list source and a stub member computation (LQ1a's `LlmqSource` in shape) -----------------

/// `LlmqSource::mn_list`: `Ok(None)` when the view does not hold the list.
pub trait ListSource {
    type Error;
    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<SmlList>>, Self::Error>;
}

/// Holds lists by block; answers the fixed error for blocks listed in `failing`.
pub struct MapSource<E> {
    lists: BTreeMap<BlockId, Arc<SmlList>>,
    failing: BTreeMap<BlockId, E>,
}

impl<E: Clone> ListSource for MapSource<E> {
    type Error = E;
    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<SmlList>>, E> {
        if let Some(e) = self.failing.get(&at) {
            return Err(e.clone());
        }
        Ok(self.lists.get(&at).cloned())
    }
}

/// A stub member computation: the valid entries of the base's list. One error type flows
/// through the call; a list the view does not hold is `Missing(base)`.
pub fn stub_members<S: ListSource>(
    source: &S,
    base: BlockId,
) -> Result<QuorumMembers<Arc<SmlEntry>>, MemberError<S::Error>> {
    let list =
        source.mn_list(base).map_err(MemberError::View)?.ok_or(MemberError::Missing(base))?;
    let mut members = Vec::new();
    MnListView::for_each(&*list, true, &mut |e| members.push(Arc::clone(e)));
    Ok(members.into())
}

// ---- An anchored chain view and a stub rotation predicate (EV addendum §2.2, §2.5) --------------

/// An anchored client's chain: it holds the blocks at or above `floor` except the `withheld`
/// heights (not fetched yet), and errs for the rest, never answering `Ok(None)` for a block it
/// lacks (the §2.2 contract).
pub struct AnchoredChain<'a> {
    chain: &'a MemChain,
    floor: u32,
    withheld: &'a [u32],
}

impl LlmqChain for AnchoredChain<'_> {
    type Error = MnViewError;
    fn header(&self, id: BlockId) -> Result<HeaderCtx, MnViewError> {
        let header = self.chain.header(id);
        if header.height < self.floor {
            return Err(MnViewError::BelowFloor {
                height: header.height,
            });
        }
        if self.withheld.contains(&header.height) {
            return Err(MnViewError::NotHeld {
                at: id,
            });
        }
        Ok(header)
    }
    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, MnViewError> {
        let top = self.header(id)?.height;
        if height > top {
            return Ok(None); // Core's null GetAncestor
        }
        if height < self.floor {
            return Err(MnViewError::BelowFloor {
                height,
            });
        }
        let block = self.chain.ancestor(id, height);
        match block {
            Some(at) if self.withheld.contains(&height) => Err(MnViewError::NotHeld {
                at,
            }),
            _ => Ok(block),
        }
    }
    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, MnViewError> {
        match self.header(id)?.height {
            0 => Ok(None), // actual genesis
            h => self.ancestor(id, h - 1),
        }
    }
}

/// `IsQuorumRotationEnabled` in shape (`llmq/options.cpp` v24:50-61): Core's null branch for a
/// cycle base below 1 is taken without a view call; otherwise the cycle predecessor is read, and
/// a stub deployment rule (`active_from`) stands in for `DeploymentActiveAfter(V20)`. A view
/// error is `View(e)`, never `Ok(false)`.
pub fn stub_rotation_enabled<C: LlmqChain + ?Sized>(
    chain: &C,
    at: BlockId,
    cycle_base: i64,
    active_from: u32,
) -> Result<bool, MemberError<C::Error>> {
    if cycle_base < 1 {
        return Ok(false);
    }
    let predecessor = u32::try_from(cycle_base - 1).expect("cycle bases are block heights");
    let Some(block) = chain.ancestor(at, predecessor).map_err(MemberError::View)? else {
        return Ok(false);
    };
    Ok(chain.header(block).map_err(MemberError::View)?.height >= active_from)
}

// ---- Checks -------------------------------------------------------------------------------------

fn source<E>(lists: Vec<(BlockId, SmlList)>, failing: Vec<(BlockId, E)>) -> MapSource<E> {
    MapSource {
        lists: lists.into_iter().map(|(at, l)| (at, Arc::new(l))).collect(),
        failing: failing.into_iter().collect(),
    }
}

#[test]
fn an_sml_list_is_an_lq1_list_view() {
    let list = SmlList::of(vec![SmlEntry::new(1, true), SmlEntry::new(2, false)]);
    assert_eq!(
        list.counts(),
        ListCounts {
            total: 2,
            enabled: 1
        }
    );
    assert!(list.is_pose_banned(&pro_tx(2)) && !list.is_pose_banned(&pro_tx(3)));
    let mut scratch = list.new_scratch();
    let entry = SmlEntry::new(1, true);
    assert!(scratch.try_add(&entry).is_ok());
    assert!(scratch.try_add(&entry).is_err(), "AddMN rejects a duplicate proTxHash");
    assert_eq!(scratch.total(), 1);
}

/// EV addendum §2.4, shared part: a source whose `mn_list(at)` is `Ok(None)` yields
/// `Missing(at)` in both products, which they read differently.
#[test]
fn a_list_not_held_is_missing_in_both_products() {
    let (held, absent) = (BlockId(1), BlockId(2));
    let list = || SmlList::of(vec![SmlEntry::new(1, true), SmlEntry::new(2, false)]);

    let node = source::<NodeStyleError>(vec![(held, list())], vec![]);
    assert_eq!(stub_members(&node, held).unwrap().len(), 1);
    let err = stub_members(&node, absent).unwrap_err();
    assert_eq!(err, MemberError::Missing(absent));
    assert!(node_is_fatal(&err));

    let spv = source::<MnViewError>(vec![(held, list())], vec![]);
    assert_eq!(stub_members(&spv, held).unwrap()[0].pro_tx_hash(), &pro_tx(1));
    let err = stub_members(&spv, absent).unwrap_err();
    assert_eq!(err, MemberError::Missing(absent));
    assert_eq!(spv_outcome(err), SpvOutcome::Request(absent));
}

/// A view error reaches the consumer's mapping unchanged.
#[test]
fn view_errors_reach_the_mapping_unchanged() {
    let cases = [
        (
            MnViewError::BelowFloor {
                height: 5,
            },
            SpvOutcome::Unavailable,
        ),
        (
            MnViewError::NotHeld {
                at: BlockId(3),
            },
            SpvOutcome::Request(BlockId(3)),
        ),
        (MnViewError::Storage, SpvOutcome::Stop),
    ];
    for (e, outcome) in cases {
        let spv = source(vec![], vec![(BlockId(3), e.clone())]);
        let err = stub_members(&spv, BlockId(3)).unwrap_err();
        assert_eq!(err, MemberError::View(e));
        assert_eq!(spv_outcome(err), outcome);
    }
    let node = source(vec![], vec![(BlockId(3), NodeStyleError("read"))]);
    let err = stub_members(&node, BlockId(3)).unwrap_err();
    assert_eq!(err, MemberError::View(NodeStyleError("read")));
    assert!(node_is_fatal(&err));
}

/// EV addendum §2.5 negative control (1), in shape ahead of LQ1a: a cycle predecessor the client
/// has not fetched is `View(NotHeld)`, and one below its floor `View(BelowFloor)`; never
/// `Ok(false)`. The positive controls are Core's values.
#[test]
fn a_withheld_predecessor_is_a_view_error_not_rotation_disabled() {
    let mut chain = MemChain::new();
    let tip = chain.extend(chain.genesis(), 40);
    let anchored = AnchoredChain {
        chain: &chain,
        floor: 20,
        withheld: &[23],
    };

    // The predecessor of cycle base 24 is at 23, not fetched yet.
    let not_held = chain.ancestor(tip, 23).unwrap();
    assert_eq!(
        stub_rotation_enabled(&anchored, tip, 24, 10),
        Err(MemberError::View(MnViewError::NotHeld {
            at: not_held
        }))
    );
    // The predecessor of cycle base 16 is at 15, below the floor.
    assert_eq!(
        stub_rotation_enabled(&anchored, tip, 16, 10),
        Err(MemberError::View(MnViewError::BelowFloor {
            height: 15
        }))
    );
    // Held predecessors give the deployment's answer.
    assert_eq!(stub_rotation_enabled(&anchored, tip, 32, 10), Ok(true));
    assert_eq!(stub_rotation_enabled(&anchored, tip, 32, 35), Ok(false));
    // Core's null branch: no view call at all, so even a view that errs for every block answers.
    let holds_nothing = AnchoredChain {
        chain: &chain,
        floor: 41,
        withheld: &[],
    };
    assert_eq!(stub_rotation_enabled(&holds_nothing, tip, 0, 0), Ok(false));
    // `prev` at the floor is an error; at actual genesis it is `Ok(None)`.
    let at_floor = chain.ancestor(tip, 20).unwrap();
    assert!(anchored.prev(at_floor).is_err());
    let full = AnchoredChain {
        chain: &chain,
        floor: 0,
        withheld: &[],
    };
    assert_eq!(full.prev(chain.genesis()), Ok(None));

    // The node's complete chain answers the same question with no error.
    let complete = CompleteChain::<'_, NodeStyleError>::new(&chain);
    assert_eq!(stub_rotation_enabled(&complete, tip, 16, 10), Ok(true));
}
