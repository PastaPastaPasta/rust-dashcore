//! Every reject reason of EV1's checks at the pin (EV1 §2.2, §3.13), one `&'static str` constant
//! per distinct string, and [`CLASSES`]: the class Core passes with each, from the census
//! (`contrib/node/ev1-census.py`, `tests/data/ev1_census.tsv`), which `census_matches_pin`
//! checks both against.

use dash_consensus::validation::TxValidationResult;

pub const BAD_TX_TYPE_DIP3_INACTIVE: &str = "bad-tx-type-dip3-inactive";
pub const BAD_CBTX_INVALID: &str = "bad-cbtx-invalid";
pub const BAD_CBTX_PAYLOAD: &str = "bad-cbtx-payload";
pub const FAILED_CHECK_SPECIAL_TX: &str = "failed-check-special-tx";
pub const BAD_TX_TYPE_CHECK: &str = "bad-tx-type-check";
pub const BAD_PROTX_SIG: &str = "bad-protx-sig";
pub const BAD_PROTX_TYPE: &str = "bad-protx-type";
pub const BAD_PROTX_PAYLOAD: &str = "bad-protx-payload";
pub const BAD_PROTX_VERSION: &str = "bad-protx-version";
pub const BAD_PROTX_VERSION_DOWNGRADE: &str = "bad-protx-version-downgrade";
pub const BAD_PROTX_VERSION_DISALLOWED: &str = "bad-protx-version-disallowed";
pub const BAD_PROTX_SHARES_EVO: &str = "bad-protx-shares-evo";
pub const BAD_PROTX_PAYOUTS_EVO: &str = "bad-protx-payouts-evo";
pub const BAD_PROTX_COLLATERAL: &str = "bad-protx-collateral";
pub const BAD_PROTX_COLLATERAL_DEST: &str = "bad-protx-collateral-dest";
pub const BAD_PROTX_COLLATERAL_PKH: &str = "bad-protx-collateral-pkh";
pub const BAD_PROTX_COLLATERAL_INDEX: &str = "bad-protx-collateral-index";
pub const BAD_PROTX_SHARES_COLLATERAL_SCRIPT: &str = "bad-protx-shares-collateral-script";
pub const BAD_PROTX_DUP_NETINFO_ENTRY: &str = "bad-protx-dup-netinfo-entry";
pub const BAD_PROTX_NETINFO_ENTRY: &str = "bad-protx-netinfo-entry";
pub const BAD_PROTX_DUP_KEY: &str = "bad-protx-dup-key";
pub const BAD_PROTX_DUP_PLATFORMNODEID: &str = "bad-protx-dup-platformnodeid";
pub const BAD_PROTX_KEY_NOT_SAME: &str = "bad-protx-key-not-same";
pub const BAD_PROTX_SHARES_SIG: &str = "bad-protx-shares-sig";
pub const BAD_PROTX_HASH: &str = "bad-protx-hash";
pub const BAD_PROTX_TYPE_MISMATCH: &str = "bad-protx-type-mismatch";
pub const BAD_PROTX_OPERATOR_PAYEE: &str = "bad-protx-operator-payee";
pub const BAD_PROTX_SHARED_MN: &str = "bad-protx-shared-mn";
pub const BAD_PROUPSHARE_TOO_EARLY: &str = "bad-proupshare-too-early";
pub const BAD_PROUPSHARE_HASH: &str = "bad-proupshare-hash";
pub const BAD_PROUPSHARE_NOT_SHARED: &str = "bad-proupshare-not-shared";
pub const BAD_PROUPSHARE_INDEX: &str = "bad-proupshare-index";
pub const BAD_PROUPSHARE_PAYEE_DEST: &str = "bad-proupshare-payee-dest";
pub const BAD_PROUPSHARE_PAYEE_REUSE: &str = "bad-proupshare-payee-reuse";
pub const BAD_PROUPSHARE_SIG: &str = "bad-proupshare-sig";
pub const BAD_PROUPSHAREDREG_TOO_EARLY: &str = "bad-proupsharedreg-too-early";
pub const BAD_PROUPSHAREDREG_HASH: &str = "bad-proupsharedreg-hash";
pub const BAD_PROUPSHAREDREG_NOT_SHARED: &str = "bad-proupsharedreg-not-shared";
pub const BAD_PROUPSHAREDREG_SIG_COUNT: &str = "bad-proupsharedreg-sig-count";
pub const BAD_PROUPSHAREDREG_PAYEE_REUSE: &str = "bad-proupsharedreg-payee-reuse";
pub const BAD_PROUPSHAREDREG_SIG: &str = "bad-proupsharedreg-sig";
pub const BAD_PRODIS_HASH: &str = "bad-prodis-hash";
pub const BAD_PRODIS_NOT_SHARED: &str = "bad-prodis-not-shared";
pub const BAD_PRODIS_INPUT: &str = "bad-prodis-input";
pub const BAD_PRODIS_ACTOR: &str = "bad-prodis-actor";
pub const BAD_PRODIS_SIG_COUNT: &str = "bad-prodis-sig-count";
pub const BAD_PRODIS_PAYEE_COUNT: &str = "bad-prodis-payee-count";
pub const BAD_PRODIS_PAYEE: &str = "bad-prodis-payee";
pub const BAD_PRODIS_PENALTY_FLOOR: &str = "bad-prodis-penalty-floor";
pub const BAD_PRODIS_ACTOR_OUTPUT_ZERO: &str = "bad-prodis-actor-output-zero";
pub const BAD_PRODIS_PENALTY_SUM: &str = "bad-prodis-penalty-sum";
pub const BAD_PRODIS_FEE: &str = "bad-prodis-fee";
pub const BAD_PRODIS_BONUS: &str = "bad-prodis-bonus";
pub const BAD_PRODIS_SIG: &str = "bad-prodis-sig";
pub const BAD_PRODIS_TOO_EARLY: &str = "bad-prodis-too-early";
pub const BAD_SHARED_COLLATERAL_SPEND: &str = "bad-shared-collateral-spend";
pub const BAD_SHARED_COLLATERAL_CREATE: &str = "bad-shared-collateral-create";
pub const BAD_PROTX_PAYOUTS_COUNT: &str = "bad-protx-payouts-count";
pub const BAD_PROTX_PAYOUT_REWARD: &str = "bad-protx-payout-reward";
pub const BAD_PROTX_PAYEE: &str = "bad-protx-payee";
pub const BAD_PROTX_PAYEE_DUP: &str = "bad-protx-payee-dup";
pub const BAD_PROTX_PAYEE_DEST: &str = "bad-protx-payee-dest";
pub const BAD_PROTX_PAYEE_REUSE: &str = "bad-protx-payee-reuse";
pub const BAD_PROTX_PAYOUT_REWARD_SUM: &str = "bad-protx-payout-reward-sum";
pub const BAD_PROTX_SHARES_COUNT: &str = "bad-protx-shares-count";
pub const BAD_PROTX_SHARES_EARLY_PERIOD: &str = "bad-protx-shares-early-period";
pub const BAD_PROTX_SHARES_AMOUNT: &str = "bad-protx-shares-amount";
pub const BAD_PROTX_SHARES_KEY_NULL: &str = "bad-protx-shares-key-null";
pub const BAD_PROTX_SHARES_DUP_KEY: &str = "bad-protx-shares-dup-key";
pub const BAD_PROTX_SHARES_DUP_REFUND: &str = "bad-protx-shares-dup-refund";
pub const BAD_PROTX_SHARES_PAYEE: &str = "bad-protx-shares-payee";
pub const BAD_PROTX_SHARES_PAYEE_DEST: &str = "bad-protx-shares-payee-dest";
pub const BAD_PROTX_SHARES_PAYEE_REUSE: &str = "bad-protx-shares-payee-reuse";
pub const BAD_PROTX_SHARES_AMOUNT_SUM: &str = "bad-protx-shares-amount-sum";
pub const BAD_PROTX_SHARES_PENALTY: &str = "bad-protx-shares-penalty";
pub const BAD_PROTX_COLLATERAL_REUSE: &str = "bad-protx-collateral-reuse";
pub const BAD_PROTX_NETINFO_EMPTY: &str = "bad-protx-netinfo-empty";
pub const BAD_PROTX_NETINFO_BAD: &str = "bad-protx-netinfo-bad";
pub const BAD_PROTX_NETINFO_ADDR: &str = "bad-protx-netinfo-addr";
pub const BAD_PROTX_NETINFO_PORT: &str = "bad-protx-netinfo-port";
pub const BAD_PROTX_NETINFO_ADDR_TYPE: &str = "bad-protx-netinfo-addr-type";
pub const BAD_PROTX_NETINFO_ADDR_UNROUTABLE: &str = "bad-protx-netinfo-addr-unroutable";
pub const BAD_PROTX_NETINFO_MAXLIMIT: &str = "bad-protx-netinfo-maxlimit";
pub const BAD_PROTX_NETINFO_VERSION: &str = "bad-protx-netinfo-version";
pub const BAD_PROTX_PLATFORM_NODEID: &str = "bad-protx-platform-nodeid";
pub const BAD_PROTX_PLATFORM_P2P_PORT: &str = "bad-protx-platform-p2p-port";
pub const BAD_PROTX_PLATFORM_HTTP_PORT: &str = "bad-protx-platform-http-port";
pub const BAD_PROTX_PLATFORM_DUP_PORTS: &str = "bad-protx-platform-dup-ports";
pub const BAD_PROTX_EVO_VERSION: &str = "bad-protx-evo-version";
pub const BAD_PROTX_MODE: &str = "bad-protx-mode";
pub const BAD_PROTX_SHARES_EXTERNAL: &str = "bad-protx-shares-external";
pub const BAD_PROTX_SHARES_OWNER_KEY: &str = "bad-protx-shares-owner-key";
pub const BAD_PROTX_SHARES_PAYOUTS: &str = "bad-protx-shares-payouts";
pub const BAD_PROTX_SHARES_EMPTY_FIELDS: &str = "bad-protx-shares-empty-fields";
pub const BAD_PROTX_KEY_NULL: &str = "bad-protx-key-null";
pub const BAD_PROTX_OPERATOR_PUBKEY: &str = "bad-protx-operator-pubkey";
pub const BAD_PROTX_OPERATOR_REWARD: &str = "bad-protx-operator-reward";
pub const BAD_PROTX_REASON: &str = "bad-protx-reason";
pub const BAD_PRODIS_VERSION: &str = "bad-prodis-version";
pub const BAD_PROUPSHARE_VERSION: &str = "bad-proupshare-version";
pub const BAD_PROUPSHARE_SIG_SIZE: &str = "bad-proupshare-sig-size";
pub const BAD_PROUPSHARE_PAYEE: &str = "bad-proupshare-payee";
pub const BAD_PROUPSHAREDREG_VERSION: &str = "bad-proupsharedreg-version";
pub const BAD_PROTX_INPUTS_HASH: &str = "bad-protx-inputs-hash";
pub const BAD_CBTX_VERSION: &str = "bad-cbtx-version";
pub const BAD_CBTX_HEIGHT: &str = "bad-cbtx-height";
pub const BAD_MNHF_VERSION: &str = "bad-mnhf-version";
pub const BAD_MNHF_NBIT_OUT_OF_BOUNDS: &str = "bad-mnhf-nbit-out-of-bounds";
pub const BAD_MNHF_TYPE: &str = "bad-mnhf-type";
pub const BAD_MNHF_PAYLOAD: &str = "bad-mnhf-payload";
pub const BAD_MNHF_NON_EHF: &str = "bad-mnhf-non-ehf";
pub const BAD_MNHF_QUORUM_HASH: &str = "bad-mnhf-quorum-hash";
pub const BAD_MNHF_MISSING_QUORUM: &str = "bad-mnhf-missing-quorum";
pub const BAD_MNHF_INVALID: &str = "bad-mnhf-invalid";
pub const BAD_ASSETLOCKTX_TYPE: &str = "bad-assetlocktx-type";
pub const BAD_ASSETLOCKTX_NON_EMPTY_RETURN: &str = "bad-assetlocktx-non-empty-return";
pub const BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE: &str = "bad-assetlocktx-opreturn-outofrange";
pub const BAD_ASSETLOCKTX_MULTIPLE_RETURN: &str = "bad-assetlocktx-multiple-return";
pub const BAD_ASSETLOCKTX_NO_RETURN: &str = "bad-assetlocktx-no-return";
pub const BAD_ASSETLOCKTX_PAYLOAD: &str = "bad-assetlocktx-payload";
pub const BAD_ASSETLOCKTX_VERSION: &str = "bad-assetlocktx-version";
pub const BAD_ASSETLOCKTX_VERSION_2: &str = "bad-assetlocktx-version-2";
pub const BAD_ASSETLOCKTX_EMPTYCREDITOUTPUTS: &str = "bad-assetlocktx-emptycreditoutputs";
pub const BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE: &str = "bad-assetlocktx-credit-outofrange";
pub const BAD_ASSETLOCKTX_SCRIPT_PUBKEY: &str = "bad-assetlocktx-script-pubkey";
pub const BAD_ASSETLOCKTX_PUBKEYHASH: &str = "bad-assetlocktx-pubKeyHash";
pub const BAD_ASSETLOCKTX_CREDITAMOUNT: &str = "bad-assetlocktx-creditamount";
pub const BAD_ASSETUNLOCK_TOO_OLD_QUORUM: &str = "bad-assetunlock-too-old-quorum";
pub const BAD_ASSETUNLOCK_TOO_LATE: &str = "bad-assetunlock-too-late";
pub const BAD_ASSETUNLOCK_QUORUM_INTERNAL_ERROR: &str = "bad-assetunlock-quorum-internal-error";
pub const BAD_ASSETUNLOCK_NOT_VERIFIED: &str = "bad-assetunlock-not-verified";
pub const BAD_ASSETUNLOCKTX_TYPE: &str = "bad-assetunlocktx-type";
pub const BAD_ASSETUNLOCKTX_HAVE_INPUT: &str = "bad-assetunlocktx-have-input";
pub const BAD_ASSETUNLOCKTX_TOO_MANY_OUTS: &str = "bad-assetunlocktx-too-many-outs";
pub const BAD_ASSETUNLOCKTX_PAYLOAD: &str = "bad-assetunlocktx-payload";
pub const BAD_ASSETUNLOCKTX_VERSION: &str = "bad-assetunlocktx-version";
pub const BAD_ASSETUNLOCKTX_VERSION_2: &str = "bad-assetunlocktx-version-2";
pub const BAD_ASSETUNLOCK_DUPLICATED_INDEX: &str = "bad-assetunlock-duplicated-index";
pub const BAD_ASSETUNLOCK_QUORUM_HASH: &str = "bad-assetunlock-quorum-hash";
pub const BAD_QC_PAYLOAD: &str = "bad-qc-payload";
pub const BAD_QC_COMMITMENT_TYPE: &str = "bad-qc-commitment-type";
pub const BAD_QC_VERSION: &str = "bad-qc-version";
pub const BAD_QC_HEIGHT: &str = "bad-qc-height";
pub const BAD_QC_QUORUM_HASH: &str = "bad-qc-quorum-hash";
pub const BAD_QC_INVALID_NULL: &str = "bad-qc-invalid-null";
pub const BAD_QC_INVALID: &str = "bad-qc-invalid";

