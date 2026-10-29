//! EV2b's check-layer scaffolding (D56): Core's check loop as `ProcessSpecialTxsInBlock` runs it
//! before the build (v24 `specialtxman.cpp:853-879`; the shim's `DmlBlockCheckLoop`), for
//! [`crate::dml_block`] to compare with Core's verdict, transaction by transaction and step by
//! step.
//!
//! This is test scaffolding, not the node's check layer. It composes dash-dml's pieces (EV1-1's
//! [`network`](dash_dml::specialtx::network) checks, `netinfo::migration_reject`, the list,
//! [`unknown_pro_tx_version`], which the build shares) and adds no consensus logic to dash-dml.
//! Each step of Core's loop is a [`Step`] of a [`Table`] in Core's order (EV1 §3.2–3.17), either
//! implemented here or owned by the EV1 slice that lands it ([`Step::owner`]): the node's
//! missing checks. Each EV1 slice deletes its part of this module and its rows as part of its
//! acceptance (`missing_checks_have_not_landed` fails once a row's code lands), and EV1-3's
//! `check_special_tx_inner` replaces what is left.
//!
//! The comparison ([`compare`]) follows Core's rejection point. Our rejection of an earlier
//! transaction than Core's is a divergence, as is a different reason. Our acceptance of Core's
//! transaction, or a rejection of ours with another reason, is out of domain only when a step we
//! lack, reached before ours, gives Core's reason and no step of ours we passed does (along a
//! transaction's walk no reason is both ours and one we lack: `check_tables_are_consistent`);
//! where a step from ours on could give it too, Core's message does not place its rejection and
//! the outcome is [`CheckOutcome::Ambiguous`], not compared. When Core's loop accepts, any
//! rejection of ours is a divergence.

use std::sync::Arc;

use dash_consensus::BlockValidationResult;
use dash_consensus::primitives::{Block, Tx};
use dash_consensus::validation::TxValidationError;
use dash_dml::netinfo::{self, NetInfoRules, migration_reject};
use dash_dml::specialtx::reasons::*;
use dash_dml::specialtx::trivial::{
    OwnerPayouts, collateral_amount, is_payout_list_trivially_valid, is_share_list_trivially_valid,
};
use dash_dml::specialtx::{
    ProviderNetworkFields, check_provider_network_fields, is_net_info_trivially_valid, version_max,
};
use dash_dml::{
    DmlParams, Dmn, KeyId, MnList, MnType, ProTxHash, RawMnType, payload, payload_entries,
    unknown_pro_tx_version,
};
use dash_oracle_sys::Rejection;
use dash_params::ChainParams;
use dash_pkc::bls::{BlsLazyPk, BlsSchemeKind};
use dash_primitives::{
    MnType as PayloadMnType, NetInfo, OutPoint, PlatformNodeId, ProRegTx, ProUpRegTx, ProUpRevTx,
    ProUpServTx, RevocationReason, SpecialPayload, TxHash, TxType,
};
use dash_types::Numeric;

use crate::checks::result_code;
use crate::dml::{CORE_NET_INFO, mainnet};
use crate::dml_apply::checked::inputs_hash;
use crate::{Divergence, divergence};

/// The EV1 slice that lands a step the node lacks.
#[derive(Debug)]
pub struct Owner {
    /// `EV1-2` to `EV1-5` (EV1 §7), with the base-sdk task BLS validity waits on, if any.
    pub slice: &'static str,
    /// What lands with it: a file of `dash-dml/src/specialtx/`, or a function defined there
    /// (`fn …`).
    pub lands: &'static str,
}

/// One step of Core's check loop.
#[derive(Debug)]
pub struct Step {
    /// The step in EV1's table (`designs/EV1-specialtx.md`): `GVP n` for `GetValidatedPayload`,
    /// `Tn` for `IsTriviallyValid`, `Cn` for the type's `Check…Tx`.
    pub id: &'static str,
    /// The reasons it rejects with.
    pub reasons: &'static [&'static str],
    /// `None` when implemented here.
    pub owner: Option<Owner>,
}

/// The steps of one part of the loop, in Core's order.
#[derive(Debug)]
pub struct Table {
    pub name: &'static str,
    /// `RejectInLoop`'s label for a rejection here (the shim's debug message).
    pub label: &'static str,
    pub steps: &'static [Step],
}

const SPECIAL: &str = "Special Transaction check failed";
const SHARED_COLLATERAL: &str = "Shared collateral check failed";

const fn ours(id: &'static str, reasons: &'static [&'static str]) -> Step {
    Step {
        id,
        reasons,
        owner: None,
    }
}

const fn owned(
    id: &'static str,
    reasons: &'static [&'static str],
    slice: &'static str,
    lands: &'static str,
) -> Step {
    Step {
        id,
        reasons,
        owner: Some(Owner {
            slice,
            lands,
        }),
    }
}

/// `IsPayoutListTriviallyValid`'s reasons but `bad-protx-payee-reuse`, which a later step we lack
/// (`IsPayoutListKeySafe`, EV1-3) gives too. [`compare`] calls a passed step of ours that names
/// Core's reason a divergence, so while that later step is lacking the reuse check must be one we
/// lack as well ([`PAYEE_REUSE`]); make it ours only once C7 and C10 land.
const PAYOUT_LIST: &[&str] = &[
    BAD_PROTX_PAYOUTS_COUNT,
    BAD_PROTX_PAYOUT_REWARD,
    BAD_PROTX_PAYEE,
    BAD_PROTX_PAYEE_DUP,
    BAD_PROTX_PAYEE_DEST,
    BAD_PROTX_PAYOUT_REWARD_SUM,
];
const PAYEE_REUSE: &[&str] = &[BAD_PROTX_PAYEE_REUSE];
const LISTS: &[&str] = &[
    BAD_PROTX_SHARES_COUNT,
    BAD_PROTX_SHARES_EARLY_PERIOD,
    BAD_PROTX_SHARES_AMOUNT,
    BAD_PROTX_SHARES_KEY_NULL,
    BAD_PROTX_SHARES_DUP_KEY,
    BAD_PROTX_SHARES_DUP_REFUND,
    BAD_PROTX_SHARES_PAYEE,
    BAD_PROTX_SHARES_PAYEE_DEST,
    BAD_PROTX_SHARES_PAYEE_REUSE,
    BAD_PROTX_SHARES_AMOUNT_SUM,
    BAD_PROTX_SHARES_PENALTY,
    BAD_PROTX_PAYOUTS_COUNT,
    BAD_PROTX_PAYOUT_REWARD,
    BAD_PROTX_PAYEE,
    BAD_PROTX_PAYEE_DUP,
    BAD_PROTX_PAYEE_DEST,
    BAD_PROTX_PAYOUT_REWARD_SUM,
];
/// `CheckProviderNetworkFields` (§3.15), with `CheckNetInfo`'s statuses.
const NETWORK_FIELDS: &[&str] = &[
    BAD_PROTX_NETINFO_VERSION,
    BAD_PROTX_NETINFO_EMPTY,
    BAD_PROTX_NETINFO_BAD,
    BAD_PROTX_NETINFO_ADDR,
    BAD_PROTX_NETINFO_PORT,
    BAD_PROTX_NETINFO_ADDR_TYPE,
    BAD_PROTX_NETINFO_ADDR_UNROUTABLE,
    BAD_PROTX_NETINFO_ENTRY,
    BAD_PROTX_DUP_NETINFO_ENTRY,
    BAD_PROTX_NETINFO_MAXLIMIT,
    BAD_PROTX_PLATFORM_NODEID,
    BAD_PROTX_PLATFORM_P2P_PORT,
    BAD_PROTX_PLATFORM_HTTP_PORT,
    BAD_PROTX_PLATFORM_DUP_PORTS,
];

