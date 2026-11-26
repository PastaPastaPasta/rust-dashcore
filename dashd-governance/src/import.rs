//! FR-GOV-10: Dash Core's `governance.dat` ([`GovernanceDat`], read by `dash-coreimport`) as the
//! manager's [`StoreState`]. Core's file and rdashd's own store ([`crate::store`]) share the
//! decoder and this conversion.
//!
//! - A `-17` file (v24.0.0) maps field for field.
//! - A `-16` file (v23, v24.0.0-rc.3) also maps field for field, except that every vote instance
//!   `{NONE, 0, 0}` is dropped, and then every masternode record left with no instance, whether
//!   the drop emptied it or the file held it empty. This is rdashd's declared migration policy,
//!   not Core's own V16 loading: v24.0.0 rejects a `-16` file wholesale (`governance.h:216-223`),
//!   and never holds such an instance or record, because it reads the current instance without
//!   inserting one (`object.cpp:414-419`), writes one only for an accepted vote
//!   (`object.cpp:460-473`) and erases a record its last instance leaves (`object.cpp:524-526`).
//!   Its invalid-vote cache is already discarded by the decoder.
//! - A rate record that [`RateCheckBuffer::from_parts`] refuses is repaired ([`rate_record`]),
//!   never dropped.

use std::collections::BTreeMap;
use std::fmt;

use dash_coreimport::netstate::governance::{
    GovernanceDat, GovernanceObject, GovernanceVote, LastObjectRec, OutPoint, StoreVersion,
    VoteRecord,
};
use dash_dml::MnList;
use dash_governance::manager::StoreState;
use dash_governance::object;
use dash_governance::rate::{LastObjectRecord, RATE_BUFFER_SIZE, RateCheckBuffer};
use dash_governance::tally::{ObjectState, OutpointKey, VoteFile, VoteInstance};
use dash_primitives::{GovObject, GovVote};
use dash_types::codec::BaseCodec;

/// Why a Core store did not import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// The version string is neither `-16` nor `-17`; Core loads nothing from such a file.
    UnknownVersion,
    /// An object the manager's object type cannot hold: a `-16` signature over 96 bytes.
    Object([u8; 32]),
    /// A vote of this object's vote file, or an orphan vote waiting for it, that the manager's
    /// vote type cannot hold.
    Vote([u8; 32]),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::UnknownVersion => f.write_str("governance store of an unknown version"),
            ImportError::Object(hash) => write!(f, "governance object {} unreadable", hex(hash)),
            ImportError::Vote(hash) => write!(f, "a vote for {} unreadable", hex(hash)),
        }
    }
}

impl std::error::Error for ImportError {}

