#!/usr/bin/env python3
"""Record GV1b governance transcripts from release dashd 24.0.0.

Run with python3 -I. --build-source CORE builds the input network using CORE's
v24.0.0 functional framework and the existing gov_capture.py network builder.
Subsequent runs replay bootstrap.json.gz into fresh observed nodes. --check
re-captures R1 and compares it with the stored transcript, listing every mask.
Datadirs are confined to /work/scratch/gv1b-captures, with mode 0700.
"""

import argparse
import contextlib
import difflib
import gzip
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
from itertools import pairwise
from pathlib import Path
from types import SimpleNamespace

REPO = Path(__file__).resolve().parents[3]
ROOT = Path('/work/scratch/gv1b-captures')
FIXTURES = REPO / 'dashd-governance/tests/fixtures/dashd-v24.0.0'
DASHD = Path.home() / '.rust-dashcore-test/dashcore-24.0.0/bin/dashd'


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


p2a = load('gv1b_p2a', REPO / 'contrib/node/p2a-transcripts/capture.py')
p2a.ROOT = ROOT
p2a.DASHD = str(DASHD)
p2a.QUIET = 0.2
SIM_START = p2a.SIM_START
CHAIN_ARGS = ['-dip3params=2:2', '-testactivationheight=v20@100',
              '-testactivationheight=mn_rr@100', '-budgetparams=10:10:10',
              '-txindex=1', '-debug=gobject', '-debug=mnpayments', '-deprecatedrpc=banscore',
              '-par=2', '-rpcthreads=2']


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), default=str)


def take(stream, count):
    raw = stream.read(count)
    if len(raw) != count:
        raise ValueError('truncated payload')
    return raw


def read_compact(stream):
    tag = take(stream, 1)[0]
    if tag < 253:
        return tag
    size = {253: 2, 254: 4, 255: 8}[tag]
    value = int.from_bytes(take(stream, size), 'little')
    if value < {253: 253, 254: 65536, 255: 4294967296}[tag]:
        raise ValueError('noncanonical compact size')
    return value


def block_transactions(block):
    stream = io.BytesIO(bytes.fromhex(block))
    take(stream, 80)
    transactions = []
    for _ in range(read_compact(stream)):
        start = stream.tell()
        version, = struct.unpack('<I', take(stream, 4))
        for _ in range(read_compact(stream)):
            take(stream, 36)
            take(stream, read_compact(stream))
            take(stream, 4)
        for _ in range(read_compact(stream)):
            take(stream, 8)
            take(stream, read_compact(stream))
        take(stream, 4)
        if (version & 0xffff) >= 3 and version >> 16:
            take(stream, read_compact(stream))
        transactions.append(stream.getvalue()[start:stream.tell()])
    if stream.read():
        raise ValueError('trailing block bytes')
    return transactions


def chainlock_spork():
    source = REPO / 'dashd-masternode/tests/fixtures/dashd-v24/sporks-receive.txt'
    for line in source.read_text().splitlines():
        if line.startswith('< 1 spork '):
            raw = bytes.fromhex(line.split()[3])
            if struct.unpack('<iq', raw[:12]) == (10018, 0):
                return raw
    raise RuntimeError('recorded ChainLock spork not found')


def free_ports(count=2, low=12000, high=20000):
    """Probe every port used by a run, keeping them bound until selection ends."""
    for base in range(low, high - count):
        held = []
        try:
            for port in range(base, base + count):
                s = socket.socket()
                held.append(s)
                s.bind(('127.0.0.1', port))
            return base
        except OSError:
            pass
        finally:
            for s in held:
                s.close()
    raise RuntimeError('no free ports below 32768')


def inventory(kind, hashes):
    return p2a.compact(len(hashes)) + b''.join(
        struct.pack('<I', kind) + bytes.fromhex(h)[::-1] for h in hashes)


class Node(p2a.Node):
    def __init__(self, *args, **kwargs):
        try:
            super().__init__(*args, **kwargs)
        except BaseException:
            if hasattr(self, 'proc'):
                self.stop()
                self.proc.wait(timeout=60)
            raise

    def stop(self):
        if self.proc.poll() is None:
            super().stop()
        else:
            self.stderr.close()
        self.proc.wait(timeout=60)


class Transcript(p2a.Transcript):
    def __init__(self, name, node, now):
        super().__init__(name, node, [], ibd=False)
        # Bootstrap blocks are stored once, rather than repeated in ten files.
        self.lines = [line for line in self.lines if not line.startswith('chain ')]
        self.lines.insert(2, '# bootstrap: bootstrap.json.gz; blocks in height order')
        self.lines.append(f'time {now}')
        self.now = now
        self.observed_logs = []
        self.peer_ids = {}
        self.scores = {}
        self.log_offset = (node.dir / 'regtest/debug.log').stat().st_size

    def _record(self, peer, msgs):
        for cmd, payload in msgs:
            if cmd == 'govsync':
                bloom = io.BytesIO(payload[32:])
                size = read_compact(bloom)
                take(bloom, size)
                funcs, tweak = struct.unpack('<I4s', take(bloom, 8))
                if funcs:
                    self.lines.append('rand bloom ' + tweak.hex())
            super()._record(peer, [(cmd, payload)])

    def rpc_record(self, method, *params, allow_error=False):
        if method in ('invalidateblock', 'reconsiderblock', 'mnsync') and params[0] != 'status':
            self.lines.append(method + ' ' + str(params[0]))
        elif method == 'generatetodescriptor':
            self.lines.append(f'generate {params[0]} {params[1]}')
        try:
            result = {'result': self.node.rpc(method, *params)}
        except RuntimeError as e:
            if not allow_error:
                raise
            result = {'error': str(e)}
        self.lines.append('check rpc ' + encoded({'method': method, 'params': params, **result}))
        return result

    def checkpoint(self, label):
        self.node.rpc('syncwithvalidationinterfacequeue')
        objects = self.node.rpc('gobject', 'list', 'all')
        votes = {h: self.node.rpc('gobject', 'getcurrentvotes', h) for h in sorted(objects)}
        height = self.node.rpc('getgovernanceinfo')['nextsuperblock']
        self.lines.append('check gobject ' + encoded({
            'label': label, 'objects': objects, 'votes': votes,
            'budget': {'height': height, 'amount': self.node.rpc('getsuperblockbudget', height)},
            'sync': self.node.rpc('mnsync', 'status'), 'chain': self.node.rpc('getblockchaininfo')}))
        self.score(label)

    def score(self, label):
        self.logs()
        live = {p['id']: p for p in self.node.rpc('getpeerinfo')}
        for n, core_id in self.peer_ids.items():
            if core_id in live:
                self.scores[n] = live[core_id]['banscore']
        self.lines.append('check score ' + encoded({
            'label': label, 'peers': {str(n): {'core_id': core_id, 'score': self.scores[n],
                                            'connected': core_id in live}
                                     for n, core_id in self.peer_ids.items()}}))

    def block(self, block):
        """Record both RPC verdict and lookup for these exact block bytes."""
        import dash_hash
        h = dash_hash.getPoWHash(bytes.fromhex(block[:160]))[::-1].hex()
        self.lines.append('submitblock ' + block)
        result = self.rpc_record('submitblock', block, allow_error=True)
        self.rpc_record('getblock', h, 1, allow_error=True)
        self.settle()
        return result

    def logs(self):
        with (self.node.dir / 'regtest/debug.log').open('rb') as f:
            f.seek(self.log_offset)
            complete, _, _ = f.read().rpartition(b'\n')
            if not complete:
                return
            self.log_offset += len(complete) + 1
            lines = complete.decode(errors='strict').splitlines()
        for line in lines:
            match = re.search(r'Misbehaving: peer=(\d+) \(\d+ -> (\d+)\)', line)
            if match:
                for n, core_id in self.peer_ids.items():
                    if core_id == int(match[1]):
                        self.scores[n] = int(match[2])
            keep = ('Misbehaving:', 'Rejected vote', 'MNGOVERNANCEOBJECT --',
                    'malformed governance', 'CSuperblock::IsExpired',
                    'CGovernanceManager::UpdatedBlockTip -- nCachedBlockHeight:',
                    'SuperblockManager::Clean -- superblock status =',
                    'validation of chainlocked block=')
            if self.name == 'R9':
                keep += ('Preparing masternode indexes and governance triggers',
                         'Masternode indexes and governance triggers prepared',
                         'CSuperblock --')
            if any(s in line for s in keep):
                self.observed_logs.append('log ' + encoded(line))

    def flush_logs(self):
        self.logs()
        self.lines.extend(self.observed_logs)
        self.observed_logs.clear()

    def write(self, out_dir):
        self.node.stop()
        self.flush_logs()
        self.lines = [line.rstrip() for line in self.lines]
        super().write(out_dir)

    def advance(self, seconds):
        self.now += seconds
        self.set_time(self.now)
        self.lines.append(f'mockscheduler {seconds}')
        self.node.rpc('mockscheduler', seconds)
        self.settle()
        for n, peer in self.peers.items():
            if peer.last_ping is not None and not peer.closed:
                self.pong(n)

    def peer(self, n=1, outbound=False, offset=None):
        if outbound:
            self.connect(n, 'manual' if offset is not None else 'outbound-full-relay',
                         f'127.0.0.{n}' if offset is not None else '127.0.0.1')
        else:
            self.accept(n)
        self.handshake(n, p2a.FULL, offset)
        addr = '{}:{}'.format(*self.peers[n].sock.getsockname())
        info = next(p for p in self.node.rpc('getpeerinfo') if p['addr'] == addr)
        self.peer_ids[n] = info['id']
        self.scores[n] = info['banscore']
        return n

    def handshake(self, n, services, offset=None):
        raw = bytearray(p2a.version_payload(services, p2a.VERSION_NONCE + n))
        if offset is not None:
            struct.pack_into('<q', raw, 12, self.now + offset)
        self.send(n, 'version', bytes(raw))
        deadline = time.monotonic() + 120
        while not self.peers[n].got_verack:
            if self.peers[n].closed or time.monotonic() > deadline:
                raise RuntimeError(f'{self.name}: peer {n} handshake failed')
            self.settle()
        self.send(n, 'verack')

    def connect(self, n, conn_type, host='127.0.0.1'):
        with socket.socket() as listener:
            listener.bind((host, free_ports(1, 24000, 26000)))
            listener.listen(1)
            self.lines.append(f'connect {n} {conn_type} 0')
            address = f'{host}:{listener.getsockname()[1]}'
            if conn_type == 'manual':
                self.node.rpc('addnode', address, 'onetry', False)
            else:
                self.node.rpc('addconnection', address, conn_type, False)
            listener.settimeout(120)
            sock, _ = listener.accept()
        self.peers[n] = p2a.Peer(n, sock)
        self.settle()

    def finish_sync(self):
        for _ in range(4):
            if self.node.rpc('mnsync', 'status')['IsSynced']:
                return
            self.rpc_record('mnsync', 'next')
        raise RuntimeError('mnsync did not finish')

    def generate(self, count):
        hashes = self.rpc_record('generatetodescriptor', count, 'raw(51)')['result']
        for h in hashes:
            raw = self.node.rpc('getblock', h, 0)
            self.block(raw)
        return hashes

    def request(self, kind, hashes, n=1):
        start = len(self.lines)
        self.send(n, 'inv', inventory(kind, hashes))
        for _ in range(30):
            replies = [line for line in self.lines[start:] if line.startswith(f'< {n} getdata ')]
            if all(any(hbytes(h) in line for line in replies) for h in hashes):
                return
            self.settle()
        raise RuntimeError(f'{self.name}: dashd did not request every announced item')

    def deliver(self, kind, h, raw, n=1, requested=True):
        if requested:
            self.request(kind, [h], n)
        self.send(n, 'govobj' if kind == 17 else 'govobjvote', bytes.fromhex(raw))
        self.logs()