/// `CheckSharedCollateralTemplateOutputs`, every transaction under V24 (§3.17).
pub const COVENANT: Table = Table {
    name: "covenant (§3.17)",
    label: SHARED_COLLATERAL,
    steps: &[owned("template outputs", &[BAD_SHARED_COLLATERAL_CREATE], "EV1-3", "covenant.rs")],
};

/// `CheckSpecialTxInner`'s dispatch (§3.2).
pub const DISPATCH: Table = Table {
    name: "dispatcher (§3.2)",
    label: SPECIAL,
    steps: &[
        ours("2", &[BAD_TX_TYPE_DIP3_INACTIVE]),
        // Core's catch, inside the DIP3 test (v24:249-253): an exception anywhere in the type's
        // checks, so before them in order.
        owned("4", &[FAILED_CHECK_SPECIAL_TX], "EV1-3", "fn check_special_tx_inner"),
        ours("5", &[BAD_TX_TYPE_CHECK]),
    ],
};

/// `CheckProRegTx` (§3.2 `GetValidatedPayload`, §3.3).
pub const PRO_REG: Table = Table {
    name: "ProRegTx (§3.3)",
    label: SPECIAL,
    steps: &[
        ours("GVP 2", &[BAD_PROTX_PAYLOAD]),
        ours("GVP 3", &[BAD_PROTX_VERSION]),
        ours("T1", &[BAD_PROTX_VERSION]),
        ours("T2", &[BAD_PROTX_EVO_VERSION]),
        ours("T3", &[BAD_PROTX_TYPE]),
        ours("T4", &[BAD_PROTX_MODE]),
        // A shared registration's fields (T5a-c), else T5d and T5e: one step, as the walk takes
        // one branch.
        ours(
            "T5",
            &[
                BAD_PROTX_SHARES_EXTERNAL,
                BAD_PROTX_SHARES_OWNER_KEY,
                BAD_PROTX_SHARES_PAYOUTS,
                BAD_PROTX_SHARES_EMPTY_FIELDS,
                BAD_PROTX_KEY_NULL,
            ],
        ),
        ours("T6", &[BAD_PROTX_KEY_NULL]),
        ours("T7", &[BAD_PROTX_OPERATOR_PUBKEY]),
        // The share list, else the payout list.
        ours("T8", LISTS),
        owned("T8 reuse", PAYEE_REUSE, "EV1-3", "fn check_pro_reg_tx"),
        ours("T9", &[BAD_PROTX_NETINFO_VERSION]),
        ours("T10", &[BAD_PROTX_NETINFO_EMPTY, BAD_PROTX_NETINFO_BAD]),
        // rc.4 dropped T11, the entry loop: an invalid entry reaches C4's `CheckNetInfo` (CE4-03;
        // `providertx.cpp` rc.4:318-321).
        ours("T12", &[BAD_PROTX_OPERATOR_REWARD]),
        ours("C2", &[BAD_PROTX_VERSION_DISALLOWED]),
        // `rules.evo_shares`, which the context does not carry.
        owned("C3", &[BAD_PROTX_SHARES_EVO, BAD_PROTX_PAYOUTS_EVO], "EV1-3", "fn check_pro_reg_tx"),
        ours("C4", NETWORK_FIELDS),
        owned(
            "C6",
            &[
                BAD_PROTX_COLLATERAL,
                BAD_PROTX_COLLATERAL_DEST,
                BAD_PROTX_COLLATERAL_PKH,
                BAD_PROTX_COLLATERAL_INDEX,
                BAD_PROTX_SHARES_COLLATERAL_SCRIPT,
            ],
            "EV1-3",
            "fn check_pro_reg_tx",
        ),
        owned(
            "C7",
            &[BAD_PROTX_COLLATERAL_REUSE, BAD_PROTX_PAYEE_REUSE],
            "EV1-3",
            "fn check_pro_reg_tx",
        ),
        ours("C8", &[BAD_PROTX_DUP_NETINFO_ENTRY, BAD_PROTX_NETINFO_ENTRY]),
        owned(
            "C9-C11",
            &[BAD_PROTX_DUP_KEY, BAD_PROTX_DUP_PLATFORMNODEID],
            "EV1-3",
            "fn check_pro_reg_tx",
        ),
        owned("C12", &[BAD_PROTX_KEY_NOT_SAME], "EV1-3", "fn check_pro_reg_tx"),
        ours("C13", &[BAD_PROTX_INPUTS_HASH]),
        ours("C14", &[BAD_PROTX_SIG]),
    ],
};

/// `CheckProUpServTx` (§3.2 `GetValidatedPayload`, §3.4).
pub const PRO_UP_SERV: Table = Table {
    name: "ProUpServTx (§3.4)",
    label: SPECIAL,
    steps: &[
        ours("GVP 2", &[BAD_PROTX_PAYLOAD]),
        ours("GVP 3", &[BAD_PROTX_VERSION]),
        ours("T1", &[BAD_PROTX_VERSION]),
        ours("T2", &[BAD_PROTX_EVO_VERSION]),
        ours("T3", &[BAD_PROTX_TYPE]),
        ours("T4", &[BAD_PROTX_NETINFO_VERSION]),
        ours("T5", &[BAD_PROTX_NETINFO_EMPTY]),
        ours("T6", &[BAD_PROTX_NETINFO_EMPTY, BAD_PROTX_NETINFO_BAD]),
        // rc.4 dropped T7, the entry loop: an invalid entry reaches C2's `CheckNetInfo` (CE4-03;
        // `providertx.cpp` rc.4:424-428).
        ours("C2", NETWORK_FIELDS),
        ours("C3", &[BAD_PROTX_HASH]),
        ours("C4", &[BAD_PROTX_TYPE_MISMATCH]),
        ours("C5", &[BAD_PROTX_VERSION_DOWNGRADE]),
        ours("C6", &[BAD_PROTX_VERSION_DISALLOWED]),
        ours("C7", &[BAD_PROTX_NETINFO_VERSION]),
        owned("C8", &[BAD_PROTX_DUP_KEY], "EV1-4", "fn check_pro_up_serv_tx"),
        ours("C9", &[BAD_PROTX_DUP_NETINFO_ENTRY, BAD_PROTX_NETINFO_ENTRY]),
        owned("C10", &[BAD_PROTX_DUP_PLATFORMNODEID], "EV1-4", "fn check_pro_up_serv_tx"),
        owned("C11", &[BAD_PROTX_OPERATOR_PAYEE], "EV1-4", "fn check_pro_up_serv_tx"),
        ours("C12", &[BAD_PROTX_INPUTS_HASH]),
    ],
};

