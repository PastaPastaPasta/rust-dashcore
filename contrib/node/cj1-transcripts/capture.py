#!/usr/bin/env python3
"""Record CJ1 pre-V24 CoinJoin transcripts from release Dash Core v24.0.0."""
import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import random
import re
import select
import shutil
import socket
import struct
import subprocess
import sys
import time
import traceback
import tempfile

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
SCRATCH = Path(os.environ.get('CJ1_SCRATCH', '/work/scratch/cj1/impl-transcripts'))
FIXTURES = REPO / 'dashd-coinjoin/tests/fixtures/dashd-v24'
os.environ.setdefault('P2A_CAPTURE_DIR', str(SCRATCH))
spec = importlib.util.spec_from_file_location('p2b', HERE.parent / 'p2b-transcripts/capture.py')
p2b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p2b)
p2a = p2b.p2a
compact, sha256d = p2a.compact, p2a.sha256d
MASKS = {
    'session': 'nonzero dssu/dsf/dsc session ids mapped by first appearance; zero stays zero',
    'dsq': 'time and BLS signature omitted from comparisons; original bytes retained and signature verified',
    'dstx': 'BLS signature and sigTime omitted from comparisons; original bytes retained',
    'background': 'handshake, ping, addresses, headers, sporks, quorum traffic excluded from expect records',
    'logs': 'raw log records are evidence; compare verdict, score delta, map size and confirmed height observations',
    'collateral': 'compare charged count and eligible participant aliases; only selection among multiple offenders may vary',
    'inventory': 'compare sorted inventory tuples; observed DSQ bodies define queue aliases; collateral hashes map to participants',
}


def dump(obj):
    return json.dumps(obj, sort_keys=True, separators=(',', ':'))


def raw_manifest(lines):
    records = [line for line in lines if line.startswith(('< ', '> ', 'rpc ', 'log '))]
    return dict(raw_count=len(records), raw_sha256=hashlib.sha256('\n'.join(records).encode()).hexdigest())


def free_port():
    for _ in range(100):
        port = random.randrange(42000, 61000)
        with socket.socket() as sock:
            try:
                sock.bind(('127.0.0.1', port))
                return port
            except OSError:
                pass
    raise RuntimeError('no free high port')


def wait_for(fn, label, seconds=300):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if fn():
            return
        time.sleep(0.2)
    raise RuntimeError(f'timeout: {label}')


class Node(p2a.Node):
    def __init__(self, binary, source, name, extra, wallet=''):
        self.name, self.chain = name, 'regtest'
        self.dir = SCRATCH / f'run-{os.getpid()}' / name
        self.proc = None
        self.stderr = None
        self.offset = 0
        try:
            self.dir.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            shutil.copytree(source, self.dir)
            self.dir.chmod(0o700)
            for filename in ('peers.dat', 'anchors.dat', 'debug.log'):
                (self.dir / 'regtest' / filename).unlink(missing_ok=True)
            # The immutable image is copied, including wallets; never reuse its listeners/config.
            (self.dir / 'dash.conf').write_text('')
            self.port, self.rpc_port = free_port(), free_port()
            self.args = [str(binary), f'-datadir={self.dir}', '-regtest', '-server', '-listen=1',
                         f'-bind=127.0.0.1:{self.port}', f'-port={self.port}',
                         f'-rpcport={self.rpc_port}', '-rpcbind=127.0.0.1', '-rpcallowip=127.0.0.1',
                         '-rpcuser=p2a', '-rpcpassword=p2a', '-listenonion=0', '-discover=0',
                         '-dnsseed=0', '-fixedseeds=0', '-upnp=0', '-natpmp=0',
                         '-v2transport=0', '-debug=coinjoin', '-debug=net', '-logips=0',
                         f'-externalip=127.0.0.1:{self.port}', f'-mocktime={p2a.SIM_START}', '-printtoconsole=0', '-par=1', '-fallbackfee=0.00001', *extra]
            if wallet:
                self.args.append(f'-wallet={wallet}')
            self.stderr = open(self.dir / 'stderr.txt', 'wb')
            self.proc = subprocess.Popen(self.args, stdout=subprocess.DEVNULL, stderr=self.stderr)

            def ready():
                if self.proc.poll() is not None:
                    raise RuntimeError(f'{name} exited: {(self.dir / "stderr.txt").read_text()}')
                try:
                    self.rpc('getblockcount')
                    return True
                except (RuntimeError, OSError):
                    return False
            wait_for(ready, f'{name} startup', 900)
            self.offset = (self.dir / 'regtest/debug.log').stat().st_size
        except BaseException:
            errors = self.reap() + self.delete_datadir()
            if errors:
                print(f'{name}: startup cleanup errors: {errors}', file=sys.stderr)
            raise

    def reap(self):
        errors = []
        if self.proc is not None and self.proc.poll() is None:
            try:
                self.rpc('stop')
            except Exception as error:
                errors.append(f'stop RPC: {error}')
            try:
                self.proc.wait(timeout=600)
            except Exception:
                try:
                    self.proc.kill()
                    self.proc.wait(timeout=60)
                except Exception as error:
                    errors.append(f'kill/wait: {error}')
        if self.proc is not None and self.proc.poll() is not None:
            try:
                self.proc.wait(timeout=1)
            except Exception as error:
                errors.append(f'reap: {error}')
        if self.stderr is not None:
            try:
                self.stderr.close()
            except Exception as error:
                errors.append(f'stderr close: {error}')
            self.stderr = None
        return errors

    def delete_datadir(self):
        if self.proc is not None and self.proc.poll() is None:
            return ['datadir retained: process still running']
        if not self.dir.exists():
            return []
        try:
            shutil.rmtree(self.dir)
        except Exception as error:
            return [f'datadir delete: {error}']
        return []

    def stop(self):
        return self.reap() + self.delete_datadir()

    def logs(self):
        with (self.dir / 'regtest/debug.log').open() as f:
            f.seek(self.offset)
            lines = f.read().splitlines()
            self.offset = f.tell()
        return [l for l in lines if '[coinjoin]' in l or 'Misbehaving' in l
                or re.search(r'\b(dsa|dsi|dss|dssu|dsq|dsf|dsc|dstx)\b', l)]


class Network:
    def __init__(self, binary, image, masternodes=True, blocks_only=False):
        self.nodes = []
        self.metadata = json.loads((image / 'network.json').read_text())
        self.binary = binary
        try:
            extra = self.metadata['dashd_extra_args']
            self.controller = self.start(binary, image / self.metadata['controller']['datadir'],
                                         'controller', extra + [f'-sporkkey={self.metadata["spork_private_key"]}'] + (['-blocksonly=1'] if blocks_only else []), self.metadata['controller']['wallet'])
            self.mns = []
            if masternodes:
                for mn in self.metadata['masternodes']:
                    node = self.start(binary, image / mn['datadir'], mn['datadir'],
                                      extra + ['-disablewallet=1', '-txindex=1', f'-masternodeblsprivkey={mn["bls_private_key"]}'])
                    self.mns.append(node)
                    node.rpc('addnode', f'127.0.0.1:{self.controller.port}', 'onetry')
                for node, mn in zip(self.mns, self.metadata['masternodes']):
                    self.controller.rpc('protx', 'update_service', mn['pro_tx_hash'],
                                        f'127.0.0.1:{node.port}', mn['bls_private_key'])
            tip = self.controller.rpc('getblock', self.controller.rpc('getbestblockhash'))
            self.now = max(tip['time'] + 1, p2a.SIM_START)
            self.bump(1)
            if masternodes:
                self.mine(1)
            for _ in range(12):
                self.bump(30)
                if all(n.rpc('mnsync', 'status')['IsBlockchainSynced'] for n in self.nodes):
                    break
                time.sleep(1)
            for n in self.nodes:
                for _ in range(8):
                    if n.rpc('mnsync', 'status')['IsSynced']:
                        break
                    n.rpc('mnsync', 'next')
                assert n.rpc('mnsync', 'status')['IsBlockchainSynced']
            for n in self.mns:
                wait_for(lambda: n.rpc('masternode', 'status').get('state') == 'READY',
                         f'{n.name} active masternode', 300)
            self.info = self.controller.rpc('getblockchaininfo')
            self.info['softforks'] = self.controller.rpc('getdeploymentinfo')['deployments']
            if self.info.get('softforks', {}).get('v24', {}).get('active'):
                raise RuntimeError('V24 active: this recorder requires pre-V24')
        except BaseException:
            self.close()
            raise

    def start(self, *args):
        n = Node(*args)
        self.nodes.append(n)
        return n

    def bump(self, seconds):
        self.now += seconds
        for n in self.nodes:
            n.rpc('setmocktime', self.now)
            n.rpc('mockscheduler', seconds)

    def sync(self):
        tip = self.controller.rpc('getbestblockhash')
        wait_for(lambda: all(n.rpc('getbestblockhash') == tip for n in self.nodes), 'chain sync')

    def mine(self, count):
        self.bump(1)
        addr = self.controller.rpc('getnewaddress')
        blocks = self.controller.rpc('generatetoaddress', count, addr)
        self.sync()
        return blocks

    def close(self):
        errors = []
        for operation in ('reap', 'delete_datadir'):
            for n in reversed(self.nodes):
                try:
                    errors.extend(f'{n.name}: {error}' for error in getattr(n, operation)())
                except Exception as error:
                    errors.append(f'{n.name}: {operation}: {error}')
        parent = SCRATCH / f'run-{os.getpid()}'
        if parent.exists():
            try:
                parent.rmdir()
            except Exception as error:
                errors.append(f'run directory delete: {error}')
        self.cleanup_errors = errors
        if errors:
            print(f'cleanup errors after all nodes handled: {errors}', file=sys.stderr)


