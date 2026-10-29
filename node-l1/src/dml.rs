//! EV2a's masternode list (`dash-dml`) against Core v24's (O2a-EV, ABI 8): the oracle targets of
//! EV2 §6.3, one comparison each. `l1_dml` runs the same rules over Core's fixtures.
//!
//! Lists are mainnet-shaped, so the chain is mainnet: Core's strict canonical reader checks each
//! entry's network info against the selected network's parameters, which our codec does with
//! EV1's port of `Validate()` ([`core_net_info`]). A state outside Core's state domain, which
//! our list refuses and Core applies (D17, D20), is [`Outcome::OutOfDomain`]: no Core write site
//! produces one.
//!
//! Refusals compare as Core's text up to its first rendered value (`=…`, EV2a D3), wherever our
//! reader or list owns the text. The stream's own refusals (end of data, CompactSize range) only
//! need to be refusals.
//!
//! Input layouts:
//! - `diff_mnlist_codec`: byte 0 bit 0 selects a canonical list (0) or an evodb list (1); the
//!   rest is the list. Compared: the verdict and every output of Core's read, whole
//!   ([`compare_list_outputs`]); a canonical list's evodb bytes are then read by both.
//! - `diff_mnlist_diff`: byte 0 mod 3 selects the mode; byte 1 picks a base list from Core's
//!   fixtures (`dash-dml/tests/data/codec/lists.txt`). Mode 0: the rest is up to [`MAX_EDITS`]
//!   (op, arg) pairs that edit the base list into a second one (see [`edit`]); compared are both
//!   reads, `BuildDiff`'s two byte outputs ([`compare_diff_outputs`]), `ApplyDiff` and every
//!   state diff. Modes 1 and 2: the
//!   rest is a canonical (1) or evodb (2) diff, read and applied to the base list.
//! - `diff_mnlist_order`: up to [`MAX_OPS`] (op, arg) pairs, each an `AddMN`, `RemoveMN` or
//!   `UpdateMN` over 16 proTxHashes in two 64-bit hash collision classes (see
//!   [`compare_order`]). Compared: Core's iteration order and list, or the operation Core throws
//!   at and its text. The payee and projection legs belong to EV2b-1's payee code.
//! - `diff_unique_hash`: byte 0 selects the property ([`unique_property`]: bits 0-6 mod 6, and
//!   for an operator key bit 7 is the legacy flag); the rest is the value as Core serializes it
//!   (a service in ADDRv2 format). Compared: the verdict, the pre-image and its hash
//!   ([`compare_unique_outputs`]).

use std::sync::{Arc, OnceLock};

use bitcoin_hashes::sha256d;
use dash_dml::codec::{
    canonical_mn_list_hash, core_disk, decode_canonical_diff, decode_canonical_list,
    encode_canonical_diff, encode_canonical_list_unvalidated,
};
use dash_dml::netinfo::{CoreNetInfoValidity, NetInfoRules};
pub(crate) use dash_dml::testkit::replay::{CORE_NET_INFO, mainnet, payee_rules};
use dash_dml::testkit::{
    dmn_bytes, fresh, list_disk_bytes, numbered, operator_key, state_bytes, state_diff_bytes,
};
use dash_dml::{
    Dmn, KeyId, MnCold, MnList, MnListDiff, MnListError, MnStateDiff, MnType, OperatorKey,
    ProTxHash, UniqueKey,
};
use dash_oracle_sys::{
    CanonicalList, Chain, Error, ErrorKind, ListDiff, ListEncoding, ListOp, Oracle, Pin,
    UniqueHash, UniqueProperty,
};
use dash_pkc::bls::{BlsSchemeKind, BlsSchemeSource};
use dash_primitives::{
    AddrV1, AddrV2, BlockHash, NIEntry, NIPurpose, NetInfo, NetInfoV1, NetInfoV2, OutPoint,
    PayoutShare, PlatformNodeId, ServiceV1, ServiceV2, TxHash,
};
use dash_types::codec::BaseCodec;

use crate::{Compared, Divergence, Outcome, check, divergence};

/// Core's `Validate()` as the oracle runs it (EV1-1 D40): mainnet, every network reachable,
/// `-allowprivatenet` off.
pub fn core_net_info() -> &'static CoreNetInfoValidity {
    static NET: OnceLock<CoreNetInfoValidity> = OnceLock::new();
    NET.get_or_init(|| CoreNetInfoValidity {
        rules: NetInfoRules::new(mainnet(), mainnet(), CORE_NET_INFO),
    })
}

/// Core-built lists (provenance in that directory's README).
const LISTS: &str = include_str!("../../dash-dml/tests/data/codec/lists.txt");

/// The most edits `diff_mnlist_diff` applies to its base list.
pub const MAX_EDITS: usize = 32;
/// The most operations `diff_mnlist_order` replays.
pub const MAX_OPS: usize = 48;

/// Our refusal of bytes after a list or diff.
const TRAILING: &str = "trailing";

/// Core's `what()` without the `: iostream error` a `std::ios_base::failure` appends.
pub fn core_text(message: &str) -> &str {
    message.strip_suffix(": iostream error").unwrap_or(message)
}

/// A text up to its first rendered value (`=…`, EV2a D3).
pub fn class(text: &str) -> &str {
    text.split('=').next().unwrap_or(text)
}

