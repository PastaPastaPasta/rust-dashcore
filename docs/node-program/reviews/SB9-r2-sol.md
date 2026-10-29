# SB9 implementation review — Sol r2 (narrow)

Reviewed `node/sb9-dash-filters` at **11164dbcb**, against r1 4848ea92f and base 300721627.

**Verdict: APPROVE.** No new findings (no R2-*). Sol gate satisfied.

- **R1-M1 — closed.** The pre-SB9 reader/writer was compiled alongside the adapter. For both matchers:
  - a complete filter followed by `WouldBlock` returns after four bytes;
  - concatenated filters consume four bytes;
  - a zero-element filter followed by more input consumes one byte;
  - empty queries consume only the count.

  41,678 differential comparisons matched on results, bytes consumed and read-error boundaries. They covered truncated and noncanonical counts, body failures, hits, misses and early returns. Complete filters at N = 252/253/254/65535/65536 matched. No extra byte reads were found. Evidence: `/work/scratch/sb9-r2-sol/adapter-probe.log`.
- **R1-m1 — closed.**
  - The r1 reproductions now match the old encoder: `p=33, m=784931` gives 6 bytes, and `p=19, m=2^32` gives 567 bytes.
  - Also tested: `p=63`, `m=u64::MAX`, and products equal to or just below `u64::MAX`.
  - Overflow returns `RangeOverflow` in the pure path and `InvalidData` through the adapter, before reading the body or writing output.
- **R1-n1 — closed.** Both new PF0 rows resolve to the direct `dash-filters` SipHash bench, and both pass in bench test mode. Retiring the duplicate is BL-78.
- **Qualification (pre-existing in r1, not a finding).** A count above Core's `MAX_SIZE` becomes zero. For `N=MAX_SIZE+1, m=0`, the old reader returns true after 8 bytes, while the adapter returns false after 5.
- **Core parity.** Both the pure crate and the adapter pass:
  - the 7 `blockfilters.json` vectors;
  - the 149 special-transaction filters and headers.
- **PF0.** `kwm-filter-scan-2000` +0.195%, all in `ipnsort<u64>` (about 830 instructions per sort, same call counts). The diff gives no explanation (element type, count or extra copy). Under the 0.5% gate.