def hbytes(h):
    return bytes.fromhex(h)[::-1].hex()


@contextlib.contextmanager
def observed(name, data):
    p2a.PORT_BASE = free_ports()
    previous_root = p2a.ROOT
    run_root = Path(tempfile.mkdtemp(prefix=f'{name}-', dir=ROOT))
    p2a.ROOT = run_root
    node = None
    try:
        node = Node(name, 0, SIM_START, extra=CHAIN_ARGS + ['-disablewallet'])
        now = data['time']
        node.rpc('setmocktime', now)
        node.bootstrap_answers = []
        for block in data['blocks'][:data['tip']]:
            answer = node.rpc('submitblock', block)
            if answer is not None:
                raise RuntimeError(f'bootstrap block rejected: {answer}')
            if name.split('-')[0] in ('R6', 'R7', 'R8', 'R9'):
                h = node.rpc('getbestblockhash')
                node.bootstrap_answers.append({'submitblock': answer, 'getblock': node.rpc('getblock', h, 1)})
        yield node, now
    finally:
        if node is not None:
            node.stop()
            node.proc.wait(timeout=60)
        p2a.ROOT = previous_root
        shutil.rmtree(run_root)


def populate(t, data, votes=True):
    objects = sorted(data['objects'].items())
    t.request(17, [h for h, _ in objects])
    for h, obj in objects:
        t.deliver(17, h, obj['hex'], requested=False)
    if votes:
        items = sorted(data['votes'].items())
        t.request(18, [h for h, _ in items])
        for h, vote in items:
            t.deliver(18, h, vote['hex'], requested=False)
    t.advance(301)


def write_block_answers(out, name, node):
    with gzip.GzipFile(out / f'{name}.bootstrap.json.gz', 'wb', mtime=0) as f:
        f.write((encoded(node.bootstrap_answers) + '\n').encode())