def bls():
    from py_ecc.bls import G2Basic
    return G2Basic


def sign(mn, message):
    scheme = bls()
    key = int(mn['bls_private_key'], 16)
    assert scheme.SkToPk(key).hex() == mn['bls_public_key']
    return scheme.Sign(key, message)


def dsq(mn, now, ready=False, denom=16, signature=True):
    body = struct.pack('<i', denom) + bytes.fromhex(mn['pro_tx_hash'])[::-1] + struct.pack('<q?', now, ready)
    sig = sign(mn, sha256d(body)) if signature else bytes(96)
    return body + compact(len(sig)) + sig


def dstx(tx, mn, now, signature=True):
    protx = bytes.fromhex(mn['pro_tx_hash'])[::-1]
    stamp = struct.pack('<q', now)
    sig = sign(mn, sha256d(tx + protx + stamp)) if signature else bytes(96)
    return tx + protx + compact(len(sig)) + sig + stamp


def inventory(kind, hashes):
    return compact(len(hashes)) + b''.join(struct.pack('<I', kind) + h for h in hashes)


class Reader:
    def __init__(self, raw):
        self.raw, self.pos = raw, 0

    def take(self, size):
        if size < 0 or self.pos + size > len(self.raw):
            raise ValueError('truncated payload')
        part = self.raw[self.pos:self.pos + size]
        self.pos += size
        return part

    def count(self):
        n = self.take(1)[0]
        if n < 253:
            return n
        size = {253: 2, 254: 4, 255: 8}[n]
        value = int.from_bytes(self.take(size), 'little')
        if value < {253: 253, 254: 65536, 255: 2**32}[n]:
            raise ValueError('noncanonical CompactSize')
        return value

    def varbytes(self):
        return self.take(self.count())

    def tx(self):
        start = self.pos
        self.take(4)
        for _ in range(self.count()):
            self.take(36)
            self.varbytes()
            self.take(4)
        for _ in range(self.count()):
            self.take(8)
            self.varbytes()
        self.take(4)
        return self.raw[start:self.pos]


def project(cmd, payload, sessions, queues):
    if cmd in ('dssu', 'dsf', 'dsc'):
        session, = struct.unpack('<i', payload[:4])
        label = sessions.setdefault(session, f'session-{len(sessions)}') if session else 0
        if cmd == 'dssu':
            state, status, message = struct.unpack('<iii', payload[4:])
            return dict(session=label, state=state, status=status, message=message)
        if cmd == 'dsc':
            message, = struct.unpack('<i', payload[4:])
            return dict(session=label, message=message)
        r = Reader(payload[4:])
        tx = r.tx()
        assert r.pos == len(r.raw)
        return dict(session=label, tx=tx.hex())
    if cmd == 'dsq':
        assert len(payload) == 142 and payload[45] == 96
        denom, = struct.unpack('<i', payload[:4])
        value = dict(denom=denom, protx=payload[4:36].hex(), ready=bool(payload[44]), signature='verified')
        alias = queues.get(sha256d(payload).hex())
        if alias:
            value['queue'] = alias
        return value
    if cmd == 'tx':
        reader = Reader(payload)
        tx = reader.tx()
        assert reader.pos == len(payload)
        return dict(tx=tx.hex())
    if cmd in ('inv', 'notfound'):
        r = Reader(payload)
        items = []
        for _ in range(r.count()):
            kind = int.from_bytes(r.take(4), 'little')
            h = r.take(32)
            if kind in (1, 16, 32):
                items.append([kind, queues.get(h.hex(), h.hex())])
        assert r.pos == len(payload)
        return sorted(items, key=str) or None
    if cmd == 'dstx':
        r = Reader(payload)
        tx = r.tx()
        protx, sig, stamp = r.take(32), r.varbytes(), r.take(8)
        assert len(sig) == 96 and r.pos == len(payload)
        return dict(tx=tx.hex(), protx=protx.hex())
    if cmd == 'closed':
        return True
    return None


def verify_bls(cmd, payload, masternodes, signatures=True):
    if cmd == 'dsq':
        assert len(payload) == 142 and payload[45] == 96
        protx, sig, message = payload[4:36], payload[46:], sha256d(payload[:45])
    else:
        r = Reader(payload)
        tx = r.tx()
        protx, sig, stamp = r.take(32), r.varbytes(), r.take(8)
        assert r.pos == len(payload)
        message = sha256d(tx + protx + stamp)
    mn = next(m for m in masternodes if bytes.fromhex(m['pro_tx_hash'])[::-1] == protx)
    assert len(sig) == 96
    if signatures:
        assert bls().Verify(bytes.fromhex(mn['bls_public_key']), message, sig), 'invalid BLS signature'


DSTX_VERDICTS = {
    'DSTX -- Already have ': 'already-have',
    'DSTX -- Invalid DSTX structure:': 'invalid-structure',
    "DSTX -- Can't find masternode ": 'unknown-masternode',
    'is sending too many transactions': 'mixing-limit',
    'DSTX -- CheckSignature() failed': 'bad-signature',
    'DSTX -- Got Masternode transaction': 'verified',
    'DSTX -- Masternode transaction accepted': 'accepted',
}


def dstx_verdict(line):
    return next((v for fragment, v in DSTX_VERDICTS.items() if fragment in line), None)


