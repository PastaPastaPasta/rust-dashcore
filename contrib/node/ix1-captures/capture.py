#!/usr/bin/env python3
"""Record release dashd's three index oracles; --check compares a fresh recording."""

import argparse
import ast
import contextlib
import difflib
import hashlib
import importlib.util
import json
import os
import pathlib
import random
import shutil
import socket
import struct
import subprocess
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[3]
SCRATCH = pathlib.Path('/work/scratch/ix1-captures')
FIXTURE = REPO / 'dashd-indexes/tests/fixtures/dashd-v24'
REORG_FIXTURE = REPO / 'dashd-indexes/tests/fixtures/dashd-v24-reorg'
CF_FIXTURE = REPO / 'dashd-indexes/tests/fixtures/dashd-v24-cfcheckpt'
SEED = 'cMfbiEsnG5b8Gwm6vEgfWvZLuXZNC4zsN2y7Es3An9xHRWRjmwgR'
OPERATOR = '00' * 31 + '01'
SERVICE = '127.0.0.1:19899'
FLAGS = (
    '-txindex=1', '-blockfilterindex=1', '-coinstatsindex=1',
    '-dip3params=2:2', '-testactivationheight=v20@100',
    '-testactivationheight=mn_rr@1000', '-llmqtestplatformparams=1:1',
    '-keypool=1', '-par=1', '-parbls=1', '-persistmempool=0', '-tinyblk=1',
)

spec = importlib.util.spec_from_file_location('p2a_capture', REPO / 'contrib/node/p2a-transcripts/capture.py')
p2a = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p2a)


def wait_for(check, description):
    end = time.monotonic() + 180
    while time.monotonic() < end:
        if check():
            return
        time.sleep(0.1)
    raise RuntimeError(f'timed out waiting for {description}')


def free_ports():
    for port in random.sample(range(12000, 32766, 2), 10383):
        with contextlib.ExitStack() as stack:
            try:
                for number in (port, port + 1):
                    sock = stack.enter_context(socket.socket())
                    sock.bind(('127.0.0.1', number))
                return port
            except OSError:
                pass
    raise RuntimeError('no free pair of ports below 32768')


@contextlib.contextmanager
def node(name, extra=()):
    p2a.PORT_BASE = free_ports()
    instance = p2a.Node.__new__(p2a.Node)
    try:
        instance.__init__(name, 0, p2a.SIM_START, extra=FLAGS + extra)
        if instance.rpc('getnetworkinfo')['subversion'] != '/Dash Core:24.0.0/':
            raise RuntimeError('capture requires release dashd v24.0.0')
        yield instance
    finally:
        if hasattr(instance, 'proc'):
            instance.stop()
            instance.proc.wait(timeout=30)
        elif hasattr(instance, 'stderr'):
            instance.stderr.close()


def write_json(path, value):
    path.write_text(json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False) + '\n')


def producer_header(n):
    return [line.replace(f'-port={n.port}', '-port=<p2p-port>')
            .replace(f'-bind=127.0.0.1:{n.port}', '-bind=127.0.0.1:<p2p-port>')
            for line in p2a.Transcript('indexes', n, [], False).lines[:2]]


def outcome(n, method, *params):
    try:
        return {'result': n.rpc(method, *params)}
    except RuntimeError as error:
        return {'error': ast.literal_eval(str(error).split(': ', 1)[1])}


def satoshis(value):
    from decimal import Decimal
    return int(Decimal(str(value)) * 100_000_000)


def amount(value):
    return round(value / 100_000_000, 8)


def vector(data):
    return p2a.compact(len(data)) + data


def output(value, script):
    return struct.pack('<q', value) + vector(script)


def special_tx(kind, inputs, outputs, payload):
    return (struct.pack('<HH', 3, kind) + p2a.compact(len(inputs)) + b''.join(inputs)
            + p2a.compact(len(outputs)) + b''.join(outputs) + bytes(4) + vector(payload)).hex()


