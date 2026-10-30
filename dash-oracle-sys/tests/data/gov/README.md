# Governance vectors (GV1a, O2a-GOV)

Every answer in these files is Core's, computed by the v24.0 oracle at the pin named in each file's
header. The tests regenerate them with `DASH_ORACLE_REGENERATE=1` and otherwise check that the
oracle still computes exactly the committed text.

## `proposals.txt` (L0: `governance::ValidateProposal()`)

Written by `tests/gov.rs`. One line per call, fields separated by ` | `:

```
<name> | expiration <0|1> | script <0|1> | time <adjusted time> | <data hex> | <valid|invalid> | <errors or ->
```

- `name`: `valid <i>` / `invalid <i>` is the index into Core's `src/test/data/proposals_valid.json`
  / `proposals_invalid.json`. The suffix says which call of `governance_validators_tests.cpp` it is:
  ` legacy` is `CreateEncodedProposalObject()` (`[["proposal", {...}]]`) with `fCheckExpiration`
  true, ` legacy unexpired` is the same data with it false, ` expired` is the current form with it
  true, and no suffix is the current form with it false. `extreme end epoch` is
  `extreme_end_epoch_is_not_expired`.
- `expiration` / `script`: `fCheckExpiration` and `fAllowScript`.
- `time`: the adjusted time `CProposalValidator::Validate()` compares `end_epoch` with
  (`GetAdjustedTime()`, set as mock time on mainnet).
- `data hex`: the `strHexData` argument.
- `errors`: Core's `strErrorMessages` exactly (each message ends in `;`), or `-` when empty.