class Transcript(p2a.Transcript):
    def __init__(self, name, net, node, notes):
        # Preserve P2a's grammar; image facts replace hundreds of unneeded chain headers.
        self.name, self.node, self.net = name, node, net
        self.peers, self.sessions, self.queues = {}, {}, {}
        self.queue_bodies = {}
        self.requested_queues = set()
        self.collaterals = {}
        self.queue_observer = None
        self.lines = [f'# {name}: Dash Core v24.0.0 pre-V24 regtest', 'format cj1-v1',
                      f'mocktime {net.now}', 'ibd 0', 'mask ' + dump(MASKS)]
        self.lines += ['# ' + note for note in notes]
        deployments = node.rpc('getdeploymentinfo')['deployments']
        if deployments['v24']['active']:
            raise RuntimeError('refusing post-V24 fixture')
        self.fact('setup', dict(height=node.rpc('getblockcount'), tip=node.rpc('getbestblockhash'),
                               blockchain_synced=node.rpc('mnsync', 'status')['IsBlockchainSynced'],
                               softforks=deployments, masternodes=[
                                   {k: m[k] for k in ('pro_tx_hash', 'bls_public_key')}
                                   for m in net.metadata['masternodes']]))
        self.fact('commands', [[a.split('=')[0] + '=<network.json>' if a.startswith(('-masternodeblsprivkey=', '-sporkkey=')) else a for a in n.args] for n in net.nodes])
        self.fact('binary', dict(version='v24.0.0', sha256=hashlib.sha256(net.binary.read_bytes()).hexdigest()))
        self.fact('mn_list', node.rpc('protx', 'list', 'registered', True))
        self.fact('image_metadata_sha256', hashlib.sha256(dump(net.metadata).encode()).hexdigest())
        node.logs()

    def fact(self, key, value):
        self.lines.append('fact ' + dump(dict(key=key, value=value)))

    def observe(self, key, value):
        self.lines.append('observe ' + dump(dict(key=key, value=value)))
        if key == 'charged_count':
            snapshot = next(json.loads(line[4:])['result'] for line in reversed(self.lines)
                            if line.startswith('rpc ') and json.loads(line[4:])['method'] == 'getrawmempool')
            charged = sorted(self.collaterals[bytes.fromhex(txid)[::-1].hex()] for txid in snapshot
                             if bytes.fromhex(txid)[::-1].hex() in self.collaterals)
            self.lines.append('observe ' + dump(dict(key='charged_participants', value=charged)))

    def logs(self):
        for line in self.node.logs():
            self.lines.append('log ' + dump(line))
            score = re.search(r'Misbehaving:.*?\((\d+) -> (\d+)\)', line)
            size = re.search(r'mapDSTX.size\(\)=(\d+)', line)
            height = re.search(r'UpdateDSTXConfirmedHeight.*nHeight=(-?\d+)', line)
            verdict = dstx_verdict(line)
            if verdict:
                self.observe('dstx_verdict', verdict)
            if score:
                self.observe('score_delta', int(score[2]) - int(score[1]))
            if size:
                self.observe('dstx_map_size', int(size[1]))
            if height:
                self.observe('dstx_confirmed_height', int(height[1]))

    def settle(self):
        # P2b's quiet condition, then the same ping barrier sent to all peers together.
        while True:
            live = [p for p in self.peers.values() if not p.closed]
            ready, _, _ = select.select([p.sock for p in live], [], [], p2a.QUIET) if live else ([], [], [])
            if not ready:
                break
            for p in live:
                if p.sock in ready:
                    self._record(p, p.poll(0.01))
        pending = {p.n: p for p in live if p.handshaken}
        for p in pending.values():
            p.send('ping', p.sync_pong)
        end = time.monotonic() + 300
        while pending:
            if time.monotonic() > end:
                raise RuntimeError(f'{self.name}: ping barrier timeout: {list(pending)}')
            ready, _, _ = select.select([p.sock for p in pending.values()], [], [], 0.5)
            for p in list(pending.values()):
                if p.sock in ready:
                    msgs = p.poll(0.01)
                    self._record(p, msgs)
                    if p.closed or ('pong', p.sync_pong) in msgs:
                        del pending[p.n]
        self.logs()

    def _record(self, peer, msgs):
        super()._record(peer, msgs)
        for cmd, payload in msgs:
            if cmd == 'inv':
                reader = Reader(payload)
                for _ in range(reader.count()):
                    kind = int.from_bytes(reader.take(4), 'little')
                    inv_hash = reader.take(32)
                    if kind == 32:
                        if self.queue_observer is not None and inv_hash not in self.requested_queues:
                            self.requested_queues.add(inv_hash)
                            observer = self.peers[self.queue_observer]
                            request = inventory(32, [inv_hash])
                            observer.send('getdata', request)
                            self.lines.append(f'> {self.queue_observer} getdata {request.hex()}')
                assert reader.pos == len(payload)
            if cmd == 'dsq':
                verify_bls(cmd, payload, self.net.metadata['masternodes'])
                queue_hash = sha256d(payload)
                alias = self.queues.setdefault(queue_hash.hex(), 'queue-' + str(len(self.queues) + 1))
                self.queue_bodies.setdefault(alias, dict(hash=queue_hash.hex(), body=payload.hex(),
                                                         denom=struct.unpack('<i', payload[:4])[0],
                                                         protx=payload[4:36].hex(), ready=bool(payload[44]),
                                                         observer=peer.n))
            if cmd == 'dstx':
                verify_bls(cmd, payload, self.net.metadata['masternodes'])
            value = project(cmd, payload, self.sessions, self.queues)
            if value is not None:
                self.lines.append('expect ' + dump(dict(peer=peer.n, command=cmd, value=value)))

    def handshake(self, n, protocol=70242, wants=True):
        self.accept(n)
        p2b.handshake(self, n, p2a.FULL, protocol=protocol)
        self.send(n, 'senddsq', bytes([wants]))
        if self.peers[n].last_ping:
            self.pong(n)

    def add_queue_observer(self, n=90):
        self.queue_observer = n
        self.handshake(n, protocol=70242, wants=True)

    def advance(self, seconds):
        self.lines.append(f'time {self.net.now + seconds}')
        self.net.bump(seconds)
        time.sleep(1.1)
        self.settle()

    def rpc(self, method, *params):
        result = self.node.rpc(method, *params)
        self.lines.append('rpc ' + dump(dict(method=method, params=params, result=result)))
        self.settle()
        return result

    def write(self, out):
        # Queue hashes may be announced before the observer's full body arrives: resolve after collection.
        self.lines.append('fact ' + dump(dict(key='queues', value=self.queues)))
        self.lines.append('fact ' + dump(dict(key='collaterals', value=self.collaterals)))
        self.lines.append('fact ' + dump(dict(key='queue_bodies', value=self.queue_bodies)))
        self.lines.append('fact ' + dump(dict(key='queue_observer', value=self.queue_observer)))
        sessions = {}
        final = []
        for line in self.lines:
            if line.startswith('expect '):
                continue
            final.append(line)
            if line.startswith('< '):
                parts = line.split(maxsplit=3)
                cmd = parts[2]
                payload = bytes.fromhex(parts[3]) if len(parts) == 4 else None
                value = project(cmd, payload, sessions, dict(self.queues, **self.collaterals))
                if value is not None:
                    final.append('expect ' + dump(dict(peer=int(parts[1]), command=cmd, value=value)))
        self.lines = [line.rstrip() for line in final]
        self.lines.append('fact ' + dump(dict(key='manifest', value=raw_manifest(self.lines))))
        (SCRATCH / f'{self.name}-last-attempt.txt').write_text('\n'.join(self.lines) + '\n')
        validate_scenario(self.name, self.lines)
        candidate_dir = SCRATCH / f'candidate-{os.getpid()}'
        candidate_dir.mkdir(mode=0o700, exist_ok=True)
        super().write(candidate_dir)
        candidate = candidate_dir / f'{self.name}.txt'
        check_fixture(candidate)
        Path(out).mkdir(parents=True, exist_ok=True)
        shutil.move(candidate, Path(out) / candidate.name)
        candidate_dir.rmdir()


@contextlib.contextmanager
def scenario(net, name, node, notes=()):
    t = Transcript(name, net, node, notes)
    try:
        yield t
        t.write(FIXTURES)
    finally:
        for p in t.peers.values():
            p.sock.close()