/// Texts the canonical readers own, which must be Core's up to D3.
pub fn reader_text(core: &str) -> bool {
    [
        "noncanonical",
        "invalid canonical",
        "negative canonical",
        "canonical MN",
        "oversized",
        "historical",
        "Invalid data",
    ]
    .iter()
    .any(|p| core.starts_with(p))
}

pub fn canonical(list: &MnList) -> Vec<u8> {
    let mut out = Vec::new();
    encode_canonical_list_unvalidated(list, &mut out);
    out
}

pub fn canonical_diff(diff: &MnListDiff) -> Vec<u8> {
    let mut out = Vec::new();
    encode_canonical_diff(diff, &mut out);
    out
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// A list Core built, decoded, with its canonical bytes.
pub struct CoreList {
    pub name: String,
    pub list: MnList,
    pub bytes: Vec<u8>,
}

/// The lists of `lists.txt`, in file order. One that does not decode is left out, which
/// `l1_dml` would notice.
pub fn core_lists() -> &'static [CoreList] {
    static LISTS_READ: OnceLock<Vec<CoreList>> = OnceLock::new();
    LISTS_READ.get_or_init(|| {
        LISTS
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .filter_map(|l| {
                let f: Vec<&str> = l.split(' ').collect();
                let bytes = unhex(f.get(2)?)?;
                let list = decode_canonical_list(&mut bytes.as_slice(), core_net_info()).ok()?;
                Some(CoreList {
                    name: f.get(1)?.to_string(),
                    list,
                    bytes,
                })
            })
            .collect()
    })
}

/// Our canonical list reader over the whole input.
pub(crate) fn read_canonical(bytes: &[u8]) -> Result<MnList, String> {
    let mut input = bytes;
    let list = decode_canonical_list(&mut input, core_net_info()).map_err(|e| e.0)?;
    if input.is_empty() {
        Ok(list)
    } else {
        Err(TRAILING.into())
    }
}

/// Both readers' verdicts on one input agree, and what Core's says.
enum Read<T> {
    Accepted(T),
    Refused(Outcome),
}

/// Core's refusal of a canonical input against ours: alike (with Core's text where our reader
/// owns it), or a divergence.
pub(crate) fn canonical_refusals<T>(
    ours: Result<T, String>,
    core: &Error,
) -> Result<Outcome, Divergence> {
    let text = core_text(&core.message);
    match ours {
        Err(ours) if ours == text => Ok(Outcome::BothReject),
        Err(ours) if core.kind == ErrorKind::Decode => {
            if reader_text(text) {
                check(class(&ours) == class(text), "refusal")?;
            }
            Ok(Outcome::BothReject)
        }
        Err(ours) if core.kind == ErrorKind::Trailing && ours == TRAILING => {
            Ok(Outcome::BothReject)
        }
        Err(_) if core.kind == ErrorKind::Argument => Ok(Outcome::OutOfDomain),
        Err(_) => Err(divergence("oracle error")),
        Ok(_) => Err(divergence("decode verdict")),
    }
}

/// Core's outputs for a list it read against ours, each whole: the canonical bytes (members,
/// internal ids, the registration counter), the hash, and the evodb bytes against
/// [`list_disk_bytes`] (Core's iteration order, so the collision buckets' too).
pub fn compare_list_outputs(list: &MnList, core: &CanonicalList) -> Result<(), Divergence> {
    check(canonical(list) == core.canonical, "canonical list")?;
    check(canonical_mn_list_hash(list).as_bytes() == &core.hash, "list hash")?;
    check(list_disk_bytes(list) == core.disk, "evodb list")
}

/// Core's evodb bytes of a list, read by both evodb readers: the same list, every output whole.
/// Core's reader adds the members in the file's order, so it reverses a collision bucket (which
/// iterates newest first): the reference is Core's read, not the bytes read.
fn compare_evodb_reads(oracle: &Oracle, chain: &Chain, disk: &[u8]) -> Result<(), Divergence> {
    let mut rest = disk;
    let ours = core_disk::decode_list_s3(&mut rest).map_err(|_| divergence("evodb list read"))?;
    check(rest.is_empty(), "evodb list read")?;
    let core = oracle
        .mnlist_canonical(chain, ListEncoding::Disk, disk)
        .map_err(|_| divergence("oracle error"))?;
    compare_list_outputs(&ours, &core)
}

/// Core's `BuildDiff` outputs against our diff, byte for byte. A diff `BuildDiff` produces has
/// the same bytes in both of v24's encodings (the additions, then the updates and the removals
/// sorted by internal id), so ours is compared with each. Our evodb reader must also read all of
/// Core's evodb bytes back to the same bytes.
pub fn compare_diff_outputs(diff: &MnListDiff, core: &ListDiff) -> Result<(), Divergence> {
    let ours = canonical_diff(diff);
    check(ours == core.canonical, "canonical diff")?;
    check(ours == core.disk, "evodb diff")?;
    let mut rest = core.disk.as_slice();
    let imported = core_disk::decode_diff_d4(&mut rest);
    check(
        rest.is_empty() && matches!(&imported, Ok(d) if canonical_diff(d) == core.disk),
        "evodb diff read",
    )
}