def record(name, data, out):
    with observed(name, data) as (node, now):
        t = Transcript(name, node, now)
        t.peer()
        if name != 'R5':
            t.finish_sync()
        t.checkpoint('start')
        proposals = [(h, o) for h, o in sorted(data['objects'].items()) if o['type'] == 1]
        triggers = [(h, o) for h, o in sorted(data['objects'].items()) if o['type'] == 2]
        if name == 'R1':
            t.peer(2)
            # The first object is sent unsolicited, then announced and sent again.
            h, obj = proposals[0]
            t.deliver(17, h, obj['hex'], requested=False)
            t.checkpoint('unrequested-object')
            t.request(17, [h])
            t.send(2, 'inv', inventory(17, [h]))
            t.checkpoint('two-announcers')
            t.deliver(17, h, obj['hex'], requested=False)
            t.checkpoint('proposal')
            t.deliver(17, h, obj['hex'], n=2, requested=False)
            t.checkpoint('other-announcer-forgotten')
            for h, obj in triggers:
                t.deliver(17, h, obj['hex'])
            remaining = proposals[1:]
            t.request(17, [h for h, _ in remaining])
            frames = []
            for h, obj in remaining:
                t.lines.append('> 1 govobj ' + obj['hex'])
                frames.append(p2a.frame('govobj', bytes.fromhex(obj['hex'])))
            t.peers[1].sock.sendall(b''.join(frames))
            t.settle()
            t.advance(2)
            t.checkpoint('trigger-and-relay')
            # A proposal from dashd whose collateral is not on this chain.
            h, obj = data['unconfirmed']
            t.deliver(17, h, obj['hex'])
            t.checkpoint('zero-confirmations')
        elif name == 'R2':
            populate(t, data)
            t.peer(2)
            t.peer(3)

            def sync(payload, seconds=0):
                start = len(t.lines)
                t.send(2, 'govsync', payload)
                if seconds:
                    t.advance(seconds)
                replies = []
                for line in t.lines[start:]:
                    fields = line.split()
                    if (len(fields) == 4 and fields[0] == '<' and fields[2] == 'inv'
                            and fields[1] != '2'):
                        replies.append('# declared-difference: R2-govsync-cross-peer '
                                       f'requester=2 recipient={fields[1]}')
                    replies.append(line)
                t.lines[start:] = replies

            sync(bytes(42), 1)
            t.checkpoint('full-sync')
            sync(bytes(42))
            t.checkpoint('full-sync-repeat')
            h = proposals[0][0]
            sync(bytes.fromhex(h)[::-1] + bytes(10))
            t.checkpoint('object-fetch')
            # One-byte, one-function bloom, then the same tracked vote sync again.
            bloom = b'\x01\x00' + struct.pack('<II', 1, 0) + b'\x00'
            for _ in range(2):
                sync(bytes.fromhex(h)[::-1] + bloom, 1)
                t.checkpoint('vote-sync')
            t.peer(5)
            absent = '01' * 32
            if absent in data['objects']:
                raise RuntimeError('absent parent unexpectedly exists')
            for i in range(2):
                t.send(5, 'govsync', bytes.fromhex(absent)[::-1] + bytes(10))
                t.checkpoint(f'unknown-parent-{i + 1}')
            t.peer(4)
            t.send(4, 'govsync', bytes(32) + p2a.compact(36001) + bytes(36001) + bytes(9))
            t.checkpoint('oversized-bloom')
            t.rpc_record('mnsync', 'reset')
            sync(bytes(42))
            t.checkpoint('unsynced')
        elif name == 'R3':
            voted = [(h, obj) for h, obj in proposals
                     if any(v['parent'] == h for v in data['votes'].values())]
            parent, obj = voted[0]
            vote_h, vote = next((h, v) for h, v in sorted(data['votes'].items()) if v['parent'] == parent)
            t.deliver(18, vote_h, vote['hex'])
            t.advance(301)
            t.checkpoint('orphan-parent-tick')
            t.deliver(17, parent, obj['hex'])
            t.checkpoint('parent-arrival')
            other_parent, other_obj = voted[1]
            vh, v = next((h, v) for h, v in sorted(data['votes'].items()) if v['parent'] == other_parent)
            t.deliver(18, vh, v['hex'])
            t.advance(601)
            t.deliver(17, other_parent, other_obj['hex'])
            t.checkpoint('orphan-expiry')
            t.peer(2)
            t.send(2, 'govobjvote', b'\x00')
            t.checkpoint('malformed-vote')
            bad_h, bad_vote = next((h, v) for h, v in sorted(data['votes'].items())
                                   if v['parent'] == voted[0][0] and h != vote_h)
            raw = bytearray.fromhex(bad_vote['hex'])
            raw[-1] ^= 1
            t.peer(3)
            t.deliver(18, bad_h, raw.hex(), n=3)
            t.checkpoint('bad-signature')
            t.rpc_record('mnsync', 'reset')
            t.rpc_record('mnsync', 'next')
            status = node.rpc('mnsync', 'status')
            if not status['IsBlockchainSynced'] or status['IsSynced']:
                raise RuntimeError('bad-signature subcase requires partial sync')
            other_h, other_vote = next((h, v) for h, v in sorted(data['votes'].items())
                                       if v['parent'] == parent and h not in (vote_h, bad_h))
            raw = bytearray.fromhex(other_vote['hex'])
            raw[-1] ^= 1
            t.peer(4)
            t.checkpoint('partial-sync-before-bad-signature')
            t.deliver(18, other_h, raw.hex(), n=4)
            t.checkpoint('partial-sync-bad-signature')
        elif name == 'R4':
            populate(t, data)
            t.block(data['superblock']['hex'])
            h, obj = data['unconfirmed']
            t.deliver(17, h, obj['hex'])
            t.checkpoint('collateral-zero')
            for i, block in enumerate(data['collateral_blocks'], 1):
                t.block(block)
                if i == 1:
                    t.deliver(17, h, obj['hex'])
                t.checkpoint(f'collateral-{i}')
        elif name == 'R5':
            # Advance the same scheduler used by SP1, with real objects present.
            t.finish_sync()
            populate(t, data)
            t.rpc_record('mnsync', 'reset')
            # Reset clears reached-best-header; a tip update restores it.
            t.advance(1)
            t.generate(1)
            t.peer(2, outbound=True)
            t.peer(3, outbound=True)
            for _ in range(120):
                start = len(t.lines)
                t.advance(1)
                requests = [line.split() for line in t.lines[start:]
                            if line.startswith('< ') and ' govsync ' in line]
                for _, peer, _, raw in requests:
                    h = bytes.fromhex(raw[:64])[::-1].hex()
                    if h == '0' * 64:
                        count = len(t.rpc_record('gobject', 'list', 'all')['result'])
                        item = 10
                    else:
                        count = len(t.rpc_record('gobject', 'getcurrentvotes', h)['result'])
                        item = 11
                    t.send(int(peer), 'ssc', struct.pack('<ii', item, count))
                if t.rpc_record('mnsync', 'status')['result']['IsSynced']:
                    break
            else:
                raise RuntimeError('R5 did not reach FINISHED')
            t.checkpoint('sync-finished')
            t.advance(6)
            t.checkpoint('votes-after-finished')
        elif name in ('R6', 'R7', 'R8', 'R9'):
            populate(t, data)
            t.checkpoint('funded-trigger')
            if name == 'R7':
                t.rpc_record('mnsync', 'reset')
                t.checkpoint('unsynced')
            if name in ('R6', 'R7'):
                for label, raw in sorted(data['block_variants'].items()):
                    t.block(raw)
                    t.checkpoint(label)
                if name == 'R7':
                    # Judge the source sibling on its own after the accepted variant.
                    t.rpc_record('invalidateblock', node.rpc('getbestblockhash'))
            if name == 'R9':
                t.rpc_record('getblocktemplate')
                for p in t.peers.values():
                    p.sock.close()
                t.peers.clear()
                node.stop()
                node.proc.wait(timeout=60)
                t.flush_logs()
                shutil.copyfile(node.dir / 'regtest/governance.dat', out / 'R9.governance.dat')
                t.lines.append('restart')
                t.peer_ids.clear()
                t.scores.clear()
                t.lines.append(f'time {SIM_START}')
                # Preserve the same datadir and arguments; Node's constructor clears it.
                node.stderr = open(node.dir / 'restart-stderr.txt', 'wb')  # noqa: SIM115
                node.proc = subprocess.Popen(node.args, stdout=subprocess.DEVNULL, stderr=node.stderr)
                deadline = time.monotonic() + 120
                while True:
                    try:
                        node.rpc('getblockcount')
                        break
                    except (OSError, RuntimeError):
                        if node.proc.poll() is not None or time.monotonic() > deadline:
                            raise RuntimeError('restart failed')
                        time.sleep(0.2)
                t.flush_logs()
                t.checkpoint('restart-start')
                t.rpc_record('getgovernanceinfo')
                t.rpc_record('gobject', 'list', 'all', 'triggers')
                for h, _ in triggers:
                    t.rpc_record('gobject', 'get', h)
                t.rpc_record('getblocktemplate', allow_error=True)
                t.set_time(t.now)
                t.peer()
                t.finish_sync()
                t.checkpoint('loaded')
                t.rpc_record('getblocktemplate')
            t.block(data['superblock']['hex'])
            t.checkpoint('superblock')
            if name == 'R8':
                t.rpc_record('invalidateblock', data['superblock']['hash'])
                t.checkpoint('pure-disconnect')
                t.advance(1)
                alternate = t.generate(2)
                t.checkpoint('alternate-branch')
                t.rpc_record('reconsiderblock', data['superblock']['hash'])
                t.rpc_record('invalidateblock', alternate[0])
                t.checkpoint('reconnected')
                t.advance(1)
                t.generate(20)
                t.advance(301)
                t.checkpoint('expiry-boundary')
                t.generate(1)
                t.advance(301)
                t.checkpoint('cycle-expiry')
        elif name == 'R10':
            populate(t, data)
            t.block(data['superblock']['hex'])
            for block in data['collateral_blocks']:
                t.block(block)
            t.checkpoint('before-key-change')
            for label, block in data['key_change_blocks']:
                tx_type = 0 if label == 'masternode-removal' else 3
                matches = [tx for tx in block_transactions(block)
                           if int.from_bytes(tx[:4], 'little') >> 16 == tx_type]
                if len(matches) != 1:
                    raise RuntimeError('ambiguous source mutation transaction')
                raw = matches[0]
                t.send(1, 'tx', raw)
                txid = p2a.sha256d(raw)[::-1].hex()
                if txid not in t.rpc_record('getrawmempool')['result']:
                    raise RuntimeError(f'mutation transaction not accepted: {label}')
                t.generate(1)
                t.checkpoint(label)
                if label == 'operator-key-change':
                    t.rpc_record('protx', 'info', data['source']['masternodes'][2]['proTxHash'])
            target = node.rpc('getgovernanceinfo')['nextsuperblock']
            t.generate(target - node.rpc('getblockcount'))
            t.checkpoint('next-superblock')
        else:
            raise ValueError(name)
        t.write(out)
        if name in ('R6', 'R7', 'R8', 'R9'):
            write_block_answers(out, name, node)
    if name == 'R8':
        record_future_reorg(data, out)
    if name == 'R6':
        record_competing(data, out)
    if name == 'R3':
        record_offset(data, out)
    if name == 'R7' and 'error' not in data['chainlock']:
        record_chainlock(data, out)


def record_chainlock(data, out):
    inputs = data['chainlock']
    for locked in (False, True):
        name = 'R7-chainlocked' if locked else 'R7-control'
        with observed(name, inputs) as (node, now):
            t = Transcript(name, node, now)
            t.peer()
            t.finish_sync()
            t.send(1, 'spork', chainlock_spork())
            if t.rpc_record('spork', 'show')['result']['SPORK_19_CHAINLOCKS_ENABLED'] != 0:
                raise RuntimeError('ChainLock spork was not activated')
            populate(t, inputs)
            t.checkpoint('fully-synced-funded-control')
            t.rpc_record('quorum', 'list')
            if locked:
                t.send(1, 'headers', b'\x01' + bytes.fromhex(inputs['block'][:160]) + b'\x00')
                lock_hash = p2a.sha256d(bytes.fromhex(inputs['clsig']))[::-1].hex()
                t.request(29, [lock_hash])
                t.send(1, 'clsig', bytes.fromhex(inputs['clsig']))
                deadline = time.monotonic() + 30
                while True:
                    try:
                        lock = node.rpc('getbestchainlock')
                        if lock['blockhash'] == inputs['lock']['blockhash']:
                            break
                    except RuntimeError:
                        pass
                    if time.monotonic() > deadline:
                        raise RuntimeError('observed node did not verify ChainLock')
                    time.sleep(0.1)
                t.rpc_record('getbestchainlock')
                t.checkpoint('chainlock-before-body')
            result = t.block(inputs['block'])['result']
            t.checkpoint('block-verdict')
            t.write(out)
            write_block_answers(out, name, node)
            if result != (None if locked else 'bad-cb-amount'):
                raise RuntimeError(f'ChainLock differential failed: {result}')


def record_competing(data, out):
    inputs = data['competition']
    for phase in ('rank', 'tie'):
        name = 'R6-' + phase
        with observed(name, data) as (node, now):
            t = Transcript(name, node, now)
            t.peer()
            t.finish_sync()
            for h, obj in sorted(inputs['objects'].items()):
                t.deliver(17, h, obj['hex'])
            votes = dict(inputs['rank_votes'])
            if phase == 'tie':
                votes.update(inputs['tie_votes'])
            for h, vote in sorted(votes.items()):
                t.deliver(18, h, vote['hex'])
            t.advance(301)
            t.checkpoint('competing-' + phase)
            winner = inputs['high'] if phase == 'rank' else inputs['low']
            loser = inputs['low'] if phase == 'rank' else inputs['high']
            if t.block(inputs['blocks'][loser])['result'] != 'bad-cb-amount':
                raise RuntimeError('losing trigger block did not fail')
            if t.block(inputs['blocks'][winner])['result'] is not None:
                raise RuntimeError('winning trigger block did not pass')
            t.checkpoint('selected-' + phase)
            t.write(out)
            write_block_answers(out, name, node)


