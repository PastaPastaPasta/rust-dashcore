#!/usr/bin/env python3
"""Patch O9b S6a's and S6b-0's env-gated mutants into a reflink copy (never the live worktree).

Each mutant loosens one bound by one, drops one check, or (a -T mutant) tightens a bound by one,
when O9B_MUT names it; O9b-S6a-notes D24 lists S6a's and D27 S6b-0's (M-RH-*, M-CB-*) with the
test each fails. Usage:

    cp -a --reflink=always <worktree> /work/scratch/<task>/mut
    mutants.py /work/scratch/<task>/mut
    cargo test -p dash-dml --lib --no-run   # in the copy
    O9B_MUT=M-QTYPES <test binary> snapshot::tests::   # once per mutant; none = baseline

M-RECORDS-CONST is killed only by the ignored release test `record_budget_constant_matches_core`.
"""
import pathlib
import sys

root = pathlib.Path(sys.argv[1]) / "dash-dml/src"
assert "/work/scratch/" in str(root), "mutants only in a scratch copy"


def M(name):
    return f'crate::o9b_mut("{name}")'


def P(name):
    return f"(usize::from({M(name)}))"


def drop(name, check):
    """`check` (a refusal condition) never holds under the mutant."""
    return f"(!{M(name)} && {check})"