/// Core's `GetUniquePropertyHash` outputs against our key: the pre-image, and the hash as
/// SHA256d of our pre-image (EV2 §2.2; the node's map hashes the pre-image with SipHash instead,
/// DEC-182).
pub fn compare_unique_outputs(key: &UniqueKey, core: &UniqueHash) -> Result<(), Divergence> {
    check(key.as_bytes() == core.preimage.as_slice(), "pre-image")?;
    check(sha256d::Hash::hash(key.as_bytes()).to_byte_array() == core.hash, "hash")
}

/// A canonical list read by both: the same list, every output whole
/// ([`compare_list_outputs`]), with Core's evodb bytes of it read alike by both; or alike
/// refusals.
fn read_list(oracle: &Oracle, chain: &Chain, bytes: &[u8]) -> Result<Read<MnList>, Divergence> {
    let ours = read_canonical(bytes);
    match oracle.mnlist_canonical(chain, ListEncoding::Canonical, bytes) {
        Ok(core) => {
            let list = ours.map_err(|_| divergence("decode verdict"))?;
            compare_list_outputs(&list, &core)?;
            compare_evodb_reads(oracle, chain, &core.disk)?;
            Ok(Read::Accepted(list))
        }
        Err(core) => canonical_refusals(ours, &core).map(Read::Refused),
    }
}

/// `diff_mnlist_codec`: a canonical list or an evodb list (see the module docs).
pub fn compare_codec(input: &[u8], pin: Pin) -> Compared {
    let Some((&mode, bytes)) = input.split_first() else {
        return Ok(Outcome::OutOfDomain);
    };
    let oracle = Oracle::new(pin);
    let chain = Chain::main();
    if mode & 1 == 0 {
        return Ok(match read_list(&oracle, &chain, bytes)? {
            Read::Accepted(_) => Outcome::BothAccept,
            Read::Refused(outcome) => outcome,
        });
    }
    // Core's evodb reader (`ReadMnList`), each entry through `AddMN`.
    let mut rest = bytes;
    let ours = core_disk::decode_list_s3(&mut rest).map_err(|e| e.0).and_then(|list| {
        if rest.is_empty() {
            Ok(list)
        } else {
            Err(TRAILING.into())
        }
    });
    match (ours, oracle.mnlist_canonical(&chain, ListEncoding::Disk, bytes)) {
        (Ok(list), Ok(core)) => {
            compare_list_outputs(&list, &core)?;
            Ok(Outcome::BothAccept)
        }
        // The reserved internal id, which `AddMN` asserts on.
        (_, Err(core)) if core.kind == ErrorKind::Argument => Ok(Outcome::OutOfDomain),
        (Err(_), Err(core)) if core.kind == ErrorKind::Decode => Ok(Outcome::BothReject),
        (Err(ours), Err(core)) if core.kind == ErrorKind::Trailing && ours == TRAILING => {
            Ok(Outcome::BothReject)
        }
        // `AddMN`'s refusals, whose texts our list renders.
        (Err(ours), Err(core)) if core.kind == ErrorKind::Exception => {
            check(class(&ours) == class(&core.message), "refusal")?;
            Ok(Outcome::BothReject)
        }
        (Err(_), Err(_)) => Err(divergence("oracle error")),
        _ => Err(divergence("decode verdict")),
    }
}

/// The block every diff is applied at: the base list's next height.
const APPLY_BLOCK: [u8; 32] = [0x5a; 32];

/// `ApplyDiff` by both, ours given: the same list, or alike refusals. A refusal of a state
/// outside Core's state domain is ours only (D20).
fn compare_applied(
    ours: Result<MnList, MnListError>,
    core: dash_oracle_sys::Result<dash_oracle_sys::AppliedList>,
) -> Compared {
    match (ours, core) {
        (Ok(list), Ok(core)) => {
            check(canonical(&list) == core.canonical, "applied list")?;
            check(canonical_mn_list_hash(&list).as_bytes() == &core.hash, "applied hash")?;
            Ok(Outcome::BothAccept)
        }
        (
            Err(MnListError::VersionHiddenField {
                ..
            }),
            _,
        ) => Ok(Outcome::OutOfDomain),
        (Err(ours), Err(core)) if core.kind == ErrorKind::Exception => {
            check(class(&ours.core_message()) == class(&core.message), "apply refusal")?;
            Ok(Outcome::BothReject)
        }
        (_, Err(core)) if core.kind != ErrorKind::Exception => Err(divergence("oracle error")),
        _ => Err(divergence("apply verdict")),
    }
}

fn apply(base: &MnList, diff: &MnListDiff) -> Result<MnList, MnListError> {
    let mut list = base.clone();
    list.apply_diff(BlockHash::from(APPLY_BLOCK), base.height() + 1, diff).map(|()| list)
}

/// `diff_mnlist_diff` (see the module docs).
pub fn compare_diff(input: &[u8], pin: Pin) -> Compared {
    let [mode, pick, rest @ ..] = input else {
        return Ok(Outcome::OutOfDomain);
    };
    let lists = core_lists();
    let Some(base) = lists.get(usize::from(*pick) % lists.len().max(1)) else {
        return Err(divergence("fixtures"));
    };
    let oracle = Oracle::new(pin);
    let chain = Chain::main();
    match mode % 3 {
        0 => built_diff(&oracle, &chain, base, rest),
        1 => foreign_diff(&oracle, &chain, base, ListEncoding::Canonical, rest),
        _ => foreign_diff(&oracle, &chain, base, ListEncoding::Disk, rest),
    }
}

