//! Pure quorum probe target selection.

use std::collections::HashSet;

use super::ValidAddr;

/// Selects the masternodes that Core's DKG probe phase queues and logs the quorum hash.
// Core: llmq/net_dkg.cpp:807-841
pub fn probe_targets(
    spork23_on: bool,
    quorum_hash: [u8; 32],
    members: &[[u8; 32]],
    me: [u8; 32],
    last_outbound: impl Fn(&[u8; 32]) -> i64,
    now: i64,
    tip_list: &dyn ValidAddr,
) -> (HashSet<[u8; 32]>, Vec<String>) {
    if !spork23_on {
        return (HashSet::new(), Vec::new());
    }

    let targets: HashSet<[u8; 32]> = members
        .iter()
        .copied()
        .filter(|pro_tx_hash| *pro_tx_hash != me)
        .filter(|pro_tx_hash| now - last_outbound(pro_tx_hash) >= 600)
        .collect();

    if targets.is_empty() {
        return (targets, Vec::new());
    }

    let mut log = vec![format!(
        "AddQuorumProbeConnections -- adding masternodes probes for quorum {}:",
        hash_hex(&quorum_hash)
    )];
    let mut ordered: Vec<_> = targets.iter().copied().collect();
    ordered.sort_unstable();
    for pro_tx_hash in ordered {
        let address = tip_list
            .primary_service(&pro_tx_hash)
            .unwrap_or_else(|| "not in valid MN set anymore".to_owned());
        log.push(format!("  {} ({address})", hash_hex(&pro_tx_hash)));
    }
    (targets, log)
}

fn hash_hex(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Addresses {
        valid: HashSet<[u8; 32]>,
    }

    impl ValidAddr for Addresses {
        fn primary_service(&self, pro_tx_hash: &[u8; 32]) -> Option<String> {
            self.valid.contains(pro_tx_hash).then(|| "10.0.0.1:19999".to_owned())
        }
    }

    fn hash(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn probe_targets_skips_self_and_uses_600_second_gate() {
        let addresses = Addresses {
            valid: HashSet::new(),
        };
        let me = hash(1);
        let members = [me, hash(2), hash(3), hash(4)];
        let (targets, _) = probe_targets(
            true,
            hash(9),
            &members,
            me,
            |pro_tx_hash| match pro_tx_hash[0] {
                2 => 401,
                3 => 400,
                _ => 399,
            },
            1000,
            &addresses,
        );
        assert_eq!(targets, HashSet::from([hash(3), hash(4)]));
    }

    #[test]
    fn probe_targets_respects_spork_23() {
        let addresses = Addresses {
            valid: HashSet::new(),
        };
        let members = [hash(1)];
        let (targets, log) =
            probe_targets(false, hash(9), &members, hash(2), |_| 0, 1000, &addresses);
        assert!(targets.is_empty());
        assert!(log.is_empty());
    }

    #[test]
    fn probe_targets_logs_valid_and_invalid_targets() {
        let addresses = Addresses {
            valid: HashSet::from([hash(2)]),
        };
        let members = [hash(2), hash(3)];
        let (_, log) = probe_targets(true, hash(9), &members, hash(1), |_| 0, 600, &addresses);
        assert_eq!(
            log,
            vec![
                format!(
                    "AddQuorumProbeConnections -- adding masternodes probes for quorum {}:",
                    "09".repeat(32)
                ),
                format!("  {} (10.0.0.1:19999)", "02".repeat(32)),
                format!("  {} (not in valid MN set anymore)", "03".repeat(32)),
            ]
        );
    }
}
