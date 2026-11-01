#!/usr/bin/env python3
"""ProTx version 3 payloads with non-canonical extended addresses, checked by dashd.

Dash Core v24.0.0-rc.4 reads the `netInfo` purpose lists of a ProRegTx/ProUpServTx version 3 only
when the purposes are known and strictly ascending (a repeated, out-of-order or unknown purpose
fails the read, #7888), reads entries it can't use as invalid instead of failing, and signs and
verifies the payload over the object serialized again. This builds such payloads from
transactions Core made itself and has Core judge them:

1. Restart node0 of a Core functional-test chain where v24 is active (feature_masternode_payout_shares)
   on a copy, with the chain parameters the test used.
2. Register a regular masternode and an Evo node through `protx register_prepare` and
   `protx register_prepare_evo`, sign Core's sign message with the collateral key, and later update
   both through `protx update_service` and `protx update_service_evo` with submit=false. Core
   builds and signs every payload.
3. Rewrite only the netInfo bytes of those payloads: repeat a purpose, put the purposes out of
   order, use unknown purposes (each a payload Core cannot read), end a list with an address
   claiming more bytes than are left (Core consumes none of them), use invalid entries, overlong
   or truncated domains. The payload signatures stay Core's; each variant gets a higher fee and
   its inputs re-signed by the wallet.
4. Record per variant Core's `testmempoolaccept` verdict and `decoderawtransaction` JSON. Core's
   own payloads are mined (the updates need the registrations); record `getrawtransaction` for
   them and the blocks.

Usage: craft_netinfo.py <dashd-bin-dir> <functional-test-node0-datadir> <work-dir> <out.json>
"""
import base64
import hashlib
import json
import os
import struct
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from extract import dashd_version, start_node  # noqa: E402

FEE_BUMP = 20000  # duffs, covers the bytes the variants add


def cs(n):
    """Compact size."""
    if n < 0xfd:
        return bytes([n])
    if n <= 0xffff:
        return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)


def read_cs(b, i):
    n = b[i]
    if n < 0xfd:
        return n, i + 1
    if n == 0xfd:
        return struct.unpack_from("<H", b, i + 1)[0], i + 3
    if n == 0xfe:
        return struct.unpack_from("<I", b, i + 1)[0], i + 5
    return struct.unpack_from("<Q", b, i + 1)[0], i + 9


def service(net, addr, port):
    return bytes([0x01, net]) + cs(len(addr)) + addr + struct.pack(">H", port)


def domain(host, port):
    return b"\x02" + cs(len(host)) + host + struct.pack(">H", port)


def net_info(lists):
    """An ExtNetInfo version 1 from (purpose, [entry bytes]) lists, in the given order."""
    out = b"\x01" + cs(len(lists))
    for purpose, entries in lists:
        out += bytes([purpose]) + cs(len(entries)) + b"".join(entries)
    return out


def parse_net_info(b, i):
    """Parses a well-formed ExtNetInfo version 1 at b[i:], returning ([(purpose, [entry])], end)."""
    assert b[i] == 1, b[i:i + 8].hex()
    count, i = read_cs(b, i + 1)
    lists = []
    for _ in range(count):
        purpose = b[i]
        n, i = read_cs(b, i + 1)
        entries = []
        for _ in range(n):
            start = i
            if b[i] == 0x01:
                size, i = read_cs(b, i + 2)
                i += size + 2
            elif b[i] == 0x02:
                size, i = read_cs(b, i + 1)
                i += size + 2
            else:
                assert b[i] == 0xff
                i += 1
            entries.append(b[start:i])
        lists.append((purpose, entries))
    return lists, i


