//! The preimages of the provider signatures the node builds itself (T6 §3.3): Core's
//! `SER_GETHASH` serialization of a ProRegTx, a ProUpRegTx and a ProUpSharedRegTx (every field but
//! the signatures), and `MakeSharedRegConsentHash`'s stream. Each takes the operator key as its
//! 48 bytes from the caller, which reads them from the payload side-car's key.

use dash_primitives::{
    CollateralShare, MnType, NIEntry, NetInfo, PayoutShare, ProRegTx, ProTxVersion, ProUpRegTx,
    ProUpSharedRegTx, Transaction,
};

use dash_types::Numeric as _;

use super::canonical::{write_output, write_prevout};
use super::serialize::{CoreWrite, write_bytes, write_compact_size};
use crate::hash::HashWriter;

/// A ProRegTx without its signature, the operator key written as `operator_key`.
// Core: evo/providertx.h CProRegTx::SerializationOp (SER_GETHASH)
pub struct ProRegTxUnsigned<'a> {
    pub payload: &'a ProRegTx,
    pub operator_key: [u8; 48],
}

/// A ProUpRegTx without its signature, the operator key written as `operator_key`.
// Core: evo/providertx.h CProUpRegTx::SerializationOp (SER_GETHASH)
pub struct ProUpRegTxUnsigned<'a> {
    pub payload: &'a ProUpRegTx,
    pub operator_key: [u8; 48],
}

/// A ProUpSharedRegTx without its signatures, the operator key written as `operator_key`.
// Core: evo/providertx.h CProUpSharedRegTx::SerializationOp (SER_GETHASH)
pub struct ProUpSharedRegTxUnsigned<'a> {
    pub payload: &'a ProUpSharedRegTx,
    pub operator_key: [u8; 48],
}

/// The stream whose SHA256d each share owner of a shared registration signs.
// Core: evo/providertx.cpp:380-418 (v24.0.0) MakeSharedRegConsentHash
pub struct SharedRegConsent<'a> {
    pub payload: &'a ProRegTx,
    pub operator_key: [u8; 48],
    pub tx: &'a Transaction,
}

impl ProRegTxUnsigned<'_> {
    /// The preimage's bytes.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, w: &mut impl CoreWrite) {
        let p = self.payload;
        let v24 = p.version() >= ProTxVersion::CoreV24;
        w.write(&u16::from(p.version()).to_le_bytes());
        w.write(&p.mn_type().to_base().to_le_bytes());
        w.write(&p.mode().to_le_bytes());
        w.write(p.collateral_hash().as_bytes());
        w.write(&p.collateral_index().to_le_bytes());
        write_net_info(p.net_info(), v24, w);
        w.write(p.key_id_owner().as_bytes());
        w.write(&self.operator_key);
        w.write(p.key_id_voting().as_bytes());
        w.write(&p.operator_reward().to_le_bytes());
        if v24 {
            write_payouts(p.payouts(), w);
            write_shares(p.shares(), w);
            for sig in p.join_sigs() {
                w.write(sig.as_bytes());
            }
            w.write(&p.early_period_blocks().to_le_bytes());
            w.write(&p.early_penalty().to_le_bytes());
        } else {
            write_bytes(p.script_payout().as_bytes(), w);
        }
        w.write(p.inputs_hash().as_bytes());
        if p.mn_type() == MnType::Evo {
            w.write(p.platform_node_id().unwrap_or_default().as_bytes());
            if !v24 {
                w.write(&p.platform_p2p_port().unwrap_or(0).to_le_bytes());
                w.write(&p.platform_http_port().unwrap_or(0).to_le_bytes());
            }
        }
    }
}

impl ProUpRegTxUnsigned<'_> {
    /// The preimage's bytes.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, w: &mut impl CoreWrite) {
        let p = self.payload;
        w.write(&u16::from(p.version()).to_le_bytes());
        w.write(p.pro_tx_hash().as_bytes());
        w.write(&p.mode().to_le_bytes());
        w.write(&self.operator_key);
        w.write(p.key_id_voting().as_bytes());
        if p.version() >= ProTxVersion::CoreV24 {
            write_payouts(p.payouts(), w);
        } else {
            write_bytes(p.script_payout().as_bytes(), w);
        }
        w.write(p.inputs_hash().as_bytes());
    }
}

