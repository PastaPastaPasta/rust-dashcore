//! The masternode-list entry points (ABI 8; node-program O2a-EV, EV2 §6.3): Core's
//! canonical list codec and `CanonicalMNListHash`, and its unique-property keys.

use std::os::raw::c_int;

use crate::{Chain, Error, ErrorKind, Oracle, Result, Verdict, block_verdict, ffi};

/// The encoding of a masternode list given to the oracle (SER_DISK, CLIENT_VERSION both).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListEncoding {
    /// `evo/snapshot.h`'s `SerializeCanonicalMNList`, read with Core's strict
    /// `UnserializeCanonicalMNList` (its refusals are [`crate::ErrorKind::Decode`]).
    Canonical,
    /// A `CDeterministicMNList` as evodb stores it (`dmn_S3`; a [`crate::Context`]'s list).
    Disk,
}

impl ListEncoding {
    fn code(self) -> c_int {
        match self {
            ListEncoding::Canonical => ffi::ORACLE_MNLIST_CANONICAL,
            ListEncoding::Disk => ffi::ORACLE_MNLIST_DISK,
        }
    }
}

/// A list Core read, in both encodings, with its `CanonicalMNListHash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalList {
    /// `SerializeCanonicalMNList(list)`.
    pub canonical: Vec<u8>,
    /// Core's `Serialize(list)`, the evodb encoding.
    pub disk: Vec<u8>,
    /// `evo::CanonicalMNListHash(list)` (SHA256d of `canonical`), internal byte order.
    pub hash: [u8; 32],
}

/// The type of a value in Core's unique-property map (`GetUniquePropertyHash<T>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UniqueProperty {
    /// `COutPoint`: the collateral.
    Collateral,
    /// `CService`, given in ADDRv2 format (any network).
    Service,
    /// `DomainPort`: an ExtNetInfo domain entry.
    Domain,
    /// `CKeyID`: the owner key, or a share's owner key.
    KeyId,
    /// `uint160`: an Evo masternode's platformNodeID.
    PlatformNodeId,
    /// `CBLSLazyPublicKey`: an operator key's 48 bytes, in the legacy (`true`) or basic
    /// encoding, as a state's lazy key keeps them.
    OperatorKey {
        legacy: bool,
    },
}

impl UniqueProperty {
    fn code(self) -> c_int {
        match self {
            UniqueProperty::Collateral => ffi::ORACLE_UNIQUE_COLLATERAL,
            UniqueProperty::Service => ffi::ORACLE_UNIQUE_SERVICE,
            UniqueProperty::Domain => ffi::ORACLE_UNIQUE_DOMAIN,
            UniqueProperty::KeyId => ffi::ORACLE_UNIQUE_KEY_ID,
            UniqueProperty::PlatformNodeId => ffi::ORACLE_UNIQUE_PLATFORM_NODE_ID,
            UniqueProperty::OperatorKey {
                ..
            } => ffi::ORACLE_UNIQUE_OPERATOR_KEY,
        }
    }
}

/// A unique-property key: what Core hashes and the hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UniqueHash {
    /// `SerializeHash`'s input: the SER_GETHASH serialization at PROTOCOL_VERSION
    /// (`| ADDRV2_FORMAT` for a service that is not ADDRv1-compatible).
    pub preimage: Vec<u8>,
    /// `GetUniquePropertyHash(value)`, internal byte order.
    pub hash: [u8; 32],
}

/// What a provider transaction's signatures sign (`oracle_special_tx_sign_hash`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignHashes {
    /// `::SerializeHash(payload)`, signature fields excluded.
    pub payload_hash: [u8; 32],
    /// ProRegTx: `MakeSignString()` and the hash `CMessageSigner::VerifyMessage()` checks.
    pub sign_message: Option<(String, [u8; 32])>,
    /// Shared ProRegTx: `MakeSharedRegConsentHash(tx)`; ProDisTx: `MakeSignHash(tx, sigs)`.
    pub consent_hash: Option<[u8; 32]>,
}

