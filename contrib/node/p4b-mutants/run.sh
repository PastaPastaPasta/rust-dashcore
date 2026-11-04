#!/bin/bash
# P4b mutants: each is a perl substitution on one committed file; the sim tests, the dashd
# transcript replays or the library's unit tests must fail.
# Restores with `git checkout` (a fresh mtime, so cargo rebuilds).
set -u
cd "$(git rev-parse --show-toplevel)"
export TMPDIR=${TMPDIR:-/work/scratch/p4b/tmp} CARGO_BUILD_JOBS=12
S=dashd-p2p/src/processing/serve_blocks.rs
B=dashd-p2p/src/processing/bloom.rs
L=dashd-p2p/src/processing/logic.rs
TESTS=(-p dashd-p2p --features sim --lib --test sim_serve_blocks --test dashd_serve_transcripts)
run() { # name file perl-expr [cargo test args, default TESTS]
  local name=$1 file=$2 expr=$3
  shift 3
  (($#)) || set -- "${TESTS[@]}"
  perl -0pi -e "$expr" "$file"
  if git diff --quiet -- "$file"; then echo "MUTANT $name: NOT APPLIED"; return; fi
  out=$(cargo test --no-fail-fast "$@" 2>&1)
  git checkout -q -- "$file"
  if echo "$out" | grep -q "^error\[\|^error:.*could not compile"; then
    echo "MUTANT $name: BUILD FAILED"
  elif ! echo "$out" | grep -q "^test .* FAILED"; then
    echo "MUTANT $name: SURVIVED"
  else
    echo "MUTANT $name: KILLED by $(echo "$out" | grep -E '^test .* FAILED' | grep -v '^test result' | sed 's/^test //; s/ \.\.\. FAILED//' | tr '\n' ' ')"
  fi
}
run limited-buffer-3  $S 's/NODE_NETWORK_LIMITED_MIN_BLOCKS \+ 2/NODE_NETWORK_LIMITED_MIN_BLOCKS + 3/'
run limited-ge        $S 's/\) > NODE_NETWORK_LIMITED_MIN_BLOCKS/) >= NODE_NETWORK_LIMITED_MIN_BLOCKS/'
run limited-noban-off $S 's/!cx\.peer\.has_permission\(Permissions::NO_BAN\)\n        && //'
run limited-network   $S 's/&& !services\.has\(ServiceFlags::NODE_NETWORK\)\n//'
run bloom-size-lt     $B 's/self\.data\.len\(\) <= MAX_BLOOM_FILTER_SIZE/self.data.len() < MAX_BLOOM_FILTER_SIZE/'
run bloom-funcs-lt    $B 's/self\.hash_funcs <= MAX_HASH_FUNCS/self.hash_funcs < MAX_HASH_FUNCS/'
run bloom-funcs-le51  $B 's/self\.hash_funcs <= MAX_HASH_FUNCS/self.hash_funcs <= MAX_HASH_FUNCS + 1/'
run getblocks-501     $S 's/pub const GETBLOCKS_LIMIT: usize = 500;/pub const GETBLOCKS_LIMIT: usize = 501;/'
run getblocks-cont-off $S 's/if i \+ 1 == GETBLOCKS_LIMIT/if i == GETBLOCKS_LIMIT/'
run locator-102       dashd-p2p/src/processing/stubs.rs 's/MAX_LOCATOR_SZ: usize = 101/MAX_LOCATOR_SZ: usize = 102/'
run block-notfound    $L 's/if peer\.pause_send\(\) \|\| inv\.inv_type\.is_gen_blk\(\)/if peer.pause_send()/'
run failed-no-disconnect $S 's/cx\.disconnect\("cannot load block from disk"\);\n//'
run pruned-no-disconnect $S 's/cx\.disconnect\("block pruned before it could be read"\);\n//'
run filteradd-no-score $S 's/None => cx\.misbehaving\(100, "bad filteradd message"\)/None => {}/'
run blocktxn-depth-11 $S 's/pub const MAX_BLOCKTXN_DEPTH: i64 = 10;/pub const MAX_BLOCKTXN_DEPTH: i64 = 11;/'
run cmpct-depth-6     $S 's/pub const MAX_CMPCTBLOCK_DEPTH: i64 = 5;/pub const MAX_CMPCTBLOCK_DEPTH: i64 = 6;/'
run no-data-served    $S 's/let Some\(data_pos\) = data_pos\(cx, snap, e\.id\) else \{\n        return Ok\(None\);\n    \};/let data_pos = data_pos(cx, snap, e.id).unwrap_or(BlockPos { file: 0, pos: e.id.0 });/'
M=dashd-p2p/src/processing/merkle.rs
run getblocks-499     $S 's/pub const GETBLOCKS_LIMIT: usize = 500;/pub const GETBLOCKS_LIMIT: usize = 499;/'
run read-branch-swap  $S 's/tracing::debug!\(target: "net", "Block was pruned before it could be read, disconnect peer=\{id\}"\);/tracing::error!("Cannot load block from disk, disconnect peer={id}");/'
run protx-reg-insert-off $B 's/                    \|\| share;\n                if found && self\.update_all\(\) \{/                    || share;\n                if found \&\& false {/'
run protx-hash-check-off $B 's/if self\.contains\(pro_tx_hash\.as_bytes\(\)\) \{\n            return true;/if false {\n            return true;/'
run protx-owner-off   $B 's/\|\| self\.contains\(p\.key_id_owner\.as_bytes\(\)\)\n                    \|\| self\.contains\(p\.key_id_voting/|| self.contains(p.key_id_voting/'
run p2pubkey-only-off $B 's/\|\| mode == BLOOM_UPDATE_P2PUBKEY_ONLY/|| false/'
run allowed-type-6    $M 's/!matches!\(ty\.to_base\(\), 6 \| 7 \| 13\.\.\)/!matches!(ty.to_base(), 7 | 13..)/'
run allowed-type-13   $M 's/!matches!\(ty\.to_base\(\), 6 \| 7 \| 13\.\.\)/!matches!(ty.to_base(), 6 | 7 | 14..)/'
# Review r1 fix-forward (DEC-319).
P=dash-p2p/src/policy.rs
run share-owner-off   $B 's/\n\s*\|\| self\.contains\(s\.key_id_owner\.as_bytes\(\)\)//'
run share-reward-refund $B 's/\|\| self\.check_script\(reward\.as_bytes\(\)\)/|| self.check_script(s.script_refund.as_bytes())/'
run assetlock-vout-index $B 's/(p\.credit_outputs\.iter\(\)\.enumerate\(\) \{\n\s*found = self\.process_tx_out\(out, &hash, )i as u32\)/$1(tx.outputs().len() + i) as u32)/'
run proupshare-insert-off $B 's/SpecialPayload::ProviderUpdateShare\(p\) => self\n\s*\.update_payload_matches\(&p\.pro_tx_hash, \|f\| \{\n\s*f\.check_script\(p\.script_reward\.as_bytes\(\)\)\n\s*\}\),/SpecialPayload::ProviderUpdateShare(p) => {\n                self.contains(p.pro_tx_hash.as_bytes()) || self.check_script(p.script_reward.as_bytes())\n            }/'
run owner-payout-eq3  $B 's/if version >= 3 \{/if version == 3 {/'
run tx-relay-arm-off  $S 's/\} else if cx\.state\.tx_relay \{\n(\s*)cx\.state\.bloom = Some\(filter\);/} else {\n$1cx.state.bloom = Some(filter);/'
run read-live-pos     $S 's/cx\.chain\.read_block_at\(e\.id, serve\.data_pos\)/cx.chain.read_block(e.id)/'
run txn-live-pos      $S 's/cx\.chain\.read_block_at\(e\.id, pos\)/cx.chain.read_block(e.id)/'
run getblocks-can-relay-off $S 's/if can_relay \{/if true {/'
run filterload-limit-text $P 's/\s*"filterload" => \(0, MAX_BLOOM_FILTER_SIZE as u64\),//'
run filteradd-limit-text $P 's/\s*"filteradd" => \(0, MAX_SCRIPT_ELEMENT_SIZE as u64\),//'
run nobloom-target-net $S 's/target: "net\|netconn", "\{cmd\} received/target: "net", "{cmd} received/'
# Review r2 (DEC-328).
run update-all-p2pk   $B 's/self\.flags & BLOOM_UPDATE_MASK == BLOOM_UPDATE_ALL/self.flags \& BLOOM_UPDATE_MASK != 0/'
run data-pos-no-data  dashd-node/src/p2p/chain.rs 's/data_pos: have_data\.then\(/data_pos: true.then(/' \
  -p dashd-node --lib p2p::chain::