def record_offset(data, out):
    with observed('R3-offset', data) as (node, now):
        t = Transcript('R3-offset', node, now)
        t.peer()
        t.finish_sync()
        for n in range(2, 6):
            t.peer(n, outbound=True, offset=600)
        offset = t.rpc_record('getnetworkinfo')['result']['timeoffset']
        if offset != 600:
            raise RuntimeError(f'expected offset 600, got {offset}')
        t.lines.append(f'timeoffset {offset}')
        parent = min(v['parent'] for v in data['votes'].values()
                     if data['objects'][v['parent']]['type'] == 1)
        vh, vote = next((h, v) for h, v in sorted(data['votes'].items()) if v['parent'] == parent)
        t.deliver(18, vh, vote['hex'])
        t.checkpoint('offset-orphan')
        t.set_time(now + 300)
        t.deliver(17, parent, data['objects'][parent]['hex'])
        t.checkpoint('offset-parent-before-wall-deadline')
        node.rpc('gobject', 'get', parent)
        if node.rpc('gobject', 'getcurrentvotes', parent):
            raise RuntimeError('adjusted-time expired vote was applied')
        t.write(out)


def record_future_reorg(data, out):
    with observed('R8-future', data) as (node, now):
        t = Transcript('R8-future', node, now)
        t.peer()
        t.finish_sync()
        populate(t, data)
        t.checkpoint('before-deep-reorg')
        h = node.rpc('getblockhash', 139)
        t.rpc_record('invalidateblock', h)
        t.checkpoint('deep-pure-disconnect')
        t.advance(1)
        t.generate(1)
        t.checkpoint('alternate-tip-139')
        t.advance(1)
        t.generate(1)
        t.advance(301)
        t.checkpoint('far-future-clean')
        t.write(out)
        write_block_answers(out, 'R8-future', node)


def parse_records(text):
    records = []
    peers = set()
    pending_rand = None

    def require(ok, message):
        if not ok:
            raise ValueError(message)

    def hex_bytes(value, size=None):
        require(re.fullmatch(r'(?:[0-9a-f]{2})*', value) is not None, 'invalid hex')
        raw = bytes.fromhex(value)
        require(size is None or len(raw) == size, 'wrong hex length')
        return raw

    def number(value, minimum=0):
        require(re.fullmatch(r'0|[1-9][0-9]*', value) is not None, 'invalid integer')
        require(int(value) >= minimum, 'integer outside range')
        return int(value)

    for index, line in enumerate(text.splitlines(), 1):
        try:
            if line.startswith('#') or not line:
                continue
            fields = line.split(' ')
            op = fields[0]
            value = fields[1:]
            if op in ('mocktime', 'time', 'mockscheduler'):
                require(len(value) == 1, 'clock arity')
                number(value[0])
            elif op == 'ibd':
                require(value in (['0'], ['1']), 'IBD must be 0 or 1')
            elif op in ('invalidateblock', 'reconsiderblock'):
                require(len(value) == 1, 'block action arity')
                hex_bytes(value[0], 32)
            elif op == 'submitblock':
                require(len(value) == 1, 'block arity')
                require(len(hex_bytes(value[0])) >= 81, 'short block')
            elif op == 'restart':
                require(not value, 'restart arity')
                peers.clear()
            elif op == 'mnsync':
                require(value in (['next'], ['reset']), 'invalid sync action')
            elif op == 'generate':
                require(len(value) == 2 and value[1] == 'raw(51)', 'mining arguments')
                number(value[0], 1)
            elif op == 'accept':
                require(len(value) == 1, 'accept arity')
                n = number(value[0], 1)
                require(n not in peers, 'duplicate peer')
                peers.add(n)
            elif op == 'connect':
                require(len(value) == 3, 'connect arity')
                n = number(value[0], 1)
                require(n not in peers, 'duplicate peer')
                require(value[1] in ('outbound-full-relay', 'manual'), 'connection type')
                number(value[2])
                peers.add(n)
            elif op in ('>', '<'):
                require(len(value) in (2, 3), 'message arity')
                require(number(value[0], 1) in peers, 'unknown peer')
                command = value[1]
                if op == '<' and command == 'closed':
                    require(len(value) == 2, 'closed payload')
                else:
                    require(re.fullmatch('[a-z0-9]{1,12}', command) is not None, 'command')
                    payload = value[2] if len(value) == 3 else ''
                    if not (op == '>' and command == 'pong' and payload == 'echo'):
                        raw = hex_bytes(payload)
                        if op == '<' and command == 'govsync':
                            stream = io.BytesIO(raw)
                            take(stream, 32)
                            size = read_compact(stream)
                            require(size <= 36000, 'oversized outgoing bloom')
                            take(stream, size)
                            funcs, tweak, flags = struct.unpack('<I4sB', take(stream, 9))
                            require(not stream.read() and funcs <= 50 and flags <= 2, 'bloom encoding')
                            if funcs:
                                require(pending_rand == tweak.hex(), 'missing or mismatched bloom input')
                                pending_rand = None
            elif op == 'rand':
                require(len(value) == 2 and value[0] == 'bloom', 'random input kind')
                hex_bytes(value[1], 4)
                require(pending_rand is None, 'unused random input')
                pending_rand = value[1]
            elif op == 'check':
                kind, sep, payload = line[6:].partition(' ')
                require(bool(sep), 'missing check payload')
                data = json.loads(payload)
                require(isinstance(data, dict), 'check must be an object')
                if kind == 'gobject':
                    require(set(data) == {'label', 'objects', 'votes', 'budget', 'sync', 'chain'}, 'object check keys')
                    require(isinstance(data['label'], str) and bool(data['label']), 'checkpoint label')
                    require(isinstance(data['objects'], dict) and isinstance(data['votes'], dict), 'object/vote maps')
                    require(data['objects'].keys() == data['votes'].keys(), 'vote snapshot coverage')
                    for h in data['objects']:
                        hex_bytes(h, 32)
                        require(isinstance(data['objects'][h], dict) and isinstance(data['votes'][h], dict), 'object/vote values')
                        obj = data['objects'][h]
                        require(obj.get('Hash') == h and type(obj.get('ObjectType')) is int
                                and obj['ObjectType'] in (1, 2), 'object identity')
                        require(isinstance(obj.get('DataString'), str), 'object data')
                        for field in ('YesCount', 'NoCount', 'AbsoluteYesCount'):
                            require(type(obj.get(field)) is int, 'vote count')
                        for vh in data['votes'][h]:
                            hex_bytes(vh, 32)
                            require(isinstance(data['votes'][h][vh], str), 'vote text')
                    require(isinstance(data['budget'], dict) and set(data['budget']) == {'height', 'amount'}, 'budget keys')
                    require(type(data['budget']['height']) is int and data['budget']['height'] >= 0, 'budget height')
                    require(type(data['budget']['amount']) in (int, float) and data['budget']['amount'] >= 0, 'budget amount')
                    require(isinstance(data['sync'], dict) and isinstance(data['chain'], dict), 'status objects')
                    require(type(data['sync'].get('IsSynced')) is bool, 'sync status')
                    require(type(data['sync'].get('IsBlockchainSynced')) is bool, 'blockchain sync status')
                    require(type(data['chain'].get('blocks')) is int and data['chain']['blocks'] >= 0, 'chain height')
                    hex_bytes(data['chain']['bestblockhash'], 32)
                elif kind == 'score':
                    require(set(data) == {'label', 'peers'} and isinstance(data['label'], str) and bool(data['label']), 'score check keys')
                    require(isinstance(data['peers'], dict), 'peer scores')
                    for n, score in data['peers'].items():
                        require(number(n, 1) in peers, 'unknown score peer')
                        require(isinstance(score, dict) and set(score) == {'core_id', 'score', 'connected'}, 'score fields')
                        require(type(score['score']) is int and score['score'] >= 0, 'score value')
                        require(type(score['core_id']) is int and score['core_id'] >= 0, 'core peer id')
                        require(type(score['connected']) is bool, 'connection state')
                elif kind == 'rpc':
                    require(set(data) in ({'method', 'params', 'result'}, {'method', 'params', 'error'}), 'RPC check keys')
                    require(data['method'] in ('mnsync', 'gobject', 'getsuperblockbudget',
                            'getblockchaininfo', 'getgovernanceinfo', 'getblocktemplate',
                            'getblock', 'submitblock', 'generatetodescriptor', 'invalidateblock',
                            'reconsiderblock', 'getbestchainlock', 'submitchainlock', 'quorum',
                            'getnetworkinfo', 'protx', 'getrawmempool', 'spork'), 'RPC method')
                    require(isinstance(data['params'], list), 'RPC parameters')
                    require('error' not in data or isinstance(data['error'], str), 'RPC error')
                else:
                    raise ValueError('unknown check kind')
                records.append(('check', kind, data))
                continue
            elif op == 'log':
                entry = json.loads(line[4:])
                require(isinstance(entry, str), 'log must be a JSON string')
                records.append(('log', entry))
                continue
            elif op == 'timeoffset':
                require(len(value) == 1 and re.fullmatch(r'-?(0|[1-9][0-9]*)', value[0]) is not None, 'time offset')
            else:
                raise ValueError('unknown record')
            records.append(tuple(fields))
        except (ValueError, KeyError, TypeError, struct.error) as e:
            raise ValueError(f'line {index}: {e}: {line[:100]}') from e
    require(pending_rand is None, 'unused random input at end')
    return records


