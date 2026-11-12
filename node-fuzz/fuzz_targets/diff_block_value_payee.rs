//! `diff_block_value_payee` (EV3-1): dash-evo's coinbase value and payee checks and dash-dml's
//! masternode payment (`GetBlockTxOuts`, `IsTransactionValid`, `CSuperblock::IsValid`) against
//! Core's `IsBlockValueValid` and `IsBlockPayeeValid`. Run only through `node-fuzz/run.sh`.
#![no_main]

node_fuzz::l1_target!("diff_block_value_payee");
