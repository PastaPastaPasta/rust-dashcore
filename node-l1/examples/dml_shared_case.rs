//! Writes `node-l1/src/dml_apply/shared-case.json` (EV2b D23): a block of one shared ProReg on
//! a pre-V24 crafted case with V24 forced on (EV2b D22), which Core accepts, and the list Core
//! builds from it (`oracle_dml_apply`, the evodb encoding). `node_l1::dml_apply` derives from it
//! a case whose list already holds a shared masternode, so `diff_dml_apply` reaches share
//! updates and collateral spends of a masternode registered in an earlier block.
//!
//! The block is the coinbase and the first transaction of the first splitmix64 input (byte 2
//! the ProReg operation) whose first transaction is a shared ProReg with its own collateral.
//!
//! `cargo run -p node-l1 --features oracle --example dml_shared_case`

use dash_dml::testkit::replay::hex;
use dash_oracle_sys::{Oracle, Pin, Verdict};
use dash_primitives::SpecialPayload;
use node_l1::dml_apply::{FLAG_FORCE_V24, assemble, cases, generate, split_block, splitmix_bytes};

fn main() -> std::io::Result<()> {
    let oracle = Oracle::new(Pin::V24_0);
    let pre_v24: Vec<u8> =
        (0..cases().len() as u8).filter(|&i| !cases()[usize::from(i)].v24).collect();
    for attempt in 0..1_000_000u64 {
        let mut input = splitmix_bytes(0x0e2b_8000_0000 + attempt, 40);
        input[0] = pre_v24[usize::from(input[0]) % pre_v24.len()];
        input[1] = FLAG_FORCE_V24;
        input[2] = 3;
        let Some(generated) = generate(&input) else {
            continue;
        };
        if attempt % 10_000 == 0 {
            eprintln!("attempt {attempt}");
        }
        let txs = generated.txs();
        let Some(tx) = txs.first() else {
            continue;
        };
        let Some(Ok(d)) = tx.payload() else {
            continue;
        };
        let SpecialPayload::ProviderRegister(p) = &d.payload else {
            continue;
        };
        if p.shares.is_empty() || !p.collateral_hash.is_null() {
            continue;
        }
        let (header, parts) = split_block(&generated.block).expect("a generated block");
        let block = assemble(header, &parts[..2]);
        let Ok(core) = oracle.dml_apply(&generated.chain(), &block, &generated.ctx) else {
            continue;
        };
        let (Verdict::Accept, Some(list)) = (&core.verdict, &core.new_list) else {
            continue;
        };
        let out = format!(
            "{{\n  \"base\": \"{}\",\n  \"input\": \"{}\",\n  \"block\": \"{}\",\n  \"list\": \"{}\"\n}}\n",
            generated.case.name,
            hex(&input),
            hex(&block),
            hex(list)
        );
        let path = format!("{}/src/dml_apply/shared-case.json", env!("CARGO_MANIFEST_DIR"));
        std::fs::write(&path, out)?;
        eprintln!(
            "attempt {attempt}: {} ({} shares) -> {path}",
            generated.case.name,
            p.shares.len()
        );
        return Ok(());
    }
    panic!("no accepted block of one shared ProReg");
}