class Capture:
    def __init__(self, n):
        self.n = n
        self.blocks = [n.rpc('getblockhash', 0)]
        self.events = []
        self.phases = []
        self.roles = {}
        self.miner = None
        self.mn = None

    def sync(self):
        tip = self.n.rpc('getbestblockhash')
        if self.mn:
            wait_for(lambda: self.mn.rpc('getbestblockhash') == tip, 'masternode tip')
        height = self.n.rpc('getblockcount')
        wait_for(lambda: all(v['synced'] and v['best_block_height'] == height
                             for v in self.n.rpc('getindexinfo').values()), 'index tip')

    def mine(self, count, role, transactions=None):
        hashes = []
        for _ in range(count):
            height = self.n.rpc('getblockcount') + 1
            self.n.rpc('setmocktime', p2a.SIM_START + height)
            if self.mn:
                self.mn.rpc('setmocktime', p2a.SIM_START + height)
            if transactions is None:
                h = self.n.rpc('generatetoaddress', 1, self.miner)[0]
            else:
                h = self.n.rpc('generateblock', self.miner, transactions)['hash']
            self.blocks.append(h)
            self.roles[h] = role
            self.events.append({'connect': h})
            hashes.append(h)
            self.sync()
        return hashes

    def phase(self, name, hashes=()):
        self.sync()
        self.phases.append({
            'name': name, 'tip': self.n.rpc('getbestblockhash'),
            'getindexinfo': self.n.rpc('getindexinfo'),
            'blocks': [self.block(h, transactions=True) for h in hashes],
        })
        print(f"{name}: height {self.n.rpc('getblockcount')}", flush=True)

    def block(self, h, transactions=False):
        record = {
            'hash': h, 'role': self.roles.get(h, 'genesis'),
            'getblockheader': self.n.rpc('getblockheader', h),
            'getblockfilter': self.n.rpc('getblockfilter', h),
            'gettxoutsetinfo': self.n.rpc('gettxoutsetinfo', 'muhash', h, True),
            'txids': self.n.rpc('getblock', h)['tx'],
        }
        if transactions:
            record['transactions'] = [self.transaction(txid, [h]) for txid in record['txids']]
        return record

    def transaction(self, txid, hashes):
        without = outcome(self.n, 'getrawtransaction', txid, True)
        with_hash = {h: outcome(self.n, 'getrawtransaction', txid, True, h) for h in hashes}
        results = [reply['result'] for reply in [without, *with_hash.values()] if 'result' in reply]
        common = {key: value for key, value in results[0].items()
                  if all(key in result and result[key] == value for result in results)} if results else {}
        for result in results:
            for key in common:
                del result[key]
        return {'txid': txid, 'common_result': common,
                'without_blockhash': without, 'with_blockhash': with_hash}

    def spend(self, coins, outputs):
        raw = self.n.rpc('createrawtransaction', coins, outputs)
        signed = self.n.rpc('signrawtransactionwithwallet', raw)
        if not signed['complete']:
            raise RuntimeError('incomplete transaction signature')
        return self.n.rpc('sendrawtransaction', signed['hex'])

    def coinbase(self, height):
        h = self.n.rpc('getblockhash', height)
        txid = self.n.rpc('getblock', h)['tx'][0]
        tx = self.n.rpc('getrawtransaction', txid, True, h)
        return {'txid': txid, 'vout': 0}, satoshis(tx['vout'][0]['value'])