edits = {
    "lib.rs": [
        ("pub mod blockprocessor;\n",
         "pub mod blockprocessor;\n#[allow(clippy::disallowed_methods)]\n"
         "pub(crate) fn o9b_mut(name: &str) -> bool {\n"
         "    std::env::var(\"O9B_MUT\").is_ok_and(|v| v == name)\n}\n"),
    ],
    "snapshot/codec.rs": [
        ("read_bounded(&mut r, dash_params::AVAILABLE_LLMQS.len(), ",
         f"read_bounded(&mut r, dash_params::AVAILABLE_LLMQS.len() + {P('M-QTYPES')}, "),
        ("read_bounded(&mut r, max_historical_mn_lists(), ",
         f"read_bounded(&mut r, max_historical_mn_lists() + {P('M-HISTORY')}, "),
        ("let mut remaining_operations = max_historical_mn_operations();",
         f"let mut remaining_operations = max_historical_mn_operations() + {P('M-OPS')};"),
        ("read_bounded(&mut r, MAX_MODIFIERS, ",
         f"read_bounded(&mut r, MAX_MODIFIERS + {P('M-MODIFIERS')}, "),
        ("RangesSet::decode_bounded(input, MAX_RANGES)",
         f"RangesSet::decode_bounded(input, MAX_RANGES + u64::from({M('M-RANGES')}))"),
        ("read_bounded(&mut r, MAX_MNHF_SIGNALS, ",
         f"read_bounded(&mut r, MAX_MNHF_SIGNALS + {P('M-SIGNALS')}, "),
        ('read_bounded(r, MAX_QUORUM_SIZE, "commitment signers")',
         f'read_bounded(r, MAX_QUORUM_SIZE + {P("M-SIGNERS")} - {P("M-SIGNERS-T")}, '
         '"commitment signers")'),
        ('read_bounded(r, MAX_QUORUM_SIZE, "commitment valid members")',
         f'read_bounded(r, MAX_QUORUM_SIZE + {P("M-VALID")} - {P("M-VALID-T")}, '
         '"commitment valid members")'),
        ('read_bounded(r, MAX_MNS, "rotation bitset")',
         f'read_bounded(r, MAX_MNS + {P("M-ROTBITS")}, "rotation bitset")'),
        ("read_bounded(r, MAX_SKIPLIST_ENTRIES, ",
         f"read_bounded(r, MAX_SKIPLIST_ENTRIES + {P('M-SKIP')}, "),
        ("read_bounded(r, expected_active, ",
         f"read_bounded(r, expected_active + {P('M-ACTIVE')}, "),
        ("read_bounded(r, total.saturating_sub(expected_active), ",
         f"read_bounded(r, total.saturating_sub(expected_active) + {P('M-SAFETY')}, "),
        ("read_bounded(r, ROTATION_CYCLES, ",
         f"read_bounded(r, ROTATION_CYCLES + {P('M-ROTSNAPS')}, "),
        ("if nbytes > r.remaining() {",
         f"if {drop('M-BITSET-REMAIN', 'nbytes > r.remaining()')} {{"),
        ("if nbytes * 8 != size && bytes",
         f"if !{M('M-PADDING')} && nbytes * 8 != size && bytes"),
        ("if rotation_flag > 1 {",
         f"if rotation_flag > 1 + u8::from({M('M-ROTFLAG')}) {{"),
    ],
    "snapshot/validate.rs": [
        ("if records_processed > max_records {",
         f"if records_processed > max_records + {P('M-RECORDS')} {{"),
        ("usize::from(bit) >= MAX_MNHF_SIGNALS ",
         f"usize::from(bit) >= MAX_MNHF_SIGNALS + {P('M-SIGNAL-BIT')} "),
        ("i32::from(index) >= params.signing_active_quorum_count",
         f"i32::from(index) >= params.signing_active_quorum_count + i32::from({M('M-INDEX')})"),
        ("|| qc.signers.num_bits > MAX_QUORUM_SIZE as u64",
         f"|| qc.signers.num_bits > MAX_QUORUM_SIZE as u64 + u64::from({M('M-SIGNERS-V')})"),
        ("if entry.previous_block_hash != previous_hash",
         f"if {drop('M-CHAIN-PREV', 'entry.previous_block_hash != previous_hash')}"),
        ("|| entry.block_hash.is_null()",
         f"|| {drop('M-CHAIN-NULL', 'entry.block_hash.is_null()')}"),
        ("|| entry.block_hash == self.base_block_hash",
         f"|| {drop('M-CHAIN-BASE', 'entry.block_hash == self.base_block_hash')}"),
        ("|| lists.contains_key(entry.block_hash.as_bytes())",
         f"|| {drop('M-CHAIN-REPEAT', 'lists.contains_key(entry.block_hash.as_bytes())')}"),
        ("|| entry.height < 0",
         f"|| {drop('M-CHAIN-NEGATIVE', 'entry.height < 0')}"),
        ("|| entry.canonical_list_hash.is_null()",
         f"|| {drop('M-CHAIN-LISTHASH', 'entry.canonical_list_hash.is_null()')}"),
        ("        if diff.added.iter().any(|dmn| {",
         f"        if !{M('M-REUSE')} && diff.added.iter().any(|dmn| {{"),
        ("            check_canonical_invariants(&current, net)?;",
         f"            if !{M('M-POSTINV')} {{ check_canonical_invariants(&current, net)?; }}"),
        ('.expect("ApplyDiffForSnapshot put it back");\n                check_mn_encoding(dmn, &mut bytes)?;',
         '.expect("ApplyDiffForSnapshot put it back");\n'
         f"                if !{M('M-POSTENC')} {{ check_mn_encoding(dmn, &mut bytes)?; }}"),
        ("        for (id, state) in diff.updated.iter().rev() {",
         "        let mut order: Vec<_> = diff.updated.iter().collect();\n"
         f"        if !{M('M-ORDER')} {{ order.reverse(); }}\n"
         "        for (id, state) in order {"),
        ("if self.quorums.len() > dash_params::AVAILABLE_LLMQS.len()",
         f"if self.quorums.len() > dash_params::AVAILABLE_LLMQS.len() + {P('M-V-QUORUMS')}"),
        ("|| self.historical_mn_list_diffs.len() > max_historical_mn_lists()",
         f"|| self.historical_mn_list_diffs.len() > max_historical_mn_lists() + {P('M-V-HISTORY')}"),
        ("|| self.quorum_modifiers.len() > MAX_MODIFIERS",
         f"|| self.quorum_modifiers.len() > MAX_MODIFIERS + {P('M-V-MODIFIERS')}"),
        ("|| self.mnhf_signals.len() > MAX_MNHF_SIGNALS",
         f"|| self.mnhf_signals.len() > MAX_MNHF_SIGNALS + {P('M-V-SIGNALS')}"),
        ("if operations > max_historical_mn_operations() {",
         f"if operations > max_historical_mn_operations() + {P('M-V-OPS')} {{"),
        ("if pool.indexes.range_count() as u64 > MAX_RANGES {",
         f"if pool.indexes.range_count() as u64 > MAX_RANGES + u64::from({M('M-V-RANGES')}) {{"),
        ("|| snapshot.active_quorum_members.len() > MAX_MNS",
         f"|| snapshot.active_quorum_members.len() > MAX_MNS + {P('M-V-ROTBITS')}"),
        ("|| snapshot.skip_list.len() > MAX_SKIPLIST_ENTRIES",
         f"|| snapshot.skip_list.len() > MAX_SKIPLIST_ENTRIES + {P('M-V-SKIP')}"),
        ("if qc.signers.num_bits != qc.valid_members.num_bits",
         f"if {drop('M-V-SIZEEQ', 'qc.signers.num_bits != qc.valid_members.num_bits')}"),
        ("|| qc.signers.num_bits == 0",
         f"|| {drop('M-V-EMPTY', 'qc.signers.num_bits == 0')}"),
        ("if data.active_commitments.len() > active",
         f"if data.active_commitments.len() > active + {P('M-V-ACTIVE')}"),
        ("|| data.safety_commitments.len() > total.saturating_sub(active)",
         f"|| data.safety_commitments.len() > total.saturating_sub(active) + {P('M-V-SAFETY')}"),
        ("                ROTATION_CYCLES\n            } else {",
         f"                ROTATION_CYCLES + {P('M-V-ROTCYCLES')}\n            }} else {{"),
        ("if self.version != EVO_SNAPSHOT_VERSION {",
         f"if {drop('M-V-VERSION', 'self.version != EVO_SNAPSHOT_VERSION')} {{"),
        ("        check_canonical_list(&self.mn_list, net)?;",
         f"        if !{M('M-V-BASELIST')} {{ check_canonical_list(&self.mn_list, net)?; }}"),
        ("self.reconstruct_historical_mn_lists(net, MAX_RECONSTRUCTION_RECORDS)",
         f"self.reconstruct_historical_mn_lists(net, MAX_RECONSTRUCTION_RECORDS + {P('M-RECORDS-CONST')})"),
        ("|| data.rotation_snapshots.len() > max_snapshots",
         f"|| data.rotation_snapshots.len() > max_snapshots + {P('M-SNAPCOUNT')}"),
        ("&& !is_strictly_sorted_by(&data.rotation_snapshots, rotation_key)",
         f"&& !{M('M-ROTORDER')} && !is_strictly_sorted_by(&data.rotation_snapshots, rotation_key)"),
        ("|| !cycle_hashes.insert(*entry.cycle_base_block_hash.as_bytes())",
         f"|| (!cycle_hashes.insert(*entry.cycle_base_block_hash.as_bytes()) && !{M('M-CYCLEDUP')})"),
        ("|| !historical.contains_key(entry.work_block_hash.as_bytes())",
         f"|| {drop('M-ROTWORK', '!historical.contains_key(entry.work_block_hash.as_bytes())')}"),
    ],
    "codec.rs": [
        ("    if n > MAX_MNS as u64 {\n        return Err(CodecError::new(format!(\"oversized evo snapshot {field}\")));",
         f"    if n > MAX_MNS as u64 + u64::from({M('M-MNSECTION')}) {{\n"
         "        return Err(CodecError::new(format!(\"oversized evo snapshot {field}\")));"),
        ("let mut dmn = Reader::new(input, Some(MAX_MN_COMPACT_ITEMS)).dmn()?;",
         f"let mut dmn = Reader::new(input, Some(MAX_MN_COMPACT_ITEMS + u64::from({M('M-MNBUDGET')}))).dmn()?;"),
        ("    if *run > MAX_HASH_PREFIX_RUN {",
         f"    if *run > MAX_HASH_PREFIX_RUN + {P('M-PREFIX')} {{"),
        ("    if cold.payouts.len() > MAX_PAYOUT_SHARES {",
         f"    if cold.payouts.len() > MAX_PAYOUT_SHARES + {P('M-PAYOUTS')} {{"),
        ("        if previous_id.is_some_and(|p| p >= dmn.internal_id)",
         "        if previous_id.is_some_and(|p| "
         f"(p > dmn.internal_id && !{M('M-ADDORDER')}) || (p == dmn.internal_id && !{M('M-ADDORDER-EQ')}))"),
    ],
    "snapshot/checks.rs": [
        ("if base_height < 0 {",
         f"if base_height < 0 - i32::from({M('M-RH-BASE')}) + i32::from({M('M-RH-BASE-T')}) {{"),
        ("if params.dkg_interval <= 0 || params.signing_active_quorum_count <= 0 {",
         f"if params.dkg_interval <= 0 - i32::from({M('M-RH-DKG')}) + i32::from({M('M-RH-DKG-T')})\n"
         f"            || params.signing_active_quorum_count\n"
         f"                <= 0 - i32::from({M('M-RH-ACTIVE')}) + i32::from({M('M-RH-ACTIVE-T')})\n"
         "        {"),
        ("let h = base_height - base_height % params.dkg_interval;",
         f"let h = base_height - if {M('M-RH-ROUND')} {{ 0 }} else {{ base_height % params.dkg_interval }};"),
        ("(1, ROTATION_CYCLES)",
         f"(1 - usize::from({M('M-RH-FIRST')}), ROTATION_CYCLES + {P('M-RH-ROTCOUNT')})"),
        ("params.signing_active_quorum_count.checked_add(1).ok_or_else(invalid)?;",
         f"if !{M('M-RH-ACTIVE-OVERFLOW')} {{\n"
         "                params.signing_active_quorum_count.checked_add(1).ok_or_else(invalid)?;\n"
         "            }"),
        ("(0, snapshot_commitment_count(params, false)?)",
         f"(0, snapshot_commitment_count(params, false)? + {P('M-RH-COUNT')})"),
        (".and_then(|i| i.checked_mul(params.dkg_interval))\n"
         "                .and_then(|back| h.checked_sub(back))",
         f".and_then(|i| if {M('M-RH-OVERFLOW')} {{ Some(i.wrapping_mul(params.dkg_interval)) }} "
         "else { i.checked_mul(params.dkg_interval) })\n"
         f"                .and_then(|back| if {M('M-RH-OVERFLOW')} {{ Some(h.wrapping_sub(back)) }} "
         "else { h.checked_sub(back) })"),
        ("quorum_height.checked_sub(WORK_DIFF_DEPTH as i32)",
         f"if {M('M-RH-WORK-OVERFLOW')} {{ Some(quorum_height.wrapping_sub(WORK_DIFF_DEPTH as i32)) }} "
         f"else {{ quorum_height.checked_sub(WORK_DIFF_DEPTH as i32 - i32::from({M('M-RH-WORK')})) }}"),
        ("        self.validate(false, net)?;\n"
         "        if cbtx.height != self.mn_list.height() {",
         f"        if {M('M-CB-ORDER')} && cbtx.height != self.mn_list.height() {{\n"
         '            return Err(CodecError::new("evo snapshot coinbase height mismatch"));\n'
         "        }\n"
         f"        if !{M('M-CB-VALIDATE')} {{\n"
         f"            self.validate({M('M-CB-CANON')}, net)?;\n"
         "        }\n"
         f"        if {drop('M-CB-HEIGHT', 'cbtx.height != self.mn_list.height()')} {{"),
        ("if merkle_root_mn_list_full(&self.mn_list).ok() != Some(cbtx.merkle_root_mn_list) {",
         f"if {drop('M-CB-MNROOT', 'merkle_root_mn_list_full(&self.mn_list).ok() != Some(cbtx.merkle_root_mn_list)')} {{"),
        ("if cbtx.version >= CBTX_VERSION_MERKLE_ROOT_QUORUMS {",
         f"if cbtx.version >= CBTX_VERSION_MERKLE_ROOT_QUORUMS + u16::from({M('M-CB-QGATE')})\n"
         f"            - u16::from({M('M-CB-QGATE-T')})\n"
         "        {"),
        (".flat_map(|data| &data.active_commitments)",
         ".flat_map(|data| data.active_commitments.iter().chain(\n"
         f"                    data.safety_commitments[..if {M('M-CB-SAFETY')} {{ data.safety_commitments.len() }} else {{ 0 }}].iter()))"),
        ("            if merkle_root_quorums_from_hashes(hashes).ok()\n"
         "                != Some(cbtx.merkle_root_quorums.unwrap_or_default())\n",
         f"            if !{M('M-CB-QROOT')}\n"
         f"                && !({M('M-CB-NONE')} && cbtx.merkle_root_quorums.is_none())\n"
         "                && merkle_root_quorums_from_hashes(hashes).ok()\n"
         "                    != Some(cbtx.merkle_root_quorums.unwrap_or_default())\n"),
        ("if cbtx.version >= CBTX_VERSION_CLSIG_AND_BALANCE\n"
         "            && self.credit_pool.locked",
         f"if !{M('M-CB-BAL')}\n"
         f"            && cbtx.version >= CBTX_VERSION_CLSIG_AND_BALANCE + u16::from({M('M-CB-BALGATE')})\n"
         f"                - u16::from({M('M-CB-BALGATE-T')})\n"
         "            && self.credit_pool.locked"),
    ],
}

for rel, pairs in edits.items():
    path = root / rel
    text = path.read_text()
    for old, new in pairs:
        n = text.count(old)
        assert n == 1, f"{rel}: {n} matches for {old!r}"
        text = text.replace(old, new)
    path.write_text(text)
    print(f"{rel}: {len(pairs)} edits")
