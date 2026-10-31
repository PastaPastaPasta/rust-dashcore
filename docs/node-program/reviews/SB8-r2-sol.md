# SB8 nit round: Sol r2 (gpt-6-sol xhigh), head f7f600015

**Verdict: ACCEPT WITH NITS**

1. **Low (doc; theoretical over-accept): SB8-notes.md:21, dash-evo/src/sml/view.rs:143-150.** D5's claim that SmlScratch only accepts where Core's AddMN rejects is not literally true. Core's GetUniquePropertyHash is an untagged SerializeHash. A native IPv6 CService with bytes `0x0f || "aaa.example.com"` and port 9999 serializes to the same 18 bytes as DomainPort("aaa.example.com", 9999), yet UniqueKey::Service and UniqueKey::Domain are distinct keys. The separation predates r2. It cannot occur in a canonical DML/SML, and membersSig binds the key multiset. Fix: qualify D5 to canonical on-chain entries.
2. **Nit (test precision): l1_llmq.rs:1447-1457.** `core_line_names` does not enforce D24's five suffix forms. Fix: allow only the named suffix shape for the value-bearing reasons and an empty suffix for the rest.
3. **Nit (wording): SB8-notes.md:40, l1_llmq.rs:1426-1439, 1569-1606.** D24 says every sampled commitment is mutated per check, but some mutations are conditional. Fix the wording.

**Checks:**
- The legacy 18-byte wire form is Core's ADDRv1 CService.
- NetInfoV1::decode empties an invalid service, as Core does.
- IPv4-mapped legacy and extended IPv4 get the same key.
- The 12 SML tests and 13 mutation forms pass, and the reasons match Core's log.
- Deferring quorumIndex is correct.
- D20 and D22 match their code.

**Validation:** sml::view::tests 12 passed; l1_llmq sampled_commitments… 1 passed.