def script_chain(c, mn):
    n = c.n
    n.rpc('createwallet', 'capture', False, True, '', False, False, False)
    n.rpc('sethdseed', True, SEED)
    c.miner = n.rpc('getnewaddress')
    c.phase('genesis')
    c.mine(110, 'coinbase maturity and DIP3/v20 activation')
    c.phase('mature')
    owner, voting, collateral, funds, payee, updated_voting = [n.rpc('getnewaddress') for _ in range(6)]
    coins, values = zip(*(c.coinbase(h) for h in range(1, 11)))
    c.spend(list(coins), [{funds: 4000.1}, {c.miner: amount(sum(values) - 400_010_100_000)}])
    c.mine(1, 'plain spend funding EvoNode collateral and fees')
    public = n.rpc('bls', 'fromsecret', OPERATOR)['public']
    protx = n.rpc('protx', 'register_fund_evo', collateral, SERVICE, owner, public, voting, 0,
                  payee, '11' * 20, 19900, 19901, funds, True)
    c.mine(1, 'ProRegTx (EvoNode)')
    c.phase('provider_registered')
    mn.rpc('addnode', f'127.0.0.1:{n.port}', 'onetry')
    c.mn = mn
    c.sync()
    for _ in range(4):
        mn.rpc('mnsync', 'next')
    wait_for(lambda: mn.rpc('masternode', 'status')['state'] == 'READY', 'active masternode')
    # A fixed advertised service keeps transactions independent of listener ports.
    mn.rpc('sporkupdate', 'SPORK_17_QUORUM_DKG_ENABLED', 0)
    mn.rpc('sporkupdate', 'SPORK_19_CHAINLOCKS_ENABLED', 4070908800)
    mn.rpc('sporkupdate', 'SPORK_2_INSTANTSEND_ENABLED', 4070908800)
    c.mine(32, 'reach Platform quorum base after membership lookback')
    wait_for(lambda: any(q['llmqType'] == 'llmq_test_platform' and q['status']['phase'] == 6
                         for q in mn.rpc('quorum', 'dkgstatus')['session']), 'Platform commitment')
    c.mine(9, 'Platform quorum mining window')
    wait_for(lambda: any(q['llmqType'] == 106 and q['validMembersCount'] == 1
                         for q in n.rpc('quorum', 'dkgstatus')['minableCommitments']), 'relayed Platform commitment')
    c.mine(1, 'Platform quorum commitment')
    c.mine(8, 'Platform quorum signing offset')
    c.phase('platform_quorum')

    coin, value = c.coinbase(11)
    credit_script = bytes.fromhex(n.rpc('getaddressinfo', c.miner)['scriptPubKey'])
    lock_amount, fee = 1_000_000_000, 70_000
    payload = b'\x01' + b'\x01' + output(lock_amount, credit_script)
    txin = bytes.fromhex(coin['txid'])[::-1] + struct.pack('<I', coin['vout']) + b'\x00' + b'\xff' * 4
    raw = special_tx(8, [txin], [output(value - lock_amount - fee, credit_script),
                               output(lock_amount, b'\x6a\x00')], payload)
    signed = n.rpc('signrawtransactionwithwallet', raw)
    if not signed['complete']:
        raise RuntimeError('incomplete asset lock signature')
    n.rpc('sendrawtransaction', signed['hex'])
    c.mine(1, 'asset lock (10 DASH)')
    c.phase('asset_locked')

    request_id = p2a.sha256d(vector(b'plwdtx') + struct.pack('<Q', 101))[::-1].hex()
    quorum = mn.rpc('quorum', 'selectquorum', 106, request_id)['quorumHash']
    payload = (struct.pack('<BQI', 1, 101, fee) + struct.pack('<I', n.rpc('getblockcount'))
               + bytes.fromhex(quorum)[::-1])
    raw = special_tx(9, [], [output(100_000_000 - fee, credit_script)], payload + bytes(96))
    msg_hash = p2a.sha256d(bytes.fromhex(raw))[::-1].hex()
    sig = mn.rpc('quorum', 'platformsign', request_id, msg_hash, quorum, False)['signature']
    raw = special_tx(9, [], [output(100_000_000 - fee, credit_script)], payload + bytes.fromhex(sig))
    n.rpc('sendrawtransaction', raw)
    unlock_block = c.mine(1, 'asset unlock (1 DASH)')[0]
    c.phase('asset_unlocked', [unlock_block])
    stats = n.rpc('gettxoutsetinfo', 'muhash', unlock_block, True)
    if stats['block_info']['unspendables']['unclaimed_rewards'] >= 0:
        raise RuntimeError('asset unlock did not make block unclaimed_rewards negative')

    n.rpc('protx', 'update_service_evo', protx, SERVICE, OPERATOR, '22' * 20,
          19902, 19903, '', funds, True)
    c.mine(1, 'ProUpServTx (new Platform endpoints)')
    n.rpc('protx', 'update_registrar', protx, '', updated_voting, '', funds, True)
    c.mine(1, 'ProUpRegTx (new voting key)')
    n.rpc('protx', 'revoke', protx, OPERATOR, 1, funds, True)
    c.mine(1, 'ProUpRevTx')
    c.phase('provider_revoked')

    coin, value = c.coinbase(12)
    remine = c.spend([coin], [{payee: amount(value - 100_000)}])
    original = c.mine(1, 'original branch: plain spend to re-mine')
    coin, value = c.coinbase(13)
    c.spend([coin], [{payee: amount(value - 100_000)}, {'data': '495831'}])
    original += c.mine(1, 'original branch: OP_RETURN spend')
    original += c.mine(1, 'original branch: empty tip')
    c.phase('before_invalidate', original)
    n.rpc('invalidateblock', original[0])
    c.events += [{'disconnect': h} for h in reversed(original)]
    mn.rpc('invalidateblock', original[0])
    c.phase('invalidated', original)
    competing = c.mine(1, 'competing branch: same plain transaction', [remine])
    competing += c.mine(1, 'competing branch: empty tip', [])
    c.phase('competing', original + competing)
    n.rpc('reconsiderblock', original[0])
    c.events += [{'disconnect': h} for h in reversed(competing)]
    c.events += [{'connect': h} for h in original]
    mn.rpc('reconsiderblock', original[0])
    c.phase('reconsidered', original + competing)
    if n.rpc('getbestblockhash') != original[-1]:
        raise RuntimeError('reconsiderblock did not restore the longer original branch')