def check_fixture(path, signatures=True):
    lines = path.read_text().splitlines()
    assert 'format cj1-v1' in lines
    masks = [json.loads(l[5:]) for l in lines if l.startswith('mask ')]
    assert masks == [MASKS]
    facts = {j['key']: j['value'] for l in lines if l.startswith('fact ') for j in [json.loads(l[5:])]}
    assert facts['binary']['version'] == 'v24.0.0'
    assert re.fullmatch('[0-9a-f]{64}', facts['binary']['sha256'])
    for args in facts['commands']:
        assert '-debug=coinjoin' in args and '-debug=net' in args
        assert all(a.endswith('=<network.json>') for a in args if a.startswith(('-masternodeblsprivkey=', '-sporkkey=')))
    assert facts['setup']['blockchain_synced'] and not facts['setup']['softforks']['v24']['active']
    verified = set()
    def verify(cmd, payload):
        if (cmd, payload) not in verified:
            verify_bls(cmd, payload, facts['setup']['masternodes'], signatures)
            verified.add((cmd, payload))
    queues, sessions, expected, actual = facts['queues'], {}, [], []
    queue_bodies = facts.get('queue_bodies', {})
    for alias, body_info in queue_bodies.items():
        body = bytes.fromhex(body_info['body'])
        assert sha256d(body).hex() == body_info['hash']
        assert body_info['hash'] in queues and queues[body_info['hash']] == alias
        assert body_info['denom'] == struct.unpack('<i', body[:4])[0]
        assert body_info['protx'] == body[4:36].hex()
        assert body_info['ready'] == bool(body[44])
        verify('dsq', body)
    assert facts['manifest'] == raw_manifest(lines), 'raw evidence integrity mismatch'
    client_facts = facts.get('clients', [])
    client_txids, collateral_aliases = {}, {}
    for index, client in enumerate(client_facts, 1):
        assert client['participant'] == f'participant-{index}'
        raw = bytes.fromhex(client['collateral'])
        reader = Reader(raw)
        assert reader.tx() == raw and reader.pos == len(raw)
        txid = sha256d(raw)[::-1].hex()
        assert txid == client['acceptance']['txid']
        client_txids[txid] = client['participant']
        collateral_aliases[sha256d(raw).hex()] = client['participant']
    assert facts['collaterals'] == collateral_aliases
    observations = [json.loads(line[8:]) for line in lines if line.startswith('observe ')]
    charged_observations = [o['value'] for o in observations if o['key'] == 'charged_count']
    participant_observations = [o['value'] for o in observations if o['key'] == 'charged_participants']
    if path.stem in ('server-session', 'server-empty-entry', 'server-timeout-no-charge', 'server-timeout-charge'):
        snapshots = [json.loads(line[4:])['result'] for line in lines if line.startswith('rpc ')
                     and json.loads(line[4:])['method'] == 'getrawmempool']
        assert snapshots and charged_observations
        charged_sequences = [sorted(client_txids[txid] for txid in snapshot if txid in client_txids)
                             for snapshot in snapshots]
        assert [len(charged) for charged in charged_sequences] == charged_observations
        assert charged_sequences == participant_observations
        participants, submitted, empty_entries = set(), set(), set()
        for line in lines:
            if not line.startswith('> '):
                continue
            parts = line.split()
            if parts[2] == 'dsa' and len(parts) == 4:
                payload = bytes.fromhex(parts[3])
                for client in client_facts:
                    if payload == struct.pack('<i', 16) + bytes.fromhex(client['collateral']) + b'\x00':
                        participants.add(client['participant'])
            if parts[2] == 'dsi' and len(parts) == 4:
                payload = bytes.fromhex(parts[3])
                for client in client_facts:
                    cooked = {k: bytes.fromhex(v) if k in ('collateral', 'dest', 'script') else v
                              for k, v in client.items()}
                    if payload == entry(cooked):
                        submitted.add(client['participant'])
                    if payload == entry(cooked, empty=True):
                        empty_entries.add(client['participant'])
        assert participants == set(client_txids.values())
        offenders = empty_entries if path.stem == 'server-empty-entry' else participants - submitted
        assert set(facts['eligibility']['offenders']) == offenders
        for charged in charged_sequences:
            assert set(charged) <= offenders, 'charged a cooperating participant'
        for line in lines:
            if line.startswith('< ') and ' inv ' in line:
                projected = project('inv', bytes.fromhex(line.split()[3]), {}, collateral_aliases) or []
                assert {alias for kind, alias in projected if kind == 1 and alias in client_txids.values()} <= offenders
        if path.stem == 'server-empty-entry':
            assert len(empty_entries) == 1 and charged_sequences == [sorted(empty_entries)]
            assert set(snapshots[0]) == {txid for txid, alias in client_txids.items() if alias in empty_entries}
        if path.stem == 'server-timeout-charge':
            assert len(offenders) == 1 and charged_sequences[-1] == sorted(offenders)
    else:
        assert not charged_observations and not participant_observations
    peers = set()
    logs = 0
    observed_logs, derived_logs = [], []
    for line in lines:
        if not line or line.startswith('#') or line == 'format cj1-v1':
            continue
        tag, rest = line.split(' ', 1)
        if tag in ('fact', 'mask', 'observe', 'rpc', 'log', 'expect'):
            value = json.loads(rest)
            if tag == 'log':
                assert isinstance(value, str)
                logs += 1
                verdict = dstx_verdict(value)
                score = re.search(r'Misbehaving:.*?\((\d+) -> (\d+)\)', value)
                size = re.search(r'mapDSTX.size\(\)=(\d+)', value)
                height = re.search(r'UpdateDSTXConfirmedHeight.*nHeight=(-?\d+)', value)
                if verdict:
                    derived_logs.append(dict(key='dstx_verdict', value=verdict))
                if score:
                    derived_logs.append(dict(key='score_delta', value=int(score[2]) - int(score[1])))
                if size:
                    derived_logs.append(dict(key='dstx_map_size', value=int(size[1])))
                if height:
                    derived_logs.append(dict(key='dstx_confirmed_height', value=int(height[1])))
            if tag == 'observe' and value['key'] in ('score_delta', 'dstx_map_size', 'dstx_confirmed_height', 'dstx_verdict'):
                observed_logs.append(value)
            if tag == 'expect':
                actual.append(value)
        elif tag in ('mocktime', 'time', 'ibd'):
            int(rest)
        elif tag == 'accept':
            assert int(rest) not in peers
            peers.add(int(rest))
        elif tag in ('>', '<'):
            parts = rest.split(maxsplit=2)
            peer, cmd = int(parts[0]), parts[1]
            assert peer in peers
            payload = bytes.fromhex(parts[2]) if len(parts) == 3 and parts[2] != 'echo' else b''
            if tag == '<':
                if cmd in ('dsq', 'dstx'):
                    verify(cmd, payload)
                value = project(cmd, payload, sessions, dict(queues, **facts['collaterals']))
                if value is not None:
                    expected.append(dict(peer=peer, command=cmd, value=value))
        else:
            raise ValueError(f'{path}: unknown record {tag}')
    derived_completion_ids = [e['value']['message'] for e in expected if e['command'] == 'dsc']
    completion_observations = [json.loads(line[8:])['value'] for line in lines
                               if line.startswith('observe ') and json.loads(line[8:])['key'] == 'completion_ids']
    if completion_observations:
        assert completion_observations == [derived_completion_ids]
    announced = set()
    for line in lines:
        if line.startswith('< ') and ' inv ' in line:
            payload = bytes.fromhex(line.split(maxsplit=3)[3])
            reader = Reader(payload)
            for _ in range(reader.count()):
                kind = int.from_bytes(reader.take(4), 'little')
                inv_hash = reader.take(32)
                if kind == 32:
                    announced.add(inv_hash.hex())
            assert reader.pos == len(payload)
    assert queues == {info['hash']: alias for alias, info in queue_bodies.items()}
    assert announced <= set(queues), 'announced queue body missing'
    for alias, info in queue_bodies.items():
        assert f"< {info['observer']} dsq {info['body']}" in lines, 'queue body is not observed'
        assert any(e['command'] == 'dsq' and e['value'].get('queue') == alias for e in expected)
    if facts['queue_observer'] is not None:
        observer = facts['queue_observer']
        for queue_hash in announced:
            assert f"> {observer} getdata {inventory(32, [bytes.fromhex(queue_hash)]).hex()}" in lines
            assert f"< {observer} dsq {queue_bodies[queues[queue_hash]]['body']}" in lines
    assert observed_logs == derived_logs, f'{path}: observations differ from Core logs'
    assert expected == actual, f'{path}: projection differs from raw responses'
    assert logs > 0 and peers, f'{path}: missing Core evidence'
    validate_scenario(path.stem, lines)
    return len(expected)