/// A diff from the base list to its edited copy.
fn built_diff(oracle: &Oracle, chain: &Chain, base: &CoreList, edits: &[u8]) -> Compared {
    let mut to = base
        .list
        .clone()
        .with_height(base.list.height() + 1)
        .with_block_hash(BlockHash::from(APPLY_BLOCK));
    for (step, &[op, arg]) in edits.as_chunks::<2>().0.iter().take(MAX_EDITS).enumerate() {
        edit(&mut to, step as u8, op, arg);
    }
    let to_bytes = canonical(&to);
    let to = match read_list(oracle, chain, &to_bytes)? {
        Read::Accepted(list) => list,
        Read::Refused(outcome) => return Ok(outcome),
    };
    let from = &base.list;
    let core = oracle
        .mnlist_diff(chain, ListEncoding::Canonical, &base.bytes, &to_bytes)
        .map_err(|_| divergence("oracle error"))?;
    let diff = from.diff_to(&to);
    compare_diff_outputs(&diff, &core)?;
    // ApplyDiff of what both read from Core's bytes.
    let read =
        decode_canonical_diff(&mut core.canonical.as_slice(), &mut { usize::MAX }, core_net_info())
            .map_err(|_| divergence("diff read"))?;
    let core_applied = oracle.mnlist_apply_diff(
        chain,
        ListEncoding::Canonical,
        &base.bytes,
        ListEncoding::Canonical,
        &core.canonical,
        &APPLY_BLOCK,
        from.height() + 1,
    );
    compare_applied(apply(from, &read), core_applied)?;

    for b in to.iter() {
        let Some(a) = from.get(&b.pro_tx_hash) else {
            continue;
        };
        let state = MnStateDiff::between(a, b);
        let core = oracle
            .state_diff(&state_bytes(a), &state_bytes(b))
            .map_err(|_| divergence("oracle error"))?;
        check(state_diff_bytes(&state) == core.diff, "state diff")?;
        let (hot, cold) = state.apply(&a.hot, &a.cold);
        let after = Dmn {
            hot,
            cold,
            ..(**a).clone()
        };
        check(state_bytes(&after) == core.applied, "applied state")?;
    }
    Ok(Outcome::BothAccept)
}

/// A diff from the input, read and applied by both.
fn foreign_diff(
    oracle: &Oracle,
    chain: &Chain,
    base: &CoreList,
    encoding: ListEncoding,
    bytes: &[u8],
) -> Compared {
    let mut rest = bytes;
    let read = match encoding {
        ListEncoding::Canonical => {
            decode_canonical_diff(&mut rest, &mut { usize::MAX }, core_net_info()).map_err(|e| e.0)
        }
        ListEncoding::Disk => core_disk::decode_diff_d4(&mut rest).map_err(|e| e.0),
    }
    .and_then(|diff| {
        if rest.is_empty() {
            Ok(diff)
        } else {
            Err(TRAILING.to_string())
        }
    });
    let core = oracle.mnlist_apply_diff(
        chain,
        ListEncoding::Canonical,
        &base.bytes,
        encoding,
        bytes,
        &APPLY_BLOCK,
        base.list.height() + 1,
    );
    match (read, core) {
        // Core's read refused the diff (an exception is `ApplyDiff`'s).
        (read, Err(core)) if core.kind != ErrorKind::Exception => match (read, core.kind) {
            (read, _) if encoding == ListEncoding::Canonical => {
                canonical_refusals(read.map(|_| ()), &core)
            }
            (Err(_), ErrorKind::Decode) => Ok(Outcome::BothReject),
            (Err(ours), ErrorKind::Trailing) if ours == TRAILING => Ok(Outcome::BothReject),
            (_, ErrorKind::Argument) => Ok(Outcome::OutOfDomain),
            (Ok(_), ErrorKind::Decode | ErrorKind::Trailing) => Err(divergence("decode verdict")),
            _ => Err(divergence("oracle error")),
        },
        (Ok(diff), core) => compare_applied(apply(&base.list, &diff), core),
        (Err(_), _) => Err(divergence("decode verdict")),
    }
}

/// 32 bytes from `(a, b, salt)` (splitmix64), distinct for distinct inputs.
pub(crate) fn spread(a: u8, b: u8, salt: u8) -> [u8; 32] {
    let mut state = u64::from_le_bytes([a, b, salt, 0x0e, 0x72, 0xa0, 0, 0]);
    let mut out = [0u8; 32];
    for chunk in out.chunks_mut(8) {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        chunk.copy_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out
}

pub(crate) fn twenty(bytes: [u8; 32]) -> [u8; 20] {
    std::array::from_fn(|i| bytes[i])
}

/// Six valid operator keys (basic encoding); edits re-encode them under a state's scheme.
fn keys() -> &'static [OperatorKey] {
    static KEYS: OnceLock<Vec<OperatorKey>> = OnceLock::new();
    KEYS.get_or_init(|| (1..=6).map(|n| operator_key(n, BlsSchemeKind::Basic)).collect())
}

/// Mainnet-shaped masternodes to add (one Evo, seven regular), given new identities by [`edit`].
fn templates() -> &'static [Arc<Dmn>] {
    static TEMPLATES: OnceLock<Vec<Arc<Dmn>>> = OnceLock::new();
    TEMPLATES.get_or_init(|| (0..8).map(numbered).collect())
}