/// `CheckProUpRegTx` (§3.2 `GetValidatedPayload`, §3.5).
pub const PRO_UP_REG: Table = Table {
    name: "ProUpRegTx (§3.5)",
    label: SPECIAL,
    steps: &[
        ours("GVP 2", &[BAD_PROTX_PAYLOAD]),
        ours("GVP 3", &[BAD_PROTX_VERSION]),
        ours("T1", &[BAD_PROTX_VERSION]),
        ours("T2", &[BAD_PROTX_MODE]),
        ours("T3", &[BAD_PROTX_KEY_NULL]),
        ours("T4", &[BAD_PROTX_OPERATOR_PUBKEY]),
        ours("T5", PAYOUT_LIST),
        owned("T5 reuse", PAYEE_REUSE, "EV1-3", "fn check_pro_up_reg_tx"),
        ours("C2", &[BAD_PROTX_HASH]),
        ours("C3", &[BAD_PROTX_SHARED_MN]),
        ours("C4", &[BAD_PROTX_VERSION_DOWNGRADE]),
        owned("C5a", &[BAD_PROTX_DUP_KEY], "EV1-3", "fn check_pro_up_reg_tx"),
        ours("C5b", &[BAD_PROTX_NETINFO_VERSION]),
        ours("C6", PAYOUT_LIST),
        owned("C6 reuse", PAYEE_REUSE, "EV1-3", "fn check_pro_up_reg_tx"),
        // `rules.evo_shares`, which the context does not carry.
        owned("C7", &[BAD_PROTX_PAYOUTS_EVO], "EV1-3", "fn check_pro_up_reg_tx"),
        owned(
            "C8-C9",
            &[BAD_PROTX_COLLATERAL, BAD_PROTX_COLLATERAL_DEST],
            "EV1-3",
            "fn check_pro_up_reg_tx",
        ),
        owned(
            "C10",
            &[BAD_PROTX_COLLATERAL_REUSE, BAD_PROTX_PAYEE_REUSE],
            "EV1-3",
            "fn check_pro_up_reg_tx",
        ),
        owned("C11", &[BAD_PROTX_DUP_KEY], "EV1-3", "fn check_pro_up_reg_tx"),
        owned("C12", &[BAD_PROTX_KEY_NOT_SAME], "EV1-3", "fn check_pro_up_reg_tx"),
        ours("C13", &[BAD_PROTX_INPUTS_HASH]),
    ],
};

/// `CheckProUpRevTx` (§3.2 `GetValidatedPayload`, §3.6).
pub const PRO_UP_REV: Table = Table {
    name: "ProUpRevTx (§3.6)",
    label: SPECIAL,
    steps: &[
        ours("GVP 2", &[BAD_PROTX_PAYLOAD]),
        ours("GVP 3", &[BAD_PROTX_VERSION]),
        ours("T1", &[BAD_PROTX_VERSION]),
        ours("T2", &[BAD_PROTX_REASON]),
        ours("C2", &[BAD_PROTX_HASH]),
        ours("C3", &[BAD_PROTX_VERSION_DOWNGRADE]),
        ours("C4", &[BAD_PROTX_INPUTS_HASH]),
    ],
};

/// A type the node checks nothing of yet: one row with every reason its checks have.
const fn whole_type(name: &'static str, steps: &'static [Step]) -> Table {
    Table {
        name,
        label: SPECIAL,
        steps,
    }
}

pub const PRO_DIS: Table = whole_type(
    "ProDisTx (§3.7)",
    &[owned(
        "§3.7",
        &[
            BAD_PROTX_PAYLOAD,
            BAD_PROTX_VERSION,
            BAD_PRODIS_VERSION,
            BAD_PRODIS_SIG_COUNT,
            BAD_PRODIS_TOO_EARLY,
            BAD_PRODIS_HASH,
            BAD_PRODIS_NOT_SHARED,
            BAD_PRODIS_INPUT,
            BAD_PRODIS_ACTOR,
            BAD_PRODIS_PAYEE_COUNT,
            BAD_PRODIS_PAYEE,
            BAD_PRODIS_PENALTY_FLOOR,
            BAD_PRODIS_ACTOR_OUTPUT_ZERO,
            BAD_PRODIS_PENALTY_SUM,
            BAD_PRODIS_FEE,
            BAD_PRODIS_BONUS,
            BAD_PRODIS_SIG,
        ],
        "EV1-3",
        "shared.rs",
    )],
);

pub const PRO_UP_SHARE: Table = whole_type(
    "ProUpShareTx (§3.8)",
    &[owned(
        "§3.8",
        &[
            BAD_PROTX_PAYLOAD,
            BAD_PROTX_VERSION,
            BAD_PROUPSHARE_VERSION,
            BAD_PROUPSHARE_SIG_SIZE,
            BAD_PROUPSHARE_PAYEE,
            BAD_PROUPSHARE_TOO_EARLY,
            BAD_PROUPSHARE_HASH,
            BAD_PROUPSHARE_NOT_SHARED,
            BAD_PROUPSHARE_INDEX,
            BAD_PROUPSHARE_PAYEE_DEST,
            BAD_PROUPSHARE_PAYEE_REUSE,
            BAD_PROTX_INPUTS_HASH,
            BAD_PROUPSHARE_SIG,
        ],
        "EV1-3",
        "shared.rs",
    )],
);