def selftest(out):
    files = sorted(out.glob('R*.txt'))
    if not files:
        raise RuntimeError('no fixtures to validate')
    total = 0
    captures = {}
    for path in files:
        records = parse_records(path.read_text())
        captures[path.stem] = records
        checks = [r for r in records if r[:2] == ('check', 'gobject')]
        scores = [r for r in records if r[:2] == ('check', 'score')]
        if not checks or [r[2]['label'] for r in checks] != [r[2]['label'] for r in scores]:
            raise ValueError(f'{path.name}: missing checkpoint scores')
        total += len(records)
    invalid = ['rpc {}', 'checkpoint old', 'mockscheduler -1', 'time 1 2',
               'restart extra', 'invalidateblock 00', 'rand bloom 00', 'rand other 00000000',
               'check unknown {}', 'check gobject {}', 'check score {"label":"x","peers":{"1":{}}}',
               'accept 1\naccept 1', 'accept 1\n> 2 ping 00', 'log {}',
               'accept 1\n< 1 govsync 00', 'rand bloom 00000000']
    bloom = '00' * 32 + '0100010000000102030400'
    invalid.extend(['accept 1\n< 1 govsync ' + bloom,
                    'accept 1\nrand bloom 00000000\n< 1 govsync ' + bloom,
                    'accept 1\nrand bloom 01020304\n< 1 govsync ' + bloom + '00',
                    'check rpc {"method":"unknown","params":[],"result":null}',
                    'accept 1\ncheck score {"label":"x","peers":{"1":null}}',
                    'timeoffset 600 extra', 'timeoffset 6.0', 'timeoffset +600'])
    for text in invalid:
        try:
            parse_records(text)
        except ValueError:
            continue
        raise AssertionError(f'malformed control accepted: {text}')
    parse_records('accept 1\n> 1 verack\n< 1 verack\nmockscheduler 300\ntimeoffset 600\ntimeoffset -600\n'
                  'rand bloom 01020304\n< 1 govsync ' + bloom + '\n'
                  'invalidateblock ' + '00' * 32 + '\nrestart\naccept 1\n')
    if all(f'R{i}' in captures for i in range(1, 11)):
        capture_contract(captures)
    print(f'--selftest: {len(files)} fixtures, {total} records; {len(invalid)} negative controls passed')


def capture_contract(captures):
    def point(name, kind, label):
        return next(r[2] for r in captures[name]
                    if r[:2] == ('check', kind) and r[2]['label'] == label)

    def verdicts(name):
        return [r[2].get('result') for r in captures[name]
                if r[:2] == ('check', 'rpc') and r[2]['method'] == 'submitblock']

    def vote_count(label):
        return sum(len(v) for v in point('R10', 'gobject', label)['votes'].values())

    assert sum('Received unrequested object:' in r[1] for r in captures['R1']
               if r[0] == 'log') >= 2, 'R1 global forgetting observation'
    for label in ('unknown-parent-1', 'unknown-parent-2'):
        assert point('R2', 'score', label)['peers']['5']['score'] == 0
    status = point('R3', 'gobject', 'partial-sync-bad-signature')['sync']
    assert status['IsBlockchainSynced'] and not status['IsSynced']
    assert point('R3', 'score', 'bad-signature')['peers']['3']['score'] == 20
    assert point('R3', 'score', 'partial-sync-bad-signature')['peers']['4']['score'] == 0
    assert ('timeoffset', '600') in captures['R3-offset']
    offset_rpc = next(r[2] for r in captures['R3-offset'] if r[:2] == ('check', 'rpc')
                      and r[2]['method'] == 'getnetworkinfo')
    assert offset_rpc['result']['timeoffset'] == 600
    offset_records = captures['R3-offset']
    outbound = [r[1] for r in offset_records if r[0] == 'connect']
    assert len(outbound) == 4
    start_time = next(int(r[1]) for r in offset_records if r[0] == 'time')
    for peer in outbound:
        version = next(r[3] for r in offset_records if r[:3] == ('>', peer, 'version'))
        assert struct.unpack_from('<q', bytes.fromhex(version), 12)[0] == start_time + 600
    orphan = next(i for i, r in enumerate(offset_records) if r[:3] == ('>', '1', 'govobjvote'))
    parent = next(i for i, r in enumerate(offset_records) if r[:3] == ('>', '1', 'govobj'))
    assert offset_records.index(('check', 'rpc', offset_rpc)) < orphan < parent
    assert ('time', str(start_time + 300)) in offset_records[orphan:parent]
    assert not any(r[0] == 'mockscheduler' for r in offset_records[orphan:parent])
    assert all(not v for v in point('R3-offset', 'gobject',
               'offset-parent-before-wall-deadline')['votes'].values())
    for phase in ('rank', 'tie'):
        name = 'R6-' + phase
        objects = point(name, 'gobject', 'competing-' + phase)['objects']
        low, high = sorted(objects, key=hbytes)
        assert (objects[low]['YesCount'], objects[low]['NoCount']) == ((4, 3) if phase == 'rank' else (5, 3))
        assert (objects[high]['YesCount'], objects[high]['NoCount']) == (2, 0)
        assert verdicts(name) == ['bad-cb-amount', None]
    counts = [vote_count(label) for label in ('before-key-change', 'voting-key-change',
                                            'operator-key-change', 'masternode-removal')]
    assert all(a > b for a, b in pairwise(counts)), counts
    final = point('R10', 'gobject', 'next-superblock')['chain']['blocks']
    assert final % 20 == 0 and verdicts('R10')[-1] == 'duplicate'
    assert any(r[:2] == ('check', 'rpc') and r[2]['method'] == 'protx'
               and 'result' in r[2] for r in captures['R10'])
    if 'R7-chainlocked' in captures:
        assert verdicts('R7-control') == ['bad-cb-amount']
        assert verdicts('R7-chainlocked') == [None]
        assert point('R7-control', 'gobject', 'fully-synced-funded-control')['sync']['IsSynced']
        assert point('R7-chainlocked', 'gobject', 'chainlock-before-body')['sync']['IsSynced']
        assert any('validation of chainlocked block=' in r[1]
                   for r in captures['R7-chainlocked'] if r[0] == 'log')
    print('--selftest: review regression assertions passed')


def normalize(text):
    """Only transport entropy and wall-clock metadata are excluded from --check."""
    out = []
    for line in text.splitlines():
        if line.startswith('# args:'):
            line = re.sub(r'(-port=|-bind=127.0.0.1:)\d+', r'\1PORT', line)
        if line.startswith('< '):
            fields = line.split()
            if len(fields) == 4 and fields[2] == 'version':
                raw = bytearray.fromhex(fields[3])
                raw[72:80] = bytes(8)
                raw[-33:-1] = bytes(32)
                fields[3] = raw.hex()
                line = ' '.join(fields)
            elif len(fields) == 4 and fields[2] == 'ping':
                line = ' '.join(fields[:3]) + ' NONCE'
        if line.startswith('log '):
            line = re.sub(r'\d{4}-\d\d-\d\dT\S+ ', 'WALLTIME ', line)
        if line.startswith('check '):
            _, kind, payload = line.split(' ', 2)
            value = json.loads(payload)
            if kind == 'rpc' and value['method'] == 'getblockchaininfo':
                value.get('result', {}).pop('verificationprogress', None)
            if kind == 'gobject':
                value['chain'].pop('verificationprogress', None)
            line = f'check {kind} ' + encoded(value)
        out.append(line)
    return '\n'.join(out) + '\n'