/// `key` holding the same point under `scheme`, as Core's `SetStateVersion` leaves it; the
/// default key stays the default.
fn reencoded(key: &OperatorKey, scheme: BlsSchemeKind) -> OperatorKey {
    if key.scheme() == scheme {
        key.clone()
    } else if *key == OperatorKey::new(key.scheme()) {
        OperatorKey::new(scheme)
    } else {
        OperatorKey::from_value(key.get(), scheme)
    }
}

pub(crate) fn payout(script: &[u8]) -> Option<PayoutShare> {
    let mut bytes = vec![u8::try_from(script.len()).ok()?];
    bytes.extend_from_slice(script);
    bytes.extend_from_slice(&10_000u16.to_le_bytes());
    PayoutShare::decode(&mut bytes.as_slice()).ok()
}

/// A routable mainnet service on the default port.
pub(crate) fn service(a: u8, b: u8) -> ServiceV2 {
    ServiceV2 {
        addr: AddrV2::Ipv4([1, 2 + (a & 0x3f), b, 7]),
        port: 9999,
    }
}

pub(crate) fn legacy(service: &ServiceV2) -> NetInfo {
    let mut ip = [0u8; 16];
    ip[10..12].copy_from_slice(&[0xff, 0xff]);
    if let AddrV2::Ipv4(v4) = service.addr {
        ip[12..].copy_from_slice(&v4);
    }
    NetInfo::Legacy(NetInfoV1(ServiceV1 {
        addr: AddrV1(ip),
        port: service.port,
    }))
}

pub(crate) fn extended(service: ServiceV2) -> NetInfo {
    NetInfo::Extended(NetInfoV2 {
        version: 1,
        entries: vec![(NIPurpose::CoreP2p, vec![NIEntry::Service(service)])],
    })
}

/// The state version change `arg` selects: 1 and 2 swap (the key re-encoded); 2 and 3 swap
/// the network info's kind and the payout's field. Platform fields are cleared on the way to 3.
fn change_version(cold: &mut MnCold, arg: u8) {
    let version = match (cold.version, arg & 1) {
        (1, _) => 2,
        (2, 0) => 1,
        (2, _) => 3,
        _ => 2,
    };
    let scheme = BlsSchemeKind::from_legacy(version == 1);
    cold.pub_key_operator = reencoded(&cold.pub_key_operator, scheme);
    match (cold.version, version) {
        (2, 3) => {
            cold.net_info = Arc::new(extended(service(arg, 0x30)));
            cold.payouts = payout(&cold.script_payout).into_iter().collect();
            cold.script_payout = Vec::new();
            cold.platform_p2p_port = 0;
            cold.platform_http_port = 0;
        }
        (3, 2) => {
            cold.net_info = Arc::new(legacy(&service(arg, 0x31)));
            cold.script_payout =
                cold.payouts.first().map(|p| p.script_payout.to_vec()).unwrap_or_default();
            cold.payouts = Vec::new();
            cold.shares = Vec::new();
            cold.early_period_blocks = 0;
            cold.early_penalty = 0;
        }
        _ => {}
    }
    cold.version = version;
}

/// One edit of `list` (step `step`). `op` mod 8: 0 removes, 1 and 2 change the hot fields
/// (payment, PoSe), 3 to 6 the state (owner key; operator key; version, with key re-encoding and
/// network-info kind changes; confirmed hash, voting key or revocation reason), 7 adds a
/// masternode with a fresh identity and a bumped counter. `arg` picks the masternode and the
/// value. An edit our list refuses is skipped, so the edited list is one Core could have built.
pub fn edit(list: &mut MnList, step: u8, op: u8, arg: u8) {
    let picked = list.iter().nth(usize::from(arg) % list.len().max(1)).cloned();
    let height = list.height();
    match (op % 8, picked) {
        (7, _) => {
            let Some(template) = templates().get(usize::from(arg) % 8) else {
                return;
            };
            let mut cold = (*template.cold).clone();
            cold.key_id_owner = KeyId::from(twenty(spread(step, arg, 1)));
            cold.key_id_voting = KeyId::from(twenty(spread(step, arg, 2)));
            if let Some(key) = keys().get(usize::from(arg / 8) % 6) {
                cold.pub_key_operator = reencoded(key, cold.key_scheme());
            }
            cold.net_info = Arc::new(legacy(&service(step, arg)));
            // `numbered(0)` is the Evo one.
            if arg.is_multiple_of(8) {
                cold.platform_node_id = PlatformNodeId::from(twenty(spread(step, arg, 3)));
            }
            cold.registered_height = height;
            let dmn = Dmn {
                pro_tx_hash: ProTxHash::from(spread(step, arg, 4)),
                internal_id: u64::from(list.total_registered()),
                collateral: OutPoint {
                    hash: TxHash::from(spread(step, arg, 5)),
                    index: u32::from(arg % 3),
                },
                cold: Arc::new(fresh(cold)),
                ..(**template).clone()
            };
            let _ = list.try_add(Arc::new(dmn), true);
        }
        (_, None) => {}
        (0, Some(dmn)) => {
            let _ = list.remove(&dmn.pro_tx_hash);
        }
        (1, Some(dmn)) => {
            let mut hot = dmn.hot;
            hot.last_paid_height = height - i32::from(arg);
            hot.consecutive_payments = i32::from(arg % 5);
            let _ = list.update_hot(&dmn.pro_tx_hash, hot);
        }
        (2, Some(dmn)) => {
            let mut hot = dmn.hot;
            if arg & 1 == 0 {
                hot.ban_if_not_banned(height);
            } else {
                hot.revive(height);
            }
            hot.pose_penalty = i32::from(arg >> 1);
            let _ = list.update_hot(&dmn.pro_tx_hash, hot);
        }
        (op, Some(dmn)) => {
            let mut cold = (*dmn.cold).clone();
            match op {
                3 => cold.key_id_owner = KeyId::from([arg; 20]),
                4 if arg & 0x80 != 0 => cold.pub_key_operator = OperatorKey::new(cold.key_scheme()),
                4 => {
                    if let Some(key) = keys().get(usize::from(arg) % 6) {
                        cold.pub_key_operator = reencoded(key, cold.key_scheme());
                    }
                }
                5 => change_version(&mut cold, arg),
                _ => match arg % 3 {
                    0 => cold.update_confirmed_hash(&dmn.pro_tx_hash, &spread(step, arg, 6).into()),
                    1 => cold.key_id_voting = KeyId::from([arg; 20]),
                    _ => cold.revocation_reason = u16::from(arg % 4),
                },
            }
            let _ = list.update(&dmn.pro_tx_hash, dmn.hot, Arc::new(fresh(cold)));
        }
    }
}