def record(out, binary):
    p2a.DASHD = str(binary)
    with tempfile.TemporaryDirectory(prefix='record-', dir=SCRATCH) as work:
        p2a.ROOT = pathlib.Path(work)
        with contextlib.ExitStack() as stack:
            n = stack.enter_context(node('indexes'))
            mn = stack.enter_context(node('masternode', (
                '-masternodeblsprivkey=' + OPERATOR, '-externalip=' + SERVICE,
                '-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK',
            )))
            info = n.rpc('getnetworkinfo')
            if info['subversion'] != '/Dash Core:24.0.0/':
                raise RuntimeError('capture requires release dashd v24.0.0')
            c = Capture(n)
            script_chain(c, mn)
            blocks = [c.block(h) for h in c.blocks]
            locations = {}
            for block in blocks:
                for txid in block['txids']:
                    locations.setdefault(txid, []).append(block['hash'])
            transactions = [c.transaction(txid, hashes) for txid, hashes in sorted(locations.items())]
            metadata = {'header': producer_header(n), 'masternode_header': producer_header(mn),
                        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                        'mocktime': p2a.SIM_START, 'events': c.events, 'phases': c.phases}
        out.mkdir(parents=True, exist_ok=True)
        write_json(out / 'chain.json', metadata)
        write_json(out / 'blocks.json', blocks)
        write_json(out / 'transactions.json', transactions)
        blocks_out = out / 'blocks'
        blocks_out.mkdir()
        for pattern in ('blk*.dat', 'rev*.dat'):
            for source in sorted((n.dir / 'regtest/blocks').glob(pattern)):
                shutil.copyfile(source, blocks_out / source.name)
    size = sum(p.stat().st_size for p in out.rglob('*') if p.is_file())
    for path in sorted(p for p in out.rglob('*') if p.is_file()):
        print(f'{path.relative_to(out)}: {path.stat().st_size} bytes', flush=True)
    if size > 2 * 1024 * 1024:
        raise RuntimeError(f'fixture is too large: {size} bytes')
    print(f'recorded {len(blocks)} blocks, {len(transactions)} transactions, {size} bytes', flush=True)


def create_wallet(n):
    n.rpc('createwallet', 'capture', False, True, '', False, False, False)
    n.rpc('sethdseed', True, SEED)
    return n.rpc('getnewaddress')


def phase(c, name, hashes=(), **extra):
    c.phase(name, hashes)
    c.phases[-1].update(extra)


def restart(n):
    n.rpc('stop')
    n.proc.wait(timeout=30)
    n.stderr.close()
    n.stderr = open(n.dir / 'stderr-restart.txt', 'wb')
    n.proc = subprocess.Popen(n.args, stdout=subprocess.DEVNULL, stderr=n.stderr)

    def ready():
        if n.proc.poll() is not None:
            raise RuntimeError('dashd exited during restart')
        try:
            n.rpc('getblockcount')
            return True
        except (RuntimeError, OSError):
            return False

    wait_for(ready, 'dashd restart')