def validate_scenario(name, lines):
    expected = [json.loads(l[7:]) for l in lines if l.startswith('expect ')]
    obs = [json.loads(l[8:]) for l in lines if l.startswith('observe ')]
    def values(key):
        return [o['value'] for o in obs if o['key'] == key]
    messages = [e['value']['message'] for e in expected if e['command'] == 'dssu']
    inv = [e for e in expected if e['command'] == 'inv']
    facts = {j['key']: j['value'] for l in lines if l.startswith('fact ') for j in [json.loads(l[5:])]}
    if name in ('server-session', 'server-empty-entry', 'server-recent', 'server-timeout-no-charge', 'server-timeout-charge'):
        bodies = list(facts['queue_bodies'].values())
        assert bodies and facts['queue_observer'] is not None
        assert all(b['denom'] == 16 and bytes.fromhex(b['protx'])[::-1].hex() == facts['setup']['masternodes'][0]['pro_tx_hash'] for b in bodies)
        assert any(b['ready'] for b in bodies) == (name != 'server-recent')
    if name == 'server-session':
        assert {0, 6, 9, 11, 14, 16}.issubset(messages), messages
        assert values('completion_ids') == [[20, 20]]
        assert any(any(i[0] == 16 for i in e['value']) for e in inv), 'missing DSTX inventory'
        assert sum(line.startswith('> ') and ' dsa ' in line for line in lines) >= 3
        assert sum(line.startswith('> ') and ' dsi ' in line for line in lines) >= 2
        assert sum(line.startswith('> ') and ' dss ' in line for line in lines) >= 5
        assert '> 9 dsa ff' in lines and '> 9 dsi ff' in lines and '> 9 dss ff' in lines
        for peer in (1, 2):
            assert any(line.startswith(f'> {peer} dss 01') and len(line.split()[3]) > 80 for line in lines)
    elif name == 'server-empty-entry':
        assert 22 in messages and values('charged_count') == [1]
        assert any(line.startswith('> ') and ' dsi ' in line for line in lines)
    elif name == 'server-mn-list':
        assert 10 in messages
        assert any(line.startswith('> 1 dsa ') for line in lines)
    elif name == 'server-recent':
        assert 15 in messages
        assert any(line.startswith('> 1 dsq ') for line in lines)
        assert any(line.startswith('> 1 dsa ') for line in lines)
    elif name == 'server-timeout-no-charge':
        assert values('charged_count') == [0]
        assert any('timed out -- resetting' in l for l in lines)
    elif name == 'server-timeout-charge':
        assert values('charged_count')[-1] == 1
        assert any(line.startswith('> ') and ' dsi ' in line for line in lines)
    elif name == 'dstx-intake':
        assert values('score_delta') == [10, 1, 10]
        assert values('dstx_verdict').count('accepted') == 6
        assert values('dstx_verdict').count('mixing-limit') == 1
        assert 'already-have' in values('dstx_verdict')
        assert any(e['command'] == 'dstx' for e in expected), 'getdata did not serve DSTX'
        assert any(e['command'] == 'dstx' and e['peer'] == 8 for e in expected)
        assert any(e['command'] == 'tx' and e['peer'] == 9 for e in expected)
        assert any(line.startswith('> 8 getdata ') for line in lines)
        assert any(line.startswith('> 9 getdata ') for line in lines)
        assert any(any(i[0] == 16 for i in e['value']) for e in inv), 'missing DSTX relay'
    elif name == 'dstx-lifecycle-reorg':
        heights = values('dstx_confirmed_height')
        assert heights[0] == -1 and heights[-1] == heights[1] + 1, heights
        assert values('dstx_map_size')[-1] == 0
        assert values('depth') == [24, 25]
        assert values('dstx_verdict').count('already-have') >= 2
    elif name == 'dstx-removed-mn':
        assert values('last_present_depth') == [1, 23, 24]
        assert values('dstx_verdict').count('accepted') == 2
        assert values('score_delta') == [1]
    elif name == 'dstx-chainlock':
        assert values('dstx_map_size')[-1] == 0
        assert any('"key":"chainlocked_block"' in l for l in lines)
    elif name == 'dsq-wallet-threshold':
        assert values('score_delta') == []
        assert any(e['command'] == 'notfound' for e in expected)
        assert any(e['command'] == 'dsq' and e['value']['ready'] for e in expected)
        assert any('sending too many dsq messages' in l for l in lines)
        assert not inv
    elif name == 'dsq-blocksonly':
        assert values('score_delta') == []
        assert not any(e['command'] in ('dsq', 'inv') for e in expected)
        assert any(e['command'] == 'notfound' for e in expected)
        assert any(e['command'] == 'closed' and e['peer'] == 1 for e in expected)
    elif name == 'dsq-modes':
        assert {e['peer'] for e in expected if e['command'] == 'dsq'} == {1, 3}
        assert {e['peer'] for e in inv if any(i[0] == 32 for i in e['value'])} == {3, 5}
        assert any(e['command'] == 'notfound' for e in expected)
    elif name in ('dsq-wallet-intake', 'dsq-server-intake'):
        assert values('score_delta') == ([100, 10, 10, 10] if name == 'dsq-server-intake' else [100, 10, 10])
        has_queue_inv = any(any(i[0] == 32 for i in e['value']) for e in inv)
        assert has_queue_inv == (name == 'dsq-server-intake')
    else:
        raise ValueError(f'unknown scenario fixture {name}')


def cleanup_selftest():
    from unittest.mock import patch
    events = []
    class FakeNode:
        def __init__(self, name, fail=False):
            self.name, self.fail = name, fail
        def reap(self):
            events.append(('reap', self.name))
            if self.fail:
                raise OSError('injected reap failure')
            return []
        def delete_datadir(self):
            events.append(('delete', self.name))
            if self.fail:
                raise OSError('injected delete failure')
            return []
    net = Network.__new__(Network)
    net.nodes = [FakeNode('controller'), FakeNode('mn', fail=True)]
    with contextlib.redirect_stderr(io.StringIO()):
        net.close()
    assert events == [('reap', 'mn'), ('reap', 'controller'), ('delete', 'mn'), ('delete', 'controller')]
    assert len(net.cleanup_errors) == 2
    with tempfile.TemporaryDirectory(prefix='construction-', dir=SCRATCH) as tmp:
        source = Path(tmp) / 'image'
        (source / 'regtest').mkdir(parents=True)
        for failure in ('copy', 'open', 'spawn'):
            name = 'partial-' + failure
            def broken_copy(src, dst):
                dst.mkdir()
                raise OSError('injected copy failure')
            target = {'copy': 'shutil.copytree', 'open': 'builtins.open', 'spawn': 'subprocess.Popen'}[failure]
            effect = broken_copy if failure == 'copy' else OSError('injected construction failure')
            handles = []
            real_open = open
            def tracked_open(*args, **kwargs):
                handle = real_open(*args, **kwargs)
                handles.append(handle)
                return handle
            open_patch = patch('builtins.open', side_effect=tracked_open) if failure == 'spawn' else contextlib.nullcontext()
            with patch(target, side_effect=effect), open_patch:
                try:
                    Node(Path('unused'), source, name, [])
                except OSError:
                    pass
                else:
                    raise AssertionError('construction failure was not injected')
            assert all(handle.closed for handle in handles)
            assert not (SCRATCH / f'run-{os.getpid()}' / name).exists()
        parent = SCRATCH / f'run-{os.getpid()}'
        if parent.exists():
            parent.rmdir()


def selftest():
    SCRATCH.mkdir(mode=0o700, parents=True, exist_ok=True)
    p2a.tempfile.tempdir = str(SCRATCH)
    p2a.selftest()
    sources = {path.stem: path.read_text() for path in FIXTURES.glob('*.txt')}
    def corrupt_reply(text):
        return re.sub(r'(< \d+ dssu )([0-9a-f]+)', lambda m: m[1] + m[2][:-2] + 'ff', text, count=1)
    def corrupt_signature(text):
        return re.sub(r'(< \d+ dsq )([0-9a-f]+)', lambda m: m[1] + m[2][:-2] + ('00' if m[2][-2:] != '00' else '01'), text, count=1)
    def refresh_manifest(text):
        manifest = dump(dict(key='manifest', value=raw_manifest(text.splitlines())))
        return re.sub(r'^fact \{"key":"manifest".*$', 'fact ' + manifest, text, flags=re.MULTILINE)
    def remove_record(text, pattern):
        return refresh_manifest('\n'.join(line for line in text.splitlines() if not re.search(pattern, line)) + '\n')
    def empty_mempool(text):
        return refresh_manifest(re.sub(r'^(rpc .*\"method\":\"getrawmempool\".*\"result\":)\[[^\n]*\]', r'\1[]', text, count=1, flags=re.MULTILINE))
    def wrong_charge(text):
        facts = {j['key']: j['value'] for l in text.splitlines() if l.startswith('fact ') for j in [json.loads(l[5:])]}
        innocent, offender = facts['clients']
        old, new = offender['acceptance']['txid'], innocent['acceptance']['txid']
        return refresh_manifest('\n'.join(line.replace(old, new) if line.startswith('rpc ') else line.replace('participant-2', 'participant-1') if line.startswith('observe ') else line for line in text.splitlines()) + '\n')
    def wrong_inventory(text):
        facts = {j['key']: j['value'] for l in text.splitlines() if l.startswith('fact ') for j in [json.loads(l[5:])]}
        innocent, offender = facts['clients']
        old, new = sha256d(bytes.fromhex(offender['collateral'])).hex(), sha256d(bytes.fromhex(innocent['collateral'])).hex()
        return refresh_manifest('\n'.join(line.replace(old, new) if line.startswith('< ') else line.replace('participant-2', 'participant-1') if line.startswith('expect ') else line for line in text.splitlines()) + '\n')
    cases = [
        ('server-session', lambda t: t.replace('mask ', 'unknown ', 1)),
        ('server-session', lambda t: refresh_manifest(corrupt_reply(t))),
        ('server-session', lambda t: t.replace('"version":"v24.0.0"', '"version":"v24.0.0-rc.3"', 1)),
        ('dsq-wallet-intake', lambda t: refresh_manifest(corrupt_signature(t))),
        ('dsq-wallet-intake', lambda t: t.replace('"key":"score_delta","value":100', '"key":"score_delta","value":99', 1)),
        ('dstx-intake', lambda t: remove_record(t, r'^(< 9 tx |expect .*"command":"tx","peer":9,)')),
        *[('server-session', lambda t, cmd=cmd: remove_record(t, rf'^> \d+ {cmd} ')) for cmd in ('dsa', 'dsi', 'dss')],
        ('server-session', lambda t: t.replace('"key":"completion_ids","value":[20,20]', '"key":"completion_ids","value":[20,21]', 1)),
        ('dsq-blocksonly', lambda t: remove_record(t, r'^(< 1 closed|expect .*"command":"closed")')),
        ('server-empty-entry', empty_mempool),
        ('server-timeout-charge', wrong_charge),
        ('server-timeout-charge', wrong_inventory),
        ('server-session', lambda t: re.sub(r'^log .*\n', '', t, count=1, flags=re.MULTILINE)),
        ('server-session', lambda t: remove_record(t, r'^< 90 dsq ')),
        ('server-empty-entry', lambda t: t.replace('"key":"charged_count","value":1', '"key":"charged_count","value":0', 1)),
    ]
    with tempfile.TemporaryDirectory(prefix='selftest-', dir=SCRATCH) as tmp:
        for name, mutate in cases:
            text = mutate(sources[name])
            assert text != sources[name], 'negative control did not alter the fixture'
            path = Path(tmp) / f'{name}.txt'
            path.write_text(text)
            try:
                check_fixture(path)
            except (AssertionError, ValueError, StopIteration, struct.error):
                pass
            else:
                raise RuntimeError(f'accepted corrupt fixture: {name}')
    cleanup_selftest()
    print(f'selftest: {len(cases)} transcript corruption controls and cleanup controls passed')