class Tx:
    """A special transaction split into the parts the variants touch."""

    def __init__(self, hex_):
        b = bytes.fromhex(hex_)
        i = 4
        n_in, i = read_cs(b, i)
        for _ in range(n_in):
            i += 36
            n, i = read_cs(b, i)
            i += n + 4
        self.head = b[:i]
        n_out, i = read_cs(b, i)
        self.outputs = []
        for _ in range(n_out):
            value = struct.unpack_from("<q", b, i)[0]
            n, j = read_cs(b, i + 8)
            self.outputs.append([value, b[i + 8:j + n]])
            i = j + n
        self.locktime = b[i:i + 4]
        n, i = read_cs(b, i + 4)
        self.payload = b[i:i + n]
        assert i + n == len(b)

    def with_payload(self, payload, fee_bump=FEE_BUMP):
        outputs = [list(o) for o in self.outputs]
        change = max(range(len(outputs)), key=lambda k: outputs[k][0])
        outputs[change][0] -= fee_bump
        body = cs(len(outputs)) + b"".join(struct.pack("<q", v) + s for v, s in outputs)
        return (self.head + body + self.locktime + cs(len(payload)) + payload).hex()


# Entries Core reads as invalid, one per way of getting there, plus valid addresses of every network.
# Each is consumed exactly as written: the "throw" ones end where Core stops reading.
JUNK = [
    ("unknown type", b"\x00"),
    ("invalid", b"\xff"),
    ("IPv4 0.0.0.0", service(1, bytes(4), 9999)),
    ("IPv4 255.255.255.255", service(1, b"\xff" * 4, 9999)),
    ("IPv4 in IPv6", service(2, bytes(10) + b"\xff\xff" + bytes([1, 2, 3, 4]), 9999)),
    ("IPv6 documentation", service(2, bytes.fromhex("20010db8") + bytes(11) + b"\x01", 9999)),
    ("Tor v2 in IPv6", service(2, bytes.fromhex("fd87d87eeb43") + bytes(range(10)), 9999)),
    ("internal in IPv6", service(2, bytes.fromhex("fd6b88c08724") + bytes(range(10)), 9999)),
    ("IPv6 unspecified", service(2, bytes(16), 9999)),
    ("Tor v2 network id", service(3, bytes(range(10)), 9999)),
    ("unknown network id", service(9, bytes(range(20)), 9999)),
    ("CJDNS outside fc00::/8", service(6, b"\xfd" + bytes(range(15)), 9999)),
    ("CJDNS", service(6, b"\xfc" + bytes(range(1, 16)), 9999)),
    ("Tor v3", service(4, bytes(range(32)), 9999)),
    ("I2P", service(5, bytes(range(32, 64)), 9999)),
    ("IPv6", service(2, bytes.fromhex("2a01") + bytes(range(14)), 9999)),
    ("IPv4 with a 5-byte length (Core throws after the length)", b"\x01\x01\x05"),
    ("non-canonical length (thrown after reading it)", b"\x01\x01\xfd\x04\x00"),
    ("513-byte address (too long)", b"\x01\x07\xfd\x01\x02"),
    ("domain in uppercase", domain(b"Dup.Dash.org", 443)),
    ("domain with port 0", domain(b"dup.dash.org", 0)),
    ("domain with an underscore", domain(b"dup_x.dash.org", 443)),
    ("domain not UTF-8", domain(b"d\xffp.dash.org", 443)),
    ("domain", domain(b"dup.dash.org", 443)),
    ("253-byte domain", domain(b".".join([b"a" * 63, b"b" * 63, b"c" * 63, b"d" * 61]), 443)),
]
JUNK_ENTRIES = [entry for _, entry in JUNK]
# An unknown network's 511-byte address: fewer bytes are left in any of these payloads, so Core's
# DataStream::ignore throws without consuming them and the payload goes on from there.
SHORT_CLAIM = b"\x01\x07\xfd\xff\x01"