def copy_blockfiles(out, n):
    blocks_out = out / 'blocks'
    blocks_out.mkdir()
    for pattern in ('blk*.dat', 'rev*.dat'):
        for source in sorted((n.dir / 'regtest/blocks').glob(pattern)):
            shutil.copyfile(source, blocks_out / source.name)


def make_underclaim(c):
    n = c.n
    n.rpc('setmocktime', p2a.SIM_START + 1)
    template = n.rpc('getblocktemplate', {'rules': []})
    if template['height'] != 1 or template['transactions']:
        raise RuntimeError('underclaim requires a genesis-only chain')
    before = n.rpc('gettxoutsetinfo', 'muhash', template['previousblockhash'], True)
    script = bytes.fromhex(n.rpc('getaddressinfo', c.miner)['scriptPubKey'])
    script_sig = b'\x51' + bytes.fromhex(template['coinbaseaux'].get('flags', '')) + b'\x00'
    tx = (struct.pack('<I', 1) + b'\x01' + bytes(32) + b'\xff' * 4
          + vector(script_sig) + b'\xff' * 4 + b'\x01'
          + output(template['coinbasevalue'] - 100_000_000, script) + bytes(4))
    header = (struct.pack('<I', template['version'])
              + bytes.fromhex(template['previousblockhash'])[::-1] + p2a.sha256d(tx)
              + struct.pack('<II', template['curtime'], int(template['bits'], 16)))
    # dashd checks X11 proof of work; no extra hashing package is needed.
    for nonce in range(1024):
        raw = (header + struct.pack('<I', nonce) + b'\x01' + tx).hex()
        reply = n.rpc('submitblock', raw)
        if reply is None:
            break
        if reply != 'high-hash':
            raise RuntimeError(f'underclaim rejected: {reply}')
    else:
        raise RuntimeError('could not solve underclaim block')
    h = n.rpc('getbestblockhash')
    c.blocks.append(h)
    c.roles[h] = 'template coinbase underclaims one DASH'
    c.events.append({'connect': h})
    c.sync()
    after = n.rpc('gettxoutsetinfo', 'muhash', h, True)
    if after['block_info']['unspendables']['unclaimed_rewards'] != 1.0:
        raise RuntimeError('underclaim did not leave one DASH unclaimed')
    phase(c, 'underclaim', [h], underclaim={'template': template, 'before': before, 'after': after})


def submit_prefix(n, blocks):
    for raw in blocks:
        if n.rpc('submitblock', raw) is not None:
            raise RuntimeError('isolated miner rejected common prefix')


def submit_branch(c, miner, hashes, label):
    n = c.n
    previous = n.rpc('getbestblockhash')
    for h in hashes:
        header = miner.rpc('getblockheader', h)
        c.blocks.append(h)
        c.roles[h] = f'{label}{header["height"] - 100}'
        n.rpc('setmocktime', header['time'])
        reply = n.rpc('submitblock', miner.rpc('getblock', h, 0))
        if reply not in (None, 'inconclusive'):
            raise RuntimeError(f'observed node rejected branch {label}: {reply}')
        if n.rpc('getblock', h)['hash'] != h:
            raise RuntimeError(f'observed node did not store branch {label}')
        tip = n.rpc('getbestblockhash')
        if h != hashes[-1] and tip != previous:
            raise RuntimeError(f'branch {label} switched before gaining more work')
    if n.rpc('getbestblockhash') != hashes[-1]:
        raise RuntimeError(f'branch {label} did not win')
    old = n.rpc('getblockheader', previous)
    new = n.rpc('getblockheader', hashes[-1])
    connects = []
    while old['hash'] != new['hash']:
        if old['height'] >= new['height']:
            c.events.append({'disconnect': old['hash']})
            old = n.rpc('getblockheader', old['previousblockhash'])
        else:
            connects.append(new['hash'])
            new = n.rpc('getblockheader', new['previousblockhash'])
    c.events.extend({'connect': h} for h in reversed(connects))
    c.sync()


