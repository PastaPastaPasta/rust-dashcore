use hashes::Hash;

use crate::hash_types::{QuorumModifierHash, ScoreHash};
use crate::sml::masternode_list_entry::qualified_masternode_list_entry::QualifiedMasternodeListEntry;

impl QualifiedMasternodeListEntry {
    /// The score of a masternode list entry within a quorum expressed by a quorum modifier hash.
    pub fn score(&self, modifier: QuorumModifierHash) -> Option<ScoreHash> {
        if !self.masternode_list_entry.is_valid {
            return None;
        }
        let confirmed = self.confirmed_hash_hashed_with_pro_reg_tx?.to_byte_array();
        let score = dash_evo::sml::quorum_score(&confirmed, &modifier.to_byte_array());
        Some(ScoreHash::from_byte_array(score.0))
    }
}