def main():
    bindir, src, work, out = sys.argv[1:5]
    os.makedirs(work, exist_ok=True)
    # The functional tests' rpcdoccheck rejects Core's JSON for an unknown purpose, a key "".
    rpc = start_node(bindir, src, os.path.join(work, "node0"), 31570, ["-rpcdoccheck=0"])
    version = dashd_version(bindir)
    cases, mined = [], []

    def fund(amount):
        addr = rpc("getnewaddress")
        txid = rpc("sendtoaddress", addr, amount)
        return addr, txid

    # The miner holds back transactions without an InstantSend lock for 10 minutes, so move the
    # mock clock past that before every block.
    clock = [int(time.time())]
    rpc("setmocktime", clock[0])

    def mine():
        clock[0] += 11 * 60
        rpc("setmocktime", clock[0])
        mempool = rpc("getrawmempool")
        block = rpc("generatetoaddress", 1, rpc("getnewaddress"))[0]
        assert set(mempool) <= set(rpc("getblock", block, 1)["tx"]), "mempool not mined"
        return block

    def record(name, desc, hex_, original, outcome, extra=None):
        signed = rpc("signrawtransactionwithwallet", hex_)
        assert signed["complete"], signed
        hex_ = signed["hex"]
        verdict = rpc("testmempoolaccept", [hex_])[0]
        decoded = rpc("decoderawtransaction", hex_)
        assert verdict["allowed"] == (outcome == "mined"), (name, verdict)
        payload_key = "proRegTx" if decoded["type"] == 1 else "proUpServTx"
        assert (payload_key in decoded) == (outcome != "unparsable"), (name, decoded.keys())
        case = {"name": name, "description": desc, "outcome": outcome, "hex": hex_, "txid": decoded["txid"],
                "testmempoolaccept": verdict, "core": decoded, "core_original_hex": original}
        case.update(extra or {})
        cases.append(case)
        if outcome == "mined":
            rpc("sendrawtransaction", hex_)
            mined.append(case)
        print(f"{name}: {outcome} {verdict.get('reject-reason', '')}", file=sys.stderr)
        return case

    # Collaterals and separate fee sources, so prepared transactions never share inputs.
    coll_regular, coll_regular_tx = fund(1000)
    coll_regular2, coll_regular2_tx = fund(1000)
    coll_evo, coll_evo_tx = fund(4000)
    fee_sources = [fund(10)[0] for _ in range(5)]
    mine()

    def vout_of(txid, amount):
        return [o["n"] for o in rpc("getrawtransaction", txid, True)["vout"] if o["value"] == amount][0]

    def keys():
        bls = rpc("bls", "generate")
        return {"owner": rpc("getnewaddress"), "voting": rpc("getnewaddress"), "payout": rpc("getnewaddress"),
                "operator_secret": bls["secret"], "operator_public": bls["public"]}

    def sign_registration(prepared, payload_without_sig):
        sig = base64.b64decode(rpc("signmessage", prepared["collateralAddress"], prepared["signMessage"]))
        return payload_without_sig + cs(len(sig)) + sig

    def registration(prep):
        """Core's prepared ProRegTx: its netInfo lists, a builder of variants with another netInfo
        (and bytes after the signature), the original signed, and what its signature is checked
        against."""
        tx = Tx(prep["tx"])
        p = tx.payload
        assert p[:2] == b"\x03\x00" and p[-1] == 0, p.hex()  # version 3, empty collateral signature
        lists, end = parse_net_info(p, 42)  # after version, type, mode and the collateral outpoint

        def variant(ni, trailing=b""):
            return tx.with_payload(sign_registration(prep, p[:42] + ni + p[end:-1]) + trailing)

        extra = {"sign_message": prep["signMessage"], "collateral_address": prep["collateralAddress"]}
        return lists, variant, tx.with_payload(sign_registration(prep, p[:-1]), 0), extra

    def update(hex_):
        """Core's prepared ProUpServTx: its netInfo lists and a builder of variants with another one."""
        tx = Tx(hex_)
        p = tx.payload
        assert p[:2] == b"\x03\x00", p.hex()
        lists, end = parse_net_info(p, 36)  # after version, type and proTxHash
        return lists, lambda ni: tx.with_payload(p[:36] + ni + p[end:])

    platform_node_id = hashlib.sha1(b"rust-dashcore netinfo vectors").hexdigest()
    regular, regular2, evo = keys(), keys(), keys()

    # --- ProRegTx (regular) -------------------------------------------------------------------
    prep = rpc("protx", "register_prepare", coll_regular_tx, vout_of(coll_regular_tx, 1000), "127.0.0.1:30001",
               regular["owner"], regular["operator_public"], regular["voting"], 0, regular["payout"],
               fee_sources[0])
    lists, reg_variant, original, reg_extra = registration(prep)
    (core_p2p, core_entries), = lists

    # Not extractable first: they share inputs with the registration that gets mined.
    record("proreg_domain_too_long", "the CORE_P2P list ends with a 254-byte domain (LIMITED_STRING 253)",
           reg_variant(net_info([(0, core_entries + [domain(b"a" * 250 + b".org", 443)])])), original,
           "unparsable", reg_extra)
    record("proreg_trailing_byte_after_short_claim",
           "proreg_short_claim with one byte after the signature: Core leaves it unread",
           reg_variant(net_info([(0, core_entries + [SHORT_CLAIM])]), b"\x00"), original, "unparsable",
           reg_extra)
    record("proreg_invalid_entries_kept", "the only CORE_P2P list holds entries Core reads as invalid",
           reg_variant(net_info([(0, core_entries + [b"\x00", service(1, bytes(4), 9999),
                                                     domain(b"Dup.Dash.org", 443)])])),
           original, "rejected", reg_extra)
    record("proreg_every_entry_kind_kept",
           "the only CORE_P2P list holds the address and every junk entry: Core decodes and shows each "
           "(an address, or [invalid entry]), and validation refuses more than four entries",
           reg_variant(net_info([(0, core_entries + JUNK_ENTRIES)])), original, "rejected",
           {**reg_extra, "junk": [desc for desc, _ in JUNK]})
    record("proreg_unknown_purpose", "a list for purpose 7, which Core cannot read",
           reg_variant(net_info([(0, core_entries), (7, [service(1, bytes([127, 0, 0, 1]), 30009)])])),
           original, "unparsable", reg_extra)
    record("proreg_repeated_purpose_junk",
           "CORE_P2P repeated with entries of every kind Core reads as invalid, and valid ones of every network",
           reg_variant(net_info([(0, core_entries), (0, JUNK_ENTRIES)])), original, "unparsable",
           {**reg_extra, "junk": [desc for desc, _ in JUNK]})
    regular_case = record("proreg_core", "Core's own payload", original, original, "mined", reg_extra)

    prep2 = rpc("protx", "register_prepare", coll_regular2_tx, vout_of(coll_regular2_tx, 1000), "127.0.0.1:30002",
                regular2["owner"], regular2["operator_public"], regular2["voting"], 0, regular2["payout"],
                fee_sources[1])
    lists2, variant2, original2, extra2 = registration(prep2)
    (_, core_entries2), = lists2
    record("proreg_short_claim",
           "the CORE_P2P list ends with an entry claiming a 511-byte address of an unknown network; fewer bytes "
           "are left, so Core consumes none of them and reads the rest of the payload from there",
           variant2(net_info([(0, core_entries2 + [SHORT_CLAIM])])), original2, "rejected", extra2)

    # --- ProRegTx (Evo) -------------------------------------------------------------------------
    prep3 = rpc("protx", "register_prepare_evo", coll_evo_tx, vout_of(coll_evo_tx, 4000), "127.0.0.1:30003",
                evo["owner"], evo["operator_public"], evo["voting"], 0, evo["payout"], platform_node_id,
                "127.0.0.1:30004", "127.0.0.1:30005", fee_sources[2])
    lists3, variant3, original3, extra3 = registration(prep3)
    assert [purpose for purpose, _ in lists3] == [0, 1, 2], lists3
    reordered = [lists3[2], lists3[1], lists3[0], (1, [b"\x00", SHORT_CLAIM])]
    record("proreg_evo_purposes_out_of_order",
           "the Evo node's purposes as platform_https, platform_p2p, core_p2p, then platform_p2p repeated",
           variant3(net_info(reordered)), original3, "unparsable", extra3)
    evo_case = record("proreg_evo_core", "Core's own Evo payload", original3, original3, "mined", extra3)

    first_block = mine()
    regular_protx = regular_case["txid"]
    evo_protx = evo_case["txid"]

    # --- ProUpServTx (regular) ------------------------------------------------------------------
    upd = rpc("protx", "update_service", regular_protx, "127.0.0.1:30011", regular["operator_secret"], "",
              fee_sources[3], False)
    lists4, upd_variant = update(upd)
    (_, upd_entries), = lists4
    upd_extra = {"operator_public_key": regular["operator_public"]}

    record("proupserv_domain_truncated",
           "the CORE_P2P list ends with a domain claiming 200 bytes, more than are left",
           upd_variant(net_info([(0, upd_entries + [b"\x02\xc8"])])), upd, "unparsable", upd_extra)
    record("proupserv_invalid_first_entry", "the CORE_P2P list starts with an unknown network's address",
           upd_variant(net_info([(0, [service(9, bytes(20), 9999)] + upd_entries)])), upd, "rejected", upd_extra)
    record("proupserv_repeated_purpose_junk_short_claim",
           "CORE_P2P repeated twice: with the junk entries, then with the short claim",
           upd_variant(net_info([(0, upd_entries), (0, JUNK_ENTRIES), (0, [SHORT_CLAIM])])), upd, "unparsable",
           upd_extra)
    record("proupserv_core", "Core's own payload", upd, upd, "mined", upd_extra)

    # --- ProUpServTx (Evo) ----------------------------------------------------------------------
    upd5 = rpc("protx", "update_service_evo", evo_protx, "127.0.0.1:30013", evo["operator_secret"],
               platform_node_id, "127.0.0.1:30014", "127.0.0.1:30015", "", fee_sources[4], False)
    lists5, variant5 = update(upd5)
    assert [purpose for purpose, _ in lists5] == [0, 1, 2], lists5
    record("proupserv_evo_purposes_out_of_order",
           "the Evo node's purposes as platform_p2p, platform_https, core_p2p, then core_p2p repeated",
           variant5(net_info([lists5[1], lists5[2], lists5[0], (0, [b"\xff"])])),
           upd5, "unparsable", {"operator_public_key": evo["operator_public"]})
    record("proupserv_evo_core", "Core's own Evo payload", upd5, upd5, "mined",
           {"operator_public_key": evo["operator_public"]})

    second_block = mine()

    blocks = []
    for bh in (first_block, second_block):
        block = rpc("getblock", bh, 1)
        blocks.append({"hash": bh, "height": block["height"], "merkleroot": block["merkleroot"],
                       "txids": block["tx"], "hex": rpc("getblock", bh, 0)})
    for case in mined:
        verbose = rpc("getrawtransaction", case["txid"], True)
        assert verbose["hex"] == case["hex"] and verbose["blockhash"] in (first_block, second_block), case["name"]
        case["core"] = verbose
        case["block_hash"] = verbose["blockhash"]
    for case in cases:
        case["core"].pop("confirmations", None)

    rpc("stop")
    json.dump({"generator": f"{version} (`dashd -version`) on regtest: {os.path.basename(__file__)} on a copy of "
                            f"{os.path.relpath(src, os.path.dirname(os.path.dirname(src)))}",
               "cases": cases, "blocks": blocks}, open(out, "w"), indent=1)
    print(f"{len(cases)} cases, {len(mined)} mined in {len(blocks)} blocks", file=sys.stderr)


if __name__ == "__main__":
    main()