/// `merkleRootMNList` of a list (`oracle_sml_merkle_root`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmlRoot {
    /// Accept, or Core's rejection of a mutated tree.
    pub verdict: Verdict,
    /// `CalcCbTxMerkleRootMNList`'s root, internal byte order.
    pub root: [u8; 32],
    /// Every entry's `CalcHash()` in the simplified list's order (by proRegTxHash).
    pub entry_hashes: Vec<[u8; 32]>,
}

/// A masternode-state diff and the state it produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateDiff {
    /// Core's serialization of `CDeterministicMNStateDiff(a, b)`.
    pub diff: Vec<u8>,
    /// `a` after `diff.ApplyToState(a)` (SER_DISK).
    pub applied: Vec<u8>,
}

/// A masternode-list diff, in both encodings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListDiff {
    /// `evo::SerializeCanonicalMNListDiff(diff)`.
    pub canonical: Vec<u8>,
    /// Core's `Serialize(diff)`, the evodb `dmn_D4` row.
    pub disk: Vec<u8>,
}

/// A list after `ApplyDiff`, unvalidated as Core writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppliedList {
    /// `SerializeCanonicalMNList(list)`.
    pub canonical: Vec<u8>,
    /// `CanonicalMNListHash(list)`, internal byte order.
    pub hash: [u8; 32],
}

/// One mutation of a masternode list, in Core's encodings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListOp {
    /// `AddMN(dmn, bump)`: `dmn` is a `CDeterministicMN` (SER_DISK).
    Add {
        dmn: Vec<u8>,
        bump_total_count: bool,
    },
    /// `UpdateMN(pro_tx_hash, state)`: `state` is a `CDeterministicMNState` (SER_DISK).
    Update {
        pro_tx_hash: [u8; 32],
        state: Vec<u8>,
    },
    /// `RemoveMN(pro_tx_hash)`.
    Remove {
        pro_tx_hash: [u8; 32],
    },
}

impl ListOp {
    fn encode(&self) -> (u8, Vec<u8>) {
        match self {
            ListOp::Add {
                dmn,
                bump_total_count,
            } => {
                let mut payload = dmn.clone();
                payload.push((*bump_total_count).into());
                (ffi::ORACLE_MNLIST_OP_ADD, payload)
            }
            ListOp::Update {
                pro_tx_hash,
                state,
            } => {
                let mut payload = pro_tx_hash.to_vec();
                payload.extend_from_slice(state);
                (ffi::ORACLE_MNLIST_OP_UPDATE, payload)
            }
            ListOp::Remove {
                pro_tx_hash,
            } => (ffi::ORACLE_MNLIST_OP_REMOVE, pro_tx_hash.to_vec()),
        }
    }
}

/// Core's iteration order of a list, and the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreOrder {
    /// The proTxHashes in `ForEachMN` order, internal byte order.
    pub order: Vec<[u8; 32]>,
    /// `SerializeCanonicalMNList(list)` (sorted by proTxHash, unlike `order`).
    pub canonical: Vec<u8>,
}

impl Oracle {
    /// Read a masternode list and write it canonically and as evodb stores it, with its
    /// `CanonicalMNListHash` (`oracle_mnlist_canonical`).
    pub fn mnlist_canonical(
        &self,
        chain: &Chain,
        input: ListEncoding,
        list: &[u8],
    ) -> Result<CanonicalList> {
        self.on_chain(chain, || {
            let mut hash = [0u8; 32];
            let (canonical, disk) = self.with_buffers(
                list.len() + 64,
                |canonical, canonical_len, disk, disk_len| unsafe {
                    (self.api.mnlist_canonical)(
                        input.code(),
                        list.as_ptr(),
                        list.len(),
                        canonical,
                        canonical_len,
                        disk,
                        disk_len,
                        hash.as_mut_ptr(),
                    )
                },
            )?;
            Ok(CanonicalList {
                canonical,
                disk,
                hash,
            })
        })
    }