def verify_version(binary):
    version = subprocess.check_output([str(binary), '--version'], text=True).splitlines()[0]
    if version != 'Dash Core version v24.0.0':
        raise SystemExit(f'refusing capture: expected v24.0.0, got {version}')
    print(version, hashlib.sha256(binary.read_bytes()).hexdigest(), flush=True)


def main():
    global FIXTURES
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['setup', 'capture', 'selfcheck', 'structural-selfcheck', 'selftest', 'probe'])
    parser.add_argument('scenarios', nargs='*', metavar='SCENARIO')
    parser.add_argument('--setup', type=Path, default=SCRATCH / 'setup.json')
    parser.add_argument('--fixtures', type=Path, default=FIXTURES)
    args = parser.parse_args()
    FIXTURES = args.fixtures
    unknown = set(args.scenarios) - SCENARIOS.keys()
    if unknown:
        parser.error('unknown scenarios: ' + ', '.join(sorted(unknown)))
    if args.mode == 'selftest':
        selftest()
        return
    if args.mode in ('selfcheck', 'structural-selfcheck'):
        paths = sorted(FIXTURES.glob('*.txt'))
        assert paths, 'no fixtures'
        for path in paths:
            print(path.name, check_fixture(path, signatures=args.mode == 'selfcheck'), 'comparisons')
        return
    SCRATCH.mkdir(mode=0o700, parents=True, exist_ok=True)
    if args.mode == 'setup':
        os.environ['DASHVERSION'] = '24.0.0'
        os.environ.setdefault('CACHE_DIR', str(SCRATCH / 'cache'))
        spec = importlib.util.spec_from_file_location('setup_dashd', REPO / 'contrib/setup-dashd.py')
        setup_tool = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(setup_tool)
        cache = setup_tool.get_cache_dir()
        Path(cache).mkdir(mode=0o700, parents=True, exist_ok=True)
        binary = Path(setup_tool.setup_dashd(cache))
        verify_version(binary)
        image = setup_tool.setup_test_data(cache, 'regtest-mn', 'network.json')
        args.setup.write_text(dump(dict(dashd=str(binary), network=image)) + '\n')
        return
    setup = json.loads(args.setup.read_text())
    binary, image = Path(setup['dashd']), Path(setup['network'])
    verify_version(binary)
    failures = []
    for name in args.scenarios or (['probe'] if args.mode == 'probe' else [n for n in SCENARIOS if n != 'chainlock']):
        net = Network(binary, image, blocks_only=name == 'dsq-blocksonly')
        try:
            if args.mode == 'probe':
                print(dump(dict(info=net.info, mn=[n.rpc('masternode', 'status') for n in net.mns])), flush=True)
            else:
                SCENARIOS[name](net)
        except Exception:
            failures.append(name)
            traceback.print_exc()
        finally:
            net.close()
            if net.cleanup_errors:
                failures.append(name + ' cleanup')
    if failures:
        raise SystemExit('capture failed: ' + ', '.join(failures))


def fund(net, count):
    c = net.controller
    outputs, specs = {}, []
    for _ in range(count):
        denom, collateral, dest = [c.rpc('getnewaddress') for _ in range(3)]
        outputs[denom], outputs[collateral] = 0.00100001, 0.01
        specs.append((denom, collateral, dest))
    parent = c.rpc('sendmany', '', outputs)
    net.mine(1)
    decoded = c.rpc('getrawtransaction', parent, True)
    result = []
    for denom, collateral, dest in specs:
        def output(address):
            return next(v for v in decoded['vout'] if v['scriptPubKey'].get('address') == address)
        d, col = output(denom), output(collateral)
        raw = c.rpc('createrawtransaction', [dict(txid=parent, vout=col['n'])], {dest: 0.0099})
        signed = c.rpc('signrawtransactionwithwallet', raw)
        assert signed['complete']
        acceptance = c.rpc('testmempoolaccept', [signed['hex']])[0]
        assert acceptance['allowed'], acceptance
        dest_script = bytes.fromhex(c.rpc('getaddressinfo', c.rpc('getnewaddress'))['scriptPubKey'])
        result.append(dict(collateral=bytes.fromhex(signed['hex']), acceptance=acceptance,
                           txid=parent, vout=d['n'], script=bytes.fromhex(d['scriptPubKey']['hex']),
                           dest=dest_script))
    return result


def txin(client, script=b''):
    return bytes.fromhex(client['txid'])[::-1] + struct.pack('<I', client['vout']) + compact(len(script)) + script + b'\xff' * 4


def txout(client):
    return struct.pack('<q', 100001) + compact(len(client['dest'])) + client['dest']


def entry(client, empty=False):
    ins = b'\x00' if empty else b'\x01' + txin(client)
    outs = b'\x00' if empty else b'\x01' + txout(client)
    return ins + client['collateral'] + outs


def mixed_tx(clients):
    # Core BIP69 orders by display hash then index; RPC signs the resulting bytes.
    clients = sorted(clients, key=lambda c: (c['txid'], c['vout']))
    return (struct.pack('<i', 2) + compact(len(clients)) + b''.join(txin(c) for c in clients)
            + compact(len(clients)) + b''.join(sorted(txout(c) for c in clients)) + bytes(4))


def signed_mix(net, clients):
    signed = net.controller.rpc('signrawtransactionwithwallet', mixed_tx(clients).hex(), [], 'ALL|ANYONECANPAY')
    assert signed['complete']
    return bytes.fromhex(signed['hex'])


def note_clients(t, clients):
    for index, c in enumerate(clients):
        alias = f'participant-{index + 1}'
        t.collaterals[sha256d(c['collateral']).hex()] = alias
    t.fact('coins', [t.net.controller.rpc('gettxout', c['txid'], c['vout']) for c in clients])
    t.fact('clients', [dict(participant=f'participant-{index + 1}',
                            **{k: (v.hex() if isinstance(v, bytes) else v) for k, v in c.items()})
                       for index, c in enumerate(clients)])


def note_eligibility(t, offenders):
    t.fact('eligibility', dict(offenders=offenders))


def start_session(t, clients, duplicate=False):
    for i, c in enumerate(clients, 1):
        t.handshake(i)
        t.send(i, 'dsa', struct.pack('<i', 16) + c['collateral'] + b'\x00')
        if duplicate and i == 1:
            t.send(i, 'dsa', struct.pack('<i', 16) + c['collateral'] + b'\x00')
    t.advance(30)
    assert t.node.rpc('getcoinjoininfo')['state'] == 'ACCEPTING_ENTRIES', t.node.rpc('getcoinjoininfo')


def last_payload(t, command, peer=1):
    prefix = f'< {peer} {command} '
    return bytes.fromhex(next(l[len(prefix):] for l in reversed(t.lines) if l.startswith(prefix)))