fn hex(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// A converted store and the masternodes whose rate record was repaired.
#[derive(Clone, Debug, Default)]
pub(crate) struct Converted {
    pub(crate) state: StoreState,
    pub(crate) repaired: Vec<OutpointKey>,
}

/// Dash Core's store as the manager's state; see the module docs for the `-16` policy.
///
/// A file holding an object or vote the manager's types cannot hold is not imported at all: one
/// `-16` object whose signature is over 96 bytes (rc.4's limit, `common.h:37,85`) fails the whole
/// import with [`ImportError::Object`] naming it, and the node resyncs governance instead.
pub fn from_core(dat: GovernanceDat) -> Result<StoreState, ImportError> {
    convert(dat).map(|c| c.state)
}

pub(crate) fn convert(dat: GovernanceDat) -> Result<Converted, ImportError> {
    let version = dat.version.ok_or(ImportError::UnknownVersion)?;
    let mut objects = BTreeMap::new();
    for (key, o) in dat.objects {
        let state = object_state(&key, o, version)?;
        objects.insert(key, state);
    }
    let orphans = dat
        .orphan_votes
        .items
        .into_iter()
        .map(|(parent, orphan)| {
            Ok((parent, (vote(&orphan.vote).ok_or(ImportError::Vote(parent))?, orphan.expiration)))
        })
        .collect::<Result<_, _>>()?;
    let mut repaired = Vec::new();
    let last_object = dat
        .last_masternode_object
        .iter()
        .map(|(op, rec)| {
            let (record, was_repaired) = rate_record(rec);
            if was_repaired {
                repaired.push(key(op));
            }
            (key(op), record)
        })
        .collect();
    Ok(Converted {
        state: StoreState {
            erased: dat.erased,
            orphans,
            objects,
            last_object,
            last_mn_list: last_mn_list(dat.mn_list),
        },
        repaired,
    })
}

/// Core's `lastMNListForVotingKeys` starts as the default pre-DIP3 list, which the manager
/// holds as `None`: both make the first `RemoveInvalidVotes` invalidate nothing.
// Core: governance/governance.cpp:68-72,1180-1221
fn last_mn_list(list: MnList) -> Option<MnList> {
    (!list.is_default()).then_some(list)
}

fn key(op: &OutPoint) -> OutpointKey {
    (op.hash, op.n)
}

/// A rate record as the manager holds it, and whether it was repaired. A buffer
/// [`RateCheckBuffer::from_parts`] accepts is kept as it is. Otherwise the timestamps are cut or
/// zero-filled to [`RATE_BUFFER_SIZE`] slots and the parts tried again, and parts that still don't
/// fit give the constructor's empty buffer. `fStatusOK` is kept as read.
// Core: governance/governance.h:66-153 CRateCheckBuffer; governance.cpp:716-725,783-801
pub fn rate_record(rec: &LastObjectRec) -> (LastObjectRecord, bool) {
    let b = &rec.trigger_buffer;
    let keep = RateCheckBuffer::from_parts(&b.timestamps, b.data_start, b.data_end, b.buffer_empty);
    let repaired = keep.is_none();
    let buffer = keep.unwrap_or_else(|| {
        let mut timestamps = [0i64; RATE_BUFFER_SIZE];
        for (slot, t) in timestamps.iter_mut().zip(&b.timestamps) {
            *slot = *t;
        }
        RateCheckBuffer::from_parts(&timestamps, b.data_start, b.data_end, b.buffer_empty)
            .unwrap_or_default()
    });
    (
        LastObjectRecord {
            buffer,
            status_ok: rec.status_ok,
        },
        repaired,
    )
}

fn object_state(
    file_key: &[u8; 32],
    o: GovernanceObject,
    version: StoreVersion,
) -> Result<(GovObject, ObjectState), ImportError> {
    let obj = gov_object(&o).ok_or(ImportError::Object(*file_key))?;
    let votes = o
        .vote_file
        .votes
        .iter()
        .map(|v| vote(v).ok_or(ImportError::Vote(*file_key)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut state = ObjectState::new(o.object_type, object::hash(&obj));
    state.deletion_time = o.deletion_time;
    state.expired = o.expired;
    state.current = o
        .current_votes
        .iter()
        .filter_map(|(op, record)| current_record(record, version).map(|r| (key(op), r)))
        .collect();
    state.file = VoteFile::from_list(votes);
    Ok((obj, state))
}

/// One masternode's instances; `None` for a `-16` record left with no instance.
fn current_record(
    record: &VoteRecord,
    version: StoreVersion,
) -> Option<BTreeMap<i32, VoteInstance>> {
    let instances: BTreeMap<_, _> = record
        .iter()
        .filter(|(_, i)| {
            version == StoreVersion::V17
                || (i.outcome, i.last_update, i.creation_time)
                    != (dash_governance::vote::OUTCOME_NONE, 0, 0)
        })
        .map(|(signal, i)| {
            (
                *signal,
                VoteInstance {
                    outcome: i.outcome,
                    last_update: i.last_update,
                    creation_time: i.creation_time,
                },
            )
        })
        .collect();
    (version == StoreVersion::V17 || !instances.is_empty()).then_some(instances)
}

fn gov_object(o: &GovernanceObject) -> Option<GovObject> {
    let mut b = Vec::with_capacity(o.data.len() + o.signature.len() + 128);
    b.extend_from_slice(&o.parent_hash);
    b.extend_from_slice(&o.revision.to_le_bytes());
    b.extend_from_slice(&o.time.to_le_bytes());
    b.extend_from_slice(&o.collateral_hash);
    push_bytes(&mut b, &o.data);
    b.extend_from_slice(&o.object_type.to_le_bytes());
    b.extend_from_slice(&o.masternode_outpoint.hash);
    b.extend_from_slice(&o.masternode_outpoint.n.to_le_bytes());
    push_bytes(&mut b, &o.signature);
    decode_all(&b)
}

fn vote(v: &GovernanceVote) -> Option<GovVote> {
    let mut b = Vec::with_capacity(v.signature.len() + 96);
    b.extend_from_slice(&v.masternode_outpoint.hash);
    b.extend_from_slice(&v.masternode_outpoint.n.to_le_bytes());
    b.extend_from_slice(&v.parent_hash);
    b.extend_from_slice(&v.outcome.to_le_bytes());
    b.extend_from_slice(&v.signal.to_le_bytes());
    b.extend_from_slice(&v.time.to_le_bytes());
    push_bytes(&mut b, &v.signature);
    decode_all(&b)
}

fn decode_all<T: BaseCodec>(mut b: &[u8]) -> Option<T> {
    let value = T::decode(&mut b).ok()?;
    b.is_empty().then_some(value)
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    dash_types::CompactSize::from(bytes.len()).encode(out);
    out.extend_from_slice(bytes);
}