pub const PRO_UP_SHARED_REG: Table = whole_type(
    "ProUpSharedRegTx (§3.9)",
    &[owned(
        "§3.9",
        &[
            BAD_PROTX_PAYLOAD,
            BAD_PROTX_VERSION,
            BAD_PROUPSHAREDREG_VERSION,
            BAD_PROTX_KEY_NULL,
            BAD_PROTX_OPERATOR_PUBKEY,
            BAD_PROUPSHAREDREG_SIG_COUNT,
            BAD_PROUPSHAREDREG_TOO_EARLY,
            BAD_PROUPSHAREDREG_HASH,
            BAD_PROUPSHAREDREG_NOT_SHARED,
            BAD_PROTX_DUP_KEY,
            BAD_PROUPSHAREDREG_PAYEE_REUSE,
            BAD_PROTX_INPUTS_HASH,
            BAD_PROUPSHAREDREG_SIG,
        ],
        "EV1-3",
        "shared.rs",
    )],
);

/// A coinbase past the first transaction (§3.2 3.9–3.10): its coinbase test (3.8), then the rest
/// (§3.10).
pub const CBTX: Table = whole_type(
    "CbTx (§3.2 3.8-3.10, §3.10)",
    &[
        ours("3.8", &[BAD_CBTX_INVALID]),
        owned("§3.10", &[BAD_CBTX_PAYLOAD, BAD_CBTX_VERSION, BAD_CBTX_HEIGHT], "EV1-5", "cbtx.rs"),
    ],
);

/// LQ2 landed `CheckLLMQCommitment` (`specialtx/qc.rs`) before the check loop dispatches to it,
/// and this replay has no LLMQ chain, block index or members to run it with; its exact
/// comparison is LQ2's QcTx differential (`l1_lq2`), so the row waits for the dispatcher
/// (LQ2 D21).
pub const QC: Table = whole_type(
    "QcTx (§3.11)",
    &[owned(
        "§3.11",
        &[
            BAD_QC_PAYLOAD,
            BAD_QC_COMMITMENT_TYPE,
            BAD_QC_VERSION,
            BAD_QC_HEIGHT,
            BAD_QC_QUORUM_HASH,
            BAD_QC_INVALID_NULL,
            BAD_QC_INVALID,
        ],
        "EV1-5",
        "fn check_special_tx_inner",
    )],
);

pub const MNHF: Table = whole_type(
    "MnhfTx (§3.12)",
    &[owned(
        "§3.12 MNHF",
        &[
            BAD_MNHF_TYPE,
            BAD_MNHF_PAYLOAD,
            BAD_MNHF_VERSION,
            BAD_MNHF_NBIT_OUT_OF_BOUNDS,
            BAD_MNHF_NON_EHF,
            BAD_MNHF_QUORUM_HASH,
            BAD_MNHF_MISSING_QUORUM,
            BAD_MNHF_INVALID,
        ],
        "EV1-5",
        "mnhf.rs",
    )],
);

pub const ASSET_LOCK: Table = whole_type(
    "AssetLockTx (§3.12)",
    &[owned(
        "§3.12 asset lock",
        &[
            BAD_ASSETLOCKTX_TYPE,
            BAD_ASSETLOCKTX_NON_EMPTY_RETURN,
            BAD_ASSETLOCKTX_OPRETURN_OUTOFRANGE,
            BAD_ASSETLOCKTX_MULTIPLE_RETURN,
            BAD_ASSETLOCKTX_NO_RETURN,
            BAD_ASSETLOCKTX_PAYLOAD,
            BAD_ASSETLOCKTX_VERSION,
            BAD_ASSETLOCKTX_VERSION_2,
            BAD_ASSETLOCKTX_EMPTYCREDITOUTPUTS,
            BAD_ASSETLOCKTX_CREDIT_OUTOFRANGE,
            BAD_ASSETLOCKTX_SCRIPT_PUBKEY,
            BAD_ASSETLOCKTX_PUBKEYHASH,
            BAD_ASSETLOCKTX_CREDITAMOUNT,
        ],
        "EV1-5",
        "asset.rs",
    )],
);

pub const ASSET_UNLOCK: Table = whole_type(
    "AssetUnlockTx (§3.12)",
    &[owned(
        "§3.12 asset unlock",
        &[
            BAD_ASSETUNLOCKTX_TYPE,
            BAD_ASSETUNLOCKTX_HAVE_INPUT,
            BAD_ASSETUNLOCKTX_TOO_MANY_OUTS,
            BAD_ASSETUNLOCKTX_PAYLOAD,
            BAD_ASSETUNLOCKTX_VERSION,
            BAD_ASSETUNLOCKTX_VERSION_2,
            BAD_ASSETUNLOCK_DUPLICATED_INDEX,
            BAD_ASSETUNLOCK_QUORUM_HASH,
            BAD_ASSETUNLOCK_TOO_OLD_QUORUM,
            BAD_ASSETUNLOCK_TOO_LATE,
            BAD_ASSETUNLOCK_QUORUM_INTERNAL_ERROR,
            BAD_ASSETUNLOCK_NOT_VERIFIED,
        ],
        "EV1-5",
        "asset.rs",
    )],
);

/// Every table, for the tests and the coverage report.
pub const TABLES: &[&Table] = &[
    &COVENANT,
    &DISPATCH,
    &PRO_REG,
    &PRO_UP_SERV,
    &PRO_UP_REG,
    &PRO_UP_REV,
    &PRO_DIS,
    &PRO_UP_SHARE,
    &PRO_UP_SHARED_REG,
    &CBTX,
    &QC,
    &MNHF,
    &ASSET_LOCK,
    &ASSET_UNLOCK,
];

/// The node's missing checks: every owned step.
pub fn missing_checks() -> impl Iterator<Item = (&'static Table, &'static Step)> {
    TABLES.iter().flat_map(|t| t.steps.iter().filter(|s| s.owner.is_some()).map(move |s| (*t, s)))
}

/// Where a step stopped a transaction.
#[derive(Clone, Copy, Debug)]
pub struct At {
    pub table: &'static Table,
    pub step: &'static Step,
}