def scenario_server(net):
    clients = fund(net, 2)
    with scenario(net, 'server-session', net.mns[0], [
        'Pre-V24 two-client session; duplicate collateral and all five BL-180 pre-decode guards.',
        'dsa during SIGNING returns ERR_MODE. Raw tx and RPC admission answers are chain facts.'
    ]) as t:
        note_clients(t, clients)
        t.add_queue_observer()
        note_eligibility(t, [])
        t.handshake(9)
        t.send(9, 'dsi', b'\xff')
        t.send(9, 'dss', b'\xff')
        start_session(t, clients, duplicate=True)
        t.send(9, 'dsa', b'\xff')
        for i, c in enumerate(clients, 1):
            t.send(i, 'dsi', entry(c))
        final = last_payload(t, 'dsf')[4:]
        t.send(9, 'dss', b'\xff')
        t.send(1, 'dss', compact(181))
        t.send(1, 'dsa', struct.pack('<i', 16) + clients[0]['collateral'] + b'\x00')
        signed = net.controller.rpc('signrawtransactionwithwallet', final.hex(), [], 'ALL|ANYONECANPAY')
        t.fact('final_signing', signed)
        assert signed['complete']
        r = Reader(bytes.fromhex(signed['hex']))
        r.take(4)
        signed_inputs = []
        for _ in range(r.count()):
            begin = r.pos
            r.take(36)
            r.varbytes()
            r.take(4)
            signed_inputs.append(r.raw[begin:r.pos])
        for i, c in enumerate(clients, 1):
            own = next(x for x in signed_inputs if x[:36] == txin(c)[:36])
            t.send(i, 'dss', b'\x01' + own)
        t.advance(30)
        t.observe('completion_ids', [struct.unpack('<i', bytes.fromhex(l.split()[3])[4:])[0]
                                    for l in t.lines if re.match(r'< \d+ dsc ', l)])
        mempool = t.rpc('getrawmempool')
        col_ids = [net.controller.rpc('decoderawtransaction', c['collateral'].hex())['txid'] for c in clients]
        t.observe('charged_count', len(set(col_ids) & set(mempool)))
        # Recent queue may survive reset only in metaman; Core is the source of the verdict.
        t.send(1, 'dsa', struct.pack('<i', 16) + clients[0]['collateral'] + b'\x00')


def scenario_empty_timeout(net):
    clients = fund(net, 2)
    with scenario(net, 'server-empty-entry', net.mns[0], ['MNO-19: empty entry rejected and its collateral consumed.']) as t:
        note_clients(t, clients)
        t.add_queue_observer()
        note_eligibility(t, ['participant-1'])
        start_session(t, clients)
        t.send(1, 'dsi', entry(clients[0], empty=True))
        t.observe('charged_count', int(net.controller.rpc('decoderawtransaction', clients[0]['collateral'].hex())['txid'] in t.rpc('getrawmempool')))


def scenario_timeout(net):
    clients = fund(net, 2)
    with scenario(net, 'server-timeout-no-charge', net.mns[0], ['All clients omit entries: timeout resets without charging everyone.']) as t:
        note_clients(t, clients)
        t.add_queue_observer()
        note_eligibility(t, ['participant-1', 'participant-2'])
        start_session(t, clients)
        t.advance(31)
        t.observe('coinjoin', t.rpc('getcoinjoininfo'))
        ids = [net.controller.rpc('decoderawtransaction', c['collateral'].hex())['txid'] for c in clients]
        t.observe('charged_count', len(set(ids) & set(t.rpc('getrawmempool'))))


def scenario_dsq_modes(net):
    clients = fund(net, 2)
    with scenario(net, 'dsq-modes', net.mns[0], ['70233 true = full DSQ; 70234+ true = inv; false = none.']) as t:
        note_clients(t, clients)
        for i, version, wants in [(1, 70233, True), (2, 70233, False), (3, 70234, True), (4, 70234, False), (5, 70242, True)]:
            t.handshake(i, version, wants)
        for i, c in enumerate(clients, 1):
            t.send(i, 'dsa', struct.pack('<i', 16) + c['collateral'])
        t.advance(30)
        t.advance(1)
        queue = last_payload(t, 'dsq')
        t.send(3, 'getdata', inventory(32, [sha256d(queue)]))
        t.send(3, 'getdata', inventory(32, [bytes.fromhex('12' * 32)]))
        t.fact('subscriptions', [[1, 70233, True], [2, 70233, False], [3, 70234, True], [4, 70234, False], [5, 70242, True]])


def scenario_dsq_intake(net, server=False):
    with scenario(net, 'dsq-server-intake' if server else 'dsq-wallet-intake', net.mns[0] if server else net.controller, ['DSQ null +100, unknown +10, bad signature +10, valid/duplicate/expired; server relays, wallet does not.']) as t:
        mn = net.metadata['masternodes'][1 if server else 0]
        t.handshake(8)
        for i, payload in [(1, dsq(dict(mn, pro_tx_hash='00' * 32), net.now, signature=False)),
                           (2, dsq(dict(mn, pro_tx_hash='11' * 32), net.now, signature=False)),
                           (3, dsq(mn, net.now, signature=False)),
                           (4, dsq(mn, net.now - 31)), (5, dsq(mn, net.now))]:
            t.handshake(i)
            t.send(i, 'dsq', payload)
            if i == 5:
                t.send(i, 'dsq', payload)
                t.send(8, 'getdata', inventory(32, [sha256d(payload)]))
        t.send(8, 'getdata', inventory(32, [bytes.fromhex('12' * 32)]))
        if server:
            t.handshake(7)
            t.send(7, 'dsq', dsq(mn, net.now, denom=0))


def scenario_dstx(net):
    clients = fund(net, 16)
    mn = net.metadata['masternodes'][0]
    with scenario(net, 'dstx-intake', net.controller, ['DSTX invalid structure, unknown MN, bad signature; seven signed mixes exercise mixing allowance (Core admits six, drops the seventh); bad-wrapper resend.']) as t:
        note_clients(t, clients)
        t.handshake(8)
        t.handshake(9, protocol=70240)
        t.handshake(1)
        # AllowMixing from an authenticated queue resets this MN's DSTX allowance.
        t.send(1, 'dsq', dsq(mn, net.now))
        for i, chosen, raw, sig in [
            (2, mn, mixed_tx(clients[:1]), True),
            (3, dict(mn, pro_tx_hash='11' * 32), mixed_tx(clients[:2]), False),
            (4, mn, mixed_tx(clients[2:4]), False),
        ]:
            t.handshake(i)
            t.send(i, 'dstx', dstx(raw, chosen, net.now, sig))
        t.handshake(5)
        for offset in range(0, 14, 2):
            raw = signed_mix(net, clients[offset:offset + 2])
            wrapped = dstx(raw, mn, net.now)
            t.send(5, 'dstx', wrapped)
            if offset == 0:
                t.send(5, 'dstx', dstx(raw, dict(mn, pro_tx_hash='11' * 32), net.now, False))
                t.advance(30)
                t.send(8, 'getdata', inventory(16, [sha256d(raw)]))
                t.send(9, 'getdata', inventory(1, [sha256d(raw)]))
        t.advance(2)
        t.rpc('getrawmempool')


def disable_chainlocks(net):
    net.controller.rpc('sporkupdate', 'SPORK_19_CHAINLOCKS_ENABLED', 4070908800)
    net.bump(1)
    time.sleep(1)


def scenario_lifecycle(net):
    disable_chainlocks(net)
    clients = fund(net, 2)
    mn = net.metadata['masternodes'][0]
    with scenario(net, 'dstx-lifecycle-reorg', net.controller, [
        'm-3: confirm at h, disconnect, mempool re-add, competing empty block at h, re-mine at h+1.',
        'ChainLocks disabled for depth-24/25 expiry. RPC snapshots and logs record the new confirmation base.'
    ]) as t:
        note_clients(t, clients)
        t.handshake(1)
        t.handshake(2)
        t.send(1, 'dsq', dsq(mn, net.now))
        raw = signed_mix(net, clients)
        wrapper = dstx(raw, mn, net.now)
        txid = sha256d(raw)[::-1].hex()
        t.send(1, 'dstx', wrapper)
        assert txid in t.rpc('getrawmempool')
        block = t.rpc('generatetoaddress', 1, net.controller.rpc('getnewaddress'))[0]
        h = t.rpc('getblockcount')
        t.rpc('invalidateblock', block)
        assert txid in t.rpc('getrawmempool')
        # Submit an empty candidate made by Core's miner (no mempool txs) before reconsidering.
        # generateblock with [] explicitly excludes the DSTX and makes the alternative h.
        side = t.rpc('generateblock', net.controller.rpc('getnewaddress'), [])['hash']
        t.fact('fork', dict(original=block, alternate=side, original_height=h))
        t.rpc('generateblock', net.controller.rpc('getnewaddress'), [raw.hex()])
        t.rpc('reconsiderblock', block)
        h2 = t.rpc('getblockcount')
        assert h2 == h + 1
        confirmed = t.rpc('getrawtransaction', txid, True)
        assert confirmed['height'] == h2 and confirmed['blockhash'] != block
        t.send(1, 'dstx', wrapper)
        for depth in (24, 25):
            current = t.node.rpc('getblockcount')
            t.rpc('generatetoaddress', h2 + depth - current, net.controller.rpc('getnewaddress'))
            t.send(2, 'getdata', inventory(16, [sha256d(raw)]))
            t.send(1, 'dstx', dstx(raw, dict(mn, pro_tx_hash='11' * 32), net.now, False))
            t.observe('depth', depth)