/// `diff_mnlist_order`'s proTxHashes: 1 to 4 share one 64-bit hash and 5 to 8 another (the
/// sign bit set), 9 to 16 have their own.
pub fn order_key(n: u8) -> ProTxHash {
    let mut bytes = [n; 32];
    let hash = match n {
        1..=4 => 0x0101_0101_0101_0101u64,
        5..=8 => 0x8000_0000_0000_0002u64,
        _ => u64::from(n) << 56,
    };
    bytes[..8].copy_from_slice(&hash.to_le_bytes());
    ProTxHash::from(bytes)
}

/// Masternode `n` (Evo when even) with internal id `id`, owner and voting key ids `[n; 20]`;
/// `shared` gives it collateral `([1; 32], 0)` instead of its own.
fn order_mn(n: u8, id: u64, shared: bool) -> Arc<Dmn> {
    let mn_type = if n.is_multiple_of(2) {
        MnType::Evo
    } else {
        MnType::Regular
    };
    dash_dml::testkit::dmn_with(n, mn_type, |d, _| {
        d.pro_tx_hash = order_key(n);
        d.internal_id = id;
        if shared {
            d.collateral.hash = TxHash::from([1; 32]);
        }
    })
}

/// The masternode an operation of `diff_mnlist_order` names: for `arg < 0xf8`, a member if
/// `present` (the `arg`-th, cyclically) or else the first non-member from `arg % 16 + 1` on, so
/// the operation applies; for `arg >= 0xf8` (or none to pick), `arg % 16 + 1`.
fn order_target(list: &MnList, arg: u8, present: bool) -> u8 {
    let start = arg % 16 + 1;
    let wanted: Vec<u8> =
        (1..=16).filter(|n| list.get(&order_key(*n)).is_some() == present).collect();
    match wanted.first() {
        Some(&first) if arg < 0xf8 => {
            if present {
                wanted[usize::from(arg) % wanted.len()]
            } else {
                wanted.iter().copied().find(|n| *n >= start).unwrap_or(first)
            }
        }
        _ => start,
    }
}

