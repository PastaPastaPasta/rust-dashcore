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
import tempfile
import time

REPO = pathlib.Path(__file__).resolve().parents[3]
SCRATCH = pathlib.Path('/work/scratch/ix1-captures')
FIXTURE = REPO / 'dashd-indexes/tests/fixtures/dashd-v24'
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
    parser.add_argument('--dashd', type=pathlib.Path, default=pathlib.Path(
        os.environ.get('IX1_DASHD', '~/.rust-dashcore-test/dashcore-24.0.0/bin/dashd')).expanduser())
    args = parser.parse_args()
    SCRATCH.mkdir(mode=0o700, parents=True, exist_ok=True)
    SCRATCH.chmod(0o700)
    print('Normalisation: runtime datadirs/RPC settings are omitted from header args; listener ports use placeholders. '
          'All RPC response fields and block/undo bytes are retained.', flush=True)
    with tempfile.TemporaryDirectory(prefix='check-' if args.check else 'output-', dir=SCRATCH) as work:
        out = pathlib.Path(work) / 'fixture'
        record(out, args.dashd.resolve())
        if args.check:
            if not compare(FIXTURE, out):
                raise SystemExit('--check FAILED')
            print('--check PASS: every fixture byte reproduced')
        else:
            if FIXTURE.exists():
                shutil.rmtree(FIXTURE)
            shutil.copytree(out, FIXTURE)
            print(f'wrote {FIXTURE}')


if __name__ == '__main__':
    main()