impl ProUpSharedRegTxUnsigned<'_> {
    /// The preimage's bytes.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, w: &mut impl CoreWrite) {
        let p = self.payload;
        w.write(&p.version().to_le_bytes());
        w.write(p.pro_tx_hash().as_bytes());
        w.write(&self.operator_key);
        w.write(p.key_id_voting().as_bytes());
        w.write(p.inputs_hash().as_bytes());
    }
}

impl SharedRegConsent<'_> {
    /// The preimage's bytes.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, w: &mut impl CoreWrite) {
        let (p, tx) = (self.payload, self.tx);
        write_bytes(b"DashSharedMNReg", w);
        w.write(&u16::from(p.version()).to_le_bytes());
        w.write(&tx.version().to_le_bytes());
        w.write(&tx.tx_type().to_base().to_le_bytes());
        w.write(&tx.lock_time().to_le_bytes());
        let mut inputs = HashWriter::new();
        for input in tx.inputs() {
            write_prevout(&input.prevout(), &mut inputs);
        }
        w.write(&inputs.finish());
        for input in tx.inputs() {
            w.write(&input.sequence().to_le_bytes());
        }
        let mut outputs = HashWriter::new();
        for output in tx.outputs() {
            write_output(output.value(), output.script_pubkey().as_bytes(), &mut outputs);
        }
        w.write(&outputs.finish());
        w.write(&p.mn_type().to_base().to_le_bytes());
        w.write(&p.mode().to_le_bytes());
        write_net_info(p.net_info(), p.version() >= ProTxVersion::CoreV24, w);
        if p.mn_type() == MnType::Evo {
            w.write(p.platform_node_id().unwrap_or_default().as_bytes());
        }
        w.write(p.key_id_voting().as_bytes());
        w.write(&self.operator_key);
        w.write(&p.operator_reward().to_le_bytes());
        write_shares(p.shares(), w);
        w.write(&p.early_period_blocks().to_le_bytes());
        w.write(&p.early_penalty().to_le_bytes());
    }
}

/// A list's count as the provider payloads write it: one byte, wrapping past 255.
fn write_count(n: usize, w: &mut impl CoreWrite) {
    w.write(&[n as u8]);
}

fn write_payouts(payouts: &[PayoutShare], w: &mut impl CoreWrite) {
    write_count(payouts.len(), w);
    for payout in payouts {
        write_bytes(payout.script_payout().as_bytes(), w);
        w.write(&payout.reward().to_le_bytes());
    }
}

fn write_shares(shares: &[CollateralShare], w: &mut impl CoreWrite) {
    write_count(shares.len(), w);
    for share in shares {
        w.write(&share.amount().to_le_bytes());
        write_bytes(share.script_refund().as_bytes(), w);
        write_bytes(share.script_reward().as_bytes(), w);
        w.write(share.key_id_owner().as_bytes());
    }
}

/// The network info the payload version selects (extended from version 3); a variant the version
/// does not select writes nothing. An extended info of a version Core does not know (only 1 is)
/// is its version byte alone.
// Core: evo/netinfo.h:327 MnNetInfo::Serialize, :412 ExtNetInfo::Serialize
pub fn write_net_info(net_info: &NetInfo, extended: bool, w: &mut impl CoreWrite) {
    match net_info {
        NetInfo::Legacy(v1) if !extended => {
            let service = v1.service();
            w.write(service.addr().as_bytes());
            w.write(&service.port().to_be_bytes());
        }
        NetInfo::Extended(v2) if extended => {
            w.write(&[v2.version()]);
            if v2.version() != 1 {
                return;
            }
            write_compact_size(v2.groups().len() as u64, w);
            for (purpose, group) in v2.groups() {
                w.write(&[purpose.to_base()]);
                write_compact_size(group.len() as u64, w);
                for entry in group {
                    match entry {
                        NIEntry::Service(service) => {
                            w.write(&[0x01, service.addr().network().to_base()]);
                            write_bytes(service.addr().bytes(), w);
                            w.write(&service.port().to_be_bytes());
                        }
                        NIEntry::Domain {
                            name,
                            port,
                        } => {
                            w.write(&[0x02]);
                            write_bytes(name, w);
                            w.write(&port.to_be_bytes());
                        }
                        NIEntry::Invalid => w.write(&[0xff]),
                    }
                }
            }
        }
        _ => {}
    }
}