/// `diff_mnlist_order`: each (op, arg) pair is one operation on the masternode
/// [`order_target`] picks. `op` mod 4 (0 on an empty list): 0 `AddMN`, 1 `RemoveMN`, 2
/// `UpdateMN` of the hot fields, 3 `UpdateMN` of the owner key id. With `v = op >> 2`: an
/// addition bumps the counter unless `v % 8 == 7` (then it takes the lowest free id below the
/// counter, as a reloaded list's members do), takes id 0 when `v == 6` and the shared collateral
/// `([1; 32], 0)` when `v == 5`; an owner key id is masternode `v / 32 * 8 + 1`'s when
/// `v % 32 == 0`, else `[0x40 + v; 20]`. An update of an absent masternode sends its fresh
/// state. Our list's first refusal ends the sequence: Core must throw at the same operation,
/// with the same text up to its first value.
pub fn compare_order(input: &[u8], pin: Pin) -> Compared {
    let oracle = Oracle::new(pin);
    let chain = Chain::main();
    let mut ours = MnList::default_pre_dip3();
    let mut ops = Vec::new();
    for (step, &[op, arg]) in input.as_chunks::<2>().0.iter().take(MAX_OPS).enumerate() {
        let v = op >> 2;
        // An operation on an empty list adds, unless it names its masternode.
        let kind = if ours.is_empty() && arg < 0xf8 {
            0
        } else {
            op % 4
        };
        let n = order_target(&ours, arg, kind != 0);
        let pro_tx = order_key(n);
        let old = ours.get(&pro_tx).cloned().unwrap_or_else(|| order_mn(n, 0, false));
        let result = match kind {
            0 => {
                let total = u64::from(ours.total_registered());
                let free = (0..total).find(|id| ours.iter().all(|d| d.internal_id != *id));
                let bump = v % 8 != 7 || free.is_none();
                let id = match (v, bump) {
                    (6, _) => 0,
                    (_, true) => total,
                    (_, false) => free.unwrap_or(total),
                };
                let dmn = order_mn(n, id, v == 5);
                ops.push(ListOp::Add {
                    dmn: dmn_bytes(&dmn),
                    bump_total_count: bump,
                });
                ours.try_add(dmn, bump)
            }
            1 => {
                ops.push(ListOp::Remove {
                    pro_tx_hash: *pro_tx.as_bytes(),
                });
                ours.remove(&pro_tx)
            }
            2 => {
                let mut hot = old.hot;
                hot.last_paid_height = step as i32 + 1;
                let new = Dmn {
                    hot,
                    ..(*old).clone()
                };
                ops.push(ListOp::Update {
                    pro_tx_hash: *pro_tx.as_bytes(),
                    state: state_bytes(&new),
                });
                ours.update_hot(&pro_tx, hot)
            }
            _ => {
                let mut cold = (*old.cold).clone();
                cold.key_id_owner = KeyId::from(if v.is_multiple_of(32) {
                    [v / 32 * 8 + 1; 20]
                } else {
                    [0x40 + v; 20]
                });
                let cold = Arc::new(fresh(cold));
                let new = Dmn {
                    cold: Arc::clone(&cold),
                    ..(*old).clone()
                };
                ops.push(ListOp::Update {
                    pro_tx_hash: *pro_tx.as_bytes(),
                    state: state_bytes(&new),
                });
                ours.update(&pro_tx, old.hot, cold)
            }
        };
        if let Err(refusal) = result {
            let core = oracle.mnlist_core_order(&chain, &ops);
            let Err(core) = core.map(|_| ()) else {
                return Err(divergence("operation verdict"));
            };
            let at = format!("operation {step}: ");
            let text = match core.message.strip_prefix(&at) {
                Some(text) if core.kind == ErrorKind::Exception => text,
                _ => return Err(divergence("failed operation")),
            };
            check(class(text) == class(&refusal.core_message()), "refusal")?;
            return Ok(Outcome::BothReject);
        }
    }
    let core =
        oracle.mnlist_core_order(&chain, &ops).map_err(|_| divergence("operation verdict"))?;
    let order: Vec<[u8; 32]> = ours.core_order().iter().map(|p| *p.as_bytes()).collect();
    check(order == core.order, "iteration order")?;
    check(canonical(&ours) == core.canonical, "canonical list")?;
    Ok(Outcome::BothAccept)
}

/// Our key of a network-info entry (`tag`, then `value`) as the extended network info's reader
/// keeps it. Only a kept entry reaches the map: that reader, as Core's, marks the others invalid
/// (an address `IsValid` refuses, an IPv4, TORv2 or internal address in IPv6, a domain
/// `DomainPort::Validate` refuses), so they are [`Outcome::OutOfDomain`].
fn entry_key(tag: u8, value: &[u8]) -> Result<UniqueKey, Outcome> {
    let entry = [&[tag][..], value].concat();
    let mut input = entry.as_slice();
    match NIEntry::decode(&mut input) {
        Ok(NIEntry::Service(service)) if input.is_empty() => Ok(UniqueKey::service(&service)),
        Ok(NIEntry::Domain {
            name,
            port,
        }) if input.is_empty() => Ok(UniqueKey::domain(&name, port)),
        _ => Err(Outcome::OutOfDomain),
    }
}

/// Our key of a service value (ADDRv2), or `None` for bytes we refuse.
fn service_key(value: &[u8]) -> Result<Option<UniqueKey>, Outcome> {
    let mut input = value;
    match ServiceV2::decode(&mut input) {
        Ok(_) if input.is_empty() => entry_key(0x01, value).map(Some),
        _ => Ok(None),
    }
}

/// Our key of a domain value, or `None` for bytes we refuse.
fn domain_key(value: &[u8]) -> Result<Option<UniqueKey>, Outcome> {
    let entry = [&[0x02][..], value].concat();
    let mut input = entry.as_slice();
    match NIEntry::decode(&mut input) {
        Ok(_) if input.is_empty() => entry_key(0x02, value).map(Some),
        _ => Ok(None),
    }
}

/// The property `diff_unique_hash`'s selector byte names: bits 0-6 mod 6 the type, bit 7 an
/// operator key's legacy scheme.
pub fn unique_property(kind: u8) -> UniqueProperty {
    match (kind & 0x7f) % 6 {
        0 => UniqueProperty::Collateral,
        1 => UniqueProperty::Service,
        2 => UniqueProperty::Domain,
        3 => UniqueProperty::KeyId,
        4 => UniqueProperty::PlatformNodeId,
        _ => UniqueProperty::OperatorKey {
            legacy: kind & 0x80 != 0,
        },
    }
}

/// The selector byte of `property` ([`unique_property`]'s inverse on 0..=5 and 0x85).
fn unique_kind(property: UniqueProperty) -> u8 {
    match property {
        UniqueProperty::Collateral => 0,
        UniqueProperty::Service => 1,
        UniqueProperty::Domain => 2,
        UniqueProperty::KeyId => 3,
        UniqueProperty::PlatformNodeId => 4,
        UniqueProperty::OperatorKey {
            legacy,
        } => 5 | u8::from(legacy) << 7,
    }
}