/// Our check loop's verdict on one transaction.
#[derive(Debug)]
pub struct TxCheck {
    /// The step of ours that rejects it and its reason; `None` when every step we have passes.
    pub reject: Option<(At, &'static str)>,
    /// The steps before our rejection, in Core's order: ours that passed, and those we lack
    /// ([`Step::owner`]).
    pub reached: Vec<At>,
}

/// What a transaction's check stops at.
enum Stop {
    Reject(At, &'static str),
    /// Our walk broke a table's order or ownership: a bug of this module.
    Bug(&'static str),
}

/// A walk through the tables of one transaction, in their order.
struct Walk {
    table: &'static Table,
    next: usize,
    reached: Vec<At>,
}

impl Walk {
    fn enter(&mut self, table: &'static Table) {
        self.table = table;
        self.next = 0;
    }

    /// The step `id` of the table, which must follow the last one walked; the steps between are
    /// ones we lack (a transaction does not reach them all), never ones we have.
    fn row(&mut self, id: &str) -> Result<At, Stop> {
        let (at, step) = (self.table.steps.iter().enumerate().skip(self.next))
            .find(|(_, s)| s.id == id)
            .ok_or(Stop::Bug("check step order"))?;
        let skipped = self.table.steps.get(self.next..at).unwrap_or_default();
        if skipped.iter().any(|s| s.owner.is_none()) {
            return Err(Stop::Bug("implemented check skipped"));
        }
        self.next = at + 1;
        Ok(At {
            table: self.table,
            step,
        })
    }

    /// A step implemented here: its rejection, if any.
    fn verdict(&mut self, id: &str, reject: Option<&str>) -> Result<(), Stop> {
        let at = self.row(id)?;
        if at.step.owner.is_some() {
            return Err(Stop::Bug("listed check implemented"));
        }
        let Some(reason) = reject else {
            self.reached.push(at);
            return Ok(());
        };
        let reason =
            at.step.reasons.iter().find(|r| **r == reason).ok_or(Stop::Bug("check step reason"))?;
        Err(Stop::Reject(at, reason))
    }

    fn check(&mut self, id: &str, fails: bool, reason: &'static str) -> Result<(), Stop> {
        self.verdict(id, fails.then_some(reason))
    }

    /// A step dash-dml decides.
    fn decided(&mut self, id: &str, result: Result<(), TxValidationError>) -> Result<(), Stop> {
        let error = result.err();
        self.verdict(id, error.as_ref().map(|e| e.reject_reason()))
    }

    /// The end of a type's walk that passed: the steps left are ones we lack, never ones we
    /// have (Opus r2: [`Walk::row`] sees only the steps before one walked).
    fn finish(&self) -> Result<(), Stop> {
        let left = self.table.steps.get(self.next..).unwrap_or_default();
        if left.iter().any(|s| s.owner.is_none()) {
            return Err(Stop::Bug("implemented check skipped"));
        }
        Ok(())
    }

    /// A step the node lacks, which the transaction reaches.
    fn missing(&mut self, id: &str) -> Result<(), Stop> {
        let at = self.row(id)?;
        if at.step.owner.is_none() {
            return Err(Stop::Bug("implemented check missing"));
        }
        self.reached.push(at);
        Ok(())
    }
}

/// What Core's check loop reads beside the block (`CheckSpecialTxInner`'s arguments), with
/// `check_sigs` off, as `diff_dml_block` calls the oracle (`dml_block_effects`' `check_sigs`):
/// the signatures it would check are not compared.
pub struct CheckCtx<'a> {
    /// `pindex->nHeight`.
    pub height: i32,
    /// DIP3, V19 active at the block (`DeploymentActiveAfter(pindexPrev, …)`).
    pub dip3: bool,
    pub v19: bool,
    /// `rules.v24`.
    pub v24: bool,
    /// The tip's list (`GetListForBlock(pindexPrev)`).
    pub list: &'a MnList,
    pub net_info: NetInfoRules,
}

impl<'a> CheckCtx<'a> {
    pub fn new(params: &ChainParams, height: i32, v24: bool, list: &'a MnList) -> CheckCtx<'a> {
        let v19 = i64::from(height) >= i64::from(params.consensus.v19_height.to_u32());
        CheckCtx {
            height,
            dip3: height >= DmlParams::new(params).dip3_height,
            v19,
            v24,
            list,
            net_info: NetInfoRules::new(params, mainnet(), CORE_NET_INFO),
        }
    }
}

/// Our check loop over `block`, up to and including the first transaction we reject.
pub fn check_block(cx: &CheckCtx, block: &Block) -> Result<Vec<TxCheck>, Divergence> {
    let mut checks = Vec::new();
    for (i, tx) in block.txs().iter().enumerate() {
        let mut walk = Walk {
            table: &COVENANT,
            next: 0,
            reached: Vec::new(),
        };
        let reject = match check_tx(cx, i, tx, &mut walk) {
            Ok(()) => None,
            Err(Stop::Reject(at, reason)) => Some((at, reason)),
            Err(Stop::Bug(what)) => return Err(divergence(what)),
        };
        let stop = reject.is_some();
        checks.push(TxCheck {
            reject,
            reached: walk.reached,
        });
        if stop {
            break;
        }
    }
    Ok(checks)
}

/// One transaction of the loop (`specialtxman.cpp` v24:857-879).
fn check_tx(cx: &CheckCtx, i: usize, tx: &Tx, w: &mut Walk) -> Result<(), Stop> {
    if cx.v24 {
        w.missing("template outputs")?;
    }
    if i == 0 && tx.tx_type() == TxType::CoinbaseCommitment {
        return Ok(());
    }
    // `CheckSpecialTxInner` (v24:217-274).
    w.enter(&DISPATCH);
    if !tx.has_extra_payload_field() {
        return Ok(());
    }
    w.check("2", !cx.dip3, BAD_TX_TYPE_DIP3_INACTIVE)?;
    w.missing("4")?;
    match tx.tx_type() {
        TxType::ProviderRegister => pro_reg(cx, tx, w)?,
        TxType::ProviderUpdateService => pro_up_serv(cx, tx, w)?,
        TxType::ProviderUpdateRegistrar => pro_up_reg(cx, tx, w)?,
        TxType::ProviderUpdateRevoke => pro_up_rev(cx, tx, w)?,
        TxType::CoinbaseCommitment => {
            w.enter(&CBTX);
            w.check("3.8", !tx.is_coinbase(), BAD_CBTX_INVALID)?;
            w.missing("§3.10")?;
        }
        tx_type => {
            let rest = match tx_type {
                TxType::QuorumCommitment => &QC,
                TxType::MnhfSignal => &MNHF,
                TxType::AssetLock => &ASSET_LOCK,
                TxType::AssetUnlock => &ASSET_UNLOCK,
                TxType::ProviderDissolve => &PRO_DIS,
                TxType::ProviderUpdateShare => &PRO_UP_SHARE,
                TxType::ProviderUpdateSharedRegistrar => &PRO_UP_SHARED_REG,
                _ => return w.check("5", true, BAD_TX_TYPE_CHECK),
            };
            w.enter(rest);
            w.missing(rest.steps.first().map_or("", |s| s.id))?;
        }
    }
    w.finish()
}

/// `GetValidatedPayload<T>` up to `IsTriviallyValid`'s first step (§3.2): the payload, which
/// `pick` takes from the decoded one, read to its end and of a version the block allows.
fn validated<'t, T>(
    cx: &CheckCtx,
    tx: &'t Tx,
    w: &mut Walk,
    pick: impl Fn(&'t SpecialPayload) -> Option<&'t T>,
    version: impl Fn(&T) -> u16,
) -> Result<&'t T, Stop> {
    let typed = payload(tx).and_then(pick);
    let unknown = unknown_pro_tx_version(tx);
    w.check("GVP 2", typed.is_none() && unknown.is_none(), BAD_PROTX_PAYLOAD)?;
    let max = version_max(cx.v19, cx.v24);
    if let Some(v) = unknown {
        w.check("GVP 3", v > max, BAD_PROTX_VERSION)?;
        // Version 0 reaches `IsTriviallyValid`.
        w.check("T1", true, BAD_PROTX_VERSION)?;
        return Err(Stop::Bug("T1"));
    }
    let p = typed.ok_or(Stop::Bug("check payload"))?;
    let v = version(p);
    w.check("GVP 3", v > max, BAD_PROTX_VERSION)?;
    w.check("T1", v == 0 || v > 3, BAD_PROTX_VERSION)?;
    Ok(p)
}

/// The type a payload names, which must be a defined one (`IsValidMnType`; `IsTriviallyValid`'s
/// step `id`).
fn known(w: &mut Walk, id: &'static str, t: PayloadMnType) -> Result<MnType, Stop> {
    let known = MnType::from_raw(RawMnType(t.to_base()));
    w.check(id, known.is_none(), BAD_PROTX_TYPE)?;
    known.ok_or(Stop::Bug(id))
}

/// `IsTriviallyValid`'s operator and voting keys (steps `ids`): both set, and the operator key read
/// under the version's scheme (`providertx.cpp` rc.4:300-305, 459-464).
fn operator_keys(
    w: &mut Walk,
    operator: &BlsLazyPk,
    voting: &KeyId,
    version: u16,
    ids: [&'static str; 2],
) -> Result<(), Stop> {
    w.check(ids[0], !operator.get().is_valid() || voting.is_null(), BAD_PROTX_KEY_NULL)?;
    let legacy = operator.scheme() == BlsSchemeKind::Legacy;
    w.check(ids[1], legacy != (version == 1), BAD_PROTX_OPERATOR_PUBKEY)
}

/// `IsPayoutListTriviallyValid`'s `result` as steps `ids`: the list's checks, then the reuse
/// check we count as lacking ([`PAYOUT_LIST`]), reached whenever the list's checks pass; on a
/// reuse rejection our walk goes on, as it does past any step we lack.
fn payout_list(
    w: &mut Walk,
    ids: [&'static str; 2],
    result: Result<(), TxValidationError>,
) -> Result<(), Stop> {
    let result = match result {
        Err(e) if e.reject_reason() == BAD_PROTX_PAYEE_REUSE => Ok(()),
        other => other,
    };
    w.decided(ids[0], result)?;
    w.missing(ids[1])
}

/// The tip's masternode `hash` names, which must be listed (step `id`).
fn listed(cx: &CheckCtx, w: &mut Walk, id: &'static str, hash: &TxHash) -> Result<Arc<Dmn>, Stop> {
    let dmn = cx.list.get(&ProTxHash::from(*hash.as_bytes())).cloned();
    w.check(id, dmn.is_none(), BAD_PROTX_HASH)?;
    dmn.ok_or(Stop::Bug(id))
}

/// Whether the payload's NetInfo is not the version's (extended from version 3).
fn net_info_version_mismatch(net_info: &NetInfo, version: u16) -> bool {
    matches!(net_info, NetInfo::Extended(_)) != (version >= 3)
}

/// The tip's list against the payload's entries (`specialtxman.cpp` v24:1256-1270,
/// 1402-1416): an entry another masternode (`other`) holds, or one that is neither an address
/// nor a domain.
fn entries_held(
    cx: &CheckCtx,
    net_info: &NetInfo,
    other: impl Fn(&Dmn) -> bool,
) -> Option<&'static str> {
    payload_entries(net_info).iter().find_map(|entry| match entry {
        Some(key) => cx
            .list
            .unique_owner(key)
            .is_some_and(|owner| other(owner))
            .then_some(BAD_PROTX_DUP_NETINFO_ENTRY),
        None => Some(BAD_PROTX_NETINFO_ENTRY),
    })
}

/// `IsVersionChangeValid` (§3.2): no step down to the legacy scheme under V24.
fn is_downgrade(v24: bool, state_version: i32, version: u16) -> bool {
    v24 && state_version >= 2 && version == 1
}

/// The state's version as the build reads it (`MnCold::state_version`).
fn state_version(d: &Dmn) -> u16 {
    d.cold.version as u16
}

/// `CheckInputsHash`: the payload's against the transaction's prevouts.
fn inputs_hash_differs(tx: &Tx, payload: &dash_primitives::InputsHash) -> bool {
    inputs_hash(tx.inputs()) != *payload
}

/// `CheckProRegTx` with `check_sigs` off (§3.3).
fn pro_reg(cx: &CheckCtx, tx: &Tx, w: &mut Walk) -> Result<(), Stop> {
    w.enter(&PRO_REG);
    let p: &ProRegTx = validated(
        cx,
        tx,
        w,
        |d| match d {
            SpecialPayload::ProviderRegister(p) => Some(p),
            _ => None,
        },
        |p| p.version,
    )?;
    w.check("T2", p.version < 2 && p.mn_type == PayloadMnType::Evo, BAD_PROTX_EVO_VERSION)?;
    let mn_type = known(w, "T3", p.mn_type)?;
    w.check("T4", p.mode != 0, BAD_PROTX_MODE)?;
    let shared = !p.shares.is_empty();
    if shared {
        let reject = if !p.collateral_hash.is_null() {
            Some(BAD_PROTX_SHARES_EXTERNAL)
        } else if !p.key_id_owner.is_null() {
            Some(BAD_PROTX_SHARES_OWNER_KEY)
        } else {
            (!p.payouts.is_empty()).then_some(BAD_PROTX_SHARES_PAYOUTS)
        };
        w.verdict("T5", reject)?;
    } else {
        let reject = if p.early_period_blocks != 0 || p.early_penalty != 0 {
            Some(BAD_PROTX_SHARES_EMPTY_FIELDS)
        } else {
            p.key_id_owner.is_null().then_some(BAD_PROTX_KEY_NULL)
        };
        w.verdict("T5", reject)?;
    }
    operator_keys(w, &p.pub_key_operator, &p.key_id_voting, p.version, ["T6", "T7"])?;
    if shared {
        let required = collateral_amount(mn_type);
        w.decided(
            "T8",
            is_share_list_trivially_valid(
                &p.shares,
                p.early_period_blocks,
                p.early_penalty,
                required,
                &p.key_id_voting,
            ),
        )?;
    } else {
        let payouts = OwnerPayouts::new(p.version, &p.payouts, p.script_payout.as_bytes());
        let result = is_payout_list_trivially_valid(payouts, &p.key_id_owner, &p.key_id_voting);
        payout_list(w, ["T8", "T8 reuse"], result)?;
    }
    w.check("T9", net_info_version_mismatch(&p.net_info, p.version), BAD_PROTX_NETINFO_VERSION)?;
    if netinfo::is_empty(&p.net_info) {
        w.check("T10", false, BAD_PROTX_NETINFO_EMPTY)?;
    } else {
        w.decided("T10", is_net_info_trivially_valid(&p.net_info, mn_type))?;
    }
    w.check("T12", p.operator_reward > 10_000, BAD_PROTX_OPERATOR_REWARD)?;
    w.check("C2", cx.v24 && p.version < 2, BAD_PROTX_VERSION_DISALLOWED)?;
    w.missing("C3")?;
    // Core's consensus callers pass the payload's id, the null one when it has none.
    let null = PlatformNodeId::from([0; 20]);
    let fields = ProviderNetworkFields {
        net_info: &p.net_info,
        mn_type,
        version: p.version,
        platform_node_id: Some(p.platform_node_id.as_ref().unwrap_or(&null)),
        platform_p2p_port: p.platform_p2p_port.unwrap_or(0),
        platform_http_port: p.platform_http_port.unwrap_or(0),
    };
    w.decided("C4", check_provider_network_fields(&fields, true, &cx.net_info))?;
    w.missing("C6")?;
    if !shared {
        w.missing("C7")?;
    }
    // Only the masternode on the same collateral (which the registration replaces) may hold an
    // entry: the internal collateral is the transaction's own output.
    let collateral = OutPoint {
        hash: if p.collateral_hash.is_null() {
            tx.txid()
        } else {
            p.collateral_hash
        },
        index: p.collateral_index,
    };
    w.verdict("C8", entries_held(cx, &p.net_info, |owner| owner.collateral != collateral))?;
    w.missing("C9-C11")?;
    w.missing("C12")?;
    w.check("C13", inputs_hash_differs(tx, &p.inputs_hash), BAD_PROTX_INPUTS_HASH)?;
    // The internal collateral's signature must be empty; the others are checked only with
    // `check_sigs`.
    let internal = p.collateral_hash.as_bytes().iter().all(|b| *b == 0);
    w.check("C14", internal && !p.vch_sig.is_empty(), BAD_PROTX_SIG)
}

/// `CheckProUpServTx` with `check_sigs` off (§3.4).
fn pro_up_serv(cx: &CheckCtx, tx: &Tx, w: &mut Walk) -> Result<(), Stop> {
    w.enter(&PRO_UP_SERV);
    let p: &ProUpServTx = validated(
        cx,
        tx,
        w,
        |d| match d {
            SpecialPayload::ProviderUpdateService(p) => Some(p),
            _ => None,
        },
        |p| p.version,
    )?;
    w.check("T2", p.version < 2 && p.mn_type == PayloadMnType::Evo, BAD_PROTX_EVO_VERSION)?;
    let mn_type = known(w, "T3", p.mn_type)?;
    w.check("T4", net_info_version_mismatch(&p.net_info, p.version), BAD_PROTX_NETINFO_VERSION)?;
    w.check("T5", netinfo::is_empty(&p.net_info), BAD_PROTX_NETINFO_EMPTY)?;
    w.decided("T6", is_net_info_trivially_valid(&p.net_info, mn_type))?;
    let null = PlatformNodeId::from([0; 20]);
    let fields = ProviderNetworkFields {
        net_info: &p.net_info,
        mn_type,
        version: p.version,
        platform_node_id: Some(p.platform_node_id.as_ref().unwrap_or(&null)),
        platform_p2p_port: p.platform_p2p_port.unwrap_or(0),
        platform_http_port: p.platform_http_port.unwrap_or(0),
    };
    w.decided("C2", check_provider_network_fields(&fields, false, &cx.net_info))?;
    let dmn = listed(cx, w, "C3", &p.pro_tx_hash)?;
    w.check("C4", RawMnType(p.mn_type.to_base()) != dmn.mn_type, BAD_PROTX_TYPE_MISMATCH)?;
    w.check("C5", is_downgrade(cx.v24, dmn.cold.version, p.version), BAD_PROTX_VERSION_DOWNGRADE)?;
    let evo = mn_type == MnType::Evo;
    w.check("C6", cx.v24 && evo && p.version < 3, BAD_PROTX_VERSION_DISALLOWED)?;
    // `ApplyProUpServTx` on a copy of the state: its NetInfo step (§3.16.1).
    let migration = cx.v24.then(|| {
        let target = state_version(&dmn).max(p.version);
        migration_reject(&p.net_info, p.version, target, mn_type, &cx.net_info)
    });
    w.verdict("C7", migration.flatten())?;
    w.missing("C8")?;
    let pro_tx = ProTxHash::from(*p.pro_tx_hash.as_bytes());
    w.verdict("C9", entries_held(cx, &p.net_info, |owner| owner.pro_tx_hash != pro_tx))?;
    w.missing("C10")?;
    w.missing("C11")?;
    w.check("C12", inputs_hash_differs(tx, &p.inputs_hash), BAD_PROTX_INPUTS_HASH)
}

/// `CheckProUpRegTx` with `check_sigs` off (§3.5).
fn pro_up_reg(cx: &CheckCtx, tx: &Tx, w: &mut Walk) -> Result<(), Stop> {
    w.enter(&PRO_UP_REG);
    let p: &ProUpRegTx = validated(
        cx,
        tx,
        w,
        |d| match d {
            SpecialPayload::ProviderUpdateRegistrar(p) => Some(p),
            _ => None,
        },
        |p| p.version,
    )?;
    w.check("T2", p.mode != 0, BAD_PROTX_MODE)?;
    operator_keys(w, &p.pub_key_operator, &p.key_id_voting, p.version, ["T3", "T4"])?;
    // `IsTriviallyValid` has no owner key to compare: `CheckProUpRegTx` repeats the list against
    // the masternode's (C6).
    let payouts = OwnerPayouts::new(p.version, &p.payouts, p.script_payout.as_bytes());
    let result = is_payout_list_trivially_valid(payouts, &KeyId::default(), &p.key_id_voting);
    payout_list(w, ["T5", "T5 reuse"], result)?;
    let dmn = listed(cx, w, "C2", &p.pro_tx_hash)?;
    w.check("C3", dmn.cold.is_shared(), BAD_PROTX_SHARED_MN)?;
    w.check("C4", is_downgrade(cx.v24, dmn.cold.version, p.version), BAD_PROTX_VERSION_DOWNGRADE)?;
    w.missing("C5a")?;
    // `CBLSLazyWrapper::operator==` (D17), as the build compares them.
    let key_changed = dmn.cold.pub_key_operator != p.pub_key_operator;
    // `SetStateVersion` on a copy of the state: its NetInfo step (§3.16.1). An undefined stored
    // type is not Evo, the one type the step reads.
    let migration = (cx.v24 && !key_changed).then(|| {
        let current = state_version(&dmn);
        let mn_type = dmn.mn_type.stored_type().unwrap_or(MnType::Regular);
        migration_reject(&dmn.cold.net_info, current, current.max(p.version), mn_type, &cx.net_info)
    });
    w.verdict("C5b", migration.flatten())?;
    let result = is_payout_list_trivially_valid(payouts, &dmn.cold.key_id_owner, &p.key_id_voting);
    payout_list(w, ["C6", "C6 reuse"], result)?;
    w.missing("C7")?;
    w.missing("C8-C9")?;
    w.missing("C10")?;
    w.missing("C11")?;
    w.missing("C12")?;
    w.check("C13", inputs_hash_differs(tx, &p.inputs_hash), BAD_PROTX_INPUTS_HASH)
}

/// `CheckProUpRevTx` with `check_sigs` off (§3.6).
fn pro_up_rev(cx: &CheckCtx, tx: &Tx, w: &mut Walk) -> Result<(), Stop> {
    w.enter(&PRO_UP_REV);
    let p: &ProUpRevTx = validated(
        cx,
        tx,
        w,
        |d| match d {
            SpecialPayload::ProviderUpdateRevoke(p) => Some(p),
            _ => None,
        },
        |p| p.version,
    )?;
    // `nReason > REASON_LAST`.
    w.check("T2", matches!(p.reason, RevocationReason::Unknown(_)), BAD_PROTX_REASON)?;
    let dmn = listed(cx, w, "C2", &p.pro_tx_hash)?;
    w.check("C3", is_downgrade(cx.v24, dmn.cold.version, p.version), BAD_PROTX_VERSION_DOWNGRADE)?;
    w.check("C4", inputs_hash_differs(tx, &p.inputs_hash), BAD_PROTX_INPUTS_HASH)
}

/// The check layer's comparison with Core's loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Both loops accept every transaction.
    Accepted,
    /// Both reject the same transaction (index `tx`, of `tx_type`) at the same step's reason;
    /// `at` is our step, of `table`.
    Exact {
        tx: usize,
        tx_type: TxType,
        at: &'static str,
        table: &'static str,
    },
    /// Core rejects at a step the node lacks (`at`), reached before any rejection of ours.
    OutOfDomain {
        tx: usize,
        tx_type: TxType,
        at: &'static str,
        table: &'static str,
    },
    /// Core's reason names a step we lack before our rejection (`at`) and one at or after it
    /// (`ours`, our rejecting step): Core's message does not say which rejected, so our step is
    /// not compared. `at`'s slice resolves it: once that step is ours, Core's rejection is
    /// placed.
    Ambiguous {
        tx: usize,
        tx_type: TxType,
        at: &'static str,
        ours: &'static str,
        table: &'static str,
    },
}

/// Core's loop's rejection: the label and the transaction its debug message names
/// (`RejectInLoop`: "%s (tx hash %s) %s").
fn core_rejected(core: &Rejection) -> Option<(&str, &str)> {
    let (label, rest) = core.debug_message.split_once(" (tx hash ")?;
    Some((label, rest.get(..64)?))
}

/// Our loop's verdicts `ours` (from [`check_block`]) against Core's: `core` is its loop's
/// rejection, `None` when it accepted (D56).
pub fn compare(
    ours: &[TxCheck],
    block: &Block,
    core: Option<&Rejection>,
) -> Result<CheckOutcome, Divergence> {
    let ours_first = ours.iter().position(|t| t.reject.is_some());
    let Some(core) = core else {
        return match ours_first {
            Some(_) => Err(divergence("check layer: rejects a block Core's checks accept")),
            None => Ok(CheckOutcome::Accepted),
        };
    };
    if result_code(&core.result) != Some(BlockValidationResult::Consensus as i32) {
        return Err(divergence("check layer: result class"));
    }
    let (label, hash) = core_rejected(core).ok_or(divergence("check layer: Core's message"))?;
    let (k, tx) = (block.txs().iter().enumerate())
        .find(|(_, tx)| tx.txid().to_string() == hash)
        .ok_or(divergence("check layer: Core's transaction"))?;
    if ours_first.is_some_and(|j| j < k) {
        return Err(divergence("check layer: rejects an earlier transaction"));
    }
    let ours = ours.get(k).ok_or(divergence("check layer: Core's transaction"))?;
    let tx_type = tx.tx_type();
    if let Some((at, reason)) = ours.reject
        && reason == core.reason
        && at.table.label == label
    {
        return Ok(CheckOutcome::Exact {
            tx: k,
            tx_type,
            at: at.step.id,
            table: at.table.name,
        });
    }
    let reason = core.reason.as_str();
    let names = |at: &At| at.table.label == label && at.step.reasons.contains(&reason);
    // A step of ours that passed, with Core's reason: Core may have rejected there.
    if ours.reached.iter().any(|at| at.step.owner.is_none() && names(at)) {
        return Err(divergence("check layer: passes a step Core's reason names"));
    }
    // A step we lack, before ours, with Core's reason.
    let Some(lacking) = ours.reached.iter().find(|at| at.step.owner.is_some() && names(at)) else {
        return Err(divergence(match ours.reject {
            Some(_) => "check layer: reason",
            None => "check layer: accepts a transaction Core's checks reject",
        }));
    };
    let out_of_domain = CheckOutcome::OutOfDomain {
        tx: k,
        tx_type,
        at: lacking.step.id,
        table: lacking.table.name,
    };
    let Some((ours_at, _)) = ours.reject else {
        return Ok(out_of_domain);
    };
    // Core may instead have passed our rejecting step and rejected at it or after it: an
    // exception (`4`) anywhere in the type's checks, or a step of the table from ours on.
    let mut from_ours = ours_at.table.steps.iter().skip_while(|s| s.id != ours_at.step.id);
    let later = lacking.step.reasons.contains(&FAILED_CHECK_SPECIAL_TX)
        || (ours_at.table.label == label && from_ours.any(|s| s.reasons.contains(&reason)));
    Ok(if later {
        CheckOutcome::Ambiguous {
            tx: k,
            tx_type,
            at: lacking.step.id,
            ours: ours_at.step.id,
            table: ours_at.table.name,
        }
    } else {
        out_of_domain
    })
}