/// Every `(reason, class)` pair EV1's functions reject with, in census order: the literal sites,
/// then the forwarding sites. Three reasons carry both classes: `bad-protx-collateral`,
/// `bad-protx-collateral-dest`, and `bad-protx-netinfo-version` (a C literal, and forwarded from
/// the migration as S).
pub const CLASSES: &[(&str, TxValidationResult)] = &[
    (BAD_TX_TYPE_DIP3_INACTIVE, TxValidationResult::BadSpecial),
    (BAD_CBTX_INVALID, TxValidationResult::Consensus),
    (BAD_CBTX_PAYLOAD, TxValidationResult::Consensus),
    (FAILED_CHECK_SPECIAL_TX, TxValidationResult::Consensus),
    (BAD_TX_TYPE_CHECK, TxValidationResult::BadSpecial),
    (BAD_PROTX_SIG, TxValidationResult::Consensus),
    (BAD_PROTX_TYPE, TxValidationResult::Consensus),
    (BAD_PROTX_PAYLOAD, TxValidationResult::Consensus),
    (BAD_PROTX_VERSION, TxValidationResult::Consensus),
    (BAD_PROTX_VERSION_DOWNGRADE, TxValidationResult::Consensus),
    (BAD_PROTX_VERSION_DISALLOWED, TxValidationResult::Consensus),
    (BAD_PROTX_SHARES_EVO, TxValidationResult::BadSpecial),
    (BAD_PROTX_PAYOUTS_EVO, TxValidationResult::BadSpecial),
    (BAD_PROTX_COLLATERAL, TxValidationResult::BadSpecial),
    (BAD_PROTX_COLLATERAL_DEST, TxValidationResult::BadSpecial),
    (BAD_PROTX_COLLATERAL_PKH, TxValidationResult::BadSpecial),
    (BAD_PROTX_COLLATERAL_INDEX, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_COLLATERAL_SCRIPT, TxValidationResult::BadSpecial),
    (BAD_PROTX_DUP_NETINFO_ENTRY, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_ENTRY, TxValidationResult::BadSpecial),
    (BAD_PROTX_DUP_KEY, TxValidationResult::BadSpecial),
    (BAD_PROTX_DUP_PLATFORMNODEID, TxValidationResult::BadSpecial),
    (BAD_PROTX_KEY_NOT_SAME, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_SIG, TxValidationResult::BadSpecial),
    (BAD_PROTX_HASH, TxValidationResult::Consensus),
    (BAD_PROTX_TYPE_MISMATCH, TxValidationResult::Consensus),
    (BAD_PROTX_OPERATOR_PAYEE, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARED_MN, TxValidationResult::BadSpecial),
    (BAD_PROTX_COLLATERAL, TxValidationResult::Consensus),
    (BAD_PROTX_COLLATERAL_DEST, TxValidationResult::Consensus),
    (BAD_PROUPSHARE_TOO_EARLY, TxValidationResult::Consensus),
    (BAD_PROUPSHARE_HASH, TxValidationResult::BadSpecial),
    (BAD_PROUPSHARE_NOT_SHARED, TxValidationResult::BadSpecial),
    (BAD_PROUPSHARE_INDEX, TxValidationResult::BadSpecial),
    (BAD_PROUPSHARE_PAYEE_DEST, TxValidationResult::BadSpecial),
    (BAD_PROUPSHARE_PAYEE_REUSE, TxValidationResult::BadSpecial),
    (BAD_PROUPSHARE_SIG, TxValidationResult::BadSpecial),
    (BAD_PROUPSHAREDREG_TOO_EARLY, TxValidationResult::Consensus),
    (BAD_PROUPSHAREDREG_HASH, TxValidationResult::BadSpecial),
    (BAD_PROUPSHAREDREG_NOT_SHARED, TxValidationResult::BadSpecial),
    (BAD_PROUPSHAREDREG_SIG_COUNT, TxValidationResult::BadSpecial),
    (BAD_PROUPSHAREDREG_PAYEE_REUSE, TxValidationResult::BadSpecial),
    (BAD_PROUPSHAREDREG_SIG, TxValidationResult::BadSpecial),
    (BAD_PRODIS_HASH, TxValidationResult::BadSpecial),
    (BAD_PRODIS_NOT_SHARED, TxValidationResult::BadSpecial),
    (BAD_PRODIS_INPUT, TxValidationResult::BadSpecial),
    (BAD_PRODIS_ACTOR, TxValidationResult::BadSpecial),
    (BAD_PRODIS_SIG_COUNT, TxValidationResult::BadSpecial),
    (BAD_PRODIS_PAYEE_COUNT, TxValidationResult::BadSpecial),
    (BAD_PRODIS_PAYEE, TxValidationResult::BadSpecial),
    (BAD_PRODIS_PENALTY_FLOOR, TxValidationResult::BadSpecial),
    (BAD_PRODIS_ACTOR_OUTPUT_ZERO, TxValidationResult::BadSpecial),
    (BAD_PRODIS_PENALTY_SUM, TxValidationResult::BadSpecial),
    (BAD_PRODIS_FEE, TxValidationResult::BadSpecial),
    (BAD_PRODIS_BONUS, TxValidationResult::BadSpecial),
    (BAD_PRODIS_SIG, TxValidationResult::BadSpecial),
    (BAD_PRODIS_TOO_EARLY, TxValidationResult::Consensus),
    (BAD_SHARED_COLLATERAL_SPEND, TxValidationResult::Consensus),
    (BAD_SHARED_COLLATERAL_CREATE, TxValidationResult::Consensus),
    (BAD_PROTX_PAYOUTS_COUNT, TxValidationResult::BadSpecial),
    (BAD_PROTX_PAYOUT_REWARD, TxValidationResult::BadSpecial),
    (BAD_PROTX_PAYEE, TxValidationResult::BadSpecial),
    (BAD_PROTX_PAYEE_DUP, TxValidationResult::BadSpecial),
    (BAD_PROTX_PAYEE_DEST, TxValidationResult::BadSpecial),
    (BAD_PROTX_PAYEE_REUSE, TxValidationResult::BadSpecial),
    (BAD_PROTX_PAYOUT_REWARD_SUM, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_COUNT, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_EARLY_PERIOD, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_AMOUNT, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_KEY_NULL, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_DUP_KEY, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_DUP_REFUND, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_PAYEE, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_PAYEE_DEST, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_PAYEE_REUSE, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_AMOUNT_SUM, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_PENALTY, TxValidationResult::BadSpecial),
    (BAD_PROTX_COLLATERAL_REUSE, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_EMPTY, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_BAD, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_ADDR, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_PORT, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_ADDR_TYPE, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_ADDR_UNROUTABLE, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_MAXLIMIT, TxValidationResult::BadSpecial),
    (BAD_PROTX_NETINFO_VERSION, TxValidationResult::Consensus),
    (BAD_PROTX_PLATFORM_NODEID, TxValidationResult::BadSpecial),
    (BAD_PROTX_PLATFORM_P2P_PORT, TxValidationResult::BadSpecial),
    (BAD_PROTX_PLATFORM_HTTP_PORT, TxValidationResult::BadSpecial),
    (BAD_PROTX_PLATFORM_DUP_PORTS, TxValidationResult::BadSpecial),
    (BAD_PROTX_EVO_VERSION, TxValidationResult::Consensus),
    (BAD_PROTX_MODE, TxValidationResult::Consensus),
    (BAD_PROTX_SHARES_EXTERNAL, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_OWNER_KEY, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_PAYOUTS, TxValidationResult::BadSpecial),
    (BAD_PROTX_SHARES_EMPTY_FIELDS, TxValidationResult::BadSpecial),
    (BAD_PROTX_KEY_NULL, TxValidationResult::BadSpecial),
    (BAD_PROTX_OPERATOR_PUBKEY, TxValidationResult::BadSpecial),
    (BAD_PROTX_OPERATOR_REWARD, TxValidationResult::BadSpecial),
    (BAD_PROTX_REASON, TxValidationResult::Consensus),
    (BAD_PRODIS_VERSION, TxValidationResult::Consensus),
    (BAD_PROUPSHARE_VERSION, TxValidationResult::Consensus),
    (BAD_PROUPSHARE_SIG_SIZE, TxValidationResult::BadSpecial),
    (BAD_PROUPSHARE_PAYEE, TxValidationResult::BadSpecial),
    (BAD_PROUPSHAREDREG_VERSION, TxValidationResult::Consensus),
    (BAD_PROTX_INPUTS_HASH, TxValidationResult::Consensus),
    (BAD_CBTX_VERSION, TxValidationResult::Consensus),
    (BAD_CBTX_HEIGHT, TxValidationResult::Consensus),
    (BAD_MNHF_VERSION, TxValidationResult::Consensus),
    (BAD_MNHF_NBIT_OUT_OF_BOUNDS, TxValidationResult::Consensus),
    (BAD_MNHF_TYPE, TxValidationResult::Consensus),
    (BAD_MNHF_PAYLOAD, TxValidationResult::Consensus),
    (BAD_MNHF_NON_EHF, TxValidationResult::Consensus),
    (BAD_MNHF_QUORUM_HASH, TxValidationResult::Consensus),
    (BAD_MNHF_MISSING_QUORUM, TxValidationResult::Consensus),
    (BAD_MNHF_INVALID, TxValidationResult::Consensus),
    (BAD_ASSETLOCKTX_TYPE, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_NON_EMPTY_RETURN, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_MULTIPLE_RETURN, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_NO_RETURN, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_PAYLOAD, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_VERSION, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_VERSION_2, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_EMPTYCREDITOUTPUTS, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_SCRIPT_PUBKEY, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_PUBKEYHASH, TxValidationResult::BadSpecial),
    (BAD_ASSETLOCKTX_CREDITAMOUNT, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCK_TOO_OLD_QUORUM, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCK_TOO_LATE, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCK_QUORUM_INTERNAL_ERROR, TxValidationResult::Consensus),
    (BAD_ASSETUNLOCK_NOT_VERIFIED, TxValidationResult::Consensus),
    (BAD_ASSETUNLOCKTX_TYPE, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCKTX_HAVE_INPUT, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCKTX_TOO_MANY_OUTS, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCKTX_PAYLOAD, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCKTX_VERSION, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCKTX_VERSION_2, TxValidationResult::BadSpecial),
    (BAD_ASSETUNLOCK_DUPLICATED_INDEX, TxValidationResult::Consensus),
    (BAD_ASSETUNLOCK_QUORUM_HASH, TxValidationResult::Consensus),
    (BAD_QC_PAYLOAD, TxValidationResult::Consensus),
    (BAD_QC_COMMITMENT_TYPE, TxValidationResult::Consensus),
    (BAD_QC_VERSION, TxValidationResult::Consensus),
    (BAD_QC_HEIGHT, TxValidationResult::Consensus),
    (BAD_QC_QUORUM_HASH, TxValidationResult::Consensus),
    (BAD_QC_INVALID_NULL, TxValidationResult::Consensus),
    (BAD_QC_INVALID, TxValidationResult::Consensus),
    (BAD_PROTX_NETINFO_VERSION, TxValidationResult::BadSpecial),
];

#[cfg(test)]
mod tests;
