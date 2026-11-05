use dash_consensus::script::ScriptNum;
use dash_primitives::MerkleRoot;

use super::*;

fn facts(height: u32) -> TipFacts {
    TipFacts {
        prev: BlockHash::from([7; 32]),
        height,
        version: 0x3000_0000,
        time: 1_700_000_150,
        bits: 0x207f_ffff,
        subsidy: 500_000_000,
    }
}

fn script_sig(height: u32) -> Vec<u8> {
    let block = block(&facts(height), &[0x51]);
    block.txs()[0].inputs()[0].script_sig.to_vec()
}

#[test]
fn heights_one_to_sixteen_are_small_integer_opcodes() {
    for h in 1..=16u8 {
        assert_eq!(script_sig(u32::from(h)), [OP_1 - 1 + h, OP_0], "height {h}");
    }
}

/// Above 16 the height is a push of its minimal script number, which the interpreter reads back
/// as the height under the minimal-encoding rule: `CScriptNum::serialize` and `set_vch` agree.
#[test]
fn larger_heights_push_their_minimal_script_number() {
    for h in [17u32, 127, 128, 255, 256, 431, 32_767, 32_768, 8_388_607, 8_388_608, u32::MAX] {
        let sig = script_sig(h);
        let (len, rest) = sig.split_first().unwrap();
        assert_eq!(usize::from(*len), rest.len() - 1, "height {h}");
        let (num, tail) = rest.split_at(usize::from(*len));
        assert_eq!(tail, [OP_0], "height {h}");
        assert_eq!(ScriptNum::decode(num, true, 5), Ok(ScriptNum::new(h.into())), "height {h}");
    }
}

#[test]
fn the_coinbase_pays_the_subsidy_to_the_script() {
    let f = facts(300);
    let block = block(&f, &[0x76, 0xa9]);
    assert_eq!(block.txs().len(), 1);
    let cb = &block.txs()[0];
    assert!(cb.is_coinbase());
    assert_eq!(cb.version(), 2);
    assert_eq!(cb.tx_type(), TxType::new(0));
    assert_eq!(cb.lock_time(), 0);
    assert!(cb.extra_payload().is_empty());
    let input = &cb.inputs()[0];
    assert!(input.prevout.is_null());
    assert_eq!(input.sequence, u32::MAX);
    assert_eq!(cb.outputs().len(), 1);
    assert_eq!(cb.outputs()[0].value, f.subsidy);
    assert_eq!(cb.outputs()[0].script_pubkey.to_vec(), [0x76, 0xa9]);

    let h = block.header();
    assert_eq!(
        (h.version(), h.prev_hash(), h.time(), h.bits(), h.nonce()),
        (f.version, f.prev, f.time, f.bits, 0)
    );
    assert_eq!(h.merkle_root(), MerkleRoot::from([0; 32]));
}

#[test]
fn the_time_is_now_unless_the_median_time_past_is_not_behind_it() {
    assert_eq!(block_time(1_000, 500), 1_000);
    assert_eq!(block_time(1_000, 999), 1_000);
    assert_eq!(block_time(1_000, 1_000), 1_001);
    assert_eq!(block_time(1_000, 5_000), 5_001);
    // `uint32_t` stores, as Core's.
    assert_eq!(block_time(1 << 32, 0), 0);
    assert_eq!(block_time(0, (1 << 32) + 4), 5);
}