def network_worker(core, out):
    """Extend the existing network builder; never duplicate its MN setup."""
    sys.path.insert(0, str(core / 'test/functional'))
    gov = load('gv1b_builder', REPO / 'dash-oracle-sys/scripts/gov_capture.py')
    from test_framework.util import PortSeed, p2p_port, rpc_port

    class Builder(gov.GovCapture):
        def set_test_params(self):
            # The framework metaclass requires this override in every subclass.
            super().set_test_params()

        def wait_until(self, test_function, timeout=60, lock=None, sleep=0.05, do_assert=True):
            # The builder's optional propagation waits need not delay input collection.
            if not do_assert:
                timeout = min(timeout, 3)
            return super().wait_until(test_function, timeout, lock, sleep, do_assert)

        def _initialize_mocktime(self, is_genesis):
            self.mocktime = SIM_START
            for node in self.nodes:
                node.mocktime = self.mocktime

        def probe(self, name, raw, hash_hex):
            # The source supplies bytes; verdicts belong to the observed transcripts.
            return {'name': name, 'vote': raw.hex(), 'hash': hash_hex}

        def move_blocks(self, nodes, num_blocks):
            # Batch mining can expire triggers before the other nodes validate them.
            for _ in range(num_blocks):
                self.bump_mocktime(1, nodes=nodes)
                if (self.nodes[0].getblockcount() + 1) % 20 == 0:
                    time.sleep(2)
                self.generate(self.nodes[0], 1, sync_fun=lambda: self.sync_blocks(nodes))

        def run_test(self):
            super().run_test()
            self.start_node(0)
            node = self.nodes[0]
            for i in range(1, len(self.nodes)):
                self.connect_nodes(i, 0)
            while not node.mnsync('status')['IsSynced']:
                node.mnsync('next')
            capture = json.loads((out / 'capture.json').read_text())
            capture['blocks'] = [node.getblock(node.getblockhash(h), 0)
                                 for h in range(1, capture['tip'] + 1)]
            capture['time'] = int(self.mocktime)
            from test_framework.messages import CBlock
            variants = {}
            for label in ('wrong-payee', 'over-budget'):
                block = CBlock()
                block.deserialize(io.BytesIO(bytes.fromhex(capture['superblock']['hex'])))
                if label == 'wrong-payee':
                    block.vtx[0].vout[-1].scriptPubKey = block.vtx[0].vout[0].scriptPubKey
                else:
                    block.vtx[0].vout[-1].nValue += int(node.getsuperblockbudget(capture['tip'] + 1) * 100000000)
                block.vtx[0].rehash()
                block.hashMerkleRoot = block.calc_merkle_root()
                block.solve()
                variants[label] = block.serialize().hex()
            capture['block_variants'] = variants
            adapter = SimpleNamespace(
                rpc=lambda method, *params: getattr(node, method)(*params),
                args=node.args + node.extra_args, chain='regtest', port=p2p_port(0),
                header=lambda h: bytes.fromhex(node.getblockheader(node.getblockhash(h), False)))
            wire = p2a.Transcript('source', adapter, [], False)
            wire.accept(1)
            wire.handshake(1, p2a.FULL)
            objects, votes = {}, {}

            def fetch(kind, h, cmd):
                start = len(wire.lines)
                wire.send(1, 'getdata', inventory(kind, [h]))
                return next(line.split(' ', 3)[3] for line in wire.lines[start:]
                            if line.startswith(f'< 1 {cmd} '))

            for h, obj in sorted(node.gobject('list', 'all').items()):
                objects[h] = {'hex': fetch(17, h, 'govobj'), 'type': obj['ObjectType']}
                for vh in sorted(node.gobject('getcurrentvotes', h)):
                    votes[vh] = {'hex': fetch(18, vh, 'govobjvote'), 'parent': h}
            capture['objects'], capture['votes'] = objects, votes
            capture['competition'] = self.isolated_inputs(
                'R6-source', capture,
                lambda oracle, wire, fetch: self.competing_inputs(node, oracle, wire, fetch, capture))
            # Build an unconfirmed proposal on the source, then let dashd serialize it.
            from test_framework.governance import prepare_object
            proposal = prepare_object(node, 1, '0', self.mocktime, 1,
                                      'GV1b_unconfirmed', '1.0', node.getnewaddress())
            self.bump_mocktime(1)
            # gobject submit requires six confirmations. Keep each mined block separately.
            capture['collateral_blocks'] = []
            for _ in range(6):
                h = self.generate(node, 1, sync_fun=lambda: self.sync_blocks())[0]
                capture['collateral_blocks'].append(node.getblock(h, 0))
            h = node.gobject('submit', '0', 1, self.mocktime - 1,
                             proposal['hex'], proposal['collateralHash'])
            capture['unconfirmed'] = [h, {'hex': fetch(17, h, 'govobj'), 'type': 1}]
            try:
                capture['chainlock'] = self.chainlock(node, fetch)
            except Exception as e:
                self.log.exception('ChainLock source extension failed')
                capture['chainlock'] = {'error': str(e), 'height': node.getblockcount(),
                                        'quorums': node.quorum('list')}
            capture['key_change_blocks'] = []
            mn = self.mninfo[0]
            node.protx('update_registrar', mn.proTxHash, '', node.getnewaddress(), '', mn.fundsAddr)
            h = self.generate(node, 1, sync_fun=lambda: self.sync_blocks())[0]
            capture['key_change_blocks'].append(['voting-key-change', node.getblock(h, 0)])
            operator = node.bls('generate')
            operator_mn = self.mninfo[2]
            node.protx('update_registrar', operator_mn.proTxHash, operator['public'], '', '', operator_mn.fundsAddr)
            h = self.generate(node, 1, sync_fun=lambda: self.sync_blocks())[0]
            capture['key_change_blocks'].append(['operator-key-change', node.getblock(h, 0)])
            txid, index = self.collateral(mn)
            raw = node.createrawtransaction([{'txid': txid, 'vout': index}],
                                            {node.getnewaddress(): 999.9999})
            signed = node.signrawtransactionwithwallet(raw)
            node.sendrawtransaction(signed['hex'])
            h = self.generate(node, 1, sync_fun=lambda: self.sync_blocks())[0]
            capture['key_change_blocks'].append(['masternode-removal', node.getblock(h, 0)])
            for peer in wire.peers.values():
                peer.sock.close()
            retained = {key: capture[key] for key in (
                'blocks', 'time', 'tip', 'objects', 'votes', 'probes', 'superblock',
                'unconfirmed', 'collateral_blocks', 'key_change_blocks', 'block_variants',
                'competition', 'chainlock')}
            retained['source'] = {'networkinfo': node.getnetworkinfo(),
                                  'masternodes': capture['masternodes'],
                                  'args': CHAIN_ARGS, 'start_mocktime': SIM_START}
            with gzip.GzipFile(out / 'bootstrap.json.gz', 'wb', mtime=0) as f:
                f.write((encoded(retained) + '\n').encode())

        def chainlock(self, node, fetch):
            from test_framework.messages import hash256

            node.sporkupdate('SPORK_17_QUORUM_DKG_ENABLED', 0)
            self.wait_for_sporks_same()
            self.mine_cycle_quorum()
            self.wait_for_chainlocked_block_all_nodes(node.getbestblockhash())
            target = node.getgovernanceinfo()['nextsuperblock']
            while node.getblockcount() < target - 1:
                self.bump_mocktime(1)
                self.generate(node, 1, sync_fun=lambda: self.sync_blocks())
            body = {
                'event_block_height': target, 'payment_addresses': node.getnewaddress(),
                'payment_amounts': '2.20000000', 'proposal_hashes': '01' * 32, 'type': 2}
            before = node.getblockcount()
            blocks = [node.getblock(node.getblockhash(i), 0) for i in range(1, before + 1)]

            def control(oracle, isolated, get_payload):
                h, raw = self.signed_trigger(self.mninfo[3], body)
                isolated.deliver(17, h, raw.hex())
                if h not in oracle.gobject('list', 'all'):
                    raise RuntimeError('source rejected ChainLock control trigger')
                obj = {'hex': get_payload(17, h, 'govobj'), 'type': 2}
                votes = dict(self.trigger_vote(oracle, isolated, get_payload, h, mn, 'yes')
                             for mn in self.mninfo[:2])
                return {'objects': {h: obj}, 'votes': votes}

            inputs = self.isolated_inputs(
                'R7-source', {'blocks': blocks, 'tip': before, 'time': int(self.mocktime)}, control)
            self.bump_mocktime(1)
            bh = self.generate(node, 1, sync_fun=lambda: self.sync_blocks())[0]
            self.wait_for_chainlocked_block_all_nodes(bh)
            lock = node.getbestchainlock()
            if lock['blockhash'] != bh:
                raise RuntimeError('unexpected ChainLock target')
            lock_h = hash256(struct.pack('<i', lock['height'])
                             + bytes.fromhex(bh)[::-1] + bytes.fromhex(lock['signature']))[::-1].hex()
            raw_lock = fetch(29, lock_h, 'clsig')
            return {'blocks': blocks, 'tip': before, 'time': int(self.mocktime),
                    **inputs, 'block': node.getblock(bh, 0),
                    'lock': lock, 'clsig': raw_lock,
                    'quorums': node.quorum('list')}

        def signed_trigger(self, mn, body):
            from py_ecc.bls import G2Basic
            from test_framework.messages import hash256, set_string

            raw_data = encoded(body).encode()
            outpoint = gov.outpoint_bytes(*self.collateral(mn))
            prefix = bytes(32) + struct.pack('<iq', 1, int(self.mocktime))
            unsigned = prefix + bytes(32) + set_string(raw_data) + struct.pack('<i', 2) + outpoint
            sig = G2Basic.Sign(self.operator_key(mn), hash256(unsigned))
            raw = unsigned + set_string(sig)
            h = hash256(prefix + set_string(raw_data.hex().encode()) + outpoint
                        + b'\x00\xff\xff\xff\xff' + set_string(sig))[::-1].hex()
            return h, raw

        def trigger_vote(self, node, wire, fetch, h, mn, outcome):
            outpoint = gov.outpoint_bytes(*self.collateral(mn))
            now = int(self.mocktime)
            result = gov.OUTCOMES[outcome]
            sig = self.operator_sign(mn, outpoint, h, 1, result, now)
            raw = gov.vote_bytes(outpoint, h, 1, result, now, sig)
            vh = gov.vote_hash(outpoint, h, 1, result, now)
            wire.send(1, 'inv', inventory(18, [vh]))
            wire.send(1, 'govobjvote', raw)
            if vh not in node.gobject('getcurrentvotes', h):
                raise RuntimeError('source rejected trigger vote')
            return vh, {'hex': fetch(18, vh, 'govobjvote'), 'parent': h}

        def isolated_inputs(self, name, capture, collect):
            with observed(name, capture) as (oracle, now):
                isolated = Transcript(name, oracle, now)
                try:
                    isolated.peer()
                    isolated.finish_sync()

                    def get_payload(kind, h, cmd):
                        start = len(isolated.lines)
                        isolated.send(1, 'getdata', inventory(kind, [h]))
                        return next(line.split(' ', 3)[3] for line in isolated.lines[start:]
                                    if line.startswith(f'< 1 {cmd} '))

                    adapter = SimpleNamespace(gobject=lambda *params: oracle.rpc('gobject', *params))
                    return collect(adapter, isolated, get_payload)
                finally:
                    for peer in isolated.peers.values():
                        peer.sock.close()

        def competing_inputs(self, controller, node, wire, fetch, capture):
            from test_framework.messages import CBlock
            from test_framework.script import CScript

            base = next(o for o in controller.gobject('list', 'all').values() if o['ObjectType'] == 2)
            triggers = {}
            blocks = {}
            for mn in self.mninfo[1:3]:
                body = json.loads(base['DataString'])
                addr = controller.getnewaddress()
                body['payment_addresses'] = addr
                h, raw = self.signed_trigger(mn, body)
                wire.send(1, 'inv', inventory(17, [h]))
                wire.send(1, 'govobj', raw)
                if h not in node.gobject('list', 'all'):
                    raise RuntimeError('source rejected competing trigger')
                triggers[h] = {'hex': fetch(17, h, 'govobj'), 'type': 2}
                block = CBlock()
                block.deserialize(io.BytesIO(bytes.fromhex(capture['superblock']['hex'])))
                block.vtx[0].vout[-1].scriptPubKey = CScript(bytes.fromhex(controller.validateaddress(addr)['scriptPubKey']))
                block.vtx[0].rehash()
                block.hashMerkleRoot = block.calc_merkle_root()
                block.solve()
                blocks[h] = block.serialize().hex()
            low, high = sorted(triggers, key=hbytes)
            distributions = [(low, self.mninfo[4], 'yes'),
                             *[(low, mn, 'no') for mn in self.mninfo[:3]],
                             *[(high, mn, 'yes') for mn in self.mninfo[:2]]]
            stages = []
            for distribution in (distributions, [(low, self.mninfo[3], 'yes')]):
                votes = {}
                for h, mn, outcome in distribution:
                    vh, vote = self.trigger_vote(node, wire, fetch, h, mn, outcome)
                    votes[vh] = vote
                stages.append(votes)
            return {'objects': triggers, 'blocks': blocks, 'rank_votes': stages[0],
                    'tie_votes': stages[1], 'low': low, 'high': high}

    # Framework ports comprise disjoint 10,000-port P2P/RPC ranges.
    for seed in range(1, 80):
        PortSeed.n = seed
        held = []
        try:
            for port in [p(i) for p in (p2p_port, rpc_port) for i in range(10)]:
                if port >= 32768:
                    raise RuntimeError('framework port outside allowed range')
                s = socket.socket()
                held.append(s)
                s.bind(('127.0.0.1', port))
            break
        except OSError:
            continue
        finally:
            for s in held:
                s.close()
    else:
        raise RuntimeError('no free framework port set')
    config = ROOT / 'framework.ini'
    config.write_text('[environment]\nPACKAGE_NAME=Dash Core\nPACKAGE_BUGREPORT=none\n'
                      f'SRCDIR={core}\nBUILDDIR={DASHD.parent.parent}\nEXEEXT=\n'
                      '[components]\nENABLE_WALLET=true\nUSE_SQLITE=true\nUSE_BDB=true\n'
                      'ENABLE_CLI=true\nENABLE_UTIL_TOOL=true\nENABLE_WALLET_TOOL=true\n'
                      'ENABLE_BITCOIND=true\nENABLE_ZMQ=false\n')
    network_dir = Path(tempfile.mkdtemp(prefix='network-', dir=ROOT))
    network_dir.rmdir()
    sys.argv = [sys.argv[0], f'--configfile={config}', f'--tmpdir={network_dir}',
                f'--outdir={out}', '--timeout-factor=8', f'--portseed={seed}',
                '--randomseed=733']
    os.environ['DASHD'] = str(DASHD)
    os.environ['DASHCLI'] = str(DASHD.with_name('dash-cli'))
    Builder().main()