    /// Core's unique-property key of a value (`oracle_unique_property_hash`). `value` is
    /// its network serialization (a service in ADDRv2 format). Reads no chain state.
    pub fn unique_property_hash(&self, kind: UniqueProperty, value: &[u8]) -> Result<UniqueHash> {
        self.shared(|| {
            let legacy = matches!(
                kind,
                UniqueProperty::OperatorKey {
                    legacy: true
                }
            );
            let mut hash = [0u8; 32];
            let preimage = self.with_buffer(value.len() + 64, |out, len| unsafe {
                (self.api.unique_property_hash)(
                    kind.code(),
                    legacy.into(),
                    value.as_ptr(),
                    value.len(),
                    out,
                    len,
                    hash.as_mut_ptr(),
                )
            })?;
            Ok(UniqueHash {
                preimage,
                hash,
            })
        })
    }

    /// `CDeterministicMNStateDiff(a, b)` of two states (SER_DISK), and `a` after the diff is
    /// applied to it (`oracle_state_diff`). Reads no chain state.
    pub fn state_diff(&self, a: &[u8], b: &[u8]) -> Result<StateDiff> {
        self.shared(|| {
            let (diff, applied) = self.with_buffers(
                a.len() + b.len() + 64,
                |diff, diff_len, applied, applied_len| unsafe {
                    (self.api.state_diff)(
                        a.as_ptr(),
                        a.len(),
                        b.as_ptr(),
                        b.len(),
                        diff,
                        diff_len,
                        applied,
                        applied_len,
                    )
                },
            )?;
            Ok(StateDiff {
                diff,
                applied,
            })
        })
    }

    /// `from.BuildDiff(to)` of two lists in the `input` encoding (`oracle_mnlist_diff`).
    pub fn mnlist_diff(
        &self,
        chain: &Chain,
        input: ListEncoding,
        from: &[u8],
        to: &[u8],
    ) -> Result<ListDiff> {
        self.on_chain(chain, || {
            let initial = from.len() + to.len() + 64;
            let (canonical, disk) =
                self.with_buffers(initial, |canonical, canonical_len, disk, disk_len| unsafe {
                    (self.api.mnlist_diff)(
                        input.code(),
                        from.as_ptr(),
                        from.len(),
                        to.as_ptr(),
                        to.len(),
                        canonical,
                        canonical_len,
                        disk,
                        disk_len,
                    )
                })?;
            Ok(ListDiff {
                canonical,
                disk,
            })
        })
    }

    /// `list.ApplyDiff(block, diff)` for a block at `height` with hash `block_hash` (internal
    /// byte order): the resulting list, canonical, with its hash (`oracle_mnlist_apply_diff`).
    #[allow(clippy::too_many_arguments)]
    pub fn mnlist_apply_diff(
        &self,
        chain: &Chain,
        list_input: ListEncoding,
        list: &[u8],
        diff_input: ListEncoding,
        diff: &[u8],
        block_hash: &[u8; 32],
        height: i32,
    ) -> Result<AppliedList> {
        self.on_chain(chain, || {
            let mut hash = [0u8; 32];
            let canonical = self.with_buffer(list.len() + diff.len() + 64, |out, len| unsafe {
                (self.api.mnlist_apply_diff)(
                    list_input.code(),
                    list.as_ptr(),
                    list.len(),
                    diff_input.code(),
                    diff.as_ptr(),
                    diff.len(),
                    block_hash.as_ptr(),
                    height,
                    out,
                    len,
                    hash.as_mut_ptr(),
                )
            })?;
            Ok(AppliedList {
                canonical,
                hash,
            })
        })
    }