def scenario_mn_list(net):
    clients = fund(net, 1)
    mn = net.metadata['masternodes'][0]
    with scenario(net, 'server-mn-list', net.mns[0], ['Revoke this registered MN; dsa is decoded, then ERR_MN_LIST before collateral checking.']) as t:
        t.handshake(1)
        result = net.controller.rpc('protx', 'revoke', mn['pro_tx_hash'], mn['bls_private_key'], 0)
        t.fact('revoke_txid', result)
        net.mine(1)
        t.lines.append('rpc ' + dump(dict(method='controller.protx-revoke-and-mine', params=[mn['pro_tx_hash']], result=result)))
        t.settle()
        t.send(1, 'dsa', struct.pack('<i', 16) + clients[0]['collateral'] + b'\x00')


def scenario_removed_mn(net):
    disable_chainlocks(net)
    clients = fund(net, 6)
    mn = net.metadata['masternodes'][0]
    with scenario(net, 'dstx-removed-mn', net.controller, ['Spend MN collateral; historical lookup accepts at depths 1 and 23, rejects at depth 24.']) as t:
        note_clients(t, clients)
        t.handshake(1)
        t.send(1, 'dsq', dsq(mn, net.now))
        details = t.node.rpc('protx', 'info', mn['pro_tx_hash'])
        coin = t.node.rpc('gettxout', details['collateralHash'], details['collateralIndex'])
        raw = t.node.rpc('createrawtransaction', [dict(txid=details['collateralHash'], vout=details['collateralIndex'])],
                         {t.node.rpc('getnewaddress'): round(coin['value'] - 0.0001, 8)})
        signed = t.node.rpc('signrawtransactionwithwallet', raw)
        assert signed['complete']
        t.rpc('sendrawtransaction', signed['hex'])
        t.rpc('generatetoaddress', 1, t.node.rpc('getnewaddress'))
        removed_height = t.rpc('getblockcount')
        t.fact('removed_mn', dict(protx=mn['pro_tx_hash'], height=removed_height, collateral=details))
        for offset, depth in enumerate((1, 23, 24)):
            height = removed_height + depth - 1
            count = height - t.node.rpc('getblockcount')
            if count:
                t.rpc('generatetoaddress', count, t.node.rpc('getnewaddress'))
            raw = signed_mix(net, clients[2 * offset:2 * offset + 2])
            t.send(1, 'dstx', dstx(raw, mn, net.now))
            t.observe('last_present_depth', depth)
            t.rpc('getrawmempool')


def scenario_timeout_charge(net):
    clients = fund(net, 2)
    # Sampling is explicit: keep the complete sequence of real attempts, never choose an identity.
    with scenario(net, 'server-timeout-charge', net.mns[0], ['One client submits, one omits entries. Repeat until Core randomly charges exactly one offender (cap 60).']) as t:
        note_clients(t, clients)
        t.add_queue_observer()
        note_eligibility(t, ['participant-2'])
        start_session(t, clients)
        for attempt in range(60):
            if attempt:
                for i, c in enumerate(clients, 1):
                    t.send(i, 'dsa', struct.pack('<i', 16) + c['collateral'] + b'\x00')
                t.advance(30)
            t.send(1, 'dsi', entry(clients[0]))
            t.advance(31)
            ids = [net.controller.rpc('decoderawtransaction', c['collateral'].hex())['txid'] for c in clients]
            count = len(set(ids) & set(t.rpc('getrawmempool')))
            t.observe('charged_count', count)
            if count:
                assert count == 1
                break
        else:
            raise RuntimeError('no charged timeout in 60 independent real sessions')


def scenario_threshold(net):
    c = net.controller
    key = c.rpc('bls', 'generate')['public']
    funding = c.rpc('getnewaddress')
    c.rpc('sendtoaddress', funding, 1100)
    net.mine(1)
    fifth = c.rpc('protx', 'register_fund', c.rpc('getnewaddress'), f'127.0.0.1:{free_port()}',
                  c.rpc('getnewaddress'), key, c.rpc('getnewaddress'), 0, c.rpc('getnewaddress'), funding)
    net.mine(1)
    with scenario(net, 'dsq-wallet-threshold', c, ['Five registered MNs make enabled_count/5 = 1; a ready DSQ is dropped until another MN advances the global counter.']) as t:
        t.fact('fifth_mn', dict(protx=fifth, public_key=key))
        t.handshake(1)
        t.handshake(8)
        mn, other = net.metadata['masternodes'][:2]
        ready = dsq(mn, net.now, ready=True)
        t.send(1, 'dsq', dsq(mn, net.now))
        t.send(1, 'dsq', ready)
        t.send(8, 'getdata', inventory(32, [sha256d(ready)]))
        t.send(1, 'dsq', dsq(other, net.now))
        t.send(1, 'dsq', ready)
        t.send(8, 'getdata', inventory(32, [sha256d(ready)]))


def scenario_blocksonly(net):
    with scenario(net, 'dsq-blocksonly', net.controller, ['Blocks-only non-MN: no DSQ queue; getdata notfound; incoming DSQ violates blocks-only policy.']) as t:
        t.accept(1)
        p2b.handshake(t, 1, p2a.FULL)
        mn = net.metadata['masternodes'][0]
        payload = dsq(mn, net.now)
        t.send(1, 'getdata', inventory(32, [sha256d(payload)]))
        t.send(1, 'dsq', payload)
        t.send(1, 'dsq', dsq(dict(mn, pro_tx_hash='00' * 32), net.now, signature=False))
        t.send(1, 'getdata', inventory(32, [sha256d(payload)]))


def scenario_recent(net):
    clients = fund(net, 1)
    with scenario(net, 'server-recent', net.mns[0], ['An authenticated queue from this MN prevents a new session: ERR_RECENT.']) as t:
        note_clients(t, clients)
        t.add_queue_observer()
        note_eligibility(t, [])
        t.handshake(1)
        t.send(1, 'dsq', dsq(net.metadata['masternodes'][0], net.now))
        t.send(1, 'dsa', struct.pack('<i', 16) + clients[0]['collateral'] + b'\x00')



def scenario_chainlock(net):
    # Match MasternodeTestContext: a full-mesh quorum requirement prevents signing on this image.
    net.controller.rpc('sporkupdate', 'SPORK_21_QUORUM_ALL_CONNECTED', 4070908800)
    net.mine(1)
    for _ in range(int(os.environ.get('CJ1_CL_WARMUP_BLOCKS', '0'))):
        net.mine(1)
        net.bump(1)
        time.sleep(1)
    clients = fund(net, 2)
    mn = net.metadata['masternodes'][0]
    with scenario(net, 'dstx-chainlock', net.controller, ['Accept and mine a DSTX, then a real quorum ChainLock expires it at the tip.']) as t:
        note_clients(t, clients)
        t.handshake(1)
        t.handshake(2)
        t.send(1, 'dsq', dsq(mn, net.now))
        raw = signed_mix(net, clients)
        t.send(1, 'dstx', dstx(raw, mn, net.now))
        assert sha256d(raw)[::-1].hex() in t.rpc('getrawmempool')
        block = t.rpc('generatetoaddress', 1, net.controller.rpc('getnewaddress'))[0]
        for _ in range(120):
            t.advance(1)
            info = t.node.rpc('getblock', block)
            if info['chainlock']:
                t.fact('chainlocked_block', info)
                t.send(1, 'dstx', dstx(raw, dict(mn, pro_tx_hash='11' * 32), net.now, False))
                break
        else:
            t.fact('quorums', t.node.rpc('quorum', 'list'))
            (SCRATCH / 'chainlock-attempt.txt').write_text('\n'.join(t.lines) + '\n')
            raise RuntimeError('v0.0.4 image test quorums did not produce a ChainLock in 120 ticks')


SCENARIOS = {
    'dsq-threshold': scenario_threshold,
    'dsq-blocksonly': scenario_blocksonly,
    'recent': scenario_recent,
    'dsq-server': lambda net: scenario_dsq_intake(net, server=True),
    'chainlock': scenario_chainlock,


    'lifecycle': scenario_lifecycle,
    'mn-list': scenario_mn_list,
    'removed-mn': scenario_removed_mn,
    'timeout-charge': scenario_timeout_charge,
    'server': scenario_server,
    'empty': scenario_empty_timeout,
    'timeout': scenario_timeout,
    'dsq-modes': scenario_dsq_modes,
    'dsq-intake': scenario_dsq_intake,
    'dstx': scenario_dstx,
}

if __name__ == '__main__':
    main()