def provenance(out):
    files = sorted(p for p in out.iterdir() if p.name != 'PROVENANCE.md' and p.is_file())
    rows = '\n'.join(f'| `{p.name}` | {p.stat().st_size} | '
                     f'`{hashlib.sha256(p.read_bytes()).hexdigest()}` |' for p in files)
    binary_hash = hashlib.sha256(DASHD.read_bytes()).hexdigest()
    with gzip.open(out / 'bootstrap.json.gz', 'rt') as f:
        source = json.load(f)
    lock = source.get('chainlock', {'error': 'source extension not available'})
    lock_coverage = ('Quorum formation failed: ' + str(lock['error']) if 'error' in lock else
                     'A real regtest quorum/ChainLock and control distinguish fully synced governance bypass.')
    text = '''# GV1b release-node recordings

Release dashd v24.0.0: `{DASHD}`. Binary SHA256:
`{binary_hash}`. Acquired with
`DASHVERSION=24.0.0 python3 -I contrib/setup-dashd.py`.

The recorder imports P2A's node, transport and framing helpers. Its source
builder extends `dash-oracle-sys/scripts/gov_capture.py`, the builder behind
`gen-gov-regtest.py`: controller, four regular masternodes and two evonodes.
The functional framework is Core tag v24.0.0, commit
`a94ecf49c2dc2ae77346acda3b80c3707ab2430e`. The observer replays full blocks
from the frozen bootstrap into a fresh wallet-disabled release node.

## Reproduction

Run from this repository. These commands prepare the exact source tree and
Python package versions used. They require git, uv and a C compiler for the
small `dash-hash` extension; no Core build is involved.

```sh
DASHVERSION=24.0.0 python3 -I contrib/setup-dashd.py
install -d -m 700 /work/scratch/gv1b-captures/core
git clone --depth 1 --branch v24.0.0 https://github.com/dashpay/dash.git /work/scratch/gv1b-captures/dash-v24
git -C /work/scratch/gv1b-captures/dash-v24 archive v24.0.0 test | tar -x -C /work/scratch/gv1b-captures/core
uv venv --python 3.12.15 /work/scratch/gv1b-captures/venv
uv pip install --python /work/scratch/gv1b-captures/venv/bin/python3 annotated-types==0.8.0 cytoolz==1.2.0 dash-hash==1.5.1 eth-hash==0.8.0 eth-typing==6.0.0 eth-utils==6.0.0 py-ecc==8.0.0 pydantic==2.14.3 pydantic-core==2.50.4 toolz==1.2.0 typing-extensions==4.16.0 typing-inspection==0.4.5
/work/scratch/gv1b-captures/venv/bin/python3 -I contrib/node/gv1b-transcripts/capture.py --build-source /work/scratch/gv1b-captures/core
/work/scratch/gv1b-captures/venv/bin/python3 -I contrib/node/gv1b-transcripts/capture.py --check
/work/scratch/gv1b-captures/venv/bin/python3 -I contrib/node/gv1b-transcripts/capture.py --selftest
```

Without `--build-source`, recordings reuse `bootstrap.json.gz`. A new source
build changes wallet keys, transactions and hashes and must regenerate every
row. `--check` uses the frozen bootstrap, never a randomized source rebuild.
All datadirs are mode 0700 below `/work/scratch/gv1b-captures/`. Listeners use
probed ports below 32768. Observers stop in `finally` and delete their own
datadirs; the framework stops its source nodes and cleans up after success.

## Record grammar

UTF-8, one record per line. Blank lines and lines starting with `#` are
comments. `N` is a positive logical peer ID; `S`, `T`, `COUNT` and `SERVICES`
are nonnegative decimal integers (COUNT is positive). `H` is a 32-byte hash
in displayed hexadecimal order. `HEX` is even-length lowercase hexadecimal;
an empty message payload omits HEX. Writers omit trailing whitespace.
JSON occupies the rest of its line and follows the schemas below.

```text
mocktime T
ibd 0|1
time T
mockscheduler S
timeoffset SIGNED_SECONDS
accept N
connect N outbound-full-relay|manual SERVICES
> N CMD [HEX]
> N pong echo
< N CMD [HEX]
< N closed
rand bloom HEX
mnsync reset|next
invalidateblock H
reconsiderblock H
generate COUNT raw(51)
submitblock HEX
restart
check gobject JSON
check score JSON
check rpc JSON
log JSON_STRING
```

`mocktime` describes process startup; `time` only sets the wall mock clock.
`mockscheduler` independently advances Core's steady scheduler in seconds.
`timeoffset` supplies the adjusted-time offset observed in `getnetworkinfo`;
the preceding version messages show how the release node obtained it.
`accept` and `connect` create logical peers; `restart` drops connections and
restarts the same datadir with the original arguments and startup mock time.
Peer IDs may be reused after restart. `mnsync`, invalidation, reconsideration,
generation and submission are inputs. Their results appear in `check rpc`.
`submitblock` carries the entire serialized block, including transactions.

P2A checks frame magic, length, command padding and checksums. Its internal
synchronization pings/pongs are omitted; ordinary node pings remain. A final
incomplete frame is an error. Per-peer outgoing message order is retained.
`rand bloom` is exactly four bytes, copied from the tweak field of the next
outgoing nonempty `govsync` bloom filter, in wire order. It is never generated
by the recorder. Empty-filter fetches consume no random input. The Python
parser verifies each random input against the actual outgoing payload.

`check gobject` is an object with exactly these keys:

- `label`: checkpoint name;
- `objects`: unmodified `gobject list all` result, keyed by object hash;
- `votes`: object hash to unmodified `gobject getcurrentvotes` result;
- `budget`: `height` (next superblock) and `amount` (`getsuperblockbudget`);
- `sync`: unmodified `mnsync status` result;
- `chain`: unmodified `getblockchaininfo` result.

Every object checkpoint first drains the validation notification queue and
has a matching `check score`. Its keys are `label` and `peers`. Each peer
entry has `core_id`, `score` and `connected`. Live scores come directly from
`getpeerinfo.banscore`, enabled with `-deprecatedrpc=banscore`. A disconnected
peer retains its last RPC score, updated by any later original `Misbehaving`
log transition. It is never assigned a guessed terminal score.

`check rpc` has `method`, `params`, and exactly one of `result` or `error`.
It is an observation, never an instruction to invoke the method. Its method
allowlist is in `parse_records`; the full result/error is kept. In particular,
mining records precede the result and subsequent submit/lookup observations
for the same block bytes. Resubmitting an already mined block normally
returns `duplicate`. `log` retains an original release log line as a JSON
string. Legacy `rpc` and `checkpoint` records are rejected.

`--selftest` parses every `R*.txt`, validates record arities, hex sizes, JSON
schemas, peer references, checkpoint score coverage and bloom input bindings,
and runs malformed-record controls. This slice changes Python only; the Rust
parser and replay hooks belong to S3b.

## Fixed retention and comparison rules

The bootstrap retains only fields selected in the recorder and uses gzip
mtime zero. Shared bootstrap blocks are not repeated in each transcript;
R6–R9 and their subcases retain submit/lookup answers in companion compressed
JSON files. ChainLock subcases use the separate chain in `chainlock.blocks`.
Later submitted blocks carry full hex and both RPC answers in the transcript.
R9's governance cache is copied unchanged after a clean stop, before restart.
Logs use only the fixed substring filter in `Transcript.logs`, never hand
selection. They are appended in original order at shutdown; R9 also flushes
them immediately before restart and after RPC readiness to expose load order.

Block probes replace the last coinbase payee or add one whole superblock
budget, then recompute merkle root and proof of work with Core's Python block
codec. Trigger probes use real source MN keys, accepted and serialized back
by a separate release node. Signed funding votes use BLS operator keys for
triggers. The competing payees have equal amounts and different scripts, so
their blocks distinguish the selected trigger. All verdicts and check values
come from observed release dashd; no expected verdict is written as a fixture.

The ChainLock observers both receive the unchanged signed enable spork from
the first matching outgoing `spork` in the committed SP0 recording
`dashd-masternode/tests/fixtures/dashd-v24/sporks-receive.txt`. That recording
obtained the packet from a release-node signer using Core's regtest test key.
The locked observer announces/requests the real `clsig` before delivery;
its verified lock is checked before the block body arrives.

R10 extracts each unchanged signed mutation transaction from its source block,
sends it as `tx`, checks mempool acceptance, then mines it on the observer.
This tests key changes at heights 187–189 without importing the unrelated DKG
chain and its later governance objects. It mines through the next superblock
at 200 and retains its full bytes, mining result, resubmission and lookup.

`--check` compares R1 exactly after the listed exclusions: runtime listener
ports, outgoing version nonce and MNAUTH challenge, ordinary ping nonce, log
wall timestamp and blockchain `verificationprogress` (a wall-clock estimate).
No governance hash, payload, score, count, height, mock time, log outcome or
scheduler delta is masked. Stored files are not rewritten by normalization.

R2 retains and marks non-requester inventories with
`# declared-difference: R2-govsync-cross-peer requester=2 recipient=N`.
This slice adds no ALLOWED-DIFFS row. R9's immediate startup checkpoint precedes
mock-time restoration, peer setup, forced sync advancement and a new block.
Its logs show trigger reconstruction/far-future testing before the first
cached-height update. Core initializes `nCachedBlockHeight` to zero; no RPC
exposes that initial value, so no invented numeric observation is emitted.

## Coverage

- R1: unsolicited drop, two announcers, global request forgetting observed by
  the second delivery being logged as unrequested, proposals/trigger, invalid
  collateral +20 and batched relay.
- R2: full sync/SSC10, fetch, bloom vote sync/SSC11, repeat penalties, repeated
  absent-parent empty-filter requests with unchanged score, oversized bloom,
  unsynced ignore and marked cross-peer announcements.
- R3: immediate/periodic orphan requests, arrival, expiry, malformed +100,
  bad signature +20 fully synced and zero in GOVERNANCE sync. `R3-offset`
  observes +600 adjusted offset from four distinct outbound loopback peers,
  then a parent arriving before the wall deadline but after the adjusted
  deadline, without scheduler cleanup in between.
- R4: rejection at zero confirmations, postponed at 1–5, automatic acceptance
  at six without resending.
- R5: per-object vote requests, extracted bloom tweaks, SSC replies and natural
  FINISHED timing. Request order and blooms remain as sent by dashd.
- R6: correct/wrong-payee/over-budget verdicts; `R6-rank` and `R6-tie` use two
  triggers and different-paying blocks. The lower internal hash starts with
  weighted YES 4 and NO 3 (absolute 1); the higher hash has YES 2 (absolute 2).
  Adding one YES to the lower hash makes an absolute-2 tie. Internal order is
  Core's little-endian byte comparison, not displayed-hash order. Both wrong
  payees and excess value really return `bad-cb-amount` in v24.0.0.
- R7: unsynced max bounds and independently judged sibling blocks. {lock_coverage}
- R8: execution, pure disconnect, alternate branch/reactivation, the shared
  regtest expiry boundary and far-future trigger after deep reorg. A 20-block
  cycle cannot distinguish mainnet's two lifetimes; that is S1b coverage.
- R9: clean-stop cache, startup load order and immediate trigger/template state,
  all current votes, and the same next block accepted. Startup marks the trigger
  too far in the future; its later deletion is retained rather than corrected.
- R10: voting-key update, operator-key update while registered, removal,
  invalidation snapshots and the next superblock's mining/submit/lookup verdicts.
  Resubmission of each mined block is `duplicate`.

No internal oracle stream was recorded. The existing optional build is
documented in `contrib/node/statedigest/build.sh` (`--out` directory), with
`record-ft.py` and `dashd-wrapper` documenting recording. No such binary was
built or located. Direct callback-idempotence inputs remain outside these
release-node transcripts.

## Artifacts

| File | Bytes | SHA256 |
|---|---:|---|
{rows}
'''
    for key, value in {'DASHD': DASHD, 'binary_hash': binary_hash,
                       'lock_coverage': lock_coverage, 'rows': rows}.items():
        text = text.replace('{' + key + '}', str(value))
    (out / 'PROVENANCE.md').write_text(text)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, default=FIXTURES)
    parser.add_argument('--build-source', type=Path)
    parser.add_argument('--network-worker', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--selftest', action='store_true')
    parser.add_argument('scenarios', nargs='*', choices=[f'R{i}' for i in range(1, 11)])
    args = parser.parse_args()
    os.umask(0o077)
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    args.out.mkdir(parents=True, exist_ok=True)
    if args.selftest:
        selftest(args.out)
        return
    if args.network_worker:
        network_worker(args.network_worker, args.out)
        return
    if args.build_source:
        with tempfile.TemporaryDirectory(prefix='source-', dir=ROOT) as directory:
            source = Path(directory)
            subprocess.run([sys.executable, '-I', __file__, '--network-worker', str(args.build_source),
                            '--out', str(source)], check=True)
            shutil.copyfile(source / 'bootstrap.json.gz', args.out / 'bootstrap.json.gz')
    with gzip.open(args.out / 'bootstrap.json.gz', 'rt') as f:
        data = json.load(f)
    if args.check:
        print('Excluded from comparison: runtime ports; version nonce and MNAUTH challenge; '
              'ping nonce; log wall timestamp; verificationprogress (wall-clock estimate).')
        with tempfile.TemporaryDirectory(prefix='check-', dir=ROOT) as directory:
            record('R1', data, Path(directory))
            before = normalize((args.out / 'R1.txt').read_text())
            after = normalize((Path(directory) / 'R1.txt').read_text())
            if before != after:
                sys.stdout.writelines(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                                          fromfile='fixture', tofile='recapture'))
                raise SystemExit('--check: R1 differs')
        print('--check: R1 matches')
        return
    for name in args.scenarios or [f'R{i}' for i in range(1, 11)]:
        print(f'Capturing {name}', flush=True)
        record(name, data, args.out)
    provenance(args.out)


if __name__ == '__main__':
    main()
