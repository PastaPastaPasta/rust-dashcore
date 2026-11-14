#!/usr/bin/env python3
"""Render -capturemessages directories as a base-sdk corpus."""
import argparse
import base64
from collections import Counter, defaultdict
import contextlib
from datetime import datetime, timedelta, timezone
import io
import ipaddress
import json
from pathlib import Path
import re
import struct
import sys
import tempfile

from p2p import read_capture

INIT_VERSION = 209
SIZE_CAP = 131072
MAPPED = b"\0" * 10 + b"\xff\xff"
ADDRESS_LENGTHS = {1: 4, 2: 16, 4: 32, 5: 32, 6: 16}
PRIVATE_COMMANDS = {"mnlistdiff", "qrinfo"}


class Reader:
    def __init__(self, raw):
        self.raw, self.pos = raw, 0

    def take(self, n):
        end = self.pos + n
        if end > len(self.raw):
            raise ValueError("truncated payload")
        out = self.raw[self.pos:end]
        self.pos = end
        return out

    def compact(self):
        tag = self.take(1)[0]
        if tag < 253:
            return tag
        size = {253: 2, 254: 4, 255: 8}[tag]
        n = int.from_bytes(self.take(size), "little")
        if n < {253: 253, 254: 65536, 255: 4294967296}[tag]:
            raise ValueError("non-canonical CompactSize")
        return n

    def finish(self):
        if self.pos != len(self.raw):
            raise ValueError("trailing address payload bytes")


def address_fields(command, raw):
    r, fields, supported = Reader(raw), [], True

    def field(kind, size):
        start = r.pos
        address = r.take(size)
        fields.append((start, kind, address))

    if command == "version":
        r.take(28)
        field(2, 16)
        r.take(2)
        if r.pos < len(raw):
            r.take(8)
            field(2, 16)
            r.take(10)
    elif command == "addr":
        for _ in range(r.compact()):
            r.take(12)
            field(2, 16)
            r.take(2)
        r.finish()
    elif command == "addrv2":
        for _ in range(r.compact()):
            r.take(4)
            r.compact()
            kind = r.take(1)[0]
            size = r.compact()
            field(kind, size)
            r.take(2)
            if kind not in ADDRESS_LENGTHS:
                supported = False
            elif size != ADDRESS_LENGTHS[kind]:
                raise ValueError("invalid addrv2 address length")
            elif kind == 6 and fields[-1][2][0] != 0xfc:
                raise ValueError("invalid CJDNS prefix")
        r.finish()
    return fields, supported


def canonical(kind, address):
    if kind == 2 and address.startswith(MAPPED):
        return 1, address[12:]
    return kind, address


def unspecified(kind, address):
    kind, address = canonical(kind, address)
    return kind in (1, 2) and not any(address)