def record_reorg(out, binary):
    p2a.DASHD = str(binary)
    with tempfile.TemporaryDirectory(prefix='reorg-', dir=SCRATCH) as work:
        p2a.ROOT = pathlib.Path(work)
        with node('reorg-observed') as n:
            c = Capture(n)
            c.miner = create_wallet(n)
            c.phase('genesis')
            make_underclaim(c)
            c.mine(99, 'coinbase maturity prefix')
            c.phase('mature')
            c.mine(8, 'common branch prefix (1 through 8)')
            prefix = [n.rpc('getblock', h, 0) for h in c.blocks[1:]]
            coin, value = c.coinbase(2)
            raw = n.rpc('createrawtransaction', [coin], [{c.miner: amount(value - 100_000)}])
            signed = n.rpc('signrawtransactionwithwallet', raw)
            if not signed['complete']:
                raise RuntimeError('transaction T was not signed')
            txid = n.rpc('sendrawtransaction', signed['hex'])
            a9 = c.mine(1, 'A9 (without T)', [])[0]
            a10 = c.mine(1, 'A10 (T)', [txid])[0]
            phase(c, 'original_a10', [a9, a10], transaction_t=c.transaction(txid, [a10]))

            with node('reorg-miner-b') as miner:
                create_wallet(miner)
                address = miner.rpc('getnewaddress')
                submit_prefix(miner, prefix)
                b = Capture(miner)
                b.miner = address
                b.sync()
                b9 = b.mine(1, 'B9 (T)', [signed['hex']])[0]
                btail = b.mine(2, 'B10 through B11', [])
                submit_branch(c, miner, [b9, *btail], 'B')
                phase(c, 'switched_to_b11', [a9, a10, b9, *btail],
                      transaction_t=c.transaction(txid, [a10, b9]))
                miner_b_header = producer_header(miner)

            with node('reorg-miner-c') as miner:
                create_wallet(miner)
                miner.rpc('getnewaddress')
                address = miner.rpc('getnewaddress')
                submit_prefix(miner, prefix)
                # A conflicting spend keeps T out of the mempool after C wins, exposing txindex.
                raw = miner.rpc('createrawtransaction', [coin], [{address: amount(value - 200_000)}])
                conflict = miner.rpc('signrawtransactionwithwallet', raw)
                if not conflict['complete']:
                    raise RuntimeError('C9 conflicting spend was not signed')
                branch = Capture(miner)
                branch.miner = address
                branch.sync()
                c9 = branch.mine(1, 'C9 (conflicting spend, without T)', [conflict['hex']])[0]
                ctail = branch.mine(3, 'C10 through C12 (without T)', [])
                submit_branch(c, miner, [c9, *ctail], 'C')
                phase(c, 'switched_to_c12', [a10, b9, c9, *ctail],
                      transaction_t=c.transaction(txid, [a10, b9, c9]))
                miner_c_header = producer_header(miner)

            restart(n)
            phase(c, 'after_restart', [a10, b9, c9, ctail[-1]],
                  transaction_t=c.transaction(txid, [a10, b9, c9]))
            for snapshot in c.phases[-2:]:
                reply = snapshot['transaction_t']
                result = {**reply['common_result'], **reply['without_blockhash']['result']}
                if result['blockhash'] != b9 or result['confirmations'] != 0:
                    raise RuntimeError('txindex did not retain stale B9 across C12 and restart')
            blocks = [c.block(h) for h in c.blocks]
            locations = {}
            for block in blocks:
                for t in block['txids']:
                    locations.setdefault(t, []).append(block['hash'])
            transactions = [c.transaction(t, hs) for t, hs in sorted(locations.items())]
            metadata = {'header': producer_header(n), 'miner_b_header': miner_b_header,
                        'miner_c_header': miner_c_header,
                        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                        'mocktime': p2a.SIM_START, 'events': c.events, 'phases': c.phases,
                        'transaction_t': txid, 'branch_height_offset': 100}
        out.mkdir(parents=True)
        write_json(out / 'chain.json', metadata)
        write_json(out / 'blocks.json', blocks)
        write_json(out / 'transactions.json', transactions)
        copy_blockfiles(out, n)
    size = sum(p.stat().st_size for p in out.rglob('*') if p.is_file())
    if size > 2 * 1024 * 1024:
        raise RuntimeError(f'reorg fixture is too large: {size} bytes')
    print(f'recorded reorg fixture: {len(blocks)} blocks, {len(transactions)} transactions, {size} bytes', flush=True)