    /// The order Core iterates the list `ops` builds from an empty one (`ForEachMN`), with
    /// its canonical bytes (`oracle_mnlist_core_order`). A Core throw is an
    /// [`crate::ErrorKind::Exception`] whose message starts with `operation <index>:`.
    pub fn mnlist_core_order(&self, chain: &Chain, ops: &[ListOp]) -> Result<CoreOrder> {
        let mut encoded = Vec::new();
        crate::write_compact_size(&mut encoded, ops.len() as u64);
        for op in ops {
            let (code, payload) = op.encode();
            encoded.push(code);
            crate::write_compact_size(&mut encoded, payload.len() as u64);
            encoded.extend_from_slice(&payload);
        }
        self.on_chain(chain, || {
            let initial = encoded.len() + 64;
            let (order, canonical) =
                self.with_buffers(initial, |order, order_len, canonical, canonical_len| unsafe {
                    (self.api.mnlist_core_order)(
                        encoded.as_ptr(),
                        encoded.len(),
                        order,
                        order_len,
                        canonical,
                        canonical_len,
                        std::ptr::null_mut(),
                    )
                })?;
            let order = hashes32(&order);
            Ok(CoreOrder {
                order,
                canonical,
            })
        })
    }

    /// `GetMNPayee()` of a list for the block after `prev_height`, whose V19 and MN_RR
    /// states come from the chain's buried heights (`oracle_mnlist_payee`).
    pub fn mnlist_payee(
        &self,
        chain: &Chain,
        input: ListEncoding,
        list: &[u8],
        prev_height: i32,
    ) -> Result<Option<[u8; 32]>> {
        self.on_chain(chain, || {
            let (mut payee, mut has_payee) = ([0u8; 32], 0 as c_int);
            self.ok(unsafe {
                (self.api.mnlist_payee)(
                    input.code(),
                    list.as_ptr(),
                    list.len(),
                    prev_height,
                    payee.as_mut_ptr(),
                    &mut has_payee,
                )
            })?;
            Ok((has_payee != 0).then_some(payee))
        })
    }

    /// `GetProjectedMNPayees(count)` of a list, as [`Oracle::mnlist_payee`] reads its
    /// deployments (`oracle_mnlist_projected`).
    pub fn mnlist_projected(
        &self,
        chain: &Chain,
        input: ListEncoding,
        list: &[u8],
        prev_height: i32,
        count: i32,
    ) -> Result<Vec<[u8; 32]>> {
        self.on_chain(chain, || {
            let initial = 32 * usize::try_from(count).unwrap_or(0).min(list.len());
            let payees = self.with_buffer(initial, |out, len| unsafe {
                (self.api.mnlist_projected)(
                    input.code(),
                    list.as_ptr(),
                    list.len(),
                    prev_height,
                    count,
                    out,
                    len,
                )
            })?;
            Ok(hashes32(&payees))
        })
    }

    /// A masternode's (`CDeterministicMN`, SER_DISK) simplified-list entry as mnlistdiff
    /// carries it, and its `CalcHash()` (`oracle_sml_entry_hash`).
    pub fn sml_entry_hash(&self, dmn: &[u8]) -> Result<(Vec<u8>, [u8; 32])> {
        self.shared(|| {
            let mut hash = [0u8; 32];
            let entry = self.with_buffer(dmn.len() + 64, |out, len| unsafe {
                (self.api.sml_entry_hash)(dmn.as_ptr(), dmn.len(), out, len, hash.as_mut_ptr())
            })?;
            Ok((entry, hash))
        })
    }

    /// `merkleRootMNList` of a list: `CalcCbTxMerkleRootMNList(list.to_sml())`
    /// (`oracle_sml_merkle_root`). Core's root cache makes the call stateful: the shim runs
    /// it once and serves the `ORACLE_E_BUFFER` retry from the kept output.
    pub fn sml_merkle_root(
        &self,
        chain: &Chain,
        input: ListEncoding,
        list: &[u8],
    ) -> Result<SmlRoot> {
        self.sml_merkle_root_sized(chain, input, list, list.len() / 4 + 64)
    }

