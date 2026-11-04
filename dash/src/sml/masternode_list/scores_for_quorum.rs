use std::collections::BTreeMap;

use dash_evo::sml::{ScoredEntry, calculate_quorum, scores_for_quorum};
use dash_evo::types::RawMnType;
use dash_primitives::OutPoint;
use hashes::Hash;

use crate::Network;
use crate::hash_types::{QuorumModifierHash, ScoreHash};
use crate::network::message_qrinfo::QuorumSnapshot;
use crate::sml::llmq_type::{LLMQType, network::NetworkLLMQExt};
use crate::sml::masternode_list::MasternodeList;
use crate::sml::masternode_list_entry::EntryMasternodeType;
use crate::sml::masternode_list_entry::qualified_masternode_list_entry::QualifiedMasternodeListEntry;
use crate::sml::quorum_entry::qualified_quorum_entry::QualifiedQuorumEntry;
use crate::sml::quorum_entry::quorum_modifier_type::LLMQModifierType;

impl MasternodeList {
    pub fn valid_masternodes_for_quorum<'a, T>(
        &'a self,
        quorum: &QualifiedQuorumEntry,
        quorum_modifier: LLMQModifierType,
        network: Network,
    ) -> T
    where
        T: FromIterator<&'a QualifiedMasternodeListEntry>,
    {
        let llmq_type = quorum.quorum_entry.llmq_type;
        let hpmn_only = llmq_type == network.platform_type();
        let quorum_modifier = quorum_modifier.build_llmq_hash();
        Self::calculate_quorum_for_masternodes(
            self.masternodes.values().map(|mn| &**mn),
            quorum_modifier,
            hpmn_only,
            Some(llmq_type.size() as usize),
        )
        .into_iter()
        .collect()
    }

    pub fn used_and_unused_masternodes_for_quorum(
        &self,
        quorum_llmq_type: LLMQType,
        quorum_modifier: LLMQModifierType,
        quorum_snapshot: &QuorumSnapshot,
        network: Network,
    ) -> (Vec<&QualifiedMasternodeListEntry>, Vec<&QualifiedMasternodeListEntry>) {
        let hpmn_only = quorum_llmq_type == network.platform_type();
        let quorum_modifier = quorum_modifier.build_llmq_hash();
        let masternode_entry_list = Self::calculate_quorum_for_masternodes(
            self.masternodes.values().map(|mn| &**mn),
            quorum_modifier,
            hpmn_only,
            None,
        );
        let mut i = 0;
        masternode_entry_list.into_iter().partition(|_| {
            let used = quorum_snapshot.active_quorum_members.get(i).copied().unwrap_or_default();
            i += 1;
            used
        })
    }

    /// Core's `CalculateQuorum` over the simplified list, `dash_evo::sml::calculate_quorum` (SB7):
    /// with no collateral to break them, equal scores keep the input order.
    pub fn calculate_quorum_for_masternodes<'a, T>(
        entries: T,
        quorum_modifier: QuorumModifierHash,
        hpmn_only: bool,
        max_size: Option<usize>,
    ) -> Vec<&'a QualifiedMasternodeListEntry>
    where
        T: IntoIterator<Item = &'a QualifiedMasternodeListEntry>,
    {
        calculate_quorum(entries, &quorum_modifier.to_byte_array(), hpmn_only, max_size)
    }

    /// Each valid, confirmed entry (Evo only with `hpmn_only`) by its score for the modifier,
    /// lowest first: `dash_evo::sml::scores_for_quorum` (SB7). Keyed by score, so it keeps one of
    /// two tied entries; quorum members come from [`Self::calculate_quorum_for_masternodes`].
    pub fn scores_for_quorum_for_masternodes<'a, T>(
        entries: T,
        quorum_modifier: QuorumModifierHash,
        hpmn_only: bool,
    ) -> BTreeMap<ScoreHash, &'a QualifiedMasternodeListEntry>
    where
        T: IntoIterator<Item = &'a QualifiedMasternodeListEntry>,
    {
        // `ScoreHash` orders as `QuorumScore` does, as a number.
        scores_for_quorum(entries, &quorum_modifier.to_byte_array(), hpmn_only)
            .into_iter()
            .map(|(score, entry)| (ScoreHash::from_byte_array(score.0), entry))
            .collect()
    }

    pub fn scores_for_quorum(
        &self,
        quorum_modifier: QuorumModifierHash,
        hpmn_only: bool,
    ) -> BTreeMap<ScoreHash, &QualifiedMasternodeListEntry> {
        Self::scores_for_quorum_for_masternodes(
            self.masternodes.values().map(|mn| &**mn),
            quorum_modifier,
            hpmn_only,
        )
    }
}

impl ScoredEntry for QualifiedMasternodeListEntry {
    fn is_valid(&self) -> bool {
        self.masternode_list_entry.is_valid
    }

    fn mn_type(&self) -> RawMnType {
        match self.masternode_list_entry.mn_type {
            EntryMasternodeType::Regular => RawMnType::REGULAR,
            EntryMasternodeType::HighPerformance {
                ..
            } => RawMnType::EVO,
        }
    }

    fn confirmed_hash_with_pro_reg_tx(&self) -> Option<[u8; 32]> {
        self.confirmed_hash_hashed_with_pro_reg_tx.map(|hash| hash.to_byte_array())
    }

    fn collateral_outpoint(&self) -> Option<OutPoint> {
        None
    }
}