def record_cfcheckpt(out, binary):
    p2a.DASHD = str(binary)
    with tempfile.TemporaryDirectory(prefix='cfcheckpt-', dir=SCRATCH) as work:
        p2a.ROOT = pathlib.Path(work)
        # Defer DIP3 and later deployments so every block has exactly one coinbase.
        with node('cfcheckpt', ('-dip3params=3000:3000', '-testactivationheight=v20@3000',
                               '-testactivationheight=mn_rr@4000')) as n:
            c = Capture(n)
            c.miner = create_wallet(n)
            c.phase('genesis')
            heights = (999, 1000, 1001, 1999, 2000, 2001)
            selected = []
            for height in heights:
                c.mine(height - n.rpc('getblockcount'), 'coinbase-only')
                h = n.rpc('getbestblockhash')
                c.phase(f'height_{height}', [h])
                selected.append({'height': height, 'hash': h,
                                 'getblockheader': n.rpc('getblockheader', h),
                                 'getblockfilter': n.rpc('getblockfilter', h)})
            for h in c.blocks[1:]:
                if n.rpc('getblockheader', h)['nTx'] != 1:
                    raise RuntimeError('checkpoint chain contains a non-coinbase transaction')
            metadata = {'header': producer_header(n), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                        'mocktime': p2a.SIM_START, 'events': c.events, 'phases': c.phases,
                        'kept': 'All ordered connect hashes and seven phase snapshots; six boundary RPC answers. No raw blocks or undo files.'}
        out.mkdir(parents=True)
        write_json(out / 'chain.json', metadata)
        write_json(out / 'checkpoints.json', selected)
    size = sum(p.stat().st_size for p in out.rglob('*') if p.is_file())
    if size > 256 * 1024:
        raise RuntimeError(f'checkpoint fixture is too large: {size} bytes')
    print(f'recorded cfcheckpt fixture: 2001 connects, six boundary blocks, {size} bytes', flush=True)


def compare(expected, actual):
    left = {p.relative_to(expected): p.read_bytes() for p in expected.rglob('*') if p.is_file()}
    right = {p.relative_to(actual): p.read_bytes() for p in actual.rglob('*') if p.is_file()}
    changes = [p for p in sorted(left.keys() | right.keys()) if left.get(p) != right.get(p)]
    for path in changes:
        print(f'differs: {path}')
        if path.suffix == '.json' and path in left and path in right:
            print(''.join(difflib.unified_diff(left[path].decode().splitlines(True),
                                             right[path].decode().splitlines(True)))[:4000])
    return not changes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--fixture', choices=('all', 'g', 'reorg', 'cfcheckpt'), default='all',
                        help='record or compare one fixture (default: all three)')
    parser.add_argument('--dashd', type=pathlib.Path, default=pathlib.Path(
        os.environ.get('IX1_DASHD', '~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd')).expanduser())
    args = parser.parse_args()
    SCRATCH.mkdir(mode=0o700, parents=True, exist_ok=True)
    SCRATCH.chmod(0o700)
    print('Normalisation: runtime datadirs/RPC settings are omitted from header args; listener ports use placeholders. '
          'Recorded RPC answers retain all fields; g and reorg retain exact block/undo files. '
          'cfcheckpt retains the six boundary answers and ordered connects.', flush=True)
    binary = args.dashd.resolve()
    fixtures = {'g': (FIXTURE, record), 'reorg': (REORG_FIXTURE, record_reorg),
                'cfcheckpt': (CF_FIXTURE, record_cfcheckpt)}
    selected = ('g', 'reorg', 'cfcheckpt') if args.fixture == 'all' else (args.fixture,)
    with tempfile.TemporaryDirectory(prefix='check-' if args.check else 'output-', dir=SCRATCH) as work:
        for name in selected:
            expected, recorder = fixtures[name]
            out = pathlib.Path(work) / name
            recorder(out, binary)
            if args.check:
                if not compare(expected, out):
                    raise SystemExit(f'--check FAILED ({name})')
                print(f'--check PASS ({name}): every fixture byte reproduced')
            else:
                if expected.exists():
                    shutil.rmtree(expected)
                shutil.copytree(out, expected)
                print(f'wrote {expected}')


if __name__ == '__main__':
    main()