    /// [`Oracle::sml_merkle_root`] with the hashes buffer's first size (tests force the
    /// `ORACLE_E_BUFFER` retry with 0).
    #[doc(hidden)]
    pub fn sml_merkle_root_sized(
        &self,
        chain: &Chain,
        input: ListEncoding,
        list: &[u8],
        initial: usize,
    ) -> Result<SmlRoot> {
        self.on_chain(chain, || {
            let mut root = [0u8; 32];
            let mut state = ffi::OracleValidation::default();
            let mut accepted = false;
            let hashes = self.with_buffer(initial, |out, len| {
                let status = unsafe {
                    (self.api.sml_merkle_root)(
                        input.code(),
                        list.as_ptr(),
                        list.len(),
                        out,
                        len,
                        root.as_mut_ptr(),
                        &mut state,
                    )
                };
                accepted = status == ffi::ORACLE_OK;
                status
            })?;
            Ok(SmlRoot {
                verdict: block_verdict(accepted, &state),
                root,
                entry_hashes: hashes32(&hashes),
            })
        })
    }

    /// What a provider transaction's signatures sign (`oracle_special_tx_sign_hash`). The
    /// ProRegTx sign string encodes addresses for `chain`.
    pub fn special_tx_sign_hash(&self, chain: &Chain, tx: &[u8]) -> Result<SignHashes> {
        self.on_chain(chain, || {
            let (mut payload_hash, mut message_hash, mut consent_hash) =
                ([0u8; 32], [0u8; 32], [0u8; 32]);
            let mut has_consent: c_int = 0;
            let text = self.with_buffer(256, |out, len| unsafe {
                (self.api.special_tx_sign_hash)(
                    tx.as_ptr(),
                    tx.len(),
                    payload_hash.as_mut_ptr(),
                    out.cast(),
                    len,
                    message_hash.as_mut_ptr(),
                    consent_hash.as_mut_ptr(),
                    &mut has_consent,
                )
            })?;
            let text = match text.split_last() {
                Some((0, text)) => String::from_utf8(text.to_vec()).ok(),
                _ => None,
            }
            .ok_or_else(|| Error {
                kind: ErrorKind::Exception,
                message: "the sign string is not NUL-terminated UTF-8".into(),
            })?;
            Ok(SignHashes {
                payload_hash,
                sign_message: (!text.is_empty()).then_some((text, message_hash)),
                consent_hash: (has_consent != 0).then_some(consent_hash),
            })
        })
    }

    /// Call `f` with two output buffers and return both once the shim reports success,
    /// growing them once on `ORACLE_E_BUFFER`.
    pub(crate) fn with_buffers(
        &self,
        initial: usize,
        mut f: impl FnMut(*mut u8, *mut usize, *mut u8, *mut usize) -> c_int,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let (mut first, mut second) = (vec![0u8; initial.max(16)], vec![0u8; initial.max(16)]);
        let (mut first_len, mut second_len) = (first.len(), second.len());
        let mut status =
            f(first.as_mut_ptr(), &mut first_len, second.as_mut_ptr(), &mut second_len);
        if status == ffi::ORACLE_E_BUFFER {
            first.resize(first_len.max(first.len()), 0);
            second.resize(second_len.max(second.len()), 0);
            (first_len, second_len) = (first.len(), second.len());
            status = f(first.as_mut_ptr(), &mut first_len, second.as_mut_ptr(), &mut second_len);
        }
        self.ok(status)?;
        first.truncate(first_len);
        second.truncate(second_len);
        Ok((first, second))
    }
}

/// A buffer of 32-byte hashes.
fn hashes32(bytes: &[u8]) -> Vec<[u8; 32]> {
    bytes.as_chunks::<32>().0.to_vec()
}