/// `diff_unique_hash`'s seeds (the `dml_seeds` example writes them): a valid value of every
/// property type and class `l1_dml` checks, each an input naming its property. Core accepts every
/// one, and so does the target.
pub fn unique_seeds() -> Vec<(UniqueProperty, Vec<u8>)> {
    let mut values = Vec::new();
    let mut collateral = (1..=32).collect::<Vec<u8>>();
    collateral.extend_from_slice(&1u32.to_le_bytes());
    values.push((UniqueProperty::Collateral, collateral));
    values.push((UniqueProperty::Collateral, [vec![0; 32], vec![0xff; 4]].concat()));
    let tor: Vec<u8> = (1..=32).collect();
    let i2p: Vec<u8> = (33..=64).collect();
    let mut cjdns = vec![0u8; 16];
    cjdns[0] = 0xfc;
    cjdns[15] = 1;
    let ipv6 = [0x20, 0x01, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88];
    for (net, addr, port) in [
        (1u8, vec![1, 2, 3, 4], 9999u16),
        (2, ipv6.to_vec(), 9999),
        (4, tor, 9999),
        (5, i2p, 0),
        (6, cjdns, 9999),
    ] {
        let service = [vec![net, addr.len() as u8], addr, port.to_be_bytes().to_vec()].concat();
        values.push((UniqueProperty::Service, service));
    }
    let domain = [vec![16], b"evo5.example.org".to_vec(), 443u16.to_be_bytes().to_vec()].concat();
    values.push((UniqueProperty::Domain, domain));
    let id: Vec<u8> = (0x11..0x11 + 20).collect();
    values.push((UniqueProperty::KeyId, id.clone()));
    values.push((UniqueProperty::PlatformNodeId, id));
    // Core's evo_simplifiedmns_tests' first legacy key, its basic encoding, the legacy bytes
    // under the basic flag, and the default key under each scheme.
    let legacy = unhex(
        "81fc56c400756e2f053ea015d7a61606f668a10f5ee0808f86df9443778021b802174b1d9cdb0cde3c3a4fe5803d02b5",
    )
    .unwrap_or_default();
    let mut basic = legacy.clone();
    if let Some(first) = basic.first_mut() {
        *first = 0x80 | (*first & 0x80) >> 2 | *first & 0x1f;
    }
    for (is_legacy, key) in [(true, legacy.clone()), (false, basic), (false, legacy)] {
        values.push((
            UniqueProperty::OperatorKey {
                legacy: is_legacy,
            },
            key,
        ));
    }
    for is_legacy in [true, false] {
        values.push((
            UniqueProperty::OperatorKey {
                legacy: is_legacy,
            },
            vec![0; 48],
        ));
    }
    values
        .into_iter()
        .map(|(property, value)| (property, [vec![unique_kind(property)], value].concat()))
        .collect()
}

/// `diff_unique_hash` (see the module docs). Compared: the verdict, the pre-image and the hash
/// ([`compare_unique_outputs`]).
pub fn compare_unique_hash(input: &[u8], pin: Pin) -> Compared {
    let Some((&kind, value)) = input.split_first() else {
        return Ok(Outcome::OutOfDomain);
    };
    let property = unique_property(kind);
    let fixed = |len: usize| (value.len() == len).then_some(value);
    let ours = match property {
        UniqueProperty::Collateral => fixed(36).map(|b| {
            UniqueKey::collateral(&OutPoint {
                hash: TxHash::from(std::array::from_fn::<u8, 32, _>(|i| b[i])),
                index: u32::from_le_bytes([b[32], b[33], b[34], b[35]]),
            })
        }),
        UniqueProperty::Service => match service_key(value) {
            Ok(key) => key,
            Err(outcome) => return Ok(outcome),
        },
        UniqueProperty::Domain => match domain_key(value) {
            Ok(key) => key,
            Err(outcome) => return Ok(outcome),
        },
        UniqueProperty::KeyId => fixed(20).map(|b| UniqueKey::key_id(&KeyId::from(twenty_of(b)))),
        UniqueProperty::PlatformNodeId => {
            fixed(20).map(|b| UniqueKey::platform_node_id(&PlatformNodeId::from(twenty_of(b))))
        }
        UniqueProperty::OperatorKey {
            legacy,
        } => fixed(48).map(|b| {
            let bytes: [u8; 48] = std::array::from_fn(|i| b[i]);
            let scheme = BlsSchemeSource::Version(BlsSchemeKind::from_legacy(legacy));
            UniqueKey::operator_key(&OperatorKey::from_bytes(&bytes, scheme))
        }),
    };
    match (ours, Oracle::new(pin).unique_property_hash(property, value)) {
        (Some(ours), Ok(core)) => {
            compare_unique_outputs(&ours, &core)?;
            Ok(Outcome::BothAccept)
        }
        (None, Err(core)) if matches!(core.kind, ErrorKind::Decode | ErrorKind::Trailing) => {
            Ok(Outcome::BothReject)
        }
        (_, Err(core)) if !matches!(core.kind, ErrorKind::Decode | ErrorKind::Trailing) => {
            Err(divergence("oracle error"))
        }
        _ => Err(divergence("decode verdict")),
    }
}

fn twenty_of(b: &[u8]) -> [u8; 20] {
    std::array::from_fn(|i| b[i])
}