class Addresses:
    def __init__(self):
        self.originals = set()
        self.replacements = {}
        self.counters = Counter()

    def observe(self, kind, address):
        if unspecified(kind, address):
            return
        kind, address = canonical(kind, address)
        if address:
            if kind == 1:
                self.originals.update((MAPPED + address, b"\x01\x04" + address))
            else:
                self.originals.add(address)
        if kind in ADDRESS_LENGTHS and len(address) == ADDRESS_LENGTHS[kind]:
            self.replace(kind, address)

    def replace(self, kind, address):
        key = canonical(kind, address)
        if key not in self.replacements:
            family = key[0]
            self.counters[family] += 1
            n = self.counters[family]
            if family == 1:
                prefix = (b"\xc0\x00\x02", b"\xc6\x33\x64", b"\xcb\x00\x71")
                replacement = prefix[((n - 1) // 254) % 3] + bytes([(n - 1) % 254 + 1])
            elif family == 2:
                replacement = b"\x20\x01\x0d\xb8" + n.to_bytes(12, "big")
            elif family in (4, 5):
                replacement = bytes([0xa0 + family]) * 24 + n.to_bytes(8, "big")
            elif family == 6:
                replacement = b"\xfc" + b"\xc6" * 7 + n.to_bytes(8, "big")
            else:
                raise ValueError("unsupported address family")
            self.replacements[key] = replacement
        replacement = self.replacements[key]
        return MAPPED + replacement if kind == 2 and key[0] == 1 else replacement

    def rewrite(self, command, raw):
        fields, supported = address_fields(command, raw)
        if not supported:
            return None
        out = bytearray(raw)
        for start, kind, address in fields:
            if unspecified(kind, address):
                continue
            replacement = self.replace(kind, address)
            if len(replacement) != len(address):
                raise ValueError("address rewrite changed length")
            out[start:start + len(address)] = replacement
        return bytes(out)

    def check(self, payloads):
        for raw in payloads:
            if any(address in raw for address in self.originals):
                raise ValueError("address leak detected")


def peer_address(name):
    host, separator, port = name.rpartition("_")
    if not separator or not port.isdecimal() or not 0 <= int(port) <= 65535:
        raise ValueError("invalid capture peer directory")
    host = host.strip("[]").replace("_", ":")
    if host.endswith(".onion"):
        encoded, kind = host[:-6], 4
    elif host.endswith(".b32.i2p"):
        encoded, kind = host[:-8], 5
    else:
        ip = ipaddress.ip_address(host)
        return (1 if ip.version == 4 else 2), ip.packed
    raw = base64.b32decode(encoded.upper() + "=" * (-len(encoded) % 8))
    if kind == 4 and len(raw) == 35 and raw[-1] == 3:
        return kind, raw[:32]
    if kind == 5 and len(raw) == 32:
        return kind, raw
    raise ValueError("unsupported capture peer directory")


def capture(path):
    if not path.exists():
        return []
    rows = read_capture(path)
    with path.open("rb") as f:
        for _, _, raw in rows:
            header = f.read(24)
            if len(header) != 24 or struct.unpack_from("<I", header, 20)[0] != len(raw):
                raise ValueError("truncated capture record")
            f.seek(len(raw), 1)
        if f.read(1):
            raise ValueError("truncated capture record")
    return rows


def version_info(raw):
    address_fields("version", raw)
    version, = struct.unpack_from("<i", raw)
    agent = None
    if len(raw) > 80:
        r = Reader(raw)
        r.take(80)
        agent = r.take(r.compact())
    return version, agent


def select(payloads, cap):
    ordered = sorted((raw for raw in set(payloads) if len(raw) <= cap), key=lambda raw: (len(raw), raw))
    if not ordered:
        return []
    chosen = {ordered[0], ordered[len(ordered) // 2], ordered[-1]}
    return sorted(chosen, key=lambda raw: (len(raw), raw))


def render(inputs, cap):
    addresses, counts, candidates = Addresses(), Counter(), defaultdict(set)
    times, agents, stats = [], set(), Counter()
    for network, root in sorted(inputs, key=lambda item: (item[0], str(item[1]))):
        if not re.fullmatch(r"[A-Za-z0-9_-]+", network) or not root.is_dir():
            raise ValueError("invalid network capture directory")
        for peer in sorted(root.iterdir()):
            if not peer.is_dir():
                continue
            addresses.observe(*peer_address(peer.name))
            streams = {direction: capture(peer / ("msgs_%s.dat" % direction))
                       for direction in ("recv", "sent")}
            rows = sorted((timestamp, direction, index, command, raw)
                          for direction, stream in streams.items()
                          for index, (timestamp, command, raw) in enumerate(stream))
            for timestamp, _, _, command, raw in rows:
                times.append(timestamp)
                fields, _ = address_fields(command, raw)
                for _, kind, address in fields:
                    addresses.observe(kind, address)
            common = ours = None
            for _, direction, _, command, raw in rows:
                if direction == "sent" and command == "version":
                    ours, agent = version_info(raw)
                    if agent is not None:
                        agents.add(agent)
                    common, handshake = INIT_VERSION, False
                    stats["connections"] += 1
                if common is None:
                    raise ValueError("capture does not start with our version")
                key = network, direction, command, common
                counts[key] += 1
                if command in PRIVATE_COMMANDS:
                    stats["private_messages"] += 1
                else:
                    candidates[key].add(raw)
                if direction == "recv" and command == "version" and not handshake:
                    common, handshake = min(version_info(raw)[0], ours), True
    messages = []
    for key in sorted(candidates):
        stats["over_cap_payloads"] += sum(len(raw) > cap for raw in candidates[key])
        for raw in select(candidates[key], cap):
            rewritten = addresses.rewrite(key[2], raw)
            if rewritten is None:
                stats["unsupported_addrv2_payloads"] += 1
                continue
            messages.append(dict(zip(("network", "dir", "command", "version"), key), raw=rewritten.hex()))
    addresses.check(bytes.fromhex(row["raw"]) for row in messages)
    seen = [dict(zip(("network", "dir", "command", "version"), key), count=counts[key])
            for key in sorted(counts)]
    epoch = datetime(1970, 1, 1, tzinfo=timezone.utc)
    window = "empty"
    if times:
        window = "%s to %s" % tuple((epoch + timedelta(microseconds=t)).isoformat()
                                    for t in (min(times), max(times)))
    header = [
        "Live Dash Core -capturemessages corpus.",
        "dashd sent-version user agents: " + json.dumps(sorted(a.decode("ascii", errors="backslashreplace")
                                                              for a in agents), ensure_ascii=True),
        "Networks: " + ", ".join(sorted({net for net, _ in inputs})),
        "Capture window (UTC): " + window,
        "Stream versions replay each connection: 209 until the peer's first",
        "version has been read, then the lower of its version and ours.",
        "Address rewrite: sequential first appearances; IPv4 documentation /24s",
        "(hosts 1..254, wrapping), IPv6 2001:db8::/32; mapped IPv4 shares its",
        "replacement; TorV3/I2P/CJDNS use counter patterns (CJDNS starts fc).",
        "Ports, unspecified (all-zero) addresses and payload lengths are unchanged;",
        "mnlistdiff/qrinfo are omitted.",
        "Selection per (network, dir, command, version): distinct payloads of at",
        "most %d bytes sorted by (size, bytes); smallest, upper middle, largest." % cap,
        "Distinct payloads over the size cap, not kept: %d." % stats["over_cap_payloads"],
        "Connections, each from the version we sent: %d." % stats["connections"],
        "Count-only mnlistdiff/qrinfo messages: %d." % stats["private_messages"],
        "Dropped selected addrv2 payloads with unknown network ids: %d." % stats["unsupported_addrv2_payloads"],
    ]
    return "".join("// %s\n" % line for line in header) + json.dumps({"seen": seen, "messages": messages}, indent=1) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("captures", nargs="*", metavar="NETWORK=DIR")
    parser.add_argument("--size-cap", type=int, default=SIZE_CAP)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.selftest:
            selftest()
            return
        if not args.captures or args.size_cap < 0:
            raise ValueError("provide NETWORK=DIR and a non-negative size cap")
        inputs = []
        for value in args.captures:
            network, separator, directory = value.partition("=")
            if not separator or not directory:
                raise ValueError("expected NETWORK=DIR")
            inputs.append((network, Path(directory)))
        output = render(inputs, args.size_cap)
    except (ValueError, OSError, struct.error, UnicodeError, OverflowError):
        sys.exit("FAIL: invalid capture input or address leak; no corpus written")
    sys.stdout.write(output)


def compact(n):
    if n < 253:
        return bytes([n])
    tag, size = (253, 2) if n <= 65535 else ((254, 4) if n <= 4294967295 else (255, 8))
    return bytes([tag]) + n.to_bytes(size, "little")


def synthetic(root):
    def mapped(text):
        return MAPPED + ipaddress.IPv4Address(text).packed

    def version(number, recv, sent):
        agent = b"/Dash Core:24.0.0-rc.3/"
        addr = lambda raw: struct.pack("<Q", 1) + raw + struct.pack(">H", 9999)
        return (struct.pack("<iQq", number, 1, 1700000000) + addr(recv) + addr(sent)
                + struct.pack("<Q", 42) + compact(len(agent)) + agent
                + struct.pack("<iB", 1, 1) + b"\x8a" * 32 + b"\0")

    def v2(entries):
        return compact(len(entries)) + b"".join(
            struct.pack("<I", 1700000000) + compact(1024) + bytes([kind])
            + compact(len(raw)) + raw + struct.pack(">H", 19999) for kind, raw in entries)

    def write(peer, events):
        peer.mkdir(parents=True, exist_ok=True)
        for direction in ("recv", "sent"):
            with (peer / ("msgs_%s.dat" % direction)).open("wb") as f:
                for i, (way, command, raw) in enumerate(events):
                    if way == direction:
                        f.write(struct.pack("<Q", 1700000000000000 + i * 1000000))
                        f.write(command.encode().ljust(12, b"\0"))
                        f.write(struct.pack("<I", len(raw)))
                        f.write(raw)

    ipv4 = ipaddress.IPv4Address("8.9.10.11").packed
    ipv6 = ipaddress.IPv6Address("2606:4700::1234").packed
    entries = [(1, ipv4), (2, ipv6), (4, b"\x74" * 32), (5, b"\x69" * 32),
               (6, b"\xfc" + b"\x63" * 15)]
    addr = compact(2) + b"".join(struct.pack("<IQ", 1700000000, 1) + raw + struct.pack(">H", 9999)
                                   for raw in (MAPPED + ipv4, ipv6))
    for net in ("testnet", "mainnet"):
        for name, remote, ours in (("8.9.10.11_9999", 70240, 70242),
                                    ("[2606_4700__1234]_19999", 70242, 70241)):
            hello = ("sent", "version", version(ours, MAPPED + ipv4, mapped("9.10.11.12")))
            events = [hello, ("recv", "ping", struct.pack("<Q", 100)), ("sent", "ping", struct.pack("<Q", 101)),
                      ("recv", "version", version(remote, mapped("9.10.11.12"), MAPPED + ipv4)),
                      ("recv", "verack", b""), ("recv", "addr", addr), ("recv", "addrv2", v2(entries))]
            events += [("recv", "ping", struct.pack("<Q", i)) for i in range(7)]
            events += [("recv", "filteradd", compact(n) + b"\x31" * n) for n in (1, 5, 9, 17, 33)]
            events += [("recv", "version", version(70243, mapped("9.10.11.12"), b"\0" * 16)),
                       ("recv", "notfound", compact(1) + struct.pack("<I", 1) + b"\0" * 32),
                       ("recv", "filteradd", compact(5) + b"\x31" * 5),
                       ("recv", "mnlistdiff", b"\x79"), ("recv", "qrinfo", b"\x79"),
                       ("recv", "addrv2", v2([(1, b"\x0c\x0d\x0e\x0f"), (99, b"\x75" * 7),
                                              (2, ipaddress.IPv6Address("2606:4700::dead").packed)])),
                       ("sent", "verack", b""), ("sent", "ping", struct.pack("<Q", 42))]
            if remote == 70240:
                events += [hello, ("recv", "version", version(70240, mapped("9.10.11.12"), MAPPED + ipv4)),
                           ("recv", "ping", struct.pack("<Q", 7)), ("recv", "filteradd", compact(40) + b"\x31" * 40)]
            write(root / net / name, events)
    return [(net, root / net) for net in ("mainnet", "testnet")]


def selftest():
    with tempfile.TemporaryDirectory(prefix="capture-corpus-") as directory:
        root = Path(directory)
        inputs = synthetic(root)
        output = render(inputs, 200)
        assert output == render(list(reversed(inputs)), 200)
        corpus = json.loads("\n".join(line for line in output.splitlines() if not line.startswith("//")))
        seen, messages = corpus["seen"], corpus["messages"]
        def count(direction, command, version):
            return [row["count"] for row in seen
                    if (row["dir"], row["command"], row["version"]) == (direction, command, version)]

        assert {row["version"] for row in seen} == {209, 70240, 70241}
        expected = {("sent", "version", 209): 3, ("recv", "version", 209): 3, ("recv", "version", 70240): 1,
                    ("recv", "version", 70241): 1, ("recv", "ping", 209): 2, ("sent", "ping", 209): 2,
                    ("recv", "ping", 70240): 8, ("recv", "ping", 70241): 7, ("sent", "ping", 70240): 1,
                    ("sent", "ping", 70241): 1, ("recv", "filteradd", 70240): 7, ("recv", "filteradd", 70241): 6}
        for key, n in expected.items():
            assert count(*key) == [n, n], key
        for net in ("mainnet", "testnet"):
            merged = sorted(len(row["raw"]) // 2 for row in messages
                            if (row["network"], row["dir"], row["command"], row["version"])
                            == (net, "recv", "filteradd", 70240))
            assert merged == [2, 18, 41], merged
        assert "Connections, each from the version we sent: 6." in output
        assert {row["dir"] for row in messages} == {"recv", "sent"}
        assert {row["network"] for row in messages} == {"mainnet", "testnet"}
        assert not any(row["command"] in PRIVATE_COMMANDS for row in messages)
        assert any(row["command"] == "notfound" for row in messages)
        assert any(unspecified(kind, address) for row in messages if row["command"] == "version"
                   for _, kind, address in address_fields("version", bytes.fromhex(row["raw"]))[0])
        assert "Dropped selected addrv2 payloads with unknown network ids: 4." in output
        assert '"/Dash Core:24.0.0-rc.3/"' in output
        assert "2023-11-14T22:13:20+00:00 to 2023-11-14T22:13:50+00:00" in output
        filters = [bytes.fromhex(row["raw"]) for row in messages if row["command"] == "filteradd"]
        assert {len(raw) for raw in filters} == {2, 10, 18, 34, 41}
        capped = render(inputs, 20)
        capped_rows = json.loads("\n".join(line for line in capped.splitlines() if not line.startswith("//")))
        assert capped_rows["seen"] == seen
        capped_filters = [bytes.fromhex(row["raw"]) for row in capped_rows["messages"]
                          if row["command"] == "filteradd"]
        assert {len(raw) for raw in capped_filters} == {2, 10, 18}
        assert all(len(row["raw"]) <= 40 for row in capped_rows["messages"])
        assert "Distinct payloads over the size cap, not kept: 0." in output
        assert "Distinct payloads over the size cap, not kept: 0." not in capped
        assert select([b"b", b"a", b"c", b"a"], 1) == [b"a", b"b", b"c"]
        assert select([b"a", b"bb", b"ccc", b"dddd", b"eeeee"], 2) == [b"a", b"bb"]
        assert select([b"a", b"bb", b"ccc", b"dddd", b"eeeee"], 4) == [b"a", b"ccc", b"dddd"]
        assert not select([b"aa", b"bbb"], 1)
        assert not select([], 1)
        originals = Addresses()
        for _, netroot in inputs:
            for peer in sorted(netroot.iterdir()):
                originals.observe(*peer_address(peer.name))
                for direction in ("recv", "sent"):
                    for _, command, raw in capture(peer / ("msgs_%s.dat" % direction)):
                        for _, kind, address in address_fields(command, raw)[0]:
                            originals.observe(kind, address)
        originals.check(bytes.fromhex(row["raw"]) for row in messages)
        for row in messages:
            if row["command"] not in ("version", "addr", "addrv2"):
                continue
            raw = bytes.fromhex(row["raw"])
            fields, supported = address_fields(row["command"], raw)
            assert supported
            for _, kind, address in fields:
                family, addr = canonical(kind, address)
                if unspecified(kind, address):
                    assert row["command"] == "version"
                elif family == 1:
                    assert addr[:3] in (b"\xc0\0\x02", b"\xc6\x33\x64", b"\xcb\0\x71")
                elif family == 2:
                    assert addr[:4] == b"\x20\x01\x0d\xb8"
                elif kind == 6:
                    assert address[0] == 0xfc
            original_payloads = [payload for _, netroot in inputs for peer in netroot.iterdir()
                                 for _, command, payload in capture(peer / ("msgs_%s.dat" % row["dir"]))
                                 if command == row["command"]]
            assert any(len(original) == len(raw) and all(
                original[i] == raw[i] for i in range(len(raw))
                if not any(start <= i < start + len(address) for start, _, address in fields))
                       for original in original_payloads)
        replacements = Addresses()
        ip = b"\x08\x09\x0a\x0b"
        assert replacements.replace(2, MAPPED + ip) == MAPPED + replacements.replace(1, ip)
        replacements.observe(2, bytes.fromhex("26064700000000000000000000005678"))
        assert replacements.replace(1, b"\x09\x0a\x0b\x0c") == b"\xc0\0\x02\x02"
        replacements = Addresses()
        for n in range(1, 764):
            got = replacements.replace(1, n.to_bytes(4, "big"))
            assert got[-1] == (n - 1) % 254 + 1
            assert got[:3] == (b"\xc0\0\x02", b"\xc6\x33\x64", b"\xcb\0\x71")[((n - 1) // 254) % 3]
        for leak in (MAPPED + ip, b"\x01\x04" + ip):
            try:
                originals.check([b"prefix" + leak + b"suffix"])
            except ValueError:
                pass
            else:
                raise AssertionError("planted leak was not caught")
        for kind, size in ADDRESS_LENGTHS.items():
            source = bytes([0xfc if kind == 6 else 0x77]) * size
            rewritten = replacements.rewrite("addrv2", compact(1) + struct.pack("<I", 1)
                                             + compact(65536) + bytes([kind]) + compact(size)
                                             + source + struct.pack(">H", 9999))
            fields, supported = address_fields("addrv2", rewritten)
            assert supported and fields[0][2] != source
        for size in (1, 23, 24, 25):
            truncated = root / "truncated.dat"
            truncated.write_bytes((struct.pack("<Q12sI", 1, b"ping", 8) + b"x" * 8)[:size])
            try:
                capture(truncated)
            except (ValueError, struct.error):
                pass
            else:
                raise AssertionError("accepted a truncated record")
        inbound = inputs[0][1] / "12.13.14.15_9999"
        inbound.mkdir()
        (inbound / "msgs_recv.dat").write_bytes(struct.pack("<Q12sI", 1, b"ping", 8) + b"\0" * 8)
        try:
            render(inputs, 20)
        except ValueError:
            pass
        else:
            raise AssertionError("accepted a capture that does not start with our version")
        (inbound / "msgs_recv.dat").unlink()
        inbound.rmdir()
        empty = inputs[0][1] / "11.12.13.14_9999"
        empty.mkdir()
        (empty / "msgs_recv.dat").write_bytes(b"")
        assert "Connections, each from the version we sent: 6." in render(inputs, 20)
        peer = inputs[0][1] / "8.9.10.11_9999"
        path = peer / "msgs_sent.dat"
        clean = path.read_bytes()
        leaks = [MAPPED + ip, b"\x01\x04" + ip, b"\x01\x04\x0b\x0c\x0d\x0e",
                 b"\x01\x04\x0c\x0d\x0e\x0f", ipaddress.IPv6Address("2606:4700::dead").packed,
                 ipaddress.IPv6Address("2606:4700::1234").packed, b"\x74" * 32, b"\x69" * 32,
                 b"\xfc" + b"\x63" * 15]
        for leak in leaks:
            path.write_bytes(clean + struct.pack("<Q12sI", 1700000100000000, b"filteradd", len(leak)) + leak)
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                try:
                    main(["%s=%s" % item for item in inputs])
                except SystemExit as e:
                    assert e.code == "FAIL: invalid capture input or address leak; no corpus written"
                else:
                    raise AssertionError("CLI accepted a planted leak")
            assert not out.getvalue()
            assert err.getvalue() == ""
        path.write_bytes(clean)
    print("ok  capture corpus: versions, selection, rewrites, leak controls", file=sys.stderr)


if __name__ == "__main__":
    main()
