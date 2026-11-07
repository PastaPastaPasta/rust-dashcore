| inclusive share of all instructions | dwarf-w1520000-serial-av | dwarf-w925000-serial-noav | dwarf-w421000-serial-noav | dwarf-w925000-serial-av |
|---|---|---|---|---|
| total instructions (G) / samples without a stack | 92.0 / 2.3 % | 63.0 / 5.7 % | 25.8 / 9.6 % | 10.4 / 28.0 % |
| ProcessSpecialTxsInBlock | 32.9 % | 30.6 % | 31.8 % | 22.2 % |
| CQuorumBlockProcessor::ProcessBlock (commitments) | 20.0 % | 17.7 % | 25.2 % | 7.7 % |
| BLS Verify* / AggregateVerify* | 25.0 % | 22.7 % | 25.3 % | 0.0 % |
| BLS check-queue work (CCheckQueue<..Bls..>) | 18.7 % | 16.8 % | 24.5 % | 0.0 % |
| G2Element::FromBytes / IsValid (signatures) | 8.8 % | 7.9 % | 5.1 % | 20.3 % |
| G1Element::FromBytes / IsValid (public keys) | 11.0 % | 10.3 % | 5.0 % | 2.7 % |
| G1 or G2 decode/check (union) | 19.8 % | 18.2 % | 10.1 % | 23.0 % |
| chainlock (CheckCbTxBestChainlock / GetNonNullCoinbaseChainlock / VerifyChainLock) | 6.0 % | 6.0 % | 0.0 % | 0.4 % |
| GetCreditDataFromBlock | 0.6 % | 0.4 % | 0.0 % | 6.9 % |
| RebuildListFromBlock | 0.5 % | 0.6 % | 0.8 % | 5.7 % |
| CheckMerkleRoot | 0.8 % | 0.8 % | 0.0 % | 5.0 % |
| VerifyScript | 0.7 % | 0.0 % | 6.4 % | 0.0 % |
